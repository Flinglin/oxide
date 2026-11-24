use std::collections::{HashMap, HashSet};

use super::PaneId;

// None identifies the temporary group; saved UUIDs retain their existing on-disk meaning.
pub(super) type SyncGroupId = Option<uuid::Uuid>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct SyncMember {
    pub group: SyncGroupId,
    pub isolated: bool,
}

/// Runtime membership owns no transport, terminal entity, input buffer, or background task.
#[derive(Default)]
pub(super) struct TerminalSyncGroups {
    enabled: HashMap<SyncGroupId, bool>,
    members: HashMap<PaneId, SyncMember>,
}

impl TerminalSyncGroups {
    pub fn initialize(&mut self, group: SyncGroupId, candidates: &[PaneId]) {
        if self.enabled.contains_key(&group) {
            return;
        }
        self.enabled.insert(group, false);
        for pane in candidates {
            self.add(group, *pane);
        }
    }

    pub fn member(&self, pane: PaneId) -> Option<SyncMember> {
        self.members.get(&pane).copied()
    }

    pub fn enabled(&self, group: SyncGroupId) -> bool {
        self.enabled.get(&group).copied().unwrap_or(false)
    }

    pub fn add(&mut self, group: SyncGroupId, pane: PaneId) -> bool {
        // Joining another group requires explicitly leaving the previous one.
        if self.members.contains_key(&pane) {
            return false;
        }
        self.enabled.entry(group).or_insert(false);
        self.members.insert(
            pane,
            SyncMember {
                group,
                isolated: false,
            },
        );
        true
    }

    pub fn remove(&mut self, pane: PaneId) {
        if let Some(member) = self.members.remove(&pane) {
            if !self
                .members
                .values()
                .any(|other| other.group == member.group)
            {
                self.enabled.insert(member.group, false);
            }
        }
    }

    pub fn retain_groups(&mut self, saved: &HashSet<uuid::Uuid>) {
        self.members
            .retain(|_, member| member.group.is_none_or(|id| saved.contains(&id)));
        self.enabled
            .retain(|group, _| group.is_none_or(|id| saved.contains(&id)));
    }

    pub fn retain_panes(&mut self, live: &HashSet<PaneId>) {
        let closed: Vec<_> = self
            .members
            .keys()
            .filter(|pane| !live.contains(pane))
            .copied()
            .collect();
        for pane in closed {
            self.remove(pane);
        }
    }

    pub fn toggle_enabled(&mut self, group: SyncGroupId) {
        if self.members.values().any(|member| member.group == group) {
            let enabled = self.enabled.entry(group).or_insert(false);
            *enabled = !*enabled;
        }
    }

    pub fn toggle_isolated(&mut self, pane: PaneId) {
        if let Some(member) = self.members.get_mut(&pane) {
            member.isolated = !member.isolated;
        }
    }

    pub fn remount(&mut self, old: PaneId, new: PaneId) {
        if let Some(mut member) = self.members.remove(&old) {
            // A reconnected shell may no longer share the group's editor/cursor state.
            member.isolated = true;
            self.members.insert(new, member);
        }
    }

    pub fn panes(&self, group: SyncGroupId) -> Vec<PaneId> {
        let mut panes: Vec<_> = self
            .members
            .iter()
            .filter(|(_, member)| member.group == group)
            .map(|(pane, _)| *pane)
            .collect();
        panes.sort_by_key(|pane| pane.0);
        panes
    }

    pub fn targets(&self, source: PaneId) -> Vec<PaneId> {
        let Some(source_member) = self.member(source) else {
            return Vec::new();
        };
        if source_member.isolated || !self.enabled(source_member.group) {
            return Vec::new();
        }
        self.panes(source_member.group)
            .into_iter()
            .filter(|pane| {
                *pane != source && self.member(*pane).is_some_and(|member| !member.isolated)
            })
            .collect()
    }
}
