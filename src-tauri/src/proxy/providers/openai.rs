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
        let body = serde_json::to_value(req)
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
                Some(status >= 500 || status == 429),
            ));
        }
        let resp: OAIChatResponse = res
            .json()
            .await
            .map_err(|e| crate::proxy::canonical::make_error(format!("{} decode: {}", self.id, e), Some(502), Some(true)))?;
        Ok(resp)
    }

    fn stream(
        &self,
        req: &ChatCompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>>, ProviderError> {
        let client = reqwest::Client::builder()
            .timeout(self.timeout)
            .build()
            .map_err(|e| crate::proxy::canonical::make_error(format!("http client: {e}"), None, Some(false)))?;
        let mut body = serde_json::to_value(req)
            .map_err(|e| crate::proxy::canonical::make_error(format!("serialize: {e}"), None, Some(false)))?;
        if let Some(obj) = body.as_object_mut() {
            obj.insert("stream".into(), serde_json::Value::Bool(true));
        }
        let endpoint = self.endpoint();
        let headers = self.headers();
        let provider_id = self.id.clone();
        let stream = async_stream::stream! {
            let res = match client.post(&endpoint).headers(headers).json(&body).send().await {
                Ok(r) => r,
                Err(e) => { log::warn!("{} stream request: {}", provider_id, e); return; }
            };
            let status = res.status().as_u16();
            if status >= 400 {
                let text = res.text().await.unwrap_or_default();
                log::warn!("{} stream HTTP {}: {}", provider_id, status, &text.chars().take(300).collect::<String>());
                return;
            }
            let byte_stream = res.bytes_stream().map(|r| {
                r.map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
            });
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
                            log::debug!("{}: skipping unparseable SSE chunk", provider_id);
                        }
                    }
                    Err(_) => log::debug!("{}: skipping invalid JSON in SSE", provider_id),
                }
            }
        };
        Ok(Box::pin(stream))
    }
}
