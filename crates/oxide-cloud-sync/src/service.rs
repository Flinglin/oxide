use std::collections::{BTreeMap, HashSet};

use anyhow::{Context, Result, bail};
use chrono::DateTime;
use oxide_connections::{
    ApplySavedConnectionsSyncOutcome, ConnectionStore, ManagedSshKeyInfo, MoshProfilesSyncSnapshot,
    RemoteDesktopProfilesSyncSnapshot, SavedConnectionsConflictStrategy,
    SavedConnectionsSyncSnapshot, SerialProfilesSyncSnapshot, StandaloneSftpProfilesSyncSnapshot,
    TelnetProfilesSyncSnapshot, oxide_file::EncryptedPluginSetting,
};
use oxide_forwarding::{
    ApplySavedForwardsSyncSnapshotResult, ForwardType, ForwardingRegistry,
    SavedForwardsSyncSnapshot,
};
use oxide_settings::{PersistedSettings, SettingsStore, export_oxide_settings_snapshot_json};
use serde::Serialize;
use serde_json::{Value, json};

use crate::{
    LocalSyncMetadata, OXIDE_APP_SETTINGS_SECTION_IDS, RawSyncScope, StructuredDirtyInfo,
    StructuredLocalState, SyncScope, compute_structured_dirty_sections,
    count_structured_upload_plan_units, normalize_sync_scope, plugin_settings,
};

