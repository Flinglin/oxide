use std::collections::BTreeMap;

use serde::Deserialize;

use crate::{InstallFlavor, PlatformTarget, is_update_newer};

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NativeUpdateManifest {
    pub version: String,
    #[serde(default, alias = "body", alias = "notes")]
    pub body: Option<String>,
    #[serde(default, alias = "date", alias = "pub_date")]
    pub date: Option<String>,
    #[serde(default)]
    pub platforms: BTreeMap<String, NativeUpdateAsset>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NativeUpdateAsset {
    pub url: String,
    #[serde(default)]
    pub signature: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeUpdatePackage {
    pub version: String,
    pub current_version: String,
    pub body: Option<String>,
    pub date: Option<String>,
    pub platform_key: String,
    pub url: String,
    pub signature: Option<String>,
}

impl NativeUpdateManifest {
    pub fn select_package(
        &self,
        current_version: &str,
        target: &PlatformTarget,
        install_flavor: InstallFlavor,
    ) -> Option<NativeUpdatePackage> {
        if !is_update_newer(&self.version, current_version) {
            return None;
        }

        let (platform_key, asset) = target
            .candidate_keys(install_flavor)
            .iter()
            .find_map(|key| self.platforms.get_key_value(key))?;

        Some(NativeUpdatePackage {
            version: self.version.clone(),
            current_version: current_version.to_string(),
            body: self.body.clone(),
            date: self.date.clone(),
            platform_key: platform_key.clone(),
            url: asset.url.clone(),
            signature: asset.signature.clone(),
        })
    }
}
