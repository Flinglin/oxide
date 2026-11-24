//! Persistent storage for the application background-image gallery.

use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow};

const BACKGROUND_DIRECTORY_NAME: &str = "backgrounds";
const BACKGROUND_FILE_PREFIX: &str = "background";

/// Returns whether a path can be imported into the background media gallery.
pub fn is_supported_background_image(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" | "mp4" | "m4v"
            )
        })
        .unwrap_or(false)
}

/// Resolves the managed gallery beside the active settings file.
pub fn background_images_directory(settings_path: &Path) -> PathBuf {
    settings_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(BACKGROUND_DIRECTORY_NAME)
}

/// Installs a versioned bundled image into the managed gallery without replacing user files.
pub fn ensure_bundled_background_image(
    settings_path: &Path,
    file_name: &str,
    image_bytes: &[u8],
) -> Result<PathBuf> {
    let file_name_path = Path::new(file_name);
    if file_name_path.components().count() != 1
        || !matches!(
            file_name_path.components().next(),
            Some(Component::Normal(_))
        )
        || !is_supported_background_image(file_name_path)
    {
        return Err(anyhow!("invalid bundled background image filename"));
    }

    let directory = background_images_directory(settings_path);
    fs::create_dir_all(&directory).with_context(|| {
        format!(
            "failed to create background gallery {}",
            directory.display()
        )
    })?;
    let destination = directory.join(file_name_path);
    if destination.is_file() {
        return Ok(destination);
    }
    if destination.exists() {
        return Err(anyhow!(
            "bundled background destination is not a file: {}",
            destination.display()
        ));
    }

    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&destination)
    {
        Ok(mut destination_file) => {
            if let Err(error) = destination_file.write_all(image_bytes) {
                // Never expose a partially written bundled asset in the gallery.
                let _ = fs::remove_file(&destination);
                return Err(error).with_context(|| {
                    format!(
                        "failed to install bundled background image {}",
                        destination.display()
                    )
                });
            }
            Ok(destination)
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists && destination.is_file() => {
            // Another process may finish the same first-run installation concurrently.
            Ok(destination)
        }
        Err(error) => Err(error).with_context(|| {
            format!(
                "failed to create bundled background image {}",
                destination.display()
            )
        }),
    }
}

/// Returns whether an existing image is owned by the managed gallery.
pub fn is_managed_background_image(settings_path: &Path, image_path: &Path) -> bool {
    let directory = background_images_directory(settings_path);
    let Ok(canonical_directory) = directory.canonicalize() else {
        return false;
    };
    let Ok(canonical_image) = image_path.canonicalize() else {
        return false;
    };
    canonical_image.starts_with(canonical_directory)
}

/// Lists managed gallery images from newest to oldest without reading image bytes.
pub fn list_background_images(settings_path: &Path) -> Result<Vec<PathBuf>> {
    let directory = background_images_directory(settings_path);
    if !directory.exists() {
        return Ok(Vec::new());
    }

    let mut images = Vec::new();
    for entry in fs::read_dir(&directory)
        .with_context(|| format!("failed to read background gallery {}", directory.display()))?
    {
        let entry = entry.with_context(|| {
            format!(
                "failed to read an entry from background gallery {}",
                directory.display()
            )
        })?;
        let path = entry.path();
        if !entry.file_type()?.is_file() || !is_supported_background_image(&path) {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .unwrap_or(UNIX_EPOCH);
        images.push((path, modified));
    }

    // Modification time matches the gallery's user-facing "newest first" order.
    images.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| right.0.cmp(&left.0)));
    Ok(images.into_iter().map(|(path, _)| path).collect())
}

/// Copies selected source images into the managed gallery and returns stored paths.
pub fn import_background_images(
    settings_path: &Path,
    source_paths: &[PathBuf],
) -> Result<Vec<PathBuf>> {
    let directory = background_images_directory(settings_path);
    fs::create_dir_all(&directory).with_context(|| {
        format!(
            "failed to create background gallery {}",
            directory.display()
        )
    })?;

    // Validate the complete batch before copying so an invalid selection cannot
    // leave only a prefix of the requested gallery update behind.
    for source_path in source_paths {
        if !source_path.is_file() {
            return Err(anyhow!(
                "background image does not exist: {}",
                source_path.display()
            ));
        }
        if !is_supported_background_image(source_path) {
            return Err(anyhow!(
                "unsupported background image format: {}",
                source_path.display()
            ));
        }
    }

    let mut imported_paths = Vec::with_capacity(source_paths.len());
    for source_path in source_paths {
        match copy_background_image(source_path, &directory) {
            Ok(imported_path) => imported_paths.push(imported_path),
            Err(error) => {
                // Roll back files from this batch when a later copy fails.
                for imported_path in &imported_paths {
                    let _ = fs::remove_file(imported_path);
                }
                return Err(error);
            }
        }
    }
    Ok(imported_paths)
}

