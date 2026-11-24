use std::fs::{self, OpenOptions};
use std::path::PathBuf;

use crate::error::TrzszError;
use crate::path_guard::{
    build_download_target_path, canonicalize_existing_root, ensure_within_root,
    sanitize_download_rel_path, validate_api_version, validate_owner_id,
};
use crate::{
    MAX_TRANSFER_CHUNK_SIZE, TrzszCreateDownloadDirectoryDto, TrzszDownloadOpenDto,
    TrzszPreparedDownloadRootDto, TrzszState,
};

pub fn prepare_download_root(
    state: &TrzszState,
    owner_id: &str,
    api_version: u32,
    root_path: String,
) -> Result<TrzszPreparedDownloadRootDto, TrzszError> {
    validate_api_version(api_version)?;
    validate_owner_id(owner_id)?;
    let canonical_root = canonicalize_existing_root(&root_path)?;
    Ok(state.prepare_download_root(owner_id, canonical_root))
}

pub fn open_save_file(
    state: &TrzszState,
    owner_id: &str,
    api_version: u32,
    root_path: String,
    file_name: String,
    _directory: bool,
    overwrite: bool,
) -> Result<TrzszDownloadOpenDto, TrzszError> {
    validate_api_version(api_version)?;
    validate_owner_id(owner_id)?;

    let prepared_root = state
        .prepared_download_root(owner_id)
        .ok_or(TrzszError::RootNotPrepared)?;
    let requested_root = canonicalize_existing_root(&root_path)?;
    if prepared_root != requested_root {
        return Err(TrzszError::RootMismatch);
    }

    let rel_components = sanitize_download_rel_path(&file_name)?;
    let final_path = build_download_target_path(&prepared_root, &rel_components)?;
    ensure_within_root(&prepared_root, &final_path)?;

    if let Ok(metadata) = fs::symlink_metadata(&final_path) {
        if metadata.file_type().is_symlink() {
            return Err(TrzszError::SymlinkNotAllowed(
                final_path.display().to_string(),
            ));
        }
        if metadata.is_dir() {
            return Err(TrzszError::InvalidPath(format!(
                "Target path resolves to a directory: {}",
                final_path.display()
            )));
        }
        if !overwrite {
            return Err(TrzszError::AlreadyExists(final_path.display().to_string()));
        }
    }

    let local_name = rel_components
        .last()
        .cloned()
        .ok_or_else(|| TrzszError::InvalidPath("Empty file name".to_string()))?;
    let temp_path = build_temp_path(&final_path, &local_name)?;
    let file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp_path)?;

    state.register_download_handle(
        owner_id,
        local_name.clone(),
        local_name,
        prepared_root,
        final_path,
        temp_path,
        overwrite,
        file,
    )
}

pub fn create_download_directory(
    state: &TrzszState,
    owner_id: &str,
    api_version: u32,
    root_path: String,
    directory_path: String,
    must_create: bool,
) -> Result<TrzszCreateDownloadDirectoryDto, TrzszError> {
    validate_api_version(api_version)?;
    validate_owner_id(owner_id)?;

    let prepared_root = state
        .prepared_download_root(owner_id)
        .ok_or(TrzszError::RootNotPrepared)?;
    let requested_root = canonicalize_existing_root(&root_path)?;
    if prepared_root != requested_root {
        return Err(TrzszError::RootMismatch);
    }

    let rel_components = sanitize_download_rel_path(&directory_path)?;
    if rel_components.len() > 1 {
        let mut parent_path = prepared_root.clone();
        for component in &rel_components[..rel_components.len() - 1] {
            parent_path.push(component);
            match fs::symlink_metadata(&parent_path) {
                Ok(metadata) => {
                    if metadata.file_type().is_symlink() {
                        return Err(TrzszError::SymlinkNotAllowed(
                            parent_path.display().to_string(),
                        ));
                    }
                    if !metadata.is_dir() {
                        return Err(TrzszError::InvalidPath(format!(
                            "Parent path is not a directory: {}",
                            parent_path.display()
                        )));
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Err(TrzszError::InvalidPath(format!(
                        "Parent directory does not exist: {}",
                        parent_path.display()
                    )));
                }
                Err(error) => return Err(TrzszError::Io(error)),
            }
        }
    }
    let final_path = build_download_target_path(&prepared_root, &rel_components)?;
    ensure_within_root(&prepared_root, &final_path)?;

    match fs::symlink_metadata(&final_path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                return Err(TrzszError::SymlinkNotAllowed(
                    final_path.display().to_string(),
                ));
            }
            if metadata.is_dir() {
                if must_create {
                    return Err(TrzszError::AlreadyExists(final_path.display().to_string()));
                }
                return Ok(TrzszCreateDownloadDirectoryDto { created: false });
            }
            return Err(TrzszError::InvalidPath(format!(
                "Target path resolves to a file: {}",
                final_path.display()
            )));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(TrzszError::Io(error)),
    }

    fs::create_dir(&final_path)?;
    state.register_download_directory(owner_id, final_path);
    Ok(TrzszCreateDownloadDirectoryDto { created: true })
}

