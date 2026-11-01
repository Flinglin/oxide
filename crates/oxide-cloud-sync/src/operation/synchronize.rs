// Copyright (C) 2026 AnalyseDeCircuit
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::backend::sha256_hex;
use crate::secrets::CloudSyncSecrets;
use crate::sync_v3::{
    ConfigurationView, LocalReplica, PublicationId, PublishedReplica, RecoveryJournal,
    ReplicaStore, SyncConflict, SyncReplica, SyncResource, SyncValues,
};
use std::collections::HashSet;
mod password_change;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SyncDirection {
    #[default]
    Merge,
    Download,
    Upload,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncOutcome {
    #[serde(skip)]
    pub switched_settings: Option<CloudSyncSettings>,
    pub applied: bool,
    pub published: bool,
    pub downloaded: bool,
    pub publication_pending: bool,
    pub recovered: bool,
    pub cleanup_pending: bool,
    pub publication: Option<PublicationId>,
    pub created_remote_id: Option<String>,
    #[serde(skip)]
    pub local_snapshot: CloudSyncLocalSnapshot,
    pub conflicts: Vec<SyncConflict>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SyncPlanSummary {
    pub changed_fields: usize,
    pub conflicts: Vec<SyncConflict>,
    pub upgrading: bool,
    pub direction: SyncDirection,
    pub first_sync: bool,
    pub has_remote: bool,
}

#[derive(Clone)]
pub struct SyncConflictPreview {
    pub name: Option<String>,
    pub candidates: Vec<Option<zeroize::Zeroizing<String>>>,
    pub current: Option<usize>,
}

/// Preparation holds the replica lock without publishing remote data. Callers
/// recover and apply changes on the live configuration owner.
pub struct PreparedSync {
    _permit: CloudSyncOperationPermit,
    store: ReplicaStore,
    local: LocalReplica,
    remote: Vec<PublishedReplica>,
    observed: Vec<PublicationId>,
    upgrade: Option<crate::sync_v3::ReplicaSnapshot>,
    backend: CloudSyncBackend,
    settings: CloudSyncSettings,
    secrets: CloudSyncSecrets,
    scope: crate::SyncScope,
    filter: StructuredUploadItemFilter,
    recovered: bool,
    resolutions: Vec<(SyncConflict, usize)>,
    preview_values: SyncValues,
    direction: SyncDirection,
}

impl std::fmt::Debug for PreparedSync {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedSync").finish_non_exhaustive()
    }
}

pub struct AppliedSync {
    _permit: CloudSyncOperationPermit,
    store: ReplicaStore,
    local: LocalReplica,
    backend: CloudSyncBackend,
    settings: CloudSyncSettings,
    secrets: CloudSyncSecrets,
    outcome: SyncOutcome,
    direction: SyncDirection,
}

impl CloudSyncOperationService {
    pub fn has_local_replica(
        settings_path: &std::path::Path,
        settings: &CloudSyncSettings,
    ) -> Result<bool> {
        Ok(replica_directory(settings_path, settings)?
            .join("replica.oxide")
            .try_exists()?)
    }
    pub async fn prepare_sync(
        &self,
        connections: &mut ConnectionStore,
        forwards: &ForwardingRegistry,
        settings_store: &mut SettingsStore,
        settings: &CloudSyncSettings,
        provider: &mut impl CloudSyncSecretProvider,
        scope: crate::SyncScope,
        filter: StructuredUploadItemFilter,
    ) -> Result<PreparedSync> {
        self.prepare_sync_with_direction(
            connections,
            forwards,
            settings_store,
            settings,
            provider,
            scope,
            filter,
            SyncDirection::Merge,
        )
        .await
    }

