use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::sensitive::{PluginHostCallSensitivity, zeroize_json_value};

#[derive(PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginHostCall {
    pub request_id: String,
    pub namespace: String,
    pub method: String,
    #[serde(default)]
    pub args: Value,
}

impl PluginHostCall {
    pub fn sensitivity(&self) -> PluginHostCallSensitivity {
        PluginHostCallSensitivity::classify(&self.namespace, &self.method)
    }

    pub fn zeroize_args(&mut self) {
        // Sensitive handlers call this at their ownership boundary after moving
        // any value needed by the backend out of the JSON object.
        zeroize_json_value(&mut self.args);
    }
}

impl fmt::Debug for PluginHostCall {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut debug = formatter.debug_struct("PluginHostCall");
        debug
            .field("request_id", &self.request_id)
            .field("namespace", &self.namespace)
            .field("method", &self.method);
        if self.sensitivity().is_sensitive() {
            debug.field("args", &"<redacted>");
        } else {
            debug.field("args", &self.args);
        }
        debug.finish()
    }
}
