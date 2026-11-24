use super::*;

use gpui::Task;
use oxide_connection_monitor::ResourceSampler;
use oxide_editor_core::utf16::replace_utf16;
use oxide_topology::ConnectionTopologySnapshot;

fn finish_host_query_audit(
    audit: oxide_audit::AuditOperation,
    result: &Result<oxide_ssh::SshCommandOutput, ()>,
) {
    let exit_code = result.as_ref().ok().and_then(|output| output.exit_code);
    let output_bytes = result
        .as_ref()
        .ok()
        .map(|output| output.stdout.len().saturating_add(output.stderr.len()) as u64);
    let outcome = match (result, exit_code) {
        (Err(_), _) => oxide_audit::AuditOutcome::Failed,
        (_, Some(0)) => oxide_audit::AuditOutcome::Succeeded,
        (_, None) => oxide_audit::AuditOutcome::Unknown,
        (_, Some(_)) => oxide_audit::AuditOutcome::Failed,
    };
    audit.finish(
        outcome,
        if exit_code.is_some() {
            oxide_audit::AuditEvidence::ExitCode
        } else {
            oxide_audit::AuditEvidence::Protocol
        },
        exit_code,
        output_bytes,
    );
}

#[derive(Clone)]
struct HostToolsSshConsumerContext {
    node_router: NodeRouter,
    prompt_handler: Arc<dyn SshPromptHandler>,
    managed_key_resolver: ManagedKeyResolver,
}

/// Owns Host Tools sampling state independently from WorkspaceApp and SSH nodes.
pub(in crate::workspace) struct HostToolsEntity {
    // This handle is private to the Entity. Host Tools exposes snapshots and
    // sampler acquisition only; page code cannot disconnect shared nodes.
    ssh_registry: SshConnectionRegistry,
    ssh_consumer_context: Option<HostToolsSshConsumerContext>,
    pub(super) profiler_registry: ProfilerRegistry,
    pub(super) profiler_update_tx: tokio::sync::mpsc::Sender<()>,
    pub(super) sampler_delivery_wake: crate::workspace::delivery::ActiveDeliveryWake,
    pub(super) sampler_delivery_rx:
        std::sync::mpsc::Receiver<super::delivery::HostToolsSamplerDelivery>,
    // Reliable user-action results have their own wake and budget so sampler
    // traffic cannot delay a completed Host Tools operation.
    pub(super) reliable_delivery_wake: crate::workspace::delivery::ActiveDeliveryWake,
    pub(super) reliable_delivery_tx: crate::workspace::delivery::ActiveDeliverySender<
        super::delivery::HostToolsReliableDelivery,
    >,
    pub(super) reliable_delivery_rx:
        std::sync::mpsc::Receiver<super::delivery::HostToolsReliableDelivery>,
    // Search, list, expansion, and dialog state belongs to the shared Host
    // Tools surface rather than whichever workspace mount renders it.
    pub(in crate::workspace) ui: HostToolsUiState,
    pub(super) host_process_actions: HostProcessActionsState,
    pub(super) host_docker_operations: HostDockerOperationsState,
    pub(super) host_services: HostServicesState,
    pub(super) host_tmux: HostTmuxState,
    pub(super) host_gpu: HostGpuViewState,
    pub(super) host_logs: HostLogsState,
    pub(super) host_ports: HostPortsState,
    pub(super) host_filesystems: HostFilesystemsState,
    pub(super) host_packages: HostPackagesState,
    pub(super) host_schedules: HostSchedulesState,
    pub(in crate::workspace) active_runtime_section: ConnectionRuntimeSection,
    pub(in crate::workspace) previous_runtime_section: ConnectionRuntimeSection,
    selected_connection_id: Option<String>,
    selector_open: bool,
    selector_highlighted_index: Option<usize>,
    selector_focus_origin: Option<browser_behavior::BrowserFocusOrigin>,
    // Tool state survives sidebar navigation and visibility changes.
    active_tool: HostTools,
    // Visibility is an Entity lifecycle input. It controls only page-scoped
    // sampling and never owns or releases shared SSH transports.
    pub(super) visibility: HostToolsVisibility,
    // These inputs let the Entity refresh its own samplers after an action.
    // The runtime handle has no registry release or disconnect capability.
    pub(super) lifecycle_runtime: Option<tokio::runtime::Handle>,
    pub(super) sampling_config: oxide_connection_monitor::ResourceSamplingConfig,
    // Host Tools receives one read-only settings snapshot. Persistent writes
    // remain owned by SettingsStore and are applied back through lifecycle.
    pub(super) monitoring: oxide_settings::HostToolsSettings,
    pub(super) messages: Option<HostToolsMessages>,
    pub(super) lifecycle_refresh_task: Option<Task<()>>,
    pool_stats: Option<ConnectionPoolMonitorStats>,
    pool_summaries: Vec<ConnectionPoolEntrySummary>,
    topology_snapshot: Option<ConnectionTopologySnapshot>,
    last_pool_refresh: Option<Instant>,
    // Topology interactions belong to the shared Host Tools surface, not to
    // the workspace window that happens to render the graph.
    pub(super) topology_transform: TopologyTransform,
    pub(super) topology_drag: Option<TopologyDragState>,
    pub(super) topology_menu: Option<TopologyNodeMenuState>,
    compact_monitor_list_state: ListState,
    compact_monitor_list_cache: RefCell<VirtualListSignatureCache>,
}

impl HostToolsEntity {
    fn host_tool_audit(
        &self,
        connection_id: &str,
        action: &str,
        detail: Option<&str>,
    ) -> oxide_audit::AuditOperation {
        let context = self
            .ssh_registry
            .get(connection_id)
            .and_then(|handle| handle.audit_context());
        if let Some(context) = context {
            oxide_audit::AuditOperation::in_request(
                Some(&context),
                oxide_audit::AuditCategory::Host,
                action,
                detail,
            )
        } else {
            oxide_audit::AuditOperation::begin(
                oxide_audit::AuditCategory::Host,
                action,
                Some(connection_id),
                detail,
            )
        }
    }

    pub(in crate::workspace) fn window_modal_snapshot(
        &self,
    ) -> Option<HostToolsWindowModalSnapshot> {
        if self.host_schedules.logs_dialog.is_some() {
            Some(HostToolsWindowModalSnapshot::ScheduleLogs)
        } else if let Some(confirm) = self.host_schedules.pending_confirm.as_ref() {
            Some(HostToolsWindowModalSnapshot::ScheduleConfirm(
                confirm.presence.phase(),
            ))
        } else if self.ui.host_tmux_input_dialog.is_some() {
            Some(HostToolsWindowModalSnapshot::TmuxInput)
        } else if let Some(confirm) = self.host_tmux.pending_confirm.as_ref() {
            Some(HostToolsWindowModalSnapshot::TmuxConfirm(
                confirm.presence.phase(),
            ))
        } else if self.host_services.logs_dialog.is_some() {
            Some(HostToolsWindowModalSnapshot::ServiceLogs)
        } else if let Some(confirm) = self.host_services.pending_confirm.as_ref() {
            Some(HostToolsWindowModalSnapshot::ServiceConfirm(
                confirm.presence.phase(),
            ))
        } else if self.host_docker_operations.logs_dialog.is_some() {
            Some(HostToolsWindowModalSnapshot::DockerLogs)
        } else if let Some(confirm) = self.host_docker_operations.pending_confirm.as_ref() {
            Some(HostToolsWindowModalSnapshot::DockerConfirm(
                confirm.presence.phase(),
            ))
        } else {
            self.host_process_actions
                .pending_confirm
                .as_ref()
                .map(|confirm| {
                    HostToolsWindowModalSnapshot::ProcessConfirm(confirm.presence.phase())
                })
        }
    }

