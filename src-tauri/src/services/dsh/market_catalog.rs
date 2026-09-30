//! Read-only marketplace catalog and explicit bounded GitHub refresh.
//! Registry metadata is not a security audit or authority to install a package.
#![allow(non_camel_case_types)]

#[path = "market_normalize.rs"]
mod normalize;
#[cfg(test)]
#[path = "market_tests.rs"]
mod tests;

use crate::error::AppError;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    path::Path,
    sync::OnceLock,
    time::Duration,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Market_privacy_scrub {
    pub schema_version: u32,
    pub fields: Vec<String>,
    pub replacement_tokens: BTreeMap<String, String>,
    pub source_entry_count: usize,
    pub modified_entries: usize,
    pub replacements: BTreeMap<String, usize>,
    pub total_replacements: usize,
}

pub const SOURCE_REGISTRY: &str = "YELEBAI/dsh-plugin-marketplace";
pub const MAX_FEED_BYTES: usize = 24 * 1024 * 1024;
pub const MAX_CATALOG_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_ENTRIES: usize = 20_000;
const REFRESH_TIMEOUT_SECS: u64 = 30;
const CACHE_FILE: &str = "catalog.json";
const BUNDLED_CATALOG: &str = include_str!("../../../../shared/dsh_market_catalog.json");
static REFRESH_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Market_entry {
    pub id: String,
    pub name: String,
    pub full_name: String,
    pub description: String,
    pub description_en: Option<String>,
    pub description_zh: Option<String>,
    pub repo_url: String,
    pub source_commit: Option<String>,
    pub source_commit_url: Option<String>,
    pub package_name: Option<String>,
    pub version: Option<String>,
    pub install_method: String,
    pub install_spec: Option<String>,
    pub categories: Vec<String>,
    pub use_cases: Vec<String>,
    pub modules: Vec<String>,
    pub classification_basis: String,
    pub author: String,
    pub license: Option<String>,
    pub stars: u64,
    pub updated_at: Option<String>,
    pub has_client: bool,
    pub verification: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Market_catalog {
    pub schema_version: u32,
    pub fetched_at: String,
    pub registry_generated_at: String,
    pub source_registry: String,
    pub registry_commit: String,
    pub registry_url: String,
    pub collector_url: String,
    pub entry_count: usize,
    pub privacy_scrub: Market_privacy_scrub,
    pub entries: Vec<Market_entry>,
    pub cache_status: String,
    pub warning: Option<String>,
}

fn failure(code: &str) -> AppError {
    AppError::Message(format!("dsh_market:{code}"))
}

/// The root is the dedicated AAG catalog-cache directory. Reads never create it.
pub fn get_catalog(root: &Path) -> Result<Market_catalog, AppError> {
    let cache_path = root.join(CACHE_FILE);
    let read_result = (|| {
        super::config::safe_path(&cache_path)?;
        let file = match fs::File::open(&cache_path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(failure("cache_unreadable")),
        };
        let mut data = Vec::new();
        file.take(MAX_CATALOG_BYTES as u64 + 1)
            .read_to_end(&mut data)
            .map_err(|_| failure("cache_unreadable"))?;
        let mut catalog = normalize::parse_catalog(&data)?;
        catalog.cache_status = "cached".into();
        catalog.warning = None;
        Ok(Some(catalog))
    })();
    match read_result {
        Ok(Some(catalog)) => Ok(catalog),
        result => {
            let mut catalog = normalize::parse_catalog(BUNDLED_CATALOG.as_bytes())?;
            catalog.cache_status = "bundled".into();
            catalog.warning = if result.is_err() {
                Some("cache_invalid_using_bundled".into())
            } else {
                None
            };
            Ok(catalog)
        }
    }
}

/// Two credential-free, allowlisted GETs. Failure leaves the existing cache intact.
/// The caller should retain its current view and display this command's error.
pub async fn refresh_catalog(root: &Path) -> Result<Market_catalog, AppError> {
    let _guard = REFRESH_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .try_lock()
        .map_err(|_| failure("refresh_busy"))?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(REFRESH_TIMEOUT_SECS))
        .user_agent("aag-dsh-market-catalog")
        .build()
        .map_err(|_| failure("http_client_failed"))?;
    let mut catalog = tokio::time::timeout(Duration::from_secs(REFRESH_TIMEOUT_SECS), async {
        let commit_url = format!("https://api.github.com/repos/{SOURCE_REGISTRY}/commits/main");
        let commit_data = fetch_bounded(&client, &commit_url, 2 * 1024 * 1024).await?;
        let value: serde_json::Value =
            serde_json::from_slice(&commit_data).map_err(|_| failure("invalid_commit_response"))?;
        let commit = value
            .get("sha")
            .and_then(|value| value.as_str())
            .filter(|value| normalize::valid_sha(value))
            .ok_or_else(|| failure("invalid_commit_response"))?;
        let feed_url = format!(
            "https://raw.githubusercontent.com/{SOURCE_REGISTRY}/{commit}/registry/plugins.json"
        );
        let feed = fetch_bounded(&client, &feed_url, MAX_FEED_BYTES).await?;
        normalize::normalize_feed(
            &feed,
            commit,
            &chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        )
    })
    .await
    .map_err(|_| failure("refresh_timeout"))??;
    catalog.cache_status = "refreshed".into();
    replace_cache(root, &catalog)?;
    Ok(catalog)
}

