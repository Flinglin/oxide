use std::{
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use redb::ReadableTable;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::{RemoteRelayDisposition, SftpError};

const REMOTE_RELAY_PROGRESS_VERSION: u8 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteRelayProgressContext {
    pub profile_id: String,
    pub profile_revision: String,
    pub source_endpoint_id: String,
    pub destination_endpoint_id: String,
}

impl RemoteRelayProgressContext {
    pub fn storage_key(&self) -> String {
        format!("standalone-sftp-relay:{}", self.profile_id)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredRemoteRelayProgress {
    #[serde(default = "default_remote_relay_progress_version")]
    pub version: u8,
    pub profile_id: String,
    pub profile_revision: String,
    pub source_endpoint_id: String,
    pub destination_endpoint_id: String,
    pub staging_path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup_path: Option<PathBuf>,
    pub disposition: RemoteRelayDisposition,
    pub source_size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_modified: Option<u32>,
    pub source_sample_sha256: String,
}

const fn default_remote_relay_progress_version() -> u8 {
    REMOTE_RELAY_PROGRESS_VERSION
}

impl StoredRemoteRelayProgress {
    pub fn new(
        context: &RemoteRelayProgressContext,
        staging_path: PathBuf,
        backup_path: Option<PathBuf>,
        disposition: RemoteRelayDisposition,
        source_size: u64,
        source_modified: Option<u32>,
        source_sample_sha256: String,
    ) -> Self {
        Self {
            version: REMOTE_RELAY_PROGRESS_VERSION,
            profile_id: context.profile_id.clone(),
            profile_revision: context.profile_revision.clone(),
            source_endpoint_id: context.source_endpoint_id.clone(),
            destination_endpoint_id: context.destination_endpoint_id.clone(),
            staging_path,
            backup_path,
            disposition,
            source_size,
            source_modified,
            source_sample_sha256,
        }
    }

    pub fn supports_resume(&self) -> bool {
        self.version == REMOTE_RELAY_PROGRESS_VERSION
    }

    pub fn contains_endpoint(&self, endpoint_id: &str) -> bool {
        self.source_endpoint_id == endpoint_id || self.destination_endpoint_id == endpoint_id
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredTransferProgress {
    pub transfer_id: String,
    pub transfer_type: TransferType,
    #[serde(default)]
    pub protocol: TransferProtocol,
    #[serde(default)]
    pub strategy: TransferStrategy,
    pub source_path: PathBuf,
    pub destination_path: PathBuf,
    pub transferred_bytes: u64,
    pub total_bytes: u64,
    pub status: TransferStatus,
    pub last_updated: DateTime<Utc>,
    pub session_id: String,
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_relay: Option<StoredRemoteRelayProgress>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum TransferProtocol {
    #[default]
    Sftp,
    Scp,
    Ftp,
    Ftps,
}

impl TransferProtocol {
    /// SCP can pause a live channel but cannot restart from a byte offset.
    pub const fn supports_restart_resume(self) -> bool {
        matches!(self, Self::Sftp)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TransferType {
    Upload,
    Download,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum TransferStrategy {
    #[default]
    File,
    DirectoryRecursive,
    DirectoryTar,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TransferStatus {
    Active,
    Paused,
    Failed,
    Completed,
    Cancelled,
}

impl StoredTransferProgress {
    pub fn new(
        transfer_id: String,
        transfer_type: TransferType,
        source_path: PathBuf,
        destination_path: PathBuf,
        total_bytes: u64,
        session_id: String,
    ) -> Self {
        Self {
            transfer_id,
            transfer_type,
            protocol: TransferProtocol::Sftp,
            strategy: TransferStrategy::File,
            source_path,
            destination_path,
            transferred_bytes: 0,
            total_bytes,
            status: TransferStatus::Active,
            last_updated: Utc::now(),
            session_id,
            error: None,
            remote_relay: None,
        }
    }

    pub fn progress_percent(&self) -> f64 {
        if self.total_bytes == 0 {
            0.0
        } else {
            (self.transferred_bytes as f64 / self.total_bytes as f64) * 100.0
        }
    }

    pub fn is_incomplete(&self) -> bool {
        matches!(self.status, TransferStatus::Paused | TransferStatus::Failed)
    }

    pub fn is_active(&self) -> bool {
        self.status == TransferStatus::Active
    }

    pub fn is_directory(&self) -> bool {
        self.strategy != TransferStrategy::File
    }

    pub fn update_progress(&mut self, transferred_bytes: u64) {
        self.transferred_bytes = transferred_bytes;
        self.last_updated = Utc::now();
    }

    pub fn mark_completed(&mut self) {
        self.status = TransferStatus::Completed;
        self.transferred_bytes = self.total_bytes;
        self.error = None;
        self.last_updated = Utc::now();
    }

    pub fn mark_failed(&mut self, error: String) {
        self.status = TransferStatus::Failed;
        self.error = Some(error);
        self.last_updated = Utc::now();
    }

    pub fn mark_paused(&mut self) {
        self.status = TransferStatus::Paused;
        self.last_updated = Utc::now();
    }

    pub fn mark_cancelled(&mut self) {
        self.status = TransferStatus::Cancelled;
        self.last_updated = Utc::now();
    }

    pub fn mark_active(&mut self) {
        self.status = TransferStatus::Active;
        self.error = None;
        self.last_updated = Utc::now();
    }
}

#[async_trait]
pub trait ProgressStore: Send + Sync {
    async fn save(&self, progress: &StoredTransferProgress) -> Result<(), SftpError>;
    async fn load(&self, transfer_id: &str) -> Result<Option<StoredTransferProgress>, SftpError>;
    async fn list_incomplete(
        &self,
        session_id: &str,
    ) -> Result<Vec<StoredTransferProgress>, SftpError>;
    async fn list_all_incomplete(&self) -> Result<Vec<StoredTransferProgress>, SftpError>;
    async fn delete(&self, transfer_id: &str) -> Result<(), SftpError>;
    async fn delete_for_session(&self, session_id: &str) -> Result<(), SftpError>;
}

pub struct DummyProgressStore;

#[async_trait]
impl ProgressStore for DummyProgressStore {
    async fn save(&self, _progress: &StoredTransferProgress) -> Result<(), SftpError> {
        Ok(())
    }

    async fn load(&self, _transfer_id: &str) -> Result<Option<StoredTransferProgress>, SftpError> {
        Ok(None)
    }

    async fn list_incomplete(
        &self,
        _session_id: &str,
    ) -> Result<Vec<StoredTransferProgress>, SftpError> {
        Ok(Vec::new())
    }

    async fn list_all_incomplete(&self) -> Result<Vec<StoredTransferProgress>, SftpError> {
        Ok(Vec::new())
    }

    async fn delete(&self, _transfer_id: &str) -> Result<(), SftpError> {
        Ok(())
    }

    async fn delete_for_session(&self, _session_id: &str) -> Result<(), SftpError> {
        Ok(())
    }
}

pub struct LazyProgressStore {
    db_path: PathBuf,
    store: OnceLock<Arc<dyn ProgressStore>>,
}

impl LazyProgressStore {
    pub fn new(db_path: impl Into<PathBuf>) -> Self {
        Self {
            db_path: db_path.into(),
            store: OnceLock::new(),
        }
    }

    fn store(&self) -> Arc<dyn ProgressStore> {
        self.store
            .get_or_init(|| match RedbProgressStore::new(&self.db_path) {
                Ok(store) => Arc::new(store),
                Err(error) => {
                    warn!(
                        path = %self.db_path.display(),
                        error = %error,
                        "falling back to in-memory SFTP progress store"
                    );
                    Arc::new(DummyProgressStore)
                }
            })
            .clone()
    }
}

#[async_trait]
impl ProgressStore for LazyProgressStore {
    async fn save(&self, progress: &StoredTransferProgress) -> Result<(), SftpError> {
        self.store().save(progress).await
    }

    async fn load(&self, transfer_id: &str) -> Result<Option<StoredTransferProgress>, SftpError> {
        self.store().load(transfer_id).await
    }

    async fn list_incomplete(
        &self,
        session_id: &str,
    ) -> Result<Vec<StoredTransferProgress>, SftpError> {
        self.store().list_incomplete(session_id).await
    }

    async fn list_all_incomplete(&self) -> Result<Vec<StoredTransferProgress>, SftpError> {
        self.store().list_all_incomplete().await
    }

    async fn delete(&self, transfer_id: &str) -> Result<(), SftpError> {
        self.store().delete(transfer_id).await
    }

    async fn delete_for_session(&self, session_id: &str) -> Result<(), SftpError> {
        self.store().delete_for_session(session_id).await
    }
}

const PROGRESS_TABLE: redb::TableDefinition<&str, &[u8]> =
    redb::TableDefinition::new("sftp_transfer_progress");
const INCOMPLETE_PROGRESS_TABLE: redb::TableDefinition<&str, &[u8]> =
    redb::TableDefinition::new("sftp_transfer_incomplete_progress");
const SESSION_INCOMPLETE_INDEX_TABLE: redb::TableDefinition<&str, &str> =
    redb::TableDefinition::new("sftp_transfer_incomplete_session_index");

pub struct RedbProgressStore {
    db: Arc<redb::Database>,
}

fn session_incomplete_index_key(session_id: &str, transfer_id: &str) -> String {
    format!("{session_id}:{transfer_id}")
}

fn session_incomplete_index_end_key(session_id: &str) -> String {
    format!("{session_id};")
}

impl RedbProgressStore {
    pub fn new(db_path: impl AsRef<Path>) -> Result<Self, SftpError> {
        let db_path = db_path.as_ref();
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                SftpError::StorageError(format!("Failed to create progress directory: {error}"))
            })?;
        }
        info!("Creating SFTP progress store at: {:?}", db_path);
        let db = redb::Database::create(db_path)
            .map(Arc::new)
            .map_err(|error| {
                SftpError::StorageError(format!("Failed to create progress database: {error}"))
            })?;
        let store = Self { db };
        store.ensure_tables()?;
        // LazyProgressStore does not publish this store until construction
        // finishes, so normalization cannot race transfers in this process.
        store.normalize_stale_progress_and_rebuild_indexes()?;
        Ok(store)
    }

    fn ensure_tables(&self) -> Result<(), SftpError> {
        let write_txn = self.db.begin_write().map_err(|error| {
            SftpError::StorageError(format!("Failed to begin write transaction: {error}"))
        })?;
        {
            write_txn.open_table(PROGRESS_TABLE).map_err(|error| {
                SftpError::StorageError(format!("Failed to open progress table: {error}"))
            })?;
            write_txn
                .open_table(INCOMPLETE_PROGRESS_TABLE)
                .map_err(|error| {
                    SftpError::StorageError(format!(
                        "Failed to open incomplete progress table: {error}"
                    ))
                })?;
            write_txn
                .open_table(SESSION_INCOMPLETE_INDEX_TABLE)
                .map_err(|error| {
                    SftpError::StorageError(format!(
                        "Failed to open session incomplete index table: {error}"
                    ))
                })?;
        }
        write_txn.commit().map_err(|error| {
            SftpError::StorageError(format!("Failed to commit transaction: {error}"))
        })
    }

    fn normalize_stale_progress_and_rebuild_indexes(&self) -> Result<(), SftpError> {
        let read_txn = self.db.begin_read().map_err(|error| {
            SftpError::StorageError(format!("Failed to begin read transaction: {error}"))
        })?;
        let table = read_txn.open_table(PROGRESS_TABLE).map_err(|error| {
            SftpError::StorageError(format!("Failed to open progress table: {error}"))
        })?;
        let mut incomplete_entries = Vec::new();
        let mut recovered_active_entries = Vec::new();
        let mut entries_to_delete = Vec::new();
        for item in table.iter().map_err(|error| {
            SftpError::StorageError(format!("Failed to iterate progress table: {error}"))
        })? {
            let (key, value) = item.map_err(|error| {
                SftpError::StorageError(format!("Failed to read progress entry: {error}"))
            })?;
            let transfer_id = key.value().to_string();
            let progress: StoredTransferProgress = match rmp_serde::from_slice(value.value()) {
                Ok(progress) => progress,
                Err(error) => {
                    // Older native builds briefly wrote incompatible progress rows while the
                    // transfer schema was being ported. Tauri rebuilds indexes from valid
                    // resumable transfers; one stale row must not disable the whole store.
                    warn!(
                        transfer_id,
                        error = %error,
                        "dropping unreadable SFTP transfer progress row during index rebuild"
                    );
                    entries_to_delete.push(transfer_id);
                    continue;
                }
            };
            match progress.status {
                TransferStatus::Active => {
                    let mut recovered = progress;
                    recovered.mark_paused();
                    let serialized = rmp_serde::to_vec_named(&recovered).map_err(|error| {
                        SftpError::StorageError(format!(
                            "Failed to serialize recovered progress: {error}"
                        ))
                    })?;
                    recovered_active_entries.push((transfer_id, recovered.session_id, serialized));
                }
                TransferStatus::Paused | TransferStatus::Failed => {
                    incomplete_entries.push((transfer_id, progress.session_id));
                }
                TransferStatus::Completed | TransferStatus::Cancelled => {
                    entries_to_delete.push(transfer_id);
                }
            }
        }
        drop(table);
        drop(read_txn);

        let write_txn = self.db.begin_write().map_err(|error| {
            SftpError::StorageError(format!("Failed to begin write transaction: {error}"))
        })?;
        {
            let mut progress_table = write_txn.open_table(PROGRESS_TABLE).map_err(|error| {
                SftpError::StorageError(format!("Failed to open progress table: {error}"))
            })?;
            let mut incomplete_table =
                write_txn
                    .open_table(INCOMPLETE_PROGRESS_TABLE)
                    .map_err(|error| {
                        SftpError::StorageError(format!(
                            "Failed to open incomplete progress table: {error}"
                        ))
                    })?;
            let mut session_index_table = write_txn
                .open_table(SESSION_INCOMPLETE_INDEX_TABLE)
                .map_err(|error| {
                    SftpError::StorageError(format!(
                        "Failed to open session incomplete index table: {error}"
                    ))
                })?;
            incomplete_table
                .retain_in::<&str, _>(.., |_, _| false)
                .map_err(|error| {
                    SftpError::StorageError(format!(
                        "Failed to clear incomplete progress table: {error}"
                    ))
                })?;
            session_index_table
                .retain_in::<&str, _>(.., |_, _| false)
                .map_err(|error| {
                    SftpError::StorageError(format!(
                        "Failed to clear session incomplete index table: {error}"
                    ))
                })?;
            for transfer_id in entries_to_delete {
                progress_table
                    .remove(transfer_id.as_str())
                    .map_err(|error| {
                        SftpError::StorageError(format!(
                            "Failed to remove stale progress entry: {error}"
                        ))
                    })?;
            }
            for (transfer_id, session_id, serialized) in recovered_active_entries {
                progress_table
                    .insert(transfer_id.as_str(), serialized.as_slice())
                    .map_err(|error| {
                        SftpError::StorageError(format!(
                            "Failed to recover active progress entry: {error}"
                        ))
                    })?;
                incomplete_table
                    .insert(transfer_id.as_str(), serialized.as_slice())
                    .map_err(|error| {
                        SftpError::StorageError(format!(
                            "Failed to index recovered progress entry: {error}"
                        ))
                    })?;
                let session_key = session_incomplete_index_key(&session_id, &transfer_id);
                session_index_table
                    .insert(session_key.as_str(), transfer_id.as_str())
                    .map_err(|error| {
                        SftpError::StorageError(format!(
                            "Failed to index recovered progress session: {error}"
                        ))
                    })?;
            }
            for (transfer_id, session_id) in incomplete_entries {
                if let Some(value) = progress_table.get(transfer_id.as_str()).map_err(|error| {
                    SftpError::StorageError(format!(
                        "Failed to load progress during index rebuild: {error}"
                    ))
                })? {
                    incomplete_table
                        .insert(transfer_id.as_str(), value.value())
                        .map_err(|error| {
                            SftpError::StorageError(format!(
                                "Failed to rebuild incomplete progress table: {error}"
                            ))
                        })?;
                    let session_key = session_incomplete_index_key(&session_id, &transfer_id);
                    session_index_table
                        .insert(session_key.as_str(), transfer_id.as_str())
                        .map_err(|error| {
                            SftpError::StorageError(format!(
                                "Failed to rebuild session incomplete index: {error}"
                            ))
                        })?;
                }
            }
        }
        write_txn.commit().map_err(|error| {
            SftpError::StorageError(format!("Failed to commit transaction: {error}"))
        })
    }

    fn save_sync(db: &redb::Database, progress: &StoredTransferProgress) -> Result<(), SftpError> {
        let transfer_id = progress.transfer_id.as_str();
        let session_index_key = session_incomplete_index_key(&progress.session_id, transfer_id);
        let serialized = rmp_serde::to_vec_named(progress).map_err(|error| {
            SftpError::StorageError(format!("Failed to serialize progress: {error}"))
        })?;
        let write_txn = db.begin_write().map_err(|error| {
            SftpError::StorageError(format!("Failed to begin write transaction: {error}"))
        })?;
        {
            let mut table = write_txn.open_table(PROGRESS_TABLE).map_err(|error| {
                SftpError::StorageError(format!("Failed to open progress table: {error}"))
            })?;
            let mut incomplete_table =
                write_txn
                    .open_table(INCOMPLETE_PROGRESS_TABLE)
                    .map_err(|error| {
                        SftpError::StorageError(format!(
                            "Failed to open incomplete progress table: {error}"
                        ))
                    })?;
            let mut session_index_table = write_txn
                .open_table(SESSION_INCOMPLETE_INDEX_TABLE)
                .map_err(|error| {
                    SftpError::StorageError(format!(
                        "Failed to open session incomplete index table: {error}"
                    ))
                })?;
            let previous_session_index_key = table
                .get(transfer_id)
                .map_err(|error| {
                    SftpError::StorageError(format!("Failed to read previous progress: {error}"))
                })?
                .map(|value| {
                    rmp_serde::from_slice::<StoredTransferProgress>(value.value()).map_err(
                        |error| {
                            SftpError::StorageError(format!(
                                "Failed to deserialize previous progress: {error}"
                            ))
                        },
                    )
                })
                .transpose()?
                .filter(|previous| previous.session_id != progress.session_id)
                .map(|previous| session_incomplete_index_key(&previous.session_id, transfer_id));
            if let Some(previous_session_index_key) = previous_session_index_key {
                // A reconnect moves one logical transfer to a new connection
                // generation; keep exactly one session lookup index for it.
                session_index_table
                    .remove(previous_session_index_key.as_str())
                    .map_err(|error| {
                        SftpError::StorageError(format!(
                            "Failed to remove previous session index: {error}"
                        ))
                    })?;
            }
            table
                .insert(transfer_id, serialized.as_slice())
                .map_err(|error| {
                    SftpError::StorageError(format!("Failed to insert progress: {error}"))
                })?;
            if progress.is_incomplete() {
                incomplete_table
                    .insert(transfer_id, serialized.as_slice())
                    .map_err(|error| {
                        SftpError::StorageError(format!(
                            "Failed to insert incomplete progress: {error}"
                        ))
                    })?;
                session_index_table
                    .insert(session_index_key.as_str(), transfer_id)
                    .map_err(|error| {
                        SftpError::StorageError(format!(
                            "Failed to insert incomplete session index: {error}"
                        ))
                    })?;
            } else {
                incomplete_table.remove(transfer_id).map_err(|error| {
                    SftpError::StorageError(format!(
                        "Failed to remove incomplete progress: {error}"
                    ))
                })?;
                session_index_table
                    .remove(session_index_key.as_str())
                    .map_err(|error| {
                        SftpError::StorageError(format!(
                            "Failed to remove incomplete session index: {error}"
                        ))
                    })?;
            }
        }
        write_txn.commit().map_err(|error| {
            SftpError::StorageError(format!("Failed to commit transaction: {error}"))
        })
    }
}

#[async_trait]
impl ProgressStore for RedbProgressStore {
    async fn save(&self, progress: &StoredTransferProgress) -> Result<(), SftpError> {
        let transfer_id = progress.transfer_id.clone();
        let db = Arc::clone(&self.db);
        let progress = progress.clone();
        // Redb write transactions can wait for another writer and commit synchronously.
        // Keep that work off the shared async runtime used by active SFTP transfers.
        tokio::task::spawn_blocking(move || Self::save_sync(db.as_ref(), &progress))
            .await
            .map_err(|error| {
                SftpError::StorageError(format!("SFTP progress save task failed: {error}"))
            })??;
        debug!("Progress saved successfully for transfer {}", transfer_id);
        Ok(())
    }

    async fn load(&self, transfer_id: &str) -> Result<Option<StoredTransferProgress>, SftpError> {
        let read_txn = self.db.begin_read().map_err(|error| {
            SftpError::StorageError(format!("Failed to begin read transaction: {error}"))
        })?;
        let table = read_txn.open_table(PROGRESS_TABLE).map_err(|error| {
            SftpError::StorageError(format!("Failed to open progress table: {error}"))
        })?;
        let Some(value) = table.get(transfer_id).map_err(|error| {
            SftpError::StorageError(format!("Failed to read progress: {error}"))
        })?
        else {
            return Ok(None);
        };
        let progress = rmp_serde::from_slice(value.value()).map_err(|error| {
            SftpError::StorageError(format!("Failed to deserialize progress: {error}"))
        })?;
        Ok(Some(progress))
    }

    async fn list_incomplete(
        &self,
        session_id: &str,
    ) -> Result<Vec<StoredTransferProgress>, SftpError> {
        let read_txn = self.db.begin_read().map_err(|error| {
            SftpError::StorageError(format!("Failed to begin read transaction: {error}"))
        })?;
        let session_index_table = read_txn
            .open_table(SESSION_INCOMPLETE_INDEX_TABLE)
            .map_err(|error| {
                SftpError::StorageError(format!(
                    "Failed to open session incomplete index table: {error}"
                ))
            })?;
        let incomplete_table = read_txn
            .open_table(INCOMPLETE_PROGRESS_TABLE)
            .map_err(|error| {
                SftpError::StorageError(format!(
                    "Failed to open incomplete progress table: {error}"
                ))
            })?;
        let mut results = Vec::new();
        let start_key = session_incomplete_index_key(session_id, "");
        let end_key = session_incomplete_index_end_key(session_id);
        for item in session_index_table
            .range(start_key.as_str()..end_key.as_str())
            .map_err(|error| {
                SftpError::StorageError(format!(
                    "Failed to iterate incomplete session index: {error}"
                ))
            })?
        {
            let (_key, transfer_id) = item.map_err(|error| {
                SftpError::StorageError(format!("Failed to read session index entry: {error}"))
            })?;
            if let Some(value) = incomplete_table.get(transfer_id.value()).map_err(|error| {
                SftpError::StorageError(format!(
                    "Failed to read indexed incomplete progress: {error}"
                ))
            })? {
                results.push(rmp_serde::from_slice(value.value()).map_err(|error| {
                    SftpError::StorageError(format!(
                        "Failed to deserialize indexed progress: {error}"
                    ))
                })?);
            }
        }
        Ok(results)
    }

    async fn list_all_incomplete(&self) -> Result<Vec<StoredTransferProgress>, SftpError> {
        let read_txn = self.db.begin_read().map_err(|error| {
            SftpError::StorageError(format!("Failed to begin read transaction: {error}"))
        })?;
        let table = read_txn
            .open_table(INCOMPLETE_PROGRESS_TABLE)
            .map_err(|error| {
                SftpError::StorageError(format!(
                    "Failed to open incomplete progress table: {error}"
                ))
            })?;
        let mut results = Vec::new();
        for item in table.iter().map_err(|error| {
            SftpError::StorageError(format!("Failed to iterate progress table: {error}"))
        })? {
            let (_key, value) = item.map_err(|error| {
                SftpError::StorageError(format!(
                    "Failed to read incomplete progress entry: {error}"
                ))
            })?;
            results.push(rmp_serde::from_slice(value.value()).map_err(|error| {
                SftpError::StorageError(format!("Failed to deserialize progress: {error}"))
            })?);
        }
        Ok(results)
    }

    async fn delete(&self, transfer_id: &str) -> Result<(), SftpError> {
        let existing = self.load(transfer_id).await?;
        let write_txn = self.db.begin_write().map_err(|error| {
            SftpError::StorageError(format!("Failed to begin write transaction: {error}"))
        })?;
        {
            let mut table = write_txn.open_table(PROGRESS_TABLE).map_err(|error| {
                SftpError::StorageError(format!("Failed to open progress table: {error}"))
            })?;
            let mut incomplete_table =
                write_txn
                    .open_table(INCOMPLETE_PROGRESS_TABLE)
                    .map_err(|error| {
                        SftpError::StorageError(format!(
                            "Failed to open incomplete progress table: {error}"
                        ))
                    })?;
            let mut session_index_table = write_txn
                .open_table(SESSION_INCOMPLETE_INDEX_TABLE)
                .map_err(|error| {
                    SftpError::StorageError(format!(
                        "Failed to open session incomplete index table: {error}"
                    ))
                })?;
            table.remove(transfer_id).map_err(|error| {
                SftpError::StorageError(format!("Failed to delete progress: {error}"))
            })?;
            incomplete_table.remove(transfer_id).map_err(|error| {
                SftpError::StorageError(format!("Failed to delete incomplete progress: {error}"))
            })?;
            if let Some(progress) = existing.as_ref() {
                let session_key = session_incomplete_index_key(&progress.session_id, transfer_id);
                session_index_table
                    .remove(session_key.as_str())
                    .map_err(|error| {
                        SftpError::StorageError(format!(
                            "Failed to delete session incomplete index entry: {error}"
                        ))
                    })?;
            }
        }
        write_txn.commit().map_err(|error| {
            SftpError::StorageError(format!("Failed to commit transaction: {error}"))
        })
    }

    async fn delete_for_session(&self, session_id: &str) -> Result<(), SftpError> {
        for progress in self.list_incomplete(session_id).await? {
            self.delete(&progress.transfer_id).await?;
        }
        Ok(())
    }
}
