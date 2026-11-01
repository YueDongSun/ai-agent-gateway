//! Upstream model-catalog fetching for auto-discovery.
//!
//! One probe implementation shared by the manual `proxy_fetch_models`
//! command and the scheduled discovery refresh: protocol-aware candidate
//! URLs (`proxy::endpoint::models_urls`), per-provider-type auth headers,
//! and parsing of both the OpenAI `data[].id` shape and Gemini's
//! `models[].name` projection.

use crate::database::dao::proxy_providers::ProxyProvider;
use crate::proxy::providers::http_client::apply_extra_headers;
use crate::proxy::providers::AnthropicAuthScheme;
use reqwest::header::{HeaderMap, HeaderValue};

const DISCOVERY_TIMEOUT_SECS: u64 = 15;
const ANTHROPIC_VERSION: &str = "2023-06-01";
const MAX_CATALOG_BYTES: usize = 2 * 1024 * 1024;
const MAX_CATALOG_PAGES: usize = 10;
const MAX_CATALOG_MODELS: usize = 1000;

fn valid_model_id(id: &str) -> bool {
    !id.trim().is_empty() && id.len() <= 256 && !id.chars().any(char::is_control)
}

/// A single model entry parsed from an upstream catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UpstreamModel {
    pub id: String,
    /// Upstream `created` unix timestamp when the catalog provides one
    /// (OpenAI-shaped responses); used as a deterministic tiebreak.
    pub created: Option<i64>,
}

/// Parsed catalog plus the raw JSON entries the UI suggestion flow returns.
#[derive(Debug, Clone)]
pub(crate) struct FetchedModels {
    /// Only the test target reads this field today: the scheduled refresh
    /// switched to the ids-only [`parse_models_ids`], while
    /// `proxy_fetch_models` returns `raw` and the unit tests pin `models`.
    #[allow(dead_code)]
    pub models: Vec<UpstreamModel>,
    pub raw: Vec<serde_json::Value>,
}

/// Probe failures carry the exact stable strings the frontend already
/// matches on from `proxy_fetch_models` (bypassing `AppError`'s detail
/// sanitizer, which would collapse them to `operation_failed`).
#[derive(Debug)]
pub(crate) enum FetchError {
    HttpClientBuild,
    InvalidHeader,
    Decode,
    Http(u16),
    Transport,
    Timeout,
    Limit,
    Pagination,
    Unavailable,
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchError::HttpClientBuild => write!(f, "proxy_http_client_build_failed"),
            FetchError::InvalidHeader => write!(f, "proxy_models_invalid_header"),
            FetchError::Decode => write!(f, "proxy_models_decode_failed"),
            FetchError::Http(status) => write!(f, "proxy_models_http_error_{status}"),
            FetchError::Transport => write!(f, "proxy_models_transport_failed"),
            FetchError::Timeout => write!(f, "proxy_models_timeout"),
            FetchError::Limit => write!(f, "proxy_models_limit_exceeded"),
            FetchError::Pagination => write!(f, "proxy_models_pagination_invalid"),
            FetchError::Unavailable => write!(f, "proxy_models_unavailable"),
        }
    }
}

/// Parse one `/v1/models` response body. Returns `None` when the body has
/// neither a supported catalog shape nor valid nonempty ids on every row.
pub(crate) fn parse_models_body(body: &serde_json::Value) -> Option<FetchedModels> {
    if let Some(entries) = body.get("data").and_then(|d| d.as_array()) {
        let models: Vec<UpstreamModel> = entries
            .iter()
            .map(|e| {
                let id = e.get("id").and_then(|v| v.as_str())?;
                Some(UpstreamModel {
                    id: valid_model_id(id).then_some(id)?.to_string(),
                    created: e.get("created").and_then(|v| v.as_i64()),
                })
            })
            .collect::<Option<Vec<_>>>()?;
        return Some(FetchedModels {
            models,
            raw: entries.clone(),
        });
    }
    if let Some(entries) = body.get("models").and_then(|m| m.as_array()) {
        // Gemini lists `models[].name` ("models/gemini-2.5-pro"); project
        // onto the OpenAI shape the rest of the app expects.
        let mut models = Vec::new();
        let mut raw = Vec::new();
        for entry in entries {
            let name = entry.get("name").and_then(|n| n.as_str())?;
            let id = name.strip_prefix("models/").unwrap_or(name);
            if !valid_model_id(id) {
                return None;
            }
            let display = entry
                .get("displayName")
                .and_then(|d| d.as_str())
                .unwrap_or(id);
            models.push(UpstreamModel {
                id: id.to_string(),
                created: None,
            });
            raw.push(serde_json::json!({
                "id": id,
                "object": "model",
                "owned_by": "google",
                "display_name": display,
            }));
        }
        return Some(FetchedModels { models, raw });
    }
    None
}

