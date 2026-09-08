//! Provider trait + factory.
//!
//! Each upstream API provider (mock, openai, anthropic) implements this trait.
//! The Router dispatches to providers via this interface.

pub mod anthropic;
pub mod mock;
pub mod openai;
pub mod sse;

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

    /// Non-streaming completion.
    async fn complete(&self, req: &ChatCompletionRequest) -> Result<OAIChatResponse, ProviderError>;

    /// Streaming completion as an async stream of canonical OAI chunks.
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
        "openai" => Ok(Box::new(openai::OpenAIProvider::new(
            pp.id.clone(),
            pp.base_url.clone(),
            pp.api_key.clone(),
            pp.models.clone(),
            pp.enabled,
            pp.timeout_ms,
            pp.circuit_breaker.clone(),
        ))),
        "anthropic" => Ok(Box::new(anthropic::AnthropicProvider::new(
            pp.id.clone(),
            pp.base_url.clone(),
            pp.api_key.clone(),
            pp.models.clone(),
            pp.enabled,
            pp.timeout_ms,
            pp.circuit_breaker.clone(),
        ))),
        other => Err(format!("unknown provider type: \"{other}\"")),
    }
}