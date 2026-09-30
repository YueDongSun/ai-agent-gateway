//! Provider trait + factory.
//!
//! Each upstream API provider (mock, openai, anthropic) implements this trait.
//! The Router dispatches to providers via this interface.

pub mod anthropic;
pub mod gemini;
mod http_client;
pub mod mock;
pub mod openai;
pub mod sse;

#[cfg(test)]
mod protocol_tests;

use crate::proxy::canonical::{ChatCompletionRequest, OAIChatResponse, ProviderError};
use crate::proxy::circuit_breaker::CircuitBreaker;
use async_trait::async_trait;
use futures_core::Stream;

/// A provider instance with a circuit breaker. Shared across requests via `Arc`.
#[async_trait]
pub trait Provider: Send + Sync {
    fn id(&self) -> &str;
    fn enabled(&self) -> bool;
    fn models(&self) -> &[String];
    fn breaker(&self) -> &CircuitBreaker;

    /// True for providers that authenticate upstream with the CLIENT's
    /// inbound credential (anthropic_passthrough). The router skips such
    /// hops when the request carries no client credential — only the
    /// /v1/messages edge attaches one, and a hop that cannot authenticate
    /// would just burn an upstream 401 and terminate the route.
    fn requires_client_credential(&self) -> bool {
        false
    }

    /// Non-streaming completion.
    ///
    /// `upstream_model` is the route hop's upstream model for this attempt —
    /// it overrides `req.model` on the wire. Passing it as a parameter lets
    /// the router dispatch every hop against the SAME shared request, so a
    /// large conversation payload is never cloned per hop.
    async fn complete(
        &self,
        req: &ChatCompletionRequest,
        upstream_model: &str,
    ) -> Result<OAIChatResponse, ProviderError>;

    /// Streaming completion as an async stream of canonical OAI chunks.
    ///
    /// `upstream_model` carries the route hop's model (see `complete`).
    ///
    /// Async so the provider can open the upstream connection and check the
    /// HTTP status eagerly: an upstream 4xx/5xx surfaces as `Err` here
    /// (carrying the status) BEFORE any streaming begins, letting the router
    /// fail over cleanly instead of swallowing the error into an empty
    /// stream. Once Ok is returned, mid-stream errors surface to the client
    /// (no transparent failover).
    async fn stream(
        &self,
        req: &ChatCompletionRequest,
        upstream_model: &str,
    ) -> Result<std::pin::Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>>, ProviderError>;
}

// Re-export so lib.rs can use it without a separate import path.
pub use crate::proxy::canonical::OAIStreamChunk;

/// Build a boxed Provider from a ProxyProvider row.
pub fn build_provider(
    pp: &crate::database::dao::proxy_providers::ProxyProvider,
) -> Result<Box<dyn Provider>, String> {
    match pp.provider_type.as_str() {
        "mock" => Ok(Box::new(mock::MockProvider::new(
            pp.id.clone(),
            pp.models.clone(),
            pp.enabled,
            pp.circuit_breaker.clone(),
        ))),
        "openai" => openai::OpenAIProvider::new(
            pp.id.clone(),
            pp.base_url.clone(),
            pp.api_key.clone(),
            pp.models.clone(),
            pp.enabled,
            pp.timeout_ms,
            pp.circuit_breaker.clone(),
            pp.extra_headers.clone(),
        )
        .map(|provider| Box::new(provider) as Box<dyn Provider>),
        "anthropic" => anthropic::AnthropicProvider::new(
            pp.id.clone(),
            pp.base_url.clone(),
            pp.api_key.clone(),
            pp.models.clone(),
            pp.enabled,
            pp.timeout_ms,
            pp.circuit_breaker.clone(),
            pp.extra_headers.clone(),
        )
        .map(|provider| Box::new(provider) as Box<dyn Provider>),
        "gemini" => gemini::GeminiProvider::new(
            pp.id.clone(),
            pp.base_url.clone(),
            pp.api_key.clone(),
            pp.models.clone(),
            pp.enabled,
            pp.timeout_ms,
            pp.circuit_breaker.clone(),
            pp.extra_headers.clone(),
        )
        .map(|provider| Box::new(provider) as Box<dyn Provider>),
        // Subscription passthrough: no stored key; the client's inbound
        // credential is relayed verbatim to the (official Anthropic) base.
        "anthropic_passthrough" => anthropic::AnthropicProvider::new_passthrough(
            pp.id.clone(),
            pp.base_url.clone(),
            pp.models.clone(),
            pp.enabled,
            pp.timeout_ms,
            pp.circuit_breaker.clone(),
            pp.extra_headers.clone(),
        )
        .map(|provider| Box::new(provider) as Box<dyn Provider>),
        other => Err(format!("unknown provider type: \"{other}\"")),
    }
}
