use std::{
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicU8, AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use parking_lot::RwLock;
use std::collections::HashMap;
use tokio::sync::{Notify, OnceCell, Semaphore, watch};

use serde::{Deserialize, Serialize};

use crate::{
    ScpCapabilities, SftpError, SftpExecChannelOpener, TarCapabilities, TransferProtocol,
    TransferStrategy, probe_scp_capabilities, probe_tar_capabilities,
};

pub const DEFAULT_SFTP_CONCURRENT_TRANSFERS: usize = 3;
pub const DEFAULT_SFTP_DIRECTORY_PARALLELISM: usize = 4;
pub const MAX_SFTP_CONCURRENT_TRANSFERS: usize = 10;
pub const MAX_SFTP_DIRECTORY_PARALLELISM: usize = 16;
const FINISHED_BACKGROUND_TRANSFER_RETENTION_MS: u64 = 5 * 60 * 1000;
const TRANSFER_MANAGER_RUNNING: u8 = 0;
const TRANSFER_MANAGER_SHUTTING_DOWN: u8 = 1;
const TRANSFER_MANAGER_STOPPED: u8 = 2;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundTransferDirection {
    Upload,
    Download,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundTransferKind {
    File,
    Directory,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundTransferState {
    Pending,
    Active,
    Paused,
    Completed,
    Cancelled,
    Error,
}

impl BackgroundTransferState {
    fn is_finished(self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled | Self::Error)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundTransferSnapshot {
    pub id: String,
    pub node_id: String,
    pub name: String,
    pub local_path: String,
    pub remote_path: String,
    pub direction: BackgroundTransferDirection,
    pub kind: BackgroundTransferKind,
    pub protocol: TransferProtocol,
    pub strategy: TransferStrategy,
    pub state: BackgroundTransferState,
    pub size: u64,
    pub transferred: u64,
    pub backend_speed: Option<u64>,
    pub error: Option<String>,
    pub start_time: u64,
    pub end_time: Option<u64>,
    pub item_count: Option<u64>,
}

impl BackgroundTransferSnapshot {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: String,
        node_id: String,
        name: String,
        local_path: String,
        remote_path: String,
        direction: BackgroundTransferDirection,
        kind: BackgroundTransferKind,
        strategy: TransferStrategy,
        size: u64,
        transferred: u64,
    ) -> Self {
        Self {
            id,
            node_id,
            name,
            local_path,
            remote_path,
            direction,
            kind,
            protocol: TransferProtocol::Sftp,
            strategy,
            state: BackgroundTransferState::Pending,
            size,
            transferred,
            backend_speed: None,
            error: None,
            start_time: now_ms(),
            end_time: None,
            item_count: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SftpTransferRuntimeSettings {
    pub max_concurrent_transfers: usize,
    pub speed_limit_kbps: usize,
    pub directory_parallelism: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SftpTransferStats {
    pub active: usize,
    pub queued: usize,
    pub completed: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SftpTransferShutdownReport {
    pub started: bool,
    pub drained: bool,
    pub cancelled_transfers: usize,
    pub remaining_transfers: usize,
}

impl Default for SftpTransferRuntimeSettings {
    fn default() -> Self {
        Self {
            max_concurrent_transfers: DEFAULT_SFTP_CONCURRENT_TRANSFERS,
            speed_limit_kbps: 0,
            directory_parallelism: DEFAULT_SFTP_DIRECTORY_PARALLELISM,
        }
    }
}

#[derive(Debug)]
pub struct SftpTransferPermit {
    _permit: tokio::sync::OwnedSemaphorePermit,
    active_count: Arc<AtomicUsize>,
    availability_notify: Arc<Notify>,
}

impl Drop for SftpTransferPermit {
    fn drop(&mut self) {
        let _ = self
            .active_count
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                Some(count.saturating_sub(1))
            });
        self.availability_notify.notify_waiters();
    }
}

#[derive(Debug)]
pub struct SftpTransferControl {
    cancel_tx: watch::Sender<bool>,
    cancel_rx: watch::Receiver<bool>,
    pause_tx: watch::Sender<bool>,
    pause_rx: watch::Receiver<bool>,
    interrupt_tx: watch::Sender<Option<String>>,
    interrupt_rx: watch::Receiver<Option<String>>,
}

impl SftpTransferControl {
    pub fn new() -> Self {
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let (pause_tx, pause_rx) = watch::channel(false);
        let (interrupt_tx, interrupt_rx) = watch::channel(None);
        Self {
            cancel_tx,
            cancel_rx,
            pause_tx,
            pause_rx,
            interrupt_tx,
            interrupt_rx,
        }
    }

    pub fn is_cancelled(&self) -> bool {
        *self.cancel_rx.borrow()
    }

    pub fn is_paused(&self) -> bool {
        *self.pause_rx.borrow()
    }

    pub fn interrupt_reason(&self) -> Option<String> {
        self.interrupt_rx.borrow().clone()
    }

    pub fn cancel(&self) {
        let _ = self.cancel_tx.send(true);
    }

    pub fn pause(&self) {
        let _ = self.pause_tx.send(true);
    }

    pub fn resume(&self) {
        let _ = self.pause_tx.send(false);
    }

    pub fn interrupt(&self, reason: impl Into<String>) {
        let _ = self.interrupt_tx.send(Some(reason.into()));
    }

    pub fn subscribe_cancellation(&self) -> watch::Receiver<bool> {
        self.cancel_rx.clone()
    }

    pub fn subscribe_pause(&self) -> watch::Receiver<bool> {
        self.pause_rx.clone()
    }
}

impl Default for SftpTransferControl {
    fn default() -> Self {
        Self::new()
    }
}

pub struct SftpTransferGuard {
    manager: Option<Arc<SftpTransferManager>>,
    transfer_id: String,
}

impl SftpTransferGuard {
    pub fn new(manager: Option<&Arc<SftpTransferManager>>, transfer_id: impl Into<String>) -> Self {
        Self {
            manager: manager.cloned(),
            transfer_id: transfer_id.into(),
        }
    }
}

impl Drop for SftpTransferGuard {
    fn drop(&mut self) {
        if let Some(manager) = &self.manager {
            manager.unregister(&self.transfer_id);
        }
    }
}

#[derive(Debug)]
pub struct SftpTransferManager {
    semaphore: Arc<Semaphore>,
    controls: RwLock<HashMap<String, RegisteredTransferControl>>,
    active_count: Arc<AtomicUsize>,
    max_concurrent_transfers: AtomicUsize,
    directory_parallelism: AtomicUsize,
    speed_limit_bps: AtomicUsize,
    availability_notify: Arc<Notify>,
    shutdown_state: AtomicU8,
    shutdown_notify: Arc<Notify>,
    background_transfers: RwLock<HashMap<String, BackgroundTransferSnapshot>>,
    audit_transfers: RwLock<HashMap<String, AuditedTransfer>>,
    previous_attempts: RwLock<HashMap<String, String>>,
    background_notify: Arc<Notify>,
    background_state_changes: watch::Sender<()>,
    tar_capability_probes: RwLock<HashMap<String, Arc<OnceCell<TarCapabilities>>>>,
    scp_capability_probes: RwLock<HashMap<String, Arc<OnceCell<ScpCapabilities>>>>,
}

#[derive(Debug)]
struct RegisteredTransferControl {
    control: Arc<SftpTransferControl>,
    owner_count: usize,
    node_id: Option<String>,
}

struct AuditedTransfer {
    operation: oxide_audit::AuditOperation,
    context: oxide_audit::AuditContext,
    attempt_id: String,
    retry_of: Option<String>,
    stream_bytes: Option<u64>,
}

fn transfer_audit_detail(
    snapshot: &BackgroundTransferSnapshot,
    attempt_id: &str,
    retry_of: Option<&str>,
    stream_bytes: Option<u64>,
) -> String {
    let per_file = if snapshot.strategy == TransferStrategy::DirectoryTar {
        "unavailable:archive_stream"
    } else if snapshot.protocol == TransferProtocol::Scp
        && snapshot.kind == BackgroundTransferKind::Directory
    {
        "unavailable:batch_only"
    } else {
        "available"
    };
    let count_basis = if snapshot.protocol == TransferProtocol::Scp
        && snapshot.kind == BackgroundTransferKind::Directory
    {
        "entries_including_directories"
    } else if snapshot.kind == BackgroundTransferKind::Directory {
        "regular_files"
    } else {
        "single_file"
    };
    format!(
        "attempt={attempt_id}; retry_of={}; direction={:?}; kind={:?}; local={}; remote={}; protocol={:?}; strategy={:?}; per_file={per_file}; count_basis={count_basis}; completed_items={}; stream_bytes={}",
        retry_of.unwrap_or("none"),
        snapshot.direction,
        snapshot.kind,
        snapshot.local_path,
        snapshot.remote_path,
        snapshot.protocol,
        snapshot.strategy,
        snapshot
            .item_count
            .map_or_else(|| "unknown".to_owned(), |count| count.to_string()),
        stream_bytes.map_or_else(|| "unknown".to_owned(), |count| count.to_string()),
    )
}

impl std::fmt::Debug for AuditedTransfer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AuditedTransfer { .. }")
    }
}

impl SftpTransferManager {
    pub fn new() -> Self {
        Self {
            semaphore: Arc::new(Semaphore::new(MAX_SFTP_CONCURRENT_TRANSFERS)),
            controls: RwLock::new(HashMap::new()),
            active_count: Arc::new(AtomicUsize::new(0)),
            max_concurrent_transfers: AtomicUsize::new(DEFAULT_SFTP_CONCURRENT_TRANSFERS),
            directory_parallelism: AtomicUsize::new(DEFAULT_SFTP_DIRECTORY_PARALLELISM),
            speed_limit_bps: AtomicUsize::new(0),
            availability_notify: Arc::new(Notify::new()),
            shutdown_state: AtomicU8::new(TRANSFER_MANAGER_RUNNING),
            shutdown_notify: Arc::new(Notify::new()),
            background_transfers: RwLock::new(HashMap::new()),
            audit_transfers: RwLock::new(HashMap::new()),
            previous_attempts: RwLock::new(HashMap::new()),
            background_notify: Arc::new(Notify::new()),
            background_state_changes: watch::channel(()).0,
            tar_capability_probes: RwLock::new(HashMap::new()),
            scp_capability_probes: RwLock::new(HashMap::new()),
        }
    }

    /// Returns tar capabilities cached for one live SSH connection generation.
    pub async fn tar_capabilities<O>(&self, connection_id: &str, opener: &O) -> TarCapabilities
    where
        O: SftpExecChannelOpener,
    {
        self.cached_tar_capabilities(connection_id, || probe_tar_capabilities(opener))
            .await
    }

    async fn cached_tar_capabilities<F, Fut>(
        &self,
        connection_id: &str,
        probe: F,
    ) -> TarCapabilities
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = TarCapabilities>,
    {
        // A connection id identifies one live SSH generation. Reconnects use a
        // new key, so positive and negative capability results cannot leak into
        // the replacement transport for the same long-lived node.
        let probe_cell = if let Some(cell) = self
            .tar_capability_probes
            .read()
            .get(connection_id)
            .cloned()
        {
            cell
        } else {
            self.tar_capability_probes
                .write()
                .entry(connection_id.to_string())
                .or_insert_with(|| Arc::new(OnceCell::new()))
                .clone()
        };

        *probe_cell.get_or_init(probe).await
    }

    /// Returns SCP capabilities cached for one live SSH connection generation.
    pub async fn scp_capabilities<O>(&self, connection_id: &str, opener: &O) -> ScpCapabilities
    where
        O: SftpExecChannelOpener,
    {
        self.cached_scp_capabilities(connection_id, || probe_scp_capabilities(opener))
            .await
    }

    async fn cached_scp_capabilities<F, Fut>(
        &self,
        connection_id: &str,
        probe: F,
    ) -> ScpCapabilities
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = ScpCapabilities>,
    {
        // Connection generations isolate both positive and negative probes.
        let probe_cell = if let Some(cell) = self
            .scp_capability_probes
            .read()
            .get(connection_id)
            .cloned()
        {
            cell
        } else {
            self.scp_capability_probes
                .write()
                .entry(connection_id.to_string())
                .or_insert_with(|| Arc::new(OnceCell::new()))
                .clone()
        };
        *probe_cell.get_or_init(probe).await
    }

    fn cleanup_background_transfers(&self) {
        let now = now_ms();
        let mut transfers = self.background_transfers.write();
        transfers.retain(|_, snapshot| {
            !snapshot.state.is_finished()
                || snapshot
                    .end_time
                    .map(|end| now.saturating_sub(end) <= FINISHED_BACKGROUND_TRANSFER_RETENTION_MS)
                    .unwrap_or(true)
        });
        self.previous_attempts
            .write()
            .retain(|id, _| transfers.contains_key(id));
    }

    pub fn apply_settings(&self, settings: SftpTransferRuntimeSettings) {
        self.set_max_concurrent(settings.max_concurrent_transfers);
        self.set_speed_limit_kbps(settings.speed_limit_kbps);
        self.set_directory_parallelism(settings.directory_parallelism);
    }

    pub fn set_max_concurrent(&self, max: usize) {
        let clamped = max.clamp(1, MAX_SFTP_CONCURRENT_TRANSFERS);
        self.max_concurrent_transfers
            .store(clamped, Ordering::Release);
        self.availability_notify.notify_waiters();
    }

    pub fn max_concurrent(&self) -> usize {
        self.max_concurrent_transfers.load(Ordering::Acquire)
    }

    pub fn set_directory_parallelism(&self, parallelism: usize) {
        let clamped = parallelism.clamp(1, MAX_SFTP_DIRECTORY_PARALLELISM);
        self.directory_parallelism.store(clamped, Ordering::Release);
    }

    pub fn directory_parallelism(&self) -> usize {
        self.directory_parallelism.load(Ordering::Acquire)
    }

    pub fn set_speed_limit_kbps(&self, kbps: usize) {
        self.speed_limit_bps
            .store(kbps.saturating_mul(1024), Ordering::Release);
    }

    pub fn speed_limit_bps(&self) -> usize {
        self.speed_limit_bps.load(Ordering::Acquire)
    }

    pub fn active_count(&self) -> usize {
        self.active_count.load(Ordering::Acquire)
    }
    /// Observes task membership and state, without waking UI consumers for byte progress.
    pub fn subscribe_background_state_changes(&self) -> watch::Receiver<()> {
        self.background_state_changes.subscribe()
    }
    pub fn registered_count(&self) -> usize {
        self.controls.read().len()
    }

    pub fn transfer_stats(&self) -> SftpTransferStats {
        let active = self.active_count();
        let registered = self.registered_count();
        SftpTransferStats {
            active,
            queued: registered.saturating_sub(active),
            // Matches Tauri sftp_transfer_stats: completed is intentionally
            // reserved and currently not tracked by TransferManager.
            completed: 0,
        }
    }

    pub fn register(&self, transfer_id: &str) -> Arc<SftpTransferControl> {
        self.register_owned(transfer_id, None)
    }

    /// Registers the runtime node that owns a transfer before it enters the queue.
    pub fn register_for_node(
        &self,
        transfer_id: &str,
        node_id: impl Into<String>,
    ) -> Arc<SftpTransferControl> {
        self.register_owned(transfer_id, Some(node_id.into()))
    }

    fn register_owned(
        &self,
        transfer_id: &str,
        node_id: Option<String>,
    ) -> Arc<SftpTransferControl> {
        let mut controls = self.controls.write();
        if let Some(registered) = controls.get_mut(transfer_id) {
            // Nested transfer helpers share the task-level control so a queued
            // cancellation cannot be replaced when the data path starts.
            registered.owner_count += 1;
            if registered.node_id.is_none() {
                registered.node_id = node_id;
            }
            return registered.control.clone();
        }

        let control = Arc::new(SftpTransferControl::new());
        if self.shutdown_state.load(Ordering::Acquire) != TRANSFER_MANAGER_RUNNING {
            // A producer racing final session shutdown must not start new work.
            control.cancel();
        }
        controls.insert(
            transfer_id.to_string(),
            RegisteredTransferControl {
                control: control.clone(),
                owner_count: 1,
                node_id,
            },
        );
        control
    }

    pub fn get_control(&self, transfer_id: &str) -> Option<Arc<SftpTransferControl>> {
        self.controls
            .read()
            .get(transfer_id)
            .map(|registered| registered.control.clone())
    }

    pub fn unregister(&self, transfer_id: &str) {
        let mut controls = self.controls.write();
        let should_remove = controls.get_mut(transfer_id).is_some_and(|registered| {
            registered.owner_count = registered.owner_count.saturating_sub(1);
            registered.owner_count == 0
        });
        if should_remove {
            controls.remove(transfer_id);
            self.shutdown_notify.notify_waiters();
        }
    }

    /// Interrupts every queued or active transfer owned by one SSH node.
    pub fn interrupt_node(&self, node_id: &str, reason: impl Into<String>) -> Vec<String> {
        let transfer_ids = self
            .controls
            .read()
            .iter()
            .filter(|(_, registered)| registered.node_id.as_deref() == Some(node_id))
            .map(|(transfer_id, _)| transfer_id.clone())
            .collect::<Vec<_>>();
        let mut transfer_ids = transfer_ids;
        transfer_ids.sort();
        let reason = reason.into();
        for transfer_id in &transfer_ids {
            self.interrupt(transfer_id, reason.clone());
        }
        transfer_ids
    }

    /// The producer supplies the context of the connection that will execute this attempt.
    pub fn register_background_transfer(
        &self,
        mut snapshot: BackgroundTransferSnapshot,
        context: Option<&oxide_audit::AuditContext>,
    ) {
        self.cleanup_background_transfers();
        if let Some(context) = context {
            let context = context.for_request();
            let attempt_id = uuid::Uuid::new_v4().to_string();
            let retry_of = self.previous_attempts.read().get(&snapshot.id).cloned();
            let operation = context.operation(
                oxide_audit::AuditCategory::File,
                "file_transfer",
                Some(&transfer_audit_detail(
                    &snapshot,
                    &attempt_id,
                    retry_of.as_deref(),
                    None,
                )),
            );
            let mut child_context = context;
            child_context.parent_id = operation.id().map(str::to_owned);
            self.audit_transfers.write().insert(
                snapshot.id.clone(),
                AuditedTransfer {
                    operation,
                    context: child_context,
                    attempt_id,
                    retry_of,
                    stream_bytes: None,
                },
            );
        }
        // Match Tauri: callers may seed a speculative state, but registration
        // always exposes a queued background transfer until the task starts.
        let accepted = self.shutdown_state.load(Ordering::Acquire) == TRANSFER_MANAGER_RUNNING;
        if accepted {
            snapshot.state = BackgroundTransferState::Pending;
        } else {
            // Late delivery after session release is terminal, never resumable.
            snapshot.state = BackgroundTransferState::Cancelled;
            snapshot.backend_speed = Some(0);
            snapshot.end_time = Some(now_ms());
        }
        let transfer_id = snapshot.id.clone();
        self.background_transfers
            .write()
            .insert(transfer_id.clone(), snapshot);
        self.background_notify.notify_waiters();
        self.background_state_changes.send_replace(());
        if !accepted {
            let _ = self.finish_background_transfer(
                &transfer_id,
                BackgroundTransferState::Cancelled,
                None,
                None,
            );
        }
    }

    pub fn update_background_transfer_strategy(
        &self,
        transfer_id: &str,
        strategy: TransferStrategy,
    ) {
        if let Some(snapshot) = self.background_transfers.write().get_mut(transfer_id) {
            snapshot.strategy = strategy;
            self.background_notify.notify_waiters();
        }
    }

    /// Records bytes from a completed archive stream, separate from file payload progress.
    pub fn record_background_transfer_stream_bytes(&self, transfer_id: &str, stream_bytes: u64) {
        if let Some(transfer) = self.audit_transfers.write().get_mut(transfer_id) {
            transfer.stream_bytes = Some(stream_bytes);
        }
    }

    pub fn mark_background_transfer_active(&self, transfer_id: &str) {
        if let Some(snapshot) = self.background_transfers.write().get_mut(transfer_id)
            && !snapshot.state.is_finished()
        {
            let changed = snapshot.state != BackgroundTransferState::Active;
            snapshot.state = BackgroundTransferState::Active;
            if changed {
                self.background_state_changes.send_replace(());
            }
            self.background_notify.notify_waiters();
        }
    }

    pub fn update_background_transfer_progress(
        &self,
        transfer_id: &str,
        transferred: u64,
        total: u64,
        speed: u64,
    ) {
        if let Some(snapshot) = self.background_transfers.write().get_mut(transfer_id)
            && !snapshot.state.is_finished()
        {
            snapshot.transferred = transferred;
            if total > 0 {
                snapshot.size = total;
            }
            snapshot.backend_speed = Some(speed);
            let changed = snapshot.state != BackgroundTransferState::Active;
            snapshot.state = BackgroundTransferState::Active;
            if changed {
                self.background_state_changes.send_replace(());
            }
            self.background_notify.notify_waiters();
        }
    }

    pub fn finish_background_transfer(
        &self,
        transfer_id: &str,
        state: BackgroundTransferState,
        error: Option<String>,
        item_count: Option<u64>,
    ) -> Option<BackgroundTransferSnapshot> {
        let mut transfers = self.background_transfers.write();
        let snapshot = transfers.get_mut(transfer_id)?;
        let shutdown_cancelled = self.shutdown_state.load(Ordering::Acquire)
            != TRANSFER_MANAGER_RUNNING
            && snapshot.state == BackgroundTransferState::Cancelled;
        snapshot.state = if shutdown_cancelled {
            BackgroundTransferState::Cancelled
        } else {
            state
        };
        snapshot.error = if shutdown_cancelled { None } else { error };
        snapshot.item_count = item_count;
        snapshot.end_time = Some(now_ms());
        let snapshot = snapshot.clone();
        drop(transfers);
        if let Some(mut audited) = self.audit_transfers.write().remove(transfer_id) {
            use oxide_audit::{AuditEvidence, AuditOutcome};
            audited.operation.summary(&transfer_audit_detail(
                &snapshot,
                &audited.attempt_id,
                audited.retry_of.as_deref(),
                audited.stream_bytes,
            ));
            let outcome = match snapshot.state {
                BackgroundTransferState::Completed => AuditOutcome::Succeeded,
                BackgroundTransferState::Cancelled => AuditOutcome::Cancelled,
                BackgroundTransferState::Error
                    if snapshot.error.is_some() && snapshot.transferred > 0 =>
                {
                    AuditOutcome::Partial
                }
                BackgroundTransferState::Error if snapshot.error.is_some() => AuditOutcome::Failed,
                _ => AuditOutcome::Unknown,
            };
            if let Some(id) = audited.operation.id() {
                self.previous_attempts
                    .write()
                    .insert(transfer_id.to_owned(), id.to_owned());
            }
            audited.operation.finish(
                outcome,
                AuditEvidence::Protocol,
                None,
                Some(snapshot.transferred),
            );
        }
        self.background_notify.notify_waiters();
        self.background_state_changes.send_replace(());
        Some(snapshot)
    }

    pub fn get_background_transfer(&self, transfer_id: &str) -> Option<BackgroundTransferSnapshot> {
        self.cleanup_background_transfers();
        self.background_transfers.read().get(transfer_id).cloned()
    }

    pub fn audit_context_for_transfer(
        &self,
        transfer_id: &str,
    ) -> Option<oxide_audit::AuditContext> {
        self.audit_transfers
            .read()
            .get(transfer_id)
            .map(|transfer| transfer.context.clone())
    }

    pub fn list_background_transfers(
        &self,
        node_id: Option<&str>,
    ) -> Vec<BackgroundTransferSnapshot> {
        self.cleanup_background_transfers();
        let mut snapshots: Vec<_> = self
            .background_transfers
            .read()
            .values()
            .filter(|snapshot| node_id.is_none_or(|id| snapshot.node_id == id))
            .cloned()
            .collect();
        snapshots.sort_by_key(|snapshot| snapshot.start_time);
        snapshots
    }

    pub async fn wait_background_transfer_finished(
        &self,
        transfer_id: &str,
    ) -> Option<BackgroundTransferSnapshot> {
        loop {
            let notified = self.background_notify.notified();
            match self.get_background_transfer(transfer_id) {
                Some(snapshot) if snapshot.state.is_finished() => return Some(snapshot),
                Some(_) => notified.await,
                None => return None,
            }
        }
    }

    pub fn cancel(&self, transfer_id: &str) -> bool {
        if let Some(control) = self.get_control(transfer_id) {
            self.audit_control_request(
                transfer_id,
                "file_transfer_cancel",
                oxide_audit::AuditOutcome::CancelRequested,
            );
            control.cancel();
            true
        } else {
            false
        }
    }

    pub fn pause(&self, transfer_id: &str) -> bool {
        if let Some(control) = self.get_control(transfer_id) {
            self.audit_control_request(
                transfer_id,
                "file_transfer_pause",
                oxide_audit::AuditOutcome::Sent,
            );
            control.pause();
            if let Some(snapshot) = self.background_transfers.write().get_mut(transfer_id)
                && !snapshot.state.is_finished()
            {
                snapshot.state = BackgroundTransferState::Paused;
                snapshot.backend_speed = Some(0);
                self.background_state_changes.send_replace(());
                self.background_notify.notify_waiters();
            }
            true
        } else {
            false
        }
    }

    pub fn resume(&self, transfer_id: &str) -> bool {
        if let Some(control) = self.get_control(transfer_id) {
            self.audit_control_request(
                transfer_id,
                "file_transfer_resume",
                oxide_audit::AuditOutcome::Sent,
            );
            control.resume();
            if let Some(snapshot) = self.background_transfers.write().get_mut(transfer_id)
                && snapshot.state == BackgroundTransferState::Paused
            {
                snapshot.state = BackgroundTransferState::Pending;
                self.background_state_changes.send_replace(());
                self.background_notify.notify_waiters();
            }
            true
        } else {
            false
        }
    }

    fn audit_control_request(
        &self,
        transfer_id: &str,
        action: &str,
        outcome: oxide_audit::AuditOutcome,
    ) {
        let context = self
            .audit_transfers
            .read()
            .get(transfer_id)
            .map(|transfer| transfer.context.clone());
        if let Some(context) = context {
            context
                .operation(oxide_audit::AuditCategory::File, action, None)
                .finish(outcome, oxide_audit::AuditEvidence::Request, None, None);
        }
    }

    pub fn interrupt(&self, transfer_id: &str, reason: impl Into<String>) -> bool {
        let reason = reason.into();
        // This is distinct from cancel: reconnect wants the running worker to
        // stop using the broken SSH channel while leaving progress resumable.
        let had_control = if let Some(control) = self.get_control(transfer_id) {
            control.interrupt(reason.clone());
            true
        } else {
            false
        };
        if let Some(snapshot) = self.background_transfers.write().get_mut(transfer_id)
            && !snapshot.state.is_finished()
        {
            snapshot.state = BackgroundTransferState::Error;
            snapshot.error = Some(reason);
            snapshot.backend_speed = Some(0);
            snapshot.end_time = Some(now_ms());
            self.background_state_changes.send_replace(());
            self.background_notify.notify_waiters();
            return true;
        }
        had_control
    }

    pub fn cancel_all(&self) {
        for registered in self.controls.read().values() {
            registered.control.cancel();
        }
    }

    /// Cancels session-owned transfers once and waits only for the supplied grace period.
    pub async fn shutdown_session_transfers(
        &self,
        grace_period: Duration,
    ) -> SftpTransferShutdownReport {
        if self
            .shutdown_state
            .compare_exchange(
                TRANSFER_MANAGER_RUNNING,
                TRANSFER_MANAGER_SHUTTING_DOWN,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            let remaining_transfers = self.registered_count();
            return SftpTransferShutdownReport {
                started: false,
                drained: remaining_transfers == 0,
                cancelled_transfers: 0,
                remaining_transfers,
            };
        }

        let controls = self
            .controls
            .read()
            .values()
            .map(|registered| registered.control.clone())
            .collect::<Vec<_>>();
        for control in &controls {
            control.cancel();
        }

        {
            let shutdown_time = now_ms();
            let mut transfers = self.background_transfers.write();
            for snapshot in transfers.values_mut() {
                if !snapshot.state.is_finished() {
                    // Session exit is an explicit cancellation, not a resumable transport error.
                    snapshot.state = BackgroundTransferState::Cancelled;
                    snapshot.backend_speed = Some(0);
                    snapshot.error = None;
                    snapshot.end_time = Some(shutdown_time);
                }
            }
        }
        self.background_notify.notify_waiters();
        self.background_state_changes.send_replace(());
        let drained = tokio::time::timeout(grace_period, async {
            loop {
                let notified = self.shutdown_notify.notified();
                if self.registered_count() == 0 {
                    break;
                }
                notified.await;
            }
        })
        .await
        .is_ok();
        let remaining_transfers = self.registered_count();
        self.shutdown_state
            .store(TRANSFER_MANAGER_STOPPED, Ordering::Release);
        self.shutdown_notify.notify_waiters();

        SftpTransferShutdownReport {
            started: true,
            drained,
            cancelled_transfers: controls.len(),
            remaining_transfers,
        }
    }

    pub async fn check_control(&self, transfer_id: &str) -> Result<(), SftpError> {
        let Some(control) = self.get_control(transfer_id) else {
            return Ok(());
        };
        if control.is_cancelled() {
            if self.shutdown_state.load(Ordering::Acquire) != TRANSFER_MANAGER_RUNNING {
                // Application shutdown stops live I/O but keeps resumable progress;
                // an explicit user cancellation still removes partial targets.
                return Err(SftpError::TransferShutdown);
            }
            return Err(SftpError::TransferCancelled);
        }
        if let Some(reason) = control.interrupt_reason() {
            return Err(SftpError::TransferInterrupted(reason));
        }
        while control.is_paused() {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            if control.is_cancelled() {
                if self.shutdown_state.load(Ordering::Acquire) != TRANSFER_MANAGER_RUNNING {
                    return Err(SftpError::TransferShutdown);
                }
                return Err(SftpError::TransferCancelled);
            }
            if let Some(reason) = control.interrupt_reason() {
                return Err(SftpError::TransferInterrupted(reason));
            }
        }
        Ok(())
    }

    pub async fn acquire_permit(&self) -> SftpTransferPermit {
        loop {
            let notified = self.availability_notify.notified();
            if self.active_count() < self.max_concurrent() {
                break;
            }
            notified.await;
        }

        let permit = self
            .semaphore
            .clone()
            .acquire_owned()
            .await
            .expect("SFTP transfer semaphore should stay open for app lifetime");
        self.active_count.fetch_add(1, Ordering::AcqRel);
        SftpTransferPermit {
            _permit: permit,
            active_count: self.active_count.clone(),
            availability_notify: self.availability_notify.clone(),
        }
    }
}

impl Default for SftpTransferManager {
    fn default() -> Self {
        Self::new()
    }
}
