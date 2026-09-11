//! Axum HTTP server — the local proxy.
//!
//! Routes:
//! - `GET  /health`         — provider health + model list
//! - `GET  /v1/models`      — list model aliases
//! - `POST /v1/chat/completions` — OpenAI-compatible non-streaming completion
//!
//! Ported from `src/gateway/server.ts` (Hono). Uses axum 0.7.

use crate::proxy::canonical::ChatCompletionRequest;
use crate::proxy::converter::{anthropic_req_to_oai_req, oai_chunks_to_anthropic_stream, oai_resp_to_anthropic_resp, AnthropicRequest};
use crate::proxy::router::Router;
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json, Response},
    routing::{get, post},
    Router as AxumRouter,
};
use futures_util::StreamExt;
use serde_json::json;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

/// Shared state for the proxy server. `Arc<Router>` is built once at start
/// time from the DB snapshot.
#[derive(Clone)]
pub struct ProxyAppState {
    pub router: Arc<Router>,
    pub auth_token: String,
}

pub async fn serve(
    bind_addr: std::net::SocketAddr,
    state: ProxyAppState,
    shutdown_rx: oneshot::Receiver<()>,
) -> Result<(), String> {
    let app = build_router(state);
    let listener = TcpListener::bind(bind_addr)
        .await
        .map_err(|e| format!("bind {bind_addr}: {e}"))?;
    log::info!("proxy: listening on http://{}", bind_addr);
    axum::serve(listener, app.into_make_service())
        .with_graceful_shutdown(async move {
            let _ = shutdown_rx.await;
            log::info!("proxy: graceful shutdown signal received");
        })
        .await
        .map_err(|e| format!("server error: {e}"))?;
    Ok(())
}

fn build_router(state: ProxyAppState) -> AxumRouter {
    AxumRouter::new()
        .route("/health", get(health))
        .route("/v1/models", get(list_models))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/messages", post(messages))
        .layer(tower_http::cors::CorsLayer::permissive())
        .with_state(state)
}

/// Check Bearer / x-api-key auth. Returns Ok(()) if auth disabled or matches.
fn check_auth(headers: &HeaderMap, expected: &str) -> Result<(), Response> {
    if expected.is_empty() {
        return Ok(());
    }
    let provided = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim_start_matches("Bearer ").trim().to_string())
        .or_else(|| {
            headers
                .get("x-api-key")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string())
        });
    match provided {
        Some(t) if t == expected => Ok(()),
        _ => Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": {"message": "invalid auth token", "type": "auth_error"}})),
        )
            .into_response()),
    }
}

async fn health(State(state): State<ProxyAppState>) -> impl IntoResponse {
    Json(json!({
        "status": "ok",
        "providers": state.router.health(),
        "models": state.router.list_models(),
    }))
}

/// Model list in a shape both SDKs accept: Anthropic Models API fields
/// (`type`, `display_name`, `created_at`, envelope `has_more`/`first_id`/
/// `last_id`) plus the OpenAI fields (`object`, `created`, `owned_by`) kept
/// for back-compat with existing clients. Unknown SDKs ignore extra fields.
async fn list_models(State(state): State<ProxyAppState>) -> impl IntoResponse {
    let mut ids = state.router.list_models();
    ids.sort();
    let data: Vec<_> = ids.iter().map(|id| {
        let display_name = crate::proxy::models::display_name_for(id)
            .map(|s| s.to_string())
            .unwrap_or_else(|| id.clone());
        json!({
            "id": id,
            "type": "model",
            "display_name": display_name,
            "created_at": "2026-01-01T00:00:00Z",
            "object": "model",
            "created": 1_700_000_000,
            "owned_by": "ai-agent-gateway",
        })
    }).collect();
    Json(json!({
        "object": "list",
        "data": data,
        "has_more": false,
        "first_id": ids.first().cloned(),
        "last_id": ids.last().cloned(),
    }))
}