    pub async fn prepare_sync_with_direction(
        &self,
        connections: &mut ConnectionStore,
        forwards: &ForwardingRegistry,
        settings_store: &mut SettingsStore,
        settings: &CloudSyncSettings,
        provider: &mut impl CloudSyncSecretProvider,
        scope: crate::SyncScope,
        filter: StructuredUploadItemFilter,
        direction: SyncDirection,
    ) -> Result<PreparedSync> {
        if settings.local_file_mode {
            bail!(
                "Cloud synchronization requires a remote backend; use .oxide import/export in local-file mode"
            );
        }
        let permit = self
            .guard
            .begin(CloudSyncOperationKind::Upload, false)?
            .ok_or_else(|| anyhow::anyhow!("Cloud sync is already running"))?;
        let store = open_replica(settings_store.path(), settings, provider)?;
        if RecoveryJournal::has_pending(settings_store.path())? {
            bail!(
                "Cloud sync recovery must complete on the configuration owner before preparing a new operation"
            );
        }
        let recovered = false;
        let mut local = store.load()?.unwrap_or_default();
        let mut secrets = get_action_secrets(settings, provider, true, SecretReadMode::Prompt)?;
        self.prepare_action_secrets(settings, provider, &mut secrets)
            .await?;
        let password =
            required_sync_password(secrets.sync_password.as_deref().map(String::as_str))?;
        store.bind_sync_password(&mut local, password)?;
        let mut settings = settings.clone();
        if settings.backend_type == BackendType::GithubGist
            && settings.git_repository.trim().is_empty()
        {
            if let Some(id) = &local.created_gist_id {
                settings.git_repository = id.clone();
            }
        }
        let view = collect(connections, forwards, settings_store, &scope, &filter)?;
        // Keep preview edits in this plan; cancelling must not advance the saved baseline.
        let mut replica = SyncReplica::load(local.snapshot.clone(), local.writer)?;
        let selected = view.selected(replica.resources()?.into_iter(), &scope, &filter);
        replica.capture_local(
            &capture_baseline(&local.effective, &scope),
            &view.values,
            &selected,
        )?;
        local.snapshot = replica.snapshot();
        update_effective(&mut local.effective, &view.values, &selected, &scope);

        let new_gist = settings.backend_type == BackendType::GithubGist
            && settings.git_repository.trim().is_empty();
        let publications = if new_gist {
            Vec::new()
        } else {
            self.backend.list_publications(&settings, &secrets).await?
        };
        let upgrade = if !new_gist
            && publications.is_empty()
            && (!local.legacy_imported || direction == SyncDirection::Download)
        {
            let metadata = self
                .backend
                .fetch_remote_metadata(&settings, &secrets)
                .await?;
            if metadata.exists {
                Some(
                    self.read_upgrade_snapshot(connections, &settings, &secrets, metadata, &scope)
                        .await?,
                )
            } else {
                None
            }
        } else {
            None
        };
        let latest = latest_publications(publications)?;
        if direction != SyncDirection::Download
            && local.observed.contains_key(&local.writer)
            && !latest.iter().any(|id| id.writer == local.writer)
        {
            local.request_publication();
            store.save(&local)?;
        }
        let password =
            required_sync_password(secrets.sync_password.as_deref().map(String::as_str))?;
        let mut decryption = OxideBatchDecryptionContext::new(password)?;
        let mut remote = Vec::new();
        let mut observed = Vec::new();
        for id in latest {
            if let Some(previous) = local.observed.get(&id.writer) {
                if previous == &id && direction == SyncDirection::Merge {
                    continue;
                }
                if previous != &id && previous.sequence >= id.sequence {
                    bail!("Cloud sync publication history moved backwards or changed identity");
                }
            }
            if id.writer == local.writer && id.sequence >= local.next_sequence {
                bail!(
                    "Cloud sync writer identity is already used by another copy of this data directory"
                );
            }
            let object = self
                .backend
                .read_remote_object(&settings, &secrets, &id.path())
                .await?
                .context("Published cloud sync snapshot is missing")?;
            remote.push(PublishedReplica::decode(
                &id,
                &object.bytes,
                &mut decryption,
            )?);
            observed.push(id);
        }
        Ok(PreparedSync {
            _permit: permit,
            store,
            local,
            remote,
            upgrade,
            observed,
            backend: self.backend.clone(),
            settings,
            secrets,
            scope,
            filter,
            recovered,
            resolutions: Vec::new(),
            preview_values: view.values,
            direction,
        })
    }

    pub async fn synchronize(
        &self,
        connections: &mut ConnectionStore,
        forwards: &ForwardingRegistry,
        settings_store: &mut SettingsStore,
        settings: &CloudSyncSettings,
        provider: &mut impl CloudSyncSecretProvider,
        scope: crate::SyncScope,
        filter: StructuredUploadItemFilter,
    ) -> Result<SyncOutcome> {
        self.prepare_sync(
            connections,
            forwards,
            settings_store,
            settings,
            provider,
            scope,
            filter,
        )
        .await?
        .apply(connections, forwards, settings_store)?
        .publish()
        .await
    }
}

impl PreparedSync {
    pub fn set_direction(&mut self, direction: SyncDirection) {
        self.direction = direction;
        self.resolutions.clear();
    }

