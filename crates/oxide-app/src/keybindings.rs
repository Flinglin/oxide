use gpui::{KeyBinding, Keystroke, NoAction};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{borrow::Cow, sync::LazyLock};

use crate::{
    CloseOtherTabs, CloseTab, CommandPalette, Copy, Cut, Find, FontDecrease, FontIncrease,
    FontReset, GoToTab1, GoToTab2, GoToTab3, GoToTab4, GoToTab5, GoToTab6, GoToTab7, GoToTab8,
    GoToTab9, NewConnection, NewTerminal, NextTab, OpenSettings, PaletteBroadcast, PaletteEventLog,
    PaletteRightSidebar, Paste, PrevTab, Quit, ShellLauncher, ShowShortcuts, SplitHorizontal,
    SplitNavLeft, SplitNavRight, SplitVertical, TerminalClearScreen, TerminalFreeTypeMode,
    TerminalRecording, ToggleFullscreen, ToggleLeftSidebar, ZenMode,
};

const CONTEXT: &str = "Workspace";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ActionScope {
    Global,
    Terminal,
    Split,
    Palette,
    Editor,
    Sftp,
    FileManager,
    Preview,
    RemoteDesktop,
    Plugin,
    AiPanel,
}

impl ActionScope {
    fn local(self) -> bool {
        matches!(
            self,
            Self::Editor
                | Self::Sftp
                | Self::FileManager
                | Self::Preview
                | Self::RemoteDesktop
                | Self::Plugin
                | Self::AiPanel
        )
    }

    fn overlaps(self, other: Self) -> bool {
        self == other
            || matches!(self, Self::Global | Self::Palette | Self::Plugin)
            || matches!(other, Self::Global | Self::Palette | Self::Plugin)
            || matches!(
                (self, other),
                (Self::Terminal, Self::Split)
                    | (Self::Split, Self::Terminal)
                    | (Self::FileManager, Self::Preview)
                    | (Self::Preview, Self::FileManager)
                    | (Self::AiPanel, Self::Terminal | Self::Split | Self::Editor)
                    | (Self::Terminal | Self::Split | Self::Editor, Self::AiPanel)
            )
    }

    pub(crate) fn label_key(self) -> &'static str {
        match self {
            Self::Global => "settings_view.keybindings.scope_global",
            Self::Terminal => "settings_view.keybindings.scope_terminal",
            Self::Split => "settings_view.keybindings.scope_split",
            Self::Palette => "settings_view.keybindings.scope_palette",
            Self::Editor => "settings_view.keybindings.scope_editor",
            Self::Sftp => "settings_view.keybindings.scope_sftp",
            Self::FileManager => "settings_view.keybindings.scope_files",
            Self::Preview => "settings_view.keybindings.scope_preview",
            Self::RemoteDesktop => "settings_view.keybindings.scope_remote_desktop",
            Self::Plugin => "settings_view.keybindings.scope_plugins",
            Self::AiPanel => "settings_view.keybindings.scope_ai_panel",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TerminalBehavior {
    Always,
    Never,
    WhenPanelOpen,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum KeybindingSide {
    Mac,
    Other,
}

impl KeybindingSide {
    pub(crate) fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::Mac
        } else {
            Self::Other
        }
    }

    fn json_key(self) -> &'static str {
        match self {
            Self::Mac => "mac",
            Self::Other => "other",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct KeyCombo {
    pub(crate) key: String,
    #[serde(default)]
    pub(crate) ctrl: bool,
    #[serde(default)]
    pub(crate) shift: bool,
    #[serde(default)]
    pub(crate) alt: bool,
    #[serde(default)]
    pub(crate) meta: bool,
}

impl KeyCombo {
    fn plain(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            ctrl: false,
            shift: false,
            alt: false,
            meta: false,
        }
    }

    fn cmd(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            ctrl: false,
            shift: false,
            alt: false,
            meta: true,
        }
    }

    fn cmd_shift(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            ctrl: false,
            shift: true,
            alt: false,
            meta: true,
        }
    }

