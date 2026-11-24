//! Settings input draft conversion for persisted settings.
//!
//! GPUI owns focus, caret, and IME state. This module owns the settings-domain
//! mapping between an input identity, its displayed value, and the validated
//! mutation applied to `PersistedSettings`.

use std::path::Path;

use oxide_settings::{
    MAX_TERMINAL_FONT_WEIGHT, MAX_TERMINAL_PADDING, MIN_TERMINAL_FONT_WEIGHT, PersistedSettings,
    RECOMMENDED_FOCUS_HANDOFF_COMMANDS, SettingsUpstreamProxyAuth, UpdateProxyMode,
    parse_terminal_session_log_content_template, parse_terminal_session_log_directory_template,
    parse_terminal_session_log_file_name_template, reindex_highlight_rules,
};
use oxide_terminal_semantic::SEMANTIC_CLASSES;

use crate::{SettingsInput, edit_custom_semantic_scheme, parse_focus_handoff_command_list};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SettingsInputDraftApply {
    Applied,
    Invalid,
    Unhandled,
}

pub fn persisted_settings_input_value(
    settings: &PersistedSettings,
    input: SettingsInput,
) -> Option<String> {
    let value = match input {
        SettingsInput::TerminalCustomFontFamily => settings.terminal.custom_font_family.clone(),
        SettingsInput::TerminalCjkFontFamily => settings.terminal.cjk_font_family.clone(),
        SettingsInput::TerminalFontSize => settings.terminal.font_size.to_string(),
        SettingsInput::TerminalFontWeight => settings.terminal.font_weight.to_string(),
        SettingsInput::TerminalScrollback => settings.terminal.scrollback.to_string(),
        SettingsInput::CommandHistoryRetention => settings
            .terminal
            .autosuggest
            .history_retention_days
            .to_string(),
        SettingsInput::TerminalLineHeight => compact_decimal(settings.terminal.line_height),
        SettingsInput::TerminalPaddingHorizontal => {
            settings.terminal.padding_horizontal.to_string()
        }
        SettingsInput::TerminalPaddingVertical => settings.terminal.padding_vertical.to_string(),
        SettingsInput::IdeCustomFontFamily => settings.ide.custom_font_family.clone(),
        SettingsInput::IdeCjkFontFamily => settings.ide.cjk_font_family.clone().unwrap_or_default(),
        SettingsInput::IdeFontWeight => settings
            .ide
            .font_weight
            .map(|value| value.to_string())
            .unwrap_or_default(),
        SettingsInput::IdeFontSize => settings
            .ide
            .font_size
            .map(|value| value.to_string())
            .unwrap_or_default(),
        SettingsInput::IdeLineHeight => settings
            .ide
            .line_height
            .map(compact_decimal)
            .unwrap_or_default(),
        SettingsInput::AppearanceUiFont => settings.appearance.ui_font_family.clone(),
        SettingsInput::BackgroundMaxWidth => settings
            .terminal
            .background_max_width
            .map(|n| n.to_string())
            .unwrap_or_default(),
        SettingsInput::BackgroundMaxHeight => settings
            .terminal
            .background_max_height
            .map(|n| n.to_string())
            .unwrap_or_default(),
        SettingsInput::BackgroundMaxFps => settings
            .terminal
            .background_max_fps
            .map(|n| n.to_string())
            .unwrap_or_default(),
        SettingsInput::BackgroundEffectColor(index) => settings
            .terminal
            .background_effect
            .as_ref()
            .and_then(|effect| effect.colors)
            .and_then(|colors| colors.get(index).copied())
            .map(|color| format!("#{color:06x}"))
            .unwrap_or_default(),
        SettingsInput::BackgroundEffectMaxFps => settings
            .terminal
            .background_effect
            .as_ref()
            .and_then(|effect| effect.max_fps)
            .map(|value| value.to_string())
            .unwrap_or_default(),
        SettingsInput::LocalDefaultCwd => settings
            .local_terminal
            .default_cwd
            .clone()
            .unwrap_or_default(),
        SettingsInput::LocalShellPath(_) => return None,
        SettingsInput::LocalOhMyPoshTheme => settings
            .local_terminal
            .oh_my_posh_theme
            .clone()
            .unwrap_or_default(),
        SettingsInput::ConnectionDefaultUsername => settings.connection_defaults.username.clone(),
        SettingsInput::ConnectionDefaultPort => settings.connection_defaults.port.to_string(),
        SettingsInput::ConnectionImportTargetGroup => return None,
        SettingsInput::NetworkProxyHost => settings
            .network
            .upstream_proxy
            .as_ref()
            .map(|proxy| proxy.host.clone())
            .unwrap_or_default(),
        SettingsInput::NetworkProxyPort => settings
            .network
            .upstream_proxy
            .as_ref()
            .map(|proxy| proxy.port.to_string())
            .unwrap_or_else(|| "1080".to_string()),
        SettingsInput::NetworkProxyNoProxy => settings
            .network
            .upstream_proxy
            .as_ref()
            .map(|proxy| proxy.no_proxy.clone())
            .unwrap_or_default(),
        SettingsInput::NetworkProxyUsername => settings
            .network
            .upstream_proxy
            .as_ref()
            .and_then(|proxy| match &proxy.auth {
                SettingsUpstreamProxyAuth::Password { username, .. } => Some(username.clone()),
                SettingsUpstreamProxyAuth::None => None,
            })
            .unwrap_or_default(),
        SettingsInput::NetworkProxyPassword => String::new(),
        SettingsInput::NetworkProxyTestHost | SettingsInput::NetworkProxyTestPort => return None,
        SettingsInput::UpdateProxyHost => settings.general.update_proxy.host.clone(),
        SettingsInput::UpdateProxyPort => settings.general.update_proxy.port.to_string(),
        SettingsInput::UpdateProxyNoProxy => settings.general.update_proxy.no_proxy.clone(),
        SettingsInput::SftpSpeedLimitKbps => settings.sftp.speed_limit_kbps.to_string(),
        SettingsInput::InBandTransferMaxChunkBytes => settings
            .terminal
            .in_band_transfer
            .max_chunk_bytes
            .to_string(),
        SettingsInput::InBandTransferMaxFileCount => settings
            .terminal
            .in_band_transfer
            .max_file_count
            .to_string(),
        SettingsInput::InBandTransferMaxTotalBytes => settings
            .terminal
            .in_band_transfer
            .max_total_bytes
            .to_string(),
        SettingsInput::TerminalSessionLogRetentionDays => {
            settings.terminal.session_log.retention_days.to_string()
        }
        SettingsInput::TerminalSessionLogMaxFileSizeMib => {
            settings.terminal.session_log.max_file_size_mib.to_string()
        }
        SettingsInput::TerminalSessionLogDirectory => settings
            .terminal
            .session_log
            .directory
            .clone()
            .unwrap_or_default(),
        SettingsInput::TerminalSessionLogDirectoryTemplate => {
            settings.terminal.session_log.directory_template.clone()
        }
        SettingsInput::TerminalSessionLogFileNameTemplate => {
            settings.terminal.session_log.file_name_template.clone()
        }
        SettingsInput::TerminalSessionLogContentTemplate => {
            settings.terminal.session_log.content_template.clone()
        }
        SettingsInput::TerminalCommandBarFocusHandoff => settings
            .terminal
            .command_bar
            .focus_handoff_commands
            .iter()
            .filter(|command| !RECOMMENDED_FOCUS_HANDOFF_COMMANDS.contains(&command.as_str()))
            .cloned()
            .collect::<Vec<_>>()
            .join(", "),
        SettingsInput::SemanticSchemeName => settings
            .terminal
            .active_custom_semantic_scheme()
            .map(|scheme| scheme.name.clone())
            .unwrap_or_default(),
        SettingsInput::SemanticSchemeRulePattern(index) => settings
            .terminal
            .active_custom_semantic_scheme()
            .and_then(|scheme| scheme.rules.get(index))
            .map(|rule| rule.pattern.clone())
            .unwrap_or_default(),
        SettingsInput::SemanticSchemeRuleCapture(index) => settings
            .terminal
            .active_custom_semantic_scheme()
            .and_then(|scheme| scheme.rules.get(index))
            .map(|rule| rule.capture.to_string())
            .unwrap_or_default(),
        SettingsInput::SemanticSchemeColor(index) => SEMANTIC_CLASSES
            .get(index)
            .and_then(|class| {
                settings
                    .terminal
                    .active_custom_semantic_scheme()?
                    .colors
                    .get(class)
                    .cloned()
            })
            .unwrap_or_default(),
        SettingsInput::HighlightRuleSetName => settings
            .terminal
            .default_highlight_rule_set
            .as_deref()
            .and_then(|id| settings.terminal.highlight_rule_set(id))
            .map(|rule_set| rule_set.name.clone())
            .unwrap_or_default(),
        SettingsInput::HighlightLabel(index) => settings
            .terminal
            .effective_highlight_rules()
            .get(index)
            .map(|rule| rule.label.clone())
            .unwrap_or_default(),
        SettingsInput::HighlightPattern(index) => settings
            .terminal
            .effective_highlight_rules()
            .get(index)
            .map(|rule| rule.pattern.clone())
            .unwrap_or_default(),
        SettingsInput::HighlightForeground(index) => settings
            .terminal
            .effective_highlight_rules()
            .get(index)
            .and_then(|rule| rule.foreground.clone())
            .unwrap_or_default(),
        SettingsInput::HighlightBackground(index) => settings
            .terminal
            .effective_highlight_rules()
            .get(index)
            .and_then(|rule| rule.background.clone())
            .unwrap_or_default(),
        _ => return None,
    };
    Some(value)
}

