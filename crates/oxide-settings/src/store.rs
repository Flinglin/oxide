use std::{
    fs, io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow};
use oxide_atomic_file::{durable_remove, durable_write_with_before_replace};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    model::{PersistedSettings, SETTINGS_SCHEMA_VERSION},
    normalize::sanitize_settings_value,
};

pub const SETTINGS_FILENAME: &str = "settings.json";
const MAX_SETTINGS_FILE_BYTES: u64 = 2 * 1024 * 1024;
const BOOTSTRAP_FILENAME: &str = "bootstrap.json";

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct BootstrapConfig {
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    data_dir: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DataDirectoryInfo {
    pub path: PathBuf,
    pub is_custom: bool,
    pub default_path: PathBuf,
    pub is_portable: bool,
    pub can_change: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DataDirectoryCheck {
    pub has_existing_data: bool,
    pub files_found: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsEnvelope {
    pub version: u32,
    pub settings: PersistedSettings,
    pub updated_at: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SettingsLoadResult {
    pub settings: PersistedSettings,
    pub version: u32,
    pub updated_at: u64,
    pub migration_warnings: Vec<String>,
    pub validation_warnings: Vec<String>,
    pub recovered_from_corrupt_file: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SettingsSaveResult {
    pub settings: PersistedSettings,
    pub version: u32,
    pub updated_at: u64,
    pub validation_warnings: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct SettingsStore {
    path: PathBuf,
    settings: PersistedSettings,
    updated_at: u64,
    writes_blocked: bool,
}

#[derive(Serialize, Deserialize)]
enum SettingsFileCheckpoint {
    Missing,
    Present(Vec<u8>),
}

/// Opaque rollback state for the exact settings file and in-memory envelope.
#[must_use = "settings checkpoints should be restored or deliberately discarded"]
#[derive(Serialize, Deserialize)]
pub struct SettingsStoreCheckpoint {
    path: PathBuf,
    settings: PersistedSettings,
    updated_at: u64,
    writes_blocked: bool,
    file: SettingsFileCheckpoint,
}

impl std::fmt::Debug for SettingsStoreCheckpoint {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SettingsStoreCheckpoint")
            .field("path", &self.path)
            .field("contents", &"[redacted settings checkpoint]")
            .finish()
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

pub fn default_settings_path() -> PathBuf {
    //如果是便携版，就从exe所在目录加载设置文件
    if let Ok(Some(data_dir)) = oxide_portable_runtime::portable_data_dir() {
        return user_visible_data_dir_path(data_dir).join(SETTINGS_FILENAME);
    }

    //如果是安装版，就从Oxide数据目录加载设置文件
    if let Some(data_dir) = bootstrap_data_dir() {
        return data_dir.join(SETTINGS_FILENAME);
    }

    //如果数据目录不存在设置，就加载默认设置文件
    default_settings_dir().join(SETTINGS_FILENAME)
}

fn default_settings_dir() -> PathBuf {
    if cfg!(windows) {
        if let Some(config_home) = std::env::var_os("APPDATA") {
            return PathBuf::from(config_home).join("oxide");
        }
    }

    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".oxide");
    }

    PathBuf::from(".")
}

pub fn data_directory_info() -> Result<DataDirectoryInfo> {
    let is_portable = oxide_portable_runtime::is_portable_mode()
        .map_err(|error| anyhow!("failed to detect portable mode: {}", error))?;
    let default_path = user_visible_data_dir_path(default_settings_dir());
    let path = if is_portable {
        oxide_portable_runtime::portable_data_dir()
            .map_err(|error| anyhow!("failed to resolve portable data directory: {}", error))?
            .map(user_visible_data_dir_path)
            .unwrap_or_else(|| default_path.clone())
    } else {
        bootstrap_data_dir().unwrap_or_else(|| default_path.clone())
    };
    Ok(DataDirectoryInfo {
        is_custom: !is_portable && path != default_path,
        can_change: !is_portable,
        is_portable,
        path,
        default_path,
    })
}

pub fn check_data_directory(path: &Path) -> Result<DataDirectoryCheck> {
    if !path.is_dir() {
        return Ok(DataDirectoryCheck {
            has_existing_data: false,
            files_found: Vec::new(),
        });
    }

    let known_files = [
        "connections.json",
        "state.redb",
        "chat_history.redb",
        "agent_history.redb",
        "sftp_progress.redb",
        "rag_index.redb",
        "plugin-config.json",
        "bootstrap.json",
        "topology_edges.json",
    ];
    let mut files_found = Vec::new();
    for name in known_files {
        if path.join(name).exists() {
            files_found.push(name.to_string());
        }
    }
    for name in ["logs", "plugins", "rag_hnsw.bin"] {
        if path.join(name).exists() {
            files_found.push(name.to_string());
        }
    }

    Ok(DataDirectoryCheck {
        has_existing_data: !files_found.is_empty(),
        files_found,
    })
}

pub fn set_data_directory(path: &Path) -> Result<()> {
    if oxide_portable_runtime::is_portable_mode()
        .map_err(|error| anyhow!("failed to detect portable mode: {}", error))?
    {
        return Err(anyhow!("Data directory cannot be changed in portable mode"));
    }
    if !path.is_absolute() {
        return Err(anyhow!("Data directory must be an absolute path"));
    }
    if path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(anyhow!("Data directory path must not contain '..'"));
    }

    fs::create_dir_all(path).context("Failed to create directory")?;
    let canonical = path.canonicalize().context("Failed to resolve path")?;
    let test_file = canonical.join(format!(".oxide_test_{}", std::process::id()));
    // Tauri verifies writability before writing bootstrap.json; keep the same
    // guard so a restart never points native at an unusable data directory.
    fs::write(&test_file, b"test").context("Directory is not writable")?;
    let _ = fs::remove_file(&test_file);

    let data_dir = user_visible_data_dir_path(canonical);
    save_bootstrap_config(&BootstrapConfig {
        data_dir: Some(data_dir.to_string_lossy().to_string()),
    })
}

pub fn reset_data_directory() -> Result<()> {
    if oxide_portable_runtime::is_portable_mode()
        .map_err(|error| anyhow!("failed to detect portable mode: {}", error))?
    {
        return Err(anyhow!("Data directory cannot be reset in portable mode"));
    }
    save_bootstrap_config(&BootstrapConfig::default())
}

fn save_bootstrap_config(config: &BootstrapConfig) -> Result<()> {
    let path = default_settings_dir().join(BOOTSTRAP_FILENAME);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).context("failed to create bootstrap directory")?;
    }
    let bytes =
        serde_json::to_vec_pretty(config).context("failed to serialize bootstrap config")?;
    atomic_write_file(&path, &bytes).context("failed to write bootstrap config")
}

fn bootstrap_data_dir() -> Option<PathBuf> {
    //存储oxide数据目录路径的文件位于%APPDATA%\Roaming\Oxide
    let path = default_settings_dir().join(BOOTSTRAP_FILENAME);
    let contents = fs::read_to_string(path).ok()?;
    let bootstrap: BootstrapConfig = serde_json::from_str(&contents).ok()?;
    let data_dir = user_visible_data_dir_path(PathBuf::from(bootstrap.data_dir?));
    data_dir.is_absolute().then_some(data_dir)
}

fn user_visible_data_dir_path(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    // Windows canonicalize() returns verbatim paths such as `\\?\D:\...`.
    // Bootstrap and settings UI should keep the user-facing path form.
    if let Some(stripped) = text.strip_prefix("\\\\?\\UNC\\") {
        return PathBuf::from(format!("\\\\{stripped}"));
    }
    if let Some(stripped) = text.strip_prefix("\\\\?\\") {
        return PathBuf::from(stripped);
    }
    path
}

pub fn save_settings_to_path(
    path: &Path,
    settings: PersistedSettings,
) -> Result<SettingsSaveResult> {
    let mut audit = if oxide_audit::AuditContext::current_request()
        .as_ref()
        .is_some_and(|context| context.protocol.as_deref() == Some("cloud_sync_apply"))
    {
        // The cross-store transaction reports the final result after commit or rollback.
        oxide_audit::AuditOperation::in_context(
            None,
            oxide_audit::AuditCategory::Configuration,
            "settings_save",
            None,
        )
    } else {
        oxide_audit::AuditOperation::begin(
            oxide_audit::AuditCategory::Configuration,
            "settings_save",
            None,
            None,
        )
    };
    let audit_result = (|| {
        // Non-GPUI writers share the same sanitize-and-envelope path as SettingsStore::save.
        let sanitized = sanitize_settings_value(settings.to_value())?;
        let previous = read_envelope(path).ok().flatten().map(|(value, _)| value);
        let changed_sections = changed_settings_sections(previous.as_ref(), &sanitized.settings);
        audit.summary(&changed_sections.join(","));
        let updated_at = now_ms();
        write_envelope(path, &sanitized.settings, updated_at)?;
        Ok((
            SettingsSaveResult {
                settings: sanitized.settings,
                version: SETTINGS_SCHEMA_VERSION,
                updated_at,
                validation_warnings: sanitized.validation_warnings,
            },
            !changed_sections.is_empty(),
        ))
    })();
    match &audit_result {
        Ok((_, true)) => audit.finish(
            oxide_audit::AuditOutcome::Succeeded,
            oxide_audit::AuditEvidence::Protocol,
            None,
            None,
        ),
        Ok((_, false)) => audit.finish(
            oxide_audit::AuditOutcome::Unchanged,
            oxide_audit::AuditEvidence::Protocol,
            None,
            None,
        ),
        Err(_) => audit.finish(
            oxide_audit::AuditOutcome::Failed,
            oxide_audit::AuditEvidence::Protocol,
            None,
            None,
        ),
    }
    audit_result.map(|(saved, _)| saved)
}

fn changed_settings_sections(previous: Option<&Value>, current: &PersistedSettings) -> Vec<String> {
    let current = current.to_value();
    let schema = PersistedSettings::default().to_value();
    let mut changed = Vec::new();
    let Some(previous) = previous else {
        return vec!["settings".to_string()];
    };
    let Some(schema_sections) = schema.as_object() else {
        return vec!["settings".to_string()];
    };
    for (section, defaults) in schema_sections {
        let before = previous.get(section);
        let after = current.get(section);
        if before == after {
            continue;
        }
        match defaults.as_object() {
            Some(fields) if !fields.is_empty() => {
                let changed_before = changed.len();
                for field in fields.keys() {
                    if before.and_then(|value| value.get(field))
                        != after.and_then(|value| value.get(field))
                    {
                        changed.push(format!("{section}.{field}"));
                    }
                }
                let dynamic_before = before.and_then(Value::as_object).map(|map| {
                    map.iter()
                        .filter(|(key, _)| !fields.contains_key(*key))
                        .collect::<Vec<_>>()
                });
                let dynamic_after = after.and_then(Value::as_object).map(|map| {
                    map.iter()
                        .filter(|(key, _)| !fields.contains_key(*key))
                        .collect::<Vec<_>>()
                });
                // Dynamic keys are never copied into audit metadata.
                if dynamic_before != dynamic_after || changed.len() == changed_before {
                    changed.push(section.clone());
                }
            }
            _ => changed.push(section.clone()),
        }
    }
    let unknown_before = previous.as_object().map(|map| {
        map.iter()
            .filter(|(key, _)| !schema_sections.contains_key(*key))
            .collect::<Vec<_>>()
    });
    let unknown_after = current.as_object().map(|map| {
        map.iter()
            .filter(|(key, _)| !schema_sections.contains_key(*key))
            .collect::<Vec<_>>()
    });
    if unknown_before != unknown_after {
        changed.push("other".to_string());
    }
    changed
}

fn read_envelope(path: &Path) -> Result<Option<(Value, u64)>> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(anyhow!("failed to stat settings file: {}", err)),
    };
    if metadata.len() > MAX_SETTINGS_FILE_BYTES {
        return Err(anyhow!("settings file exceeds size limit"));
    }
    let contents = fs::read_to_string(path).context("failed to read settings file")?;
    if contents.trim().is_empty() {
        return Err(anyhow!("settings file is empty"));
    }
    let value: Value = serde_json::from_str(&contents).context("failed to parse settings file")?;
    if value.get("settings").is_some() {
        let envelope_version = value
            .get("version")
            .and_then(Value::as_u64)
            .ok_or_else(|| anyhow!("settings envelope version is missing or invalid"))?;
        if envelope_version > u64::from(SETTINGS_SCHEMA_VERSION) {
            return Err(anyhow!(
                "settings envelope version {envelope_version} is newer than supported version {SETTINGS_SCHEMA_VERSION}"
            ));
        }
        let updated_at = value
            .get("updatedAt")
            .or_else(|| value.get("updated_at"))
            .and_then(Value::as_u64)
            .unwrap_or_else(now_ms);
        Ok(Some((value["settings"].clone(), updated_at)))
    } else {
        Ok(Some((value, now_ms())))
    }
}

