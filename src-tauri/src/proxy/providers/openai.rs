use crate::database::dao::proxy_providers::CircuitBreakerConfig;
use crate::proxy::canonical::{
    self, ChatCompletionRequest, OAIChatResponse, OAIStreamChunk, ProviderError,
};
use crate::proxy::circuit_breaker::CircuitBreaker;
use crate::proxy::providers::http_client::{build_http_client, error_kind};
use crate::proxy::providers::sse::SseDataStream;
use crate::proxy::providers::Provider;
use async_trait::async_trait;
use futures_core::Stream;
use futures_util::StreamExt;
use std::pin::Pin;
use std::time::{Duration, Instant};

pub struct OpenAIProvider {
    id: String,
    endpoint: String,
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

impl OpenAIProvider {
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
        // Resolve the request endpoint once at construction (see anthropic.rs).
        let endpoint = crate::proxy::endpoint::endpoint_url(
            if base_url.is_empty() {
                "https://api.openai.com/v1"
            } else {
                &base_url
            },
            crate::proxy::endpoint::Protocol::Openai,
        )
        .map_err(|error| format!("invalid openai base_url: {error}"))?;
        Ok(Self {
            id,
            endpoint,
            api_key,
            extra_headers,
            models,
            enabled,
            timeout: Duration::from_millis(timeout_ms),
            client: build_http_client("OpenAI")?,
            breaker: CircuitBreaker::new(cb_cfg),
        })
    }

    fn endpoint(&self) -> &str {
        &self.endpoint
    }

    fn headers(&self) -> reqwest::header::HeaderMap {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert("content-type", "application/json".parse().unwrap());
        if !self.api_key.is_empty() {
            h.insert(
                "authorization",
                format!("Bearer {}", self.api_key).parse().unwrap(),
            );
        }
        crate::proxy::providers::http_client::apply_extra_headers(&mut h, &self.extra_headers);
        h
    }

    fn request_body(
        &self,
        req: &ChatCompletionRequest,
        upstream_model: &str,
        stream: bool,
    ) -> Result<serde_json::Value, ProviderError> {
        // Serialize the shared request directly — the hop's upstream model
        // is injected into the JSON, so the payload is never cloned here.
        let mut body = serde_json::to_value(req).map_err(|error| {
            let _ = error;
            crate::proxy::canonical::make_provider_error(
                "request_serialize_error",
                "request_serialize",
                None,
                Some(false),
                None,
            )
        })?;
        if let Some(object) = body.as_object_mut() {
            object.insert(
                "model".into(),
                serde_json::json!(crate::proxy::strip_one_m_suffix(upstream_model)),
            );
            object.insert("stream".into(), serde_json::Value::Bool(stream));
            if stream && !object.contains_key("stream_options") {
                object.insert(
                    "stream_options".into(),
                    serde_json::json!({ "include_usage": true }),
                );
            }
        }
        Ok(body)
    }

    async fn send_body(
        &self,
        body: &serde_json::Value,
        stream: bool,
        request_id: &str,
    ) -> Result<reqwest::Response, ProviderError> {
        let started = Instant::now();
        let mut request = self
            .client
            .post(self.endpoint())
            .headers(self.headers())
            .json(body);
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
            "provider: headers request_id={} provider={} protocol=openai stream={} elapsed_ms={}",
            request_id,
            crate::diagnostics::safe_reference(&self.id),
            stream,
            started.elapsed().as_millis()
        );
        let status = response.status().as_u16();
        if status >= 400 {
            // Read the response body so we can classify it. `response.text()`
            // returns an empty string on non-UTF-8 bodies, which is fine —
            // the classifier returns None and the error stays unclassified
            // (terminal at the router).
            let body = response.text().await.unwrap_or_default();
            let response_bytes = body.len();
            let kind = canonical::classify_upstream_4xx(status, &body);
            return Err(canonical::make_provider_error_with_kind(
                "upstream_http_error",
                "response_status",
                Some(status),
                None,
                Some(response_bytes),
                kind,
            ));
        }
        Ok(response)
    }
}