    pub(in crate::workspace) fn new(
        profiler_update_tx: tokio::sync::mpsc::Sender<()>,
        profiler_update_rx: tokio::sync::mpsc::Receiver<()>,
        ssh_registry: SshConnectionRegistry,
        cx: &mut Context<Self>,
    ) -> Self {
        let sampler_delivery_wake = crate::workspace::delivery::ActiveDeliveryWake::default();
        let (sampler_delivery_tx, sampler_delivery_rx) =
            crate::workspace::delivery::ActiveDeliverySender::channel_with_wake(
                sampler_delivery_wake.clone(),
            );
        let (gpu_update_tx, gpu_update_rx) = tokio::sync::mpsc::unbounded_channel();
        let reliable_delivery_wake = crate::workspace::delivery::ActiveDeliveryWake::default();
        let (reliable_delivery_tx, reliable_delivery_rx) =
            crate::workspace::delivery::ActiveDeliverySender::channel_with_wake(
                reliable_delivery_wake.clone(),
            );
        let mut entity = Self {
            ssh_registry,
            ssh_consumer_context: None,
            profiler_registry: ProfilerRegistry::new(),
            profiler_update_tx,
            sampler_delivery_wake,
            sampler_delivery_rx,
            reliable_delivery_wake,
            reliable_delivery_tx,
            reliable_delivery_rx,
            ui: HostToolsUiState::new(),
            host_process_actions: HostProcessActionsState::new(),
            host_docker_operations: HostDockerOperationsState::new(),
            host_services: HostServicesState::new(),
            host_tmux: HostTmuxState::new(),
            host_gpu: HostGpuViewState::new(gpu_update_tx),
            host_logs: HostLogsState::new(),
            host_ports: HostPortsState::new(),
            host_filesystems: HostFilesystemsState::new(),
            host_packages: HostPackagesState::new(),
            host_schedules: HostSchedulesState::new(),
            active_runtime_section: ConnectionRuntimeSection::Overview,
            previous_runtime_section: ConnectionRuntimeSection::Overview,
            selected_connection_id: None,
            selector_open: false,
            selector_highlighted_index: None,
            selector_focus_origin: None,
            active_tool: HostTools::Monitor,
            visibility: HostToolsVisibility::Hidden,
            lifecycle_runtime: None,
            sampling_config: oxide_connection_monitor::ResourceSamplingConfig::default(),
            monitoring: oxide_settings::HostToolsSettings::default(),
            messages: None,
            lifecycle_refresh_task: None,
            pool_stats: None,
            pool_summaries: Vec::new(),
            topology_snapshot: None,
            last_pool_refresh: None,
            topology_transform: TopologyTransform::default(),
            topology_drag: None,
            topology_menu: None,
            compact_monitor_list_state: tauri_virtual_list_state(
                0,
                ListAlignment::Top,
                TauriVirtualListSpec::new(
                    px(COMPACT_MONITOR_LIST_ESTIMATED_ROW_HEIGHT),
                    COMPACT_MONITOR_LIST_OVERSCAN,
                ),
            ),
            compact_monitor_list_cache: RefCell::new(VirtualListSignatureCache::default()),
        };
        entity.schedule_sampler_delivery(
            super::delivery::HostToolsDeliveryBridges {
                profiler_update_rx,
                gpu_update_rx,
                sampler_delivery_tx,
            },
            cx,
        );
        entity.schedule_reliable_delivery(cx);
        entity.schedule_lifecycle_refresh(cx);
        entity
    }

    pub(in crate::workspace) fn set_ssh_consumer_context(
        &mut self,
        node_router: NodeRouter,
        prompt_handler: Arc<dyn SshPromptHandler>,
        managed_key_resolver: ManagedKeyResolver,
    ) {
        self.ssh_consumer_context = Some(HostToolsSshConsumerContext {
            node_router,
            prompt_handler,
            managed_key_resolver,
        });
    }

    pub(in crate::workspace) fn replace_text_input(
        &mut self,
        input: HostToolsTextInput,
        replacement_range: Option<std::ops::Range<usize>>,
        text: &str,
    ) -> bool {
        let Some(value) = self.ui.input_value_mut(input) else {
            return false;
        };
        replace_utf16(value, replacement_range, text);
        // Search edits invalidate expansion identities owned by the same
        // surface; command inputs preserve their surrounding action state.
        match input {
            HostToolsTextInput::ProcessSearch => self.ui.host_process_expanded_pid = None,
            HostToolsTextInput::DockerSearch => self.ui.host_docker_expanded_id = None,
            HostToolsTextInput::ServiceSearch => self.ui.host_service_expanded_id = None,
            HostToolsTextInput::LogSearch => self.host_logs.expanded_index = None,
            HostToolsTextInput::TmuxSearch => {
                self.ui.host_tmux_expanded_session_id = None;
                self.ui.host_tmux_expanded_window_id = None;
            }
            HostToolsTextInput::PortSearch => self.host_ports.expanded_index = None,
            HostToolsTextInput::ScheduleSearch => self.host_schedules.expanded_index = None,
            HostToolsTextInput::FilesystemSearch => self.host_filesystems.expanded_index = None,
            HostToolsTextInput::PackageSearch => self.host_packages.expanded_index = None,
            HostToolsTextInput::ProcessRenice | HostToolsTextInput::TmuxDialog => {}
        }
        true
    }

    pub(in crate::workspace) fn profiler_registry(&self) -> &ProfilerRegistry {
        &self.profiler_registry
    }

    pub(in crate::workspace) fn refresh_pool_snapshot(&mut self, cx: &mut Context<Self>) {
        self.pool_stats = Some(self.ssh_registry.monitor_stats());
        self.pool_summaries = self.ssh_registry.list_connection_summaries();
        self.topology_snapshot = Some(self.ssh_registry.connection_topology_snapshot());
        self.last_pool_refresh = Some(Instant::now());
        cx.notify();
    }

    pub(super) fn pool_refresh_is_stale(&self, interval: Duration) -> bool {
        self.last_pool_refresh
            .is_none_or(|last_refresh| last_refresh.elapsed() >= interval)
    }

    pub(super) fn pool_stats_snapshot(&self) -> Option<ConnectionPoolMonitorStats> {
        self.pool_stats.clone()
    }

    pub(super) fn pool_summaries_snapshot(&self) -> Vec<ConnectionPoolEntrySummary> {
        // Runtime views receive an immutable projection instead of borrowing
        // the registry-owned cache across GPUI rendering callbacks.
        self.pool_summaries.clone()
    }

    pub(super) fn topology_snapshot(&self) -> Option<ConnectionTopologySnapshot> {
        self.topology_snapshot.clone()
    }