/// Ids-only parse of one `/v1/models` response body: same shapes and the
/// same `None` verdicts as [`parse_models_body`], but without cloning the
/// whole catalog tree into `raw`. Used by the scheduled refresh loop, which
/// never reads `raw`.
pub(crate) fn parse_models_ids(body: &serde_json::Value) -> Option<Vec<UpstreamModel>> {
    if let Some(entries) = body.get("data").and_then(|d| d.as_array()) {
        return entries
            .iter()
            .map(|e| {
                let id = e.get("id").and_then(|v| v.as_str())?;
                Some(UpstreamModel {
                    id: valid_model_id(id).then_some(id)?.to_string(),
                    created: e.get("created").and_then(|v| v.as_i64()),
                })
            })
            .collect::<Option<Vec<_>>>();
    }
    if let Some(entries) = body.get("models").and_then(|m| m.as_array()) {
        // Gemini lists `models[].name` ("models/gemini-2.5-pro"); project
        // onto the OpenAI shape the rest of the app expects.
        let mut models = Vec::new();
        for entry in entries {
            let name = entry.get("name").and_then(|n| n.as_str())?;
            let id = name.strip_prefix("models/").unwrap_or(name);
            if !valid_model_id(id) {
                return None;
            }
            models.push(UpstreamModel {
                id: id.to_string(),
                created: None,
            });
        }
        return Some(models);
    }
    None
}

/// Build protocol credentials before I/O; invalid trusted values fail redacted.
pub(super) fn discovery_headers(
    pp: &ProxyProvider,
    auth_scheme: AnthropicAuthScheme,
) -> Result<HeaderMap, FetchError> {
    let mut headers = HeaderMap::new();
    if pp.provider_type == "anthropic" {
        headers.insert(
            "anthropic-version",
            HeaderValue::from_static(ANTHROPIC_VERSION),
        );
    }
    if !pp.api_key.is_empty() {
        let (name, value) = match pp.provider_type.as_str() {
            "gemini" => ("x-goog-api-key", pp.api_key.clone()),
            "anthropic" if auth_scheme == AnthropicAuthScheme::ApiKey => {
                ("x-api-key", pp.api_key.clone())
            }
            "anthropic" => ("authorization", format!("Bearer {}", pp.api_key)),
            _ => ("authorization", format!("Bearer {}", pp.api_key)),
        };
        let mut value = HeaderValue::from_str(&value).map_err(|_| FetchError::InvalidHeader)?;
        value.set_sensitive(true);
        headers.insert(name, value);
    }
    apply_extra_headers(&mut headers, &pp.extra_headers);
    Ok(headers)
}

/// Fetch and parse the model catalog for one pool provider, keeping the raw
/// JSON entries the UI suggestion flow returns.
pub(crate) async fn fetch_models(
    pp: &ProxyProvider,
    auth_scheme: AnthropicAuthScheme,
) -> Result<FetchedModels, FetchError> {
    fetch_catalog(pp, auth_scheme, parse_models_body).await
}

/// Ids-only probe for the discovery refresh loop: the refresh path never
/// reads `raw`, so the whole catalog tree is not cloned per pass.
pub(crate) async fn fetch_models_ids(
    pp: &ProxyProvider,
    auth_scheme: AnthropicAuthScheme,
) -> Result<Vec<UpstreamModel>, FetchError> {
    fetch_catalog(pp, auth_scheme, parse_models_ids).await
}

/// Shared probe: protocol-aware candidate URLs and per-provider auth
/// headers, then a caller-shaped parse of the first successful response
/// body. A body with no supported catalog shape fails with
/// [`FetchError::Decode`].
async fn fetch_catalog<F, T>(
    pp: &ProxyProvider,
    auth_scheme: AnthropicAuthScheme,
    parse: F,
) -> Result<T, FetchError>
where
    F: Fn(&serde_json::Value) -> Option<T>,
{
    tokio::time::timeout(
        std::time::Duration::from_secs(DISCOVERY_TIMEOUT_SECS),
        fetch_catalog_bounded(pp, auth_scheme, parse),
    )
    .await
    .map_err(|_| FetchError::Timeout)?
}

