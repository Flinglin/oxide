use super::*;

impl RemoteDesktopSessionEntity {
    fn refresh_plugin_provider(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(plugins) = self.plugin_entity.as_ref() else {
            return true;
        };
        let provider = plugins.upgrade().and_then(|plugins| {
            plugins
                .read(cx)
                .remote_desktop_providers()
                .into_iter()
                .find(|provider| provider.id == self.provider.id)
        });
        if let Some(provider) = provider {
            self.provider = provider;
            true
        } else {
            self.state.apply_event(RemoteDesktopHelperEvent::Status {
                status: RemoteDesktopSessionStatus::Disconnected,
                message: None,
            });
            cx.emit(RemoteDesktopSessionEvent::PluginRequired {
                protocol: self.profile.protocol,
            });
            cx.notify();
            false
        }
    }

    fn start_connection_audit(&mut self, action: &str, source: Option<oxide_audit::AuditSource>) {
        if action == "desktop_connect" {
            if let Some(audit) = self
                .ssh_tunnel
                .as_mut()
                .and_then(|lease| lease.connect_audit.take())
            {
                self.connect_audit = Some(audit);
                return;
            }
        }
        self.connect_audit = desktop_connection_audit(self.audit_context.as_ref(), action, source);
    }

    fn finish_connection_audit(&mut self, outcome: oxide_audit::AuditOutcome) {
        if let Some(audit) = self.connect_audit.take() {
            audit.finish(outcome, oxide_audit::AuditEvidence::Protocol, None, None);
        }
    }

    pub(in crate::workspace) fn observe_desktop(
        &self,
        category: oxide_audit::AuditCategory,
        action: &str,
        detail: &str,
        outcome: oxide_audit::AuditOutcome,
        bytes: Option<u64>,
    ) {
        if let Some(context) = self.audit_context.as_ref() {
            let context = context.for_request();
            let mut audit = context.operation(category, action, Some(detail));
            let authorization = if matches!(
                context.source,
                oxide_audit::AuditSource::Mcp | oxide_audit::AuditSource::Plugin
            ) {
                oxide_audit::AuditAuthorization::Approved
            } else {
                oxide_audit::AuditAuthorization::NotRequired
            };
            audit.authorization(authorization, None);
            audit.finish(outcome, oxide_audit::AuditEvidence::Protocol, None, bytes);
        }
    }
    pub(super) fn install_release_handler(&self, cx: &mut Context<Self>) {
        cx.on_release(|session, _cx| {
            // Entity destruction owns helper shutdown, but never shared SSH
            // nodes, SFTP sessions, or forwarding runtimes.
            if let Some(worker_wake) = session.worker_wake.take() {
                worker_wake.stop();
            }
            session.cancel_automatic_reconnect();
            session.shutdown_worker();
            drop(session.ssh_tunnel.take());
            drop(session.password.take());
            session.credential_prompt_task.take();
        })
        .detach();
    }

    pub(super) fn bind_window(&mut self, window_handle: AnyWindowHandle) {
        let window_changed = self.window_handle != window_handle;
        self.window_handle = window_handle;
        if window_changed {
            self.clipboard_activation = None;
        }
        if window_changed && let Some(worker_wake) = self.worker_wake.as_ref() {
            // A wake may have targeted the old window during handoff. Store
            // one fresh permit without polling render.
            worker_wake.mark();
        }
    }

    fn shutdown_worker(&mut self) {
        self.last_clipboard_files = None;
        self.reset_vnc_file_browser_connection();
        if let Some(mut worker) = self.worker.take() {
            worker.shutdown();
        }
    }

    fn cancel_automatic_reconnect(&mut self) {
        self.automatic_reconnect_worker_generation = None;
        self.automatic_reconnect_attempt = 0;
        drop(self.automatic_reconnect_task.take());
    }

    fn mark_connection_established(&mut self) {
        self.finish_connection_audit(oxide_audit::AuditOutcome::Succeeded);
        self.has_connected = true;
        self.automatic_reconnect_worker_generation = None;
        self.automatic_reconnect_attempt = 0;
        drop(self.automatic_reconnect_task.take());
    }

    fn begin_automatic_reconnect(
        &mut self,
        failed_generation: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.automatic_reconnect_worker_generation == Some(failed_generation) {
            return;
        }
        if let Some(worker_wake) = self.worker_wake.take() {
            worker_wake.stop();
        }
        self.shutdown_worker();

        let replacement_frame_slot = RemoteDesktopFrameDeliverySlot::new();
        // Freeze the last presented frame and sever queued deltas from the
        // failed worker before the replacement graphics epoch starts.
        replacement_frame_slot.set_visible(self.frame_slot.is_visible());
        self.frame_slot = replacement_frame_slot;
        self.state.begin_transport_reconnect();
        let retired_images = self.state.take_retired_images();
        let retired_textures = self.state.take_retired_textures();
        self.drop_images(retired_images, window, cx);
        Self::drop_textures(retired_textures, window);

        let attempt = self.automatic_reconnect_attempt;
        self.automatic_reconnect_attempt = self.automatic_reconnect_attempt.saturating_add(1);
        self.automatic_reconnect_worker_generation = Some(failed_generation);
        let delay = remote_desktop_automatic_reconnect_delay(attempt);
        let reconnect_task = cx.spawn(async move |session, cx| {
            Timer::after(delay).await;
            let window_handle = session
                .update(cx, |session, _cx| {
                    (session.automatic_reconnect_worker_generation == Some(failed_generation)
                        && session.worker_generation == failed_generation)
                        .then_some(session.window_handle)
                })
                .ok()
                .flatten();
            let Some(window_handle) = window_handle else {
                return;
            };
            let _ = cx.update_window(window_handle, move |_, window, cx| {
                let _ = session.update(cx, |session, cx| {
                    if session.automatic_reconnect_worker_generation != Some(failed_generation)
                        || session.worker_generation != failed_generation
                    {
                        return;
                    }
                    session.automatic_reconnect_worker_generation = None;
                    session.restart_worker_preserving_frame(
                        oxide_audit::AuditSource::System,
                        window,
                        cx,
                    );
                    cx.notify();
                });
            });
        });
        self.automatic_reconnect_task = Some(reconnect_task);
    }

    pub(super) fn shutdown(&mut self, mut window: Option<&mut Window>, cx: &mut Context<Self>) {
        let had_worker = self.worker.is_some();
        if self.connect_audit.is_some() {
            self.finish_connection_audit(oxide_audit::AuditOutcome::Cancelled);
        }
        self.cancel_automatic_reconnect();
        if let Some(worker_wake) = self.worker_wake.take() {
            worker_wake.stop();
        }
        self.shutdown_worker();
        if had_worker {
            self.observe_desktop(
                oxide_audit::AuditCategory::Connection,
                "desktop_disconnect",
                "helper close requested",
                oxide_audit::AuditOutcome::Sent,
                None,
            );
        }

        drop(self.ssh_tunnel.take());
        drop(self.password.take());
        self.credential_prompt_task.take();
        let images = self.state.take_all_images();
        let textures = self.state.take_all_textures();
        for image in images {
            cx.drop_image(image, window.as_deref_mut());
        }
        // A destroyed window has already released its texture atlas.
        if let Some(window) = window {
            Self::drop_textures(textures, window);
        }
    }