    pub(super) fn monitor_connections(&self) -> Vec<MonitorConnectionOption> {
        if !self.pool_summaries.is_empty() {
            return self
                .pool_summaries
                .iter()
                .filter(|summary| summary.is_displayed_in_pool())
                .map(MonitorConnectionOption::from_pool_summary)
                .collect();
        }

        let mut connections = self
            .ssh_registry
            .list()
            .into_iter()
            .map(MonitorConnectionOption::from_connection_info)
            .collect::<Vec<_>>();
        connections.sort_by(|left, right| {
            monitor_connection_label(left).cmp(&monitor_connection_label(right))
        });
        connections
    }

    pub(super) fn connection_os_type(&self, connection_id: &str) -> Option<String> {
        self.ssh_registry.get(connection_id).map(|handle| {
            handle
                .remote_env()
                .map(|environment| environment.os_type)
                .unwrap_or_else(|| "Unknown".to_string())
        })
    }

    pub(super) fn remote_env_detection_running(&self, connection_id: &str) -> bool {
        self.ssh_registry
            .get(connection_id)
            .is_some_and(|handle| handle.remote_env_detection_running())
    }

    pub(super) fn spawn_log_snapshot_capture(
        &self,
        command: String,
        request: HostLogSnapshotRequest,
        timeout: Duration,
        max_output_size: usize,
        runtime: tokio::runtime::Handle,
    ) -> bool {
        let audit = request.feedback.should_toast().then(|| {
            self.host_tool_audit(
                &request.connection_id,
                "query_host_logs",
                Some(&format!(
                    "preset={:?} limit={}",
                    request.preset, request.limit
                )),
            )
        });
        let Some(handle) = self.ssh_registry.get(&request.connection_id) else {
            if let Some(audit) = audit {
                audit.finish(
                    oxide_audit::AuditOutcome::Failed,
                    oxide_audit::AuditEvidence::Request,
                    None,
                    None,
                );
            }
            return false;
        };
        let delivery_tx = self.reliable_delivery_tx.clone();
        // Only the generated log command crosses the task boundary. The
        // registry-owned handle keeps credentials and node lifetime private.
        runtime.spawn(async move {
            let result = handle
                .run_command_capture(&command, timeout, max_output_size)
                .await
                .map_err(|_| ());
            if let Some(audit) = audit {
                finish_host_query_audit(audit, &result);
            }
            let _ = delivery_tx.send(super::delivery::HostToolsReliableDelivery::LogSnapshot(
                HostLogSnapshotDelivery { request, result },
            ));
        });
        true
    }

    pub(super) fn spawn_port_snapshot_capture(
        &self,
        command: String,
        request: HostPortSnapshotRequest,
        timeout: Duration,
        max_output_size: usize,
        runtime: tokio::runtime::Handle,
    ) -> bool {
        let audit = request
            .feedback
            .should_toast()
            .then(|| self.host_tool_audit(&request.connection_id, "query_host_ports", None));
        let Some(handle) = self.ssh_registry.get(&request.connection_id) else {
            if let Some(audit) = audit {
                audit.finish(
                    oxide_audit::AuditOutcome::Failed,
                    oxide_audit::AuditEvidence::Request,
                    None,
                    None,
                );
            }
            return false;
        };
        let delivery_tx = self.reliable_delivery_tx.clone();
        // The registry handle keeps authentication and node ownership inside
        // the SSH runtime while the generated diagnostic command runs.
        runtime.spawn(async move {
            let result = handle
                .run_command_capture(&command, timeout, max_output_size)
                .await
                .map_err(|_| ());
            if let Some(audit) = audit {
                finish_host_query_audit(audit, &result);
            }
            let _ = delivery_tx.send(super::delivery::HostToolsReliableDelivery::PortSnapshot(
                HostPortSnapshotDelivery { request, result },
            ));
        });
        true
    }

    pub(super) fn spawn_filesystem_snapshot_capture(
        &self,
        command: String,
        request: HostFilesystemSnapshotRequest,
        timeout: Duration,
        max_output_size: usize,
        runtime: tokio::runtime::Handle,
    ) -> bool {
        let audit = request
            .feedback
            .should_toast()
            .then(|| self.host_tool_audit(&request.connection_id, "query_host_filesystems", None));
        let Some(handle) = self.ssh_registry.get(&request.connection_id) else {
            if let Some(audit) = audit {
                audit.finish(
                    oxide_audit::AuditOutcome::Failed,
                    oxide_audit::AuditEvidence::Request,
                    None,
                    None,
                );
            }
            return false;
        };
        let delivery_tx = self.reliable_delivery_tx.clone();
        // Filesystem capture uses the registry-owned transport without moving
        // credentials or node lifecycle control into the page task.
        runtime.spawn(async move {
            let result = handle
                .run_command_capture(&command, timeout, max_output_size)
                .await
                .map_err(|_| ());
            if let Some(audit) = audit {
                finish_host_query_audit(audit, &result);
            }
            let _ = delivery_tx.send(
                super::delivery::HostToolsReliableDelivery::FilesystemSnapshot(
                    HostFilesystemSnapshotDelivery { request, result },
                ),
            );
        });
        true
    }

    pub(super) fn spawn_package_snapshot_capture(
        &self,
        command: String,
        request: HostPackageSnapshotRequest,
        timeout: Duration,
        max_output_size: usize,
        runtime: tokio::runtime::Handle,
    ) -> bool {
        let audit = request
            .feedback
            .should_toast()
            .then(|| self.host_tool_audit(&request.connection_id, "query_host_packages", None));
        let Some(handle) = self.ssh_registry.get(&request.connection_id) else {
            if let Some(audit) = audit {
                audit.finish(
                    oxide_audit::AuditOutcome::Failed,
                    oxide_audit::AuditEvidence::Request,
                    None,
                    None,
                );
            }
            return false;
        };
        let delivery_tx = self.reliable_delivery_tx.clone();
        // Package inventory uses the registry-owned transport and cannot
        // release the shared node or inspect its authentication material.
        runtime.spawn(async move {
            let result = handle
                .run_command_capture(&command, timeout, max_output_size)
                .await
                .map_err(|_| ());
            if let Some(audit) = audit {
                finish_host_query_audit(audit, &result);
            }
            let _ = delivery_tx.send(super::delivery::HostToolsReliableDelivery::PackageSnapshot(
                HostPackageSnapshotDelivery { request, result },
            ));
        });
        true
    }

    pub(super) fn spawn_schedule_snapshot_capture(
        &self,
        command: String,
        request: HostScheduleSnapshotRequest,
        timeout: Duration,
        max_output_size: usize,
        runtime: tokio::runtime::Handle,
    ) -> bool {
        let audit = request
            .feedback
            .should_toast()
            .then(|| self.host_tool_audit(&request.connection_id, "query_host_schedules", None));
        let Some(handle) = self.ssh_registry.get(&request.connection_id) else {
            if let Some(audit) = audit {
                audit.finish(
                    oxide_audit::AuditOutcome::Failed,
                    oxide_audit::AuditEvidence::Request,
                    None,
                    None,
                );
            }
            return false;
        };
        let delivery_tx = self.reliable_delivery_tx.clone();
        // Scheduled-task inventory runs on the registry-owned transport and
        // cannot acquire node release or authentication capabilities.
        runtime.spawn(async move {
            let result = handle
                .run_command_capture(&command, timeout, max_output_size)
                .await
                .map_err(|_| ());
            if let Some(audit) = audit {
                finish_host_query_audit(audit, &result);
            }
            let _ = delivery_tx.send(
                super::delivery::HostToolsReliableDelivery::ScheduleSnapshot(
                    HostScheduleSnapshotDelivery { request, result },
                ),
            );
        });
        true
    }

