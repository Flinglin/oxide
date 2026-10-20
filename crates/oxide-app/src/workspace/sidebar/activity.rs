use super::*;
use gpui::StatefulInteractiveElement;
use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
};

impl WorkspaceApp {
    pub(in crate::workspace) fn render_nav_rail(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme_tokens.ui;
        let top_items = [
            (NavItem::Sessions, LucideIcon::Link2),
            (NavItem::Connections, LucideIcon::LayoutList),
            (NavItem::Runtime, LucideIcon::Gauge),
            (NavItem::Extensions, LucideIcon::Puzzle),
            (NavItem::CloudSync, LucideIcon::Cloud),
            (NavItem::HostTools, LucideIcon::Wrench),
        ];
        let bottom_items = [
            (NavItem::Workspace, LucideIcon::Square),
            (NavItem::Files, LucideIcon::FolderOpen),
            (NavItem::Notifications, LucideIcon::Bell),
            (NavItem::Settings, LucideIcon::Settings),
        ];
        let mut bar = div()
            .w(px(self.theme_tokens.metrics.activity_bar_width))
            .h_full()
            .flex()
            .flex_col()
            .items_center()
            .border_r_1()
            .border_color(self.workspace_chrome_divider());

        bar = bar.child(
            div()
                .relative()
                .flex_none()
                .w_full()
                .h(px(self.theme_tokens.metrics.tabbar_height))
                .flex()
                .items_center()
                .justify_center()
                // Paint top chrome directly over the window background so it
                // matches the adjacent sidebar and workspace tab bars exactly.
                .bg(self.workspace_chrome_background(theme.bg))
                .border_b_1()
                .border_color(self.workspace_chrome_divider())
                .child(
                    div()
                        .id("activity-sidebar-toggle")
                        .size(px(self.theme_tokens.metrics.activity_icon_size))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(self.theme_tokens.radii.md))
                        .cursor_pointer()
                        // Keep the primary-sidebar toggle feedback aligned with its context-sidebar counterpart.
                        .hover(move |button| button.bg(rgb(theme.bg_hover)))
                        .child(Self::render_lucide_icon(
                            if self.left_sidebar_collapsed {
                                LucideIcon::PanelLeft
                            } else {
                                LucideIcon::PanelLeftClose
                            },
                            self.theme_tokens.metrics.sidebar_collapse_icon_size,
                            rgb(theme.text_muted),
                        ))
                        .on_mouse_move(cx.listener({
                            let label = self.i18n.t(if self.left_sidebar_collapsed {
                                "sidebar.actions.expand"
                            } else {
                                "sidebar.actions.collapse"
                            });
                            move |this, event: &MouseMoveEvent, _window, cx| {
                                this.queue_workspace_tooltip(
                                    "activity-sidebar-toggle",
                                    label.clone(),
                                    f32::from(event.position.x) + 12.0,
                                    f32::from(event.position.y) + 16.0,
                                    cx,
                                );
                            }
                        }))
                        .on_hover(cx.listener(|this, hovered: &bool, _window, cx| {
                            if !*hovered {
                                this.clear_workspace_tooltip("activity-sidebar-toggle", cx);
                            }
                        }))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _event, _window, cx| {
                                this.toggle_left_sidebar(cx);
                            }),
                        ),
                ),
        );

        let mut primary_items = div()
            .id("activity-primary-items")
            .min_h_0()
            .flex_1()
            .w_full()
            .pt(px(PRIMARY_SIDEBAR_CONTENT_TOP_INSET))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .items_center();
        for (section, icon) in top_items {
            primary_items = primary_items.child(self.render_nav_item(section, icon, cx));
        }
        let plugin_activity_items = self
            .plugin_entity
            .read(cx)
            .registry()
            .contributions()
            .runtime_activity_bar_items();
        let plugin_sidebar_panels = self
            .plugin_entity
            .read(cx)
            .registry()
            .contributions()
            .runtime_sidebar_panels();
        // The sessions footer owns the lock action while visible. Keep the rail
        // entry reachable when that footer is hidden or another panel is selected.
        if self.settings_store.settings().sidebar_ui.show_app_lock_icon
            && (self.left_sidebar_collapsed
                || self.effective_sidebar_panel_section() != NavItem::Sessions)
        {
            primary_items =
                primary_items.child(
                    div()
                        .mb(px(self.theme_tokens.metrics.activity_icon_gap))
                        .child(self.render_app_lock_button(
                            self.theme_tokens.metrics.activity_icon_size,
                            cx,
                        )),
                );
        }

        // Keep built-in entries stationary as plugins are enabled or disabled.
        if plugin_sidebar_panels
            .iter()
            .any(|panel| panel.position == "top")
            || plugin_activity_items
                .iter()
                .any(|item| item.position == "top")
        {
            primary_items = primary_items.child(
                div()
                    .flex_none()
                    .w(px(self.theme_tokens.metrics.divider_width))
                    .h(px(self.theme_tokens.metrics.divider_height))
                    .mb(px(self.theme_tokens.metrics.activity_icon_gap))
                    .bg(self.workspace_divider_color(theme.divider, 0xff)),
            );
        }
        for panel in plugin_sidebar_panels
            .iter()
            .filter(|panel| panel.position == "top")
            .cloned()
        {
            primary_items =
                primary_items.child(self.render_plugin_sidebar_activity_icon(panel, cx));
        }
        for item in plugin_activity_items
            .iter()
            .filter(|item| item.position == "top")
            .cloned()
        {
            primary_items = primary_items.child(self.render_plugin_activity_action_icon(item, cx));
        }

        let mut bottom = div().relative().flex().flex_col().items_center().child(
            div()
                .w(px(self.theme_tokens.metrics.divider_width))
                .h(px(self.theme_tokens.metrics.divider_height))
                .bg(self.workspace_divider_color(theme.divider, 0xff)),
        );
        for panel in plugin_sidebar_panels
            .into_iter()
            .filter(|panel| panel.position == "bottom")
        {
            bottom = bottom.child(self.render_plugin_sidebar_activity_icon(panel, cx));
        }
        for item in plugin_activity_items
            .into_iter()
            .filter(|item| item.position == "bottom")
        {
            bottom = bottom.child(self.render_plugin_activity_action_icon(item, cx));
        }
        bottom = bottom.children(
            bottom_items
                .into_iter()
                .map(|(section, icon)| self.render_nav_item(section, icon, cx)),
        );
        if let Some(popover) = self.render_detached_local_terminals_popover(cx) {
            bottom = bottom.child(popover);
        }

        bar.child(
            div()
                .flex_1()
                .min_h_0()
                .w_full()
                .pb_2()
                .flex()
                .flex_col()
                .items_center()
                // The content column keeps the lighter sidebar tint without
                // placing another translucent layer beneath the top chrome.
                .bg(self.workspace_sidebar_background(theme.bg))
                .child(primary_items)
                .child(bottom),
        )
        .into_any_element()
    }

    pub(in crate::workspace) fn render_nav_item(
        &self,
        section: NavItem,
        icon: LucideIcon,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = self.theme_tokens.ui;
        let active = match section {
            NavItem::Terminal => false,
            NavItem::Runtime => self
                .active_content_tab(cx)
                .is_some_and(|tab| tab.kind == TabKind::Runtime),
            NavItem::Network => {
                self.active_content_tab(cx)
                    .is_some_and(|tab| tab.kind == TabKind::Runtime)
                    && self.host_tools.read(cx).active_runtime_section
                        == ConnectionRuntimeSection::Topology
            }
            NavItem::Files => self
                .active_content_tab(cx)
                .is_some_and(|tab| tab.kind == TabKind::FileManager),
            NavItem::Notifications => self
                .active_content_tab(cx)
                .is_some_and(|tab| tab.kind == TabKind::NotificationCenter),
            NavItem::Extensions => self
                .active_content_tab(cx)
                .is_some_and(|tab| tab.kind == TabKind::PluginManager),
            NavItem::CloudSync => self
                .active_content_tab(cx)
                .is_some_and(|tab| tab.kind == TabKind::CloudSync),

            NavItem::Settings => self
                .active_content_tab(cx)
                .is_some_and(|tab| tab.kind == TabKind::Settings),

            NavItem::HostTools => {
                self.right_sidebar_visible()
                    && self.active_right_sidebar_panel == RightSidebarPanel::HostTools
            }
            _ => self.active_sidebar_section == section,
        };
        let tooltip = self.activity_icon_tooltip(section);
        let tooltip_id = format!("activity-icon-{}", section.as_settings_key());
        let tooltip_id_for_move = tooltip_id.clone();
        let badge_count = if section == NavItem::Notifications {
            let notification_count = if self.notification_center.notifications.dnd_enabled {
                0
            } else {
                self.notification_center.notifications.unread_count
            };
            notification_count
        } else if section == NavItem::Workspace {
            self.visible_local_terminal_session_count(cx)
                .saturating_add(self.detached_local_terminals.len())
                .min(u32::MAX as usize) as u32
        } else {
            0
        };
        let badge_is_error = section == NavItem::Notifications
            && !self.notification_center.notifications.dnd_enabled
            && self.notification_center.notifications.unread_critical_count > 0;
        let badge_color = if badge_is_error {
            theme.error
        } else if section == NavItem::Workspace && !self.detached_local_terminals.is_empty() {
            theme.warning
        } else {
            theme.accent
        };
        let badge_text_color = if badge_color == theme.accent {
            theme.accent_text
        } else {
            theme.bg
        };

        // Activity entries use the same static selected-card treatment as the
        // settings navigation while retaining the shared icon-button states.
        let button = oxide_ui::button::icon_button(
            &self.theme_tokens,
            Self::render_lucide_icon(
                icon,
                self.theme_tokens.metrics.activity_icon_glyph_size,
                rgb(if active { theme.accent } else { theme.text }),
            ),
            oxide_ui::button::IconButtonOptions {
                size: self.theme_tokens.metrics.activity_icon_size,
                radius: oxide_ui::button::ButtonRadius::Md,
                has_background: active,
                background: active.then(|| self.settings_panel_background(theme.bg_panel)),
                border: active.then(|| rgb(theme.border)),
                hover_background: Some(rgb(theme.bg_hover)),
                idle_opacity: 1.0,
                ..oxide_ui::button::IconButtonOptions::compact(
                    self.theme_tokens.metrics.activity_icon_size,
                )
            },
        );
        let button = if active {
            oxide_ui::theme_card_surface_shadow(button, &self.theme_tokens)
        } else {
            button
        };

        button
            .id(("activity-icon", section as u64))
            .relative()
            .mb(px(self.theme_tokens.metrics.activity_icon_gap))
            .when(badge_count > 0, |icon_el| {
                icon_el.child(
                    div()
                        .absolute()
                        // Tauri badges sit outside the `h-9 w-9` Button
                        // (`-top-1 -right-1`) so the active button chrome and
                        // numeric pill do not visually collide.
                        .right(px(-4.0))
                        .top(px(-4.0))
                        .min_w(px(14.0))
                        .h(px(14.0))
                        .px(px(3.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .bg(rgb(badge_color))
                        .text_color(rgb(badge_text_color))
                        .text_size(px(9.0))
                        .child(if badge_count > 99 {
                            "99+".to_string()
                        } else {
                            badge_count.to_string()
                        }),
                )
            })
            .on_mouse_move(cx.listener({
                move |this, event: &MouseMoveEvent, _window, cx| {
                    this.queue_workspace_tooltip(
                        tooltip_id_for_move.clone(),
                        tooltip.clone(),
                        f32::from(event.position.x) + 12.0,
                        f32::from(event.position.y) + 16.0,
                        cx,
                    );
                }
            }))
            .on_hover(cx.listener(move |this, hovered: &bool, _window, cx| {
                if !*hovered {
                    this.clear_workspace_tooltip(&tooltip_id, cx);
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _event, window, cx| match section {
                    NavItem::Connections => this.open_session_manager_tab(window, cx),
                    NavItem::Workspace => {
                        if !this.detached_local_terminals.is_empty() {
                            this.detached_local_terminals_popover_open =
                                !this.detached_local_terminals_popover_open;
                            cx.notify();
                        } else {
                            let _ = this.create_local_terminal_tab(window, cx);
                        }
                    }

                    NavItem::Settings => this.open_settings(window, cx),

                    NavItem::Terminal => this.open_connection_runtime_tab(
                        ConnectionRuntimeSection::Overview,
                        window,
                        cx,
                    ),
                    NavItem::Runtime => this.open_connection_runtime_tab(
                        ConnectionRuntimeSection::Overview,
                        window,
                        cx,
                    ),
                    NavItem::Network => this.open_topology_tab(window, cx),

                    NavItem::Files => this.open_file_manager_tab(window, cx),
                    NavItem::Notifications => this.open_notification_center_tab(window, cx),
                    NavItem::HostTools => {
                        let _ = this.toggle_right_sidebar_panel(RightSidebarPanel::HostTools, cx);
                    }
                    NavItem::CloudSync => this.open_cloud_sync_tab(window, cx),
                    NavItem::Extensions => this.open_plugin_manager_tab(window, cx),
                    _ => {
                        this.active_surface = ActiveSurface::Terminal;
                        this.toggle_sidebar_section(section, cx);
                    }
                }),
            )
            .into_any_element()
    }

    pub(in crate::workspace) fn activity_icon_tooltip(&self, section: NavItem) -> String {
        match section {
            NavItem::Sessions => self.i18n.t("sidebar.panels.sessions"),
            NavItem::Connections => self.i18n.t("sidebar.panels.open_session_manager"),
            NavItem::Forwards => self.i18n.t("forwards.table.title"),
            NavItem::Terminal => self.i18n.t("sidebar.panels.runtime_overview"),
            NavItem::Runtime => self.i18n.t("sidebar.panels.runtime"),
            NavItem::Network => self.i18n.t("sidebar.panels.connection_matrix"),
            NavItem::Extensions => self.i18n.t("sidebar.panels.plugins"),
            NavItem::CloudSync => self.i18n.t("plugin.cloud_sync.panel_title"),

            NavItem::HostTools => self.i18n.t("sidebar.panels.host_tools"),
            NavItem::Automation => self.i18n.t("sidebar.panels.activity"),
            NavItem::Workspace => self.i18n.t("sidebar.actions.new_local_terminal"),
            NavItem::Files => self.i18n.t("sidebar.panels.files"),
            NavItem::Notifications => self.i18n.t("sidebar.panels.notifications"),
            NavItem::Settings => self.i18n.t("sidebar.tooltips.settings"),
        }
    }

    pub(in crate::workspace) fn render_plugin_sidebar_activity_icon(
        &self,
        panel: plugin_host::NativePluginRuntimeSidebarPanelContribution,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = self.theme_tokens.ui;
        let selection = plugin_ui::NativePluginSidebarPanelSelection {
            plugin_id: panel.plugin_id.clone(),
            panel_id: panel.panel_id.clone(),
        };
        let active = self.active_sidebar_section == NavItem::Extensions
            && self
                .plugin_manager_state(cx)
                .active_sidebar_panel
                .as_ref()
                .is_some_and(|active_panel| active_panel == &selection);
        let tooltip = panel.title.clone();
        let tooltip_id = format!("activity-plugin-{}-{}", panel.plugin_id, panel.panel_id);
        let tooltip_id_for_move = tooltip_id.clone();
        let icon = LucideIcon::from_plugin_name(&panel.icon);

        let button = oxide_ui::button::icon_button(
            &self.theme_tokens,
            Self::render_lucide_icon(
                icon,
                self.theme_tokens.metrics.activity_icon_glyph_size,
                rgb(if active { theme.accent } else { theme.text }),
            ),
            oxide_ui::button::IconButtonOptions {
                size: self.theme_tokens.metrics.activity_icon_size,
                radius: oxide_ui::button::ButtonRadius::Md,
                has_background: active,
                background: active.then(|| self.settings_panel_background(theme.bg_panel)),
                border: active.then(|| rgb(theme.border)),
                hover_background: Some(rgb(theme.bg_hover)),
                idle_opacity: 1.0,
                ..oxide_ui::button::IconButtonOptions::compact(
                    self.theme_tokens.metrics.activity_icon_size,
                )
            },
        );
        let button = if active {
            oxide_ui::theme_card_surface_shadow(button, &self.theme_tokens)
        } else {
            button
        };

        button
            .id((
                "activity-plugin-icon",
                native_plugin_sidebar_activity_id(&panel),
            ))
            .relative()
            .mb(px(self.theme_tokens.metrics.activity_icon_gap))
            .on_mouse_move(cx.listener({
                move |this, event: &MouseMoveEvent, _window, cx| {
                    this.queue_workspace_tooltip(
                        tooltip_id_for_move.clone(),
                        tooltip.clone(),
                        f32::from(event.position.x) + 12.0,
                        f32::from(event.position.y) + 16.0,
                        cx,
                    );
                }
            }))
            .on_hover(cx.listener(move |this, hovered: &bool, _window, cx| {
                if !*hovered {
                    this.clear_workspace_tooltip(&tooltip_id, cx);
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _event, _window, cx| {
                    let requested_panel_is_visible = !this.left_sidebar_collapsed
                        && this.active_sidebar_section == NavItem::Extensions
                        && this
                            .plugin_manager_state(cx)
                            .active_sidebar_panel
                            .as_ref()
                            .is_some_and(|active_panel| active_panel == &selection);
                    if requested_panel_is_visible {
                        this.toggle_left_sidebar(cx);
                        cx.stop_propagation();
                        return;
                    }
                    // Mirrors Tauri's `sidebarActiveSection = "plugin:<id>:<panel>"`
                    // path: choosing a plugin panel switches only the sidebar
                    // content, while Plugin Manager remains a separate tab.
                    this.plugin_entity.update(cx, |plugins, _cx| {
                        plugins.select_sidebar_panel(selection.clone());
                    });
                    this.active_surface = ActiveSurface::Terminal;
                    this.set_sidebar_section(NavItem::Extensions, cx);
                    cx.stop_propagation();
                }),
            )
            .into_any_element()
    }

    pub(in crate::workspace) fn render_plugin_activity_action_icon(
        &self,
        item: plugin_host::NativePluginRuntimeActivityBarItemContribution,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = self.theme_tokens.ui;
        let tooltip_id = format!("activity-plugin-action-{}-{}", item.plugin_id, item.item_id);
        let tooltip_id_for_move = tooltip_id.clone();
        let tooltip = item.title.clone();
        let plugin_id = item.plugin_id.clone();
        let command = item.command.clone();
        let icon = LucideIcon::from_plugin_name(&item.icon);
        let button = oxide_ui::button::icon_button(
            &self.theme_tokens,
            Self::render_lucide_icon(
                icon,
                self.theme_tokens.metrics.activity_icon_glyph_size,
                rgb(theme.text),
            ),
            oxide_ui::button::IconButtonOptions {
                size: self.theme_tokens.metrics.activity_icon_size,
                radius: oxide_ui::button::ButtonRadius::Md,
                hover_background: Some(rgb(theme.bg_hover)),
                idle_opacity: 1.0,
                ..oxide_ui::button::IconButtonOptions::compact(
                    self.theme_tokens.metrics.activity_icon_size,
                )
            },
        );

        button
            .id((
                "activity-plugin-action",
                native_plugin_activity_bar_item_id(&item),
            ))
            .relative()
            .mb(px(self.theme_tokens.metrics.activity_icon_gap))
            .on_mouse_move(
                cx.listener(move |this, event: &MouseMoveEvent, _window, cx| {
                    this.queue_workspace_tooltip(
                        tooltip_id_for_move.clone(),
                        tooltip.clone(),
                        f32::from(event.position.x) + 12.0,
                        f32::from(event.position.y) + 16.0,
                        cx,
                    );
                }),
            )
            .on_hover(cx.listener(move |this, hovered: &bool, _window, cx| {
                if !*hovered {
                    this.clear_workspace_tooltip(&tooltip_id, cx);
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _event, _window, cx| {
                    // Action items invoke only the manifest-declared command
                    // associated with their host-owned runtime registration.
                    this.dispatch_native_plugin_command(plugin_id.clone(), command.clone(), cx);
                    cx.stop_propagation();
                }),
            )
            .into_any_element()
    }

    pub(in crate::workspace) fn render_lucide_icon(
        icon: LucideIcon,
        size: f32,
        color: Rgba,
    ) -> AnyElement {
        svg()
            .path(icon.path())
            .size(px(size))
            .text_color(color)
            .into_any_element()
    }

    pub(in crate::workspace) fn render_animated_chevron(
        &self,
        id: impl Into<gpui::ElementId>,
        expanded: bool,
        size: f32,
        color: Rgba,
    ) -> AnyElement {
        // A single right-facing asset keeps state changes continuous instead
        // of swapping two unrelated SVG trees between renders.
        oxide_ui::motion::animated_chevron(
            &self.theme_tokens,
            id,
            svg()
                .path(LucideIcon::ChevronRight.path())
                .size(px(size))
                .text_color(color),
            expanded,
        )
    }

    pub(in crate::workspace) fn render_loading_icon(
        &self,
        id: impl Into<gpui::ElementId>,
        size: f32,
        color: Rgba,
    ) -> AnyElement {
        // Spinner frames are requested only while callers render a genuine
        // loading state; static status icons continue using render_lucide_icon.
        oxide_ui::motion::animated_spinner(
            &self.theme_tokens,
            id,
            svg()
                .path(LucideIcon::LoaderCircle.path())
                .size(px(size))
                .text_color(color),
        )
    }
}

pub(in crate::workspace) fn native_plugin_sidebar_activity_id(
    panel: &plugin_host::NativePluginRuntimeSidebarPanelContribution,
) -> u64 {
    let mut hasher = DefaultHasher::new();
    panel.registration_id.hash(&mut hasher);
    panel.plugin_id.hash(&mut hasher);
    panel.panel_id.hash(&mut hasher);
    hasher.finish()
}

pub(in crate::workspace) fn native_plugin_activity_bar_item_id(
    item: &plugin_host::NativePluginRuntimeActivityBarItemContribution,
) -> u64 {
    let mut hasher = DefaultHasher::new();
    item.registration_id.hash(&mut hasher);
    item.plugin_id.hash(&mut hasher);
    item.item_id.hash(&mut hasher);
    hasher.finish()
}
