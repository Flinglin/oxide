// Copyright (C) 2026 AnalyseDeCircuit
// SPDX-License-Identifier: GPL-3.0-only

use anyhow::{Context, Result};
use serde_json::{Map, Value, json};

use crate::{
    ParsedTerminalSessionLogTemplate, TerminalSessionLogTemplateError, model::*,
    parse_terminal_session_log_content_template, parse_terminal_session_log_directory_template,
    parse_terminal_session_log_file_name_template,
};

#[derive(Clone, Debug, PartialEq)]
pub struct SanitizedSettings {
    pub settings: PersistedSettings,
    pub migration_warnings: Vec<String>,
    pub validation_warnings: Vec<String>,
}

fn merge_json(defaults: &mut Value, incoming: &Value) {
    match (defaults, incoming) {
        (Value::Object(default_map), Value::Object(incoming_map)) => {
            for (key, value) in incoming_map {
                if let Some(target) = default_map.get_mut(key) {
                    merge_json(target, value);
                } else {
                    default_map.insert(key.clone(), value.clone());
                }
            }
        }
        (target, incoming_value) => *target = incoming_value.clone(),
    }
}

fn get_path_mut<'a>(value: &'a mut Value, path: &[&str]) -> Option<&'a mut Value> {
    let mut current = value;
    for segment in path {
        current = current.get_mut(*segment)?;
    }
    Some(current)
}

fn object_mut<'a>(value: &'a mut Value, key: &str) -> Option<&'a mut Map<String, Value>> {
    value.get_mut(key).and_then(Value::as_object_mut)
}

fn normalize_sftp_speed_limit_key(settings: &mut Value, raw: &Value) {
    let Some(sftp) = object_mut(settings, "sftp") else {
        return;
    };
    let Some(value) = sftp.remove("speedLimitKbps") else {
        return;
    };

    if raw
        .get("sftp")
        .and_then(|settings| settings.get("speedLimitKBps"))
        .is_some()
    {
        return;
    }

    // Keep the Tauri spelling canonical while still accepting older native
    // files that used serde's plain camelCase acronym handling.
    sftp.insert("speedLimitKBps".to_string(), value);
}

fn clamp_i64(
    value: &mut Value,
    fallback: i64,
    min: i64,
    max: i64,
    path: &str,
    warnings: &mut Vec<String>,
) {
    let Some(number) = value
        .as_i64()
        .or_else(|| value.as_f64().map(|v| v.round() as i64))
    else {
        *value = json!(fallback);
        warnings.push(format!("{} reset to default {}", path, fallback));
        return;
    };
    let clamped = number.clamp(min, max);
    if clamped != number {
        warnings.push(format!("{} clamped from {} to {}", path, number, clamped));
    }
    *value = json!(clamped);
}

fn clamp_f64(
    value: &mut Value,
    fallback: f64,
    min: f64,
    max: f64,
    path: &str,
    warnings: &mut Vec<String>,
) {
    let Some(number) = value.as_f64() else {
        *value = json!(fallback);
        warnings.push(format!("{} reset to default {}", path, fallback));
        return;
    };
    let clamped = number.clamp(min, max);
    if (clamped - number).abs() > f64::EPSILON {
        warnings.push(format!("{} clamped from {} to {}", path, number, clamped));
    }
    *value = json!(clamped);
}

fn sanitize_enum(
    root: &mut Value,
    path: &[&str],
    allowed: &[&str],
    fallback: &str,
    warnings: &mut Vec<String>,
) {
    let Some(value) = get_path_mut(root, path) else {
        return;
    };
    if value.as_str().is_some_and(|item| allowed.contains(&item)) {
        return;
    }
    *value = json!(fallback);
    warnings.push(format!("{} reset to {}", path.join("."), fallback));
}

fn clamp_backend_hot_lines(lines: i64) -> i64 {
    lines.clamp(BACKEND_HOT_BUFFER_MIN, BACKEND_HOT_BUFFER_MAX)
}

fn clamp_terminal_scrollback(lines: i64) -> i64 {
    lines.clamp(TERMINAL_SCROLLBACK_MIN, TERMINAL_SCROLLBACK_MAX)
}

