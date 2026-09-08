//! WebDAV HTTP transport for cloud sync (Phase 8 Step 3).
//!
//! Minimal v1 protocol: a single `aag.db` file is PUT/GET against a WebDAV
//! endpoint using HTTP basic auth. No manifest, no versioning, no skills.zip —
//! those land in later iterations alongside the S3 transport.
//!
//! URL shape: `{endpoint}{remote_path}/aag.db`
//! - `endpoint`: full WebDAV URL (e.g. `https://dav.example.com/webdav/`).
//! - `remote_path`: directory under endpoint (e.g. `/aag/`). Empty = root.
//!
//! Reference: cc-switch's `services/webdav.rs` (PUT/GET/HEAD/MKCOL primitives)
//! and `services/webdav_sync.rs` (manifest-based sync). Our v1 is simpler —
//! just enough to round-trip the SQLite file.

use crate::config::get_db_path;
use crate::database::Database;
use crate::error::AppError;
use reqwest::StatusCode;
use std::time::Duration;

const HTTP_TIMEOUT_SECS: u64 = 60;

/// Result of an upload or download attempt. Returned to the UI so it can
/// surface timing + byte counts.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebDavResult {
    pub status: &'static str, // "uploaded" | "downloaded" | "tested"
    pub bytes: u64,
}

/// Build a reqwest client configured with a generous body timeout for big
/// `aag.db` uploads. Connection pool reuses TLS handshakes across calls.
fn build_client() -> Result<reqwest::Client, AppError> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
        .build()
        .map_err(|e| AppError::Message(format!("reqwest client build failed: {e}")))
}

/// Parse + validate a WebDAV base URL. Must be http or https.
fn parse_endpoint(endpoint: &str) -> Result<reqwest::Url, AppError> {
    let trimmed = endpoint.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err(AppError::InvalidInput("endpoint is required".into()));
    }
    let url = reqwest::Url::parse(trimmed)
        .map_err(|e| AppError::InvalidInput(format!("invalid endpoint URL: {e}")))?;
    match url.scheme() {
        "http" | "https" => Ok(url),
        other => Err(AppError::InvalidInput(format!(
            "endpoint scheme must be http or https, got \"{other}\""
        ))),
    }
}

/// Strip a trailing slash from `remote_path`. Empty/None becomes "".
fn normalize_remote_path(remote_path: Option<&str>) -> String {
    remote_path
        .unwrap_or("")
        .trim()
        .trim_matches('/')
        .to_string()
}

/// Build the full PUT/GET URL: `{endpoint}/{remote_path}/aag.db`.
fn remote_db_url(endpoint: &str, remote_path: Option<&str>) -> Result<reqwest::Url, AppError> {
    let base = parse_endpoint(endpoint)?;
    let rel = normalize_remote_path(remote_path);
    let joined = if rel.is_empty() {
        format!("{}/aag.db", base)
    } else {
        format!("{}/{}/aag.db", base, rel)
    };
    reqwest::Url::parse(&joined)
        .map_err(|e| AppError::InvalidInput(format!("remote URL build failed: {e}")))
}

/// HEAD the remote `aag.db` to confirm the endpoint + credentials are valid.
/// Returns `Ok(())` on any 2xx/4xx-with-expectable-response (a 404 still means
/// the auth worked — the file just doesn't exist yet). Returns `Err` on 5xx,
/// connection refused, DNS failure, or auth-rejected 401/403.
pub async fn check_connection(
    endpoint: &str,
    username: &str,
    password: &str,
    remote_path: Option<&str>,
) -> Result<(), AppError> {
    let url = remote_db_url(endpoint, remote_path)?;
    let client = build_client()?;
    let mut req = client.head(url);
    if !username.is_empty() {
        req = req.basic_auth(username, Some(password));
    }
    let resp = req
        .send()
        .await
        .map_err(|e| AppError::Message(format!("HEAD failed: {e}")))?;
    let status = resp.status();
    match status {
        StatusCode::OK | StatusCode::NOT_FOUND => Ok(()),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(AppError::Message(format!(
            "auth rejected (HTTP {status})"
        ))),
        s if s.is_server_error() => Err(AppError::Message(format!(
            "server error HTTP {s}"
        ))),
        s => Err(AppError::Message(format!(
            "unexpected HTTP {s} from HEAD request"
        ))),
    }
}

/// PUT the local `aag.db` to the remote URL.
pub async fn upload(
    endpoint: &str,
    username: &str,
    password: &str,
    remote_path: Option<&str>,
) -> Result<WebDavResult, AppError> {
    let url = remote_db_url(endpoint, remote_path)?;
    let local_path = get_db_path();
    let bytes = std::fs::read(&local_path).map_err(|e| AppError::io(&local_path, e))?;
    let size = bytes.len() as u64;

    let client = build_client()?;
    let mut req = client.put(url).body(bytes);
    if !username.is_empty() {
        req = req.basic_auth(username, Some(password));
    }
    let resp = req
        .send()
        .await
        .map_err(|e| AppError::Message(format!("PUT failed: {e}")))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(AppError::Message(format!(
            "upload rejected: HTTP {status}"
        )));
    }
    Ok(WebDavResult {
        status: "uploaded",
        bytes: size,
    })
}