    pub fn first_sync(&self) -> bool {
        self.local.observed.is_empty()
            && self.local.next_sequence == 1
            && !self.local.legacy_imported
    }

    pub fn has_remote(&self) -> bool {
        !self.remote.is_empty() || self.upgrade.is_some()
    }

    fn remote_replica(&self) -> Result<SyncReplica> {
        let mut replica = SyncReplica::new(self.local.writer);
        for remote in &self.remote {
            replica.merge(remote.snapshot.clone())?;
        }
        if let Some(upgrade) = &self.upgrade {
            replica.merge(upgrade.clone())?;
        }
        Ok(replica)
    }

    fn planned_replica(&self, values: &SyncValues) -> Result<SyncReplica> {
        let mut remote = self.remote_replica()?;
        if self.direction == SyncDirection::Download {
            if self.remote.is_empty() && self.upgrade.is_none() {
                bail!("sync_remote_empty");
            }
            for (conflict, index) in &self.resolutions {
                let choice = remote
                    .candidate_value(&conflict.candidates[*index])
                    .cloned();
                remote.resolve(conflict, choice.as_ref())?;
            }
            return Ok(remote);
        }
        let mut replica = SyncReplica::load(self.local.snapshot.clone(), self.local.writer)?;
        let mut view = ConfigurationView::empty();
        view.values = values.clone();
        let selected = view.selected(
            replica.resources()?.into_iter().chain(remote.resources()?),
            &self.scope,
            &self.filter,
        );
        if self.direction == SyncDirection::Merge {
            replica.capture_local(
                &capture_baseline(&self.local.effective, &self.scope),
                values,
                &selected,
            )?;
        }
        replica.merge(remote.snapshot())?;
        if self.direction == SyncDirection::Merge {
            for (conflict, index) in &self.resolutions {
                let choice = replica
                    .candidate_value(&conflict.candidates[*index])
                    .cloned();
                replica.resolve(conflict, choice.as_ref())?;
            }
        } else {
            let fields = replica
                .fields()?
                .into_iter()
                .chain(values.keys().cloned())
                .filter(|field| {
                    selected.contains(&field.resource) && field_enabled(field, &self.scope)
                })
                .collect();
            replica.replace_fields(values, &fields)?;
        }
        Ok(replica)
    }