// placeholder impl
#[async_trait]
impl Provider for OpenAIProvider {
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
        let body = self.request_body(req, upstream_model, false)?;
        let request_id = req.request_id.as_deref().unwrap_or("unknown");
        let res = self.send_body(&body, false, request_id).await?;
        let bytes = res.bytes().await.map_err(|error| {
            crate::proxy::canonical::make_provider_error(
                "upstream_transport_error",
                error_kind(&error),
                Some(502),
                Some(true),
                None,
            )
        })?;
        let resp: OAIChatResponse = serde_json::from_slice(&bytes).map_err(|_| {
            crate::proxy::canonical::make_provider_error(
                "upstream_decode_error",
                "response_decode",
                Some(502),
                Some(true),
                Some(bytes.len()),
            )
        })?;
        Ok(resp)
    }

    async fn stream(
        &self,
        req: &ChatCompletionRequest,
        upstream_model: &str,
    ) -> Result<Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>>, ProviderError> {
        let body = self.request_body(req, upstream_model, true)?;
        let provider_id = self.id.clone();
        // Eager send + status check: an upstream 4xx/5xx becomes a real
        // ProviderError with the status code, so the router can fail over
        // immediately (one RTT) instead of timing out on an empty stream.
        let request_id = req.request_id.clone().unwrap_or_else(|| "unknown".into());
        let res = self.send_body(&body, true, &request_id).await?;
        // `bytes_stream()` consumes the response by value, so build the byte
        // stream eagerly and move it into the generator (keeps `res` alive
        // for as long as the stream lives).
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
        let model = crate::proxy::strip_one_m_suffix(upstream_model);
        let stream = async_stream::stream! {
            let mut events = std::pin::pin!(SseDataStream::new(byte_stream));
            let mut received_event = false;
            let mut finished = false;
            while let Some(event) = events.next().await {
                let data = match event {
                    Ok(data) => data,
                    Err(_) => {
                        yield crate::proxy::canonical::stream_error_chunk(&model, 502, "proxy_stream_error");
                        return;
                    }
                };
                if data == "[DONE]" {
                    if !finished && received_event {
                        yield crate::proxy::canonical::stream_error_chunk(&model, 502, "proxy_stream_error");
                    }
                    return;
                }
                let value: serde_json::Value = match serde_json::from_str(&data) {
                    Ok(value) => value,
                    Err(_) => {
                        yield crate::proxy::canonical::stream_error_chunk(&model, 502, "proxy_stream_error");
                        return;
                    }
                };
                if value.get("error").is_some() {
                    let status = crate::proxy::canonical::stream_error_status(&value);
                    yield crate::proxy::canonical::stream_error_chunk(&model, status, "upstream_error");
                    return;
                }
                match serde_json::from_value::<OAIStreamChunk>(value) {
                    Ok(chunk) => {
                        received_event = true;
                        finished |= chunk.choices.iter().any(|choice| choice.finish_reason.is_some());
                        yield chunk;
                    }
                    Err(_) => {
                        yield crate::proxy::canonical::stream_error_chunk(&model, 502, "proxy_stream_error");
                        return;
                    }
                }
            }
            if received_event && !finished {
                yield crate::proxy::canonical::stream_error_chunk(&model, 502, "proxy_stream_error");
            }
        };
        Ok(Box::pin(stream))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::canonical::ChatMessage;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use axum::routing::post;
    use axum::Router;
    use serde_json::json;

    fn provider() -> OpenAIProvider {
        OpenAIProvider::new(
            "test".into(),
            "http://127.0.0.1:1/v1".into(),
            String::new(),
            vec!["model".into()],
            true,
            1_000,
            CircuitBreakerConfig::default(),
            Default::default(),
        )
        .unwrap()
    }

    fn request() -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "model".into(),
            messages: vec![ChatMessage {
                role: "user".into(),
                content: json!("hello"),
                tool_calls: None,
                tool_call_id: None,
                extra: Default::default(),
            }],
            stream: true,
            temperature: None,
            max_tokens: Some(32),
            tools: None,
            tool_choice: None,
            extra: Default::default(),
            source_anthropic_request: None,
            request_id: None,
            client_credential: None,
        }
    }

    #[test]
    fn streaming_body_requests_usage_without_overwriting_caller_value() {
        let provider = provider();
        let body = provider.request_body(&request(), "model", true).unwrap();
        assert_eq!(body["stream_options"]["include_usage"], true);

        let mut explicit = request();
        explicit.extra.insert(
            "stream_options".into(),
            json!({"include_usage": false, "custom": "keep"}),
        );
        let body = provider.request_body(&explicit, "gpt-5", true).unwrap();
        assert_eq!(body["stream_options"]["include_usage"], false);
        assert_eq!(body["stream_options"]["custom"], "keep");
    }

    /// The endpoint is resolved once at construction and never doubles the
    /// path for a full-endpoint base; the full case matrix (including the
    /// bare-root `/v1` append and Azure query preservation) lives in
    /// `proxy::endpoint::tests`.
    #[test]
    fn endpoint_is_resolved_once_at_construction() {
        let provider = OpenAIProvider::new(
            "test".into(),
            "https://host.test/v1/chat/completions".into(),
            String::new(),
            vec!["model".into()],
            true,
            1_000,
            CircuitBreakerConfig::default(),
            Default::default(),
        )
        .unwrap();
        assert_eq!(provider.endpoint(), "https://host.test/v1/chat/completions");
        assert!(OpenAIProvider::new(
            "test".into(),
            "not a url".into(),
            String::new(),
            vec!["model".into()],
            true,
            1_000,
            CircuitBreakerConfig::default(),
            Default::default(),
        )
        .is_err());
    }

    #[tokio::test]
    async fn upstream_error_body_and_private_identity_never_enter_provider_error() {
        async fn reject() -> impl IntoResponse {
            (
                StatusCode::BAD_GATEWAY,
                "CANARY_RESPONSE_UNIQUE_92 sk-private-93 private.person@example.invalid 100.64.12.34",
            )
        }
        let app = Router::new().route("/v1/chat/completions", post(reject));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let provider = OpenAIProvider::new(
            "provider-CANARY_PROMPT_UNIQUE_91".into(),
            format!("http://{address}/v1"),
            "sk-private-93".into(),
            vec!["model-CANARY_PROMPT_UNIQUE_91".into()],
            true,
            1_000,
            CircuitBreakerConfig::default(),
            Default::default(),
        )
        .unwrap();
        let error = provider
            .send_body(&serde_json::json!({}), false, "req_test")
            .await
            .expect_err("upstream rejection must fail");
        let rendered = format!("{error:?}");
        for private in [
            "CANARY_PROMPT_UNIQUE_91",
            "CANARY_RESPONSE_UNIQUE_92",
            "sk-private-93",
            "private.person@example.invalid",
            "100.64.12.34",
        ] {
            assert!(
                !rendered.contains(private),
                "private value survived: {rendered}"
            );
        }
        assert_eq!(error.code, "upstream_http_error");
        assert_eq!(error.stage, "response_status");
        assert!(error.response_bytes.unwrap_or(0) > 0);
        server.abort();
    }
}
