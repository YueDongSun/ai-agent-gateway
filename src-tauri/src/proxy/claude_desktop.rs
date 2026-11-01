//! Claude Desktop's isolated gateway surface. Explicit mirror roles retain
//! their routing queues; legacy exact selectors retain their original target.
//! Picker family metadata describes a client role, never upstream identity.
//! Requests never read the live DB.

use super::converter::AnthropicRequest;
use super::server::{self, ProxyAppState};
use axum::extract::{rejection::JsonRejection, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub(crate) const DESKTOP_ROLE_MODELS: [&str; 3] =
    ["claude-sonnet-aag", "claude-opus-aag", "claude-haiku-aag"];
const DESKTOP_MODEL_PREFIX: &str = "claude-sonnet-aag-model-";
const ROUTING_ROLES: &[(&str, &str, &str)] = &[
    ("claude-fable-5", "Fable (AAG routing)", "fable"),
    ("claude-opus-5", "Opus (AAG routing)", "opus"),
    ("claude-sonnet-5", "Sonnet (AAG routing)", "sonnet"),
    ("claude-haiku-5-5", "Haiku (AAG routing)", "haiku"),
];

#[allow(non_camel_case_types)]
struct Desktop_model {
    selector: String,
    actual_model: String,
    display_name: String,
    family: &'static str,
    family_default: bool,
}

fn desktop_catalog(state: &ProxyAppState) -> Result<Vec<Desktop_model>, Box<Response>> {
    if !state.router.mirror_roles_enabled() {
        return exact_catalog(state);
    }
    let pinned = state
        .pinned_provider
        .lock()
        .ok()
        .and_then(|pin| pin.clone());
    let available = state
        .router
        .mirror_role_models(super::router::SURFACE_CLAUDE, pinned.as_deref());
    Ok(ROUTING_ROLES
        .iter()
        .filter(|(selector, _, _)| {
            available
                .iter()
                .any(|available| available.as_str() == *selector)
        })
        .map(|(selector, display_name, family)| Desktop_model {
            selector: (*selector).into(),
            actual_model: (*selector).into(),
            display_name: (*display_name).into(),
            family: *family,
            family_default: true,
        })
        .collect())
}

/// A stable wire identity avoids the native client's other-model-family filter.
/// The full digest is consumed by exact lookup in this admitted catalog; it is
/// never an upstream name, an ordinal mapping, or a content-integrity sidecar.
fn exact_catalog(state: &ProxyAppState) -> Result<Vec<Desktop_model>, Box<Response>> {
    let pinned = state
        .pinned_provider
        .lock()
        .ok()
        .and_then(|pin| pin.clone());
    let mut identities = BTreeMap::new();
    for model in state.router.desktop_models(pinned.as_deref()) {
        let selector = format!(
            "{DESKTOP_MODEL_PREFIX}{}",
            hex::encode(Sha256::digest(model.as_bytes()))
        );
        if identities.insert(selector, model).is_some() {
            return Err(Box::new(desktop_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "desktop_model_selector_collision",
                "The gateway model catalog contains conflicting selectors.",
            )));
        }
    }
    let mut models: Vec<_> = identities
        .into_iter()
        .map(|(selector, actual_model)| Desktop_model {
            selector,
            display_name: actual_model.clone(),
            actual_model,
            family: "sonnet",
            family_default: false,
        })
        .collect();
    let default_model = state.router.desktop_default_model();
    models.sort_by(|left, right| {
        (Some(left.actual_model.as_str()) != default_model)
            .cmp(&(Some(right.actual_model.as_str()) != default_model))
            .then_with(|| left.actual_model.cmp(&right.actual_model))
    });
    if let Some(model) = models.first_mut() {
        model.family_default = true;
    }
    Ok(models)
}

fn desktop_error(status: StatusCode, code: &str, message: &str) -> Response {
    let error_type = match status {
        StatusCode::UNAUTHORIZED => "authentication_error",
        StatusCode::BAD_REQUEST => "invalid_request_error",
        _ => "api_error",
    };
    (
        status,
        Json(json!({
            "type": "error",
            "error": { "type": error_type, "message": message, "code": code }
        })),
    )
        .into_response()
}