    fn poll_deliveries(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> RemoteDesktopDeliveryOutcome {
        let drain = delivery::drain_channel(&self.delivery_rx, REMOTE_DESKTOP_DELIVERY_BUDGET);
        let mut changed = false;
        let mut intents = Vec::new();
        for delivery in drain.items {
            match delivery {
                RemoteDesktopWorkerDelivery::FrameReady { tab_id, generation } => {
                    debug_assert_eq!(tab_id, self.tab_id);
                    if self.frame_slot.is_visible()
                        && self.apply_frame_ready(generation, window, cx)
                    {
                        changed = true;
                    }
                }
                RemoteDesktopWorkerDelivery::FrameRecoveryRequired { tab_id, generation } => {
                    debug_assert_eq!(tab_id, self.tab_id);
                    if self.worker_generation != generation {
                        continue;
                    }
                    // Saturation breaks delta continuity, so the session asks
                    // its helper for one new base frame.
                    if let Some(worker) = self.worker.as_ref() {
                        worker.send(RemoteDesktopHelperRequest::RequestFrame);
                    }
                }
                RemoteDesktopWorkerDelivery::Event {
                    tab_id,
                    generation,
                    event,
                } => {
                    debug_assert_eq!(tab_id, self.tab_id);
                    if self.worker_generation != generation {
                        continue;
                    }
                    match event {
                        RemoteDesktopHelperEvent::ServerCertificate { certificate } => {
                            self.handle_certificate(generation, certificate, cx);
                            changed = true;
                        }
                        RemoteDesktopHelperEvent::ClipboardText { text }
                            if self.profile.session_options.clipboard.text =>
                        {
                            self.observe_desktop(
                                oxide_audit::AuditCategory::File,
                                "desktop_clipboard_transfer",
                                "remote_to_local text",
                                oxide_audit::AuditOutcome::Succeeded,
                                Some(text.len() as u64),
                            );
                            // Keep a session-scoped zeroizing copy for explicitly authorized
                            // Public MCP reads; the platform clipboard remains the UI boundary.

                            cx.write_to_clipboard(ClipboardItem::new_string(text));
                            changed = true;
                        }
                        RemoteDesktopHelperEvent::ClipboardData { data }
                            if self.profile.session_options.clipboard.images =>
                        {
                            self.observe_desktop(
                                oxide_audit::AuditCategory::File,
                                "desktop_clipboard_transfer",
                                "remote_to_local image",
                                oxide_audit::AuditOutcome::Succeeded,
                                Some(data.bytes.len() as u64),
                            );

                            if let Some(item) = remote_desktop_clipboard_item_from_data(data) {
                                cx.write_to_clipboard(item);
                            }
                            changed = true;
                        }
                        RemoteDesktopHelperEvent::ClipboardFilesReady { paths, .. }
                            if self.profile.session_options.clipboard.files =>
                        {
                            self.last_clipboard_files = Some(gpui::hash(&paths));
                            self.observe_desktop(
                                oxide_audit::AuditCategory::File,
                                "desktop_file_transfer",
                                &format!("remote_to_local files={}", paths.len()),
                                oxide_audit::AuditOutcome::Sent,
                                None,
                            );
                            cx.write_to_clipboard(ClipboardItem {
                                entries: vec![ClipboardEntry::ExternalPaths(gpui::ExternalPaths(
                                    paths.into(),
                                ))],
                            });
                            changed = true;
                        }
                        RemoteDesktopHelperEvent::ClipboardTransferFailed {
                            transfer_id, ..
                        } => {
                            if let Some(audit) = self.file_audits.remove(&transfer_id) {
                                audit.finish(
                                    oxide_audit::AuditOutcome::Failed,
                                    oxide_audit::AuditEvidence::Protocol,
                                    None,
                                    None,
                                );
                            }
                            // Helper text may include remote paths or protocol
                            // details. Only a typed, content-free failure crosses
                            // into the workspace notification adapter.
                            intents.push(RemoteDesktopDeliveryIntent::ClipboardTransferFailed);
                            changed = true;
                        }
                        event @ (RemoteDesktopHelperEvent::VncRemoteFilesListed { .. }
                        | RemoteDesktopHelperEvent::VncRemoteFileListFailed { .. }
                        | RemoteDesktopHelperEvent::VncFileTransferProgress { .. }
                        | RemoteDesktopHelperEvent::VncFileTransferCompleted { .. }
                        | RemoteDesktopHelperEvent::VncFileTransferFailed { .. }) => {
                            match &event {
                                RemoteDesktopHelperEvent::VncFileTransferCompleted {
                                    transfer_id,
                                    paths,
                                    skipped_files,
                                    ..
                                } => {
                                    if let Some(mut audit) = self.file_audits.remove(transfer_id) {
                                        let bytes = self
                                            .vnc_files
                                            .transfer
                                            .as_ref()
                                            .filter(|transfer| &transfer.transfer_id == transfer_id)
                                            .map(|transfer| transfer.transferred_bytes);
                                        audit.summary(&format!(
                                            "remote_to_local files={} skipped={skipped_files}",
                                            paths.len()
                                        ));
                                        audit.finish(
                                            oxide_audit::AuditOutcome::Succeeded,
                                            oxide_audit::AuditEvidence::Protocol,
                                            None,
                                            bytes,
                                        );
                                    }
                                }
                                RemoteDesktopHelperEvent::VncFileTransferFailed {
                                    transfer_id,
                                    kind,
                                } => {
                                    if let Some(audit) = self.file_audits.remove(transfer_id) {
                                        let bytes = self
                                            .vnc_files
                                            .transfer
                                            .as_ref()
                                            .filter(|transfer| &transfer.transfer_id == transfer_id)
                                            .map(|transfer| transfer.transferred_bytes);
                                        audit.finish(
                                            if *kind
                                                == RemoteDesktopFileTransferFailureKind::Canceled
                                            {
                                                oxide_audit::AuditOutcome::Cancelled
                                            } else {
                                                oxide_audit::AuditOutcome::Failed
                                            },
                                            oxide_audit::AuditEvidence::Protocol,
                                            None,
                                            bytes,
                                        );
                                    }
                                }
                                _ => {}
                            }
                            if let Some(intent) = self.apply_vnc_file_event(event) {
                                intents.push(intent);
                            }
                            changed = true;
                        }
                        RemoteDesktopHelperEvent::ConnectionFailure { message, category } => {
                            self.finish_connection_audit(oxide_audit::AuditOutcome::Failed);
                            if self.has_connected
                                && self.automatic_reconnect_worker_generation != Some(generation)
                            {
                                self.observe_desktop(
                                    oxide_audit::AuditCategory::Connection,
                                    "desktop_disconnect",
                                    "connection failed",
                                    oxide_audit::AuditOutcome::Failed,
                                    None,
                                );
                            }
                            if category == Some(RemoteDesktopErrorCategory::Authentication)
                                && self
                                    .password
                                    .as_ref()
                                    .is_none_or(|password| password.is_empty())
                                && self.credential_prompt_generation != Some(generation)
                            {
                                self.credential_prompt_generation = Some(generation);
                                intents.push(RemoteDesktopDeliveryIntent::CredentialsRequired {
                                    generation,
                                });
                            }
                            if remote_desktop_network_failure_allows_automatic_reconnect(
                                self.has_connected,
                                category,
                            ) {
                                self.begin_automatic_reconnect(generation, window, cx);
                            } else {
                                self.state.apply_event(
                                    RemoteDesktopHelperEvent::ConnectionFailure {
                                        message,
                                        category,
                                    },
                                );
                                let retired_images = self.state.take_retired_images();
                                let retired_textures = self.state.take_retired_textures();
                                self.drop_images(retired_images, window, cx);
                                Self::drop_textures(retired_textures, window);
                            }
                            changed = true;
                        }
                        RemoteDesktopHelperEvent::Terminated { exit_code } => {
                            if self.automatic_reconnect_worker_generation == Some(generation) {
                                continue;
                            }
                            self.finish_connection_audit(oxide_audit::AuditOutcome::Failed);
                            if self.has_connected
                                && self.state.snapshot().status
                                    != RemoteDesktopSessionStatus::Disconnected
                            {
                                self.observe_desktop(
                                    oxide_audit::AuditCategory::Connection,
                                    "desktop_disconnect",
                                    "helper terminated",
                                    oxide_audit::AuditOutcome::Unknown,
                                    None,
                                );
                            }
                            self.state
                                .apply_event(RemoteDesktopHelperEvent::Terminated { exit_code });
                            let retired_images = self.state.take_retired_images();
                            let retired_textures = self.state.take_retired_textures();
                            self.drop_images(retired_images, window, cx);
                            Self::drop_textures(retired_textures, window);
                            // The terminal delivery closes the current ownership
                            // epoch and transfers its bounded join to the reaper.
                            self.shutdown_worker();
                            changed = true;
                        }
                        event => {
                            let connection_established = matches!(
                                &event,
                                RemoteDesktopHelperEvent::Connected { .. }
                                    | RemoteDesktopHelperEvent::Status {
                                        status: RemoteDesktopSessionStatus::Connected,
                                        ..
                                    }
                            );
                            let disconnected = matches!(
                                &event,
                                RemoteDesktopHelperEvent::Disconnected { .. }
                                    | RemoteDesktopHelperEvent::Status {
                                        status: RemoteDesktopSessionStatus::Disconnected,
                                        ..
                                    }
                            );
                            if disconnected {
                                self.finish_connection_audit(oxide_audit::AuditOutcome::Failed);
                                if self.state.snapshot().status
                                    == RemoteDesktopSessionStatus::Connected
                                {
                                    self.observe_desktop(
                                        oxide_audit::AuditCategory::Connection,
                                        "desktop_disconnect",
                                        "helper disconnected",
                                        oxide_audit::AuditOutcome::Succeeded,
                                        None,
                                    );
                                }
                            }
                            self.state.apply_event(event);
                            if connection_established {
                                self.mark_connection_established();
                                if window.is_window_active() {
                                    self.sync_local_file_clipboard(cx);
                                }
                            }
                            let retired_images = self.state.take_retired_images();
                            let retired_textures = self.state.take_retired_textures();
                            self.drop_images(retired_images, window, cx);
                            Self::drop_textures(retired_textures, window);
                            changed = true;
                        }
                    }
                }
                RemoteDesktopWorkerDelivery::TransportFailed {
                    tab_id,
                    generation,
                    message,
                } => {
                    debug_assert_eq!(tab_id, self.tab_id);
                    if self.worker_generation != generation {
                        continue;
                    }
                    self.finish_connection_audit(oxide_audit::AuditOutcome::Failed);
                    if self.has_connected
                        && self.automatic_reconnect_worker_generation != Some(generation)
                    {
                        self.observe_desktop(
                            oxide_audit::AuditCategory::Connection,
                            "desktop_disconnect",
                            "transport failed",
                            oxide_audit::AuditOutcome::Failed,
                            None,
                        );
                    }
                    if self.frame_slot.is_visible() {
                        let _ = self.apply_frame_ready(generation, window, cx);
                    }
                    if self.has_connected {
                        // A helper transport may be replaced only after this
                        // tab has proven that its profile can establish.
                        self.begin_automatic_reconnect(generation, window, cx);
                    } else {
                        self.state
                            .apply_event(RemoteDesktopHelperEvent::ConnectionFailure {
                                message,
                                category: Some(RemoteDesktopErrorCategory::Unknown),
                            });
                        let retired_images = self.state.take_retired_images();
                        let retired_textures = self.state.take_retired_textures();
                        self.drop_images(retired_images, window, cx);
                        Self::drop_textures(retired_textures, window);
                        // An initial helper transport failure remains terminal;
                        // retrying an unproven profile would hide setup errors.
                        self.shutdown_worker();
                    }
                    changed = true;
                }
            }
        }

        if changed {
            cx.notify();
        }
        RemoteDesktopDeliveryOutcome {
            changed,
            backlog_remaining: drain.outcome.backlog_remaining,
            intents,
        }
    }

    fn apply_frame_ready(
        &mut self,
        generation: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.worker_generation != generation || !self.frame_slot.is_visible() {
            return false;
        }
        let frame_slot = self.frame_slot.clone();
        let delay = frame_slot.next_frame_ready_delay();
        if !delay.is_zero() {
            self.schedule_frame_apply(generation, delay, cx);
            return false;
        }

        let mut events = Vec::new();
        let started_at = Instant::now();
        let mut budget_hit = false;
        for index in 0..REMOTE_DESKTOP_FRAME_READY_DRAIN_LIMIT {
            if index > 0 && started_at.elapsed() >= REMOTE_DESKTOP_FRAME_READY_DRAIN_BUDGET {
                budget_hit = true;
                break;
            }
            let Some(event) = frame_slot.take() else {
                break;
            };
            // Apply one bounded batch so image uploads cannot monopolize GPUI.
            events.push(event);
        }
        let drained_events = events.len();
        if drained_events == 0 {
            frame_slot.complete_delivery();
            return false;
        }

        frame_slot.mark_frame_presented();
        let apply_started_at = Instant::now();
        let apply_stats = self.state.apply_frame_events(events);
        if self.state.snapshot().status == RemoteDesktopSessionStatus::Connected {
            self.mark_connection_established();
        }
        let apply_elapsed = apply_started_at.elapsed();
        let retired_images = self.state.take_retired_images();
        let retired_textures = self.state.take_retired_textures();
        let retired_image_count = retired_images.len();
        self.render_diagnostics.record_batch(
            drained_events,
            budget_hit,
            apply_elapsed,
            apply_stats,
            retired_image_count,
        );
        if remote_desktop_diagnostics_enabled() {
            eprintln!(
                "[oxide:remote-desktop-render] tab={:?} protocol={:?} provider={} resize={} clipboard_data={} gen={generation} trace={:?}->{:?} drained={drained_events} budget_hit={budget_hit} apply_us={} full={} updates={} dirty_applied={} dirty_rejected={} dirty_px={} dirty_frame_px={} pending_texture_updates={} pending_texture_bytes={} texture_updates={} textures_created={} retired={} full_update_recoveries={} totals={:?}",
                self.tab_id,
                self.profile.protocol,
                self.provider.id,
                self.provider.capabilities.resize,
                self.provider.capabilities.clipboard_data,
                apply_stats.first_trace_id,
                apply_stats.last_trace_id,
                duration_micros_u64(apply_elapsed),
                apply_stats.full_frames,
                apply_stats.frame_updates,
                apply_stats.dirty_updates_applied,
                apply_stats.dirty_updates_rejected,
                apply_stats.dirty_rect_pixels,
                apply_stats.dirty_frame_pixels,
                apply_stats.pending_texture_updates,
                apply_stats.pending_texture_upload_bytes,
                apply_stats.dirty_tiles_refreshed,
                apply_stats.frame_tiles_created,
                retired_image_count,
                apply_stats.full_update_recoveries,
                self.render_diagnostics,
            );
        }
        self.drop_images(retired_images, window, cx);
        Self::drop_textures(retired_textures, window);
        if frame_slot.complete_delivery() && frame_slot.mark_frame_ready_queued() {
            self.schedule_frame_apply(generation, frame_slot.next_frame_ready_delay(), cx);
        }
        true
    }

    fn spawn_worker(
        &self,
        generation: u64,
        profile: RemoteDesktopConnectionProfile,
        provider: RemoteDesktopProviderManifest,
        password_available: bool,
        frame_slot: RemoteDesktopFrameDeliverySlot,
        worker_wake: RemoteDesktopWorkerWake,
        initial_size: RemoteDesktopSize,
        scale_factor: Option<u32>,
        monitor_layout: RemoteDesktopMonitorLayout,
        delivery_tx: mpsc::Sender<RemoteDesktopWorkerDelivery>,
    ) -> RemoteDesktopWorkerOwner {
        let tab_id = self.tab_id;
        let (request_tx, request_rx) = mpsc::channel();
        let worker_thread = thread::Builder::new()
            .name(format!("remote-desktop-{}", tab_id.0))
            .spawn(move || {
                run_remote_desktop_worker(
                    tab_id,
                    generation,
                    profile,
                    provider,
                    password_available,
                    initial_size,
                    scale_factor,
                    monitor_layout,
                    frame_slot,
                    worker_wake,
                    request_rx,
                    delivery_tx,
                );
            })
            .expect("failed to start remote desktop worker");
        RemoteDesktopWorkerOwner::new(request_tx, worker_thread)
    }

    fn start_worker(
        &mut self,
        initial_request_size: RemoteDesktopSize,
        initial_viewport_size: Option<RemoteDesktopSize>,
        scale_factor: Option<u32>,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.worker.is_some() {
            return false;
        }
        if !self.refresh_plugin_provider(cx) {
            return false;
        }
        self.start_connection_audit("desktop_connect", None);
        let profile = self.profile.clone();
        let provider = self.provider.clone();
        let password_available = self
            .password
            .as_ref()
            .is_some_and(|password| !password.is_empty());
        let frame_slot = self.frame_slot.clone();
        let delivery_tx = self.delivery_tx.clone();
        let generation = next_remote_desktop_worker_generation(self.worker_generation);
        let worker_wake = RemoteDesktopWorkerWake::default();
        let monitor_layout = remote_desktop_monitor_layout(&profile, cx);
        let worker = self.spawn_worker(
            generation,
            profile,
            provider,
            password_available,
            frame_slot,
            worker_wake.clone(),
            initial_request_size,
            scale_factor,
            monitor_layout.clone(),
            delivery_tx,
        );

        self.worker = Some(worker);
        let previous_worker_wake = self.worker_wake.replace(worker_wake.clone());
        self.worker_generation = generation;
        self.certificate_challenge = None;
        self.last_viewport_size = initial_viewport_size;
        self.last_sent_resize = None;
        self.last_viewport_scale_factor = scale_factor;
        self.last_monitor_layout = monitor_layout;
        self.last_lock_keys = None;
        self.wheel_pixel_remainder = remote_desktop_empty_wheel_delta();
        self.state.apply_event(RemoteDesktopHelperEvent::Status {
            status: RemoteDesktopSessionStatus::Connecting,
            message: None,
        });
        if let Some(previous_worker_wake) = previous_worker_wake {
            previous_worker_wake.stop();
        }
        // Store the generation before consuming a wake emitted during startup.
        self.schedule_worker_wake(generation, worker_wake, cx);
        true
    }

    fn restart_worker_preserving_frame(
        &mut self,
        source: oxide_audit::AuditSource,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.refresh_plugin_provider(cx) {
            return;
        }
        self.start_connection_audit("desktop_reconnect", Some(source));
        let (initial_request_size, initial_viewport_size) =
            initial_remote_desktop_sizes_for_session(self);
        let profile = self.profile.clone();
        let provider = self.provider.clone();
        let password_available = self
            .password
            .as_ref()
            .is_some_and(|password| !password.is_empty());
        let generation = next_remote_desktop_worker_generation(self.worker_generation);
        let scale_factor = self.last_viewport_scale_factor;
        self.shutdown_worker();

        let frame_slot = RemoteDesktopFrameDeliverySlot::new();
        // Helper replacement preserves presentation visibility independently
        // from the worker lifetime.
        frame_slot.set_visible(self.frame_slot.is_visible());
        let worker_wake = RemoteDesktopWorkerWake::default();
        let monitor_layout = remote_desktop_monitor_layout(&profile, cx);
        let worker = self.spawn_worker(
            generation,
            profile,
            provider,
            password_available,
            frame_slot.clone(),
            worker_wake.clone(),
            initial_request_size,
            scale_factor,
            monitor_layout.clone(),
            self.delivery_tx.clone(),
        );
        let previous_worker_wake = self.worker_wake.replace(worker_wake.clone());
        // Automatic transport recovery keeps the last complete frame but starts
        // the replacement helper with a fresh graphics epoch.
        self.state.begin_transport_reconnect();
        let old_images = self.state.take_retired_images();
        let old_textures = self.state.take_retired_textures();
        self.frame_slot = frame_slot;
        self.worker = Some(worker);
        self.worker_generation = generation;
        self.certificate_challenge = None;
        self.last_viewport_size = initial_viewport_size;
        self.last_sent_resize = None;
        self.last_viewport_scale_factor = scale_factor;
        self.last_monitor_layout = monitor_layout;
        self.resize_generation = Arc::new(AtomicU64::new(0));
        self.last_lock_keys = None;
        self.wheel_pixel_remainder = remote_desktop_empty_wheel_delta();
        if let Some(previous_worker_wake) = previous_worker_wake {
            previous_worker_wake.stop();
        }
        self.drop_images(old_images, window, cx);
        Self::drop_textures(old_textures, window);
        self.schedule_worker_wake(generation, worker_wake, cx);
    }

    fn sync_monitor_layout(&mut self, cx: &mut Context<Self>) {
        if !self.profile.session_options.display.use_all_monitors {
            return;
        }
        let layout = remote_desktop_monitor_layout(&self.profile, cx);
        if layout == self.last_monitor_layout {
            return;
        }
        if let Some(worker) = self.worker.as_ref() {
            worker.send(RemoteDesktopHelperRequest::UpdateDisplayLayout {
                layout: layout.clone(),
            });
        }
        self.last_monitor_layout = layout;
    }

    fn resize_capability(&self) -> NegotiatedCapabilityStatus {
        self.state
            .snapshot()
            .negotiated_capabilities
            .as_ref()
            .map(|capabilities| capabilities.resize)
            .unwrap_or(NegotiatedCapabilityStatus::Unknown)
    }

    fn send_resize_immediately(
        &mut self,
        size: RemoteDesktopSize,
        scale_factor: Option<u32>,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.state.snapshot().status != RemoteDesktopSessionStatus::Connected
            || !self.resize_capability().is_supported()
        {
            return false;
        }
        let Some(worker) = self.worker.as_ref() else {
            return false;
        };
        let size = RemoteDesktopSize::clamped(size.width, size.height);
        let resize_request = RemoteDesktopResizeRequestState { size, scale_factor };

        // Explicit actions cancel any pending layout debounce and cross the
        // existing session-owned helper channel without changing its lifetime.
        self.resize_generation.fetch_add(1, Ordering::Relaxed);
        self.last_sent_resize = Some(resize_request);
        self.state.mark_resize_requested(size);
        worker.send(RemoteDesktopHelperRequest::Resize { size, scale_factor });
        cx.notify();
        true
    }

    fn fit_current_viewport_immediately(
        &mut self,
        scale_factor: u32,
        cx: &mut Context<Self>,
    ) -> bool {
        self.last_viewport_scale_factor = Some(scale_factor);
        let Some(viewport_size) = self.geometry.viewport_size() else {
            return false;
        };
        let viewport_size = RemoteDesktopSize::clamped(viewport_size.width, viewport_size.height);
        self.last_viewport_size = Some(viewport_size);
        let request_size = remote_desktop_requested_size_for_viewport(
            viewport_size,
            self.last_viewport_scale_factor,
        );
        self.send_resize_immediately(request_size, self.last_viewport_scale_factor, cx)
    }

    fn set_follow_window_size(
        &mut self,
        enabled: bool,
        scale_factor: u32,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.follow_window_size == enabled {
            return false;
        }
        self.follow_window_size = enabled;
        if enabled {
            self.fit_current_viewport_immediately(scale_factor, cx);
        }
        true
    }

    fn request_preset_size(
        &mut self,
        size: RemoteDesktopSize,
        scale_factor: u32,
        cx: &mut Context<Self>,
    ) -> bool {
        // A fixed preset and automatic viewport following are mutually
        // exclusive; otherwise the next local layout pass would overwrite it.
        self.follow_window_size = false;
        self.last_viewport_scale_factor = Some(scale_factor);
        self.send_resize_immediately(size, Some(scale_factor), cx)
    }

    pub(super) fn schedule_viewport_resize(
        &mut self,
        scale_factor: Option<u32>,
        cx: &mut Context<Self>,
    ) -> bool {
        if let Some(scale_factor) = scale_factor {
            // Layout is measured after render; keep the first physical scale
            // before deciding whether the helper can start.
            self.last_viewport_scale_factor = Some(scale_factor);
        }
        let snapshot = self.state.snapshot();
        let Some(viewport_size) = self.geometry.viewport_size() else {
            return false;
        };
        let viewport_size = RemoteDesktopSize::clamped(viewport_size.width, viewport_size.height);
        let request_size = remote_desktop_requested_size_for_viewport(
            viewport_size,
            self.last_viewport_scale_factor,
        );
        let resize_request = RemoteDesktopResizeRequestState {
            size: request_size,
            scale_factor: self.last_viewport_scale_factor,
        };
        if self.worker.is_none() {
            if self.last_viewport_scale_factor.is_none() {
                return false;
            }
            if matches!(
                snapshot.status,
                RemoteDesktopSessionStatus::Idle
                    | RemoteDesktopSessionStatus::Connecting
                    | RemoteDesktopSessionStatus::Reconnecting
            ) {
                return self.start_worker(
                    request_size,
                    Some(viewport_size),
                    self.last_viewport_scale_factor,
                    cx,
                );
            }
            return false;
        }
        if snapshot.status != RemoteDesktopSessionStatus::Connected {
            return false;
        }
        if !self.follow_window_size {
            return false;
        }
        let should_send_resize = remote_desktop_resize_request_needed_for_capability(
            self.resize_capability().is_supported(),
            snapshot.size,
            snapshot.pending_resize,
            self.last_viewport_size,
            self.last_sent_resize,
            viewport_size,
            request_size,
            self.last_viewport_scale_factor,
        );
        if Some(viewport_size) == self.last_viewport_size && !should_send_resize {
            return false;
        }
        self.last_viewport_size = Some(viewport_size);
        if !should_send_resize {
            return false;
        }

        self.last_sent_resize = Some(resize_request);
        self.state.mark_resize_requested(request_size);
        let generation = self.resize_generation.fetch_add(1, Ordering::Relaxed) + 1;
        let resize_generation = self.resize_generation.clone();
        let Some(request_tx) = self
            .worker
            .as_ref()
            .and_then(RemoteDesktopWorkerOwner::request_sender_cloned)
        else {
            return true;
        };
        thread::Builder::new()
            .name("remote-desktop-resize-debounce".to_string())
            .spawn(move || {
                thread::sleep(REMOTE_DESKTOP_RESIZE_DEBOUNCE);
                if resize_generation.load(Ordering::Relaxed) == generation {
                    let _ = request_tx.send(RemoteDesktopHelperRequest::Resize {
                        size: resize_request.size,
                        scale_factor: resize_request.scale_factor,
                    });
                }
            })
            .ok();
        true
    }

    pub(super) fn schedule_initial_layout_probe(
        &mut self,
        initial_scale_factor: u32,
        cx: &mut Context<Self>,
    ) {
        self.last_viewport_scale_factor = Some(initial_scale_factor);
        if self.schedule_viewport_resize(None, cx) {
            cx.notify();
        }
        if self.worker.is_some() {
            return;
        }

        cx.spawn(async move |session, cx| {
            for _ in 0..REMOTE_DESKTOP_INITIAL_LAYOUT_PROBE_TICKS {
                Timer::after(REMOTE_DESKTOP_INITIAL_LAYOUT_PROBE_INTERVAL).await;
                let done = session
                    .update(cx, |session, cx| {
                        if session.worker.is_some() {
                            return true;
                        }
                        // The worker must start from the measured viewport even
                        // before it has any delivery capable of waking the UI.
                        if session.schedule_viewport_resize(None, cx) {
                            cx.notify();
                        }
                        session.worker.is_some()
                    })
                    .unwrap_or(true);
                if done {
                    break;
                }
            }
        })
        .detach();
    }

    fn schedule_window_event(&self, event: RemoteDesktopSessionEvent, cx: &mut Context<Self>) {
        cx.spawn(async move |session, cx| {
            let generation = match event {
                RemoteDesktopSessionEvent::DeliveryReady { generation }
                | RemoteDesktopSessionEvent::FrameApplyReady { generation } => generation,
                RemoteDesktopSessionEvent::PluginRequired { .. }
                | RemoteDesktopSessionEvent::CredentialsRequired { .. }
                | RemoteDesktopSessionEvent::ClipboardTransferFailed
                | RemoteDesktopSessionEvent::VncFileTransferCompleted
                | RemoteDesktopSessionEvent::VncFileTransferFailed(_) => return,
            };
            let window_handle = session
                .update(cx, |session, _cx| {
                    (session.worker_generation == generation).then_some(session.window_handle)
                })
                .ok()
                .flatten();
            let Some(window_handle) = window_handle else {
                return;
            };

            let _ = cx.update_window(window_handle, move |_, window, cx| {
                let _ = session.update(cx, |session, cx| {
                    if session.worker_generation != generation {
                        return;
                    }
                    match event {
                        RemoteDesktopSessionEvent::DeliveryReady { .. } => {
                            let outcome = session.poll_deliveries(window, cx);
                            session.sync_monitor_layout(cx);
                            if outcome.changed {
                                cx.notify();
                            }
                            for intent in outcome.intents {
                                match intent {
                                    RemoteDesktopDeliveryIntent::CredentialsRequired {
                                        generation,
                                    } => {
                                        cx.emit(RemoteDesktopSessionEvent::CredentialsRequired {
                                            generation,
                                        });
                                    }
                                    RemoteDesktopDeliveryIntent::ClipboardTransferFailed => {
                                        cx.emit(RemoteDesktopSessionEvent::ClipboardTransferFailed);
                                    }
                                    RemoteDesktopDeliveryIntent::VncFileTransferCompleted => {
                                        cx.emit(
                                            RemoteDesktopSessionEvent::VncFileTransferCompleted,
                                        );
                                    }
                                    RemoteDesktopDeliveryIntent::VncFileTransferFailed(kind) => {
                                        cx.emit(RemoteDesktopSessionEvent::VncFileTransferFailed(
                                            kind,
                                        ));
                                    }
                                }
                            }
                            if outcome.backlog_remaining
                                && let Some(worker_wake) = session.worker_wake.as_ref()
                            {
                                worker_wake.mark();
                            }
                        }
                        RemoteDesktopSessionEvent::FrameApplyReady { .. } => {
                            if session.apply_frame_ready(generation, window, cx) {
                                cx.notify();
                            }
                        }
                        RemoteDesktopSessionEvent::PluginRequired { .. }
                        | RemoteDesktopSessionEvent::CredentialsRequired { .. }
                        | RemoteDesktopSessionEvent::ClipboardTransferFailed
                        | RemoteDesktopSessionEvent::VncFileTransferCompleted
                        | RemoteDesktopSessionEvent::VncFileTransferFailed(_) => {}
                    }
                });
            });
        })
        .detach();
    }

    fn drop_images(
        &self,
        images: Vec<Arc<gpui::RenderImage>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for image in images {
            // Dynamic remote tiles remain in the sprite atlas until explicitly released.
            cx.drop_image(image, Some(window));
        }
    }

    fn drop_textures(textures: Vec<Arc<gpui::DynamicTexture>>, window: &mut Window) {
        for texture in textures {
            let _ = window.drop_dynamic_texture(texture);
        }
    }

    fn set_frame_visibility(&mut self, visible: bool, cx: &mut Context<Self>) {
        self.ui_frame_visible = visible;
        if !visible {
            self.clipboard_activation = None;
        }
        self.apply_frame_visibility(cx);
    }

    pub(super) fn apply_frame_visibility(&mut self, cx: &mut Context<Self>) {
        let effective_visible = self.ui_frame_visible;
        let visibility_changed = self.frame_slot.is_visible() != effective_visible;
        let recovery_required = self.frame_slot.set_visible(effective_visible);
        if recovery_required
            && let Some(request_tx) = self
                .worker
                .as_ref()
                .and_then(RemoteDesktopWorkerOwner::request_sender)
        {
            // The worker remains alive while hidden; request one base frame so
            // sparse deltas do not become an unbounded off-screen history.
            let _ = request_tx.send(RemoteDesktopHelperRequest::RequestFrame);
        }
        if effective_visible && visibility_changed && self.frame_slot.has_queued_frame_events() {
            self.schedule_frame_apply(self.worker_generation, Duration::ZERO, cx);
        }
    }
}

impl WorkspaceApp {
    fn prompt_remote_desktop_credentials(
        &mut self,
        session: &Entity<RemoteDesktopSessionEntity>,
        generation: u64,
        cx: &mut Context<Self>,
    ) {
        let current = session.read(cx);
        if current.worker_generation != generation {
            return;
        }
        let window_handle = current.window_handle;
        let needs_username = current.profile.protocol == RemoteDesktopProtocol::Rdp
            && current
                .profile
                .username
                .as_deref()
                .is_none_or(|name| name.trim().is_empty());
        let mut prompts = Vec::new();
        if needs_username {
            prompts.push(oxide_ssh::KeyboardInteractivePrompt {
                prompt: self.i18n.t("ssh.form.username"),
                echo: true,
            });
        }
        prompts.push(oxide_ssh::KeyboardInteractivePrompt {
            prompt: self.i18n.t("ssh.form.password"),
            echo: false,
        });
        let request = oxide_ssh::KeyboardInteractivePromptRequest {
            flow_id: uuid::Uuid::new_v4().to_string(),
            name: format!(
                "{}@{}",
                current.profile.username.as_deref().unwrap_or_default(),
                current.profile.endpoint.format_authority()
            ),
            instructions: String::new(),
            prompts,
            chained: false,
        };
        let weak_session = session.downgrade();
        let task = cx.spawn(async move |workspace, cx| {
            let (sender, receiver) = tokio::sync::oneshot::channel();
            let _ = cx.update_window(window_handle, |_, window, cx| {
                let _ = workspace.update(cx, |workspace, cx| {
                    workspace.open_keyboard_interactive_challenge(request, sender, window, cx)
                });
            });
            let Ok(Ok(mut responses)) = receiver.await else {
                return;
            };
            if responses.len() != if needs_username { 2 } else { 1 } {
                return;
            }
            let _ = cx.update_window(window_handle, |_, window, cx| {
                let _ = weak_session.update(cx, |session, cx| {
                    // A closed or replaced transport cannot consume a stale prompt response.
                    if session.worker_generation != generation {
                        return;
                    }
                    if needs_username {
                        session.profile.username = Some(std::mem::take(&mut responses[0]));
                    }
                    let index = usize::from(needs_username);
                    session.password = Some(RemoteDesktopSecret::from(Zeroizing::new(
                        std::mem::take(&mut responses[index]),
                    )));
                    session.restart_worker_preserving_frame(
                        oxide_audit::AuditSource::User,
                        window,
                        cx,
                    );
                    cx.notify();
                });
            });
        });
        session.update(cx, |session, _| session.credential_prompt_task = Some(task));
    }

