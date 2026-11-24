use std::{
    collections::{BTreeMap, HashMap},
    fmt,
    time::{Duration, SystemTime},
};

use dashmap::DashMap;
use oxide_audit::{
    AuditCategory, AuditContext, AuditEvidence, AuditOperation, AuditOutcome, AuditSource,
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const GRACE_PERIOD: Duration = Duration::from_secs(30);
pub const PROACTIVE_KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(5);
pub const WEBSOCKET_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
pub const WEBSOCKET_HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(300);
pub const SSH_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);
pub const MAX_RETAINED_RECONNECT_JOBS: usize = 200;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReconnectPhase {
    Queued,
    Snapshot,
    GracePeriod,
    SshConnect,
    AwaitTerminal,
    RestoreForwards,
    ResumeTransfers,
    RestoreIde,
    Verify,
    Done,
    Failed,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhaseResult {
    Running,
    Ok,
    Failed,
    Skipped,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PhaseEvent {
    pub phase: ReconnectPhase,
    pub started_at: SystemTime,
    pub ended_at: Option<SystemTime>,
    pub result: PhaseResult,
    pub detail: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconnectSnapshot {
    pub node_id: String,
    pub terminal_pane_ids: Vec<String>,
    pub old_terminal_session_ids: Vec<String>,
    pub terminal_sessions_by_node: Vec<ReconnectNodeTerminalSnapshot>,
    pub forward_rules: Vec<ReconnectForwardRuleSnapshot>,
    /// Legacy summary retained for existing diagnostics. Reconnect restore uses
    /// `forward_rules`, matching Tauri's rule snapshot semantics.
    pub active_port_forward_ids: Vec<String>,
    pub inflight_sftp_transfer_ids: Vec<String>,
    pub incomplete_sftp_transfers_by_node: Vec<ReconnectNodeTransferSnapshot>,
    pub ide_snapshot: Option<ReconnectIdeSnapshot>,
    /// Tauri keeps oldConnectionIds as a nodeId -> connectionId map. Native
    /// retains the legacy flat list below for diagnostics, but reconnect
    /// grace-period recovery uses this node-scoped map.
    pub old_connections_by_node: Vec<ReconnectNodeConnectionSnapshot>,
    pub old_connection_ids: Vec<String>,
    pub snapshot_at: Option<SystemTime>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconnectIdeFileFormat {
    pub encoding: String,
    pub has_bom: bool,
    pub line_ending: String,
}

#[derive(Clone, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconnectIdeSnapshot {
    pub project_path: String,
    pub tab_paths: Vec<String>,
    /// Tauri stores nodeId in the ideSnapshot.connectionId slot during
    /// reconnect; keep the field name for parity with its restore phase.
    pub connection_id: String,
    pub dirty_contents: BTreeMap<String, String>,
    #[serde(default)]
    pub file_formats: BTreeMap<String, ReconnectIdeFileFormat>,
}

impl fmt::Debug for ReconnectIdeSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReconnectIdeSnapshot")
            .field("project_path", &"[redacted path]")
            .field("tab_count", &self.tab_paths.len())
            .field("connection_id", &self.connection_id)
            .field("dirty_file_count", &self.dirty_contents.len())
            .finish()
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconnectNodeTerminalSnapshot {
    pub node_id: String,
    pub old_terminal_session_ids: Vec<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconnectForwardRuleSnapshot {
    pub node_id: String,
    pub rules: Vec<ReconnectForwardRule>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconnectForwardRule {
    pub id: String,
    pub forward_type: String,
    pub bind_address: String,
    pub bind_port: u16,
    pub target_host: String,
    pub target_port: u16,
    pub status: String,
    pub description: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconnectNodeTransferSnapshot {
    pub node_id: String,
    pub transfer_ids: Vec<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconnectNodeConnectionSnapshot {
    pub node_id: String,
    pub old_connection_id: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReconnectTiming {
    pub grace_period: Duration,
    pub retry_base_delay: Duration,
    pub retry_max_delay: Duration,
    pub proactive_keepalive_timeout: Duration,
    pub websocket_heartbeat_interval: Duration,
    pub websocket_heartbeat_timeout: Duration,
    pub ssh_keepalive_interval: Duration,
}

impl Default for ReconnectTiming {
    fn default() -> Self {
        Self {
            grace_period: GRACE_PERIOD,
            retry_base_delay: Duration::from_secs(1),
            retry_max_delay: Duration::from_secs(15),
            proactive_keepalive_timeout: PROACTIVE_KEEPALIVE_TIMEOUT,
            websocket_heartbeat_interval: WEBSOCKET_HEARTBEAT_INTERVAL,
            websocket_heartbeat_timeout: WEBSOCKET_HEARTBEAT_TIMEOUT,
            ssh_keepalive_interval: SSH_KEEPALIVE_INTERVAL,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconnectJob {
    pub job_id: String,
    pub node_id: String,
    pub node_name: String,
    pub status: ReconnectPhase,
    pub attempt: u32,
    pub max_attempts: u32,
    pub started_at: SystemTime,
    pub ended_at: Option<SystemTime>,
    pub error: Option<String>,
    pub snapshot: ReconnectSnapshot,
    pub restored_count: u32,
    pub phase_history: Vec<PhaseEvent>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReconnectRetry {
    pub attempt: u32,
    pub max_attempts: u32,
    pub delay: Duration,
}

/// Lightweight reconnect state for frequently rendered progress surfaces.
#[derive(Clone, Debug)]
pub struct ReconnectProgress {
    pub status: ReconnectPhase,
    pub attempt: u32,
    pub max_attempts: u32,
    pub phase_history: Vec<PhaseEvent>,
}

/// The reconnect data needed to rebuild forwarding without cloning unrelated snapshots.
#[derive(Clone, Debug)]
pub struct ReconnectForwardRestorePlan {
    pub job_id: String,
    pub forward_rules: Vec<ReconnectForwardRuleSnapshot>,
    pub old_connections_by_node: Vec<ReconnectNodeConnectionSnapshot>,
}

#[derive(Debug)]
pub struct ReconnectOrchestratorStore {
    jobs: DashMap<String, ReconnectJob>,
    audit_contexts: DashMap<String, AuditContext>,
    audit_jobs: Mutex<HashMap<String, AuditOperation>>,
    audit_phases: Mutex<HashMap<String, AuditOperation>>,
    timing: ReconnectTiming,
    max_attempts: u32,
}

impl ReconnectOrchestratorStore {
    pub fn new(timing: ReconnectTiming, max_attempts: u32) -> Self {
        Self {
            jobs: DashMap::new(),
            audit_contexts: DashMap::new(),
            audit_jobs: Mutex::new(HashMap::new()),
            audit_phases: Mutex::new(HashMap::new()),
            timing,
            max_attempts: max_attempts.max(1),
        }
    }

    pub fn configure(&mut self, timing: ReconnectTiming, max_attempts: u32) {
        self.timing = timing;
        self.max_attempts = max_attempts.max(1);
    }

    pub fn schedule(
        &self,
        node_id: impl Into<String>,
        node_name: impl Into<String>,
        mut snapshot: ReconnectSnapshot,
    ) -> ReconnectJob {
        let node_id = node_id.into();
        snapshot.node_id = node_id.clone();
        snapshot.snapshot_at = Some(SystemTime::now());

        if let Some(job) = self.jobs.get(&node_id) {
            if job.ended_at.is_none() {
                return job.clone();
            }
            drop(job);
            self.jobs.remove(&node_id);
        }

        let mut job = ReconnectJob {
            job_id: Uuid::new_v4().to_string(),
            node_id: node_id.clone(),
            node_name: node_name.into(),
            status: ReconnectPhase::Queued,
            attempt: 0,
            max_attempts: self.max_attempts,
            started_at: SystemTime::now(),
            ended_at: None,
            error: None,
            snapshot,
            restored_count: 0,
            phase_history: Vec::new(),
        };
        Self::push_phase(&mut job, ReconnectPhase::Queued, PhaseResult::Running, None);
        self.jobs.insert(node_id, job.clone());
        job
    }

    pub fn schedule_audited(
        &self,
        node_id: impl Into<String>,
        node_name: impl Into<String>,
        snapshot: ReconnectSnapshot,
        audit_context: Option<AuditContext>,
    ) -> ReconnectJob {
        let node_id = node_id.into();
        let active = self.is_active(&node_id);
        let job = self.schedule(node_id.clone(), node_name, snapshot);
        if !active {
            if let Some(mut context) = audit_context {
                context.source = AuditSource::System;
                let operation = context.operation(
                    AuditCategory::Connection,
                    "ssh_reconnect",
                    Some(&format!("job={}", job.job_id)),
                );
                context.parent_id = operation.id().map(str::to_owned);
                self.audit_contexts.insert(node_id.clone(), context);
                self.audit_jobs.lock().insert(node_id.clone(), operation);
                self.begin_audit_phase(&node_id, &job);
            }
        }
        job
    }

    fn begin_audit_phase(&self, node_id: &str, job: &ReconnectJob) {
        if let Some(context) = self.audit_contexts.get(node_id) {
            let operation = context.operation(
                AuditCategory::Connection,
                "ssh_reconnect_phase",
                Some(&format!(
                    "job={} phase={:?} attempt={}",
                    job.job_id, job.status, job.attempt
                )),
            );
            self.audit_phases
                .lock()
                .insert(node_id.to_string(), operation);
        }
    }

    fn finish_audit_phase(&self, node_id: &str, outcome: AuditOutcome) {
        if let Some(operation) = self.audit_phases.lock().remove(node_id) {
            operation.finish(outcome, AuditEvidence::Lifecycle, None, None);
        }
    }

    pub fn advance(&self, node_id: &str, phase: ReconnectPhase) -> Option<ReconnectJob> {
        self.finish_audit_phase(node_id, AuditOutcome::Unknown);
        let mut job = self.jobs.get_mut(node_id)?;
        job.status = phase.clone();
        Self::push_phase(&mut job, phase, PhaseResult::Running, None);
        let updated = job.clone();
        drop(job);
        self.begin_audit_phase(node_id, &updated);
        Some(updated)
    }

    pub fn complete_phase(
        &self,
        node_id: &str,
        result: PhaseResult,
        detail: Option<String>,
    ) -> Option<ReconnectJob> {
        let mut job = self.jobs.get_mut(node_id)?;
        if let Some(event) = job.phase_history.last_mut() {
            event.ended_at = Some(SystemTime::now());
            event.result = result;
            event.detail = detail;
        }
        let outcome = match result {
            PhaseResult::Ok => Some(AuditOutcome::Succeeded),
            PhaseResult::Failed => Some(AuditOutcome::Failed),
            PhaseResult::Skipped => Some(AuditOutcome::Unchanged),
            PhaseResult::Running => None,
        };
        if let Some(outcome) = outcome {
            self.finish_audit_phase(node_id, outcome);
        }
        Some(job.clone())
    }

    pub fn finish(&self, node_id: &str, result: Result<u32, String>) -> Option<ReconnectJob> {
        self.finish_audit_phase(
            node_id,
            if result.is_ok() {
                AuditOutcome::Succeeded
            } else {
                AuditOutcome::Failed
            },
        );
        let mut job = self.jobs.get_mut(node_id)?;
        job.ended_at = Some(SystemTime::now());
        match result {
            Ok(restored_count) => {
                job.status = ReconnectPhase::Done;
                job.restored_count = restored_count;
                Self::push_phase(&mut job, ReconnectPhase::Done, PhaseResult::Ok, None);
            }
            Err(error) => {
                job.status = ReconnectPhase::Failed;
                job.error = Some(error.clone());
                Self::push_phase(
                    &mut job,
                    ReconnectPhase::Failed,
                    PhaseResult::Failed,
                    Some(error),
                );
            }
        }
        let finished = job.clone();
        drop(job);
        self.begin_audit_phase(node_id, &finished);
        self.finish_audit_phase(
            node_id,
            if finished.status == ReconnectPhase::Done {
                AuditOutcome::Succeeded
            } else {
                AuditOutcome::Failed
            },
        );
        if let Some(operation) = self.audit_jobs.lock().remove(node_id) {
            operation.finish(
                if finished.status == ReconnectPhase::Done {
                    AuditOutcome::Succeeded
                } else {
                    AuditOutcome::Failed
                },
                AuditEvidence::Lifecycle,
                None,
                None,
            );
        }
        self.audit_contexts.remove(node_id);
        Some(finished)
    }

    pub fn cleanup_terminal_job(&self, node_id: &str, started_at: SystemTime) -> bool {
        let should_remove = self
            .jobs
            .get(node_id)
            .is_some_and(|job| is_terminal_phase(&job.status) && job.started_at == started_at);
        if should_remove {
            self.jobs.remove(node_id);
            self.audit_contexts.remove(node_id);
            self.audit_phases.lock().remove(node_id);
            self.audit_jobs.lock().remove(node_id);
        }
        should_remove
    }

    pub fn enforce_terminal_job_cap(&self, max_retained: usize) {
        let mut terminal_jobs = self
            .jobs
            .iter()
            .filter(|entry| is_terminal_phase(&entry.status))
            .map(|entry| {
                (
                    entry.key().clone(),
                    entry.ended_at.unwrap_or(entry.started_at),
                    entry.started_at,
                )
            })
            .collect::<Vec<_>>();
        if terminal_jobs.len() <= max_retained {
            return;
        }
        terminal_jobs.sort_by_key(|(_, ended_at, started_at)| (*ended_at, *started_at));
        let to_remove = terminal_jobs.len() - max_retained;
        for (node_id, _, _) in terminal_jobs.into_iter().take(to_remove) {
            self.jobs.remove(&node_id);
            self.audit_contexts.remove(&node_id);
            self.audit_phases.lock().remove(&node_id);
            self.audit_jobs.lock().remove(&node_id);
        }
    }

    pub fn cancel(&self, node_id: &str) -> Option<ReconnectJob> {
        self.finish_audit_phase(node_id, AuditOutcome::Interrupted);
        let mut job = self.jobs.get_mut(node_id)?;
        job.status = ReconnectPhase::Cancelled;
        job.ended_at = Some(SystemTime::now());
        Self::push_phase(&mut job, ReconnectPhase::Cancelled, PhaseResult::Ok, None);
        let cancelled = job.clone();
        drop(job);
        self.begin_audit_phase(node_id, &cancelled);
        self.finish_audit_phase(node_id, AuditOutcome::Interrupted);
        if let Some(operation) = self.audit_jobs.lock().remove(node_id) {
            operation.finish(
                AuditOutcome::Interrupted,
                AuditEvidence::Lifecycle,
                None,
                None,
            );
        }
        self.audit_contexts.remove(node_id);
        Some(cancelled)
    }

    pub fn job(&self, node_id: &str) -> Option<ReconnectJob> {
        self.jobs.get(node_id).map(|job| job.clone())
    }

    pub fn is_active(&self, node_id: &str) -> bool {
        self.jobs
            .get(node_id)
            .is_some_and(|job| job.ended_at.is_none())
    }

    pub fn is_current(&self, node_id: &str, job_id: &str) -> bool {
        self.jobs
            .get(node_id)
            .is_some_and(|job| job.ended_at.is_none() && job.job_id == job_id)
    }

    pub fn active_node_ids(&self) -> Vec<String> {
        self.jobs
            .iter()
            .filter(|job| job.ended_at.is_none())
            .map(|job| job.node_id.clone())
            .collect()
    }

    pub fn active_attempt(&self, node_id: &str) -> Option<(u32, u32)> {
        // Keep retry scheduling from cloning terminal, transfer, IDE, and forwarding snapshots.
        self.jobs.get(node_id).and_then(|job| {
            job.ended_at
                .is_none()
                .then_some((job.attempt, job.max_attempts))
        })
    }

    pub fn active_job_id(&self, node_id: &str) -> Option<String> {
        self.jobs
            .get(node_id)
            .and_then(|job| job.ended_at.is_none().then(|| job.job_id.clone()))
    }

    pub fn active_progress(&self, node_id: &str) -> Option<ReconnectProgress> {
        // Renderers must never clone the full job because it can contain unsaved IDE contents.
        self.jobs.get(node_id).and_then(|job| {
            job.ended_at.is_none().then(|| ReconnectProgress {
                status: job.status.clone(),
                attempt: job.attempt,
                max_attempts: job.max_attempts,
                phase_history: job.phase_history.clone(),
            })
        })
    }

    pub fn has_forward_snapshot(&self, node_id: &str) -> bool {
        self.jobs
            .get(node_id)
            .is_some_and(|job| !job.snapshot.forward_rules.is_empty())
    }

    pub fn terminal_session_ids_for_node(&self, node_id: &str) -> Vec<String> {
        // Prefer the node-scoped snapshot while retaining the legacy fallback.
        self.jobs
            .get(node_id)
            .map(|job| {
                job.snapshot
                    .terminal_sessions_by_node
                    .iter()
                    .find(|entry| entry.node_id == node_id)
                    .map(|entry| entry.old_terminal_session_ids.clone())
                    .unwrap_or_else(|| job.snapshot.old_terminal_session_ids.clone())
            })
            .unwrap_or_default()
    }

    pub fn incomplete_sftp_transfers(&self, node_id: &str) -> Vec<ReconnectNodeTransferSnapshot> {
        self.jobs
            .get(node_id)
            .map(|job| job.snapshot.incomplete_sftp_transfers_by_node.clone())
            .unwrap_or_default()
    }

    pub fn ide_snapshot(
        &self,
        node_id: &str,
    ) -> Option<(ReconnectIdeSnapshot, Option<SystemTime>)> {
        // The timestamp preserves the user-close guard without copying unrelated restore state.
        self.jobs.get(node_id).and_then(|job| {
            job.snapshot
                .ide_snapshot
                .clone()
                .map(|snapshot| (snapshot, job.snapshot.snapshot_at))
        })
    }

    pub fn forward_restore_plan(&self, node_id: &str) -> Option<ReconnectForwardRestorePlan> {
        self.jobs
            .get(node_id)
            .map(|job| ReconnectForwardRestorePlan {
                job_id: job.job_id.clone(),
                forward_rules: job.snapshot.forward_rules.clone(),
                old_connections_by_node: job.snapshot.old_connections_by_node.clone(),
            })
    }

    pub fn forward_rule_snapshots(&self, node_id: &str) -> Vec<ReconnectForwardRuleSnapshot> {
        self.jobs
            .get(node_id)
            .map(|job| job.snapshot.forward_rules.clone())
            .unwrap_or_default()
    }

    pub fn update_snapshot<F>(&self, node_id: &str, update: F) -> Option<ReconnectJob>
    where
        F: FnOnce(&mut ReconnectSnapshot),
    {
        let mut job = self.jobs.get_mut(node_id)?;
        update(&mut job.snapshot);
        Some(job.clone())
    }

    pub fn jobs(&self) -> Vec<ReconnectJob> {
        self.jobs.iter().map(|job| job.clone()).collect()
    }

    pub fn timing(&self) -> ReconnectTiming {
        self.timing
    }

    pub fn retry_delay_for_attempt(&self, attempt: u32) -> Duration {
        retry_delay(self.timing, attempt)
    }

    pub fn begin_ssh_attempt(&self, node_id: &str) -> Option<ReconnectJob> {
        let mut job = self.jobs.get_mut(node_id)?;
        if job.ended_at.is_some() {
            return None;
        }
        job.attempt = job.attempt.saturating_add(1).min(job.max_attempts);
        Some(job.clone())
    }

    pub fn schedule_retry(&self, node_id: &str) -> Option<ReconnectRetry> {
        let mut job = self.jobs.get_mut(node_id)?;
        if job.ended_at.is_some() || job.attempt >= job.max_attempts {
            return None;
        }
        self.finish_audit_phase(node_id, AuditOutcome::Unknown);
        let next_attempt = job.attempt + 1;
        let max_attempts = job.max_attempts;
        let delay = retry_delay(self.timing, job.attempt);
        job.status = ReconnectPhase::Queued;
        Self::push_phase(
            &mut job,
            ReconnectPhase::Queued,
            PhaseResult::Running,
            Some(format!(
                "retry {next_attempt}/{max_attempts} in {:?}",
                delay
            )),
        );
        let updated = job.clone();
        drop(job);
        self.begin_audit_phase(node_id, &updated);
        Some(ReconnectRetry {
            attempt: next_attempt,
            max_attempts,
            delay,
        })
    }

    fn push_phase(
        job: &mut ReconnectJob,
        phase: ReconnectPhase,
        result: PhaseResult,
        detail: Option<String>,
    ) {
        job.phase_history.push(PhaseEvent {
            phase,
            started_at: SystemTime::now(),
            ended_at: (result != PhaseResult::Running).then(SystemTime::now),
            result,
            detail,
        });
        if job.phase_history.len() > 64 {
            job.phase_history.remove(0);
        }
    }
}

fn is_terminal_phase(phase: &ReconnectPhase) -> bool {
    matches!(
        phase,
        ReconnectPhase::Done | ReconnectPhase::Failed | ReconnectPhase::Cancelled
    )
}

fn retry_delay(timing: ReconnectTiming, attempt: u32) -> Duration {
    retry_delay_with_jitter(timing, attempt, retry_jitter_factor())
}

fn retry_delay_with_jitter(timing: ReconnectTiming, attempt: u32, jitter_factor: f64) -> Duration {
    const BACKOFF_MULTIPLIER: f64 = 1.5;
    let exponent = attempt.saturating_sub(1) as i32;
    let base_ms = timing.retry_base_delay.as_millis() as f64;
    let max_ms = timing.retry_max_delay.as_millis() as f64;
    let delay_ms = (base_ms * BACKOFF_MULTIPLIER.powi(exponent)).min(max_ms);
    Duration::from_millis((delay_ms * jitter_factor).round().max(1.0) as u64)
}

fn retry_jitter_factor() -> f64 {
    let sample = (Uuid::new_v4().as_u128() & 0xffff) as f64 / 65_535.0;
    0.8 + sample * 0.4
}

impl Default for ReconnectOrchestratorStore {
    fn default() -> Self {
        Self::new(ReconnectTiming::default(), 5)
    }
}
