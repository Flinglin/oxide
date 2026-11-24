use std::collections::{HashMap, HashSet};

use super::model::{
    GitBranchIdentity, GitBranchListOutcome, GitBranchReference, GitChangedPath, GitOperationKind,
    GitProbeError, GitProbeOutcome, GitRepositorySnapshot, GitRepositoryStatus,
    GitStagedDiffContext, GitStagedDiffOutcome,
};
use super::probe::{SHELL_BRANCH_LIST_SENTINEL, SHELL_PROBE_SENTINEL, SHELL_STAGED_DIFF_SENTINEL};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitCommandOutput {
    pub success: bool,
    pub stdout: String,
}

impl GitCommandOutput {
    pub fn success(stdout: impl Into<String>) -> Self {
        Self {
            success: true,
            stdout: stdout.into(),
        }
    }

    pub fn failure(stdout: impl Into<String>) -> Self {
        Self {
            success: false,
            stdout: stdout.into(),
        }
    }
}

pub fn interpret_git_command_outputs(
    root: GitCommandOutput,
    branch: GitCommandOutput,
    head: GitCommandOutput,
) -> GitProbeOutcome {
    interpret_git_command_outputs_with_status_and_operation(
        root,
        branch,
        head,
        GitCommandOutput::failure(""),
        GitCommandOutput::failure(""),
    )
}

pub fn interpret_git_command_outputs_with_status(
    root: GitCommandOutput,
    branch: GitCommandOutput,
    head: GitCommandOutput,
    status: GitCommandOutput,
) -> GitProbeOutcome {
    interpret_git_command_outputs_with_status_and_operation(
        root,
        branch,
        head,
        status,
        GitCommandOutput::failure(""),
    )
}

pub fn interpret_git_command_outputs_with_status_and_operation(
    root: GitCommandOutput,
    branch: GitCommandOutput,
    head: GitCommandOutput,
    status: GitCommandOutput,
    operation: GitCommandOutput,
) -> GitProbeOutcome {
    if !root.success {
        return GitProbeOutcome::NotRepository;
    }

    let Some(repo_root) = non_empty_line(&root.stdout) else {
        return GitProbeOutcome::Error(GitProbeError::new("git root output was empty"));
    };

    if branch.success
        && let Some(branch_name) = non_empty_line(&branch.stdout)
    {
        return ready_with_status(
            repo_root,
            GitBranchIdentity::Branch(branch_name),
            parse_successful_git_status(status)
                .with_operation(parse_successful_git_operation(operation)),
        );
    }

    if head.success
        && let Some(head_name) = non_empty_line(&head.stdout)
    {
        return ready_with_status(
            repo_root,
            GitBranchIdentity::Detached(head_name),
            parse_successful_git_status(status)
                .with_operation(parse_successful_git_operation(operation)),
        );
    }

    GitProbeOutcome::Error(GitProbeError::new("git branch output was empty"))
}

