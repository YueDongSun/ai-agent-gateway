//! Google Gemini (Generative Language API) upstream provider.
//!
//! Converts the canonical chat-completions shape to a Gemini
//! `GenerateContentRequest` and posts it to
//! `{base}/v1beta/models/{model}:generateContent` (or `:streamGenerateContent`
//! with `alt=sse`). Unlike the OpenAI/Anthropic providers the request URL
//! embeds the model id, so the endpoint is resolved per request via
//! [`crate::proxy::endpoint::gemini_method_url`] instead of once at
//! construction.

use crate::database::dao::proxy_providers::CircuitBreakerConfig;
use crate::proxy::canonical::{
    ChatCompletionRequest, OAIChatResponse, OAIStreamChunk, ProviderError,
};
use crate::proxy::circuit_breaker::CircuitBreaker;
use crate::proxy::gemini::{
    gemini_resp_to_oai_resp, gemini_stream_to_oai_chunks, oai_req_to_gemini_req,
};
use crate::proxy::providers::http_client::{build_http_client, error_kind};
use crate::proxy::providers::sse::SseDataStream;
use crate::proxy::providers::Provider;
use async_trait::async_trait;
use futures_core::Stream;
use futures_util::StreamExt;
use serde_json::json;
use std::pin::Pin;
use std::time::{Duration, Instant};

pub struct GeminiProvider {
    id: String,
    base_url: String,
    api_key: String,
    /// Provider-mandated extra request headers, merged in `headers()` after
    /// the trusted set (reserved headers never overridden).
    extra_headers: std::collections::HashMap<String, String>,
    models: Vec<String>,
    enabled: bool,
    timeout: Duration,
    client: reqwest::Client,
    breaker: CircuitBreaker,
}

impl GeminiProvider {
    pub fn new(
        id: String,
        base_url: String,
        api_key: String,
        models: Vec<String>,
        enabled: bool,
        timeout_ms: u64,
        cb_cfg: CircuitBreakerConfig,
        extra_headers: std::collections::HashMap<String, String>,
    ) -> Result<Self, String> {
        let client = build_http_client("Gemini")?;
        let base_url = if base_url.is_empty() {
            "https://generativelanguage.googleapis.com".to_string()
        } else {
            base_url
        };
        // Validate the base once at construction so a malformed base surfaces
        // in the builder's warn log rather than on the first request.
        crate::proxy::endpoint::gemini_method_url(&base_url, "gemini-2.5-pro", "generateContent")
            .map_err(|error| format!("invalid gemini base_url: {error}"))?;
        Ok(Self {
            id,
            base_url,
            api_key,
            extra_headers,
            models,
            enabled,
            timeout: Duration::from_millis(timeout_ms),
            client,
            breaker: CircuitBreaker::new(cb_cfg),
        })
    }

    fn method_url(&self, model: &str, method: &str) -> Result<String, ProviderError> {
        let model = crate::proxy::strip_one_m_suffix(model);
        crate::proxy::endpoint::gemini_method_url(&self.base_url, &model, method).map_err(|_| {
            crate::proxy::canonical::make_provider_error(
                "upstream_transport_error",
                "endpoint_build",
                Some(502),
                Some(true),
                None,
            )
        })
    }

    fn headers(&self) -> reqwest::header::HeaderMap {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert("content-type", "application/json".parse().unwrap());
        if !self.api_key.is_empty() {
            // The Generative Language API's standard client header. OAuth
            // bearer credentials are not supported by this provider (cc-switch
            // handles `ya29.` / JSON service creds; AAG providers are API-key
            // based).
            h.insert("x-goog-api-key", self.api_key.parse().unwrap());
        }
        crate::proxy::providers::http_client::apply_extra_headers(&mut h, &self.extra_headers);
        h
    }

    async fn send(
        &self,
        url: &str,
        body: &crate::proxy::gemini::GeminiRequest,
        stream: bool,
        request_id: &str,
    ) -> Result<reqwest::Response, ProviderError> {
        let started = Instant::now();
        let mut request = self.client.post(url).headers(self.headers()).json(body);
        if !stream {
            request = request.timeout(self.timeout);
        }
        let send = request.send();
        let response = if stream {
            tokio::time::timeout(self.timeout, send)
                .await
                .map_err(|_| {
                    crate::proxy::canonical::make_provider_error(
                        "upstream_timeout",
                        "response_headers",
                        None,
                        Some(true),
                        None,
                    )
                })?
        } else {
            send.await
        }
        .map_err(|error| {
            crate::proxy::canonical::make_provider_error(
                "upstream_transport_error",
                error_kind(&error),
                None,
                Some(true),
                None,
            )
        })?;
        log::debug!(
            "provider: headers request_id={} provider={} protocol=gemini stream={} elapsed_ms={}",
            request_id,
            crate::diagnostics::safe_reference(&self.id),
            stream,
            started.elapsed().as_millis()
        );
        let status = response.status().as_u16();
        if status >= 400 {
            // Read the body only for its size — diagnostics never include the
            // upstream payload (it may echo private material).
            let response_bytes = response.bytes().await.map(|b| b.len()).unwrap_or(0);
            return Err(crate::proxy::canonical::make_provider_error(
                "upstream_http_error",
                "response_status",
                Some(status),
                None,
                Some(response_bytes),
            ));
        }
        Ok(response)
    }
}

#[async_trait]
impl Provider for GeminiProvider {
    fn id(&self) -> &str {
        &self.id
    }
    fn enabled(&self) -> bool {
        self.enabled
    }
    fn models(&self) -> &[String] {
        &self.models
    }
    fn breaker(&self) -> &CircuitBreaker {
        &self.breaker
    }

