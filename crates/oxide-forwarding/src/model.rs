use serde::{Deserialize, Serialize};

const DEFAULT_FORWARD_HOST: &str = "localhost";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ForwardType {
    Local,
    Remote,
    Dynamic,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ForwardStatus {
    Starting,
    Active,
    Stopped,
    Error,
    Suspended,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForwardRule {
    pub id: String,
    pub forward_type: ForwardType,
    pub bind_address: String,
    pub bind_port: u16,
    pub target_host: String,
    pub target_port: u16,
    pub status: ForwardStatus,
    pub description: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForwardStats {
    pub connection_count: u64,
    pub active_connections: u64,
    pub bytes_sent: u64,
    pub bytes_received: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ForwardUpdate {
    pub forward_type: Option<ForwardType>,
    pub bind_address: Option<String>,
    pub bind_port: Option<u16>,
    pub target_host: Option<String>,
    pub target_port: Option<u16>,
    pub description: Option<String>,
}

fn normalize_forward_host(host: impl Into<String>) -> String {
    let host = host.into();
    let trimmed = host.trim();
    if trimmed.is_empty() {
        DEFAULT_FORWARD_HOST.to_string()
    } else {
        trimmed.to_string()
    }
}

impl ForwardRule {
    pub fn local(
        bind_address: impl Into<String>,
        bind_port: u16,
        target_host: impl Into<String>,
        target_port: u16,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            forward_type: ForwardType::Local,
            bind_address: normalize_forward_host(bind_address),
            bind_port,
            target_host: normalize_forward_host(target_host),
            target_port,
            status: ForwardStatus::Starting,
            description: String::new(),
        }
    }

    pub fn remote(
        bind_address: impl Into<String>,
        bind_port: u16,
        target_host: impl Into<String>,
        target_port: u16,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            forward_type: ForwardType::Remote,
            bind_address: normalize_forward_host(bind_address),
            bind_port,
            target_host: normalize_forward_host(target_host),
            target_port,
            status: ForwardStatus::Starting,
            description: String::new(),
        }
    }

    pub fn dynamic(bind_address: impl Into<String>, bind_port: u16) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            forward_type: ForwardType::Dynamic,
            bind_address: normalize_forward_host(bind_address),
            bind_port,
            target_host: String::new(),
            target_port: 0,
            status: ForwardStatus::Starting,
            description: "SOCKS5 Proxy".to_string(),
        }
    }

    pub fn apply_update(&mut self, update: ForwardUpdate) {
        if let Some(forward_type) = update.forward_type {
            self.forward_type = forward_type;
        }
        if let Some(bind_address) = update.bind_address {
            self.bind_address = normalize_forward_host(bind_address);
        }
        if let Some(bind_port) = update.bind_port {
            self.bind_port = bind_port;
        }
        if let Some(target_host) = update.target_host {
            self.target_host = normalize_forward_host(target_host);
        }
        if let Some(target_port) = update.target_port {
            self.target_port = target_port;
        }
        if let Some(description) = update.description {
            self.description = description;
        }
    }

    #[cfg(any(feature = "runtime", test))]
    pub(crate) fn normalize_hosts_for_runtime(&mut self) {
        self.bind_address = normalize_forward_host(std::mem::take(&mut self.bind_address));
        if self.forward_type != ForwardType::Dynamic {
            // SOCKS dynamic forwards pick the target per connection, so only
            // non-dynamic rules inherit Tauri's blank target host fallback.
            self.target_host = normalize_forward_host(std::mem::take(&mut self.target_host));
        }
    }
}
