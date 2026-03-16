use std::{
    fs,
    hash::{Hash, Hasher},
    path::PathBuf,
    time::Duration,
};

use anyhow::{Context, Result};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tracing::{debug, warn};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct CacheEnvelope {
    saved_at_unix: u64,
    value: Value,
}

pub(crate) fn cache_root_dir() -> PathBuf {
    if let Some(custom) = crate::first_non_empty_env(&["ARC_CACHE_DIR"]) {
        return PathBuf::from(custom);
    }
    PathBuf::from("cache")
}

fn cache_file_path(namespace: &str, key: &str) -> PathBuf {
    cache_root_dir()
        .join(namespace)
        .join(format!("{}.json", short_hash(key)))
}

pub(crate) fn short_hash(value: &str) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

pub(crate) fn read_cache_typed<T>(
    namespace: &str,
    key: &str,
    max_age: Option<Duration>,
) -> Option<T>
where
    T: DeserializeOwned,
{
    let path = cache_file_path(namespace, key);
    let content = fs::read_to_string(path).ok()?;
    let envelope: CacheEnvelope = serde_json::from_str(&content).ok()?;

    if let Some(max_age) = max_age {
        let now = crate::now_unix_seconds();
        if now.saturating_sub(envelope.saved_at_unix) > max_age.as_secs() {
            return None;
        }
    }

    serde_json::from_value(envelope.value).ok()
}

pub(crate) fn write_cache_typed<T>(namespace: &str, key: &str, value: &T)
where
    T: Serialize,
{
    let value = match serde_json::to_value(value) {
        Ok(value) => value,
        Err(err) => {
            warn!(error = %err, namespace, "cache_write: failed to serialize value");
            return;
        }
    };
    let envelope = CacheEnvelope {
        saved_at_unix: crate::now_unix_seconds(),
        value,
    };
    let output = match serde_json::to_string(&envelope) {
        Ok(output) => output,
        Err(err) => {
            warn!(error = %err, namespace, "cache_write: failed to encode envelope");
            return;
        }
    };

    let path = cache_file_path(namespace, key);
    if let Some(parent) = path.parent()
        && let Err(err) = fs::create_dir_all(parent)
    {
        warn!(error = %err, "cache_write: failed to create parent directory");
        return;
    }
    if let Err(err) = fs::write(path, output) {
        warn!(error = %err, "cache_write: failed to write cache file");
    }
}

fn tracked_state_path() -> PathBuf {
    cache_root_dir().join(crate::CACHE_FILE_TRACKED_STATE)
}

pub(crate) fn load_tracked_state() -> Option<crate::PersistedTrackedState> {
    let path = tracked_state_path();
    let content = fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

pub(crate) fn save_tracked_state(state: &crate::PersistedTrackedState) -> Result<()> {
    let path = tracked_state_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create '{}'", parent.display()))?;
    }
    let content = serde_json::to_string(state).context("failed to encode tracked state")?;
    fs::write(&path, content).with_context(|| format!("failed to write '{}'", path.display()))
}

pub(crate) async fn get_json_cached<T>(
    request: reqwest::RequestBuilder,
    namespace: &str,
    cache_key: &str,
    ttl: Duration,
) -> Result<T>
where
    T: DeserializeOwned,
{
    if let Some(cached_value) = read_cache_typed::<Value>(namespace, cache_key, Some(ttl)) {
        debug!(namespace, cache_key, "get_json_cached: cache hit");
        return serde_json::from_value(cached_value)
            .context("failed to decode cached JSON payload");
    }

    let fetched_value: Value = crate::get_json(request).await?;
    write_cache_typed(namespace, cache_key, &fetched_value);
    serde_json::from_value(fetched_value).context("failed to decode fetched JSON payload")
}

fn image_cache_file_path(url: &str) -> PathBuf {
    cache_root_dir()
        .join(crate::CACHE_NAMESPACE_IMAGES)
        .join(format!("{}.bin", short_hash(url)))
}

pub(crate) fn read_cached_remote_image_data_uri(url: &str) -> Option<String> {
    let path = image_cache_file_path(url);
    let bytes = fs::read(path).ok()?;
    let mime = crate::image_mime_type(url);
    Some(format!("data:{mime};base64,{}", BASE64.encode(bytes)))
}

pub(crate) fn write_cached_remote_image(url: &str, bytes: &[u8]) {
    let path = image_cache_file_path(url);
    if let Some(parent) = path.parent()
        && let Err(err) = fs::create_dir_all(parent)
    {
        warn!(error = %err, "image_cache: failed to create cache directory");
        return;
    }
    if let Err(err) = fs::write(path, bytes) {
        warn!(error = %err, "image_cache: failed to write image");
    }
}