fn sanitize_custom_semantic_schemes(root: &mut Value, warnings: &mut Vec<String>) {
    let Some(terminal) = root.get_mut("terminal").and_then(Value::as_object_mut) else {
        return;
    };
    let Some(schemes) = terminal
        .get_mut("customSemanticSchemes")
        .and_then(Value::as_array_mut)
    else {
        return;
    };

    let original_scheme_count = schemes.len();
    let mut valid_ids = std::collections::HashSet::new();
    let mut sanitized = Vec::new();
    for value in schemes.drain(..).take(MAX_CUSTOM_SEMANTIC_SCHEMES) {
        let Ok(document) = serde_json::from_value::<oxide_terminal_semantic::SemanticSchemeDocument>(
            value.clone(),
        ) else {
            warnings.push("Removed malformed custom semantic scheme".to_string());
            continue;
        };
        if oxide_terminal_semantic::validate_scheme_document(&document).is_err()
            || !valid_ids.insert(document.id.clone())
        {
            warnings.push(format!(
                "Removed invalid or duplicate custom semantic scheme: {}",
                document.id
            ));
            continue;
        }
        sanitized.push(value);
    }
    if original_scheme_count > MAX_CUSTOM_SEMANTIC_SCHEMES {
        warnings.push(format!(
            "Custom semantic schemes limited to {MAX_CUSTOM_SEMANTIC_SCHEMES}"
        ));
    }
    *schemes = sanitized;

    let active_is_valid = terminal
        .get("semanticCustomScheme")
        .and_then(Value::as_str)
        .is_some_and(|id| valid_ids.contains(id));
    if !active_is_valid {
        terminal.insert("semanticCustomScheme".to_string(), Value::Null);
    }

    if let Some(shell_schemes) = root
        .get_mut("localTerminal")
        .and_then(Value::as_object_mut)
        .and_then(|local| local.get_mut("semanticSchemeByShell"))
        .and_then(Value::as_object_mut)
    {
        shell_schemes.retain(|shell_id, scheme_id| {
            let valid_shell_id = !shell_id.trim().is_empty() && shell_id.len() <= 128;
            let valid_scheme_id = scheme_id.as_str().is_some_and(|scheme_id| {
                matches!(scheme_id, "balanced" | "conservative") || valid_ids.contains(scheme_id)
            });
            valid_shell_id && valid_scheme_id
        });
    }
}

fn sanitize_highlight_rule_sets(root: &mut Value, warnings: &mut Vec<String>) {
    let Some(terminal) = root.get_mut("terminal").and_then(Value::as_object_mut) else {
        return;
    };
    let Some(value) = terminal.get_mut("highlightRuleSets") else {
        return;
    };
    let original_count = value.as_array().map(Vec::len).unwrap_or_default();
    *value = sanitize_highlight_rule_sets_value(value);
    let valid_ids = value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|rule_set| rule_set.get("id").and_then(Value::as_str))
        .map(str::to_string)
        .collect::<std::collections::HashSet<_>>();
    if original_count > MAX_HIGHLIGHT_RULE_SETS {
        warnings.push(format!(
            "Highlight rule sets limited to {MAX_HIGHLIGHT_RULE_SETS}"
        ));
    }
    let default_is_valid = terminal
        .get("defaultHighlightRuleSet")
        .and_then(Value::as_str)
        .is_some_and(|id| valid_ids.contains(id));
    if !default_is_valid {
        terminal.insert("defaultHighlightRuleSet".to_string(), Value::Null);
    }
}

fn derive_backend_hot_lines(scrollback: i64) -> i64 {
    clamp_backend_hot_lines(clamp_terminal_scrollback(scrollback) * 2)
}

