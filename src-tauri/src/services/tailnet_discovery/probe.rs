use super::{
    inventory::{is_tailnet_address, Candidate},
    types::Aag_instance,
};
use futures_util::StreamExt;
use reqwest::{redirect::Policy, Client, StatusCode};
use serde::Deserialize;
use std::{
    net::SocketAddr,
    time::{Duration, Instant},
};

const BODY_LIMIT: usize = 8 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_millis(750);
const PROBE_TIMEOUT: Duration = Duration::from_millis(1500);

pub(super) enum Probe_outcome {
    Online(Aag_instance),
    AuthRequired,
    Offline,
    Unverified,
    Failed,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Discovery_marker {
    service: String,
    schema_version: u8,
    status: String,
    version: String,
    #[serde(default, rename = "runtime_kind")]
    runtime_kind: Option<String>,
}

fn parse_marker(bytes: &[u8]) -> Option<String> {
    if bytes.len() > BODY_LIMIT {
        return None;
    }
    let marker: Discovery_marker = serde_json::from_slice(bytes).ok()?;
    if marker.service != "ai-agent-gateway"
        || marker.schema_version != 1
        || marker.status != "running"
        || marker
            .runtime_kind
            .as_deref()
            .is_some_and(|kind| !matches!(kind, "desktop" | "node"))
        || marker.version.is_empty()
        || marker.version.len() > 64
        || !marker
            .version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b".+-".contains(&byte))
    {
        return None;
    }
    Some(marker.version)
}

pub(super) fn build_client() -> Result<Client, String> {
    Client::builder()
        .no_proxy()
        .redirect(Policy::none())
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(PROBE_TIMEOUT)
        .pool_max_idle_per_host(0)
        .user_agent("AAG-Discovery/1")
        .build()
        .map_err(|_| "tailnet_discovery_http_client_failed".into())
}

async fn probe_socket(
    client: &Client,
    address: SocketAddr,
) -> Result<Option<String>, Probe_outcome> {
    // This function is private. Production entry below only accepts validated
    // inventory addresses; tests exercise the HTTP contract on isolated loopback.
    let response = client
        .get(format!("http://{address}/aag/discovery"))
        .send()
        .await
        .map_err(|error| {
            if error.is_connect() || error.is_timeout() {
                Probe_outcome::Offline
            } else {
                Probe_outcome::Failed
            }
        })?;
    match response.status() {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            return Err(Probe_outcome::AuthRequired)
        }
        StatusCode::OK => (),
        status if status.is_server_error() => return Err(Probe_outcome::Failed),
        _ => return Err(Probe_outcome::Unverified),
    }
    if response
        .content_length()
        .is_some_and(|size| size > BODY_LIMIT as u64)
        || !response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| {
                value
                    .split(';')
                    .next()
                    .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"))
            })
    {
        return Err(Probe_outcome::Unverified);
    }
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| Probe_outcome::Failed)?;
        if bytes.len() + chunk.len() > BODY_LIMIT {
            return Err(Probe_outcome::Unverified);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(parse_marker(&bytes))
}

