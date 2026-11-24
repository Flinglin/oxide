//! Hygon DCU sampling protocol and version-tolerant `hy-smi` parser.

use std::collections::BTreeMap;

use super::{ProviderSnapshot, ProviderStatus, first_section_line, sanitized_error, section};
use crate::gpu::{GpuDevice, GpuProcess, GpuProvider};

pub(super) fn sample_command() -> String {
    concat!(
        "echo '===HYGON_STATUS==='; ",
        "hygon_tool=''; hygon_cached_output=''; hygon_cached_exit=''; ",
        "if command -v hy-smi >/dev/null 2>&1; then ",
        "hygon_tool='hy-smi'; ",
        "elif command -v rocm-smi >/dev/null 2>&1; then ",
        "hygon_cached_output=$(LC_ALL=C rocm-smi 2>&1); hygon_cached_exit=$?; ",
        "if printf '%s\\n' \"$hygon_cached_output\" | grep -Eq '(^|[[:space:]])DCU([[:space:]]|$).*DCU%'; then hygon_tool='rocm-smi'; fi; ",
        "fi; ",
        "if [ -z \"$hygon_tool\" ]; then ",
        "echo unavailable; ",
        "else ",
        "echo available; ",
        "echo '===HYGON_DATA==='; ",
        "if [ -n \"$hygon_cached_exit\" ]; then hygon_output=$hygon_cached_output; hygon_exit=$hygon_cached_exit; ",
        "else hygon_output=$(LC_ALL=C \"$hygon_tool\" 2>&1); hygon_exit=$?; fi; ",
        "printf '%s\\n' \"$hygon_output\"; ",
        "echo '===HYGON_QUERY_EXIT==='; echo \"$hygon_exit\"; ",
        "if [ \"$hygon_exit\" -eq 0 ]; then ",
        "echo '===HYGON_PRODUCTS==='; LC_ALL=C \"$hygon_tool\" --showproductname 2>/dev/null || true; ",
        "echo '===HYGON_DRIVER==='; LC_ALL=C \"$hygon_tool\" --showdriverversion 2>/dev/null || true; ",
        "else ",
        "echo '===HYGON_ERROR==='; printf '%s\\n' \"$hygon_output\"; ",
        "fi; ",
        "fi; "
    )
    .to_string()
}

pub(super) fn parse(output: &str) -> ProviderSnapshot {
    match first_section_line(output, "HYGON_STATUS") {
        Some("unavailable") => return empty_snapshot(ProviderStatus::Unavailable),
        Some("available") => {}
        _ => return empty_snapshot(ProviderStatus::Unknown),
    }

    let query_exit =
        first_section_line(output, "HYGON_QUERY_EXIT").and_then(|value| value.parse::<i32>().ok());
    if query_exit.is_some_and(|exit| exit != 0) {
        let message = sanitized_error(output, "HYGON_ERROR", "hy-smi query failed");
        if reports_no_devices(&message) {
            return empty_snapshot(ProviderStatus::NoDevices);
        }
        return empty_snapshot(ProviderStatus::Error(format!("Hygon: {message}")));
    }

    let Some(payload) = section(output, "HYGON_DATA") else {
        return empty_snapshot(ProviderStatus::NoDevices);
    };
    if reports_no_devices(payload) {
        return empty_snapshot(ProviderStatus::NoDevices);
    }

    let products = section(output, "HYGON_PRODUCTS")
        .map(parse_product_names)
        .unwrap_or_default();
    let driver_version = section(output, "HYGON_DRIVER").and_then(parse_driver_version);
    let devices = parse_devices(payload, &products, driver_version.as_deref());
    if devices.is_empty() {
        let status = if payload.trim().is_empty() || looks_like_empty_table(payload) {
            ProviderStatus::NoDevices
        } else {
            ProviderStatus::Error("Hygon: unrecognized hy-smi output".into())
        };
        return empty_snapshot(status);
    }

    let mut processes = parse_processes(payload, &devices);
    processes.sort_by(|left, right| {
        left.gpu_uuid
            .cmp(&right.gpu_uuid)
            .then_with(|| left.pid.cmp(&right.pid))
    });

    ProviderSnapshot {
        status: ProviderStatus::Available,
        devices: devices.into_values().collect(),
        processes,
    }
}

fn parse_devices(
    payload: &str,
    products: &BTreeMap<u32, String>,
    driver_version: Option<&str>,
) -> BTreeMap<u32, GpuDevice> {
    let mut devices = parse_concise_table(payload, products, driver_version);
    parse_hsmi_table(payload, products, driver_version, &mut devices);
    devices
}

