use std::{
    collections::HashSet,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use serde_json::{Map, Value, json};

use crate::{PersistedSettings, SETTINGS_SCHEMA_VERSION, sanitize_settings_value};

pub const OXIDE_SETTINGS_FORMAT: &str = "oxide-settings-sections-v1";
pub const OXIDE_SETTINGS_VERSION: u32 = 1;

pub const DEFAULT_OXIDE_SETTINGS_SECTIONS: &[&str] = &[
    "general",
    "terminalAppearance",
    "terminalBehavior",
    "appearance",
    "connections",
    "network",
    "fileAndEditor",
];
pub const ALL_OXIDE_SETTINGS_SECTIONS: &[&str] = &[
    "general",
    "terminalAppearance",
    "terminalBehavior",
    "appearance",
    "connections",
    "network",
    "fileAndEditor",
    "localTerminal",
    "nativePreferences",
];

const GENERAL_KEYS: &[&str] = &["language", "updateChannel"];
const TERMINAL_APPEARANCE_KEYS: &[&str] = &[
    "theme",
    "fontFamily",
    "customFontFamily",
    "fontSize",
    "lineHeight",
    "paddingHorizontal",
    "paddingVertical",
    "cursorStyle",
    "cursorBlink",
    "backgroundEnabled",
    "backgroundImage",
    "backgroundOpacity",
    "backgroundBlur",
    "backgroundFit",
    "backgroundScope",
    "backgroundEnabledTabs",
];
const TERMINAL_BEHAVIOR_KEYS: &[&str] = &[
    "scrollback",
    "renderer",
    "adaptiveRenderer",
    "showFpsOverlay",
    "highlightTabOnNewOutput",
    "pasteProtection",
    "smartCopy",
    "osc52Clipboard",
    "osc52ClipboardRead",
    "copyOnSelect",
    "middleClickPaste",
    "rightClickPaste",
    "openLinksWithModifier",
    "selectionRequiresShift",
    "autosuggest",
    "commandBar",
    "commandMarks",
    "highlightRules",
    "inBandTransfer",
    "xmodemBlockSize",
    "sessionLog",
    "terminalEncoding",
    "backspaceSequence",
    "deleteSequence",
    "graphics",
    "unicode",
];
const APPEARANCE_KEYS: &[&str] = &[
    "theme",
    "sidebarCollapsedDefault",
    "uiDensity",
    "borderRadius",
    "uiFontFamily",
    "showWindowTitlebar",
    "windowOpacity",
    "animationSpeed",
    "frostedGlass",
    "renderProfile",
];
const CONNECTION_DEFAULT_KEYS: &[&str] = &["username", "port"];
const RECONNECT_KEYS: &[&str] = &["enabled", "maxAttempts", "baseDelayMs", "maxDelayMs"];
const CONNECTION_POOL_KEYS: &[&str] = &["idleTimeoutSecs"];
const NETWORK_KEYS: &[&str] = &[
    "upstreamProxy",
    "upstreamProxyDisclaimerAccepted",
    "applicationProxyMode",
];
const SFTP_KEYS: &[&str] = &[
    "presentation",
    "sidebarViewMode",
    "maxConcurrentTransfers",
    "directoryParallelism",
    "speedLimitEnabled",
    "speedLimitKBps",
    "conflictAction",
];
const IDE_KEYS: &[&str] = &[
    "autoSave",
    "fontSize",
    "lineHeight",
    "agentMode",
    "wordWrap",
];
const LOCAL_TERMINAL_KEYS: &[&str] = &[
    // shellPaths is device-local: neither cloud sync nor .oxide imports may replace it.
    "defaultShellId",
    "recentShellIds",
    "defaultCwd",
    "gitBashPath",
    "loadShellProfile",
    "ohMyPoshEnabled",
    "ohMyPoshTheme",
];
const NATIVE_PREFERENCES_KEYS: &[&str] = &[
    "keybindings",
    "customThemes",
    "experimental",
    "newConnection",
    "settingsNavigation",
    "hostTools",
];

pub fn export_oxide_settings_snapshot_json(
    settings: &PersistedSettings,
    selected_sections: Option<&HashSet<String>>,
    include_local_terminal_env_vars: bool,
) -> Result<String> {
    let source = settings.to_value();
    let mut partial = json!({ "version": SETTINGS_SCHEMA_VERSION });
    let section_ids = effective_section_ids(selected_sections);

    for section in &section_ids {
        copy_section(
            &source,
            &mut partial,
            section,
            include_local_terminal_env_vars,
        );
    }

    // Protected-store handles belong to one device; cloud secrets restore a new local handle.
    if let Some(auth) = partial
        .pointer_mut("/network/upstreamProxy/auth")
        .and_then(Value::as_object_mut)
    {
        auth.remove("keychain_id");
    }
    let envelope = json!({
        "format": OXIDE_SETTINGS_FORMAT,
        "version": OXIDE_SETTINGS_VERSION,
        "exportedAt": now_ms(),
        "sectionIds": section_ids,
        "settings": partial,
    });
    serde_json::to_string_pretty(&envelope).context("failed to serialize app settings snapshot")
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// Materializes the released shared theme before snapshot fields are filtered by section.
pub fn migrate_legacy_theme_selection(settings: &mut Value) {
    if settings.pointer("/appearance/theme").is_none()
        && let Some(theme) = settings.pointer("/terminal/theme").cloned()
    {
        ensure_object_path(settings, &["appearance"]).insert("theme".to_string(), theme);
    }
}

pub fn merge_oxide_settings_snapshot(
    current: &PersistedSettings,
    snapshot_json: &str,
    selected_sections: Option<&HashSet<String>>,
) -> Result<PersistedSettings> {
    let parsed: Value =
        serde_json::from_str(snapshot_json).context("failed to parse app settings snapshot")?;
    let (mut snapshot_settings, snapshot_sections) =
        if parsed.get("format").and_then(Value::as_str) == Some(OXIDE_SETTINGS_FORMAT) {
            let settings = parsed
                .get("settings")
                .cloned()
                .context("app settings snapshot is missing settings")?;
            let sections = parsed
                .get("sectionIds")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect::<HashSet<_>>()
                })
                .unwrap_or_else(|| {
                    DEFAULT_OXIDE_SETTINGS_SECTIONS
                        .iter()
                        .map(|s| s.to_string())
                        .collect()
                });
            (settings, sections)
        } else {
            (
                parsed,
                DEFAULT_OXIDE_SETTINGS_SECTIONS
                    .iter()
                    .map(|section| section.to_string())
                    .collect(),
            )
        };

    // Materialize the released shared theme before current settings fill this key.
    // Copying sections below still limits it to imports that include appearance.
    migrate_legacy_theme_selection(&mut snapshot_settings);

    let requested = selected_sections
        .cloned()
        .unwrap_or_else(|| snapshot_sections.clone());
    let allowed = requested
        .intersection(&snapshot_sections)
        .cloned()
        .collect::<HashSet<_>>();
    let mut merged = current.to_value();
    for section in allowed {
        copy_section(&snapshot_settings, &mut merged, &section, false);
    }
    sanitize_settings_value(merged).map(|sanitized| sanitized.settings)
}