pub fn apply_persisted_settings_input_draft(
    settings: &mut PersistedSettings,
    input: SettingsInput,
    draft: &str,
) -> SettingsInputDraftApply {
    match input {
        SettingsInput::BackgroundEffectColor(index) => {
            if index >= 2 {
                return SettingsInputDraftApply::Invalid;
            }
            let color = if draft.trim().is_empty() {
                None
            } else {
                let Some(color) = crate::parse_rgb24_hex(draft) else {
                    return SettingsInputDraftApply::Invalid;
                };
                Some(color)
            };
            let effect = settings
                .terminal
                .background_effect
                .get_or_insert_with(Default::default);
            if let Some(color) = color {
                let colors = effect.colors.get_or_insert([0x6fa8dc, 0x75c9bb]);
                colors[index] = color;
            } else {
                effect.colors = None;
            }
            SettingsInputDraftApply::Applied
        }
        SettingsInput::TerminalCustomFontFamily => {
            settings.terminal.custom_font_family = draft.trim().to_string();
            SettingsInputDraftApply::Applied
        }
        SettingsInput::BackgroundMaxWidth
        | SettingsInput::BackgroundMaxHeight
        | SettingsInput::BackgroundMaxFps
        | SettingsInput::BackgroundEffectMaxFps => {
            let max = match input {
                SettingsInput::BackgroundMaxFps => 240,
                SettingsInput::BackgroundEffectMaxFps => 30,
                _ => 8192,
            };
            let value = if draft.trim().is_empty() {
                None
            } else {
                let Ok(value) = draft.trim().parse::<u32>() else {
                    return SettingsInputDraftApply::Invalid;
                };
                if !(1..=max).contains(&value) {
                    return SettingsInputDraftApply::Invalid;
                }
                Some(value)
            };
            match input {
                SettingsInput::BackgroundMaxWidth => settings.terminal.background_max_width = value,
                SettingsInput::BackgroundMaxHeight => {
                    settings.terminal.background_max_height = value
                }
                SettingsInput::BackgroundEffectMaxFps => {
                    settings
                        .terminal
                        .background_effect
                        .get_or_insert_with(Default::default)
                        .max_fps = value;
                }
                _ => settings.terminal.background_max_fps = value,
            }
            SettingsInputDraftApply::Applied
        }
        SettingsInput::TerminalCjkFontFamily => {
            settings.terminal.cjk_font_family = draft.trim().to_string();
            SettingsInputDraftApply::Applied
        }
        SettingsInput::TerminalFontSize => parse_i64(draft)
            .map(|value| settings.terminal.font_size = value.clamp(8, 32))
            .into(),
        SettingsInput::TerminalFontWeight => parse_i64(draft)
            .map(|value| {
                settings.terminal.font_weight =
                    value.clamp(MIN_TERMINAL_FONT_WEIGHT, MAX_TERMINAL_FONT_WEIGHT)
            })
            .into(),
        SettingsInput::TerminalScrollback => parse_i64(draft)
            .map(|value| settings.terminal.scrollback = value.clamp(500, 20_000))
            .into(),
        SettingsInput::CommandHistoryRetention => parse_i64(draft)
            .map(|value| {
                settings.terminal.autosuggest.history_retention_days =
                    value.clamp(0, u32::MAX as i64) as u32
            })
            .into(),
        SettingsInput::TerminalLineHeight => parse_f64(draft)
            .map(|value| settings.terminal.line_height = value.clamp(0.8, 2.0))
            .into(),
        SettingsInput::TerminalPaddingHorizontal => parse_i64(draft)
            .map(|value| {
                settings.terminal.padding_horizontal = value.clamp(0, MAX_TERMINAL_PADDING)
            })
            .into(),
        SettingsInput::TerminalPaddingVertical => parse_i64(draft)
            .map(|value| settings.terminal.padding_vertical = value.clamp(0, MAX_TERMINAL_PADDING))
            .into(),
        SettingsInput::IdeCustomFontFamily => {
            settings.ide.custom_font_family = draft.trim().to_string();
            SettingsInputDraftApply::Applied
        }
        SettingsInput::IdeCjkFontFamily => {
            settings.ide.cjk_font_family = Some(draft.trim().to_string());
            SettingsInputDraftApply::Applied
        }
        SettingsInput::IdeFontWeight => {
            if draft.trim().is_empty() {
                settings.ide.font_weight = None;
                SettingsInputDraftApply::Applied
            } else {
                parse_i64(draft)
                    .map(|value| settings.ide.font_weight = Some(value.clamp(100, 900)))
                    .into()
            }
        }
        SettingsInput::IdeFontSize => {
            let value = draft.trim();
            if value.is_empty() {
                settings.ide.font_size = None;
                SettingsInputDraftApply::Applied
            } else {
                parse_i64(value)
                    .map(|value| settings.ide.font_size = Some(value.clamp(8, 32)))
                    .into()
            }
        }
        SettingsInput::IdeLineHeight => {
            let value = draft.trim();
            if value.is_empty() {
                settings.ide.line_height = None;
                SettingsInputDraftApply::Applied
            } else {
                parse_f64(value)
                    .map(|value| settings.ide.line_height = Some(value.clamp(0.8, 3.0)))
                    .into()
            }
        }
        SettingsInput::AppearanceUiFont => {
            settings.appearance.ui_font_family = draft.trim().to_string();
            SettingsInputDraftApply::Applied
        }
        SettingsInput::LocalDefaultCwd => {
            settings.local_terminal.default_cwd = non_empty_trimmed(draft);
            SettingsInputDraftApply::Applied
        }
        SettingsInput::LocalShellPath(_) => SettingsInputDraftApply::Unhandled,
        SettingsInput::LocalOhMyPoshTheme => {
            settings.local_terminal.oh_my_posh_theme = non_empty_trimmed(draft);
            SettingsInputDraftApply::Applied
        }
        SettingsInput::ConnectionDefaultUsername => {
            settings.connection_defaults.username = draft.trim().to_string();
            SettingsInputDraftApply::Applied
        }
        SettingsInput::ConnectionDefaultPort => parse_i64(draft)
            .map(|value| settings.connection_defaults.port = value.clamp(1, 65_535))
            .into(),
        SettingsInput::ConnectionImportTargetGroup => SettingsInputDraftApply::Unhandled,
        SettingsInput::NetworkProxyHost => {
            edit_upstream_proxy(settings, |proxy| proxy.host = draft.trim().to_string())
        }
        SettingsInput::NetworkProxyPort => parse_i64(draft)
            .map(|value| {
                edit_upstream_proxy(settings, |proxy| proxy.port = value.clamp(1, 65_535) as u16);
            })
            .into(),
        SettingsInput::NetworkProxyNoProxy => {
            edit_upstream_proxy(settings, |proxy| proxy.no_proxy = draft.trim().to_string())
        }
        SettingsInput::NetworkProxyUsername => edit_upstream_proxy(settings, |proxy| {
            proxy.auth = SettingsUpstreamProxyAuth::Password {
                username: draft.trim().to_string(),
                keychain_id: match &proxy.auth {
                    SettingsUpstreamProxyAuth::Password { keychain_id, .. } => keychain_id.clone(),
                    SettingsUpstreamProxyAuth::None => None,
                },
            };
        }),
        SettingsInput::NetworkProxyPassword => SettingsInputDraftApply::Unhandled,
        SettingsInput::NetworkProxyTestHost | SettingsInput::NetworkProxyTestPort => {
            SettingsInputDraftApply::Unhandled
        }
        SettingsInput::UpdateProxyHost => {
            settings.general.update_proxy.host = draft.trim().to_string();
            SettingsInputDraftApply::Applied
        }
        SettingsInput::UpdateProxyPort => parse_i64(draft)
            .map(|value| {
                settings.general.update_proxy.port = value.clamp(1, 65_535) as u16;
                settings.general.update_proxy.mode = UpdateProxyMode::Custom;
            })
            .into(),
        SettingsInput::UpdateProxyNoProxy => {
            settings.general.update_proxy.no_proxy = draft.trim().to_string();
            SettingsInputDraftApply::Applied
        }
        SettingsInput::SftpSpeedLimitKbps => parse_i64(draft)
            .map(|value| settings.sftp.speed_limit_kbps = value.max(0))
            .into(),
        SettingsInput::InBandTransferMaxChunkBytes => parse_i64(draft)
            .map(|value| settings.terminal.in_band_transfer.max_chunk_bytes = value.max(1024))
            .into(),
        SettingsInput::InBandTransferMaxFileCount => parse_i64(draft)
            .map(|value| settings.terminal.in_band_transfer.max_file_count = value.max(1))
            .into(),
        SettingsInput::InBandTransferMaxTotalBytes => parse_i64(draft)
            .map(|value| settings.terminal.in_band_transfer.max_total_bytes = value.max(1024))
            .into(),
        SettingsInput::TerminalSessionLogRetentionDays => parse_i64(draft)
            .map(|value| settings.terminal.session_log.retention_days = value.clamp(0, 3650))
            .into(),
        SettingsInput::TerminalSessionLogMaxFileSizeMib => parse_i64(draft)
            .map(|value| settings.terminal.session_log.max_file_size_mib = value.clamp(0, 4096))
            .into(),
        SettingsInput::TerminalSessionLogDirectory => {
            let directory = draft.trim();
            if directory.is_empty() {
                settings.terminal.session_log.directory = None;
                SettingsInputDraftApply::Applied
            } else if Path::new(directory).is_absolute() {
                settings.terminal.session_log.directory = Some(directory.to_string());
                SettingsInputDraftApply::Applied
            } else {
                SettingsInputDraftApply::Invalid
            }
        }
        SettingsInput::TerminalSessionLogDirectoryTemplate => {
            if parse_terminal_session_log_directory_template(draft).is_err() {
                SettingsInputDraftApply::Invalid
            } else {
                settings.terminal.session_log.directory_template = draft.to_string();
                SettingsInputDraftApply::Applied
            }
        }
        SettingsInput::TerminalSessionLogFileNameTemplate => {
            if parse_terminal_session_log_file_name_template(draft).is_err() {
                SettingsInputDraftApply::Invalid
            } else {
                settings.terminal.session_log.file_name_template = draft.to_string();
                SettingsInputDraftApply::Applied
            }
        }
        SettingsInput::TerminalSessionLogContentTemplate => {
            if parse_terminal_session_log_content_template(draft).is_err() {
                SettingsInputDraftApply::Invalid
            } else {
                settings.terminal.session_log.content_template = draft.to_string();
                SettingsInputDraftApply::Applied
            }
        }
        SettingsInput::TerminalCommandBarFocusHandoff => {
            let mut commands = settings
                .terminal
                .command_bar
                .focus_handoff_commands
                .iter()
                .filter(|command| RECOMMENDED_FOCUS_HANDOFF_COMMANDS.contains(&command.as_str()))
                .cloned()
                .collect::<Vec<_>>();
            for command in parse_focus_handoff_command_list(draft) {
                if !RECOMMENDED_FOCUS_HANDOFF_COMMANDS.contains(&command.as_str()) {
                    commands.push(command);
                }
            }
            settings.terminal.command_bar.focus_handoff_commands = commands;
            SettingsInputDraftApply::Applied
        }
        SettingsInput::SemanticSchemeName => edit_custom_semantic_scheme(settings, |scheme| {
            scheme.name = draft.trim().to_string();
        })
        .map(|()| SettingsInputDraftApply::Applied)
        .unwrap_or(SettingsInputDraftApply::Invalid),
        SettingsInput::SemanticSchemeRulePattern(index) => {
            edit_custom_semantic_scheme(settings, |scheme| {
                if let Some(rule) = scheme.rules.get_mut(index) {
                    rule.pattern = draft.trim().to_string();
                }
            })
            .map(|()| SettingsInputDraftApply::Applied)
            .unwrap_or(SettingsInputDraftApply::Invalid)
        }
        SettingsInput::SemanticSchemeRuleCapture(index) => {
            let Ok(capture) = draft.trim().parse::<usize>() else {
                return SettingsInputDraftApply::Invalid;
            };
            edit_custom_semantic_scheme(settings, |scheme| {
                if let Some(rule) = scheme.rules.get_mut(index) {
                    rule.capture = capture;
                }
            })
            .map(|()| SettingsInputDraftApply::Applied)
            .unwrap_or(SettingsInputDraftApply::Invalid)
        }
        SettingsInput::SemanticSchemeColor(index) => {
            let Some(&class) = SEMANTIC_CLASSES.get(index) else {
                return SettingsInputDraftApply::Invalid;
            };
            edit_custom_semantic_scheme(settings, |scheme| {
                let color = draft.trim();
                if color.is_empty() {
                    scheme.colors.remove(&class);
                } else {
                    scheme.colors.insert(class, color.to_string());
                }
            })
            .map(|()| SettingsInputDraftApply::Applied)
            .unwrap_or(SettingsInputDraftApply::Invalid)
        }
        SettingsInput::HighlightRuleSetName => {
            let name = draft.trim();
            if name.is_empty() {
                return SettingsInputDraftApply::Invalid;
            }
            let Some(id) = settings.terminal.default_highlight_rule_set.clone() else {
                return SettingsInputDraftApply::Applied;
            };
            if let Some(rule_set) = settings
                .terminal
                .highlight_rule_sets
                .iter_mut()
                .find(|rule_set| rule_set.id == id)
            {
                rule_set.name = name.to_string();
            }
            SettingsInputDraftApply::Applied
        }
        SettingsInput::HighlightLabel(index) => edit_highlight_rule(settings, index, |rule| {
            rule.label = draft.trim().to_string()
        }),
        SettingsInput::HighlightPattern(index) => edit_highlight_rule(settings, index, |rule| {
            rule.pattern = draft.trim().to_string()
        }),
        SettingsInput::HighlightForeground(index) => edit_highlight_rule(settings, index, |rule| {
            rule.foreground = non_empty_trimmed(draft);
        }),
        SettingsInput::HighlightBackground(index) => edit_highlight_rule(settings, index, |rule| {
            rule.background = non_empty_trimmed(draft);
        }),
        _ => SettingsInputDraftApply::Unhandled,
    }
}