    pub(in crate::workspace) fn handle_remote_desktop_session_event(
        &mut self,
        tab_id: TabId,
        session_entity: &Entity<RemoteDesktopSessionEntity>,
        event: &RemoteDesktopSessionEvent,
        cx: &mut Context<Self>,
    ) {
        debug_assert_eq!(session_entity.read(cx).tab_id, tab_id);
        match event {
            RemoteDesktopSessionEvent::PluginRequired { protocol } => {
                self.show_missing_remote_desktop_plugin(*protocol, cx);
                return;
            }
            RemoteDesktopSessionEvent::CredentialsRequired { generation } => {
                self.prompt_remote_desktop_credentials(session_entity, *generation, cx);
                return;
            }
            RemoteDesktopSessionEvent::ClipboardTransferFailed => {
                self.push_command_palette_toast(
                    self.i18n.t("remote_desktop.clipboard_file_failed"),
                    None,
                    TerminalNoticeVariant::Error,
                    cx,
                );
                return;
            }
            RemoteDesktopSessionEvent::VncFileTransferCompleted => {
                self.push_command_palette_toast(
                    self.i18n.t("remote_desktop.file_download_completed"),
                    None,
                    TerminalNoticeVariant::Success,
                    cx,
                );
                return;
            }
            RemoteDesktopSessionEvent::VncFileTransferFailed(kind) => {
                let key = match kind {
                    RemoteDesktopFileTransferFailureKind::Remote => {
                        "remote_desktop.file_download_failed_remote"
                    }
                    RemoteDesktopFileTransferFailureKind::Local => {
                        "remote_desktop.file_download_failed_local"
                    }
                    RemoteDesktopFileTransferFailureKind::Protocol => {
                        "remote_desktop.file_download_failed_protocol"
                    }
                    RemoteDesktopFileTransferFailureKind::Canceled => {
                        "remote_desktop.file_download_canceled"
                    }
                };
                self.push_command_palette_toast(
                    self.i18n.t(key),
                    None,
                    if *kind == RemoteDesktopFileTransferFailureKind::Canceled {
                        TerminalNoticeVariant::Default
                    } else {
                        TerminalNoticeVariant::Error
                    },
                    cx,
                );
                return;
            }
            RemoteDesktopSessionEvent::DeliveryReady { .. }
            | RemoteDesktopSessionEvent::FrameApplyReady { .. } => {}
        }

        let visible = self.remote_desktop_tab_visible(tab_id, cx);
        // The root supplies cross-tab visibility, then the Entity owns the
        // window-affine delivery and lifecycle transition.
        session_entity.update(cx, |session, cx| {
            session.set_frame_visibility(visible, cx);
            session.schedule_window_event(*event, cx);
        });
    }

