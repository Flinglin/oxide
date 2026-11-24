use super::*;
use crate::sync_v3::{ConfigurationView, ReplicaSnapshot, ResourceKind, SyncReplica, SyncValues};
use oxide_connections::oxide_file::{
    EncryptedPayload, decode_archive_sync_connections, decrypt_oxide_file_with_context_and_progress,
};

impl CloudSyncOperationService {
    pub(super) async fn read_upgrade_snapshot(
        &self,
        connections: &ConnectionStore,
        settings: &CloudSyncSettings,
        secrets: &crate::secrets::CloudSyncSecrets,
        metadata: RemoteMetadata,
        _scope: &crate::SyncScope,
    ) -> Result<ReplicaSnapshot> {
        // Preserve the entire remote snapshot during conversion. The coordinator
        // separately limits materialization to this device's selected resources.
        let scope = &crate::SyncScope {
            sync_sensitive_credentials: true,
            include_local_terminal_env_vars: true,
            app_settings_sections: crate::OXIDE_APP_SETTINGS_SECTION_IDS
                .iter()
                .map(|id| id.to_string())
                .collect(),
            ..Default::default()
        };
        let mut view = ConfigurationView::empty();
        let password =
            required_sync_password(secrets.sync_password.as_deref().map(String::as_str))?;
        let mut decoder = OxideBatchDecryptionContext::new(password)?;
        if crate::structured_manifest_format_supported(metadata.format.as_deref()) {
            let manifest = manifest_from_metadata(&metadata)?;
            if let Some(entry) = &manifest.sections.connections {
                view.insert_connections(serde_json::from_slice(
                    &self
                        .read_required_object(settings, secrets, entry)
                        .await?
                        .bytes,
                )?)?;
            }
            for (kind, entry) in [
                (ResourceKind::Forward, manifest.sections.forwards.as_ref()),
                (
                    ResourceKind::Serial,
                    manifest.sections.serial_profiles.as_ref(),
                ),
                (
                    ResourceKind::Telnet,
                    manifest.sections.telnet_profiles.as_ref(),
                ),
                (ResourceKind::Mosh, manifest.sections.mosh_profiles.as_ref()),
                (
                    ResourceKind::StandaloneSftp,
                    manifest.sections.standalone_sftp_profiles.as_ref(),
                ),
                (
                    ResourceKind::RemoteDesktop,
                    manifest.sections.remote_desktop_profiles.as_ref(),
                ),
            ] {
                if let Some(entry) = entry {
                    insert_records(
                        &mut view,
                        kind,
                        &self
                            .read_required_object(settings, secrets, entry)
                            .await?
                            .bytes,
                    )?;
                }
            }
            if let Some(entry) = &manifest.sections.quick_commands {
                insert_commands(
                    &mut view,
                    &self
                        .read_required_object(settings, secrets, entry)
                        .await?
                        .bytes,
                )?;
            }
            for entry in manifest
                .sections
                .app_settings
                .values()
                .chain(manifest.sections.plugin_settings.values())
                .chain(
                    manifest
                        .sections
                        .sensitive_credentials
                        .iter()
                        .filter(|_| scope.sync_sensitive_credentials),
                )
            {
                let object = self.read_required_object(settings, secrets, entry).await?;
                insert_archive(
                    &mut view,
                    connections,
                    decode_archive(&object.bytes, &mut decoder)?,
                    scope,
                )?;
            }
        } else {
            let remote = self
                .backend
                .download_remote_snapshot(settings, secrets)
                .await?;
            insert_archive(
                &mut view,
                connections,
                decode_archive(&remote.bytes, &mut decoder)?,
                scope,
            )?;
        }
        let mut replica = SyncReplica::new(uuid::Uuid::new_v4());
        let selected = view.selected(std::iter::empty(), scope, &Default::default());
        replica.capture_local(&SyncValues::new(), &view.values, &selected)?;
        Ok(replica.snapshot())
    }
}

fn decode_archive(
    bytes: &[u8],
    decoder: &mut OxideBatchDecryptionContext,
) -> Result<EncryptedPayload> {
    Ok(decrypt_oxide_file_with_context_and_progress(
        &OxideFile::from_bytes(bytes)?,
        decoder,
        |_| {},
    )?)
}