    pub fn matches_settings(&self, settings: &CloudSyncSettings) -> bool {
        let mut expected = settings.clone();
        if expected.backend_type == BackendType::GithubGist
            && expected.git_repository.trim().is_empty()
        {
            if let Some(id) = &self.local.created_gist_id {
                expected.git_repository = id.clone();
            }
        }
        self.settings == expected
    }
    pub fn conflict_previews(&self) -> Result<Vec<SyncConflictPreview>> {
        use crate::sync_v3::{ResourceKind, SyncField};
        let replica = self.planned_replica(&self.preview_values)?;
        self.summary()?
            .conflicts
            .into_iter()
            .map(|conflict| {
                let name = self
                    .local
                    .effective
                    .get(&SyncField {
                        resource: conflict.field.resource.clone(),
                        group: "name".into(),
                    })
                    .and_then(|value| value.decode::<String>().ok());
                let current = conflict.candidates.iter().position(|candidate| {
                    replica.candidate_value(candidate) == self.preview_values.get(&conflict.field)
                });
                let sensitive = matches!(
                    conflict.field.resource.kind,
                    ResourceKind::Credential
                        | ResourceKind::ManagedKey
                        | ResourceKind::PluginCredential
                        | ResourceKind::PrivilegeCredential
                        | ResourceKind::PluginSettings
                ) || conflict.field.group.contains("customEnvVars");
                let candidates = conflict
                    .candidates
                    .iter()
                    .map(|candidate| {
                        if sensitive {
                            return Ok(None);
                        }
                        let Some(value) = replica.candidate_value(candidate) else {
                            return Ok(Some(zeroize::Zeroizing::new(String::new())));
                        };
                        let owned = crate::sync_v3::PrivateJson(value.decode()?);
                        let value = &owned.0;
                        let text = if let Some(value) = value.as_str() {
                            value.to_owned()
                        } else if let Some(object) = value.as_object() {
                            [
                                "host",
                                "username",
                                "port",
                                "port_path",
                                "baud_rate",
                                "cwd",
                                "shell_id",
                                "body",
                            ]
                            .iter()
                            .filter_map(|key| object.get(*key))
                            .map(|value| {
                                value
                                    .as_str()
                                    .map(str::to_owned)
                                    .unwrap_or_else(|| value.to_string())
                            })
                            .collect::<Vec<_>>()
                            .join(" · ")
                        } else {
                            value.to_string()
                        };
                        let text = zeroize::Zeroizing::new(text);
                        Ok(Some(zeroize::Zeroizing::new(
                            text.chars().take(400).collect(),
                        )))
                    })
                    .collect::<Result<Vec<_>>>()?;
                Ok(SyncConflictPreview {
                    name,
                    candidates,
                    current,
                })
            })
            .collect()
    }
    pub fn summary(&self) -> Result<SyncPlanSummary> {
        let replica = self.planned_replica(&self.preview_values)?;
        let resources = replica.resources()?;
        let mut view = ConfigurationView::empty();
        view.values = replica.materialize(&self.preview_values, &resources)?;
        let selected = view.selected(resources.into_iter(), &self.scope, &self.filter);
        let conflicts = replica
            .conflicts()?
            .into_iter()
            .filter(|conflict| {
                selected.contains(&conflict.field.resource)
                    && field_enabled(&conflict.field, &self.scope)
            })
            .collect();
        let effective = replica.materialize(&self.preview_values, &selected)?;
        let before = if self.direction == SyncDirection::Upload {
            self.remote_replica()?
                .materialize(&SyncValues::new(), &selected)?
        } else {
            self.preview_values.clone()
        };
        let changed_fields = effective
            .keys()
            .chain(before.keys())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .filter(|field| {
                selected.contains(&field.resource)
                    && field_enabled(field, &self.scope)
                    && effective.get(*field) != before.get(*field)
            })
            .count();
        Ok(SyncPlanSummary {
            changed_fields,
            conflicts,
            upgrading: self.upgrade.is_some(),
            direction: self.direction,
            first_sync: self.first_sync(),
            has_remote: self.has_remote(),
        })
    }

    pub fn choose_candidate(&mut self, conflict: SyncConflict, index: usize) -> Result<()> {
        if index >= conflict.candidates.len() {
            bail!("Invalid conflict candidate");
        }
        self.resolutions
            .retain(|(previous, _)| previous.field != conflict.field);
        self.resolutions.push((conflict, index));
        Ok(())
    }