    pub(in crate::workspace) fn remote_desktop_session_entity(
        &self,
        tab_id: TabId,
        cx: &App,
    ) -> Option<Entity<RemoteDesktopSessionEntity>> {
        self.remote_desktop.read(cx).session(tab_id)
    }

    pub(in crate::workspace) fn bind_remote_desktop_window(
        &mut self,
        tab_id: TabId,
        window_handle: AnyWindowHandle,
        cx: &mut Context<Self>,
    ) {
        if let Some(session) = self.remote_desktop_session_entity(tab_id, cx) {
            // Window affinity is a lifecycle property: delivery and resource
            // cleanup must follow the tab across detach and dock transitions.
            session.update(cx, |session, _cx| {
                session.bind_window(window_handle);
            });
        }
    }

    pub(in crate::workspace) fn remote_desktop_tab_visible(&self, tab_id: TabId, cx: &App) -> bool {
        let tab_host = self.tab_host.read(cx);
        let main_tab_visible = tab_host.surface_is_visible(tab_id) && !tab_host.is_detached(tab_id);
        let detached_tab_visible = tab_host.is_detached(tab_id);
        remote_desktop_tab_visible(main_tab_visible, detached_tab_visible)
    }

    pub(in crate::workspace) fn resume_remote_desktop_frame_delivery(
        &self,
        tab_id: TabId,
        cx: &mut Context<Self>,
    ) {
        let Some(session_entity) = self.remote_desktop_session_entity(tab_id, cx) else {
            return;
        };
        // Hidden tabs retain one coalesced frame. Visibility resumes that frame
        // without restarting or disconnecting either protocol worker.
        session_entity.update(cx, |session, cx| session.set_frame_visibility(true, cx));
    }

