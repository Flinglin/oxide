//! Trusted Git CWD selection and local Git control-directory inspection.

use std::path::{Path, PathBuf};

use crate::cwd::{CurrentDirectoryScope, CurrentDirectorySnapshot, CurrentDirectorySource};

use super::model::{GitOperationKind, GitProbeScope};

/// Selects a scoped CWD snapshot that is trusted for Git probing.
pub fn git_cwd_from_directory_snapshot(
    scope: &GitProbeScope,
    snapshot: &CurrentDirectorySnapshot,
) -> Option<String> {
    match (scope, snapshot.scope()) {
        (GitProbeScope::Local, CurrentDirectoryScope::Local) => {
            Some(expand_local_git_home(snapshot.path()))
        }
        (GitProbeScope::SshNode(expected), CurrentDirectoryScope::SshNode(actual))
            if expected == actual && remote_git_cwd_source_is_trusted(snapshot.source()) =>
        {
            Some(snapshot.path().to_string())
        }
        _ => None,
    }
}

/// Chooses the authoritative CWD while rejecting visible-text fallback for SSH.
pub fn preferred_git_cwd(
    scope: &GitProbeScope,
    snapshot_cwd: Option<String>,
    visible_cwd: Option<String>,
) -> Option<String> {
    if matches!(scope, GitProbeScope::SshNode(_)) {
        return snapshot_cwd;
    }
    snapshot_cwd.or(visible_cwd)
}

/// Expands local `~` and `~/...` paths before passing them to `git -C`.
pub fn expand_local_git_home(cwd: &str) -> String {
    expand_local_git_home_with(cwd, local_home().as_deref())
}

/// Detects the active operation from Git's control files and directories.
pub fn git_operation_kind_from_git_dir(git_dir: &Path) -> Option<GitOperationKind> {
    if git_dir.join("rebase-merge").is_dir() || git_dir.join("rebase-apply").is_dir() {
        Some(GitOperationKind::Rebase)
    } else if git_dir.join("MERGE_HEAD").is_file() {
        Some(GitOperationKind::Merge)
    } else if git_dir.join("CHERRY_PICK_HEAD").is_file() {
        Some(GitOperationKind::CherryPick)
    } else if git_dir.join("REVERT_HEAD").is_file() {
        Some(GitOperationKind::Revert)
    } else {
        None
    }
}

fn remote_git_cwd_source_is_trusted(source: CurrentDirectorySource) -> bool {
    matches!(
        source,
        CurrentDirectorySource::ShellIntegration | CurrentDirectorySource::UserAction
    )
}

fn local_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn expand_local_git_home_with(cwd: &str, home: Option<&Path>) -> String {
    if cwd == "~" {
        return home
            .map(|home| home.to_string_lossy().to_string())
            .unwrap_or_else(|| cwd.to_string());
    }
    if let Some(rest) = cwd.strip_prefix("~/") {
        if let Some(home) = home {
            return home.join(rest).to_string_lossy().to_string();
        }
    }
    cwd.to_string()
}