/// Imports versioned pack media without replacing existing gallery files.
pub fn import_background_pack(
    settings_path: &Path,
    pack_key: &str,
    source_paths: &[PathBuf],
) -> Result<Vec<PathBuf>> {
    let directory = background_images_directory(settings_path);
    let mut destinations = Vec::with_capacity(source_paths.len());
    for source in source_paths {
        if !source.is_file() || !is_supported_background_image(source) {
            return Err(anyhow!("invalid background pack media"));
        }
        let name = source
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| anyhow!("invalid background pack filename"))?;
        let name = format!("pack-{pack_key}-{name}");
        let path = Path::new(&name);
        if path.components().count() != 1
            || !matches!(path.components().next(), Some(Component::Normal(_)))
        {
            return Err(anyhow!("invalid background pack identity"));
        }
        destinations.push(directory.join(name));
    }

    let mut imported = Vec::new();
    let mut created = Vec::new();
    for (source, destination) in source_paths.iter().zip(destinations) {
        let existed = destination.is_file();
        let result = if existed {
            Ok(destination.clone())
        } else {
            fs::read(source)
                .context("failed to read background pack media")
                .and_then(|bytes| {
                    ensure_bundled_background_image(
                        settings_path,
                        destination.file_name().unwrap().to_str().unwrap(),
                        &bytes,
                    )
                })
        };
        match result {
            Ok(path) => {
                if !existed {
                    created.push(path.clone());
                }
                imported.push(path);
            }
            Err(error) => {
                // The gallery owns these copies; a failed batch cannot leave partial imports.
                for path in created {
                    let _ = fs::remove_file(path);
                }
                return Err(error);
            }
        }
    }
    Ok(imported)
}

/// Deletes one managed image while refusing paths outside the gallery directory.
pub fn remove_background_image(settings_path: &Path, image_path: &Path) -> Result<()> {
    if !image_path.exists() {
        return Ok(());
    }

    let directory = background_images_directory(settings_path);
    let canonical_directory = directory.canonicalize().with_context(|| {
        format!(
            "failed to resolve background gallery {}",
            directory.display()
        )
    })?;
    let canonical_image = image_path.canonicalize().with_context(|| {
        format!(
            "failed to resolve background image {}",
            image_path.display()
        )
    })?;
    if !canonical_image.starts_with(&canonical_directory) {
        return Err(anyhow!(
            "refusing to delete a file outside the background gallery"
        ));
    }

    fs::remove_file(&canonical_image).with_context(|| {
        format!(
            "failed to remove background image {}",
            canonical_image.display()
        )
    })
}

/// Removes every file owned by the managed background gallery.
pub fn clear_background_images(settings_path: &Path) -> Result<()> {
    let directory = background_images_directory(settings_path);
    if !directory.exists() {
        return Ok(());
    }

    for entry in fs::read_dir(&directory)
        .with_context(|| format!("failed to read background gallery {}", directory.display()))?
    {
        let entry = entry.with_context(|| {
            format!(
                "failed to read an entry from background gallery {}",
                directory.display()
            )
        })?;
        if entry.file_type()?.is_file() {
            fs::remove_file(entry.path()).with_context(|| {
                format!(
                    "failed to clear background image {}",
                    entry.path().display()
                )
            })?;
        }
    }
    Ok(())
}

fn copy_background_image(source_path: &Path, directory: &Path) -> Result<PathBuf> {
    let extension = source_path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| anyhow!("background image has no file extension"))?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut collision_index = 0_u32;

    loop {
        let destination = directory.join(format!(
            "{BACKGROUND_FILE_PREFIX}_{timestamp}_{collision_index}.{extension}"
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)
        {
            Ok(mut destination_file) => {
                let copy_result = fs::File::open(source_path)
                    .and_then(|mut source_file| io::copy(&mut source_file, &mut destination_file));
                if let Err(error) = copy_result {
                    // A partial gallery file must never be exposed as a selectable image.
                    let _ = fs::remove_file(&destination);
                    return Err(error).with_context(|| {
                        format!("failed to copy background image {}", source_path.display())
                    });
                }
                return Ok(destination);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                collision_index = collision_index
                    .checked_add(1)
                    .ok_or_else(|| anyhow!("background image filename space exhausted"))?;
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "failed to create a stored background for {}",
                        source_path.display()
                    )
                });
            }
        }
    }
}