    pub(in crate::workspace) fn sync_remote_desktop_frame_visibility(
        &self,
        tab_id: TabId,
        cx: &mut App,
    ) {
        let visible = self.remote_desktop_tab_visible(tab_id, cx);
        if let Some(session_entity) = self.remote_desktop_session_entity(tab_id, cx) {
            session_entity.update(cx, |session, cx| {
                session.set_frame_visibility(visible, cx);
            });
        }
    }

    pub(in crate::workspace) fn close_remote_desktop_tab(
        &mut self,
        tab_id: TabId,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        if let Some(session) = self.remote_desktop_session_entity(tab_id, cx) {
            session.update(cx, |session, cx| session.shutdown(window, cx));
        }
        if self.remote_desktop_resize_menu_tab_id == Some(tab_id) {
            self.remote_desktop_resize_menu_tab_id = None;
        }
        self.remote_desktop
            .update(cx, |remote_desktop, _cx| remote_desktop.remove(tab_id));
    }

    pub(in crate::workspace) fn release_remote_desktop_inputs_for_tab(
        &mut self,
        tab_id: TabId,
        cx: &mut Context<Self>,
    ) {
        let mut pages = vec![tab_id];
        if let Some(root) = self
            .tab_by_id(tab_id, cx)
            .and_then(|tab| tab.root_pane.as_ref())
        {
            root.collect_page_ids(&mut pages);
        }
        for page in pages {
            if let Some(session) = self.remote_desktop_session_entity(page, cx) {
                session.update(cx, |session, _cx| session.release_inputs());
            }
        }
    }