fn write_envelope(path: &Path, settings: &PersistedSettings, updated_at: u64) -> Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).context("failed to create settings directory")?;
    }
    let envelope = SettingsEnvelope {
        version: SETTINGS_SCHEMA_VERSION,
        settings: settings.clone(),
        updated_at,
    };
    let json = serde_json::to_vec_pretty(&envelope).context("failed to serialize settings")?;
    if json.len() as u64 > MAX_SETTINGS_FILE_BYTES {
        return Err(anyhow!("settings snapshot exceeds size limit"));
    }
    atomic_write_file(path, &json).context("failed to replace settings file")
}

fn atomic_write_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    durable_write_with_before_replace(path, bytes, fail_before_atomic_replace_for_tests)
}

fn fail_before_atomic_replace_for_tests() -> io::Result<()> {
    Ok(())
}

impl SettingsStore {
    pub fn from_read_only(path: impl Into<PathBuf>, settings: PersistedSettings) -> Self {
        // CLI previews need the same in-memory shape as SettingsStore without triggering
        // migrations or envelope rewrites in the user's settings directory.
        Self {
            path: path.into(),
            settings,
            updated_at: 0,
            writes_blocked: true,
        }
    }

    pub fn load_default() -> Result<Self> {
        Self::load_from_path(default_settings_path())
    }

