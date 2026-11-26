use super::*;
use std::{cell::Cell, rc::Rc};
pub(in crate::workspace) const TERMINAL_SETTINGS_SWITCHER_ID: &str =
    "terminal-settings-page-switcher";

pub(in crate::workspace) const CLOUD_SYNC_SWITCHER_ID: &str = "cloud-sync-tab-bar";
pub(in crate::workspace) const PLUGIN_MANAGER_SWITCHER_ID: &str = "plugin-manager-tab-bar";
pub(in crate::workspace) const CONNECTION_RUNTIME_SWITCHER_ID: &str = "connection-runtime-tab-bar";
pub(in crate::workspace) const NOTIFICATION_CENTER_SWITCHER_ID: &str =
    "notification-center-tab-bar";
pub(in crate::workspace) const SETTINGS_NAVIGATION_ID: &str = "settings-navigation";
pub(in crate::workspace) const FILE_MANAGER_NAVIGATION_ID: &str = "file-manager-navigation";
pub(in crate::workspace) const NEW_CONNECTION_TRANSPORT_SELECTOR_ID: &str =
    "new-connection-transport-selector";
pub(in crate::workspace) const NEW_CONNECTION_AUTH_SELECTOR_ID: &str =
    "new-connection-auth-selector";
pub(in crate::workspace) const EDIT_CONNECTION_AUTH_SELECTOR_ID: &str =
    "edit-connection-auth-selector";
pub(in crate::workspace) const PROMPT_CONNECTION_AUTH_SELECTOR_ID: &str =
    "prompt-connection-auth-selector";
pub(in crate::workspace) const DRILL_DOWN_AUTH_SELECTOR_ID: &str = "drill-down-auth-selector";
pub(in crate::workspace) const JUMP_CONNECTION_AUTH_SELECTOR_ID: &str =
    "jump-connection-auth-selector";
pub(in crate::workspace) const VERSION_MIGRATION_MOTION_SWITCHER_ID: &str =
    "version-migration-motion-switcher";
pub(in crate::workspace) const APPEARANCE_BACKGROUND_SCOPE_SWITCHER_ID: &str =
    "appearance-background-scope-switcher";

#[derive(Default)]
pub(super) struct UserSegmentedControlMotionState {
    next_generation: u64,
    active_transitions: HashMap<&'static str, ActiveUserTransition>,
}

#[derive(Clone)]
struct ActiveUserTransition {
    generation: u64,
    target_index: usize,
    previous_index: Option<usize>,
    vertical_offset_y: Option<f32>,
    vertical_from: gpui::SpringState,
    vertical_position: Rc<Cell<gpui::SpringState>>,
}

impl UserSegmentedControlMotionState {
    fn begin_with_vertical_offset(
        &mut self,
        control_id: &'static str,
        target_index: usize,
        vertical_offset_y: Option<f32>,
    ) -> u64 {
        self.begin_with_previous_index(control_id, None, target_index, vertical_offset_y)
    }

    fn begin_with_previous_index(
        &mut self,
        control_id: &'static str,
        previous_index: Option<usize>,
        target_index: usize,
        vertical_offset_y: Option<f32>,
    ) -> u64 {
        // One monotonic counter prevents a cleared control from reusing a
        // generation while its older completion task is still pending.
        self.next_generation = self.next_generation.wrapping_add(1).max(1);
        let generation = self.next_generation;
        let previous = self
            .active_transitions
            .get(control_id)
            .map(|transition| transition.vertical_position.get());
        let vertical_from = gpui::SpringState {
            position: vertical_offset_y.unwrap_or(0.0)
                + previous.map_or(0.0, |state| state.position),
            velocity: previous.map_or(0.0, |state| state.velocity),
        };
        self.active_transitions.insert(
            control_id,
            ActiveUserTransition {
                generation,
                target_index,
                previous_index,
                vertical_offset_y,
                vertical_from,
                vertical_position: Rc::new(Cell::new(vertical_from)),
            },
        );
        generation
    }

    fn finish(&mut self, control_id: &'static str, generation: u64) -> bool {
        if self
            .active_transitions
            .get(control_id)
            .map(|transition| transition.generation)
            != Some(generation)
        {
            return false;
        }
        self.active_transitions.remove(control_id);
        true
    }

    pub(super) fn clear(&mut self, control_id: &'static str) {
        self.active_transitions.remove(control_id);
    }

    fn is_active_for(&self, control_id: &'static str, active_index: usize) -> bool {
        self.active_transitions
            .get(control_id)
            .is_some_and(|transition| transition.target_index == active_index)
    }

    fn transition_for(
        &self,
        control_id: &'static str,
        active_index: usize,
    ) -> Option<(u64, Option<f32>)> {
        self.active_transitions
            .get(control_id)
            .filter(|transition| transition.target_index == active_index)
            .map(|transition| (transition.generation, transition.vertical_offset_y))
    }

    fn previous_index_for(&self, control_id: &'static str, active_index: usize) -> Option<usize> {
        self.active_transitions
            .get(control_id)
            .filter(|transition| transition.target_index == active_index)
            .and_then(|transition| transition.previous_index)
    }
}