    fn cmd_alt(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            ctrl: false,
            shift: false,
            alt: true,
            meta: true,
        }
    }

    fn cmd_ctrl(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            ctrl: true,
            shift: false,
            alt: false,
            meta: true,
        }
    }

    fn ctrl(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            ctrl: true,
            shift: false,
            alt: false,
            meta: false,
        }
    }

    fn ctrl_shift(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            ctrl: true,
            shift: true,
            alt: false,
            meta: false,
        }
    }

    fn ctrl_alt(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            ctrl: true,
            shift: false,
            alt: true,
            meta: false,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ActionDefinition {
    pub(crate) id: Cow<'static, str>,
    pub(crate) label: Option<String>,
    pub(crate) scope: ActionScope,
    pub(crate) terminal_behavior: TerminalBehavior,
    pub(crate) mac: KeyCombo,
    pub(crate) other: KeyCombo,
}

impl ActionDefinition {
    pub(crate) fn default_combo(&self, side: KeybindingSide) -> &KeyCombo {
        match side {
            KeybindingSide::Mac => &self.mac,
            KeybindingSide::Other => &self.other,
        }
    }

    pub(crate) fn label_key(&self) -> String {
        format!("settings_view.keybindings.actions.{}", self.id)
    }
}

pub(crate) static ACTION_DEFINITIONS: LazyLock<Vec<ActionDefinition>> = LazyLock::new(|| {
    let mut actions = vec![
        def(
            "app.newTerminal",
            ActionScope::Global,
            KeyCombo::cmd("t"),
            KeyCombo::ctrl("t"),
        ),
        def(
            "app.shellLauncher",
            ActionScope::Global,
            KeyCombo::cmd_shift("t"),
            KeyCombo::ctrl_shift("t"),
        ),
        def_with_terminal_behavior(
            "app.closeTab",
            ActionScope::Global,
            TerminalBehavior::Never,
            KeyCombo::cmd("w"),
            KeyCombo::ctrl("w"),
        ),
        def(
            "app.closeOtherTabs",
            ActionScope::Global,
            KeyCombo::cmd_shift("w"),
            KeyCombo::ctrl_shift("w"),
        ),
        def(
            "app.newConnection",
            ActionScope::Global,
            KeyCombo::cmd("n"),
            KeyCombo::ctrl("n"),
        ),
        def(
            "app.settings",
            ActionScope::Global,
            KeyCombo::cmd(","),
            KeyCombo::ctrl(","),
        ),
        def(
            "app.quit",
            ActionScope::Global,
            KeyCombo::cmd("q"),
            KeyCombo::ctrl("q"),
        ),
        def(
            "app.toggleSidebar",
            ActionScope::Global,
            KeyCombo::cmd("\\"),
            KeyCombo::ctrl("\\"),
        ),
        def(
            "app.commandPalette",
            ActionScope::Global,
            KeyCombo::cmd("k"),
            KeyCombo::ctrl("k"),
        ),
        def(
            "app.zenMode",
            ActionScope::Global,
            KeyCombo::cmd_shift("z"),
            KeyCombo::ctrl_shift("z"),
        ),
        def(
            "app.toggleFullscreen",
            ActionScope::Global,
            KeyCombo::cmd_ctrl("f"),
            KeyCombo::plain("f11"),
        ),
        def(
            "app.nextTab",
            ActionScope::Global,
            KeyCombo::cmd("}"),
            KeyCombo::ctrl("tab"),
        ),
        def(
            "app.prevTab",
            ActionScope::Global,
            KeyCombo::cmd("{"),
            KeyCombo::ctrl_shift("tab"),
        ),
        def(
            "app.navBack",
            ActionScope::Global,
            KeyCombo::cmd("["),
            KeyCombo {
                key: "arrowleft".to_string(),
                ctrl: false,
                shift: false,
                alt: true,
                meta: false,
            },
        ),
        def(
            "app.navForward",
            ActionScope::Global,
            KeyCombo::cmd("]"),
            KeyCombo {
                key: "arrowright".to_string(),
                ctrl: false,
                shift: false,
                alt: true,
                meta: false,
            },
        ),
    ];

    for index in 1..=9 {
        actions.push(def(
            format!("app.goToTab{index}"),
            ActionScope::Global,
            KeyCombo::cmd(index.to_string()),
            KeyCombo::ctrl(index.to_string()),
        ));
    }

    actions.extend([
        def(
            "app.fontIncrease",
            ActionScope::Global,
            KeyCombo::cmd("="),
            KeyCombo::ctrl("="),
        ),
        def(
            "app.fontDecrease",
            ActionScope::Global,
            KeyCombo::cmd("-"),
            KeyCombo::ctrl("-"),
        ),
        def(
            "app.fontReset",
            ActionScope::Global,
            KeyCombo::cmd("0"),
            KeyCombo::ctrl("0"),
        ),
        def(
            "app.showShortcuts",
            ActionScope::Global,
            KeyCombo::cmd("/"),
            KeyCombo::ctrl("/"),
        ),
        def(
            "terminal.search",
            ActionScope::Terminal,
            KeyCombo::cmd("f"),
            KeyCombo::ctrl_shift("f"),
        ),
        def(
            "terminal.copy",
            ActionScope::Terminal,
            KeyCombo::cmd("c"),
            KeyCombo::ctrl_shift("c"),
        ),
        def(
            "terminal.cut",
            ActionScope::Terminal,
            KeyCombo::cmd("x"),
            KeyCombo::ctrl_shift("x"),
        ),
        def(
            "terminal.paste",
            ActionScope::Terminal,
            KeyCombo::cmd("v"),
            KeyCombo::ctrl_shift("v"),
        ),
        def(
            "terminal.clearScreen",
            ActionScope::Terminal,
            // Shells own Ctrl+L on every platform and use it to clear and
            // redraw the prompt. Keep the host-only action on a shifted chord.
            KeyCombo::ctrl_shift("l"),
            KeyCombo::ctrl_shift("l"),
        ),
        def(
            "terminal.aiPanel",
            ActionScope::Terminal,
            KeyCombo::cmd("i"),
            KeyCombo::ctrl_shift("i"),
        ),
        def(
            "terminal.recording",
            ActionScope::Terminal,
            KeyCombo::cmd_shift("r"),
            KeyCombo::ctrl_shift("r"),
        ),
        def(
            "terminal.toggleFreeTypeMode",
            ActionScope::Terminal,
            KeyCombo::cmd_shift("f"),
            KeyCombo::ctrl_alt("f"),
        ),
        def_with_terminal_behavior(
            "terminal.closePanel",
            ActionScope::Terminal,
            TerminalBehavior::WhenPanelOpen,
            KeyCombo::plain("escape"),
            KeyCombo::plain("escape"),
        ),
        def(
            "split.horizontal",
            ActionScope::Split,
            KeyCombo::cmd_shift("e"),
            KeyCombo::ctrl_shift("e"),
        ),
        def(
            "split.vertical",
            ActionScope::Split,
            KeyCombo::cmd_shift("d"),
            KeyCombo::ctrl_shift("d"),
        ),
        def(
            "split.closePane",
            ActionScope::Split,
            KeyCombo::cmd_shift("w"),
            KeyCombo::ctrl_shift("w"),
        ),
        def(
            "split.navLeft",
            ActionScope::Split,
            KeyCombo::cmd_alt("arrowleft"),
            KeyCombo::ctrl_alt("arrowleft"),
        ),
        def(
            "split.navRight",
            ActionScope::Split,
            KeyCombo::cmd_alt("arrowright"),
            KeyCombo::ctrl_alt("arrowright"),
        ),
        def(
            "palette.eventLog",
            ActionScope::Palette,
            KeyCombo::cmd("j"),
            KeyCombo::ctrl("j"),
        ),
        def(
            "palette.aiSidebar",
            ActionScope::Palette,
            KeyCombo::cmd_shift("a"),
            KeyCombo::ctrl_shift("a"),
        ),
        // Keep Ctrl+B available as the tmux prefix on Windows and Linux.
        def(
            "palette.broadcast",
            ActionScope::Palette,
            KeyCombo::cmd("b"),
            KeyCombo::ctrl_shift("b"),
        ),
    ]);

    actions.extend([
        def(
            "editor.save",
            ActionScope::Editor,
            KeyCombo::cmd("s"),
            KeyCombo::ctrl("s"),
        ),
        def(
            "editor.copy",
            ActionScope::Editor,
            KeyCombo::cmd("c"),
            KeyCombo::ctrl("c"),
        ),
        def(
            "editor.cut",
            ActionScope::Editor,
            KeyCombo::cmd("x"),
            KeyCombo::ctrl("x"),
        ),
        def(
            "editor.paste",
            ActionScope::Editor,
            KeyCombo::cmd("v"),
            KeyCombo::ctrl("v"),
        ),
        def(
            "editor.selectAll",
            ActionScope::Editor,
            KeyCombo::cmd("a"),
            KeyCombo::ctrl("a"),
        ),
        def(
            "editor.undo",
            ActionScope::Editor,
            KeyCombo::cmd("z"),
            KeyCombo::ctrl("z"),
        ),
        def(
            "editor.redo",
            ActionScope::Editor,
            KeyCombo::cmd_shift("z"),
            KeyCombo::ctrl_shift("z"),
        ),
        def(
            "editor.addNextMatch",
            ActionScope::Editor,
            KeyCombo::cmd("d"),
            KeyCombo::ctrl("d"),
        ),
        def(
            "editor.find",
            ActionScope::Editor,
            KeyCombo::cmd("f"),
            KeyCombo::ctrl("f"),
        ),
        def(
            "sftp.selectAll",
            ActionScope::Sftp,
            KeyCombo::cmd("a"),
            KeyCombo::ctrl("a"),
        ),
        def(
            "sftp.editPath",
            ActionScope::Sftp,
            KeyCombo::cmd("l"),
            KeyCombo::ctrl("l"),
        ),
        def(
            "sftp.revealTerminalDirectory",
            ActionScope::Sftp,
            KeyCombo::cmd_shift("g"),
            KeyCombo::ctrl_shift("g"),
        ),
        def(
            "sftp.open",
            ActionScope::Sftp,
            KeyCombo::plain("enter"),
            KeyCombo::plain("enter"),
        ),
        def(
            "sftp.preview",
            ActionScope::Sftp,
            KeyCombo::plain("space"),
            KeyCombo::plain("space"),
        ),
        def(
            "sftp.upload",
            ActionScope::Sftp,
            KeyCombo::plain("arrowright"),
            KeyCombo::plain("arrowright"),
        ),
        def(
            "sftp.download",
            ActionScope::Sftp,
            KeyCombo::plain("arrowleft"),
            KeyCombo::plain("arrowleft"),
        ),
        def(
            "sftp.delete",
            ActionScope::Sftp,
            KeyCombo::plain("delete"),
            KeyCombo::plain("delete"),
        ),
        def(
            "sftp.rename",
            ActionScope::Sftp,
            KeyCombo::plain("f2"),
            KeyCombo::plain("f2"),
        ),
    ]);

    actions.push(def(
        "sftp.togglePreviewSource",
        ActionScope::Sftp,
        KeyCombo::plain("u"),
        KeyCombo::plain("u"),
    ));

    actions.extend([
        def(
            "fileManager.selectAll",
            ActionScope::FileManager,
            KeyCombo::cmd("a"),
            KeyCombo::ctrl("a"),
        ),
        def(
            "fileManager.copy",
            ActionScope::FileManager,
            KeyCombo::cmd("c"),
            KeyCombo::ctrl("c"),
        ),
        def(
            "fileManager.cut",
            ActionScope::FileManager,
            KeyCombo::cmd("x"),
            KeyCombo::ctrl("x"),
        ),
        def(
            "fileManager.paste",
            ActionScope::FileManager,
            KeyCombo::cmd("v"),
            KeyCombo::ctrl("v"),
        ),
        def(
            "fileManager.editPath",
            ActionScope::FileManager,
            KeyCombo::cmd("l"),
            KeyCombo::ctrl("l"),
        ),
        def(
            "fileManager.open",
            ActionScope::FileManager,
            KeyCombo::plain("enter"),
            KeyCombo::plain("enter"),
        ),
        def(
            "fileManager.preview",
            ActionScope::FileManager,
            KeyCombo::plain("space"),
            KeyCombo::plain("space"),
        ),
        def(
            "fileManager.delete",
            ActionScope::FileManager,
            KeyCombo::plain("delete"),
            KeyCombo::plain("delete"),
        ),
        def(
            "fileManager.deleteOrParent",
            ActionScope::FileManager,
            KeyCombo::plain("backspace"),
            KeyCombo::plain("backspace"),
        ),
        def(
            "fileManager.rename",
            ActionScope::FileManager,
            KeyCombo::plain("f2"),
            KeyCombo::plain("f2"),
        ),
        def(
            "preview.previous",
            ActionScope::Preview,
            KeyCombo::plain("arrowleft"),
            KeyCombo::plain("arrowleft"),
        ),
        def(
            "preview.next",
            ActionScope::Preview,
            KeyCombo::plain("arrowright"),
            KeyCombo::plain("arrowright"),
        ),
        def(
            "preview.metadata",
            ActionScope::Preview,
            KeyCombo::plain("i"),
            KeyCombo::plain("i"),
        ),
        def(
            "preview.source",
            ActionScope::Preview,
            KeyCombo::plain("u"),
            KeyCombo::plain("u"),
        ),
        def(
            "preview.zoomIn",
            ActionScope::Preview,
            KeyCombo::plain("="),
            KeyCombo::plain("="),
        ),
        def(
            "preview.zoomOut",
            ActionScope::Preview,
            KeyCombo::plain("-"),
            KeyCombo::plain("-"),
        ),
        def(
            "preview.resetZoom",
            ActionScope::Preview,
            KeyCombo::plain("0"),
            KeyCombo::plain("0"),
        ),
        def(
            "preview.rotate",
            ActionScope::Preview,
            KeyCombo::plain("r"),
            KeyCombo::plain("r"),
        ),
        def(
            "remoteDesktop.copy",
            ActionScope::RemoteDesktop,
            KeyCombo::cmd("c"),
            KeyCombo::ctrl("c"),
        ),
        def(
            "remoteDesktop.paste",
            ActionScope::RemoteDesktop,
            KeyCombo::cmd("v"),
            KeyCombo::ctrl("v"),
        ),
    ]);
    actions.extend([
        def(
            "terminal.wordBackward",
            ActionScope::Terminal,
            KeyCombo {
                alt: true,
                ..KeyCombo::plain("arrowleft")
            },
            KeyCombo {
                alt: true,
                ..KeyCombo::plain("b")
            },
        ),
        def(
            "terminal.wordForward",
            ActionScope::Terminal,
            KeyCombo {
                alt: true,
                ..KeyCombo::plain("arrowright")
            },
            KeyCombo {
                alt: true,
                ..KeyCombo::plain("f")
            },
        ),
        def(
            "terminal.scrollPageUp",
            ActionScope::Terminal,
            KeyCombo {
                shift: true,
                ..KeyCombo::plain("pageup")
            },
            KeyCombo {
                shift: true,
                ..KeyCombo::plain("pageup")
            },
        ),
        def(
            "terminal.scrollPageDown",
            ActionScope::Terminal,
            KeyCombo {
                shift: true,
                ..KeyCombo::plain("pagedown")
            },
            KeyCombo {
                shift: true,
                ..KeyCombo::plain("pagedown")
            },
        ),
        def(
            "terminal.scrollLineUp",
            ActionScope::Terminal,
            KeyCombo {
                shift: true,
                ..KeyCombo::plain("arrowup")
            },
            KeyCombo {
                shift: true,
                ..KeyCombo::plain("arrowup")
            },
        ),
        def(
            "terminal.scrollLineDown",
            ActionScope::Terminal,
            KeyCombo {
                shift: true,
                ..KeyCombo::plain("arrowdown")
            },
            KeyCombo {
                shift: true,
                ..KeyCombo::plain("arrowdown")
            },
        ),
        def(
            "terminal.scrollTop",
            ActionScope::Terminal,
            KeyCombo {
                shift: true,
                ..KeyCombo::plain("home")
            },
            KeyCombo {
                shift: true,
                ..KeyCombo::plain("home")
            },
        ),
        def(
            "terminal.scrollBottom",
            ActionScope::Terminal,
            KeyCombo {
                shift: true,
                ..KeyCombo::plain("end")
            },
            KeyCombo {
                shift: true,
                ..KeyCombo::plain("end")
            },
        ),
        def(
            "terminal.pasteAlternate",
            ActionScope::Terminal,
            KeyCombo {
                shift: true,
                ..KeyCombo::plain("insert")
            },
            KeyCombo {
                shift: true,
                ..KeyCombo::plain("insert")
            },
        ),
        def(
            "terminal.copyAlternate",
            ActionScope::Terminal,
            KeyCombo::ctrl("insert"),
            KeyCombo::ctrl("insert"),
        ),
        def(
            "terminal.terminateTask",
            ActionScope::Terminal,
            KeyCombo::cmd_shift("k"),
            KeyCombo::cmd_shift("k"),
        ),
        def(
            "terminal.killTask",
            ActionScope::Terminal,
            KeyCombo {
                alt: true,
                ..KeyCombo::cmd_shift("k")
            },
            KeyCombo {
                alt: true,
                ..KeyCombo::cmd_shift("k")
            },
        ),
    ]);
    actions.extend([
        def(
            "terminal.aiSubmit",
            ActionScope::AiPanel,
            KeyCombo::plain("enter"),
            KeyCombo::plain("enter"),
        ),
        def(
            "terminal.aiInsert",
            ActionScope::AiPanel,
            KeyCombo::plain("tab"),
            KeyCombo::plain("tab"),
        ),
    ]);
    actions
});

fn def(
    id: impl Into<Cow<'static, str>>,
    scope: ActionScope,
    mac: KeyCombo,
    other: KeyCombo,
) -> ActionDefinition {
    def_with_terminal_behavior(id, scope, TerminalBehavior::Always, mac, other)
}

fn def_with_terminal_behavior(
    id: impl Into<Cow<'static, str>>,
    scope: ActionScope,
    terminal_behavior: TerminalBehavior,
    mac: KeyCombo,
    other: KeyCombo,
) -> ActionDefinition {
    ActionDefinition {
        id: id.into(),
        label: None,
        scope,
        terminal_behavior,
        mac: normalize_combo(mac),
        other: normalize_combo(other),
    }
}

pub(crate) fn action_definition(id: &str) -> Option<&'static ActionDefinition> {
    ACTION_DEFINITIONS.iter().find(|action| action.id == id)
}

