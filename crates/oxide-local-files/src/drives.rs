use std::{fmt::Display, path::Path};
use windows_registry::{CURRENT_USER, Key, Result};

#[cfg(unix)]
use std::collections::HashMap;
#[cfg(not(unix))]
use std::{collections::HashSet, path::PathBuf};

use crate::{LocalDrive, paths::extract_drive_letter};

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
    let wsl = get_wsl_drive_letter();
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
                        path: format!("\\\\wsl.localhost\\{}\\home", name),
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
    let raw_name = disk.name().to_string_lossy();
    if !raw_name.is_empty() {
        if let Some(drive) = extract_drive_letter(mount_point) {
            return format!("{}({}:)", raw_name, drive);
        }
        return raw_name.to_string();
    }
    let mount = mount_point.to_string_lossy();
    let drive_char = mount_point
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| {
            if mount == "/" {
                "System".to_string()
            } else {
                mount.to_string()
            }
        });
    return format!("{}({})", raw_name, drive_char);
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

fn enum_lxss_subkeys() -> Result<Vec<String>> {
    let root = CURRENT_USER.open(r"Software\Microsoft\Windows\CurrentVersion\Lxss")?;
    let mut keys = Vec::new();
    for key_name in root.keys()? {
        if key_name.starts_with('{') && key_name.ends_with('}') {
            keys.push(key_name.to_string());
        }
    }
    Ok(keys)
}

fn query_value(key: &Key, value_name: &str) -> Option<String> {
    key.get_string(value_name).ok().filter(|s| !s.is_empty())
}

fn get_wsl_drive_letter() -> Option<Vec<(String, char)>> {
    let root_path = r"Software\Microsoft\Windows\CurrentVersion\Lxss";
    let guids = enum_lxss_subkeys().ok()?;

    let mut result = Vec::new();
    for guid in guids {
        let key = CURRENT_USER
            .open(&format!(r"{}\{}", root_path, guid))
            .ok()?;

        let name = query_value(&key, "DistributionName")?;
        let path = query_value(&key, "BasePath")?;
        let drive = extract_drive_letter(Path::new(&path))?;

        result.push((name, drive));
    }
    Some(result)
}