pub fn parse_shell_probe_output(output: &str) -> GitProbeOutcome {
    let Some(start) = output.find(SHELL_PROBE_SENTINEL) else {
        return GitProbeOutcome::Error(GitProbeError::new("missing git probe sentinel"));
    };
    let records = output[start..].split('\0').collect::<Vec<_>>();
    let Some(position) = records
        .iter()
        .position(|part| *part == SHELL_PROBE_SENTINEL)
    else {
        return GitProbeOutcome::Error(GitProbeError::new("malformed git probe sentinel"));
    };
    let fields = &records[position + 1..];

    match field_value(fields, "state") {
        Some("not_repo") => GitProbeOutcome::NotRepository,
        Some("git_missing") => GitProbeOutcome::GitUnavailable,
        Some("cwd_missing") => GitProbeOutcome::CwdUnavailable,
        Some("repo") => {
            let Some(root) = field_value(fields, "root").filter(|value| !value.trim().is_empty())
            else {
                return GitProbeOutcome::Error(GitProbeError::new("missing git root"));
            };
            if let Some(branch) =
                field_value(fields, "branch").filter(|value| !value.trim().is_empty())
            {
                return ready_with_status(
                    root.to_string(),
                    GitBranchIdentity::Branch(branch.to_string()),
                    field_value(fields, "status")
                        .map(parse_git_status_summary)
                        .unwrap_or_default()
                        .with_operation(
                            field_value(fields, "operation").and_then(parse_git_operation_kind),
                        ),
                );
            }
            if let Some(head) =
                field_value(fields, "detached").filter(|value| !value.trim().is_empty())
            {
                return ready_with_status(
                    root.to_string(),
                    GitBranchIdentity::Detached(head.to_string()),
                    field_value(fields, "status")
                        .map(parse_git_status_summary)
                        .unwrap_or_default()
                        .with_operation(
                            field_value(fields, "operation").and_then(parse_git_operation_kind),
                        ),
                );
            }
            GitProbeOutcome::Error(GitProbeError::new("missing git branch"))
        }
        Some(_) => GitProbeOutcome::Error(GitProbeError::new("unknown git probe state")),
        None => GitProbeOutcome::Error(GitProbeError::new("missing git probe state")),
    }
}

pub fn parse_git_operation_kind(value: &str) -> Option<GitOperationKind> {
    match value.trim() {
        "merge" => Some(GitOperationKind::Merge),
        "rebase" => Some(GitOperationKind::Rebase),
        "cherry_pick" => Some(GitOperationKind::CherryPick),
        "revert" => Some(GitOperationKind::Revert),
        _ => None,
    }
}

pub fn parse_git_status_summary(output: &str) -> GitRepositoryStatus {
    let mut upstream = None;
    let mut ahead = 0;
    let mut behind = 0;
    let mut staged = 0;
    let mut modified = 0;
    let mut untracked = 0;
    let mut conflicts = 0;
    let mut paths = Vec::new();

    for line in output.lines() {
        if let Some(value) = line.strip_prefix("# branch.upstream ") {
            upstream = Some(value.to_string());
            continue;
        }
        if let Some(value) = line.strip_prefix("# branch.ab ") {
            let (parsed_ahead, parsed_behind) = parse_branch_ahead_behind(value);
            ahead = parsed_ahead;
            behind = parsed_behind;
            continue;
        }
        if let Some(status) = porcelain_v2_xy(line, '1').or_else(|| porcelain_v2_xy(line, '2')) {
            if status.0 != '.' {
                staged += 1;
            }
            if status.1 != '.' {
                modified += 1;
            }
            if let Some(path) = parse_porcelain_v2_changed_path(line, status.0, status.1) {
                paths.push(path);
            }
            continue;
        }
        if line.starts_with("u ") {
            conflicts += 1;
            if let Some(path) = parse_porcelain_v2_conflict_path(line) {
                paths.push(path);
            }
            continue;
        }
        if let Some(path) = line.strip_prefix("? ") {
            untracked += 1;
            if let Some(path) =
                GitChangedPath::from_parts(path, None::<String>, false, false, true, false)
            {
                paths.push(path);
            }
        }
    }

    GitRepositoryStatus::new(
        upstream, ahead, behind, staged, modified, untracked, conflicts,
    )
    .with_paths(paths)
}

pub fn interpret_git_branch_list_output(branches: GitCommandOutput) -> GitBranchListOutcome {
    interpret_git_branch_list_outputs(branches, GitCommandOutput::failure(""))
}

pub fn interpret_git_branch_list_outputs(
    branches: GitCommandOutput,
    worktrees: GitCommandOutput,
) -> GitBranchListOutcome {
    if !branches.success {
        return GitBranchListOutcome::NotRepository;
    }
    let worktree_paths = worktrees
        .success
        .then(|| parse_worktree_branch_paths(&worktrees.stdout))
        .unwrap_or_default();
    GitBranchListOutcome::Ready(parse_branch_list_lines(&branches.stdout, &worktree_paths))
}

