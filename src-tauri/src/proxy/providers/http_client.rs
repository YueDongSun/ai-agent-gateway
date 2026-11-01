//! Shared HTTP client construction for upstream providers.

use futures_util::StreamExt;
use std::time::Duration;

const POOL_IDLE_TIMEOUT_SECS: u64 = 90;
const TCP_KEEPALIVE_SECS: u64 = 60;
const POOL_MAX_IDLE_PER_HOST: usize = 4;

/// Upper bound for upstream error-body buffering. Error bodies are read only
/// for classification (the classifier's first-8-KiB window plus the compat
/// substring scans) and for the recorded size, and the inbound request-size
/// limit does not apply to upstream responses — so a hostile or misconfigured
/// upstream returning a giant 4xx/5xx page must never be buffered whole.
/// 64 KiB comfortably covers every realistic error message.
pub(crate) const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;

#[derive(Default)]
struct CapturedProbeResponse {
    text: Option<String>,
    truncated: bool,
    redacted: bool,
}

/// This slot is empty for normal provider instances. It never implements Debug,
/// Serialize, or logging; only the dedicated screening caller can take its text.
#[derive(Default)]
pub(crate) struct ProbeResponseCapture(std::sync::Mutex<CapturedProbeResponse>);

impl ProbeResponseCapture {
    pub(crate) fn clear(&self) {
        if let Ok(mut state) = self.0.lock() {
            *state = CapturedProbeResponse::default();
        }
    }
    pub(crate) fn take(&self) -> Option<String> {
        self.0.lock().ok().and_then(|mut state| state.text.take())
    }
    pub(crate) fn flags(&self) -> (bool, bool) {
        self.0
            .lock()
            .map(|state| (state.truncated, state.redacted))
            .unwrap_or((false, false))
    }
    pub(crate) fn record(
        &self,
        bytes: &[u8],
        limit: Option<usize>,
        api_key: &str,
        headers: &std::collections::HashMap<String, String>,
        source_truncated: bool,
    ) {
        let Some(limit) = limit else {
            return;
        };
        let raw = String::from_utf8_lossy(bytes);
        let mut secrets: Vec<String> = std::iter::once(api_key)
            .chain(headers.values().map(String::as_str))
            .filter(|value| !value.is_empty())
            .flat_map(|value| {
                let encoded = serde_json::to_string(value).unwrap_or_default();
                vec![
                    value.to_owned(),
                    encoded
                        .strip_prefix('"')
                        .and_then(|value| value.strip_suffix('"'))
                        .unwrap_or(value)
                        .to_owned(),
                ]
            })
            .collect();
        secrets.sort_by_key(|value| std::cmp::Reverse(value.len()));
        let valid_json = serde_json::from_str::<serde::de::IgnoredAny>(&raw).is_ok();
        // The transport cap can cut an otherwise valid JSON document. Redact
        // complete string tokens in that prefix as well, before literal fallback.
        let mut text = redact_json_strings(&raw, &secrets).unwrap_or_else(|| raw.to_string());
        if !valid_json {
            for secret in &secrets {
                text = text.replace(secret.as_str(), "[redacted]");
                // An already capped HTTP error may end halfway through a secret.
                // Redact that suffix before this instance's smaller capture cap.
                if source_truncated {
                    if let Some(prefix) = secret
                        .char_indices()
                        .map(|(index, _)| index)
                        .rev()
                        .filter(|index| *index > 0)
                        .find(|index| text.ends_with(&secret[..*index]))
                    {
                        text.truncate(text.len() - prefix);
                        text.push_str("[redacted]");
                    }
                }
            }
        }
        let redacted = text.as_str() != raw.as_ref();
        let truncated = source_truncated || text.len() > limit;
        if text.len() > limit {
            let mut boundary = limit;
            while !text.is_char_boundary(boundary) {
                boundary -= 1;
            }
            text.truncate(boundary);
        }
        if let Ok(mut state) = self.0.lock() {
            *state = CapturedProbeResponse {
                text: Some(text),
                truncated,
                redacted,
            };
        }
    }
}