    pub(super) fn spawn_schedule_logs_capture(
        &self,
        command: String,
        request: HostScheduleLogsRequest,
        timeout: Duration,
        max_output_size: usize,
        runtime: tokio::runtime::Handle,
    ) -> bool {
        let audit = self.host_tool_audit(
            &request.connection_id,
            "query_scheduled_task_logs",
            Some(&format!("task_id={}", request.task_id)),
        );
        let Some(handle) = self.ssh_registry.get(&request.connection_id) else {
            audit.finish(
                oxide_audit::AuditOutcome::Failed,
                oxide_audit::AuditEvidence::Request,
                None,
                None,
            );
            return false;
        };
        let delivery_tx = self.reliable_delivery_tx.clone();
        // Raw log output stays inside the Entity delivery and dialog state.
        runtime.spawn(async move {
            let result = handle
                .run_command_capture(&command, timeout, max_output_size)
                .await
                .map_err(|_| ());
            finish_host_query_audit(audit, &result);
            let _ = delivery_tx.send(super::delivery::HostToolsReliableDelivery::ScheduleLogs(
                HostScheduleLogsDelivery { request, result },
            ));
        });
        true
    }

    pub(super) fn spawn_schedule_action(
        &self,
        command: String,
        request: HostScheduleActionRequest,
        timeout: Duration,
        max_output_size: usize,
        runtime: tokio::runtime::Handle,
    ) -> bool {
        let audit = self.host_tool_audit(
            &request.connection_id,
            "scheduled_task_action",
            Some(&format!(
                "task={} action={:?}",
                request.task_id, request.action
            )),
        );
        let Some(handle) = self.ssh_registry.get(&request.connection_id) else {
            audit.finish(
                oxide_audit::AuditOutcome::Failed,
                oxide_audit::AuditEvidence::Request,
                None,
                None,
            );
            return false;
        };
        let delivery_tx = self.reliable_delivery_tx.clone();
        // Action output is reduced to a success bit before it enters delivery.
        runtime.spawn(async move {
            let mut audit_exit_code = None;
            let result = handle
                .run_command_capture(&command, timeout, max_output_size)
                .await
                .map(|mut output| {
                    audit_exit_code = output.exit_code;
                    let succeeded = output.exit_code.unwrap_or(0) == 0;
                    // Captured action text may contain credentials and has no UI consumer.
                    zeroize::Zeroize::zeroize(&mut output.stdout);
                    zeroize::Zeroize::zeroize(&mut output.stderr);
                    succeeded
                })
                .map_err(|_| ());
            let audit_outcome = match (&result, audit_exit_code) {
                (Err(_), _) => oxide_audit::AuditOutcome::Failed,
                (_, Some(0)) => oxide_audit::AuditOutcome::Succeeded,
                (_, Some(_)) => oxide_audit::AuditOutcome::Failed,
                (_, None) => oxide_audit::AuditOutcome::Unknown,
            };
            audit.finish(
                audit_outcome,
                if audit_exit_code.is_some() {
                    oxide_audit::AuditEvidence::ExitCode
                } else {
                    oxide_audit::AuditEvidence::Protocol
                },
                audit_exit_code,
                None,
            );
            let _ = delivery_tx.send(super::delivery::HostToolsReliableDelivery::ScheduleAction(
                HostScheduleActionDelivery { request, result },
            ));
        });
        true
    }

    pub(super) fn spawn_process_action(
        &self,
        command: String,
        request: HostProcessActionRun,
        timeout: Duration,
        max_output_size: usize,
        runtime: tokio::runtime::Handle,
    ) -> bool {
        let audit = self.host_tool_audit(
            &request.connection_id,
            "process_action",
            Some(&format!("pid={} action={:?}", request.pid, request.action)),
        );
        let Some(handle) = self.ssh_registry.get(&request.connection_id) else {
            audit.finish(
                oxide_audit::AuditOutcome::Failed,
                oxide_audit::AuditEvidence::Request,
                None,
                None,
            );
            return false;
        };
        let delivery_tx = self.reliable_delivery_tx.clone();
        // Remote process output has no UI consumer. Reduce it in the worker
        // and clear both buffers before crossing the Entity delivery boundary.
        runtime.spawn(async move {
            let mut audit_exit_code = None;
            let result = handle
                .run_command_capture(&command, timeout, max_output_size)
                .await
                .map(|mut output| {
                    audit_exit_code = output.exit_code;
                    let succeeded = output.exit_code.unwrap_or(0) == 0;
                    zeroize::Zeroize::zeroize(&mut output.stdout);
                    zeroize::Zeroize::zeroize(&mut output.stderr);
                    succeeded
                })
                .map_err(|_| ());
            let audit_outcome = match (&result, audit_exit_code) {
                (Err(_), _) => oxide_audit::AuditOutcome::Failed,
                (_, Some(0)) => oxide_audit::AuditOutcome::Succeeded,
                (_, Some(_)) => oxide_audit::AuditOutcome::Failed,
                (_, None) => oxide_audit::AuditOutcome::Unknown,
            };
            audit.finish(
                audit_outcome,
                if audit_exit_code.is_some() {
                    oxide_audit::AuditEvidence::ExitCode
                } else {
                    oxide_audit::AuditEvidence::Protocol
                },
                audit_exit_code,
                None,
            );
            let _ = delivery_tx.send(super::delivery::HostToolsReliableDelivery::ProcessAction(
                HostProcessActionDelivery { request, result },
            ));
        });
        true
    }

    pub(super) fn spawn_docker_action(
        &self,
        command: String,
        request: HostDockerActionRequest,
        timeout: Duration,
        max_output_size: usize,
        runtime: tokio::runtime::Handle,
    ) -> bool {
        let audit = self.host_tool_audit(
            &request.connection_id,
            "container_action",
            Some(&format!(
                "container={} action={:?}",
                request.container_id, request.action
            )),
        );
        let Some(handle) = self.ssh_registry.get(&request.connection_id) else {
            audit.finish(
                oxide_audit::AuditOutcome::Failed,
                oxide_audit::AuditEvidence::Request,
                None,
                None,
            );
            return false;
        };
        let delivery_tx = self.reliable_delivery_tx.clone();
        // Docker action output is not product data. Clear it before delivery.
        runtime.spawn(async move {
            let mut audit_exit_code = None;
            let result = handle
                .run_command_capture(&command, timeout, max_output_size)
                .await
                .map(|mut output| {
                    audit_exit_code = output.exit_code;
                    let succeeded = output.exit_code.unwrap_or(0) == 0;
                    zeroize::Zeroize::zeroize(&mut output.stdout);
                    zeroize::Zeroize::zeroize(&mut output.stderr);
                    succeeded
                })
                .map_err(|_| ());
            let audit_outcome = match (&result, audit_exit_code) {
                (Err(_), _) => oxide_audit::AuditOutcome::Failed,
                (_, Some(0)) => oxide_audit::AuditOutcome::Succeeded,
                (_, Some(_)) => oxide_audit::AuditOutcome::Failed,
                (_, None) => oxide_audit::AuditOutcome::Unknown,
            };
            audit.finish(
                audit_outcome,
                if audit_exit_code.is_some() {
                    oxide_audit::AuditEvidence::ExitCode
                } else {
                    oxide_audit::AuditEvidence::Protocol
                },
                audit_exit_code,
                None,
            );
            let _ = delivery_tx.send(super::delivery::HostToolsReliableDelivery::DockerAction(
                HostDockerActionDelivery { request, result },
            ));
        });
        true
    }