    pub(in crate::workspace) fn release_active_remote_desktop_inputs(
        &mut self,
        cx: &mut Context<Self>,
    ) {
        if let Some(tab_id) = self.active_remote_desktop_tab_id(cx) {
            self.release_remote_desktop_inputs_for_tab(tab_id, cx);
        }
    }

    pub(in crate::workspace) fn focus_remote_desktop_keyboard(
        &mut self,
        tab_id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The remote surface stops root mouse propagation, so it must run the
        // same blur path that an outside workspace click would normally run.
        self.blur_text_inputs(cx);

        let mut changed = self.clear_ime_selection();
        changed |= self.ime_marked_text.take().is_some();
        changed |= self.pending_platform_text_commit.take().is_some();

        self.sync_remote_desktop_lock_keys(tab_id, window.capslock(), cx);
        window.focus(&self.focus_handle, cx);
        if changed {
            cx.notify();
        }
    }

    fn fit_remote_desktop_to_current_viewport(
        &mut self,
        tab_id: TabId,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.remote_desktop_session_entity(tab_id, cx) else {
            return;
        };
        let scale_factor = remote_desktop_scale_factor_percent(window.scale_factor());
        session.update(cx, |session, cx| {
            session.fit_current_viewport_immediately(scale_factor, cx);
        });
    }

    fn toggle_remote_desktop_resize_menu(&mut self, tab_id: TabId) {
        self.remote_desktop_resize_menu_tab_id =
            (self.remote_desktop_resize_menu_tab_id != Some(tab_id)).then_some(tab_id);
    }

    fn set_remote_desktop_follow_window_size(
        &mut self,
        tab_id: TabId,
        enabled: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.remote_desktop_session_entity(tab_id, cx) else {
            return;
        };
        let scale_factor = remote_desktop_scale_factor_percent(window.scale_factor());
        session.update(cx, |session, cx| {
            if session.set_follow_window_size(enabled, scale_factor, cx) {
                cx.notify();
            }
        });
    }

    fn set_remote_desktop_preset_size(
        &mut self,
        tab_id: TabId,
        size: RemoteDesktopSize,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.remote_desktop_session_entity(tab_id, cx) else {
            return;
        };
        let scale_factor = remote_desktop_scale_factor_percent(window.scale_factor());
        session.update(cx, |session, cx| {
            session.request_preset_size(size, scale_factor, cx);
        });
    }

    fn render_remote_desktop_resize_control(
        &self,
        tab_id: TabId,
        menu_open: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = self.theme_tokens.ui;
        let workspace = cx.entity();
        let fit_button = self.workspace_toolbar_action_button(
            self.i18n.t("remote_desktop.fit_current_window"),
            Some(Self::render_lucide_icon(
                LucideIcon::Monitor,
                12.0,
                rgb(theme.text_muted),
            )),
            ToolbarButtonOptions {
                button: ButtonOptions {
                    variant: ButtonVariant::Secondary,
                    size: ButtonSize::Sm,
                    radius: ButtonRadius::Md,
                    disabled: false,
                },
                height: Some(24.0),
                padding_x: Some(8.0),
                font_size: Some(self.theme_tokens.metrics.ui_text_xs),
                ..ToolbarButtonOptions::default()
            },
            cx.listener(move |this, _event, window, cx| {
                this.fit_remote_desktop_to_current_viewport(tab_id, window, cx);
                cx.stop_propagation();
            }),
        );
        let menu_button = self.workspace_icon_action_button(
            LucideIcon::ChevronDown,
            12.0,
            rgb(theme.text_muted),
            IconButtonOptions {
                radius: ButtonRadius::Md,
                has_background: menu_open,
                ..IconButtonOptions::compact(24.0)
            },
            move |this, _event, _window, cx| {
                this.toggle_remote_desktop_resize_menu(tab_id);
                cx.stop_propagation();
                cx.notify();
            },
            cx,
        );
        let trigger = div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(self.theme_tokens.spacing.one))
            .child(fit_button)
            .child(menu_button);

        // Keep the split-button bounds warm so the first menu open can be
        // placed above the footer without relying on pointer coordinates.
        select_anchor_probe(
            SelectAnchorId::RemoteDesktopResizeMenu(tab_id.0),
            trigger,
            move |anchor, _window, cx| {
                let _ = workspace.update(cx, |this, cx| {
                    this.update_select_anchor(anchor, cx);
                });
            },
        )
        .into_any_element()
    }

