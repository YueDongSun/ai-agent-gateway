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
}

impl Router {
    /// Build a Router from the proxy providers and routes loaded from the DB.
    /// `build_provider` in `providers::mod.rs` must have already been called.
    pub fn new(
        providers: HashMap<String, Arc<dyn Provider>>,
        routes: HashMap<String, Vec<RouteHop>>,
    ) -> Self {
        Self { providers, routes }
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
                extra: req.extra.clone(),
            };
            match provider.complete(&sub_req).await {
                Ok(resp) => {
                    provider.breaker().record_success();
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
    use futures_util::StreamExt;
    let hops = self.resolve_route(&req.model).ok_or_else(|| {
        canonical::make_error(
            format!("no route / provider for model \"{}\"", req.model),
            Some(404),
            Some(false),
        )
    })?;

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
                    crate::proxy::canonical::StreamDelta { role: Some("assistant".into()), content: None },
                    None,
                ),
                crate::proxy::converter::make_stream_chunk(
                    "id",
                    &model,
                    crate::proxy::canonical::StreamDelta { role: None, content: None },
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
}