/// Snapshot fields default to an empty local state so callers can specify only
/// the sections relevant to a projection or test fixture.
#[derive(Clone, Debug, Default)]
pub struct CloudSyncLocalSnapshot {
    pub metadata: LocalSyncMetadata,
    pub scope: SyncScope,
    pub dirty: StructuredDirtyInfo,
    pub upload_units: usize,
    pub connections_record_count: usize,
    pub forwards_record_count: usize,
    pub quick_commands_record_count: usize,
    pub serial_profiles_record_count: usize,
    pub telnet_profiles_record_count: usize,
    pub mosh_profiles_record_count: usize,
    pub remote_desktop_profiles_record_count: usize,
    pub sensitive_credentials_record_count: usize,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct CloudSyncApplyOutcome {
    pub connections: Option<ApplySavedConnectionsSyncOutcome>,
    pub forwards: Option<ApplySavedForwardsSyncSnapshotResult>,
    pub quick_commands_applied: usize,
    pub serial_profiles_applied: usize,
    pub telnet_profiles_applied: usize,
    pub mosh_profiles_applied: usize,
    pub remote_desktop_profiles_applied: usize,
    pub app_settings_applied: usize,
    pub plugin_settings_applied: usize,
}

pub fn build_local_snapshot(
    connection_store: &ConnectionStore,
    forwarding_registry: &ForwardingRegistry,
    settings_store: &SettingsStore,
    baseline_state: Option<&StructuredLocalState>,
    raw_scope: Option<&RawSyncScope>,
) -> Result<CloudSyncLocalSnapshot> {
    let available_plugin_ids = plugin_settings::plugin_settings_revision_map(settings_store.path())
        .map_err(anyhow::Error::msg)?
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    let scope = normalize_sync_scope(raw_scope, &available_plugin_ids);

    let connections_snapshot = connection_store.export_saved_connections_snapshot()?;
    let forwards_snapshot = forwarding_registry.export_saved_forwards_snapshot()?;
    let quick_commands_json = oxide_quick_commands::export_snapshot_json(settings_store.path())
        .map_err(anyhow::Error::msg)?;
    let quick_commands_snapshot = oxide_quick_commands::decode_snapshot_json(&quick_commands_json)
        .map_err(anyhow::Error::msg)
        .context("failed to decode quick commands snapshot")?;
    let serial_profiles_snapshot = connection_store.export_serial_profiles_snapshot()?;
    let telnet_profiles_snapshot = connection_store.export_telnet_profiles_snapshot()?;
    let mosh_profiles_snapshot = connection_store.export_mosh_profiles_snapshot()?;
    let standalone_sftp_profiles_snapshot =
        connection_store.export_standalone_sftp_profiles_snapshot()?;
    let remote_desktop_profiles_snapshot =
        connection_store.export_remote_desktop_profiles_snapshot()?;
    let app_settings_section_revisions =
        build_app_settings_section_revision_map(settings_store, &scope)?;
    let plugin_settings_revisions =
        plugin_settings::plugin_settings_revision_map(settings_store.path())
            .map_err(anyhow::Error::msg)?;
    let syncable_settings_payload = build_syncable_settings_payload(settings_store);
    let sensitive_credentials_revision = tauri_simple_stable_hash(&(
        connection_store.profile_credentials_revision()?,
        json!({
            "connectionsRevision": &connections_snapshot.revision,
            "managedKeys": &referenced_managed_key_revision_payload(connection_store, &connections_snapshot),
        }),
    ))?;

    let saved_connections_revision = tauri_simple_stable_hash(&(
        connections_snapshot.revision.as_str(),
        standalone_sftp_profiles_snapshot.revision.as_str(),
    ))?;
    let metadata = LocalSyncMetadata {
        // Standalone SFTP profiles share the Connections cloud-sync section.
        saved_connections_revision: Some(saved_connections_revision),
        saved_forwards_revision: Some(forwards_snapshot.revision.clone()),
        quick_commands_revision: Some(tauri_simple_stable_hash(&quick_commands_json)?),
        serial_profiles_revision: Some(serial_profiles_snapshot.revision.clone()),
        telnet_profiles_revision: Some(telnet_profiles_snapshot.revision.clone()),
        mosh_profiles_revision: Some(mosh_profiles_snapshot.revision.clone()),
        remote_desktop_profiles_revision: Some(remote_desktop_profiles_snapshot.revision.clone()),
        sensitive_credentials_revision: Some(sensitive_credentials_revision),
        settings_revision: Some(tauri_simple_stable_hash(&syncable_settings_payload)?),
        app_settings_section_revisions,
        plugin_settings_revisions,
    };
    let dirty = compute_structured_dirty_sections(&metadata, baseline_state, &scope);
    let upload_units = count_structured_upload_plan_units(&metadata, &scope);

    Ok(CloudSyncLocalSnapshot {
        metadata,
        scope: scope.clone(),
        dirty,
        upload_units,
        connections_record_count: connections_snapshot.record_count(),
        forwards_record_count: forwards_snapshot.records.len(),
        quick_commands_record_count: quick_commands_snapshot.commands.len(),
        serial_profiles_record_count: serial_profiles_snapshot.records.len(),
        telnet_profiles_record_count: telnet_profiles_snapshot.records.len(),
        mosh_profiles_record_count: mosh_profiles_snapshot.records.len(),
        remote_desktop_profiles_record_count: remote_desktop_profiles_snapshot.records.len(),
        sensitive_credentials_record_count: if scope.sync_connections {
            connections_snapshot.record_count()
        } else {
            0
        } + connection_store.profile_credential_count(
            &crate::credentials::credential_selection(
                connection_store,
                &scope,
                &crate::operation::StructuredUploadItemFilter::default(),
            ),
            crate::credentials::global_proxy(settings_store.settings()).as_ref(),
        ),
    })
}

#[allow(dead_code)]
pub fn apply_structured_snapshots(
    connection_store: &mut ConnectionStore,
    forwarding_registry: &ForwardingRegistry,
    settings_store: &mut SettingsStore,
    connections_snapshot: Option<SavedConnectionsSyncSnapshot>,
    forwards_snapshot: Option<SavedForwardsSyncSnapshot>,
    quick_commands_snapshot_json: Option<String>,
    serial_profiles_snapshot: Option<SerialProfilesSyncSnapshot>,
    telnet_profiles_snapshot: Option<TelnetProfilesSyncSnapshot>,
    mosh_profiles_snapshot: Option<MoshProfilesSyncSnapshot>,
    standalone_sftp_profiles_snapshot: Option<StandaloneSftpProfilesSyncSnapshot>,
    remote_desktop_profiles_snapshot: Option<RemoteDesktopProfilesSyncSnapshot>,
    app_settings_snapshots: BTreeMap<String, String>,
    plugin_settings_snapshot: Vec<EncryptedPluginSetting>,
    conflict_strategy: SavedConnectionsConflictStrategy,
) -> Result<CloudSyncApplyOutcome> {
    apply_structured_snapshots_with_credentials(
        connection_store,
        forwarding_registry,
        settings_store,
        connections_snapshot,
        forwards_snapshot,
        quick_commands_snapshot_json,
        serial_profiles_snapshot,
        telnet_profiles_snapshot,
        mosh_profiles_snapshot,
        standalone_sftp_profiles_snapshot,
        remote_desktop_profiles_snapshot,
        app_settings_snapshots,
        plugin_settings_snapshot,
        conflict_strategy,
        None,
    )
}

pub(crate) fn apply_structured_snapshots_with_credentials(
    connection_store: &mut ConnectionStore,
    forwarding_registry: &ForwardingRegistry,
    settings_store: &mut SettingsStore,
    connections_snapshot: Option<SavedConnectionsSyncSnapshot>,
    forwards_snapshot: Option<SavedForwardsSyncSnapshot>,
    quick_commands_snapshot_json: Option<String>,
    serial_profiles_snapshot: Option<SerialProfilesSyncSnapshot>,
    telnet_profiles_snapshot: Option<TelnetProfilesSyncSnapshot>,
    mosh_profiles_snapshot: Option<MoshProfilesSyncSnapshot>,
    standalone_sftp_profiles_snapshot: Option<StandaloneSftpProfilesSyncSnapshot>,
    mut remote_desktop_profiles_snapshot: Option<RemoteDesktopProfilesSyncSnapshot>,
    app_settings_snapshots: BTreeMap<String, String>,
    plugin_settings_snapshot: Vec<EncryptedPluginSetting>,
    conflict_strategy: SavedConnectionsConflictStrategy,
    mut credentials: Option<&mut crate::credentials::ProfileCredentialImport>,
) -> Result<CloudSyncApplyOutcome> {
    let mut audit = oxide_audit::AuditOperation::begin(
        oxide_audit::AuditCategory::Configuration,
        "cloud_sync_apply",
        None,
        Some("structured"),
    );
    audit.summary(&format!(
        "mode=structured,connections={},forwards={},profiles={}",
        connections_snapshot
            .as_ref()
            .map_or(0, |snapshot| snapshot.records.len()),
        forwards_snapshot
            .as_ref()
            .map_or(0, |snapshot| snapshot.records.len()),
        serial_profiles_snapshot
            .as_ref()
            .map_or(0, |snapshot| snapshot.records.len())
            + telnet_profiles_snapshot
                .as_ref()
                .map_or(0, |snapshot| snapshot.records.len())
            + mosh_profiles_snapshot
                .as_ref()
                .map_or(0, |snapshot| snapshot.records.len())
            + standalone_sftp_profiles_snapshot
                .as_ref()
                .map_or(0, |snapshot| snapshot.records.len())
            + remote_desktop_profiles_snapshot
                .as_ref()
                .map_or(0, |snapshot| snapshot.records.len()),
    ));
    let mut rollback_attempted = false;
    let mut rollback_failed = false;
    let mut standalone_sftp_applied = 0usize;
    let mut credentials_applied = 0usize;
    let child_context = oxide_audit::AuditContext::current_request()
        .or_else(oxide_audit::AuditContext::current)
        .map(|mut context| {
            context.parent_id = audit.id().map(str::to_owned);
            context.protocol = Some("cloud_sync_apply".to_string());
            context
        });
    let audit_result = oxide_audit::AuditContext::with_sync_request(child_context.as_ref(), || {
        // Validate every independently supplied resource before the coordinated
        // transaction captures owner checkpoints and performs its first write.
        let mut staged_app_settings = preflight_structured_snapshots(
            settings_store,
            forwards_snapshot.as_ref(),
            quick_commands_snapshot_json.as_deref(),
            serial_profiles_snapshot.as_ref(),
            telnet_profiles_snapshot.as_ref(),
            mosh_profiles_snapshot.as_ref(),
            standalone_sftp_profiles_snapshot.as_ref(),
            remote_desktop_profiles_snapshot.as_ref(),
            &app_settings_snapshots,
            &plugin_settings_snapshot,
        )?;

        if let Some(next) = staged_app_settings.as_mut() {
            crate::credentials::preserve_global_proxy_reference(settings_store.settings(), next);
        }

        // Capture every owner before the first write. The connection checkpoint is
        // always required because profile-only sync still mutates ConnectionStore.
        let settings_path = settings_store.path().to_path_buf();
        let connection_checkpoint = connection_store
            .create_checkpoint()
            .context("failed to checkpoint connection store before cloud sync apply")?;
        let forwards_checkpoint = forwarding_registry
            .checkpoint_saved_forwards()
            .map_err(anyhow::Error::msg)
            .context("failed to checkpoint saved forwards before cloud sync apply")?;
        let quick_commands_checkpoint = oxide_quick_commands::capture_checkpoint(&settings_path)
            .map_err(anyhow::Error::msg)
            .context("failed to checkpoint Quick Commands before cloud sync apply")?;
        let plugin_settings_checkpoint =
            plugin_settings::checkpoint_plugin_settings(&settings_path)
                .map_err(anyhow::Error::msg)
                .context("failed to checkpoint plugin settings before cloud sync apply")?;
        let settings_checkpoint = settings_store
            .create_checkpoint()
            .context("failed to checkpoint app settings before cloud sync apply")?;

        let mut prepared_connections = None;
        let mut prepared_credentials = None;
        let mut forwards_attempted = false;
        let mut quick_commands_attempted = false;
        let mut settings_applied = false;
        let mut plugin_settings_attempted = false;

        let apply_result = (|| -> Result<CloudSyncApplyOutcome> {
            let connections = if let Some(snapshot) = connections_snapshot {
                let prepared = connection_store
                    .prepare_saved_connections_snapshot(snapshot, conflict_strategy)
                    .context("failed to prepare saved connections cloud sync")?;
                let outcome = prepared.outcome().clone();
                prepared_connections = Some(prepared);
                Some(outcome)
            } else {
                None
            };
            if let Some(snapshot) = remote_desktop_profiles_snapshot.as_mut() {
                // Connection preparation has already resolved skip/replace/merge
                // conflicts, so only references in the staged store remain valid.
                crate::operation::selection::retain_available_remote_desktop_gateway_refs(
                    snapshot,
                    connection_store,
                );
            }

            if let Some(outcome) = connections.as_ref() {
                forwards_attempted = true;
                for connection_id in &outcome.deleted_connection_ids {
                    forwarding_registry
                        .delete_owned_forwards(connection_id)
                        .map_err(anyhow::Error::msg)?;
                }
            }

            let valid_owner_connection_ids = connection_store
                .connections()
                .iter()
                .map(|connection| connection.id.clone())
                .collect::<HashSet<_>>();
            let forwards = if let Some(snapshot) = forwards_snapshot {
                forwards_attempted = true;
                Some(
                    forwarding_registry
                        .apply_saved_forwards_snapshot(snapshot, &valid_owner_connection_ids)
                        .map_err(anyhow::Error::msg)?,
                )
            } else {
                None
            };

            let quick_commands_applied = if let Some(snapshot_json) = quick_commands_snapshot_json {
                quick_commands_attempted = true;
                let result = oxide_quick_commands::apply_snapshot_json(
                    &settings_path,
                    &snapshot_json,
                    oxide_quick_commands::QuickCommandImportStrategy::Merge,
                );
                if !result.errors.is_empty() {
                    bail!(
                        "failed to apply quick commands snapshot: {}",
                        result.errors.join("; ")
                    );
                }
                result.imported
            } else {
                0
            };

            let serial_profiles_applied = if let Some(snapshot) = serial_profiles_snapshot {
                connection_store.apply_serial_profiles_snapshot(snapshot)?
            } else {
                0
            };
            let telnet_profiles_applied = if let Some(snapshot) = telnet_profiles_snapshot {
                connection_store.apply_telnet_profiles_snapshot(snapshot)?
            } else {
                0
            };
            let mosh_profiles_applied = if let Some(snapshot) = mosh_profiles_snapshot {
                connection_store.apply_mosh_profiles_snapshot(snapshot)?
            } else {
                0
            };
            if let Some(snapshot) = standalone_sftp_profiles_snapshot {
                standalone_sftp_applied =
                    connection_store.apply_standalone_sftp_profiles_snapshot(snapshot)?;
            }
            let remote_desktop_profiles_applied =
                if let Some(snapshot) = remote_desktop_profiles_snapshot {
                    connection_store.apply_remote_desktop_profiles_snapshot(snapshot)?
                } else {
                    0
                };

            let app_settings_applied = app_settings_snapshots.len();
            if let Some(next) = staged_app_settings {
                // SettingsStore changes memory only after its durable swap succeeds.
                settings_store.replace_and_save(next)?;
                settings_applied = true;
            }

            plugin_settings_attempted = true;
            let plugin_settings_applied =
                plugin_settings::upsert_plugin_settings(&settings_path, &plugin_settings_snapshot)
                    .map_err(anyhow::Error::msg)?;

            if let Some(batch) = credentials.as_mut() {
                let mut proxy = crate::credentials::global_proxy(settings_store.settings());
                let prepared = connection_store.prepare_profile_credentials(
                    &batch.secrets,
                    &batch.selection,
                    &mut proxy,
                )?;
                batch.summary = prepared.summary;
                credentials_applied = batch.summary.restored + batch.summary.cleared;
                prepared_credentials = Some(prepared);
                let mut next = settings_store.settings().clone();
                crate::credentials::apply_global_proxy_reference(&mut next, proxy.as_ref());
                if &next != settings_store.settings() {
                    settings_store.replace_and_save(next)?;
                    settings_applied = true;
                }
                connection_store.save()?;
            }

            Ok(CloudSyncApplyOutcome {
                connections,
                forwards,
                quick_commands_applied,
                serial_profiles_applied,
                telnet_profiles_applied,
                mosh_profiles_applied,
                remote_desktop_profiles_applied,
                app_settings_applied,
                plugin_settings_applied,
            })
        })();

        let outcome = match apply_result {
            Ok(outcome) => outcome,
            Err(error) => {
                rollback_attempted = true;
                let rollback_errors = rollback_structured_apply(
                    connection_store,
                    forwarding_registry,
                    settings_store,
                    &settings_path,
                    &connection_checkpoint,
                    forwards_checkpoint.as_ref(),
                    &quick_commands_checkpoint,
                    &plugin_settings_checkpoint,
                    &settings_checkpoint,
                    forwards_attempted,
                    quick_commands_attempted,
                    settings_applied,
                    plugin_settings_attempted,
                );
                let mut rollback_errors = rollback_errors;
                if let Some(prepared) = prepared_credentials.take() {
                    if let Err(cleanup) = connection_store.rollback_profile_credentials(prepared) {
                        rollback_errors.push(cleanup.to_string());
                    }
                }
                rollback_failed = !rollback_errors.is_empty();
                return Err(cloud_sync_transaction_error(error, rollback_errors));
            }
        };

        if let Some(prepared) = prepared_connections {
            let mut cleanup =
                match connection_store.commit_prepared_saved_connections_snapshot(prepared) {
                    Ok(cleanup) => cleanup,
                    Err(error) => {
                        rollback_attempted = true;
                        let rollback_errors = rollback_structured_apply(
                            connection_store,
                            forwarding_registry,
                            settings_store,
                            &settings_path,
                            &connection_checkpoint,
                            forwards_checkpoint.as_ref(),
                            &quick_commands_checkpoint,
                            &plugin_settings_checkpoint,
                            &settings_checkpoint,
                            forwards_attempted,
                            quick_commands_attempted,
                            settings_applied,
                            plugin_settings_attempted,
                        );
                        let mut rollback_errors = rollback_errors;
                        if let Some(prepared) = prepared_credentials.take() {
                            if let Err(cleanup) =
                                connection_store.rollback_profile_credentials(prepared)
                            {
                                rollback_errors.push(cleanup.to_string());
                            }
                        }
                        rollback_failed = !rollback_errors.is_empty();
                        return Err(cloud_sync_transaction_error(
                            error.context("failed to commit prepared saved connections cloud sync"),
                            rollback_errors,
                        ));
                    }
                };

            // Cleanup is intentionally outside the rollback boundary: all data is
            // committed, and stale credentials are harmless if deletion fails.
            if connection_store
                .finalize_saved_connections_sync_cleanup(&mut cleanup)
                .is_err()
                && connection_store
                    .finalize_saved_connections_sync_cleanup(&mut cleanup)
                    .is_err()
            {
                // The synchronized data is already committed, so housekeeping must
                // not make the operation look failed and trigger a duplicate apply.
                // A future cleanup queue should persist this retry state across runs.
                eprintln!(
                    "warning: cloud sync committed, but {} stale keychain entries remain after cleanup retry",
                    cleanup.pending_keychain_entries()
                );
            }
        }

        if let Some(mut prepared) = prepared_credentials {
            if connection_store
                .commit_profile_credentials(&mut prepared)
                .is_err()
                && connection_store
                    .commit_profile_credentials(&mut prepared)
                    .is_err()
            {
                // Metadata is committed; cleanup can retry without replaying the import.
                eprintln!(
                    "warning: cloud sync committed, but {} credential slots await cleanup",
                    prepared.pending_cleanup_count()
                );
            }
        }
        Ok(outcome)
    });
    if let Ok(outcome) = &audit_result {
        audit.summary(&format!(
            "mode=structured,connections_applied={},forwards_applied={},profiles_applied={},settings_applied={},plugins_applied={},credentials_applied={}",
            outcome.connections.as_ref().map_or(0, |connections| connections.result.applied),
            outcome.forwards.as_ref().map_or(0, |forwards| forwards.applied),
            outcome.serial_profiles_applied + outcome.telnet_profiles_applied
                + outcome.mosh_profiles_applied + standalone_sftp_applied
                + outcome.remote_desktop_profiles_applied,
            outcome.app_settings_applied,
            outcome.plugin_settings_applied,
            credentials_applied,
        ));
    } else {
        audit.summary(match (rollback_attempted, rollback_failed) {
            (true, true) => "mode=structured,rollback=failed",
            (true, false) => "mode=structured,rollback=complete",
            (false, _) => "mode=structured,phase=preflight",
        });
    }
    audit.finish(
        match &audit_result {
            Ok(outcome)
                if outcome
                    .connections
                    .as_ref()
                    .map_or(0, |connections| connections.result.applied)
                    + outcome
                        .forwards
                        .as_ref()
                        .map_or(0, |forwards| forwards.applied)
                    + outcome.quick_commands_applied
                    + outcome.serial_profiles_applied
                    + outcome.telnet_profiles_applied
                    + outcome.mosh_profiles_applied
                    + standalone_sftp_applied
                    + outcome.remote_desktop_profiles_applied
                    + outcome.app_settings_applied
                    + outcome.plugin_settings_applied
                    + credentials_applied
                    == 0 =>
            {
                oxide_audit::AuditOutcome::Unchanged
            }
            Ok(_) => oxide_audit::AuditOutcome::Succeeded,
            Err(_) if rollback_failed => oxide_audit::AuditOutcome::Partial,
            Err(_) => oxide_audit::AuditOutcome::Failed,
        },
        oxide_audit::AuditEvidence::Protocol,
        None,
        None,
    );
    audit_result
}

#[allow(clippy::too_many_arguments)]
fn rollback_structured_apply(
    connection_store: &mut ConnectionStore,
    forwarding_registry: &ForwardingRegistry,
    settings_store: &mut SettingsStore,
    settings_path: &std::path::Path,
    connection_checkpoint: &oxide_connections::ConnectionStoreCheckpoint,
    forwards_checkpoint: Option<&oxide_forwarding::SavedForwardCheckpoint>,
    quick_commands_checkpoint: &oxide_quick_commands::QuickCommandsCheckpoint,
    plugin_settings_checkpoint: &plugin_settings::PluginSettingsCheckpoint,
    settings_checkpoint: &oxide_settings::SettingsStoreCheckpoint,
    forwards_attempted: bool,
    quick_commands_attempted: bool,
    settings_applied: bool,
    plugin_settings_attempted: bool,
) -> Vec<String> {
    let mut errors = Vec::new();

    if plugin_settings_attempted
        && let Err(error) =
            plugin_settings::restore_plugin_settings(settings_path, plugin_settings_checkpoint)
    {
        errors.push(format!("plugin settings restore failed: {error}"));
    }
    if settings_applied && let Err(error) = settings_store.restore_checkpoint(settings_checkpoint) {
        errors.push(format!("app settings restore failed: {error}"));
    }
    if quick_commands_attempted
        && let Err(error) =
            oxide_quick_commands::restore_checkpoint(settings_path, quick_commands_checkpoint)
    {
        errors.push(format!("Quick Commands restore failed: {error}"));
    }
    if forwards_attempted
        && let Some(checkpoint) = forwards_checkpoint
        && let Err(error) = forwarding_registry.restore_saved_forwards(checkpoint)
    {
        errors.push(format!("saved forwards restore failed: {error}"));
    }
    if let Err(error) = connection_store.restore_checkpoint(connection_checkpoint) {
        errors.push(format!("connection store restore failed: {error}"));
    }

    errors
}

fn cloud_sync_transaction_error(
    error: anyhow::Error,
    rollback_errors: Vec<String>,
) -> anyhow::Error {
    if rollback_errors.is_empty() {
        error.context("cloud sync apply failed; all modified stores were restored")
    } else {
        anyhow::anyhow!(
            "cloud sync apply failed: {error:#}; rollback also failed: {}",
            rollback_errors.join("; ")
        )
    }
}

fn preflight_structured_snapshots(
    settings_store: &SettingsStore,
    forwards_snapshot: Option<&SavedForwardsSyncSnapshot>,
    quick_commands_snapshot_json: Option<&str>,
    serial_profiles_snapshot: Option<&SerialProfilesSyncSnapshot>,
    telnet_profiles_snapshot: Option<&TelnetProfilesSyncSnapshot>,
    mosh_profiles_snapshot: Option<&MoshProfilesSyncSnapshot>,
    standalone_sftp_profiles_snapshot: Option<&StandaloneSftpProfilesSyncSnapshot>,
    remote_desktop_profiles_snapshot: Option<&RemoteDesktopProfilesSyncSnapshot>,
    app_settings_snapshots: &BTreeMap<String, String>,
    plugin_settings_snapshot: &[EncryptedPluginSetting],
) -> Result<Option<PersistedSettings>> {
    if let Some(snapshot) = forwards_snapshot {
        validate_forwards_snapshot(snapshot)?;
    }
    if let Some(snapshot_json) = quick_commands_snapshot_json {
        oxide_quick_commands::decode_snapshot_json(snapshot_json)
            .map_err(anyhow::Error::msg)
            .context("failed to decode quick commands snapshot")?;
    }
    for profile in serial_profiles_snapshot
        .into_iter()
        .flat_map(|snapshot| &snapshot.records)
    {
        profile.validate()?;
    }
    for profile in telnet_profiles_snapshot
        .into_iter()
        .flat_map(|snapshot| &snapshot.records)
    {
        profile.validate()?;
    }
    for profile in mosh_profiles_snapshot
        .into_iter()
        .flat_map(|snapshot| &snapshot.records)
    {
        profile.validate()?;
    }
    for profile in standalone_sftp_profiles_snapshot
        .into_iter()
        .flat_map(|snapshot| &snapshot.records)
    {
        profile.validate()?;
    }
    for profile in standalone_sftp_profiles_snapshot
        .and_then(|s| s.ftp.as_ref())
        .into_iter()
        .flat_map(|s| &s.records)
    {
        profile.validate()?;
    }
    for profile in remote_desktop_profiles_snapshot
        .into_iter()
        .flat_map(|snapshot| &snapshot.records)
    {
        profile.validate()?;
    }

    let mut staged_settings = settings_store.settings().clone();
    for (section_id, snapshot_json) in app_settings_snapshots {
        let selected = HashSet::from([section_id.clone()]);
        staged_settings = oxide_settings::merge_oxide_settings_snapshot(
            &staged_settings,
            snapshot_json,
            Some(&selected),
        )?;
    }

    if !plugin_settings_snapshot.is_empty() {
        // Reading the current plugin file before any mutation prevents a late
        // parse failure from following successful connection/profile writes.
        plugin_settings::load_plugin_settings(settings_store.path()).map_err(anyhow::Error::msg)?;
    }

    Ok((!app_settings_snapshots.is_empty()).then_some(staged_settings))
}

fn validate_forwards_snapshot(snapshot: &SavedForwardsSyncSnapshot) -> Result<()> {
    for record in &snapshot.records {
        DateTime::parse_from_rfc3339(&record.updated_at)
            .with_context(|| format!("invalid saved forward updated_at '{}'", record.updated_at))?;
        if record.deleted {
            continue;
        }
        let Some(payload) = record.payload.as_ref() else {
            continue;
        };
        DateTime::parse_from_rfc3339(&payload.created_at).with_context(|| {
            format!("invalid saved forward created_at '{}'", payload.created_at)
        })?;
        ForwardType::try_from_tauri_str(&payload.forward_type).map_err(anyhow::Error::msg)?;
    }
    Ok(())
}

fn build_app_settings_section_revision_map(
    settings_store: &SettingsStore,
    scope: &SyncScope,
) -> Result<BTreeMap<String, String>> {
    let mut revisions = BTreeMap::new();
    for section_id in OXIDE_APP_SETTINGS_SECTION_IDS {
        let section_id = (*section_id).to_string();
        let selected = HashSet::from([section_id.clone()]);
        let snapshot = export_oxide_settings_snapshot_json(
            settings_store.settings(),
            Some(&selected),
            scope.include_local_terminal_env_vars,
        )
        .with_context(|| format!("failed to export app settings section {section_id}"))?;
        revisions.insert(section_id, app_settings_snapshot_revision(&snapshot)?);
    }
    Ok(revisions)
}

fn app_settings_snapshot_revision(snapshot: &str) -> Result<String> {
    let mut value: Value = serde_json::from_str(snapshot)
        .context("failed to parse app settings snapshot for revision")?;
    if let Some(envelope) = value.as_object_mut() {
        // Export time describes the artifact, not the settings content, so it must not make a clean section dirty.
        envelope.remove("exportedAt");
    }
    tauri_simple_stable_hash(&value)
}

fn build_syncable_settings_payload(settings_store: &SettingsStore) -> Value {
    let settings = settings_store.settings();
    json!({
        "appearance": {
            "language": settings.general.language,
            "uiDensity": settings.appearance.ui_density,
        },
        "terminal": {
            "fontSize": settings.terminal.font_size,
            "theme": settings.terminal.theme,
        },
        "reconnect": {
            "autoReconnect": settings.reconnect.enabled,
        },
    })
}

fn referenced_managed_key_revision_payload(
    connection_store: &ConnectionStore,
    connections_snapshot: &SavedConnectionsSyncSnapshot,
) -> Vec<Value> {
    let referenced_ids = connections_snapshot
        .records
        .iter()
        .filter_map(|record| record.payload.as_ref())
        .flat_map(|payload| {
            std::iter::once(payload.managed_key_id.as_ref()).chain(
                payload
                    .proxy_chain
                    .iter()
                    .map(|hop| hop.managed_key_id.as_ref()),
            )
        })
        .flatten()
        .cloned()
        .collect::<HashSet<_>>();
    let mut managed_keys = connection_store
        .managed_ssh_keys()
        .into_iter()
        .filter(|key| referenced_ids.contains(&key.id))
        .collect::<Vec<_>>();
    managed_keys.sort_by(|left, right| left.id.cmp(&right.id));
    managed_keys
        .into_iter()
        .map(managed_key_revision_payload)
        .collect()
}

fn managed_key_revision_payload(key: ManagedSshKeyInfo) -> Value {
    json!({
        "id": key.id,
        "name": key.name,
        "fingerprint": key.fingerprint,
        "publicKey": key.public_key,
        "requiresPassphrase": key.requires_passphrase,
        "origin": key.origin,
        "updatedAt": key.updated_at,
    })
}

fn tauri_simple_stable_hash<T: Serialize>(value: &T) -> Result<String> {
    let text = serde_json::to_string(value).context("failed to serialize stable hash value")?;
    Ok(tauri_fnv1a_stable_hash_text(&text))
}

fn tauri_fnv1a_stable_hash_text(text: &str) -> String {
    let mut hash = 2166136261u32;
    for code_unit in text.encode_utf16() {
        hash ^= u32::from(code_unit);
        hash = hash.wrapping_mul(16777619);
    }
    format!("fnv1a-{hash:x}")
}
