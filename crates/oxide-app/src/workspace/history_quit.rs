use gpui::{App, Context};

use crate::workspace::{TabCloseConfirm, WorkspaceApp, window_registry};

/// All explicit exit paths leave the UI running while the database acknowledges its final changes.
pub(crate) fn request_app_quit(cx: &mut App) {
    // Quit can originate inside a workspace update; release its lease before reading it.
    cx.defer(start_app_quit);
}
fn start_app_quit(cx: &mut App) {
    oxide_desktop_presence::request_quit();
    cx.quit();
}

impl WorkspaceApp {
    /// Keeps explicit quit intents behind the same confirmation used for terminal closes.
    pub(crate) fn request_application_quit(&mut self, cx: &mut Context<Self>) {
        if self.tab_host.read(cx).close_confirm() != Some(&TabCloseConfirm::ApplicationQuit) {
            self.skip_future_ssh_close_confirmations = false;
            self.tab_host.update(cx, |host, cx| {
                host.open_close_confirm(TabCloseConfirm::ApplicationQuit, cx);
            });
        }
        // The close dialog is mounted by the main shell, including for tray and detached-window quits.
        oxide_desktop_presence::show_main_window();
        if let Some(handle) = self
            .window_registry
            .handle_for_role(window_registry::WindowRole::Main)
        {
            let _ = handle.update(cx, |_root, window, _cx| window.activate_window());
        }
        cx.notify();
    }

    /// Continues an approved exit while preserving the unsaved-document guard.
    pub(in crate::workspace) fn quit_after_confirmation(&mut self, cx: &mut Context<Self>) {
        request_app_quit(cx);
    }
}