fn parse_concise_table(
    payload: &str,
    products: &BTreeMap<u32, String>,
    driver_version: Option<&str>,
) -> BTreeMap<u32, GpuDevice> {
    let lines = payload.lines().collect::<Vec<_>>();
    let Some((header_index, headers)) = lines.iter().enumerate().find_map(|(index, line)| {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        let lower = fields
            .iter()
            .map(|field| field.to_ascii_lowercase())
            .collect::<Vec<_>>();
        (lower.iter().any(|field| field == "dcu")
            && lower.iter().any(|field| field.starts_with("temp"))
            && lower.iter().any(|field| field == "dcu%"))
        .then_some((index, lower))
    }) else {
        return BTreeMap::new();
    };

    let mut devices = BTreeMap::new();
    for line in lines.into_iter().skip(header_index + 1) {
        let values = line.split_whitespace().collect::<Vec<_>>();
        let Some(index) = values.first().and_then(|value| value.parse::<u32>().ok()) else {
            continue;
        };
        if values.len() < headers.len() {
            continue;
        }

        let value = |names: &[&str]| -> Option<&str> {
            let position = headers
                .iter()
                .position(|header| names.iter().any(|name| header == name))?;
            values.get(position).copied()
        };
        let name = products
            .get(&index)
            .cloned()
            .unwrap_or_else(|| format!("Hygon DCU {index}"));
        devices.insert(
            index,
            GpuDevice {
                provider: GpuProvider::Hygon,
                index,
                uuid: format!("HYGON-DCU-{index}"),
                pci_bus_id: format!("HYGON:{index}"),
                name,
                driver_version: driver_version.map(str::to_string),
                performance_state: value(&["perf"]).and_then(optional_text),
                health_status: None,
                utilization_percent: value(&["dcu%"])
                    .and_then(|value| number_with_suffix(value, '%')),
                memory_utilization_percent: value(&["vram%"])
                    .and_then(|value| number_with_suffix(value, '%')),
                memory_used: None,
                memory_total: None,
                temperature_celsius: value(&["temp"])
                    .and_then(|value| number_with_suffix_case_insensitive(value, 'c')),
                power_draw_watts: value(&["avgpwr"])
                    .and_then(|value| number_with_suffix_case_insensitive(value, 'w')),
                power_limit_watts: value(&["pwrcap"])
                    .and_then(|value| number_with_suffix_case_insensitive(value, 'w')),
                fan_speed_percent: value(&["fan"]).and_then(|value| number_with_suffix(value, '%')),
            },
        );
    }
    devices
}

fn parse_hsmi_table(
    payload: &str,
    products: &BTreeMap<u32, String>,
    driver_version: Option<&str>,
    devices: &mut BTreeMap<u32, GpuDevice>,
) {
    let mut pending_index = None;
    for line in payload.lines().map(str::trim) {
        if !line.starts_with('|') {
            continue;
        }
        let columns = line
            .trim_matches('|')
            .split('|')
            .map(str::trim)
            .collect::<Vec<_>>();
        if columns.len() < 3 {
            continue;
        }

        let mut identity = columns[0].split_whitespace();
        if let Some(index) = identity.next().and_then(|value| value.parse::<u32>().ok()) {
            let inline_name = identity.collect::<Vec<_>>().join(" ");
            let name = products
                .get(&index)
                .cloned()
                .or_else(|| optional_text(&inline_name))
                .unwrap_or_else(|| format!("Hygon DCU {index}"));
            let memory = parse_memory_pair(columns[2]);
            let telemetry = columns[1].split_whitespace().collect::<Vec<_>>();
            devices.insert(
                index,
                GpuDevice {
                    provider: GpuProvider::Hygon,
                    index,
                    uuid: format!("HYGON-DCU-{index}"),
                    pci_bus_id: format!("HYGON:{index}"),
                    name,
                    driver_version: driver_version.map(str::to_string),
                    performance_state: None,
                    health_status: None,
                    utilization_percent: None,
                    memory_utilization_percent: memory.and_then(|(used, total)| {
                        (total > 0).then_some((used as f64 / total as f64) * 100.0)
                    }),
                    memory_used: memory.map(|(used, _)| used),
                    memory_total: memory.map(|(_, total)| total),
                    temperature_celsius: telemetry
                        .iter()
                        .find_map(|value| number_with_suffix_case_insensitive(value, 'c')),
                    power_draw_watts: telemetry
                        .iter()
                        .find_map(|value| number_with_suffix_case_insensitive(value, 'w')),
                    power_limit_watts: None,
                    fan_speed_percent: None,
                },
            );
            pending_index = Some(index);
            continue;
        }

        let Some(index) = pending_index else {
            continue;
        };
        let Some(device) = devices.get_mut(&index) else {
            continue;
        };
        device.fan_speed_percent = columns[1]
            .split_whitespace()
            .find_map(|value| number_with_suffix(value, '%'));
        device.utilization_percent = columns[2]
            .split_whitespace()
            .find_map(|value| number_with_suffix(value, '%'));
    }
}

