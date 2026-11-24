//! Local filesystem behavior for terminal current-directory awareness.

use std::{cmp::Ordering, path::PathBuf};

use super::model::{CurrentDirectoryEntry, CurrentDirectoryEntryKind};

/// List a local directory using the same normalized paths exposed to shells.
pub fn list_local_current_directory(
    cwd: &str,
    max_entries: usize,
) -> Option<Vec<CurrentDirectoryEntry>> {
    let actual_cwd = expand_local_home_path(cwd);
    let entries = std::fs::read_dir(&actual_cwd).ok()?;
    let mut rows = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let metadata = std::fs::symlink_metadata(entry.path()).ok()?;
            let kind = if metadata.is_dir() {
                CurrentDirectoryEntryKind::Directory
            } else if metadata.is_file() || metadata.file_type().is_symlink() {
                CurrentDirectoryEntryKind::File
            } else {
                return None;
            };
            let name = entry.file_name().to_string_lossy().to_string();
            let path = display_child_path(cwd, &entry.path(), &name);
            CurrentDirectoryEntry::new_with_kind(name, path, kind)
        })
        .collect::<Vec<_>>();
    sort_current_directory_entries(&mut rows);
    rows.truncate(max_entries);
    Some(rows)
}

/// Sort directories before files and compare names case-insensitively.
pub fn sort_current_directory_entries(entries: &mut [CurrentDirectoryEntry]) {
    entries.sort_by(current_directory_entry_order);
}

/// Return whether a typed value is an explicit path rather than a bare name.
pub fn current_directory_path_is_explicit(value: &str) -> bool {
    value == "~"
        || value.starts_with("~/")
        || value.starts_with('/')
        || value.starts_with("\\\\")
        || (value.len() > 2 && value.as_bytes().get(1) == Some(&b':'))
}

pub(crate) fn expand_local_home_path(path: &str) -> PathBuf {
    let path = path.trim();
    if path == "~" {
        return local_home().unwrap_or_else(|| PathBuf::from(path));
    }
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = local_home()
    {
        return home.join(rest);
    }
    PathBuf::from(path)
}

fn current_directory_entry_order(
    left: &CurrentDirectoryEntry,
    right: &CurrentDirectoryEntry,
) -> Ordering {
    match (left.kind(), right.kind()) {
        (CurrentDirectoryEntryKind::Directory, CurrentDirectoryEntryKind::File) => Ordering::Less,
        (CurrentDirectoryEntryKind::File, CurrentDirectoryEntryKind::Directory) => {
            Ordering::Greater
        }
        _ => left.name().to_lowercase().cmp(&right.name().to_lowercase()),
    }
}

fn local_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
}

fn display_child_path(cwd: &str, absolute_path: &std::path::Path, name: &str) -> String {
    let cwd = cwd.trim_end_matches(['/', '\\']);
    if cwd == "~" {
        format!("~/{name}")
    } else if cwd.starts_with("~/") {
        format!("{cwd}/{name}")
    } else {
        absolute_path.to_string_lossy().to_string()
    }
}
