use oxide_editor::{EditorKeybindings, EditorShortcut};
use oxide_markdown::{MarkdownDocument, MarkdownOptions, MarkdownVirtualListScrollHandle};
use oxide_ui::button::{IconButtonOptions, icon_button};

#[derive(Default)]
struct MarkdownPreviewState {
    editing: bool,
    source: Option<Arc<str>>,
    document: Option<MarkdownDocument>,
    scroll: MarkdownVirtualListScrollHandle,
    // The file tab owns parsing; closing it cancels delivery to the surface.
    parse_task: Option<Task<()>>,
}

impl IdeSurface {
    fn active_markdown_tab(&self) -> Option<EditorTabId> {
        self.workspace
            .active_tab()
            .filter(|tab_id| self.is_markdown_tab(*tab_id))
    }

    fn is_markdown_tab(&self, tab_id: EditorTabId) -> bool {
        let Some(tab) = self.workspace.tabs().iter().find(|tab| tab.id == tab_id) else {
            return false;
        };
        let path = match &tab.location {
            IdeLocation::Local { path } => path.as_path(),
            IdeLocation::Remote { path, .. } => Path::new(path),
        };
        path.extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
    }

    fn is_markdown_preview(&self) -> bool {
        self.active_markdown_tab().is_some_and(|tab_id| {
            !self
                .markdown_previews
                .get(&tab_id)
                .is_some_and(|state| state.editing)
        })
    }

    fn show_markdown_source(&mut self, tab_id: EditorTabId) {
        if self.is_markdown_tab(tab_id) {
            self.markdown_previews.entry(tab_id).or_default().editing = true;
        }
    }

    fn remove_editor(&mut self, tab_id: EditorTabId) {
        self.editors.remove(&tab_id);
        self.markdown_previews.remove(&tab_id);
    }

    fn toggle_markdown_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab_id) = self.active_markdown_tab() else {
            return;
        };
        let Some(editor) = self.active_editor() else {
            return;
        };
        let editing = self.is_markdown_preview();
        self.sync_editor_to_workspace(tab_id, cx);
        self.markdown_previews.entry(tab_id).or_default().editing = editing;
        if editing {
            window.focus(&editor.focus_handle(cx), cx);
        } else {
            // The hidden editor must no longer receive text, IME, or command events.
            window.focus(&self.focus_handle, cx);
        }
        cx.notify();
    }

    fn markdown_options(&self) -> MarkdownOptions {
        let mut options = MarkdownOptions::from_theme(&self.tokens);
        options.background_surface_active = self.runtime_settings.background_active;
        options.html_details_label = self.labels.markdown_details.clone();
        options.mermaid_error_prefix = self.labels.markdown_mermaid_error.clone();
        options.mermaid_loading_label = self.labels.markdown_mermaid_loading.clone();
        // Remote paths must never resolve against the client filesystem.
        options.allowed_link_schemes = vec!["http", "https", "mailto"];
        options.allowed_image_schemes = vec!["http", "https", "data"];
        options
    }

    fn render_markdown_toggle(&self, cx: &mut Context<Self>) -> AnyElement {
        let preview = self.is_markdown_preview();
        let label = if preview {
            &self.labels.markdown_edit
        } else {
            &self.labels.markdown_preview
        }
        .clone();
        let tokens = self.tokens;
        div()
            .flex_none()
            .px_1()
            .child(
                icon_button(
                    &tokens,
                    self.icon(
                        if preview {
                            "lucide/pencil.svg"
                        } else {
                            "lucide/eye.svg"
                        },
                        IDE_TREE_TOOLBAR_ICON_SIZE,
                        tokens.ui.text_muted,
                    ),
                    IconButtonOptions::compact(IDE_TREE_TOOLBAR_BUTTON_SIZE),
                )
                .id("ide-markdown-toggle")
                .debug_selector(|| "ide-markdown-toggle".into())
                .tooltip(move |_, cx| {
                    oxide_ui::tooltip::tooltip_view(tokens, label.clone(), None, cx)
                })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| {
                        this.toggle_markdown_mode(window, cx);
                        cx.stop_propagation();
                    }),
                ),
            )
            .into_any_element()
    }

    fn prepare_markdown_preview(
        &mut self,
        tab_id: EditorTabId,
        source: Arc<str>,
        cx: &mut Context<Self>,
    ) {
        let state = self.markdown_previews.entry(tab_id).or_default();
        if state
            .source
            .as_ref()
            .is_some_and(|previous| Arc::ptr_eq(previous, &source))
        {
            return;
        }
        state.source = Some(source.clone());
        state.document = None;
        let parse_source = source.clone();
        let parse = cx
            .background_executor()
            .spawn(async move { oxide_markdown::parser::parse(&parse_source) });
        state.parse_task = Some(cx.spawn(async move |weak, cx| {
            let document = parse.await;
            let _ = weak.update(cx, |this, cx| {
                let Some(state) = this.markdown_previews.get_mut(&tab_id) else {
                    return;
                };
                if state
                    .source
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &source))
                {
                    state.document = Some(document);
                    state.parse_task = None;
                    cx.notify();
                }
            });
        }));
    }

    fn render_markdown_preview(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let tab_id = self.active_markdown_tab().expect("markdown tab selected");
        let source = self.editors[&tab_id].read(cx).buffer().text_snapshot();
        self.prepare_markdown_preview(tab_id, source, cx);
        let state = &self.markdown_previews[&tab_id];
        let Some(document) = &state.document else {
            return self.render_loading_file();
        };
        div()
            .debug_selector(|| "ide-markdown-preview".into())
            .size_full()
            .p_4()
            .child(oxide_markdown::render::render_document_virtual(
                SharedString::from(format!("ide-markdown-preview-{}", tab_id.0)),
                document,
                &self.tokens,
                &self.markdown_options(),
                &state.scroll,
            ))
            .into_any_element()
    }

    fn handle_markdown_preview_key(
        &mut self,
        event: &KeyDownEvent,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.is_markdown_preview() {
            return false;
        }
        let shortcut = if let Some(bindings) = cx.try_global::<EditorKeybindings>() {
            bindings.resolve(&event.keystroke)
        } else {
            let modifiers = event.keystroke.modifiers;
            let command = if cfg!(target_os = "macos") {
                modifiers.platform
            } else {
                modifiers.control
            };
            (command
                && !modifiers.shift
                && !modifiers.alt
                && event.keystroke.key.eq_ignore_ascii_case("s"))
            .then_some(EditorShortcut::Save)
        };
        if shortcut == Some(EditorShortcut::Save) {
            if let Some(tab_id) = self.workspace.active_tab() {
                self.sync_editor_to_workspace(tab_id, cx);
                self.save_tab(tab_id, cx);
            }
            cx.stop_propagation();
            return true;
        }
        false
    }
}
