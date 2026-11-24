//! NVIDIA SMI command protocol and CSV parser.

use super::{ProviderSnapshot, ProviderStatus, first_section_line, sanitized_error, section};
use crate::gpu::{GpuDevice, GpuProcess, GpuProvider};

const GPU_QUERY_FIELDS: &str = concat!(
    "index,uuid,pci.bus_id,name,driver_version,pstate,",
    "utilization.gpu,utilization.memory,memory.used,memory.total,",
    "temperature.gpu,power.draw,power.limit,fan.speed"
);
const GPU_PROCESS_QUERY_FIELDS: &str = "gpu_uuid,pid,process_name,used_gpu_memory";
const GPU_TRAILING_FIELD_COUNT: usize = 10;

pub(super) fn sample_command() -> String {
    format!(
        concat!(
            "echo '===NVIDIA_STATUS==='; ",
            "if ! command -v nvidia-smi >/dev/null 2>&1; then ",
            "echo unavailable; ",
            "else ",
            "echo available; ",
            "echo '===NVIDIA_GPUS==='; ",
            "gpu_output=$(LC_ALL=C nvidia-smi --query-gpu={gpu_fields} --format=csv,noheader,nounits 2>&1); ",
            "gpu_exit=$?; printf '%s\\n' \"$gpu_output\"; ",
            "echo '===NVIDIA_GPU_QUERY_EXIT==='; echo \"$gpu_exit\"; ",
            "if [ \"$gpu_exit\" -eq 0 ]; then ",
            "echo '===NVIDIA_PROCESSES==='; ",
            "LC_ALL=C nvidia-smi --query-compute-apps={process_fields} --format=csv,noheader,nounits 2>/dev/null || true; ",
            "else ",
            "echo '===NVIDIA_ERROR==='; printf '%s\\n' \"$gpu_output\"; ",
            "fi; ",
            "fi; "
        ),
        gpu_fields = GPU_QUERY_FIELDS,
        process_fields = GPU_PROCESS_QUERY_FIELDS,
    )
}

pub(super) fn parse(output: &str) -> ProviderSnapshot {
    let mut devices = section(output, "NVIDIA_GPUS")
        .into_iter()
        .flat_map(str::lines)
        .filter_map(parse_device)
        .collect::<Vec<_>>();
    let mut processes = section(output, "NVIDIA_PROCESSES")
        .into_iter()
        .flat_map(str::lines)
        .filter_map(parse_process)
        .collect::<Vec<_>>();
    devices.sort_by_key(|device| device.index);
    processes.sort_by(|left, right| {
        left.gpu_uuid
            .cmp(&right.gpu_uuid)
            .then_with(|| left.pid.cmp(&right.pid))
    });

    let query_exit = first_section_line(output, "NVIDIA_GPU_QUERY_EXIT")
        .and_then(|value| value.parse::<i32>().ok());
    let status = match first_section_line(output, "NVIDIA_STATUS") {
        Some("unavailable") => ProviderStatus::Unavailable,
        Some("available") if query_exit.is_some_and(|exit| exit != 0) => {
            let message = sanitized_error(output, "NVIDIA_ERROR", "NVIDIA nvidia-smi query failed");
            if message
                .to_ascii_lowercase()
                .contains("no devices were found")
            {
                ProviderStatus::NoDevices
            } else {
                ProviderStatus::Error(format!("NVIDIA: {message}"))
            }
        }
        Some("available") if devices.is_empty() => ProviderStatus::NoDevices,
        Some("available") => ProviderStatus::Available,
        _ => ProviderStatus::Unknown,
    };

    ProviderSnapshot {
        status,
        devices,
        processes,
    }
}

fn parse_device(line: &str) -> Option<GpuDevice> {
    let fields = line.split(',').map(str::trim).collect::<Vec<_>>();
    if fields.len() < 4 + GPU_TRAILING_FIELD_COUNT {
        return None;
    }
    let trailing_start = fields.len() - GPU_TRAILING_FIELD_COUNT;
    let name = fields[3..trailing_start].join(", ");
    let trailing = &fields[trailing_start..];

    Some(GpuDevice {
        provider: GpuProvider::Nvidia,
        index: fields[0].parse().ok()?,
        uuid: required_text(fields[1])?,
        pci_bus_id: required_text(fields[2])?,
        name: required_text(&name)?,
        driver_version: optional_text(trailing[0]),
        performance_state: optional_text(trailing[1]),
        health_status: None,
        utilization_percent: optional_number(trailing[2]),
        memory_utilization_percent: optional_number(trailing[3]),
        memory_used: optional_mib(trailing[4]),
        memory_total: optional_mib(trailing[5]),
        temperature_celsius: optional_number(trailing[6]),
        power_draw_watts: optional_number(trailing[7]),
        power_limit_watts: optional_number(trailing[8]),
        fan_speed_percent: optional_number(trailing[9]),
    })
}

fn parse_process(line: &str) -> Option<GpuProcess> {
    let fields = line.split(',').map(str::trim).collect::<Vec<_>>();
    if fields.len() < 4 {
        return None;
    }
    let process_name = fields[2..fields.len() - 1].join(", ");
    Some(GpuProcess {
        provider: GpuProvider::Nvidia,
        gpu_uuid: required_text(fields[0])?,
        pid: fields[1].parse().ok()?,
        process_name: required_text(&process_name)?,
        used_memory: optional_mib(fields[fields.len() - 1]),
    })
}

fn required_text(value: &str) -> Option<String> {
    optional_text(value).filter(|value| !value.is_empty())
}

fn optional_text(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() || is_unavailable_value(value) {
        None
    } else {
        Some(value.to_string())
    }
}

fn optional_number(value: &str) -> Option<f64> {
    if is_unavailable_value(value) {
        return None;
    }
    value
        .trim()
        .trim_end_matches('%')
        .trim()
        .parse::<f64>()
        .ok()
}

fn optional_mib(value: &str) -> Option<u64> {
    let mib = optional_number(value)?;
    Some((mib.max(0.0) * 1024.0 * 1024.0).round() as u64)
}

fn is_unavailable_value(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "n/a" | "[n/a]" | "not supported" | "not available"
    )
}