fn admit(state: ProxyAppState, headers: &HeaderMap) -> Result<ProxyAppState, Box<Response>> {
    let mut state = state.capture_snapshot();
    // The general loopback API can explicitly disable authentication. Desktop
    // profiles always carry a static AAG token, so this surface fails closed.
    if state.auth_token.trim().is_empty() {
        return Err(Box::new(desktop_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "gateway_auth_not_configured",
            "Configure an AAG gateway token before using Claude Desktop.",
        )));
    }
    if server::check_auth(headers, &state.auth_token).is_err() {
        return Err(Box::new(desktop_error(
            StatusCode::UNAUTHORIZED,
            "authentication_failed",
            "Claude Desktop requires the AAG gateway token.",
        )));
    }
    // Reused Anthropic handlers must neither admit OAuth nor attach any inbound
    // credential for upstream passthrough, including the AAG token itself.
    state.passthrough_auth_allowed = false;
    Ok(state)
}

fn selected_model(state: &ProxyAppState, selector: &str) -> Result<String, Box<Response>> {
    let catalog = desktop_catalog(state)?;
    if state.router.mirror_roles_enabled() {
        let selector = match selector {
            "claude-sonnet-aag" => "claude-sonnet-5",
            "claude-opus-aag" => "claude-opus-5",
            "claude-haiku-aag" => "claude-haiku-5-5",
            other => other,
        };
        if catalog.iter().any(|model| model.selector == selector) {
            // Keep the role intact until the router admits the request, so
            // retriable quota failures can advance through the full queue.
            return Ok(selector.into());
        }
        if selector.starts_with(DESKTOP_MODEL_PREFIX) {
            if let Some(model) = exact_catalog(state)?
                .into_iter()
                .find(|model| model.selector == selector)
            {
                return Ok(model.actual_model);
            }
        }
        let unavailable = ROUTING_ROLES.iter().any(|(role, _, _)| *role == selector)
            || selector.starts_with(DESKTOP_MODEL_PREFIX);
        return Err(Box::new(desktop_error(
            if unavailable {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::BAD_REQUEST
            },
            if unavailable {
                "desktop_model_unavailable"
            } else {
                "desktop_model_selector_invalid"
            },
            "Select an available model from the Claude Desktop gateway model list.",
        )));
    }
    // Older conversations keep their role selector. New discovery presents
    // actual choices, so selecting a model no longer means selecting a role.
    if DESKTOP_ROLE_MODELS.contains(&selector) {
        // The admitted catalog orders the configured default first when it
        // remains eligible. A pin may exclude it; use that catalog's first
        // available model rather than reviving an ineligible default.
        return catalog
            .into_iter()
            .next()
            .map(|model| model.actual_model)
            .ok_or_else(|| {
                Box::new(desktop_error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "desktop_supply_unavailable",
                    "Add an enabled unified provider and model in AAG before using Claude Desktop.",
                ))
            });
    }
    if let Some(model) = catalog.into_iter().find(|model| model.selector == selector) {
        return Ok(model.actual_model);
    }
    let unavailable = selector.starts_with(DESKTOP_MODEL_PREFIX);
    Err(Box::new(desktop_error(
        if unavailable {
            StatusCode::NOT_FOUND
        } else {
            StatusCode::BAD_REQUEST
        },
        if unavailable {
            "desktop_model_unavailable"
        } else {
            "desktop_model_selector_invalid"
        },
        "Select an available model from the Claude Desktop gateway model list.",
    )))
}

pub(super) async fn list_models(
    State(state): State<ProxyAppState>,
    headers: HeaderMap,
) -> Response {
    let state = match admit(state, &headers) {
        Ok(state) => state,
        Err(response) => return *response,
    };
    let models = match desktop_catalog(&state) {
        Ok(models) => models,
        Err(response) => return *response,
    };
    let data: Vec<_> = models
        .into_iter()
        .map(|model| {
            json!({
                "id": model.selector,
                "type": "model",
                "display_name": model.display_name,
                "created_at": "2026-01-01T00:00:00Z",
                "anthropic_family_tier": model.family,
                "is_family_default": model.family_default
            })
        })
        .collect();
    log::info!("desktop_models: catalog_ready count={}", data.len());
    Json(json!({
        "first_id": data.first().and_then(|model| model.get("id")),
        "last_id": data.last().and_then(|model| model.get("id")),
        "data": data,
        "has_more": false
    }))
    .into_response()
}

