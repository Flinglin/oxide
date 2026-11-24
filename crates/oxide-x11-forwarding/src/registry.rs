use std::fmt;

use crate::{
    X11AuthCookie, X11AuthMaterial, X11AuthProtocol, X11Result, inspect_setup_authentication,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct X11SpoofedAuth<ChannelId> {
    pub channel_id: ChannelId,
    pub auth: X11AuthMaterial,
    pub single_connection: bool,
}

pub struct X11AuthSpoofRegistry<ChannelId> {
    entries: Vec<X11SpoofedAuth<ChannelId>>,
}

impl<ChannelId> X11AuthSpoofRegistry<ChannelId> {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl<ChannelId> Default for X11AuthSpoofRegistry<ChannelId> {
    fn default() -> Self {
        Self::new()
    }
}

impl<ChannelId> fmt::Debug for X11AuthSpoofRegistry<ChannelId> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Fake X11 cookies are bearer credentials, so registry Debug exposes
        // only the count and never the stored auth material.
        f.debug_struct("X11AuthSpoofRegistry")
            .field("entries_len", &self.entries.len())
            .finish()
    }
}

impl<ChannelId: Clone + Eq> X11AuthSpoofRegistry<ChannelId> {
    pub fn register_mit_magic_cookie(
        &mut self,
        channel_id: ChannelId,
        local_cookie: X11AuthCookie,
        single_connection: bool,
    ) -> X11AuthMaterial {
        let auth = X11AuthMaterial::mit_magic_cookie(local_cookie);
        self.insert(X11SpoofedAuth {
            channel_id,
            auth: auth.clone(),
            single_connection,
        });
        auth
    }

    pub fn insert(
        &mut self,
        entry: X11SpoofedAuth<ChannelId>,
    ) -> Option<X11SpoofedAuth<ChannelId>> {
        let replaced = self
            .entries
            .iter()
            .position(|existing| existing.auth.fake_cookie == entry.auth.fake_cookie)
            .map(|index| self.entries.remove(index));
        self.entries.push(entry);
        replaced
    }

    pub fn resolve(
        &mut self,
        protocol: X11AuthProtocol,
        fake_cookie: &X11AuthCookie,
    ) -> Option<X11SpoofedAuth<ChannelId>> {
        let index = self.entries.iter().position(|entry| {
            entry.auth.protocol == protocol && entry.auth.fake_cookie.constant_time_eq(fake_cookie)
        })?;

        if self.entries[index].single_connection {
            Some(self.entries.remove(index))
        } else {
            Some(self.entries[index].clone())
        }
    }

    pub fn resolve_setup_packet(
        &mut self,
        packet: &[u8],
    ) -> X11Result<Option<X11SpoofedAuth<ChannelId>>> {
        let setup_auth = inspect_setup_authentication(packet)?;
        Ok(self.resolve(setup_auth.protocol, &setup_auth.fake_cookie))
    }

    pub fn remove_channel(&mut self, channel_id: &ChannelId) -> Vec<X11SpoofedAuth<ChannelId>> {
        let mut removed = Vec::new();
        let mut index = 0;
        while index < self.entries.len() {
            if &self.entries[index].channel_id == channel_id {
                removed.push(self.entries.remove(index));
            } else {
                index += 1;
            }
        }
        removed
    }
}
