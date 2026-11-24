use super::*;

impl FileManagerState {
    fn save_bookmarks(&self) -> Result<(), String> {
        let bytes =
            serde_json::to_vec_pretty(&self.bookmarks).map_err(|error| error.to_string())?;
        oxide_atomic_file::durable_write(&self.bookmarks_path, &bytes)
            .map_err(|error| error.to_string())
    }

    fn reorder_bookmark(
        &mut self,
        source: &str,
        target: &str,
        after: bool,
    ) -> Result<bool, String> {
        let Some(from) = self
            .bookmarks
            .iter()
            .position(|bookmark| bookmark.id == source)
        else {
            return Ok(false);
        };
        let Some(target) = self
            .bookmarks
            .iter()
            .position(|bookmark| bookmark.id == target)
        else {
            return Ok(false);
        };
        if from == target {
            return Ok(false);
        }
        let slot = target + usize::from(after);
        let to = slot - usize::from(from < slot);
        if from == to {
            return Ok(false);
        }
        let previous = self.bookmarks.clone();
        let moved = self.bookmarks.remove(from);
        self.bookmarks.insert(to, moved);
        if let Err(error) = self.save_bookmarks() {
            self.bookmarks = previous;
            return Err(error);
        }
        Ok(true)
    }
}

impl WorkspaceApp {
    pub(in crate::workspace::file_manager) fn is_file_manager_path_bookmarked(
        &self,
        path: &str,
        cx: &App,
    ) -> bool {
        self.file_manager
            .read(cx)
            .bookmarks
            .iter()
            .any(|bookmark| bookmark.path == path)
    }

    pub(in crate::workspace::file_manager) fn toggle_file_manager_current_bookmark(
        &mut self,
        cx: &mut Context<Self>,
    ) {
        let added = self.file_manager.update(cx, |file_manager, cx| {
            let path = file_manager.path.clone();
            let added = if let Some(index) = file_manager
                .bookmarks
                .iter()
                .position(|bookmark| bookmark.path == path)
            {
                file_manager.bookmarks.remove(index);
                false
            } else {
                file_manager.bookmarks.push(LocalBookmark {
                    id: new_file_manager_bookmark_id(),
                    name: bookmark_name_for_path(&path),
                    path,
                    created_at: now_ms(),
                });
                true
            };
            cx.notify();
            added
        });
        if added {
            self.push_file_manager_toast(
                self.i18n.t("fileManager.bookmarked"),
                None,
                TerminalNoticeVariant::Success,
                cx,
            );
        } else {
            self.push_file_manager_toast(
                self.i18n.t("fileManager.removeBookmark"),
                None,
                TerminalNoticeVariant::Default,
                cx,
            );
        }
        self.persist_file_manager_bookmarks(cx);
    }

    pub(in crate::workspace::file_manager) fn remove_file_manager_bookmark(
        &mut self,
        id: &str,
        cx: &mut Context<Self>,
    ) {
        self.file_manager.update(cx, |file_manager, cx| {
            file_manager.bookmarks.retain(|bookmark| bookmark.id != id);
            cx.notify();
        });
        self.persist_file_manager_bookmarks(cx);
    }

    pub(in crate::workspace::file_manager) fn open_file_manager_edit_bookmark_dialog(
        &mut self,
        bookmark: LocalBookmark,
        cx: &mut Context<Self>,
    ) {
        self.file_manager.update(cx, |file_manager, cx| {
            file_manager.dialog = Some(FileManagerDialog::EditBookmark {
                id: bookmark.id,
                path: bookmark.path,
            });
            file_manager.dialog_value = bookmark.name;
            file_manager.focused_input = Some(FileManagerInput::DialogValue);
            cx.notify();
        });
        self.ime_marked_text = None;
    }

    pub(super) fn update_file_manager_bookmark_name(&mut self, id: String, cx: &mut Context<Self>) {
        let name = self.file_manager.read(cx).dialog_value.trim().to_string();
        if name.is_empty() {
            return;
        }
        let changed = self.file_manager.update(cx, |file_manager, cx| {
            let changed = if let Some(bookmark) = file_manager
                .bookmarks
                .iter_mut()
                .find(|bookmark| bookmark.id == id)
            {
                bookmark.name = name;
                true
            } else {
                false
            };
            cx.notify();
            changed
        });
        if changed {
            self.persist_file_manager_bookmarks(cx);
        }
        self.close_file_manager_dialog(cx);
    }
    pub(in crate::workspace::file_manager) fn reorder_file_manager_bookmark(
        &mut self,
        source: &str,
        target: &str,
        after: bool,
        cx: &mut Context<Self>,
    ) {
        let result = self.file_manager.update(cx, |file_manager, cx| {
            let result = file_manager.reorder_bookmark(source, target, after);
            if matches!(result, Ok(true)) {
                cx.notify();
            }
            result
        });
        match result {
            Ok(true) => {
                self.segmented_control_user_motion
                    .clear(selection_motion::FILE_MANAGER_NAVIGATION_ID);
                cx.notify();
            }
            Ok(false) => {}
            Err(error) => self.push_file_manager_toast(
                self.i18n.t("fileManager.error"),
                Some(error),
                TerminalNoticeVariant::Error,
                cx,
            ),
        }
    }

    fn persist_file_manager_bookmarks(&mut self, cx: &App) {
        match self.file_manager.read(cx).save_bookmarks() {
            Ok(()) => {}
            Err(error) => self.push_file_manager_toast(
                self.i18n.t("fileManager.error"),
                Some(error),
                TerminalNoticeVariant::Error,
                cx,
            ),
        }
    }
}
