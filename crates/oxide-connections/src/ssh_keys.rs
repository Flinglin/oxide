use std::path::PathBuf;

use zeroize::Zeroizing;

use crate::ssh_paths::default_ssh_dir;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SshKeyInfo {
    pub name: String,
    pub path: String,
    pub key_type: String,
    pub has_passphrase: bool,
}

pub fn list_available_ssh_keys() -> Vec<SshKeyInfo> {
    default_private_key_paths_in_ssh_dir(default_ssh_dir())
        .into_iter()
        .filter_map(|path| {
            let status = default_private_key_status(&path, None)?;
            let name = path.file_name()?.to_str()?;
            Some(SshKeyInfo {
                name: name.to_string(),
                path: path.to_string_lossy().into_owned(),
                key_type: ssh_key_type_from_name(name).to_string(),
                has_passphrase: status == DefaultPrivateKeyStatus::RequiresPassphrase,
            })
        })
        .collect()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DefaultPrivateKeyStatus {
    Loadable,
    RequiresPassphrase,
}

pub(crate) fn default_private_key_paths_in_ssh_dir(ssh: PathBuf) -> Vec<PathBuf> {
    let preferred_names = ["id_ed25519", "id_ecdsa", "id_rsa"];
    let mut paths = preferred_names
        .iter()
        .map(|name| ssh.join(name))
        .collect::<Vec<_>>();

    let Ok(entries) = std::fs::read_dir(&ssh) else {
        return paths;
    };
    let mut discovered = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| default_key_candidate_name(path).is_some())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_none_or(|name| !preferred_names.contains(&name))
        })
        .collect::<Vec<_>>();
    discovered.sort_by(|left, right| {
        left.file_name()
            .unwrap_or_default()
            .cmp(right.file_name().unwrap_or_default())
    });
    paths.extend(discovered);
    paths
}

pub(crate) fn default_private_key_status(
    path: &PathBuf,
    passphrase: Option<&str>,
) -> Option<DefaultPrivateKeyStatus> {
    if !path.exists() || default_key_candidate_name(path).is_none() {
        return None;
    }
    let key_data = Zeroizing::new(std::fs::read_to_string(path).ok()?);
    if private_key_text_looks_unsupported_direct_key(&key_data) {
        return None;
    }
    match russh::keys::decode_secret_key(&key_data, passphrase) {
        Ok(_) => Some(DefaultPrivateKeyStatus::Loadable),
        Err(error) if private_key_error_is_passphrase_related(&error) => {
            Some(DefaultPrivateKeyStatus::RequiresPassphrase)
        }
        Err(_) => None,
    }
}

fn default_key_candidate_name(path: &PathBuf) -> Option<&str> {
    let name = path.file_name()?.to_str()?;
    if name.starts_with("id_") && !name.ends_with(".pub") && !name.ends_with("-cert.pub") {
        Some(name)
    } else {
        None
    }
}

fn private_key_text_looks_unsupported_direct_key(private_key: &str) -> bool {
    private_key.contains("-----BEGIN DSA PRIVATE KEY-----") || private_key.contains("ssh-dss")
}

pub(crate) fn private_key_error_is_passphrase_related(error: &russh::keys::Error) -> bool {
    let normalized = error.to_string().to_ascii_lowercase();
    normalized.contains("decrypt")
        || normalized.contains("password")
        || normalized.contains("passphrase")
        || normalized.contains("encrypted")
        || normalized.contains("bcrypt")
        || normalized.contains("kdf")
        || normalized.contains("crypto")
        || normalized.contains("cryptographic")
}

fn ssh_key_type_from_name(name: &str) -> &'static str {
    if name.contains("ed25519") && name.ends_with("_sk") {
        "ED25519-SK"
    } else if name.contains("ecdsa") && name.ends_with("_sk") {
        "ECDSA-SK"
    } else if name.contains("ed25519") {
        "ED25519"
    } else if name.contains("ecdsa") {
        "ECDSA"
    } else if name.contains("rsa") {
        "RSA"
    } else if name.contains("dsa") {
        "DSA"
    } else {
        "Unknown"
    }
}
