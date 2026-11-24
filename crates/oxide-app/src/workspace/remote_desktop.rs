use std::{
    collections::HashSet,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use super::forwards;

use oxide_remote_desktop::{
    RemoteDesktopFrameApplyStats, RemoteDesktopMappedPoint, RemoteDesktopViewState,
    SharedRemoteDesktopGeometry, remote_desktop_surface_with_geometry,
};
use oxide_remote_desktop_core::{
    NegotiatedCapabilities, NegotiatedCapabilityStatus, RemoteDesktopClipboardData,
    RemoteDesktopClipboardFormat, RemoteDesktopConnectionProfile, RemoteDesktopEndpoint,
    RemoteDesktopErrorCategory, RemoteDesktopFileConflictPolicy,
    RemoteDesktopFileTransferFailureKind, RemoteDesktopFrameDeliverySlot, RemoteDesktopHelperEvent,
    RemoteDesktopHelperRequest, RemoteDesktopKey, RemoteDesktopKeyState, RemoteDesktopLockKeys,
    RemoteDesktopMonitor, RemoteDesktopMonitorLayout, RemoteDesktopMonitorOrientation,
    RemoteDesktopMouseButton, RemoteDesktopMouseButtonState, RemoteDesktopProtocol,
    RemoteDesktopProviderManifest, RemoteDesktopRemoteFileEntry, RemoteDesktopRemoteFileKind,
    RemoteDesktopSecret, RemoteDesktopSessionStatus, RemoteDesktopSize, RemoteDesktopWheelDelta,
    builtin_preview_provider_registry,
};
use oxide_ui::button::{
    ButtonOptions, ButtonRadius, ButtonSize, ButtonVariant, IconButtonOptions, ToolbarButtonOptions,
};
use oxide_ui::{
    context_menu::{
        context_menu_event_boundary, context_menu_item_height_estimate,
        context_menu_separator_height_estimate,
    },
    dropdown_menu::{
        DropdownMenuItemKind, dropdown_menu_content, dropdown_menu_item, dropdown_menu_label,
        dropdown_menu_separator,
    },
    modal::{
        dialog_overlay, modal_body, modal_container, modal_footer, modal_header,
        overlay_content_boundary,
    },
};
use oxide_workspace::{Tab, TabKind, TabTitleSource};
use tokio::sync::Notify;
use zeroize::Zeroizing;

use super::*;

mod certificate;
mod clipboard;
mod input;
mod interaction;

mod session;
mod vendor_files;
mod view;
mod worker;

pub(in crate::workspace) use interaction::remote_desktop_keyboard_capture;

use certificate::*;
use clipboard::*;
use input::*;
use vendor_files::*;
use worker::*;

const REMOTE_DESKTOP_INITIAL_WIDTH: u32 = 1280;
const REMOTE_DESKTOP_INITIAL_HEIGHT: u32 = 720;
const REMOTE_DESKTOP_SCROLL_LINE: f32 = 38.0;
const REMOTE_DESKTOP_INITIAL_LAYOUT_PROBE_INTERVAL: Duration = Duration::from_millis(16);
const REMOTE_DESKTOP_INITIAL_LAYOUT_PROBE_TICKS: usize = 120;
const REMOTE_DESKTOP_RESIZE_DEBOUNCE: Duration = Duration::from_millis(120);
const REMOTE_DESKTOP_RESIZE_DELTA_THRESHOLD: u32 = 16;
const REMOTE_DESKTOP_RESIZE_MENU_WIDTH: f32 = 240.0;
const REMOTE_DESKTOP_RESIZE_MENU_GAP: f32 = 4.0;
const REMOTE_DESKTOP_RESIZE_MENU_VIEWPORT_PADDING: f32 = 8.0;
const REMOTE_DESKTOP_COMMON_RESOLUTIONS: [RemoteDesktopSize; 5] = [
    RemoteDesktopSize {
        width: 1280,
        height: 720,
    },
    RemoteDesktopSize {
        width: 1366,
        height: 768,
    },
    RemoteDesktopSize {
        width: 1920,
        height: 1080,
    },
    RemoteDesktopSize {
        width: 2560,
        height: 1440,
    },
    RemoteDesktopSize {
        width: 3840,
        height: 2160,
    },
];
const REMOTE_DESKTOP_DEFAULT_SCALE_FACTOR_PERCENT: u32 = 100;
const REMOTE_DESKTOP_MIN_SCALE_FACTOR_PERCENT: u32 = 100;
const REMOTE_DESKTOP_MAX_SCALE_FACTOR_PERCENT: u32 = 500;
const REMOTE_DESKTOP_SCALE_PERCENT_MULTIPLIER: f32 = 100.0;
const REMOTE_DESKTOP_SCROLL_PIXEL_STEP: f32 = 120.0;
const REMOTE_DESKTOP_DELIVERY_BUDGET: delivery::DeliveryBudget =
    delivery::DeliveryBudget::new(64, Duration::from_millis(4));
const REMOTE_DESKTOP_FRAME_READY_DRAIN_LIMIT: usize = 32;
const REMOTE_DESKTOP_FRAME_READY_DRAIN_BUDGET: Duration = Duration::from_millis(6);
const REMOTE_DESKTOP_DIAGNOSTICS_ENV: &str = "oxide_remote_desktop_core_DIAGNOSTICS";
const REMOTE_DESKTOP_AUTOMATIC_RECONNECT_DELAYS: [Duration; 4] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(5),
    Duration::from_secs(10),
];