pub fn sanitize_settings_value(raw: Value) -> Result<SanitizedSettings> {
    let saved_version = raw.get("version").and_then(Value::as_u64).unwrap_or(0);
    if saved_version > u64::from(SETTINGS_SCHEMA_VERSION) {
        anyhow::bail!(
            "settings version {saved_version} is newer than supported version {SETTINGS_SCHEMA_VERSION}"
        );
    }
    let mut migration_warnings = Vec::new();
    let mut validation_warnings = Vec::new();
    let mut settings = PersistedSettings::default().to_value();

    merge_json(&mut settings, &raw);
    // Released settings used the terminal theme for the entire application.
    // Materialize that choice once so subsequent terminal changes stay independent.
    if raw.pointer("/appearance/theme").is_none() {
        let theme = settings.pointer("/terminal/theme").cloned();
        if let (Some(appearance), Some(theme)) = (object_mut(&mut settings, "appearance"), theme) {
            appearance.insert("theme".to_string(), theme);
        }
    }
    if let Some(object) = settings.as_object_mut() {
        object.insert("version".to_string(), json!(SETTINGS_SCHEMA_VERSION));
    }
    normalize_sftp_speed_limit_key(&mut settings, &raw);

    if saved_version < u64::from(SETTINGS_SCHEMA_VERSION)
        && let Some(old_scrollback) = raw
            .get("terminal")
            .and_then(|terminal| terminal.get("scrollback"))
            .and_then(Value::as_i64)
    {
        if let Some(value) = get_path_mut(&mut settings, &["terminal", "scrollback"]) {
            *value = json!(old_scrollback.min(DEFAULT_TERMINAL_SCROLLBACK));
        }
        if let Some(value) = get_path_mut(&mut settings, &["buffer", "maxLines"]) {
            *value = json!(derive_backend_hot_lines(old_scrollback));
        }
        migration_warnings.push(
            "Migrated legacy terminal.scrollback into terminal.scrollback + buffer.maxLines"
                .to_string(),
        );
    }

    for (path, fallback, min, max) in [
        (
            "terminal.scrollback",
            DEFAULT_TERMINAL_SCROLLBACK,
            TERMINAL_SCROLLBACK_MIN,
            TERMINAL_SCROLLBACK_MAX,
        ),
        (
            "buffer.maxLines",
            DEFAULT_BACKEND_HOT_BUFFER_LINES,
            BACKEND_HOT_BUFFER_MIN,
            BACKEND_HOT_BUFFER_MAX,
        ),
        ("terminal.fontSize", 14, 8, 32),
        (
            "terminal.fontWeight",
            DEFAULT_TERMINAL_FONT_WEIGHT,
            MIN_TERMINAL_FONT_WEIGHT,
            MAX_TERMINAL_FONT_WEIGHT,
        ),
        (
            "terminal.paddingHorizontal",
            DEFAULT_TERMINAL_PADDING_HORIZONTAL,
            0,
            MAX_TERMINAL_PADDING,
        ),
        (
            "terminal.paddingVertical",
            DEFAULT_TERMINAL_PADDING_VERTICAL,
            0,
            MAX_TERMINAL_PADDING,
        ),
        ("terminal.backgroundBlur", 0, 0, 20),
        ("appearance.borderRadius", 6, 0, 16),
        ("appearance.uiFontSize", DEFAULT_UI_FONT_SIZE, 11, 20),
        ("connectionDefaults.port", 22, 1, 65_535),
        ("sidebarUI.width", 300, 200, 600),
        ("sidebarUI.aiSidebarWidth", 340, 280, 500),
        ("sftp.maxConcurrentTransfers", 3, 1, 10),
        ("sftp.directoryParallelism", 4, 1, 16),
        ("sftp.speedLimitKBps", 0, 0, 10_000_000),
        ("reconnect.maxAttempts", 5, 1, 20),
        ("reconnect.baseDelayMs", 1000, 500, 10_000),
        ("reconnect.maxDelayMs", 15_000, 5_000, 60_000),
        ("connectionPool.idleTimeoutSecs", 1800, 0, 86_400),
        (
            "terminal.inBandTransfer.maxChunkBytes",
            1024 * 1024,
            64 * 1024,
            8 * 1024 * 1024,
        ),
        ("terminal.inBandTransfer.maxFileCount", 1024, 1, 10_000),
        (
            "terminal.inBandTransfer.maxTotalBytes",
            10 * 1024 * 1024 * 1024,
            100 * 1024 * 1024,
            100 * 1024 * 1024 * 1024,
        ),
        ("terminal.sessionLog.retentionDays", 30, 0, 3650),
        ("terminal.sessionLog.maxFileSizeMib", 100, 0, 4096),
    ] {
        let segments: Vec<_> = path.split('.').collect();
        if let Some(value) = get_path_mut(&mut settings, &segments) {
            clamp_i64(value, fallback, min, max, path, &mut validation_warnings);
        }
    }

    for (path, fallback, validator) in [
        (
            "terminal.sessionLog.fileNameTemplate",
            "{date}_{time}_{protocol}_{session}.log",
            parse_terminal_session_log_file_name_template
                as fn(
                    &str,
                ) -> std::result::Result<
                    ParsedTerminalSessionLogTemplate,
                    TerminalSessionLogTemplateError,
                >,
        ),
        (
            "terminal.sessionLog.contentTemplate",
            "[{timestamp}] {text}",
            parse_terminal_session_log_content_template
                as fn(
                    &str,
                ) -> std::result::Result<
                    ParsedTerminalSessionLogTemplate,
                    TerminalSessionLogTemplateError,
                >,
        ),
    ] {
        let segments: Vec<_> = path.split('.').collect();
        if let Some(value) = get_path_mut(&mut settings, &segments)
            && value
                .as_str()
                .is_none_or(|template| validator(template).is_err())
        {
            *value = json!(fallback);
            validation_warnings.push(format!("Reset invalid {path}"));
        }
    }

    if let Some(value) = get_path_mut(
        &mut settings,
        &["terminal", "sessionLog", "directoryTemplate"],
    ) && value
        .as_str()
        .is_none_or(|template| parse_terminal_session_log_directory_template(template).is_err())
    {
        *value = json!("");
        validation_warnings.push("Reset invalid terminal.sessionLog.directoryTemplate".to_string());
    }

    for (path, fallback, min, max) in [
        ("terminal.lineHeight", 1.2, 0.8, 3.0),
        (
            "terminal.backgroundOpacity",
            DEFAULT_TERMINAL_BACKGROUND_OPACITY,
            MIN_TERMINAL_BACKGROUND_OPACITY,
            MAX_TERMINAL_BACKGROUND_OPACITY,
        ),
        (
            "appearance.windowOpacity",
            DEFAULT_WINDOW_OPACITY,
            MIN_WINDOW_OPACITY,
            MAX_WINDOW_OPACITY,
        ),
    ] {
        let segments: Vec<_> = path.split('.').collect();
        if let Some(value) = get_path_mut(&mut settings, &segments) {
            clamp_f64(value, fallback, min, max, path, &mut validation_warnings);
        }
    }

    sanitize_enum(
        &mut settings,
        &["general", "language"],
        &[
            "zh-CN", "en", "fr-FR", "ja", "es-ES", "pt-BR", "vi", "ko", "de", "it", "zh-TW",
        ],
        "zh-CN",
        &mut validation_warnings,
    );
    // Retired channels fall back to the channel appropriate for this build so
    // shared settings from older installations remain loadable.
    sanitize_enum(
        &mut settings,
        &["general", "updateChannel"],
        &["stable", "beta"],
        match UpdateChannel::default() {
            UpdateChannel::Stable => "stable",
            UpdateChannel::Beta => "beta",
        },
        &mut validation_warnings,
    );
    sanitize_enum(
        &mut settings,
        &["terminal", "fontFamily"],
        &[
            "jetbrains",
            "meslo",
            "maple",
            "cascadia",
            "consolas",
            "menlo",
            "custom",
        ],
        "jetbrains",
        &mut validation_warnings,
    );
    sanitize_enum(
        &mut settings,
        &["terminal", "cursorStyle"],
        &["block", "underline", "bar"],
        "block",
        &mut validation_warnings,
    );
    sanitize_enum(
        &mut settings,
        &["terminal", "renderer"],
        &["auto", "webgl", "canvas"],
        if cfg!(windows) { "canvas" } else { "auto" },
        &mut validation_warnings,
    );
    sanitize_enum(
        &mut settings,
        &["terminal", "sessionLog", "fileMode"],
        &["unique", "append", "overwrite"],
        "unique",
        &mut validation_warnings,
    );
    sanitize_enum(
        &mut settings,
        &["terminal", "terminalEncoding"],
        &[
            "utf-8",
            "gbk",
            "gb18030",
            "big5",
            "shift_jis",
            "euc-jp",
            "euc-kr",
            "windows-1252",
        ],
        "utf-8",
        &mut validation_warnings,
    );
    sanitize_enum(
        &mut settings,
        &["terminal", "backspaceSequence"],
        &["delete", "controlH"],
        "delete",
        &mut validation_warnings,
    );
    sanitize_enum(
        &mut settings,
        &["terminal", "deleteSequence"],
        &["csi3Tilde", "delete", "controlH"],
        "csi3Tilde",
        &mut validation_warnings,
    );
    sanitize_enum(
        &mut settings,
        &["terminal", "adaptiveRenderer"],
        &["auto", "always-60", "off"],
        "auto",
        &mut validation_warnings,
    );
    sanitize_enum(
        &mut settings,
        &["terminal", "backgroundFit"],
        &["cover", "contain", "fill", "tile"],
        "cover",
        &mut validation_warnings,
    );
    sanitize_enum(
        &mut settings,
        &["appearance", "uiDensity"],
        &["compact", "comfortable", "spacious"],
        "comfortable",
        &mut validation_warnings,
    );
    sanitize_enum(
        &mut settings,
        &["appearance", "animationSpeed"],
        &["off", "reduced", "normal", "fast"],
        "normal",
        &mut validation_warnings,
    );
    sanitize_enum(
        &mut settings,
        &["appearance", "frostedGlass"],
        &["off", "native", "system", "mica", "acrylic"],
        "off",
        &mut validation_warnings,
    );
    sanitize_enum(
        &mut settings,
        &["appearance", "renderProfile"],
        &["auto", "quality", "low-power", "compatibility"],
        "auto",
        &mut validation_warnings,
    );
    sanitize_enum(
        &mut settings,
        &["sftp", "conflictAction"],
        &["ask", "overwrite", "skip", "rename"],
        "ask",
        &mut validation_warnings,
    );
    sanitize_enum(
        &mut settings,
        &["ide", "agentMode"],
        &["ask", "enabled", "disabled"],
        "ask",
        &mut validation_warnings,
    );

    if let Some(terminal) = object_mut(&mut settings, "terminal")
        && let Some(in_band) = terminal
            .get_mut("inBandTransfer")
            .and_then(Value::as_object_mut)
    {
        in_band.insert("provider".to_string(), json!("trzsz"));
    }

    if let Some(value) = get_path_mut(&mut settings, &["terminal", "highlightRules"]) {
        *value = sanitize_highlight_rules_value(value);
    }
    sanitize_highlight_rule_sets(&mut settings, &mut validation_warnings);
    sanitize_custom_semantic_schemes(&mut settings, &mut validation_warnings);

    let settings =
        serde_json::from_value(settings).context("sanitized settings did not match schema")?;
    Ok(SanitizedSettings {
        settings,
        migration_warnings,
        validation_warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_terminal_rule_references_are_removed_while_valid_bindings_survive() {
        let sanitized = sanitize_settings_value(json!({
            "terminal": {
                "semanticCustomScheme": "custom:invalid",
                "customSemanticSchemes": [{
                    "version": 1, "id": "custom:invalid", "name": "Invalid",
                    "rules": [{
                        "id": "bad-regex", "enabled": true, "pattern": "(", "capture": 0,
                        "class": "error", "priority": 80, "context": "any"
                    }]
                }],
                "defaultHighlightRuleSet": "missing",
                "highlightRuleSets": [{ "id": "operations", "name": "Operations", "rules": [] }]
            },
            "localTerminal": { "semanticSchemeByShell": { "bash": "conservative", "zsh": "custom:missing" } }
        })).unwrap();
        let terminal = &sanitized.settings.terminal;
        assert!(terminal.custom_semantic_schemes.is_empty());
        assert_eq!(terminal.semantic_custom_scheme, None);
        assert_eq!(terminal.default_highlight_rule_set, None);
        assert_eq!(
            terminal
                .highlight_rule_sets
                .iter()
                .map(|rules| (rules.id.as_str(), rules.name.as_str()))
                .collect::<Vec<_>>(),
            [("operations", "Operations")]
        );
        assert_eq!(
            sanitized
                .settings
                .local_terminal
                .semantic_scheme_for_shell("bash"),
            Some("conservative")
        );
        assert_eq!(
            sanitized
                .settings
                .local_terminal
                .semantic_scheme_for_shell("zsh"),
            None
        );
        assert!(
            sanitized
                .validation_warnings
                .iter()
                .any(|warning| warning.contains("custom:invalid"))
        );
    }

    #[test]
    fn invalid_session_log_templates_and_mode_fall_back_to_safe_defaults() {
        let sanitized = sanitize_settings_value(json!({
            "terminal": {
                "sessionLog": {
                    "fileNameTemplate": "../escape.log",
                    "contentTemplate": "missing text variable",
                    "fileMode": "replaceAnything"
                }
            }
        }))
        .expect("sanitize settings");

        let session_log = sanitized.settings.terminal.session_log;
        assert_eq!(
            session_log.file_name_template,
            "{date}_{time}_{protocol}_{session}.log"
        );
        assert_eq!(session_log.content_template, "[{timestamp}] {text}");
        assert_eq!(session_log.file_mode, TerminalSessionLogFileMode::Unique);
        assert_eq!(sanitized.validation_warnings.len(), 3);
    }

    #[test]
    fn saved_display_values_use_supported_defaults_and_bounds() {
        for (input, path, expected, warning) in [
            (
                json!({"general": {"updateChannel": "gpui-preview"}}),
                "/general/updateChannel",
                serde_json::to_value(UpdateChannel::default()).unwrap(),
                Some("general.updateChannel"),
            ),
            (
                json!({"terminal": {"backgroundOpacity": 1.0}}),
                "/terminal/backgroundOpacity",
                json!(MAX_TERMINAL_BACKGROUND_OPACITY),
                None,
            ),
            (
                json!({"terminal": {"backgroundOpacity": 1.5}}),
                "/terminal/backgroundOpacity",
                json!(MAX_TERMINAL_BACKGROUND_OPACITY),
                Some("terminal.backgroundOpacity"),
            ),
            (
                json!({"appearance": {}}),
                "/appearance/windowOpacity",
                json!(DEFAULT_WINDOW_OPACITY),
                None,
            ),
            (
                json!({"appearance": {"windowOpacity": 0.1}}),
                "/appearance/windowOpacity",
                json!(MIN_WINDOW_OPACITY),
                Some("appearance.windowOpacity"),
            ),
            (
                json!({"appearance": {}}),
                "/appearance/uiFontSize",
                json!(DEFAULT_UI_FONT_SIZE),
                None,
            ),
            (
                json!({"appearance": {"uiFontSize": 100}}),
                "/appearance/uiFontSize",
                json!(20),
                Some("appearance.uiFontSize"),
            ),
            (
                json!({"appearance": {"frostedGlass": "css"}}),
                "/appearance/frostedGlass",
                json!("off"),
                Some("appearance.frostedGlass"),
            ),
        ] {
            let sanitized = sanitize_settings_value(input).unwrap();
            assert_eq!(
                sanitized.settings.to_value().pointer(path),
                Some(&expected),
                "{path}"
            );
            match warning {
                Some(field) => assert!(
                    sanitized
                        .validation_warnings
                        .iter()
                        .any(|warning| warning.contains(field)),
                    "{path}: {:?}",
                    sanitized.validation_warnings
                ),
                None => assert!(
                    sanitized.validation_warnings.is_empty(),
                    "{path}: {:?}",
                    sanitized.validation_warnings
                ),
            }
        }
    }

    #[test]
    fn legacy_theme_initializes_application_theme_without_recoupling_saved_settings() {
        let migrated = sanitize_settings_value(json!({
            "terminal": { "theme": "monokai" }
        }))
        .expect("load shared theme settings");
        let mut saved = migrated.settings.to_value();
        assert_eq!(saved.pointer("/appearance/theme"), Some(&json!("monokai")));

        saved["terminal"]["theme"] = json!("github-dark");
        let reloaded = sanitize_settings_value(saved).expect("load independent themes");
        let reloaded = reloaded.settings.to_value();
        assert_eq!(
            reloaded.pointer("/appearance/theme"),
            Some(&json!("monokai"))
        );
        assert_eq!(
            reloaded.pointer("/terminal/theme"),
            Some(&json!("github-dark"))
        );
    }

    #[test]
    fn appearance_matrix_values_survive_sanitization() {
        for density in ["compact", "comfortable", "spacious"] {
            for frosted_glass in ["off", "native", "system", "mica", "acrylic"] {
                let sanitized = sanitize_settings_value(json!({
                    "appearance": {
                        "uiDensity": density,
                        "animationSpeed": "off",
                        "borderRadius": 16,
                        "frostedGlass": frosted_glass
                    },
                    "terminal": {
                        "backgroundBlur": 20,
                        "backgroundOpacity": 0.15
                    }
                }))
                .expect("sanitize appearance matrix settings");

                assert_eq!(
                    serde_json::to_value(sanitized.settings.appearance.ui_density)
                        .expect("serialize density"),
                    json!(density)
                );
                assert_eq!(sanitized.settings.appearance.border_radius, 16);
                assert_eq!(sanitized.settings.terminal.background_blur, 20);
            }
        }
    }

    #[test]
    fn accepts_legacy_native_sftp_speed_limit_key() {
        let sanitized = sanitize_settings_value(json!({
            "sftp": {
                "speedLimitEnabled": true,
                "speedLimitKbps": 2048
            }
        }))
        .expect("sanitize settings");

        assert!(sanitized.settings.sftp.speed_limit_enabled);
        assert_eq!(sanitized.settings.sftp.speed_limit_kbps, 2048);
        assert!(!sanitized.settings.sftp.extra.contains_key("speedLimitKbps"));
    }
}