    pub fn choose_local_conflicts(&mut self) -> Result<()> {
        let mut replica = SyncReplica::load(self.local.snapshot.clone(), self.local.writer)?;
        for remote in &self.remote {
            replica.merge(remote.snapshot.clone())?;
        }
        if let Some(upgrade) = &self.upgrade {
            replica.merge(upgrade.clone())?;
        }
        for conflict in self.summary()?.conflicts {
            let local = self.local.effective.get(&conflict.field);
            if let Some(index) = conflict
                .candidates
                .iter()
                .position(|candidate| replica.candidate_value(candidate) == local)
            {
                self.choose_candidate(conflict, index)?;
            }
        }
        Ok(())
    }
    pub fn apply(
        self,
        connections: &mut ConnectionStore,
        forwards: &ForwardingRegistry,
        settings_store: &mut SettingsStore,
    ) -> Result<AppliedSync> {
        // Re-read values from the live owner after network work, before observing
        // remote history. These edits must remain concurrent with remote edits.
        let current = collect(
            connections,
            forwards,
            settings_store,
            &self.scope,
            &self.filter,
        )?;

        let upgraded = self.upgrade.is_some();
        let first_sync = self.first_sync();
        let mut replica = self.planned_replica(&current.values)?;
        let resources = replica.resources()?;
        let merged = replica.materialize(&current.values, &resources)?;
        let selected =
            current.select_merged(merged, resources.into_iter(), &self.scope, &self.filter);
        let mut desired = replica.materialize(&current.values, &selected)?;
        desired.retain(|field, _| field_enabled(field, &self.scope));
        let changed = desired != current.values;
        let conflicts = replica
            .conflicts()?
            .into_iter()
            .filter(|conflict| {
                selected.contains(&conflict.field.resource)
                    && field_enabled(&conflict.field, &self.scope)
            })
            .collect::<Vec<_>>();
        if self.direction == SyncDirection::Download && !conflicts.is_empty() {
            bail!("sync_download_conflicts");
        }
        if self.direction == SyncDirection::Download && !self.first_sync() {
            // Preserve causal history outside the downloaded scope, including pending local edits.
            let mut combined = SyncReplica::load(self.local.snapshot.clone(), self.local.writer)?;
            combined.merge(replica.snapshot())?;
            let fields = combined
                .fields()?
                .into_iter()
                .chain(desired.keys().cloned())
                .filter(|field| {
                    selected.contains(&field.resource) && field_enabled(field, &self.scope)
                })
                .collect();
            combined.replace_fields(&desired, &fields)?;
            replica = combined;
        }
        let mut next = self.local.clone();
        next.legacy_imported |= upgraded;
        next.observed
            .extend(self.observed.into_iter().map(|id| (id.writer, id)));
        next.snapshot = replica.snapshot();
        update_effective(&mut next.effective, &desired, &selected, &self.scope);
        // Legacy cloud data still needs a later normal sync to publish its converted history.
        if self.direction == SyncDirection::Download && first_sync && !upgraded {
            next.accept_download();
        }
        let mut cleanup_pending = false;
        if changed {
            // All decoding and domain-independent validation precede checkpointing.
            let configuration = current.resolved_connections(&desired)?;
            let owners = configuration
                .connections
                .records
                .iter()
                .filter(|record| !record.deleted)
                .map(|record| record.id.clone())
                .collect::<HashSet<_>>();
            let forward_records = current.resolved_forwards(&desired)?;
            if forward_records.iter().any(|record| {
                record
                    .owner_connection_id
                    .as_ref()
                    .is_none_or(|id| !owners.contains(id))
            }) {
                bail!("A synchronized forward references an unavailable connection");
            }
            let commands = current.resolved_commands(&desired)?;
            let settings = current.resolved_settings(&desired, settings_store.settings())?;
            let plugins = current.resolved_plugins(&desired)?;
            let mut credentials = current.resolved_credentials(&desired)?;
            let secret_changes = desired
                .iter()
                .filter(|(field, value)| current.values.get(*field) != Some(*value))
                .map(|(field, value)| (field.clone(), value.clone()))
                .collect();
            let auxiliary = crate::sync_v3::auxiliary_secrets::auxiliary_values(&secret_changes)?;
            let managed = crate::sync_v3::auxiliary_secrets::managed_values(&secret_changes)?;
            let privileges = crate::sync_v3::auxiliary_secrets::privilege_values(&desired)?;

            // let providers = oxide_ai::provider_views(&settings.ai.providers);
            // if auxiliary.iter().any(|secret| {
            //     secret.kind == "ai_provider_key"
            //         && (secret.id.starts_with("plugin-secret:")
            //             || !providers.iter().any(|provider| provider.id == secret.id))
            // }) {
            //     bail!("Synchronized AI key has no matching provider");
            // }

            if credentials
                .iter()
                .any(|secret| !oxide_connections::is_profile_credential(secret))
            {
                bail!(
                    "A synchronized credential requires its application owner before it can be applied"
                );
            }
            let mut journal = RecoveryJournal::begin(
                &self.store,
                &self.local,
                connections,
                forwards,
                settings_store,
            )?;
            let audit = oxide_audit::AuditOperation::begin(
                oxide_audit::AuditCategory::Configuration,
                "cloud_sync_apply",
                None,
                Some("causal"),
            );
            let result = (|| -> Result<()> {
                journal.checkpoint_auxiliary(&self.store, &auxiliary)?;
                for key in managed {
                    if let Some(old) = connections.prepare_managed_key_sync(key, |reference| {
                        journal.record_managed_creation(&self.store, reference)
                    })? {
                        journal.defer_managed_cleanup(old);
                    }
                }
                let prepared = connections.prepare_resolved_configuration(configuration)?;
                let cleanup = connections.commit_prepared_saved_connections_snapshot(prepared)?;
                forwards.replace_resolved_forwards(forward_records, &owners)?;
                oxide_quick_commands::save_snapshot(settings_store.path(), &commands)
                    .map_err(anyhow::Error::msg)?;
                settings_store.replace_and_save(settings)?;
                crate::plugin_settings::replace_resolved_plugin_settings(
                    settings_store.path(),
                    plugins,
                )
                .map_err(anyhow::Error::msg)?;
                let credential_cleanup = if self.scope.sync_sensitive_credentials {
                    let selection = crate::credentials::credential_selection(
                        connections,
                        &self.scope,
                        &self.filter,
                    );
                    let stale = connections.prepare_privilege_sync(
                        privileges,
                        &selection.connection_ids,
                        |reference| journal.record_privilege_creation(&self.store, reference),
                    )?;
                    journal.defer_privilege_cleanup(stale);
                    let mut proxy = crate::credentials::global_proxy(settings_store.settings());
                    let prepared = connections.prepare_profile_credentials_with_journal(
                        &credentials,
                        &selection,
                        &mut proxy,
                        |reference| journal.record_credential_creation(&self.store, reference),
                    )?;
                    let mut settings = settings_store.settings().clone();
                    crate::credentials::apply_global_proxy_reference(&mut settings, proxy.as_ref());
                    settings_store.replace_and_save(settings)?;
                    connections.save()?;
                    Some(prepared)
                } else {
                    None
                };
                crate::sync_v3::auxiliary_secrets::apply(&auxiliary)?;
                journal.commit(&self.store, &next, Some(cleanup), credential_cleanup)?;
                Ok(())
            })();
            audit.result(&result);
            for secret in &mut credentials {
                zeroize::Zeroize::zeroize(&mut secret.secret);
            }
            if let Err(error) = result {
                RecoveryJournal::recover(&self.store, connections, forwards, settings_store)
                    .context("Cloud sync apply failed and recovery remains pending")?;
                return Err(error);
            }
            cleanup_pending =
                RecoveryJournal::recover(&self.store, connections, forwards, settings_store)
                    .is_err();
        } else {
            self.store.save(&next)?;
        }
        let mut local_snapshot =
            build_local_snapshot(connections, forwards, settings_store, None, None)?;
        local_snapshot.scope = self.scope.clone();
        local_snapshot.dirty.current_state =
            crate::build_structured_local_state(&local_snapshot.metadata, &self.scope);
        let publication_pending = next.needs_publication();
        Ok(AppliedSync {
            _permit: self._permit,
            store: self.store,
            local: next,
            backend: self.backend,
            settings: self.settings,
            secrets: self.secrets,
            direction: self.direction,
            outcome: SyncOutcome {
                switched_settings: None,
                applied: changed,
                published: false,
                downloaded: false,
                publication_pending,
                recovered: self.recovered,
                cleanup_pending,
                publication: None,
                created_remote_id: None,
                local_snapshot,
                conflicts,
            },
        })
    }
}

impl AppliedSync {
    /// A download commits the local baseline without making any remote writes.
    fn finish_download(mut self) -> SyncOutcome {
        self.outcome.downloaded = true;
        self.outcome.publication = self.local.observed.values().last().cloned();
        self.outcome
    }

