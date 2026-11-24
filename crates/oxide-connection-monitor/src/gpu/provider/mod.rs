//! Vendor-specific GPU sampling commands and parsers.

mod amd;
mod ascend;
mod cambricon;
mod hygon;
mod intel;
mod mthreads;
mod nvidia;

use super::{GpuDevice, GpuProcess, GpuSnapshot, GpuSnapshotStatus};

pub const GPU_END_MARKER: &str = "===GPU_NPU_SAMPLE_END===";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum ProviderStatus {
    Available,
    NoDevices,
    Unavailable,
    Error(String),
    Unknown,
}

pub(super) struct ProviderSnapshot {
    pub status: ProviderStatus,
    pub devices: Vec<GpuDevice>,
    pub processes: Vec<GpuProcess>,
}

/// Builds one bounded probe that can collect every supported accelerator vendor.
pub fn build_gpu_sample_command(os_type: &str) -> String {
    if !gpu_os_supported(os_type) {
        return format!("echo '===GPU_NPU_STATUS==='; echo unsupported; echo '{GPU_END_MARKER}'\n");
    }

    let mut command = nvidia::sample_command();
    if matches!(os_type, "Linux" | "linux") {
        command.push_str(&amd::sample_command());
        command.push_str(&hygon::sample_command());
        command.push_str(&ascend::sample_command());
        command.push_str(&cambricon::sample_command());
        command.push_str(&intel::sample_command());
        command.push_str(&mthreads::sample_command());
    }
    command.push_str(&format!("echo '{GPU_END_MARKER}'\n"));
    command
}

/// Parses all vendor sections into one provider-neutral snapshot.
pub fn parse_gpu_snapshot(output: &str, timestamp_ms: u64) -> GpuSnapshot {
    if first_section_line(output, "GPU_NPU_STATUS") == Some("unsupported") {
        return GpuSnapshot {
            timestamp_ms,
            status: GpuSnapshotStatus::Unsupported,
            devices: Vec::new(),
            processes: Vec::new(),
        };
    }

    let provider_snapshots = [
        nvidia::parse(output),
        amd::parse(output),
        hygon::parse(output),
        ascend::parse(output),
        cambricon::parse(output),
        intel::parse(output),
        mthreads::parse(output),
    ];
    let mut devices = provider_snapshots
        .iter()
        .flat_map(|snapshot| snapshot.devices.iter().cloned())
        .collect::<Vec<_>>();
    let mut processes = provider_snapshots
        .iter()
        .flat_map(|snapshot| snapshot.processes.iter().cloned())
        .collect::<Vec<_>>();
    devices.sort_by_key(|device| (device.provider, device.index));
    processes.sort_by(|left, right| {
        left.provider
            .cmp(&right.provider)
            .then_with(|| left.gpu_uuid.cmp(&right.gpu_uuid))
            .then_with(|| left.pid.cmp(&right.pid))
    });

    let status = merged_status(&provider_snapshots, !devices.is_empty());
    GpuSnapshot {
        timestamp_ms,
        status,
        devices,
        processes,
    }
}

fn merged_status(snapshots: &[ProviderSnapshot], has_devices: bool) -> GpuSnapshotStatus {
    if has_devices {
        // A failing secondary provider must not hide usable devices from the
        // other vendor on mixed GPU hosts.
        return GpuSnapshotStatus::Available;
    }

    let errors = snapshots
        .iter()
        .filter_map(|snapshot| match &snapshot.status {
            ProviderStatus::Error(message) => Some(message.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    if !errors.is_empty() {
        return GpuSnapshotStatus::Error(errors.join("; "));
    }
    if snapshots.iter().any(|snapshot| {
        matches!(
            snapshot.status,
            ProviderStatus::Available | ProviderStatus::NoDevices
        )
    }) {
        return GpuSnapshotStatus::NoDevices;
    }
    if snapshots
        .iter()
        .any(|snapshot| snapshot.status == ProviderStatus::Unavailable)
    {
        return GpuSnapshotStatus::Unavailable;
    }
    GpuSnapshotStatus::Unknown
}

pub(super) fn section<'a>(output: &'a str, marker: &str) -> Option<&'a str> {
    let marker = format!("==={marker}===");
    let start = output.find(&marker)? + marker.len();
    let rest = output[start..].trim_start_matches(['\r', '\n']);
    // Protocol markers use uppercase identifiers. This distinction prevents
    // vendor table separators and banners made from '=' from ending a section.
    let end = rest
        .match_indices("\n===")
        .find_map(|(offset, _)| {
            let line = rest[offset + 1..].lines().next()?.trim_end_matches('\r');
            let identifier = line.strip_prefix("===")?.strip_suffix("===")?;
            (!identifier.is_empty()
                && identifier.chars().all(|character| {
                    character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_'
                }))
            .then_some(offset)
        })
        .unwrap_or(rest.len());
    Some(rest[..end].trim())
}

pub(super) fn first_section_line<'a>(output: &'a str, marker: &str) -> Option<&'a str> {
    section(output, marker)?
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(str::trim)
}

pub(super) fn sanitized_error(output: &str, marker: &str, fallback: &str) -> String {
    let message = first_section_line(output, marker).unwrap_or(fallback);
    message
        .chars()
        .filter(|character| !character.is_control())
        .take(240)
        .collect()
}

fn gpu_os_supported(os_type: &str) -> bool {
    matches!(
        os_type,
        "Linux" | "linux" | "Windows_MinGW" | "Windows_MSYS" | "Windows_Cygwin"
    )
}
