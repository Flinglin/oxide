use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use oxide_atomic_file::durable_write_with_before_replace;

use crate::{
    TERMINAL_TRIGGERS_SCHEMA_VERSION, TerminalTriggerError, TerminalTriggersSnapshot,
    validate_snapshot,
};

const TERMINAL_TRIGGERS_FILENAME: &str = "terminal-triggers.json";
const MAX_TERMINAL_TRIGGERS_FILE_BYTES: u64 = 512 * 1024;
static TERMINAL_TRIGGER_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn terminal_triggers_path(settings_path: &Path) -> PathBuf {
    settings_path
        .parent()
        .unwrap_or(settings_path)
        .join(TERMINAL_TRIGGERS_FILENAME)
}

pub fn default_snapshot() -> TerminalTriggersSnapshot {
    TerminalTriggersSnapshot {
        version: TERMINAL_TRIGGERS_SCHEMA_VERSION,
        triggers: Vec::new(),
        updated_at: now_ms(),
    }
}

pub fn load_snapshot(
    settings_path: &Path,
) -> Result<TerminalTriggersSnapshot, TerminalTriggerError> {
    let path = terminal_triggers_path(settings_path);
    let metadata = match fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(default_snapshot()),
        Err(error) => return Err(error.into()),
    };
    if metadata.len() > MAX_TERMINAL_TRIGGERS_FILE_BYTES {
        return Err(TerminalTriggerError::FileTooLarge);
    }
    let contents = fs::read_to_string(path)?;
    if contents.trim().is_empty() {
        return Ok(default_snapshot());
    }
    let snapshot = serde_json::from_str::<TerminalTriggersSnapshot>(&contents)?;
    validate_snapshot(&snapshot)?;
    Ok(snapshot)
}

pub fn save_snapshot(
    settings_path: &Path,
    snapshot: &TerminalTriggersSnapshot,
) -> Result<(), TerminalTriggerError> {
    // Validation compiles every matcher before any configuration reaches disk.
    validate_snapshot(snapshot)?;
    let path = terminal_triggers_path(settings_path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_vec_pretty(snapshot)?;
    if json.len() as u64 > MAX_TERMINAL_TRIGGERS_FILE_BYTES {
        return Err(TerminalTriggerError::FileTooLarge);
    }
    durable_write_with_before_replace(&path, &json, fail_before_atomic_replace_for_tests)?;
    Ok(())
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

pub fn new_trigger_id() -> String {
    format!(
        "trigger-{}-{}",
        now_ms(),
        TERMINAL_TRIGGER_ID_COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

fn fail_before_atomic_replace_for_tests() -> io::Result<()> {
    Ok(())
}
