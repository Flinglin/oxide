use std::collections::HashSet;
use std::fs::{self, File};
use std::path::{Path, PathBuf};

use walkdir::WalkDir;

use crate::error::TrzszError;
use crate::path_guard::{sanitize_upload_rel_path, validate_api_version, validate_owner_id};
use crate::{MAX_TRANSFER_CHUNK_SIZE, TrzszState, TrzszUploadEntryDto, TrzszUploadHandleDto};

pub fn build_upload_entries(
    state: &TrzszState,
    owner_id: &str,
    api_version: u32,
    paths: Vec<String>,
    allow_directory: bool,
) -> Result<Vec<TrzszUploadEntryDto>, TrzszError> {
    validate_api_version(api_version)?;
    validate_owner_id(owner_id)?;

    if paths.is_empty() {
        return Err(TrzszError::InvalidPath(
            "At least one upload path is required".to_string(),
        ));
    }

    let mut entries = Vec::new();
    let mut authorized_paths = HashSet::new();

    for (index, raw_path) in paths.iter().enumerate() {
        let root = PathBuf::from(raw_path);
        let root_metadata = fs::symlink_metadata(&root)?;
        if root_metadata.file_type().is_symlink() {
            return Err(TrzszError::SymlinkNotAllowed(root.display().to_string()));
        }
        if root_metadata.is_dir() && !allow_directory {
            return Err(TrzszError::DirectoryNotAllowed(root.display().to_string()));
        }

        let canonical_root = fs::canonicalize(&root)?;
        let rel_root = sanitize_upload_rel_path(
            root.file_name()
                .map(Path::new)
                .ok_or_else(|| TrzszError::InvalidPath(root.display().to_string()))?,
        )?;

        if root_metadata.is_file() {
            authorized_paths.insert(canonical_root.clone());
            entries.push(TrzszUploadEntryDto {
                path_id: (index + 1) as u64,
                path: canonical_root.to_string_lossy().to_string(),
                rel_path: rel_root,
                size: root_metadata.len(),
                is_dir: false,
                is_symlink: false,
            });
            continue;
        }

        for entry in WalkDir::new(&canonical_root).follow_links(false) {
            let entry = entry.map_err(|error| TrzszError::InvalidPath(error.to_string()))?;
            let metadata = fs::symlink_metadata(entry.path())?;
            if metadata.file_type().is_symlink() {
                return Err(TrzszError::SymlinkNotAllowed(
                    entry.path().display().to_string(),
                ));
            }

            let relative = entry
                .path()
                .strip_prefix(&canonical_root)
                .map_err(|error| TrzszError::InvalidPath(error.to_string()))?;
            let mut rel_path = rel_root.clone();
            if !relative.as_os_str().is_empty() {
                rel_path.extend(sanitize_upload_rel_path(relative)?);
            }

            authorized_paths.insert(entry.path().to_path_buf());
            entries.push(TrzszUploadEntryDto {
                path_id: (index + 1) as u64,
                path: entry.path().to_string_lossy().to_string(),
                rel_path,
                size: if metadata.is_file() {
                    metadata.len()
                } else {
                    0
                },
                is_dir: metadata.is_dir(),
                is_symlink: false,
            });
        }
    }

    // Tauri authorizes only the exact scanned canonical paths for this owner;
    // open_upload_file must reject arbitrary later paths even if the user can
    // otherwise read them locally.
    state.set_authorized_upload_paths(owner_id, authorized_paths);
    Ok(entries)
}

pub fn open_upload_file(
    state: &TrzszState,
    owner_id: &str,
    api_version: u32,
    path: String,
) -> Result<TrzszUploadHandleDto, TrzszError> {
    validate_api_version(api_version)?;
    validate_owner_id(owner_id)?;

    let target = PathBuf::from(path);
    let metadata = fs::symlink_metadata(&target)?;
    if metadata.file_type().is_symlink() {
        return Err(TrzszError::SymlinkNotAllowed(target.display().to_string()));
    }
    if !metadata.is_file() {
        return Err(TrzszError::InvalidPath(format!(
            "Upload target is not a regular file: {}",
            target.display()
        )));
    }

    let canonical = fs::canonicalize(&target)?;
    if !state.is_upload_path_authorized(owner_id, &canonical) {
        return Err(TrzszError::UnauthorizedPath(
            canonical.display().to_string(),
        ));
    }

    let file = File::open(&canonical)?;
    state.register_upload_handle(owner_id, file, metadata.len())
}

pub fn read_upload_chunk(
    state: &TrzszState,
    owner_id: &str,
    api_version: u32,
    handle_id: &str,
    offset: u64,
    length: usize,
) -> Result<Vec<u8>, TrzszError> {
    validate_api_version(api_version)?;
    validate_owner_id(owner_id)?;
    if length > MAX_TRANSFER_CHUNK_SIZE {
        return Err(TrzszError::ChunkTooLarge {
            requested: length,
            max: MAX_TRANSFER_CHUNK_SIZE,
        });
    }
    state.read_upload_chunk(owner_id, handle_id, offset, length)
}

pub fn close_upload_file(
    state: &TrzszState,
    owner_id: &str,
    api_version: u32,
    handle_id: &str,
) -> Result<(), TrzszError> {
    validate_api_version(api_version)?;
    validate_owner_id(owner_id)?;
    state.close_upload_handle(owner_id, handle_id)
}