pub(crate) fn effective_combo(
    definition: &ActionDefinition,
    overrides: &Map<String, Value>,
    side: KeybindingSide,
) -> Option<KeyCombo> {
    override_binding(&definition.id, overrides, side)
        .unwrap_or_else(|| Some(definition.default_combo(side).clone()))
}

fn effective_combos(
    definition: &ActionDefinition,
    overrides: &Map<String, Value>,
    side: KeybindingSide,
) -> Vec<KeyCombo> {
    let mut combos: Vec<_> = effective_combo(definition, overrides, side)
        .into_iter()
        .collect();
    if override_binding(&definition.id, overrides, side).is_none() {
        if definition.id == "terminal.scrollPageUp" {
            combos.push(KeyCombo::cmd("arrowup"));
        }
        if definition.id == "terminal.scrollPageDown" {
            combos.push(KeyCombo::cmd("arrowdown"));
        }
        if definition.scope == ActionScope::RemoteDesktop {
            for (ctrl, meta) in [(true, false), (false, true), (true, true)] {
                for shift in [false, true] {
                    let mut alias = definition.other.clone();
                    alias.ctrl = ctrl;
                    alias.meta = meta;
                    alias.shift = shift;
                    if !combos.contains(&alias) {
                        combos.push(alias);
                    }
                }
            }
        }
        if definition.id == "preview.zoomIn" {
            combos.push(KeyCombo::plain("+"));
        }
        if side == KeybindingSide::Mac
            && definition.scope == ActionScope::Plugin
            && definition.other.ctrl
        {
            combos.push(definition.other.clone());
        }
        if side == KeybindingSide::Mac
            && definition.scope == ActionScope::FileManager
            && definition.mac.meta
        {
            combos.push(definition.other.clone());
        }
        if definition.id == "sftp.delete" {
            combos.push(KeyCombo::plain("backspace"));
        }
        if side == KeybindingSide::Mac
            && ((definition.scope == ActionScope::Editor && definition.id != "editor.find")
                || matches!(definition.id.as_ref(), "sftp.selectAll" | "sftp.editPath"))
        {
            let mut control = definition.mac.clone();
            control.meta = false;
            control.ctrl = true;
            combos.push(control);
        }
        if definition.id == "editor.redo" {
            combos.push(KeyCombo::ctrl("y"));
            if side == KeybindingSide::Mac {
                combos.push(KeyCombo::cmd("y"));
            }
        }
    }
    combos
}

