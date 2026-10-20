use super::*;

const SIDEBAR_VIEWPORT_MIN_WIDTH_RATIO: f32 = 0.16;
const SIDEBAR_VIEWPORT_MAX_WIDTH_RATIO: f32 = 0.45;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::workspace) struct ResponsiveSidebarWidthBounds {
    pub min: f32,
    pub max: f32,
}

pub(in crate::workspace) fn responsive_sidebar_width_bounds(
    viewport_width: f32,
    absolute_min_width: f32,
    absolute_max_width: f32,
) -> ResponsiveSidebarWidthBounds {
    // Relative limits let wide windows use their available space, while the
    // absolute limits preserve usable controls on ordinary and compact windows.
    let viewport_width = viewport_width.max(0.0);
    let min = absolute_min_width.max(viewport_width * SIDEBAR_VIEWPORT_MIN_WIDTH_RATIO);
    let max = absolute_max_width
        .max(viewport_width * SIDEBAR_VIEWPORT_MAX_WIDTH_RATIO)
        .min(viewport_width.max(min))
        .max(min);
    ResponsiveSidebarWidthBounds { min, max }
}

pub(in crate::workspace) fn clamp_responsive_sidebar_width(
    width: f32,
    viewport_width: f32,
    absolute_min_width: f32,
    absolute_max_width: f32,
) -> f32 {
    let bounds =
        responsive_sidebar_width_bounds(viewport_width, absolute_min_width, absolute_max_width);
    width.clamp(bounds.min, bounds.max)
}

fn should_collapse_right_sidebar_panel(
    sidebar_visible: bool,
    active_panel: RightSidebarPanel,
    requested_panel: RightSidebarPanel,
) -> bool {
    sidebar_visible && active_panel == requested_panel
}

fn should_collapse_primary_sidebar_section(
    sidebar_collapsed: bool,
    visible_section: NavItem,
    requested_section: NavItem,
) -> bool {
    !sidebar_collapsed && visible_section == requested_section
}

pub(in crate::workspace) fn right_sidebar_panel_visible(
    sidebar_collapsed: bool,
    zen_mode: bool,
    active_panel: RightSidebarPanel,
) -> bool {
    if sidebar_collapsed || zen_mode {
        return false;
    }

    // Host Tools shares the companion sidebar shell, but its visibility must
    // remain independent from the optional AI feature.
    match active_panel {
        RightSidebarPanel::HostTools => true,
    }
}