    pub(super) fn spawn_docker_logs_capture(
        &self,
        command: String,
        request: HostDockerLogsRequest,
        timeout: Duration,
        max_output_size: usize,
        runtime: tokio::runtime::Handle,
    ) -> bool {
        let audit = self.host_tool_audit(
            &request.connection_id,
            "query_container_logs",
            Some(&format!("container_id={}", request.container_id)),
        );
        let Some(handle) = self.ssh_registry.get(&request.connection_id) else {
            audit.finish(
                oxide_audit::AuditOutcome::Failed,
                oxide_audit::AuditEvidence::Request,
                None,
                None,
            );
            return false;
        };
        let delivery_tx = self.reliable_delivery_tx.clone();
        // Raw logs remain inside the Entity delivery and dialog state.
        runtime.spawn(async move {
            let result = handle
                .run_command_capture(&command, timeout, max_output_size)
                .await
                .map_err(|_| ());
            finish_host_query_audit(audit, &result);
            let _ = delivery_tx.send(super::delivery::HostToolsReliableDelivery::DockerLogs(
                HostDockerLogsDelivery { request, result },
            ));
        });
        true
    }

    pub(super) fn spawn_service_snapshot_capture(
        &self,
        command: String,
        request: HostServiceSnapshotRequest,
        timeout: Duration,
        max_output_size: usize,
        runtime: tokio::runtime::Handle,
    ) -> bool {
        let Some(handle) = self.ssh_registry.get(&request.connection_id) else {
            return false;
        };
        let delivery_tx = self.reliable_delivery_tx.clone();
        // Inventory output stays inside the Entity and is parsed before render.
        runtime.spawn(async move {
            let result = handle
                .run_command_capture(&command, timeout, max_output_size)
                .await
                .map_err(|_| ());
            let _ = delivery_tx.send(super::delivery::HostToolsReliableDelivery::ServiceSnapshot(
                HostServiceSnapshotDelivery { request, result },
            ));
        });
        true
    }

    pub(super) fn spawn_service_action(
        &self,
        command: String,
        request: HostServiceActionRequest,
        timeout: Duration,
        max_output_size: usize,
        runtime: tokio::runtime::Handle,
    ) -> bool {
        let audit = self.host_tool_audit(
            &request.connection_id,
            "service_action",
            Some(&format!(
                "service={} action={:?}",
                request.service_id, request.action
            )),
        );
        let Some(handle) = self.ssh_registry.get(&request.connection_id) else {
            audit.finish(
                oxide_audit::AuditOutcome::Failed,
                oxide_audit::AuditEvidence::Request,
                None,
                None,
            );
            return false;
        };
        let delivery_tx = self.reliable_delivery_tx.clone();
        // Service action output has no UI consumer and is cleared in the worker.
        runtime.spawn(async move {
            let mut audit_exit_code = None;
            let result = handle
                .run_command_capture(&command, timeout, max_output_size)
                .await
                .map(|mut output| {
                    audit_exit_code = output.exit_code;
                    let succeeded = output.exit_code.unwrap_or(0) == 0;
                    zeroize::Zeroize::zeroize(&mut output.stdout);
                    zeroize::Zeroize::zeroize(&mut output.stderr);
                    succeeded
                })
                .map_err(|_| ());
            let audit_outcome = match (&result, audit_exit_code) {
                (Err(_), _) => oxide_audit::AuditOutcome::Failed,
                (_, Some(0)) => oxide_audit::AuditOutcome::Succeeded,
                (_, Some(_)) => oxide_audit::AuditOutcome::Failed,
                (_, None) => oxide_audit::AuditOutcome::Unknown,
            };
            audit.finish(
                audit_outcome,
                if audit_exit_code.is_some() {
                    oxide_audit::AuditEvidence::ExitCode
                } else {
                    oxide_audit::AuditEvidence::Protocol
                },
                audit_exit_code,
                None,
            );
            let _ = delivery_tx.send(super::delivery::HostToolsReliableDelivery::ServiceAction(
                HostServiceActionDelivery { request, result },
            ));
        });
        true
    }

    pub(super) fn spawn_service_logs_capture(
        &self,
        command: String,
        request: HostServiceLogsRequest,
        timeout: Duration,
        max_output_size: usize,
        runtime: tokio::runtime::Handle,
    ) -> bool {
        let audit = self.host_tool_audit(
            &request.connection_id,
            "query_service_logs",
            Some(&format!("service_id={}", request.service_id)),
        );
        let Some(handle) = self.ssh_registry.get(&request.connection_id) else {
            audit.finish(
                oxide_audit::AuditOutcome::Failed,
                oxide_audit::AuditEvidence::Request,
                None,
                None,
            );
            return false;
        };
        let delivery_tx = self.reliable_delivery_tx.clone();
        // Raw service logs never cross the typed workspace event boundary.
        runtime.spawn(async move {
            let result = handle
                .run_command_capture(&command, timeout, max_output_size)
                .await
                .map_err(|_| ());
            finish_host_query_audit(audit, &result);
            let _ = delivery_tx.send(super::delivery::HostToolsReliableDelivery::ServiceLogs(
                HostServiceLogsDelivery { request, result },
            ));
        });
        true
    }

    pub(super) fn spawn_tmux_snapshot_capture(
        &self,
        command: String,
        request: HostTmuxSnapshotRequest,
        timeout: Duration,
        max_output_size: usize,
        runtime: tokio::runtime::Handle,
    ) -> bool {
        let audit = request
            .feedback
            .should_toast()
            .then(|| self.host_tool_audit(&request.connection_id, "query_host_tmux", None));
        let Some(handle) = self.ssh_registry.get(&request.connection_id) else {
            if let Some(audit) = audit {
                audit.finish(
                    oxide_audit::AuditOutcome::Failed,
                    oxide_audit::AuditEvidence::Request,
                    None,
                    None,
                );
            }
            return false;
        };
        let delivery_tx = self.reliable_delivery_tx.clone();
        // Raw tmux inventory stays inside the Entity delivery boundary.
        runtime.spawn(async move {
            let result = handle
                .run_command_capture(&command, timeout, max_output_size)
                .await
                .map_err(|_| ());
            if let Some(audit) = audit {
                finish_host_query_audit(audit, &result);
            }
            let _ = delivery_tx.send(super::delivery::HostToolsReliableDelivery::TmuxSnapshot(
                HostTmuxSnapshotDelivery { request, result },
            ));
        });
        true
    }