    pub fn load_from_path(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let load = load_settings_from_path(&path)?;
        Ok(Self {
            path,
            settings: load.settings,
            updated_at: load.updated_at,
            writes_blocked: load.recovered_from_corrupt_file,
        })
    }

    pub fn settings(&self) -> &PersistedSettings {
        &self.settings
    }

    pub fn settings_mut(&mut self) -> &mut PersistedSettings {
        &mut self.settings
    }

    pub fn updated_at(&self) -> u64 {
        self.updated_at
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn create_checkpoint(&self) -> Result<SettingsStoreCheckpoint> {
        let file = match fs::read(&self.path) {
            Ok(bytes) => SettingsFileCheckpoint::Present(bytes),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                SettingsFileCheckpoint::Missing
            }
            Err(error) => return Err(error).context("failed to checkpoint settings file"),
        };
        Ok(SettingsStoreCheckpoint {
            path: self.path.clone(),
            settings: self.settings.clone(),
            updated_at: self.updated_at,
            writes_blocked: self.writes_blocked,
            file,
        })
    }

    pub fn restore_checkpoint(&mut self, checkpoint: &SettingsStoreCheckpoint) -> Result<()> {
        if self.path != checkpoint.path {
            return Err(anyhow!("settings checkpoint belongs to a different store"));
        }
        // Restore disk first so memory continues to describe the current file
        // if the durable rollback itself fails.
        match &checkpoint.file {
            SettingsFileCheckpoint::Missing => {
                if self.path.exists() {
                    durable_remove(&self.path).context("failed to remove settings file")?;
                }
            }
            SettingsFileCheckpoint::Present(bytes) => {
                atomic_write_file(&self.path, bytes).context("failed to restore settings file")?;
            }
        }
        self.settings = checkpoint.settings.clone();
        self.updated_at = checkpoint.updated_at;
        self.writes_blocked = checkpoint.writes_blocked;
        Ok(())
    }