fn insert_records(view: &mut ConfigurationView, kind: ResourceKind, bytes: &[u8]) -> Result<()> {
    let value: serde_json::Value = serde_json::from_slice(bytes)?;
    for record in value
        .get("records")
        .and_then(serde_json::Value::as_array)
        .context("Invalid legacy records")?
    {
        let id = record
            .get("id")
            .and_then(serde_json::Value::as_str)
            .context("Missing legacy record identity")?;
        if kind == ResourceKind::Forward {
            if record.get("deleted").and_then(serde_json::Value::as_bool) == Some(true) {
                view.mark_deleted(kind, id)?;
                continue;
            }
            view.insert(
                kind,
                id,
                record.get("payload").context("Missing forward payload")?,
            )?;
        } else {
            view.insert(kind, id, record)?;
        }
    }
    if kind == ResourceKind::StandaloneSftp
        && let Some(ftp) = value.get("ftp").filter(|value| !value.is_null())
    {
        insert_records(view, ResourceKind::Ftp, &serde_json::to_vec(ftp)?)?;
    }
    if let Some(tombstones) = value
        .get("tombstones")
        .and_then(serde_json::Value::as_array)
    {
        for record in tombstones {
            if let Some(id) = record.get("id").and_then(serde_json::Value::as_str) {
                view.mark_deleted(kind, id)?;
            }
        }
    }
    Ok(())
}

fn insert_commands(view: &mut ConfigurationView, bytes: &[u8]) -> Result<()> {
    let text = std::str::from_utf8(bytes)?;
    let snapshot = oxide_quick_commands::decode_snapshot_json(text).map_err(anyhow::Error::msg)?;
    for record in snapshot.commands {
        view.insert(ResourceKind::QuickCommand, &record.id, &record)?;
    }
    for record in snapshot.categories {
        view.insert(ResourceKind::QuickCommandCategory, &record.id, &record)?;
    }
    Ok(())
}

fn insert_archive(
    view: &mut ConfigurationView,
    connections: &ConnectionStore,
    mut payload: EncryptedPayload,
    scope: &crate::SyncScope,
) -> Result<()> {
    let mut decoded =
        decode_archive_sync_connections(connections, std::mem::take(&mut payload.connections))?;
    decoded.snapshot.plugin_profiles = std::mem::take(&mut payload.plugin_profiles)
        .into_iter()
        .map(|profile| profile.portable())
        .collect();
    // Structured records carry authoritative stable metadata; the credential
    // archive may also contain a reduced copy of those same connections.
    let mut archive = ConfigurationView::empty();
    archive.insert_connections(decoded.snapshot)?;
    for (field, value) in archive.values {
        view.values.entry(field).or_insert(value);
    }
    if scope.sync_sensitive_credentials {
        view.include_credentials(decoded.credentials)?;
        for record in decoded.privilege_credentials {
            let id = serde_json::to_string(&(&record.metadata.connection_id, &record.metadata.id))?;
            view.insert_secret(ResourceKind::PrivilegeCredential, &id, &record)?;
        }
        for key in decoded.managed_keys {
            view.insert_secret(ResourceKind::ManagedKey, &key.metadata.id, &key)?;
        }
        let mut profile = Vec::new();
        for secret in std::mem::take(&mut payload.portable_secrets) {
            match secret.kind.as_str() {
                "plugin_secret" => view.insert_secret(
                    ResourceKind::PluginCredential,
                    &secret.id,
                    &Some(secret.secret),
                )?,
                _ if oxide_connections::is_profile_credential(&secret) => profile.push(secret),
                _ => bail!("Unsupported credential owner in legacy cloud data"),
            }
        }
        view.include_credentials(profile)?;
    }
    for credential in payload.totp_credentials {
        view.insert(ResourceKind::Totp, &credential.id, &credential.portable())?;
    }
    if let Some(json) = payload.app_settings_json {
        view.insert_app_settings(&json)?;
    }
    if let Some(json) = payload.quick_commands_json {
        insert_commands(view, json.as_bytes())?;
    }
    for (kind, json) in [
        (ResourceKind::Serial, payload.serial_profiles_json),
        (ResourceKind::Telnet, payload.telnet_profiles_json),
        (ResourceKind::Mosh, payload.mosh_profiles_json),
        (
            ResourceKind::StandaloneSftp,
            payload.standalone_sftp_profiles_json,
        ),
        (
            ResourceKind::RemoteDesktop,
            payload.remote_desktop_profiles_json,
        ),
    ] {
        if let Some(json) = json {
            insert_records(view, kind, json.as_bytes())?;
        }
    }
    for record in payload.plugin_settings {
        view.insert_secret(
            ResourceKind::PluginSettings,
            &record.storage_key,
            &record.serialized_value,
        )?;
    }
    for record in decoded.forwards {
        let id = record
            .id
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let value = serde_json::json!({"id":id,"sessionId":format!("saved:{}",record.connection_id),"ownerConnectionId":record.connection_id,"forwardType":record.forward_type,"bindAddress":record.bind_address,"bindPort":record.bind_port,"targetHost":record.target_host,"targetPort":record.target_port,"description":record.description.unwrap_or_default(),"autoStart":record.auto_start,"createdAt":chrono::Utc::now().to_rfc3339()});
        view.insert(ResourceKind::Forward, &id, &value)?;
    }
    Ok(())
}