fn override_binding(
    action_id: &str,
    overrides: &Map<String, Value>,
    side: KeybindingSide,
) -> Option<Option<KeyCombo>> {
    let side_value = overrides.get(action_id)?.get(side.json_key())?;
    if side_value.is_null() {
        return Some(None);
    }
    serde_json::from_value::<KeyCombo>(side_value.clone())
        .ok()
        .map(normalize_combo)
        .map(Some)
}

pub(crate) fn set_unbound_override(
    overrides: &mut Map<String, Value>,
    action_id: &str,
    side: KeybindingSide,
) {
    if action_definition(action_id).is_none() && !is_plugin_action_id(action_id) {
        return;
    }
    let mut entry = overrides
        .get(action_id)
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    // JSON null is an explicit tombstone that suppresses the built-in default.
    entry.insert(side.json_key().to_string(), Value::Null);
    overrides.insert(action_id.to_string(), Value::Object(entry));
}

pub(crate) fn set_definition_override(
    overrides: &mut Map<String, Value>,
    definition: &ActionDefinition,
    side: KeybindingSide,
    combo: KeyCombo,
) {
    let action_id = definition.id.as_ref();
    let combo = normalize_combo(combo);
    if combo == *definition.default_combo(side) {
        reset_override(overrides, action_id, side);
        return;
    }

    let mut entry = overrides
        .get(action_id)
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    if let Ok(value) = serde_json::to_value(combo) {
        entry.insert(side.json_key().to_string(), value);
        overrides.insert(action_id.to_string(), Value::Object(entry));
    }
}