fn fetch_url_allowed(value: &str) -> bool {
    if value == format!("https://api.github.com/repos/{SOURCE_REGISTRY}/commits/main") {
        return true;
    }
    let prefix = format!("https://raw.githubusercontent.com/{SOURCE_REGISTRY}/");
    value
        .strip_prefix(&prefix)
        .and_then(|remaining| remaining.strip_suffix("/registry/plugins.json"))
        .map(normalize::valid_sha)
        .unwrap_or(false)
}

async fn fetch_bounded(
    client: &reqwest::Client,
    target: &str,
    limit: usize,
) -> Result<Vec<u8>, AppError> {
    if !fetch_url_allowed(target) {
        return Err(failure("fetch_url_rejected"));
    }
    let response = client
        .get(target)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|_| failure("registry_unreachable"))?;
    if !response.status().is_success() {
        return Err(failure(
            if [403, 429].contains(&response.status().as_u16()) {
                "registry_rate_limited"
            } else {
                "registry_http_error"
            },
        ));
    }
    if response
        .content_length()
        .map(|length| length > limit as u64)
        .unwrap_or(false)
    {
        return Err(failure("feed_too_large"));
    }
    let mut result = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| failure("registry_read_failed"))?;
        if result.len().saturating_add(chunk.len()) > limit {
            return Err(failure("feed_too_large"));
        }
        result.extend_from_slice(&chunk);
    }
    Ok(result)
}

fn replace_cache(root: &Path, catalog: &Market_catalog) -> Result<(), AppError> {
    replace_cache_with(root, catalog, |temporary, target| {
        fs::rename(temporary, target)
    })
}

fn replace_cache_with<F>(
    root: &Path,
    catalog: &Market_catalog,
    rename_file: F,
) -> Result<(), AppError>
where
    F: FnOnce(&Path, &Path) -> std::io::Result<()>,
{
    let encoded = serde_json::to_vec(catalog).map_err(|_| failure("catalog_encode_failed"))?;
    normalize::parse_catalog(&encoded)?;
    let target = root.join(CACHE_FILE);
    super::config::safe_path(&target)?;
    fs::create_dir_all(root).map_err(|_| failure("cache_directory_failed"))?;
    let mut random = [0u8; 16];
    getrandom::getrandom(&mut random).map_err(|_| failure("cache_id_failed"))?;
    let temporary = root.join(format!("catalog_{}.tmp", hex::encode(random)));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|_| failure("cache_create_failed"))?;
        file.write_all(&encoded)
            .and_then(|_| file.sync_all())
            .map_err(|_| failure("cache_write_failed"))?;
        drop(file);
        // rename is atomic and replaces a file on Windows and Unix. Never unlink the old catalog first.
        rename_file(&temporary, &target).map_err(|_| failure("cache_replace_failed"))
    })();
    if temporary.exists() {
        fs::remove_file(&temporary).map_err(|_| failure("cache_cleanup_failed"))?;
    }
    result
}

/// Resolve source links from the validated catalog; arbitrary caller URLs are not accepted.
pub fn source_url(
    catalog: &Market_catalog,
    entry_id: &str,
    source_kind: &str,
) -> Result<String, AppError> {
    normalize::validate_catalog(catalog)?;
    match source_kind {
        "registry" => Ok(catalog.registry_url.clone()),
        "collector" => Ok(catalog.collector_url.clone()),
        "repository" | "commit" => {
            let entry = catalog
                .entries
                .iter()
                .find(|entry| entry.id == entry_id)
                .ok_or_else(|| failure("entry_not_found"))?;
            if source_kind == "repository" {
                Ok(entry.repo_url.clone())
            } else {
                entry
                    .source_commit_url
                    .clone()
                    .ok_or_else(|| failure("source_commit_unknown"))
            }
        }
        _ => Err(failure("source_kind_rejected")),
    }
}
