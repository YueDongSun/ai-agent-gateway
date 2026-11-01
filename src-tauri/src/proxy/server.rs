//! Axum HTTP server — the local proxy.
//!
//! Routes:
//! - `GET  /health`         — provider health + model list
//! - `GET  /v1/models`      — list model aliases
//! - `POST /v1/chat/completions` — OpenAI-compatible non-streaming completion
//! - `POST /v1/messages/count_tokens` — Anthropic surface token counting
//!   (upstream relay on Anthropic hops, local estimate elsewhere)
//!
//! Ported from `src/gateway/server.ts` (Hono). Uses axum 0.7.

use crate::proxy::canonical::{ChatCompletionRequest, OAIStreamChunk, UpstreamErrorKind};
use crate::proxy::converter::{
    anthropic_req_to_oai_req, chat_chunks_to_responses_stream_with_namespaces,
    chat_to_responses_with_namespaces, oai_chunks_to_anthropic_stream, oai_resp_to_anthropic_resp,
    responses_to_chat, AnthropicRequest, Responses_namespace_map,
};
use crate::proxy::gemini::{
    gemini_req_to_oai_req, oai_chunks_to_gemini_stream, oai_resp_to_gemini_resp,
};
use crate::proxy::responses::ResponsesRequest;
use crate::proxy::router::Router;
use axum::{
    extract::{rejection::JsonRejection, DefaultBodyLimit, Path, Query, Request, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Json, Response},
    routing::{any, get, post},
    Router as AxumRouter,
};
use futures_util::stream::BoxStream;
use futures_util::StreamExt;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tower_http::cors::{AllowOrigin, CorsLayer};

/// Trigger the Codex mirror's dead-session auto-heal in response to an
/// upstream `AuthExpired` 4xx. Reads the `codex_cli_home` setting via
/// the proxy's database handle (if attached). The heal itself is a
/// per-process one-shot and refuses to touch the native `~/.codex`, so
/// repeated calls are safe.
fn maybe_heal_codex_mirror(state: &ProxyAppState) {
    if !state.allow_client_config_repair {
        return;
    }
    let Some(db) = state.db.as_ref() else { return };
    if !matches!(
        crate::services::session_authority::client_config::managed_launch_required(db),
        Ok(false)
    ) {
        return;
    }
    let Ok(codex_cli_home) = db.get_setting(crate::database::dao::settings::KEY_CODEX_CLI_HOME)
    else {
        return;
    };
    let outcome = crate::services::provider::codex::maybe_heal_mirror_on_auth_expired(
        codex_cli_home.as_deref(),
    );
    if !matches!(
        outcome,
        crate::services::provider::codex::MirrorHealOutcome::Healed
            | crate::services::provider::codex::MirrorHealOutcome::SkippedAlreadyFired
    ) {
        // Healthy/fresh/native-refused paths are routine; only log when the
        // heal gate changes state or the guard refused. Keeps the log
        // signal-to-noise high.
        return;
    }
    log::info!(
        "request: codex mirror auto-heal gate fired outcome={:?}",
        outcome
    );
}

/// Large JSON request bodies the POST surfaces accept: Anthropic image blocks
/// (base64), Codex/Claude long sessions with big tool outputs, and Gemini
/// inline data. Keep a finite route-local cap large enough for those payloads
/// while avoiding a process-wide unlimited body buffer. Applied to every large
/// JSON POST surface (`/v1/messages`, `/v1/responses`, `/v1/chat/completions`,
/// and the `/v1beta` Gemini methods). Upstream providers may impose smaller
/// platform limits of their own.
const LARGE_REQUEST_BODY_BYTES: usize = 32 * 1024 * 1024;
const RUNTIME_KIND: &str = "desktop";

/// Shared state for the proxy server. `Arc<Router>` is built once at start
/// time from the DB snapshot. `pinned_provider` is a runtime toggle: when
/// set to Some(id), ALL requests are forced through that provider (bypassing
/// the failover chain).
#[derive(Clone)]
pub struct ProxyAppState {
    pub router: Arc<Router>,
    pub auth_token: String,
    pub pinned_provider: Arc<std::sync::Mutex<Option<String>>>,
    /// Runtime toggle for the `/v1/responses` built-in chain-of-thought
    /// system prompt (see `proxy::cot`). Live-updated via
    /// `proxy_set_responses_cot`; initialized from the persisted setting at
    /// proxy start. `Relaxed` ordering — a standalone boolean with no
    /// happens-before requirements.
    pub responses_cot_enabled: Arc<std::sync::atomic::AtomicBool>,
    /// Whether `/v1/messages` admits any non-empty inbound credential (a
    /// subscription OAuth bearer) so `anthropic_passthrough` providers can
    /// relay it upstream. Computed once at proxy start: an enabled
    /// passthrough provider exists in the snapshot AND the bind host is
    /// loopback. Fail-closed — `false` keeps strict token auth.
    pub passthrough_auth_allowed: bool,
    /// Used to record the failover winner (the provider that actually served
    /// a request) as the new `is_current` provider for the app, mirroring
    /// cc-switch's `hot_switch_provider` behaviour. `None` skips persistence
    /// (e.g. unit-test path that builds a state directly).
    /// Review fix: was `Option<Arc<Database>>` — redundant since `Database`
    /// already derives `Clone` over an internal `Arc<Mutex<Connection>>`.
    pub db: Option<crate::database::Database>,
    /// Present only on the server's Axum state. A request extracts one
    /// immutable snapshot at ingress and this field is then cleared, so
    /// handler code can never mix router, pin, or credential policy from
    /// different generations while an upstream call is awaited.
    pub live: Option<Arc<crate::services::proxy_runtime::live::Live_runtime>>,
    /// True only for explicit legacy mirror-repair operations, never for the
    /// unified gateway's request/error path.
    pub allow_client_config_repair: bool,
}

impl ProxyAppState {
    /// The metadata and dispatch state always come from one captured Arc.
    /// No caller re-enters live.current() after checking an admission condition.
    fn captured_snapshot(&self) -> (Self, Option<(u64, bool)>) {
        let Some(live) = self.live.as_ref() else {
            return (self.clone(), None);
        };
        let snapshot = live.current();
        let metadata = (snapshot.generation, snapshot.pinned_provider.is_some());
        let state = Self {
            router: Arc::clone(&snapshot.router),
            auth_token: self.auth_token.clone(),
            pinned_provider: Arc::new(std::sync::Mutex::new(snapshot.pinned_provider.clone())),
            responses_cot_enabled: Arc::clone(&self.responses_cot_enabled),
            passthrough_auth_allowed: snapshot.passthrough_auth_allowed,
            db: self.db.clone(),
            live: None,
            allow_client_config_repair: self.allow_client_config_repair,
        };
        (state, Some(metadata))
    }

    /// Legacy and v1 callers preserve their existing snapshot behavior.
    pub(super) fn capture_snapshot(&self) -> Self {
        let (state, metadata) = self.captured_snapshot();
        if let Some((generation, _)) = metadata {
            crate::proxy::request_timeline::observe_generation(generation);
            crate::proxy::routing_receipts::observe_generation(generation);
        }
        state
    }

    /// V2 checks its private request-local precondition before provider effects.
    /// Header-absent and v1 observations have no precondition and keep their route.
    pub(super) fn capture_snapshot_checked(&self) -> Result<Self, Response> {
        let (state, metadata) = self.captured_snapshot();
        crate::proxy::routing_receipts::check_admission(metadata)
            .map_err(crate::proxy::routing_receipts::http::admission_error)?;
        if let Some((generation, _)) = metadata {
            crate::proxy::request_timeline::observe_generation(generation);
            crate::proxy::routing_receipts::observe_generation(generation);
        }
        Ok(state)
    }
}

/// What the proxy ended up listening on. `port` is the actually-bound port
/// (may differ from the requested port when bind() failed and the caller
/// fell back).
#[derive(Debug, Clone)]
pub struct BoundInfo {
    pub addr: std::net::SocketAddr,
}

#[allow(non_camel_case_types)]
struct Stream_diagnostics {
    request_id: String,
    surface: &'static str,
    model_ref: String,
    started: Instant,
    text_chars: usize,
    thinking_chars: usize,
    tool_delta_count: usize,
    input_tokens: u64,
    output_tokens: u64,
    usage_seen: bool,
    stop_reason: Option<String>,
    completed: bool,
}

impl Stream_diagnostics {
    fn new(request_id: String, surface: &'static str, model: String, started: Instant) -> Self {
        Self {
            request_id,
            surface,
            model_ref: crate::diagnostics::safe_reference(&model),
            started,
            text_chars: 0,
            thinking_chars: 0,
            tool_delta_count: 0,
            input_tokens: 0,
            output_tokens: 0,
            usage_seen: false,
            stop_reason: None,
            completed: false,
        }
    }

    fn observe(&mut self, chunk: &OAIStreamChunk) {
        for choice in &chunk.choices {
            self.text_chars += choice.delta.content.as_deref().map(str::len).unwrap_or(0);
            self.thinking_chars += choice.delta.reasoning_text().map(str::len).unwrap_or(0);
            self.tool_delta_count += choice.delta.tool_calls.as_ref().map(Vec::len).unwrap_or(0);
            if let Some(reason) = &choice.finish_reason {
                self.stop_reason = Some(reason.clone());
            }
        }
        if let Some(usage) = chunk.extra.get("usage") {
            self.usage_seen = true;
            self.input_tokens = usage
                .get("prompt_tokens")
                .or_else(|| usage.get("input_tokens"))
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(self.input_tokens);
            self.output_tokens = usage
                .get("completion_tokens")
                .or_else(|| usage.get("output_tokens"))
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(self.output_tokens);
        }
    }

    fn finish(&mut self) {
        self.completed = true;
        log::info!(
            "request: complete id={} surface={} model_ref={} stream=true elapsed_ms={} text_chars={} thinking_chars={} tool_deltas={} input_tokens={} output_tokens={} usage_seen={} stop_reason={}",
            self.request_id,
            self.surface,
            self.model_ref,
            self.started.elapsed().as_millis(),
            self.text_chars,
            self.thinking_chars,
            self.tool_delta_count,
            self.input_tokens,
            self.output_tokens,
            self.usage_seen,
            safe_stop_reason(self.stop_reason.as_deref())
        );
    }
}

impl Drop for Stream_diagnostics {
    fn drop(&mut self) {
        if !self.completed {
            log::warn!(
                "request: stream dropped id={} surface={} model_ref={} elapsed_ms={} text_chars={} thinking_chars={}",
                self.request_id,
                self.surface,
                self.model_ref,
                self.started.elapsed().as_millis(),
                self.text_chars,
                self.thinking_chars
            );
        }
    }
}

fn start_request(request: &mut ChatCompletionRequest, surface: &'static str) -> (String, Instant) {
    let request_id = crate::proxy::request_timeline::request_id();
    crate::proxy::request_timeline::observe_model(&request.model);
    crate::proxy::routing_receipts::observe_selector(&request.model);
    request.request_id = Some(request_id.clone());
    let started = Instant::now();
    log::info!(
        "request: start id={} surface={} model_ref={} stream={} messages={} tools={}",
        request_id,
        surface,
        crate::diagnostics::safe_reference(&request.model),
        request.stream,
        request.messages.len(),
        request.tools.as_ref().map(Vec::len).unwrap_or(0)
    );
    (request_id, started)
}

fn observe_stream(
    mut stream: BoxStream<'static, OAIStreamChunk>,
    request_id: String,
    surface: &'static str,
    model: String,
    started: Instant,
) -> BoxStream<'static, OAIStreamChunk> {
    let observation = crate::proxy::request_timeline::Stream_observation::capture();
    async_stream::stream! {
        let mut diagnostics = Stream_diagnostics::new(request_id, surface, model, started);
        while let Some(chunk) = stream.next().await {
            diagnostics.observe(&chunk);
            let failed = chunk.extra.contains_key("error");
            if failed {
                observation.failed();
                diagnostics.completed = true;
                log::warn!("request: stream failed id={} surface={} elapsed_ms={}",
                    diagnostics.request_id, diagnostics.surface, diagnostics.started.elapsed().as_millis());
            }
            yield chunk;
            if failed { return; }
        }
        diagnostics.finish();
    }
    .boxed()
}

#[allow(dead_code)] // live-config write path retained for tests/history; not called in the app since Phase 15.9
pub async fn serve(
    bind_addr: std::net::SocketAddr,
    state: ProxyAppState,
    shutdown_rx: oneshot::Receiver<()>,
) -> Result<BoundInfo, String> {
    let (bound, handle) = serve_detached(bind_addr, state, shutdown_rx).await?;
    handle
        .await
        .map_err(|_| "proxy_server_task_join_failed".to_string())??;
    Ok(bound)
}

