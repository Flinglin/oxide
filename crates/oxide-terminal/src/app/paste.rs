use super::*;
use gpui::{AnyElement, Entity, Focusable, KeyDownEvent, div, prelude::*, rgb};
use oxide_editor::{EditorSettings, TextEditorView};
use oxide_ui::{
    button::{ButtonOptions, ButtonSize, ButtonVariant, button_with},
    modal::{
        dialog_content, dialog_footer, dialog_header, dialog_overlay, dialog_title,
        overlay_content_boundary,
    },
    separator::{SeparatorOrientation, separator},
};

pub(super) struct PasteEditor {
    editor: Entity<TextEditorView>,
    _observation: Subscription,
}

impl TerminalPane {
    pub fn paste_editor_focused(&self, window: &Window, cx: &App) -> bool {
        self.paste_editor
            .as_ref()
            .is_some_and(|draft| draft.editor.focus_handle(cx).contains_focused(window, cx))
    }

    pub(super) fn edit_clipboard_paste(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.terminal_accepts_input() {
            return;
        }
        let Some(text) = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .filter(|text| !text.is_empty())
        else {
            return;
        };
        self.pending_paste = Some(Zeroizing::new(text));
        self.pending_paste_prefix = None;
        self.open_paste_editor(window, cx);
    }

