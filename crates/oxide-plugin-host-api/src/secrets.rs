const PLUGIN_SECRET_ACCOUNT_PREFIX: &str = "plugin-secret";

pub fn plugin_secret_account_id(plugin_id: &str, key: &str) -> Result<String, String> {
    validate_plugin_secret_plugin_id(plugin_id)?;
    validate_plugin_secret_key(key)?;
    // Length prefixes keep the keychain namespace unambiguous without ever
    // including the secret value itself in the persisted account identifier.
    Ok(format!(
        "{PLUGIN_SECRET_ACCOUNT_PREFIX}:{}:{}:{}:{}",
        plugin_id.len(),
        plugin_id,
        key.len(),
        key
    ))
}

fn validate_plugin_secret_plugin_id(plugin_id: &str) -> Result<(), String> {
    if plugin_id.is_empty() {
        return Err("Plugin ID cannot be empty".to_string());
    }
    if plugin_id.contains('/') || plugin_id.contains('\\') || plugin_id.contains("..") {
        return Err("Plugin ID contains invalid path characters".to_string());
    }
    if plugin_id.bytes().any(|byte| byte < 0x20) {
        return Err("Plugin ID contains invalid characters".to_string());
    }
    Ok(())
}

fn validate_plugin_secret_key(key: &str) -> Result<(), String> {
    if key.is_empty() {
        return Err("Plugin secret key cannot be empty".to_string());
    }
    if key.bytes().any(|byte| byte < 0x20) {
        return Err("Plugin secret key contains invalid characters".to_string());
    }
    Ok(())
}
