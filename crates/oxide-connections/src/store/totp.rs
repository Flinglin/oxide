use super::*;
use crate::totp::{
    TotpCredential, TotpError, TotpGenerator, TotpParameters, validate_totp_pattern,
};

#[derive(Clone)]
pub struct TotpResolver {
    path: PathBuf,
    keychain: ConnectionKeychain,
}

#[derive(Clone)]
pub struct TotpBinding {
    pub credential_id: String,
    resolver: TotpResolver,
}

impl std::fmt::Debug for TotpBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TotpBinding")
            .field("credential_id", &self.credential_id)
            .finish_non_exhaustive()
    }
}

impl PartialEq for TotpBinding {
    fn eq(&self, other: &Self) -> bool {
        self.credential_id == other.credential_id && self.resolver.path == other.resolver.path
    }
}
impl Eq for TotpBinding {}

impl TotpBinding {
    pub fn metadata(&self) -> Result<TotpCredential, TotpError> {
        self.resolver.metadata(&self.credential_id)
    }
    pub fn resolve(&self) -> Result<(TotpCredential, TotpGenerator), TotpError> {
        self.resolver.resolve(&self.credential_id)
    }
}

impl TotpResolver {
    fn metadata(&self, id: &str) -> Result<TotpCredential, TotpError> {
        let bytes = fs::read(&self.path).map_err(|_| TotpError::Unavailable)?;
        let data = decode_connection_store_data(&bytes)
            .map_err(|_| TotpError::Unavailable)?
            .data;
        data.totp_credentials
            .into_iter()
            .find(|entry| entry.id == id && entry.enabled)
            .ok_or(TotpError::Unavailable)
    }
    pub fn resolve(&self, id: &str) -> Result<(TotpCredential, TotpGenerator), TotpError> {
        let audit = oxide_audit::AuditOperation::begin(
            oxide_audit::AuditCategory::Security,
            "credential_read",
            Some(id),
            Some("totp"),
        );
        let result = (|| {
            // Read current metadata on demand so edits and revocation apply to reconnects,
            // without retaining a seed in the long-lived connection registry.
            let credential = self.metadata(id)?;
            let secret = self
                .keychain
                .get(&credential.secret_reference)
                .map_err(|_| TotpError::Unavailable)?;
            let generator = TotpGenerator::parse(secret.expose_secret(), credential.parameters)?;
            Ok((credential, generator))
        })();
        audit.result(&result);
        result
    }
}

impl ConnectionStore {
    pub fn totp_binding(&self, credential_id: Option<&str>) -> Option<TotpBinding> {
        credential_id.map(|id| TotpBinding {
            credential_id: id.into(),
            resolver: self.totp_resolver(),
        })
    }
    pub fn totp_credentials(&self) -> &[TotpCredential] {
        &self.data.totp_credentials
    }
    pub(crate) fn privilege_credential_uses_totp(&self, id: &str) -> bool {
        self.data
            .connections
            .iter()
            .flat_map(|connection| &connection.privilege_credentials)
            .chain(&self.data.local_privilege_credentials)
            .chain(&self.data.ssh_common_privilege_credentials)
            .any(|credential| {
                credential.response_mode.uses_totp()
                    && credential.totp_credential_id.as_deref() == Some(id)
            })
    }
    pub fn totp_resolver(&self) -> TotpResolver {
        TotpResolver {
            path: self.path.clone(),
            keychain: self.keychain.clone(),
        }
    }

    pub(crate) fn export_totp_secret(
        &self,
        credential: &TotpCredential,
    ) -> Result<Option<crate::oxide_file::EncryptedPortableSecret>> {
        if credential.secret_reference.is_empty() {
            return Ok(None);
        }
        Ok(Some(crate::oxide_file::EncryptedPortableSecret {
            kind: PROFILE_CREDENTIAL_KIND.into(),
            id: serde_json::to_string(&CredentialTarget {
                owner: CredentialOwner::Totp(credential.id.clone()),
                slot: CredentialSlot::Primary,
                identity: credential.secret_revision.clone(),
            })?,
            secret: self
                .keychain
                .get(&credential.secret_reference)
                .context("Could not read TOTP secret from protected storage")?
                .into_zeroizing(),
        }))
    }