    async fn complete(
        &self,
        req: &ChatCompletionRequest,
        upstream_model: &str,
    ) -> Result<OAIChatResponse, ProviderError> {
        let body = oai_req_to_gemini_req(req);
        let request_id = req.request_id.as_deref().unwrap_or("unknown");
        let url = self.method_url(upstream_model, "generateContent")?;
        let res = self.send(&url, &body, false, request_id).await?;
        let bytes = res.bytes().await.map_err(|error| {
            crate::proxy::canonical::make_provider_error(
                "upstream_transport_error",
                error_kind(&error),
                Some(502),
                Some(true),
                None,
            )
        })?;
        let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| {
            crate::proxy::canonical::make_provider_error(
                "upstream_decode_error",
                "response_decode",
                Some(502),
                Some(true),
                Some(bytes.len()),
            )
        })?;
        if let Some(error) = value.get("error") {
            // Gemini reports application errors with HTTP 200 in some relays.
            let status = crate::proxy::canonical::stream_error_status(error);
            return Err(crate::proxy::canonical::make_provider_error(
                "upstream_http_error",
                "response_status",
                Some(status),
                None,
                Some(bytes.len()),
            ));
        }
        Ok(gemini_resp_to_oai_resp(&value, upstream_model))
    }

    async fn stream(
        &self,
        req: &ChatCompletionRequest,
        upstream_model: &str,
    ) -> Result<Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>>, ProviderError> {
        let body = oai_req_to_gemini_req(req);
        let request_id = req.request_id.clone().unwrap_or_else(|| "unknown".into());
        let mut url = self.method_url(upstream_model, "streamGenerateContent")?;
        // Gemini's SSE framing is opt-in via `alt=sse`.
        url.push_str("?alt=sse");
        let provider_id = self.id.clone();
        // Eager send + status check so the router can fail over on a 4xx/5xx
        // in one RTT instead of timing out on an empty stream.
        let res = self.send(&url, &body, true, &request_id).await?;
        let model = crate::proxy::strip_one_m_suffix(upstream_model);
        let byte_stream = res.bytes_stream().map(move |r| {
            r.map_err(|e| {
                log::warn!(
                    "provider: stream body failed request_id={} provider={} kind={}",
                    request_id,
                    crate::diagnostics::safe_reference(&provider_id),
                    error_kind(&e)
                );
                std::io::Error::other(error_kind(&e))
            })
        });
        let stream = async_stream::stream! {
            let sse_stream = SseDataStream::new(byte_stream).map(|event| {
                event.unwrap_or_else(|_| {
                    json!({
                        "error": {
                            "code": 502,
                            "message": "Upstream stream transport failed",
                            "status": "UNAVAILABLE",
                        }
                    })
                    .to_string()
                })
            });
            let oai_chunks = gemini_stream_to_oai_chunks(sse_stream, model.clone());
            let mut oai_chunks = std::pin::pin!(oai_chunks);
            while let Some(chunk) = oai_chunks.next().await {
                yield chunk;
            }
        };
        Ok(Box::pin(stream))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::dao::proxy_providers::CircuitBreakerConfig;
    use axum::response::{IntoResponse, Response};
    use axum::routing::post;
    use axum::{Json, Router};
    use serde_json::{json, Value};

    #[tokio::test]
    async fn complete_posts_gemini_wire_and_converts_response() {
        async fn handler(
            axum::extract::Path(tail): axum::extract::Path<String>,
            Json(body): Json<Value>,
        ) -> Response {
            // The model id + method live in the URL tail, Gemini-style.
            assert!(
                tail.contains("gemini-2.5-pro:generateContent"),
                "tail={tail}"
            );
            // The request must be Gemini-shaped: contents + systemInstruction.
            assert!(body.get("contents").is_some());
            assert!(body.get("model").is_none()); // model lives in the URL
            Json(json!({
                "candidates": [{
                    "content": { "role": "model", "parts": [{ "text": "hi there" }] },
                    "finishReason": "STOP",
                }],
                "usageMetadata": {
                    "promptTokenCount": 3, "candidatesTokenCount": 2, "totalTokenCount": 5
                },
                "modelVersion": "gemini-2.5-pro",
            }))
            .into_response()
        }
        let app = Router::new().route("/v1beta/models/*tail", post(handler));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let provider = GeminiProvider::new(
            "p".into(),
            format!("http://{address}"),
            "key".into(),
            vec!["gemini-2.5-pro".into()],
            true,
            2_000,
            CircuitBreakerConfig::default(),
            Default::default(),
        )
        .unwrap();
        let req: ChatCompletionRequest = serde_json::from_value(json!({
            "model": "gemini-2.5-pro",
            "messages": [
                { "role": "system", "content": "be brief" },
                { "role": "user", "content": "hi" },
            ],
        }))
        .unwrap();
        let response = provider.complete(&req, "gemini-2.5-pro").await.unwrap();
        assert_eq!(
            response.choices[0].message.content.as_deref(),
            Some("hi there")
        );
        assert_eq!(response.usage.as_ref().unwrap().total_tokens, 5);
        server.abort();
    }

    #[test]
    fn malformed_base_fails_at_construction() {
        assert!(GeminiProvider::new(
            "p".into(),
            "not a url".into(),
            String::new(),
            vec!["m".into()],
            true,
            1_000,
            CircuitBreakerConfig::default(),
            Default::default(),
        )
        .is_err());
    }
}