/// Bind immediately, then serve on a detached task. Returns as soon as
/// the listener is bound (so callers know the actual port / bind error
/// without waiting for shutdown); the accept loop keeps running until
/// `shutdown_rx` fires. `proxy_start` uses this so a successful bind is
/// never mistaken for a timeout.
pub async fn serve_detached(
    bind_addr: std::net::SocketAddr,
    state: ProxyAppState,
    shutdown_rx: oneshot::Receiver<()>,
) -> Result<(BoundInfo, tokio::task::JoinHandle<Result<(), String>>), String> {
    if state.auth_token.is_empty() && !bind_addr.ip().is_loopback() {
        return Err("proxy_auth_required_for_non_loopback_bind".to_string());
    }
    let app = build_router(state);
    let listener = TcpListener::bind(bind_addr)
        .await
        .map_err(|_| "proxy_bind_failed".to_string())?;
    let bound = BoundInfo {
        addr: listener
            .local_addr()
            .map_err(|_| "proxy_local_address_failed".to_string())?,
    };
    log::info!("proxy: listening port={}", bound.addr.port());
    let handle = tokio::spawn(async move {
        axum::serve(listener, app.into_make_service())
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
                log::info!("proxy: graceful shutdown signal received");
            })
            .await
            .map_err(|_| "proxy_server_failed".to_string())
    });
    Ok((bound, handle))
}

/// Relay mirror-role degradation on the native surfaces: when the router
/// served the requested role from a lower-tier role's supply, the response
/// carries the requested role name in `x-aag-role-degraded`.
async fn attach_role_degradation(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let mut response = next.run(request).await;
    if let Some(role) = crate::proxy::request_timeline::role_degradation() {
        if let Ok(value) = axum::http::HeaderValue::from_str(&role) {
            response.headers_mut().insert("x-aag-role-degraded", value);
        }
    }
    response
}

pub(super) fn build_router(state: ProxyAppState) -> AxumRouter {
    let router = AxumRouter::new()
        .route("/aag/discovery", get(discovery_identity))
        .route("/health", get(health))
        .route("/v1/models", get(list_models))
        .route(
            "/claude-desktop/v1/models",
            get(super::claude_desktop::list_models),
        )
        .route(
            "/claude-desktop/v1/messages",
            post(super::claude_desktop::messages)
                .layer(DefaultBodyLimit::max(LARGE_REQUEST_BODY_BYTES))
                .layer(middleware::from_fn(attach_role_degradation)),
        )
        .route(
            "/claude-desktop/v1/messages/count_tokens",
            post(super::claude_desktop::count_tokens)
                .layer(DefaultBodyLimit::max(LARGE_REQUEST_BODY_BYTES)),
        )
        .route(
            "/v1/chat/completions",
            post(chat_completions).layer(DefaultBodyLimit::max(LARGE_REQUEST_BODY_BYTES)),
        )
        .route(
            "/v1/messages",
            post(messages)
                .layer(DefaultBodyLimit::max(LARGE_REQUEST_BODY_BYTES))
                .layer(middleware::from_fn(attach_role_degradation)),
        )
        .route(
            "/v1/responses",
            post(responses)
                .layer(DefaultBodyLimit::max(LARGE_REQUEST_BODY_BYTES))
                .layer(middleware::from_fn(attach_role_degradation)),
        )
        .route(
            "/v1/messages/count_tokens",
            post(messages_count_tokens).layer(DefaultBodyLimit::max(LARGE_REQUEST_BODY_BYTES)),
        )
        // Gemini-native surface for Gemini CLI / Gemini SDK clients
        // (`GOOGLE_GEMINI_BASE_URL` points here). `any` (not just POST) so
        // model discovery GETs don't 404 — the SDK probes `/models`. The
        // body layer raises the cap for generateContent/streamGenerateContent/
        // countTokens (large inline data) without limiting discovery GETs.
        .route(
            "/v1beta/models/*tail",
            any(gemini_native).layer(DefaultBodyLimit::max(LARGE_REQUEST_BODY_BYTES)),
        )
        .route("/v1beta/models", get(gemini_list_models))
        .route(
            "/v1/models/*tail",
            any(gemini_native).layer(DefaultBodyLimit::max(LARGE_REQUEST_BODY_BYTES)),
        );
    let router = router.layer(middleware::from_fn_with_state(state.clone(), refresh_catalog_admission));
    super::routing_receipts::http::attach(router, &state.auth_token)
        .layer(middleware::from_fn(enforce_browser_origin))
        .layer(middleware::from_fn(
            crate::proxy::request_timeline::observe_request,
        ))
        .layer(local_desktop_cors())
        .with_state(state)
}

/// Metadata refresh is an authenticated request operation, never a health
/// probe, completion-side recapture, background task or passthrough grant.
async fn refresh_catalog_admission(
    State(state): State<ProxyAppState>, request: Request, next: Next,
) -> Response {
    let path = request.uri().path();
    let catalog = request.method() == Method::GET && (matches!(path,
        "/v1/models" | "/v1beta/models" | "/claude-desktop/v1/models")
        || path.starts_with("/v1beta/models/") || path.starts_with("/v1/models/"));
    if catalog && check_publication_auth(&request, &state.auth_token).is_ok() {
        if let Some(live) = &state.live {
            if live.refresh_admission().await.is_err() {
                if path.starts_with("/v1beta/models") || path.starts_with("/v1/models/") {
                    return gemini_err_response(503, "Model catalog refresh is temporarily unavailable.");
                }
                if path.starts_with("/claude-desktop/") {
                    return anthropic_err_response(503, "model_catalog_refresh_failed", "api_error");
                }
                return err_response(503, "model_catalog_refresh_failed", "proxy_error");
            }
        }
    }
    next.run(request).await
}

async fn discovery_identity(
    Query(query): Query<HashMap<String, String>>,
) -> impl axum::response::IntoResponse {
    let mut identity = serde_json::json!({
        "service":"ai-agent-gateway", "schemaVersion":1,
        "status":"running", "version":env!("CARGO_PKG_VERSION")
    });
    // Released Tailnet readers require exactly the original four fields.
    // Runtime-aware callers explicitly opt in to the additional fixed marker.
    if query.get("runtime").is_some_and(|value| value == "1") {
        identity["runtime_kind"] = RUNTIME_KIND.into();
    }
    (
        [(axum::http::header::CACHE_CONTROL, "no-store")],
        axum::Json(identity),
    )
}

fn local_desktop_cors() -> CorsLayer {
    let cors = CorsLayer::new()
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers([
            header::AUTHORIZATION,
            header::CONTENT_TYPE,
            header::HeaderName::from_static("x-api-key"),
            header::HeaderName::from_static("x-goog-api-key"),
        ]);
    if cfg!(debug_assertions) {
        // Vite may serve the dev WebView from any auto-incremented port;
        // mirror `trusted_browser_origin` exactly so CORS and the origin
        // gate can never disagree.
        return cors
            .allow_origin(AllowOrigin::predicate(|origin, _| trusted_browser_origin(origin)));
    }
    cors.allow_origin(AllowOrigin::list([
        HeaderValue::from_static("tauri://localhost"),
        HeaderValue::from_static("http://tauri.localhost"),
        HeaderValue::from_static("https://tauri.localhost"),
    ]))
}

async fn enforce_browser_origin(request: Request, next: Next) -> Response {
    if let Some(origin) = request.headers().get(header::ORIGIN) {
        if !trusted_browser_origin(origin) {
            if request.uri().path() == "/v1/messages" {
                return anthropic_err_response(
                    403,
                    "browser_origin_forbidden",
                    "authentication_error",
                );
            }
            return err_response(403, "browser_origin_forbidden", "auth_error");
        }
    }
    next.run(request).await
}

fn trusted_browser_origin(origin: &HeaderValue) -> bool {
    let Ok(origin) = origin.to_str() else {
        return false;
    };
    matches!(
        origin,
        "tauri://localhost" | "http://tauri.localhost" | "https://tauri.localhost"
    ) || (cfg!(debug_assertions) && dev_loopback_origin(origin))
}

/// Dev WebView origins vary: Vite starts at 1421 and auto-increments while
/// the port is taken (see `ui/vite.config.ts`), so a debug build trusts any
/// loopback HTTP origin instead of one fixed port. Release builds keep the
/// fixed Tauri WebView origins above.
fn dev_loopback_origin(origin: &str) -> bool {
    ["http://localhost:", "http://127.0.0.1:"].iter().any(|prefix| {
        origin
            .strip_prefix(prefix)
            .is_some_and(|port| port.parse::<u16>().is_ok())
    })
}

fn safe_stop_reason(reason: Option<&str>) -> &'static str {
    match reason {
        Some("stop") => "stop",
        Some("length") => "length",
        Some("tool_calls") => "tool_calls",
        Some("content_filter") => "content_filter",
        Some("end_turn") => "end_turn",
        Some("max_tokens") => "max_tokens",
        Some("stop_sequence") => "stop_sequence",
        Some(_) => "other",
        None => "unknown",
    }
}

fn auth_matches(headers: &HeaderMap, expected: &str) -> bool {
    if expected.is_empty() {
        return true;
    }
    let provided = provided_credential(headers);
    match (&provided.authorization, &provided.x_api_key) {
        (Some(authorization), _) => authorization.trim_start_matches("Bearer ").trim() == expected,
        (None, Some(x_api_key)) => x_api_key == expected,
        (None, None) => false,
    }
}

/// Capture the inbound client credential verbatim (raw header values) for
/// `anthropic_passthrough` relay. The contents are secrets: never log,
/// persist, or serialize them (see `canonical::Client_credential`).
fn provided_credential(headers: &HeaderMap) -> crate::proxy::canonical::Client_credential {
    crate::proxy::canonical::Client_credential {
        passthrough_only: false,
        authorization: headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .map(String::from),
        x_api_key: headers
            .get("x-api-key")
            .and_then(|v| v.to_str().ok())
            .map(String::from),
        anthropic_beta: headers
            .get("anthropic-beta")
            .and_then(|v| v.to_str().ok())
            .map(String::from),
    }
}

/// Check Bearer / x-api-key auth. Returns Ok(()) if auth disabled or matches.
/// The rejection is boxed: these helpers run on every request and an inline
/// `Response` Err-variant would make every in-flight Result carry it.
pub(super) fn check_auth(headers: &HeaderMap, expected: &str) -> Result<(), Box<Response>> {
    if auth_matches(headers, expected) {
        Ok(())
    } else {
        Err(Box::new(err_response(
            401,
            "authentication_failed",
            "auth_error",
        )))
    }
}

/// Remote publication always requires the existing gateway credential,
/// independently of the local Anthropic subscription-admission exception.
/// Reuse each protocol's credential precedence without reading its body.
pub(super) fn check_publication_auth(
    request: &Request,
    expected: &str,
) -> Result<(), Box<Response>> {
    let path = request.uri().path();
    if path == "/v1beta/models"
        || path.starts_with("/v1beta/models/")
        || path.starts_with("/v1/models/")
    {
        let query = Query::<std::collections::HashMap<String, String>>::try_from_uri(request.uri());
        let query_key = query.as_ref().ok().and_then(|query| query.get("key"));
        if gemini_auth_matches(request.headers(), query_key.map(String::as_str), expected) {
            return Ok(());
        }
        return Err(Box::new(gemini_err_response(401, "Authentication failed.")));
    }
    if path.starts_with("/v1/messages") || path.starts_with("/claude-desktop/") {
        if auth_matches(request.headers(), expected) {
            return Ok(());
        }
        return Err(Box::new(anthropic_err_response(
            401,
            "authentication_failed",
            "authentication_error",
        )));
    }
    check_auth(request.headers(), expected)
}

/// Anthropic-surface auth. `passthrough_allowed` admits any non-empty
/// inbound credential (a subscription OAuth bearer that is not the AAG
/// token) so a passthrough provider can relay it upstream. Computed once at
/// proxy start: enabled iff an `anthropic_passthrough` provider was built
/// into the snapshot AND the bind host is loopback (fail-closed). The returned
/// internal credential restricts this admission to passthrough providers;
/// a valid gateway credential or explicitly disabled gateway auth retains
/// ordinary routing authority.
fn check_anthropic_auth(
    headers: &HeaderMap,
    expected: &str,
    passthrough_allowed: bool,
) -> Result<Option<crate::proxy::canonical::Client_credential>, Box<Response>> {
    let mut credential = provided_credential(headers);
    if auth_matches(headers, expected) {
        return Ok(passthrough_allowed.then_some(credential));
    }
    let has_credential = credential
        .authorization
        .as_deref()
        .into_iter()
        .chain(credential.x_api_key.as_deref())
        .any(|value| !value.trim().is_empty());
    if passthrough_allowed && has_credential {
        credential.passthrough_only = true;
        return Ok(Some(credential));
    }
    Err(Box::new(anthropic_err_response(
        401,
        "authentication_failed",
        "authentication_error",
    )))
}

/// Whether a bind host only accepts local connections (loopback family).
/// Used to gate the passthrough auth relaxation. Wildcard binds
/// (`0.0.0.0`, `::`, `[::]`) are deliberately NOT loopback here: they
/// accept connections from the network, so relaxing auth on them would
/// expose the gateway's stored static-key pool to LAN clients.
pub(crate) fn host_is_loopback(host: &str) -> bool {
    matches!(
        host.trim().to_lowercase().as_str(),
        "" | "127.0.0.1" | "localhost" | "::1" | "[::1]"
    )
}