async fn chat_completions(
    State(state): State<ProxyAppState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Response {
    if let Err(resp) = check_auth(&headers, &state.auth_token) {
        return resp;
    }
    let req: ChatCompletionRequest = match serde_json::from_value(body) {
        Ok(r) => r,
        Err(e) => return err_response(400, format!("invalid JSON body: {e}"), "invalid_request_error"),
    };
    if req.model.is_empty() || req.messages.is_empty() {
        return err_response(400, "model and messages are required", "invalid_request_error");
    }
    if req.stream {
        return stream_chat_completions(state, req).await;
    }
    match state.router.complete(&req).await {
        Ok(resp) => match serde_json::to_value(resp) {
            Ok(v) => Json(v).into_response(),
            Err(e) => err_response(500, format!("serialize error: {e}"), "internal_error"),
        },
        Err(e) => {
            let status = e.status.unwrap_or(500);
            err_response(status, e.message, if status >= 500 { "proxy_error" } else { "invalid_request_error" })
        }
    }
}

async fn stream_chat_completions(state: ProxyAppState, req: ChatCompletionRequest) -> Response {
    let stream = match state.router.stream(&req).await {
        Ok(s) => s,
        Err(e) => {
            let status = e.status.unwrap_or(500);
            return err_response(status, e.message, if status >= 500 { "proxy_error" } else { "invalid_request_error" });
        }
    };
    use futures_util::stream::StreamExt;

    // Map each upstream OAI chunk to an SSE `data: <json>\n\n` frame, then
    // append the OpenAI-spec `[DONE]` sentinel once the upstream stream
    // ends. This matches cc-switch's behavior and the Node
    // `tests/e2e.test.ts` assertion that the body ends with `data: [DONE]`.
    let mapped = stream.map(|chunk| {
        let json = serde_json::to_string(&chunk).unwrap_or("{}".into());
        let bytes = format!("data: {}\n\n", json);
        Result::<bytes::Bytes, std::io::Error>::Ok(bytes.into())
    });
    let with_done = mapped.chain(futures_util::stream::once(async {
        Result::<bytes::Bytes, std::io::Error>::Ok("data: [DONE]\n\n".into())
    }));
    let body = axum::body::Body::from_stream(with_done);
    Response::builder()
        .status(200)
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .header("connection", "keep-alive")
        .body(body)
        .unwrap()
}

async fn messages(
    State(state): State<ProxyAppState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Response {
    if let Err(resp) = check_auth(&headers, &state.auth_token) {
        return resp;
    }
    let ant_req: AnthropicRequest = match serde_json::from_value(body) {
        Ok(r) => r,
        Err(e) => return err_response(400, format!("invalid JSON body: {e}"), "invalid_request_error"),
    };
    let oai_req = anthropic_req_to_oai_req(&ant_req);
    let requested_model = ant_req.model.clone();
    if ant_req.stream == Some(true) {
        let stream = match state.router.stream(&oai_req).await {
            Ok(s) => s,
            Err(e) => {
                let status = e.status.unwrap_or(500);
                return err_response(status, e.message, "api_error");
            }
        };
        let model = requested_model.clone();
        let ant_stream = oai_chunks_to_anthropic_stream(stream, model);
        let body = axum::body::Body::from_stream(ant_stream.map(|sse| Result::<bytes::Bytes, std::io::Error>::Ok(sse.into_bytes().into())));
        return Response::builder()
            .status(200)
            .header("content-type", "text/event-stream")
            .header("cache-control", "no-cache")
            .header("connection", "keep-alive")
            .body(body)
            .unwrap();
    }
    let oai_resp = match state.router.complete(&oai_req).await {
        Ok(r) => r,
        Err(e) => {
            let status = e.status.unwrap_or(500);
            return err_response(status, e.message, "api_error");
        }
    };
    let ant_resp = oai_resp_to_anthropic_resp(&oai_resp, &requested_model);
    match serde_json::to_value(ant_resp) {
        Ok(v) => Json(v).into_response(),
        Err(e) => err_response(500, format!("serialize error: {e}"), "internal_error"),
    }
}


fn err_response(status: u16, message: impl Into<String>, kind: &str) -> Response {
    let body = json!({ "error": { "message": message.into(), "type": kind } });
    (StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR), Json(body)).into_response()
}

/// Bind a socket address from a host:port pair. Convenience for tests.
pub fn socket_addr(host: &str, port: u16) -> std::net::SocketAddr {
    format!("{host}:{port}").parse().expect("valid socket addr")
}