    pub(super) fn render_remote_desktop_resize_menu(
        &self,
        tab_id: TabId,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(anchor) = self
            .select_anchors
            .get(&SelectAnchorId::RemoteDesktopResizeMenu(tab_id.0))
            .copied()
        else {
            return div().into_any_element();
        };
        let Some(session_entity) = self.remote_desktop_session_entity(tab_id, cx) else {
            return div().into_any_element();
        };
        let session = session_entity.read(cx);
        let snapshot = session.state.snapshot();
        let follow_window_size = session.follow_window_size;
        let current_size = snapshot.size;
        let viewport = window.viewport_size();
        let actionable_or_value_count = 2 + REMOTE_DESKTOP_COMMON_RESOLUTIONS.len();
        let menu_label_count = 2;
        let menu_separator_count = 2.0;
        let menu_height = context_menu_item_height_estimate(&self.theme_tokens)
            * (actionable_or_value_count + menu_label_count) as f32
            + context_menu_separator_height_estimate(&self.theme_tokens) * menu_separator_count
            + self.theme_tokens.metrics.ui_menu_padding * 2.0;
        let placement = browser_behavior::clamp_context_menu_position(
            f32::from(anchor.bounds.right()) - REMOTE_DESKTOP_RESIZE_MENU_WIDTH,
            f32::from(anchor.bounds.top()) - menu_height - REMOTE_DESKTOP_RESIZE_MENU_GAP,
            f32::from(viewport.width),
            f32::from(viewport.height),
            REMOTE_DESKTOP_RESIZE_MENU_WIDTH,
            menu_height,
            REMOTE_DESKTOP_RESIZE_MENU_VIEWPORT_PADDING,
        );
        let follow_item = dropdown_menu_item(
            &self.theme_tokens,
            self.i18n.t("remote_desktop.follow_window_size"),
            DropdownMenuItemKind::Checkbox(follow_window_size),
            false,
            false,
        );
        let mut menu = context_menu_event_boundary(
            dropdown_menu_content(&self.theme_tokens).w(px(REMOTE_DESKTOP_RESIZE_MENU_WIDTH)),
        )
        .child(self.workspace_context_menu_persistent_styled_action(
            follow_item,
            false,
            false,
            oxide_ui::context_menu::ContextMenuActionableStyle::default(),
            move |this, _event, window, cx| {
                this.set_remote_desktop_follow_window_size(tab_id, !follow_window_size, window, cx);
            },
            cx,
        ))
        .child(dropdown_menu_separator(&self.theme_tokens))
        .child(dropdown_menu_label(
            &self.theme_tokens,
            self.i18n.t("remote_desktop.current_remote_size"),
            false,
        ));
        let current_size_label = current_size
            .map(remote_desktop_resolution_label)
            .unwrap_or_else(|| self.i18n.t("remote_desktop.size_unavailable"));
        menu = menu
            .child(dropdown_menu_item(
                &self.theme_tokens,
                current_size_label,
                DropdownMenuItemKind::Plain,
                false,
                true,
            ))
            .child(dropdown_menu_separator(&self.theme_tokens))
            .child(dropdown_menu_label(
                &self.theme_tokens,
                self.i18n.t("remote_desktop.common_resolutions"),
                false,
            ));
        for size in REMOTE_DESKTOP_COMMON_RESOLUTIONS {
            let selected = !follow_window_size && current_size == Some(size);
            let item = dropdown_menu_item(
                &self.theme_tokens,
                remote_desktop_resolution_label(size),
                DropdownMenuItemKind::Radio(selected),
                false,
                false,
            );
            menu = menu.child(self.workspace_context_menu_styled_action(
                item,
                false,
                false,
                oxide_ui::context_menu::ContextMenuActionableStyle::default(),
                |this| this.remote_desktop_resize_menu_tab_id = None,
                move |this, _event, window, cx| {
                    this.set_remote_desktop_preset_size(tab_id, size, window, cx);
                },
                cx,
            ));
        }

        deferred(
            anchored()
                .anchor(Corner::TopLeft)
                .position(gpui::point(px(placement.x), px(placement.y)))
                .position_mode(AnchoredPositionMode::Window)
                .child(overlay_content_boundary(menu)),
        )
        .with_priority(oxide_ui::modal::TAURI_POPOVER_LAYER_PRIORITY)
        .into_any_element()
    }

