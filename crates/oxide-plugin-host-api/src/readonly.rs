//! Read-only plugin host API snapshots and returnable call routing.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use chrono::{SecondsFormat, Utc};
use oxide_i18n::I18n;
use oxide_plugin_protocol as plugin_runtime;
use oxide_plugin_registry::NativePluginRegistry;
use serde_json::{Value, json};

use crate::{
    app::{
        native_plugin_connection_summaries, native_plugin_custom_event_from_args,
        native_plugin_i18n_translate, native_plugin_platform_label, native_plugin_session_summary,
        native_plugin_settings_section, native_plugin_settings_summary,
        native_plugin_theme_snapshot,
    },
    catalog::host_api_catalog_json,
    settings::{
        native_normalize_syncable_settings_payload, native_syncable_settings_payload,
        native_syncable_settings_payload_arg, native_syncable_settings_revision,
    },
    terminal::{
        NativePluginTerminalNodeSnapshot, native_plugin_terminal_buffer_size_response,
        native_plugin_terminal_scroll_buffer_response, native_plugin_terminal_search_response,
    },
};

#[derive(Clone)]
pub struct NativePluginHostApiSnapshot {
    pub registry: Arc<NativePluginRegistry>,
    pub i18n: I18n,
    pub settings: Value,
    pub locale: String,
    pub theme_name: String,
    pub pool_stats: Value,
    pub layout: Value,
    pub connections: Vec<Value>,
    pub saved_connections: Vec<Value>,
    pub connection_states: HashMap<String, Value>,
    pub node_connection_ids: HashMap<String, String>,
    pub session_tree: Vec<Value>,
    pub session_node_states: HashMap<String, String>,
    pub event_log_entries: Vec<Value>,
    pub active_terminal_target: Value,
    pub terminal_nodes: HashMap<String, NativePluginTerminalNodeSnapshot>,
    /// Aggregate notification metadata; notification content never crosses this boundary.
    pub notification_summary: Value,
    /// Complete notification projections, guarded by notifications.read.
    pub notifications: Value,
    /// Quick-command discovery metadata without executable command text or host patterns.
    pub quick_command_metadata: Value,
    /// Complete quick-command definitions, guarded by quick_commands.read.
    pub quick_commands: Value,
    /// The complete, currently effective theme token set.
    pub theme_tokens: Value,
    /// Built-in and custom theme identifiers without custom theme source values.
    pub available_themes: Value,
    /// Allowlisted Cloud Sync status; destinations, credentials, errors, and payloads stay host-side.
    pub cloud_sync_summary: Value,
    /// Sanitized Cloud Sync history without errors, revisions, destinations, or payloads.
    pub cloud_sync_history: Value,
    /// Cached Host Tools snapshots keyed by their stable node identifiers.
    pub host_tools_snapshots: Value,
    /// Workspace metadata only; excludes terminal buffers, paths and diagnostic text.
    pub workspace_summary: Value,
}

fn native_plugin_ui_registration_preflight_response(
    snapshot: &NativePluginHostApiSnapshot,
    plugin_id: &str,
    call: plugin_runtime::PluginHostCall,
    kind: plugin_runtime::PluginRegistrationKind,
) -> plugin_runtime::PluginResponse {
    let request_id = call.request_id.clone();
    match native_plugin_ui_registration_from_args(plugin_id, kind, &call.args).and_then(
        |registration| {
            let mut registry = snapshot.registry.as_ref().clone();
            registry.apply_runtime_registration(registration)
        },
    ) {
        Ok(()) => plugin_runtime::PluginResponse::ok(
            request_id,
            json!({
                "queued": true,
            }),
        ),
        Err(error) => {
            let error_code = if kind == plugin_runtime::PluginRegistrationKind::ActivityBarItem {
                "invalid_ui_registration"
            } else {
                "invalid_declarative_ui"
            };
            plugin_runtime::PluginResponse::error(
                request_id,
                plugin_runtime::PluginError::protocol(error_code, error),
            )
        }
    }
}

