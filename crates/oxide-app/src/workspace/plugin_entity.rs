use super::*;
use gpui::Task;
use std::{
    future::Future,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

use super::plugin_lifecycle::{
    NativePluginConfirmDialog, NativePluginConfirmRequest, NativePluginOxideImportCoreResult,
    NativePluginOxideImportWorkerMessage, NativePluginOxidePostImportOptions,
    NativePluginProductUiEffect, NativePluginRuntimeDelivery, NativePluginSyncRequest,
    NativePluginTerminalRequest,
};

// Plugin-owned workers use bounded waits without involving the workspace heartbeat.
const NATIVE_PLUGIN_LIFECYCLE_TIMEOUT: Duration = Duration::from_secs(5);
const NATIVE_PLUGIN_DELIVERY_POLL_INTERVAL: Duration = Duration::from_millis(80);
const NATIVE_PLUGIN_RELEASE_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const NATIVE_PLUGIN_WORKSPACE_RELEASED_CODE: &str = "plugin_workspace_released";

fn plugin_request_audit_context(plugin_id: &str) -> Option<oxide_audit::AuditContext> {
    oxide_audit::AuditContext::current_request()
        .or_else(oxide_audit::AuditContext::current)
        .map(|mut context| {
            context.source = oxide_audit::AuditSource::Plugin;
            context.agent_id = Some(oxide_audit::redact(plugin_id));
            context
        })
}

fn plugin_management_audit(plugin_id: Option<&str>, action: &str) -> oxide_audit::AuditOperation {
    let context = oxide_audit::AuditContext::current_request()
        .or_else(oxide_audit::AuditContext::current)
        .map(|mut context| {
            if context.source == oxide_audit::AuditSource::Application {
                context.source = oxide_audit::AuditSource::User;
            }
            context.target = plugin_id.map(oxide_audit::redact);
            context
        });
    oxide_audit::AuditOperation::in_context(
        context.as_ref(),
        oxide_audit::AuditCategory::Configuration,
        action,
        None,
    )
}

pub(in crate::workspace) enum PluginWorkspaceEvent {
    ManagerDeliveryReady,
    RuntimeRequestsReady,
    RuntimeSubscriptionSampleDue,
    RuntimeIntentsReady,
    OxideImportIntentsReady,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(in crate::workspace) enum PluginSubscriptionSample {
    Layout,
    Sessions,
    SavedForwards,
    Transfers,
    Profiler,
    Ide,
    EventLog,
}

/// Producer endpoints shared by one native plugin host resolver.
pub(in crate::workspace) struct PluginRuntimeRequestSenders {
    pub(in crate::workspace) confirm: delivery::ActiveDeliverySender<NativePluginConfirmRequest>,
    pub(in crate::workspace) terminal: delivery::ActiveDeliverySender<NativePluginTerminalRequest>,
    pub(in crate::workspace) sync: delivery::ActiveDeliverySender<NativePluginSyncRequest>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::workspace) enum PluginRuntimeAdapterRefresh {
    None,
    TerminalHooks,
    TerminalInputInterceptors,
    All,
}

pub(in crate::workspace) enum PluginRuntimeIntent {
    PreparePush {
        resolver: tokio::sync::oneshot::Sender<plugin_runtime::NativeHostApiResolver>,
    },
    ApplyEffects {
        plugin_id: String,
        audit_context: Option<oxide_audit::AuditContext>,
        effects: Vec<plugin_runtime::PluginOutboundEffect>,
        refresh: PluginRuntimeAdapterRefresh,
    },
    StateChanged,
}

pub(in crate::workspace) enum PluginOxideImportIntent {
    Progress {
        plugin_id: Arc<str>,
        registration_id: Arc<str>,
        value: serde_json::Value,
    },
    Complete {
        plugin_id: Arc<str>,
        progress_registration_id: Option<Arc<str>>,
        request_id: String,
        result: Result<NativePluginOxideImportCoreResult, ()>,
        options: NativePluginOxidePostImportOptions,
        response_tx: std::sync::mpsc::Sender<plugin_runtime::PluginResponse>,
    },
}

struct PluginOxideImportContext {
    plugin_id: Arc<str>,
    progress_registration_id: Option<Arc<str>>,
    request_id: String,
    options: NativePluginOxidePostImportOptions,
    response_tx: std::sync::mpsc::Sender<plugin_runtime::PluginResponse>,
}

pub(in crate::workspace) const NATIVE_PLUGIN_RUNTIME_FAILURE_DIAGNOSTIC: &str =
    "native_plugin_runtime_failure";

fn native_plugin_runtime_failure_message(error: plugin_runtime::PluginError) -> String {
    let plugin_runtime::PluginError {
        code,
        message,
        recoverable: _,
    } = error;
    // Runtime text is plugin-controlled and may echo request data. Keep only
    // known host codes and erase the raw text at the delivery boundary.
    let _sensitive_message = Zeroizing::new(message);
    if code == plugin_runtime::WASM_RUNTIME_UNAVAILABLE_CODE {
        code
    } else {
        NATIVE_PLUGIN_RUNTIME_FAILURE_DIAGNOSTIC.to_string()
    }
}

fn plugin_workspace_released_response(request_id: String) -> plugin_runtime::PluginResponse {
    plugin_runtime::PluginResponse::error(
        request_id,
        plugin_runtime::PluginError::runtime(
            NATIVE_PLUGIN_WORKSPACE_RELEASED_CODE,
            "Plugin workspace is shutting down",
        ),
    )
}

/// Owns plugin workers and reliable delivery independently from plugin page visibility.
pub(in crate::workspace) struct PluginWorkspaceEntity {
    pub(in crate::workspace) connections: super::plugin_connections::PluginConnections,
    task_runtime: Arc<tokio::runtime::Runtime>,
    registry: Arc<plugin_host::NativePluginRegistry>,
    security_key_provider: Option<Arc<oxide_ssh::SecurityKeyProvider>>,
    runtime_host: Arc<tokio::sync::Mutex<plugin_runtime::NativePluginRuntimeHost>>,
    runtime_delivery_tx: delivery::ActiveDeliverySender<NativePluginRuntimeDelivery>,
    runtime_delivery_rx: std::sync::mpsc::Receiver<NativePluginRuntimeDelivery>,
    runtime_intents: VecDeque<PluginRuntimeIntent>,
    active_runtime_plugin_ids: HashSet<String>,
    runtime_generations: HashMap<String, u64>,
    owned_tasks: Arc<Mutex<HashMap<u64, Option<tokio::task::AbortHandle>>>>,
    next_owned_task_id: AtomicU64,
    release_shutdown_started: bool,
    manager_state: plugin_manager::NativePluginManagerState,
    catalog_history_cancellation: Option<tokio_util::sync::CancellationToken>,
    ui_state: plugin_ui::NativePluginUiState,
    manager_operation_in_flight: bool,
    remote_desktop_install_pending: HashSet<String>,
    remote_desktop_removals: HashSet<String>,
    uninstall_task: Option<Task<()>>,
    compatibility_refresh_pending: bool,
    manager_delivery_tx:
        delivery::ActiveDeliverySender<plugin_manager::NativePluginManagerDelivery>,
    manager_delivery_rx: std::sync::mpsc::Receiver<plugin_manager::NativePluginManagerDelivery>,
    manager_deliveries: VecDeque<plugin_manager::NativePluginManagerDelivery>,
    runtime_request_wake: delivery::ActiveDeliveryWake,
    confirm_tx: delivery::ActiveDeliverySender<NativePluginConfirmRequest>,
    confirm_rx: std::sync::mpsc::Receiver<NativePluginConfirmRequest>,
    confirm: Option<NativePluginConfirmDialog>,
    confirm_presence: oxide_ui::motion::ExitPresence,
    terminal_tx: delivery::ActiveDeliverySender<NativePluginTerminalRequest>,
    terminal_rx: std::sync::mpsc::Receiver<NativePluginTerminalRequest>,
    sync_tx: delivery::ActiveDeliverySender<NativePluginSyncRequest>,
    sync_rx: std::sync::mpsc::Receiver<NativePluginSyncRequest>,
    product_ui_effects: VecDeque<NativePluginProductUiEffect>,
    oxide_import_delivery_tx: delivery::ActiveDeliverySender<NativePluginOxideImportWorkerMessage>,
    oxide_import_delivery_rx: std::sync::mpsc::Receiver<NativePluginOxideImportWorkerMessage>,
    oxide_import_contexts: HashMap<u64, PluginOxideImportContext>,
    oxide_import_intents: VecDeque<PluginOxideImportIntent>,
    next_oxide_import_id: u64,
    runtime_services_started: bool,
    subscription_samples: Vec<PluginSubscriptionSample>,
    subscription_snapshots: HashMap<PluginSubscriptionSample, serde_json::Value>,
    subscription_sampler_generation: u64,
    subscription_sampler_running: bool,
    subscription_sampler_task: Option<Task<()>>,
    transfer_progress_last_emitted: Option<Instant>,
    runtime_profiler_last_emitted: Option<Instant>,
    event_log_last_id: u64,
}

impl PluginWorkspaceEntity {
    pub(in crate::workspace) fn new(
        task_runtime: Arc<tokio::runtime::Runtime>,
        registry: plugin_host::NativePluginRegistry,
        cx: &mut Context<Self>,
    ) -> Self {
        let (runtime_delivery_tx, runtime_delivery_rx) = delivery::ActiveDeliverySender::channel();
        let (manager_delivery_tx, manager_delivery_rx) = delivery::ActiveDeliverySender::channel();
        let runtime_request_wake = delivery::ActiveDeliveryWake::default();
        let (confirm_tx, confirm_rx) =
            delivery::ActiveDeliverySender::channel_with_wake(runtime_request_wake.clone());
        let (terminal_tx, terminal_rx) =
            delivery::ActiveDeliverySender::channel_with_wake(runtime_request_wake.clone());
        let (sync_tx, sync_rx) =
            delivery::ActiveDeliverySender::channel_with_wake(runtime_request_wake.clone());
        let (oxide_import_delivery_tx, oxide_import_delivery_rx) =
            delivery::ActiveDeliverySender::channel();
        let entity = Self {
            connections: Default::default(),
            task_runtime,
            registry: Arc::new(registry),
            security_key_provider: None,
            runtime_host: Arc::new(tokio::sync::Mutex::new(
                plugin_runtime::NativePluginRuntimeHost::default(),
            )),
            runtime_delivery_tx,
            runtime_delivery_rx,
            runtime_intents: VecDeque::new(),
            active_runtime_plugin_ids: HashSet::new(),
            runtime_generations: HashMap::new(),
            owned_tasks: Arc::new(Mutex::new(HashMap::new())),
            next_owned_task_id: AtomicU64::new(1),
            release_shutdown_started: false,
            manager_state: plugin_manager::NativePluginManagerState::new(),
            catalog_history_cancellation: None,
            ui_state: plugin_ui::NativePluginUiState::default(),
            manager_operation_in_flight: false,
            remote_desktop_install_pending: HashSet::new(),
            remote_desktop_removals: HashSet::new(),
            uninstall_task: None,
            compatibility_refresh_pending: false,
            manager_delivery_tx,
            manager_delivery_rx,
            manager_deliveries: VecDeque::new(),
            runtime_request_wake,
            confirm_tx,
            confirm_rx,
            confirm: None,
            confirm_presence: oxide_ui::motion::ExitPresence::visible(),
            terminal_tx,
            terminal_rx,
            sync_tx,
            sync_rx,
            product_ui_effects: VecDeque::new(),
            oxide_import_delivery_tx,
            oxide_import_delivery_rx,
            oxide_import_contexts: HashMap::new(),
            oxide_import_intents: VecDeque::new(),
            next_oxide_import_id: 1,
            runtime_services_started: false,
            subscription_samples: Vec::new(),
            subscription_snapshots: HashMap::new(),
            subscription_sampler_generation: 0,
            subscription_sampler_running: false,
            subscription_sampler_task: None,
            transfer_progress_last_emitted: None,
            runtime_profiler_last_emitted: None,
            event_log_last_id: 0,
        };
        entity.schedule_runtime_delivery(cx);
        entity.schedule_runtime_pushes();
        entity.schedule_manager_delivery(cx);
        entity.schedule_runtime_request_delivery(cx);
        entity.schedule_oxide_import_delivery(cx);
        entity.schedule_release_shutdown(cx);
        entity.sync_language_plugins(cx);
        entity
    }

    pub(in crate::workspace) fn manager_operation_in_flight(&self) -> bool {
        self.manager_operation_in_flight
            || self.manager_state.active_install.is_some()
            || !self.remote_desktop_removals.is_empty()
    }

    pub(in crate::workspace) fn take_next_package_install(
        &mut self,
    ) -> Option<plugin_manager::NativePluginInstallRequest> {
        if self.release_shutdown_started
            || self.manager_operation_in_flight()
            || self.manager_state.pending_overwrite.is_some()
        {
            return None;
        }
        self.manager_state.install_queue.pop_front()
    }

    pub(in crate::workspace) fn plugin_change_in_flight(&self, id: &str) -> bool {
        self.manager_state.install_pending(Some(id))
            || self
                .manager_state
                .active_install
                .as_ref()
                .is_some_and(|active| active.expected_id.is_none())
            || !self.remote_desktop_removals.is_empty()
    }

    fn mark_package_install(&mut self, expected_id: Option<&str>) {
        let name = expected_id.and_then(|id| {
            self.manager_state
                .marketplace_entries
                .iter()
                .find(|entry| entry.id == id)
                .map(|entry| entry.name.clone())
                .or_else(|| {
                    self.registry
                        .plugins()
                        .iter()
                        .find(|plugin| plugin.manifest.id == id)
                        .map(|plugin| plugin.manifest.name.clone())
                })
        });
        self.manager_state.active_install = Some(plugin_manager::NativePluginInstallTarget {
            expected_id: expected_id.map(str::to_owned),
            name,
        });
    }

    pub(in crate::workspace) fn manager_state(&self) -> &plugin_manager::NativePluginManagerState {
        &self.manager_state
    }

    pub(in crate::workspace) fn manager_state_mut(
        &mut self,
    ) -> &mut plugin_manager::NativePluginManagerState {
        &mut self.manager_state
    }

    pub(in crate::workspace) fn ui_state(&self) -> &plugin_ui::NativePluginUiState {
        &self.ui_state
    }

    pub(in crate::workspace) fn ui_state_mut(&mut self) -> &mut plugin_ui::NativePluginUiState {
        &mut self.ui_state
    }

    pub(in crate::workspace) fn select_sidebar_panel(
        &mut self,
        selection: plugin_ui::NativePluginSidebarPanelSelection,
    ) {
        let previous = self.manager_state.active_sidebar_panel.replace(selection);
        if let Some(previous) = previous
            && self.manager_state.active_sidebar_panel.as_ref() != Some(&previous)
        {
            self.ui_state
                .remove_surface(&previous.plugin_id, "sidebarPanel", &previous.panel_id);
        }
    }

    pub(in crate::workspace) fn registry(&self) -> &plugin_host::NativePluginRegistry {
        &self.registry
    }

    pub(in crate::workspace) fn registry_mut(&mut self) -> &mut plugin_host::NativePluginRegistry {
        Arc::make_mut(&mut self.registry)
    }

    pub(in crate::workspace) fn registry_snapshot(&self) -> Arc<plugin_host::NativePluginRegistry> {
        Arc::clone(&self.registry)
    }

    pub(in crate::workspace) fn mosh_executable(&self) -> Option<PathBuf> {
        if self.compatibility_refresh_pending
            || self.release_shutdown_started
            || self
                .remote_desktop_install_pending
                .contains("com.oxide.terminal.mosh")
            || self
                .remote_desktop_removals
                .contains("com.oxide.terminal.mosh")
        {
            None
        } else {
            self.registry.mosh_executable()
        }
    }

    pub(in crate::workspace) fn security_key_provider(
        &mut self,
    ) -> Option<Arc<oxide_ssh::SecurityKeyProvider>> {
        if self.compatibility_refresh_pending
            || self.release_shutdown_started
            || self
                .manager_state
                .active_install
                .as_ref()
                .is_some_and(|active| {
                    active
                        .expected_id
                        .as_deref()
                        .is_none_or(|id| id == oxide_ssh::SECURITY_KEY_PLUGIN_ID)
                })
            || self
                .remote_desktop_install_pending
                .contains(oxide_ssh::SECURITY_KEY_PLUGIN_ID)
            || self
                .remote_desktop_removals
                .contains(oxide_ssh::SECURITY_KEY_PLUGIN_ID)
        {
            return None;
        }
        let executable = self.registry.security_key_executable()?;
        let provider = self
            .security_key_provider
            .get_or_insert_with(|| oxide_ssh::SecurityKeyProvider::new(executable));
        Some(Arc::clone(provider))
    }

    pub(in crate::workspace) fn retire_security_key(
        &mut self,
        plugin_id: Option<&str>,
    ) -> Option<std::thread::JoinHandle<()>> {
        if plugin_id.is_some_and(|id| id != oxide_ssh::SECURITY_KEY_PLUGIN_ID) {
            return None;
        }
        self.security_key_provider
            .take()
            .map(|provider| provider.retire())
    }

    pub(in crate::workspace) fn remote_desktop_providers(
        &self,
    ) -> Vec<oxide_remote_desktop_core::RemoteDesktopProviderManifest> {
        if self.compatibility_refresh_pending || self.release_shutdown_started {
            Vec::new()
        } else {
            self.registry
                .remote_desktop_providers()
                .into_iter()
                .filter(|provider| {
                    !self.remote_desktop_install_pending.contains(&provider.id)
                        && !self.remote_desktop_removals.contains(&provider.id)
                })
                .collect()
        }
    }

    pub(in crate::workspace) fn remote_desktop_plugin_enabled(
        &self,
        protocol: oxide_remote_desktop_core::RemoteDesktopProtocol,
    ) -> bool {
        !self.compatibility_refresh_pending
            && !self.release_shutdown_started
            && self
                .registry
                .enabled_remote_desktop_plugin_ids(protocol)
                .any(|id| {
                    !self.remote_desktop_install_pending.contains(id)
                        && !self.remote_desktop_removals.contains(id)
                })
    }

    pub(in crate::workspace) fn replace_registry(
        &mut self,
        mut registry: plugin_host::NativePluginRegistry,
        cx: &mut gpui::App,
    ) {
        registry.preserve_unchanged_runtimes(&self.registry);
        let old_security_key = self
            .registry
            .plugins()
            .iter()
            .find(|plugin| plugin.manifest.id == oxide_ssh::SECURITY_KEY_PLUGIN_ID);
        let new_security_key = registry
            .plugins()
            .iter()
            .find(|plugin| plugin.manifest.id == oxide_ssh::SECURITY_KEY_PLUGIN_ID);
        if old_security_key.map(|plugin| (&plugin.manifest, plugin.state))
            != new_security_key.map(|plugin| (&plugin.manifest, plugin.state))
        {
            self.retire_security_key(None);
        }
        let enabled_runtime_plugin_ids = registry
            .plugins()
            .iter()
            .filter(|plugin| {
                matches!(
                    plugin.state,
                    plugin_host::NativePluginState::ReadyProcess
                        | plugin_host::NativePluginState::ReadyWasm
                        | plugin_host::NativePluginState::Loading
                        | plugin_host::NativePluginState::Active
                )
            })
            .map(|plugin| plugin.manifest.id.as_str())
            .collect::<HashSet<_>>();
        let stale_runtime_plugin_ids = self
            .active_runtime_plugin_ids
            .iter()
            .filter(|plugin_id| !enabled_runtime_plugin_ids.contains(plugin_id.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        self.registry = Arc::new(registry);
        self.sync_language_plugins(cx);
        for plugin_id in stale_runtime_plugin_ids {
            self.start_runtime_deactivation(plugin_id);
        }
    }

    pub(in crate::workspace) fn set_plugin_enabled(
        &mut self,
        plugin_id: &str,
        enabled: bool,
        cx: &mut gpui::App,
    ) -> Result<(), String> {
        let audit = plugin_management_audit(Some(plugin_id), "plugin_enabled_change");
        let result = self.registry_mut().set_plugin_enabled(plugin_id, enabled);
        if result.is_ok() {
            self.sync_language_plugins(cx);
        }
        if result.is_ok() && !enabled {
            self.retire_security_key(Some(plugin_id));
            self.start_runtime_deactivation(plugin_id.to_string());
        }
        audit.finish(
            if result.is_ok() {
                oxide_audit::AuditOutcome::Succeeded
            } else {
                oxide_audit::AuditOutcome::Failed
            },
            oxide_audit::AuditEvidence::Protocol,
            None,
            None,
        );
        result
    }

    pub(in crate::workspace) fn begin_remote_desktop_removal(&mut self, plugin_id: &str) {
        self.remote_desktop_removals.insert(plugin_id.to_string());
    }

    pub(in crate::workspace) fn start_plugin_uninstall(
        &mut self,
        plugin_id: String,
        remove_storage: bool,
        workers: Vec<std::thread::JoinHandle<()>>,
        cx: &mut Context<Self>,
    ) -> tokio::sync::oneshot::Receiver<Result<(), String>> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        // The plugin entity survives window handoff and owns file replacement.
        self.uninstall_task = Some(cx.spawn(async move |plugins, cx| {
            cx.background_executor()
                .spawn(async move {
                    for worker in workers {
                        let _ = worker.join();
                    }
                })
                .await;
            let result = plugins
                .update(cx, |plugins, cx| {
                    let result = plugins.uninstall_plugin(&plugin_id, remove_storage, cx);
                    cx.notify();
                    result
                })
                .unwrap_or_else(|_| Err(NATIVE_PLUGIN_WORKSPACE_RELEASED_CODE.to_string()));
            let _ = sender.send(result);
        }));
        receiver
    }

    pub(in crate::workspace) fn uninstall_plugin(
        &mut self,
        plugin_id: &str,
        remove_storage: bool,
        cx: &mut gpui::App,
    ) -> Result<(), String> {
        let audit = plugin_management_audit(Some(plugin_id), "plugin_uninstall");
        let result = self
            .registry_mut()
            .uninstall_plugin(plugin_id, remove_storage);
        self.remote_desktop_removals.remove(plugin_id);
        if result.is_ok() {
            self.sync_language_plugins(cx);
            self.start_runtime_deactivation(plugin_id.to_string());
        }
        audit.finish(
            if result.is_ok() {
                oxide_audit::AuditOutcome::Succeeded
            } else {
                oxide_audit::AuditOutcome::Failed
            },
            oxide_audit::AuditEvidence::Protocol,
            None,
            None,
        );
        result
    }

    fn sync_language_plugins(&self, cx: &mut gpui::App) {
        let mut installed = self.registry.plugins().iter().collect::<Vec<_>>();
        installed.sort_by(|left, right| {
            let ready = |plugin: &plugin_host::NativePluginInfo| {
                matches!(
                    plugin.state,
                    plugin_host::NativePluginState::ReadyManifestOnly
                )
            };
            ready(right)
                .cmp(&ready(left))
                .then_with(|| left.manifest.id.cmp(&right.manifest.id))
        });
        let sources = installed
            .iter()
            .filter(|plugin| {
                matches!(
                    plugin.state,
                    plugin_host::NativePluginState::ReadyManifestOnly
                )
            })
            .filter_map(|plugin| {
                let language = plugin.manifest.contributes.as_ref()?.language.as_ref()?;
                let id = oxide_editor_syntax::LanguageId::from_plugin_key(&language.definition.id)?;
                Some(oxide_editor_syntax::PluginGrammarSource {
                    language: id,
                    grammar_name: language
                        .definition
                        .grammar_name
                        .clone()
                        .unwrap_or_else(|| language.definition.id.clone()),
                    parser: plugin
                        .install_dir
                        .join(&plugin.manifest.runtime.as_ref()?.entry),
                    highlights: plugin.install_dir.join(&language.highlights),
                    parser_sha256: language.parser_sha256.clone(),
                    highlights_sha256: language.highlights_sha256.clone(),
                    injections: language
                        .injections
                        .iter()
                        .map(
                            |injection| oxide_editor_syntax::PluginGrammarInjectionSource {
                                query: plugin.install_dir.join(&injection.query),
                                query_sha256: injection.query_sha256.clone(),
                                grammar: Box::new(oxide_editor_syntax::PluginGrammarSource {
                                    language: oxide_editor_syntax::LanguageId::from_plugin_key(
                                        &injection.id,
                                    )
                                    .expect("validated embedded language"),
                                    grammar_name: injection
                                        .grammar_name
                                        .clone()
                                        .unwrap_or_else(|| injection.id.clone()),
                                    parser: plugin.install_dir.join(&injection.parser),
                                    highlights: plugin.install_dir.join(&injection.highlights),
                                    parser_sha256: injection.parser_sha256.clone(),
                                    highlights_sha256: injection.highlights_sha256.clone(),
                                    injections: Vec::new(),
                                }),
                            },
                        )
                        .collect(),
                })
            })
            .collect();
        let mut definitions = installed
            .iter()
            .filter_map(|plugin| {
                let definition = &plugin
                    .manifest
                    .contributes
                    .as_ref()?
                    .language
                    .as_ref()?
                    .definition;
                Some(oxide_editor::EditorLanguageDefinition {
                    plugin_id: plugin.manifest.id.clone(),
                    definition: definition.clone(),
                })
            })
            .collect::<Vec<_>>();
        let catalog = self
            .registry
            .catalog_languages()
            .iter()
            .map(|(plugin_id, definition)| (plugin_id.clone(), definition.clone()));
        let loaded = self
            .manager_state
            .marketplace_entries
            .iter()
            .filter(|entry| {
                plugin_host::NativePluginRegistry::registry_entry_supports_current_version(entry)
            })
            .filter_map(|entry| {
                entry
                    .language
                    .clone()
                    .map(|definition| (entry.id.clone(), definition))
            });
        definitions.extend(loaded.chain(catalog).map(|(plugin_id, definition)| {
            oxide_editor::EditorLanguageDefinition {
                plugin_id,
                definition,
            }
        }));
        oxide_editor::EditorLanguagePlugins::update_languages(sources, definitions, cx);
    }

    pub(in crate::workspace) fn runtime_host(
        &self,
    ) -> Arc<tokio::sync::Mutex<plugin_runtime::NativePluginRuntimeHost>> {
        self.runtime_host.clone()
    }

    fn spawn_owned_task<F>(&self, future: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let task_id = self.next_owned_task_id.fetch_add(1, Ordering::Relaxed);
        {
            let mut tasks = self
                .owned_tasks
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            tasks.insert(task_id, None);
        }
        let owned_tasks = Arc::clone(&self.owned_tasks);
        let task = self.task_runtime.spawn(async move {
            future.await;
            owned_tasks
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&task_id);
        });
        let abort_handle = task.abort_handle();
        let mut tasks = self
            .owned_tasks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(handle) = tasks.get_mut(&task_id) {
            *handle = Some(abort_handle);
        }
    }

    pub(in crate::workspace) fn start_runtime_bootstrap(
        &mut self,
        host_api_resolver: plugin_runtime::NativeHostApiResolver,
    ) -> bool {
        if self.release_shutdown_started || self.compatibility_refresh_pending {
            return false;
        }
        let process_plans = self.registry.process_activation_plans();
        let wasm_plans = self.registry.wasm_activation_plans();
        if process_plans.is_empty() && wasm_plans.is_empty() {
            return false;
        }
        self.start_runtime_services();
        for plan in &process_plans {
            let _ = self.registry_mut().mark_runtime_loading(&plan.plugin_id);
        }
        for plan in &wasm_plans {
            let _ = self.registry_mut().mark_runtime_loading(&plan.plugin_id);
        }

        let host = self.runtime_host.clone();
        let delivery_tx = self.runtime_delivery_tx.clone();
        self.spawn_owned_task(async move {
            let mut host = host.lock().await;
            host.set_host_api_resolver(host_api_resolver);
            // Preserve deterministic activation order across process and WASM runtimes.
            for plan in process_plans {
                let plugin_id = plan.plugin_id.clone();
                let result = match super::plugin_lifecycle::native_plugin_permissions(
                    &plan.manifest,
                    true,
                ) {
                    Ok(permissions) => {
                        host.activate_process_plugin(
                            plan.manifest,
                            plan.install_dir,
                            plan.entry,
                            permissions,
                            NATIVE_PLUGIN_LIFECYCLE_TIMEOUT,
                        )
                        .await
                    }
                    Err(error) => Err(error),
                };
                if delivery_tx
                    .send(NativePluginRuntimeDelivery::Activation { plugin_id, result })
                    .is_err()
                {
                    return;
                }
            }
            for plan in wasm_plans {
                let plugin_id = plan.plugin_id.clone();
                let result =
                    match super::plugin_lifecycle::native_plugin_permissions(&plan.manifest, false)
                    {
                        Ok(permissions) => {
                            host.activate_wasm_plugin(
                                plan.manifest,
                                plan.install_dir,
                                plan.entry,
                                permissions,
                                NATIVE_PLUGIN_LIFECYCLE_TIMEOUT,
                            )
                            .await
                        }
                        Err(error) => Err(error),
                    };
                if delivery_tx
                    .send(NativePluginRuntimeDelivery::Activation { plugin_id, result })
                    .is_err()
                {
                    return;
                }
            }
        });
        true
    }

    pub(in crate::workspace) fn start_runtime_command(
        &self,
        plugin_id: String,
        command: String,
        host_api_resolver: plugin_runtime::NativeHostApiResolver,
    ) {
        self.start_runtime_command_with_arguments(
            plugin_id,
            command,
            serde_json::Value::Null,
            host_api_resolver,
        );
    }

    pub(in crate::workspace) fn start_connection_open(
        &self,
        plugin_id: String,
        instance_id: String,
        arguments: serde_json::Value,
        cancel: tokio_util::sync::CancellationToken,
        resolver: plugin_runtime::NativeHostApiResolver,
    ) {
        let mut arguments = arguments;
        let Some(generation) = self.runtime_generations.get(&plugin_id).copied() else {
            oxide_plugin_protocol::zeroize_json_value(&mut arguments);
            return;
        };
        let host = self.runtime_host.clone();
        let delivery = self.runtime_delivery_tx.clone();
        // The owned task and protocol request erase credentials even when activation is retired.
        let encoded =
            Zeroizing::new(serde_json::to_string(&arguments).expect("JSON value serializes"));
        oxide_plugin_protocol::zeroize_json_value(&mut arguments);
        self.spawn_owned_task(async move {
            let mut host = tokio::select! {
                _ = cancel.cancelled() => return,
                host = host.lock() => host,
            };
            if cancel.is_cancelled() || host.process_generation(&plugin_id) != Some(generation) { return; }
            host.set_host_api_resolver(resolver);
            let arguments = serde_json::from_str(&encoded).expect("host serialized JSON");
            drop(encoded);
            let result = host.dispatch_command(&plugin_id, "connection.open".into(), arguments, NATIVE_PLUGIN_LIFECYCLE_TIMEOUT).await;
            if result.is_err() {
                // A timed-out stream cannot safely correlate later responses.
                let _ = host.deactivate_plugin(&plugin_id).await;
            } else if cancel.is_cancelled() {
                let close = host.dispatch_command(&plugin_id, "connection.close".into(), serde_json::json!({"instanceId":instance_id}), NATIVE_PLUGIN_LIFECYCLE_TIMEOUT).await;
                host.dispose_connection_views(&plugin_id, &instance_id);
                if !matches!(close, Ok(ref dispatch) if matches!(dispatch.response.result, plugin_runtime::PluginResponseResult::Ok { .. })) {
                    let _ = host.deactivate_plugin(&plugin_id).await;
                }
                let _ = delivery.send(NativePluginRuntimeDelivery::ConnectionClose { plugin_id: plugin_id.clone(), generation, result: close });
            }
            let _ = delivery.send(NativePluginRuntimeDelivery::ConnectionOpen { plugin_id, instance_id, generation, result });
        });
    }

    pub(in crate::workspace) fn start_connection_close(
        &self,
        plugin_id: String,
        instance_id: String,
        resolver: plugin_runtime::NativeHostApiResolver,
    ) {
        let Some(generation) = self.runtime_generations.get(&plugin_id).copied() else {
            return;
        };
        let host = self.runtime_host.clone();
        let delivery = self.runtime_delivery_tx.clone();
        self.spawn_owned_task(async move {
            let mut host = host.lock().await;
            if host.process_generation(&plugin_id) != Some(generation) { return; }
            host.set_host_api_resolver(resolver);
            let result = host.dispatch_command(&plugin_id, "connection.close".into(), serde_json::json!({"instanceId":instance_id}), NATIVE_PLUGIN_LIFECYCLE_TIMEOUT).await;
            host.dispose_connection_views(&plugin_id, &instance_id);
            if !matches!(result, Ok(ref dispatch) if matches!(dispatch.response.result, plugin_runtime::PluginResponseResult::Ok { .. })) {
                let _ = host.deactivate_plugin(&plugin_id).await;
            }
            let _ = delivery.send(NativePluginRuntimeDelivery::ConnectionClose { plugin_id, generation, result });
        });
    }

    pub(in crate::workspace) fn start_runtime_command_with_arguments(
        &self,
        plugin_id: String,
        command: String,
        arguments: serde_json::Value,
        host_api_resolver: plugin_runtime::NativeHostApiResolver,
    ) {
        if self.release_shutdown_started {
            return;
        }
        let host = self.runtime_host.clone();
        let delivery_tx = self.runtime_delivery_tx.clone();
        let audit_context = plugin_request_audit_context(&plugin_id);
        let scoped_context = audit_context.clone();
        self.spawn_owned_task(oxide_audit::AuditContext::scope_optional(
            scoped_context,
            async move {
                let mut host = host.lock().await;
                host.set_host_api_resolver(host_api_resolver);
                let result = host
                    .dispatch_command(
                        &plugin_id,
                        command,
                        arguments,
                        NATIVE_PLUGIN_LIFECYCLE_TIMEOUT,
                    )
                    .await;
                let _ = delivery_tx.send(NativePluginRuntimeDelivery::CommandDispatch {
                    plugin_id,
                    audit_context,
                    result,
                });
            },
        ));
    }

    pub(in crate::workspace) fn start_runtime_event(
        &self,
        plugin_id: String,
        event: plugin_runtime::PluginEvent,
        host_api_resolver: plugin_runtime::NativeHostApiResolver,
    ) {
        if self.release_shutdown_started {
            return;
        }
        let host = self.runtime_host.clone();
        let delivery_tx = self.runtime_delivery_tx.clone();
        let audit_context = plugin_request_audit_context(&plugin_id);
        let scoped_context = audit_context.clone();
        self.spawn_owned_task(oxide_audit::AuditContext::scope_optional(
            scoped_context,
            async move {
                let mut host = host.lock().await;
                host.set_host_api_resolver(host_api_resolver);
                let result = host
                    .dispatch_event(&plugin_id, event, NATIVE_PLUGIN_LIFECYCLE_TIMEOUT)
                    .await;
                let _ = delivery_tx.send(NativePluginRuntimeDelivery::EventDispatch {
                    plugin_id,
                    audit_context,
                    result,
                });
            },
        ));
    }

    fn start_runtime_deactivation(&mut self, plugin_id: String) {
        self.connections.stop_plugin(&plugin_id);
        if self.release_shutdown_started {
            return;
        }
        // Declarative views are no longer valid once their runtime starts deactivating.
        self.ui_state.remove_plugin(&plugin_id);
        self.active_runtime_plugin_ids.remove(&plugin_id);
        self.runtime_generations.remove(&plugin_id);
        let host = self.runtime_host.clone();
        let delivery_tx = self.runtime_delivery_tx.clone();
        self.spawn_owned_task(async move {
            let result = host.lock().await.deactivate_plugin(&plugin_id).await;
            let _ =
                delivery_tx.send(NativePluginRuntimeDelivery::Deactivation { plugin_id, result });
        });
    }

    pub(in crate::workspace) fn take_runtime_intents(&mut self) -> VecDeque<PluginRuntimeIntent> {
        std::mem::take(&mut self.runtime_intents)
    }

    pub(in crate::workspace) fn start_oxide_import(
        &mut self,
        store: oxide_connections::ConnectionStore,
        plugin_id: String,
        request_id: String,
        bytes: Vec<u8>,
        password: Zeroizing<String>,
        options: oxide_plugin_host_api::sync::NativePluginOxideImportOptions,
        progress_registration_id: Option<String>,
        response_tx: std::sync::mpsc::Sender<plugin_runtime::PluginResponse>,
    ) {
        if self.release_shutdown_started {
            // Dropping these owned inputs also erases the zeroizing password.
            let _ = response_tx.send(plugin_workspace_released_response(request_id));
            return;
        }
        let operation_id = self.next_oxide_import_id;
        self.next_oxide_import_id = self.next_oxide_import_id.wrapping_add(1).max(1);
        let oxide_plugin_host_api::sync::NativePluginOxideImportOptions {
            oxide_options,
            import_app_settings,
            selected_app_settings_sections,
            import_plugin_settings,
            selected_plugin_ids,
            import_quick_commands,
            quick_command_strategy,
        } = options;
        self.oxide_import_contexts.insert(
            operation_id,
            PluginOxideImportContext {
                plugin_id: Arc::from(plugin_id),
                progress_registration_id: progress_registration_id.map(Arc::from),
                request_id,
                options: NativePluginOxidePostImportOptions {
                    import_app_settings,
                    selected_app_settings_sections,
                    import_plugin_settings,
                    selected_plugin_ids,
                    import_quick_commands,
                    quick_command_strategy,
                },
                response_tx,
            },
        );

        let delivery_tx = self.oxide_import_delivery_tx.clone();
        std::thread::spawn(move || {
            let mut store = store;
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                oxide_plugin_host_api::sync::native_plugin_apply_oxide_import_core_with_progress(
                    &mut store,
                    &bytes,
                    &password,
                    oxide_options,
                    |stage, current, total| {
                        let _ = delivery_tx.send(NativePluginOxideImportWorkerMessage::Progress {
                            operation_id,
                            stage: stage.to_string(),
                            current,
                            total,
                        });
                    },
                )
            }))
            .map_err(|_| ())
            .and_then(|result| {
                result
                    .map(|envelope| NativePluginOxideImportCoreResult { store, envelope })
                    .map_err(|error| {
                        // Import errors may include paths or decrypted payload details.
                        let _sensitive_error = Zeroizing::new(error);
                    })
            });
            let _ = delivery_tx.send(NativePluginOxideImportWorkerMessage::Done {
                operation_id,
                result,
            });
        });
    }

    pub(in crate::workspace) fn take_oxide_import_intents(
        &mut self,
    ) -> VecDeque<PluginOxideImportIntent> {
        std::mem::take(&mut self.oxide_import_intents)
    }

    pub(in crate::workspace) fn start_package_install(
        &mut self,
        settings_path: PathBuf,
        expected_id: Option<String>,
        download_url: Zeroizing<String>,
        checksum: Option<String>,
        overwrite: bool,
        retired_desktops: Vec<std::thread::JoinHandle<()>>,
    ) -> bool {
        if self.manager_operation_in_flight() || self.release_shutdown_started {
            return false;
        }
        self.manager_operation_in_flight = true;
        self.mark_package_install(expected_id.as_deref());
        self.remote_desktop_install_pending = self
            .registry
            .remote_desktop_providers()
            .into_iter()
            .filter(|provider| {
                overwrite && expected_id.as_ref().is_none_or(|id| id == &provider.id)
            })
            .map(|provider| provider.id)
            .collect();
        if overwrite
            && expected_id
                .as_deref()
                .is_none_or(|id| id == "com.oxide.terminal.mosh")
        {
            self.remote_desktop_install_pending
                .insert("com.oxide.terminal.mosh".into());
        }
        let delivery_tx = self.manager_delivery_tx.clone();
        let mut audit = plugin_management_audit(
            expected_id.as_deref(),
            if overwrite {
                "plugin_upgrade"
            } else {
                "plugin_install"
            },
        );
        self.spawn_owned_task(async move {
            if !retired_desktops.is_empty() {
                let _ = tokio::task::spawn_blocking(move || {
                    for worker in retired_desktops {
                        let _ = worker.join();
                    }
                })
                .await;
            }
            let result = match (expected_id.as_deref(), checksum.as_deref()) {
                (Some(expected_id), Some(checksum)) => {
                    plugin_host::NativePluginRegistry::install_managed_plugin_package_from_url(
                        &settings_path,
                        expected_id,
                        download_url.trim(),
                        checksum,
                        overwrite,
                    )
                    .await
                }
                (Some(_), None) => Err("Marketplace plugin package is missing SHA-256".to_string()),
                (None, checksum) => {
                    plugin_host::NativePluginRegistry::install_plugin_package_from_url(
                        &settings_path,
                        download_url.trim(),
                        checksum,
                        overwrite,
                    )
                    .await
                }
            };
            let outcome = match result {
                Ok(result) => {
                    audit.summary(&result.manifest.id);
                    audit.finish(
                        oxide_audit::AuditOutcome::Succeeded,
                        oxide_audit::AuditEvidence::Protocol,
                        None,
                        None,
                    );
                    plugin_manager::NativePluginInstallOutcome::Installed(result)
                }
                Err(error) => {
                    let error = Zeroizing::new(error);
                    audit.finish(
                        oxide_audit::AuditOutcome::Failed,
                        oxide_audit::AuditEvidence::Protocol,
                        None,
                        None,
                    );
                    match plugin_host::native_plugin_conflict_id(&error) {
                        Some(plugin_id) => {
                            plugin_manager::NativePluginInstallOutcome::Conflict { plugin_id }
                        }
                        None => plugin_manager::NativePluginInstallOutcome::Failed,
                    }
                }
            };
            // Move the original request values into the result; no duplicate
            // URL or checksum is retained while the package worker runs.
            let _ = delivery_tx.send(plugin_manager::NativePluginManagerDelivery::Install {
                expected_id,
                download_url,
                checksum,
                outcome,
            });
        });
        true
    }

    pub(in crate::workspace) fn start_marketplace_load(&mut self) -> bool {
        if self.manager_state.marketplace_load_state
            == plugin_manager::NativePluginMarketplaceLoadState::Loading
            || self.release_shutdown_started
        {
            return false;
        }
        self.manager_state.marketplace_load_state =
            plugin_manager::NativePluginMarketplaceLoadState::Loading;
        let delivery_tx = self.manager_delivery_tx.clone();
        let settings_path = self.registry.config_path().with_file_name("settings.json");
        let installed_ids = self
            .registry
            .plugins()
            .iter()
            .map(|plugin| plugin.manifest.id.clone())
            .collect::<Vec<_>>();
        self.manager_state.pending_histories.clear();
        self.manager_state.failed_histories.clear();
        self.manager_state.history_page_ids.clear();
        if let Some(cancellation) = self.catalog_history_cancellation.take() {
            cancellation.cancel();
        }

        self.spawn_owned_task(async move {
            let registry = match tokio::time::timeout(NATIVE_PLUGIN_LIFECYCLE_TIMEOUT, async {
                let mut registry =
                    plugin_host::NativePluginRegistry::fetch_official_plugin_registry().await?;

                plugin_host::NativePluginRegistry::hydrate_registry_histories(
                    &settings_path,
                    &mut registry,
                    &installed_ids,
                )
                .await?;
                Ok::<_, String>(registry)
            })
            .await
            {
                Ok(Ok(registry)) => {
                    match plugin_host::NativePluginRegistry::cache_official_catalog(
                        &settings_path,
                        &registry,
                    ) {
                        Ok(()) => Some(registry),
                        Err(error) => {
                            drop(Zeroizing::new(error));
                            None
                        }
                    }
                }
                Ok(Err(error)) => {
                    drop(Zeroizing::new(error));
                    None
                }
                Err(_) => None,
            };
            let _ = delivery_tx
                .send(plugin_manager::NativePluginManagerDelivery::LoadMarketplace(registry));
        });
        true
    }

    pub(in crate::workspace) fn start_catalog_history_load(&mut self, ids: Vec<String>) {
        if self.release_shutdown_started {
            return;
        }
        if ids != self.manager_state.history_page_ids {
            if let Some(cancellation) = self.catalog_history_cancellation.take() {
                cancellation.cancel();
            }
            self.manager_state.pending_histories.clear();
            self.manager_state.history_page_ids = ids.clone();
        }
        let installed = self
            .registry
            .plugins()
            .iter()
            .map(|plugin| plugin.manifest.id.as_str())
            .collect::<Vec<_>>();
        for entry in &mut self.manager_state.marketplace_entries {
            if entry.history.is_some()
                && !ids.contains(&entry.id)
                && !installed.contains(&entry.id.as_str())
            {
                *entry = plugin_host::NativePluginRegistry::registry_summary(entry);
            }
        }
        let expected = self
            .manager_state
            .marketplace_entries
            .iter()
            .filter(|entry| {
                ids.contains(&entry.id)
                    && entry.history_pending()
                    && !self.manager_state.pending_histories.contains(&entry.id)
                    && !self.manager_state.failed_histories.contains(&entry.id)
            })
            .cloned()
            .collect::<Vec<_>>();
        if expected.is_empty() {
            return;
        }
        self.manager_state
            .pending_histories
            .extend(expected.iter().map(|entry| entry.id.clone()));
        let settings = self.registry.config_path().with_file_name("settings.json");
        let tx = self.manager_delivery_tx.clone();
        let cancellation = tokio_util::sync::CancellationToken::new();
        self.catalog_history_cancellation = Some(cancellation.clone());
        self.spawn_owned_task(async move {
            let results = tokio::select! {
                _ = cancellation.cancelled() => return,
                results = plugin_host::NativePluginRegistry::fetch_registry_histories(&settings,&expected) => results,
            };
            let _ = tx.send(plugin_manager::NativePluginManagerDelivery::CatalogHistories {expected,results});
        });
    }

    pub(in crate::workspace) fn start_compatibility_refresh(&mut self) {
        // The owned fetch is cancelled with this entity; offline startup uses the last cache.
        if !self.registry.plugins().is_empty() && self.start_marketplace_load() {
            self.compatibility_refresh_pending = true;
        }
    }

    pub(in crate::workspace) fn start_update_check(
        &mut self,
        registry_url: Zeroizing<String>,
        installed: Vec<plugin_host::NativePluginInstalledInfo>,
    ) -> bool {
        if self.manager_operation_in_flight() || self.release_shutdown_started {
            return false;
        }
        self.manager_operation_in_flight = true;
        let delivery_tx = self.manager_delivery_tx.clone();
        let settings_path = self.registry.config_path().with_file_name("settings.json");
        self.spawn_owned_task(async move {
            let result =
                match plugin_host::NativePluginRegistry::fetch_plugin_registry(registry_url.trim())
                    .await
                {
                    Ok(mut index) => {
                        let ids = installed
                            .iter()
                            .map(|plugin| plugin.id.clone())
                            .collect::<Vec<_>>();
                        match plugin_host::NativePluginRegistry::hydrate_registry_histories(
                            &settings_path,
                            &mut index,
                            &ids,
                        )
                        .await
                        {
                            Ok(()) => {
                                Some(plugin_host::NativePluginRegistry::check_plugin_updates(
                                    index, &installed,
                                ))
                            }
                            Err(error) => {
                                drop(Zeroizing::new(error));
                                None
                            }
                        }
                    }
                    Err(error) => {
                        // Registry errors may echo credential-bearing URLs.
                        drop(Zeroizing::new(error));
                        None
                    }
                };
            let _ = delivery_tx.send(plugin_manager::NativePluginManagerDelivery::CheckUpdates(
                result,
            ));
        });
        true
    }

    pub(in crate::workspace) fn apply_manager_deliveries(
        &mut self,
        settings_path: &std::path::Path,
        i18n: &I18n,
        cx: &mut App,
    ) -> (bool, Vec<PathBuf>) {
        let mut bootstrap_runtime = false;
        let mut imported_backgrounds = Vec::new();
        while let Some(delivery) = self.manager_deliveries.pop_front() {
            if let plugin_manager::NativePluginManagerDelivery::Install {
                outcome: plugin_manager::NativePluginInstallOutcome::Installed(result),
                ..
            } = &delivery
            {
                imported_backgrounds.extend_from_slice(&result.imported_backgrounds);
            }
            bootstrap_runtime |= self.apply_manager_delivery(delivery, settings_path, i18n, cx);
        }
        (bootstrap_runtime, imported_backgrounds)
    }

    fn apply_manager_delivery(
        &mut self,
        delivery: plugin_manager::NativePluginManagerDelivery,
        settings_path: &std::path::Path,
        i18n: &I18n,
        cx: &mut App,
    ) -> bool {
        match delivery {
            plugin_manager::NativePluginManagerDelivery::CatalogHistories { expected, results } => {
                let mut changed = false;
                for (id, result) in results {
                    if !self.manager_state.history_page_ids.contains(&id) {
                        continue;
                    }
                    let Some(original) = expected.iter().find(|entry| entry.id == id) else {
                        continue;
                    };
                    let Some(entry) = self
                        .manager_state
                        .marketplace_entries
                        .iter_mut()
                        .find(|entry| entry.id == id && entry.history == original.history)
                    else {
                        continue;
                    };
                    self.manager_state.pending_histories.remove(&id);
                    match result {
                        Ok(history) => {
                            *entry = history;
                            changed = true;
                        }
                        Err(error) => {
                            drop(Zeroizing::new(error));
                            self.manager_state.failed_histories.insert(id);
                        }
                    }
                }
                if changed {
                    let _ = plugin_host::NativePluginRegistry::cache_official_catalog(
                        settings_path,
                        &plugin_host::NativePluginRegistryIndex {
                            version: self.manager_state.catalog_version,
                            plugins: self.manager_state.marketplace_entries.clone(),
                        },
                    );
                }
                self.manager_state.section_list_state.splice(
                    plugin_manager::PLUGIN_MANAGER_TABBED_CONTENT_SECTION_INDEX
                        ..plugin_manager::PLUGIN_MANAGER_TABBED_CONTENT_SECTION_INDEX + 1,
                    1,
                );
                false
            }
            plugin_manager::NativePluginManagerDelivery::Install {
                expected_id,
                download_url,
                checksum,
                outcome,
            } => {
                self.manager_state.active_install = None;
                if let Some(id) = &expected_id {
                    if matches!(&outcome, plugin_manager::NativePluginInstallOutcome::Failed) {
                        self.manager_state.failed_install_ids.insert(id.clone());
                    } else {
                        self.manager_state.failed_install_ids.remove(id);
                    }
                }
                match outcome {
                    plugin_manager::NativePluginInstallOutcome::Installed(result) => {
                        let installed_id = result.manifest.id.clone();
                        self.manager_state.failed_install_ids.remove(&installed_id);
                        let message = i18n
                            .t("plugin.url_install_success")
                            .replace("{{name}}", &result.manifest.name);
                        self.replace_registry(
                            plugin_host::NativePluginRegistry::discover(settings_path),
                            cx,
                        );
                        self.manager_state
                            .available_updates
                            .retain(|entry| entry.id != installed_id);
                        self.manager_state.pending_overwrite = None;
                        self.manager_state.operation_status =
                            plugin_manager::NativePluginManagerOperationStatus::Success(message);
                        true
                    }
                    plugin_manager::NativePluginInstallOutcome::Conflict { plugin_id } => {
                        // The retry keeps the only secret-bearing URL owner.
                        self.manager_state.pending_overwrite =
                            Some(plugin_manager::NativePluginPendingOverwrite {
                                plugin_id,
                                expected_id,
                                download_url,
                                checksum,
                            });
                        self.manager_state.operation_status =
                            plugin_manager::NativePluginManagerOperationStatus::Error(
                                i18n.t("plugin.url_conflict_title"),
                            );
                        false
                    }
                    plugin_manager::NativePluginInstallOutcome::Failed => {
                        self.manager_state.operation_status =
                            plugin_manager::NativePluginManagerOperationStatus::Error(
                                i18n.t("plugin.install_error"),
                            );
                        false
                    }
                }
            }
            plugin_manager::NativePluginManagerDelivery::LoadMarketplace(result) => {
                let initial_refresh = std::mem::take(&mut self.compatibility_refresh_pending);
                if initial_refresh && result.is_some() {
                    self.replace_registry(
                        plugin_host::NativePluginRegistry::discover(settings_path),
                        cx,
                    );
                }
                match result {
                    Some(registry) => {
                        let installed = self
                            .registry
                            .plugins()
                            .iter()
                            .map(|plugin| plugin_host::NativePluginInstalledInfo {
                                id: plugin.manifest.id.clone(),
                                version: plugin.manifest.version.clone(),
                            })
                            .collect::<Vec<_>>();
                        self.manager_state.available_updates =
                            plugin_host::NativePluginRegistry::check_plugin_updates(
                                registry.clone(),
                                &installed,
                            );
                        self.manager_state.catalog_version = registry.version;
                        self.manager_state.marketplace_entries = registry.plugins;
                        self.sync_language_plugins(cx);
                        self.manager_state.marketplace_load_state =
                            plugin_manager::NativePluginMarketplaceLoadState::Loaded;
                        self.manager_state.operation_status =
                            plugin_manager::NativePluginManagerOperationStatus::Idle;
                    }
                    None => {
                        self.manager_state.marketplace_load_state =
                            plugin_manager::NativePluginMarketplaceLoadState::Failed;
                        self.manager_state.operation_status =
                            plugin_manager::NativePluginManagerOperationStatus::Error(
                                i18n.t("plugin.marketplace_load_error"),
                            );
                    }
                }
                self.manager_state.section_list_state.splice(
                    plugin_manager::PLUGIN_MANAGER_TABBED_CONTENT_SECTION_INDEX
                        ..plugin_manager::PLUGIN_MANAGER_TABBED_CONTENT_SECTION_INDEX + 1,
                    1,
                );
                initial_refresh
            }
            plugin_manager::NativePluginManagerDelivery::CheckUpdates(result) => {
                match result {
                    Some(updates) => {
                        let update_count = updates.len();
                        self.manager_state.available_updates = updates;
                        self.manager_state.operation_status =
                            plugin_manager::NativePluginManagerOperationStatus::Success(format!(
                                "{update_count} {}",
                                i18n.t("plugin.updates")
                            ));
                    }
                    None => {
                        self.manager_state.operation_status =
                            plugin_manager::NativePluginManagerOperationStatus::Error(
                                i18n.t("plugin.registry_error"),
                            );
                    }
                }
                false
            }
        }
    }

    pub(in crate::workspace) fn runtime_request_senders(&self) -> PluginRuntimeRequestSenders {
        // These are lightweight channel endpoints; request payloads stay unique
        // and are moved through the channels without being cloned.
        PluginRuntimeRequestSenders {
            confirm: self.confirm_tx.clone(),
            terminal: self.terminal_tx.clone(),
            sync: self.sync_tx.clone(),
        }
    }

    pub(in crate::workspace) fn promote_confirm_request(&mut self) -> bool {
        if self.confirm.is_some() {
            return false;
        }
        let Ok(request) = self.confirm_rx.try_recv() else {
            return false;
        };
        self.confirm = Some(request.into());
        self.confirm_presence.reopen();
        true
    }

    pub(in crate::workspace) fn confirm_dialog(&self) -> Option<&NativePluginConfirmDialog> {
        self.confirm.as_ref()
    }

    pub(in crate::workspace) fn confirm_phase(&self) -> oxide_ui::motion::ExitPhase {
        self.confirm_presence.phase()
    }

    pub(in crate::workspace) fn begin_confirm_exit(&mut self, confirmed: bool) -> Option<u64> {
        let dialog = self.confirm.as_mut()?;
        let generation = self.confirm_presence.begin_exit()?;
        // Resolve exactly once while retaining the dialog for its exit frame.
        dialog.respond(confirmed);
        Some(generation)
    }

    pub(in crate::workspace) fn finish_confirm_exit(&mut self, generation: u64) -> bool {
        if !self.confirm_presence.finish_exit(generation) {
            return false;
        }
        self.confirm = None;
        self.promote_confirm_request()
    }

    pub(in crate::workspace) fn take_terminal_requests(
        &mut self,
    ) -> delivery::ChannelDrain<NativePluginTerminalRequest> {
        delivery::drain_channel(&self.terminal_rx, delivery::USER_ACTION_DELIVERY_BUDGET)
    }

    pub(in crate::workspace) fn take_sync_requests(
        &mut self,
    ) -> delivery::ChannelDrain<NativePluginSyncRequest> {
        delivery::drain_channel(&self.sync_rx, delivery::USER_ACTION_DELIVERY_BUDGET)
    }

    pub(in crate::workspace) fn enqueue_product_ui_effect(
        &mut self,
        effect: NativePluginProductUiEffect,
    ) {
        self.product_ui_effects.push_back(effect);
        self.runtime_request_wake.mark();
    }

    pub(in crate::workspace) fn take_product_ui_effects(
        &mut self,
    ) -> (VecDeque<NativePluginProductUiEffect>, bool) {
        let started_at = Instant::now();
        let mut effects = VecDeque::new();
        while delivery::USER_ACTION_DELIVERY_BUDGET.allows_next(effects.len(), started_at.elapsed())
        {
            let Some(effect) = self.product_ui_effects.pop_front() else {
                break;
            };
            effects.push_back(effect);
        }
        (effects, !self.product_ui_effects.is_empty())
    }

    pub(in crate::workspace) fn mark_runtime_requests_ready(&self) {
        self.runtime_request_wake.mark();
    }

    pub(in crate::workspace) fn start_runtime_services(&mut self) -> bool {
        if self.runtime_services_started || self.release_shutdown_started {
            return false;
        }
        self.runtime_services_started = true;
        true
    }

    fn schedule_release_shutdown(&self, cx: &mut Context<Self>) {
        cx.on_release(|entity, _| {
            entity.begin_release_shutdown();
        })
        .detach();
    }

    fn begin_release_shutdown(&mut self) -> bool {
        if self.release_shutdown_started {
            return false;
        }
        self.release_shutdown_started = true;
        self.retire_security_key(None);
        self.uninstall_task.take();
        self.runtime_services_started = false;
        self.manager_operation_in_flight = false;
        // Dropping queued requests erases their URL buffers with the runtime owner.
        self.manager_state.install_queue.clear();
        self.manager_state.active_install = None;
        self.manager_state.pending_overwrite = None;
        self.subscription_sampler_running = false;
        self.subscription_sampler_generation = self.subscription_sampler_generation.wrapping_add(1);
        self.subscription_sampler_task.take();
        self.subscription_samples.clear();
        self.subscription_snapshots.clear();

        // Stopping every wake rejects new foreground delivery before receivers drop.
        self.manager_delivery_tx.wake().stop();
        self.runtime_delivery_tx.wake().stop();
        self.runtime_request_wake.stop();
        self.oxide_import_delivery_tx.wake().stop();
        self.reject_pending_runtime_requests();

        let abort_handles = self
            .owned_tasks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .drain()
            .filter_map(|(_, handle)| handle)
            .collect::<Vec<_>>();
        for abort_handle in abort_handles {
            abort_handle.abort();
        }
        // Dropping queued producer results releases owned URLs and import data promptly.
        while let Ok(delivery) = self.runtime_delivery_rx.try_recv() {
            drop(delivery);
        }
        while let Ok(delivery) = self.manager_delivery_rx.try_recv() {
            drop(delivery);
        }
        while let Ok(delivery) = self.oxide_import_delivery_rx.try_recv() {
            drop(delivery);
        }

        let mut plugin_ids = std::mem::take(&mut self.active_runtime_plugin_ids);
        plugin_ids.extend(
            self.registry
                .plugins()
                .iter()
                .filter(|plugin| {
                    matches!(
                        plugin.state,
                        plugin_host::NativePluginState::ReadyProcess
                            | plugin_host::NativePluginState::ReadyWasm
                            | plugin_host::NativePluginState::Loading
                            | plugin_host::NativePluginState::Active
                    )
                })
                .map(|plugin| plugin.manifest.id.clone()),
        );
        let mut plugin_ids = plugin_ids.into_iter().collect::<Vec<_>>();
        plugin_ids.sort_unstable();

        let runtime_host = Arc::clone(&self.runtime_host);
        self.task_runtime.spawn(async move {
            // One total deadline bounds lock acquisition and every process/WASM teardown.
            let _ = tokio::time::timeout(NATIVE_PLUGIN_RELEASE_SHUTDOWN_TIMEOUT, async move {
                let mut runtime_host = runtime_host.lock().await;
                for plugin_id in plugin_ids {
                    let _ = runtime_host.deactivate_plugin(&plugin_id).await;
                }
            })
            .await;
        });
        true
    }

    fn reject_pending_runtime_requests(&mut self) {
        if let Some(mut dialog) = self.confirm.take() {
            dialog.respond(false);
        }
        while let Ok(request) = self.confirm_rx.try_recv() {
            let _ = request.response_tx.send(false);
        }
        while let Ok(request) = self.terminal_rx.try_recv() {
            let response = plugin_workspace_released_response(request.request_id);
            let _ = request.response_tx.send(response);
            // The action is dropped in place; terminal text is never cloned for shutdown.
            drop(request.action);
        }
        while let Ok(request) = self.sync_rx.try_recv() {
            let response = plugin_workspace_released_response(request.request_id);
            let _ = request.response_tx.send(response);
            // ImportOxide owns a Zeroizing password that is erased when this action drops.
            drop(request.action);
        }
        for (_, context) in self.oxide_import_contexts.drain() {
            let response = plugin_workspace_released_response(context.request_id);
            let _ = context.response_tx.send(response);
        }
        while let Some(intent) = self.oxide_import_intents.pop_front() {
            if let PluginOxideImportIntent::Complete {
                request_id,
                response_tx,
                ..
            } = intent
            {
                let _ = response_tx.send(plugin_workspace_released_response(request_id));
            }
        }
        self.product_ui_effects.clear();
        self.runtime_intents.clear();
        self.manager_deliveries.clear();
    }

    pub(in crate::workspace) fn configure_subscription_samples(
        &mut self,
        samples: Vec<(PluginSubscriptionSample, serde_json::Value)>,
        event_log_last_id: Option<u64>,
        cx: &mut Context<Self>,
    ) {
        if self.release_shutdown_started {
            return;
        }
        // These samples serve registered runtime event subscriptions. They are
        // intentionally independent from Plugin Manager page visibility.
        let sample_kinds = samples.iter().map(|(kind, _)| *kind).collect::<Vec<_>>();
        self.subscription_samples = sample_kinds;
        self.subscription_snapshots = samples.into_iter().collect();
        if let Some(last_id) = event_log_last_id {
            self.event_log_last_id = last_id;
        }

        if self.subscription_samples.is_empty() {
            self.subscription_sampler_running = false;
            self.subscription_sampler_generation =
                self.subscription_sampler_generation.wrapping_add(1);
            self.subscription_sampler_task.take();
            return;
        }
        if self.subscription_sampler_running {
            return;
        }

        self.subscription_sampler_running = true;
        self.subscription_sampler_generation = self.subscription_sampler_generation.wrapping_add(1);
        let generation = self.subscription_sampler_generation;
        self.subscription_sampler_task = Some(cx.spawn(async move |entity, cx| {
            loop {
                Timer::after(NATIVE_PLUGIN_DELIVERY_POLL_INTERVAL).await;
                let keep_running = entity
                    .update(cx, |entity, cx| {
                        if !entity.subscription_sampler_running
                            || entity.subscription_sampler_generation != generation
                        {
                            return false;
                        }
                        cx.emit(PluginWorkspaceEvent::RuntimeSubscriptionSampleDue);
                        true
                    })
                    .unwrap_or(false);
                if !keep_running {
                    break;
                }
            }
        }));
    }

    pub(in crate::workspace) fn subscription_samples(&self) -> Vec<PluginSubscriptionSample> {
        self.subscription_samples.clone()
    }

    pub(in crate::workspace) fn update_subscription_snapshot(
        &mut self,
        kind: PluginSubscriptionSample,
        next: serde_json::Value,
    ) -> (Option<serde_json::Value>, serde_json::Value) {
        let Some(previous) = self.subscription_snapshots.get_mut(&kind) else {
            return (None, next);
        };
        if *previous == next {
            return (None, next);
        }
        let retained = next.clone();
        let previous = std::mem::replace(previous, retained);
        (Some(previous), next)
    }

    pub(in crate::workspace) fn transfer_progress_due(&mut self, interval: Duration) -> bool {
        let due = self
            .transfer_progress_last_emitted
            .map(|last_emitted| last_emitted.elapsed() >= interval)
            .unwrap_or(true);
        if due {
            self.transfer_progress_last_emitted = Some(Instant::now());
        }
        due
    }

    pub(in crate::workspace) fn runtime_profiler_metrics_due(
        &mut self,
        interval: Duration,
    ) -> bool {
        let due = self
            .runtime_profiler_last_emitted
            .map(|last_emitted| last_emitted.elapsed() >= interval)
            .unwrap_or(true);
        if due {
            self.runtime_profiler_last_emitted = Some(Instant::now());
        }
        due
    }

    pub(in crate::workspace) fn advance_event_log_last_id(&mut self, next_last_id: u64) -> u64 {
        std::mem::replace(&mut self.event_log_last_id, next_last_id)
    }

    fn schedule_runtime_delivery(&self, cx: &mut Context<Self>) {
        let delivery_wake = self.runtime_delivery_tx.wake();
        let release_wake = delivery_wake.clone();
        cx.on_release(move |_, _| {
            // Runtime workers may finish independently, but this UI waiter is workspace-scoped.
            release_wake.stop();
        })
        .detach();
        cx.spawn(async move |entity, cx| {
            loop {
                delivery_wake.wait().await;
                let should_drain = delivery_wake.take();
                let stopped = delivery_wake.is_stopped();
                if should_drain {
                    let backlog_remaining = entity
                        .update(cx, |entity, cx| entity.drain_runtime_deliveries(cx))
                        .unwrap_or(false);
                    if backlog_remaining {
                        delivery_wake.mark();
                    }
                }
                if stopped {
                    break;
                }
            }
        })
        .detach();
    }
    fn schedule_runtime_pushes(&self) {
        let host = self.runtime_host.clone();
        let delivery = self.runtime_delivery_tx.clone();
        // The entity cancels this owner on release. Acknowledgement bounds UI
        // backlog even when the plugin produces faster than GPUI can render.
        self.spawn_owned_task(async move {
            let wake = host.lock().await.output_wake();
            loop {
                wake.notified().await;
                let (resolver, ready) = tokio::sync::oneshot::channel();
                if delivery
                    .send(NativePluginRuntimeDelivery::PreparePush { resolver })
                    .is_err()
                {
                    return;
                }
                let Ok(resolver) = ready.await else {
                    return;
                };
                let pushes = {
                    let mut host = host.lock().await;
                    host.set_host_api_resolver(resolver);
                    host.receive_pending_output().await
                };
                for (plugin_id, generation, result) in pushes {
                    let (consumed, receipt) = tokio::sync::oneshot::channel();
                    if delivery
                        .send(NativePluginRuntimeDelivery::Push {
                            plugin_id,
                            generation,
                            result,
                            consumed,
                        })
                        .is_err()
                    {
                        return;
                    }
                    if receipt.await.is_err() {
                        return;
                    }
                }
            }
        });
    }

    fn drain_runtime_deliveries(&mut self, cx: &mut Context<Self>) -> bool {
        let drain = delivery::drain_channel(
            &self.runtime_delivery_rx,
            delivery::LIFECYCLE_DELIVERY_BUDGET,
        );
        if !drain.items.is_empty() {
            for delivery in drain.items {
                let intent = self.apply_runtime_delivery(delivery);
                self.runtime_intents.push_back(intent);
            }
            cx.emit(PluginWorkspaceEvent::RuntimeIntentsReady);
            cx.notify();
        }
        drain.outcome.backlog_remaining
    }

    fn apply_runtime_delivery(
        &mut self,
        delivery: NativePluginRuntimeDelivery,
    ) -> PluginRuntimeIntent {
        match delivery {
            NativePluginRuntimeDelivery::ConnectionClose {
                plugin_id,
                generation,
                result,
            } => {
                if self.runtime_generations.get(&plugin_id) != Some(&generation) {
                    return PluginRuntimeIntent::StateChanged;
                }
                if !matches!(result, Ok(ref dispatch) if matches!(dispatch.response.result, plugin_runtime::PluginResponseResult::Ok { .. }))
                {
                    self.start_runtime_deactivation(plugin_id.clone());
                    let _ = self.registry_mut().mark_runtime_error(
                        &plugin_id,
                        NATIVE_PLUGIN_RUNTIME_FAILURE_DIAGNOSTIC.into(),
                    );
                    return PluginRuntimeIntent::StateChanged;
                }
                self.apply_command_dispatch_result(plugin_id, None, result)
            }
            NativePluginRuntimeDelivery::ConnectionOpen {
                plugin_id,
                instance_id,
                generation,
                result,
            } => {
                if self.runtime_generations.get(&plugin_id) != Some(&generation) {
                    return PluginRuntimeIntent::StateChanged;
                }
                if result.is_err() {
                    self.start_runtime_deactivation(plugin_id.clone());
                    let _ = self.registry_mut().mark_runtime_error(
                        &plugin_id,
                        NATIVE_PLUGIN_RUNTIME_FAILURE_DIAGNOSTIC.into(),
                    );
                    return PluginRuntimeIntent::StateChanged;
                }
                let value = result
                    .as_ref()
                    .ok()
                    .and_then(|dispatch| match &dispatch.response.result {
                        plugin_runtime::PluginResponseResult::Ok { value } => Some(value.clone()),
                        _ => None,
                    })
                    .unwrap_or(serde_json::Value::Null);
                self.enqueue_product_ui_effect(NativePluginProductUiEffect {
                    plugin_id: plugin_id.clone(),
                    audit_context: None,
                    namespace: "pluginConnection".into(),
                    method: "opened".into(),
                    args: serde_json::json!({"instanceId":instance_id,"value":value}),
                });
                self.apply_command_dispatch_result(plugin_id, None, result)
            }
            NativePluginRuntimeDelivery::PreparePush { resolver } => {
                PluginRuntimeIntent::PreparePush { resolver }
            }
            NativePluginRuntimeDelivery::Push {
                plugin_id,
                generation,
                result,
                consumed,
            } => {
                let intent = self.apply_runtime_push(plugin_id, generation, result);
                let _ = consumed.send(());
                intent
            }
            NativePluginRuntimeDelivery::Activation { plugin_id, result } => {
                self.apply_activation_result(plugin_id, result)
            }
            NativePluginRuntimeDelivery::Deactivation { plugin_id, result } => {
                self.apply_deactivation_result(plugin_id, result)
            }
            NativePluginRuntimeDelivery::CommandDispatch {
                plugin_id,
                audit_context,
                result,
            } => self.apply_command_dispatch_result(plugin_id, audit_context, result),
            NativePluginRuntimeDelivery::EventDispatch {
                plugin_id,
                audit_context,
                result,
            } => self.apply_event_dispatch_result(plugin_id, audit_context, result),
        }
    }
    fn apply_runtime_push(
        &mut self,
        plugin_id: String,
        generation: u64,
        result: Result<plugin_runtime::NativePluginRuntimePush, plugin_runtime::PluginError>,
    ) -> PluginRuntimeIntent {
        if !self.active_runtime_plugin_ids.contains(&plugin_id)
            || self.runtime_generations.get(&plugin_id) != Some(&generation)
        {
            return PluginRuntimeIntent::StateChanged;
        }
        let push = match result {
            Ok(push) => push,
            Err(error) => {
                self.start_runtime_deactivation(plugin_id.clone());
                self.registry_mut()
                    .cleanup_runtime_plugin_contributions(&plugin_id);
                let _ = self
                    .registry_mut()
                    .mark_runtime_error(&plugin_id, native_plugin_runtime_failure_message(error));
                return PluginRuntimeIntent::StateChanged;
            }
        };
        for message in &push.messages {
            if let Err(error) = self.apply_plugin_message(&plugin_id, message) {
                self.start_runtime_deactivation(plugin_id.clone());
                self.registry_mut()
                    .cleanup_runtime_plugin_contributions(&plugin_id);
                let _ = self.registry_mut().mark_runtime_error(&plugin_id, error);
                return PluginRuntimeIntent::StateChanged;
            }
        }
        let refresh = if push.messages.iter().all(|message| matches!(message,
            plugin_runtime::PluginOutboundMessage::RegisterContribution { registration }
                if matches!(registration.kind, plugin_runtime::PluginRegistrationKind::Tab | plugin_runtime::PluginRegistrationKind::SidebarPanel)
        )) { PluginRuntimeAdapterRefresh::None } else { PluginRuntimeAdapterRefresh::All };
        PluginRuntimeIntent::ApplyEffects {
            plugin_id,
            audit_context: None,
            effects: push.effects,
            refresh,
        }
    }

    fn apply_activation_result(
        &mut self,
        plugin_id: String,
        result: Result<plugin_runtime::NativePluginRuntimeActivation, plugin_runtime::PluginError>,
    ) -> PluginRuntimeIntent {
        let activation = match result {
            Ok(activation) => activation,
            Err(error) => {
                let message = native_plugin_runtime_failure_message(error);
                let _ = self.registry_mut().mark_runtime_error(&plugin_id, message);
                return PluginRuntimeIntent::StateChanged;
            }
        };

        let plugin_runtime::NativePluginRuntimeActivation {
            plugin_id: activated_plugin_id,
            generation,
            response,
            messages,
            effects,
        } = activation;
        if activated_plugin_id != plugin_id {
            self.start_runtime_deactivation(activated_plugin_id.clone());
            let _ = self.registry_mut().mark_runtime_error(
                &plugin_id,
                format!(
                    "Runtime activated plugin \"{}\" while loading \"{}\"",
                    activated_plugin_id, plugin_id
                ),
            );
            return PluginRuntimeIntent::StateChanged;
        }

        for message in &messages {
            if let Err(error) = self.apply_plugin_message(&plugin_id, message) {
                self.registry_mut()
                    .cleanup_runtime_plugin_contributions(&plugin_id);
                let _ = self.registry_mut().mark_runtime_error(&plugin_id, error);
                self.start_runtime_deactivation(plugin_id);
                return PluginRuntimeIntent::StateChanged;
            }
        }

        match response.result {
            plugin_runtime::PluginResponseResult::Ok { .. } => {
                let _ = self.registry_mut().mark_runtime_active(&plugin_id);
                self.active_runtime_plugin_ids.insert(plugin_id.clone());
                self.runtime_generations
                    .insert(plugin_id.clone(), generation);
            }
            plugin_runtime::PluginResponseResult::Error { error } => {
                self.registry_mut()
                    .cleanup_runtime_plugin_contributions(&plugin_id);
                let message = native_plugin_runtime_failure_message(error);
                let _ = self.registry_mut().mark_runtime_error(&plugin_id, message);
                self.start_runtime_deactivation(plugin_id.clone());
            }
        }

        PluginRuntimeIntent::ApplyEffects {
            plugin_id,
            audit_context: None,
            effects,
            refresh: PluginRuntimeAdapterRefresh::TerminalHooks,
        }
    }

    fn apply_deactivation_result(
        &mut self,
        plugin_id: String,
        result: Result<plugin_runtime::PluginResponse, plugin_runtime::PluginError>,
    ) -> PluginRuntimeIntent {
        self.active_runtime_plugin_ids.remove(&plugin_id);
        self.runtime_generations.remove(&plugin_id);
        self.registry_mut()
            .cleanup_runtime_plugin_contributions(&plugin_id);
        match result {
            Ok(response) => {
                if let plugin_runtime::PluginResponseResult::Error { error } = response.result {
                    let message = native_plugin_runtime_failure_message(error);
                    self.registry_mut()
                        .record_manager_error(plugin_id.clone(), message);
                }
            }
            Err(error) => {
                let message = native_plugin_runtime_failure_message(error);
                self.registry_mut()
                    .record_manager_error(plugin_id.clone(), message);
            }
        }
        PluginRuntimeIntent::ApplyEffects {
            plugin_id,
            audit_context: None,
            effects: Vec::new(),
            refresh: PluginRuntimeAdapterRefresh::All,
        }
    }

    fn apply_command_dispatch_result(
        &mut self,
        plugin_id: String,
        audit_context: Option<oxide_audit::AuditContext>,
        result: Result<
            plugin_runtime::NativePluginRuntimeCommandDispatch,
            plugin_runtime::PluginError,
        >,
    ) -> PluginRuntimeIntent {
        let dispatch = match result {
            Ok(dispatch) => dispatch,
            Err(error) => {
                let message = native_plugin_runtime_failure_message(error);
                self.registry_mut().record_manager_error(plugin_id, message);
                return PluginRuntimeIntent::StateChanged;
            }
        };

        let plugin_runtime::NativePluginRuntimeCommandDispatch {
            plugin_id: dispatched_plugin_id,
            command: _command,
            response,
            messages,
            effects,
        } = dispatch;
        if dispatched_plugin_id != plugin_id {
            self.registry_mut().record_manager_error(
                plugin_id,
                NATIVE_PLUGIN_RUNTIME_FAILURE_DIAGNOSTIC.to_string(),
            );
            return PluginRuntimeIntent::StateChanged;
        }

        for message in &messages {
            if let Err(error) = self.apply_plugin_message(&dispatched_plugin_id, message) {
                self.registry_mut().record_manager_error(
                    dispatched_plugin_id.clone(),
                    format!("Native plugin command contribution update failed: {error}"),
                );
            }
        }
        if let plugin_runtime::PluginResponseResult::Error { error } = response.result {
            let message = native_plugin_runtime_failure_message(error);
            self.registry_mut()
                .record_manager_error(dispatched_plugin_id.clone(), message);
        }

        PluginRuntimeIntent::ApplyEffects {
            plugin_id: dispatched_plugin_id,
            audit_context,
            effects,
            refresh: PluginRuntimeAdapterRefresh::TerminalHooks,
        }
    }

    fn apply_plugin_message(
        &mut self,
        plugin: &str,
        message: &plugin_runtime::PluginOutboundMessage,
    ) -> Result<(), String> {
        if let Some(result) = self.connections.apply_message(plugin, message) {
            return result;
        }
        self.registry_mut()
            .apply_runtime_outbound_message(plugin, message)
    }

    fn apply_event_dispatch_result(
        &mut self,
        plugin_id: String,
        audit_context: Option<oxide_audit::AuditContext>,
        result: Result<
            plugin_runtime::NativePluginRuntimeEventDispatch,
            plugin_runtime::PluginError,
        >,
    ) -> PluginRuntimeIntent {
        let dispatch = match result {
            Ok(dispatch) => dispatch,
            Err(error) => {
                let message = native_plugin_runtime_failure_message(error);
                self.registry_mut().record_manager_error(plugin_id, message);
                return PluginRuntimeIntent::StateChanged;
            }
        };

        let plugin_runtime::NativePluginRuntimeEventDispatch {
            plugin_id: dispatched_plugin_id,
            event: _event,
            response,
            messages,
            effects,
        } = dispatch;
        if dispatched_plugin_id != plugin_id {
            self.registry_mut().record_manager_error(
                plugin_id,
                NATIVE_PLUGIN_RUNTIME_FAILURE_DIAGNOSTIC.to_string(),
            );
            return PluginRuntimeIntent::StateChanged;
        }

        for message in &messages {
            if let Err(error) = self.apply_plugin_message(&dispatched_plugin_id, message) {
                self.registry_mut().record_manager_error(
                    dispatched_plugin_id.clone(),
                    format!("Native plugin event contribution update failed: {error}"),
                );
            }
        }
        if let plugin_runtime::PluginResponseResult::Error { error } = response.result {
            let message = native_plugin_runtime_failure_message(error);
            self.registry_mut()
                .record_manager_error(dispatched_plugin_id.clone(), message);
        }

        PluginRuntimeIntent::ApplyEffects {
            plugin_id: dispatched_plugin_id,
            audit_context,
            effects,
            refresh: PluginRuntimeAdapterRefresh::TerminalInputInterceptors,
        }
    }

    fn schedule_manager_delivery(&self, cx: &mut Context<Self>) {
        let delivery_wake = self.manager_delivery_tx.wake();
        let release_wake = delivery_wake.clone();
        cx.on_release(move |_, _| {
            // The entity release path stops this waiter and aborts its package producer.
            release_wake.stop();
        })
        .detach();
        cx.spawn(async move |entity, cx| {
            loop {
                delivery_wake.wait().await;
                let should_drain = delivery_wake.take();
                let stopped = delivery_wake.is_stopped();
                if should_drain {
                    let backlog_remaining = entity
                        .update(cx, |entity, cx| entity.drain_manager_deliveries(cx))
                        .unwrap_or(false);
                    if backlog_remaining {
                        delivery_wake.mark();
                    }
                }
                if stopped {
                    break;
                }
            }
        })
        .detach();
    }

    fn schedule_runtime_request_delivery(&self, cx: &mut Context<Self>) {
        let request_wake = self.runtime_request_wake.clone();
        let release_wake = request_wake.clone();
        cx.on_release(move |_, _| {
            // The entity owns this waiter for the full workspace lifetime.
            release_wake.stop();
        })
        .detach();
        cx.spawn(async move |_entity, cx| {
            loop {
                request_wake.wait().await;
                let should_deliver = request_wake.take();
                let stopped = request_wake.is_stopped();
                if should_deliver {
                    let _ = _entity.update(cx, |_entity, cx| {
                        cx.emit(PluginWorkspaceEvent::RuntimeRequestsReady);
                    });
                }
                if stopped {
                    break;
                }
            }
        })
        .detach();
    }

    fn schedule_oxide_import_delivery(&self, cx: &mut Context<Self>) {
        let delivery_wake = self.oxide_import_delivery_tx.wake();
        let release_wake = delivery_wake.clone();
        cx.on_release(move |_, _| {
            // Workspace release stops only foreground delivery; an in-flight
            // import still owns and erases its password inside the worker.
            release_wake.stop();
        })
        .detach();
        cx.spawn(async move |entity, cx| {
            loop {
                delivery_wake.wait().await;
                let should_drain = delivery_wake.take();
                let stopped = delivery_wake.is_stopped();
                if should_drain {
                    let backlog_remaining = entity
                        .update(cx, |entity, cx| entity.drain_oxide_import_deliveries(cx))
                        .unwrap_or(false);
                    if backlog_remaining {
                        delivery_wake.mark();
                    }
                }
                if stopped {
                    break;
                }
            }
        })
        .detach();
    }

    fn drain_oxide_import_deliveries(&mut self, cx: &mut Context<Self>) -> bool {
        let drain = delivery::drain_channel(
            &self.oxide_import_delivery_rx,
            delivery::USER_ACTION_DELIVERY_BUDGET,
        );
        let mut added_intent = false;
        for delivery in drain.items {
            match delivery {
                NativePluginOxideImportWorkerMessage::Progress {
                    operation_id,
                    stage,
                    current,
                    total,
                } => {
                    let Some(context) = self.oxide_import_contexts.get(&operation_id) else {
                        continue;
                    };
                    let Some(registration_id) = context.progress_registration_id.as_ref() else {
                        continue;
                    };
                    self.oxide_import_intents
                        .push_back(PluginOxideImportIntent::Progress {
                            plugin_id: Arc::clone(&context.plugin_id),
                            registration_id: Arc::clone(registration_id),
                            value: oxide_plugin_host_api::sync::native_plugin_sync_progress_value(
                                "Importing .oxide",
                                &stage,
                                current,
                                total,
                                false,
                            ),
                        });
                    added_intent = true;
                }
                NativePluginOxideImportWorkerMessage::Done {
                    operation_id,
                    result,
                } => {
                    let Some(context) = self.oxide_import_contexts.remove(&operation_id) else {
                        continue;
                    };
                    self.oxide_import_intents
                        .push_back(PluginOxideImportIntent::Complete {
                            plugin_id: context.plugin_id,
                            progress_registration_id: context.progress_registration_id,
                            request_id: context.request_id,
                            result,
                            options: context.options,
                            response_tx: context.response_tx,
                        });
                    added_intent = true;
                }
            }
        }
        if added_intent {
            cx.emit(PluginWorkspaceEvent::OxideImportIntentsReady);
            cx.notify();
        }
        drain.outcome.backlog_remaining
    }

    fn drain_manager_deliveries(&mut self, cx: &mut Context<Self>) -> bool {
        let drain = delivery::drain_channel(
            &self.manager_delivery_rx,
            delivery::USER_ACTION_DELIVERY_BUDGET,
        );
        if !drain.items.is_empty() {
            if drain.items.iter().any(|delivery| {
                matches!(
                    delivery,
                    plugin_manager::NativePluginManagerDelivery::Install { .. }
                        | plugin_manager::NativePluginManagerDelivery::CheckUpdates(_)
                )
            }) {
                self.manager_operation_in_flight = false;
            }
            if drain.items.iter().any(|delivery| {
                matches!(
                    delivery,
                    plugin_manager::NativePluginManagerDelivery::Install { .. }
                )
            }) {
                self.remote_desktop_install_pending.clear();
            }
            self.manager_deliveries.extend(drain.items);
            cx.emit(PluginWorkspaceEvent::ManagerDeliveryReady);
            cx.notify();
        }
        drain.outcome.backlog_remaining
    }
}

impl gpui::EventEmitter<PluginWorkspaceEvent> for PluginWorkspaceEntity {}
