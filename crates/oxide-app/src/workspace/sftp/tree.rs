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
        self.push_sftp_tree_rows(&self.remote_files, 0, &mut rows);
        rows
    }

    /// The directory represented by the visible tree focus. A selected file
    /// resolves to its containing directory; stale selections are ignored so
    /// navigation and collapse cannot leave the sync target outside the tree.
    pub(in crate::workspace::sftp) fn tree_sync_directory_path(&self) -> Option<String> {
        let selected = self.tree_selected.as_deref()?;
        let row = self
            .flatten_sftp_tree_rows()
            .into_iter()
            .find(|row| row.entry.path == selected)?;
        if row.entry.file_type == SftpFileType::Directory {
            Some(row.entry.path)
        } else {
            Some(parent_path(&row.entry.path, true).to_string())
        }
    }

    fn push_sftp_tree_rows(
        &self,
        files: &[SftpFileEntry],
        depth: usize,
        rows: &mut Vec<SftpTreeRow>,
    ) {
        let indices = sftp_sorted_filtered_indices(
            files,
            "",
            self.remote_sort_field,
            self.remote_sort_direction,
        );
        for index in indices {
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
                self.push_sftp_tree_rows(children, depth + 1, rows);
            }
        }
    }
}
