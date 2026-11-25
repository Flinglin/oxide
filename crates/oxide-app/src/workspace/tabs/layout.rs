use super::*;
use gpui::{TextRun, font};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum TabStripItemKind {
    Live,
    Returning,
    Exiting(usize),
}

pub(super) struct TabStripItem {
    pub tab_id: TabId,
    pub width: f32,
    pub kind: TabStripItemKind,
}

pub(super) struct TabStripLayout {
    pub items: Vec<TabStripItem>,
    leading: f32,
}

impl TabStripLayout {
    pub fn return_index(&self) -> Option<usize> {
        let mut index = 0;
        for item in &self.items {
            match item.kind {
                TabStripItemKind::Returning => return Some(index),
                TabStripItemKind::Live => index += 1,
                TabStripItemKind::Exiting(_) => {}
            }
        }
        None
    }

    pub fn bounds(&self, tab_id: TabId) -> Option<std::ops::Range<f32>> {
        let mut left = self.leading;
        for item in &self.items {
            if item.tab_id == tab_id {
                return Some(left..left + item.width);
            }
            left += item.width;
        }
        None
    }

    pub fn content_width(&self) -> f32 {
        self.leading + self.items.iter().map(|item| item.width).sum::<f32>()
    }

    pub fn live_widths(&self) -> Vec<f32> {
        self.items
            .iter()
            .filter(|item| item.kind == TabStripItemKind::Live)
            .map(|item| item.width)
            .collect()
    }

    pub fn insertion_index(&self, pointer_x: f32) -> usize {
        let mut left = self.leading;
        let mut index = 0;
        for item in &self.items {
            if item.kind == TabStripItemKind::Live {
                if pointer_x < left + item.width / 2.0 {
                    return index;
                }
                index += 1;
            }
            left += item.width;
        }
        index
    }
}

impl WorkspaceApp {
    pub(super) fn tab_strip_layout(&self, window: &Window, cx: &App) -> TabStripLayout {
        let mut layout = TabStripLayout {
            items: self
                .main_tabs(cx)
                .map(|(_, tab)| TabStripItem {
                    tab_id: tab.id,
                    width: self.tab_natural_width(tab, window, cx),
                    kind: TabStripItemKind::Live,
                })
                .collect(),
            leading: self.theme_tokens.metrics.tabbar_leading_offset,
        };
        let returning = self.detached_tab_return_drag.filter(|drag| {
            drag.active
                && self.main_window_tabbar_drop_bounds.is_some_and(|bounds| {
                    bounds.contains(&gpui::point(
                        px(drag.current_screen_x),
                        px(drag.current_screen_y),
                    ))
                })
        });
        if let Some(tab) = returning.and_then(|drag| self.tab_by_id(drag.tab_id, cx)) {
            layout.items.push(TabStripItem {
                tab_id: tab.id,
                width: self.tab_natural_width(tab, window, cx),
                kind: TabStripItemKind::Returning,
            });
        }
        let mut exits = self
            .main_window_tabs
            .exiting_tabs
            .iter()
            .enumerate()
            .map(|(index, tab)| {
                let width = if self.theme_tokens.motion.spatial_enabled {
                    tab.width * tab.exit_visibility(&self.theme_tokens)
                } else {
                    tab.width
                };
                (index, tab, width)
            })
            .collect::<Vec<_>>();
        exits.sort_by_key(|(_, tab, _)| tab.visual_index);
        for (index, exiting, width) in exits {
            layout.items.insert(
                exiting.visual_index.min(layout.items.len()),
                TabStripItem {
                    tab_id: exiting.tab_id,
                    width,
                    kind: TabStripItemKind::Exiting(index),
                },
            );
        }
        if let Some(drag) = returning
            && let Some(index) = layout
                .items
                .iter()
                .position(|item| item.kind == TabStripItemKind::Returning)
        {
            let item = layout.items.remove(index);
            let bounds = self.main_window_tabbar_drop_bounds.unwrap();
            let x = drag.current_screen_x
                - f32::from(bounds.origin.x)
                - f32::from(self.main_window_tabs.scroll_handle.offset().x);
            let target = layout.insertion_index(x);
            let slot = layout
                .items
                .iter()
                .enumerate()
                .filter(|(_, item)| item.kind == TabStripItemKind::Live)
                .nth(target)
                .map_or(layout.items.len(), |(index, _)| index);
            layout.items.insert(slot, item);
        }
        layout
    }

    fn tab_natural_width(&self, tab: &Tab, window: &Window, cx: &App) -> f32 {
        let metrics = self.theme_tokens.metrics;
        let title = self.tab_display_title(tab);
        let controls = self
            .reconnect_node_id_for_tab(tab, cx)
            .and_then(|node| {
                self.workspace_runtime
                    .read(cx)
                    .reconnect_active_progress(&node)
            })
            .map_or(metrics.tab_close_button_size, |job| {
                self.tab_reconnect_controls_width(&job, window)
            });
        (self.tab_text_width(&title, metrics.tab_font_size, window)
            + metrics.tab_padding_x * 2.0
            + metrics.tab_icon_size
            + metrics.tab_gap * 2.0
            + controls
            + 1.0)
            .ceil()
    }

    pub(super) fn tab_text_width(&self, text: &str, size: f32, window: &Window) -> f32 {
        let text = SharedString::from(text.to_string());
        let run = TextRun {
            len: text.len(),
            font: font(settings_ui_font_family(
                &self.settings_store.settings().appearance.ui_font_family,
            )),
            color: rgb(self.theme_tokens.ui.text).into(),
            background_color: None,
            underline: None,
            strikethrough: None,
            letter_spacing: None,
        };
        // Use the same shaper and font as the label, including fallback glyphs.
        f32::from(
            window
                .text_system()
                .shape_line(text, px(size), &[run], None)
                .width,
        )
    }
}

impl ExitingTabVisual {
    pub(super) fn exit_visibility(&self, tokens: &oxide_theme::ThemeTokens) -> f32 {
        let duration =
            oxide_ui::motion::duration(tokens, oxide_ui::motion::MotionDuration::Control);
        if duration.is_zero() {
            return 0.0;
        }
        1.0 - oxide_ui::motion::ease_in_out_cubic(
            self.started_at.elapsed().as_secs_f32() / duration.as_secs_f32(),
        )
    }
}
