//! Huawei Ascend NPU SMI command protocol and table parser.

use super::{ProviderSnapshot, ProviderStatus, first_section_line, sanitized_error, section};
use crate::gpu::{GpuDevice, GpuProcess, GpuProvider};

pub(super) fn sample_command() -> String {
    concat!(
        "echo '===ASCEND_STATUS==='; ",
        "if ! command -v npu-smi >/dev/null 2>&1; then ",
        "echo unavailable; ",
        "else ",
        "echo available; ",
        "echo '===ASCEND_DATA==='; ",
        "ascend_output=$(LC_ALL=C npu-smi info 2>&1); ",
        "ascend_exit=$?; printf '%s\\n' \"$ascend_output\"; ",
        "echo '===ASCEND_QUERY_EXIT==='; echo \"$ascend_exit\"; ",
        "if [ \"$ascend_exit\" -ne 0 ]; then ",
        "echo '===ASCEND_ERROR==='; printf '%s\\n' \"$ascend_output\"; ",
        "fi; ",
        "fi; "
    )
    .to_string()
}

pub(super) fn parse(output: &str) -> ProviderSnapshot {
    let query_exit =
        first_section_line(output, "ASCEND_QUERY_EXIT").and_then(|value| value.parse::<i32>().ok());
    match first_section_line(output, "ASCEND_STATUS") {
        Some("unavailable") => return empty_snapshot(ProviderStatus::Unavailable),
        Some("available") => {}
        _ => return empty_snapshot(ProviderStatus::Unknown),
    }
    if query_exit.is_some_and(|exit| exit != 0) {
        let message = sanitized_error(output, "ASCEND_ERROR", "npu-smi info query failed");
        if reports_no_devices(&message) {
            return empty_snapshot(ProviderStatus::NoDevices);
        }
        return empty_snapshot(ProviderStatus::Error(format!("Ascend: {message}")));
    }

    let Some(payload) = section(output, "ASCEND_DATA") else {
        return empty_snapshot(ProviderStatus::NoDevices);
    };
    if reports_no_devices(payload) {
        return empty_snapshot(ProviderStatus::NoDevices);
    }
    let parsed = parse_tables(payload);
    if parsed.devices.is_empty() {
        let status = if parsed.saw_device_header || payload.trim().is_empty() {
            ProviderStatus::NoDevices
        } else {
            ProviderStatus::Error("Ascend: unrecognized npu-smi info output".into())
        };
        return empty_snapshot(status);
    }

    ProviderSnapshot {
        status: ProviderStatus::Available,
        devices: parsed
            .devices
            .into_iter()
            .map(|device| device.device)
            .collect(),
        processes: parsed.processes,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TableMode {
    Unknown,
    Devices,
    Processes,
}

struct PendingDevice {
    npu_id: u32,
    name: String,
    health: String,
    power_watts: Option<f64>,
    temperature_celsius: Option<f64>,
}

struct ParsedDevice {
    npu_id: u32,
    chip_id: u32,
    device: GpuDevice,
}

struct ParsedTables {
    saw_device_header: bool,
    devices: Vec<ParsedDevice>,
    processes: Vec<GpuProcess>,
}

fn parse_tables(payload: &str) -> ParsedTables {
    let driver_version = driver_version(payload);
    let mut mode = TableMode::Unknown;
    let mut saw_device_header = false;
    let mut pending_device = None;
    let mut devices = Vec::new();
    let mut processes = Vec::new();

    for line in payload.lines().map(str::trim) {
        if line.contains("Process id") && line.contains("Process memory") {
            mode = TableMode::Processes;
            pending_device = None;
            continue;
        }
        if line.contains("NPU") && line.contains("Name") && line.contains("Health") {
            mode = TableMode::Devices;
            saw_device_header = true;
            continue;
        }
        if !line.starts_with('|') {
            continue;
        }
        let columns = line
            .trim_matches('|')
            .split('|')
            .map(str::trim)
            .collect::<Vec<_>>();
        match mode {
            TableMode::Devices if columns.len() >= 3 => {
                if is_health_status(columns[1]) {
                    pending_device = parse_device_header(&columns);
                } else if let Some(header) = pending_device.take()
                    && let Some(device) =
                        parse_device_metrics(header, &columns, driver_version.as_deref())
                {
                    devices.push(device);
                }
            }
            TableMode::Processes if columns.len() >= 4 => {
                if let Some(process) = parse_process(&columns, &devices) {
                    processes.push(process);
                }
            }
            TableMode::Unknown | TableMode::Devices | TableMode::Processes => {}
        }
    }

    devices.sort_by_key(|device| (device.npu_id, device.chip_id));
    processes.sort_by(|left, right| {
        left.gpu_uuid
            .cmp(&right.gpu_uuid)
            .then_with(|| left.pid.cmp(&right.pid))
    });
    ParsedTables {
        saw_device_header,
        devices,
        processes,
    }
}

fn parse_device_header(columns: &[&str]) -> Option<PendingDevice> {
    let mut identity = columns[0].split_whitespace();
    let npu_id = identity.next()?.parse::<u32>().ok()?;
    let raw_name = identity.collect::<Vec<_>>().join(" ");
    let name = if raw_name.is_empty() {
        format!("Ascend NPU {npu_id}")
    } else if raw_name.to_ascii_lowercase().contains("ascend") {
        raw_name
    } else {
        format!("Ascend {raw_name}")
    };
    let telemetry = numbers(columns[2]);
    Some(PendingDevice {
        npu_id,
        name,
        health: columns[1].to_string(),
        power_watts: telemetry.first().copied(),
        temperature_celsius: telemetry.get(1).copied(),
    })
}

fn parse_device_metrics(
    header: PendingDevice,
    columns: &[&str],
    driver_version: Option<&str>,
) -> Option<ParsedDevice> {
    let chip_id = columns[0].split_whitespace().next()?.parse::<u32>().ok()?;
    let pci_bus_id =
        optional_text(columns[1]).unwrap_or_else(|| format!("ASCEND:{}:{chip_id}", header.npu_id));
    let telemetry = numbers(columns[2]);
    let utilization_percent = telemetry.first().copied();
    let memory_pair = (telemetry.len() >= 3).then(|| {
        (
            &telemetry[telemetry.len() - 2],
            &telemetry[telemetry.len() - 1],
        )
    });
    let memory_used = memory_pair.map(|(used, _)| mibibytes(*used));
    let memory_total = memory_pair.map(|(_, total)| mibibytes(*total));
    let memory_utilization_percent = match (memory_used, memory_total) {
        (Some(used), Some(total)) if total > 0 => Some((used as f64 / total as f64) * 100.0),
        _ => None,
    };
    let uuid = ascend_device_id(header.npu_id, chip_id, &pci_bus_id);

    Some(ParsedDevice {
        npu_id: header.npu_id,
        chip_id,
        device: GpuDevice {
            provider: GpuProvider::Ascend,
            index: header.npu_id,
            uuid,
            pci_bus_id,
            name: format!("{} · Chip {chip_id}", header.name),
            driver_version: driver_version.map(str::to_string),
            performance_state: None,
            health_status: Some(header.health),
            utilization_percent,
            memory_utilization_percent,
            memory_used,
            memory_total,
            temperature_celsius: header.temperature_celsius,
            power_draw_watts: header.power_watts,
            power_limit_watts: None,
            fan_speed_percent: None,
        },
    })
}

fn parse_process(columns: &[&str], devices: &[ParsedDevice]) -> Option<GpuProcess> {
    let identity = columns[0]
        .split_whitespace()
        .filter_map(|value| value.parse::<u32>().ok())
        .collect::<Vec<_>>();
    let npu_id = *identity.first()?;
    let chip_id = *identity.get(1)?;
    let pid = columns[1].parse::<u32>().ok()?;
    let device = devices
        .iter()
        .find(|device| device.npu_id == npu_id && device.chip_id == chip_id)?;
    let process_name = optional_text(columns[2]).unwrap_or_else(|| format!("PID {pid}"));

    Some(GpuProcess {
        provider: GpuProvider::Ascend,
        gpu_uuid: device.device.uuid.clone(),
        pid,
        process_name,
        used_memory: numbers(columns[3]).first().copied().map(mibibytes),
    })
}

fn driver_version(payload: &str) -> Option<String> {
    payload.lines().find_map(|line| {
        let (_, version) = line.split_once("Version:")?;
        optional_text(version.trim().trim_matches('|'))
    })
}

fn numbers(value: &str) -> Vec<f64> {
    value
        .split(|character: char| character.is_whitespace() || character == '/')
        .filter_map(|value| value.trim().parse::<f64>().ok())
        .collect()
}

fn mibibytes(value: f64) -> u64 {
    (value.max(0.0) * 1024.0 * 1024.0)
        .round()
        .min(u64::MAX as f64) as u64
}

fn ascend_device_id(npu_id: u32, chip_id: u32, pci_bus_id: &str) -> String {
    if pci_bus_id.starts_with("ASCEND:") {
        format!("ASCEND-NPU-{npu_id}-CHIP-{chip_id}")
    } else {
        format!("ASCEND-{pci_bus_id}-CHIP-{chip_id}")
    }
}

fn optional_text(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty()
        || matches!(
            value.to_ascii_lowercase().as_str(),
            "n/a" | "na" | "unknown" | "not available"
        )
    {
        None
    } else {
        Some(value.to_string())
    }
}

fn is_health_status(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "ok" | "warning" | "alarm" | "critical" | "unknown"
    )
}

fn reports_no_devices(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    value.contains("no npu")
        || value.contains("no device")
        || value.contains("device not found")
        || value.contains("there is no device")
}

fn empty_snapshot(status: ProviderStatus) -> ProviderSnapshot {
    ProviderSnapshot {
        status,
        devices: Vec::new(),
        processes: Vec::new(),
    }
}
