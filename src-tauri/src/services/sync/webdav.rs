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
        .map_err(|_| AppError::Message("webdav_client_build_failed".into()))
}

/// Parse + validate a WebDAV base URL. Remote endpoints require HTTPS;
/// plaintext HTTP is limited to explicit loopback development endpoints.
fn parse_endpoint(endpoint: &str) -> Result<reqwest::Url, AppError> {
    let trimmed = endpoint.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err(AppError::InvalidInput("endpoint is required".into()));
    }
    let url = reqwest::Url::parse(trimmed)
        .map_err(|_| AppError::InvalidInput("invalid endpoint URL".into()))?;
    let loopback = matches!(
        url.host_str(),
        Some("localhost" | "127.0.0.1" | "::1" | "[::1]")
    );
    if url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !(url.scheme() == "https" || (url.scheme() == "http" && loopback))
    {
        return Err(AppError::InvalidInput(
            "WebDAV endpoint requires HTTPS or explicit loopback HTTP".into(),
        ));
    }
    Ok(url)
}

fn require_encrypted_payload() -> Result<(), AppError> {
    Err(AppError::Message(
        "cloud_sync_encryption_required: database transfer is disabled".into(),
    ))
}

/// Strip a trailing slash from `remote_path`. Empty/None becomes "".
fn normalize_remote_path(remote_path: Option<&str>) -> Result<String, AppError> {
    let normalized = remote_path
        .unwrap_or("")
        .trim()
        .trim_matches('/')
        .to_string();
    if normalized.contains(['\\', '?', '#', '\0'])
        || normalized
            .split('/')
            .any(|segment| matches!(segment, "." | ".."))
    {
        return Err(AppError::InvalidInput("invalid WebDAV remote path".into()));
    }
    Ok(normalized)
}

/// Build the full PUT/GET URL: `{endpoint}/{remote_path}/aag.db`.
fn remote_db_url(endpoint: &str, remote_path: Option<&str>) -> Result<reqwest::Url, AppError> {
    let mut base = parse_endpoint(endpoint)?;
    let rel = normalize_remote_path(remote_path)?;
    {
        let mut segments = base
            .path_segments_mut()
            .map_err(|_| AppError::InvalidInput("invalid WebDAV base URL".into()))?;
        segments.pop_if_empty();
        for segment in rel.split('/').filter(|segment| !segment.is_empty()) {
            segments.push(segment);
        }
        segments.push("aag.db");
    }
    Ok(base)
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
        .map_err(|_| AppError::Message("webdav_head_failed".into()))?;
    let status = resp.status();
    classify_head_status(status)
}

fn classify_head_status(status: StatusCode) -> Result<(), AppError> {
    match status {
        s if s.is_success() || s == StatusCode::NOT_FOUND => Ok(()),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            Err(AppError::Message(format!("auth rejected (HTTP {status})")))
        }
        s if s.is_server_error() => Err(AppError::Message(format!("server error HTTP {s}"))),
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
    require_encrypted_payload()?;
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
        .map_err(|_| AppError::Message("webdav_put_failed".into()))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(AppError::Message(format!("upload rejected: HTTP {status}")));
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
    require_encrypted_payload()?;
    let url = remote_db_url(endpoint, remote_path)?;
    let client = build_client()?;
    let mut req = client.get(url);
    if !username.is_empty() {
        req = req.basic_auth(username, Some(password));
    }
    let resp = req
        .send()
        .await
        .map_err(|_| AppError::Message("webdav_get_failed".into()))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(AppError::Message(format!(
            "download rejected: HTTP {status}"
        )));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|_| AppError::Message("webdav_response_read_failed".into()))?;
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

async fn stamp_outcome<T, E>(db: &Database, result: &Result<T, E>) {
    let target = crate::database::dao::sync_config::SyncTarget::Webdav;
    let err_msg = result.as_ref().err().map(|_| "sync_operation_failed");
    if let Err(stamp_err) = db.record_sync_result(target, err_msg.as_deref()) {
        let _ = stamp_err;
        log::warn!("webdav: sync result stamp failed code=database_error");
    }
}

// ─── Tests ───────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_remote_path_strips_and_drops_empty() {
        assert_eq!(normalize_remote_path(None).unwrap(), "");
        assert_eq!(normalize_remote_path(Some("")).unwrap(), "");
        assert_eq!(normalize_remote_path(Some("/aag/")).unwrap(), "aag");
        assert_eq!(normalize_remote_path(Some("aag/")).unwrap(), "aag");
        assert_eq!(normalize_remote_path(Some("/aag")).unwrap(), "aag");
        assert_eq!(normalize_remote_path(Some("aag/sub")).unwrap(), "aag/sub");
        assert_eq!(normalize_remote_path(Some("  /aag/  ")).unwrap(), "aag");
        for rejected in [
            "../private",
            "aag/../private",
            "aag?token=x",
            "aag#private",
            "aag\\private",
        ] {
            assert!(normalize_remote_path(Some(rejected)).is_err());
        }
    }

    #[test]
    fn parse_endpoint_rejects_empty_or_wrong_scheme() {
        assert!(parse_endpoint("").is_err());
        assert!(parse_endpoint("ftp://example.com/").is_err());
        assert!(parse_endpoint("not a url").is_err());
        assert!(parse_endpoint("https://dav.example.com/").is_ok());
        assert!(parse_endpoint("http://localhost:8080/").is_ok());
        assert!(parse_endpoint("http://dav.example.com/").is_err());
        let authenticated = format!("https://{}:{}@{}/", "user", "secret", "dav.example.com");
        assert!(parse_endpoint(&authenticated).is_err());
        assert!(parse_endpoint("https://dav.example.com/#private").is_err());
        assert!(parse_endpoint("https://dav.example.com/?token=private").is_err());
    }

    #[test]
    fn database_transfer_requires_encrypted_payload_format() {
        assert!(require_encrypted_payload().is_err());
    }

    #[test]
    fn head_accepts_all_success_statuses_and_intended_not_found() {
        for status in [
            StatusCode::OK,
            StatusCode::NO_CONTENT,
            StatusCode::from_u16(207).unwrap(),
            StatusCode::NOT_FOUND,
        ] {
            assert!(classify_head_status(status).is_ok());
        }
        assert!(classify_head_status(StatusCode::UNAUTHORIZED).is_err());
        assert!(classify_head_status(StatusCode::INTERNAL_SERVER_ERROR).is_err());
    }

    #[test]
    fn remote_db_url_builds_expected_paths() {
        // Root endpoint + no remote_path: just /aag.db
        let u = remote_db_url("https://dav.example.com/webdav/", None).unwrap();
        assert_eq!(u.as_str(), "https://dav.example.com/webdav/aag.db");

        // Endpoint with trailing slash + remote path
        let u = remote_db_url("https://dav.example.com/webdav/", Some("/aag/")).unwrap();
        assert_eq!(u.as_str(), "https://dav.example.com/webdav/aag/aag.db");

        // No trailing slash on endpoint
        let u = remote_db_url("https://dav.example.com/webdav", Some("aag/sub")).unwrap();
        assert_eq!(u.as_str(), "https://dav.example.com/webdav/aag/sub/aag.db");
    }

    #[test]
    fn remote_db_url_rejects_bad_endpoint() {
        assert!(remote_db_url("", Some("aag")).is_err());
        assert!(remote_db_url("ftp://x/", Some("aag")).is_err());
    }
}
