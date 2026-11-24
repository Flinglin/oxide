use std::path::Path;

pub fn copy_recursively(source: &Path, target: &Path) -> std::io::Result<()> {
    copy_recursively_with_progress(source, target, &mut |_, _| {})
}

pub fn local_operation_unit_count(path: &Path) -> usize {
    if !path.is_dir() {
        return 1;
    }
    walkdir::WalkDir::new(path)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.path() != path)
        .count()
        .saturating_add(1)
}

/// Emits one result per object: copied bytes for success, or `None` for the failed path.
pub fn copy_recursively_with_progress(
    source: &Path,
    target: &Path,
    progress: &mut impl FnMut(&Path, Option<u64>),
) -> std::io::Result<()> {
    let metadata = std::fs::symlink_metadata(source).inspect_err(|_| progress(source, None))?;
    if metadata.is_dir() {
        std::fs::create_dir_all(target).inspect_err(|_| progress(source, None))?;
        let entries = std::fs::read_dir(source).inspect_err(|_| progress(source, None))?;
        for entry in entries {
            let entry = entry.inspect_err(|_| progress(source, None))?;
            copy_recursively_with_progress(
                &entry.path(),
                &target.join(entry.file_name()),
                progress,
            )?;
        }
        progress(source, Some(0));
    } else {
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).inspect_err(|_| progress(source, None))?;
        }
        let copied = std::fs::copy(source, target).inspect_err(|_| progress(source, None))?;
        progress(source, Some(copied));
    }
    Ok(())
}