    pub async fn publish(mut self) -> Result<SyncOutcome> {
        if self.direction == SyncDirection::Download {
            return Ok(self.finish_download());
        }
        self.outcome.publication = self.local.observed.get(&self.local.writer).cloned();
        if self.outcome.cleanup_pending {
            return Ok(self.outcome);
        }
        if self.settings.backend_type == BackendType::GithubGist {
            if self.settings.git_repository.trim().is_empty() {
                let id = self
                    .backend
                    .create_github_gist(&self.settings, &self.secrets)
                    .await?;
                // Persist the destination before uploading any configuration. A
                // retry after a failed upload must reuse this same private Gist.
                self.local.created_gist_id = Some(id.clone());
                self.store.save(&self.local)?;
                self.settings.git_repository = id;
            }
            self.outcome.created_remote_id = self.local.created_gist_id.clone();
        }
        while self.local.needs_publication() {
            let password =
                required_sync_password(self.secrets.sync_password.as_deref().map(String::as_str))?;
            let context = OxideBatchEncryptionContext::new(password)?;
            self.store.stage_publication(&mut self.local, &context)?;
            let pending = self
                .local
                .pending
                .clone()
                .context("Cloud sync publication was not staged")?;
            self.backend
                .publish_replica(&self.settings, &self.secrets, &pending)
                .await?;
            self.store
                .confirm_publication(&mut self.local, &pending.path)?;
            self.outcome.published = true;
            self.outcome.publication = Some(PublicationId::parse(&pending.path)?);
        }
        for id in self
            .local
            .superseded_publications(chrono::Utc::now().timestamp_millis())
        {
            if self
                .backend
                .delete_publication(&self.settings, &self.secrets, &id)
                .await
                .is_err()
            {
                self.outcome.cleanup_pending = true;
                break;
            }
            self.store.confirm_removal(&mut self.local, &id)?;
        }
        self.outcome.publication_pending = self.local.needs_publication();
        Ok(self.outcome)
    }
}

fn replica_directory(
    settings_path: &std::path::Path,
    settings: &CloudSyncSettings,
) -> Result<std::path::PathBuf> {
    let target = serde_json::to_vec(&(
        &settings.backend_type,
        &settings.endpoint,
        &settings.namespace,
        &settings.git_repository,
        &settings.git_branch,
        &settings.s3_bucket,
        &settings.s3_region,
    ))?;
    Ok(settings_path
        .parent()
        .context("Settings directory is unavailable")?
        .join("cloud-sync-v3")
        .join(sha256_hex(&target).replace(':', "-")))
}

fn open_replica(
    settings_path: &std::path::Path,
    settings: &CloudSyncSettings,
    provider: &mut impl CloudSyncSecretProvider,
) -> Result<ReplicaStore> {
    let directory = replica_directory(settings_path, settings)?;
    if settings.backend_type == BackendType::GithubGist
        && !settings.git_repository.trim().is_empty()
    {
        let mut initial = settings.clone();
        initial.git_repository.clear();
        let bootstrap = replica_directory(settings_path, &initial)?;
        if bootstrap.join("replica.oxide").try_exists()? {
            let store = ReplicaStore::open(&bootstrap, provider)?;
            if store
                .load()?
                .and_then(|local| local.created_gist_id)
                .as_deref()
                == Some(settings.git_repository.as_str())
            {
                return Ok(store);
            }
        }
    }
    ReplicaStore::open(&directory, provider)
}

fn collect(
    connections: &ConnectionStore,
    forwards: &ForwardingRegistry,
    settings: &SettingsStore,
    scope: &crate::SyncScope,
    filter: &StructuredUploadItemFilter,
) -> Result<ConfigurationView> {
    let mut view = ConfigurationView::collect(connections, forwards, settings, scope)?;
    if scope.sync_sensitive_credentials {
        view.include_credentials(connections.export_sync_credentials(
            &crate::credentials::credential_selection(connections, scope, filter),
            crate::credentials::global_proxy(settings.settings()).as_ref(),
        )?)?;
        crate::sync_v3::auxiliary_secrets::collect(&mut view, connections, scope, filter)?;
    }
    Ok(view)
}

fn update_effective(
    effective: &mut SyncValues,
    current: &SyncValues,
    selected: &BTreeSet<SyncResource>,
    scope: &crate::SyncScope,
) {
    effective
        .retain(|field, _| !selected.contains(&field.resource) || !field_enabled(field, scope));
    effective.extend(
        current
            .iter()
            .filter(|(field, _)| selected.contains(&field.resource))
            .map(|(field, value)| (field.clone(), value.clone())),
    );
}

fn field_enabled(field: &crate::sync_v3::SyncField, scope: &crate::SyncScope) -> bool {
    if scope.include_local_terminal_env_vars
        || field.resource.kind != crate::sync_v3::ResourceKind::AppSettings
    {
        return true;
    }
    !field
        .group
        .strip_prefix("setting:")
        .and_then(|path| serde_json::from_str::<Vec<String>>(path).ok())
        .is_some_and(|path| {
            path.len() >= 2 && path[0] == "localTerminal" && path[1] == "customEnvVars"
        })
}

fn capture_baseline(effective: &SyncValues, scope: &crate::SyncScope) -> SyncValues {
    effective
        .iter()
        .filter(|(field, _)| field_enabled(field, scope))
        .map(|(field, value)| (field.clone(), value.clone()))
        .collect()
}

fn latest_publications(publications: Vec<PublicationId>) -> Result<Vec<PublicationId>> {
    let mut latest = std::collections::BTreeMap::<uuid::Uuid, PublicationId>::new();
    for publication in publications {
        if let Some(previous) = latest.get(&publication.writer) {
            if previous.sequence == publication.sequence && previous.digest != publication.digest {
                bail!("A cloud sync writer published conflicting sequence identities");
            }
            if previous.sequence > publication.sequence {
                continue;
            }
        }
        latest.insert(publication.writer, publication);
    }
    Ok(latest.into_values().collect())
}