    fn open_paste_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = self.pending_paste.as_ref() else {
            return;
        };
        let tokens = self.theme.tokens;
        // This pane owns the transient editor and its undo history. Neither is persisted;
        // submission or cancellation drops the entity and all draft UI state.
        let editor = cx.new(|cx| {
            let mut editor = TextEditorView::new(text.as_str(), &tokens, cx);
            editor.set_border_visible(false);
            editor.set_settings(
                EditorSettings {
                    soft_wrap: true,
                    indentation_markers: false,
                    highlight_current_line: false,
                    ..Default::default()
                },
                cx,
            );
            editor
        });
        let observation = cx.observe(&editor, |_, _, cx| cx.notify());
        self.paste_editor = Some(PasteEditor {
            editor: editor.clone(),
            _observation: observation,
        });
        self.refresh_paste_editor(cx);
        self.dismiss_terminal_context_menu(cx);
        window.focus(&editor.focus_handle(cx), cx);
        cx.notify();
    }

    pub(super) fn refresh_paste_editor(&mut self, cx: &mut Context<Self>) {
        let Some(draft) = &self.paste_editor else {
            return;
        };
        draft.editor.update(cx, |editor, cx| {
            editor.set_context_menu_labels(self.preferences.paste_labels.editor_menu.clone());
            editor.apply_ide_runtime_settings(
                &self.theme.tokens,
                self.preferences.font_family.clone(),
                self.preferences.font_weight,
                self.preferences.cjk_font_family.clone(),
                self.preferences.font_size,
                self.preferences.line_height,
                true,
                false,
                cx,
            );
        });
    }

    fn edited_paste_text(&self, cx: &App) -> Option<Zeroizing<String>> {
        let original = self.pending_paste.as_ref()?;
        let draft = self.paste_editor.as_ref()?;
        let edited = Zeroizing::new(draft.editor.read(cx).buffer().text());
        let normalized = Zeroizing::new(original.replace("\r\n", "\n").replace('\r', "\n"));
        // Opening the editor or undoing all edits must preserve the original bytes,
        // including mixed endings. Edited documents retain the source's CRLF convention.
        if *edited == *normalized {
            Some(original.clone())
        } else if original.contains("\r\n") {
            Some(Zeroizing::new(edited.replace('\n', "\r\n")))
        } else {
            Some(edited)
        }
    }

    fn submit_edited_paste(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = self.edited_paste_text(cx).filter(|text| !text.is_empty()) else {
            return;
        };
        self.paste_editor = None;
        self.pending_paste = Some(text);
        self.confirm_pending_paste(cx);
        window.focus(&self.focus_handle, cx);
    }

    fn close_paste_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.paste_editor = None;
        self.cancel_pending_paste(cx);
        window.focus(&self.focus_handle, cx);
    }

    fn strip_paste_fence(&mut self, cx: &mut Context<Self>) {
        let Some(draft) = &self.paste_editor else {
            return;
        };
        let text = Zeroizing::new(draft.editor.read(cx).buffer().text());
        if let Some(body) = fenced_body(&text) {
            draft
                .editor
                .update(cx, |editor, cx| editor.replace_text_external(body, cx));
        }
    }

    pub(super) fn render_paste_overlay(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tokens = self.theme.tokens;
        let labels = &self.preferences.paste_labels;
        let editing = self.paste_editor.is_some();
        let width = (f32::from(window.viewport_size().width) - 32.0)
            .max(0.0)
            .min(if editing { 760.0 } else { 480.0 });
        let height = (f32::from(window.viewport_size().height) - 32.0).clamp(0.0, 520.0);
        let options = ButtonOptions {
            size: ButtonSize::Sm,
            ..Default::default()
        };
        let text = self
            .pending_paste
            .as_deref()
            .map(String::as_str)
            .unwrap_or_default();
        let title = if editing {
            labels.edit_title.clone()
        } else {
            labels
                .title_template
                .replace("{{count}}", &text.split('\n').count().to_string())
        };
        let mut content = dialog_content(&tokens)
            .debug_selector(|| "paste-dialog".into())
            .w(px(width))
            .max_h(px(height))
            .when(editing, |dialog| dialog.h(px(height)))
            .bg(rgb(tokens.ui.bg_panel))
            .shadow(oxide_ui::theme_overlay_shadow(&tokens))
            .flex()
            .flex_col()
            .font_family(tokens.metrics.font_family)
            .text_color(rgb(tokens.ui.text))
            .child(dialog_header(&tokens).child(dialog_title(&tokens, title)));
        if let Some(draft) = &self.paste_editor {
            let text = Zeroizing::new(draft.editor.read(cx).buffer().text());
            let tool_options = ButtonOptions {
                variant: ButtonVariant::Ghost,
                ..options
            };
            content = content
                .child(
                    div()
                        .flex()
                        .items_center()
                        .flex_wrap()
                        .flex_none()
                        .px(px(tokens.spacing.two))
                        .py(px(tokens.spacing.one))
                        .border_b_1()
                        .border_color(rgb(tokens.ui.border))
                        .child(
                            button_with(&tokens, labels.undo.clone(), tool_options)
                                .id("paste-undo")
                                .debug_selector(|| "paste-undo".into())
                                .on_click(cx.listener(|pane, _, _, cx| {
                                    if let Some(draft) = &pane.paste_editor {
                                        draft
                                            .editor
                                            .update(cx, |editor, cx| editor.undo_external(cx));
                                    }
                                })),
                        )
                        .child(
                            button_with(&tokens, labels.redo.clone(), tool_options)
                                .id("paste-redo")
                                .on_click(cx.listener(|pane, _, _, cx| {
                                    if let Some(draft) = &pane.paste_editor {
                                        draft
                                            .editor
                                            .update(cx, |editor, cx| editor.redo_external(cx));
                                    }
                                })),
                        )
                        .child(
                            separator(&tokens, SeparatorOrientation::Vertical)
                                .h(px(tokens.metrics.ui_text_sm))
                                .mx(px(tokens.spacing.one)),
                        )
                        .child(
                            button_with(
                                &tokens,
                                labels.strip_fence.clone(),
                                ButtonOptions {
                                    disabled: fenced_body(&text).is_none(),
                                    ..tool_options
                                },
                            )
                            .id("paste-strip-fence")
                            .debug_selector(|| "paste-strip-fence".into())
                            .on_click(cx.listener(|pane, _, _, cx| pane.strip_paste_fence(cx))),
                        ),
                )
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .overflow_hidden()
                        .debug_selector(|| "paste-editor".into())
                        .child(draft.editor.clone()),
                );
        } else {
            let mut preview = div()
                .p(px(tokens.metrics.modal_body_padding))
                .min_h_0()
                .overflow_hidden()
                .flex()
                .flex_col()
                .font_family(self.preferences.font_family.clone())
                .text_size(px(tokens.metrics.ui_text_sm));
            for line in text.split('\n').take(5) {
                preview = preview.child(
                    div()
                        .overflow_hidden()
                        .child(if line.is_empty() { " " } else { line }.to_string()),
                );
            }
            let remaining = text.split('\n').count().saturating_sub(5);
            if remaining > 0 {
                preview = preview.child(
                    div().text_color(rgb(tokens.ui.text_muted)).child(
                        labels
                            .more_lines_template
                            .replace("{{count}}", &remaining.to_string()),
                    ),
                );
            }
            content = content.child(preview);
        }
        let empty = self
            .paste_editor
            .as_ref()
            .is_some_and(|draft| draft.editor.read(cx).buffer().is_empty());
        content = content.child(
            dialog_footer(&tokens)
                .h_auto()
                .flex_none()
                .min_h(px(tokens.metrics.modal_footer_height))
                .py(px(tokens.spacing.two))
                .flex_wrap()
                .child(
                    button_with(
                        &tokens,
                        labels.cancel.clone(),
                        ButtonOptions {
                            variant: ButtonVariant::Ghost,
                            ..options
                        },
                    )
                    .id("paste-cancel")
                    .debug_selector(|| "paste-cancel".into())
                    .on_click(
                        cx.listener(|pane, _, window, cx| pane.close_paste_editor(window, cx)),
                    ),
                )
                .when(!editing, |footer| {
                    footer.child(
                        button_with(&tokens, labels.edit.clone(), options)
                            .id("paste-edit")
                            .debug_selector(|| "paste-edit".into())
                            .on_click(cx.listener(|pane, _, window, cx| {
                                pane.open_paste_editor(window, cx)
                            })),
                    )
                })
                .child(
                    button_with(
                        &tokens,
                        labels.paste.clone(),
                        ButtonOptions {
                            variant: ButtonVariant::Default,
                            disabled: empty,
                            ..options
                        },
                    )
                    .id("paste-submit")
                    .debug_selector(|| "paste-submit".into())
                    .on_click(cx.listener(move |pane, _, window, cx| {
                        if editing {
                            pane.submit_edited_paste(window, cx);
                        } else {
                            pane.confirm_pending_paste(cx);
                            window.focus(&pane.focus_handle, cx);
                        }
                    })),
                ),
        );
        let content =
            overlay_content_boundary(content)
                .on_key_up(|_, _, cx| cx.stop_propagation())
                .on_key_down(cx.listener(move |pane, event: &KeyDownEvent, window, cx| {
                    if event.keystroke.key == "escape" {
                        pane.close_paste_editor(window, cx);
                        window.prevent_default();
                    } else if pane.paste_editor_focused(window, cx)
                        && !event.keystroke.modifiers.control
                        && !event.keystroke.modifiers.platform
                        && event.keystroke.key_char.as_deref().is_some_and(|text| {
                            !text.is_empty() && !text.chars().any(char::is_control)
                        })
                    {
                        // X11 and Wayland commit characters only after an unhandled keydown.
                        // The focused editor owns text input; the terminal skips keys while editing.
                        return;
                    }
                    cx.stop_propagation();
                }));
        gpui::deferred(
            gpui::anchored()
                .anchor(gpui::Anchor::TopLeft)
                .position_mode(gpui::AnchoredPositionMode::Window)
                .position(gpui::point(px(0.0), px(0.0)))
                .child(
                    div()
                        .relative()
                        .w(window.viewport_size().width)
                        .h(window.viewport_size().height)
                        .child(dialog_overlay(&tokens, content)),
                ),
        )
        .with_priority(oxide_ui::modal::TAURI_POPOVER_LAYER_PRIORITY)
        .into_any_element()
    }
}

// Only unwrap one complete outer fenced block. Prose, multiple blocks and code
// whitespace stay untouched; this operation is never part of ordinary paste.
fn fenced_body(text: &str) -> Option<&str> {
    let text = text.trim();
    let (opening, rest) = text.split_once('\n')?;
    let marker = *opening.as_bytes().first()?;
    if !matches!(marker, b'`' | b'~') {
        return None;
    }
    let count = opening.bytes().take_while(|byte| *byte == marker).count();
    if count < 3 || (marker == b'`' && opening[count..].contains('`')) {
        return None;
    }
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        let closing = line.trim();
        if closing.len() >= count && closing.bytes().all(|byte| byte == marker) {
            return rest[offset + line.len()..]
                .trim()
                .is_empty()
                .then_some(&rest[..offset]);
        }
        offset += line.len();
    }
    None
}
