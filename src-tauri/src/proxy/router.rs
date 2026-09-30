//! Router — resolves model aliases to ordered failover chains and executes
//! them with circuit breaker protection.
//!
//! Ported from `src/gateway/router.ts`.

use crate::database::dao::routes::RouteHop;
use crate::proxy::canonical::{
    self, ChatCompletionRequest, OAIChatResponse, OAIStreamChunk, ProviderError,
};
use crate::proxy::circuit_breaker::{CircuitBreakerSnapshot, Circuit_outcome, Circuit_permit};
use crate::proxy::providers::Provider;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

/// Health snapshot for a single provider (used in /health response).
#[derive(Debug, Serialize)]
pub struct ProviderHealth {
    pub id: String,
    pub enabled: bool,
    pub breaker: CircuitBreakerSnapshot,
}

/// A resolved route: the alias name and its ordered hops.
#[allow(dead_code)]
pub struct ResolvedRoute {
    pub alias: String,
    pub hops: Vec<RouteHop>,
}

/// The core routing engine. Immutable after construction (providers + routes
/// are built from the DB at proxy_start time). Each provider holds its own
/// `CircuitBreaker` internally, which is mutated atomically per-request.
pub struct Router {
    providers: HashMap<String, Arc<dyn Provider>>,
    routes: HashMap<String, Arc<Vec<RouteHop>>>,
    /// Legacy global failover queue (checked after explicit routes and
    /// per-provider auto-routing miss). Kept for tests / compatibility.
    queue: Vec<String>,
    /// Per-surface failover queues (Phase 17). A "surface" is an entry
    /// endpoint family: `"claude"` (`/v1/messages`), `"codex"`
    /// (`/v1/responses`), `"chat"` (`/v1/chat/completions`). Each app's
    /// provider chain (from the `providers` table, `in_failover_queue = 1`,
    /// current provider first) is tried in order when no explicit route or
    /// auto-route matches — the requested model passes through unchanged.
    queues: HashMap<String, Vec<String>>,
    /// The provider id that served the most recent successful request, per
    /// app type. Updated atomically by `complete_hops` / `stream_hops` on
    /// success. Read by the health endpoint for UI flow visualization AND
    /// by `persist_failover_winner` to record the current provider.
    ///
    /// Round 1 review fix: was a single global slot keyed only by
    /// provider id, which made cross-app races possible (a claude_code
    /// request's persist could read a codex winner after a concurrent
    /// codex request overwrote the slot). Scoping by app_type eliminates
    /// the cross-app race.
    pub last_used: Arc<std::sync::Mutex<std::collections::HashMap<String, String>>>,
    /// Per-hop first-byte timeout for streaming requests (ms). Once a
    /// provider has been selected and its stream opened, the router probes
    /// the first chunk within this budget; if nothing arrives, it records a
    /// STREAM TIMEOUT failure and moves to the next hop. Production default
    /// is 10 s (overridable by tests to keep them fast).
    pub first_byte_timeout_ms: u64,
}

/// Entry-endpoint surfaces, each backed by its own failover chain.
pub const SURFACE_CLAUDE: &str = "claude";
pub const SURFACE_CODEX: &str = "codex";
pub const SURFACE_CHAT: &str = "chat";
pub const SURFACE_GEMINI: &str = "gemini";

/// One admitted provider attempt. Dropping a canceled setup or response stream
/// releases its own half-open permit without fabricating success or failure.
#[allow(non_camel_case_types)]
struct Provider_attempt {
    provider: Arc<dyn Provider>,
    permit: Option<Circuit_permit>,
}

impl Provider_attempt {
    fn acquire(provider: Arc<dyn Provider>) -> Option<Self> {
        let permit = provider.breaker().acquire()?;
        Some(Self {
            provider,
            permit: Some(permit),
        })
    }

    fn settle(&mut self, outcome: Circuit_outcome) {
        if let Some(permit) = self.permit.take() {
            self.provider.breaker().settle(permit, outcome);
        }
    }
}

impl Drop for Provider_attempt {
    fn drop(&mut self) {
        self.settle(Circuit_outcome::Cancelled);
    }
}

/// Some OpenAI-shaped upstreams accept plain Responses requests but reject
/// function tools with a pre-stream HTTP 400. That is a hop capability mismatch,
/// so a finite route should try its next provider.
///
/// Only TRUE tool-capability rejections are retried: the upstream must have
/// been classified as such by `classify_upstream_4xx` (`kind ==
/// ToolCapability`). All other 400s — authentication failures
/// (`refresh_token` invalidated, "sign in again"), malformed user input,
/// and post-commit stream errors — remain terminal, matching the invariant
/// that auth and malformed input must never be replayed or hidden by
/// failover. An unclassified 400 is also terminal: the legacy default.
fn should_failover_tool_capability_rejection(
    request: &ChatCompletionRequest,
    error: &ProviderError,
) -> bool {
    request
        .tools
        .as_ref()
        .map(|tools| !tools.is_empty())
        .unwrap_or(false)
        && error.status == Some(400)
        && error.code == "upstream_http_error"
        && error.stage == "response_status"
        && error.kind == Some(canonical::UpstreamErrorKind::ToolCapability)
}

impl Router {
    /// Build a Router from the proxy providers and routes loaded from the DB.
    /// `build_provider` in `providers::mod.rs` must have already been called.
    pub fn new(
        providers: HashMap<String, Arc<dyn Provider>>,
        routes: HashMap<String, Vec<RouteHop>>,
    ) -> Self {
        let routes: HashMap<String, Arc<Vec<RouteHop>>> = routes
            .into_iter()
            .map(|(alias, hops)| (alias, Arc::new(hops)))
            .collect();
        Self {
            providers,
            routes,
            queue: Vec::new(),
            queues: HashMap::new(),
            last_used: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            // Production default: 10 s. A working-but-slow upstream with
            // TTFB > 1.5 s (observed with GLM) must not be killed. This cap
            // only catches 200-then-silence stalls; with providers now
            // propagating HTTP errors eagerly, dead hops already fail over in
            // one RTT without waiting for the timeout.
            first_byte_timeout_ms: 10_000,
        }
    }

    /// Attach the homepage failover queue (checked after explicit routes
    /// and per-provider auto-routing miss).
    pub fn with_queue(mut self, queue: Vec<String>) -> Self {
        self.queue = queue;
        self
    }

    /// Attach the per-surface failover queues. Keys are surface ids
    /// (`SURFACE_CLAUDE` / `SURFACE_CODEX` / `SURFACE_CHAT`); values are
    /// ordered provider ids (chain head first).
    pub fn with_queues(mut self, queues: HashMap<String, Vec<String>>) -> Self {
        self.queues = queues;
        self
    }

    /// Resolve a model name to its route hops. If no explicit route exists,
    /// Resolve a model alias to a failover chain: explicit route first, then
    /// check if any enabled provider directly serves the model (auto-route).
    ///
    /// Hop order is the configured order (route editor / queue sort_index) —
    /// deliberately NOT reordered by recent success. cc-switch parity: its
    /// failover queue is static and the "sticky switch" on failover success
    /// only updates the current-provider marker, never the queue order. The
    /// fast path comes from the circuit breakers skipping dead hops at zero
    /// cost, not from reordering.
    pub fn resolve_route(&self, model: &str) -> Option<Arc<Vec<RouteHop>>> {
        if let Some(hops) = self.routes.get(model) {
            return Some(Arc::clone(hops));
        }
        let matches = self.matching_model_hops(model, &self.queue);
        if !matches.is_empty() {
            return Some(Arc::new(matches));
        }
        // Homepage failover queue: last-chance chain, model passed through.
        if !self.queue.is_empty() {
            return Some(Arc::new(
                self.queue
                    .iter()
                    .map(|pid| RouteHop {
                        provider: pid.clone(),
                        model: model.into(),
                    })
                    .collect(),
            ));
        }
        None
    }