async fn health(State(state): State<ProxyAppState>, headers: HeaderMap) -> Response {
    let state = state.capture_snapshot();
    if let Err(response) = check_auth(&headers, &state.auth_token) {
        return *response;
    }
    // Per-app winner map. The UI (AppPage + header flow viz) reads each
    // app's value independently. Returning the full map avoids the
    // earlier `g.values().max().cloned()` hack that picked the
    // lexicographically largest provider id across all apps.
    let last_used = state
        .router
        .last_used
        .lock()
        .ok()
        .map(|g| g.clone())
        .unwrap_or_default();
    let pinned = state.pinned_provider.lock().ok().and_then(|g| g.clone());
    Json(json!({
        "status": "ok",
        "runtime_kind": RUNTIME_KIND,
        "providers": state.router.health(),
        "models": state.router.list_models(),
        "availableModels": available_model_ids(&state, pinned.as_deref()),
        "lastWinners": last_used,
        "pinnedProvider": pinned,
    }))
    .into_response()
}

fn available_model_ids(state: &ProxyAppState, pinned_provider: Option<&str>) -> Vec<String> {
    let mut ids: Vec<_> = [
        crate::proxy::router::SURFACE_CODEX,
        crate::proxy::router::SURFACE_CHAT,
        crate::proxy::router::SURFACE_CLAUDE,
        crate::proxy::router::SURFACE_GEMINI,
    ]
    .into_iter()
    .flat_map(|surface| state.router.catalog_models(surface, pinned_provider))
    .collect();
    ids.sort();
    ids.dedup();
    ids
}

/// Model list in a shape both SDKs accept: Anthropic Models API fields
/// (`type`, `display_name`, `created_at`, envelope `has_more`/`first_id`/
/// `last_id`) plus the OpenAI fields (`object`, `created`, `owned_by`) kept
/// for back-compat with existing clients. Unknown SDKs ignore extra fields.
async fn list_models(State(state): State<ProxyAppState>, headers: HeaderMap) -> Response {
    let state = state.capture_snapshot();
    if let Err(response) = check_auth(&headers, &state.auth_token) {
        return *response;
    }
    let pinned = state
        .pinned_provider
        .lock()
        .ok()
        .and_then(|pin| pin.clone());
    let ids = available_model_ids(&state, pinned.as_deref());
    let data: Vec<_> = ids
        .iter()
        .map(|id| {
            let alias = (!state.router.offers_exact_model(id))
                .then(|| {
                    crate::proxy::planner::CAPABILITY_ALIAS_SELECTORS
                        .iter()
                        .find(|(name, _)| {
                            id.as_str() == *name || id.as_str() == format!("alias:{name}")
                        })
                })
                .flatten();
            let display_name = crate::proxy::models::display_name_for(id)
                .map(|s| s.to_string())
                .unwrap_or_else(|| crate::proxy::models::pretty_display_name(id));
            let mut row = json!({
                "id": id,
                "type": "model",
                "display_name": display_name,
                "created_at": "2026-01-01T00:00:00Z",
                "object": "model",
                "created": 1_700_000_000,
                "owned_by": "ai-agent-gateway",
                "kind": if state.router.offers_exact_model(id) { "exact" } else { "alias" },
            });
            if let Some((name, rank)) = alias {
                row["selector"] = json!(format!("alias:{name}"));
                row["aliasTier"] = json!(rank);
            }
            row
        })
        .collect();
    let listed_ids: Vec<String> = data
        .iter()
        .filter_map(|entry| {
            entry
                .get("id")
                .and_then(|id| id.as_str())
                .map(str::to_string)
        })
        .collect();
    Json(json!({
        "object": "list",
        "data": data,
        "has_more": false,
        "first_id": listed_ids.first().cloned(),
        "last_id": listed_ids.last().cloned(),
    }))
    .into_response()
}

async fn chat_completions(
    State(state): State<ProxyAppState>,
    headers: HeaderMap,
    // Capture extraction failure so authentication keeps precedence over body errors.
    payload: Result<Json<ChatCompletionRequest>, JsonRejection>,
) -> Response {
    let state = match state.capture_snapshot_checked() {
        Ok(state) => state,
        Err(response) => return response,
    };
    if let Err(resp) = check_auth(&headers, &state.auth_token) {
        return *resp;
    }
    let mut req = match payload {
        Ok(Json(req)) => req,
        Err(_) => return err_response(400, "invalid_request", "invalid_request_error"),
    };
    if req.model.is_empty() || req.messages.is_empty() {
        return err_response(400, "invalid_request", "invalid_request_error");
    }
    let (request_id, started) = start_request(&mut req, "chat");
    if req.stream {
        return stream_chat_completions(state, req, request_id, started).await;
    }
    let pinned = state.pinned_provider.lock().ok().and_then(|g| g.clone());
    let result = if let Some(ref pid) = pinned {
        state
            .router
            .complete_with_provider(crate::proxy::router::SURFACE_CHAT, "copilot", pid, &req)
            .await
    } else {
        state
            .router
            .complete_for(crate::proxy::router::SURFACE_CHAT, "copilot", &req)
            .await
    };
    match result {
        Ok(resp) => {
            log::info!(
                "request: complete id={} surface=chat model_ref={} stream=false elapsed_ms={} output_tokens={}",
                request_id,
                crate::diagnostics::safe_reference(&req.model),
                started.elapsed().as_millis(),
                resp.usage.as_ref().map(|usage| usage.completion_tokens).unwrap_or(0)
            );
            match serde_json::to_value(resp) {
                Ok(v) => Json(v).into_response(),
                Err(_) => err_response(500, "response_serialize_error", "internal_error"),
            }
        }
        Err(e) => {
            log::warn!(
                "request: failed id={} surface=chat model_ref={} elapsed_ms={} status={} code={} stage={} response_bytes={}",
                request_id,
                crate::diagnostics::safe_reference(&req.model),
                started.elapsed().as_millis(),
                e.status.unwrap_or(500),
                e.code,
                e.stage,
                e.response_bytes.unwrap_or(0)
            );
            let status = e.status.unwrap_or(500);
            err_response(
                status,
                e.code,
                if status >= 500 {
                    "proxy_error"
                } else {
                    "invalid_request_error"
                },
            )
        }
    }
}

async fn stream_chat_completions(
    state: ProxyAppState,
    req: ChatCompletionRequest,
    request_id: String,
    started: Instant,
) -> Response {
    let pinned = state.pinned_provider.lock().ok().and_then(|g| g.clone());
    let stream = if let Some(ref pid) = pinned {
        state
            .router
            .stream_with_provider(crate::proxy::router::SURFACE_CHAT, "copilot", pid, &req)
            .await
    } else {
        state
            .router
            .stream_for(crate::proxy::router::SURFACE_CHAT, "copilot", &req)
            .await
    };
    let stream = match stream {
        Ok(s) => s,
        Err(e) => {
            let status = e.status.unwrap_or(500);
            log::warn!(
                "request: stream setup failed id={} surface=chat model_ref={} elapsed_ms={} status={} code={} stage={} response_bytes={}",
                request_id,
                crate::diagnostics::safe_reference(&req.model),
                started.elapsed().as_millis(),
                status,
                e.code,
                e.stage,
                e.response_bytes.unwrap_or(0)
            );
            return err_response(
                status,
                e.code,
                if status >= 500 {
                    "proxy_error"
                } else {
                    "invalid_request_error"
                },
            );
        }
    };
    use futures_util::stream::StreamExt;

    // Map each upstream OAI chunk to an SSE `data: <json>\n\n` frame, then
    // append the OpenAI-spec `[DONE]` sentinel once the upstream stream
    // ends. This matches cc-switch's behavior and the Node
    // `tests/e2e.test.ts` assertion that the body ends with `data: [DONE]`.
    let stream = observe_stream(stream, request_id, "chat", req.model.clone(), started);
    let with_done = async_stream::stream! {
        let mut stream = stream;
        while let Some(chunk) = stream.next().await {
            if let Some(error) = chunk.extra.get("error") {
                let frame = format!("data: {}\n\n", json!({"error": error}));
                yield Result::<bytes::Bytes, std::io::Error>::Ok(frame.into());
                return;
            }
            let frame = format!("data: {}\n\n", serde_json::to_string(&chunk).unwrap_or("{}".into()));
            yield Result::<bytes::Bytes, std::io::Error>::Ok(frame.into());
        }
        yield Result::<bytes::Bytes, std::io::Error>::Ok("data: [DONE]\n\n".into());
    };
    let body = axum::body::Body::from_stream(with_done);
    Response::builder()
        .status(200)
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .header("connection", "keep-alive")
        .body(body)
        .unwrap()
}

pub(super) async fn messages(
    State(state): State<ProxyAppState>,
    headers: HeaderMap,
    payload: Result<Json<AnthropicRequest>, JsonRejection>,
) -> Response {
    let state = match state.capture_snapshot_checked() {
        Ok(state) => state,
        Err(response) => return response,
    };
    // Receipt authentication proves gateway authority, not upstream relay authority.
    // A mixed snapshot can forward the inbound credential on a later fallback;
    // reject this opt-in scope before attaching it or invoking any provider.
    if crate::proxy::routing_receipts::current().is_some() && state.passthrough_auth_allowed {
        return (StatusCode::BAD_REQUEST, Json(json!({
            "type":"error",
            "error":{
                "type":"invalid_request_error",
                "code":"receipt_credential_scope_unsupported",
                "message":"Routing receipts are unavailable for this Messages credential scope."
            }
        }))).into_response();
    }
    let credential =
        match check_anthropic_auth(&headers, &state.auth_token, state.passthrough_auth_allowed) {
            Ok(credential) => credential,
            Err(response) => return *response,
        };
    let ant_req = match payload {
        Ok(Json(req)) => req,
        Err(_) => return anthropic_err_response(400, "invalid_request", "invalid_request_error"),
    };
    let mut oai_req = anthropic_req_to_oai_req(&ant_req);
    // Capture the inbound credential BEFORE the streaming branch: the stream
    // helpers take the built request (not headers), so this is the single
    // choke point where a passthrough provider can relay it upstream.
    oai_req.client_credential = credential;
    let requested_model = ant_req.model.clone();
    let (request_id, started) = start_request(&mut oai_req, "claude");
    if ant_req.stream == Some(true) {
        let pinned = state.pinned_provider.lock().ok().and_then(|g| g.clone());
        let stream = if let Some(ref pid) = pinned {
            state
                .router
                .stream_with_provider(
                    crate::proxy::router::SURFACE_CLAUDE,
                    "claude_code",
                    pid,
                    &oai_req,
                )
                .await
        } else {
            state
                .router
                .stream_for(
                    crate::proxy::router::SURFACE_CLAUDE,
                    "claude_code",
                    &oai_req,
                )
                .await
        };
        let stream = match stream {
            Ok(s) => s,
            Err(e) => {
                let status = e.status.unwrap_or(500);
                log::warn!(
                    "request: stream setup failed id={} surface=claude model_ref={} elapsed_ms={} status={} code={} stage={} response_bytes={}",
                    request_id,
                    crate::diagnostics::safe_reference(&requested_model),
                    started.elapsed().as_millis(),
                    status,
                    e.code,
                    e.stage,
                    e.response_bytes.unwrap_or(0)
                );
                let kind = anthropic_error_kind(&e);
                return anthropic_err_response(status, e.code, kind);
            }
        };
        let stream = observe_stream(
            stream,
            request_id,
            "claude",
            requested_model.clone(),
            started,
        );
        let model = requested_model.clone();
        let ant_stream = oai_chunks_to_anthropic_stream(stream, model);
        let body = axum::body::Body::from_stream(
            ant_stream
                .map(|sse| Result::<bytes::Bytes, std::io::Error>::Ok(sse.into_bytes().into())),
        );
        return Response::builder()
            .status(200)
            .header("content-type", "text/event-stream")
            .header("cache-control", "no-cache")
            .header("connection", "keep-alive")
            .body(body)
            .unwrap();
    }
    let pinned = state.pinned_provider.lock().ok().and_then(|g| g.clone());
    let oai_resp = if let Some(ref pid) = pinned {
        state
            .router
            .complete_with_provider(
                crate::proxy::router::SURFACE_CLAUDE,
                "claude_code",
                pid,
                &oai_req,
            )
            .await
    } else {
        state
            .router
            .complete_for(
                crate::proxy::router::SURFACE_CLAUDE,
                "claude_code",
                &oai_req,
            )
            .await
    };
    let oai_resp = match oai_resp {
        Ok(r) => r,
        Err(e) => {
            log::warn!(
                "request: failed id={} surface=claude model_ref={} elapsed_ms={} status={} code={} stage={} response_bytes={}",
                request_id,
                crate::diagnostics::safe_reference(&requested_model),
                started.elapsed().as_millis(),
                e.status.unwrap_or(500),
                e.code,
                e.stage,
                e.response_bytes.unwrap_or(0)
            );
            let status = e.status.unwrap_or(500);
            let kind = anthropic_error_kind(&e);
            return anthropic_err_response(status, e.code, kind);
        }
    };
    log::info!(
        "request: complete id={} surface=claude model_ref={} stream=false elapsed_ms={} output_tokens={}",
        request_id,
        crate::diagnostics::safe_reference(&requested_model),
        started.elapsed().as_millis(),
        oai_resp.usage.as_ref().map(|usage| usage.completion_tokens).unwrap_or(0)
    );
    let ant_resp = oai_resp_to_anthropic_resp(&oai_resp, &requested_model);
    match serde_json::to_value(ant_resp) {
        Ok(v) => Json(v).into_response(),
        Err(_) => anthropic_err_response(500, "response_serialize_error", "api_error"),
    }
}