    pub(in crate::workspace) fn render_remote_desktop_footer(
        &self,
        tab_id: TabId,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(session_entity) = self.remote_desktop_session_entity(tab_id, cx) else {
            return div().into_any_element();
        };
        let session = session_entity.read(cx);
        let theme = self.theme_tokens.ui;
        let snapshot = session.state.snapshot();
        let status = snapshot.status;
        let status_color = remote_desktop_status_color(&self.theme_tokens, status);
        let reconnect_disabled = !remote_desktop_reconnect_enabled(status);
        let resize_capability = snapshot
            .negotiated_capabilities
            .as_ref()
            .map(|capabilities| capabilities.resize)
            .unwrap_or_else(|| {
                if session.provider.capabilities.resize {
                    NegotiatedCapabilityStatus::Unknown
                } else {
                    NegotiatedCapabilityStatus::Unsupported
                }
            });
        let resize_capability_label = match resize_capability {
            NegotiatedCapabilityStatus::Supported => {
                self.i18n.t("remote_desktop.fit_current_window")
            }
            NegotiatedCapabilityStatus::Unsupported => self.i18n.t("remote_desktop.resize_fixed"),
            NegotiatedCapabilityStatus::Unknown => self.i18n.t("remote_desktop.resize_unknown"),
        };
        let resize_menu_open = self.remote_desktop_resize_menu_tab_id == Some(tab_id);
        let vnc_file_download_available = session.vnc_file_download_available();
        let vnc_capability_presentation =
            (snapshot.protocol == RemoteDesktopProtocol::Vnc).then(|| {
                self.remote_desktop_vnc_capability_presentation(
                    snapshot.negotiated_capabilities.as_ref(),
                )
            });
        let label = format!(
            "{} · {}:{}",
            session.provider.name, session.profile.endpoint.host, session.profile.endpoint.port
        );

        div()
            .flex_none()
            .h(px(oxide_ui::WORKSPACE_STATUS_BAR_HEIGHT))
            .px(px(14.0))
            .flex()
            .items_center()
            .gap(px(self.theme_tokens.spacing.one))
            .bg(rgb(theme.bg_panel))
            .border_t_1()
            .border_color(rgba((theme.border << 8) | 0x80))
            .child(remote_desktop_protocol_chip(
                &self.theme_tokens,
                snapshot.protocol,
            ))
            .child(
                div()
                    .size(px(7.0))
                    .rounded_full()
                    .bg(rgb(status_color))
                    .flex_none(),
            )
            .child(
                div()
                    .min_w(px(0.0))
                    .flex_1()
                    .truncate()
                    .text_size(px(self.theme_tokens.metrics.ui_text_xs))
                    .text_color(rgb(theme.text_muted))
                    .child(label),
            )
            .when(resize_capability.is_supported(), |footer| {
                footer.child(self.render_remote_desktop_resize_control(
                    tab_id,
                    resize_menu_open,
                    cx,
                ))
            })
            .when(!resize_capability.is_supported(), |footer| {
                footer.child(remote_desktop_capability_chip(
                    &self.theme_tokens,
                    resize_capability_label,
                ))
            })
            .when_some(
                vnc_capability_presentation,
                |footer, (capability_label, capability_tooltip)| {
                    let tooltip_for_move = capability_tooltip;
                    footer.child(
                        remote_desktop_capability_chip(&self.theme_tokens, capability_label)
                            .id("remote-desktop-vnc-capabilities")
                            .on_mouse_move(cx.listener(
                                move |this, event: &MouseMoveEvent, _window, cx| {
                                    this.queue_workspace_tooltip(
                                        "remote-desktop-vnc-capabilities",
                                        tooltip_for_move.clone(),
                                        f32::from(event.position.x) + 12.0,
                                        f32::from(event.position.y) + 16.0,
                                        cx,
                                    );
                                },
                            ))
                            .on_hover(cx.listener(move |this, hovered: &bool, _window, cx| {
                                if !*hovered {
                                    this.clear_workspace_tooltip(
                                        "remote-desktop-vnc-capabilities",
                                        cx,
                                    );
                                }
                            })),
                    )
                },
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(self.theme_tokens.spacing.one))
                    .when(vnc_file_download_available, |actions| {
                        actions.child(self.workspace_toolbar_action_button(
                            self.i18n.t("remote_desktop.file_browser"),
                            Some(Self::render_lucide_icon(
                                LucideIcon::FolderOpen,
                                12.0,
                                rgb(theme.text_muted),
                            )),
                            ToolbarButtonOptions {
                                button: ButtonOptions {
                                    variant: ButtonVariant::Secondary,
                                    size: ButtonSize::Sm,
                                    radius: ButtonRadius::Md,
                                    disabled: false,
                                },
                                height: Some(24.0),
                                padding_x: Some(8.0),
                                font_size: Some(self.theme_tokens.metrics.ui_text_xs),
                                ..ToolbarButtonOptions::default()
                            },
                            cx.listener(move |this, _event, _window, cx| {
                                this.open_vnc_file_browser(tab_id, cx);
                            }),
                        ))
                    })
                    .child(self.workspace_toolbar_action_button(
                        self.i18n.t("remote_desktop.reconnect"),
                        Some(Self::render_lucide_icon(
                            LucideIcon::RefreshCw,
                            12.0,
                            rgb(theme.text_muted),
                        )),
                        ToolbarButtonOptions {
                            button: ButtonOptions {
                                variant: ButtonVariant::Secondary,
                                size: ButtonSize::Sm,
                                radius: ButtonRadius::Md,
                                disabled: reconnect_disabled,
                            },
                            height: Some(24.0),
                            padding_x: Some(8.0),
                            font_size: Some(self.theme_tokens.metrics.ui_text_xs),
                            ..ToolbarButtonOptions::default()
                        },
                        cx.listener(move |this, _event, window, cx| {
                            this.reconnect_remote_desktop(tab_id, window, cx);
                            cx.notify();
                        }),
                    ))
                    .child(self.workspace_toolbar_action_button(
                        self.i18n.t("remote_desktop.disconnect"),
                        Some(Self::render_lucide_icon(
                            LucideIcon::Power,
                            12.0,
                            rgb(theme.text_muted),
                        )),
                        ToolbarButtonOptions {
                            button: ButtonOptions {
                                variant: ButtonVariant::Destructive,
                                size: ButtonSize::Sm,
                                radius: ButtonRadius::Md,
                                disabled: false,
                            },
                            height: Some(24.0),
                            padding_x: Some(8.0),
                            font_size: Some(self.theme_tokens.metrics.ui_text_xs),
                            ..ToolbarButtonOptions::default()
                        },
                        cx.listener(move |this, _event, window, cx| {
                            this.release_remote_desktop_inputs_for_tab(tab_id, cx);
                            this.disconnect_remote_desktop(tab_id, window, cx);
                            cx.notify();
                        }),
                    )),
            )
            .into_any_element()
    }

    fn remote_desktop_vnc_capability_presentation(
        &self,
        capabilities: Option<&NegotiatedCapabilities>,
    ) -> (String, String) {
        let unknown_capabilities = NegotiatedCapabilities::default();
        let is_pending = capabilities.is_none();
        let capabilities = capabilities.unwrap_or(&unknown_capabilities);
        let supported = self.i18n.t("remote_desktop.capability_supported");
        let unsupported = self.i18n.t("remote_desktop.capability_unsupported");
        let unknown = self.i18n.t("remote_desktop.capability_unknown");
        let status = |value: NegotiatedCapabilityStatus| match value {
            NegotiatedCapabilityStatus::Supported => supported.as_str(),
            NegotiatedCapabilityStatus::Unsupported => unsupported.as_str(),
            NegotiatedCapabilityStatus::Unknown => unknown.as_str(),
        };
        let feature_statuses = [
            capabilities.resize,
            capabilities.multi_monitor,
            capabilities.extended_clipboard,
            capabilities.tight,
            capabilities.jpeg,
            capabilities.continuous_updates,
            capabilities.fence,
            capabilities.last_rect,
            capabilities.h264,
            capabilities.qemu_audio,
            capabilities.vendor_files,
            capabilities.extended_key_events,
            capabilities.extended_mouse_buttons,
            capabilities.lock_key_sync,
        ];
        let supported_count = feature_statuses
            .iter()
            .filter(|value| value.is_supported())
            .count();
        let label = if is_pending {
            self.i18n.t("remote_desktop.capabilities_pending")
        } else {
            self.i18n
                .t("remote_desktop.capabilities_summary")
                .replace("{{supported}}", &supported_count.to_string())
                .replace("{{total}}", &feature_statuses.len().to_string())
        };
        let security_method = capabilities
            .selected_security_method
            .as_deref()
            .or_else(|| capabilities.security_methods.first().map(String::as_str))
            .unwrap_or(unknown.as_str());
        let clipboard_formats = match capabilities.extended_clipboard {
            NegotiatedCapabilityStatus::Supported
                if !capabilities.extended_clipboard_formats.is_empty() =>
            {
                capabilities.extended_clipboard_formats.join(", ")
            }
            value => status(value).to_string(),
        };
        let lines = [
            format!(
                "{}: {security_method}",
                self.i18n.t("remote_desktop.capability_security_method")
            ),
            format!(
                "{}: {}",
                self.i18n.t("remote_desktop.capability_encryption"),
                status(capabilities.encrypted)
            ),
            format!(
                "{}: {}",
                self.i18n.t("remote_desktop.capability_identity_verified"),
                status(capabilities.peer_identity_verified)
            ),
            format!(
                "{}: {}",
                self.i18n.t("remote_desktop.capability_resize"),
                status(capabilities.resize)
            ),
            format!(
                "{}: {}",
                self.i18n.t("remote_desktop.capability_multi_monitor"),
                status(capabilities.multi_monitor)
            ),
            format!(
                "{}: {clipboard_formats}",
                self.i18n.t("remote_desktop.capability_extended_clipboard")
            ),
            format!("Tight: {}", status(capabilities.tight)),
            format!("JPEG: {}", status(capabilities.jpeg)),
            format!(
                "{}: {}",
                self.i18n.t("remote_desktop.capability_continuous_updates"),
                status(capabilities.continuous_updates)
            ),
            format!("Fence: {}", status(capabilities.fence)),
            format!("LastRect: {}", status(capabilities.last_rect)),
            format!("H.264: {}", status(capabilities.h264)),
            format!("QEMU Audio: {}", status(capabilities.qemu_audio)),
            format!(
                "{}: {}",
                self.i18n.t("remote_desktop.capability_vendor_files"),
                status(capabilities.vendor_files)
            ),
            format!(
                "{}: {}",
                self.i18n.t("remote_desktop.capability_extended_keys"),
                status(capabilities.extended_key_events)
            ),
            format!(
                "{}: {}",
                self.i18n.t("remote_desktop.capability_extended_mouse"),
                status(capabilities.extended_mouse_buttons)
            ),
            format!(
                "{}: {}",
                self.i18n.t("remote_desktop.capability_lock_sync"),
                status(capabilities.lock_key_sync)
            ),
        ];

        (label, lines.join("\n"))
    }

    pub(in crate::workspace) fn disconnect_remote_desktop(
        &mut self,
        tab_id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let surface = standalone_connections::StandaloneConnectionSurface::RemoteDesktop(tab_id);
        if let Some(connection_id) = self
            .standalone_connections
            .connection_id_for_surface(surface)
        {
            self.disconnect_standalone_connection(&connection_id, window, cx);
        }
    }

    pub(in crate::workspace) fn reconnect_remote_desktop(
        &mut self,
        tab_id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let surface = standalone_connections::StandaloneConnectionSurface::RemoteDesktop(tab_id);
        if let Some(connection_id) = self
            .standalone_connections
            .connection_id_for_surface(surface)
        {
            self.reconnect_standalone_connection(&connection_id, window, cx);
        }
    }
}

fn remote_desktop_monitor_layout(
    profile: &RemoteDesktopConnectionProfile,
    cx: &App,
) -> RemoteDesktopMonitorLayout {
    if !profile.session_options.display.use_all_monitors {
        return RemoteDesktopMonitorLayout::default();
    }

    let displays = cx.displays();
    let primary_id = cx
        .primary_display()
        .map(|display| display.id())
        .or_else(|| {
            displays
                .iter()
                .find(|display| {
                    let bounds = display.physical_bounds();
                    i32::from(bounds.origin.x) == 0 && i32::from(bounds.origin.y) == 0
                })
                .map(|display| display.id())
        })
        .or_else(|| displays.first().map(|display| display.id()));
    let Some(primary_id) = primary_id else {
        return RemoteDesktopMonitorLayout::default();
    };
    let Some(primary_bounds) = displays
        .iter()
        .find(|display| display.id() == primary_id)
        .map(|display| display.physical_bounds())
    else {
        return RemoteDesktopMonitorLayout::default();
    };
    let primary_left = i32::from(primary_bounds.origin.x);
    let primary_top = i32::from(primary_bounds.origin.y);

    let mut monitors = displays
        .into_iter()
        .filter_map(|display| {
            let bounds = display.physical_bounds();
            let width = u32::try_from(i32::from(bounds.size.width)).ok()?;
            let height = u32::try_from(i32::from(bounds.size.height)).ok()?;
            if width < 200 || height < 200 {
                return None;
            }
            let width = width.min(8192) & !1;
            let height = height.min(8192);
            let primary = display.id() == primary_id;
            let desktop_scale_factor = (display.scale_factor()
                * REMOTE_DESKTOP_SCALE_PERCENT_MULTIPLIER)
                .round()
                .clamp(
                    REMOTE_DESKTOP_MIN_SCALE_FACTOR_PERCENT as f32,
                    REMOTE_DESKTOP_MAX_SCALE_FACTOR_PERCENT as f32,
                ) as u32;
            let device_scale_factor = match desktop_scale_factor {
                0..=120 => 100,
                121..=160 => 140,
                _ => 180,
            };

            Some(RemoteDesktopMonitor {
                stable_id: display
                    .uuid()
                    .map(|uuid| uuid.to_string())
                    .unwrap_or_else(|_| format!("display-{}", u64::from(display.id()))),
                left: i32::from(bounds.origin.x).saturating_sub(primary_left),
                top: i32::from(bounds.origin.y).saturating_sub(primary_top),
                width,
                height,
                primary,
                desktop_scale_factor,
                device_scale_factor,
                physical_width_mm: None,
                physical_height_mm: None,
                orientation: if width >= height {
                    RemoteDesktopMonitorOrientation::Landscape
                } else {
                    RemoteDesktopMonitorOrientation::Portrait
                },
            })
        })
        .collect::<Vec<_>>();
    monitors.sort_by_key(|monitor| (!monitor.primary, monitor.top, monitor.left));

    RemoteDesktopMonitorLayout { monitors }
}