pub(crate) fn reset_override(
    overrides: &mut Map<String, Value>,
    action_id: &str,
    side: KeybindingSide,
) {
    let mut remove_action = false;
    if let Some(value) = overrides.get_mut(action_id)
        && let Some(object) = value.as_object_mut()
    {
        object.remove(side.json_key());
        remove_action = object.is_empty();
    }
    if remove_action {
        overrides.remove(action_id);
    }
}

pub(crate) fn sanitize_imported_overrides(value: Value) -> Result<Map<String, Value>, String> {
    let Value::Object(input) = value else {
        return Err("root must be an object".to_string());
    };

    let mut sanitized = Map::new();
    for (action_id, value) in input {
        if action_definition(&action_id).is_none() && !is_plugin_action_id(&action_id) {
            return Err(format!("unknown action id: {action_id}"));
        }
        let Value::Object(object) = value else {
            return Err(format!("invalid override for {action_id}"));
        };

        let mut entry = Map::new();
        for side in [KeybindingSide::Mac, KeybindingSide::Other] {
            if let Some(combo_value) = object.get(side.json_key()) {
                if combo_value.is_null() {
                    entry.insert(side.json_key().to_string(), Value::Null);
                    continue;
                }
                let combo = serde_json::from_value::<KeyCombo>(combo_value.clone())
                    .map_err(|_| format!("invalid shortcut for {action_id}"))?;
                let normalized = normalize_combo(combo);
                let value = serde_json::to_value(normalized)
                    .map_err(|_| format!("invalid shortcut for {action_id}"))?;
                entry.insert(side.json_key().to_string(), value);
            }
        }
        if !entry.is_empty() {
            sanitized.insert(action_id, Value::Object(entry));
        }
    }

    Ok(sanitized)
}

pub(crate) fn modified_count(overrides: &Map<String, Value>) -> usize {
    overrides.len()
}

pub(crate) fn conflicts_in_definitions<'a>(
    action_id: &str,
    combo: &KeyCombo,
    overrides: &Map<String, Value>,
    side: KeybindingSide,
    definitions: &'a [ActionDefinition],
) -> Vec<&'a ActionDefinition> {
    let Some(action) = definitions
        .iter()
        .find(|definition| definition.id == action_id)
    else {
        return Vec::new();
    };
    definitions
        .iter()
        .filter(|definition| definition.id != action_id && action.scope.overlaps(definition.scope))
        .filter(|definition| effective_combos(definition, overrides, side).contains(combo))
        .collect()
}

pub(crate) fn keystroke_matches_action(
    keystroke: &Keystroke,
    action_id: &str,
    overrides: &Map<String, Value>,
) -> bool {
    let Some(definition) = action_definition(action_id) else {
        return false;
    };
    let Some(combo) = combo_from_keystroke(keystroke) else {
        return false;
    };
    effective_combos(definition, overrides, KeybindingSide::current()).contains(&combo)
}

pub(crate) fn matched_action_for_keystroke(
    keystroke: &Keystroke,
    overrides: &Map<String, Value>,
) -> Option<(&'static ActionDefinition, KeyCombo)> {
    let combo = combo_from_keystroke(keystroke)?;
    let side = KeybindingSide::current();
    ACTION_DEFINITIONS
        .iter()
        .filter(|definition| !definition.scope.local())
        .find(|definition| effective_combo(definition, overrides, side).as_ref() == Some(&combo))
        .map(|definition| (definition, combo))
}

pub(crate) fn normalize_plugin_keystroke(keystroke: &Keystroke) -> Option<String> {
    let combo = combo_from_keystroke(keystroke)?;
    let mut parts = Vec::new();
    // Tauri's pluginHostUi collapses Cmd/Meta and Ctrl into the same "ctrl"
    // token for plugin keybindings. Preserve that public contract so existing
    // plugin descriptors such as "Cmd+Shift+R" keep working cross-platform.
    if combo.ctrl || combo.meta {
        parts.push("ctrl".to_string());
    }
    if combo.shift {
        parts.push("shift".to_string());
    }
    if combo.alt {
        parts.push("alt".to_string());
    }
    parts.push(normalize_plugin_event_key(&combo.key)?);
    parts.sort();
    Some(parts.join("+"))
}

