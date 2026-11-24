use crate::SecretString;
use anyhow::{Context, Result};
use oxide_portable_runtime::keystore::{self as portable_keystore, PortableKeystoreError};
use oxide_secret_store::NativeSecretStore;
const SERVICE_NAME: &str = "com.oxide.ssh";
const PORTABLE_ACCOUNT_PREFIX: &str = "portable:v1:";
const LEGACY_ACCOUNT_SEPARATOR: &str = "@";

#[derive(Clone, Debug)]
pub(crate) struct ConnectionKeychain {
    service: String,
    #[cfg(target_os = "macos")]
    authentication_reason: Option<String>,
}

/// Grants multiple reads after one operation-scoped authentication.
pub(crate) struct ConnectionKeychainBatchAccess<'a> {
    keychain: &'a ConnectionKeychain,
}

#[derive(Clone, Copy)]
enum ConnectionSecretReadMode {
    Standard,
    PreserveMultiline,
}

impl Default for ConnectionKeychain {
    fn default() -> Self {
        Self {
            service: SERVICE_NAME.to_string(),
            #[cfg(target_os = "macos")]
            authentication_reason: None,
        }
    }
}

impl ConnectionKeychain {
    pub(crate) fn with_service(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
            #[cfg(target_os = "macos")]
            authentication_reason: None,
        }
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn with_macos_device_owner_authentication(
        service: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            service: service.into(),
            authentication_reason: Some(reason.into()),
        }
    }

    pub(crate) fn store(&self, id: &str, secret: &SecretString) -> Result<()> {
        if portable_keychain_enabled()? {
            let account = portable_account(id);
            let legacy_suffix = legacy_portable_account_suffix(id);
            return portable_keystore::store_secret_replacing_legacy_accounts(
                &self.service,
                &account,
                &legacy_suffix,
                secret.expose_secret(),
            )
            .with_context(|| format!("failed to store password in portable keystore for {id}"));
        }

        NativeSecretStore::new(&self.service)
            .store(&self.native_account(id), secret.expose_secret())
            .with_context(|| format!("failed to store password in OS keychain for {id}"))
    }

    pub(crate) fn get(&self, id: &str) -> Result<SecretString> {
        self.get_optional(id)?
            .ok_or_else(|| anyhow::anyhow!("Password not saved for this connection"))
    }

    pub(crate) fn get_optional(&self, id: &str) -> Result<Option<SecretString>> {
        self.get_optional_with_authentication(id, true, ConnectionSecretReadMode::Standard)
    }

    pub(crate) fn get_preserving_multiline(&self, id: &str) -> Result<SecretString> {
        self.get_optional_with_authentication(
            id,
            true,
            ConnectionSecretReadMode::PreserveMultiline,
        )?
        .ok_or_else(|| anyhow::anyhow!("Password not saved for this connection"))
    }

    pub(crate) fn authenticate_batch_access(&self) -> Result<ConnectionKeychainBatchAccess<'_>> {
        #[cfg(target_os = "macos")]
        if let Some(reason) = self.authentication_reason.as_deref() {
            oxide_secret_store::authenticate_device_owner(reason)
                .context("failed to authenticate batch keychain access")?;
        }
        Ok(ConnectionKeychainBatchAccess { keychain: self })
    }

    fn get_optional_with_authentication(
        &self,
        id: &str,
        authenticate_device_owner: bool,
        read_mode: ConnectionSecretReadMode,
    ) -> Result<Option<SecretString>> {
        #[cfg(not(target_os = "macos"))]
        let _ = authenticate_device_owner;

        if portable_keychain_enabled()? {
            let account = portable_account(id);
            let legacy_suffix = legacy_portable_account_suffix(id);
            return match portable_keystore::get_secret_migrating_legacy_account(
                &self.service,
                &account,
                &legacy_suffix,
            ) {
                Ok(secret) => Ok(Some(SecretString::from(secret))),
                Err(PortableKeystoreError::NotFound(_)) => Ok(None),
                Err(error) => Err(error).with_context(|| {
                    format!("failed to load password from portable keystore for {id}")
                }),
            };
        }

        #[cfg(target_os = "macos")]
        if authenticate_device_owner && let Some(reason) = self.authentication_reason.as_deref() {
            oxide_secret_store::authenticate_device_owner(reason)
                .with_context(|| format!("failed to authenticate keychain access for {id}"))?;
        }

        let secret_store = NativeSecretStore::new(&self.service);
        let secret = match read_mode {
            ConnectionSecretReadMode::Standard => {
                // Connection secrets may contain arbitrary UTF-8 text. The macOS backend keeps
                // ordinary legacy reads prompt-free and migrates only ambiguous CLI output.
                secret_store.get_preserving_text(&self.native_account(id))
            }
            ConnectionSecretReadMode::PreserveMultiline => {
                // Managed private keys are the only connection secrets whose
                // newline-preserving representation requires native macOS reads.
                secret_store.get_preserving_multiline(&self.native_account(id))
            }
        };
        secret
            // Move the keychain result directly into its zeroizing domain owner
            // so no unmanaged String copy survives this boundary.
            .map(|secret| secret.map(SecretString::from))
            .with_context(|| format!("failed to load password from OS keychain for {id}"))
    }

    pub(crate) fn delete(&self, id: &str) -> Result<()> {
        if portable_keychain_enabled()? {
            let account = portable_account(id);
            let legacy_suffix = legacy_portable_account_suffix(id);
            return portable_keystore::delete_secret_with_legacy_accounts(
                &self.service,
                &account,
                &legacy_suffix,
            )
            .with_context(|| format!("failed to delete password from portable keystore for {id}"));
        }

        NativeSecretStore::new(&self.service)
            .delete(&self.native_account(id))
            .with_context(|| format!("failed to delete password from OS keychain for {id}"))
    }

    fn native_account(&self, id: &str) -> String {
        format!("{}@{}", whoami::username(), id)
    }
}

impl ConnectionKeychainBatchAccess<'_> {
    pub(crate) fn get_optional(&self, id: &str) -> Result<Option<SecretString>> {
        // The guard can only be constructed after the batch authorization step.
        self.keychain.get_optional_with_authentication(
            id,
            false,
            ConnectionSecretReadMode::Standard,
        )
    }
}

fn portable_account(id: &str) -> String {
    // Portable Vault authentication, rather than the host OS account, owns
    // access to this stable entry across machines and system users.
    format!("{PORTABLE_ACCOUNT_PREFIX}{id}")
}

fn legacy_portable_account_suffix(id: &str) -> String {
    format!("{LEGACY_ACCOUNT_SEPARATOR}{id}")
}

fn portable_keychain_enabled() -> Result<bool> {
    oxide_portable_runtime::is_portable_mode().context("failed to determine oxide portable mode")
}