    pub fn save_totp_credential(
        &mut self,
        id: Option<&str>,
        name: &str,
        input: Option<&SecretString>,
        parameters: TotpParameters,
        prompt_pattern: &str,
        enabled: bool,
    ) -> Result<String, TotpError> {
        self.save_totp_credential_with_connections(
            id,
            name,
            input,
            parameters,
            prompt_pattern,
            enabled,
            None,
        )
    }

    /// Saves the credential and its direct SSH bindings in one metadata transaction.
    #[allow(clippy::too_many_arguments)]
    pub fn save_totp_credential_with_connections(
        &mut self,
        id: Option<&str>,
        name: &str,
        input: Option<&SecretString>,
        parameters: TotpParameters,
        prompt_pattern: &str,
        enabled: bool,
        connection_ids: Option<&HashSet<String>>,
    ) -> Result<String, TotpError> {
        let audit = oxide_audit::AuditOperation::begin(
            oxide_audit::AuditCategory::Configuration,
            "credential_set",
            id,
            Some("totp"),
        );
        let result = (|| {
            let name = name.trim();
            if name.is_empty() || name.len() > 256 {
                return Err(TotpError::InvalidParameters);
            }
            validate_totp_pattern(prompt_pattern)?;
            let previous = id
                .and_then(|id| {
                    self.data
                        .totp_credentials
                        .iter()
                        .find(|entry| entry.id == id)
                })
                .cloned();
            if id.is_some() && previous.is_none() {
                return Err(TotpError::Unavailable);
            }
            if connection_ids.is_some_and(|ids| ids.iter().any(|id| self.get(id).is_none())) {
                return Err(TotpError::Unavailable);
            }
            let id = id
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| Uuid::new_v4().to_string());
            let generator = input
                .map(|secret| TotpGenerator::parse(secret.expose_secret(), parameters))
                .transpose()?;
            let reference = if generator.is_some() {
                format!("totp:{}", Uuid::new_v4())
            } else {
                previous
                    .as_ref()
                    .ok_or(TotpError::InvalidSecret)?
                    .secret_reference
                    .clone()
            };
            let secret_revision = previous
                .as_ref()
                .filter(|_| input.is_none())
                .map(|p| p.secret_revision.clone())
                .unwrap_or_else(|| Uuid::new_v4().to_string());
            let credential = TotpCredential {
                id: id.clone(),
                name: name.into(),
                parameters: generator
                    .as_ref()
                    .map_or(parameters, TotpGenerator::parameters),
                prompt_pattern: prompt_pattern.into(),
                enabled,
                updated_at: Utc::now(),
                secret_revision,
                secret_reference: reference.clone(),
            };
            credential.validate()?;
            if let Some(generator) = &generator {
                let secret = SecretString::from(generator.encoded_secret());
                self.keychain
                    .store(&reference, &secret)
                    .map_err(|_| TotpError::SaveFailed)?;
            }
            let mut candidate = self.clone();
            candidate
                .data
                .totp_credentials
                .retain(|entry| entry.id != id);
            candidate.data.totp_credentials.push(credential);
            if let Some(connection_ids) = connection_ids {
                for connection in &mut candidate.data.connections {
                    let selected = connection_ids.contains(&connection.id);
                    let current = connection.options.totp_credential_id.as_deref();
                    // Unchecking removes only this credential, preserving other bindings and hops.
                    if selected && current != Some(id.as_str()) {
                        connection.options.totp_credential_id = Some(id.clone());
                        connection.updated_at = Some(Utc::now());
                    } else if !selected && current == Some(id.as_str()) {
                        connection.options.totp_credential_id = None;
                        connection.updated_at = Some(Utc::now());
                    }
                }
            }
            if let Some(previous) = &previous
                && input.is_some()
                && !previous.secret_reference.is_empty()
            {
                candidate
                    .data
                    .pending_keychain_cleanup
                    .push(previous.secret_reference.clone());
            }
            if candidate.save().is_err() {
                if input.is_some() {
                    let _ = self.keychain.delete(&reference);
                }
                return Err(TotpError::SaveFailed);
            }
            self.data = candidate.data;
            let _ = self.retry_persisted_keychain_cleanup();
            Ok(id)
        })();
        audit.result(&result);
        result
    }

    pub fn delete_totp_credential(&mut self, id: &str) -> Result<(), TotpError> {
        let audit = oxide_audit::AuditOperation::begin(
            oxide_audit::AuditCategory::Configuration,
            "credential_delete",
            Some(id),
            Some("totp"),
        );
        let result = (|| {
            let all = CredentialSyncSelection {
                connection_ids: self.data.connections.iter().map(|p| p.id.clone()).collect(),
                mosh_ids: self
                    .data
                    .mosh_profiles
                    .iter()
                    .map(|p| p.id.clone())
                    .collect(),
                sftp_ids: self
                    .data
                    .standalone_sftp_profiles
                    .iter()
                    .map(|p| p.id.clone())
                    .collect(),
                ..Default::default()
            };
            if self.totp_selected(id, &all) || self.privilege_credential_uses_totp(id) {
                return Err(TotpError::InUse);
            }
            let previous = self
                .data
                .totp_credentials
                .iter()
                .find(|entry| entry.id == id)
                .ok_or(TotpError::Unavailable)?;
            let mut candidate = self.clone();
            candidate
                .data
                .pending_keychain_cleanup
                .push(previous.secret_reference.clone());
            candidate
                .data
                .totp_credentials
                .retain(|entry| entry.id != id);
            candidate.save().map_err(|_| TotpError::SaveFailed)?;
            self.data = candidate.data;
            let _ = self.retry_persisted_keychain_cleanup();
            Ok(())
        })();
        audit.result(&result);
        result
    }
    pub(crate) fn totp_selected(&self, id: &str, selection: &CredentialSyncSelection) -> bool {
        // Exporters that already resolved the referenced seeds can select them
        // directly, e.g. privilege copies filtered by the exported host.
        if selection.totp_ids.contains(id) {
            return true;
        }
        let hops_use = |hops: &[SavedProxyHop]| {
            hops.iter()
                .any(|h| h.totp_credential_id.as_deref() == Some(id))
        };
        let privilege_credentials_use = |credentials: &[SavedPrivilegeCredential]| {
            credentials.iter().any(|credential| {
                credential.response_mode.uses_totp()
                    && credential.totp_credential_id.as_deref() == Some(id)
            })
        };
        self.data.connections.iter().any(|p| {
            selection.connection_ids.contains(&p.id)
                && (p.options.totp_credential_id.as_deref() == Some(id)
                    || hops_use(&p.proxy_chain)
                    || privilege_credentials_use(&p.privilege_credentials)
                    || p.options
                        .jump_host
                        .as_deref()
                        .and_then(|jump| self.get(jump))
                        .is_some_and(|jump| jump.options.totp_credential_id.as_deref() == Some(id)))
        }) || (selection
            .connection_ids
            .contains(SSH_COMMON_PRIVILEGE_SCOPE_ID)
            && privilege_credentials_use(&self.data.ssh_common_privilege_credentials))
            || self
                .data
                .mosh_profiles
                .iter()
                .any(|p| selection.mosh_ids.contains(&p.id) && hops_use(&p.proxy_chain))
            || self.data.standalone_sftp_profiles.iter().any(|p| {
                selection.sftp_ids.contains(&p.id)
                    && (hops_use(&p.proxy_chain)
                        || p.secondary_endpoint
                            .as_ref()
                            .is_some_and(|e| hops_use(&e.proxy_chain)))
            })
    }

    pub(crate) fn merge_totp_credentials(
        &mut self,
        credentials: Vec<TotpCredential>,
        replace: bool,
    ) -> Result<usize> {
        let mut seen = std::collections::HashSet::new();
        for credential in &credentials {
            credential.validate()?;
            if !seen.insert(&credential.id) {
                anyhow::bail!("Duplicate TOTP credential");
            }
        }
        let mut changed = 0;
        for mut credential in credentials {
            credential.secret_reference.clear();
            if let Some(existing) = self
                .data
                .totp_credentials
                .iter_mut()
                .find(|p| p.id == credential.id)
            {
                if !replace && existing.updated_at >= credential.updated_at {
                    continue;
                }
                if existing.secret_revision == credential.secret_revision {
                    credential.secret_reference = existing.secret_reference.clone();
                } else if !existing.secret_reference.is_empty() {
                    self.data
                        .pending_keychain_cleanup
                        .push(existing.secret_reference.clone());
                }
                *existing = credential;
            } else {
                self.data.totp_credentials.push(credential);
            }
            changed += 1;
        }
        Ok(changed)
    }
}
