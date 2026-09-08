use crate::database::dao::proxy_providers::CircuitBreakerConfig;
use crate::proxy::canonical::{ChatCompletionRequest, OAIChatResponse, OAIStreamChunk, ProviderError};
use crate::proxy::circuit_breaker::CircuitBreaker;
use crate::proxy::providers::sse::SseDataStream;
use crate::proxy::providers::Provider;
use async_trait::async_trait;
use futures_core::Stream;
use futures_util::StreamExt;
use std::pin::Pin;
use std::time::Duration;

pub struct OpenAIProvider {
    id: String,
    base_url: String,
    api_key: String,
    models: Vec<String>,
    enabled: bool,
    timeout: Duration,
    breaker: CircuitBreaker,
}

impl OpenAIProvider {
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
            id,
            base_url,
            api_key,
            models,
            enabled,
            timeout: Duration::from_millis(timeout_ms),
            breaker: CircuitBreaker::new(cb_cfg),
        }
    }

    fn endpoint(&self) -> String {
        let base = self.base_url.trim_end_matches('/');
        let base = if base.is_empty() { "https://api.openai.com/v1" } else { base };
        format!("{}/chat/completions", base)
    }

    fn headers(&self) -> reqwest::header::HeaderMap {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert("content-type", "application/json".parse().unwrap());
        if !self.api_key.is_empty() {
            h.insert("authorization", format!("Bearer {}", self.api_key).parse().unwrap());
        }
        h
    }
}

// placeholder impl
#[async_trait]
impl Provider for OpenAIProvider {
    fn id(&self) -> &str { &self.id }
    fn enabled(&self) -> bool { self.enabled }
    fn models(&self) -> &[String] { &self.models }
    fn breaker(&self) -> &CircuitBreaker { &self.breaker }

    async fn complete(&self, req: &ChatCompletionRequest) -> Result<OAIChatResponse, ProviderError> {
        let client = reqwest::Client::builder()
            .timeout(self.timeout)
            .build()
            .map_err(|e| crate::proxy::canonical::make_error(format!("http client: {e}"), None, Some(false)))?;
        // Defensive: client may carry the local 1M-context marker in the
        // model field (e.g. `claude-fable-5[1M]` from Claude Code). Upstream
        // OpenAI-compatible endpoints don't recognize the suffix — strip
        // before serializing.
        let mut send_req = req.clone();
        send_req.model = crate::proxy::strip_one_m_suffix(&send_req.model);
        let body = serde_json::to_value(&send_req)
            .map_err(|e| crate::proxy::canonical::make_error(format!("serialize: {e}"), None, Some(false)))?;
        let mut body = body;
        if let Some(obj) = body.as_object_mut() {
            obj.insert("stream".into(), serde_json::Value::Bool(false));
        }
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
        let resp: OAIChatResponse = res
            .json()
            .await
            .map_err(|e| crate::proxy::canonical::make_error(format!("{} decode: {}", self.id, e), Some(502), Some(true)))?;
        Ok(resp)
    }

    async fn stream(
        &self,
        req: &ChatCompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>>, ProviderError> {
        // Streaming requests get a near-unbounded total timeout (the response
        // body may stream for many minutes); the pre-stream phase is bounded
        // by the Router's first-byte timeout instead. Matches cc-switch's
        // 24h stream cap.
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(24 * 60 * 60))
            .build()
            .map_err(|e| crate::proxy::canonical::make_error(format!("http client: {e}"), None, Some(false)))?;
        let mut send_req = req.clone();
        send_req.model = crate::proxy::strip_one_m_suffix(&send_req.model);
        let mut body = serde_json::to_value(&send_req)
            .map_err(|e| crate::proxy::canonical::make_error(format!("serialize: {e}"), None, Some(false)))?;
        if let Some(obj) = body.as_object_mut() {
            obj.insert("stream".into(), serde_json::Value::Bool(true));
        }
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
        let log_id = self.id.clone();
        let stream = async_stream::stream! {
            let sse_stream = SseDataStream::new(byte_stream);
            let mut chunks = std::pin::pin!(sse_stream);
            while let Some(data) = chunks.next().await {
                if data.as_str() == "[DONE]" { return; }
                let parsed: Result<serde_json::Value, _> = serde_json::from_str(&data);
                match parsed {
                    Ok(val) => {
                        if let Ok(chunk) = serde_json::from_value::<OAIStreamChunk>(val) {
                            yield chunk;
                        } else {
                            log::debug!("{}: skipping unparseable SSE chunk", log_id);
                        }
                    }
                    Err(_) => log::debug!("{}: skipping invalid JSON in SSE", log_id),
                }
            }
        };
        Ok(Box::pin(stream))
    }
}