fn edit_highlight_rule(
    settings: &mut PersistedSettings,
    index: usize,
    edit: impl FnOnce(&mut oxide_settings::HighlightRule),
) -> SettingsInputDraftApply {
    let rules = settings.terminal.effective_highlight_rules_mut();
    let Some(rule) = rules.get_mut(index) else {
        return SettingsInputDraftApply::Applied;
    };
    edit(rule);
    *rules = reindex_highlight_rules(rules.clone());
    SettingsInputDraftApply::Applied
}

fn edit_upstream_proxy(
    settings: &mut PersistedSettings,
    edit: impl FnOnce(&mut oxide_settings::SettingsUpstreamProxyConfig),
) -> SettingsInputDraftApply {
    if let Some(proxy) = settings.network.upstream_proxy.as_mut() {
        edit(proxy);
    }
    SettingsInputDraftApply::Applied
}

fn non_empty_trimmed(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn parse_i64(value: &str) -> Option<i64> {
    value.parse::<i64>().ok()
}

fn parse_f64(value: &str) -> Option<f64> {
    value.parse::<f64>().ok()
}

fn compact_decimal(value: f64) -> String {
    let mut text = format!("{value:.2}");
    while text.contains('.') && text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.pop();
    }
    text
}

impl From<Option<()>> for SettingsInputDraftApply {
    fn from(value: Option<()>) -> Self {
        if value.is_some() {
            Self::Applied
        } else {
            Self::Invalid
        }
    }
}