async fn fetch_catalog_bounded<F, T>(
    pp: &ProxyProvider,
    auth_scheme: AnthropicAuthScheme,
    parse: F,
) -> Result<T, FetchError>
where
    F: Fn(&serde_json::Value) -> Option<T>,
{
    let headers = discovery_headers(pp, auth_scheme)?;
    let base_url = pp.base_url.trim_end_matches('/');
    let protocol = match pp.provider_type.as_str() {
        "anthropic" => crate::proxy::endpoint::Protocol::Anthropic,
        "gemini" => crate::proxy::endpoint::Protocol::Gemini,
        "openai-responses" => crate::proxy::endpoint::Protocol::Responses,
        _ => crate::proxy::endpoint::Protocol::Openai,
    };
    let candidates = crate::proxy::endpoint::models_urls(base_url, protocol);
    let query = reqwest::Url::parse(base_url)
        .ok()
        .and_then(|url| url.query().map(str::to_owned));

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(DISCOVERY_TIMEOUT_SECS))
        // A catalog redirect is not authority to forward configured headers.
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| FetchError::HttpClientBuild)?;

    let mut last_error = FetchError::Unavailable;
    let mut remaining_bytes = MAX_CATALOG_BYTES;
    for candidate in &candidates {
        let mut url = reqwest::Url::parse(candidate).map_err(|_| FetchError::Unavailable)?;
        url.set_query(query.as_deref());
        match fetch_pages(&client, url, &headers, &mut remaining_bytes).await {
            Ok(body) => match parse(&body) {
                Some(parsed) => return Ok(parsed),
                None => last_error = FetchError::Decode,
            },
            // Only evidence of a wrong endpoint permits another path. An auth,
            // quota, or transient outage must retain its original diagnosis.
            Err(error @ (FetchError::Http(404 | 405) | FetchError::Decode)) => last_error = error,
            Err(error) => return Err(error),
        }
    }
    Err(last_error)
}

async fn fetch_pages(
    client: &reqwest::Client,
    mut url: reqwest::Url,
    headers: &HeaderMap,
    remaining_bytes: &mut usize,
) -> Result<serde_json::Value, FetchError> {
    let mut combined = None;
    let mut cursors = std::collections::HashSet::new();
    for _ in 0..MAX_CATALOG_PAGES {
        let mut response = client
            .get(url.clone())
            .headers(headers.clone())
            .send()
            .await
            .map_err(|error| {
                if error.is_timeout() {
                    FetchError::Timeout
                } else {
                    FetchError::Transport
                }
            })?;
        if !response.status().is_success() {
            return Err(FetchError::Http(response.status().as_u16()));
        }
        if response
            .content_length()
            .is_some_and(|length| length > *remaining_bytes as u64)
        {
            return Err(FetchError::Limit);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|error| {
            if error.is_timeout() {
                FetchError::Timeout
            } else {
                FetchError::Transport
            }
        })? {
            if chunk.len() > *remaining_bytes {
                return Err(FetchError::Limit);
            }
            *remaining_bytes -= chunk.len();
            bytes.extend_from_slice(&chunk);
        }
        let body: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| FetchError::Decode)?;
        let key = if body.get("data").is_some_and(|value| value.is_array()) {
            "data"
        } else {
            "models"
        };
        let entries = body
            .get(key)
            .and_then(|value| value.as_array())
            .ok_or(FetchError::Decode)?;
        let cursor = pagination_cursor(&body)?;
        let accumulated = combined.get_or_insert_with(|| serde_json::json!({(key): []}));
        let output = accumulated
            .get_mut(key)
            .and_then(|value| value.as_array_mut())
            .ok_or(FetchError::Decode)?;
        if output.len() + entries.len() > MAX_CATALOG_MODELS {
            return Err(FetchError::Limit);
        }
        output.extend(entries.iter().cloned());
        let Some((name, token)) = cursor else {
            return Ok(accumulated.clone());
        };
        if !cursors.insert((name, token.clone())) {
            return Err(FetchError::Pagination);
        }
        let query: Vec<_> = url
            .query_pairs()
            .filter(|(key, _)| key != name)
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        url.set_query(None);
        url.query_pairs_mut()
            .extend_pairs(query)
            .append_pair(name, &token);
    }
    Err(FetchError::Limit)
}

