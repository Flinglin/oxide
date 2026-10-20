use super::*;
use oxide_connections::{
    list_ssh_config_hosts, resolve_ssh_config_alias, saved_connection_from_ssh_host,
};

impl WorkspaceApp {
    pub(in crate::workspace) fn open_onboarding_from_palette(&mut self, cx: &mut Context<Self>) {
        self.close_settings_select();
        self.blur_text_inputs(cx);
        self.finish_settings_slider_drag(cx);
        let disclaimer_accepted = self.onboarding.disclaimer_accepted
            || self
                .settings_store
                .settings()
                .onboarding_disclaimer_accepted
            || self.settings_store.settings().onboarding_completed;
        self.edit_settings(
            move |settings| {
                settings.onboarding_completed = false;
                // Persist the implied acceptance when migrating a completed legacy flow.
                settings.onboarding_disclaimer_accepted = disclaimer_accepted;
            },
            cx,
        );
        self.onboarding
            .reset_for_open(self.settings_store.settings());
        cx.notify();
    }

    pub(in crate::workspace) fn complete_onboarding(&mut self, cx: &mut Context<Self>) {
        if !self.onboarding.disclaimer_accepted {
            return;
        }
        let previous_settings = self.settings_store.settings().clone();
        let mut next_settings = previous_settings.clone();
        next_settings.onboarding_completed = true;
        next_settings.onboarding_disclaimer_accepted = true;

        // Publish completion and runtime opt-ins only after the durable file swap succeeds.
        match self.settings_store.replace_and_save(next_settings) {
            Ok(saved) => {
                self.apply_loaded_settings_to_runtime(&previous_settings, &saved.settings, cx);
                self.settings_workspace.update(cx, |settings, _cx| {
                    settings.acknowledge_external_store_state()
                });
                self.emit_native_plugin_settings_events(&previous_settings, &saved.settings, cx);
                self.sync_tab_titles(cx);
                self.onboarding.save_failed = false;
                self.onboarding.open = false;
                self.close_settings_select();
                self.blur_text_inputs(cx);
                self.finish_settings_slider_drag(cx);
            }
            Err(_) => {
                // Settings errors may contain file content; expose only a localized save failure.
                self.onboarding.save_failed = true;
            }
        }
        cx.notify();
    }

    pub(in crate::workspace) fn toggle_onboarding_disclaimer_acceptance(
        &mut self,
        cx: &mut Context<Self>,
    ) {
        let accepted = !self.onboarding.disclaimer_accepted;
        self.onboarding.disclaimer_accepted = accepted;
        self.edit_settings(
            move |settings| settings.onboarding_disclaimer_accepted = accepted,
            cx,
        );
    }

    pub(in crate::workspace) fn onboarding_go_to_step(
        &mut self,
        step: usize,
        cx: &mut Context<Self>,
    ) {
        if step >= ONBOARDING_TOTAL_STEPS || (!self.onboarding.disclaimer_accepted && step > 0) {
            return;
        }
        if step == self.onboarding.step {
            return;
        }
        self.begin_user_segmented_control_transition_from(
            ONBOARDING_STEPS_MOTION_ID,
            self.onboarding.step,
            step,
            cx,
        );
        self.onboarding.step = step;
        self.close_settings_select();
        self.blur_text_inputs(cx);
        self.finish_settings_slider_drag(cx);
        self.onboarding.scroll_handle = ScrollHandle::new();
        if OnboardingStep::from_index(step) == OnboardingStep::Tools
            && self
                .settings_workspace
                .read(cx)
                .cli_companion_needs_refresh()
        {
            self.refresh_cli_companion_status(cx);
        }
        if OnboardingStep::from_index(step) == OnboardingStep::QuickStart
            && self.onboarding.host_count.is_none()
        {
            self.refresh_onboarding_ssh_host_count(cx);
        }
        cx.notify();
    }

    pub(in crate::workspace) fn onboarding_next(&mut self, cx: &mut Context<Self>) {
        if !self.onboarding.disclaimer_accepted {
            return;
        }
        if self.onboarding.step + 1 < ONBOARDING_TOTAL_STEPS {
            self.onboarding_go_to_step(self.onboarding.step + 1, cx);
        } else {
            self.complete_onboarding(cx);
        }
    }

    pub(in crate::workspace) fn onboarding_back(&mut self, cx: &mut Context<Self>) {
        if self.onboarding.step > 0 {
            self.onboarding_go_to_step(self.onboarding.step - 1, cx);
        }
    }