impl UserSegmentedControlMotionState {
    fn render_vertical(
        &self,
        control_id: &'static str,
        active_index: usize,
        tokens: &ThemeTokens,
        surface: gpui::Div,
    ) -> AnyElement {
        let Some(entry) = self
            .active_transitions
            .get(control_id)
            .filter(|entry| entry.target_index == active_index)
        else {
            return surface.into_any_element();
        };
        let Some(motion) = oxide_ui::segmented_control_motion(tokens) else {
            entry.vertical_position.set(gpui::SpringState {
                position: 0.0,
                velocity: 0.0,
            });
            return surface.into_any_element();
        };
        let id = gpui::ElementId::NamedInteger(control_id.into(), entry.generation);
        if !motion.spatial || entry.vertical_offset_y.is_none() {
            entry.vertical_position.set(gpui::SpringState {
                position: 0.0,
                velocity: 0.0,
            });
            return oxide_ui::motion::fade_in(
                tokens,
                id,
                surface,
                oxide_ui::motion::MotionDuration::Control,
            );
        }
        // Row reparenting changes the GPUI ID path. The workspace retains only
        // scalar position and velocity so the next row continues the same motion.
        let from = entry.vertical_from;
        let position = entry.vertical_position.clone();
        let config =
            oxide_ui::motion::spring_config(tokens, oxide_ui::motion::MotionDuration::Control);
        surface
            .with_animation(
                id,
                Animation::new(motion.duration),
                move |surface, progress| {
                    let state = if progress >= 1.0 {
                        gpui::SpringState {
                            position: 0.0,
                            velocity: 0.0,
                        }
                    } else {
                        config.step(from, 0.0, progress * motion.duration.as_secs_f32())
                    };
                    position.set(state);
                    surface.top(px(state.position)).bottom(px(-state.position))
                },
            )
            .into_any_element()
    }
}
impl WorkspaceApp {
    pub(in crate::workspace) fn render_vertical_selection_surface(
        &self,
        control_id: &'static str,
        active_index: usize,
        surface: gpui::Div,
    ) -> AnyElement {
        self.segmented_control_user_motion.render_vertical(
            control_id,
            active_index,
            &self.theme_tokens,
            surface,
        )
    }
    pub(in crate::workspace) fn begin_user_segmented_control_transition(
        &mut self,
        control_id: &'static str,
        target_index: usize,
        cx: &mut Context<Self>,
    ) {
        self.begin_user_segmented_control_transition_with_vertical_offset(
            control_id,
            target_index,
            None,
            cx,
        );
    }

    pub(in crate::workspace) fn begin_user_segmented_control_transition_from(
        &mut self,
        control_id: &'static str,
        previous_index: usize,
        target_index: usize,
        cx: &mut Context<Self>,
    ) {
        self.begin_user_segmented_control_transition_with_history(
            control_id,
            Some(previous_index),
            target_index,
            None,
            cx,
        );
    }

    pub(in crate::workspace) fn begin_user_segmented_control_transition_with_vertical_offset(
        &mut self,
        control_id: &'static str,
        target_index: usize,
        vertical_offset_y: Option<f32>,
        cx: &mut Context<Self>,
    ) {
        self.begin_user_segmented_control_transition_with_history(
            control_id,
            None,
            target_index,
            vertical_offset_y,
            cx,
        );
    }

    fn begin_user_segmented_control_transition_with_history(
        &mut self,
        control_id: &'static str,
        previous_index: Option<usize>,
        target_index: usize,
        vertical_offset_y: Option<f32>,
        cx: &mut Context<Self>,
    ) {
        let Some(motion) = oxide_ui::segmented_control_motion(&self.theme_tokens) else {
            self.segmented_control_user_motion.clear(control_id);
            return;
        };
        let generation = if previous_index.is_some() {
            self.segmented_control_user_motion
                .begin_with_previous_index(
                    control_id,
                    previous_index,
                    target_index,
                    vertical_offset_y,
                )
        } else {
            self.segmented_control_user_motion
                .begin_with_vertical_offset(control_id, target_index, vertical_offset_y)
        };
        // User intent outlives a virtual-list row only for the real transition,
        // then expires so remounts and programmatic navigation render settled.
        cx.spawn(async move |weak, cx| {
            Timer::after(motion.duration).await;
            let _ = weak.update(cx, |this, cx| {
                if this
                    .segmented_control_user_motion
                    .finish(control_id, generation)
                {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(in crate::workspace) fn segmented_control_user_transition_active(
        &self,
        control_id: &'static str,
        active_index: usize,
    ) -> bool {
        self.segmented_control_user_motion
            .is_active_for(control_id, active_index)
    }

    pub(in crate::workspace) fn segmented_control_user_transition(
        &self,
        control_id: &'static str,
        active_index: usize,
    ) -> Option<(u64, Option<f32>)> {
        self.segmented_control_user_motion
            .transition_for(control_id, active_index)
    }

    pub(in crate::workspace) fn segmented_control_user_previous_index(
        &self,
        control_id: &'static str,
        active_index: usize,
    ) -> Option<usize> {
        self.segmented_control_user_motion
            .previous_index_for(control_id, active_index)
    }
}
