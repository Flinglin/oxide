use std::fmt;

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::{X11AuthMaterial, X11AuthProtocol, X11Display, X11ForwardPolicy};

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct X11ForwardConfig {
    pub local_display: X11Display,
    pub remote_display: u16,
    pub single_connection: bool,
    pub policy: X11ForwardPolicy,
}

impl fmt::Debug for X11ForwardConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The device-local display is runtime context and must not enter diagnostics.
        formatter
            .debug_struct("X11ForwardConfig")
            .field("local_display", &"<device-local display>")
            .field("remote_display", &self.remote_display)
            .field("single_connection", &self.single_connection)
            .field("policy", &self.policy)
            .finish()
    }
}

impl X11ForwardConfig {
    pub fn new(local_display: X11Display) -> Self {
        Self {
            local_display,
            remote_display: 10,
            single_connection: false,
            policy: X11ForwardPolicy::default(),
        }
    }

    pub fn remote_display_value(&self) -> String {
        self.local_display.remote_display_value(self.remote_display)
    }

    pub fn with_policy(mut self, policy: X11ForwardPolicy) -> Self {
        self.policy = policy;
        self
    }

    pub fn ssh_request(&self, auth: &X11AuthMaterial) -> X11SshRequest {
        X11SshRequest {
            single_connection: self.single_connection,
            auth_protocol: auth.protocol,
            auth_cookie_hex: Zeroizing::new(auth.ssh_auth_cookie()),
            screen_number: self.local_display.screen as u32,
        }
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct X11SshRequest {
    pub single_connection: bool,
    pub auth_protocol: X11AuthProtocol,
    pub auth_cookie_hex: Zeroizing<String>,
    pub screen_number: u32,
}

impl X11SshRequest {
    pub fn auth_protocol_name(&self) -> &'static str {
        self.auth_protocol.ssh_name()
    }
}

impl fmt::Debug for X11SshRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("X11SshRequest")
            .field("single_connection", &self.single_connection)
            .field("auth_protocol", &self.auth_protocol)
            .field("auth_cookie_hex", &"<redacted>")
            .field("screen_number", &self.screen_number)
            .finish()
    }
}