fn native_plugin_ui_open_tab_preflight_response(
    snapshot: &NativePluginHostApiSnapshot,
    plugin_id: &str,
    call: plugin_runtime::PluginHostCall,
) -> plugin_runtime::PluginResponse {
    let request_id = call.request_id.clone();
    let Some(tab_id) = native_plugin_ui_tab_id_arg(&call.args) else {
        return plugin_runtime::PluginResponse::error(
            request_id,
            plugin_runtime::PluginError::protocol(
                "invalid_plugin_tab",
                "Native plugin ui.openTab requires args.tabId",
            ),
        );
    };
    if snapshot
        .registry
        .contributions()
        .tab_contribution(plugin_id, &tab_id)
        .is_none()
    {
        return plugin_runtime::PluginResponse::error(
            request_id,
            plugin_runtime::PluginError::protocol(
                "plugin_tab_not_declared",
                format!("Tab \"{tab_id}\" not declared in manifest contributes.tabs"),
            ),
        );
    }
    plugin_runtime::PluginResponse::ok(request_id, json!({ "queued": true }))
}

pub fn native_plugin_ui_registration_from_args(
    plugin_id: &str,
    kind: plugin_runtime::PluginRegistrationKind,
    args: &Value,
) -> Result<plugin_runtime::PluginRegistration, String> {
    let view_id = match kind {
        plugin_runtime::PluginRegistrationKind::Tab => native_plugin_ui_tab_id_arg(args)
            .ok_or_else(|| "Native plugin ui.registerTabView requires args.tabId".to_string())?,
        plugin_runtime::PluginRegistrationKind::SidebarPanel => native_plugin_ui_panel_id_arg(args)
            .ok_or_else(|| {
                "Native plugin ui.registerSidebarPanel requires args.panelId".to_string()
            })?,
        plugin_runtime::PluginRegistrationKind::ActivityBarItem => {
            native_plugin_ui_activity_item_id_arg(args).ok_or_else(|| {
                "Native plugin ui.registerActivityBarItem requires args.itemId".to_string()
            })?
        }
        _ => return Err("Unsupported native plugin declarative UI registration kind".to_string()),
    };
    let registration_id = args
        .get("registrationId")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| native_plugin_ui_registration_id(kind, &view_id));
    Ok(plugin_runtime::PluginRegistration {
        registration_id,
        plugin_id: plugin_id.to_string(),
        kind,
        metadata: args.clone(),
    })
}

