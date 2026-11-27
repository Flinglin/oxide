//! Durable atomic file replacement primitives without business-layer dependencies.

mod platform;
mod temporary;

use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::Path,
};

use temporary::{create_temporary_file, parent_directory};

/// Writes `contents` durably and atomically replaces `path`.
pub fn durable_write(path: &Path, contents: &[u8]) -> io::Result<()> {
    durable_write_with_before_replace(path, contents, || Ok(()))
}

/// Writes `contents` durably, then invokes `before_replace` immediately before replacement.
///
/// The callback is intentionally narrow so callers can preserve fault-injection semantics
/// without exposing temporary paths or platform replacement details.
pub fn durable_write_with_before_replace<F>(
    path: &Path,
    contents: &[u8],
    before_replace: F,
) -> io::Result<()>
where
    F: FnOnce() -> io::Result<()>,
{
    write_stream_and_replace(path, |file| file.write_all(contents), before_replace)
}

/// Streams contents into a private temporary file before durable atomic replacement.
pub fn durable_write_stream(
    path: &Path,
    write: impl FnOnce(&mut fs::File) -> io::Result<()>,
) -> io::Result<()> {
    write_stream_and_replace(path, write, || Ok(()))
}

fn write_stream_and_replace(
    path: &Path,
    write: impl FnOnce(&mut fs::File) -> io::Result<()>,
    before_replace: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    let parent = parent_directory(path);
    fs::create_dir_all(parent)?;
    let (temporary_path, mut temporary_file) = create_temporary_file(path)?;

    // The destination remains unchanged until all temporary contents are durable.
    let write_result = (|| {
        write(&mut temporary_file)?;
        temporary_file.flush()?;
        temporary_file.sync_all()?;
        drop(temporary_file);
        before_replace()?;
        replace_and_sync_parent(&temporary_path, path, parent)
    })();

    if write_result.is_err() {
        // Replacement removes the temporary path on success, so cleanup is harmless
        // when only the final parent-directory sync failed.
        let _ = fs::remove_file(&temporary_path);
    }
    write_result
}

/// Durably replaces `destination` with an existing file from the same directory.
pub fn durable_replace(source: &Path, destination: &Path) -> io::Result<()> {
    let destination_parent = parent_directory(destination);
    if parent_directory(source) != destination_parent {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "durable replacement requires source and destination in the same directory",
        ));
    }

    // Sync the caller-provided source before making it visible at the destination.
    // FlushFileBuffers requires write access on Windows, even for an already-written source.
    OpenOptions::new()
        .read(true)
        .write(cfg!(windows))
        .open(source)?
        .sync_all()?;
    replace_and_sync_parent(source, destination, destination_parent)
}

/// Removes `path` if present and durably records the directory entry change.
pub fn durable_remove(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => platform::sync_directory(parent_directory(path)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Persists a batch of directory-entry changes with the platform's required handle flags.
pub fn sync_directory(directory: &Path) -> io::Result<()> {
    platform::sync_directory(directory)
}

fn replace_and_sync_parent(source: &Path, destination: &Path, parent: &Path) -> io::Result<()> {
    platform::atomic_replace(source, destination)?;
    platform::sync_directory(parent)
}