/// Pagination is part of the completeness contract. A malformed or mixed
/// dialect must not masquerade as the final page of a successful catalog.
fn pagination_cursor(
    body: &serde_json::Value,
) -> Result<Option<(&'static str, String)>, FetchError> {
    let has_more = body
        .get("has_more")
        .map(|value| value.as_bool().ok_or(FetchError::Pagination))
        .transpose()?;
    let last_id = match body.get("last_id") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(value))
            if !value.is_empty() && !value.chars().any(char::is_control) =>
        {
            Some(value.as_str())
        }
        Some(_) => return Err(FetchError::Pagination),
    };
    let page_token = match body.get("nextPageToken") {
        None => None,
        Some(serde_json::Value::String(value)) if !value.chars().any(char::is_control) => {
            Some(value.as_str())
        }
        Some(_) => return Err(FetchError::Pagination),
    };
    if page_token.is_some() && (has_more.is_some() || body.get("last_id").is_some()) {
        return Err(FetchError::Pagination);
    }
    if let Some(token) = page_token.filter(|token| !token.is_empty()) {
        return Ok(Some(("pageToken", token.to_owned())));
    }
    match has_more {
        Some(true) => Ok(Some((
            "after_id",
            last_id.ok_or(FetchError::Pagination)?.to_owned(),
        ))),
        None if body.get("last_id").is_some() => Err(FetchError::Pagination),
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn catalog_collects_bounded_pages_and_rejects_cursor_cycles() {
        use axum::{extract::Query, routing::get, Json, Router};
        for cycle in [false, true] {
            let app = Router::new().route("/v1/models",get(move |Query(query):Query<std::collections::HashMap<String,String>>|async move {
                if cycle || !query.contains_key("after_id") {
                    Json(serde_json::json!({"data":[{"id":"first"}],"has_more":true,"last_id":"first"}))
                } else {
                    Json(serde_json::json!({"data":[{"id":"second"}],"has_more":false}))
                }
            }));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let provider = fixture_provider(
                "anthropic",
                format!("http://{}", listener.local_addr().unwrap()),
            );
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let result = fetch_models(&provider, AnthropicAuthScheme::ApiKey).await;
            server.abort();
            let _ = server.await;
            if cycle {
                assert!(
                    result.is_err(),
                    "partial cyclic catalogs must not be admitted"
                );
            } else {
                assert_eq!(
                    result
                        .unwrap()
                        .models
                        .iter()
                        .map(|model| model.id.as_str())
                        .collect::<Vec<_>>(),
                    ["first", "second"]
                );
            }
        }
    }

    #[tokio::test]
    async fn catalog_probe_keeps_authoritative_errors_and_skips_html_candidates() {
        use axum::{http::StatusCode, response::IntoResponse, routing::get, Json, Router};
        for status in [
            StatusCode::UNAUTHORIZED,
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::SERVICE_UNAVAILABLE,
            StatusCode::OK,
        ] {
            let app = Router::new()
                .route(
                    "/relay/v1/models",
                    get(move || async move {
                        (status, "<html>gateway landing page</html>").into_response()
                    }),
                )
                .route(
                    "/relay/models",
                    get(|| async { Json(serde_json::json!({"data":[{"id":"new-family-model"}]})) }),
                );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let provider = fixture_provider(
                "openai",
                format!("http://{}/relay", listener.local_addr().unwrap()),
            );
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let result = fetch_models(&provider, AnthropicAuthScheme::ApiKey).await;
            server.abort();
            let _ = server.await;
            if status == StatusCode::OK {
                assert_eq!(result.unwrap().models[0].id, "new-family-model");
            } else {
                assert_eq!(
                    result.unwrap_err().to_string(),
                    format!("proxy_models_http_error_{}", status.as_u16())
                );
            }
        }
    }

    #[tokio::test]
    async fn catalog_probe_retains_query_and_refuses_redirects() {
        use axum::{extract::Query, response::Redirect, routing::get, Json, Router};
        let app = Router::new()
            .route(
                "/relay/v1/models",
                get(
                    |Query(query): Query<std::collections::HashMap<String, String>>| async move {
                        assert_eq!(
                            query.get("api-version").map(String::as_str),
                            Some("2026-01-01")
                        );
                        Json(serde_json::json!({"data":[{"id":"versioned-model"}]}))
                    },
                ),
            )
            .route(
                "/redirect/v1/models",
                get(|| async { Redirect::temporary("/secret-target") }),
            )
            .route(
                "/secret-target",
                get(|| async {
                    panic!("catalog probe followed a credential-bearing redirect");
                    #[allow(unreachable_code)]
                    ""
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let result = fetch_models(
            &fixture_provider("openai", format!("{base}/relay?api-version=2026-01-01")),
            AnthropicAuthScheme::ApiKey,
        )
        .await;
        let redirected = fetch_models(
            &fixture_provider("anthropic", format!("{base}/redirect")),
            AnthropicAuthScheme::ApiKey,
        )
        .await;
        server.abort();
        let _ = server.await;
        assert_eq!(result.unwrap().models[0].id, "versioned-model");
        assert_eq!(
            redirected.unwrap_err().to_string(),
            "proxy_models_http_error_307"
        );
    }

    #[test]
    fn parses_openai_shape_with_created_timestamps() {
        let body = serde_json::json!({
            "object": "list",
            "data": [
                {"id": "MiniMax-M3", "object": "model", "created": 1700000000},
                {"id": "MiniMax-M3.1", "object": "model", "created": 1750000000}
            ]
        });
        let parsed = parse_models_body(&body).expect("openai shape");
        assert_eq!(parsed.models.len(), 2);
        assert_eq!(parsed.models[1].id, "MiniMax-M3.1");
        assert_eq!(parsed.models[1].created, Some(1_750_000_000));
        assert_eq!(parsed.raw.len(), 2);
    }

    #[test]
    fn projects_gemini_shape_with_display_names() {
        let body = serde_json::json!({
            "models": [
                {"name": "models/gemini-2.5-pro", "displayName": "Gemini 2.5 Pro"},
                {"name": "gemini-2.5-flash", "displayName": "Gemini 2.5 Flash"}
            ]
        });
        let parsed = parse_models_body(&body).expect("gemini shape");
        assert_eq!(parsed.models[0].id, "gemini-2.5-pro");
        assert_eq!(parsed.models[0].created, None);
        assert_eq!(parsed.raw[0]["display_name"], "Gemini 2.5 Pro");
        assert_eq!(parsed.raw[1]["id"], "gemini-2.5-flash");
    }

    #[test]
    fn unrecognized_shape_yields_none() {
        assert!(parse_models_body(&serde_json::json!({"error": {"message": "nope"}})).is_none());
        assert!(parse_models_body(&serde_json::json!("ok")).is_none());
    }

    #[test]
    fn ids_only_parse_matches_the_full_parse_verdicts() {
        // OpenAI shape: ids + created timestamps, no raw tree.
        let openai = serde_json::json!({
            "object": "list",
            "data": [
                {"id": "MiniMax-M3", "object": "model", "created": 1700000000},
                {"id": "MiniMax-M3.1", "object": "model", "created": 1750000000}
            ]
        });
        let ids = parse_models_ids(&openai).expect("openai shape");
        assert_eq!(ids.len(), 2);
        assert_eq!(ids[1].id, "MiniMax-M3.1");
        assert_eq!(ids[1].created, Some(1_750_000_000));

        // Gemini projection without the display-name rebuild.
        let gemini = serde_json::json!({
            "models": [
                {"name": "models/gemini-2.5-pro", "displayName": "Gemini 2.5 Pro"},
                {"name": "gemini-2.5-flash", "displayName": "Gemini 2.5 Flash"}
            ]
        });
        let ids = parse_models_ids(&gemini).expect("gemini shape");
        assert_eq!(
            ids.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["gemini-2.5-pro", "gemini-2.5-flash"]
        );
        assert!(ids.iter().all(|m| m.created.is_none()));

        // The malformed-record verdicts must match the full parse exactly.
        for body in [
            serde_json::json!({"error": {"message": "nope"}}),
            serde_json::json!({"models": [{"name": 42}]}),
            serde_json::json!({"models": [{"name": "models/"}]}),
            serde_json::json!({"data": [{"id": "minimax-m3"}, {"id": null}]}),
            serde_json::json!({"data": [{"id": " "}]}),
        ] {
            assert_eq!(
                parse_models_ids(&body).is_none(),
                parse_models_body(&body).is_none(),
                "ids-only verdict drifted for {body}"
            );
        }
    }

    #[test]
    fn fetch_error_strings_match_legacy_codes() {
        assert_eq!(FetchError::Decode.to_string(), "proxy_models_decode_failed");
        assert_eq!(
            FetchError::Http(401).to_string(),
            "proxy_models_http_error_401"
        );
        assert_eq!(
            FetchError::Transport.to_string(),
            "proxy_models_transport_failed"
        );
        assert_eq!(
            FetchError::Unavailable.to_string(),
            "proxy_models_unavailable"
        );
    }

    fn fixture_provider(provider_type: &str, base_url: String) -> ProxyProvider {
        ProxyProvider {
            id: "fixture".into(),
            name: "fixture".into(),
            provider_type: provider_type.into(),
            api_key: "fixture_key".into(),
            base_url,
            models: Vec::new(),
            enabled: true,
            timeout_ms: 60_000,
            weight: 1,
            circuit_breaker: Default::default(),
            model_mapping: Default::default(),
            extra_headers: Default::default(),
            auto_track_models: true,
            icon: None,
            icon_color: None,
        }
    }

    #[tokio::test]
    async fn catalog_requests_use_protocol_auth_and_protected_extra_headers() {
        use axum::{extract::State, http::HeaderMap, routing::get, Json, Router};
        use std::sync::{Arc, Mutex};
        let captured = Arc::new(Mutex::new(Vec::<HeaderMap>::new()));
        let app = Router::new().fallback(get(
            |State(captured): State<Arc<Mutex<Vec<HeaderMap>>>>, headers: HeaderMap| async move {
                captured.lock().unwrap().push(headers);
                Json(serde_json::json!({"data": [{"id": "minimax-m4"}]}))
            },
        )).with_state(captured.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (shutdown, stopped) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = stopped.await;
                })
                .await
                .unwrap();
        });
        let mut outcomes = Vec::new();
        for protocol in ["openai", "anthropic", "gemini"] {
            let mut provider = fixture_provider(protocol, format!("http://{address}"));
            for name in [
                "AUTHORIZATION",
                "X-API-KEY",
                "X-Goog-Api-Key",
                "anthropic-version",
                "host",
                "content-length",
            ] {
                provider
                    .extra_headers
                    .insert(name.into(), "untrusted_override".into());
            }
            provider
                .extra_headers
                .insert("x-opencode-session".into(), "fixture_session".into());
            outcomes.push(fetch_models(&provider, AnthropicAuthScheme::ApiKey).await);
        }
        let _ = shutdown.send(());
        server.await.unwrap();
        for outcome in outcomes {
            assert_eq!(outcome.unwrap().models[0].id, "minimax-m4");
        }
        let requests = captured.lock().unwrap();
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[0]["authorization"], "Bearer fixture_key");
        assert!(requests[0].get("x-api-key").is_none());
        assert!(requests[0].get("x-goog-api-key").is_none());
        assert_eq!(requests[1]["x-api-key"], "fixture_key");
        assert_eq!(requests[1]["anthropic-version"], ANTHROPIC_VERSION);
        assert!(requests[1].get("authorization").is_none());
        assert!(requests[1].get("x-goog-api-key").is_none());
        assert_eq!(requests[2]["x-goog-api-key"], "fixture_key");
        assert!(requests[2].get("authorization").is_none());
        assert!(requests[2].get("x-api-key").is_none());
        for headers in requests.iter() {
            assert_eq!(headers["x-opencode-session"], "fixture_session");
            assert!(headers.values().all(|value| value != "untrusted_override"));
        }
    }

    #[tokio::test]
    async fn invalid_trusted_credentials_fail_before_network_with_redacted_code() {
        for protocol in ["openai", "anthropic", "gemini"] {
            let mut provider = fixture_provider(protocol, "invalid_url".into());
            provider.api_key = "secret\r\ninjected: value".into();
            assert_eq!(
                fetch_models(&provider, AnthropicAuthScheme::ApiKey)
                    .await
                    .unwrap_err()
                    .to_string(),
                "proxy_models_invalid_header"
            );
        }
    }

    #[test]
    fn malformed_catalog_records_fail_the_complete_parse() {
        for body in [
            serde_json::json!({"models": [{"name": "models/gemini-2.5-pro"}, {"displayName": "missing name"}]}),
            serde_json::json!({"models": [{"name": 42}]}),
            serde_json::json!({"models": [{"name": "models/"}]}),
            serde_json::json!({"data": [{"id": "minimax-m3"}, {"id": null}]}),
            serde_json::json!({"data": [{"id": " "}]}),
        ] {
            assert!(parse_models_body(&body).is_none());
        }
        assert!(parse_models_body(&serde_json::json!({"models": []}))
            .unwrap()
            .models
            .is_empty());
    }
}
