use serde::{Deserialize, Serialize};

use crate::docker::{ResourceDockerSnapshot, parse_docker_snapshot};
use crate::service::{ResourceServiceSnapshot, parse_service_snapshot};

pub const RESOURCE_HISTORY_CAPACITY: usize = 60;

/// Numeric history for charts, without process lists or other sampled payloads.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResourceTrendSample {
    pub timestamp_ms: u64,
    pub cpu_percent: Option<f64>,
    pub rx_bytes_per_sec: Option<u64>,
    pub tx_bytes_per_sec: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricsSource {
    Full,
    Partial,
    RttOnly,
    Failed,
    Unsupported,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceCpuCore {
    pub index: u32,
    pub percent: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceDisk {
    pub mount_point: String,
    pub used: u64,
    pub total: u64,
    pub percent: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceNetInterface {
    pub name: String,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub rx_bytes_per_sec: Option<u64>,
    pub tx_bytes_per_sec: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceGpu {
    pub index: u32,
    pub name: String,
    pub utilization_percent: Option<f64>,
    pub memory_used: Option<u64>,
    pub memory_total: Option<u64>,
    pub memory_percent: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceTopProcess {
    pub pid: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ppid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_percent: Option<f64>,
    pub memory_percent: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rss_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vsz_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed: Option<String>,
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_command: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceSystemInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub architecture: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boot_time_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uptime_seconds: Option<u64>,
}

impl ResourceSystemInfo {
    fn has_values(&self) -> bool {
        self.system_name.is_some()
            || self.system_version.is_some()
            || self.architecture.is_some()
            || self.boot_time_ms.is_some()
            || self.uptime_seconds.is_some()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceMetrics {
    pub timestamp_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_info: Option<ResourceSystemInfo>,
    pub cpu_percent: Option<f64>,
    pub memory_used: Option<u64>,
    pub memory_total: Option<u64>,
    pub memory_percent: Option<f64>,
    pub memory_buffers: Option<u64>,
    pub memory_cached: Option<u64>,
    pub swap_used: Option<u64>,
    pub swap_total: Option<u64>,
    pub swap_percent: Option<f64>,
    pub disk_used: Option<u64>,
    pub disk_total: Option<u64>,
    pub disk_percent: Option<f64>,
    pub load_avg_1: Option<f64>,
    pub load_avg_5: Option<f64>,
    pub load_avg_15: Option<f64>,
    pub cpu_cores: Option<u32>,
    pub cpu_per_core: Vec<ResourceCpuCore>,
    pub disks: Vec<ResourceDisk>,
    pub net_rx_bytes_per_sec: Option<u64>,
    pub net_tx_bytes_per_sec: Option<u64>,
    pub net_interfaces: Vec<ResourceNetInterface>,
    #[serde(default)]
    pub gpus: Vec<ResourceGpu>,
    pub top_processes: Vec<ResourceTopProcess>,
    #[serde(default)]
    pub docker: ResourceDockerSnapshot,
    #[serde(default)]
    pub services: ResourceServiceSnapshot,
    pub ssh_rtt_ms: Option<u64>,
    pub source: MetricsSource,
}

impl ResourceMetrics {
    pub fn empty(timestamp_ms: u64, source: MetricsSource) -> Self {
        Self {
            timestamp_ms,
            system_info: None,
            cpu_percent: None,
            memory_used: None,
            memory_total: None,
            memory_percent: None,
            memory_buffers: None,
            memory_cached: None,
            swap_used: None,
            swap_total: None,
            swap_percent: None,
            disk_used: None,
            disk_total: None,
            disk_percent: None,
            load_avg_1: None,
            load_avg_5: None,
            load_avg_15: None,
            cpu_cores: None,
            cpu_per_core: Vec::new(),
            disks: Vec::new(),
            net_rx_bytes_per_sec: None,
            net_tx_bytes_per_sec: None,
            net_interfaces: Vec::new(),
            gpus: Vec::new(),
            top_processes: Vec::new(),
            docker: ResourceDockerSnapshot::default(),
            services: ResourceServiceSnapshot::default(),
            ssh_rtt_ms: None,
            source,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CpuSnapshot {
    pub user: u64,
    pub nice: u64,
    pub system: u64,
    pub idle: u64,
    pub iowait: u64,
    pub irq: u64,
    pub softirq: u64,
    pub steal: u64,
}

impl CpuSnapshot {
    pub fn total(&self) -> u64 {
        self.user
            .saturating_add(self.nice)
            .saturating_add(self.system)
            .saturating_add(self.idle)
            .saturating_add(self.iowait)
            .saturating_add(self.irq)
            .saturating_add(self.softirq)
            .saturating_add(self.steal)
    }

    pub fn active(&self) -> u64 {
        self.total()
            .saturating_sub(self.idle)
            .saturating_sub(self.iowait)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NetSnapshot {
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NetInterfaceSnapshot {
    pub name: String,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreviousResourceSample {
    pub cpu: CpuSnapshot,
    pub cpu_per_core: Vec<CpuSnapshot>,
    pub net: NetSnapshot,
    pub net_interfaces: Vec<NetInterfaceSnapshot>,
    pub timestamp_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemorySnapshot {
    pub used: u64,
    pub total: u64,
    pub buffers: Option<u64>,
    pub cached: Option<u64>,
    pub swap_used: Option<u64>,
    pub swap_total: Option<u64>,
}

pub fn parse_resource_metrics(
    output: &str,
    previous: Option<&PreviousResourceSample>,
    timestamp_ms: u64,
) -> ResourceMetrics {
    let system_info = parse_system_info(output);
    if extract_section(output, "UNSUPPORTED").is_some() {
        let mut metrics = ResourceMetrics::empty(timestamp_ms, MetricsSource::Unsupported);
        metrics.system_info = system_info;
        return metrics;
    }

    let cpu_snap = parse_cpu_snapshot(output);
    let cpu_direct = parse_cpu_direct(output);
    let cpu_core_snaps = parse_cpu_core_snapshots(output);
    let net_snap = parse_net_snapshot(output);
    let net_interface_snaps = parse_net_interface_snapshots(output);
    let mem = parse_memory_snapshot(output);
    let disks = parse_disks(output);
    let disk = parse_root_disk_usage(&disks).or_else(|| parse_disk_usage_legacy(output));
    let load = parse_loadavg(output);
    let nproc = parse_nproc(output);
    let gpus = parse_gpus(output);
    let top_processes = parse_top_processes(output);
    let docker = parse_docker_snapshot(output);
    let services = parse_service_snapshot(output);
    let has_memory = mem.is_some();
    let has_auxiliary_metrics = !gpus.is_empty()
        || !top_processes.is_empty()
        || !matches!(docker.status, crate::ResourceDockerStatus::Unknown);

    let cpu_percent = match (&cpu_snap, previous) {
        (Some(current), Some(previous)) => cpu_usage_percent(current, &previous.cpu),
        _ => cpu_direct,
    };

    let cpu_per_core = cpu_core_snaps
        .iter()
        .enumerate()
        .map(|(index, current)| {
            let percent = previous
                .and_then(|previous| previous.cpu_per_core.get(index))
                .and_then(|previous| cpu_usage_percent(current, previous));
            ResourceCpuCore {
                index: index as u32,
                percent,
            }
        })
        .collect::<Vec<_>>();

    let (net_rx_rate, net_tx_rate) = match (&net_snap, previous) {
        (Some(current), Some(previous)) => {
            let elapsed_ms = timestamp_ms.saturating_sub(previous.timestamp_ms);
            if elapsed_ms > 0 {
                let elapsed_secs = elapsed_ms as f64 / 1000.0;
                (
                    Some(
                        (current.rx_bytes.saturating_sub(previous.net.rx_bytes) as f64
                            / elapsed_secs) as u64,
                    ),
                    Some(
                        (current.tx_bytes.saturating_sub(previous.net.tx_bytes) as f64
                            / elapsed_secs) as u64,
                    ),
                )
            } else {
                (None, None)
            }
        }
        _ => (None, None),
    };

    let net_interfaces = net_interface_snaps
        .iter()
        .map(|current| {
            let (rx_bytes_per_sec, tx_bytes_per_sec) = previous
                .and_then(|previous| {
                    let previous_iface = previous
                        .net_interfaces
                        .iter()
                        .find(|iface| iface.name == current.name)?;
                    let elapsed_ms = timestamp_ms.saturating_sub(previous.timestamp_ms);
                    if elapsed_ms == 0 {
                        return None;
                    }
                    let elapsed_secs = elapsed_ms as f64 / 1000.0;
                    Some((
                        (current.rx_bytes.saturating_sub(previous_iface.rx_bytes) as f64
                            / elapsed_secs) as u64,
                        (current.tx_bytes.saturating_sub(previous_iface.tx_bytes) as f64
                            / elapsed_secs) as u64,
                    ))
                })
                .unwrap_or((0, 0));
            ResourceNetInterface {
                name: current.name.clone(),
                rx_bytes: current.rx_bytes,
                tx_bytes: current.tx_bytes,
                rx_bytes_per_sec: Some(rx_bytes_per_sec),
                tx_bytes_per_sec: Some(tx_bytes_per_sec),
            }
        })
        .collect::<Vec<_>>();

    let (
        memory_used,
        memory_total,
        memory_percent,
        memory_buffers,
        memory_cached,
        swap_used,
        swap_total,
        swap_percent,
    ) = match mem {
        Some(mem) => (
            Some(mem.used),
            Some(mem.total),
            percent(mem.used, mem.total),
            mem.buffers,
            mem.cached,
            mem.swap_used,
            mem.swap_total,
            match (mem.swap_used, mem.swap_total) {
                (Some(used), Some(total)) => percent(used, total),
                _ => None,
            },
        ),
        None => (None, None, None, None, None, None, None, None),
    };

    let (disk_used, disk_total, disk_percent) = match disk {
        Some((used, total)) => (Some(used), Some(total), percent(used, total)),
        None => (None, None, None),
    };

    let source = match (
        cpu_snap.is_some() || cpu_direct.is_some(),
        has_memory,
        load.is_some(),
        disk.is_some() || !disks.is_empty(),
    ) {
        (true, true, true, _) => MetricsSource::Full,
        (true, _, _, _) | (_, true, _, _) | (_, _, true, _) | (_, _, _, true) => {
            MetricsSource::Partial
        }
        _ if has_auxiliary_metrics => MetricsSource::Partial,
        _ => MetricsSource::RttOnly,
    };

    ResourceMetrics {
        timestamp_ms,
        system_info,
        cpu_percent,
        memory_used,
        memory_total,
        memory_percent,
        memory_buffers,
        memory_cached,
        swap_used,
        swap_total,
        swap_percent,
        disk_used,
        disk_total,
        disk_percent,
        load_avg_1: load.map(|(one, _, _)| one),
        load_avg_5: load.map(|(_, five, _)| five),
        load_avg_15: load.map(|(_, _, fifteen)| fifteen),
        cpu_cores: nproc,
        cpu_per_core,
        disks,
        net_rx_bytes_per_sec: net_rx_rate,
        net_tx_bytes_per_sec: net_tx_rate,
        net_interfaces,
        gpus,
        top_processes,
        docker,
        services,
        ssh_rtt_ms: None,
        source,
    }
}

pub fn parse_system_info(output: &str) -> Option<ResourceSystemInfo> {
    let section = extract_section(output, "SYSTEM_INFO")?;
    let mut system_info = ResourceSystemInfo::default();

    for line in section.lines() {
        let Some((key, raw_value)) = line.split_once('\t') else {
            continue;
        };
        let value = bounded_system_text(raw_value);
        match key.trim() {
            "system_name" => system_info.system_name = value,
            "system_version" => system_info.system_version = value,
            "architecture" => system_info.architecture = value,
            "boot_time_ms" => system_info.boot_time_ms = raw_value.trim().parse().ok(),
            "uptime_seconds" => system_info.uptime_seconds = raw_value.trim().parse().ok(),
            _ => {}
        }
    }

    system_info.has_values().then_some(system_info)
}

fn bounded_system_text(value: &str) -> Option<String> {
    const MAX_SYSTEM_TEXT_CHARS: usize = 256;

    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Remote metadata is untrusted input; bound it before it reaches UI and plugin snapshots.
    Some(trimmed.chars().take(MAX_SYSTEM_TEXT_CHARS).collect())
}

pub fn previous_sample_from_metrics(
    metrics: &ResourceMetrics,
    output: &str,
) -> Option<PreviousResourceSample> {
    Some(PreviousResourceSample {
        cpu: parse_cpu_snapshot(output)?,
        cpu_per_core: parse_cpu_core_snapshots(output),
        net: parse_net_snapshot(output).unwrap_or_default(),
        net_interfaces: parse_net_interface_snapshots(output),
        timestamp_ms: metrics.timestamp_ms,
    })
}

pub fn push_history(history: &mut Vec<ResourceMetrics>, metrics: ResourceMetrics) {
    history.push(metrics);
    if history.len() > RESOURCE_HISTORY_CAPACITY {
        history.drain(0..history.len() - RESOURCE_HISTORY_CAPACITY);
    }
}

fn extract_section<'a>(output: &'a str, marker: &str) -> Option<&'a str> {
    let start_marker = format!("==={marker}===");
    let mut section_start = None;
    let mut offset = 0;
    for line in output.split_inclusive('\n') {
        let line_start = offset;
        let line_end = line_start + line.len();
        let trimmed = line.trim();

        if section_start.is_none() {
            if trimmed == start_marker {
                section_start = Some(line_end);
            }
        } else if is_metrics_section_marker(trimmed) {
            return Some(output[section_start?..line_start].trim());
        }

        offset = line_end;
    }

    let trailing = &output[offset..];
    if !trailing.is_empty() {
        let trimmed = trailing.trim();
        if section_start.is_none() {
            if trimmed == start_marker {
                section_start = Some(output.len());
            }
        } else if is_metrics_section_marker(trimmed) {
            return Some(output[section_start?..offset].trim());
        }
    }

    section_start.map(|start| output[start..].trim())
}

fn is_metrics_section_marker(line: &str) -> bool {
    // Persistent SSH shells can echo the whole sampling command when stty -echo
    // is unavailable or ignored. Treat only standalone marker lines as section
    // boundaries so echoed command text cannot hide the real TOPPROCS payload.
    line.len() > 6 && line.starts_with("===") && line.ends_with("===")
}

pub fn parse_cpu_snapshot(output: &str) -> Option<CpuSnapshot> {
    let section = extract_section(output, "STAT")?;
    let line = section.lines().find(|line| line.starts_with("cpu "))?;
    parse_cpu_line(line)
}

fn parse_cpu_line(line: &str) -> Option<CpuSnapshot> {
    let parts = line.split_whitespace().collect::<Vec<_>>();
    if parts.len() < 9 || !parts[0].starts_with("cpu") {
        return None;
    }
    Some(CpuSnapshot {
        user: parts[1].parse().ok()?,
        nice: parts[2].parse().ok()?,
        system: parts[3].parse().ok()?,
        idle: parts[4].parse().ok()?,
        iowait: parts[5].parse().ok()?,
        irq: parts[6].parse().ok()?,
        softirq: parts[7].parse().ok()?,
        steal: parts[8].parse().ok()?,
    })
}

fn parse_cpu_core_snapshots(output: &str) -> Vec<CpuSnapshot> {
    let Some(section) = extract_section(output, "STAT") else {
        return Vec::new();
    };
    section
        .lines()
        .filter_map(|line| {
            let label = line.split_whitespace().next()?;
            if label.len() > 3
                && label.starts_with("cpu")
                && label[3..].chars().all(|ch| ch.is_ascii_digit())
            {
                parse_cpu_line(line)
            } else {
                None
            }
        })
        .collect()
}

fn parse_cpu_direct(output: &str) -> Option<f64> {
    extract_section(output, "CPU_DIRECT")?
        .lines()
        .find_map(|line| line.trim().parse::<f64>().ok())
        .map(|value| value.clamp(0.0, 100.0))
}

fn cpu_usage_percent(current: &CpuSnapshot, previous: &CpuSnapshot) -> Option<f64> {
    let total_delta = current.total().saturating_sub(previous.total());
    let active_delta = current.active().saturating_sub(previous.active());
    if total_delta > 0 {
        Some((active_delta as f64 / total_delta as f64) * 100.0)
    } else {
        None
    }
}

pub fn parse_meminfo(output: &str) -> Option<(u64, u64)> {
    let mem = parse_memory_snapshot(output)?;
    Some((mem.used, mem.total))
}

pub fn parse_memory_snapshot(output: &str) -> Option<MemorySnapshot> {
    let section = extract_section(output, "MEMINFO")?;
    let mut total_kb = None;
    let mut available_kb = None;
    let mut free_kb = None;
    let mut buffers_kb = None;
    let mut cached_kb = None;
    let mut reclaimable_kb = None;
    let mut swap_total_kb = None;
    let mut swap_free_kb = None;

    for line in section.lines() {
        if line.starts_with("MemTotal:") {
            total_kb = extract_kb_value(line);
        } else if line.starts_with("MemAvailable:") {
            available_kb = extract_kb_value(line);
        } else if line.starts_with("MemFree:") {
            free_kb = extract_kb_value(line);
        } else if line.starts_with("Buffers:") {
            buffers_kb = extract_kb_value(line);
        } else if line.starts_with("Cached:") {
            cached_kb = extract_kb_value(line);
        } else if line.starts_with("SReclaimable:") {
            reclaimable_kb = extract_kb_value(line);
        } else if line.starts_with("SwapTotal:") {
            swap_total_kb = extract_kb_value(line);
        } else if line.starts_with("SwapFree:") {
            swap_free_kb = extract_kb_value(line);
        }
    }

    let total = total_kb? * 1024;
    let available = available_kb.or_else(|| {
        Some(
            free_kb?
                .saturating_add(buffers_kb.unwrap_or_default())
                .saturating_add(cached_kb.unwrap_or_default())
                .saturating_add(reclaimable_kb.unwrap_or_default()),
        )
    })? * 1024;
    let cached = cached_kb
        .unwrap_or_default()
        .saturating_add(reclaimable_kb.unwrap_or_default());
    let swap_total = swap_total_kb.map(|value| value * 1024);
    let swap_free = swap_free_kb.map(|value| value * 1024);
    Some(MemorySnapshot {
        used: total.saturating_sub(available),
        total,
        buffers: buffers_kb.map(|value| value * 1024),
        cached: Some(cached * 1024).filter(|value| *value > 0),
        swap_used: match (swap_total, swap_free) {
            (Some(total), Some(free)) => Some(total.saturating_sub(free)),
            _ => None,
        },
        swap_total,
    })
}

fn extract_kb_value(line: &str) -> Option<u64> {
    line.split_whitespace().nth(1)?.parse().ok()
}

pub fn parse_loadavg(output: &str) -> Option<(f64, f64, f64)> {
    let section = extract_section(output, "LOADAVG")?;
    let line = section.lines().next()?;
    let parts = line.split_whitespace().collect::<Vec<_>>();
    if parts.len() < 3 {
        return None;
    }
    Some((
        parts[0].parse().ok()?,
        parts[1].parse().ok()?,
        parts[2].parse().ok()?,
    ))
}

pub fn parse_net_snapshot(output: &str) -> Option<NetSnapshot> {
    let interfaces = parse_net_interface_snapshots(output);
    if interfaces.is_empty() {
        return None;
    }
    Some(NetSnapshot {
        rx_bytes: interfaces.iter().map(|iface| iface.rx_bytes).sum(),
        tx_bytes: interfaces.iter().map(|iface| iface.tx_bytes).sum(),
    })
}

fn parse_net_interface_snapshots(output: &str) -> Vec<NetInterfaceSnapshot> {
    let Some(section) = extract_section(output, "NETDEV") else {
        return Vec::new();
    };
    section
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.contains('|') || line.is_empty() {
                return None;
            }
            let (iface, rest) = line.split_once(':')?;
            let name = iface.trim();
            if should_skip_net_interface(name) {
                return None;
            }
            let parts = rest.split_whitespace().collect::<Vec<_>>();
            if parts.len() < 9 {
                return None;
            }
            Some(NetInterfaceSnapshot {
                name: name.to_string(),
                rx_bytes: parts[0].parse().ok()?,
                tx_bytes: parts[8].parse().ok()?,
            })
        })
        .collect()
}

fn should_skip_net_interface(name: &str) -> bool {
    name == "lo"
        || name.starts_with("veth")
        || name.starts_with("docker")
        || name.starts_with("br-")
        || name.starts_with("virbr")
        || name.starts_with("cni")
        || name.starts_with("flannel")
}

pub fn parse_nproc(output: &str) -> Option<u32> {
    let section = extract_section(output, "NPROC")?;
    section.lines().next()?.trim().parse().ok()
}

pub fn parse_disk_usage(output: &str) -> Option<(u64, u64)> {
    parse_root_disk_usage(&parse_disks(output)).or_else(|| parse_disk_usage_legacy(output))
}

fn parse_disk_usage_legacy(output: &str) -> Option<(u64, u64)> {
    let section = extract_section(output, "DISK")?;
    let line = section.lines().find(|line| !line.trim().is_empty())?;
    let parts = line.split_whitespace().collect::<Vec<_>>();
    if parts.len() < 3 {
        return None;
    }
    let total_kib = parts[1].parse::<u64>().ok()?;
    let used_kib = parts[2].parse::<u64>().ok()?;
    Some((
        used_kib.saturating_mul(1024),
        total_kib.saturating_mul(1024),
    ))
}

pub fn parse_disks(output: &str) -> Vec<ResourceDisk> {
    let Some(section) = extract_section(output, "DISKS") else {
        return Vec::new();
    };
    section
        .lines()
        .filter_map(|line| {
            let parts = line.split('\t').collect::<Vec<_>>();
            if parts.len() >= 4 {
                let mount_point = parts[0].trim();
                let used = parts[1].trim().parse::<u64>().ok()?;
                let total = parts[2].trim().parse::<u64>().ok()?;
                let percent_value = parts[3].trim().parse::<f64>().ok();
                if mount_point.is_empty() || total == 0 {
                    return None;
                }
                return Some(ResourceDisk {
                    mount_point: mount_point.to_string(),
                    used,
                    total,
                    percent: percent_value.or_else(|| percent(used, total)),
                });
            }

            let parts = line.split_whitespace().collect::<Vec<_>>();
            if parts.len() < 6 || !parts[0].starts_with("/dev") {
                return None;
            }
            let total = parts[1].parse::<u64>().ok()?.saturating_mul(1024);
            let used = parts[2].parse::<u64>().ok()?.saturating_mul(1024);
            let percent_value = parts[4].trim_end_matches('%').parse::<f64>().ok();
            Some(ResourceDisk {
                mount_point: parts[5].to_string(),
                used,
                total,
                percent: percent_value.or_else(|| percent(used, total)),
            })
        })
        .collect()
}

fn parse_root_disk_usage(disks: &[ResourceDisk]) -> Option<(u64, u64)> {
    let root = disks
        .iter()
        .find(|disk| disk.mount_point == "/")
        .or_else(|| disks.first())?;
    Some((root.used, root.total))
}

pub fn parse_top_processes(output: &str) -> Vec<ResourceTopProcess> {
    let Some(section) = extract_section(output, "TOPPROCS") else {
        return Vec::new();
    };
    section
        .lines()
        .filter_map(|line| {
            let parts = line.split('\t').collect::<Vec<_>>();
            if parts.len() >= 11 {
                let memory_percent = parts[5].trim().parse::<f64>().ok()?;
                let rss_bytes = parse_process_kib(parts[6]);
                let vsz_bytes = parse_process_kib(parts[7]);
                return Some(ResourceTopProcess {
                    pid: parts[0].trim().to_string(),
                    ppid: clean_process_field(parts[1]),
                    user: clean_process_field(parts[2]),
                    state: clean_process_field(parts[3]),
                    cpu_percent: parts[4].trim().parse::<f64>().ok(),
                    memory_percent,
                    rss_bytes,
                    vsz_bytes,
                    elapsed: clean_process_field(parts[8]),
                    command: parts[9].trim().to_string(),
                    full_command: clean_process_field(parts[10]),
                });
            }

            let parts = line.splitn(3, '\t').collect::<Vec<_>>();
            if parts.len() < 3 {
                return None;
            }
            let memory_percent = parts[1].trim().parse::<f64>().ok()?;
            Some(ResourceTopProcess {
                pid: parts[0].trim().to_string(),
                ppid: None,
                user: None,
                state: None,
                cpu_percent: None,
                memory_percent,
                rss_bytes: None,
                vsz_bytes: None,
                elapsed: None,
                command: parts[2].trim().to_string(),
                full_command: None,
            })
        })
        .collect()
}

pub fn parse_gpus(output: &str) -> Vec<ResourceGpu> {
    let Some(section) = extract_section(output, "GPUS") else {
        return parse_intel_gpu_top(output).into_iter().collect();
    };
    let mut gpus = section
        .lines()
        .filter_map(|line| parse_gpu_line(line.trim()))
        .collect::<Vec<_>>();
    if !gpus
        .iter()
        .any(|gpu| gpu.name.to_ascii_lowercase().contains("intel"))
        && let Some(intel_gpu) = parse_intel_gpu_top(output)
    {
        gpus.push(intel_gpu);
    }
    gpus
}

fn parse_gpu_line(line: &str) -> Option<ResourceGpu> {
    if line.is_empty() {
        return None;
    }
    let fields = if line.contains('\t') {
        line.split('\t')
            .map(str::trim)
            .map(str::to_string)
            .collect::<Vec<_>>()
    } else {
        let parts = line.split(',').map(str::trim).collect::<Vec<_>>();
        if parts.len() < 5 {
            return None;
        }
        vec![
            parts[0].to_string(),
            parts[1..parts.len().saturating_sub(3)].join(", "),
            parts[parts.len() - 3].to_string(),
            parts[parts.len() - 2].to_string(),
            parts[parts.len() - 1].to_string(),
        ]
    };
    if fields.len() < 5 {
        return None;
    }

    let memory_used = parse_gpu_mib(&fields[3]);
    let memory_total = parse_gpu_mib(&fields[4]);
    Some(ResourceGpu {
        index: fields[0].parse().ok()?,
        name: fields[1].to_string(),
        utilization_percent: parse_gpu_percent(&fields[2]),
        memory_used,
        memory_total,
        memory_percent: match (memory_used, memory_total) {
            (Some(used), Some(total)) => percent(used, total),
            _ => None,
        },
    })
}

fn parse_gpu_percent(value: &str) -> Option<f64> {
    let trimmed = value.trim().trim_end_matches('%').trim();
    if trimmed.eq_ignore_ascii_case("N/A") {
        return None;
    }
    trimmed
        .parse::<f64>()
        .ok()
        .map(|value| value.clamp(0.0, 100.0))
}

fn parse_gpu_mib(value: &str) -> Option<u64> {
    let trimmed = value
        .trim()
        .trim_end_matches("MiB")
        .trim_end_matches("Mib")
        .trim_end_matches("MB")
        .trim();
    if trimmed.eq_ignore_ascii_case("N/A") {
        return None;
    }
    trimmed.parse::<f64>().ok().map(|mib| {
        if mib <= 0.0 {
            0
        } else {
            (mib * 1024.0 * 1024.0).round() as u64
        }
    })
}

fn parse_intel_gpu_top(output: &str) -> Option<ResourceGpu> {
    let section = extract_section(output, "GPUS_INTEL_TOP")?;
    let utilization = parse_max_intel_busy_percent(section)?;
    Some(ResourceGpu {
        index: 0,
        name: "Intel GPU".to_string(),
        utilization_percent: Some(utilization),
        memory_used: None,
        memory_total: None,
        memory_percent: None,
    })
}

fn parse_max_intel_busy_percent(section: &str) -> Option<f64> {
    let mut max_busy: Option<f64> = None;
    let mut rest = section;
    while let Some(position) = rest.find("\"busy\"") {
        rest = &rest[position + "\"busy\"".len()..];
        let Some(colon) = rest.find(':') else {
            continue;
        };
        let after_colon = rest[colon + 1..].trim_start();
        let number = after_colon
            .chars()
            .take_while(|ch| ch.is_ascii_digit() || *ch == '.')
            .collect::<String>();
        if let Ok(value) = number.parse::<f64>() {
            let clamped = value.clamp(0.0, 100.0);
            max_busy = Some(match max_busy {
                Some(current) => current.max(clamped),
                None => clamped,
            });
        }
    }
    max_busy
}

fn clean_process_field(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty() && trimmed != "-").then(|| trimmed.to_string())
}

fn parse_process_kib(value: &str) -> Option<u64> {
    value.trim().parse::<u64>().ok().map(|kib| kib * 1024)
}

fn percent(used: u64, total: u64) -> Option<f64> {
    if total > 0 {
        Some((used as f64 / total as f64) * 100.0)
    } else {
        None
    }
}