    pub(super) fn spawn_tmux_action(
        &self,
        command: zeroize::Zeroizing<String>,
        request: HostTmuxActionRun,
        timeout: Duration,
        max_output_size: usize,
        runtime: tokio::runtime::Handle,
    ) -> bool {
        let audit = self.host_tool_audit(
            &request.connection_id,
            "tmux_action",
            Some(&format!("session={}", request.session_id)),
        );
        let Some(handle) = self.ssh_registry.get(&request.connection_id) else {
            audit.finish(
                oxide_audit::AuditOutcome::Failed,
                oxide_audit::AuditEvidence::Request,
                None,
                None,
            );
            return false;
        };
        let delivery_tx = self.reliable_delivery_tx.clone();
        // The generated command is moved once into the worker. Captured output
        // is cleared before only a success bit crosses delivery.
        runtime.spawn(async move {
            let mut audit_exit_code = None;
            let result = handle
                .run_command_capture(command.as_str(), timeout, max_output_size)
                .await
                .map(|mut output| {
                    audit_exit_code = output.exit_code;
                    let succeeded = output.exit_code.unwrap_or(0) == 0;
                    zeroize::Zeroize::zeroize(&mut output.stdout);
                    zeroize::Zeroize::zeroize(&mut output.stderr);
                    succeeded
                })
                .map_err(|_| ());
            let audit_outcome = match (&result, audit_exit_code) {
                (Err(_), _) => oxide_audit::AuditOutcome::Failed,
                (_, Some(0)) => oxide_audit::AuditOutcome::Succeeded,
                (_, Some(_)) => oxide_audit::AuditOutcome::Failed,
                (_, None) => oxide_audit::AuditOutcome::Unknown,
            };
            audit.finish(
                audit_outcome,
                if audit_exit_code.is_some() {
                    oxide_audit::AuditEvidence::ExitCode
                } else {
                    oxide_audit::AuditEvidence::Protocol
                },
                audit_exit_code,
                None,
            );
            let _ = delivery_tx.send(super::delivery::HostToolsReliableDelivery::TmuxAction(
                HostTmuxActionDelivery { request, result },
            ));
        });
        true
    }

    pub(super) fn compact_monitor_list_state(&self) -> ListState {
        self.compact_monitor_list_state.clone()
    }

    pub(super) fn sync_compact_monitor_list_signatures(&self, identity: &str, signatures: &[u64]) {
        sync_tauri_variable_list_state_by_signatures(
            &self.compact_monitor_list_state,
            &mut self.compact_monitor_list_cache.borrow_mut(),
            identity,
            signatures,
            TauriVirtualListSpec::new(
                px(COMPACT_MONITOR_LIST_ESTIMATED_ROW_HEIGHT),
                COMPACT_MONITOR_LIST_OVERSCAN,
            ),
        );
    }

    pub(super) fn request_profiler_refresh(
        &mut self,
        connection_id: String,
        cx: &mut Context<Self>,
    ) {
        if !self.visibility.sidebar_is_visible() || self.sampling_config.is_empty() {
            return;
        }
        if self
            .ssh_registry
            .get(&connection_id)
            .is_some_and(|handle| handle.remote_env().is_none())
        {
            self.ssh_registry.retry_remote_env_detection(&connection_id);
            cx.notify();
            return;
        }
        if self.profiler_registry.request_refresh(&connection_id) {
            return;
        }
        let Some(runtime) = self.lifecycle_runtime.clone() else {
            return;
        };
        self.start_profiler(connection_id, self.sampling_config, runtime, cx);
    }

    pub(super) fn set_runtime_section(&mut self, section: ConnectionRuntimeSection) -> bool {
        if self.active_runtime_section == section {
            return false;
        }
        self.previous_runtime_section = self.active_runtime_section;
        self.active_runtime_section = section;
        true
    }

    pub(in crate::workspace) fn reset_runtime_section(&mut self) {
        self.active_runtime_section = ConnectionRuntimeSection::Overview;
        self.previous_runtime_section = ConnectionRuntimeSection::Overview;
    }

    pub(in crate::workspace) fn selected_connection_id(&self) -> Option<&str> {
        self.selected_connection_id.as_deref()
    }

    pub(in crate::workspace) fn active_tool(&self) -> HostTools {
        self.active_tool
    }

    pub(in crate::workspace) fn select_tool(
        &mut self,
        tool: HostTools,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.active_tool == tool {
            return false;
        }
        self.active_tool = tool;
        cx.notify();
        true
    }

    pub(in crate::workspace) fn select_sidebar_tool(
        &mut self,
        tool: HostTools,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.select_tool(tool, cx) {
            return false;
        }
        self.ui.retain_input_focus_for_tool(tool);
        if tool != HostTools::Processes {
            self.dismiss_process_confirm(cx);
        }
        if tool != HostTools::Docker {
            self.dismiss_docker_confirm(cx);
        }
        if tool != HostTools::Services {
            self.dismiss_service_confirm(cx);
            self.pause_service_refreshes();
        }
        if tool != HostTools::Tmux {
            self.dismiss_tmux_confirm(cx);
            self.dismiss_tmux_input_dialog(cx);
        }
        if tool != HostTools::Schedules {
            self.dismiss_schedule_confirm(cx);
        }

        if let Some(runtime) = self.lifecycle_runtime.clone() {
            // Tool selection replaces only page samplers. Long-running SSH,
            // transfer, and forwarding ownership remains outside this Entity.
            self.update_lifecycle(
                self.visibility,
                self.monitoring.clone(),
                self.sampling_config,
                runtime,
                true,
                cx,
            );
            self.request_active_tool_snapshot(HostSnapshotFeedback::Silent, cx);
        }
        cx.emit(HostToolsEvent::ToolSelected);
        true
    }

    pub(in crate::workspace) fn selected_connection_id_owned(&self) -> Option<String> {
        self.selected_connection_id.clone()
    }

    pub(super) fn selector_open(&self) -> bool {
        self.selector_open
    }

    pub(super) fn selector_highlighted_index(&self) -> Option<usize> {
        self.selector_highlighted_index
    }

    pub(super) fn selector_focus_origin(&self) -> Option<browser_behavior::BrowserFocusOrigin> {
        self.selector_focus_origin
    }

    pub(in crate::workspace) fn close_selector(
        &mut self,
        clear_focus: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        let changed = self.selector_open
            || self.selector_highlighted_index.is_some()
            || (clear_focus && self.selector_focus_origin.is_some());
        self.selector_open = false;
        self.selector_highlighted_index = None;
        if clear_focus {
            self.selector_focus_origin = None;
        }
        if changed {
            cx.notify();
        }
        changed
    }

    pub(super) fn toggle_selector_from_pointer(
        &mut self,
        selected_index: usize,
        cx: &mut Context<Self>,
    ) {
        self.selector_focus_origin = Some(browser_behavior::BrowserFocusOrigin::Pointer);
        if self.selector_open {
            self.selector_open = false;
            self.selector_highlighted_index = None;
        } else {
            self.selector_open = true;
            self.selector_highlighted_index = Some(selected_index);
        }
        cx.notify();
    }

    pub(super) fn highlight_selector_index(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.selector_highlighted_index != Some(index) {
            self.selector_highlighted_index = Some(index);
            cx.notify();
        }
    }

    pub(super) fn focus_selector_trigger(&mut self, cx: &mut Context<Self>) {
        self.selector_focus_origin = Some(browser_behavior::BrowserFocusOrigin::Keyboard);
        cx.notify();
    }

