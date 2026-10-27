use std::{fmt::Display, path::Path, process::Command};

#[cfg(unix)]
use std::collections::HashMap;
#[cfg(not(unix))]
use std::{collections::HashSet, path::PathBuf};

use crate::LocalDrive;

pub fn local_drives() -> Vec<LocalDrive> {
    let mut drives = platform_local_drives();

    drives.sort_by(|left, right| {
        let left_system = if left.drive_type == DiskType::Phd(true) {
            0
        } else {
            1
        };
        let right_system = if right.drive_type == DiskType::Phd(true) {
            0
        } else {
            1
        };
        left_system
            .cmp(&right_system)
            .then_with(|| left.path.cmp(&right.path))
    });
    if drives.is_empty() {
        drives.push(LocalDrive {
            name: "System".to_string(),
            path: home_path_root(),
            drive_type: DiskType::Phd(true),
            total_space: 0,
            available_space: 0,
            read_only: false,
        });
    }
    drives
}

fn home_path_root() -> String {
    #[cfg(windows)]
    {
        "C:\\".to_string()
    }
    #[cfg(not(windows))]
    {
        "/".to_string()
    }
}

fn platform_local_drives() -> Vec<LocalDrive> {
    use sysinfo::Disks;
    #[cfg(windows)]
    let wsl = get_wsl_drive_char();
    let disks = Disks::new_with_refreshed_list();
    let mut drives: Vec<LocalDrive> = Vec::new();

    #[cfg(unix)]
    let mut seen_dev_ids: HashMap<u64, usize> = HashMap::new();
    #[cfg(not(unix))]
    let mut seen_mount_points: HashSet<PathBuf> = HashSet::new();

    for disk in disks.list() {
        let mount_point = disk.mount_point().to_path_buf();

        #[cfg(unix)]
        let unix_dev_id = {
            use std::os::unix::fs::MetadataExt;
            match std::fs::metadata(&mount_point) {
                Ok(metadata) => {
                    let dev = metadata.dev();
                    if let Some(&existing_idx) = seen_dev_ids.get(&dev) {
                        if mount_point.as_os_str().len() < drives[existing_idx].path.len() {
                            drives[existing_idx].path = mount_point.to_string_lossy().to_string();
                            drives[existing_idx].name = drive_display_name(disk, &mount_point);
                        }
                        continue;
                    }
                    Some(dev)
                }
                Err(_) => None,
            }
        };
        #[cfg(not(unix))]
        {
            let canonical = mount_point
                .canonicalize()
                .unwrap_or_else(|_| mount_point.clone());
            if !seen_mount_points.insert(canonical) {
                continue;
            }
        }

        let mount = mount_point.to_string_lossy();
        if is_pseudo_mount(&mount) {
            continue;
        }

        #[cfg(unix)]
        if let Some(dev) = unix_dev_id {
            seen_dev_ids.insert(dev, drives.len());
        }

        let read_only = if cfg!(target_os = "macos") && mount == "/" {
            !std::fs::metadata("/Users")
                .map(|metadata| !metadata.permissions().readonly())
                .unwrap_or(false)
        } else {
            disk.is_read_only()
        };

        #[cfg(windows)]
        if let Some(distributions) = &wsl {
            for (name, location) in distributions {
                if let Some(mount) = mount_point.to_str()
                    && mount.starts_with(*location)
                {
                    drives.push(LocalDrive {
                        name: name.clone(),
                        path: format!("//wsl.localhost/{}/home", name),
                        drive_type: DiskType::Network,
                        total_space: disk.total_space(),
                        available_space: disk.available_space(),
                        read_only,
                    });
                }
            }
        }

        drives.push(LocalDrive {
            name: drive_display_name(disk, &mount_point),
            path: mount.to_string(),
            drive_type: classify_disk(disk),
            total_space: disk.total_space(),
            available_space: disk.available_space(),
            read_only,
        });
    }

    drives
}

fn is_pseudo_mount(mount: &str) -> bool {
    mount.starts_with("/proc")
        || mount.starts_with("/sys")
        || mount.starts_with("/dev")
        || mount.starts_with("/snap")
        || mount == "/boot"
        || mount == "/boot/efi"
        || is_blocked_run_mount(mount)
}

