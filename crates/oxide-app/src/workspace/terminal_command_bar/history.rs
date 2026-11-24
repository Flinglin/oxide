use super::*;
use oxide_terminal_triggers::{SavedConnectionKind, SavedConnectionRef};

fn saved_console_history_identity(
    connections: &ConnectionStore,
    connection: &SavedConnectionRef,
) -> Option<(String, String)> {
    // Scope uses connection metadata only. Credentials and transport tuning never enter the key.
    let (scope, label) = match connection.kind {
        SavedConnectionKind::Telnet => {
            let profile = connections
                .telnet_profiles()
                .iter()
                .find(|p| p.id == connection.id)?;
            (
                serde_json::json!([connection.kind, profile.id, profile.host, profile.port]),
                &profile.name,
            )
        }
        SavedConnectionKind::Mosh => {
            let profile = connections.get_mosh_profile(&connection.id)?;
            (
                serde_json::json!([
                    connection.kind,
                    profile.id,
                    profile.host,
                    profile.ssh_port,
                    profile.username
                ]),
                &profile.name,
            )
        }
        SavedConnectionKind::Serial => {
            let profile = connections
                .serial_profiles()
                .iter()
                .find(|p| p.id == connection.id)?;
            (
                serde_json::json!([connection.kind, profile.id, profile.port_path]),
                &profile.name,
            )
        }
        SavedConnectionKind::Ssh => return None,
    };
    Some((scope.to_string(), label.clone()))
}

impl WorkspaceApp {
    pub(in crate::workspace) fn bind_saved_console_command_history(
        &self,
        session_id: TerminalSessionId,
        cx: &mut Context<Self>,
    ) {
        let Some(connection) = self.terminal_saved_connection_refs.get(&session_id) else {
            return;
        };
        let Some((scope, label)) =
            saved_console_history_identity(&self.connection_store, connection)
        else {
            return;
        };
        let pane = {
            let host = self.tab_host.read(cx);
            host.terminal_location(session_id)
                .and_then(|location| host.panes().get(&location.pane_id).cloned())
        };
        if let Some(pane) = pane {
            let history = self.terminal_history_store.history(scope, label);
            pane.update(cx, |pane, cx| pane.set_command_history(history, cx));
        }
    }

    pub(in crate::workspace) fn command_history_icon_button(
        &self,
        icon: LucideIcon,
        size: f32,
        label_key: &'static str,
        listener: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.workspace_tooltip_icon_button(
            icon,
            size,
            rgb(self.theme_tokens.ui.text_muted),
            IconButtonOptions::compact(24.0),
            self.i18n.t(label_key),
            "command-history-action",
            true,
            listener,
            cx.entity(),
        )
    }
    pub(in crate::workspace) fn open_command_history_clear_confirm(
        &mut self,
        all: bool,
        cx: &mut Context<Self>,
    ) {
        self.overlay.update(cx, |overlay, cx| {
            overlay.open_confirm(
                overlay::WorkspaceOverlayConfirmKind::CommandHistoryClear { all },
                cx,
            );
        });
    }
    pub(in crate::workspace) fn ssh_command_history(
        &self,
        node_id: &NodeId,
    ) -> Option<SharedTerminalCommandHistory> {
        let node = self.ssh_nodes.get(node_id)?;
        if let Some(saved_id) = &node.saved_connection_id {
            // Renames keep history; changing the endpoint or account selects a different scope.
            let scope = serde_json::to_string(&(
                saved_id,
                &node.endpoint.host,
                node.endpoint.port,
                &node.endpoint.username,
            ))
            .ok()?;
            Some(
                self.terminal_history_store
                    .history(scope, node.title.clone()),
            )
        } else {
            self.ssh_terminal_command_histories.get(node_id).cloned()
        }
    }

    pub(in crate::workspace) fn active_command_history(
        &self,
        cx: &App,
    ) -> Option<SharedTerminalCommandHistory> {
        self.active_pane(cx)
            .map(|pane| pane.read(cx).command_history())
    }

    pub(in crate::workspace) fn remove_history_suggestion(
        &mut self,
        command: &str,
        cx: &mut Context<Self>,
    ) {
        if self.terminal_history_all_connections {
            for (_, _, history) in self.terminal_history_store.histories() {
                history.remove(command);
            }
        }
        if let Some(history) = self.active_command_history(cx) {
            history.remove(command);
        }
        self.terminal_command_sender
            .update(cx, |sender, cx| sender.show_compact_history(cx));
        cx.notify();
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::workspace) enum SavedConsoleProfileKind {
    Telnet,
    Mosh,
    Serial,
}

impl SavedConsoleProfileKind {
    fn connection_kind(self) -> SavedConnectionKind {
        match self {
            Self::Telnet => SavedConnectionKind::Telnet,
            Self::Mosh => SavedConnectionKind::Mosh,
            Self::Serial => SavedConnectionKind::Serial,
        }
    }
}

pub(in crate::workspace) fn delete_saved_console_profile(
    connections: &mut ConnectionStore,
    terminal_history_store: &TerminalHistoryStore,
    profile_kind: SavedConsoleProfileKind,
    id: &str,
) -> anyhow::Result<bool> {
    let mut history_prefix = serde_json::to_string(&(profile_kind.connection_kind(), id))?;
    // Keep the JSON field boundary so similar IDs cannot match; targets may have changed.
    history_prefix.pop();
    history_prefix.push(',');
    let deleted = match profile_kind {
        SavedConsoleProfileKind::Telnet => connections.delete_telnet_profile(id),
        SavedConsoleProfileKind::Mosh => connections.delete_mosh_profile(id),
        SavedConsoleProfileKind::Serial => connections.delete_serial_profile(id),
    }?;
    if deleted {
        terminal_history_store.remove_scopes_with_prefix(&history_prefix);
    }
    Ok(deleted)
}