/// Splits a settings multiline input into visual lines with UTF-16 ranges.
///
/// GPUI IME selections are tracked in UTF-16 code units to match browser input
/// semantics, so the model layer owns this conversion instead of each view.
/// Visual lines may contain private-key material, so every owned line buffer is
/// zeroized when the render frame releases it.
pub fn settings_multiline_line_ranges(
    value: &str,
) -> Vec<(std::ops::Range<usize>, zeroize::Zeroizing<String>)> {
    let mut ranges = Vec::new();
    let mut utf16_start = 0usize;
    let mut utf16_offset = 0usize;
    let mut byte_start = 0usize;

    for (byte_index, ch) in value.char_indices() {
        if ch == '\n' {
            ranges.push((
                utf16_start..utf16_offset,
                zeroize::Zeroizing::new(value[byte_start..byte_index].to_string()),
            ));
            utf16_offset += ch.len_utf16();
            utf16_start = utf16_offset;
            byte_start = byte_index + ch.len_utf8();
        } else {
            utf16_offset += ch.len_utf16();
        }
    }

    ranges.push((
        utf16_start..utf16_offset,
        zeroize::Zeroizing::new(value[byte_start..].to_string()),
    ));
    ranges
}

/// Maps a global UTF-16 selection into a single rendered settings text line.
///
/// The returned caret offset is separated from the selection range because an
/// empty browser selection renders as a caret, while a non-empty range renders
/// highlighted segments.
pub fn settings_multiline_line_selection(
    selection: Option<&std::ops::Range<usize>>,
    line_range: &std::ops::Range<usize>,
) -> (Option<std::ops::Range<usize>>, Option<usize>) {
    let Some(selection) = selection else {
        return (None, None);
    };

    if selection.start == selection.end {
        let caret = selection.start;
        if caret >= line_range.start && caret <= line_range.end {
            return (None, Some(caret.saturating_sub(line_range.start)));
        }
        return (None, None);
    }

    let start = selection.start.max(line_range.start);
    let end = selection.end.min(line_range.end);
    if start < end {
        (
            Some(start.saturating_sub(line_range.start)..end.saturating_sub(line_range.start)),
            None,
        )
    } else {
        (None, None)
    }
}