/// `POST /v1/messages/count_tokens` — the context-measurement call Claude
/// Code issues alongside every conversation turn. Part of the Anthropic
/// surface: the same auth (and the same `/v1/messages` passthrough
/// relaxation) applies, so a subscription client measures through AAG.
/// The routed provider serves the count: Anthropic-family hops relay the
/// request upstream (credential verbatim on passthrough), other hops fall
/// back to a deterministic local estimate.
pub(super) async fn messages_count_tokens(
    State(state): State<ProxyAppState>,
    headers: HeaderMap,
    // Deliberately still a `Value`: this surface relays the caller's
    // count_tokens body upstream untouched (the router forwards `body`
    // verbatim), so it must not be projected through a request struct.
    payload: Result<Json<serde_json::Value>, JsonRejection>,
) -> Response {
    let state = state.capture_snapshot();
    let credential =
        match check_anthropic_auth(&headers, &state.auth_token, state.passthrough_auth_allowed) {
            Ok(credential) => credential,
            Err(response) => return *response,
        };
    let body = match payload {
        Ok(Json(body)) => body,
        Err(_) => return anthropic_err_response(400, "invalid_request", "invalid_request_error"),
    };
    let model = body
        .get("model")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    if model.is_empty() {
        return anthropic_err_response(400, "invalid_request", "invalid_request_error");
    }
    crate::proxy::request_timeline::observe_model(&model);
    let started = std::time::Instant::now();
    let result = state
        .router
        .count_tokens_for(
            crate::proxy::router::SURFACE_CLAUDE,
            &model,
            &body,
            credential.as_ref(),
        )
        .await;
    let input_tokens = match result {
        Ok(tokens) => tokens,
        Err(e) => {
            let status = e.status.unwrap_or(500);
            log::warn!(
                "request: count_tokens failed surface=claude model_ref={} elapsed_ms={} status={} code={} stage={}",
                crate::diagnostics::safe_reference(&model),
                started.elapsed().as_millis(),
                status,
                e.code,
                e.stage
            );
            let kind = anthropic_error_kind(&e);
            return anthropic_err_response(status, e.code, kind);
        }
    };
    log::info!(
        "request: count_tokens surface=claude model_ref={} elapsed_ms={} input_tokens={}",
        crate::diagnostics::safe_reference(&model),
        started.elapsed().as_millis(),
        input_tokens
    );
    Json(json!({ "input_tokens": input_tokens })).into_response()
}

fn err_response(status: u16, code: impl AsRef<str>, kind: &str) -> Response {
    let code = safe_external_error_code(code.as_ref(), status, kind);
    let correlation_id = crate::proxy::request_timeline::request_id();
    log::warn!(
        "request: external error id={} status={} code={} kind={}",
        correlation_id,
        status,
        code,
        kind
    );
    let body = json!({
        "error": {
            "message": crate::proxy::canonical::public_error_message(&code, Some(status)),
            "type": kind,
            "code": code,
            "correlation_id": correlation_id,
        }
    });
    (
        StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        Json(body),
    )
        .into_response()
}

/// Anthropic typed error mapping. Relay the upstream's own allowlisted
/// `error.type` when the failing hop was Anthropic-protocol; otherwise map
/// the gateway's status to the documented Anthropic types so Claude Code's
/// typed retry logic (`rate_limit_error` / `overloaded_error` / …) works on
/// gateway-raised errors too. Unknown statuses stay `api_error`.
fn anthropic_error_kind(error: &crate::proxy::canonical::ProviderError) -> &'static str {
    if let Some(kind) = error.upstream_error_type {
        return kind;
    }
    match error.status.unwrap_or(500) {
        400 => "invalid_request_error",
        401 => "authentication_error",
        403 => "permission_error",
        404 => "not_found_error",
        413 => "request_too_large",
        429 => "rate_limit_error",
        529 => "overloaded_error",
        _ => "api_error",
    }
}

fn anthropic_err_response(status: u16, code: impl AsRef<str>, kind: &str) -> Response {
    let code = safe_external_error_code(code.as_ref(), status, kind);
    let correlation_id = crate::proxy::request_timeline::request_id();
    log::warn!(
        "request: external error id={} status={} code={} kind={} surface=claude",
        correlation_id,
        status,
        code,
        kind
    );
    let body = json!({
        "type": "error",
        "error": {
            "type": kind,
            "message": crate::proxy::canonical::public_error_message(&code, Some(status)),
        },
        "request_id": correlation_id,
    });
    (
        StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        Json(body),
    )
        .into_response()
}

fn safe_external_error_code(value: &str, status: u16, kind: &str) -> String {
    let allowed = [
        "model_catalog_refresh_failed",
        "model_alias_no_candidates",
        "model_precise_unavailable",
        "all_hops_exhausted",
        "invalid_request",
        "no_route",
        "response_serialize_error",
        // Stable Responses-edge conversion codes: relaying them verbatim is
        // what makes state/feature gaps on non-native hops identifiable.
        "responses_state_unsupported",
        "responses_feature_unsupported",
        "responses_invalid_request",
        "upstream_decode_error",
        "upstream_http_error",
        "upstream_stream_error",
        "upstream_timeout",
        "upstream_transport_error",
    ];
    if allowed.contains(&value) {
        value.to_string()
    } else if status >= 500 {
        "gateway_internal_error".into()
    } else if kind == "auth_error" {
        "authentication_failed".into()
    } else {
        "invalid_request".into()
    }
}

/// OpenAI Responses API endpoint. Accepts the Responses request shape,
/// converts to canonical OpenAI Chat Completions, runs the failover router,
/// then converts the response back to the Responses envelope. Streaming
/// emits SSE events using the Responses API's named-event format
/// (`event: response.created\ndata: ...\n\n` etc.) terminated by
/// `data: [DONE]\n\n`. This is the surface the Codex CLI / Codex Desktop
/// app uses natively.
async fn responses(
    State(state): State<ProxyAppState>,
    headers: HeaderMap,
    payload: Result<Json<ResponsesRequest>, JsonRejection>,
) -> Response {
    let state = state.capture_snapshot();
    if let Err(resp) = check_auth(&headers, &state.auth_token) {
        return *resp;
    }
    let req = match payload {
        Ok(Json(req)) => req,
        Err(_) => return err_response(400, "invalid_request", "invalid_request_error"),
    };
    if req.model.is_empty() {
        return err_response(400, "invalid_request", "invalid_request_error");
    }
    let requested_model = req.model.clone();
    // Defer namespace-conversion failures to the selected nonnative hop. Native
    // Responses providers retain their original declarations and output events.
    let namespace_map = Responses_namespace_map::from_request(&req).ok();
    let mut chat_req = responses_to_chat(&req);
    chat_req.source_responses_request = Some(req);
    // Built-in chain-of-thought prompt (UI toggle, live). Single choke point:
    // the streaming path below shares this converted request.
    crate::proxy::cot::maybe_inject_cot(
        &mut chat_req,
        state
            .responses_cot_enabled
            .load(std::sync::atomic::Ordering::Relaxed),
    );
    let (request_id, started) = start_request(&mut chat_req, "codex");

    if chat_req.stream {
        return stream_responses(
            state,
            chat_req,
            requested_model,
            request_id,
            started,
            namespace_map,
        )
        .await;
    }
    let pinned = state.pinned_provider.lock().ok().and_then(|g| g.clone());
    let result = if let Some(ref pid) = pinned {
        state
            .router
            .complete_with_provider(crate::proxy::router::SURFACE_CODEX, "codex", pid, &chat_req)
            .await
    } else {
        state
            .router
            .complete_for(crate::proxy::router::SURFACE_CODEX, "codex", &chat_req)
            .await
    };
    match result {
        Ok(resp) => {
            log::info!(
                "request: complete id={} surface=codex model_ref={} stream=false elapsed_ms={} output_tokens={}",
                request_id,
                crate::diagnostics::safe_reference(&requested_model),
                started.elapsed().as_millis(),
                resp.usage.as_ref().map(|usage| usage.completion_tokens).unwrap_or(0)
            );
            Json(chat_to_responses_with_namespaces(
                &resp,
                &requested_model,
                namespace_map.as_ref(),
            ))
            .into_response()
        }
        Err(e) => {
            if e.kind == Some(UpstreamErrorKind::AuthExpired) {
                maybe_heal_codex_mirror(&state);
            }
            log::warn!(
                "request: failed id={} surface=codex model_ref={} elapsed_ms={} status={} code={} stage={} response_bytes={}",
                request_id,
                crate::diagnostics::safe_reference(&requested_model),
                started.elapsed().as_millis(),
                e.status.unwrap_or(500),
                e.code,
                e.stage,
                e.response_bytes.unwrap_or(0)
            );
            let status = e.status.unwrap_or(500);
            err_response(
                status,
                e.code,
                if status >= 500 {
                    "proxy_error"
                } else {
                    "invalid_request_error"
                },
            )
        }
    }
}

async fn stream_responses(
    state: ProxyAppState,
    chat_req: ChatCompletionRequest,
    requested_model: String,
    request_id: String,
    started: Instant,
    namespace_map: Option<Responses_namespace_map>,
) -> Response {
    let pinned = state.pinned_provider.lock().ok().and_then(|g| g.clone());
    let stream = if let Some(ref pid) = pinned {
        state
            .router
            .stream_with_provider(crate::proxy::router::SURFACE_CODEX, "codex", pid, &chat_req)
            .await
    } else {
        state
            .router
            .stream_for(crate::proxy::router::SURFACE_CODEX, "codex", &chat_req)
            .await
    };
    let stream = match stream {
        Ok(s) => s,
        Err(e) => {
            if e.kind == Some(UpstreamErrorKind::AuthExpired) {
                maybe_heal_codex_mirror(&state);
            }
            let status = e.status.unwrap_or(500);
            log::warn!(
                "request: stream setup failed id={} surface=codex model_ref={} elapsed_ms={} status={} code={} stage={} response_bytes={}",
                request_id,
                crate::diagnostics::safe_reference(&requested_model),
                started.elapsed().as_millis(),
                status,
                e.code,
                e.stage,
                e.response_bytes.unwrap_or(0)
            );
            return err_response(
                status,
                e.code,
                if status >= 500 {
                    "proxy_error"
                } else {
                    "invalid_request_error"
                },
            );
        }
    };
    let stream = observe_stream(
        stream,
        request_id,
        "codex",
        requested_model.clone(),
        started,
    );
    let mapped =
        chat_chunks_to_responses_stream_with_namespaces(stream, &requested_model, namespace_map);
    // The converter owns terminal ordering and omits DONE after an error.
    let body = axum::body::Body::from_stream(
        mapped.map(|frame| Ok::<bytes::Bytes, std::io::Error>(frame.into_bytes().into())),
    );
    Response::builder()
        .status(200)
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .header("connection", "keep-alive")
        .body(body)
        .unwrap()
}

// ----- Gemini-native surface (`/v1beta/models/{model}:generateContent`) -----

/// Gemini error envelope: `{"error": {code, message, status}}` with the
/// canonical Google status names (cc-switch `gemini_error_body` parity).
fn gemini_err_response(status: u16, message: &str) -> Response {
    let google_status = match status {
        400 => "INVALID_ARGUMENT",
        401 => "UNAUTHENTICATED",
        403 => "PERMISSION_DENIED",
        404 => "NOT_FOUND",
        429 => "RESOURCE_EXHAUSTED",
        500..=599 => "UNAVAILABLE",
        _ => "INTERNAL",
    };
    let correlation_id = crate::proxy::request_timeline::request_id();
    log::warn!(
        "request: external error id={} status={} code=gemini surface=gemini",
        correlation_id,
        status
    );
    (
        StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        Json(json!({
            "error": {
                "code": status,
                "message": message,
                "status": google_status,
            }
        })),
    )
        .into_response()
}

/// Gemini clients authenticate with `x-goog-api-key` (or `?key=`), not the
/// OpenAI Bearer / Anthropic x-api-key pair. Accept all three plus the query
/// param so the CLI's `GEMINI_API_KEY` placeholder works either way.
fn gemini_auth_matches(headers: &HeaderMap, query_key: Option<&str>, expected: &str) -> bool {
    if expected.is_empty() {
        return true;
    }
    let provided = headers
        .get("x-goog-api-key")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .or_else(|| {
            headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.trim_start_matches("Bearer ").trim().to_string())
        })
        .or_else(|| {
            headers
                .get("x-api-key")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string())
        })
        .or_else(|| query_key.map(|k| k.to_string()));
    provided.as_deref() == Some(expected)
}

/// One admitted `models/{id}` catalog entry (the SDK's model-info GET).
/// Gemini CLI reads `supportedGenerationMethods` to decide what it may call.
/// Unknown capability limits are omitted instead of invented.
fn gemini_model_info(model: &str) -> serde_json::Value {
    let model = crate::proxy::gemini::normalize_gemini_model_id(model);
    json!({
        "name": format!("models/{model}"),
        "displayName": model,
        "supportedGenerationMethods": [
            "generateContent", "streamGenerateContent", "countTokens"
        ],
    })
}