/// Decode every JSON string token, including object keys and duplicate members,
/// then reserialize only changed tokens. This preserves original whitespace and
/// escape spelling when no sensitive decoded content exists. Walking tokens
/// also handles a truncated JSON prefix and does not collapse duplicate keys.
fn redact_json_strings(raw: &str, secrets: &[String]) -> Option<String> {
    let bytes = raw.as_bytes();
    let mut index = 0;
    let mut copied_until = 0;
    let mut output = String::new();
    while index < bytes.len() {
        if bytes[index] != b'"' {
            index += 1;
            continue;
        }
        let start = index;
        index += 1;
        let mut closed = false;
        while index < bytes.len() {
            if bytes[index] == b'\\' {
                index = (index + 2).min(bytes.len());
            } else if bytes[index] == b'"' {
                index += 1;
                closed = true;
                break;
            } else {
                index += 1;
            }
        }
        if !closed {
            break;
        }
        let Ok(decoded) = serde_json::from_str::<String>(&raw[start..index]) else {
            continue;
        };
        let mut redacted = decoded.clone();
        for secret in secrets {
            redacted = redacted.replace(secret.as_str(), "[redacted]");
        }
        if redacted != decoded {
            output.push_str(&raw[copied_until..start]);
            output.push_str(&serde_json::to_string(&redacted).ok()?);
            copied_until = index;
        }
    }
    if copied_until == 0 {
        Some(raw.to_owned())
    } else {
        output.push_str(&raw[copied_until..]);
        Some(output)
    }
}

/// Consume an upstream error response, buffering at most
/// `MAX_ERROR_BODY_BYTES` bytes. Returns the lossy-decoded text for
/// classification plus the number of bytes actually observed — capped, so the
/// recorded size honestly reports what was read, never a fabricated total.
pub(crate) async fn read_capped_error_body(response: reqwest::Response) -> (String, usize) {
    let (text, observed, _) = read_error_body(response, false).await;
    (text, observed)
}

pub(crate) async fn read_captured_error_body(
    response: reqwest::Response,
    limit: Option<usize>,
    capture: &ProbeResponseCapture,
    api_key: &str,
    headers: &std::collections::HashMap<String, String>,
) -> (String, usize) {
    if limit.is_none() {
        return read_capped_error_body(response).await;
    }
    let (text, observed, truncated) = read_error_body(response, true).await;
    capture.record(text.as_bytes(), limit, api_key, headers, truncated);
    (text, observed)
}

async fn read_error_body(
    response: reqwest::Response,
    detect_truncation: bool,
) -> (String, usize, bool) {
    let mut truncated = response
        .content_length()
        .is_some_and(|length| length > MAX_ERROR_BODY_BYTES as u64);
    let mut buffer: Vec<u8> = Vec::new();
    let mut chunks = response.bytes_stream();
    while detect_truncation || buffer.len() < MAX_ERROR_BODY_BYTES {
        let Some(chunk) = chunks.next().await else {
            break;
        };
        let Ok(bytes) = chunk else {
            truncated = true;
            break;
        };
        let remaining = MAX_ERROR_BODY_BYTES - buffer.len();
        let take = remaining.min(bytes.len());
        buffer.extend_from_slice(&bytes[..take]);
        if take < bytes.len() {
            truncated = true;
            break;
        }
    }
    let observed = buffer.len();
    (
        String::from_utf8_lossy(&buffer).into_owned(),
        observed,
        truncated,
    )
}

pub fn build_http_client(protocol: &str) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .pool_idle_timeout(Duration::from_secs(POOL_IDLE_TIMEOUT_SECS))
        .pool_max_idle_per_host(POOL_MAX_IDLE_PER_HOST)
        .tcp_keepalive(Duration::from_secs(TCP_KEEPALIVE_SECS))
        .build()
        .map_err(|error| format!("failed to build {protocol} HTTP client: {error}"))
}

pub fn error_kind(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() {
        "timeout"
    } else if connection_refused(error) {
        "connection_refused"
    } else if error.is_connect() {
        "connect"
    } else if error.is_body() {
        "body"
    } else if error.is_decode() {
        "decode"
    } else {
        "request"
    }
}

fn connection_refused(error: &(dyn std::error::Error + 'static)) -> bool {
    let mut current = Some(error);
    while let Some(cause) = current {
        if cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::ConnectionRefused)
        {
            return true;
        }
        current = cause.source();
    }
    false
}