pub fn parse_shell_branch_list_output(output: &str) -> GitBranchListOutcome {
    let fields = match shell_fields_after_sentinel(output, SHELL_BRANCH_LIST_SENTINEL) {
        Ok(fields) => fields,
        Err(error) => return GitBranchListOutcome::Error(error),
    };

    match field_value(&fields, "state") {
        Some("ok") => GitBranchListOutcome::Ready(parse_branch_fields(&fields)),
        Some("not_repo") => GitBranchListOutcome::NotRepository,
        Some("git_missing") => GitBranchListOutcome::GitUnavailable,
        Some("cwd_missing") => GitBranchListOutcome::CwdUnavailable,
        Some(_) => GitBranchListOutcome::Error(GitProbeError::new("unknown git branch state")),
        None => GitBranchListOutcome::Error(GitProbeError::new("missing git branch state")),
    }
}

pub fn interpret_git_staged_diff_outputs(
    stat: GitCommandOutput,
    patch: GitCommandOutput,
) -> GitStagedDiffOutcome {
    if !stat.success || !patch.success {
        return GitStagedDiffOutcome::Error(GitProbeError::new("git staged diff failed"));
    }
    GitStagedDiffContext::new(stat.stdout, patch.stdout)
        .map(GitStagedDiffOutcome::Ready)
        .unwrap_or(GitStagedDiffOutcome::Empty)
}

pub fn parse_shell_staged_diff_output(output: &str) -> GitStagedDiffOutcome {
    let fields = match shell_fields_after_sentinel(output, SHELL_STAGED_DIFF_SENTINEL) {
        Ok(fields) => fields,
        Err(error) => return GitStagedDiffOutcome::Error(error),
    };

    match field_value(&fields, "state") {
        Some("ok") => {
            let stat = field_value(&fields, "stat").unwrap_or_default();
            let patch = field_value(&fields, "patch").unwrap_or_default();
            GitStagedDiffContext::new(stat.to_string(), patch.to_string())
                .map(GitStagedDiffOutcome::Ready)
                .unwrap_or(GitStagedDiffOutcome::Empty)
        }
        Some("empty") => GitStagedDiffOutcome::Empty,
        Some("not_repo") => GitStagedDiffOutcome::NotRepository,
        Some("git_missing") => GitStagedDiffOutcome::GitUnavailable,
        Some("cwd_missing") => GitStagedDiffOutcome::CwdUnavailable,
        Some(_) => GitStagedDiffOutcome::Error(GitProbeError::new("unknown git diff state")),
        None => GitStagedDiffOutcome::Error(GitProbeError::new("missing git diff state")),
    }
}

fn shell_fields_after_sentinel<'a>(
    output: &'a str,
    sentinel: &str,
) -> Result<Vec<&'a str>, GitProbeError> {
    let Some(start) = output.find(sentinel) else {
        return Err(GitProbeError::new("missing git shell sentinel"));
    };
    let records = output[start..].split('\0').collect::<Vec<_>>();
    let Some(position) = records.iter().position(|part| *part == sentinel) else {
        return Err(GitProbeError::new("malformed git shell sentinel"));
    };
    Ok(records[position + 1..].to_vec())
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct GitBranchCandidate {
    name: String,
    current: bool,
}

impl GitBranchCandidate {
    fn local(name: impl Into<String>, current: bool) -> Option<Self> {
        candidate(name, current)
    }
}

fn candidate(name: impl Into<String>, current: bool) -> Option<GitBranchCandidate> {
    let name = name.into();
    let name = name.trim();
    (!name.is_empty()).then(|| GitBranchCandidate {
        name: name.to_string(),
        current,
    })
}

fn parse_branch_list_lines(
    output: &str,
    worktree_paths: &HashMap<String, String>,
) -> Vec<GitBranchReference> {
    branch_references_from_candidates(
        output.lines().filter_map(parse_branch_list_line),
        worktree_paths,
    )
}

