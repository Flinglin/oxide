use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::Duration,
};

use anyhow::Result;

use crate::{
    ConnectionStore, SSH_CONFIG_TAG, SSH_PROXY_COMMAND_TAG, SSH_REMOTE_COMMAND_TAG, SavedAuth,
    SavedConnection, SshConfigHost, list_ssh_config_hosts_from_path,
    saved_connection_from_ssh_host,
};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SshConfigSyncOutcome {
    pub imported: Vec<String>,
    pub updated: Vec<String>,
    pub skipped: Vec<String>,
}

pub struct SshConfigSyncService {
    stop_tx: Option<mpsc::Sender<()>>,
    worker: Option<thread::JoinHandle<()>>,
}

impl SshConfigSyncService {
    pub fn start(
        connection_store_path: PathBuf,
        ssh_config_path: PathBuf,
        interval: Duration,
    ) -> Self {
        let (stop_tx, stop_rx) = mpsc::channel();
        let interval = interval.max(Duration::from_secs(1));
        let worker = thread::Builder::new()
            .name("oxide-ssh-config-sync".to_string())
            .spawn(move || {
                let mut previous_hosts = None;
                loop {
                    let hosts = list_ssh_config_hosts_from_path(&ssh_config_path, &HashSet::new());
                    if let Ok(hosts) = hosts
                        && previous_hosts.as_ref() != Some(&hosts)
                    {
                        // The connection layer owns parsing, drift calculation,
                        // and persistence; GPUI only observes the resulting store file.
                        if sync_resolved_ssh_config_hosts(&connection_store_path, hosts.clone())
                            .is_ok()
                        {
                            previous_hosts = Some(hosts);
                        }
                    }
                    match stop_rx.recv_timeout(interval) {
                        Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                    }
                }
            })
            .ok();
        Self {
            stop_tx: Some(stop_tx),
            worker,
        }
    }
}

