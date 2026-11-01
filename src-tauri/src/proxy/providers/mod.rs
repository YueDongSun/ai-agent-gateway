//! Provider trait + factory.
//!
//! Each upstream API provider (mock, openai, anthropic) implements this trait.
//! The Router dispatches to providers via this interface.

pub mod anthropic;
pub mod gemini;
pub(crate) mod http_client;
pub mod mock;
pub mod openai;
pub mod responses;
pub mod sse;

#[cfg(test)]
mod effort_wire_tests;
#[cfg(test)]
mod protocol_tests;
#[cfg(test)]
mod responses_tests;

use crate::proxy::canonical::{ChatCompletionRequest, OAIChatResponse, ProviderError};
use crate::proxy::circuit_breaker::CircuitBreaker;
use async_trait::async_trait;
use futures_core::Stream;

/// Static Anthropic authentication selected by the configured credential source.
/// Passthrough providers continue to relay the incoming credential unchanged.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AnthropicAuthScheme {
    #[default]
    ApiKey,
    Bearer,
}

/// A provider instance with a circuit breaker. Shared across requests via `Arc`.
#[async_trait]
pub trait Provider: Send + Sync {
    /// A fresh screening-only instance may bound completion response buffering.
    /// Unsupported implementations fail closed rather than issuing an unbounded probe.
    fn set_response_byte_limit(&mut self, _limit: usize) -> bool {
        false
    }
    /// Only explicitly bounded screening instances may retain a probe body.
    fn take_probe_response(&self) -> Option<String> {
        None
    }
    /// Whether the captured body was truncated or credential-redacted.
    fn probe_response_flags(&self) -> (bool, bool) {
        (false, false)
    }
    fn id(&self) -> &str;
    fn enabled(&self) -> bool;
    fn models(&self) -> &[String];
    fn breaker(&self) -> &CircuitBreaker;

    /// Native upstream transport. Routing uses this only as a tie-breaker
    /// within the same assessed tier and configured priority.
    fn wire_protocol(&self) -> &'static str {
        "unknown"
    }

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

    /// Optional receipt-aware path. Legacy and custom providers remain valid,
    /// but the default emits no transport evidence rather than fabricating it.
    async fn complete_observed(
        &self,
        req: &ChatCompletionRequest,
        upstream_model: &str,
        _observation: crate::proxy::routing_receipts::ProviderObservation,
    ) -> Result<OAIChatResponse, ProviderError> {
        self.complete(req, upstream_model).await
    }

    /// Input-token count for an Anthropic-shaped `count_tokens` body. The
    /// router hands over the OWNED, model-patched hop body — that clone is
    /// the only whole-body copy, so implementors must rewrite `model` in
    /// place instead of cloning again (the Anthropic relay still normalizes
    /// the local `[1M]` marker on the owned body before the upstream call).
    /// Anthropic-family providers relay upstream (exact); the default is a
    /// deterministic local estimate (ASCII-ish bytes ≈ 4 chars/token, CJK
    /// ≈ 1 char/token) — good enough for context-window decisions on
    /// non-Anthropic upstreams that offer no counting endpoint.
    async fn count_tokens(
        &self,
        body: serde_json::Value,
        _client_credential: Option<&crate::proxy::canonical::Client_credential>,
    ) -> Result<u32, ProviderError> {
        Ok(estimate_input_tokens(&body))
    }

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

    async fn stream_observed(
        &self,
        req: &ChatCompletionRequest,
        upstream_model: &str,
        _observation: crate::proxy::routing_receipts::ProviderObservation,
    ) -> Result<std::pin::Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>>, ProviderError> {
        self.stream(req, upstream_model).await
    }
}

// Re-export so lib.rs can use it without a separate import path.
pub use crate::proxy::canonical::OAIStreamChunk;

/// Deterministic token estimate over an Anthropic-shaped count body:
/// ~4 chars per token for ASCII-ish text, ~1 token per CJK character.
/// Purely local — no network, no secrets read.
pub(crate) fn estimate_input_tokens(body: &serde_json::Value) -> u32 {
    let mut ascii_chars: u64 = 0;
    let mut wide_chars: u64 = 0;
    fn walk(value: &serde_json::Value, ascii_chars: &mut u64, wide_chars: &mut u64) {
        match value {
            serde_json::Value::String(text) => {
                for character in text.chars() {
                    // CJK and fullwidth ranges dominate tokenizer output at
                    // roughly one token per character; Latin text averages
                    // closer to four characters per token.
                    let is_wide = matches!(character as u32,
                        0x1100..=0x115F | 0x2E80..=0xA4CF | 0xAC00..=0xD7A3
                        | 0xF900..=0xFAFF | 0xFE30..=0xFE4F | 0xFF00..=0xFF60
                        | 0xFFE0..=0xFFE6 | 0x20000..=0x3FFFD);
                    if is_wide {
                        *wide_chars += 1;
                    } else {
                        *ascii_chars += 1;
                    }
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    walk(item, ascii_chars, wide_chars);
                }
            }
            serde_json::Value::Object(map) => {
                for (key, item) in map {
                    // Structural keys count toward the total too.
                    *ascii_chars += key.len() as u64;
                    walk(item, ascii_chars, wide_chars);
                }
            }
            _ => {}
        }
    }
    walk(body, &mut ascii_chars, &mut wide_chars);
    let estimated = (ascii_chars / 4) + wide_chars;
    u32::try_from(estimated.max(1)).unwrap_or(u32::MAX)
}

/// Build a boxed Provider from a ProxyProvider row.
pub fn build_provider(
    pp: &crate::database::dao::proxy_providers::ProxyProvider,
) -> Result<Box<dyn Provider>, String> {
    build_provider_with_auth(pp, AnthropicAuthScheme::default())
}

/// Build with an explicitly resolved static Anthropic authentication scheme.
/// Other protocols retain their own authentication contract.
pub fn build_provider_with_auth(
    pp: &crate::database::dao::proxy_providers::ProxyProvider,
    auth_scheme: AnthropicAuthScheme,
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
        .map(|provider| Box::new(provider.with_auth_scheme(auth_scheme)) as Box<dyn Provider>),
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
        // OpenAI Responses API upstream (Codex CLI / Codex Desktop native).
        "openai-responses" | "openai_responses" => responses::ResponsesProvider::new(
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
        other => Err(format!("unknown provider type: \"{other}\"")),
    }
}

#[cfg(test)]
mod count_tokens_tests {
    use super::*;

    #[test]
    fn estimate_is_deterministic_and_split_by_script() {
        let ascii = serde_json::json!({
            "model": "m",
            "messages": [{"role": "user", "content": "abcdefgh"}]
        });
        let cjk = serde_json::json!({
            "model": "m",
            "messages": [{"role": "user", "content": "八个汉字的正文"}]
        });
        let first = estimate_input_tokens(&ascii);
        assert_eq!(first, estimate_input_tokens(&ascii), "deterministic");
        // CJK content weighs roughly one token per character, so the CJK
        // body must count higher than the same-length ASCII body.
        assert!(
            estimate_input_tokens(&cjk)
                > estimate_input_tokens(&serde_json::json!({
                    "model": "m",
                    "messages": [{"role": "user", "content": "12345678"}]
                }))
        );
        assert!(first >= 1, "an empty-ish body still counts one token");
    }
}
