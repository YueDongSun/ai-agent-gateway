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
    /// The provider id that served the most recent successful request.
    /// Updated atomically by `complete_hops` / `stream_hops` on success.
    /// Read by the health endpoint for UI flow visualization.
    pub last_used: std::sync::Mutex<Option<String>>,
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
        Self { providers, routes, queue: Vec::new(), queues: HashMap::new(), last_used: std::sync::Mutex::new(None) }
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
    /// check if any enabled provider directly serves the model (auto-route).
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
        self.complete_hops(req, hops).await
    }

    /// Surface-aware non-streaming completion: the last-resort chain is the
    /// entry endpoint's per-app queue (`/v1/messages` -> Claude chain,
    /// `/v1/responses` -> Codex chain, `/v1/chat/completions` -> chat chain).
    pub async fn complete_for(
        &self,
        surface: &str,
        req: &ChatCompletionRequest,
    ) -> Result<OAIChatResponse, ProviderError> {
        let hops = self.resolve_route_for(surface, &req.model).ok_or_else(|| {
            canonical::make_error(
                format!("no route / provider for model \"{}\"", req.model),
                Some(404),
                Some(false),
            )
        })?;
        self.complete_hops(req, hops).await
    }

    /// Force all requests through a specific provider (pinned/forced mode).
    /// Bypasses the normal failover chain. The request's model passes through
    /// unchanged. Used when the user toggles the force-pin ON for a provider.
    pub async fn complete_with_provider(
        &self,
        provider_id: &str,
        req: &ChatCompletionRequest,
    ) -> Result<OAIChatResponse, ProviderError> {
        let hops = vec![RouteHop {
            provider: provider_id.to_string(),
            model: req.model.clone(),
        }];
        self.complete_hops(req, hops).await
    }

    /// Force-stream variant of `complete_with_provider`.
    pub async fn stream_with_provider(
        &self,
        provider_id: &str,
        req: &ChatCompletionRequest,
    ) -> Result<futures_util::stream::BoxStream<'static, OAIStreamChunk>, ProviderError> {
        let hops = vec![RouteHop {
            provider: provider_id.to_string(),
            model: req.model.clone(),
        }];
        self.stream_hops(req, hops).await
    }

    pub async fn complete_hops(
        &self,
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
                        *lu = Some(provider.id().to_string());
                    }
                    log::info!("router: OK  {} <- {}/{}", req.model, provider.id(), hop.model);
                    return Ok(resp);
                }
                Err(e) => {
                    let retriable = e.retriable;
                    provider.breaker().record_failure();
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
    self.stream_hops(req, hops).await
}

/// Surface-aware streaming completion (see `complete_for`).
pub async fn stream_for(
    &self,
    surface: &str,
    req: &ChatCompletionRequest,
) -> Result<futures_util::stream::BoxStream<'static, OAIStreamChunk>, ProviderError> {
    let hops = self.resolve_route_for(surface, &req.model).ok_or_else(|| {
        canonical::make_error(
            format!("no route / provider for model \"{}\"", req.model),
            Some(404),
            Some(false),
        )
    })?;
    self.stream_hops(req, hops).await
}

pub async fn stream_hops(
    &self,
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
        let mut chunk_stream = match provider.stream(&sub_req) {
            Ok(s) => s,
            Err(e) => {
                let retriable = e.retriable;
                provider.breaker().record_failure();
                log::warn!("router: STREAM FAIL {}/{}: {} (retriable={})", provider.id(), hop.model, e.message, retriable);
                last_err = Some(e);
                if !retriable { return Err(last_err.unwrap()); }
                continue;
            }
        };
        // Probe first chunk
        let first = match chunk_stream.next().await {
            Some(c) => c,
            None => {
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
        };
        provider.breaker().record_success();
        if let Ok(mut lu) = self.last_used.lock() {
            *lu = Some(provider.id().to_string());
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

        fn stream(
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

        fn stream(
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

        fn stream(
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

        fn stream(
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
                .stream_for(surface, &req_for("unknown-model"))
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
            .stream_for(SURFACE_CHAT, &req_for("special-model"))
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
        let err = match router.stream_for(SURFACE_CHAT, &req_for("unknown-model")).await {
            Ok(_) => panic!("no providers at all should 404"),
            Err(e) => e,
        };
        assert_eq!(err.status, Some(404));
    }
}