/// GET the remote `aag.db` and atomically replace the local file.
///
/// Safety: we download to `<db>.tmp.<pid>.<n>` then rename. If anything fails
/// the local DB is untouched. Note that this REPLACES the entire AAG database,
/// so all current config is lost — the UI should warn before invoking.
pub async fn download(
    endpoint: &str,
    username: &str,
    password: &str,
    remote_path: Option<&str>,
) -> Result<WebDavResult, AppError> {
    let url = remote_db_url(endpoint, remote_path)?;
    let client = build_client()?;
    let mut req = client.get(url);
    if !username.is_empty() {
        req = req.basic_auth(username, Some(password));
    }
    let resp = req
        .send()
        .await
        .map_err(|e| AppError::Message(format!("GET failed: {e}")))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(AppError::Message(format!(
            "download rejected: HTTP {status}"
        )));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| AppError::Message(format!("read response body: {e}")))?;
    let size = bytes.len() as u64;
    if size == 0 {
        return Err(AppError::Message("remote file is empty".into()));
    }

    // Write to tmp, then rename. Caller is responsible for restarting the
    // app (or re-opening the DB) so the new file takes effect.
    let target = get_db_path();
    let tmp = target.with_extension(format!(
        "db.tmp.{}.{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
    ));
    std::fs::write(&tmp, &bytes).map_err(|e| AppError::io(&tmp, e))?;
    std::fs::rename(&tmp, &target).map_err(|e| AppError::io(&target, e))?;
    Ok(WebDavResult {
        status: "downloaded",
        bytes: size,
    })
}

/// Bundle an upload + download_result-stamping into one helper. After the
/// HTTP call returns, call `db.record_sync_result(SyncTarget::Webdav, err)`
/// to surface the outcome in the UI's status section.
pub async fn upload_and_stamp(
    db: &Database,
    endpoint: &str,
    username: &str,
    password: &str,
    remote_path: Option<&str>,
) -> Result<WebDavResult, AppError> {
    let result = upload(endpoint, username, password, remote_path).await;
    stamp_outcome(db, &result).await;
    result
}

pub async fn download_and_stamp(
    db: &Database,
    endpoint: &str,
    username: &str,
    password: &str,
    remote_path: Option<&str>,
) -> Result<WebDavResult, AppError> {
    let result = download(endpoint, username, password, remote_path).await;
    stamp_outcome(db, &result).await;
    result
}

pub async fn check_and_stamp(
    db: &Database,
    endpoint: &str,
    username: &str,
    password: &str,
    remote_path: Option<&str>,
) -> Result<(), AppError> {
    let result = check_connection(endpoint, username, password, remote_path).await;
    stamp_outcome(db, &result).await;
    result
}

async fn stamp_outcome<T, E: std::fmt::Display>(
    db: &Database,
    result: &Result<T, E>,
) {
    let target = crate::database::dao::sync_config::SyncTarget::Webdav;
    let err_msg = match result {
        Ok(_) => None,
        Err(e) => Some(e.to_string()),
    };
    if let Err(stamp_err) = db.record_sync_result(target, err_msg.as_deref()) {
        log::warn!("webdav: failed to stamp sync result: {stamp_err}");
    }
}

// ─── Tests ───────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_remote_path_strips_and_drops_empty() {
        assert_eq!(normalize_remote_path(None), "");
        assert_eq!(normalize_remote_path(Some("")), "");
        assert_eq!(normalize_remote_path(Some("/aag/")), "aag");
        assert_eq!(normalize_remote_path(Some("aag/")), "aag");
        assert_eq!(normalize_remote_path(Some("/aag")), "aag");
        assert_eq!(normalize_remote_path(Some("aag/sub")), "aag/sub");
        assert_eq!(normalize_remote_path(Some("  /aag/  ")), "aag");
    }

    #[test]
    fn parse_endpoint_rejects_empty_or_wrong_scheme() {
        assert!(parse_endpoint("").is_err());
        assert!(parse_endpoint("ftp://example.com/").is_err());
        assert!(parse_endpoint("not a url").is_err());
        assert!(parse_endpoint("https://dav.example.com/").is_ok());
        assert!(parse_endpoint("http://localhost:8080/").is_ok());
    }

    #[test]
    fn remote_db_url_builds_expected_paths() {
        // Root endpoint + no remote_path: just /aag.db
        let u = remote_db_url("https://dav.example.com/webdav/", None).unwrap();
        assert_eq!(u.as_str(), "https://dav.example.com/webdav/aag.db");

        // Endpoint with trailing slash + remote path
        let u = remote_db_url(
            "https://dav.example.com/webdav/",
            Some("/aag/"),
        )
        .unwrap();
        assert_eq!(u.as_str(), "https://dav.example.com/webdav/aag/aag.db");

        // No trailing slash on endpoint
        let u =
            remote_db_url("https://dav.example.com/webdav", Some("aag/sub")).unwrap();
        assert_eq!(
            u.as_str(),
            "https://dav.example.com/webdav/aag/sub/aag.db"
        );
    }

    #[test]
    fn remote_db_url_rejects_bad_endpoint() {
        assert!(remote_db_url("", Some("aag")).is_err());
        assert!(remote_db_url("ftp://x/", Some("aag")).is_err());
    }
}