pub fn commit_download_directory(
    state: &TrzszState,
    owner_id: &str,
    api_version: u32,
    root_path: String,
    directory_path: String,
) -> Result<(), TrzszError> {
    validate_api_version(api_version)?;
    validate_owner_id(owner_id)?;

    let prepared_root = state
        .prepared_download_root(owner_id)
        .ok_or(TrzszError::RootNotPrepared)?;
    let requested_root = canonicalize_existing_root(&root_path)?;
    if prepared_root != requested_root {
        return Err(TrzszError::RootMismatch);
    }

    let rel_components = sanitize_download_rel_path(&directory_path)?;
    let final_path = build_download_target_path(&prepared_root, &rel_components)?;
    ensure_within_root(&prepared_root, &final_path)?;
    state.commit_download_directory(owner_id, &final_path);
    Ok(())
}

pub fn remove_download_directory(
    state: &TrzszState,
    owner_id: &str,
    api_version: u32,
    root_path: String,
    directory_path: String,
) -> Result<(), TrzszError> {
    validate_api_version(api_version)?;
    validate_owner_id(owner_id)?;

    let prepared_root = state
        .prepared_download_root(owner_id)
        .ok_or(TrzszError::RootNotPrepared)?;
    let requested_root = canonicalize_existing_root(&root_path)?;
    if prepared_root != requested_root {
        return Err(TrzszError::RootMismatch);
    }

    let rel_components = sanitize_download_rel_path(&directory_path)?;
    let final_path = build_download_target_path(&prepared_root, &rel_components)?;
    ensure_within_root(&prepared_root, &final_path)?;

    match fs::symlink_metadata(&final_path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                return Err(TrzszError::SymlinkNotAllowed(
                    final_path.display().to_string(),
                ));
            }
            if !metadata.is_dir() {
                return Err(TrzszError::InvalidPath(format!(
                    "Target path is not a directory: {}",
                    final_path.display()
                )));
            }
            match fs::remove_dir(&final_path) {
                Ok(()) => {
                    state.commit_download_directory(owner_id, &final_path);
                    Ok(())
                }
                Err(error) if error.kind() == std::io::ErrorKind::DirectoryNotEmpty => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    state.commit_download_directory(owner_id, &final_path);
                    Ok(())
                }
                Err(error) => Err(TrzszError::Io(error)),
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            state.commit_download_directory(owner_id, &final_path);
            Ok(())
        }
        Err(error) => Err(TrzszError::Io(error)),
    }
}

pub fn remove_download_file(
    state: &TrzszState,
    owner_id: &str,
    api_version: u32,
    root_path: String,
    file_path: String,
) -> Result<(), TrzszError> {
    validate_api_version(api_version)?;
    validate_owner_id(owner_id)?;

    let prepared_root = state
        .prepared_download_root(owner_id)
        .ok_or(TrzszError::RootNotPrepared)?;
    let requested_root = canonicalize_existing_root(&root_path)?;
    if prepared_root != requested_root {
        return Err(TrzszError::RootMismatch);
    }

    let rel_components = sanitize_download_rel_path(&file_path)?;
    let final_path = build_download_target_path(&prepared_root, &rel_components)?;
    ensure_within_root(&prepared_root, &final_path)?;

    match fs::symlink_metadata(&final_path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                return Err(TrzszError::SymlinkNotAllowed(
                    final_path.display().to_string(),
                ));
            }
            if metadata.is_dir() {
                return Err(TrzszError::InvalidPath(format!(
                    "Target path is a directory: {}",
                    final_path.display()
                )));
            }
            match fs::remove_file(&final_path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(TrzszError::Io(error)),
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(TrzszError::Io(error)),
    }
}

pub fn write_download_chunk(
    state: &TrzszState,
    owner_id: &str,
    api_version: u32,
    writer_id: &str,
    data: Vec<u8>,
) -> Result<(), TrzszError> {
    validate_api_version(api_version)?;
    validate_owner_id(owner_id)?;
    if data.len() > MAX_TRANSFER_CHUNK_SIZE {
        return Err(TrzszError::ChunkTooLarge {
            requested: data.len(),
            max: MAX_TRANSFER_CHUNK_SIZE,
        });
    }
    state.write_download_chunk(owner_id, writer_id, &data)
}

pub fn finish_download_file(
    state: &TrzszState,
    owner_id: &str,
    api_version: u32,
    writer_id: &str,
) -> Result<(), TrzszError> {
    validate_api_version(api_version)?;
    validate_owner_id(owner_id)?;
    state.finish_download_handle(owner_id, writer_id)
}

pub fn abort_download_file(
    state: &TrzszState,
    owner_id: &str,
    api_version: u32,
    writer_id: &str,
) -> Result<(), TrzszError> {
    validate_api_version(api_version)?;
    validate_owner_id(owner_id)?;
    state.abort_download_handle(owner_id, writer_id)
}

fn build_temp_path(final_path: &PathBuf, local_name: &str) -> Result<PathBuf, TrzszError> {
    let parent = final_path.parent().ok_or_else(|| {
        TrzszError::InvalidPath(format!(
            "Final path has no parent: {}",
            final_path.display()
        ))
    })?;
    let stem = format!(".{local_name}.oxide-trzsz-{}.part", uuid::Uuid::new_v4());
    Ok(parent.join(stem))
}