impl WorkspaceApp {
    pub(in crate::workspace) fn set_sidebar_collapsed_with_motion(
        &mut self,
        collapsed: bool,
        cx: &mut Context<Self>,
    ) {
        self.left_sidebar_motion.retarget(if collapsed {
            0.0
        } else {
            self.sidebar_panel_width()
        });
        self.left_sidebar_collapsed = collapsed;
        self.left_sidebar_motion_generation = self.left_sidebar_motion_generation.wrapping_add(1);
        let generation = self.left_sidebar_motion_generation;
        if !collapsed {
            self.left_sidebar_rendered = true;
            return;
        }
        if !self.theme_tokens.motion.enabled || !self.theme_tokens.motion.spatial_enabled {
            self.left_sidebar_motion.settle(0.0);
            self.left_sidebar_rendered = false;
            return;
        }
        let delay = oxide_ui::motion::duration(
            &self.theme_tokens,
            oxide_ui::motion::MotionDuration::Control,
        );
        // Keep the panel mounted until its closing transition completes.
        cx.spawn(async move |weak, cx| {
            Timer::after(delay).await;
            let _ = weak.update(cx, |this, cx| {
                if this.left_sidebar_collapsed && this.left_sidebar_motion_generation == generation
                {
                    this.left_sidebar_rendered = false;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn set_right_sidebar_rendered_with_motion(&mut self, visible: bool, cx: &mut Context<Self>) {
        self.right_sidebar_motion.retarget(if visible {
            self.right_sidebar_width
        } else {
            0.0
        });
        self.right_sidebar_motion_generation = self.right_sidebar_motion_generation.wrapping_add(1);
        let generation = self.right_sidebar_motion_generation;
        if visible {
            self.right_sidebar_rendered = true;
            return;
        }
        if !self.theme_tokens.motion.enabled || !self.theme_tokens.motion.spatial_enabled {
            self.right_sidebar_motion.settle(0.0);
            self.right_sidebar_rendered = false;
            return;
        }
        let delay = oxide_ui::motion::duration(
            &self.theme_tokens,
            oxide_ui::motion::MotionDuration::Control,
        );
        // Delayed unmount makes the right sidebar's collapse animation observable.
        cx.spawn(async move |weak, cx| {
            Timer::after(delay).await;
            let _ = weak.update(cx, |this, cx| {
                if !this.right_sidebar_visible()
                    && this.right_sidebar_motion_generation == generation
                {
                    this.right_sidebar_rendered = false;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(in crate::workspace) fn persist_sidebar_settings(&mut self, cx: &mut Context<Self>) {
        self.settings_store
            .settings_mut()
            .sidebar_ui
            .left_sidebar_collapsed = self.left_sidebar_collapsed;
        self.settings_store
            .settings_mut()
            .sidebar_ui
            .left_sidebar_width = self.left_sidebar_width.round() as i64;
        self.settings_store.settings_mut().sidebar_ui.active_section = self
            .effective_sidebar_panel_section()
            .as_settings_key()
            .to_string();
        self.persist_sidebar_settings_store(cx);
    }

    fn persist_sidebar_settings_store(&mut self, cx: &mut Context<Self>) {
        if self.settings_store.save().is_ok() {
            // Internal writes advance the Entity-owned watcher before its next tick.
            self.settings_workspace.update(cx, |settings, _cx| {
                settings.acknowledge_external_store_state()
            });
        }
    }

    pub(in crate::workspace) fn right_sidebar_visible(&self) -> bool {
        let settings = self.settings_store.settings();
        right_sidebar_panel_visible(
            settings.sidebar_ui.right_sidebar_collapsed,
            settings.sidebar_ui.zen_mode,
            self.active_right_sidebar_panel,
        )
    }

    pub(in crate::workspace) fn set_sidebar_section(
        &mut self,
        section: NavItem,
        cx: &mut Context<Self>,
    ) {
        self.active_sidebar_section = section;
        if section == NavItem::Extensions {
            self.bootstrap_native_plugin_runtime(cx);
        }
        if self.left_sidebar_collapsed {
            self.set_sidebar_collapsed_with_motion(false, cx);
        }
        if section == NavItem::Sessions {
            self.activate_embedded_sftp_sidebar_if_visible(cx);
        }
        self.persist_sidebar_settings(cx);
        cx.notify();
    }

    pub(in crate::workspace) fn toggle_sidebar_section(
        &mut self,
        section: NavItem,
        cx: &mut Context<Self>,
    ) {
        // Activity-bar panel buttons are symmetric toggles: selecting another
        // panel opens it, while selecting the visible panel hides the sidebar.
        if should_collapse_primary_sidebar_section(
            self.left_sidebar_collapsed,
            self.effective_sidebar_panel_section(),
            section,
        ) {
            self.toggle_left_sidebar(cx);
        } else {
            self.set_sidebar_section(section, cx);
        }
    }

    pub(in crate::workspace) fn toggle_left_sidebar(&mut self, cx: &mut Context<Self>) {
        self.set_sidebar_collapsed_with_motion(!self.left_sidebar_collapsed, cx);
        self.left_sidebar_resizing = false;
        self.sidebar_resize_hotzone_hovered = false;
        self.persist_sidebar_settings(cx);
        cx.notify();
    }

    pub(in crate::workspace) fn sidebar_panel_width(&self) -> f32 {
        (self.left_sidebar_width - self.theme_tokens.metrics.activity_bar_width).max(0.0)
    }

    pub(in crate::workspace) fn set_sidebar_width(
        &mut self,
        width: f32,
        viewport_width: f32,
        cx: &mut Context<Self>,
    ) -> bool {
        let next_width = clamp_responsive_sidebar_width(
            width,
            viewport_width,
            self.theme_tokens.metrics.sidebar_min_width,
            self.theme_tokens.metrics.sidebar_max_width,
        );
        if (next_width - self.left_sidebar_width).abs() < f32::EPSILON {
            return false;
        }
        // Resize mousemove is a high-frequency root-capture path. Repaint only
        // when the clamped browser-style sidebar width actually changes.
        self.left_sidebar_width = next_width;
        self.left_sidebar_motion.settle(self.sidebar_panel_width());
        cx.notify();
        true
    }

    pub(in crate::workspace) fn start_sidebar_resize(
        &mut self,
        event: &MouseDownEvent,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let was_resizing = self.left_sidebar_resizing;
        self.left_sidebar_motion_generation = self.left_sidebar_motion_generation.wrapping_add(1);
        self.left_sidebar_resizing = true;
        let viewport_width = f32::from(window.viewport_size().width);
        let width_changed = self.set_sidebar_width(
            self.sidebar_width_from_cursor(event.position.x, window),
            viewport_width,
            cx,
        );
        self.left_sidebar_motion.settle(self.sidebar_panel_width());
        if !was_resizing && !width_changed {
            cx.notify();
        }
    }

    pub(in crate::workspace) fn update_sidebar_resize(
        &mut self,
        event: &MouseMoveEvent,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if !self.left_sidebar_resizing {
            return;
        }
        if !event.dragging() {
            // Browser resize handles release as soon as the platform reports
            // the button is no longer down, even if GPUI missed mouse-up.
            self.finish_left_sidebar_resize(cx);
            return;
        }
        // Match the AI sidebar: root-level movement owns the captured drag,
        // and the visible width is derived from the current window cursor.
        self.set_sidebar_width(
            self.sidebar_width_from_cursor(event.position.x, window),
            f32::from(window.viewport_size().width),
            cx,
        );
    }

    pub(in crate::workspace) fn finish_left_sidebar_resize(&mut self, cx: &mut Context<Self>) {
        if self.left_sidebar_resizing {
            self.left_sidebar_resizing = false;
            self.persist_sidebar_settings(cx);
            cx.notify();
        }
    }

    pub(in crate::workspace) fn start_embedded_sftp_sidebar_resize(
        &mut self,
        event: &MouseDownEvent,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        self.embedded_sftp_sidebar_resizing = true;
        self.set_embedded_sftp_sidebar_fraction(event.position.y, window, cx);
    }

    pub(in crate::workspace) fn update_embedded_sftp_sidebar_resize(
        &mut self,
        event: &MouseMoveEvent,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if !self.embedded_sftp_sidebar_resizing {
            return;
        }
        if !event.dragging() {
            // End capture when the platform no longer reports a pressed mouse
            // button, even if the splitter missed the corresponding mouse-up.
            self.finish_embedded_sftp_sidebar_resize(cx);
            return;
        }
        self.set_embedded_sftp_sidebar_fraction(event.position.y, window, cx);
    }

    pub(in crate::workspace) fn finish_embedded_sftp_sidebar_resize(
        &mut self,
        cx: &mut Context<Self>,
    ) {
        if !self.embedded_sftp_sidebar_resizing {
            return;
        }
        self.embedded_sftp_sidebar_resizing = false;
        self.persist_sidebar_settings_store(cx);
        cx.notify();
    }

    fn set_embedded_sftp_sidebar_fraction(
        &mut self,
        cursor_y: Pixels,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        // The split lives below the native/custom titlebar and the fixed
        // primary-sidebar header, so convert the window cursor into that body.
        let body_top =
            self.window_titlebar_height(window) + self.theme_tokens.metrics.tabbar_height;
        let body_height = (f32::from(window.viewport_size().height) - body_top).max(1.0);
        let fraction = ((f32::from(cursor_y) - body_top) / body_height).clamp(
            EMBEDDED_SFTP_MIN_SESSION_FRACTION,
            EMBEDDED_SFTP_MAX_SESSION_FRACTION,
        );
        let current = self.settings_store.settings().sftp.sidebar_session_fraction;
        if (current - fraction).abs() < f32::EPSILON {
            return;
        }
        self.settings_store
            .settings_mut()
            .sftp
            .sidebar_session_fraction = fraction;
        cx.notify();
    }

    pub(in crate::workspace) fn sidebar_width_from_cursor(
        &self,
        cursor_x: Pixels,
        window: &Window,
    ) -> f32 {
        let viewport_width = f32::from(window.viewport_size().width);
        clamp_responsive_sidebar_width(
            f32::from(cursor_x),
            viewport_width,
            self.theme_tokens.metrics.sidebar_min_width,
            self.theme_tokens.metrics.sidebar_max_width,
        )
    }
    pub(in crate::workspace) fn toggle_host_tool_sidebar(
        &mut self,
        cx: &mut Context<Self>,
    ) -> bool {
        self.toggle_right_sidebar_panel(RightSidebarPanel::HostTools, cx)
    }
    pub(in crate::workspace) fn toggle_right_sidebar_panel(
        &mut self,
        panel: RightSidebarPanel,
        cx: &mut Context<Self>,
    ) -> bool {
        // Clicking the currently visible context panel mirrors an ordinary toggle.
        if should_collapse_right_sidebar_panel(
            self.right_sidebar_visible(),
            self.active_right_sidebar_panel,
            panel,
        ) {
            self.collapse_right_sidebar(cx);
            return true;
        }
        self.open_right_sidebar_panel(panel, cx)
    }

    pub(in crate::workspace) fn open_right_sidebar_panel(
        &mut self,
        panel: RightSidebarPanel,
        cx: &mut Context<Self>,
    ) -> bool {
        self.active_right_sidebar_panel = panel;
        self.settings_store
            .settings_mut()
            .sidebar_ui
            .right_sidebar_collapsed = false;
        self.set_right_sidebar_rendered_with_motion(true, cx);
        if panel == RightSidebarPanel::HostTools {
            // Non-AI context panels share the old right-sidebar shell, but must
            // not keep AI-specific focus or floating popovers alive.

            self.host_tools.update(cx, |host_tools, cx| {
                host_tools.reset_active_tool(cx);
            });
        }
        self.sync_host_tools_lifecycle(panel == RightSidebarPanel::HostTools, cx);

        self.persist_sidebar_settings_store(cx);
        cx.notify();
        true
    }

    pub(in crate::workspace) fn collapse_right_sidebar(&mut self, cx: &mut Context<Self>) {
        self.settings_store
            .settings_mut()
            .sidebar_ui
            .right_sidebar_collapsed = true;
        self.set_right_sidebar_rendered_with_motion(false, cx);

        self.right_sidebar_resizing = false;

        self.sidebar_resize_hotzone_hovered = false;
        self.sync_host_tools_lifecycle(false, cx);
        self.persist_sidebar_settings_store(cx);
        cx.notify();
    }

    pub(in crate::workspace) fn set_right_sidebar_width(
        &mut self,
        width: f32,
        viewport_width: f32,
        cx: &mut Context<Self>,
    ) -> bool {
        let next_width = clamp_responsive_sidebar_width(
            width,
            viewport_width,
            RIGHT_SIDEBAR_ABSOLUTE_MIN_WIDTH,
            RIGHT_SIDEBAR_ABSOLUTE_MAX_WIDTH,
        );
        if (next_width - self.right_sidebar_width).abs() < f32::EPSILON {
            return false;
        }
        // Same repaint contract as the main sidebar: pointer capture may keep
        // sending moves after the width is clamped at a boundary.
        self.right_sidebar_width = next_width;
        self.right_sidebar_motion.settle(next_width);
        cx.notify();
        true
    }

    pub(in crate::workspace) fn start_right_sidebar_resize(
        &mut self,
        event: &MouseDownEvent,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let was_resizing = self.right_sidebar_resizing;
        self.right_sidebar_motion_generation = self.right_sidebar_motion_generation.wrapping_add(1);
        self.right_sidebar_resizing = true;

        // Mirror the browser sidebar: the first press updates the width from
        // the pointer position so a resize drag is visible before the next move.
        let width_changed = self.set_right_sidebar_width(
            self.right_sidebar_width_from_cursor(event.position.x, window),
            f32::from(window.viewport_size().width),
            cx,
        );
        self.right_sidebar_motion.settle(self.right_sidebar_width);
        if !was_resizing && !width_changed {
            cx.notify();
        }
    }

    pub(in crate::workspace) fn update_right_sidebar_resize(
        &mut self,
        event: &MouseMoveEvent,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if !self.right_sidebar_resizing {
            return;
        }
        if !event.dragging() {
            // Keep both sidebars on the same release contract: a missed
            // mouse-up cannot leave the resize state latched.
            self.finish_right_sidebar_resize(cx);
            return;
        }
        // Continue from the root capture even after the pointer leaves the AI
        // sidebar edge, matching browser resize handles.
        self.set_right_sidebar_width(
            self.right_sidebar_width_from_cursor(event.position.x, window),
            f32::from(window.viewport_size().width),
            cx,
        );
    }

    pub(in crate::workspace) fn finish_right_sidebar_resize(&mut self, cx: &mut Context<Self>) {
        if self.right_sidebar_resizing {
            let sidebar_width = {
                self.right_sidebar_resizing = false;
                self.right_sidebar_width
            };
            self.settings_store
                .settings_mut()
                .sidebar_ui
                .right_sidebar_width = sidebar_width.round() as i64;
            self.persist_sidebar_settings_store(cx);
            cx.notify();
        }
    }

    pub(in crate::workspace) fn right_sidebar_width_from_cursor(
        &self,
        cursor_x: Pixels,
        window: &Window,
    ) -> f32 {
        // Pointer events use the current drawable area's coordinate space.
        // On Windows, inner_window_bounds() reports the restore bounds while
        // maximized, which would clamp every resize update to the same limit.
        let viewport_width = f32::from(window.viewport_size().width);
        right_sidebar_width_from_cursor_value(f32::from(cursor_x), viewport_width)
    }

    pub(in crate::workspace) fn clamp_sidebar_widths_to_viewport(
        &mut self,
        viewport_width: f32,
        cx: &mut Context<Self>,
    ) {
        let primary_width = clamp_responsive_sidebar_width(
            self.left_sidebar_width,
            viewport_width,
            self.theme_tokens.metrics.sidebar_min_width,
            self.theme_tokens.metrics.sidebar_max_width,
        );
        let context_width = clamp_responsive_sidebar_width(
            self.right_sidebar_width,
            viewport_width,
            RIGHT_SIDEBAR_ABSOLUTE_MIN_WIDTH,
            RIGHT_SIDEBAR_ABSOLUTE_MAX_WIDTH,
        );
        let primary_changed = (primary_width - self.left_sidebar_width).abs() >= f32::EPSILON;
        let context_changed = (context_width - self.right_sidebar_width).abs() >= f32::EPSILON;
        if !primary_changed && !context_changed {
            return;
        }
        // Window resizes update effective widths without persisting a synthetic
        // user resize; persistence remains owned by completed drag gestures.
        self.left_sidebar_width = primary_width;
        if primary_changed {
            self.left_sidebar_motion
                .settle(if self.left_sidebar_collapsed {
                    0.0
                } else {
                    self.sidebar_panel_width()
                });
        }
        if context_changed {
            self.right_sidebar_width = context_width;

            self.right_sidebar_motion
                .settle(if self.right_sidebar_visible() {
                    context_width
                } else {
                    0.0
                });
        }
        cx.notify();
    }
}

pub(in crate::workspace) fn right_sidebar_width_from_cursor_value(
    cursor_x: f32,
    viewport_width: f32,
) -> f32 {
    // The context sidebar is anchored to the right edge, so dragging left must
    // increase width and dragging right must decrease width. Keep this math in
    // a pure helper so regressions do not require constructing a GPUI Window.
    clamp_responsive_sidebar_width(
        viewport_width - cursor_x,
        viewport_width,
        RIGHT_SIDEBAR_ABSOLUTE_MIN_WIDTH,
        RIGHT_SIDEBAR_ABSOLUTE_MAX_WIDTH,
    )
}
