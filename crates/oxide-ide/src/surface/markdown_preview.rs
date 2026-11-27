use oxide_editor::{EditorKeybindings, EditorShortcut};
use oxide_markdown::{MarkdownDocument, MarkdownOptions, MarkdownVirtualListScrollHandle};
use oxide_ui::button::{IconButtonOptions, icon_button};

#[derive(Default)]
struct MarkdownPreviewState {
    editing: bool,
    source: Option<Arc<str>>,
    document: Option<MarkdownDocument>,
    selection: Option<MarkdownPreviewSelection>,
    source_version: Option<u64>,
    suspended_selection: Option<(usize, usize)>,
    scroll: MarkdownVirtualListScrollHandle,
    // The file tab owns parsing; closing it cancels delivery to the surface.
    parse_task: Option<Task<()>>,
}

impl MarkdownPreviewState {
    fn release_cache(&mut self) {
        self.parse_task = None;
        self.source = None;
        self.document = None;
        if let Some(selection) = self.selection.take() {
            self.suspended_selection = Some((selection.anchor, selection.head));
        }
        self.scroll.release_document_cache();
    }
}

impl IdeSurface {
    fn sync_tab_render_caches(&mut self, cx: &mut Context<Self>) {
        let active = self
            .workspace
            .active_tab()
            .filter(|_| self.mount.is_visible());
        let preview = self.is_markdown_preview();
        for (&tab, state) in &mut self.markdown_previews {
            if active != Some(tab) || state.editing {
                state.release_cache();
            }
        }
        for (&tab, editor) in &self.editors {
            let visible = active == Some(tab);
            editor.update(cx, |editor, cx| {
                editor.set_render_cache_active(visible && !preview, cx);
                if !visible {
                    editor.buffer().release_text_cache();
                }
            });
        }
    }

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

    fn show_markdown_source(&mut self, tab_id: EditorTabId, cx: &mut Context<Self>) {
        if self.is_markdown_tab(tab_id) {
            self.markdown_previews.entry(tab_id).or_default().editing = true;
            self.sync_tab_render_caches(cx);
        }
    }

    fn remove_editor(&mut self, tab_id: EditorTabId) {
        self.editors.remove(&tab_id);
        self.markdown_previews.remove(&tab_id);
    }

    fn toggle_markdown_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.finish_markdown_selection();
        let Some(tab_id) = self.active_markdown_tab() else {
            return;
        };
        let Some(editor) = self.active_editor() else {
            return;
        };
        let editing = self.is_markdown_preview();
        self.sync_editor_to_workspace(tab_id, cx);
        self.markdown_previews.entry(tab_id).or_default().editing = editing;
        self.sync_tab_render_caches(cx);
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
        if !self.update_markdown_source(tab_id, &source, cx) {
            return;
        }
        let parse_source = source.clone();
        let parse = cx
            .background_executor()
            .spawn(async move { oxide_markdown::parser::parse(&parse_source) });
        let task = cx.spawn(async move |weak, cx| {
            let document = parse.await;
            let _ = weak.update(cx, |this, cx| {
                if this
                    .markdown_previews
                    .get(&tab_id)
                    .and_then(|state| state.source.as_ref())
                    .is_some_and(|current| Arc::ptr_eq(current, &source))
                {
                    this.install_markdown_document(tab_id, document);
                    cx.notify();
                }
            });
        });
        self.markdown_previews.get_mut(&tab_id).unwrap().parse_task = Some(task);
    }

    fn update_markdown_source(
        &mut self,
        tab_id: EditorTabId,
        source: &Arc<str>,
        cx: &mut Context<Self>,
    ) -> bool {
        let version = self.editors[&tab_id].read(cx).buffer().version();
        let state = self.markdown_previews.entry(tab_id).or_default();
        if state
            .source
            .as_ref()
            .is_some_and(|previous| Arc::ptr_eq(previous, source))
        {
            return false;
        }
        if state.source_version != Some(version) {
            state.suspended_selection = None;
            state.scroll.invalidate_document_cache();
        }
        state.source_version = Some(version);
        state.source = Some(source.clone());
        state.document = None;
        state.selection = None;
        state.parse_task = None;
        true
    }

    fn ensure_markdown_selection(&mut self, tab: EditorTabId, cx: &mut Context<Self>) {
        let source = self.editors[&tab].read(cx).buffer().text_snapshot();
        self.update_markdown_source(tab, &source, cx);
        if self.markdown_previews[&tab].selection.is_some() {
            return;
        }
        // Copy/select-all must work even before the first resumed frame. Only
        // explicit clipboard commands synchronously rebuild a cold preview.
        let document = oxide_markdown::parser::parse(&source);
        self.install_markdown_document(tab, document);
    }

    fn install_markdown_document(&mut self, tab: EditorTabId, document: MarkdownDocument) {
        let mut selection = MarkdownPreviewSelection::new(&document, &self.markdown_options());
        let state = self.markdown_previews.get_mut(&tab).unwrap();
        state.parse_task = None;
        if let Some((anchor, head)) = state.suspended_selection.take() {
            selection.anchor = anchor;
            selection.head = head;
        }
        state.selection = Some(selection);
        state.document = Some(document);
    }

    fn render_markdown_preview(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let tab_id = self.active_markdown_tab().expect("markdown tab selected");
        let source = self.editors[&tab_id].read(cx).buffer().text_snapshot();
        self.prepare_markdown_preview(tab_id, source, cx);
        let options = self.markdown_options();
        let state = &self.markdown_previews[&tab_id];
        if let Some(selection) = &state.selection {
            selection.layouts.borrow_mut().clear();
        }
        let Some(document) = &state.document else {
            return self.render_loading_file();
        };
        div()
            .debug_selector(|| "ide-markdown-preview".into())
            .size_full()
            .p_4()
            .child(
                oxide_markdown::render::render_document_virtual_selectable(
                    SharedString::from(format!("ide-markdown-preview-{}", tab_id.0)),
                    document,
                    &self.tokens,
                    &options,
                    &state.scroll,
                    None,
                    &mut |key, text, runs, links| {
                        self.render_markdown_fragment(tab_id, key, text, runs, links, cx)
                    },
                ),
            )
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
            if command && !modifiers.shift && !modifiers.alt {
                match event.keystroke.key.to_ascii_lowercase().as_str() {
                    "s" => Some(EditorShortcut::Save),
                    "c" => Some(EditorShortcut::Copy),
                    "a" => Some(EditorShortcut::SelectAll),
                    "x" => Some(EditorShortcut::Cut),
                    "v" => Some(EditorShortcut::Paste),
                    _ => None,
                }
            } else {
                None
            }
        };
        match shortcut {
            Some(EditorShortcut::Copy) => self.copy_markdown_selection(cx),
            Some(EditorShortcut::SelectAll) => self.select_all_markdown(cx),
            Some(EditorShortcut::Cut | EditorShortcut::Paste) => {}
            _ => {
                if shortcut != Some(EditorShortcut::Save) {
                    return false;
                }
            }
        }
        if shortcut == Some(EditorShortcut::Save) {
            if let Some(tab_id) = self.workspace.active_tab() {
                self.sync_editor_to_workspace(tab_id, cx);
                self.save_tab(tab_id, cx);
            }
            cx.stop_propagation();
            return true;
        }
        cx.stop_propagation();
        true
    }
}
