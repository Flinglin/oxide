use std::{path::PathBuf};

use crate::{LocalFileEntry, LocalFileType, LocalSortDirection, LocalSortField};

pub fn list_local_files(path: &str) -> std::io::Result<Vec<LocalFileEntry>> {
    let dir = std::fs::read_dir(path)?;
    let mut entries = Vec::with_capacity(64);
    let mut path_buf = PathBuf::with_capacity(256);

    for entry in dir {
        let entry = entry?;
        let file_name = entry.file_name();

        path_buf.clear();
        path_buf.push(path);
        path_buf.push(&file_name);

        let symlink_metadata = match std::fs::symlink_metadata(&path_buf) {
            Ok(m) => m,
            Err(_) => continue,
        };

        let is_symlink = symlink_metadata.file_type().is_symlink();

        let (target_metadata, symlink_target) = if is_symlink {
            let target = std::fs::metadata(&path_buf).ok();
            let link = std::fs::read_link(&path_buf)
                .ok()
                .map(|t| t.to_string_lossy().into_owned());
            (target, link)
        } else {
            (None, None)
        };
        let metadata = target_metadata.as_ref().unwrap_or(&symlink_metadata);

        let is_dir = metadata.is_dir();
        let file_type = if is_symlink {
            LocalFileType::Symlink
        } else if is_dir {
            LocalFileType::Directory
        } else {
            LocalFileType::File
        };

        let modified = metadata
            .modified()
            .ok()
            .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64);

        entries.push(LocalFileEntry {
            name: file_name.to_string_lossy().into_owned(),
            path: path_buf.to_string_lossy().into_owned(),
            file_type,
            size: if is_dir { 0 } else { metadata.len() },
            modified,
            readonly: metadata.permissions().readonly(),
            symlink_target,
        });
    }

    entries.sort_unstable_by(local_file_default_cmp);
    Ok(entries)
}

pub fn local_file_default_cmp(left: &LocalFileEntry, right: &LocalFileEntry) -> std::cmp::Ordering {
    match (left.is_directory_like(), right.is_directory_like()) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => left.name.to_lowercase().cmp(&right.name.to_lowercase()),
    }
}

pub fn sorted_local_files(
    files: &[LocalFileEntry],
    filter: &str,
    sort_field: LocalSortField,
    sort_direction: LocalSortDirection,
) -> Vec<LocalFileEntry> {
    let filter = filter.trim().to_lowercase();
    let mut filtered = files
        .iter()
        .filter(|file| filter.is_empty() || file.name.to_lowercase().contains(&filter))
        .cloned()
        .collect::<Vec<_>>();
    filtered.sort_by(|left, right| {
        if left.is_directory_like() && !right.is_directory_like() {
            return std::cmp::Ordering::Less;
        }
        if !left.is_directory_like() && right.is_directory_like() {
            return std::cmp::Ordering::Greater;
        }
        let ordering = match sort_field {
            LocalSortField::Name => left.name.to_lowercase().cmp(&right.name.to_lowercase()),
            LocalSortField::Size => left.size.cmp(&right.size),
            LocalSortField::Modified => left.modified.cmp(&right.modified),
        };
        match sort_direction {
            LocalSortDirection::Asc => ordering,
            LocalSortDirection::Desc => ordering.reverse(),
        }
    });
    filtered
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, file_type: LocalFileType, size: u64) -> LocalFileEntry {
        LocalFileEntry {
            name: name.to_string(),
            path: format!("/tmp/{name}"),
            file_type,
            size,
            modified: None,
            readonly: false,
            symlink_target: None,
        }
    }

    #[test]
    fn sorted_files_keep_directories_before_files() {
        let files = vec![
            entry("b.txt", LocalFileType::File, 2),
            entry("a-dir", LocalFileType::Directory, 0),
            entry("a.txt", LocalFileType::File, 1),
        ];
        let sorted = sorted_local_files(&files, "", LocalSortField::Name, LocalSortDirection::Asc);
        let names = sorted
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["a-dir", "a.txt", "b.txt"]);
    }
}