/// Response cap belongs to a dedicated probe instance; ordinary traffic retains
/// its existing response contract. Oversized bodies are dropped immediately.
pub(crate) async fn read_completion_body(
    response: reqwest::Response,
    limit: Option<usize>,
) -> Result<bytes::Bytes, crate::proxy::canonical::ProviderError> {
    use crate::proxy::canonical::make_provider_error;
    let read_error = |error: reqwest::Error| {
        make_provider_error(
            "upstream_transport_error",
            error_kind(&error),
            Some(502),
            Some(true),
            None,
        )
    };
    let Some(limit) = limit else {
        return response.bytes().await.map_err(read_error);
    };
    let too_large = || {
        make_provider_error(
            "upstream_response_too_large",
            "response_body",
            Some(502),
            Some(false),
            Some(limit),
        )
    };
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(too_large());
    }
    let mut chunks = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = chunks.next().await {
        let chunk = chunk.map_err(read_error)?;
        if chunk.len() > limit.saturating_sub(body.len()) {
            return Err(too_large());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(bytes::Bytes::from(body))
}

/// Header names a provider row's `extra_headers` may never override. These are
/// the trusted auth/content headers the proxy sets itself; letting an upstream
/// pool row rewrite `authorization` / `x-api-key` would defeat credential
/// isolation, and rewriting `host` / `content-length` breaks the request.
/// Compared lowercase.
const RESERVED_EXTRA_HEADERS: &[&str] = &[
    "content-type",
    "authorization",
    "x-api-key",
    "x-goog-api-key",
    "anthropic-version",
    "host",
    "content-length",
];

/// Merge a provider's `extra_headers` into `headers`, skipping any entry whose
/// name collides with a reserved header (case-insensitive) or fails to parse.
/// Applied AFTER the trusted headers are set so a pool row can add
/// provider-mandated headers (e.g. OpenCode's `x-opencode-session`) but can
/// never silently replace the credential or content headers.
pub fn apply_extra_headers(
    headers: &mut reqwest::header::HeaderMap,
    extra_headers: &std::collections::HashMap<String, String>,
) {
    use reqwest::header::{HeaderName, HeaderValue};
    for (name, value) in extra_headers {
        let lowered = name.trim().to_ascii_lowercase();
        if lowered.is_empty() || RESERVED_EXTRA_HEADERS.contains(&lowered.as_str()) {
            continue;
        }
        let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(lowered.as_bytes()),
            HeaderValue::from_str(value),
        ) else {
            continue;
        };
        headers.insert(name, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::{ConnectInfo, State};
    use axum::response::IntoResponse;
    use axum::routing::get;
    use axum::Router;
    use std::collections::HashSet;
    use std::net::SocketAddr;
    use std::sync::{Arc, Mutex};

    #[test]
    fn probe_capture_redacts_before_truncation_and_never_captures_ordinary_bodies() {
        let capture = ProbeResponseCapture::default();
        let headers = std::collections::HashMap::new();
        capture.record(
            b"business response",
            None,
            "fixture_secret",
            &headers,
            false,
        );
        assert!(capture.take().is_none());
        assert_eq!(capture.flags(), (false, false));
        capture.record(
            b"prefix fixture_secret suffix",
            Some(12),
            "fixture_secret",
            &headers,
            false,
        );
        assert_eq!(capture.flags(), (true, true));
        assert_eq!(capture.take().as_deref(), Some("prefix [reda"));
        capture.record(
            b"prefix fixture_sec",
            Some(64),
            "fixture_secret",
            &headers,
            true,
        );
        assert_eq!(capture.flags(), (true, true));
        assert_eq!(capture.take().as_deref(), Some("prefix [redacted]"));
        capture.record(
            br#"{"key":"private\"key"}"#,
            Some(64),
            "private\"key",
            &headers,
            false,
        );
        assert_eq!(capture.take().as_deref(), Some(r#"{"key":"[redacted]"}"#));
        capture.clear();
        assert!(capture.take().is_none());
    }

    #[test]
    fn decoded_json_redaction_covers_keys_duplicates_and_preserves_nonsensitive_wire_text() {
        let capture = ProbeResponseCapture::default();
        let headers =
            std::collections::HashMap::from([("x-fixture".into(), "header/secret".into())]);
        capture.record(
            br#"{ "error":"\u0066ixture_key", "error":"public", "header\/secret":"fixture_key" }"#,
            Some(1024),
            "fixture_key",
            &headers,
            false,
        );
        assert_eq!(capture.flags(), (false, true));
        let body = capture.take().unwrap();
        assert_eq!(
            body,
            r#"{ "error":"[redacted]", "error":"public", "[redacted]":"[redacted]" }"#
        );
        let unchanged = br#"{ "public": "keep\/slash", "nested": ["\u006fther"] }"#;
        capture.record(unchanged, Some(1024), "fixture_key", &headers, false);
        assert_eq!(capture.flags(), (false, false));
        assert_eq!(capture.take().unwrap().as_bytes(), unchanged);
        capture.record(
            br#"{"error":"\u0066ixture_key","detail":"unfinished"#,
            Some(1024),
            "fixture_key",
            &headers,
            true,
        );
        let truncated = capture.take().unwrap();
        assert!(truncated.contains("[redacted]"));
        assert!(!truncated.contains(r"\u0066ixture_key"));
        assert_eq!(capture.flags(), (true, true));
    }

    async fn record_connection(
        State(remote_ports): State<Arc<Mutex<HashSet<u16>>>>,
        ConnectInfo(remote): ConnectInfo<SocketAddr>,
    ) -> &'static str {
        remote_ports.lock().unwrap().insert(remote.port());
        "ok"
    }

    #[test]
    fn apply_extra_headers_never_overrides_reserved_headers() {
        use std::collections::HashMap;
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("content-type", "application/json".parse().unwrap());
        headers.insert("x-api-key", "sk-trusted".parse().unwrap());
        headers.insert("anthropic-version", "2023-06-01".parse().unwrap());
        let mut extra = HashMap::new();
        extra.insert("X-API-Key".to_string(), "sk-attacker".to_string());
        extra.insert("Authorization".to_string(), "Bearer hijack".to_string());
        extra.insert("x-opencode-session".to_string(), "sess-1".to_string());
        apply_extra_headers(&mut headers, &extra);
        // Reserved auth/content headers are untouched...
        assert_eq!(headers.get("x-api-key").unwrap(), "sk-trusted");
        assert!(headers.get("authorization").is_none());
        // ...while unknown provider-mandated headers pass through.
        assert_eq!(headers.get("x-opencode-session").unwrap(), "sess-1");
    }

    #[test]
    fn apply_extra_headers_ignores_unparsable_entries() {
        use std::collections::HashMap;
        let mut headers = reqwest::header::HeaderMap::new();
        let mut extra = HashMap::new();
        extra.insert("".to_string(), "empty-name".to_string());
        extra.insert("bad\nname".to_string(), "x".to_string());
        extra.insert("bad-value".to_string(), "v\nw".to_string());
        extra.insert("x-ok".to_string(), "fine".to_string());
        apply_extra_headers(&mut headers, &extra);
        assert_eq!(headers.len(), 1);
        assert_eq!(headers.get("x-ok").unwrap(), "fine");
    }

    #[tokio::test]
    async fn shared_client_reuses_an_idle_connection() {
        let remote_ports = Arc::new(Mutex::new(HashSet::new()));
        let app = Router::new()
            .route("/health", get(record_connection))
            .with_state(remote_ports.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });
        let client = build_http_client("test").unwrap();
        let url = format!("http://{address}/health");
        assert_eq!(
            client.get(&url).send().await.unwrap().text().await.unwrap(),
            "ok"
        );
        assert_eq!(
            client.get(&url).send().await.unwrap().text().await.unwrap(),
            "ok"
        );
        assert_eq!(remote_ports.lock().unwrap().len(), 1);
        server.abort();
    }

    /// The bounded error read stops at MAX_ERROR_BODY_BYTES even when the
    /// upstream sends far more, and the observed size reports the capped
    /// bytes actually buffered (never the upstream's full length).
    #[tokio::test]
    async fn error_body_read_is_capped_and_counts_observed_bytes() {
        use axum::http::StatusCode;
        use axum::routing::post;

        async fn reject() -> impl IntoResponse {
            (StatusCode::BAD_GATEWAY, "x".repeat(256 * 1024))
        }
        let app = Router::new().route("/error", post(reject));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = build_http_client("test").unwrap();
        let response = client
            .post(&format!("http://{address}/error"))
            .send()
            .await
            .unwrap();
        let (text, observed) = read_capped_error_body(response).await;
        server.abort();
        assert_eq!(observed, MAX_ERROR_BODY_BYTES);
        assert_eq!(text.len(), MAX_ERROR_BODY_BYTES);
        assert!(text.chars().all(|c| c == 'x'));
    }

    /// A body smaller than the cap is read whole: the observed size equals
    /// the real body length and the lossy decode preserves the text.
    #[tokio::test]
    async fn small_error_bodies_are_read_whole() {
        use axum::http::StatusCode;
        use axum::routing::post;

        async fn reject() -> impl IntoResponse {
            (StatusCode::BAD_REQUEST, "推理😀 opaque".to_string())
        }
        let app = Router::new().route("/error", post(reject));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = build_http_client("test").unwrap();
        let response = client
            .post(&format!("http://{address}/error"))
            .send()
            .await
            .unwrap();
        let (text, observed) = read_capped_error_body(response).await;
        server.abort();
        assert_eq!(text, "推理😀 opaque");
        assert_eq!(observed, "推理😀 opaque".len());
    }
}