fn normalize_plugin_key_part(part: &str) -> Option<String> {
    let normalized = part.trim().to_lowercase();
    if normalized.is_empty() {
        return None;
    }
    Some(match normalized.as_str() {
        "cmd" | "command" | "meta" | "super" | "win" | "⌘" => "ctrl".to_string(),
        "control" | "ctrl" | "⌃" => "ctrl".to_string(),
        "option" | "alt" | "⌥" => "alt".to_string(),
        "shift" | "⇧" => "shift".to_string(),
        "escape" | "esc" => "esc".to_string(),
        "spacebar" | "space" | " " => "space".to_string(),
        "left" => "arrowleft".to_string(),
        "right" => "arrowright".to_string(),
        "up" => "arrowup".to_string(),
        "down" => "arrowdown".to_string(),
        key => key.to_string(),
    })
}

fn normalize_plugin_event_key(key: &str) -> Option<String> {
    normalize_plugin_key_part(if key == " " { "space" } else { key })
}

pub(crate) fn action_allowed_by_terminal_behavior(
    definition: &ActionDefinition,
    combo: &KeyCombo,
    terminal_active: bool,
    terminal_panel_open: bool,
) -> bool {
    if !terminal_active {
        return definition.terminal_behavior != TerminalBehavior::WhenPanelOpen
            || terminal_panel_open;
    }

    let mac_safe_meta = cfg!(target_os = "macos") && combo.meta && !combo.ctrl;
    if mac_safe_meta {
        return definition.terminal_behavior != TerminalBehavior::WhenPanelOpen
            || terminal_panel_open;
    }

    match definition.terminal_behavior {
        TerminalBehavior::Always => true,
        TerminalBehavior::Never => false,
        TerminalBehavior::WhenPanelOpen => terminal_panel_open,
    }
}

pub(crate) fn format_combo(combo: &KeyCombo) -> String {
    let mut parts = Vec::new();
    if cfg!(target_os = "macos") {
        if combo.ctrl {
            parts.push("⌃".to_string());
        }
        if combo.alt {
            parts.push("⌥".to_string());
        }
        if combo.shift {
            parts.push("⇧".to_string());
        }
        if combo.meta {
            parts.push("⌘".to_string());
        }
        parts.push(display_key(&combo.key).to_string());
        parts.join("")
    } else {
        if combo.ctrl {
            parts.push("Ctrl".to_string());
        }
        if combo.alt {
            parts.push("Alt".to_string());
        }
        if combo.shift {
            parts.push("Shift".to_string());
        }
        if combo.meta {
            parts.push("Meta".to_string());
        }
        parts.push(display_key(&combo.key).to_string());
        parts.join("+")
    }
}

fn display_key(key: &str) -> &str {
    match key {
        "arrowleft" => "←",
        "arrowright" => "→",
        "arrowup" => "↑",
        "arrowdown" => "↓",
        "escape" => "Esc",
        "tab" => "Tab",
        "enter" => "Enter",
        "backspace" => "Backspace",
        " " | "space" => "Space",
        key => key,
    }
}

pub(crate) fn combo_from_keystroke(keystroke: &Keystroke) -> Option<KeyCombo> {
    let key = normalize_key_from_keystroke(keystroke)?;
    if matches!(
        key.as_str(),
        "shift" | "control" | "ctrl" | "alt" | "meta" | "cmd" | "super"
    ) {
        return None;
    }

    Some(normalize_combo(KeyCombo {
        key,
        ctrl: keystroke.modifiers.control,
        shift: keystroke.modifiers.shift,
        alt: keystroke.modifiers.alt,
        meta: keystroke.modifiers.platform,
    }))
}

fn normalize_key_from_keystroke(keystroke: &Keystroke) -> Option<String> {
    if let Some(key_char) = keystroke.key_char.as_deref()
        && !key_char.trim().is_empty()
        && key_char.chars().count() == 1
        && !keystroke.modifiers.control
        && !keystroke.modifiers.platform
    {
        return Some(key_char.to_lowercase());
    }

    let key = keystroke.key.as_str();
    if key.is_empty() {
        return None;
    }
    Some(match key {
        "left" => "arrowleft".to_string(),
        "right" => "arrowright".to_string(),
        "up" => "arrowup".to_string(),
        "down" => "arrowdown".to_string(),
        "esc" => "escape".to_string(),
        key => key.to_lowercase(),
    })
}

fn normalize_combo(mut combo: KeyCombo) -> KeyCombo {
    combo.key = match combo.key.as_str() {
        "left" => "arrowleft".to_string(),
        "right" => "arrowright".to_string(),
        "up" => "arrowup".to_string(),
        "down" => "arrowdown".to_string(),
        "esc" => "escape".to_string(),
        "space" => "space".to_string(),
        key => key.to_lowercase(),
    };

    if printable_symbol_encodes_shift(&combo.key) {
        combo.shift = false;
    }
    if (combo.ctrl || combo.meta) && layout_symbol_may_include_alt(&combo.key) {
        combo.alt = false;
    }
    combo
}

fn printable_symbol_encodes_shift(key: &str) -> bool {
    matches!(
        key,
        "~" | "!"
            | "@"
            | "#"
            | "$"
            | "%"
            | "^"
            | "&"
            | "*"
            | "("
            | ")"
            | "_"
            | "+"
            | "{"
            | "}"
            | "|"
            | ":"
            | "\""
            | "<"
            | ">"
            | "?"
    )
}

fn layout_symbol_may_include_alt(key: &str) -> bool {
    matches!(key, "[" | "]" | "{" | "}" | "\\" | "|" | "@" | "#" | "~")
}

fn combo_to_gpui(combo: &KeyCombo) -> String {
    let mut parts = Vec::new();
    if combo.meta {
        parts.push("cmd".to_string());
    }
    if combo.ctrl {
        parts.push("ctrl".to_string());
    }
    if combo.alt {
        parts.push("alt".to_string());
    }
    if combo.shift {
        parts.push("shift".to_string());
    }
    parts.push(match combo.key.as_str() {
        "arrowleft" => "left".to_string(),
        "arrowright" => "right".to_string(),
        "arrowup" => "up".to_string(),
        "arrowdown" => "down".to_string(),
        key => key.to_string(),
    });
    parts.join("-")
}

