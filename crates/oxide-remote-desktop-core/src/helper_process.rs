use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant, SystemTime},
};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use crate::{
    RemoteDesktopHelperRequest, RemoteDesktopJsonLineError, RemoteDesktopProviderManifest,
    write_request_line,
};

#[cfg(windows)]
const HELPER_CREATE_NO_WINDOW: u32 = 0x08000000;

pub(crate) struct RemoteDesktopHelperProcess {
    pub child: Child,
    pub stdin: ChildStdin,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedRemoteDesktopHelper {
    pub command: PathBuf,
    pub prefix_args: Vec<String>,
    pub working_dir: Option<PathBuf>,
}

pub(crate) fn spawn_remote_desktop_helper(
    provider: &RemoteDesktopProviderManifest,
) -> Result<RemoteDesktopHelperProcess, std::io::Error> {
    let resolved = resolve_remote_desktop_helper_command(&provider.entry.command);
    let mut command = Command::new(&resolved.command);
    configure_remote_desktop_helper_command(&mut command);
    command
        .args(&resolved.prefix_args)
        .args(&provider.entry.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(working_dir) = provider.entry.working_dir.as_ref() {
        command.current_dir(working_dir);
    } else if let Some(working_dir) = resolved.working_dir.as_ref() {
        command.current_dir(working_dir);
    }

    let mut child = command.spawn()?;
    let stdin = child.stdin.take().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "remote desktop helper stdin is unavailable",
        )
    })?;
    Ok(RemoteDesktopHelperProcess { child, stdin })
}

pub(crate) fn write_initial_remote_desktop_connect(
    child: &mut Child,
    stdin: &mut impl Write,
    connect: &RemoteDesktopHelperRequest,
) -> Result<(), RemoteDesktopJsonLineError> {
    if let Err(error) = write_request_line(stdin, connect) {
        // A failed protocol handoff must not leave a detached helper process.
        terminate_remote_desktop_helper(child);
        return Err(error);
    }
    Ok(())
}

pub(crate) fn wait_for_remote_desktop_helper_exit(
    child: &mut Child,
    graceful_timeout: Duration,
    poll_interval: Duration,
) -> Option<ExitStatus> {
    let started_at = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if started_at.elapsed() < graceful_timeout => {
                thread::sleep(
                    poll_interval.min(graceful_timeout.saturating_sub(started_at.elapsed())),
                );
            }
            Ok(None) | Err(_) => return terminate_remote_desktop_helper(child),
        }
    }
}

fn terminate_remote_desktop_helper(child: &mut Child) -> Option<ExitStatus> {
    // Dropping Child does not terminate or reap the operating-system process.
    let _ = child.kill();
    child.wait().ok()
}

pub fn resolve_remote_desktop_helper_command(command: &str) -> ResolvedRemoteDesktopHelper {
    let command_path = Path::new(command);
    if command_path.components().count() > 1 || command_path.is_absolute() {
        return ResolvedRemoteDesktopHelper {
            command: command_path.to_path_buf(),
            prefix_args: Vec::new(),
            working_dir: None,
        };
    }

    if let Some(resolved) = development_remote_desktop_helper_command(command) {
        return resolved;
    }

    for candidate in bundled_remote_desktop_helper_candidates(command) {
        if candidate.exists() {
            return ResolvedRemoteDesktopHelper {
                command: candidate,
                prefix_args: Vec::new(),
                working_dir: None,
            };
        }
    }

    ResolvedRemoteDesktopHelper {
        command: PathBuf::from(command),
        prefix_args: Vec::new(),
        working_dir: None,
    }
}