fn parse_branch_list_line(line: &str) -> Option<GitBranchCandidate> {
    let columns = line.split('\t').collect::<Vec<_>>();
    match columns.as_slice() {
        [marker, name] => GitBranchCandidate::local(*name, marker.trim() == "*"),
        [marker, refname, short_name] => {
            parse_branch_ref_columns(marker.trim() == "*", refname, short_name)
        }
        _ => None,
    }
}

fn parse_branch_ref_columns(
    current: bool,
    refname: &str,
    short_name: &str,
) -> Option<GitBranchCandidate> {
    if let Some(name) = refname.strip_prefix("refs/heads/") {
        return GitBranchCandidate::local(name, current);
    }

    // Older callers may still pass refname plus short_name columns. Keep
    // local refs compatible but never turn remote refs into branch actions.
    refname
        .is_empty()
        .then(|| GitBranchCandidate::local(short_name, current))
        .flatten()
}

fn parse_branch_fields(fields: &[&str]) -> Vec<GitBranchReference> {
    let mut branches = Vec::new();
    let mut worktree_paths = HashMap::new();
    let mut index = 0;
    while index + 1 < fields.len() {
        match fields[index] {
            "branch" => {
                let name = fields[index + 1];
                index += 2;
                let mut current = false;
                while index + 1 < fields.len() && !matches!(fields[index], "branch" | "worktree") {
                    if fields[index] == "current" {
                        current = matches!(fields[index + 1], "1" | "true" | "*");
                    }
                    index += 2;
                }
                if let Some(branch) = candidate(name, current) {
                    branches.push(branch);
                }
            }
            "worktree" => {
                let name = fields[index + 1];
                index += 2;
                let mut path = None;
                while index + 1 < fields.len() && !matches!(fields[index], "branch" | "worktree") {
                    if fields[index] == "path" {
                        path = Some(fields[index + 1]);
                    }
                    index += 2;
                }
                if let Some(path) = path
                    && !name.trim().is_empty()
                    && !path.trim().is_empty()
                    && !git_worktree_path_is_administrative(path)
                {
                    worktree_paths.insert(name.to_string(), path.to_string());
                }
            }
            _ => index += 2,
        }
    }
    branch_references_from_candidates(branches, &worktree_paths)
}

fn branch_references_from_candidates(
    candidates: impl IntoIterator<Item = GitBranchCandidate>,
    worktree_paths: &HashMap<String, String>,
) -> Vec<GitBranchReference> {
    let mut emitted = HashSet::new();
    let mut branches = Vec::new();
    for candidate in candidates {
        if !emitted.insert(candidate.name.clone()) {
            continue;
        }
        if let Some(path) = worktree_paths.get(&candidate.name) {
            if let Some(branch) = GitBranchReference::with_worktree_path(
                candidate.name,
                candidate.current,
                Some(path.clone()),
            ) {
                branches.push(branch);
            }
        } else if let Some(branch) = GitBranchReference::new(candidate.name, candidate.current) {
            branches.push(branch);
        }
    }
    branches
}

fn parse_worktree_branch_paths(output: &str) -> HashMap<String, String> {
    let mut branch_paths = HashMap::new();
    let mut worktree_path: Option<&str> = None;
    let mut worktree_branch: Option<&str> = None;
    let mut worktree_prunable = false;

    for line in output.lines() {
        if line.is_empty() {
            insert_worktree_branch_path(
                &mut branch_paths,
                worktree_branch,
                worktree_path,
                worktree_prunable,
            );
            worktree_path = None;
            worktree_branch = None;
            worktree_prunable = false;
            continue;
        }
        if let Some(path) = line.strip_prefix("worktree ") {
            worktree_path = Some(path);
            continue;
        }
        if let Some(branch) = line.strip_prefix("branch refs/heads/") {
            worktree_branch = Some(branch);
            continue;
        }
        if line.starts_with("prunable") {
            worktree_prunable = true;
            continue;
        }
    }
    insert_worktree_branch_path(
        &mut branch_paths,
        worktree_branch,
        worktree_path,
        worktree_prunable,
    );

    branch_paths
}