/// `GET /v1beta/models` — model discovery in the Gemini ListModels shape.
async fn gemini_list_models(
    State(state): State<ProxyAppState>,
    headers: HeaderMap,
    Query(query): Query<std::collections::HashMap<String, String>>,
) -> Response {
    let state = state.capture_snapshot();
    if !gemini_auth_matches(
        &headers,
        query.get("key").map(String::as_str),
        &state.auth_token,
    ) {
        return gemini_err_response(401, "Authentication failed.");
    }
    let pinned = state
        .pinned_provider
        .lock()
        .ok()
        .and_then(|pin| pin.clone());
    let exact_ids = state
        .router
        .catalog_models(crate::proxy::router::SURFACE_GEMINI, pinned.as_deref());
    let models: Vec<serde_json::Value> = exact_ids
        .iter()
        .map(|model| gemini_model_info(model))
        .collect();
    Json(json!({ "models": models })).into_response()
}

/// A `serde_json` sink that only counts serialized bytes. The Gemini
/// countTokens estimate must measure the full JSON wire size — escaping,
/// quotes and structural characters all count — so serializing through this
/// sink reproduces `to_string(..).len()` without materializing the string or
/// cloning the subtree.
#[allow(non_camel_case_types)]
struct Byte_counting_writer {
    written: usize,
}

impl Byte_counting_writer {
    fn new() -> Self {
        Self { written: 0 }
    }
}

impl std::io::Write for Byte_counting_writer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.written += buf.len();
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Split a wildcard tail (`{model}:{method}` or a bare model id) into its
/// parts. The method is the suffix after the last colon — model ids never
/// contain one, while the SDK appends `:generateContent` & friends.
fn split_gemini_tail(tail: &str) -> (String, Option<String>) {
    let tail = tail.split('?').next().unwrap_or(tail);
    match tail.rsplit_once(':') {
        Some((model, method)) if !model.is_empty() && !method.is_empty() => {
            (model.to_string(), Some(method.to_string()))
        }
        _ => (tail.to_string(), None),
    }
}

/// The Gemini-native entry surface. `tail` is everything after
/// `/v1beta/models/`: `{model}:generateContent`, `{model}:streamGenerateContent`,
/// `{model}:countTokens`, or a bare `{model}` (model info GET).
async fn gemini_native(
    State(state): State<ProxyAppState>,
    Path(tail): Path<String>,
    method: Method,
    headers: HeaderMap,
    Query(query): Query<std::collections::HashMap<String, String>>,
    // Deliberately still a `Value`: the Gemini conversion
    // (`gemini_req_to_oai_req`) walks the body tolerantly and skips
    // malformed entries; a typed extractor would turn "ignore what you
    // don't understand" into a hard 400 on a surface tuned for that.
    payload: Option<Result<Json<serde_json::Value>, JsonRejection>>,
) -> Response {
    let state = state.capture_snapshot();
    if !gemini_auth_matches(
        &headers,
        query.get("key").map(String::as_str),
        &state.auth_token,
    ) {
        return gemini_err_response(401, "Authentication failed.");
    }
    let (model, gemini_method) = split_gemini_tail(&tail);
    crate::proxy::request_timeline::observe_model(&model);
    if model.is_empty() {
        return gemini_err_response(400, "Missing model id.");
    }

    // GETs are metadata: model info (the tail has no method, or an `alt`
    // form). Everything else must be a POST method call.
    if method == Method::GET {
        let pinned = state
            .pinned_provider
            .lock()
            .ok()
            .and_then(|pin| pin.clone());
        let model = crate::proxy::gemini::normalize_gemini_model_id(&model);
        if !state
            .router
            .catalog_models(crate::proxy::router::SURFACE_GEMINI, pinned.as_deref())
            .iter()
            .any(|id| id == model)
        {
            return gemini_err_response(404, "Model is unavailable.");
        }
        return Json(gemini_model_info(model)).into_response();
    }
    if method != Method::POST {
        return gemini_err_response(405, "Method not allowed.");
    }

    let body = match payload {
        Some(Ok(Json(body))) => body,
        _ => return gemini_err_response(400, "Invalid JSON request body."),
    };

    let Some(gemini_method) = gemini_method else {
        return gemini_err_response(400, "Missing :method suffix.");
    };

    if gemini_method == "countTokens" {
        // Local rough estimate (chars/4, cc-switch parity) — proxying a token
        // count would need a provider round trip per call. The heuristic
        // measures serialized JSON bytes, so count them through the sink
        // instead of cloning `contents` and materializing the string.
        let mut counting_writer = Byte_counting_writer::new();
        let serialized_bytes = serde_json::to_writer(&mut counting_writer, &body)
            .ok()
            .map(|()| counting_writer.written)
            .unwrap_or_default();
        let mut response =
            Json(json!({ "totalTokens": serialized_bytes.div_ceil(4).max(1) })).into_response();
        response.headers_mut().insert(
            "x-aag-token-count-mode",
            HeaderValue::from_static("estimate"),
        );
        return response;
    }
    if gemini_method != "generateContent" && gemini_method != "streamGenerateContent" {
        return gemini_err_response(400, &format!("Unsupported method {gemini_method}."));
    }

    let mut chat_req = gemini_req_to_oai_req(&body);
    chat_req.model = crate::proxy::gemini::normalize_gemini_model_id(&model).to_string();
    // Streaming is either the explicit method or the SDK's `alt=sse` query.
    let is_stream = gemini_method == "streamGenerateContent"
        || query.get("alt").map(|alt| alt == "sse").unwrap_or(false);
    chat_req.stream = is_stream;
    let (request_id, started) = start_request(&mut chat_req, "gemini");
    let requested_model = chat_req.model.clone();

    if is_stream {
        return stream_gemini(state, chat_req, requested_model, request_id, started).await;
    }

    let pinned = state.pinned_provider.lock().ok().and_then(|g| g.clone());
    let result = if let Some(ref pid) = pinned {
        state
            .router
            .complete_with_provider(
                crate::proxy::router::SURFACE_GEMINI,
                "gemini",
                pid,
                &chat_req,
            )
            .await
    } else {
        state
            .router
            .complete_for(crate::proxy::router::SURFACE_GEMINI, "gemini", &chat_req)
            .await
    };
    match result {
        Ok(resp) => {
            log::info!(
                "request: complete id={} surface=gemini model_ref={} stream=false elapsed_ms={} output_tokens={}",
                request_id,
                crate::diagnostics::safe_reference(&requested_model),
                started.elapsed().as_millis(),
                resp.usage.as_ref().map(|usage| usage.completion_tokens).unwrap_or(0)
            );
            Json(oai_resp_to_gemini_resp(&resp, &requested_model)).into_response()
        }
        Err(e) => {
            log::warn!(
                "request: failed id={} surface=gemini model_ref={} elapsed_ms={} status={} code={} stage={} response_bytes={}",
                request_id,
                crate::diagnostics::safe_reference(&requested_model),
                started.elapsed().as_millis(),
                e.status.unwrap_or(500),
                e.code,
                e.stage,
                e.response_bytes.unwrap_or(0)
            );
            let status = e.status.unwrap_or(500);
            gemini_err_response(
                status,
                crate::proxy::canonical::public_error_message(&e.code, Some(status)),
            )
        }
    }
}

async fn stream_gemini(
    state: ProxyAppState,
    chat_req: ChatCompletionRequest,
    requested_model: String,
    request_id: String,
    started: Instant,
) -> Response {
    let pinned = state.pinned_provider.lock().ok().and_then(|g| g.clone());
    let stream = if let Some(ref pid) = pinned {
        state
            .router
            .stream_with_provider(
                crate::proxy::router::SURFACE_GEMINI,
                "gemini",
                pid,
                &chat_req,
            )
            .await
    } else {
        state
            .router
            .stream_for(crate::proxy::router::SURFACE_GEMINI, "gemini", &chat_req)
            .await
    };
    let stream = match stream {
        Ok(s) => s,
        Err(e) => {
            let status = e.status.unwrap_or(500);
            log::warn!(
                "request: stream setup failed id={} surface=gemini model_ref={} elapsed_ms={} status={} code={} stage={} response_bytes={}",
                request_id,
                crate::diagnostics::safe_reference(&requested_model),
                started.elapsed().as_millis(),
                status,
                e.code,
                e.stage,
                e.response_bytes.unwrap_or(0)
            );
            return gemini_err_response(
                status,
                crate::proxy::canonical::public_error_message(&e.code, Some(status)),
            );
        }
    };
    let stream = observe_stream(
        stream,
        request_id,
        "gemini",
        requested_model.clone(),
        started,
    );
    // Gemini SSE has no `[DONE]` sentinel — the stream ends after the frame
    // carrying usageMetadata / finishReason. Each event body must still be
    // framed as an SSE `data: <json>\n\n` line: the Gemini CLI's SSE parser
    // reads complete `data:` events only, and a bare JSON chunk (no prefix,
    // no trailing blank line) surfaces client-side as
    // "Incomplete JSON segment at the end".
    let mapped = oai_chunks_to_gemini_stream(stream, requested_model.clone());
    let body = axum::body::Body::from_stream(mapped.map(|frame| {
        let mut framed = bytes::BytesMut::new();
        framed.extend_from_slice(b"data: ");
        framed.extend_from_slice(frame.as_bytes());
        framed.extend_from_slice(b"\n\n");
        Ok::<bytes::Bytes, std::io::Error>(framed.freeze())
    }));
    Response::builder()
        .status(200)
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .header("connection", "keep-alive")
        .body(body)
        .unwrap()
}