fn parse_processes(payload: &str, devices: &BTreeMap<u32, GpuDevice>) -> Vec<GpuProcess> {
    let mut in_processes = false;
    let mut processes = Vec::new();
    for line in payload.lines().map(str::trim) {
        if line.to_ascii_lowercase().contains("processes:") {
            in_processes = true;
            continue;
        }
        if !in_processes || !line.starts_with('|') {
            continue;
        }
        let fields = line
            .trim_matches('|')
            .split_whitespace()
            .collect::<Vec<_>>();
        let Some(index) = fields.first().and_then(|value| value.parse::<u32>().ok()) else {
            continue;
        };
        let Some((pid_position, pid)) = fields
            .iter()
            .enumerate()
            .skip(1)
            .find_map(|(position, value)| value.parse::<u32>().ok().map(|pid| (position, pid)))
        else {
            continue;
        };
        let Some(device) = devices.get(&index) else {
            continue;
        };
        let Some((memory_position, used_memory)) = fields
            .iter()
            .enumerate()
            .rev()
            .find_map(|(position, value)| parse_byte_value(value).map(|bytes| (position, bytes)))
        else {
            continue;
        };
        let mut name_start = pid_position + 1;
        if fields.get(name_start).is_some_and(|value| value.len() == 1) {
            name_start += 1;
        }
        let process_name = fields[name_start..memory_position].join(" ");
        processes.push(GpuProcess {
            provider: GpuProvider::Hygon,
            gpu_uuid: device.uuid.clone(),
            pid,
            process_name: optional_text(&process_name).unwrap_or_else(|| format!("PID {pid}")),
            used_memory: Some(used_memory),
        });
    }
    processes
}

fn parse_product_names(payload: &str) -> BTreeMap<u32, String> {
    let mut products = BTreeMap::new();
    for line in payload.lines() {
        let lower = line.to_ascii_lowercase();
        if !lower.contains("card series") && !lower.contains("product name") {
            continue;
        }
        let Some((_, value)) = line.rsplit_once(':') else {
            continue;
        };
        let Some(index) = accelerator_index(line) else {
            continue;
        };
        if let Some(name) = optional_text(value) {
            products.insert(index, name);
        }
    }
    products
}

fn accelerator_index(value: &str) -> Option<u32> {
    let normalized = value.replace(['[', ']', ':', '='], " ");
    let fields = normalized.split_whitespace().collect::<Vec<_>>();
    fields.windows(2).find_map(|pair| {
        matches!(
            pair[0].to_ascii_lowercase().as_str(),
            "hcu" | "dcu" | "gpu" | "card"
        )
        .then(|| pair[1].parse::<u32>().ok())
        .flatten()
    })
}

fn parse_driver_version(payload: &str) -> Option<String> {
    payload.lines().find_map(|line| {
        let value = line
            .split_once(':')
            .map(|(_, value)| value)
            .unwrap_or(line)
            .trim();
        optional_text(value)
            .filter(|value| value.chars().any(|character| character.is_ascii_digit()))
    })
}

fn parse_memory_pair(value: &str) -> Option<(u64, u64)> {
    let (used, total) = value.split_once('/')?;
    Some((parse_byte_value(used)?, parse_byte_value(total)?))
}

fn parse_byte_value(value: &str) -> Option<u64> {
    let value = value.trim().trim_end_matches('|');
    let unit_start = value
        .char_indices()
        .find(|(_, character)| character.is_ascii_alphabetic())?
        .0;
    let number = value[..unit_start].trim().parse::<f64>().ok()?.max(0.0);
    let multiplier = match value[unit_start..].trim().to_ascii_lowercase().as_str() {
        "b" => 1.0,
        "kb" | "kib" => 1024.0,
        "mb" | "mib" => 1024.0 * 1024.0,
        "gb" | "gib" => 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    Some((number * multiplier).round().min(u64::MAX as f64) as u64)
}

fn number_with_suffix(value: &str, suffix: char) -> Option<f64> {
    value
        .trim()
        .strip_suffix(suffix)?
        .trim()
        .parse::<f64>()
        .ok()
}

fn number_with_suffix_case_insensitive(value: &str, suffix: char) -> Option<f64> {
    let value = value.trim();
    let last = value.chars().last()?;
    last.eq_ignore_ascii_case(&suffix)
        .then(|| {
            value[..value.len() - last.len_utf8()]
                .trim()
                .parse::<f64>()
                .ok()
        })
        .flatten()
}

fn optional_text(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()
        && !matches!(
            value.to_ascii_lowercase().as_str(),
            "n/a" | "na" | "unknown"
        ))
    .then(|| value.to_string())
}

fn looks_like_empty_table(payload: &str) -> bool {
    payload
        .to_ascii_lowercase()
        .contains("system management interface")
        && !payload.lines().any(|line| {
            line.split_whitespace()
                .next()
                .is_some_and(|value| value.parse::<u32>().is_ok())
        })
}

fn reports_no_devices(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    value.contains("no dcu")
        || value.contains("no gpu")
        || value.contains("no device")
        || value.contains("device not found")
}

fn empty_snapshot(status: ProviderStatus) -> ProviderSnapshot {
    ProviderSnapshot {
        status,
        devices: Vec::new(),
        processes: Vec::new(),
    }
}