pub(super) async fn messages(
    State(state): State<ProxyAppState>,
    headers: HeaderMap,
    payload: Result<Json<AnthropicRequest>, JsonRejection>,
) -> Response {
    let state = match admit(state, &headers) {
        Ok(state) => state,
        Err(response) => return *response,
    };
    let mut body = match payload {
        Ok(Json(body)) => body,
        Err(_) => {
            return desktop_error(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "Invalid request body.",
            )
        }
    };
    body.model = match selected_model(&state, &body.model) {
        Ok(model) => model,
        Err(response) => return *response,
    };
    // capture_snapshot cleared the live handle at admission. The canonical
    // handler therefore reuses precisely this generation for the entire call.
    server::messages(State(state), headers, Ok(Json(body))).await
}

pub(super) async fn count_tokens(
    State(state): State<ProxyAppState>,
    headers: HeaderMap,
    payload: Result<Json<Value>, JsonRejection>,
) -> Response {
    let state = match admit(state, &headers) {
        Ok(state) => state,
        Err(response) => return *response,
    };
    let mut body = match payload {
        Ok(Json(body)) => body,
        Err(_) => {
            return desktop_error(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "Invalid request body.",
            )
        }
    };
    let model = match selected_model(
        &state,
        body.get("model")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    ) {
        Ok(model) => model,
        Err(response) => return *response,
    };
    body["model"] = Value::String(model);
    server::messages_count_tokens(State(state), headers, Ok(Json(body))).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::dao::proxy_providers::CircuitBreakerConfig;
    use crate::proxy::canonical::{
        ChatCompletionRequest, Client_credential, OAIChatResponse, OAIStreamChunk, ProviderError,
    };
    use crate::proxy::circuit_breaker::CircuitBreaker;
    use crate::proxy::providers::{mock::MockProvider, Provider};
    use crate::proxy::router::Router;
    use async_trait::async_trait;
    use axum::{body::Body, http::Request};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use tower::Service;

    const GATEWAY_TOKEN: &str = "fixture_gateway_token";
    const ACTUAL_MODEL: &str = "configured-upstream-model";

    #[derive(Default)]
    #[allow(non_camel_case_types)]
    struct Observed_requests {
        messages: Vec<(String, ChatCompletionRequest)>,
        counts: Vec<Value>,
    }

    #[allow(non_camel_case_types)]
    struct Recording_provider {
        inner: MockProvider,
        observed: Arc<Mutex<Observed_requests>>,
        passthrough: bool,
    }

    #[async_trait]
    impl Provider for Recording_provider {
        fn id(&self) -> &str {
            self.inner.id()
        }
        fn enabled(&self) -> bool {
            self.inner.enabled()
        }
        fn models(&self) -> &[String] {
            self.inner.models()
        }
        fn breaker(&self) -> &CircuitBreaker {
            self.inner.breaker()
        }
        fn requires_client_credential(&self) -> bool {
            self.passthrough
        }

        async fn complete(
            &self,
            request: &ChatCompletionRequest,
            model: &str,
        ) -> Result<OAIChatResponse, ProviderError> {
            self.observed
                .lock()
                .unwrap()
                .messages
                .push((model.to_string(), request.clone()));
            self.inner.complete(request, model).await
        }

        async fn stream(
            &self,
            request: &ChatCompletionRequest,
            model: &str,
        ) -> Result<futures_util::stream::BoxStream<'static, OAIStreamChunk>, ProviderError>
        {
            self.observed
                .lock()
                .unwrap()
                .messages
                .push((model.to_string(), request.clone()));
            self.inner.stream(request, model).await
        }

        async fn count_tokens(
            &self,
            body: Value,
            credential: Option<&Client_credential>,
        ) -> Result<u32, ProviderError> {
            assert!(
                credential.is_none(),
                "Desktop must not relay inbound credentials"
            );
            self.observed.lock().unwrap().counts.push(body);
            Ok(37)
        }
    }

    fn state(with_supply: bool) -> (ProxyAppState, Arc<Mutex<Observed_requests>>) {
        catalog_state(if with_supply { &[ACTUAL_MODEL] } else { &[] })
    }

    fn catalog_state(models: &[&str]) -> (ProxyAppState, Arc<Mutex<Observed_requests>>) {
        catalog_state_with_roles(models, false)
    }

    fn routing_state(models: &[&str]) -> (ProxyAppState, Arc<Mutex<Observed_requests>>) {
        catalog_state_with_roles(models, true)
    }

    fn catalog_state_with_roles(
        models: &[&str],
        roles_enabled: bool,
    ) -> (ProxyAppState, Arc<Mutex<Observed_requests>>) {
        let observed = Arc::new(Mutex::new(Observed_requests::default()));
        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        if !models.is_empty() {
            providers.insert(
                "selected-provider".into(),
                Arc::new(Recording_provider {
                    inner: MockProvider::new(
                        "selected-provider".into(),
                        models.iter().map(|model| (*model).to_string()).collect(),
                        true,
                        CircuitBreakerConfig::default(),
                    ),
                    observed: Arc::clone(&observed),
                    passthrough: false,
                }),
            );
        }
        let role_routes = models
            .first()
            .map(|model| {
                ROUTING_ROLES
                    .iter()
                    .map(|(role, _, _)| {
                        (
                            (*role).to_string(),
                            vec![crate::database::dao::routes::RouteHop {
                                provider: "selected-provider".into(),
                                model: (*model).to_string(),
                            }],
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let router = Router::new(providers, HashMap::new())
            .with_desktop_default_model(models.first().map(|model| (*model).to_string()))
            .with_mirror_role_routes(
                roles_enabled,
                HashMap::from([(crate::proxy::router::SURFACE_CLAUDE.into(), role_routes)]),
            );
        (
            ProxyAppState {
                router: Arc::new(router),
                auth_token: GATEWAY_TOKEN.into(),
                pinned_provider: Arc::new(Mutex::new(None)),
                responses_cot_enabled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                passthrough_auth_allowed: true,
                db: None,
                live: None,
                allow_client_config_repair: false,
            },
            observed,
        )
    }

    async fn request(
        state: ProxyAppState,
        path: &str,
        token: Option<&str>,
        body: Option<Value>,
    ) -> Response {
        let mut app = server::build_router(state);
        let mut builder = Request::builder().uri(path);
        if let Some(token) = token {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        let request = match body {
            Some(body) => builder
                .method("POST")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
            None => builder.body(Body::empty()).unwrap(),
        };
        app.call(request).await.unwrap()
    }

    async fn json_body(response: Response) -> Value {
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    fn message_body(role: &str) -> Value {
        json!({
            "model": role, "max_tokens": 32,
            "messages": [{ "role": "user", "content": "Return one word." }],
            "stream": false
        })
    }

    #[tokio::test]
    async fn routing_discovery_exposes_client_roles_without_claiming_upstream_identity() {
        let (state, _) = routing_state(&["MiniMax-M3.1-Flash"]);
        let response = request(
            state,
            "/claude-desktop/v1/models",
            Some(GATEWAY_TOKEN),
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let catalog = json_body(response).await;
        let entries = catalog["data"].as_array().unwrap();
        assert_eq!(entries.len(), ROUTING_ROLES.len());
        for (entry, (selector, label, family)) in entries.iter().zip(ROUTING_ROLES) {
            assert_eq!(entry["id"], *selector);
            assert_eq!(entry["display_name"], *label);
            assert_eq!(entry["anthropic_family_tier"], *family);
            assert_eq!(entry["is_family_default"], true);
        }
    }

    #[tokio::test]
    async fn routing_roles_reach_the_router_intact_and_dispatch_actual_models() {
        let (state, observed) = routing_state(&["MiniMax-M3.1-Flash"]);
        for (selector, _, _) in ROUTING_ROLES {
            assert_eq!(selected_model(&state, selector).unwrap(), *selector);
            let response = request(
                state.clone(),
                "/claude-desktop/v1/messages",
                Some(GATEWAY_TOKEN),
                Some(message_body(selector)),
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
            let response = json_body(response).await;
            assert!(response["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("MiniMax-M3.1-Flash"));
        }
        let observed = observed.lock().unwrap();
        assert_eq!(observed.messages.len(), ROUTING_ROLES.len());
        assert!(
            observed
                .messages
                .iter()
                .all(|(model, body)| model == "MiniMax-M3.1-Flash"
                    && body.client_credential.is_none())
        );
    }

    #[tokio::test]
    async fn routing_mode_preserves_existing_exact_and_legacy_role_conversations() {
        let (exact, _) = catalog_state(&[ACTUAL_MODEL]);
        let exact_selector = exact_catalog(&exact).unwrap().remove(0).selector;
        let (routing, observed) = routing_state(&[ACTUAL_MODEL]);
        for (old, new) in [
            ("claude-sonnet-aag", "claude-sonnet-5"),
            ("claude-opus-aag", "claude-opus-5"),
            ("claude-haiku-aag", "claude-haiku-5-5"),
        ] {
            assert_eq!(selected_model(&routing, old).unwrap(), new);
        }
        let response = request(
            routing,
            "/claude-desktop/v1/messages",
            Some(GATEWAY_TOKEN),
            Some(message_body(&exact_selector)),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(observed.lock().unwrap().messages[0].0, ACTUAL_MODEL);
    }

    #[tokio::test]
    async fn routing_roles_require_auth_and_remain_unavailable_when_pin_has_no_supply() {
        let (state, observed) = routing_state(&[ACTUAL_MODEL]);
        for (path, body) in [
            ("/claude-desktop/v1/models", None),
            (
                "/claude-desktop/v1/messages",
                Some(message_body("claude-fable-5")),
            ),
        ] {
            let response = request(state.clone(), path, None, body).await;
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }
        *state.pinned_provider.lock().unwrap() = Some("unavailable-provider".into());
        let catalog = json_body(
            request(
                state.clone(),
                "/claude-desktop/v1/models",
                Some(GATEWAY_TOKEN),
                None,
            )
            .await,
        )
        .await;
        assert_eq!(catalog["data"], json!([]));
        let response = request(
            state,
            "/claude-desktop/v1/messages",
            Some(GATEWAY_TOKEN),
            Some(message_body("claude-fable-5")),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(observed.lock().unwrap().messages.is_empty());
    }

    #[tokio::test]
    async fn desktop_discovery_lists_real_models_and_routes_each_selection_exactly() {
        let (state, observed) = catalog_state(&["gpt-fixture", "deepseek-fixture"]);
        let response = request(
            state.clone(),
            "/claude-desktop/v1/models?limit=1000",
            Some(GATEWAY_TOKEN),
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let catalog = json_body(response).await;
        let entries = catalog["data"].as_array().unwrap();
        assert_eq!(entries.len(), 2);
        for actual in ["gpt-fixture", "deepseek-fixture"] {
            let entry = entries
                .iter()
                .find(|entry| entry["display_name"] == actual)
                .expect("actual model name must be shown");
            let selector = entry["id"].as_str().unwrap();
            assert!(selector.starts_with("claude-sonnet-aag-model-"));
            assert!(selector.len() <= 255);
            assert!(!selector.contains("gpt") && !selector.contains("deepseek"));
            assert_eq!(entry["anthropic_family_tier"], "sonnet");
            let response = request(
                state.clone(),
                "/claude-desktop/v1/messages",
                Some(GATEWAY_TOKEN),
                Some(message_body(selector)),
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
            let output = json_body(response).await;
            assert!(output["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains(actual));
        }
        assert_eq!(
            entries
                .iter()
                .filter(|entry| entry["is_family_default"] == true)
                .count(),
            1
        );
        let requests = observed.lock().unwrap();
        assert_eq!(requests.messages.len(), 2);
        assert_eq!(requests.messages[0].0, "gpt-fixture");
        assert_eq!(requests.messages[1].0, "deepseek-fixture");
        assert!(requests
            .messages
            .iter()
            .all(|(_, body)| body.client_credential.is_none()));
    }

    #[tokio::test]
    async fn desktop_model_selectors_survive_reordering_but_never_retarget_deleted_models() {
        let (first, _) = catalog_state(&["gpt-fixture", "deepseek-fixture"]);
        let catalog = json_body(
            request(
                first.clone(),
                "/claude-desktop/v1/models",
                Some(GATEWAY_TOKEN),
                None,
            )
            .await,
        )
        .await;
        let selector = catalog["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["display_name"] == "gpt-fixture")
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let (reordered, reordered_requests) = catalog_state(&["deepseek-fixture", "gpt-fixture"]);
        let response = request(
            reordered,
            "/claude-desktop/v1/messages",
            Some(GATEWAY_TOKEN),
            Some(message_body(&selector)),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            reordered_requests.lock().unwrap().messages[0].0,
            "gpt-fixture"
        );
        let (removed, removed_requests) = catalog_state(&["deepseek-fixture"]);
        let response = request(
            removed,
            "/claude-desktop/v1/messages",
            Some(GATEWAY_TOKEN),
            Some(message_body(&selector)),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(removed_requests.lock().unwrap().messages.is_empty());
        let response = request(
            first,
            "/claude-desktop/v1/messages",
            Some("fixture_oauth_credential"),
            Some(message_body(&selector)),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn desktop_discovery_supports_long_model_ids_without_truncation() {
        let long_model = format!("gpt-{}", "x".repeat(252));
        let (state, observed) = catalog_state(&[&long_model]);
        let catalog = json_body(
            request(
                state.clone(),
                "/claude-desktop/v1/models",
                Some(GATEWAY_TOKEN),
                None,
            )
            .await,
        )
        .await;
        assert_eq!(catalog["data"][0]["display_name"], long_model);
        let selector = catalog["data"][0]["id"].as_str().unwrap();
        assert!(selector.len() <= 255);
        let response = request(
            state,
            "/claude-desktop/v1/messages",
            Some(GATEWAY_TOKEN),
            Some(message_body(selector)),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(observed.lock().unwrap().messages[0].0, long_model);
    }

    #[tokio::test]
    async fn compatibility_roles_route_to_unified_model_without_appearing_in_discovery() {
        let (state, observed) = state(true);
        let response = request(
            state.clone(),
            "/claude-desktop/v1/models",
            Some(GATEWAY_TOKEN),
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let models = json_body(response).await;
        let ids: Vec<_> = models["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|model| model["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids.len(), 1);
        assert!(ids.iter().all(|id| !DESKTOP_ROLE_MODELS.contains(id)));
        assert_eq!(models["data"][0]["display_name"], ACTUAL_MODEL);
        assert_eq!(models["has_more"], false);
        for role in DESKTOP_ROLE_MODELS {
            let response = request(
                state.clone(),
                "/claude-desktop/v1/messages",
                Some(GATEWAY_TOKEN),
                Some(message_body(role)),
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
            let result = json_body(response).await;
            assert!(result["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("MOCK[selected-provider] :: configured-upstream-model"));
        }
        let observed = observed.lock().unwrap();
        assert_eq!(observed.messages.len(), 3);
        for (model, body) in &observed.messages {
            assert_eq!(model, ACTUAL_MODEL);
            assert!(body.client_credential.is_none());
        }
    }

    #[tokio::test]
    async fn desktop_requires_aag_auth_even_when_normal_anthropic_surface_allows_oauth() {
        let (state, observed) = state(true);
        for token in [None, Some("fixture_oauth_credential")] {
            for (path, body) in [
                ("/claude-desktop/v1/models", None),
                (
                    "/claude-desktop/v1/messages",
                    Some(message_body("claude-sonnet-aag")),
                ),
                (
                    "/claude-desktop/v1/messages/count_tokens",
                    Some(message_body("claude-sonnet-aag")),
                ),
            ] {
                let response = request(state.clone(), path, token, body).await;
                assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
                assert_eq!(
                    json_body(response).await["error"]["type"],
                    "authentication_error"
                );
            }
        }
        assert!(observed.lock().unwrap().messages.is_empty());
        assert!(observed.lock().unwrap().counts.is_empty());
        let mut no_auth = state;
        no_auth.auth_token.clear();
        let response = request(no_auth, "/claude-desktop/v1/models", None, None).await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn empty_supply_and_raw_model_calls_do_not_fabricate_desktop_models() {
        let (empty, _) = state(false);
        let models = json_body(
            request(
                empty.clone(),
                "/claude-desktop/v1/models",
                Some(GATEWAY_TOKEN),
                None,
            )
            .await,
        )
        .await;
        assert_eq!(models["data"], json!([]));
        assert_eq!(models["first_id"], Value::Null);
        let response = request(
            empty,
            "/claude-desktop/v1/messages",
            Some(GATEWAY_TOKEN),
            Some(message_body("claude-sonnet-aag")),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let (state, observed) = state(true);
        let response = request(
            state,
            "/claude-desktop/v1/messages",
            Some(GATEWAY_TOKEN),
            Some(message_body(ACTUAL_MODEL)),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(observed.lock().unwrap().messages.is_empty());
    }

    #[tokio::test]
    async fn count_tokens_reuses_routing_and_preserves_body_after_role_remap() {
        let (state, observed) = state(true);
        let catalog = json_body(
            request(
                state.clone(),
                "/claude-desktop/v1/models",
                Some(GATEWAY_TOKEN),
                None,
            )
            .await,
        )
        .await;
        let selector = catalog["data"][0]["id"].as_str().unwrap();
        let body = json!({
            "model": selector,
            "messages": [{"role":"assistant","content":[{"type":"thinking","thinking":"fixture","signature":"fixture_signature"}]}],
            "tools": [{"name":"fixture_tool","input_schema":{"type":"object"}}],
            "thinking": {"type":"enabled","budget_tokens":1024}
        });
        let response = request(
            state,
            "/claude-desktop/v1/messages/count_tokens",
            Some(GATEWAY_TOKEN),
            Some(body.clone()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(json_body(response).await, json!({"input_tokens":37}));
        let observed = observed.lock().unwrap();
        assert_eq!(observed.counts.len(), 1);
        let mut expected = body;
        expected["model"] = ACTUAL_MODEL.into();
        assert_eq!(observed.counts[0], expected);
    }

    #[tokio::test]
    async fn desktop_catalog_excludes_oauth_only_routes_and_respects_the_admitted_pin() {
        use crate::database::dao::routes::RouteHop;
        let (mut state, observed) = catalog_state(&["gpt-fixture", "deepseek-fixture"]);
        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        providers.insert(
            "selected-provider".into(),
            Arc::new(Recording_provider {
                inner: MockProvider::new(
                    "selected-provider".into(),
                    vec!["gpt-fixture".into(), "deepseek-fixture".into()],
                    true,
                    CircuitBreakerConfig::default(),
                ),
                observed: Arc::clone(&observed),
                passthrough: false,
            }),
        );
        providers.insert(
            "oauth-provider".into(),
            Arc::new(Recording_provider {
                inner: MockProvider::new(
                    "oauth-provider".into(),
                    vec!["gpt-fixture".into(), "oauth-only".into()],
                    true,
                    CircuitBreakerConfig::default(),
                ),
                observed: Arc::clone(&observed),
                passthrough: true,
            }),
        );
        providers.insert(
            "disabled-provider".into(),
            Arc::new(MockProvider::new(
                "disabled-provider".into(),
                vec!["disabled-model".into()],
                false,
                CircuitBreakerConfig::default(),
            )),
        );
        let routes = [(
            "gpt-fixture".into(),
            vec![RouteHop {
                provider: "oauth-provider".into(),
                model: "gpt-fixture".into(),
            }],
        )]
        .into_iter()
        .collect();
        state.router = Arc::new(
            Router::new(providers, routes).with_desktop_default_model(Some("gpt-fixture".into())),
        );
        let catalog = json_body(
            request(
                state.clone(),
                "/claude-desktop/v1/models",
                Some(GATEWAY_TOKEN),
                None,
            )
            .await,
        )
        .await;
        assert_eq!(catalog["data"].as_array().unwrap().len(), 1);
        assert_eq!(catalog["data"][0]["display_name"], "deepseek-fixture");
        let selector = catalog["data"][0]["id"].as_str().unwrap();
        let response = request(
            state.clone(),
            "/claude-desktop/v1/messages",
            Some(GATEWAY_TOKEN),
            Some(message_body(selector)),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(observed.lock().unwrap().messages[0].0, "deepseek-fixture");
        *state.pinned_provider.lock().unwrap() = Some("oauth-provider".into());
        let empty = json_body(
            request(
                state.clone(),
                "/claude-desktop/v1/models",
                Some(GATEWAY_TOKEN),
                None,
            )
            .await,
        )
        .await;
        assert_eq!(empty["data"], json!([]));
        let response = request(
            state,
            "/claude-desktop/v1/messages",
            Some(GATEWAY_TOKEN),
            Some(message_body(selector)),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(observed.lock().unwrap().messages.len(), 1);
    }

    #[tokio::test]
    async fn legacy_roles_use_the_first_admitted_model_when_pin_excludes_default() {
        let (mut state, observed) = catalog_state(&["default-model"]);
        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        for (provider_id, model) in [
            ("default-provider", "default-model"),
            ("pinned-provider", "pinned-model"),
        ] {
            providers.insert(
                provider_id.into(),
                Arc::new(Recording_provider {
                    inner: MockProvider::new(
                        provider_id.into(),
                        vec![model.into()],
                        true,
                        CircuitBreakerConfig::default(),
                    ),
                    observed: Arc::clone(&observed),
                    passthrough: false,
                }),
            );
        }
        state.router = Arc::new(
            Router::new(providers, HashMap::new())
                .with_desktop_default_model(Some("default-model".into())),
        );
        *state.pinned_provider.lock().unwrap() = Some("pinned-provider".into());
        let catalog = json_body(
            request(
                state.clone(),
                "/claude-desktop/v1/models",
                Some(GATEWAY_TOKEN),
                None,
            )
            .await,
        )
        .await;
        assert_eq!(catalog["data"].as_array().unwrap().len(), 1);
        assert_eq!(catalog["data"][0]["display_name"], "pinned-model");
        for role in DESKTOP_ROLE_MODELS {
            let response = request(
                state.clone(),
                "/claude-desktop/v1/messages",
                Some(GATEWAY_TOKEN),
                Some(message_body(role)),
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
            let response = json_body(response).await;
            assert!(response["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("MOCK[pinned-provider] :: pinned-model"));
        }
        let observed = observed.lock().unwrap();
        assert_eq!(observed.messages.len(), 3);
        assert!(observed.messages.iter().all(|(model, request)| {
            model == "pinned-model" && request.client_credential.is_none()
        }));
    }

    #[tokio::test]
    async fn desktop_stream_retains_anthropic_events_and_source_blocks() {
        let (state, observed) = state(true);
        let mut body = message_body("claude-opus-aag");
        body["stream"] = true.into();
        body["thinking"] = json!({"type":"enabled","budget_tokens":1024});
        body["messages"] = json!([
            {"role":"assistant","content":[{"type":"thinking","thinking":"fixture","signature":"fixture_signature"}]},
            {"role":"user","content":"Return one word."}
        ]);
        let response = request(
            state,
            "/claude-desktop/v1/messages",
            Some(GATEWAY_TOKEN),
            Some(body.clone()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["content-type"], "text/event-stream");
        let stream = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let stream = String::from_utf8(stream.to_vec()).unwrap();
        assert!(stream.contains("event: message_start"));
        assert!(stream.contains("event: message_stop"));
        let observed = observed.lock().unwrap();
        assert_eq!(observed.messages.len(), 1);
        let request = &observed.messages[0].1;
        assert!(request.client_credential.is_none());
        let source = request
            .source_anthropic_request
            .as_ref()
            .expect("source request retained");
        assert_eq!(source["messages"], body["messages"]);
        assert_eq!(source["thinking"], body["thinking"]);
        assert_eq!(source["model"], ACTUAL_MODEL);
    }
}