pub(crate) fn startup_key_bindings(overrides: &Map<String, Value>) -> Vec<KeyBinding> {
    let side = KeybindingSide::current();
    let mut bindings = Vec::new();
    for definition in ACTION_DEFINITIONS
        .iter()
        .filter(|definition| !definition.scope.local() && !terminal_leaf_action(&definition.id))
    {
        let default = definition.default_combo(side).clone();
        let effective = effective_combo(definition, overrides, side);
        if effective.as_ref() != Some(&default) {
            let default_keystroke = combo_to_gpui(&default);
            bindings.push(KeyBinding::new(
                &default_keystroke,
                NoAction {},
                Some(CONTEXT),
            ));
            if matches!(definition.id.as_ref(), "app.commandPalette" | "app.quit") {
                bindings.push(KeyBinding::new(&default_keystroke, NoAction {}, None));
            }
        }
        if definition.terminal_behavior != TerminalBehavior::Always {
            continue;
        }
        let Some(effective) = effective else {
            continue;
        };
        if definition.id == "split.closePane" && effective == default {
            continue;
        }
        push_action_binding(&mut bindings, &definition.id, &effective);
    }
    bindings
}

pub(crate) fn runtime_rebind_key_bindings(
    action_id: &str,
    previous: Option<&KeyCombo>,
    next: Option<&KeyCombo>,
) -> Vec<KeyBinding> {
    if is_plugin_action_id(action_id) || terminal_leaf_action(action_id) {
        return Vec::new();
    }
    if action_definition(action_id).is_some_and(|definition| definition.scope.local()) {
        return Vec::new();
    }
    let mut bindings = Vec::new();
    if previous != next {
        if let Some(previous) = previous {
            let previous_keystroke = combo_to_gpui(previous);
            bindings.push(KeyBinding::new(
                &previous_keystroke,
                NoAction {},
                Some(CONTEXT),
            ));
            if matches!(action_id, "app.commandPalette" | "app.quit") {
                bindings.push(KeyBinding::new(&previous_keystroke, NoAction {}, None));
            }
        }
    }
    let Some(next) = next else {
        return bindings;
    };
    if action_id == "split.closePane"
        && action_definition(action_id)
            .is_some_and(|definition| next == definition.default_combo(KeybindingSide::current()))
    {
        return bindings;
    }
    if action_definition(action_id)
        .is_some_and(|definition| definition.terminal_behavior != TerminalBehavior::Always)
    {
        return bindings;
    }
    push_action_binding(&mut bindings, action_id, next);
    bindings
}

fn push_action_binding(bindings: &mut Vec<KeyBinding>, action_id: &str, combo: &KeyCombo) {
    let keystroke = combo_to_gpui(combo);
    macro_rules! push_binding {
        ($action:expr) => {
            bindings.push(KeyBinding::new(&keystroke, $action, Some(CONTEXT)))
        };
        ($action:expr, global) => {
            bindings.push(KeyBinding::new(&keystroke, $action, None))
        };
        ($action:expr, workspace_and_global) => {{
            bindings.push(KeyBinding::new(&keystroke, $action.clone(), Some(CONTEXT)));
            bindings.push(KeyBinding::new(&keystroke, $action, None));
        }};
    }
    match action_id {
        "app.newTerminal" => push_binding!(NewTerminal),
        "app.shellLauncher" => push_binding!(ShellLauncher),
        "app.closeTab" => push_binding!(CloseTab),
        "app.closeOtherTabs" => push_binding!(CloseOtherTabs),
        "app.newConnection" => push_binding!(NewConnection),
        "app.settings" => push_binding!(OpenSettings),
        "app.quit" => push_binding!(Quit, workspace_and_global),
        "app.toggleSidebar" => push_binding!(ToggleLeftSidebar),
        "app.commandPalette" => push_binding!(CommandPalette, workspace_and_global),
        "app.zenMode" => push_binding!(ZenMode),
        "app.toggleFullscreen" => push_binding!(ToggleFullscreen),
        "app.nextTab" => push_binding!(NextTab),
        "app.prevTab" => push_binding!(PrevTab),
        "app.goToTab1" => push_binding!(GoToTab1),
        "app.goToTab2" => push_binding!(GoToTab2),
        "app.goToTab3" => push_binding!(GoToTab3),
        "app.goToTab4" => push_binding!(GoToTab4),
        "app.goToTab5" => push_binding!(GoToTab5),
        "app.goToTab6" => push_binding!(GoToTab6),
        "app.goToTab7" => push_binding!(GoToTab7),
        "app.goToTab8" => push_binding!(GoToTab8),
        "app.goToTab9" => push_binding!(GoToTab9),
        "app.fontIncrease" => push_binding!(FontIncrease),
        "app.fontDecrease" => push_binding!(FontDecrease),
        "app.fontReset" => push_binding!(FontReset),
        "app.showShortcuts" => push_binding!(ShowShortcuts),
        "terminal.search" => push_binding!(Find),
        "terminal.copy" => push_binding!(Copy),
        "terminal.cut" => push_binding!(Cut),
        "terminal.paste" => push_binding!(Paste),
        "terminal.clearScreen" => push_binding!(TerminalClearScreen),
        "terminal.recording" => push_binding!(TerminalRecording),
        "terminal.toggleFreeTypeMode" => push_binding!(TerminalFreeTypeMode),
        "terminal.closePanel" => {}
        "split.horizontal" => push_binding!(SplitHorizontal),
        "split.vertical" => push_binding!(SplitVertical),
        "split.closePane" => push_binding!(crate::ClosePane),
        "split.navLeft" => push_binding!(SplitNavLeft),
        "split.navRight" => push_binding!(SplitNavRight),
        "palette.eventLog" => push_binding!(PaletteEventLog),
        "palette.aiSidebar" => push_binding!(PaletteRightSidebar),
        "palette.broadcast" => push_binding!(PaletteBroadcast),
        "app.navBack" | "app.navForward" => {}
        _ => {}
    }
}

