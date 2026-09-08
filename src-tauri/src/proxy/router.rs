//! Router — resolves model aliases to ordered failover chains and executes
//! them with circuit breaker protection.
//!
//! Ported from `src/gateway/router.ts`.

use crate::database::dao::routes::RouteHop;
use crate::proxy::canonical::{self, ChatCompletionRequest, OAIChatResponse, OAIStreamChunk, ProviderError};
use crate::proxy::circuit_breaker::CircuitBreakerSnapshot;
use crate::proxy::providers::Provider;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;

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
    routes: HashMap<String, Vec<RouteHop>>,
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
    pub last_used: std::sync::Mutex<std::collections::HashMap<String, String>>,
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

impl Router {
    /// Build a Router from the proxy providers and routes loaded from the DB.
    /// `build_provider` in `providers::mod.rs` must have already been called.
    pub fn new(
        providers: HashMap<String, Arc<dyn Provider>>,
        routes: HashMap<String, Vec<RouteHop>>,
    ) -> Self {
        Self {
            providers, routes, queue: Vec::new(), queues: HashMap::new(),
            last_used: std::sync::Mutex::new(std::collections::HashMap::new()),
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
    pub fn resolve_route(&self, model: &str) -> Option<Vec<RouteHop>> {
        if let Some(hops) = self.routes.get(model) {
            return Some(hops.clone());
        }
        // Auto-route: find any enabled provider that lists this model.
        for (pid, prov) in &self.providers {
            if prov.enabled() && prov.models().iter().any(|m| m == model) {
                return Some(vec![RouteHop {
                    provider: pid.clone(),
                    model: model.into(),
                }]);
            }
        }
        // Homepage failover queue: last-chance chain, model passed through.
        if !self.queue.is_empty() {
            return Some(
                self.queue
                    .iter()
                    .map(|pid| RouteHop {
                        provider: pid.clone(),
                        model: model.into(),
                    })
                    .collect(),
            );
        }
        None
    }

    /// Surface-aware resolution: same precedence as `resolve_route`, but the
    /// last-chance chain is the surface's per-app queue instead of the global
    /// one. `/v1/messages` falls back to the Claude chain, `/v1/responses`
    /// to the Codex chain, `/v1/chat/completions` to the copilot+pi chain.
    pub fn resolve_route_for(&self, surface: &str, model: &str) -> Option<Vec<RouteHop>> {
        if let Some(hops) = self.routes.get(model) {
            return Some(hops.clone());
        }
        for (pid, prov) in &self.providers {
            if prov.enabled() && prov.models().iter().any(|m| m == model) {
                return Some(vec![RouteHop {
                    provider: pid.clone(),
                    model: model.into(),
                }]);
            }
        }
        if let Some(chain) = self.queues.get(surface) {
            if !chain.is_empty() {
                return Some(
                    chain
                        .iter()
                        .map(|pid| RouteHop {
                            provider: pid.clone(),
                            model: model.into(),
                        })
                        .collect(),
                );
            }
        }
        // Legacy global queue as the very last resort.
        if !self.queue.is_empty() {
            return Some(
                self.queue
                    .iter()
                    .map(|pid| RouteHop {
                        provider: pid.clone(),
                        model: model.into(),
                    })
                    .collect(),
            );
        }
        None
    }

    /// Non-streaming completion with automatic failover. For each hop:
    /// 1. Check circuit breaker — skip if open.
    /// 2. Call provider.complete.
    /// 3. On success: record_success, return.
    /// 4. On retriable error: record_failure, try next hop.
    /// 5. On non-retriable error: propagate immediately.
    pub async fn complete(&self, req: &ChatCompletionRequest) -> Result<OAIChatResponse, ProviderError> {
        let hops = self.resolve_route(&req.model).ok_or_else(|| {
            canonical::make_error(
                format!("no route / provider for model \"{}\"", req.model),
                Some(404),
                Some(false),
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
            canonical::make_error(
                format!("no route / provider for model \"{}\"", req.model),
                Some(404),
                Some(false),
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
        let hops = vec![RouteHop {
            provider: provider_id.to_string(),
            model: req.model.clone(),
        }];
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
        let hops = vec![RouteHop {
            provider: provider_id.to_string(),
            model: req.model.clone(),
        }];
        // See complete_with_provider — surface is unused on the pinned
        // path but the unified signature requires it.
        self.stream_hops(surface, app_type, req, hops).await
    }

    pub async fn complete_hops(
        &self,
        _surface: &str,
        app_type: &str,
        req: &ChatCompletionRequest,
        hops: Vec<RouteHop>,
    ) -> Result<OAIChatResponse, ProviderError> {

        let mut last_err: Option<ProviderError> = None;
        for hop in &hops {
            let provider = match self.providers.get(&hop.provider) {
                Some(p) => p,
                None => {
                    log::warn!("router: skip unknown provider \"{}\"", hop.provider);
                    continue;
                }
            };
            if !provider.enabled() {
                log::warn!("router: skip disabled provider \"{}\"", hop.provider);
                continue;
            }
            if !provider.breaker().allow() {
                log::warn!("router: circuit OPEN for \"{}\", skipping", hop.provider);
                continue;
            }
            let sub_req = ChatCompletionRequest {
                model: hop.model.clone(),
                messages: req.messages.clone(),
                stream: req.stream,
                temperature: req.temperature,
                max_tokens: req.max_tokens,
                tools: req.tools.clone(),
                tool_choice: req.tool_choice.clone(),
                extra: req.extra.clone(),
            };
            match provider.complete(&sub_req).await {
                Ok(resp) => {
                    provider.breaker().record_success();
                    if let Ok(mut lu) = self.last_used.lock() {
                        lu.insert(app_type.to_string(), provider.id().to_string());
                    }
                    log::info!("router: OK  {} <- {}/{}", req.model, provider.id(), hop.model);
                    return Ok(resp);
                }
                Err(e) => {
                    let retriable = e.retriable;
                    // Only retriable (provider-level) failures count against
                    // the breaker - non-retriable errors are local
                    // construction problems, not the provider's fault.
                    if retriable {
                        provider.breaker().record_failure();
                    }
                    log::warn!(
                        "router: FAIL {}/{}: {} (retriable={})",
                        provider.id(),
                        hop.model,
                        e.message,
                        retriable
                    );
                    last_err = Some(e);
                    if !retriable {
                        return Err(last_err.unwrap());
                    }
                }
            }
        }
        Err(last_err.unwrap_or_else(|| canonical::make_error("all hops exhausted", Some(503), Some(false))))
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
        canonical::make_error(
            format!("no route / provider for model \"{}\"", req.model),
            Some(404),
            Some(false),
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
        canonical::make_error(
            format!("no route / provider for model \"{}\"", req.model),
            Some(404),
            Some(false),
        )
    })?;
    self.stream_hops(surface, app_type, req, hops).await
}

pub async fn stream_hops(
    &self,
    _surface: &str,
    app_type: &str,
    req: &ChatCompletionRequest,
    hops: Vec<RouteHop>,
) -> Result<futures_util::stream::BoxStream<'static, OAIStreamChunk>, ProviderError> {
    use futures_util::StreamExt;

    let mut last_err: Option<ProviderError> = None;
    for hop in &hops {
        let provider = match self.providers.get(&hop.provider) {
            Some(p) => p,
            None => continue,
        };
        if !provider.enabled() {
            log::warn!("router: skip disabled provider \"{}\"", hop.provider);
            continue;
        }
        if !provider.breaker().allow() {
            log::warn!("router: circuit OPEN for \"{}\", skipping (stream)", hop.provider);
            continue;
        }
        let sub_req = ChatCompletionRequest {
            model: hop.model.clone(),
            messages: req.messages.clone(),
            stream: req.stream,
            temperature: req.temperature,
            max_tokens: req.max_tokens,
            tools: req.tools.clone(),
            tool_choice: req.tool_choice.clone(),
            extra: req.extra.clone(),
        };
        let mut chunk_stream = match provider.stream(&sub_req).await {
            Ok(s) => s,
            Err(e) => {
                let retriable = e.retriable;
                // Only retriable (provider-level) failures count against the
                // breaker - non-retriable errors are local construction
                // problems (serialize / client build), not the provider's
                // fault, and must not pollute its health.
                if retriable {
                    provider.breaker().record_failure();
                }
                log::warn!("router: STREAM FAIL {}/{}: {} (retriable={})", provider.id(), hop.model, e.message, retriable);
                last_err = Some(e);
                if !retriable { return Err(last_err.unwrap()); }
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
                provider.breaker().record_failure();
                log::warn!(
                    "router: STREAM EMPTY {} <- {}/{} (treating as failure)",
                    req.model,
                    provider.id(),
                    hop.model
                );
                last_err = Some(canonical::make_error(
                    format!("provider {} returned an empty stream", provider.id()),
                    Some(502),
                    Some(true),
                ));
                continue;
            }
            Err(_elapsed) => {
                // Upstream stalled past the first-byte budget. Treat as a
                // retriable failure (counts against the breaker) so the next
                // hop gets a chance and the breaker opens quickly when a
                // provider is consistently slow-to-first-byte.
                provider.breaker().record_failure();
                log::warn!(
                    "router: STREAM TIMEOUT ({} ms) {} <- {}/{} (treating as failure)",
                    self.first_byte_timeout_ms,
                    req.model,
                    provider.id(),
                    hop.model
                );
                last_err = Some(canonical::make_error(
                    format!(
                        "provider {} exceeded first-byte budget ({} ms)",
                        provider.id(),
                        self.first_byte_timeout_ms
                    ),
                    Some(504),
                    Some(true),
                ));
                continue;
            }
        };
        provider.breaker().record_success();
        if let Ok(mut lu) = self.last_used.lock() {
            lu.insert(app_type.to_string(), provider.id().to_string());
        }
        log::info!("router: STREAM OK (first byte) {} <- {}/{}", req.model, provider.id(), hop.model);
        // Chain the first chunk with the rest of the stream. Mid-stream errors
        // surface to the client (no transparent failover).
        let rest = chunk_stream;
        let combined = futures_util::stream::once(async move { first }).chain(rest);
        return Ok(combined.boxed());
    }
    Err(last_err.unwrap_or_else(|| canonical::make_error("all stream hops exhausted", Some(503), Some(false))))
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
        fn id(&self) -> &str { self.id }
        fn enabled(&self) -> bool { true }
        fn models(&self) -> &[String] { &[] }
        fn breaker(&self) -> &CircuitBreaker { &self.breaker }

        async fn complete(&self, _req: &ChatCompletionRequest) -> Result<OAIChatResponse, ProviderError> {
            Err(canonical::make_error("unused", Some(500), Some(true)))
        }

        async fn stream(
            &self,
            _req: &ChatCompletionRequest,
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
        fn id(&self) -> &str { self.id }
        fn enabled(&self) -> bool { true }
        fn models(&self) -> &[String] { &[] }
        fn breaker(&self) -> &CircuitBreaker { &self.breaker }

        async fn complete(&self, _req: &ChatCompletionRequest) -> Result<OAIChatResponse, ProviderError> {
            Err(canonical::make_error("unused", Some(500), Some(true)))
        }

        async fn stream(
            &self,
            _req: &ChatCompletionRequest,
        ) -> Result<Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>>, ProviderError> {
            Err(canonical::make_error("forced failure", Some(503), Some(true)))
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
        fn id(&self) -> &str { self.id }
        fn enabled(&self) -> bool { true }
        fn models(&self) -> &[String] { &[] }
        fn breaker(&self) -> &CircuitBreaker { &self.breaker }

        async fn complete(&self, _req: &ChatCompletionRequest) -> Result<OAIChatResponse, ProviderError> {
            Err(canonical::make_error("unused", Some(500), Some(true)))
        }

        async fn stream(
            &self,
            req: &ChatCompletionRequest,
        ) -> Result<Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>>, ProviderError> {
            let model = req.model.clone();
            let stream = futures_util::stream::iter(vec![
                crate::proxy::converter::make_stream_chunk(
                    "id",
                    &model,
                    crate::proxy::canonical::StreamDelta {
                        role: Some("assistant".into()),
                        content: None,
                        tool_calls: None,
                        reasoning: None,
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
                    },
                    Some("stop".into()),
                ),
            ]);
            Ok(Box::pin(stream))
        }
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
            Self { id, breaker: CircuitBreaker::new(empty_cb()) }
        }
    }

    #[async_trait]
    impl Provider for StallProvider {
        fn id(&self) -> &str { self.id }
        fn enabled(&self) -> bool { true }
        fn models(&self) -> &[String] { &[] }
        fn breaker(&self) -> &CircuitBreaker { &self.breaker }

        async fn complete(&self, _req: &ChatCompletionRequest) -> Result<OAIChatResponse, ProviderError> {
            Err(canonical::make_error("unused", Some(500), Some(true)))
        }

        async fn stream(
            &self,
            _req: &ChatCompletionRequest,
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
                RouteHop { provider: "stall".into(), model: "m".into() },
                RouteHop { provider: "working".into(), model: "m".into() },
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
        assert!(!chunks.is_empty(), "should fail over to the working provider");
        assert_eq!(chunks[0].choices[0].delta.role.as_deref(), Some("assistant"));
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
                RouteHop { provider: "s1".into(), model: "m".into() },
                RouteHop { provider: "s2".into(), model: "m".into() },
                RouteHop { provider: "s3".into(), model: "m".into() },
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
                RouteHop { provider: "empty".into(), model: "m".into() },
                RouteHop { provider: "working".into(), model: "m".into() },
            ],
        );
        let router = Router::new(providers, routes);

        let mut s = router.stream(&req_for("m")).await.expect("stream should succeed");
        use futures_util::StreamExt;
        let chunks: Vec<OAIStreamChunk> = s.by_ref().collect().await;
        assert!(!chunks.is_empty(), "should fail over to the working provider");
        assert_eq!(chunks[0].choices[0].delta.role.as_deref(), Some("assistant"));
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
            vec![RouteHop { provider: "empty".into(), model: "m".into() }],
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
                RouteHop { provider: "err".into(), model: "m".into() },
                RouteHop { provider: "working".into(), model: "m".into() },
            ],
        );
        let router = Router::new(providers, routes);

        let mut s = router.stream(&req_for("m")).await.expect("stream should succeed");
        use futures_util::StreamExt;
        let chunks: Vec<OAIStreamChunk> = s.by_ref().collect().await;
        assert!(!chunks.is_empty(), "should fail over to the working provider");
        assert_eq!(
            chunks.last().unwrap().choices[0].finish_reason.as_deref(),
            Some("stop")
        );
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
            Self { id, breaker: CircuitBreaker::new(empty_cb()) }
        }
    }

    #[async_trait]
    impl Provider for TaggedProvider {
        fn id(&self) -> &str { self.id }
        fn enabled(&self) -> bool { true }
        fn models(&self) -> &[String] { &[] }
        fn breaker(&self) -> &CircuitBreaker { &self.breaker }

        async fn complete(&self, _req: &ChatCompletionRequest) -> Result<OAIChatResponse, ProviderError> {
            Err(canonical::make_error(
                format!("fail:{}", self.id),
                Some(500),
                Some(true),
            ))
        }

        async fn stream(
            &self,
            req: &ChatCompletionRequest,
        ) -> Result<Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>>, ProviderError> {
            // First TaggedProvider in a chain fails (retriable) so the next
            // hop is tried; a provider tagged "ok" yields a chunk carrying
            // its id in the finish_reason slot for assertion.
            if self.id.starts_with("ok") {
                let tag = self.id.to_string();
                let stream = futures_util::stream::iter(vec![
                    crate::proxy::converter::make_stream_chunk(
                        "id",
                        &req.model,
                        crate::proxy::canonical::StreamDelta {
                            role: Some("assistant".into()),
                            content: Some(tag),
                            tool_calls: None,
                            reasoning: None,
                        },
                        None,
                    ),
                ]);
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
        providers.insert("claude-bad".into(), Arc::new(TaggedProvider::new("claude-bad")));
        providers.insert("ok-claude".into(), Arc::new(TaggedProvider::new("ok-claude")));
        providers.insert("codex-bad".into(), Arc::new(TaggedProvider::new("codex-bad")));
        providers.insert("ok-codex".into(), Arc::new(TaggedProvider::new("ok-codex")));
        providers.insert("chat-bad".into(), Arc::new(TaggedProvider::new("chat-bad")));
        providers.insert("ok-chat".into(), Arc::new(TaggedProvider::new("ok-chat")));

        let mut queues: HashMap<String, Vec<String>> = HashMap::new();
        queues.insert(SURFACE_CLAUDE.into(), vec!["claude-bad".into(), "ok-claude".into()]);
        queues.insert(SURFACE_CODEX.into(), vec!["codex-bad".into(), "ok-codex".into()]);
        queues.insert(SURFACE_CHAT.into(), vec!["chat-bad".into(), "ok-chat".into()]);

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
            vec![RouteHop { provider: "ok-route".into(), model: "special-model".into() }],
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
        let err = match router.stream_for(SURFACE_CHAT, "_test", &req_for("unknown-model")).await {
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
                RouteHop { provider: "p1".into(), model: "mock-fast".into() },
                RouteHop { provider: "p2".into(), model: "mock-fast".into() },
            ],
        );
        let router = Router::new(providers, routes);

        let served_by = |c: &OAIChatResponse| -> String {
            c.choices[0].message.content.as_deref().unwrap_or("").to_string()
        };

        // 1. Head (p1) healthy → served by p1.
        let c1 = router.complete(&req_for("m")).await.expect("req 1");
        assert!(served_by(&c1).contains("MOCK[p1]"), "head should serve first: {}", served_by(&c1));

        // 2. Trip p1's breaker (threshold 2) → p1 OPEN.
        p1.breaker().record_failure();
        p1.breaker().record_failure();

        // 3. During cooldown p1 is skipped → falls to p2.
        let c2 = router.complete(&req_for("m")).await.expect("req 2");
        assert!(served_by(&c2).contains("MOCK[p2]"), "open head must fall to tail: {}", served_by(&c2));

        // 4. Cooldown expires → p1 half-open → serves the probe request.
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        let c3 = router.complete(&req_for("m")).await.expect("req 3 (half-open probe)");
        assert!(served_by(&c3).contains("MOCK[p1]"), "half-open head should probe: {}", served_by(&c3));

        // 5. Second success in half-open closes p1's breaker.
        let c4 = router.complete(&req_for("m")).await.expect("req 4 (close breaker)");
        assert!(served_by(&c4).contains("MOCK[p1]"), "head should keep serving while recovering");

        // 6. Fully recovered head serves again — request returns to the queue head.
        let c5 = router.complete(&req_for("m")).await.expect("req 5 (recovered head)");
        assert!(served_by(&c5).contains("MOCK[p1]"), "recovered head must regain requests: {}", served_by(&c5));
    }
}
