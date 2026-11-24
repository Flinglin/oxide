use super::*;

impl AppliedSync {
    /// The active settings change only after the new snapshot is read back and
    /// authenticated. The previous namespace and its password remain available.
    pub async fn change_password(
        mut self,
        password: Zeroizing<String>,
        provider: &mut impl CloudSyncSecretProvider,
        settings_path: &std::path::Path,
    ) -> Result<SyncOutcome> {
        if password.chars().count() < 6 {
            bail!("password_too_short");
        }
        if self.outcome.cleanup_pending {
            bail!("sync_cleanup_pending");
        }
        if self.secrets.sync_password.as_deref().map(String::as_str) == Some(password.as_str()) {
            return self.publish().await;
        }
        let settings = if let Some(settings) = &self.local.pending_password_change {
            let previous =
                provider.get_secret(settings.password_secret_key(), SecretReadMode::Prompt)?;
            if previous.as_deref().map(String::as_str) != Some(password.as_str()) {
                bail!("password_change_pending");
            }
            settings.clone()
        } else {
            let mut settings = self.settings.clone();
            settings.namespace = format!("{}-{}", settings.namespace, uuid::Uuid::new_v4());
            let reference = format!("sync-v3-password-{}", uuid::Uuid::new_v4());
            provider.store_secret(&reference, Some(password.as_str()))?;
            settings.sync_password_ref = Some(reference);
            self.local.pending_password_change = Some(settings.clone());
            self.store.save(&self.local)?;
            settings
        };
        let store = open_replica(settings_path, &settings, provider)?;
        let existing = store.load()?;
        let resuming = existing.is_some();
        let mut local = existing.unwrap_or_default();
        let mut replica = SyncReplica::load(local.snapshot.clone(), local.writer)?;
        replica.merge(self.local.snapshot.clone())?;
        local.snapshot = replica.snapshot();
        local.effective = self.local.effective.clone();
        local.legacy_imported = true;
        store.bind_sync_password(&mut local, &password)?;
        store.save(&local)?;
        let mut secrets = self.secrets;
        secrets.sync_password = Some(password);
        if !self
            .backend
            .list_publications(&settings, &secrets)
            .await?
            .is_empty()
            && !resuming
        {
            bail!("The new synchronization namespace is already occupied");
        }
        let mut outcome = self.outcome;
        outcome.switched_settings = Some(settings.clone());
        outcome.publication = None;
        outcome.published = false;
        outcome.publication_pending = true;
        let next = AppliedSync {
            direction: SyncDirection::Merge,
            _permit: self._permit,
            store,
            local,
            backend: self.backend,
            settings,
            secrets,
            outcome,
        };
        let outcome = next.publish().await?;
        self.local.pending_password_change = None;
        self.store.save(&self.local)?;
        Ok(outcome)
    }
}