pub(super) async fn probe_candidate(
    client: &Client,
    candidate: Candidate,
    port: u16,
) -> Probe_outcome {
    if !is_tailnet_address(candidate.address) || port == 0 {
        return Probe_outcome::Unverified;
    }
    let started = Instant::now();
    match probe_socket(client, SocketAddr::new(candidate.address, port)).await {
        Ok(Some(version)) => Probe_outcome::Online(Aag_instance {
            device_name: candidate.device_name,
            address: candidate.address.to_string(),
            port,
            version,
            latency_ms: started.elapsed().as_millis() as u64,
            is_self: candidate.is_self,
        }),
        Ok(None) => Probe_outcome::Unverified,
        Err(outcome) => outcome,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Response, routing::get, Router};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    async fn serve(app: Router) -> (SocketAddr, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (address, task)
    }

    #[test]
    fn marker_requires_bounded_exact_service_identity() {
        assert_eq!(parse_marker(br#"{"service":"ai-agent-gateway","schemaVersion":1,"status":"running","version":"0.2.0"}"#), Some("0.2.0".into()));
        for bytes in [br#"{"status":"ok"}"#.as_slice(), br#"{"service":"another-service","schemaVersion":1,"status":"running","version":"0.2.0"}"#.as_slice(), br#"{"service":"ai-agent-gateway","schemaVersion":1,"status":"running","version":"<script>"}"#.as_slice()] {
            assert_eq!(parse_marker(bytes), None);
        }
        assert_eq!(parse_marker(&vec![b' '; BODY_LIMIT + 1]), None);
    }

    #[test]
    fn marker_accepts_explicit_runtime_kind_and_preserves_legacy_compatibility() {
        for runtime_kind in [None, Some("desktop"), Some("node")] {
            let mut marker = serde_json::json!({
                "service": "ai-agent-gateway", "schemaVersion": 1,
                "status": "running", "version": "0.2.0"
            });
            if let Some(runtime_kind) = runtime_kind {
                marker["runtime_kind"] = runtime_kind.into();
            }
            assert_eq!(
                parse_marker(&serde_json::to_vec(&marker).unwrap()),
                Some("0.2.0".into())
            );
        }
        for patch in [
            serde_json::json!({"runtime_kind": "other-runtime"}),
            serde_json::json!({"runtime_kind": ""}),
            serde_json::json!({"runtime_kind": 1}),
            serde_json::json!({"runtime_kind": "desktop", "private_endpoint": "fixture"}),
        ] {
            let mut marker = serde_json::json!({
                "service": "ai-agent-gateway", "schemaVersion": 1,
                "status": "running", "version": "0.2.0"
            });
            marker
                .as_object_mut()
                .unwrap()
                .extend(patch.as_object().unwrap().clone());
            assert_eq!(parse_marker(&serde_json::to_vec(&marker).unwrap()), None);
        }
    }

    #[tokio::test]
    async fn transport_does_not_follow_redirects_or_send_credentials() {
        let redirected = Arc::new(AtomicUsize::new(0));
        let redirected_route = redirected.clone();
        let (address, server) = serve(
            Router::new()
                .route(
                    "/aag/discovery",
                    get(|headers: axum::http::HeaderMap| async move {
                        assert!(!headers.contains_key("authorization"));
                        Response::builder()
                            .status(302)
                            .header("location", "/redirected")
                            .body(Body::empty())
                            .unwrap()
                    }),
                )
                .route(
                    "/redirected",
                    get(move || {
                        let count = redirected_route.clone();
                        async move {
                            count.fetch_add(1, Ordering::SeqCst);
                            "unexpected"
                        }
                    }),
                ),
        )
        .await;
        assert!(matches!(
            probe_socket(&build_client().unwrap(), address).await,
            Err(Probe_outcome::Unverified)
        ));
        assert_eq!(redirected.load(Ordering::SeqCst), 0);
        server.abort();
    }

    #[tokio::test]
    async fn distinguishes_auth_unverified_and_aag_health() {
        for (status, content_type, body, expected) in [
            (
                401_u16,
                "application/json",
                r#"{"error":"auth_required"}"#,
                "auth",
            ),
            (200, "application/json", r#"{"status":"ok"}"#, "unverified"),
            (200, "text/html", "<h1>Other service</h1>", "unverified"),
            (
                200,
                "application/json",
                r#"{"service":"ai-agent-gateway","schemaVersion":1,"status":"running","version":"0.2.0"}"#,
                "online",
            ),
        ] {
            let (address, server) = serve(Router::new().route(
                "/aag/discovery",
                get(move || async move {
                    Response::builder()
                        .status(status)
                        .header("content-type", content_type)
                        .body(Body::from(body))
                        .unwrap()
                }),
            ))
            .await;
            let outcome = probe_socket(&build_client().unwrap(), address).await;
            assert!(match expected {
                "auth" => matches!(outcome, Err(Probe_outcome::AuthRequired)),
                "online" => matches!(outcome, Ok(Some(_))),
                _ => matches!(outcome, Ok(None) | Err(Probe_outcome::Unverified)),
            });
            server.abort();
        }
    }

    #[tokio::test]
    async fn rejects_non_inventory_destinations_before_network_io() {
        let candidate = Candidate {
            device_name: "fixture".into(),
            address: "127.0.0.1".parse().unwrap(),
            is_self: false,
        };
        assert!(matches!(
            probe_candidate(&build_client().unwrap(), candidate, 8989).await,
            Probe_outcome::Unverified
        ));
    }

    #[tokio::test]
    async fn response_size_and_slow_peer_are_bounded() {
        let (oversized_address, oversized_server) = serve(Router::new().route(
            "/aag/discovery",
            get(|| async {
                Response::builder()
                    .header("content-type", "application/json")
                    .body(Body::from(vec![b' '; BODY_LIMIT + 1]))
                    .unwrap()
            }),
        ))
        .await;
        assert!(matches!(
            probe_socket(&build_client().unwrap(), oversized_address).await,
            Err(Probe_outcome::Unverified)
        ));
        oversized_server.abort();
        let (slow_address, slow_server) = serve(Router::new().route(
            "/aag/discovery",
            get(|| async {
                tokio::time::sleep(Duration::from_secs(5)).await;
                "late"
            }),
        ))
        .await;
        let started = Instant::now();
        assert!(matches!(
            probe_socket(&build_client().unwrap(), slow_address).await,
            Err(Probe_outcome::Offline)
        ));
        assert!(started.elapsed() < Duration::from_secs(3));
        slow_server.abort();
    }
}