    pub(super) fn open_selector_from_keyboard(
        &mut self,
        selected_index: usize,
        cx: &mut Context<Self>,
    ) {
        self.selector_open = true;
        self.selector_highlighted_index = Some(selected_index);
        self.selector_focus_origin = Some(browser_behavior::BrowserFocusOrigin::Keyboard);
        cx.notify();
    }

    pub(super) fn close_selector_to_keyboard_trigger(&mut self, cx: &mut Context<Self>) {
        self.selector_open = false;
        self.selector_highlighted_index = None;
        self.selector_focus_origin = Some(browser_behavior::BrowserFocusOrigin::Keyboard);
        cx.notify();
    }

    pub(super) fn highlight_selector_from_keyboard(
        &mut self,
        index: usize,
        cx: &mut Context<Self>,
    ) {
        self.selector_highlighted_index = Some(index);
        self.selector_focus_origin = Some(browser_behavior::BrowserFocusOrigin::Keyboard);
        cx.notify();
    }

    pub(super) fn select_connection(
        &mut self,
        connection_id: String,
        focus_origin: Option<browser_behavior::BrowserFocusOrigin>,
        cx: &mut Context<Self>,
    ) {
        self.selected_connection_id = Some(connection_id);
        self.selector_open = false;
        self.selector_highlighted_index = None;
        self.selector_focus_origin = focus_origin;
        cx.notify();
    }

    pub(super) fn select_connection_for_active_tool(
        &mut self,
        connection_id: String,
        focus_origin: Option<browser_behavior::BrowserFocusOrigin>,
        cx: &mut Context<Self>,
    ) {
        self.select_connection(connection_id, focus_origin, cx);
        self.dismiss_process_confirm(cx);
        self.dismiss_docker_confirm(cx);
        self.dismiss_service_confirm(cx);
        self.dismiss_tmux_input_dialog(cx);
        self.dismiss_tmux_confirm(cx);
        self.dismiss_schedule_confirm(cx);

        let Some(runtime) = self.lifecycle_runtime.clone() else {
            return;
        };
        // Reuse the Entity lifecycle transition so a connection switch
        // replaces page samplers without releasing the registry-owned node.
        self.update_lifecycle(
            self.visibility,
            self.monitoring.clone(),
            self.sampling_config,
            runtime,
            true,
            cx,
        );
        self.request_active_tool_snapshot(HostSnapshotFeedback::Silent, cx);
    }

    pub(super) fn request_active_tool_snapshot(
        &mut self,
        feedback: HostSnapshotFeedback,
        cx: &mut Context<Self>,
    ) {
        if !self.visibility.sidebar_is_visible()
            || !self.active_tool.monitoring_enabled(&self.monitoring)
        {
            return;
        }
        let Some(connection_id) = self.selected_connection_id_owned() else {
            return;
        };
        let Some(runtime) = self.lifecycle_runtime.clone() else {
            return;
        };
        let Some(messages) = self.messages.clone() else {
            return;
        };
        let notices = match self.active_tool {
            HostTools::Services => {
                self.request_service_snapshot(
                    connection_id,
                    runtime,
                    messages.service_connection_missing,
                    messages.service_action_failed,
                    cx,
                );
                Vec::new()
            }
            HostTools::Logs => self.request_log_snapshot(
                connection_id,
                feedback,
                self.monitoring.logs_enabled,
                runtime,
                messages.log_unknown_error,
                cx,
            ),
            HostTools::Tmux => self.request_tmux_snapshot(
                connection_id,
                feedback,
                self.ui.host_tmux_search_query.clone(),
                messages.tmux_unknown_error,
                messages.tmux_unavailable,
                runtime,
                cx,
            ),
            HostTools::Ports => self.request_port_snapshot(
                connection_id,
                feedback,
                self.monitoring.ports_enabled,
                runtime,
                messages.port_unknown_error,
                cx,
            ),
            HostTools::Schedules => self.request_schedule_snapshot(
                connection_id,
                feedback,
                self.monitoring.schedules_enabled,
                runtime,
                messages.schedule_unknown_error,
                cx,
            ),
            HostTools::Filesystems => self.request_filesystem_snapshot(
                connection_id,
                feedback,
                self.monitoring.filesystems_enabled,
                runtime,
                messages.filesystem_unknown_error,
                cx,
            ),
            HostTools::Packages => self.request_package_snapshot(
                connection_id,
                feedback,
                self.monitoring.packages_enabled,
                runtime,
                messages.package_unknown_error,
                cx,
            ),
            HostTools::Monitor | HostTools::Gpu | HostTools::Processes | HostTools::Docker => {
                Vec::new()
            }
        };
        for notice in notices {
            cx.emit(HostToolsEvent::ShowNotice(notice));
        }
    }