    /// Surface-aware resolution: same precedence as `resolve_route`, but the
    /// last-chance chain is the surface's per-app queue instead of the global
    /// one. `/v1/messages` falls back to the Claude chain, `/v1/responses`
    /// to the Codex chain, `/v1/chat/completions` to the copilot+pi chain.
    pub fn resolve_route_for(&self, surface: &str, model: &str) -> Option<Arc<Vec<RouteHop>>> {
        if let Some(hops) = self.routes.get(model) {
            return Some(Arc::clone(hops));
        }
        let priority = self
            .queues
            .get(surface)
            .filter(|queue| !queue.is_empty())
            .unwrap_or(&self.queue);
        let matches = self.matching_model_hops(model, priority);
        if !matches.is_empty() {
            return Some(Arc::new(matches));
        }
        if let Some(chain) = self.queues.get(surface) {
            if !chain.is_empty() {
                return Some(Arc::new(
                    chain
                        .iter()
                        .map(|pid| RouteHop {
                            provider: pid.clone(),
                            model: model.into(),
                        })
                        .collect(),
                ));
            }
        }
        // Legacy global queue as the very last resort.
        if !self.queue.is_empty() {
            return Some(Arc::new(
                self.queue
                    .iter()
                    .map(|pid| RouteHop {
                        provider: pid.clone(),
                        model: model.into(),
                    })
                    .collect(),
            ));
        }
        None
    }

    /// Automatic same-model routes use configured queue priority, then stable
    /// provider id. Explicit routes/pins remain authoritative and no model
    /// mapping is inferred from a provider's position in another queue.
    fn matching_model_hops(&self, model: &str, priority: &[String]) -> Vec<RouteHop> {
        let mut ids: Vec<&String> = self
            .providers
            .iter()
            .filter(|(_, provider)| {
                provider.enabled() && provider.models().iter().any(|candidate| candidate == model)
            })
            .map(|(id, _)| id)
            .collect();
        ids.sort_by_key(|id| {
            (
                priority
                    .iter()
                    .position(|candidate| candidate == *id)
                    .unwrap_or(usize::MAX),
                id.as_str(),
            )
        });
        ids.into_iter()
            .map(|id| RouteHop {
                provider: id.clone(),
                model: model.into(),
            })
            .collect()
    }

    /// Non-streaming completion with automatic failover. For each hop:
    /// 1. Check circuit breaker — skip if open.
    /// 2. Call provider.complete.
    /// 3. On success: record_success, return.
    /// 4. On retriable error: record_failure, try next hop.
    /// 5. On non-retriable error: propagate immediately.
    pub async fn complete(
        &self,
        req: &ChatCompletionRequest,
    ) -> Result<OAIChatResponse, ProviderError> {
        let hops = self.resolve_route(&req.model).ok_or_else(|| {
            canonical::make_provider_error(
                "no_route",
                "route_resolution",
                Some(404),
                Some(false),
                None,
            )
        })?;
        // Legacy complete(): no app_type known; use empty key so last_used
        // writes still succeed (only used by tests; persist won't read it).
        self.complete_hops("", "_legacy", req, hops).await
    }

    /// Surface-aware non-streaming completion: the last-resort chain is the
    /// entry endpoint's per-app queue (`/v1/messages` -> Claude chain,
    /// `/v1/responses` -> Codex chain, `/v1/chat/completions` -> chat chain).
    pub async fn complete_for(
        &self,
        surface: &str,
        app_type: &str,
        req: &ChatCompletionRequest,
    ) -> Result<OAIChatResponse, ProviderError> {
        let hops = self.resolve_route_for(surface, &req.model).ok_or_else(|| {
            canonical::make_provider_error(
                "no_route",
                "route_resolution",
                Some(404),
                Some(false),
                None,
            )
        })?;
        self.complete_hops(surface, app_type, req, hops).await
    }

    /// Force all requests through a specific provider (pinned/forced mode).
    /// Bypasses the normal failover chain. The request's model passes through
    /// unchanged. Used when the user toggles the force-pin ON for a provider.
    pub async fn complete_with_provider(
        &self,
        surface: &str,
        app_type: &str,
        provider_id: &str,
        req: &ChatCompletionRequest,
    ) -> Result<OAIChatResponse, ProviderError> {
        let hops = Arc::new(vec![RouteHop {
            provider: provider_id.to_string(),
            model: req.model.clone(),
        }]);
        // `surface` is unused on the pinned path (single hop) but the
        // unified `complete_hops` signature requires it; pass the caller's.
        self.complete_hops(surface, app_type, req, hops).await
    }