fn remote_desktop_automatic_reconnect_delay(attempt: usize) -> Duration {
    REMOTE_DESKTOP_AUTOMATIC_RECONNECT_DELAYS
        .get(attempt)
        .copied()
        .unwrap_or_else(|| {
            REMOTE_DESKTOP_AUTOMATIC_RECONNECT_DELAYS
                .last()
                .copied()
                .unwrap_or(Duration::ZERO)
        })
}

fn remote_desktop_network_failure_allows_automatic_reconnect(
    has_connected: bool,
    category: Option<RemoteDesktopErrorCategory>,
) -> bool {
    // Initial setup, authentication, protocol, and dependency failures need a
    // user decision. Only a network failure after a proven session is retried.
    has_connected && category == Some(RemoteDesktopErrorCategory::Network)
}

fn remote_desktop_tab_visible(main_tab_visible: bool, detached_tab_visible: bool) -> bool {
    main_tab_visible || detached_tab_visible
}

fn remote_desktop_resolution_label(size: RemoteDesktopSize) -> String {
    format!("{} × {}", size.width, size.height)
}

#[derive(Debug)]
pub(super) enum RemoteDesktopWorkerDelivery {
    FrameReady {
        tab_id: TabId,
        generation: u64,
    },
    FrameRecoveryRequired {
        tab_id: TabId,
        generation: u64,
    },
    Event {
        tab_id: TabId,
        generation: u64,
        event: RemoteDesktopHelperEvent,
    },
    TransportFailed {
        tab_id: TabId,
        generation: u64,
        message: String,
    },
}

pub(super) enum RemoteDesktopDeliveryIntent {
    CredentialsRequired { generation: u64 },
    ClipboardTransferFailed,
    VncFileTransferCompleted,
    VncFileTransferFailed(RemoteDesktopFileTransferFailureKind),
}

pub(super) struct RemoteDesktopDeliveryOutcome {
    changed: bool,
    backlog_remaining: bool,
    intents: Vec<RemoteDesktopDeliveryIntent>,
}

#[derive(Clone)]
struct RemoteDesktopWorkerWake {
    pending: Arc<AtomicBool>,
    stopped: Arc<AtomicBool>,
    notification: Arc<Notify>,
}

impl Default for RemoteDesktopWorkerWake {
    fn default() -> Self {
        Self {
            pending: Arc::new(AtomicBool::new(false)),
            stopped: Arc::new(AtomicBool::new(false)),
            notification: Arc::new(Notify::new()),
        }
    }
}

impl RemoteDesktopWorkerWake {
    fn mark(&self) {
        // Worker threads cannot touch GPUI state directly. Notify stores one
        // permit when the foreground task has not started waiting yet.
        self.pending.store(true, Ordering::Release);
        self.notification.notify_one();
    }

    fn take(&self) -> bool {
        self.pending.swap(false, Ordering::AcqRel)
    }

    fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
        self.notification.notify_one();
    }

    fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }

    async fn wait(&self) {
        self.notification.notified().await;
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct RemoteDesktopRenderDiagnostics {
    batches: u64,
    events_drained: u64,
    drain_budget_hits: u64,
    full_frames: u64,
    frame_updates: u64,
    dirty_updates_applied: u64,
    dirty_updates_rejected: u64,
    full_update_recoveries: u64,
    corrupted_frames: u64,
    first_trace_id: Option<u64>,
    last_trace_id: Option<u64>,
    dirty_rect_pixels: u64,
    dirty_frame_pixels: u64,
    pending_texture_updates: u64,
    pending_texture_upload_bytes: u64,
    dirty_tiles_refreshed: u64,
    frame_tiles_created: u64,
    retired_images: u64,
    total_apply_micros: u64,
    max_apply_micros: u64,
}

impl RemoteDesktopRenderDiagnostics {
    fn record_batch(
        &mut self,
        drained_events: usize,
        budget_hit: bool,
        apply_elapsed: Duration,
        apply_stats: RemoteDesktopFrameApplyStats,
        retired_images: usize,
    ) {
        self.batches = self.batches.saturating_add(1);
        self.events_drained = self.events_drained.saturating_add(drained_events as u64);
        if budget_hit {
            self.drain_budget_hits = self.drain_budget_hits.saturating_add(1);
        }
        self.full_frames = self
            .full_frames
            .saturating_add(apply_stats.full_frames as u64);
        self.frame_updates = self
            .frame_updates
            .saturating_add(apply_stats.frame_updates as u64);
        self.dirty_updates_applied = self
            .dirty_updates_applied
            .saturating_add(apply_stats.dirty_updates_applied as u64);
        self.dirty_updates_rejected = self
            .dirty_updates_rejected
            .saturating_add(apply_stats.dirty_updates_rejected as u64);
        self.full_update_recoveries = self
            .full_update_recoveries
            .saturating_add(apply_stats.full_update_recoveries as u64);
        self.corrupted_frames = self
            .corrupted_frames
            .saturating_add(apply_stats.corrupted_frames as u64);
        if self.first_trace_id.is_none() {
            self.first_trace_id = apply_stats.first_trace_id;
        }
        if apply_stats.last_trace_id.is_some() {
            self.last_trace_id = apply_stats.last_trace_id;
        }
        self.dirty_rect_pixels = self
            .dirty_rect_pixels
            .saturating_add(apply_stats.dirty_rect_pixels);
        self.dirty_frame_pixels = self
            .dirty_frame_pixels
            .saturating_add(apply_stats.dirty_frame_pixels);
        self.pending_texture_updates = apply_stats.pending_texture_updates as u64;
        self.pending_texture_upload_bytes = apply_stats.pending_texture_upload_bytes as u64;
        self.dirty_tiles_refreshed = self
            .dirty_tiles_refreshed
            .saturating_add(apply_stats.dirty_tiles_refreshed as u64);
        self.frame_tiles_created = self
            .frame_tiles_created
            .saturating_add(apply_stats.frame_tiles_created as u64);
        self.retired_images = self.retired_images.saturating_add(retired_images as u64);
        let apply_micros = duration_micros_u64(apply_elapsed);
        self.total_apply_micros = self.total_apply_micros.saturating_add(apply_micros);
        self.max_apply_micros = self.max_apply_micros.max(apply_micros);
    }
}

fn send_remote_desktop_worker_delivery(
    delivery_tx: &mpsc::Sender<RemoteDesktopWorkerDelivery>,
    worker_wake: &RemoteDesktopWorkerWake,
    delivery: RemoteDesktopWorkerDelivery,
) {
    // Publish before waking so the foreground task cannot observe an empty queue.
    if delivery_tx.send(delivery).is_ok() {
        worker_wake.mark();
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct RemoteDesktopModifierState {
    // GPUI key events carry aggregate modifier state; mirror that state so the
    // helper can correct missed platform modifier key transitions.
    shift: bool,
    ctrl: bool,
    alt: bool,
    meta: bool,
}

impl RemoteDesktopModifierState {
    fn from_gpui(modifiers: gpui::Modifiers) -> Self {
        Self {
            shift: modifiers.shift,
            ctrl: modifiers.control,
            alt: modifiers.alt,
            meta: modifiers.platform,
        }
    }
}

struct RemoteDesktopWorkerOwner {
    request_tx: Option<mpsc::Sender<RemoteDesktopHelperRequest>>,
    worker_thread: Option<thread::JoinHandle<()>>,
}

impl RemoteDesktopWorkerOwner {
    fn new(
        request_tx: mpsc::Sender<RemoteDesktopHelperRequest>,
        worker_thread: thread::JoinHandle<()>,
    ) -> Self {
        Self {
            request_tx: Some(request_tx),
            worker_thread: Some(worker_thread),
        }
    }

    fn request_sender(&self) -> Option<&mpsc::Sender<RemoteDesktopHelperRequest>> {
        self.request_tx.as_ref()
    }

    fn request_sender_cloned(&self) -> Option<mpsc::Sender<RemoteDesktopHelperRequest>> {
        self.request_tx.clone()
    }

    fn send(&self, request: RemoteDesktopHelperRequest) -> bool {
        self.request_tx
            .as_ref()
            .is_some_and(|request_tx| request_tx.send(request).is_ok())
    }

    fn shutdown(&mut self) {
        let worker_thread = self.close_and_take_thread();
        Self::retire_worker_thread(worker_thread);
    }

    fn close_and_take_thread(&mut self) -> Option<thread::JoinHandle<()>> {
        if let Some(request_tx) = self.request_tx.take() {
            // Session shutdown and helper replacement release input state before
            // asking the helper to exit cooperatively.
            let _ = request_tx.send(RemoteDesktopHelperRequest::ReleaseAllInputs);
            let _ = request_tx.send(RemoteDesktopHelperRequest::Close);
        }
        self.worker_thread.take()
    }

    fn retire_worker_thread(worker_thread: Option<thread::JoinHandle<()>>) {
        let Some(worker_thread) = worker_thread else {
            return;
        };
        if worker_thread.is_finished() {
            let _ = worker_thread.join();
            return;
        }

        let reaper_name = worker_thread
            .thread()
            .name()
            .map(|name| format!("{name}-reaper"))
            .unwrap_or_else(|| "remote-desktop-worker-reaper".to_string());
        // The protocol worker has its own helper timeout, so this reaper owns a
        // bounded join without blocking the GPUI thread during session teardown.
        let _ = thread::Builder::new().name(reaper_name).spawn(move || {
            let _ = worker_thread.join();
        });
    }
}

impl Drop for RemoteDesktopWorkerOwner {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn remote_desktop_audit_context(
    profile: &RemoteDesktopConnectionProfile,
) -> Option<oxide_audit::AuditContext> {
    oxide_audit::AuditContext::current_request()
        .or_else(oxide_audit::AuditContext::current)
        .map(|mut context| {
            if context.source == oxide_audit::AuditSource::Application {
                context.source = oxide_audit::AuditSource::User;
            }
            context.session(
                profile.protocol.provider_id(),
                &format!("{}:{}", profile.endpoint.host, profile.endpoint.port),
            )
        })
}

fn desktop_connection_audit(
    context: Option<&oxide_audit::AuditContext>,
    action: &str,
    source: Option<oxide_audit::AuditSource>,
) -> Option<oxide_audit::AuditOperation> {
    context.map(|context| {
        let mut context = context.clone();
        if let Some(source) = source {
            context.source = source;
            context.agent_id = None;
        }
        let mut audit = context.operation(oxide_audit::AuditCategory::Connection, action, None);
        let mcp = context.source == oxide_audit::AuditSource::Mcp;
        audit.authorization(
            if mcp {
                oxide_audit::AuditAuthorization::Approved
            } else {
                oxide_audit::AuditAuthorization::NotRequired
            },
            mcp.then_some("mcp_client_policy"),
        );
        audit
    })
}

pub(in crate::workspace) struct RemoteDesktopSshTunnelLease {
    audit_context: Option<oxide_audit::AuditContext>,
    connect_audit: Option<oxide_audit::AuditOperation>,
    lease_id: String,
    forwarding_service: forwards::ForwardingRuntimeService,
}

pub(in crate::workspace) struct PendingRemoteDesktopSshTunnel {
    audit_context: Option<oxide_audit::AuditContext>,
    connect_audit: Option<oxide_audit::AuditOperation>,
    lease_id: Option<String>,
    forwarding_service: forwards::ForwardingRuntimeService,
    worker: Option<tokio::task::JoinHandle<Result<RemoteDesktopEndpoint, String>>>,
}

impl PendingRemoteDesktopSshTunnel {
    pub(in crate::workspace) fn new(
        lease_id: String,
        forwarding_service: forwards::ForwardingRuntimeService,
        worker: tokio::task::JoinHandle<Result<RemoteDesktopEndpoint, String>>,
        audit_context: Option<oxide_audit::AuditContext>,
        connect_audit: Option<oxide_audit::AuditOperation>,
    ) -> Self {
        Self {
            audit_context,
            connect_audit,
            lease_id: Some(lease_id),
            forwarding_service,
            worker: Some(worker),
        }
    }

    pub(in crate::workspace) async fn finish(
        mut self,
    ) -> Result<(RemoteDesktopEndpoint, RemoteDesktopSshTunnelLease), String> {
        // Retain the handle while awaiting so cancellation of this future
        // still aborts setup through the pending owner's Drop implementation.
        let worker = self
            .worker
            .as_mut()
            .expect("pending remote desktop tunnel owns one worker");
        let endpoint = match worker
            .await
            .map_err(|error| error.to_string())
            .and_then(|result| result)
        {
            Ok(endpoint) => endpoint,
            Err(error) => {
                if let Some(audit) = self.connect_audit.take() {
                    audit.finish(
                        oxide_audit::AuditOutcome::Failed,
                        oxide_audit::AuditEvidence::Protocol,
                        None,
                        None,
                    );
                }
                return Err(error);
            }
        };
        let lease_id = self
            .lease_id
            .take()
            .expect("pending remote desktop tunnel owns one lease id");
        let lease = RemoteDesktopSshTunnelLease {
            lease_id,
            forwarding_service: self.forwarding_service.clone(),
            audit_context: self.audit_context.take(),
            connect_audit: self.connect_audit.take(),
        };
        Ok((endpoint, lease))
    }
}

impl Drop for PendingRemoteDesktopSshTunnel {
    fn drop(&mut self) {
        if let Some(audit) = self.connect_audit.take() {
            audit.finish(
                oxide_audit::AuditOutcome::Cancelled,
                oxide_audit::AuditEvidence::Lifecycle,
                None,
                None,
            );
        }
        if let Some(worker) = self.worker.take() {
            worker.abort();
        }
        // Cancellation may race with listener creation, so the shared lease
        // registry remains the authoritative cleanup boundary.
        if let Some(lease_id) = self.lease_id.take() {
            self.forwarding_service
                .close_remote_desktop_tunnel(lease_id);
        }
    }
}

impl Drop for RemoteDesktopSshTunnelLease {
    fn drop(&mut self) {
        if let Some(audit) = self.connect_audit.take() {
            audit.finish(
                oxide_audit::AuditOutcome::Cancelled,
                oxide_audit::AuditEvidence::Lifecycle,
                None,
                None,
            );
        }
        // The lease stops only its hidden listener; NodeRouter retains the
        // physical SSH node for every other registered consumer.
        self.forwarding_service
            .close_remote_desktop_tunnel(self.lease_id.clone());
    }
}

pub(in crate::workspace) struct RemoteDesktopSessionEntity {
    tab_id: TabId,
    profile: RemoteDesktopConnectionProfile,
    provider: RemoteDesktopProviderManifest,
    plugin_entity: Option<gpui::WeakEntity<super::plugin_entity::PluginWorkspaceEntity>>,
    password: Option<RemoteDesktopSecret>,
    credential_prompt_task: Option<gpui::Task<()>>,
    credential_prompt_generation: Option<u64>,
    certificate_store_path: PathBuf,
    certificate_challenge: Option<RemoteDesktopCertificateChallengeState>,
    session_trusted_certificate_fingerprint: Option<String>,
    state: RemoteDesktopViewState,
    geometry: SharedRemoteDesktopGeometry,
    frame_slot: RemoteDesktopFrameDeliverySlot,
    ui_frame_visible: bool,

    clipboard_activation: Option<Subscription>,
    last_clipboard_files: Option<u64>,
    audit_context: Option<oxide_audit::AuditContext>,
    connect_audit: Option<oxide_audit::AuditOperation>,
    file_audits: HashMap<String, oxide_audit::AuditOperation>,
    ssh_tunnel: Option<RemoteDesktopSshTunnelLease>,
    delivery_tx: mpsc::Sender<RemoteDesktopWorkerDelivery>,
    delivery_rx: mpsc::Receiver<RemoteDesktopWorkerDelivery>,
    worker: Option<RemoteDesktopWorkerOwner>,
    worker_wake: Option<RemoteDesktopWorkerWake>,
    worker_generation: u64,
    has_connected: bool,
    automatic_reconnect_attempt: usize,
    automatic_reconnect_worker_generation: Option<u64>,
    automatic_reconnect_task: Option<Task<()>>,
    window_handle: AnyWindowHandle,
    last_viewport_size: Option<RemoteDesktopSize>,
    last_sent_resize: Option<RemoteDesktopResizeRequestState>,
    last_viewport_scale_factor: Option<u32>,
    last_monitor_layout: RemoteDesktopMonitorLayout,
    resize_generation: Arc<AtomicU64>,
    follow_window_size: bool,
    last_input_modifiers: RemoteDesktopModifierState,
    last_lock_keys: Option<RemoteDesktopLockKeys>,
    pressed_mouse_buttons: HashSet<RemoteDesktopMouseButton>,
    wheel_pixel_remainder: RemoteDesktopWheelDelta,
    render_diagnostics: RemoteDesktopRenderDiagnostics,
    vnc_files: RemoteDesktopVncFileBrowserState,
}

impl RemoteDesktopSessionEntity {
    pub(in crate::workspace) fn active_session_status(&self) -> RemoteDesktopSessionStatus {
        // The protocol entity remains the authoritative source for sidebar liveness.
        self.state.snapshot().status
    }

    fn new(
        tab_id: TabId,
        profile: RemoteDesktopConnectionProfile,
        provider: RemoteDesktopProviderManifest,
        password: Option<RemoteDesktopSecret>,
        certificate_store_path: PathBuf,
        frame_slot: RemoteDesktopFrameDeliverySlot,
        window_handle: AnyWindowHandle,
    ) -> Self {
        let audit_context = remote_desktop_audit_context(&profile);
        let (delivery_tx, delivery_rx) = mpsc::channel();
        let mut state = RemoteDesktopViewState::new(profile.label.clone(), profile.protocol)
            .with_read_only(profile.read_only);
        state.apply_event(RemoteDesktopHelperEvent::Status {
            status: RemoteDesktopSessionStatus::Connecting,
            message: None,
        });
        Self {
            tab_id,
            profile,
            provider,
            plugin_entity: None,
            // The tab retains one zeroizing credential owner so a reconnect
            // can answer a fresh certificate-gated authentication request.
            password,
            credential_prompt_task: None,
            credential_prompt_generation: None,
            certificate_store_path,
            certificate_challenge: None,
            session_trusted_certificate_fingerprint: None,
            state,
            geometry: SharedRemoteDesktopGeometry::default(),
            frame_slot,
            ui_frame_visible: false,

            clipboard_activation: None,
            last_clipboard_files: None,
            audit_context,
            connect_audit: None,
            file_audits: HashMap::new(),
            ssh_tunnel: None,
            // Each tab owns its delivery mailbox. A wake for one detached window
            // must never drain another tab's lifecycle events or frame notices.
            delivery_tx,
            delivery_rx,
            worker: None,
            worker_wake: None,
            worker_generation: 0,
            has_connected: false,
            automatic_reconnect_attempt: 0,
            automatic_reconnect_worker_generation: None,
            automatic_reconnect_task: None,
            window_handle,
            last_viewport_size: None,
            last_sent_resize: None,
            last_viewport_scale_factor: None,
            last_monitor_layout: RemoteDesktopMonitorLayout::default(),
            resize_generation: Arc::new(AtomicU64::new(0)),
            follow_window_size: true,
            last_input_modifiers: RemoteDesktopModifierState::default(),
            last_lock_keys: None,
            pressed_mouse_buttons: HashSet::new(),
            wheel_pixel_remainder: remote_desktop_empty_wheel_delta(),
            render_diagnostics: RemoteDesktopRenderDiagnostics::default(),
            vnc_files: RemoteDesktopVncFileBrowserState::default(),
        }
    }

    fn schedule_worker_wake(
        &self,
        generation: u64,
        worker_wake: RemoteDesktopWorkerWake,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |session, cx| {
            loop {
                worker_wake.wait().await;
                let should_deliver = worker_wake.take();
                let stopped = worker_wake.is_stopped();
                if should_deliver {
                    let delivery_available = session
                        .update(cx, |current, cx| {
                            if current.worker_generation != generation {
                                return false;
                            }
                            cx.emit(RemoteDesktopSessionEvent::DeliveryReady { generation });
                            true
                        })
                        .unwrap_or(false);
                    if !delivery_available {
                        break;
                    }
                }
                if stopped {
                    break;
                }
            }
        })
        .detach();
    }

    fn schedule_frame_apply(&self, generation: u64, delay: Duration, cx: &mut Context<Self>) {
        cx.spawn(async move |session, cx| {
            if !delay.is_zero() {
                Timer::after(delay).await;
            }
            let _ = session.update(cx, |current, cx| {
                if current.worker_generation == generation {
                    cx.emit(RemoteDesktopSessionEvent::FrameApplyReady { generation });
                }
            });
        })
        .detach();
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::workspace) enum RemoteDesktopSessionEvent {
    PluginRequired { protocol: RemoteDesktopProtocol },
    CredentialsRequired { generation: u64 },
    DeliveryReady { generation: u64 },
    FrameApplyReady { generation: u64 },
    ClipboardTransferFailed,
    VncFileTransferCompleted,
    VncFileTransferFailed(RemoteDesktopFileTransferFailureKind),
}

impl gpui::EventEmitter<RemoteDesktopSessionEvent> for RemoteDesktopSessionEntity {}

/// Owns all remote desktop sessions independently from the workspace window shell.
pub(in crate::workspace) struct RemoteDesktopWorkspaceEntity {
    sessions: HashMap<TabId, Entity<RemoteDesktopSessionEntity>>,
    session_subscriptions: HashMap<TabId, Vec<Subscription>>,
}

impl RemoteDesktopWorkspaceEntity {
    pub(in crate::workspace) fn stop_plugins(
        &self,
        plugin_id: Option<&str>,
        cx: &mut App,
    ) -> Vec<thread::JoinHandle<()>> {
        self.sessions
            .values()
            .filter_map(|session| {
                session.update(cx, |session, cx| {
                    if session.plugin_entity.is_none()
                        || plugin_id.is_some_and(|id| id != session.provider.id)
                    {
                        return None;
                    }
                    // Reap the helper before replacing its executable, including on Windows.
                    let worker_thread = session
                        .worker
                        .as_mut()
                        .and_then(RemoteDesktopWorkerOwner::close_and_take_thread);
                    session.shutdown(None, cx);
                    session.worker_generation =
                        next_remote_desktop_worker_generation(session.worker_generation);
                    session.state.apply_event(RemoteDesktopHelperEvent::Status {
                        status: RemoteDesktopSessionStatus::Disconnected,
                        message: None,
                    });
                    cx.notify();
                    worker_thread
                })
            })
            .collect()
    }

    pub(in crate::workspace) fn sync_plugins(
        &self,
        providers: &[RemoteDesktopProviderManifest],
        cx: &mut App,
    ) {
        for session in self.sessions.values() {
            let current = session.read(cx);
            if current.plugin_entity.is_none()
                || providers
                    .iter()
                    .any(|provider| provider == &current.provider)
            {
                continue;
            }
            session.update(cx, |session, cx| {
                session.shutdown(None, cx);
                session.worker_generation =
                    next_remote_desktop_worker_generation(session.worker_generation);
                session.state.apply_event(RemoteDesktopHelperEvent::Status {
                    status: RemoteDesktopSessionStatus::Disconnected,
                    message: None,
                });
                cx.notify();
            });
        }
    }

    pub(in crate::workspace) fn new() -> Self {
        Self {
            sessions: HashMap::new(),
            session_subscriptions: HashMap::new(),
        }
    }

    pub(in crate::workspace) fn session(
        &self,
        tab_id: TabId,
    ) -> Option<Entity<RemoteDesktopSessionEntity>> {
        self.sessions.get(&tab_id).cloned()
    }

    pub(in crate::workspace) fn insert(
        &mut self,
        tab_id: TabId,
        session: Entity<RemoteDesktopSessionEntity>,
        subscriptions: Vec<Subscription>,
    ) {
        self.sessions.insert(tab_id, session);
        self.session_subscriptions.insert(tab_id, subscriptions);
    }

    pub(in crate::workspace) fn remove(
        &mut self,
        tab_id: TabId,
    ) -> Option<Entity<RemoteDesktopSessionEntity>> {
        self.session_subscriptions.remove(&tab_id);
        self.sessions.remove(&tab_id)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RemoteDesktopResizeRequestState {
    size: RemoteDesktopSize,
    scale_factor: Option<u32>,
}
