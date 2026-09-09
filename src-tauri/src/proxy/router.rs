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
                provider.breaker().record_success();
                log::info!("router: STREAM OK (empty) {} <- {}/{}", req.model, provider.id(), hop.model);
                return Ok(futures_util::stream::empty().boxed());
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