fn effective_section_ids(selected_sections: Option<&HashSet<String>>) -> Vec<String> {
    let mut sections = selected_sections
        .map(|selected| {
            ALL_OXIDE_SETTINGS_SECTIONS
                .iter()
                .filter(|section| selected.contains(**section))
                .map(|section| (*section).to_string())
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|| {
            DEFAULT_OXIDE_SETTINGS_SECTIONS
                .iter()
                .map(|section| (*section).to_string())
                .collect()
        });
    sections.retain(|section| !section.is_empty());
    sections
}

fn copy_section(
    source: &Value,
    target: &mut Value,
    section_id: &str,
    include_local_terminal_env_vars: bool,
) {
    match section_id {
        "general" => copy_object_keys(source, target, &["general"], GENERAL_KEYS),
        "terminalAppearance" => {
            copy_object_keys(source, target, &["terminal"], TERMINAL_APPEARANCE_KEYS)
        }
        "terminalBehavior" => {
            copy_object_keys(source, target, &["terminal"], TERMINAL_BEHAVIOR_KEYS)
        }
        "appearance" => copy_object_keys(source, target, &["appearance"], APPEARANCE_KEYS),
        "connections" => {
            copy_object_keys(
                source,
                target,
                &["connectionDefaults"],
                CONNECTION_DEFAULT_KEYS,
            );
            copy_object_keys(source, target, &["reconnect"], RECONNECT_KEYS);
            copy_object_keys(source, target, &["connectionPool"], CONNECTION_POOL_KEYS);
        }
        "network" => copy_object_keys(source, target, &["network"], NETWORK_KEYS),
        "fileAndEditor" => {
            copy_sftp_keys(source, target);
            copy_object_keys(source, target, &["ide"], IDE_KEYS);
        }
        "localTerminal" => {
            copy_object_keys(source, target, &["localTerminal"], LOCAL_TERMINAL_KEYS);
            if include_local_terminal_env_vars {
                copy_object_keys(source, target, &["localTerminal"], &["customEnvVars"]);
            }
        }
        "nativePreferences" => copy_root_keys(source, target, NATIVE_PREFERENCES_KEYS),
        _ => {}
    }
}

fn copy_sftp_keys(source: &Value, target: &mut Value) {
    copy_object_keys(source, target, &["sftp"], SFTP_KEYS);

    let Some(source_obj) = get_path(source, &["sftp"]).and_then(Value::as_object) else {
        return;
    };
    let Some(value) = source_obj
        .get("speedLimitKBps")
        .or_else(|| source_obj.get("speedLimitKbps"))
    else {
        return;
    };

    // Older native settings may still contain serde's plain camelCase acronym.
    // Sectioned snapshots stay on Tauri's KBps spelling.
    ensure_object_path(target, &["sftp"]).insert("speedLimitKBps".to_string(), value.clone());
}

fn copy_object_keys(source: &Value, target: &mut Value, path: &[&str], keys: &[&str]) {
    let Some(source_obj) = get_path(source, path).and_then(Value::as_object) else {
        return;
    };
    let target_obj = ensure_object_path(target, path);
    for key in keys {
        if let Some(value) = source_obj.get(*key) {
            target_obj.insert((*key).to_string(), value.clone());
        }
    }
}

fn copy_root_keys(source: &Value, target: &mut Value, keys: &[&str]) {
    let Some(source_obj) = source.as_object() else {
        return;
    };
    let target_obj = target
        .as_object_mut()
        .expect("settings root must be an object");
    for key in keys {
        if let Some(value) = source_obj.get(*key) {
            target_obj.insert((*key).to_string(), value.clone());
        }
    }
}

fn get_path<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut current = value;
    for segment in path {
        current = current.get(*segment)?;
    }
    Some(current)
}

fn ensure_object_path<'a>(value: &'a mut Value, path: &[&str]) -> &'a mut Map<String, Value> {
    let mut current = value;
    for segment in path {
        if !current.get(*segment).is_some_and(Value::is_object) {
            current
                .as_object_mut()
                .expect("settings root must be an object")
                .insert((*segment).to_string(), Value::Object(Map::new()));
        }
        current = current
            .as_object_mut()
            .expect("settings root must be an object")
            .get_mut(*segment)
            .expect("path segment was just inserted");
    }
    current
        .as_object_mut()
        .expect("path target must be an object")
}