pub(crate) fn install_context_keybindings(overrides: &Map<String, Value>, cx: &mut gpui::App) {
    // Native bindings dispatch before text key handlers. Modal controls must
    // receive the chord themselves instead of invoking a background action.
    cx.bind_keys(ACTION_DEFINITIONS.iter().flat_map(|definition| {
        effective_combos(definition, overrides, KeybindingSide::current())
            .into_iter()
            .map(|combo| {
                KeyBinding::new(
                    &combo_to_gpui(&combo),
                    NoAction {},
                    Some(oxide_ui::modal::MODAL_KEY_CONTEXT),
                )
            })
    }));
    use oxide_editor::{EditorKeybindings, EditorShortcut};
    let mut bindings = Vec::new();
    let mut editor_context = Vec::new();
    for (id, action) in [
        ("editor.save", EditorShortcut::Save),
        ("editor.copy", EditorShortcut::Copy),
        ("editor.cut", EditorShortcut::Cut),
        ("editor.paste", EditorShortcut::Paste),
        ("editor.selectAll", EditorShortcut::SelectAll),
        ("editor.undo", EditorShortcut::Undo),
        ("editor.redo", EditorShortcut::Redo),
        ("editor.addNextMatch", EditorShortcut::AddNextMatch),
        ("editor.find", EditorShortcut::Find),
    ] {
        let definition = action_definition(id).expect("editor shortcut is registered");
        for combo in effective_combos(definition, overrides, KeybindingSide::current()) {
            let keystroke = combo_to_gpui(&combo);
            bindings.push((KeyBinding::new(&keystroke, NoAction {}, None), action));
            // Native action dispatch precedes keydown bubbling. Let the editor
            // handle these chords instead of a lower-priority workspace action.
            editor_context.push(KeyBinding::new(&keystroke, NoAction {}, Some("TextEditor")));
        }
    }
    cx.bind_keys(editor_context);
    cx.set_global(EditorKeybindings {
        bindings,
        normalize: |keystroke| {
            let combo = combo_from_keystroke(keystroke)?;
            Keystroke::parse(&combo_to_gpui(&combo)).ok()
        },
    });
    use oxide_terminal::{TerminalKeybindings, TerminalShortcut};
    let mut bindings = Vec::new();
    for (id, action) in [
        ("terminal.copy", TerminalShortcut::Copy),
        ("terminal.paste", TerminalShortcut::Paste),
        ("terminal.copyAlternate", TerminalShortcut::Copy),
        ("terminal.pasteAlternate", TerminalShortcut::Paste),
        ("terminal.terminateTask", TerminalShortcut::Terminate),
        ("terminal.killTask", TerminalShortcut::Kill),
        ("terminal.wordBackward", TerminalShortcut::WordBackward),
        ("terminal.wordForward", TerminalShortcut::WordForward),
        ("terminal.scrollPageUp", TerminalShortcut::PageUp),
        ("terminal.scrollPageDown", TerminalShortcut::PageDown),
        ("terminal.scrollLineUp", TerminalShortcut::LineUp),
        ("terminal.scrollLineDown", TerminalShortcut::LineDown),
        ("terminal.scrollTop", TerminalShortcut::Top),
        ("terminal.scrollBottom", TerminalShortcut::Bottom),
    ] {
        for combo in effective_combos(
            action_definition(id).unwrap(),
            overrides,
            KeybindingSide::current(),
        ) {
            bindings.push((
                KeyBinding::new(&combo_to_gpui(&combo), NoAction {}, None),
                action,
            ));
        }
    }
    cx.set_global(TerminalKeybindings {
        bindings,
        normalize: |key| {
            let combo = combo_from_keystroke(key)?;
            Keystroke::parse(&combo_to_gpui(&combo)).ok()
        },
    });
}

fn is_plugin_action_id(id: &str) -> bool {
    id.strip_prefix("plugin.keybinding:")
        .and_then(|key| serde_json::from_str::<[String; 2]>(key).ok())
        .is_some_and(|parts| parts.iter().all(|part| !part.is_empty()))
}

pub(crate) fn plugin_action_definition(
    entry: &oxide_plugin_registry::NativePluginRuntimeKeybindingContribution,
) -> Option<ActionDefinition> {
    let mut other = KeyCombo::plain("");
    for part in entry.normalized_keybinding.split('+') {
        match part {
            "ctrl" => other.ctrl = true,
            "shift" => other.shift = true,
            "alt" => other.alt = true,
            key if other.key.is_empty() => other.key = key.to_string(),
            _ => return None,
        }
    }
    if other.key.is_empty() {
        return None;
    }
    let other = normalize_combo(other);
    let mut mac = other.clone();
    if mac.ctrl {
        mac.ctrl = false;
        mac.meta = true;
    }
    Some(ActionDefinition {
        // Runtime registration IDs may change on activation. The plugin and
        // declared chord identify the binding independently of that lifecycle.
        id: format!(
            "plugin.keybinding:{}",
            serde_json::to_string(&[&entry.plugin_id, &entry.normalized_keybinding]).ok()?
        )
        .into(),
        label: Some(format!("{}: {}", entry.plugin_name, entry.label)),
        scope: ActionScope::Plugin,
        terminal_behavior: TerminalBehavior::Never,
        mac,
        other,
    })
}

pub(crate) fn plugin_binding_matches(
    entry: &oxide_plugin_registry::NativePluginRuntimeKeybindingContribution,
    keystroke: &Keystroke,
    overrides: &Map<String, Value>,
) -> bool {
    let Some(definition) = plugin_action_definition(entry) else {
        return false;
    };
    if override_binding(&definition.id, overrides, KeybindingSide::current()).is_none() {
        return normalize_plugin_keystroke(keystroke).as_ref()
            == Some(&entry.normalized_keybinding);
    }
    combo_from_keystroke(keystroke).is_some_and(|combo| {
        effective_combo(&definition, overrides, KeybindingSide::current()).as_ref() == Some(&combo)
    })
}

pub(crate) fn matched_scoped_action(
    keystroke: &Keystroke,
    scope: ActionScope,
    overrides: &Map<String, Value>,
) -> Option<&'static str> {
    let combo = combo_from_keystroke(keystroke)?;
    ACTION_DEFINITIONS
        .iter()
        .filter(|definition| definition.scope == scope)
        .find(|definition| {
            effective_combos(definition, overrides, KeybindingSide::current()).contains(&combo)
        })
        .map(|definition| definition.id.as_ref())
}

fn terminal_leaf_action(id: &str) -> bool {
    id.starts_with("terminal.scroll")
        || matches!(
            id,
            "terminal.copyAlternate"
                | "terminal.pasteAlternate"
                | "terminal.wordBackward"
                | "terminal.wordForward"
                | "terminal.terminateTask"
                | "terminal.killTask"
        )
}