fn is_blocked_run_mount(mount: &str) -> bool {
    if !mount.starts_with("/run") {
        return false;
    }
    if mount.starts_with("/run/media/") || mount.starts_with("/run/mount/") {
        return false;
    }
    mount.starts_with("/run/user/") && !mount.contains("/gvfs/")
        || (!mount.starts_with("/run/user/"))
}

fn drive_display_name(disk: &sysinfo::Disk, mount_point: &Path) -> String {
    let raw_name = disk.name().to_string_lossy().to_string();
    if !raw_name.is_empty() {
        return raw_name;
    }
    let mount = mount_point.to_string_lossy();
    mount_point
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| {
            if mount == "/" {
                "System".to_string()
            } else {
                mount.to_string()
            }
        })
}

#[derive(Clone, Debug, PartialEq)]
pub enum DiskType {
    //true表示是根目录或者系统盘
    Phd(bool),
    //网络磁盘
    Network,
    //可移动硬盘
    Removable,
}

impl Display for DiskType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Network => f.write_str("network"),
            Self::Phd(true) => f.write_str("system"),
            Self::Phd(false) => f.write_str("nonsystem"),
            Self::Removable => f.write_str("removable"),
        }
    }
}

fn classify_disk(disk: &sysinfo::Disk) -> DiskType {
    use sysinfo::DiskKind;

    let mount = disk.mount_point().to_string_lossy();
    #[cfg(not(windows))]
    if mount == "/" {
        return DiskType::Phd(true);
    }
    #[cfg(windows)]
    if mount.to_uppercase().starts_with("C:") {
        return DiskType::Phd(true);
    }
    if mount.contains("://") || mount.starts_with("//") {
        return DiskType::Network;
    }
    match disk.kind() {
        DiskKind::HDD | DiskKind::SSD => DiskType::Phd(false),
        _ => DiskType::Removable,
    }
}

pub fn directory_stats(path: &Path) -> (u64, u64, u64) {
    let mut files = 0;
    let mut dirs = 0;
    let mut size = 0;
    let Ok(entries) = std::fs::read_dir(path) else {
        return (files, dirs, size);
    };
    for entry in entries.flatten() {
        let Ok(metadata) = std::fs::symlink_metadata(entry.path()) else {
            continue;
        };
        if metadata.is_dir() {
            dirs += 1;
            let nested = directory_stats(&entry.path());
            files += nested.0;
            dirs += nested.1;
            size += nested.2;
        } else {
            files += 1;
            size += metadata.len();
        }
    }
    (files, dirs, size)
}
fn decode_output(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => {
            let (text, _, _) = encoding_rs::GBK.decode(bytes);
            text.into_owned()
        }
    }
}

fn reg_query(args: &[&str]) -> Option<String> {
    let out = Command::new("reg").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(decode_output(&out.stdout))
}

fn query_value(key: &str, value_name: &str) -> Option<String> {
    let text = reg_query(&["query", key, "/v", value_name])?;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with(value_name) {
            continue;
        }
        let rest = &trimmed[value_name.len()..];
        if !rest.starts_with(char::is_whitespace) {
            continue;
        }
        let rest = rest.trim_start();
        let type_end = rest.find(char::is_whitespace)?;
        let typ = &rest[..type_end];
        if !typ.starts_with("REG_") {
            continue;
        }
        let data = rest[type_end..].trim_start();
        return Some(data.to_string());
    }
    None
}

fn extract_drive(path: &str) -> Option<char> {
    let mut chars = path.chars();
    let first = chars.next()?;
    if first.is_ascii_alphabetic() && chars.next()? == ':' {
        Some(first.to_ascii_uppercase())
    } else {
        None
    }
}
fn get_wsl_drive_char() -> Option<Vec<(String, char)>> {
    let root = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Lxss";
    let keys_text = reg_query(&["query", root])?;

    let mut result = Vec::new();
    for line in keys_text.lines() {
        let line = line.trim();
        let guid = match line.rsplit('\\').next() {
            Some(g) if g.starts_with('{') && g.ends_with('}') => g,
            _ => continue,
        };
        let key = format!(r"{}\{}", root, guid);
        let name = match query_value(&key, "DistributionName") {
            Some(n) if !n.is_empty() => n,
            _ => continue,
        };
        let path = match query_value(&key, "BasePath") {
            Some(p) if !p.is_empty() => p,
            _ => {
                continue;
            }
        };
        let drive = match extract_drive(&path) {
            Some(d) => d,
            None => {
                continue;
            }
        };

        result.push((name, drive));
    }
    Some(result)
}