    pub fn save(&mut self) -> Result<SettingsSaveResult> {
        if self.writes_blocked {
            return Err(anyhow!(
                "refusing to overwrite an unreadable or newer settings file"
            ));
        }
        let saved = save_settings_to_path(&self.path, self.settings.clone())?;
        self.settings = saved.settings.clone();
        self.updated_at = saved.updated_at;
        Ok(saved)
    }

    pub fn replace_and_save(&mut self, settings: PersistedSettings) -> Result<SettingsSaveResult> {
        if self.writes_blocked {
            return Err(anyhow!(
                "refusing to overwrite an unreadable or newer settings file"
            ));
        }
        // Commit the in-memory replacement only after the durable file swap succeeds.
        let saved = save_settings_to_path(&self.path, settings)?;
        self.settings = saved.settings.clone();
        self.updated_at = saved.updated_at;
        Ok(saved)
    }
}

pub fn load_settings_from_path(path: &Path) -> Result<SettingsLoadResult> {
    let mut recovered_from_corrupt_file = false;
    let mut migration_warnings = Vec::new();
    let mut validation_warnings = Vec::new();

    let (raw, updated_at, should_persist) = match read_envelope(path) {
        Ok(Some((raw, updated_at))) => (raw, updated_at, true),
        Ok(None) => (PersistedSettings::default().to_value(), now_ms(), true),
        Err(err) => {
            recovered_from_corrupt_file = true;
            migration_warnings.push(format!("Recovered from unreadable settings file: {}", err));
            (PersistedSettings::default().to_value(), now_ms(), false)
        }
    };

    let sanitized = sanitize_settings_value(raw)?;
    migration_warnings.extend(sanitized.migration_warnings);
    validation_warnings.extend(sanitized.validation_warnings);
    if should_persist {
        write_envelope(path, &sanitized.settings, updated_at)?;
    }

    Ok(SettingsLoadResult {
        settings: sanitized.settings,
        version: SETTINGS_SCHEMA_VERSION,
        updated_at,
        migration_warnings,
        validation_warnings,
        recovered_from_corrupt_file,
    })
}