fn development_remote_desktop_helper_command(command: &str) -> Option<ResolvedRemoteDesktopHelper> {
    if !cfg!(debug_assertions) || !matches!(command, "oxide-rdp-helper" | "oxide-vnc-helper") {
        return None;
    }

    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)?
        .to_path_buf();
    if !workspace_root
        .join("crates")
        .join(command)
        .join("Cargo.toml")
        .exists()
    {
        return None;
    }

    if let Some(resolved) = fresh_development_helper_binary(&workspace_root, command) {
        return Some(resolved);
    }

    // Debug runs execute current helper sources when no fresh binary exists.
    Some(ResolvedRemoteDesktopHelper {
        command: std::env::var_os("CARGO")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("cargo")),
        prefix_args: vec![
            "run".to_string(),
            "--quiet".to_string(),
            "-p".to_string(),
            command.to_string(),
            "--".to_string(),
        ],
        working_dir: Some(workspace_root),
    })
}

fn fresh_development_helper_binary(
    workspace_root: &Path,
    command: &str,
) -> Option<ResolvedRemoteDesktopHelper> {
    let candidate = workspace_root
        .join("target")
        .join("debug")
        .join(platform_helper_binary_name(command));
    let binary_modified = candidate.metadata().ok()?.modified().ok()?;
    let helper_crate = workspace_root.join("crates").join(command);
    let protocol_crate = workspace_root
        .join("crates")
        .join("oxide-remote-desktop-core");
    let cargo_lock = workspace_root.join("Cargo.lock");
    if path_modified_after(&helper_crate, binary_modified)
        || path_modified_after(&protocol_crate, binary_modified)
        || path_modified_after(&cargo_lock, binary_modified)
    {
        return None;
    }

    Some(ResolvedRemoteDesktopHelper {
        command: candidate,
        prefix_args: Vec::new(),
        working_dir: None,
    })
}

fn path_modified_after(path: &Path, cutoff: SystemTime) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if metadata
        .modified()
        .map(|modified| modified > cutoff)
        .unwrap_or(false)
    {
        return true;
    }
    if !metadata.is_dir() {
        return false;
    }

    let Ok(entries) = fs::read_dir(path) else {
        return false;
    };
    for entry in entries.flatten() {
        let entry_path = entry.path();
        if entry.file_name() == "target" {
            continue;
        }
        if path_modified_after(&entry_path, cutoff) {
            return true;
        }
    }
    false
}

fn bundled_remote_desktop_helper_candidates(command: &str) -> Vec<PathBuf> {
    let Some(exe_dir) = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
    else {
        return Vec::new();
    };
    let helper_name = platform_helper_binary_name(command);
    let mut roots = vec![
        exe_dir.join("resources"),
        exe_dir.join("..").join("Resources"),
    ];
    roots.push(exe_dir);

    let mut candidates = Vec::new();
    for root in roots {
        for target_dir in helper_target_resource_dirs() {
            candidates.push(root.join("helpers").join(target_dir).join(&helper_name));
        }
        candidates.push(root.join("helpers").join(&helper_name));
        candidates.push(root.join(&helper_name));
    }
    candidates
}

fn platform_helper_binary_name(command: &str) -> String {
    if cfg!(target_os = "windows") && !command.ends_with(".exe") {
        format!("{command}.exe")
    } else {
        command.to_string()
    }
}

fn helper_target_resource_dirs() -> &'static [&'static str] {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        // Keep shorthand fallbacks for older preview resource layouts.
        ("macos", "x86_64") => &["x86_64-apple-darwin", "macos_x64"],
        ("macos", "aarch64") => &["aarch64-apple-darwin", "macos_arm64"],
        ("windows", "x86_64") => &["x86_64-pc-windows-msvc", "windows_x64"],
        ("windows", "aarch64") => &["aarch64-pc-windows-msvc", "windows_arm64"],
        ("linux", "x86_64") => &["x86_64-unknown-linux-gnu", "linux_x64"],
        ("linux", "aarch64") => &["aarch64-unknown-linux-gnu", "linux_arm64"],
        _ => &[std::env::consts::ARCH],
    }
}

fn configure_remote_desktop_helper_command(command: &mut Command) {
    #[cfg(windows)]
    {
        // Captured helper processes must not open a separate console window.
        command.creation_flags(HELPER_CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    {
        let _ = command;
    }
}