    pub(in crate::workspace) fn onboarding_skip_to_quick_start(&mut self, cx: &mut Context<Self>) {
        if self.onboarding.disclaimer_accepted {
            self.onboarding_go_to_step(ONBOARDING_TOTAL_STEPS - 1, cx);
        }
    }

    pub(in crate::workspace) fn close_onboarding_if_allowed(&mut self, cx: &mut Context<Self>) {
        if self.onboarding.disclaimer_accepted {
            self.complete_onboarding(cx);
        }
    }

    pub(in crate::workspace) fn handle_onboarding_key(
        &mut self,
        event: &KeyDownEvent,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.onboarding.open {
            return false;
        }
        if self.open_settings_select.is_some() {
            if !self.handle_appearance_theme_select_key(event, cx)
                && event.keystroke.key == "escape"
            {
                self.close_settings_select();
                cx.notify();
            }
            return true;
        }
        if self.focused_settings_input.is_some()
            || self
                .settings_workspace
                .read(cx)
                .settings_entity_focused_input()
                .is_some()
        {
            self.handle_settings_input_key(event, cx);
            return true;
        }
        match event.keystroke.key.as_str() {
            "escape" => self.close_onboarding_if_allowed(cx),
            "enter" => self.onboarding_next(cx),
            "left" | "arrowleft" => self.onboarding_back(cx),
            "right" | "arrowright" => self.onboarding_next(cx),
            _ => return false,
        }
        true
    }

    pub(in crate::workspace) fn refresh_onboarding_ssh_host_count(
        &mut self,
        cx: &mut Context<Self>,
    ) {
        let runtime = self.forwarding_runtime.clone();
        cx.spawn(async move |weak, cx| {
            let count = runtime
                .spawn_blocking(|| {
                    list_ssh_config_hosts(&HashSet::new())
                        .map(|hosts| hosts.into_iter().filter(|host| host.alias != "*").count())
                })
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result.map_err(|error| error.to_string()))
                .unwrap_or(0);
            let _ = weak.update(cx, |this, cx| {
                this.onboarding.host_count = Some(count);
                cx.notify();
            });
        })
        .detach();
    }

    pub(in crate::workspace) fn import_onboarding_ssh_hosts(&mut self, cx: &mut Context<Self>) {
        if self.onboarding.import_state != OnboardingImportState::Idle {
            return;
        }
        self.onboarding.import_state = OnboardingImportState::Loading;
        let existing_names = self
            .connection_store
            .connections()
            .iter()
            .map(|connection| connection.name.clone())
            .collect::<HashSet<_>>();
        let aliases = list_ssh_config_hosts(&existing_names)
            .map(|hosts| {
                hosts
                    .into_iter()
                    .filter(|host| host.alias != "*")
                    .map(|host| host.alias)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut imported = 0usize;
        for alias in aliases {
            if self
                .connection_store
                .connections()
                .iter()
                .any(|connection| connection.name == alias)
            {
                continue;
            }
            let Ok(Some(host)) = resolve_ssh_config_alias(&alias) else {
                continue;
            };
            let Ok(connection) = saved_connection_from_ssh_host(host) else {
                continue;
            };
            if self
                .connection_store
                .import_ssh_connection(connection)
                .is_ok()
            {
                imported += 1;
            }
        }
        let _ = self.connection_store.save();
        self.settings_workspace.update(cx, |settings, _cx| {
            settings.acknowledge_external_store_state()
        });
        self.onboarding.imported_count = imported;
        self.onboarding.import_state = OnboardingImportState::Done;
        self.onboarding.host_count = Some(imported);
        cx.notify();
    }

    pub(in crate::workspace) fn onboarding_open_terminal(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.complete_onboarding(cx);
        let _ = self.create_local_terminal_tab(window, cx);
    }

    pub(in crate::workspace) fn onboarding_open_new_connection(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.complete_onboarding(cx);
        self.open_new_connection_form(window, cx);
    }

    pub(in crate::workspace) fn onboarding_open_connection_importers(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.complete_onboarding(cx);
        self.open_connection_importers_settings(window, cx);
    }

    pub(in crate::workspace) fn onboarding_open_cli_settings(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.complete_onboarding(cx);
        self.settings_workspace.update(cx, |settings, cx| {
            settings.set_active_tab(SettingsTab::General, cx)
        });
        self.open_settings(window, cx);
    }
}