    /// Force-stream variant of `complete_with_provider`.
    pub async fn stream_with_provider(
        &self,
        surface: &str,
        app_type: &str,
        provider_id: &str,
        req: &ChatCompletionRequest,
    ) -> Result<futures_util::stream::BoxStream<'static, OAIStreamChunk>, ProviderError> {
        let hops = Arc::new(vec![RouteHop {
            provider: provider_id.to_string(),
            model: req.model.clone(),
        }]);
        // See complete_with_provider — surface is unused on the pinned
        // path but the unified signature requires it.
        self.stream_hops(surface, app_type, req, hops).await
    }

    pub async fn complete_hops(
        &self,
        _surface: &str,
        app_type: &str,
        req: &ChatCompletionRequest,
        hops: Arc<Vec<RouteHop>>,
    ) -> Result<OAIChatResponse, ProviderError> {
        let mut last_err: Option<ProviderError> = None;
        for hop in hops.iter() {
            let provider = match self.providers.get(&hop.provider) {
                Some(p) => p,
                None => {
                    log::warn!(
                        "router: skip provider_ref={} reason=unknown",
                        crate::diagnostics::safe_reference(&hop.provider)
                    );
                    continue;
                }
            };
            if !provider.enabled() {
                log::warn!(
                    "router: skip provider_ref={} reason=disabled",
                    crate::diagnostics::safe_reference(&hop.provider)
                );
                continue;
            }
            // A passthrough hop without a client credential can never
            // authenticate upstream (only /v1/messages attaches one), so
            // skip it before the breaker/acquire round-trip and let the
            // static-key hops behind it serve the request.
            if provider.requires_client_credential() && req.client_credential.is_none() {
                log::warn!(
                    "router: skip provider_ref={} reason=no_client_credential",
                    crate::diagnostics::safe_reference(&hop.provider)
                );
                continue;
            }
            let Some(mut attempt) = Provider_attempt::acquire(Arc::clone(provider)) else {
                log::warn!(
                    "router: skip provider_ref={} reason=circuit_open",
                    crate::diagnostics::safe_reference(&hop.provider)
                );
                continue;
            };
            // Zero-copy hop dispatch: the hop's upstream model rides as a
            // parameter, so the shared request payload is never cloned here
            // (Codex CLI conversations reach hundreds of KB — a per-hop
            // clone was pure overhead).
            let attempt_started = Instant::now();
            match provider.complete(req, &hop.model).await {
                Ok(resp) => {
                    attempt.settle(Circuit_outcome::Success);
                    if let Ok(mut lu) = self.last_used.lock() {
                        lu.insert(app_type.to_string(), provider.id().to_string());
                    }
                    log::info!(
                        "router: complete request_id={} model_ref={} provider_ref={} upstream_model_ref={} elapsed_ms={}",
                        req.request_id.as_deref().unwrap_or("unknown"),
                        crate::diagnostics::safe_reference(&req.model),
                        crate::diagnostics::safe_reference(provider.id()),
                        crate::diagnostics::safe_reference(&hop.model),
                        attempt_started.elapsed().as_millis()
                    );
                    return Ok(resp);
                }
                Err(e) => {
                    let retriable =
                        e.retriable || should_failover_tool_capability_rejection(req, &e);
                    // Only retriable (provider-level) failures count against
                    // the breaker - non-retriable errors are local
                    // construction problems, not the provider's fault.
                    if retriable {
                        attempt.settle(Circuit_outcome::Failure);
                    }
                    log::warn!(
                        "router: failed request_id={} provider_ref={} upstream_model_ref={} elapsed_ms={} status={} retriable={} code={} stage={} response_bytes={}",
                        req.request_id.as_deref().unwrap_or("unknown"),
                        crate::diagnostics::safe_reference(provider.id()),
                        crate::diagnostics::safe_reference(&hop.model),
                        attempt_started.elapsed().as_millis(),
                        e.status.unwrap_or(500),
                        retriable,
                        e.code,
                        e.stage,
                        e.response_bytes.unwrap_or(0)
                    );
                    last_err = Some(e);
                    if !retriable {
                        return Err(last_err.unwrap());
                    }
                }
            }
        }
        Err(last_err.unwrap_or_else(|| {
            canonical::make_provider_error(
                "all_hops_exhausted",
                "routing",
                Some(503),
                Some(false),
                None,
            )
        }))
    }

    /// Health snapshot of all providers (for /health endpoint).
    pub fn health(&self) -> Vec<ProviderHealth> {
        self.providers
            .values()
            .map(|p| ProviderHealth {
                id: p.id().to_string(),
                enabled: p.enabled(),
                breaker: p.breaker().snapshot(),
            })
            .collect()
    }

    /// All model aliases (for /v1/models endpoint).
    pub fn list_models(&self) -> Vec<String> {
        self.routes.keys().cloned().collect()
    }
    /// Streaming completion with automatic failover (only pre-first-byte).
    /// Mirrors `Router.stream` in `src/gateway/router.ts:85`.
    ///
    /// For each hop: check breaker, call provider.stream(). Probe the first chunk
    /// before committing - allows failover if the upstream errors before sending
    /// data. Once we yield the first chunk, mid-stream errors surface to the client.
    pub async fn stream(
        &self,
        req: &ChatCompletionRequest,
    ) -> Result<futures_util::stream::BoxStream<'static, OAIStreamChunk>, ProviderError> {
        let hops = self.resolve_route(&req.model).ok_or_else(|| {
            canonical::make_provider_error(
                "no_route",
                "route_resolution",
                Some(404),
                Some(false),
                None,
            )
        })?;
        self.stream_hops("_legacy", "_legacy", req, hops).await
    }

    /// Surface-aware streaming completion (see `complete_for`).
    pub async fn stream_for(
        &self,
        surface: &str,
        app_type: &str,
        req: &ChatCompletionRequest,
    ) -> Result<futures_util::stream::BoxStream<'static, OAIStreamChunk>, ProviderError> {
        let hops = self.resolve_route_for(surface, &req.model).ok_or_else(|| {
            canonical::make_provider_error(
                "no_route",
                "route_resolution",
                Some(404),
                Some(false),
                None,
            )
        })?;
        self.stream_hops(surface, app_type, req, hops).await
    }

    pub async fn stream_hops(
        &self,
        _surface: &str,
        app_type: &str,
        req: &ChatCompletionRequest,
        hops: Arc<Vec<RouteHop>>,
    ) -> Result<futures_util::stream::BoxStream<'static, OAIStreamChunk>, ProviderError> {
        use futures_util::StreamExt;

        let mut last_err: Option<ProviderError> = None;
        for hop in hops.iter() {
            let provider = match self.providers.get(&hop.provider) {
                Some(p) => p,
                None => continue,
            };
            if !provider.enabled() {
                log::warn!(
                    "router: skip provider_ref={} reason=disabled stream=true",
                    crate::diagnostics::safe_reference(&hop.provider)
                );
                continue;
            }
            // Passthrough hop without a client credential: skip (see the
            // matching check in `complete_hops`).
            if provider.requires_client_credential() && req.client_credential.is_none() {
                log::warn!(
                    "router: skip provider_ref={} reason=no_client_credential stream=true",
                    crate::diagnostics::safe_reference(&hop.provider)
                );
                continue;
            }
            let Some(mut attempt) = Provider_attempt::acquire(Arc::clone(provider)) else {
                log::warn!(
                    "router: skip provider_ref={} reason=circuit_open stream=true",
                    crate::diagnostics::safe_reference(&hop.provider)
                );
                continue;
            };
            let attempt_started = Instant::now();
            let mut chunk_stream = match provider.stream(req, &hop.model).await {
                Ok(s) => s,
                Err(e) => {
                    let retriable =
                        e.retriable || should_failover_tool_capability_rejection(req, &e);
                    // Only retriable (provider-level) failures count against the
                    // breaker - non-retriable errors are local construction
                    // problems (serialize / client build), not the provider's
                    // fault, and must not pollute its health.
                    if retriable {
                        attempt.settle(Circuit_outcome::Failure);
                    }
                    log::warn!(
                    "router: stream setup failed request_id={} provider_ref={} upstream_model_ref={} elapsed_ms={} status={} retriable={} code={} stage={} response_bytes={}",
                    req.request_id.as_deref().unwrap_or("unknown"),
                    crate::diagnostics::safe_reference(provider.id()),
                    crate::diagnostics::safe_reference(&hop.model),
                    attempt_started.elapsed().as_millis(),
                    e.status.unwrap_or(500),
                    retriable,
                    e.code,
                    e.stage,
                    e.response_bytes.unwrap_or(0)
                );
                    last_err = Some(e);
                    if !retriable {
                        return Err(last_err.unwrap());
                    }
                    continue;
                }
            };
            // Probe first chunk. Bound the wait with FIRST_BYTE_TIMEOUT_MS so a
            // dead upstream (opened a connection but never sent bytes, or took
            // longer than this budget to send its first event) can't burn a full
            // upstream-RTT before the router fails over. Without this cap, every
            // hop in the chain adds ~1 s of wall-clock latency — a 5-hop walk
            // turned a fast reply into a 5 s pre-stream wait. The cap also keeps
            // circuit-breaker failures accumulating fast enough that the bad
            // hops open after a few rounds and get skipped on subsequent
            // requests.
            let first = match tokio::time::timeout(
                std::time::Duration::from_millis(self.first_byte_timeout_ms),
                chunk_stream.next(),
            )
            .await
            {
                Ok(Some(c)) => c,
                Ok(None) => {
                    // An upstream that opened the stream but yielded zero chunks
                    // (e.g. the OpenAI/Anthropic provider swallowing an HTTP 4xx/5xx
                    // as an empty SSE body) should NOT be treated as a successful
                    // empty completion — that would surface a 200 with no data to
                    // the client. Record a failure and fail over to the next hop.
                    // Mock providers never hit this path (they always yield a role
                    // chunk + a finish chunk), so the seeded demo routes are
                    // unaffected.
                    attempt.settle(Circuit_outcome::Failure);
                    log::warn!(
                    "router: stream empty request_id={} model_ref={} provider_ref={} upstream_model_ref={} elapsed_ms={}",
                    req.request_id.as_deref().unwrap_or("unknown"),
                    crate::diagnostics::safe_reference(&req.model),
                    crate::diagnostics::safe_reference(provider.id()),
                    crate::diagnostics::safe_reference(&hop.model),
                    attempt_started.elapsed().as_millis()
                );
                    last_err = Some(canonical::make_provider_error(
                        "upstream_stream_error",
                        "stream_empty",
                        Some(502),
                        Some(true),
                        None,
                    ));
                    continue;
                }
                Err(_elapsed) => {
                    // Upstream stalled past the first-byte budget. Treat as a
                    // retriable failure (counts against the breaker) so the next
                    // hop gets a chance and the breaker opens quickly when a
                    // provider is consistently slow-to-first-byte.
                    attempt.settle(Circuit_outcome::Failure);
                    log::warn!(
                    "router: stream first event timeout request_id={} budget_ms={} model_ref={} provider_ref={} upstream_model_ref={} elapsed_ms={}",
                    req.request_id.as_deref().unwrap_or("unknown"),
                    self.first_byte_timeout_ms,
                    crate::diagnostics::safe_reference(&req.model),
                    crate::diagnostics::safe_reference(provider.id()),
                    crate::diagnostics::safe_reference(&hop.model),
                    attempt_started.elapsed().as_millis()
                );
                    last_err = Some(canonical::make_provider_error(
                        "upstream_timeout",
                        "stream_first_event",
                        Some(504),
                        Some(true),
                        None,
                    ));
                    continue;
                }
            };
            if let Some(error) = canonical::stream_chunk_error(&first) {
                if error.retriable {
                    attempt.settle(Circuit_outcome::Failure);
                    last_err = Some(error);
                    continue;
                }
                return Err(error);
            }
            log::info!(
            "router: stream first event request_id={} model_ref={} provider_ref={} upstream_model_ref={} elapsed_ms={}",
            req.request_id.as_deref().unwrap_or("unknown"),
            crate::diagnostics::safe_reference(&req.model),
            crate::diagnostics::safe_reference(provider.id()),
            crate::diagnostics::safe_reference(&hop.model),
            attempt_started.elapsed().as_millis()
        );
            // Chain the first chunk with the rest of the stream. Mid-stream errors
            // surface to the client (no transparent failover).
            let last_used = Arc::clone(&self.last_used);
            let app_type = app_type.to_string();
            let upstream_model = hop.model.clone();
            let combined = async_stream::stream! {
                let mut terminal_seen = false;
                let mut pending = Some(first);
                loop {
                    let chunk = if let Some(first) = pending.take() {
                        Some(first)
                    } else {
                        chunk_stream.next().await
                    };
                    let Some(chunk) = chunk else { break; };
                    if let Some(error) = canonical::stream_chunk_error(&chunk) {
                        attempt.settle(if error.retriable { Circuit_outcome::Failure } else { Circuit_outcome::Cancelled });
                        yield chunk;
                        return;
                    }
                    terminal_seen |= chunk.choices.iter().any(|choice| choice.finish_reason.is_some())
                        || chunk.source_anthropic_event.as_ref().and_then(|event| event.get("type")).and_then(serde_json::Value::as_str) == Some("message_stop");
                    yield chunk;
                }
                if !terminal_seen {
                    attempt.settle(Circuit_outcome::Failure);
                    yield canonical::stream_error_chunk(&upstream_model, 502, "proxy_stream_error");
                    return;
                }
                attempt.settle(Circuit_outcome::Success);
                if let Ok(mut winners) = last_used.lock() {
                    winners.insert(app_type, attempt.provider.id().to_string());
                }
            };
            return Ok(combined.boxed());
        }
        Err(last_err.unwrap_or_else(|| {
            canonical::make_provider_error(
                "all_hops_exhausted",
                "stream_routing",
                Some(503),
                Some(false),
                None,
            )
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::dao::proxy_providers::CircuitBreakerConfig;
    use crate::proxy::circuit_breaker::CircuitBreaker;
    use crate::proxy::providers::Provider;
    use async_trait::async_trait;
    use futures_core::Stream;
    use futures_util::StreamExt;
    use std::pin::Pin;

    fn empty_cb() -> CircuitBreakerConfig {
        CircuitBreakerConfig {
            failure_threshold: 3,
            cooldown_ms: 15_000,
            half_open_retries: 1,
            success_threshold: 2,
        }
    }

    /// A provider that yields a stream but with zero chunks on `stream()` —
    /// simulating an upstream that swallowed an HTTP error into an empty SSE
    /// body (see the OpenAI/Anthropic streaming providers).
    struct EmptyStreamProvider {
        id: &'static str,
        breaker: CircuitBreaker,
    }

    impl EmptyStreamProvider {
        fn new(id: &'static str) -> Self {
            Self {
                id,
                breaker: CircuitBreaker::new(empty_cb()),
            }
        }
    }

    #[async_trait]
    impl Provider for EmptyStreamProvider {
        fn id(&self) -> &str {
            self.id
        }
        fn enabled(&self) -> bool {
            true
        }
        fn models(&self) -> &[String] {
            &[]
        }
        fn breaker(&self) -> &CircuitBreaker {
            &self.breaker
        }

        async fn complete(
            &self,
            _req: &ChatCompletionRequest,
            _upstream_model: &str,
        ) -> Result<OAIChatResponse, ProviderError> {
            Err(canonical::make_error("unused", Some(500), Some(true)))
        }

        async fn stream(
            &self,
            _req: &ChatCompletionRequest,
            _upstream_model: &str,
        ) -> Result<Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>>, ProviderError> {
            Ok(futures_util::stream::empty().boxed())
        }
    }

    /// A provider whose `stream()` fails before yielding anything (like mock-fail).
    struct ErrStreamProvider {
        id: &'static str,
        breaker: CircuitBreaker,
    }

    impl ErrStreamProvider {
        fn new(id: &'static str) -> Self {
            Self {
                id,
                breaker: CircuitBreaker::new(empty_cb()),
            }
        }
    }

    #[async_trait]
    impl Provider for ErrStreamProvider {
        fn id(&self) -> &str {
            self.id
        }
        fn enabled(&self) -> bool {
            true
        }
        fn models(&self) -> &[String] {
            &[]
        }
        fn breaker(&self) -> &CircuitBreaker {
            &self.breaker
        }

        async fn complete(
            &self,
            _req: &ChatCompletionRequest,
            _upstream_model: &str,
        ) -> Result<OAIChatResponse, ProviderError> {
            Err(canonical::make_error("unused", Some(500), Some(true)))
        }

        async fn stream(
            &self,
            _req: &ChatCompletionRequest,
            _upstream_model: &str,
        ) -> Result<Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>>, ProviderError> {
            Err(canonical::make_error(
                "forced failure",
                Some(503),
                Some(true),
            ))
        }
    }

    /// A real streaming provider that yields one role chunk then a finish chunk.
    struct WorkingProvider {
        id: &'static str,
        breaker: CircuitBreaker,
    }

    impl WorkingProvider {
        fn new(id: &'static str) -> Self {
            Self {
                id,
                breaker: CircuitBreaker::new(empty_cb()),
            }
        }
    }

    #[async_trait]
    impl Provider for WorkingProvider {
        fn id(&self) -> &str {
            self.id
        }
        fn enabled(&self) -> bool {
            true
        }
        fn models(&self) -> &[String] {
            &[]
        }
        fn breaker(&self) -> &CircuitBreaker {
            &self.breaker
        }

        async fn complete(
            &self,
            _req: &ChatCompletionRequest,
            upstream_model: &str,
        ) -> Result<OAIChatResponse, ProviderError> {
            Ok(OAIChatResponse {
                id: "working".into(),
                object: "chat.completion".into(),
                created: 0,
                model: upstream_model.into(),
                choices: vec![crate::proxy::canonical::OAIChoice {
                    index: 0,
                    message: crate::proxy::canonical::OAIMessage {
                        role: "assistant".into(),
                        content: Some("working".into()),
                        tool_calls: None,
                        refusal: None,
                        extra: Default::default(),
                    },
                    finish_reason: Some("stop".into()),
                    extra: Default::default(),
                }],
                usage: None,
                extra: Default::default(),
                source_anthropic_response: None,
            })
        }

        async fn stream(
            &self,
            _req: &ChatCompletionRequest,
            upstream_model: &str,
        ) -> Result<Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>>, ProviderError> {
            let model = upstream_model.to_string();
            let stream = futures_util::stream::iter(vec![
                crate::proxy::converter::make_stream_chunk(
                    "id",
                    &model,
                    crate::proxy::canonical::StreamDelta {
                        role: Some("assistant".into()),
                        content: None,
                        tool_calls: None,
                        reasoning: None,
                        extra: Default::default(),
                    },
                    None,
                ),
                crate::proxy::converter::make_stream_chunk(
                    "id",
                    &model,
                    crate::proxy::canonical::StreamDelta {
                        role: None,
                        content: None,
                        tool_calls: None,
                        reasoning: None,
                        extra: Default::default(),
                    },
                    Some("stop".into()),
                ),
            ]);
            Ok(Box::pin(stream))
        }
    }

    /// A passthrough-shaped provider for hop-skip coverage: advertises
    /// `requires_client_credential`, records invocations, and serves a
    /// marker response so the caller can tell which hop answered.
    struct PassthroughProbeProvider {
        breaker: CircuitBreaker,
        called: std::sync::atomic::AtomicBool,
    }

    impl PassthroughProbeProvider {
        fn shared() -> (Arc<Self>, Arc<Self>) {
            let probe = Arc::new(Self {
                breaker: CircuitBreaker::new(empty_cb()),
                called: std::sync::atomic::AtomicBool::new(false),
            });
            (probe.clone(), probe)
        }
    }

    #[async_trait]
    impl Provider for PassthroughProbeProvider {
        fn id(&self) -> &str {
            "sub"
        }
        fn enabled(&self) -> bool {
            true
        }
        fn models(&self) -> &[String] {
            &[]
        }
        fn breaker(&self) -> &CircuitBreaker {
            &self.breaker
        }
        fn requires_client_credential(&self) -> bool {
            true
        }

        async fn complete(
            &self,
            _req: &ChatCompletionRequest,
            upstream_model: &str,
        ) -> Result<OAIChatResponse, ProviderError> {
            self.called.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(OAIChatResponse {
                id: "passthrough".into(),
                object: "chat.completion".into(),
                created: 0,
                model: upstream_model.into(),
                choices: vec![crate::proxy::canonical::OAIChoice {
                    index: 0,
                    message: crate::proxy::canonical::OAIMessage {
                        role: "assistant".into(),
                        content: Some("passthrough_served".into()),
                        tool_calls: None,
                        refusal: None,
                        extra: Default::default(),
                    },
                    finish_reason: Some("stop".into()),
                    extra: Default::default(),
                }],
                usage: None,
                extra: Default::default(),
                source_anthropic_response: None,
            })
        }

        async fn stream(
            &self,
            _req: &ChatCompletionRequest,
            _upstream_model: &str,
        ) -> Result<Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>>, ProviderError> {
            self.called.store(true, std::sync::atomic::Ordering::SeqCst);
            let stream =
                futures_util::stream::iter(vec![crate::proxy::converter::make_stream_chunk(
                    "id",
                    "m",
                    crate::proxy::canonical::StreamDelta {
                        role: Some("assistant".into()),
                        content: None,
                        tool_calls: None,
                        reasoning: None,
                        extra: Default::default(),
                    },
                    Some("stop".into()),
                )]);
            Ok(Box::pin(stream))
        }
    }

    /// A passthrough hop must be skipped when the request carries no client
    /// credential (only /v1/messages attaches one): chat/completions and
    /// responses surfaces would otherwise burn an upstream 401 and terminate
    /// the route before the static-key hops behind it can serve.
    #[tokio::test]
    async fn passthrough_hop_skipped_without_client_credential() {
        let (handle, probe) = PassthroughProbeProvider::shared();
        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        providers.insert("sub".into(), handle);
        providers.insert("pool".into(), Arc::new(WorkingProvider::new("pool")));
        let hops = Arc::new(vec![
            RouteHop {
                provider: "sub".into(),
                model: "m".into(),
            },
            RouteHop {
                provider: "pool".into(),
                model: "m".into(),
            },
        ]);
        let router = Router::new(providers, HashMap::new());
        let response = router
            .complete_hops("", "_test", &req_for("m"), hops)
            .await
            .expect("static-key hop must still serve the request");
        assert_eq!(
            response.choices[0].message.content.as_deref(),
            Some("working"),
            "the static-key hop behind the skipped passthrough hop must answer"
        );
        assert!(
            !probe.called.load(std::sync::atomic::Ordering::SeqCst),
            "a passthrough hop must not be dispatched without a client credential"
        );
    }

    /// With a client credential present the passthrough hop is dispatched
    /// normally — the skip must not starve /v1/messages subscription calls.
    #[tokio::test]
    async fn passthrough_hop_dispatched_with_client_credential() {
        let (handle, probe) = PassthroughProbeProvider::shared();
        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        providers.insert("sub".into(), handle);
        providers.insert("pool".into(), Arc::new(WorkingProvider::new("pool")));
        let hops = Arc::new(vec![RouteHop {
            provider: "sub".into(),
            model: "m".into(),
        }]);
        let router = Router::new(providers, HashMap::new());
        let mut request = req_for("m");
        request.client_credential = Some(canonical::Client_credential {
            authorization: Some("Bearer oauth-relay-7".into()),
            x_api_key: None,
        });
        let response = router
            .complete_hops("", "_test", &request, hops)
            .await
            .expect("the passthrough hop must serve the credentialed request");
        assert_eq!(
            response.choices[0].message.content.as_deref(),
            Some("passthrough_served")
        );
        assert!(probe.called.load(std::sync::atomic::Ordering::SeqCst));
    }

    /// Same guard on the streaming path: no credential → skip the hop, the
    /// stream comes from the static-key hop behind it.
    #[tokio::test]
    async fn passthrough_stream_hop_skipped_without_client_credential() {
        let (handle, probe) = PassthroughProbeProvider::shared();
        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        providers.insert("sub".into(), handle);
        providers.insert("pool".into(), Arc::new(WorkingProvider::new("pool")));
        let hops = Arc::new(vec![
            RouteHop {
                provider: "sub".into(),
                model: "m".into(),
            },
            RouteHop {
                provider: "pool".into(),
                model: "m".into(),
            },
        ]);
        let router = Router::new(providers, HashMap::new());
        let stream = router
            .stream_hops("", "_test", &req_for("m"), hops)
            .await
            .expect("static-key hop must still stream");
        let chunks: Vec<OAIStreamChunk> = stream.collect().await;
        assert!(!chunks.is_empty());
        assert!(
            !probe.called.load(std::sync::atomic::Ordering::SeqCst),
            "a passthrough stream hop must not be dispatched without a client credential"
        );
    }

    fn req_for(model: &str) -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: model.into(),
            messages: vec![],
            stream: true,
            temperature: None,
            max_tokens: None,
            tools: None,
            tool_choice: None,
            extra: Default::default(),
            source_anthropic_request: None,
            request_id: None,
            client_credential: None,
        }
    }

    fn req_with_tool(model: &str) -> ChatCompletionRequest {
        let mut request = req_for(model);
        request.tools = Some(vec![serde_json::json!({
            "type": "function",
            "function": {"name": "probe", "parameters": {"type": "object"}},
        })]);
        request
    }

    struct ToolRejectProvider {
        id: &'static str,
        breaker: CircuitBreaker,
        /// Optional upstream-4xx classification. The router's helper
        /// gates failover on `kind == ToolCapability`; auth-expired and
        /// unclassified 400s must remain terminal.
        kind: Option<canonical::UpstreamErrorKind>,
    }

    impl ToolRejectProvider {
        fn new(id: &'static str) -> Self {
            Self {
                id,
                breaker: CircuitBreaker::new(empty_cb()),
                kind: None,
            }
        }
        fn with_kind(id: &'static str, kind: canonical::UpstreamErrorKind) -> Self {
            Self {
                id,
                breaker: CircuitBreaker::new(empty_cb()),
                kind: Some(kind),
            }
        }
    }

    #[async_trait]
    impl Provider for ToolRejectProvider {
        fn id(&self) -> &str {
            self.id
        }
        fn enabled(&self) -> bool {
            true
        }
        fn models(&self) -> &[String] {
            &[]
        }
        fn breaker(&self) -> &CircuitBreaker {
            &self.breaker
        }

        async fn complete(
            &self,
            _req: &ChatCompletionRequest,
            _upstream_model: &str,
        ) -> Result<OAIChatResponse, ProviderError> {
            Err(canonical::make_provider_error_with_kind(
                "upstream_http_error",
                "response_status",
                Some(400),
                Some(false),
                Some(84),
                self.kind,
            ))
        }

        async fn stream(
            &self,
            _req: &ChatCompletionRequest,
            _upstream_model: &str,
        ) -> Result<Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>>, ProviderError> {
            Err(canonical::make_provider_error_with_kind(
                "upstream_http_error",
                "response_status",
                Some(400),
                Some(false),
                Some(84),
                self.kind,
            ))
        }
    }

    /// A provider whose `stream()` returns a stream that **never** yields a
    /// chunk — simulating an upstream that opened the connection but stalled
    /// before its first byte. Used to verify the router's first-byte timeout
    /// (FIRST_BYTE_TIMEOUT_MS) fires and the router advances to the next hop
    /// instead of hanging indefinitely.
    struct StallProvider {
        id: &'static str,
        breaker: CircuitBreaker,
    }

    impl StallProvider {
        fn new(id: &'static str) -> Self {
            Self {
                id,
                breaker: CircuitBreaker::new(empty_cb()),
            }
        }
    }

    #[async_trait]
    impl Provider for StallProvider {
        fn id(&self) -> &str {
            self.id
        }
        fn enabled(&self) -> bool {
            true
        }
        fn models(&self) -> &[String] {
            &[]
        }
        fn breaker(&self) -> &CircuitBreaker {
            &self.breaker
        }

        async fn complete(
            &self,
            _req: &ChatCompletionRequest,
            _upstream_model: &str,
        ) -> Result<OAIChatResponse, ProviderError> {
            Err(canonical::make_error("unused", Some(500), Some(true)))
        }

        async fn stream(
            &self,
            _req: &ChatCompletionRequest,
            _upstream_model: &str,
        ) -> Result<Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>>, ProviderError> {
            // A stream that polls Pending forever — exercises the
            // first-byte-timeout race in Router::stream_hops.
            let s = futures_util::stream::pending::<OAIStreamChunk>();
            Ok(Box::pin(s))
        }
    }

    /// First-byte timeout (FIRST_BYTE_TIMEOUT_MS) must fire on a stalled
    /// upstream and the router must fail over to the next hop. Without the
    /// race, this test would hang for the full upstream timeout (60 s) or
    /// the test's own timeout. Bound the assertion with a 3 s test deadline
    /// so a regression is caught immediately.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stalled_stream_fails_over_via_first_byte_timeout() {
        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        providers.insert("stall".into(), Arc::new(StallProvider::new("stall")));
        providers.insert("working".into(), Arc::new(WorkingProvider::new("working")));
        let mut routes: HashMap<String, Vec<RouteHop>> = HashMap::new();
        routes.insert(
            "m".into(),
            vec![
                RouteHop {
                    provider: "stall".into(),
                    model: "m".into(),
                },
                RouteHop {
                    provider: "working".into(),
                    model: "m".into(),
                },
            ],
        );
        let mut router = Router::new(providers, routes);
        // Use a short first-byte timeout so the stall test finishes fast.
        router.first_byte_timeout_ms = 1_500;

        // FIRST_BYTE_TIMEOUT_MS is 1500 ms; the test must complete well
        // under 3 s — otherwise the cap regressed.
        let stream = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            router.stream(&req_for("m")),
        )
        .await
        .expect("stream_hops should not exceed 3 s for a stalled hop")
        .expect("stream should succeed via fallback");

        use futures_util::StreamExt;
        let chunks: Vec<OAIStreamChunk> = stream.collect().await;
        assert!(
            !chunks.is_empty(),
            "should fail over to the working provider"
        );
        assert_eq!(
            chunks[0].choices[0].delta.role.as_deref(),
            Some("assistant")
        );
        assert_eq!(
            chunks.last().unwrap().choices[0].finish_reason.as_deref(),
            Some("stop")
        );
        // The stalled hop's breaker should have recorded exactly one failure.
        let breaker = router
            .health()
            .into_iter()
            .find(|h| h.id == "stall")
            .expect("stall provider present in health");
        assert_eq!(
            breaker.breaker.failures, 1,
            "stalled hop must record exactly one breaker failure"
        );
    }

    /// When every hop stalls, the router must return an error within
    /// FIRST_BYTE_TIMEOUT_MS × hops — not the upstream's 60 s request timeout.
    /// This is the case where the breaker hasn't opened yet but every
    /// provider is consistently slow-to-first-byte.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn all_stalled_hops_return_quickly() {
        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        providers.insert("s1".into(), Arc::new(StallProvider::new("s1")));
        providers.insert("s2".into(), Arc::new(StallProvider::new("s2")));
        providers.insert("s3".into(), Arc::new(StallProvider::new("s3")));
        let mut routes: HashMap<String, Vec<RouteHop>> = HashMap::new();
        routes.insert(
            "m".into(),
            vec![
                RouteHop {
                    provider: "s1".into(),
                    model: "m".into(),
                },
                RouteHop {
                    provider: "s2".into(),
                    model: "m".into(),
                },
                RouteHop {
                    provider: "s3".into(),
                    model: "m".into(),
                },
            ],
        );
        let mut router = Router::new(providers, routes);
        // Use a short first-byte timeout so the stall test finishes fast.
        router.first_byte_timeout_ms = 1_500;

        // 3 hops × 1500 ms = 4500 ms — plus headroom. Without the cap this
        // would hang for 60 s per hop.
        let res = tokio::time::timeout(
            std::time::Duration::from_secs(6),
            router.stream(&req_for("m")),
        )
        .await
        .expect("all-stalled walk should complete within 6 s (3 × 1.5 s budget)");
        assert!(res.is_err(), "all hops stalled -> should error");
        let err = res.err().unwrap();
        assert!(err.retriable, "all-stalled error is retriable");
        // Every hop's breaker should have one failure.
        for h in router.health() {
            assert_eq!(h.breaker.failures, 1, "{} should record one failure", h.id);
        }
    }

    #[tokio::test]
    async fn empty_stream_fails_over_to_next_hop() {
        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        providers.insert("empty".into(), Arc::new(EmptyStreamProvider::new("empty")));
        providers.insert("working".into(), Arc::new(WorkingProvider::new("working")));
        let mut routes: HashMap<String, Vec<RouteHop>> = HashMap::new();
        routes.insert(
            "m".into(),
            vec![
                RouteHop {
                    provider: "empty".into(),
                    model: "m".into(),
                },
                RouteHop {
                    provider: "working".into(),
                    model: "m".into(),
                },
            ],
        );
        let router = Router::new(providers, routes);

        let mut s = router
            .stream(&req_for("m"))
            .await
            .expect("stream should succeed");
        use futures_util::StreamExt;
        let chunks: Vec<OAIStreamChunk> = s.by_ref().collect().await;
        assert!(
            !chunks.is_empty(),
            "should fail over to the working provider"
        );
        assert_eq!(
            chunks[0].choices[0].delta.role.as_deref(),
            Some("assistant")
        );
        assert_eq!(
            chunks.last().unwrap().choices[0].finish_reason.as_deref(),
            Some("stop")
        );
    }

    #[tokio::test]
    async fn empty_stream_with_no_fallback_returns_error() {
        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        providers.insert("empty".into(), Arc::new(EmptyStreamProvider::new("empty")));
        let mut routes: HashMap<String, Vec<RouteHop>> = HashMap::new();
        routes.insert(
            "m".into(),
            vec![RouteHop {
                provider: "empty".into(),
                model: "m".into(),
            }],
        );
        let router = Router::new(providers, routes);

        let err = match router.stream(&req_for("m")).await {
            Ok(_) => panic!("empty stream with no fallback should error"),
            Err(e) => e,
        };
        assert_eq!(err.status, Some(502));
        assert!(err.retriable);
    }

    #[tokio::test]
    async fn error_stream_fails_over_to_next_hop() {
        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        providers.insert("err".into(), Arc::new(ErrStreamProvider::new("err")));
        providers.insert("working".into(), Arc::new(WorkingProvider::new("working")));
        let mut routes: HashMap<String, Vec<RouteHop>> = HashMap::new();
        routes.insert(
            "m".into(),
            vec![
                RouteHop {
                    provider: "err".into(),
                    model: "m".into(),
                },
                RouteHop {
                    provider: "working".into(),
                    model: "m".into(),
                },
            ],
        );
        let router = Router::new(providers, routes);

        let mut s = router
            .stream(&req_for("m"))
            .await
            .expect("stream should succeed");
        use futures_util::StreamExt;
        let chunks: Vec<OAIStreamChunk> = s.by_ref().collect().await;
        assert!(
            !chunks.is_empty(),
            "should fail over to the working provider"
        );
        assert_eq!(
            chunks.last().unwrap().choices[0].finish_reason.as_deref(),
            Some("stop")
        );
    }

    #[tokio::test]
    async fn tool_capability_400_fails_over_for_complete_and_stream() {
        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        providers.insert(
            "reject".into(),
            Arc::new(ToolRejectProvider::with_kind(
                "reject",
                canonical::UpstreamErrorKind::ToolCapability,
            )),
        );
        providers.insert("working".into(), Arc::new(WorkingProvider::new("working")));
        let mut routes: HashMap<String, Vec<RouteHop>> = HashMap::new();
        routes.insert(
            "m".into(),
            vec![
                RouteHop {
                    provider: "reject".into(),
                    model: "reject-model".into(),
                },
                RouteHop {
                    provider: "working".into(),
                    model: "working-model".into(),
                },
            ],
        );
        let router = Router::new(providers, routes);

        let complete = router
            .complete(&req_with_tool("m"))
            .await
            .expect("complete fallback");
        assert_eq!(
            complete.choices[0].message.content.as_deref(),
            Some("working")
        );
        let mut stream = router
            .stream(&req_with_tool("m"))
            .await
            .expect("stream fallback");
        let chunks: Vec<OAIStreamChunk> = stream.by_ref().collect().await;
        assert_eq!(
            chunks
                .last()
                .and_then(|chunk| chunk.choices[0].finish_reason.as_deref()),
            Some("stop")
        );
        let reject_health = router
            .health()
            .into_iter()
            .find(|health| health.id == "reject")
            .unwrap();
        assert_eq!(reject_health.breaker.failures, 2);
    }

    #[tokio::test]
    async fn ordinary_400_without_tools_remains_terminal() {
        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        providers.insert("reject".into(), Arc::new(ToolRejectProvider::new("reject")));
        providers.insert("working".into(), Arc::new(WorkingProvider::new("working")));
        let mut routes: HashMap<String, Vec<RouteHop>> = HashMap::new();
        routes.insert(
            "m".into(),
            vec![
                RouteHop {
                    provider: "reject".into(),
                    model: "reject-model".into(),
                },
                RouteHop {
                    provider: "working".into(),
                    model: "working-model".into(),
                },
            ],
        );
        let router = Router::new(providers, routes);
        let error = router
            .complete(&req_for("m"))
            .await
            .expect_err("ordinary 400 must remain terminal");
        assert_eq!(error.status, Some(400));
        assert!(!error.retriable);
    }

    /// The regression case: Codex CLI always sends tools; OpenAI returns 400
    /// with an OAuth refresh-token body; the router must NOT fail over. The
    /// breaker failures counter must stay at 0 (non-retriable errors are
    /// recorded as Cancelled, not Failure).
    #[tokio::test]
    async fn auth_expired_400_with_tools_is_terminal() {
        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        providers.insert(
            "reject".into(),
            Arc::new(ToolRejectProvider::with_kind(
                "reject",
                canonical::UpstreamErrorKind::AuthExpired,
            )),
        );
        providers.insert("working".into(), Arc::new(WorkingProvider::new("working")));
        let mut routes: HashMap<String, Vec<RouteHop>> = HashMap::new();
        routes.insert(
            "m".into(),
            vec![
                RouteHop {
                    provider: "reject".into(),
                    model: "reject-model".into(),
                },
                RouteHop {
                    provider: "working".into(),
                    model: "working-model".into(),
                },
            ],
        );
        let router = Router::new(providers, routes);

        let error = router
            .complete(&req_with_tool("m"))
            .await
            .expect_err("auth-expired 400 with tools must remain terminal");
        assert_eq!(error.status, Some(400));
        assert!(!error.retriable);
        assert_eq!(error.kind, Some(canonical::UpstreamErrorKind::AuthExpired));
        let reject_health = router
            .health()
            .into_iter()
            .find(|health| health.id == "reject")
            .unwrap();
        assert_eq!(
            reject_health.breaker.failures, 0,
            "non-retriable 400 must not increment the breaker"
        );
    }

    /// An unclassified 400 with tools must also remain terminal — the
    /// legacy default behaviour, restored after the regression. Only an
    /// explicit `ToolCapability` classification opens the failover gate.
    #[tokio::test]
    async fn unclassified_400_with_tools_is_terminal() {
        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        providers.insert("reject".into(), Arc::new(ToolRejectProvider::new("reject")));
        providers.insert("working".into(), Arc::new(WorkingProvider::new("working")));
        let mut routes: HashMap<String, Vec<RouteHop>> = HashMap::new();
        routes.insert(
            "m".into(),
            vec![
                RouteHop {
                    provider: "reject".into(),
                    model: "reject-model".into(),
                },
                RouteHop {
                    provider: "working".into(),
                    model: "working-model".into(),
                },
            ],
        );
        let router = Router::new(providers, routes);

        let error = router
            .complete(&req_with_tool("m"))
            .await
            .expect_err("unclassified 400 with tools must remain terminal");
        assert_eq!(error.status, Some(400));
        assert!(!error.retriable);
        assert_eq!(error.kind, None);
    }

    // ---- Phase 17: per-surface failover chains ----

    /// A provider whose responses carry its own id, so tests can tell which
    /// surface's chain served the request.
    struct TaggedProvider {
        id: &'static str,
        breaker: CircuitBreaker,
    }

    impl TaggedProvider {
        fn new(id: &'static str) -> Self {
            Self {
                id,
                breaker: CircuitBreaker::new(empty_cb()),
            }
        }
    }

    #[async_trait]
    impl Provider for TaggedProvider {
        fn id(&self) -> &str {
            self.id
        }
        fn enabled(&self) -> bool {
            true
        }
        fn models(&self) -> &[String] {
            &[]
        }
        fn breaker(&self) -> &CircuitBreaker {
            &self.breaker
        }

        async fn complete(
            &self,
            _req: &ChatCompletionRequest,
            _upstream_model: &str,
        ) -> Result<OAIChatResponse, ProviderError> {
            Err(canonical::make_error(
                format!("fail:{}", self.id),
                Some(500),
                Some(true),
            ))
        }

        async fn stream(
            &self,
            _req: &ChatCompletionRequest,
            upstream_model: &str,
        ) -> Result<Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>>, ProviderError> {
            // First TaggedProvider in a chain fails (retriable) so the next
            // hop is tried; a provider tagged "ok" yields a chunk carrying
            // its id in the finish_reason slot for assertion.
            if self.id.starts_with("ok") {
                let tag = self.id.to_string();
                let stream =
                    futures_util::stream::iter(vec![crate::proxy::converter::make_stream_chunk(
                        "id",
                        upstream_model,
                        crate::proxy::canonical::StreamDelta {
                            role: Some("assistant".into()),
                            content: Some(tag),
                            tool_calls: None,
                            reasoning: None,
                            extra: Default::default(),
                        },
                        None,
                    )]);
                Ok(Box::pin(stream))
            } else {
                Err(canonical::make_error(
                    format!("fail:{}", self.id),
                    Some(500),
                    Some(true),
                ))
            }
        }
    }

    #[tokio::test]
    async fn surface_chains_route_to_their_own_queue() {
        // Two-hop chains (bad -> ok-<surface>) per surface; the bad head
        // fails with a retriable error so the chain falls through to ok.
        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        providers.insert(
            "claude-bad".into(),
            Arc::new(TaggedProvider::new("claude-bad")),
        );
        providers.insert(
            "ok-claude".into(),
            Arc::new(TaggedProvider::new("ok-claude")),
        );
        providers.insert(
            "codex-bad".into(),
            Arc::new(TaggedProvider::new("codex-bad")),
        );
        providers.insert("ok-codex".into(), Arc::new(TaggedProvider::new("ok-codex")));
        providers.insert("chat-bad".into(), Arc::new(TaggedProvider::new("chat-bad")));
        providers.insert("ok-chat".into(), Arc::new(TaggedProvider::new("ok-chat")));

        let mut queues: HashMap<String, Vec<String>> = HashMap::new();
        queues.insert(
            SURFACE_CLAUDE.into(),
            vec!["claude-bad".into(), "ok-claude".into()],
        );
        queues.insert(
            SURFACE_CODEX.into(),
            vec!["codex-bad".into(), "ok-codex".into()],
        );
        queues.insert(
            SURFACE_CHAT.into(),
            vec!["chat-bad".into(), "ok-chat".into()],
        );

        let router = Router::new(providers, HashMap::new()).with_queues(queues);

        // A model with no explicit route and no auto-route: each surface's
        // chain must serve it, failing over bad -> ok.
        for (surface, expected_tag) in [
            (SURFACE_CLAUDE, "ok-claude"),
            (SURFACE_CODEX, "ok-codex"),
            (SURFACE_CHAT, "ok-chat"),
        ] {
            let mut s = router
                .stream_for(surface, "_test", &req_for("unknown-model"))
                .await
                .expect("stream should succeed via surface chain");
            let first = s.next().await.expect("first chunk");
            assert_eq!(
                first.choices[0].delta.content.as_deref(),
                Some(expected_tag),
                "surface {surface} should be served by its own chain"
            );
        }
    }

    #[tokio::test]
    async fn explicit_route_still_wins_over_surface_chain() {
        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        providers.insert("ok-chat".into(), Arc::new(TaggedProvider::new("ok-chat")));
        providers.insert("ok-route".into(), Arc::new(TaggedProvider::new("ok-route")));
        let mut routes: HashMap<String, Vec<RouteHop>> = HashMap::new();
        routes.insert(
            "special-model".into(),
            vec![RouteHop {
                provider: "ok-route".into(),
                model: "special-model".into(),
            }],
        );
        let mut queues: HashMap<String, Vec<String>> = HashMap::new();
        queues.insert(SURFACE_CHAT.into(), vec!["ok-chat".into()]);
        let router = Router::new(providers, routes).with_queues(queues);

        let mut s = router
            .stream_for(SURFACE_CHAT, "_test", &req_for("special-model"))
            .await
            .expect("stream should succeed");
        let first = s.next().await.expect("first chunk");
        assert_eq!(
            first.choices[0].delta.content.as_deref(),
            Some("ok-route"),
            "explicit route must win over the surface chain"
        );
    }

    #[tokio::test]
    async fn no_chain_anywhere_yields_404() {
        let router = Router::new(HashMap::new(), HashMap::new()).with_queues(HashMap::new());
        let err = match router
            .stream_for(SURFACE_CHAT, "_test", &req_for("unknown-model"))
            .await
        {
            Ok(_) => panic!("no providers at all should 404"),
            Err(e) => e,
        };
        assert_eq!(err.status, Some(404));
    }

    /// The queue head is always tried first. When it fails, requests fall
    /// through to the tail; when its breaker recovers (cooldown → half-open
    /// → closed), the head serves again — the failover never "sticks" to a
    /// fallback once the head is back.
    #[tokio::test]
    async fn recovered_queue_head_regains_requests() {
        use crate::proxy::providers::mock::MockProvider;
        let p1 = Arc::new(MockProvider::new(
            "p1".into(),
            vec!["mock-fast".into()],
            true,
            CircuitBreakerConfig {
                failure_threshold: 2,
                cooldown_ms: 50,
                half_open_retries: 1,
                success_threshold: 2,
            },
        )) as Arc<dyn Provider>;
        let p2 = Arc::new(MockProvider::new(
            "p2".into(),
            vec!["mock-fast".into()],
            true,
            CircuitBreakerConfig {
                failure_threshold: 5,
                cooldown_ms: 30_000,
                half_open_retries: 1,
                success_threshold: 2,
            },
        )) as Arc<dyn Provider>;

        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        providers.insert("p1".into(), p1.clone());
        providers.insert("p2".into(), p2.clone());
        let mut routes: HashMap<String, Vec<RouteHop>> = HashMap::new();
        routes.insert(
            "m".into(),
            vec![
                RouteHop {
                    provider: "p1".into(),
                    model: "mock-fast".into(),
                },
                RouteHop {
                    provider: "p2".into(),
                    model: "mock-fast".into(),
                },
            ],
        );
        let router = Router::new(providers, routes);

        let served_by = |c: &OAIChatResponse| -> String {
            c.choices[0]
                .message
                .content
                .as_deref()
                .unwrap_or("")
                .to_string()
        };

        // 1. Head (p1) healthy → served by p1.
        let c1 = router.complete(&req_for("m")).await.expect("req 1");
        assert!(
            served_by(&c1).contains("MOCK[p1]"),
            "head should serve first: {}",
            served_by(&c1)
        );

        // 2. Trip p1's breaker (threshold 2) → p1 OPEN.
        p1.breaker().record_failure();
        p1.breaker().record_failure();

        // 3. During cooldown p1 is skipped → falls to p2.
        let c2 = router.complete(&req_for("m")).await.expect("req 2");
        assert!(
            served_by(&c2).contains("MOCK[p2]"),
            "open head must fall to tail: {}",
            served_by(&c2)
        );

        // 4. Cooldown expires → p1 half-open → serves the probe request.
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        let c3 = router
            .complete(&req_for("m"))
            .await
            .expect("req 3 (half-open probe)");
        assert!(
            served_by(&c3).contains("MOCK[p1]"),
            "half-open head should probe: {}",
            served_by(&c3)
        );

        // 5. Second success in half-open closes p1's breaker.
        let c4 = router
            .complete(&req_for("m"))
            .await
            .expect("req 4 (close breaker)");
        assert!(
            served_by(&c4).contains("MOCK[p1]"),
            "head should keep serving while recovering"
        );

        // 6. Fully recovered head serves again — request returns to the queue head.
        let c5 = router
            .complete(&req_for("m"))
            .await
            .expect("req 5 (recovered head)");
        assert!(
            served_by(&c5).contains("MOCK[p1]"),
            "recovered head must regain requests: {}",
            served_by(&c5)
        );
    }
}
