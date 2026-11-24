use std::collections::HashMap;

use super::model::{ProjectProbeError, ProjectProbeKey, ProjectProbeOutcome, ProjectSnapshot};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProjectProbeState {
    Unknown,
    Loading,
    Ready,
    NoProject,
    CwdUnavailable,
    Error(ProjectProbeError),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectProbeEntry {
    state: ProjectProbeState,
    snapshot: Option<ProjectSnapshot>,
    generation: u64,
    updated_at_ms: u64,
}

impl ProjectProbeEntry {
    pub fn state(&self) -> &ProjectProbeState {
        &self.state
    }

    pub fn snapshot(&self) -> Option<&ProjectSnapshot> {
        self.snapshot.as_ref()
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn updated_at_ms(&self) -> u64 {
        self.updated_at_ms
    }
}

#[derive(Default)]
pub struct ProjectStatusStore {
    entries: HashMap<ProjectProbeKey, ProjectProbeEntry>,
    next_generation: u64,
}

impl ProjectStatusStore {
    pub fn get(&self, key: &ProjectProbeKey) -> Option<&ProjectProbeEntry> {
        self.entries.get(key)
    }

    pub fn snapshot(&self, key: &ProjectProbeKey) -> Option<&ProjectSnapshot> {
        self.get(key).and_then(ProjectProbeEntry::snapshot)
    }

    pub fn should_probe(&self, key: &ProjectProbeKey, now_ms: u64, ttl_ms: u64) -> bool {
        match self.entries.get(key) {
            None => true,
            Some(entry) if matches!(entry.state, ProjectProbeState::Loading) => false,
            Some(entry) => now_ms.saturating_sub(entry.updated_at_ms) >= ttl_ms,
        }
    }

    pub fn mark_loading(&mut self, key: ProjectProbeKey, now_ms: u64) -> u64 {
        self.next_generation = self.next_generation.saturating_add(1);
        let generation = self.next_generation;
        self.entries
            .entry(key)
            .and_modify(|entry| {
                entry.state = ProjectProbeState::Loading;
                entry.generation = generation;
                entry.updated_at_ms = now_ms;
            })
            .or_insert(ProjectProbeEntry {
                state: ProjectProbeState::Loading,
                snapshot: None,
                generation,
                updated_at_ms: now_ms,
            });
        generation
    }

    pub fn finish_probe(
        &mut self,
        key: &ProjectProbeKey,
        generation: u64,
        outcome: ProjectProbeOutcome,
        now_ms: u64,
    ) -> bool {
        let Some(entry) = self.entries.get_mut(key) else {
            return false;
        };
        if entry.generation != generation {
            return false;
        }

        match outcome {
            ProjectProbeOutcome::Ready(snapshot) => {
                entry.state = ProjectProbeState::Ready;
                entry.snapshot = Some(snapshot);
            }
            ProjectProbeOutcome::NoProject => {
                entry.state = ProjectProbeState::NoProject;
                entry.snapshot = None;
            }
            ProjectProbeOutcome::CwdMissing => {
                entry.state = ProjectProbeState::CwdUnavailable;
                entry.snapshot = None;
            }
            ProjectProbeOutcome::Error(error) => {
                entry.state = ProjectProbeState::Error(error);
            }
        }
        entry.updated_at_ms = now_ms;
        true
    }

    pub fn retain_keys(&mut self, keep: impl Fn(&ProjectProbeKey) -> bool) {
        self.entries.retain(|key, _| keep(key));
    }
}
