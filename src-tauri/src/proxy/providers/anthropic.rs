use crate::database::dao::proxy_providers::CircuitBreakerConfig;
use crate::proxy::canonical::{ChatCompletionRequest, OAIChatResponse, OAIStreamChunk, ProviderError};
use crate::proxy::circuit_breaker::CircuitBreaker;
use crate::proxy::converter::{
    anthropic_resp_to_oai_resp, anthropic_stream_to_oai_chunks, oai_req_to_anthropic_req,
    AnthropicResponse,
};
use crate::proxy::providers::sse::SseDataStream;
use crate::proxy::providers::Provider;
use async_trait::async_trait;
use futures_core::Stream;
use futures_util::StreamExt;
use std::pin::Pin;
use std::time::Duration;

pub struct AnthropicProvider {
    id: String,
    base_url: String,
    api_key: String,
    models: Vec<String>,
    enabled: bool,
    timeout: Duration,
    breaker: CircuitBreaker,
}

impl AnthropicProvider {
    pub fn new(
        id: String,
        base_url: String,
        api_key: String,
        models: Vec<String>,
        enabled: bool,
        timeout_ms: u64,
        cb_cfg: CircuitBreakerConfig,
    ) -> Self {
        Self {
            id, base_url, api_key, models, enabled,
            timeout: Duration::from_millis(timeout_ms),
            breaker: CircuitBreaker::new(cb_cfg),
        }
    }

    fn endpoint(&self) -> String {
        let base = self.base_url.trim_end_matches('/');
        let base = if base.is_empty() { "https://api.anthropic.com" } else { base };
        if base.ends_with("/v1") {
            format!("{}/messages", base)
        } else {
            format!("{}/v1/messages", base)
        }
    }

    fn headers(&self) -> reqwest::header::HeaderMap {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert("content-type", "application/json".parse().unwrap());
        h.insert("anthropic-version", "2023-06-01".parse().unwrap());
        if !self.api_key.is_empty() {
            h.insert("x-api-key", self.api_key.parse().unwrap());
        }
        h
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    fn id(&self) -> &str { &self.id }
    fn enabled(&self) -> bool { self.enabled }
    fn models(&self) -> &[String] { &self.models }
    fn breaker(&self) -> &CircuitBreaker { &self.breaker }

    async fn complete(&self, req: &ChatCompletionRequest) -> Result<OAIChatResponse, ProviderError> {
        let client = reqwest::Client::builder()
            .timeout(self.timeout)
            .build()
            .map_err(|e| crate::proxy::canonical::make_error(format!("http client: {e}"), None, Some(false)))?;
        let mut body = oai_req_to_anthropic_req(req);
        // Defensive: the client may send `model: claude-fable-5[1M]` even
        // when the route hop is unannotated. Strip the local 1M-context
        // marker before forwarding — upstream endpoints reject the suffix.
        body.model = crate::proxy::strip_one_m_suffix(&body.model);
        body.stream = Some(false);
        let res = client
            .post(self.endpoint())
            .headers(self.headers())
            .json(&body)
            .send()
            .await
            .map_err(|e| crate::proxy::canonical::make_error(format!("{} request: {}", self.id, e), None, Some(true)))?;
        let status = res.status().as_u16();
        if status >= 400 {
            let text = res.text().await.unwrap_or_default();
            return Err(crate::proxy::canonical::make_error(
                format!("{} HTTP {}: {}", self.id, status, &text.chars().take(300).collect::<String>()),
                Some(status),
                // Upstream rejection: retriable in AAG's hop model - each hop
                // is a different provider + upstream model, so a 400/403/429
                // from this provider says nothing about whether the next hop
                // can serve the request.
                Some(true),
            ));
        }
        let resp: AnthropicResponse = res
            .json()
            .await
            .map_err(|e| crate::proxy::canonical::make_error(format!("{} decode: {}", self.id, e), Some(502), Some(true)))?;
        Ok(anthropic_resp_to_oai_resp(&resp, &req.model))
    }

    async fn stream(
        &self,
        req: &ChatCompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>>, ProviderError> {
        // Streaming requests get a near-unbounded total timeout (the response
        // body may stream for many minutes); the pre-stream phase is bounded
        // by the Router's first-byte timeout instead, so a dead upstream
        // can't hold the chain hostage. Matches cc-switch's 24h stream cap.
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(24 * 60 * 60))
            .build()
            .map_err(|e| crate::proxy::canonical::make_error(format!("http client: {e}"), None, Some(false)))?;
        let mut body = oai_req_to_anthropic_req(req);
        body.stream = Some(true);
        // `oai_req_to_anthropic_req` already strips the `[1M]` marker from
        // `body.model` (converter.rs). No need to strip again here — that
        // would be a no-op invariant and a maintenance trap.
        let model = crate::proxy::strip_one_m_suffix(&req.model);
        let provider_id = self.id.clone();
        // Eager send + status check: an upstream 4xx/5xx becomes a real
        // ProviderError with the status code, so the router can fail over
        // immediately (one RTT) instead of timing out on an empty stream.
        let res = client
            .post(self.endpoint())
            .headers(self.headers())
            .json(&body)
            .send()
            .await
            .map_err(|e| crate::proxy::canonical::make_error(format!("{} request: {}", self.id, e), None, Some(true)))?;
        let status = res.status().as_u16();
        if status >= 400 {
            let text = res.text().await.unwrap_or_default();
            return Err(crate::proxy::canonical::make_error(
                format!("{} HTTP {}: {}", self.id, status, &text.chars().take(300).collect::<String>()),
                Some(status),
                Some(true),
            ));
        }
        // `bytes_stream()` consumes the response by value, so build the byte
        // stream eagerly and move it into the generator (keeps `res` alive
        // for as long as the stream lives).
        let byte_stream = res.bytes_stream().map(move |r| {
            r.map_err(|e| {
                log::warn!("{} stream body: {e}", provider_id);
                std::io::Error::new(std::io::ErrorKind::Other, e)
            })
        });
        let stream = async_stream::stream! {
            let sse_stream = SseDataStream::new(byte_stream);
            let oai_chunks = anthropic_stream_to_oai_chunks(sse_stream, model.clone());
            let mut oai_chunks = std::pin::pin!(oai_chunks);
            while let Some(chunk) = oai_chunks.next().await {
                yield chunk;
            }
        };
        Ok(Box::pin(stream))
    }
}
