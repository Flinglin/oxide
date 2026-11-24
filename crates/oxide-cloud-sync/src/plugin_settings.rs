use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};

use oxide_atomic_file::{durable_remove, durable_write_with_before_replace};
use oxide_connections::oxide_file::EncryptedPluginSetting;
use serde::{Deserialize, Serialize};

const PLUGIN_SETTINGS_FILENAME: &str = "plugin-settings.json";
const PLUGIN_SETTINGS_SCHEMA_VERSION: u32 = 1;

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PluginSettingsSnapshot {
    version: u32,
    settings: Vec<EncryptedPluginSetting>,
}

#[derive(Serialize, Deserialize)]
enum PluginSettingsFileState {
    Missing,
    Present(Vec<u8>),
}

/// Opaque on-disk state used to restore plugin settings after a failed transaction.
#[derive(Serialize, Deserialize)]
pub struct PluginSettingsCheckpoint {
    state: PluginSettingsFileState,
}

pub fn plugin_settings_path(settings_path: &Path) -> PathBuf {
    settings_path
        .parent()
        .unwrap_or(settings_path)
        .join(PLUGIN_SETTINGS_FILENAME)
}

pub fn load_plugin_settings(settings_path: &Path) -> Result<Vec<EncryptedPluginSetting>, String> {
    let path = plugin_settings_path(settings_path);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let contents = fs::read_to_string(&path).map_err(|err| err.to_string())?;
    if contents.trim().is_empty() {
        return Ok(Vec::new());
    }
    let snapshot =
        serde_json::from_str::<PluginSettingsSnapshot>(&contents).map_err(|err| err.to_string())?;
    Ok(snapshot.settings)
}

pub fn checkpoint_plugin_settings(
    settings_path: &Path,
) -> Result<PluginSettingsCheckpoint, String> {
    let path = plugin_settings_path(settings_path);
    let state = match fs::read(&path) {
        Ok(contents) => PluginSettingsFileState::Present(contents),
        Err(error) if error.kind() == io::ErrorKind::NotFound => PluginSettingsFileState::Missing,
        Err(error) => return Err(error.to_string()),
    };
    Ok(PluginSettingsCheckpoint { state })
}

pub fn restore_plugin_settings(
    settings_path: &Path,
    checkpoint: &PluginSettingsCheckpoint,
) -> Result<(), String> {
    let path = plugin_settings_path(settings_path);
    match &checkpoint.state {
        PluginSettingsFileState::Missing => remove_restored_plugin_settings_file(&path),
        PluginSettingsFileState::Present(contents) => {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            }
            atomic_write_file(&path, contents).map_err(|error| error.to_string())
        }
    }
}

pub fn upsert_plugin_settings(
    settings_path: &Path,
    incoming: &[EncryptedPluginSetting],
) -> Result<usize, String> {
    if incoming.is_empty() {
        return Ok(0);
    }
    let path = plugin_settings_path(settings_path);
    let mut existing = load_plugin_settings(settings_path)?;
    for entry in incoming {
        if let Some(current) = existing
            .iter_mut()
            .find(|candidate| candidate.storage_key == entry.storage_key)
        {
            *current = entry.clone();
        } else {
            existing.push(entry.clone());
        }
    }
    existing.sort_by(|left, right| left.storage_key.cmp(&right.storage_key));
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    let snapshot = PluginSettingsSnapshot {
        version: PLUGIN_SETTINGS_SCHEMA_VERSION,
        settings: existing,
    };
    let json = serde_json::to_vec_pretty(&snapshot).map_err(|err| err.to_string())?;
    atomic_write_file(&path, &json).map_err(|err| err.to_string())?;
    Ok(incoming.len())
}

/// Replaces effective plugin settings after the coordinator has preserved
/// unselected entries and captured a durable owner checkpoint.
pub fn replace_resolved_plugin_settings(
    settings_path: &Path,
    mut settings: Vec<EncryptedPluginSetting>,
) -> Result<(), String> {
    settings.extend(
        load_plugin_settings(settings_path)?
            .into_iter()
            .filter(|entry| {
                plugin_id_from_setting_storage_key(&entry.storage_key)
                    .is_none_or(|id| id == crate::CLOUD_SYNC_PLUGIN_ID)
            }),
    );
    let snapshot = PluginSettingsSnapshot {
        version: PLUGIN_SETTINGS_SCHEMA_VERSION,
        settings,
    };
    let contents = zeroize::Zeroizing::new(
        serde_json::to_vec_pretty(&snapshot)
            .map_err(|_| "Cannot encode plugin settings".to_string())?,
    );
    atomic_write_file(&plugin_settings_path(settings_path), &contents)
        .map_err(|error| error.to_string())
}

pub fn plugin_settings_revision_map(
    settings_path: &Path,
) -> Result<BTreeMap<String, String>, String> {
    let entries = load_plugin_settings(settings_path)?;
    let mut grouped = BTreeMap::<String, Vec<EncryptedPluginSetting>>::new();
    for entry in entries {
        let Some(plugin_id) = plugin_id_from_setting_storage_key(&entry.storage_key) else {
            continue;
        };
        grouped.entry(plugin_id).or_default().push(entry);
    }

    let mut revisions = BTreeMap::new();
    for (plugin_id, mut entries) in grouped {
        entries.sort_by(|left, right| left.storage_key.cmp(&right.storage_key));
        let normalized = entries
            .into_iter()
            .map(|entry| vec![entry.storage_key, entry.serialized_value])
            .collect::<Vec<_>>();
        let json = serde_json::to_string(&normalized).map_err(|err| err.to_string())?;
        revisions.insert(plugin_id, tauri_fnv1a_stable_hash_text(&json));
    }
    Ok(revisions)
}

pub fn plugin_id_from_setting_storage_key(storage_key: &str) -> Option<String> {
    const PREFIX: &str = "oxide-plugin-";
    const SEPARATOR: &str = "-setting-";
    let remainder = storage_key.strip_prefix(PREFIX)?;
    let separator_index = remainder.find(SEPARATOR)?;
    let plugin_id = &remainder[..separator_index];
    let setting_id = &remainder[separator_index + SEPARATOR.len()..];
    (!plugin_id.is_empty() && !setting_id.is_empty()).then(|| plugin_id.to_string())
}

fn tauri_fnv1a_stable_hash_text(text: &str) -> String {
    let mut hash = 2166136261u32;
    for code_unit in text.encode_utf16() {
        hash ^= u32::from(code_unit);
        hash = hash.wrapping_mul(16777619);
    }
    format!("fnv1a-{hash:x}")
}

fn atomic_write_file(path: &Path, contents: &[u8]) -> io::Result<()> {
    durable_write_with_before_replace(path, contents, fail_before_atomic_replace_for_tests)
}

fn remove_restored_plugin_settings_file(path: &Path) -> Result<(), String> {
    fail_before_restore_delete_for_tests().map_err(|error| error.to_string())?;
    durable_remove(path).map_err(|error| error.to_string())
}

fn fail_before_atomic_replace_for_tests() -> io::Result<()> {
    Ok(())
}

fn fail_before_restore_delete_for_tests() -> io::Result<()> {
    Ok(())
}