pub fn native_plugin_ui_tab_id_arg(args: &Value) -> Option<String> {
    args.get("tabId")
        .or_else(|| args.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn native_plugin_ui_panel_id_arg(args: &Value) -> Option<String> {
    args.get("panelId")
        .or_else(|| args.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn native_plugin_ui_activity_item_id_arg(args: &Value) -> Option<String> {
    args.get("itemId")
        .or_else(|| args.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn native_plugin_ui_registration_id(
    kind: plugin_runtime::PluginRegistrationKind,
    view_id: &str,
) -> String {
    let namespace = match kind {
        plugin_runtime::PluginRegistrationKind::Tab => "tab",
        plugin_runtime::PluginRegistrationKind::SidebarPanel => "sidebar-panel",
        plugin_runtime::PluginRegistrationKind::ActivityBarItem => "activity-bar-item",
        _ => "view",
    };
    format!("ctx.ui.{namespace}:{view_id}")
}

pub fn native_plugin_returnable_host_api_response(
    snapshot: &NativePluginHostApiSnapshot,
    plugin_id: &str,
    call: plugin_runtime::PluginHostCall,
) -> Option<plugin_runtime::PluginResponse> {
    match (call.namespace.as_str(), call.method.as_str()) {
        ("app", "getWorkspaceSummary") => Some(plugin_runtime::PluginResponse::ok(
            call.request_id,
            snapshot.workspace_summary.clone(),
        )),
        ("ui", "openWorkspace") => Some(match crate::workspace::validate_workspace_destination(
            &call.args, &snapshot.workspace_summary, &snapshot.session_node_states,
        ) {
            Ok(_) => native_plugin_queued_response(call.request_id),
            Err(message) => plugin_runtime::PluginResponse::error(
                call.request_id,
                plugin_runtime::PluginError::protocol("invalid_workspace_destination", message),
            ),
        }),
        ("api", "invoke") => Some(plugin_runtime::PluginResponse::error(
            call.request_id,
            plugin_runtime::PluginError::runtime(
                "backend_adapter_unavailable",
                "api.invoke is resolved by the Workspace backend adapter",
            ),
        )),
        ("ui", "registerTabView") => Some(native_plugin_ui_registration_preflight_response(
            snapshot,
            plugin_id,
            call,
            plugin_runtime::PluginRegistrationKind::Tab,
        )),
        ("ui", "registerSidebarPanel") => Some(native_plugin_ui_registration_preflight_response(
            snapshot,
            plugin_id,
            call,
            plugin_runtime::PluginRegistrationKind::SidebarPanel,
        )),
        ("ui", "registerActivityBarItem") => {
            Some(native_plugin_ui_registration_preflight_response(
                snapshot,
                plugin_id,
                call,
                plugin_runtime::PluginRegistrationKind::ActivityBarItem,
            ))
        }
        ("ui", "openTab") => Some(native_plugin_ui_open_tab_preflight_response(
            snapshot, plugin_id, call,
        )),
        ("app", "getTheme") => Some(plugin_runtime::PluginResponse::ok(
            call.request_id,
            native_plugin_theme_snapshot(&snapshot.theme_name),
        )),
        ("app", "getSettings") => {
            let Some(category) = call.args.get("category").and_then(Value::as_str) else {
                return Some(plugin_runtime::PluginResponse::error(
                    call.request_id,
                    plugin_runtime::PluginError::protocol(
                        "invalid_settings_category",
                        "Native plugin app.getSettings requires args.category",
                    ),
                ));
            };
            Some(plugin_runtime::PluginResponse::ok(
                call.request_id,
                native_plugin_settings_section(&snapshot.settings, category),
            ))
        }
        ("app", "getSettingsSummary") => Some(plugin_runtime::PluginResponse::ok(
            call.request_id,
            native_plugin_settings_summary(
                &snapshot.settings,
                &snapshot.locale,
                &snapshot.theme_name,
            ),
        )),
        ("app", "getVersion") => Some(plugin_runtime::PluginResponse::ok(
            call.request_id,
            json!(env!("CARGO_PKG_VERSION")),
        )),
        ("app", "getPlatform") => Some(plugin_runtime::PluginResponse::ok(
            call.request_id,
            json!(native_plugin_platform_label()),
        )),
        ("app", "getLocale") => Some(plugin_runtime::PluginResponse::ok(
            call.request_id,
            json!(snapshot.locale),
        )),
        ("app", "getApiCatalog") => Some(plugin_runtime::PluginResponse::ok(
            call.request_id,
            host_api_catalog_json(),
        )),
        ("app", "getPoolStats") => Some(plugin_runtime::PluginResponse::ok(
            call.request_id,
            snapshot.pool_stats.clone(),
        )),
        ("connections", "getAll") => Some(plugin_runtime::PluginResponse::ok(
            call.request_id,
            json!(snapshot.connections),
        )),
        ("connections", "getSummaries") => Some(plugin_runtime::PluginResponse::ok(
            call.request_id,
            native_plugin_connection_summaries(&snapshot.connections, &snapshot.connection_states),
        )),
        ("connections", "getSavedSummaries") => Some(plugin_runtime::PluginResponse::ok(
            call.request_id,
            Value::Array(
                snapshot
                    .saved_connections
                    .iter()
                    .map(|connection| {
                        json!({
                            "id": connection.get("id").and_then(Value::as_str).unwrap_or_default(),
                            "name": connection.get("name").and_then(Value::as_str).unwrap_or_default(),
                            "group": connection.get("group").cloned().unwrap_or(Value::Null),
                            "authType": connection.get("authType").cloned().unwrap_or(Value::Null),
                            "lastUsedAt": connection.get("lastUsedAt").cloned().unwrap_or(Value::Null),
                        })
                    })
                    .collect(),
            ),
        )),
        ("connections", "getSaved") => Some(plugin_runtime::PluginResponse::ok(
            call.request_id,
            Value::Array(snapshot.saved_connections.clone()),
        )),
        ("connections", "get") => {
            let Some(connection_id) = call.args.get("connectionId").and_then(Value::as_str) else {
                return Some(plugin_runtime::PluginResponse::error(
                    call.request_id,
                    plugin_runtime::PluginError::protocol(
                        "invalid_connection_id",
                        "Native plugin connections.get requires args.connectionId",
                    ),
                ));
            };
            let connection = snapshot
                .connections
                .iter()
                .find(|connection| {
                    connection.get("id").and_then(Value::as_str) == Some(connection_id)
                })
                .cloned()
                .unwrap_or(Value::Null);
            Some(plugin_runtime::PluginResponse::ok(
                call.request_id,
                connection,
            ))
        }
        ("connections", "getState") => {
            let Some(connection_id) = call.args.get("connectionId").and_then(Value::as_str) else {
                return Some(plugin_runtime::PluginResponse::error(
                    call.request_id,
                    plugin_runtime::PluginError::protocol(
                        "invalid_connection_id",
                        "Native plugin connections.getState requires args.connectionId",
                    ),
                ));
            };
            let state = snapshot
                .connection_states
                .get(connection_id)
                .cloned()
                .unwrap_or(Value::Null);
            Some(plugin_runtime::PluginResponse::ok(call.request_id, state))
        }
        ("connections", "getByNode") => {
            let Some(node_id) = call.args.get("nodeId").and_then(Value::as_str) else {
                return Some(plugin_runtime::PluginResponse::error(
                    call.request_id,
                    plugin_runtime::PluginError::protocol(
                        "invalid_node_id",
                        "Native plugin connections.getByNode requires args.nodeId",
                    ),
                ));
            };
            let connection = snapshot
                .node_connection_ids
                .get(node_id)
                .and_then(|connection_id| {
                    snapshot.connections.iter().find(|connection| {
                        connection.get("id").and_then(Value::as_str) == Some(connection_id.as_str())
                    })
                })
                .cloned()
                .unwrap_or(Value::Null);
            Some(plugin_runtime::PluginResponse::ok(
                call.request_id,
                connection,
            ))
        }
        ("sessions", "getTree") => Some(plugin_runtime::PluginResponse::ok(
            call.request_id,
            json!(snapshot.session_tree),
        )),
        ("sessions", "getSummary") => Some(plugin_runtime::PluginResponse::ok(
            call.request_id,
            native_plugin_session_summary(&snapshot.session_tree),
        )),
        ("sessions", "getActiveNodes") => Some(plugin_runtime::PluginResponse::ok(
            call.request_id,
            native_plugin_active_session_nodes(&snapshot.session_tree),
        )),
        ("sessions", "getNodeState") => {
            let Some(node_id) = call.args.get("nodeId").and_then(Value::as_str) else {
                return Some(plugin_runtime::PluginResponse::error(
                    call.request_id,
                    plugin_runtime::PluginError::protocol(
                        "invalid_node_id",
                        "Native plugin sessions.getNodeState requires args.nodeId",
                    ),
                ));
            };
            let state = snapshot
                .session_node_states
                .get(node_id)
                .map(|state| json!(state))
                .unwrap_or(Value::Null);
            Some(plugin_runtime::PluginResponse::ok(call.request_id, state))
        }
        ("eventLog", "getEntries") => Some(plugin_runtime::PluginResponse::ok(
            call.request_id,
            native_plugin_filtered_event_log_entries(&snapshot.event_log_entries, &call.args),
        )),
        ("eventLog", "getSummary") => Some(plugin_runtime::PluginResponse::ok(
            call.request_id,
            native_plugin_event_log_summary(&snapshot.event_log_entries),
        )),
        ("notifications", "getSummary") => Some(plugin_runtime::PluginResponse::ok(
            call.request_id,
            snapshot.notification_summary.clone(),
        )),
        ("notifications", "getAll") => Some(plugin_runtime::PluginResponse::ok(
            call.request_id,
            snapshot.notifications.clone(),
        )),
        ("quickCommands", "getMetadata") => Some(plugin_runtime::PluginResponse::ok(
            call.request_id,
            snapshot.quick_command_metadata.clone(),
        )),
        ("quickCommands", "getAll") => Some(plugin_runtime::PluginResponse::ok(
            call.request_id,
            snapshot.quick_commands.clone(),
        )),
        ("theme", "getTokens") => Some(plugin_runtime::PluginResponse::ok(
            call.request_id,
            snapshot.theme_tokens.clone(),
        )),
        ("theme", "getAvailable") => Some(plugin_runtime::PluginResponse::ok(
            call.request_id,
            snapshot.available_themes.clone(),
        )),
        ("cloudSync", "getSummary") => Some(plugin_runtime::PluginResponse::ok(
            call.request_id,
            snapshot.cloud_sync_summary.clone(),
        )),
        ("cloudSync", "getHistory") => Some(plugin_runtime::PluginResponse::ok(
            call.request_id,
            snapshot.cloud_sync_history.clone(),
        )),
        ("hostTools", "getSnapshot") => {
            let Some(node_id) = call.args.get("nodeId").and_then(Value::as_str) else {
                return Some(plugin_runtime::PluginResponse::error(
                    call.request_id,
                    plugin_runtime::PluginError::protocol(
                        "invalid_host_tools_node",
                        "hostTools.getSnapshot requires args.nodeId",
                    ),
                ));
            };
            let snapshot = snapshot
                .host_tools_snapshots
                .as_array()
                .into_iter()
                .flatten()
                .find(|entry| entry.get("nodeId").and_then(Value::as_str) == Some(node_id))
                .cloned()
                .unwrap_or(Value::Null);
            Some(plugin_runtime::PluginResponse::ok(
                call.request_id,
                snapshot,
            ))
        }
        ("terminal", "getActiveTarget") => Some(plugin_runtime::PluginResponse::ok(
            call.request_id,
            snapshot.active_terminal_target.clone(),
        )),
        ("terminal", "getMetadata") => Some(plugin_runtime::PluginResponse::ok(
            call.request_id,
            native_plugin_terminal_metadata(snapshot),
        )),
        ("terminal", "getNodeBuffer") => {
            let Some(node_id) = call.args.get("nodeId").and_then(Value::as_str) else {
                return Some(plugin_runtime::PluginResponse::error(
                    call.request_id,
                    plugin_runtime::PluginError::protocol(
                        "invalid_node_id",
                        "Native plugin terminal.getNodeBuffer requires args.nodeId",
                    ),
                ));
            };
            let value = snapshot
                .terminal_nodes
                .get(node_id)
                .map(|terminal| json!(terminal.buffer))
                .unwrap_or(Value::Null);
            Some(plugin_runtime::PluginResponse::ok(call.request_id, value))
        }
        ("terminal", "getNodeSelection") => {
            let Some(node_id) = call.args.get("nodeId").and_then(Value::as_str) else {
                return Some(plugin_runtime::PluginResponse::error(
                    call.request_id,
                    plugin_runtime::PluginError::protocol(
                        "invalid_node_id",
                        "Native plugin terminal.getNodeSelection requires args.nodeId",
                    ),
                ));
            };
            let value = snapshot
                .terminal_nodes
                .get(node_id)
                .and_then(|terminal| terminal.selection.clone())
                .map(Value::String)
                .unwrap_or(Value::Null);
            Some(plugin_runtime::PluginResponse::ok(call.request_id, value))
        }
        ("terminal", "search") => Some(native_plugin_terminal_search_response(
            call.request_id,
            &snapshot.terminal_nodes,
            call.args,
        )),
        ("terminal", "getScrollBuffer") => Some(native_plugin_terminal_scroll_buffer_response(
            call.request_id,
            &snapshot.terminal_nodes,
            call.args,
        )),
        ("terminal", "getBufferSize") => Some(native_plugin_terminal_buffer_size_response(
            call.request_id,
            &snapshot.terminal_nodes,
            call.args,
        )),
        ("ui", "getLayout") => Some(plugin_runtime::PluginResponse::ok(
            call.request_id,
            snapshot.layout.clone(),
        )),
        ("events", "emit") => Some(
            match native_plugin_custom_event_from_args(plugin_id, call.args) {
                Ok((event_key, _payload)) => plugin_runtime::PluginResponse::ok(
                    call.request_id,
                    json!({
                        "emitted": true,
                        "event": event_key,
                    }),
                ),
                Err(error) => plugin_runtime::PluginResponse::error(
                    call.request_id,
                    plugin_runtime::PluginError::protocol("invalid_plugin_event", error),
                ),
            },
        ),
        ("i18n", "getLanguage") => Some(plugin_runtime::PluginResponse::ok(
            call.request_id,
            json!(snapshot.locale),
        )),
        ("i18n", "t") => {
            let Some(key) = call.args.get("key").and_then(Value::as_str) else {
                return Some(plugin_runtime::PluginResponse::error(
                    call.request_id,
                    plugin_runtime::PluginError::protocol(
                        "invalid_i18n_key",
                        "Native plugin i18n.t requires args.key",
                    ),
                ));
            };
            Some(plugin_runtime::PluginResponse::ok(
                call.request_id,
                json!(native_plugin_i18n_translate(&snapshot.i18n, plugin_id, key)),
            ))
        }
        ("settings", "get") => {
            let Some(key) = call.args.get("key").and_then(Value::as_str) else {
                return Some(plugin_runtime::PluginResponse::error(
                    call.request_id,
                    plugin_runtime::PluginError::protocol(
                        "invalid_plugin_setting_key",
                        "Native plugin settings.get requires args.key",
                    ),
                ));
            };
            // Native plugin settings are declaration-backed. This intentionally
            // uses the same registry path as manifest-rendered settings controls
            // so runtime plugins cannot create a parallel config namespace.
            let value = snapshot
                .registry
                .plugin_setting_value(plugin_id, key)
                .unwrap_or(serde_json::Value::Null);
            Some(plugin_runtime::PluginResponse::ok(call.request_id, value))
        }
        ("settings", "exportSyncableSettings") => {
            let normalized = native_normalize_syncable_settings_payload(
                &native_syncable_settings_payload(&snapshot.settings),
            );
            Some(plugin_runtime::PluginResponse::ok(
                call.request_id,
                json!({
                    "revision": native_syncable_settings_revision(&normalized.payload),
                    "exportedAt": Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
                    "payload": normalized.payload,
                    "warnings": normalized.warnings,
                }),
            ))
        }
        ("settings", "applySyncableSettings") => {
            let normalized = native_normalize_syncable_settings_payload(
                &native_syncable_settings_payload_arg(call.args),
            );
            Some(plugin_runtime::PluginResponse::ok(
                call.request_id,
                json!({
                    "revision": native_syncable_settings_revision(&normalized.payload),
                    "appliedPayload": normalized.payload,
                    "warnings": normalized.warnings,
                }),
            ))
        }
        ("storage", "get") => {
            let Some(key) = call.args.get("key").and_then(serde_json::Value::as_str) else {
                return Some(plugin_runtime::PluginResponse::error(
                    call.request_id,
                    plugin_runtime::PluginError::protocol(
                        "invalid_storage_key",
                        "Native plugin storage.get requires args.key",
                    ),
                ));
            };
            // Tauri localStorage-backed plugin storage returns null for missing
            // or unreadable JSON. Native mirrors that through a scoped registry
            // lookup and returns the raw JSON value to the process runtime.
            let value = snapshot
                .registry
                .plugin_storage_value(plugin_id, key)
                .unwrap_or(serde_json::Value::Null);
            Some(plugin_runtime::PluginResponse::ok(call.request_id, value))
        }
        ("connections", "openForm") => Some(match crate::workspace::discovered_ssh_host(&call.args) {
            Ok(_) => native_plugin_queued_response(call.request_id),
            Err(message) => plugin_runtime::PluginResponse::error(
                call.request_id,
                plugin_runtime::PluginError::protocol("invalid_discovered_host", message),
            ),
        }),
        ("connections", "connect") => {
            let Some(connection_id) = call.args.get("connectionId").and_then(Value::as_str) else {
                return Some(plugin_runtime::PluginResponse::error(
                    call.request_id,
                    plugin_runtime::PluginError::protocol(
                        "invalid_connection_id",
                        "connections.connect requires non-empty args.connectionId",
                    ),
                ));
            };
            if snapshot.saved_connections.iter().any(|connection| {
                connection.get("id").and_then(Value::as_str) == Some(connection_id)
            }) {
                Some(native_plugin_queued_response(call.request_id))
            } else {
                Some(plugin_runtime::PluginResponse::error(
                    call.request_id,
                    plugin_runtime::PluginError::protocol(
                        "saved_connection_not_found",
                        "connections.connect requires an existing saved connection ID",
                    ),
                ))
            }
        }
        ("connections", "reconnect" | "disconnect") => Some(
            native_plugin_queued_string_arg_response(call, "nodeId", "invalid_node_id"),
        ),
        ("notifications", "markRead" | "remove") => Some(native_plugin_queued_u64_arg_response(
            call,
            "id",
            "invalid_notification_id",
        )),
        ("notifications", "setDnd") => Some(native_plugin_queued_bool_arg_response(
            call,
            "enabled",
            "invalid_notification_dnd",
        )),
        ("notifications", "markAllRead" | "clear")
        | ("cloudSync", "check" | "upload" | "pullPreview" | "applyPreview") => {
            Some(native_plugin_queued_response(call.request_id))
        }
        ("cloudSync", "setAutoUpload") => Some(native_plugin_queued_bool_arg_response(
            call,
            "enabled",
            "invalid_cloud_sync_auto_upload",
        )),
        ("quickCommands", "execute" | "remove") => Some(native_plugin_queued_string_arg_response(
            call,
            "id",
            "invalid_quick_command_id",
        )),
        ("quickCommands", "upsert") => {
            let has_name = call.args.get("name").and_then(Value::as_str).is_some();
            let has_command = call.args.get("command").and_then(Value::as_str).is_some();
            if has_name && has_command && quick_command_upsert_args_are_valid(&call.args) {
                Some(native_plugin_queued_response(call.request_id))
            } else {
                Some(plugin_runtime::PluginResponse::error(
                    call.request_id,
                    plugin_runtime::PluginError::protocol(
                        "invalid_quick_command",
                        "quickCommands.upsert requires a valid Quick Command definition",
                    ),
                ))
            }
        }
        ("theme", "setActive") => Some(native_plugin_queued_string_arg_response(
            call,
            "themeId",
            "invalid_theme_id",
        )),
        _ => None,
    }
}

fn quick_command_upsert_args_are_valid(args: &Value) -> bool {
    let optional_string = |key| args.get(key).is_none_or(Value::is_string);
    let string_array = |key| {
        args.get(key).is_none_or(|value| {
            value
                .as_array()
                .is_some_and(|values| values.iter().all(Value::is_string))
        })
    };
    let confirmation_is_valid = args
        .get("confirmation")
        .is_none_or(|value| matches!(value.as_str(), Some("inherit" | "always")));
    let protocols_are_valid = args.get("protocols").is_none_or(|value| {
        value.as_array().is_some_and(|protocols| {
            protocols.iter().all(|protocol| {
                matches!(
                    protocol.as_str(),
                    Some("local" | "ssh" | "mosh" | "telnet" | "serial" | "tmux")
                )
            })
        })
    });
    optional_string("id")
        && optional_string("category")
        && optional_string("description")
        && optional_string("hostPattern")
        && confirmation_is_valid
        && string_array("hostPatterns")
        && protocols_are_valid
        && args
            .get("parameters")
            .is_none_or(quick_command_parameters_are_valid)
        && !(args.get("hostPattern").is_some() && args.get("hostPatterns").is_some())
}

fn quick_command_parameters_are_valid(value: &Value) -> bool {
    let Some(parameters) = value.as_array() else {
        return false;
    };
    let mut names = HashSet::new();
    parameters.iter().all(|parameter| {
        let Some(name) = parameter.get("name").and_then(Value::as_str) else {
            return false;
        };
        let Some(label) = parameter.get("label").and_then(Value::as_str) else {
            return false;
        };
        let mut characters = name.chars();
        let valid_name = characters
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
            && characters.all(|character| character.is_ascii_alphanumeric() || character == '_');
        let kind = parameter
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("text");
        let choices = match parameter.get("choices") {
            None => Vec::new(),
            Some(value) => {
                let Some(values) = value.as_array() else {
                    return false;
                };
                let choices = values.iter().filter_map(Value::as_str).collect::<Vec<_>>();
                if choices.len() != values.len() {
                    return false;
                }
                choices
            }
        };
        let default_value = match parameter.get("defaultValue") {
            None | Some(Value::Null) => None,
            Some(Value::String(value)) => Some(value.as_str()),
            Some(_) => return false,
        };
        valid_name
            && names.insert(name)
            && !label.trim().is_empty()
            && matches!(kind, "text" | "choice")
            && parameter.get("required").is_none_or(Value::is_boolean)
            && (kind != "choice" || !choices.is_empty())
            && (kind != "choice"
                || default_value.is_none_or(|default_value| choices.contains(&default_value)))
    })
}

fn native_plugin_queued_response(request_id: String) -> plugin_runtime::PluginResponse {
    plugin_runtime::PluginResponse::ok(request_id, json!({ "queued": true }))
}

fn native_plugin_queued_string_arg_response(
    call: plugin_runtime::PluginHostCall,
    key: &str,
    error_code: &str,
) -> plugin_runtime::PluginResponse {
    if call
        .args
        .get(key)
        .and_then(Value::as_str)
        .is_some_and(|value| !value.trim().is_empty())
    {
        return native_plugin_queued_response(call.request_id);
    }
    plugin_runtime::PluginResponse::error(
        call.request_id,
        plugin_runtime::PluginError::protocol(
            error_code,
            format!(
                "{}.{} requires non-empty args.{key}",
                call.namespace, call.method
            ),
        ),
    )
}

fn native_plugin_queued_u64_arg_response(
    call: plugin_runtime::PluginHostCall,
    key: &str,
    error_code: &str,
) -> plugin_runtime::PluginResponse {
    if call.args.get(key).and_then(Value::as_u64).is_some() {
        return native_plugin_queued_response(call.request_id);
    }
    plugin_runtime::PluginResponse::error(
        call.request_id,
        plugin_runtime::PluginError::protocol(
            error_code,
            format!(
                "{}.{} requires integer args.{key}",
                call.namespace, call.method
            ),
        ),
    )
}

fn native_plugin_queued_bool_arg_response(
    call: plugin_runtime::PluginHostCall,
    key: &str,
    error_code: &str,
) -> plugin_runtime::PluginResponse {
    if call.args.get(key).and_then(Value::as_bool).is_some() {
        return native_plugin_queued_response(call.request_id);
    }
    plugin_runtime::PluginResponse::error(
        call.request_id,
        plugin_runtime::PluginError::protocol(
            error_code,
            format!(
                "{}.{} requires boolean args.{key}",
                call.namespace, call.method
            ),
        ),
    )
}

/// Projects terminal availability and sizing without terminal text or selection content.
pub fn native_plugin_terminal_metadata(snapshot: &NativePluginHostApiSnapshot) -> Value {
    let active_target = if snapshot.active_terminal_target.is_null() {
        Value::Null
    } else {
        json!({
            "sessionId": snapshot.active_terminal_target.get("sessionId").and_then(Value::as_str),
            "terminalType": snapshot.active_terminal_target.get("terminalType").and_then(Value::as_str),
            "nodeId": snapshot.active_terminal_target.get("nodeId").and_then(Value::as_str),
            "connectionId": snapshot.active_terminal_target.get("connectionId").and_then(Value::as_str),
            "connectionState": snapshot
                .active_terminal_target
                .get("connectionState")
                .and_then(Value::as_str)
                .unwrap_or("unknown"),
        })
    };
    let mut nodes = snapshot
        .terminal_nodes
        .iter()
        .map(|(node_id, terminal)| {
            json!({
                "nodeId": node_id,
                "currentLines": terminal.current_lines,
                "hasSelection": terminal.selection.is_some(),
            })
        })
        .collect::<Vec<_>>();
    nodes.sort_by(|left, right| {
        left.get("nodeId")
            .and_then(Value::as_str)
            .cmp(&right.get("nodeId").and_then(Value::as_str))
    });
    json!({
        "activeTarget": active_target,
        "terminalCount": nodes.len(),
        "nodes": nodes,
    })
}

pub fn native_plugin_active_session_nodes(session_tree: &[Value]) -> Value {
    let active_nodes = session_tree
        .iter()
        .filter(|node| {
            matches!(
                node.get("connectionState").and_then(Value::as_str),
                Some("active" | "connected")
            )
        })
        .map(|node| {
            json!({
                "nodeId": node.get("id").and_then(Value::as_str).unwrap_or_default(),
                "sessionId": node
                    .get("terminalIds")
                    .and_then(Value::as_array)
                    .and_then(|terminal_ids| terminal_ids.first())
                    .cloned()
                    .unwrap_or(Value::Null),
                "connectionState": node
                    .get("connectionState")
                    .and_then(Value::as_str)
                    .unwrap_or("idle"),
            })
        })
        .collect::<Vec<_>>();
    json!(active_nodes)
}

pub fn native_plugin_session_state_map(tree: &Value) -> HashMap<String, String> {
    tree.as_array()
        .map(|nodes| native_plugin_session_state_map_from_nodes(nodes))
        .unwrap_or_default()
}

pub fn native_plugin_session_state_map_from_nodes(nodes: &[Value]) -> HashMap<String, String> {
    nodes
        .iter()
        .filter_map(|node| {
            let node_id = node.get("id").and_then(Value::as_str)?;
            let state = node.get("connectionState").and_then(Value::as_str)?;
            Some((node_id.to_string(), state.to_string()))
        })
        .collect()
}

pub fn native_plugin_filtered_event_log_entries(entries: &[Value], args: &Value) -> Value {
    let filter = args.get("filter").unwrap_or(args);
    let severity = filter.get("severity").and_then(Value::as_str);
    let category = filter.get("category").and_then(Value::as_str);
    let filtered = entries
        .iter()
        .filter(|entry| {
            severity.is_none_or(|severity| {
                entry.get("severity").and_then(Value::as_str) == Some(severity)
            }) && category.is_none_or(|category| {
                entry.get("category").and_then(Value::as_str) == Some(category)
            })
        })
        .cloned()
        .collect::<Vec<_>>();
    json!(filtered)
}

/// Counts event metadata while excluding titles, details, sources, and timestamps.
pub fn native_plugin_event_log_summary(entries: &[Value]) -> Value {
    let mut by_severity = std::collections::BTreeMap::<String, usize>::new();
    let mut by_category = std::collections::BTreeMap::<String, usize>::new();
    for entry in entries {
        let severity = entry
            .get("severity")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let category = entry
            .get("category")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        *by_severity.entry(severity.to_string()).or_default() += 1;
        *by_category.entry(category.to_string()).or_default() += 1;
    }
    json!({
        "total": entries.len(),
        "bySeverity": by_severity,
        "byCategory": by_category,
    })
}
