//! Shared HTTP client construction for upstream providers.

use std::time::Duration;

const POOL_IDLE_TIMEOUT_SECS: u64 = 90;
const TCP_KEEPALIVE_SECS: u64 = 60;
const POOL_MAX_IDLE_PER_HOST: usize = 4;

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
    use axum::routing::get;
    use axum::Router;
    use std::collections::HashSet;
    use std::net::SocketAddr;
    use std::sync::{Arc, Mutex};

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
}