/// Bind a socket address from a host:port pair. Convenience for tests.
pub fn socket_addr(host: &str, port: u16) -> std::net::SocketAddr {
    format!("{host}:{port}").parse().expect("valid socket addr")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::dao::proxy_providers::CircuitBreakerConfig;
    use crate::proxy::providers::mock::MockProvider;
    use crate::proxy::router::Router;
    use serde_json::Value;
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::oneshot;

    /// Build a minimal ProxyAppState with one mock provider.
    async fn make_state() -> ProxyAppState {
        let mut providers: HashMap<String, Arc<dyn crate::proxy::providers::Provider>> =
            HashMap::new();
        let mock = MockProvider::new(
            "mock".into(),
            vec!["m".into()],
            true,
            CircuitBreakerConfig::default(),
        );
        providers.insert("mock".into(), Arc::new(mock));
        let router = Arc::new(Router::new(providers, HashMap::new()));
        ProxyAppState {
            router,
            auth_token: String::new(),
            pinned_provider: Arc::new(std::sync::Mutex::new(None)),
            responses_cot_enabled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            passthrough_auth_allowed: false,
            db: None, // tests don't exercise failover-winner persistence
            live: None,
            allow_client_config_repair: false,
        }
    }

    async fn gemini_test_call(
        state: ProxyAppState,
        tail: &str,
        method: Method,
        body: Option<Value>,
    ) -> Response {
        gemini_native(
            State(state),
            Path(tail.into()),
            method,
            HeaderMap::new(),
            Query(HashMap::new()),
            body.map(|body| Ok(Json(body))),
        )
        .await
    }

    async fn response_json(response: Response) -> Value {
        serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), 128 * 1024)
                .await
                .unwrap(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn catalog_refresh_requires_gateway_auth_and_removes_revoked_cached_supply() {
        let (db,live,_root)=crate::services::proxy_runtime::live::admission_catalog_fixture().await;
        let mut state=make_state().await;
        state.router=live.current().router.clone(); state.live=Some(live.clone()); state.db=Some(db.clone());
        state.auth_token="catalog_gateway_fixture".into();
        let (stop,receiver)=oneshot::channel();
        let (bound,task)=serve_detached(socket_addr("127.0.0.1",0),state,receiver).await.unwrap();
        let client=reqwest::Client::builder().timeout(std::time::Duration::from_secs(5)).build().unwrap();
        let origin=format!("http://{}",bound.addr);
        let source=db.provider_subscription_value("catalog_fixture").unwrap();
        let generation=live.current().generation;
        for path in ["/v1/models","/v1beta/models","/claude-desktop/v1/models","/v1beta/models/m"] {
            let denied=client.get(format!("{origin}{path}")).bearer_auth("passthrough_only_fixture").send().await.unwrap();
            assert_eq!(denied.status(),StatusCode::UNAUTHORIZED);
        }
        assert_eq!(live.current().generation,generation);
        assert_eq!(db.provider_subscription_value("catalog_fixture").unwrap(),source);
        let initial:Value=client.get(format!("{origin}/v1/models")).bearer_auth("catalog_gateway_fixture").send().await.unwrap().json().await.unwrap();
        assert!(initial["data"].as_array().unwrap().iter().any(|entry|entry["id"]=="m"));
        assert_eq!(live.current().generation,generation);
        assert_eq!(db.provider_subscription_value("catalog_fixture").unwrap(),source,"fresh Active status is not queried again");
        let raw=source.unwrap(); let mut revoked:Value=serde_json::from_str(&raw).unwrap();
        revoked["observation"]["evidence"]["state"]=json!("inactive");
        revoked["observation"]["probeAllowed"]=json!(false);
        assert!(db.compare_exchange_provider_subscription("catalog_fixture",Some(&raw),&revoked.to_string()).unwrap());
        let revision=db.routing_revision().unwrap();
        let denied=client.get(format!("{origin}/v1/models")).send().await.unwrap();
        assert_eq!(denied.status(),StatusCode::UNAUTHORIZED);
        assert_eq!(live.current().generation,generation,"unauthorized reads must not publish a new snapshot");
        assert_eq!(db.routing_revision().unwrap(),revision);
        let removed:Value=client.get(format!("{origin}/v1/models")).bearer_auth("catalog_gateway_fixture").send().await.unwrap().json().await.unwrap();
        assert!(removed["data"].as_array().unwrap().is_empty());
        let gemini=client.get(format!("{origin}/v1beta/models/m?key=catalog_gateway_fixture")).send().await.unwrap();
        assert_eq!(gemini.status(),StatusCode::NOT_FOUND);
        assert_eq!(live.current().queue_count,0);
        let usage_before=db.count_usage().unwrap();
        assert!(live.current().router.health().is_empty());
        for (selector,code,message) in [
            ("alias:haiku","model_alias_no_candidates","No eligible model can satisfy the requested routing tier."),
            ("precise:private-exact-fixture","model_precise_unavailable","No eligible provider is available for the requested exact model."),
        ] {
            let response=client.post(format!("{origin}/v1/chat/completions"))
                .bearer_auth("catalog_gateway_fixture")
                .json(&json!({"model":selector,"messages":[{"role":"user","content":"ok"}],"max_tokens":16,"stream":false}))
                .send().await.unwrap();
            assert_eq!(response.status(),StatusCode::NOT_FOUND);
            let body:Value=response.json().await.unwrap();
            assert_eq!(body["error"]["code"],code);
            assert_eq!(body["error"]["message"],message);
            assert!(!body.to_string().contains(selector));
        }
        assert_eq!(db.count_usage().unwrap(),usage_before);
        assert!(live.current().router.last_used.lock().unwrap().is_empty(),"resolution must fail before any upstream attempt");
        db.set_setting("model_utility_policy", "invalid fixture JSON").unwrap();
        let failed=client.get(format!("{origin}/v1/models")).bearer_auth("catalog_gateway_fixture").send().await.unwrap();
        assert_eq!(failed.status(),StatusCode::SERVICE_UNAVAILABLE);
        let body:Value=failed.json().await.unwrap();
        assert_eq!(body["error"]["code"],"model_catalog_refresh_failed");
        assert!(body.get("data").is_none());
        let _=stop.send(()); task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn runtime_identity_distinguishes_desktop_from_node_without_private_state() {
        use axum::response::IntoResponse;

        let legacy = response_json(
            discovery_identity(Query(HashMap::new()))
                .await
                .into_response(),
        )
        .await;
        assert_eq!(legacy.as_object().unwrap().len(), 4);
        assert!(legacy.get("runtime_kind").is_none());

        let response = discovery_identity(Query(HashMap::from([("runtime".into(), "1".into())])))
            .await
            .into_response();
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        let identity = response_json(response).await;
        assert_eq!(identity["service"], "ai-agent-gateway");
        assert_eq!(identity["schemaVersion"], 1);
        assert_eq!(identity["status"], "running");
        assert_eq!(identity["runtime_kind"], "desktop");
        assert_eq!(identity.as_object().unwrap().len(), 5);

        let status = response_json(health(State(make_state().await), HeaderMap::new()).await).await;
        assert_eq!(status["runtime_kind"], "desktop");
        assert_eq!(status["status"], "ok");
        assert!(status["providers"].is_array());
        assert!(status["models"].is_array());
        assert_eq!(status["availableModels"], json!(["m"]));
    }

    #[tokio::test]
    async fn health_separates_listener_liveness_from_available_model_catalog() {
        let mut state = make_state().await;
        state.router = Arc::new(Router::new(
            HashMap::new(),
            HashMap::from([(
                "declared_but_unavailable".into(),
                vec![crate::database::dao::routes::RouteHop {
                    provider: "missing_provider".into(),
                    model: "unavailable_model".into(),
                }],
            )]),
        ));
        let status = response_json(health(State(state), HeaderMap::new()).await).await;
        assert_eq!(status["status"], "ok");
        assert_eq!(status["models"], json!(["declared_but_unavailable"]));
        assert_eq!(status["availableModels"], json!([]));
    }

    #[tokio::test]
    async fn discovery_includes_offered_models_without_manual_routes_and_rejects_unknown_model_info(
    ) {
        let state = make_state().await;
        let listed = response_json(list_models(State(state.clone()), HeaderMap::new()).await).await;
        assert!(listed["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["id"] == "m"));
        let listed = response_json(
            gemini_list_models(
                State(state.clone()),
                HeaderMap::new(),
                Query(HashMap::new()),
            )
            .await,
        )
        .await;
        let info = listed["models"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["name"] == "models/m")
            .unwrap();
        assert!(info.get("inputTokenLimit").is_none());
        assert!(info.get("outputTokenLimit").is_none());
        assert_eq!(
            gemini_test_call(state.clone(), "m", Method::GET, None)
                .await
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            gemini_test_call(state, "not-offered", Method::GET, None)
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn gemini_count_estimate_includes_system_and_tools_and_declares_its_mode() {
        let body = json!({
            "contents":[{"role":"user","parts":[{"text":"fixture"}]}],
            "systemInstruction":{"parts":[{"text":"a substantial system instruction"}]},
            "tools":[{"functionDeclarations":[{"name":"lookup","description":"a substantial tool declaration","parameters":{"type":"OBJECT"}}]}]
        });
        let response = gemini_test_call(
            make_state().await,
            "m:countTokens",
            Method::POST,
            Some(body.clone()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["x-aag-token-count-mode"], "estimate");
        let counted = response_json(response).await["totalTokens"]
            .as_u64()
            .unwrap();
        assert_eq!(
            counted as usize,
            serde_json::to_vec(&body).unwrap().len().div_ceil(4)
        );
        assert!(
            counted as usize
                > serde_json::to_vec(&body["contents"])
                    .unwrap()
                    .len()
                    .div_ceil(4)
        );
    }

    #[tokio::test]
    async fn gemini_generation_captures_hot_reloaded_provider_pin() {
        use crate::database::dao::proxy_providers::ProxyProvider;
        use crate::database::dao::routes::{Route, RouteHop};
        use crate::services::proxy_runtime::{
            live::Live_runtime, snapshot::build_runtime_snapshot,
        };
        let db = crate::database::Database::memory().unwrap();
        for id in ["first", "second"] {
            let provider: ProxyProvider = serde_json::from_value(
                json!({"id":id,"name":id,"type":"mock","models":["m"],"enabled":true}),
            )
            .unwrap();
            db.save_proxy_provider(&provider).unwrap();
        }
        db.save_route(&Route {
            alias: "m".into(),
            description: String::new(),
            hops: vec![
                RouteHop {
                    provider: "first".into(),
                    model: "m".into(),
                },
                RouteHop {
                    provider: "second".into(),
                    model: "m".into(),
                },
            ],
        })
        .unwrap();
        let pin_key = crate::database::dao::settings::KEY_PROXY_PINNED_PROVIDER;
        db.set_setting(pin_key, "first").unwrap();
        let config = db.get_proxy_config().unwrap();
        let snapshot = build_runtime_snapshot(&db, &config).await.unwrap();
        let mut state = make_state().await;
        state.router = Arc::clone(&snapshot.router);
        let live = Live_runtime::new(db.clone(), config, snapshot);
        state.live = Some(Arc::clone(&live));
        let body = json!({"contents":[{"role":"user","parts":[{"text":"fixture"}]}]});
        let first = response_json(
            gemini_test_call(
                state.clone(),
                "m:generateContent",
                Method::POST,
                Some(body.clone()),
            )
            .await,
        )
        .await;
        assert!(first["candidates"][0]["content"]["parts"][0]["text"]
            .as_str()
            .unwrap()
            .contains("MOCK[first]"));
        db.set_setting(pin_key, "second").unwrap();
        assert!(live.reload().await.unwrap());
        let second = response_json(
            gemini_test_call(state, "m:generateContent", Method::POST, Some(body)).await,
        )
        .await;
        assert!(second["candidates"][0]["content"]["parts"][0]["text"]
            .as_str()
            .unwrap()
            .contains("MOCK[second]"));
    }

    #[tokio::test]
    async fn external_error_envelope_never_echoes_private_details() {
        let private_values = [
            "CANARY_PROMPT_UNIQUE_91",
            "CANARY_RESPONSE_UNIQUE_92",
            "sk-private-93",
            r"C:\Users\private-user\secret.txt",
            "https://user:pass@private.example.invalid/path?token=94#fragment",
            "private.person@example.invalid",
            "100.64.12.34",
            "private-node.tail123.ts.net",
        ];
        let response = err_response(502, private_values.join(" | "), "proxy_error");
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("read error body");
        let rendered = String::from_utf8(body.to_vec()).expect("utf8 error body");
        for private in private_values {
            assert!(
                !rendered.contains(private),
                "private value survived error boundary: {private} in {rendered}"
            );
        }
    }

    /// The Gemini countTokens estimate must count serialized JSON bytes —
    /// escaping, quotes and structural characters all count — so the
    /// counting sink must reproduce `to_string(..).len()` exactly without
    /// materializing the string (the "sum raw string lengths" shortcut would
    /// undercount and change totalTokens).
    #[test]
    fn gemini_count_tokens_counts_serialized_bytes_like_to_string() {
        let contents = json!([
            {"role": "user", "parts": [{"text": "推理😀 \"quoted\"\nline"}]},
            {"inlineData": {"mimeType": "image/png", "data": "aGVsbG8="}},
        ]);
        let mut writer = Byte_counting_writer::new();
        serde_json::to_writer(&mut writer, &contents).unwrap();
        assert_eq!(
            writer.written,
            serde_json::to_string(&contents).unwrap().len(),
            "the sink must measure the same bytes a materialized string would"
        );

        // Structural bytes count: a bare string contributes its quotes and
        // escapes, not just its character count.
        let bare = json!("ab\"c😀");
        let mut writer = Byte_counting_writer::new();
        serde_json::to_writer(&mut writer, &bare).unwrap();
        assert_eq!(writer.written, serde_json::to_string(&bare).unwrap().len());
        assert!(writer.written > bare.as_str().unwrap().chars().count());
    }

    #[tokio::test]
    async fn health_and_models_require_configured_auth() {
        let mut state = make_state().await;
        state.auth_token = "private-gateway-token".to_string();

        for response in [
            health(State(state.clone()), HeaderMap::new()).await,
            list_models(State(state.clone()), HeaderMap::new()).await,
        ] {
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
                .await
                .expect("read unauthorized body");
            let rendered = String::from_utf8(body.to_vec()).expect("utf8 body");
            assert!(!rendered.contains("mock"));
            assert!(!rendered.contains("private-gateway-token"));
        }

        let mut headers = HeaderMap::new();
        headers.insert(
            "x-api-key",
            HeaderValue::from_static("private-gateway-token"),
        );
        assert_eq!(
            health(State(state.clone()), headers.clone()).await.status(),
            StatusCode::OK
        );
        assert_eq!(
            list_models(State(state), headers).await.status(),
            StatusCode::OK
        );
    }

    /// Build a handler payload the way the extractor now does it: parse the
    /// wire JSON straight into the request type the surface extracts.
    fn anthropic_payload(
        value: Value,
    ) -> Result<axum::Json<AnthropicRequest>, axum::extract::rejection::JsonRejection> {
        Ok(axum::Json(
            serde_json::from_value(value).expect("test payload is a valid Anthropic request"),
        ))
    }

    fn chat_payload(
        value: Value,
    ) -> Result<axum::Json<ChatCompletionRequest>, axum::extract::rejection::JsonRejection> {
        Ok(axum::Json(
            serde_json::from_value(value).expect("test payload is a valid chat request"),
        ))
    }

    /// A minimal valid Anthropic Messages payload (non-stream) for direct
    /// handler invocations.
    fn anthropic_test_payload(
    ) -> Result<axum::Json<AnthropicRequest>, axum::extract::rejection::JsonRejection> {
        anthropic_payload(serde_json::json!({
            "model": "m",
            "max_tokens": 16,
            "messages": [{"role": "user", "content": "auth probe"}],
        }))
    }

    #[tokio::test]
    async fn typed_ingress_keeps_auth_precedence_over_json_rejection() {
        use tower::Service;
        let mut state = make_state().await;
        state.auth_token = "fixture_gateway".into();
        for path in ["/v1/chat/completions", "/v1/messages", "/v1/responses"] {
            for body in ["{", "[]", r#"{"model":1,"messages":[]}"#] {
                for authenticated in [false, true] {
                    let mut app = build_router(state.clone());
                    let mut request = axum::http::Request::builder()
                        .uri(path)
                        .method("POST")
                        .header("content-type", "application/json");
                    if authenticated {
                        request = request.header("authorization", "Bearer fixture_gateway");
                    }
                    let response = app
                        .call(request.body(axum::body::Body::from(body)).unwrap())
                        .await
                        .unwrap();
                    assert_eq!(
                        response.status().as_u16(),
                        if authenticated { 400 } else { 401 },
                        "{path}"
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn wire_credential_injection_cannot_escape_passthrough_scope() {
        use tower::Service;
        let mut state = make_state().await;
        state.auth_token = "fixture_gateway".into();
        state.passthrough_auth_allowed = true;
        *state.pinned_provider.lock().unwrap() = Some("mock".into());
        for stream in [false, true] {
            let mut app = build_router(state.clone());
            let body = serde_json::json!({
                "model": "m", "max_tokens": 16, "messages": [], "stream": stream,
                "client_credential": { "passthrough_only": false, "authorization": "Bearer fixture_gateway" }
            });
            let response = app
                .call(
                    axum::http::Request::builder()
                        .uri("/v1/messages")
                        .method("POST")
                        .header("content-type", "application/json")
                        .header("authorization", "Bearer fixture_relay")
                        .body(axum::body::Body::from(body.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        }
    }

    #[test]
    fn anthropic_auth_scope_is_assigned_by_the_edge() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", HeaderValue::from_static("Bearer gateway"));
        let credential = check_anthropic_auth(&headers, "gateway", true)
            .unwrap()
            .unwrap();
        assert!(!credential.passthrough_only);
        assert_eq!(credential.authorization.as_deref(), Some("Bearer gateway"));
        let credential = check_anthropic_auth(&headers, "", true).unwrap().unwrap();
        assert!(
            !credential.passthrough_only,
            "disabled gateway auth grants ordinary routing"
        );
        let credential = check_anthropic_auth(&headers, "different", true)
            .unwrap()
            .unwrap();
        assert!(credential.passthrough_only);
        assert!(check_anthropic_auth(&headers, "different", false).is_err());
        headers.clear();
        headers.insert("x-api-key", HeaderValue::from_static("relay_key"));
        let credential = check_anthropic_auth(&headers, "gateway", true)
            .unwrap()
            .unwrap();
        assert!(credential.passthrough_only);
        assert_eq!(credential.x_api_key.as_deref(), Some("relay_key"));
        headers.insert("x-api-key", HeaderValue::from_static(""));
        assert!(check_anthropic_auth(&headers, "gateway", true).is_err());
        headers.clear();
        assert!(check_anthropic_auth(&headers, "gateway", true).is_err());
        assert!(
            !check_anthropic_auth(&headers, "", true)
                .unwrap()
                .unwrap()
                .passthrough_only
        );
    }

    #[tokio::test]
    async fn passthrough_only_messages_cannot_use_static_routes_or_pins() {
        let mut state = make_state().await;
        state.auth_token = "gateway".into();
        state.passthrough_auth_allowed = true;
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            HeaderValue::from_static("Bearer invalid_relay"),
        );
        for pinned in [None, Some("mock".to_string())] {
            *state
                .pinned_provider
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = pinned;
            for stream in [false, true] {
                let response = messages(
                    State(state.clone()),
                    headers.clone(),
                    anthropic_payload(serde_json::json!({
                        "model": "m", "max_tokens": 16, "messages": [], "stream": stream,
                    })),
                )
                .await;
                assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            }
        }
    }

    #[tokio::test]
    async fn disabled_gateway_auth_retains_static_routing_with_client_headers() {
        let mut state = make_state().await;
        state.passthrough_auth_allowed = true;
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            HeaderValue::from_static("Bearer client_relay"),
        );
        let response = messages(
            State(state.clone()),
            headers.clone(),
            anthropic_test_payload(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let response =
            messages_count_tokens(State(state), headers, count_tokens_test_payload()).await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn passthrough_relaxation_admits_subscription_bearer_on_messages_only() {
        let mut state = make_state().await;
        state.auth_token = "private-gateway-token".to_string();
        state.passthrough_auth_allowed = true;
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            HeaderValue::from_static("Bearer oauth-subscription-relay-77"),
        );
        // /v1/messages: the subscription bearer passes auth (any later
        // failure is a routing outcome, not 401).
        let response = messages(
            State(state.clone()),
            headers.clone(),
            anthropic_test_payload(),
        )
        .await;
        assert_ne!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "passthrough relaxation must admit a subscription bearer on /v1/messages"
        );
        // The OpenAI surface keeps strict auth with the same credential.
        let chat_response = chat_completions(
            State(state),
            headers,
            chat_payload(serde_json::json!({
                "model": "m",
                "messages": [{"role": "user", "content": "auth probe"}],
            })),
        )
        .await;
        assert_eq!(
            chat_response.status(),
            StatusCode::UNAUTHORIZED,
            "passthrough relaxation must be /v1/messages-only"
        );
    }

    #[tokio::test]
    async fn default_state_rejects_non_gateway_credential_on_messages() {
        let mut state = make_state().await;
        state.auth_token = "private-gateway-token".to_string();
        // passthrough_auth_allowed stays false (fail-closed default).
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            HeaderValue::from_static("Bearer oauth-subscription-relay-77"),
        );
        let response = messages(State(state), headers, anthropic_test_payload()).await;
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "without a passthrough provider the strict token gate must hold"
        );
    }

    /// The passthrough auth relaxation must only ever activate on a bind
    /// that accepts local connections. Wildcard binds (`0.0.0.0`, `::`,
    /// `[::]`) listen on every interface — treating them as loopback would
    /// hand the stored static-key pool to LAN clients. Regression test for
    /// the review finding that had `0.0.0.0` in the loopback set.
    #[test]
    fn wildcard_binds_are_not_loopback_for_passthrough_relaxation() {
        for host in ["", "127.0.0.1", "localhost", "::1", "[::1]", " 127.0.0.1 "] {
            assert!(host_is_loopback(host), "{host:?} should count as loopback");
        }
        for host in ["0.0.0.0", "::", "[::]", "10.0.0.2", "aag.lan"] {
            assert!(
                !host_is_loopback(host),
                "{host:?} is network-reachable and must fail closed"
            );
        }
    }

    fn count_tokens_test_payload(
    ) -> Result<axum::Json<Value>, axum::extract::rejection::JsonRejection> {
        Ok(axum::Json(serde_json::json!({
            "model": "m",
            "messages": [{"role": "user", "content": "measure this conversation"}],
        })))
    }

    /// count_tokens sits on the Anthropic surface: the gateway token passes
    /// as always, and the passthrough relaxation admits a subscription
    /// bearer under the same fail-closed conditions as /v1/messages. The
    /// mock hop serves the local estimate.
    #[tokio::test]
    async fn count_tokens_serves_estimates_and_honors_the_relaxation_gate() {
        let mut state = make_state().await;
        state.auth_token = "private-gateway-token".to_string();
        state.passthrough_auth_allowed = true;

        let mut relay_headers = HeaderMap::new();
        relay_headers.insert(
            "authorization",
            HeaderValue::from_static("Bearer oauth-subscription-relay-77"),
        );
        let response = messages_count_tokens(
            State(state.clone()),
            relay_headers,
            count_tokens_test_payload(),
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "passthrough admission cannot invoke the stored-key count provider"
        );
        let mut gateway_headers = HeaderMap::new();
        gateway_headers.insert(
            "x-api-key",
            HeaderValue::from_static("private-gateway-token"),
        );
        let response = messages_count_tokens(
            State(state.clone()),
            gateway_headers,
            count_tokens_test_payload(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("read count body");
        let value: Value = serde_json::from_slice(&body).expect("parse count body");
        assert_eq!(
            value.get("type").and_then(Value::as_str),
            None,
            "the Anthropic count envelope has no type field"
        );
        assert!(
            value.get("input_tokens").and_then(Value::as_u64).is_some(),
            "an integer input_tokens must be present: {value}"
        );

        // Without the relaxation the strict token gate must hold.
        let mut state_strict = make_state().await;
        state_strict.auth_token = "private-gateway-token".to_string();
        let mut relay_headers_again = HeaderMap::new();
        relay_headers_again.insert(
            "authorization",
            HeaderValue::from_static("Bearer oauth-subscription-relay-77"),
        );
        let response = messages_count_tokens(
            State(state_strict),
            relay_headers_again,
            count_tokens_test_payload(),
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "without a passthrough provider the strict token gate must hold"
        );

        // A missing model is a client error, not a routing attempt (auth is
        // checked first, so use an auth-disabled state here).
        let no_auth_state = make_state().await;
        let empty = Ok(axum::Json(serde_json::json!({"messages": []})));
        let response = messages_count_tokens(State(no_auth_state), HeaderMap::new(), empty).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn anthropic_errors_keep_the_top_level_protocol_envelope() {
        let response = anthropic_err_response(502, "CANARY_PRIVATE_UPSTREAM_MESSAGE", "api_error");
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("read Anthropic error body");
        let value: Value = serde_json::from_slice(&body).expect("parse Anthropic error body");
        assert_eq!(value.get("type").and_then(Value::as_str), Some("error"));
        assert_eq!(
            value.pointer("/error/type").and_then(Value::as_str),
            Some("api_error")
        );
        assert!(value
            .pointer("/error/message")
            .and_then(Value::as_str)
            .is_some());
        assert!(value.pointer("/error/error").is_none());
        assert!(!String::from_utf8_lossy(&body).contains("CANARY_PRIVATE_UPSTREAM_MESSAGE"));
    }

    #[test]
    fn anthropic_upstream_error_type_only_relays_allowlisted_tokens() {
        use crate::proxy::canonical::anthropic_upstream_error_type;
        assert_eq!(
            anthropic_upstream_error_type(r#"{"type":"error","error":{"type":"rate_limit_error","message":"slow down"}}"#),
            Some("rate_limit_error")
        );
        assert_eq!(
            anthropic_upstream_error_type(r#"{"error":{"type":"overloaded_error"}}"#),
            Some("overloaded_error")
        );
        // Unknown spellings and unparseable bodies never relay a type.
        assert_eq!(
            anthropic_upstream_error_type(r#"{"error":{"type":"teapot_error"}}"#),
            None
        );
        assert_eq!(anthropic_upstream_error_type("not json"), None);
        assert_eq!(anthropic_upstream_error_type("{}"), None);
    }

    #[test]
    fn anthropic_error_kind_relays_upstream_types_and_maps_local_statuses() {
        let mut error = crate::proxy::canonical::make_provider_error(
            "upstream_http_error",
            "response_status",
            Some(429),
            None,
            None,
        );
        assert_eq!(anthropic_error_kind(&error), "rate_limit_error");
        error.status = Some(529);
        assert_eq!(anthropic_error_kind(&error), "overloaded_error");
        error.status = Some(401);
        assert_eq!(anthropic_error_kind(&error), "authentication_error");
        error.status = Some(400);
        assert_eq!(anthropic_error_kind(&error), "invalid_request_error");
        error.status = Some(500);
        assert_eq!(anthropic_error_kind(&error), "api_error");
        // The upstream's own allowlisted type outranks the local mapping.
        error.upstream_error_type = Some("permission_error");
        error.status = Some(429);
        assert_eq!(anthropic_error_kind(&error), "permission_error");
    }

    #[tokio::test]
    async fn anthropic_messages_accept_image_sized_request_body() {
        let state = make_state().await;
        let (tx, rx) = oneshot::channel();
        let (bound, handle) = serve_detached(socket_addr("127.0.0.1", 0), state, rx)
            .await
            .expect("bind server");
        let image_data = "A".repeat(2_200_000);
        let request = serde_json::json!({
            "model": "m",
            "max_tokens": 16,
            "messages": [{
                "role": "user",
                "content": [
                    {
                        "type": "image",
                        "source": {
                            "type": "base64",
                            "media_type": "image/png",
                            "data": image_data
                        }
                    },
                    {"type": "text", "text": "Describe this image."}
                ]
            }]
        });
        let response = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(3))
            .build()
            .expect("build client")
            .post(format!("http://{}/v1/messages", bound.addr))
            .json(&request)
            .send()
            .await
            .expect("send image-sized request");
        let status = response.status();
        let body = response.text().await.expect("read response");
        let _ = tx.send(());
        let shutdown_result = tokio::time::timeout(std::time::Duration::from_secs(1), handle).await;
        assert!(
            matches!(shutdown_result, Ok(Ok(Ok(())))),
            "server did not shut down cleanly: {shutdown_result:?}"
        );
        assert!(
            status.is_success(),
            "image-sized request returned {status}: {body}"
        );
        assert!(body.contains("MOCK"), "mock response missing: {body}");
    }

    /// A large JSON body (~3 MiB) on `/v1/responses` passes the shared
    /// `LARGE_REQUEST_BODY_BYTES` cap and reaches the mock provider.
    #[tokio::test]
    async fn responses_accept_large_request_body() {
        let state = make_state().await;
        let (tx, rx) = oneshot::channel();
        let (bound, handle) = serve_detached(socket_addr("127.0.0.1", 0), state, rx)
            .await
            .expect("bind server");
        let filler = "A".repeat(2_200_000);
        let request = serde_json::json!({
            "model": "m",
            "input": [{"role": "user", "content": [{"type": "input_text", "text": filler}]}],
            "stream": false
        });
        let response = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(3))
            .build()
            .expect("build client")
            .post(format!("http://{}/v1/responses", bound.addr))
            .json(&request)
            .send()
            .await
            .expect("send large responses request");
        let status = response.status();
        let body = response.text().await.expect("read response");
        let _ = tx.send(());
        let shutdown_result = tokio::time::timeout(std::time::Duration::from_secs(1), handle).await;
        assert!(
            matches!(shutdown_result, Ok(Ok(Ok(())))),
            "{shutdown_result:?}"
        );
        assert!(
            status.is_success(),
            "large responses body returned {status}: {body}"
        );
        assert!(body.contains("MOCK"), "mock response missing: {body}");
    }

    /// A large JSON body (~3 MiB) on `/v1/chat/completions` passes the shared
    /// `LARGE_REQUEST_BODY_BYTES` cap and reaches the mock provider.
    #[tokio::test]
    async fn chat_completions_accept_large_request_body() {
        let state = make_state().await;
        let (tx, rx) = oneshot::channel();
        let (bound, handle) = serve_detached(socket_addr("127.0.0.1", 0), state, rx)
            .await
            .expect("bind server");
        let filler = "A".repeat(2_200_000);
        let request = serde_json::json!({
            "model": "m",
            "messages": [{"role": "user", "content": filler}],
            "stream": false
        });
        let response = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(3))
            .build()
            .expect("build client")
            .post(format!("http://{}/v1/chat/completions", bound.addr))
            .json(&request)
            .send()
            .await
            .expect("send large chat request");
        let status = response.status();
        let body = response.text().await.expect("read response");
        let _ = tx.send(());
        let shutdown_result = tokio::time::timeout(std::time::Duration::from_secs(1), handle).await;
        assert!(
            matches!(shutdown_result, Ok(Ok(Ok(())))),
            "{shutdown_result:?}"
        );
        assert!(
            status.is_success(),
            "large chat body returned {status}: {body}"
        );
        assert!(body.contains("MOCK"), "mock response missing: {body}");
    }

    /// The Gemini-native SSE surface must frame every event as a complete
    /// `data: <json>\n\n` line. The Gemini CLI's SSE parser rejects bare
    /// JSON chunks with "Incomplete JSON segment at the end" — this test
    /// pins the framing so a regression to unframed bodies cannot ship.
    #[tokio::test]
    async fn gemini_native_stream_frames_complete_sse_events() {
        let state = make_state().await;
        let (tx, rx) = oneshot::channel();
        let (bound, handle) = serve_detached(socket_addr("127.0.0.1", 0), state, rx)
            .await
            .expect("bind server");
        let request = serde_json::json!({
            "contents": [{ "role": "user", "parts": [{ "text": "Reply with one word: pong" }] }],
        });
        let response = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .expect("build client")
            .post(format!(
                "http://{}/v1beta/models/m:streamGenerateContent?alt=sse",
                bound.addr
            ))
            .json(&request)
            .send()
            .await
            .expect("send gemini stream request");
        assert!(response.status().is_success());
        let body = response.text().await.expect("read stream body");
        assert!(!body.is_empty(), "stream must produce events");
        // Every non-empty line must be a complete `data: <json>` event.
        for line in body.lines() {
            if line.is_empty() {
                continue;
            }
            let Some(payload) = line.strip_prefix("data: ") else {
                panic!("unframed SSE line: {line:?}");
            };
            serde_json::from_str::<serde_json::Value>(payload)
                .unwrap_or_else(|error| panic!("incomplete JSON segment {error}: {payload:?}"));
        }
        // The terminal event must carry a finishReason so the CLI can close
        // the turn cleanly.
        assert!(
            body.contains("\"finishReason\""),
            "stream must include a finishReason event: {body}"
        );
        let _ = tx.send(());
        let shutdown_result = tokio::time::timeout(std::time::Duration::from_secs(1), handle).await;
        assert!(
            matches!(shutdown_result, Ok(Ok(Ok(())))),
            "{shutdown_result:?}"
        );
    }

    #[test]
    fn browser_origin_allowlist_is_desktop_scoped() {
        assert!(trusted_browser_origin(&HeaderValue::from_static(
            "tauri://localhost"
        )));
        assert!(trusted_browser_origin(&HeaderValue::from_static(
            "http://tauri.localhost"
        )));
        assert!(!trusted_browser_origin(&HeaderValue::from_static(
            "https://attacker.example.invalid"
        )));
        assert!(!trusted_browser_origin(&HeaderValue::from_static("null")));
        // Dev builds follow Vite's auto-incrementing port instead of a
        // fixed 1421 (ui/vite.config.ts), so any loopback port is trusted
        // while non-loopback hosts stay forbidden.
        if cfg!(debug_assertions) {
            for origin in [
                "http://localhost:1421",
                "http://localhost:1422",
                "http://127.0.0.1:1430",
            ] {
                assert!(
                    trusted_browser_origin(&HeaderValue::from_static(origin)),
                    "{origin}"
                );
            }
            assert!(!trusted_browser_origin(&HeaderValue::from_static(
                "http://localhost:notaport"
            )));
            assert!(!trusted_browser_origin(&HeaderValue::from_static(
                "http://192.168.1.5:1421"
            )));
        }
    }

    #[tokio::test]
    async fn empty_auth_cannot_bind_a_non_loopback_listener() {
        let state = make_state().await;
        let (_tx, rx) = oneshot::channel();
        let error = serve_detached(socket_addr("0.0.0.0", 0), state, rx)
            .await
            .expect_err("empty auth on a non-loopback bind must fail closed");
        assert_eq!(error, "proxy_auth_required_for_non_loopback_bind");
    }

    /// Pick an ephemeral free port from the OS so the test doesn't
    /// conflict with anything else. The listener is dropped inside
    /// this call (callers that want to HOLD the port re-bind it, e.g.
    /// `serve_returns_error_when_port_held`).
    ///
    /// NOTE (23h): an earlier fix held these listeners in a process-wide
    /// pool to close the free_port -> serve() race, but on Windows a
    /// held std listener BLOCKS the test's own second bind / serve
    /// bind (SO_REUSEADDR semantics differ), deadlocking the suite.
    /// The rare race is instead tolerated by the best-effort
    /// `tx.send(())` in the old test shape; the rewritten
    /// `serve_returns_actual_bound_port` binds port 0 and has no race.
    async fn free_port() -> u16 {
        let l = tokio::net::TcpListener::bind(socket_addr("127.0.0.1", 0))
            .await
            .expect("bind ephemeral");
        l.local_addr().unwrap().port()
    }

    #[tokio::test]
    async fn serve_returns_actual_bound_port() {
        let mut state = make_state().await;
        state.db = Some(crate::database::Database::memory().expect("memory db"));
        let (tx, rx) = oneshot::channel();
        let (bound, handle) = serve_detached(socket_addr("127.0.0.1", 0), state, rx)
            .await
            .expect("bind server");
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_millis(500))
            .timeout(std::time::Duration::from_secs(1))
            .build()
            .expect("build client");
        let health_result = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            client.get(format!("http://{}/health", bound.addr)).send(),
        )
        .await;
        let _ = tx.send(());
        let shutdown_result = tokio::time::timeout(std::time::Duration::from_secs(1), handle).await;
        assert!(
            matches!(shutdown_result, Ok(Ok(Ok(())))),
            "server did not shut down cleanly: {shutdown_result:?}"
        );
        let response = health_result
            .expect("health request timed out")
            .expect("health request failed");
        assert!(response.status().is_success());
        assert!(
            bound.addr.port() > 0,
            "kernel-assigned port must be non-zero"
        );
        assert_eq!(bound.addr.ip().to_string(), "127.0.0.1");
    }

    /// The fallback contract: when the requested port is in use, a caller
    /// can try the next port and `serve()` will bind that one. Here we
    /// directly exercise serve() against a held port and verify the error
    /// is reported cleanly — the higher-level loop in `proxy_start` is the
    /// one that decides to retry; this test confirms serve() itself
    /// surfaces a clean error so the retry is meaningful.
    #[tokio::test]
    async fn serve_returns_error_when_port_held() {
        let port = free_port().await;
        // Hold the port.
        let _holder = tokio::net::TcpListener::bind(socket_addr("127.0.0.1", port))
            .await
            .expect("hold port");
        let state = make_state().await;
        let (_tx, rx) = oneshot::channel();
        let err = serve(socket_addr("127.0.0.1", port), state, rx)
            .await
            .expect_err("bind to held port must fail");
        assert!(err.contains("bind"), "expected bind error, got: {err}");
    }

    /// End-to-end port-fallback: hold the requested port, then mimic the
    /// retry loop in `proxy_start` by trying the next 5 ports. Confirms the
    /// bind-and-probe pattern produces a working fallback server.
    #[tokio::test]
    async fn port_fallback_finds_next_free_port() {
        let base = free_port().await;
        // Hold `base` so the first attempt fails.
        let _holder = tokio::net::TcpListener::bind(socket_addr("127.0.0.1", base))
            .await
            .expect("hold base");
        let mut found: Option<BoundInfo> = None;
        let mut last_err = String::new();
        for offset in 1u16..=5 {
            let try_port = base + offset;
            let state = make_state().await;
            let (tx, rx) = oneshot::channel();
            let addr = socket_addr("127.0.0.1", try_port);
            let handle = tokio::spawn(async move { serve(addr, state, rx).await });
            tokio::time::sleep(std::time::Duration::from_millis(80)).await;
            // Probe /health to confirm we own the port.
            let probe = reqwest::Client::builder()
                .timeout(std::time::Duration::from_millis(500))
                .build()
                .unwrap()
                .get(format!("http://127.0.0.1:{try_port}/health"))
                .send()
                .await;
            match probe {
                Ok(r) if r.status().is_success() => {
                    let _ = tx.send(()); // request graceful shutdown
                    let bound = handle.await.expect("task").expect("serve ok");
                    found = Some(bound);
                    break;
                }
                Ok(_) | Err(_) => {
                    last_err = format!("port {try_port} probe failed");
                    let _ = tx.send(());
                    let _ = handle.await;
                }
            }
        }
        let bound = found.expect(&format!("no free port in base+1..base+5: {last_err}"));
        assert!(bound.addr.port() > base, "fallback must use a higher port");
        assert!(bound.addr.port() <= base + 5);
    }

    #[tokio::test]
    #[ignore = "uses one configured live upstream request; run only with explicit authorization"]
    async fn live_anthropic_thinking_canary() {
        let db = crate::database::Database::init().expect("open configured AAG database");
        let model =
            std::env::var("AAG_LIVE_CANARY_MODEL").unwrap_or_else(|_| "claude-opus-4-8".into());
        let route = db
            .get_route(&model)
            .expect("read canary route")
            .expect("configured canary route");
        let first_hop = route
            .hops
            .first()
            .expect("canary route has a first hop")
            .clone();
        let provider_row = db
            .get_all_proxy_providers()
            .expect("read proxy providers")
            .into_iter()
            .find(|provider| provider.id == first_hop.provider)
            .expect("first-hop provider exists");
        let provider = crate::proxy::providers::build_provider(&provider_row)
            .expect("build first-hop provider");
        let mut providers: HashMap<String, Arc<dyn crate::proxy::providers::Provider>> =
            HashMap::new();
        providers.insert(first_hop.provider.clone(), Arc::from(provider));
        let routes = [(model.clone(), vec![first_hop])].into_iter().collect();
        let config = db.get_proxy_config().expect("read proxy config");
        let state = ProxyAppState {
            router: Arc::new(Router::new(providers, routes)),
            auth_token: config.auth_token.clone(),
            pinned_provider: Arc::new(std::sync::Mutex::new(None)),
            responses_cot_enabled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            passthrough_auth_allowed: false,
            db: None,
            live: None,
            allow_client_config_repair: false,
        };
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let (bound, handle) = serve_detached(socket_addr("127.0.0.1", 0), state, shutdown_rx)
            .await
            .expect("start canary proxy");
        let mut request = reqwest::Client::new()
            .post(format!("http://{}/v1/messages", bound.addr))
            .header("content-type", "application/json")
            .json(&json!({
                "model": model,
                "max_tokens": 512,
                "messages": [{"role": "user", "content": "Return exactly the word OK."}],
                "stream": true
            }));
        if !config.auth_token.is_empty() {
            request = request.header("x-api-key", config.auth_token);
        }
        let started = Instant::now();
        let response = request.send().await.expect("send canary request");
        assert!(
            response.status().is_success(),
            "canary HTTP status {}",
            response.status()
        );
        let mut byte_stream = response.bytes_stream();
        let mut first_byte_ms = None;
        let mut body = Vec::new();
        while let Some(chunk) = byte_stream.next().await {
            let chunk = chunk.expect("read canary stream");
            if first_byte_ms.is_none() && !chunk.is_empty() {
                first_byte_ms = Some(started.elapsed().as_millis());
            }
            body.extend_from_slice(&chunk);
        }
        let total_ms = started.elapsed().as_millis();
        let text = String::from_utf8(body).expect("canary SSE is UTF-8");
        let events: Vec<Value> = text
            .lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .filter_map(|payload| serde_json::from_str(payload).ok())
            .collect();
        assert!(events.iter().any(|event| event["type"] == "message_start"));
        assert!(events.iter().any(|event| event["type"] == "message_stop"));
        let usage = events
            .iter()
            .rev()
            .find_map(|event| event.get("usage"))
            .expect("canary returns usage");
        assert!(
            usage
                .get("output_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0)
                > 0,
            "canary output_tokens must be non-zero"
        );
        let thinking_seen = events.iter().any(|event| {
            event
                .get("delta")
                .and_then(|delta| delta.get("type"))
                .and_then(Value::as_str)
                == Some("thinking_delta")
        });
        let signature_seen = events.iter().any(|event| {
            event
                .get("delta")
                .and_then(|delta| delta.get("type"))
                .and_then(Value::as_str)
                == Some("signature_delta")
        });
        eprintln!(
            "live canary: first_byte_ms={} total_ms={} thinking_seen={} signature_seen={} output_tokens={}",
            first_byte_ms.unwrap_or(total_ms),
            total_ms,
            thinking_seen,
            signature_seen,
            usage["output_tokens"]
        );
        let _ = shutdown_tx.send(());
        handle
            .await
            .expect("canary server task")
            .expect("canary server shutdown");
    }
}
