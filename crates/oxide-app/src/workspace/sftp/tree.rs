use super::file_list::sftp_sorted_filtered_indices;
use super::*;

/// One flattened row of the sidebar remote tree.
#[derive(Clone, Debug)]
pub(in crate::workspace::sftp) struct SftpTreeRow {
    pub(in crate::workspace::sftp) entry: SftpFileEntry,
    pub(in crate::workspace::sftp) depth: usize,
    pub(in crate::workspace::sftp) expanded: bool,
    pub(in crate::workspace::sftp) loading: bool,
}

impl SftpWorkspaceEntity {
    pub(in crate::workspace::sftp) fn flatten_sftp_tree_rows(&self) -> Vec<SftpTreeRow> {
        let mut rows = Vec::new();
        self.push_sftp_tree_rows(&self.remote_files, 0, &mut HashSet::new(), &mut rows);
        rows
    }

    pub(in crate::workspace::sftp) fn selected_tree_file(&self) -> Option<SftpFileEntry> {
        let selected = self.tree_selected.as_deref()?;
        self.flatten_sftp_tree_rows()
            .into_iter()
            .find(|row| row.entry.path == selected)
            .map(|row| row.entry)
    }

    pub(in crate::workspace::sftp) fn select_tree_file(&mut self, path: String) {
        self.active_pane = SftpPane::Remote;
        self.focused_input = None;
        self.clear_context_menu_immediately();
        self.remote_selected.clear();
        self.remote_last_selected = None;
        self.tree_selected = Some(path);
    }

    pub(in crate::workspace::sftp) fn move_tree_selection(&mut self, delta: isize) -> bool {
        let rows = self.flatten_sftp_tree_rows();
        if rows.is_empty() {
            return false;
        }
        let current = rows
            .iter()
            .position(|row| Some(&row.entry.path) == self.tree_selected.as_ref());
        let next = match current {
            Some(index) if delta > 0 => (index + 1) % rows.len(),
            Some(0) | None if delta < 0 => rows.len() - 1,
            Some(index) => index - 1,
            None => 0,
        };
        self.select_tree_file(rows[next].entry.path.clone());
        scroll_tauri_virtual_list_to_index(
            &self.remote_file_scroll,
            next,
            sftp_file_list_virtual_spec(),
            TauriVirtualScrollAlign::Nearest,
        );
        true
    }
    /// The directory represented by the visible tree focus. A selected file
    /// resolves to its containing directory; stale selections are ignored so
    /// navigation and collapse cannot leave the sync target outside the tree.
    pub(in crate::workspace::sftp) fn tree_sync_directory_path(&self) -> Option<String> {
        let file = self.selected_tree_file()?;
        if file.file_type == SftpFileType::Directory {
            Some(file.path)
        } else {
            Some(parent_path(&file.path, true).to_string())
        }
    }

    fn push_sftp_tree_rows(
        &self,
        files: &[SftpFileEntry],
        depth: usize,
        ancestors: &mut HashSet<String>,
        rows: &mut Vec<SftpTreeRow>,
    ) {
        let indices = sftp_sorted_filtered_indices(
            files,
            "",
            self.remote_sort_field,
            self.remote_sort_direction,
        );
        for index in indices {
            // SFTP canonicalizes directory listings. A link back to an ancestor
            // can return the same cached path; only prune that ancestry chain,
            // so sibling links to a shared directory remain independently visible.
            if ancestors.contains(&files[index].path) {
                continue;
            }
            let entry = files[index].clone();
            let is_directory = entry.file_type == SftpFileType::Directory;
            let expanded = is_directory && self.tree_expanded.contains(&entry.path);
            let loading = is_directory
                && (self.tree_loading.contains(&entry.path)
                    || self.tree_pending.contains(&entry.path));
            let path = entry.path.clone();
            rows.push(SftpTreeRow {
                entry,
                depth,
                expanded,
                loading,
            });
            if expanded && let Some(children) = self.tree_children.get(&path) {
                ancestors.insert(path.clone());
                self.push_sftp_tree_rows(children, depth + 1, ancestors, rows);
                ancestors.remove(&path);
            }
        }
    }
}

impl WorkspaceApp {
    pub(in crate::workspace::sftp) fn sftp_tree_mode(&self, cx: &App) -> bool {
        let sftp = self.sftp_view().read(cx);
        sftp.current_surface_id == Some(SftpSurfaceId::Sidebar)
            && self.settings_store.settings().sftp.sidebar_view_mode
                == oxide_settings::SftpSidebarViewMode::Tree
            && sftp.remote_filter.trim().is_empty()
    }
}