fn insert_worktree_branch_path(
    branch_paths: &mut HashMap<String, String>,
    branch: Option<&str>,
    path: Option<&str>,
    prunable: bool,
) {
    let (Some(branch), Some(path)) = (branch, path) else {
        return;
    };
    if prunable
        || branch.trim().is_empty()
        || path.trim().is_empty()
        || git_worktree_path_is_administrative(path)
    {
        return;
    }
    branch_paths.insert(branch.to_string(), path.to_string());
}

fn git_worktree_path_is_administrative(path: &str) -> bool {
    let components = path
        .split(['/', '\\'])
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    components
        .windows(3)
        .any(|window| window[0] == ".git" && matches!(window[1], "modules" | "worktrees"))
}

fn parse_successful_git_status(status: GitCommandOutput) -> GitRepositoryStatus {
    status
        .success
        .then(|| parse_git_status_summary(&status.stdout))
        .unwrap_or_default()
}

fn parse_successful_git_operation(operation: GitCommandOutput) -> Option<GitOperationKind> {
    operation
        .success
        .then(|| parse_git_operation_kind(&operation.stdout))
        .flatten()
}

fn parse_branch_ahead_behind(value: &str) -> (u32, u32) {
    let mut ahead = 0;
    let mut behind = 0;
    for part in value.split_whitespace() {
        if let Some(value) = part.strip_prefix('+') {
            ahead = value.parse().unwrap_or(0);
        } else if let Some(value) = part.strip_prefix('-') {
            behind = value.parse().unwrap_or(0);
        }
    }
    (ahead, behind)
}

fn porcelain_v2_xy(line: &str, record_kind: char) -> Option<(char, char)> {
    let mut parts = line.split_whitespace();
    (parts.next()?.chars().next()? == record_kind).then_some(())?;
    let xy = parts.next()?;
    let mut chars = xy.chars();
    let x = chars.next()?;
    let y = chars.next()?;
    Some((x, y))
}

fn parse_porcelain_v2_changed_path(
    line: &str,
    staged: char,
    modified: char,
) -> Option<GitChangedPath> {
    let record_kind = line.chars().next()?;
    let path_field = match record_kind {
        '1' => line.splitn(9, ' ').nth(8)?,
        '2' => line.splitn(10, ' ').nth(9)?,
        _ => return None,
    };
    let (path, original_path) = split_porcelain_path_pair(path_field);
    GitChangedPath::from_parts(
        path,
        original_path,
        staged != '.',
        modified != '.',
        false,
        false,
    )
}

fn parse_porcelain_v2_conflict_path(line: &str) -> Option<GitChangedPath> {
    let path = line.splitn(11, ' ').nth(10)?;
    GitChangedPath::from_parts(path, None::<String>, false, false, false, true)
}

fn split_porcelain_path_pair(path_field: &str) -> (&str, Option<&str>) {
    path_field
        .split_once('\t')
        .map(|(path, original)| (path, Some(original)))
        .unwrap_or((path_field, None))
}

fn field_value<'a>(fields: &'a [&str], key: &str) -> Option<&'a str> {
    fields.chunks_exact(2).find_map(|chunk| {
        if chunk[0] == key {
            Some(chunk[1])
        } else {
            None
        }
    })
}

fn non_empty_line(output: &str) -> Option<String> {
    output
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(ToOwned::to_owned)
}

fn ready_with_status(
    repo_root: String,
    branch: GitBranchIdentity,
    status: GitRepositoryStatus,
) -> GitProbeOutcome {
    GitRepositorySnapshot::with_status(repo_root, branch, status)
        .map(GitProbeOutcome::Ready)
        .unwrap_or_else(|| GitProbeOutcome::Error(GitProbeError::new("invalid git snapshot")))
}
