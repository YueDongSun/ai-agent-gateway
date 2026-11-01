//! Port-fallback bind strategy: try the configured port, then +1..+5.
//! Each candidate is bound via `serve_detached` and confirmed with a
//! `/health` probe so the proxy never lands on a port owned by another
//! process (including the Windows TIME_WAIT trap).

use crate::database::dao::proxy_config::ProxyConfig;

pub(crate) fn health_probe_request(
    client: &reqwest::Client,
    url: &str,
    auth_token: &str,
) -> reqwest::RequestBuilder {
    let request = client.get(url);
    if auth_token.is_empty() {
        request
    } else {
        request.bearer_auth(auth_token)
    }
}

/// What one successful port candidate hands back: the bound listener's
/// info, its server task, and the single-use shutdown sender.
#[allow(non_camel_case_types)] // repo style: snake_case type names are allowed explicitly
pub(crate) type Bound_server = (
    crate::proxy::server::BoundInfo,
    tokio::task::JoinHandle<Result<(), String>>,
    tokio::sync::oneshot::Sender<()>,
);

pub(crate) async fn bind_with_fallback(
    app_state: &crate::proxy::server::ProxyAppState,
    config: &ProxyConfig,
) -> Result<Bound_server, String> {
    // Port-fallback: try requested_port, then +1..+5 in order. For each
    // candidate, bind via serve_detached (bind outcome known immediately —
    // no readiness guessing), probe /health to confirm we own the port
    // (avoids landing on another app), then persist the actual port.
    const MAX_PORT_OFFSET: u16 = 5;
    let mut failed_candidates = 0_u16;
    let mut chosen: Option<Bound_server> = None;
    let original_port = config.port;
    for offset in 0..=MAX_PORT_OFFSET {
        let try_port = original_port.wrapping_add(offset);
        let try_addr = crate::proxy::server::socket_addr(&config.host, try_port);
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let (bound, handle) =
            match crate::proxy::server::serve_detached(try_addr, app_state.clone(), shutdown_rx)
                .await
            {
                Ok(v) => v,
                Err(_) => {
                    failed_candidates = failed_candidates.saturating_add(1);
                    continue;
                }
            };
        // Confirm we actually own the port via /health (rules out the
        // Windows TIME_WAIT trap where the port answers but isn't ours).
        let probe = match reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(500))
            .build()
        {
            Ok(c) => c,
            Err(_) => {
                failed_candidates = failed_candidates.saturating_add(1);
                let _ = shutdown_tx.send(());
                let _ = handle.await;
                continue;
            }
        };
        let probe_url = format!("http://{}/health", bound.addr);
        let probe_ok = health_probe_request(&probe, &probe_url, &config.auth_token)
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false);
        if !probe_ok {
            log::warn!(
                "proxy: health probe failed port={} reason=non_success_or_refused",
                bound.addr.port()
            );
            failed_candidates = failed_candidates.saturating_add(1);
            let _ = shutdown_tx.send(());
            let _ = handle.await;
            continue;
        }
        chosen = Some((bound, handle, shutdown_tx));
        break;
    }
    chosen.ok_or_else(|| {
        let _ = failed_candidates;
        "proxy_bind_exhausted".to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::{bind_with_fallback, health_probe_request};
    use crate::database::dao::proxy_config::ProxyConfig;
    use crate::database::dao::proxy_providers::CircuitBreakerConfig;
    use crate::proxy::providers::mock::MockProvider;
    use crate::proxy::router::Router;
    use crate::proxy::server::ProxyAppState;
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::oneshot;

    #[test]
    fn health_probe_reuses_configured_gateway_auth_without_auth_when_disabled() {
        let client = reqwest::Client::new();
        let authenticated = health_probe_request(
            &client,
            "http://127.0.0.1:8989/health",
            "fixture-gateway-token",
        )
        .build()
        .unwrap();
        assert_eq!(
            authenticated
                .headers()
                .get(reqwest::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok()),
            Some("Bearer fixture-gateway-token")
        );

        let unauthenticated = health_probe_request(&client, "http://127.0.0.1:8989/health", "")
            .build()
            .unwrap();
        assert!(unauthenticated
            .headers()
            .get(reqwest::header::AUTHORIZATION)
            .is_none());
    }

    #[tokio::test]
    async fn authenticated_health_probe_accepts_the_bound_proxy() {
        let mut providers: HashMap<String, Arc<dyn crate::proxy::providers::Provider>> =
            HashMap::new();
        providers.insert(
            "mock".into(),
            Arc::new(MockProvider::new(
                "mock".into(),
                vec!["m".into()],
                true,
                CircuitBreakerConfig::default(),
            )),
        );
        let state = ProxyAppState {
            router: Arc::new(Router::new(providers, HashMap::new())),
            auth_token: "fixture-gateway-token".into(),
            pinned_provider: Arc::new(std::sync::Mutex::new(None)),
            responses_cot_enabled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            passthrough_auth_allowed: false,
            db: None,
            live: None,
            allow_client_config_repair: false,
        };
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let (bound, handle) = crate::proxy::server::serve_detached(
            "127.0.0.1:0".parse().unwrap(),
            state,
            shutdown_rx,
        )
        .await
        .unwrap();
        let client = reqwest::Client::new();
        let response = health_probe_request(
            &client,
            &format!("http://{}/health", bound.addr),
            "fixture-gateway-token",
        )
        .send()
        .await
        .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        shutdown_tx.send(()).unwrap();
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn bind_with_fallback_accepts_an_authenticated_health_probe() {
        let mut providers: HashMap<String, Arc<dyn crate::proxy::providers::Provider>> =
            HashMap::new();
        providers.insert(
            "mock".into(),
            Arc::new(MockProvider::new(
                "mock".into(),
                vec!["m".into()],
                true,
                CircuitBreakerConfig::default(),
            )),
        );
        let state = ProxyAppState {
            router: Arc::new(Router::new(providers, HashMap::new())),
            auth_token: "fixture-gateway-token".into(),
            pinned_provider: Arc::new(std::sync::Mutex::new(None)),
            responses_cot_enabled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            passthrough_auth_allowed: false,
            db: None,
            live: None,
            allow_client_config_repair: false,
        };
        let config = ProxyConfig {
            host: "127.0.0.1".into(),
            port: 0,
            auth_token: "fixture-gateway-token".into(),
            enabled: true,
        };
        let (bound, handle, shutdown_tx) = bind_with_fallback(&state, &config).await.unwrap();
        assert!(bound.addr.port() > 0);
        shutdown_tx.send(()).unwrap();
        handle.await.unwrap().unwrap();
    }
}