impl Drop for SshConfigSyncService {
    fn drop(&mut self) {
        if let Some(stop_tx) = self.stop_tx.take() {
            let _ = stop_tx.send(());
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub fn sync_ssh_config_path_into_store(
    connection_store_path: &Path,
    ssh_config_path: &Path,
) -> Result<SshConfigSyncOutcome> {
    let hosts = list_ssh_config_hosts_from_path(ssh_config_path, &HashSet::new())?;
    sync_resolved_ssh_config_hosts(connection_store_path, hosts)
}

fn sync_resolved_ssh_config_hosts(
    connection_store_path: &Path,
    hosts: Vec<SshConfigHost>,
) -> Result<SshConfigSyncOutcome> {
    let mut store = ConnectionStore::load(connection_store_path.to_path_buf())?;
    let mut pending = Vec::new();
    let mut outcome = SshConfigSyncOutcome::default();

    for host in hosts {
        let alias = host.alias.clone();
        let existing = store
            .connections()
            .iter()
            .find(|connection| connection.name.eq_ignore_ascii_case(&alias))
            .cloned();
        let Some(existing) = existing else {
            pending.push(saved_connection_from_ssh_host(host)?);
            outcome.imported.push(alias);
            continue;
        };
        if !existing.tags.iter().any(|tag| tag == SSH_CONFIG_TAG) {
            // A same-name manual connection always wins over automatic import.
            outcome.skipped.push(alias);
            continue;
        }

        let mut resolved = saved_connection_from_ssh_host(host)?;
        if ssh_config_fields_match(&existing, &resolved) {
            outcome.skipped.push(alias);
            continue;
        }
        resolved.id = existing.id;
        resolved.group = existing.group;
        resolved.color = existing.color;
        resolved.icon = existing.icon;
        resolved.tags = merged_ssh_config_tags(&existing.tags, &resolved.tags);
        let existing_remote_command_managed = has_remote_command_tag(&existing.tags);
        let resolved_remote_command_managed = has_remote_command_tag(&resolved.tags);
        let resolved_post_connect_command = resolved.options.post_connect_command.take();
        let resolved_agent_forwarding = resolved.options.agent_forwarding;
        let resolved_connect_timeout_seconds = resolved.options.connect_timeout_seconds;
        let resolved_identity_agent = resolved.options.identity_agent.clone();
        let resolved_agent_forwarding_socket = resolved.options.agent_forwarding_socket.clone();
        let resolved_x11_forwarding = resolved.options.x11_forwarding;
        resolved.options = existing.options;
        // These fields remain owned by the imported OpenSSH config while
        // unrelated application-specific connection options stay untouched.
        resolved.options.connect_timeout_seconds = resolved_connect_timeout_seconds;
        resolved.options.agent_forwarding = resolved_agent_forwarding;
        resolved.options.identity_agent = resolved_identity_agent;
        resolved.options.agent_forwarding_socket = resolved_agent_forwarding_socket;
        resolved.options.x11_forwarding = resolved_x11_forwarding;
        if existing_remote_command_managed || resolved_remote_command_managed {
            // The marker transfers ownership back to OpenSSH config, including
            // clearing a command when RemoteCommand is removed or set to none.
            resolved.options.post_connect_command = resolved_post_connect_command;
            resolved.post_connect_command = None;
        } else {
            resolved.post_connect_command = existing.post_connect_command;
        }
        resolved.upstream_proxy = existing.upstream_proxy;
        pending.push(resolved);
        outcome.updated.push(alias);
    }

    if !pending.is_empty() {
        store.upsert_imported_connections_transaction(pending)?;
    }
    Ok(outcome)
}

fn ssh_config_fields_match(existing: &SavedConnection, resolved: &SavedConnection) -> bool {
    existing.host == resolved.host
        && existing.port == resolved.port
        && existing.username == resolved.username
        && auth_source_matches(&existing.auth, &resolved.auth)
        && existing.options.connect_timeout_seconds == resolved.options.connect_timeout_seconds
        && existing.options.agent_forwarding == resolved.options.agent_forwarding
        && existing.options.identity_agent == resolved.options.identity_agent
        && existing.options.agent_forwarding_socket == resolved.options.agent_forwarding_socket
        && existing.options.x11_forwarding == resolved.options.x11_forwarding
        && remote_command_fields_match(existing, resolved)
        && existing.proxy_chain.len() == resolved.proxy_chain.len()
        && existing
            .proxy_chain
            .iter()
            .zip(&resolved.proxy_chain)
            .all(|(existing, resolved)| {
                existing.host == resolved.host
                    && existing.port == resolved.port
                    && existing.username == resolved.username
                    && auth_source_matches(&existing.auth, &resolved.auth)
                    && existing.agent_forwarding == resolved.agent_forwarding
                    && existing.identity_agent == resolved.identity_agent
                    && existing.agent_forwarding_socket == resolved.agent_forwarding_socket
            })
        && has_proxy_command_tag(&existing.tags) == has_proxy_command_tag(&resolved.tags)
}

fn has_proxy_command_tag(tags: &[String]) -> bool {
    tags.iter().any(|tag| tag == SSH_PROXY_COMMAND_TAG)
}

fn has_remote_command_tag(tags: &[String]) -> bool {
    tags.iter().any(|tag| tag == SSH_REMOTE_COMMAND_TAG)
}

fn remote_command_fields_match(existing: &SavedConnection, resolved: &SavedConnection) -> bool {
    let existing_managed = has_remote_command_tag(&existing.tags);
    let resolved_managed = has_remote_command_tag(&resolved.tags);
    existing_managed == resolved_managed
        && (!resolved_managed || existing.post_connect_command() == resolved.post_connect_command())
}

fn merged_ssh_config_tags(existing: &[String], resolved: &[String]) -> Vec<String> {
    let mut tags = existing
        .iter()
        .filter(|tag| !matches!(tag.as_str(), SSH_PROXY_COMMAND_TAG | SSH_REMOTE_COMMAND_TAG))
        .cloned()
        .collect::<Vec<_>>();
    if has_proxy_command_tag(resolved) {
        tags.push(SSH_PROXY_COMMAND_TAG.to_string());
    }
    if has_remote_command_tag(resolved) {
        tags.push(SSH_REMOTE_COMMAND_TAG.to_string());
    }
    tags
}

fn auth_source_matches(existing: &SavedAuth, resolved: &SavedAuth) -> bool {
    if existing.gssapi_options() != resolved.gssapi_options() {
        return false;
    }
    let existing = existing.conventional_fallback();
    let resolved = resolved.conventional_fallback();
    match (existing, resolved) {
        (SavedAuth::Agent, SavedAuth::Agent)
        | (SavedAuth::KeyboardInteractive, SavedAuth::KeyboardInteractive) => true,
        (
            SavedAuth::Key {
                key_path: existing_path,
                ..
            },
            SavedAuth::Key {
                key_path: resolved_path,
                ..
            },
        ) => existing_path == resolved_path,
        (
            SavedAuth::Certificate {
                key_path: existing_key,
                cert_path: existing_cert,
                ..
            },
            SavedAuth::Certificate {
                key_path: resolved_key,
                cert_path: resolved_cert,
                ..
            },
        ) => existing_key == resolved_key && existing_cert == resolved_cert,
        _ => false,
    }
}
