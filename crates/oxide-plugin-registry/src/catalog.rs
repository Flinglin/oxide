use super::*;
use futures_util::{StreamExt, stream};

const HISTORY_MAX_BYTES: u64 = 8 * 1024 * 1024;

pub(super) enum MetadataError {
    Status(u16),
    Invalid,
    Transport,
    TooLarge,
}
impl std::fmt::Display for MetadataError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Status(status) => write!(f, "Plugin catalog returned HTTP {status}"),
            Self::Invalid => f.write_str("Invalid plugin catalog URL"),
            Self::Transport => f.write_str("Cannot download plugin catalog metadata"),
            Self::TooLarge => f.write_str("Plugin catalog metadata exceeds its size limit"),
        }
    }
}

pub(super) async fn download_metadata(url: &str, limit: u64) -> Result<Vec<u8>, MetadataError> {
    validate_native_plugin_package_url(url).map_err(|_| MetadataError::Invalid)?;
    let client =
        oxide_network_proxy::application_http_client().map_err(|_| MetadataError::Transport)?;
    let mut response = client
        .get(url)
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
        .map_err(|_| MetadataError::Transport)?;
    if !response.status().is_success() {
        return Err(MetadataError::Status(response.status().as_u16()));
    }
    if response.content_length().is_some_and(|size| size > limit) {
        return Err(MetadataError::TooLarge);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| MetadataError::Transport)?
    {
        if bytes.len() as u64 + chunk.len() as u64 > limit {
            return Err(MetadataError::TooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

pub(super) fn validate_history_reference(
    history: &NativePluginRegistryHistory,
) -> Result<(), String> {
    let url =
        reqwest::Url::parse(&history.download_url).map_err(|_| "Invalid catalog history URL")?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || history.size == 0
        || history.size > HISTORY_MAX_BYTES
        || history.checksum.strip_prefix("sha256:").is_none_or(|hash| {
            hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
    {
        return Err("Invalid catalog history reference".into());
    }
    Ok(())
}

pub(super) fn validate_history_origins(
    url: &str,
    index: &NativePluginRegistryIndex,
) -> Result<(), String> {
    let origin = reqwest::Url::parse(url)
        .map_err(|_| "Invalid catalog URL")?
        .origin();
    for entry in &index.plugins {
        if let Some(history) = &entry.history {
            let destination = reqwest::Url::parse(&history.download_url)
                .map_err(|_| "Invalid catalog history URL")?;
            if destination.origin() != origin {
                return Err("Catalog histories must share the catalog origin".into());
            }
        }
    }
    Ok(())
}

fn history_path(settings: &Path, entry: &NativePluginRegistryEntry) -> Result<PathBuf, String> {
    validate_native_plugin_id(&entry.id)?;
    let history = entry
        .history
        .as_ref()
        .ok_or("Catalog history reference missing")?;
    validate_history_reference(history)?;
    let hash = history
        .checksum
        .strip_prefix("sha256:")
        .expect("validated checksum");
    Ok(settings
        .parent()
        .unwrap_or(settings)
        .join("plugin-catalog-histories")
        .join(&entry.id)
        .join(format!("{hash}.json")))
}

fn decode_history(
    summary: &NativePluginRegistryEntry,
    bytes: &[u8],
) -> Result<NativePluginRegistryEntry, String> {
    let reference = summary
        .history
        .as_ref()
        .ok_or("Catalog history reference missing")?;
    validate_history_reference(reference)?;
    if bytes.len() as u64 != reference.size
        || format!("sha256:{:x}", Sha256::digest(bytes)) != reference.checksum.to_ascii_lowercase()
    {
        return Err("Catalog history checksum or size differs".into());
    }
    let mut entry: NativePluginRegistryEntry =
        serde_json::from_slice(bytes).map_err(|_| "Invalid catalog history JSON")?;
    if entry.id != summary.id || entry.history.is_some() {
        return Err("Catalog history identity differs".into());
    }
    registry::validate_native_plugin_registry(&NativePluginRegistryIndex {
        version: 1,
        plugins: vec![entry.clone()],
    })?;
    let latest = entry
        .releases
        .iter()
        .filter_map(|release| {
            semver::Version::parse(&release.version)
                .ok()
                .map(|version| (version, release))
        })
        .max_by(|left, right| left.0.cmp(&right.0))
        .ok_or("Catalog history has no releases")?;
    if latest.1.version != summary.version
        || Some(latest.1.effective_engines()) != summary.engines.as_ref()
        || entry.language != summary.language
        || entry.listed_at != summary.listed_at
        || entry.latest_release_at != summary.latest_release_at
    {
        return Err("Catalog summary and history differ".into());
    }
    entry.history = Some(reference.clone());
    Ok(entry)
}

pub(super) fn load_cached_history(
    settings: &Path,
    summary: &NativePluginRegistryEntry,
) -> Result<Option<NativePluginRegistryEntry>, String> {
    let file = history_path(settings, summary)?;
    match fs::File::open(file) {
        Ok(file) => {
            use std::io::Read;
            let mut bytes = Vec::new();
            file.take(HISTORY_MAX_BYTES + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| "Cannot read catalog history cache")?;
            decode_history(summary, &bytes).map(Some)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err("Cannot open catalog history cache".into()),
    }
}

pub(super) fn summary(entry: &NativePluginRegistryEntry) -> NativePluginRegistryEntry {
    let mut summary = entry.clone();
    if summary.history.is_none() {
        return summary;
    }
    if let Some(latest) = entry.releases.iter().max_by(|a, b| {
        semver::Version::parse(&a.version)
            .ok()
            .cmp(&semver::Version::parse(&b.version).ok())
    }) {
        summary.version = latest.version.clone();
        summary.engines = Some(latest.effective_engines().clone());
    }
    summary.packages.clear();
    summary.releases.clear();
    summary.download_url.clear();
    summary.checksum = None;
    summary.size = None;
    summary.min_oxide_version = None;
    summary
}

pub(super) fn summaries(index: &NativePluginRegistryIndex) -> NativePluginRegistryIndex {
    NativePluginRegistryIndex {
        version: index.version,
        plugins: index.plugins.iter().map(summary).collect(),
    }
}

impl NativePluginRegistry {
    pub fn registry_summary(entry: &NativePluginRegistryEntry) -> NativePluginRegistryEntry {
        summary(entry)
    }

    pub async fn fetch_registry_history(
        settings: &Path,
        summary: &NativePluginRegistryEntry,
    ) -> Result<NativePluginRegistryEntry, String> {
        if !summary.history_pending() {
            return Ok(summary.clone());
        }
        if let Ok(Some(entry)) = load_cached_history(settings, summary) {
            return Ok(entry);
        }
        let reference = summary.history.as_ref().expect("pending history reference");
        let bytes = download_metadata(&reference.download_url, reference.size)
            .await
            .map_err(|error| error.to_string())?;
        let entry = decode_history(summary, &bytes)?;
        let file = history_path(settings, summary)?;
        fs::create_dir_all(file.parent().expect("history directory"))
            .map_err(|_| "Cannot create catalog cache directory")?;
        oxide_atomic_file::durable_write(&file, &bytes)
            .map_err(|_| "Cannot save catalog history cache")?;
        Ok(entry)
    }

    pub async fn hydrate_registry_histories(
        settings: &Path,
        index: &mut NativePluginRegistryIndex,
        ids: &[String],
    ) -> Result<(), String> {
        let requested = index
            .plugins
            .iter()
            .filter(|entry| ids.contains(&entry.id) && entry.history_pending())
            .cloned()
            .collect::<Vec<_>>();
        for (_, result) in Self::fetch_registry_histories(settings, &requested).await {
            let entry = result?;
            if let Some(summary) = index
                .plugins
                .iter_mut()
                .find(|summary| summary.id == entry.id)
            {
                *summary = entry;
            }
        }
        Ok(())
    }

    pub async fn fetch_registry_histories(
        settings: &Path,
        entries: &[NativePluginRegistryEntry],
    ) -> Vec<(String, Result<NativePluginRegistryEntry, String>)> {
        let entries = entries.to_vec();
        let settings = settings.to_path_buf();
        stream::iter(entries)
            .map(move |entry| {
                let settings = settings.clone();
                async move {
                    (
                        entry.id.clone(),
                        Self::fetch_registry_history(&settings, &entry).await,
                    )
                }
            })
            .buffer_unordered(4)
            .collect()
            .await
    }
}