    pub(in crate::workspace) fn take_selected_connection(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<String> {
        let selected = self.selected_connection_id.take();
        self.close_selector(true, cx);
        selected
    }

    pub(in crate::workspace) fn ensure_selected_connection(
        &mut self,
        live_connection_ids: &[String],
        cx: &mut Context<Self>,
    ) -> Option<String> {
        if live_connection_ids.is_empty() {
            self.take_selected_connection(cx);
            return None;
        }
        let selected_is_live = self
            .selected_connection_id
            .as_ref()
            .is_some_and(|selected| {
                live_connection_ids
                    .iter()
                    .any(|connection_id| connection_id == selected)
            });
        if !selected_is_live {
            self.selected_connection_id = live_connection_ids.first().cloned();
            cx.notify();
        }
        self.selected_connection_id.clone()
    }

    pub(super) fn stop_profiler_sampling(&self) {
        self.profiler_registry.stop_all();
    }

    pub(super) fn profiler_connection_ids(&self) -> Vec<String> {
        self.profiler_registry.connection_ids()
    }

    pub(super) fn remove_profiler_connection(&self, connection_id: &str) {
        self.profiler_registry.remove(connection_id);
    }

    pub(super) fn profiler_connection_missing(&self, connection_id: &str) -> bool {
        self.profiler_registry.state(connection_id).is_none()
    }

    pub(super) fn start_profiler(
        &self,
        connection_id: String,
        sampling_config: oxide_connection_monitor::ResourceSamplingConfig,
        runtime: tokio::runtime::Handle,
        cx: &mut Context<Self>,
    ) {
        let Some(handle) = self.ssh_registry.get(&connection_id) else {
            return;
        };
        let Some(os_type) = handle.remote_env().map(|environment| environment.os_type) else {
            // Environment detection owns OS selection; never guess a probe dialect.
            return;
        };
        let sampler = self.resource_sampler(handle);
        self.profiler_registry.start_with_sampler_on_config(
            connection_id,
            sampler,
            os_type,
            sampling_config,
            Some(self.profiler_update_tx.clone()),
            runtime,
        );
        cx.notify();
    }

    pub(super) fn sync_gpu_sampling(
        &mut self,
        enabled_and_visible: bool,
        selected_connection_id: Option<String>,
        runtime: tokio::runtime::Handle,
        cx: &mut Context<Self>,
    ) {
        if !enabled_and_visible {
            if let Some(task) = self.host_gpu.sampling_task.take() {
                task.stop();
            }
            return;
        }

        let Some(connection_id) = selected_connection_id else {
            return;
        };
        if self
            .host_gpu
            .sampling_task
            .as_ref()
            .is_some_and(|task| task.connection_id() == connection_id)
        {
            return;
        }
        if let Some(task) = self.host_gpu.sampling_task.take() {
            task.stop();
        }
        let Some(handle) = self.ssh_registry.get(&connection_id) else {
            return;
        };
        let Some(os_type) = handle.remote_env().map(|environment| environment.os_type) else {
            return;
        };
        let sampler = self.resource_sampler(handle);
        self.host_gpu.snapshot_connection_id = Some(connection_id.clone());
        self.host_gpu.snapshot = None;
        self.host_gpu.expanded_uuid = None;
        // The Entity owns only the page sampler shell; the registry retains the shared node.
        self.host_gpu.sampling_task = Some(start_gpu_sampling_on(
            connection_id,
            sampler,
            os_type,
            self.host_gpu.update_tx.clone(),
            runtime,
        ));
        cx.notify();
    }

    fn resource_sampler(&self, handle: oxide_ssh::SshConnectionHandle) -> Arc<dyn ResourceSampler> {
        if let Some(context) = self.ssh_consumer_context.as_ref()
            && let Some(node_id) = context
                .node_router
                .node_id_for_connection(handle.connection_id())
            && context
                .node_router
                .node_runtime_snapshot(&node_id)
                .is_some_and(|snapshot| {
                    snapshot
                        .config
                        .ssh_channel_strategy
                        .requires_dedicated_consumers()
                })
        {
            return Arc::new(oxide_ssh::DedicatedNodeResourceSampler::new(
                context.node_router.clone(),
                node_id,
                context.prompt_handler.clone(),
                context.managed_key_resolver.clone(),
            ));
        }
        Arc::new(handle)
    }

    pub(super) fn gpu_snapshot_for(&self, connection_id: &str) -> Option<GpuSnapshot> {
        self.host_gpu
            .snapshot
            .as_ref()
            .filter(|_| self.host_gpu.snapshot_connection_id.as_deref() == Some(connection_id))
            .cloned()
    }

    pub(super) fn gpu_sampling_is_running(&self, connection_id: &str) -> bool {
        self.host_gpu
            .sampling_task
            .as_ref()
            .is_some_and(|task| task.connection_id() == connection_id && !task.is_finished())
    }

    pub(super) fn gpu_list_state(&self) -> ListState {
        self.host_gpu.list_state.clone()
    }

    pub(super) fn toggle_gpu_device(&mut self, device_uuid: String, cx: &mut Context<Self>) {
        if self.host_gpu.expanded_uuid.as_deref() == Some(device_uuid.as_str()) {
            self.host_gpu.expanded_uuid = None;
        } else {
            self.host_gpu.expanded_uuid = Some(device_uuid);
        }
        cx.notify();
    }

    pub(super) fn gpu_device_is_expanded(&self, device_uuid: &str) -> bool {
        self.host_gpu.expanded_uuid.as_deref() == Some(device_uuid)
    }

    pub(super) fn sync_gpu_list_state(
        &self,
        devices: &[GpuDevice],
        snapshot: Option<&GpuSnapshot>,
        selected_connection_id: &str,
    ) {
        let signatures = devices
            .iter()
            .map(|device| {
                let process_count = snapshot
                    .map(|snapshot| snapshot.processes_for(device).count())
                    .unwrap_or_default();
                gpu_device_row_signature(
                    device,
                    process_count,
                    self.gpu_device_is_expanded(&device.uuid),
                )
            })
            .collect::<Vec<_>>();
        sync_tauri_variable_list_state_by_signatures(
            &self.host_gpu.list_state,
            &mut self.host_gpu.list_cache.borrow_mut(),
            &format!("host-gpu:{selected_connection_id}"),
            &signatures,
            TauriVirtualListSpec::new(px(HOST_GPU_LIST_ESTIMATED_ROW_HEIGHT), 8),
        );
    }

    pub(super) fn request_gpu_refresh(&mut self, connection_id: String, cx: &mut Context<Self>) {
        let enabled_and_visible = self.monitoring.gpu_enabled
            && self.visibility.sidebar_is_visible()
            && self.active_tool() == HostTools::Gpu;
        let selected_connection_id = self.selected_connection_id_owned();
        let Some(runtime) = self.lifecycle_runtime.clone() else {
            return;
        };
        self.restart_gpu_sampling(
            connection_id,
            enabled_and_visible,
            selected_connection_id,
            runtime,
            cx,
        );
    }

    pub(super) fn restart_gpu_sampling(
        &mut self,
        connection_id: String,
        enabled_and_visible: bool,
        selected_connection_id: Option<String>,
        runtime: tokio::runtime::Handle,
        cx: &mut Context<Self>,
    ) {
        if self
            .host_gpu
            .sampling_task
            .as_ref()
            .is_some_and(|task| task.connection_id() == connection_id)
            && let Some(task) = self.host_gpu.sampling_task.take()
        {
            task.stop();
        }
        self.host_gpu.snapshot = None;
        self.sync_gpu_sampling(enabled_and_visible, selected_connection_id, runtime, cx);
    }
}

impl WorkspaceApp {
    pub(in crate::workspace) fn handle_host_tools_window_modal_key(
        &mut self,
        modal: HostToolsWindowModalSnapshot,
        event: &KeyDownEvent,
        cx: &mut Context<Self>,
    ) -> bool {
        match modal {
            HostToolsWindowModalSnapshot::ProcessConfirm(_) => {
                self.handle_host_process_confirm_key(event, cx)
            }
            HostToolsWindowModalSnapshot::DockerConfirm(_) => {
                self.handle_host_docker_confirm_key(event, cx)
            }
            HostToolsWindowModalSnapshot::DockerLogs => {
                if self.handle_host_log_search_key(event, cx) {
                    return true;
                }
                if event.keystroke.key.as_str() == "escape" {
                    self.host_tools.update(cx, |host_tools, cx| {
                        host_tools.dismiss_docker_logs_dialog(cx);
                    });
                }
                true
            }
            HostToolsWindowModalSnapshot::ServiceConfirm(_) => {
                self.handle_host_service_confirm_key(event, cx)
            }
            HostToolsWindowModalSnapshot::ServiceLogs => {
                if self.handle_host_log_search_key(event, cx) {
                    return true;
                }
                if event.keystroke.key.as_str() == "escape" {
                    self.host_tools.update(cx, |host_tools, cx| {
                        host_tools.dismiss_service_logs_dialog(cx);
                    });
                }
                true
            }
            HostToolsWindowModalSnapshot::TmuxConfirm(_) => {
                self.handle_host_tmux_confirm_key(event, cx)
            }
            HostToolsWindowModalSnapshot::TmuxInput => {
                self.handle_host_tmux_input_dialog_key(event, cx)
            }
            HostToolsWindowModalSnapshot::ScheduleConfirm(_) => {
                self.handle_host_schedule_confirm_key(event, cx)
            }
            HostToolsWindowModalSnapshot::ScheduleLogs => {
                if self.handle_host_log_search_key(event, cx) {
                    return true;
                }
                if event.keystroke.key.as_str() == "escape" {
                    self.host_tools.update(cx, |host_tools, cx| {
                        host_tools.dismiss_schedule_logs_dialog(cx);
                    });
                }
                true
            }
        }
    }
}
