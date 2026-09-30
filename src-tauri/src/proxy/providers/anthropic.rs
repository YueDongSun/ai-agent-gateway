use crate::database::dao::proxy_providers::CircuitBreakerConfig;
use crate::proxy::canonical::{
    ChatCompletionRequest, Client_credential, OAIChatResponse, OAIStreamChunk, ProviderError,
};
use crate::proxy::circuit_breaker::CircuitBreaker;
use crate::proxy::converter::{
    anthropic_resp_to_oai_resp, anthropic_stream_to_oai_chunks, oai_req_to_anthropic_req,
    AnthropicResponse,
};
use crate::proxy::providers::http_client::{apply_extra_headers, build_http_client, error_kind};
use crate::proxy::providers::sse::SseDataStream;
use crate::proxy::providers::Provider;
use crate::proxy::thinking_compat::{
    rectify_anthropic_request, response_rejects_thinking_config, should_retry_without_thinking,
};
use async_trait::async_trait;
use futures_core::Stream;
use futures_util::StreamExt;
use std::pin::Pin;
use std::time::{Duration, Instant};

pub struct AnthropicProvider {
    id: String,
    endpoint: String,
    api_key: String,
    /// Subscription passthrough: relay the client's inbound
    /// `authorization` / `x-api-key` header verbatim instead of the stored
    /// static key. Never log, persist, or serialize the relayed value.
    passthrough: bool,
    /// Provider-mandated extra request headers (e.g. OpenCode's
    /// `x-opencode-session`), merged in `headers()` after the trusted set.
    /// Reserved headers are never overridden (see `apply_extra_headers`).
    extra_headers: std::collections::HashMap<String, String>,
    models: Vec<String>,
    enabled: bool,
    timeout: Duration,
    client: reqwest::Client,
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
        extra_headers: std::collections::HashMap<String, String>,
    ) -> Result<Self, String> {
        let client = build_http_client("Anthropic")?;
        // Resolve the request endpoint once at construction: the URL is fixed
        // for the provider's lifetime, and a malformed base surfaces here
        // (where the builder warn-logs it) instead of on the first request.
        let endpoint = crate::proxy::endpoint::endpoint_url(
            if base_url.is_empty() {
                "https://api.anthropic.com"
            } else {
                &base_url
            },
            crate::proxy::endpoint::Protocol::Anthropic,
        )
        .map_err(|error| format!("invalid anthropic base_url: {error}"))?;
        Ok(Self {
            id,
            endpoint,
            api_key,
            passthrough: false,
            extra_headers,
            models,
            enabled,
            timeout: Duration::from_millis(timeout_ms),
            client,
            breaker: CircuitBreaker::new(cb_cfg),
        })
    }

    /// Subscription-passthrough variant: no stored key; upstream auth is the
    /// client's inbound credential relayed verbatim (see `Client_credential`).
    /// `base_url` should point at the official Anthropic API
    /// (`https://api.anthropic.com`) — that is where a Claude Pro/Max
    /// subscription OAuth bearer is honored.
    pub fn new_passthrough(
        id: String,
        base_url: String,
        models: Vec<String>,
        enabled: bool,
        timeout_ms: u64,
        cb_cfg: CircuitBreakerConfig,
        extra_headers: std::collections::HashMap<String, String>,
    ) -> Result<Self, String> {
        let client = build_http_client("Anthropic")?;
        let endpoint = crate::proxy::endpoint::endpoint_url(
            if base_url.is_empty() {
                "https://api.anthropic.com"
            } else {
                &base_url
            },
            crate::proxy::endpoint::Protocol::Anthropic,
        )
        .map_err(|error| format!("invalid anthropic base_url: {error}"))?;
        Ok(Self {
            id,
            endpoint,
            api_key: String::new(),
            passthrough: true,
            extra_headers,
            models,
            enabled,
            timeout: Duration::from_millis(timeout_ms),
            client,
            breaker: CircuitBreaker::new(cb_cfg),
        })
    }

    /// Passthrough failover classification. A 401 from the upstream while a
    /// client credential was relayed means THIS client is not a subscription
    /// OAuth caller (e.g. a token-mode mirror sending the gateway token):
    /// the hop cannot serve the request, but the route may still have
    /// static-key hops behind it. Returning `Some(true)` lets the router
    /// fail over instead of terminating the whole route; without a relayed
    /// credential the default 4xx classification applies unchanged.
    fn passthrough_auth_retriable(
        &self,
        status: u16,
        client_credential: Option<&Client_credential>,
    ) -> Option<bool> {
        if self.passthrough && status == 401 && client_credential.is_some() {
            Some(true)
        } else {
            None
        }
    }

    fn endpoint(&self) -> &str {
        &self.endpoint
    }

    fn headers(&self, client_credential: Option<&Client_credential>) -> reqwest::header::HeaderMap {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert("content-type", "application/json".parse().unwrap());
        h.insert("anthropic-version", "2023-06-01".parse().unwrap());
        if self.passthrough {
            // Relay the client's inbound credential verbatim (authorization
            // first — the OAuth bearer a subscription client sends — then
            // x-api-key). Both absent: send neither and let the upstream 401
            // classify as AuthExpired (terminal, breaker-visible).
            if let Some(credential) = client_credential {
                if let Some(authorization) = credential.authorization.as_deref() {
                    h.insert("authorization", authorization.parse().unwrap());
                } else if let Some(x_api_key) = credential.x_api_key.as_deref() {
                    h.insert("x-api-key", x_api_key.parse().unwrap());
                }
            }
        } else if !self.api_key.is_empty() {
            h.insert("x-api-key", self.api_key.parse().unwrap());
        }
        // Provider-mandated extras (e.g. x-opencode-session) merge last;
        // reserved auth/content headers are protected inside the helper.
        apply_extra_headers(&mut h, &self.extra_headers);
        h
    }

    async fn send_once(
        &self,
        body: &crate::proxy::converter::AnthropicRequest,
        stream: bool,
        request_id: &str,
        client_credential: Option<&Client_credential>,
    ) -> Result<reqwest::Response, ProviderError> {
        let started = Instant::now();
        let mut request = self
            .client
            .post(self.endpoint())
            .headers(self.headers(client_credential))
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
            "provider: headers request_id={} provider={} protocol=anthropic stream={} elapsed_ms={}",
            request_id,
            crate::diagnostics::safe_reference(&self.id),
            stream,
            started.elapsed().as_millis()
        );
        Ok(response)
    }

    async fn send_with_thinking_compatibility(
        &self,
        body: &crate::proxy::converter::AnthropicRequest,
        stream: bool,
        request_id: &str,
        client_credential: Option<&Client_credential>,
    ) -> Result<reqwest::Response, ProviderError> {
        let response = self
            .send_once(body, stream, request_id, client_credential)
            .await?;
        let status = response.status().as_u16();
        if status < 400 {
            return Ok(response);
        }
        let response_text = response.text().await.unwrap_or_default();
        let response_bytes = response_text.len();
        let kind = crate::proxy::canonical::classify_upstream_4xx(status, &response_text);
        if should_retry_without_thinking(status, &response_text) {
            let mut retry_body = body.clone();
            let rectification = rectify_anthropic_request(
                &mut retry_body,
                response_rejects_thinking_config(&response_text),
            );
            if rectification.removed_thinking_blocks == 0
                && rectification.removed_signature_fields == 0
                && !rectification.removed_thinking_config
            {
                return Err(crate::proxy::canonical::make_provider_error_with_kind(
                    "upstream_http_error",
                    "response_status",
                    Some(status),
                    self.passthrough_auth_retriable(status, client_credential),
                    Some(response_bytes),
                    kind,
                ));
            }
            log::warn!(
                "provider: thinking compatibility retry request_id={} provider={} status={} removed_blocks={} removed_signatures={} removed_config={}",
                request_id,
                crate::diagnostics::safe_reference(&self.id),
                status,
                rectification.removed_thinking_blocks,
                rectification.removed_signature_fields,
                rectification.removed_thinking_config
            );
            let retry_response = self
                .send_once(&retry_body, stream, request_id, client_credential)
                .await?;
            let retry_status = retry_response.status().as_u16();
            if retry_status < 400 {
                return Ok(retry_response);
            }
            let retry_text = retry_response.text().await.unwrap_or_default();
            let retry_response_bytes = retry_text.len();
            let retry_kind =
                crate::proxy::canonical::classify_upstream_4xx(retry_status, &retry_text);
            return Err(crate::proxy::canonical::make_provider_error_with_kind(
                "upstream_http_error",
                "compatibility_retry_status",
                Some(retry_status),
                self.passthrough_auth_retriable(retry_status, client_credential),
                Some(retry_response_bytes),
                retry_kind,
            ));
        }
        Err(crate::proxy::canonical::make_provider_error_with_kind(
            "upstream_http_error",
            "response_status",
            Some(status),
            self.passthrough_auth_retriable(status, client_credential),
            Some(response_bytes),
            kind,
        ))
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
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
    fn requires_client_credential(&self) -> bool {
        self.passthrough
    }

    async fn complete(
        &self,
        req: &ChatCompletionRequest,
        upstream_model: &str,
    ) -> Result<OAIChatResponse, ProviderError> {
        let mut body = oai_req_to_anthropic_req(req);
        // The route hop's upstream model wins over the client's requested
        // name. Defensive: the client may send `model: claude-fable-5[1M]`
        // even when the route hop is unannotated. Strip the local 1M-context
        // marker before forwarding — upstream endpoints reject the suffix.
        body.model = crate::proxy::strip_one_m_suffix(upstream_model);
        body.stream = Some(false);
        let request_id = req.request_id.as_deref().unwrap_or("unknown");
        let res = self
            .send_with_thinking_compatibility(
                &body,
                false,
                request_id,
                req.client_credential.as_ref(),
            )
            .await?;
        let bytes = res.bytes().await.map_err(|error| {
            crate::proxy::canonical::make_provider_error(
                "upstream_transport_error",
                error_kind(&error),
                Some(502),
                Some(true),
                None,
            )
        })?;
        let source_response: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| {
            crate::proxy::canonical::make_provider_error(
                "upstream_decode_error",
                "response_decode",
                Some(502),
                Some(true),
                Some(bytes.len()),
            )
        })?;
        let resp: AnthropicResponse =
            serde_json::from_value(source_response.clone()).map_err(|_| {
                crate::proxy::canonical::make_provider_error(
                    "upstream_decode_error",
                    "response_shape",
                    Some(502),
                    Some(true),
                    Some(bytes.len()),
                )
            })?;
        let mut canonical = anthropic_resp_to_oai_resp(&resp, upstream_model);
        canonical.source_anthropic_response = Some(source_response);
        Ok(canonical)
    }

    async fn stream(
        &self,
        req: &ChatCompletionRequest,
        upstream_model: &str,
    ) -> Result<Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>>, ProviderError> {
        // Streaming requests get a near-unbounded total timeout (the response
        // body may stream for many minutes); the pre-stream phase is bounded
        // by the Router's first-byte timeout instead, so a dead upstream
        // can't hold the chain hostage. Matches cc-switch's 24h stream cap.
        let mut body = oai_req_to_anthropic_req(req);
        body.stream = Some(true);
        // The route hop's upstream model wins; strip the local `[1M]` marker
        // (upstream endpoints reject it — see `complete`).
        let model = crate::proxy::strip_one_m_suffix(upstream_model);
        body.model = model.clone();
        let provider_id = self.id.clone();
        // Eager send + status check: an upstream 4xx/5xx becomes a real
        // ProviderError with the status code, so the router can fail over
        // immediately (one RTT) instead of timing out on an empty stream.
        let request_id = req.request_id.clone().unwrap_or_else(|| "unknown".into());
        let res = self
            .send_with_thinking_compatibility(
                &body,
                true,
                &request_id,
                req.client_credential.as_ref(),
            )
            .await?;
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
        let stream = async_stream::stream! {
            let sse_stream = SseDataStream::new(byte_stream).map(|event| {
                event.unwrap_or_else(|_| serde_json::json!({
                    "type": "error",
                    "error": {"type": "proxy_stream_error", "message": "Upstream stream transport failed"}
                }).to_string())
            });
            let oai_chunks = anthropic_stream_to_oai_chunks(sse_stream, model.clone());
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
    use crate::proxy::converter::{anthropic_req_to_oai_req, AnthropicMessage, AnthropicRequest};
    use axum::extract::State;
    use axum::response::{IntoResponse, Response};
    use axum::routing::post;
    use axum::{Json, Router};
    use serde_json::{json, Value};
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    #[allow(non_camel_case_types)]
    struct Retry_state {
        bodies: Arc<Mutex<Vec<Value>>>,
    }

    async fn signature_retry_handler(
        State(state): State<Retry_state>,
        Json(body): Json<Value>,
    ) -> Response {
        let request_index = {
            let mut bodies = state.bodies.lock().unwrap();
            bodies.push(body);
            bodies.len()
        };
        if request_index == 1 {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(json!({"error": {"message": "invalid signature"}})),
            )
                .into_response();
        }
        Json(json!({
            "id": "msg_retry",
            "type": "message",
            "role": "assistant",
            "model": "model",
            "content": [{"type": "text", "text": "ok"}],
            "stop_reason": "end_turn",
            "stop_sequence": null,
            "usage": {"input_tokens": 3, "output_tokens": 1}
        }))
        .into_response()
    }

    #[tokio::test]
    async fn explicit_signature_error_retries_once_with_rectified_history() {
        let state = Retry_state::default();
        let app = Router::new()
            .route("/v1/messages", post(signature_retry_handler))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let provider = AnthropicProvider::new(
            "provider".into(),
            format!("http://{address}"),
            String::new(),
            vec!["model".into()],
            true,
            2_000,
            CircuitBreakerConfig::default(),
            Default::default(),
        )
        .unwrap();
        let source = AnthropicRequest {
            model: "model".into(),
            max_tokens: 2048,
            system: None,
            messages: vec![AnthropicMessage {
                role: "assistant".into(),
                content: json!([
                    {"type": "thinking", "thinking": "summary", "signature": "opaque"},
                    {"type": "text", "text": "continue"}
                ]),
            }],
            temperature: None,
            stream: Some(false),
            stop_sequences: None,
            extra: [(
                "thinking".into(),
                json!({"type": "enabled", "budget_tokens": 1024}),
            )]
            .into_iter()
            .collect(),
        };
        let mut request = anthropic_req_to_oai_req(&source);
        request.request_id = Some("req_test".into());
        let response = provider.complete(&request, &request.model).await.unwrap();
        assert_eq!(response.choices[0].message.content.as_deref(), Some("ok"));

        let bodies = state.bodies.lock().unwrap();
        assert_eq!(bodies.len(), 2);
        assert_eq!(bodies[0]["messages"][0]["content"][0]["type"], "thinking");
        assert_eq!(bodies[1]["messages"][0]["content"][0]["type"], "text");
        assert_eq!(bodies[1]["thinking"]["budget_tokens"], 1024);
        server.abort();
    }

    #[tokio::test]
    async fn terminal_upstream_error_retains_only_status_and_body_size() {
        async fn reject() -> Response {
            (
                axum::http::StatusCode::BAD_GATEWAY,
                "CANARY_RESPONSE_UNIQUE_92 sk-private-93 private.person@example.invalid",
            )
                .into_response()
        }
        let app = Router::new().route("/v1/messages", post(reject));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let provider = AnthropicProvider::new(
            "provider-CANARY_PROMPT_UNIQUE_91".into(),
            format!("http://{address}"),
            "sk-private-93".into(),
            vec!["model".into()],
            true,
            2_000,
            CircuitBreakerConfig::default(),
            Default::default(),
        )
        .unwrap();
        let body = AnthropicRequest {
            model: "model".into(),
            max_tokens: 32,
            system: None,
            messages: vec![AnthropicMessage {
                role: "user".into(),
                content: json!("CANARY_PROMPT_UNIQUE_91"),
            }],
            temperature: None,
            stream: Some(false),
            stop_sequences: None,
            extra: Default::default(),
        };
        let error = provider
            .send_with_thinking_compatibility(&body, false, "req_test", None)
            .await
            .expect_err("upstream rejection must fail");
        let rendered = format!("{error:?}");
        for private in [
            "CANARY_PROMPT_UNIQUE_91",
            "CANARY_RESPONSE_UNIQUE_92",
            "sk-private-93",
            "private.person@example.invalid",
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

    /// The endpoint is resolved once at construction and cached. A base that
    /// already points at the full `/v1/messages` endpoint must be used as-is
    /// (the QwenTok token-plan regression); the full case matrix lives in
    /// `proxy::endpoint::tests`.
    #[test]
    fn endpoint_is_resolved_once_at_construction() {
        let provider = AnthropicProvider::new(
            "p".into(),
            "https://token-plan.cn-beijing.maas.aliyuncs.com/apps/anthropic/v1/messages".into(),
            String::new(),
            vec!["m".into()],
            true,
            1_000,
            CircuitBreakerConfig::default(),
            Default::default(),
        )
        .unwrap();
        assert_eq!(
            provider.endpoint(),
            "https://token-plan.cn-beijing.maas.aliyuncs.com/apps/anthropic/v1/messages"
        );
        // A malformed base fails at construction, not on first request.
        assert!(AnthropicProvider::new(
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

    /// Passthrough: the client's inbound credential is relayed verbatim
    /// (authorization preferred, x-api-key fallback), and the stored static
    /// key path is byte-identical when a credential is present on a
    /// non-passthrough provider.
    #[derive(Clone, Default)]
    #[allow(non_camel_case_types)]
    struct Header_capture_state {
        authorization: Arc<Mutex<Vec<Option<String>>>>,
        x_api_key: Arc<Mutex<Vec<Option<String>>>>,
    }

    async fn header_capture_handler(
        State(state): State<Header_capture_state>,
        headers: axum::http::HeaderMap,
    ) -> Response {
        state.authorization.lock().unwrap().push(
            headers
                .get("authorization")
                .and_then(|v| v.to_str().ok().map(String::from)),
        );
        state.x_api_key.lock().unwrap().push(
            headers
                .get("x-api-key")
                .and_then(|v| v.to_str().ok().map(String::from)),
        );
        Json(json!({
            "id": "msg_capture",
            "type": "message",
            "role": "assistant",
            "model": "model",
            "content": [{"type": "text", "text": "ok"}],
            "stop_reason": "end_turn",
            "stop_sequence": null,
            "usage": {"input_tokens": 3, "output_tokens": 1}
        }))
        .into_response()
    }

    async fn passthrough_fixture() -> (
        Header_capture_state,
        std::net::SocketAddr,
        tokio::task::JoinHandle<()>,
    ) {
        let state = Header_capture_state::default();
        let app = Router::new()
            .route("/v1/messages", post(header_capture_handler))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (state, address, server)
    }

    fn passthrough_request(
        authorization: Option<&str>,
        x_api_key: Option<&str>,
    ) -> ChatCompletionRequest {
        let mut request = anthropic_req_to_oai_req(&AnthropicRequest {
            model: "model".into(),
            max_tokens: 32,
            system: None,
            messages: vec![AnthropicMessage {
                role: "user".into(),
                content: json!("passthrough probe"),
            }],
            temperature: None,
            stream: Some(false),
            stop_sequences: None,
            extra: Default::default(),
        });
        request.request_id = Some("req_passthrough".into());
        request.client_credential = Some(Client_credential {
            authorization: authorization.map(String::from),
            x_api_key: x_api_key.map(String::from),
        });
        request
    }

    #[tokio::test]
    async fn passthrough_relays_client_authorization_verbatim() {
        let (state, address, server) = passthrough_fixture().await;
        let provider = AnthropicProvider::new_passthrough(
            "sub".into(),
            format!("http://{address}"),
            vec!["model".into()],
            true,
            2_000,
            CircuitBreakerConfig::default(),
            Default::default(),
        )
        .unwrap();
        let request = passthrough_request(Some("Bearer oauth-relay-91"), None);
        let response = provider.complete(&request, &request.model).await.unwrap();
        assert_eq!(response.choices[0].message.content.as_deref(), Some("ok"));
        let authorization = state.authorization.lock().unwrap();
        let x_api_key = state.x_api_key.lock().unwrap();
        assert_eq!(authorization[0].as_deref(), Some("Bearer oauth-relay-91"));
        assert!(x_api_key[0].is_none(), "x-api-key must not be set");
        server.abort();
    }

    #[tokio::test]
    async fn passthrough_relays_x_api_key_when_authorization_absent() {
        let (state, address, server) = passthrough_fixture().await;
        let provider = AnthropicProvider::new_passthrough(
            "sub".into(),
            format!("http://{address}"),
            vec!["model".into()],
            true,
            2_000,
            CircuitBreakerConfig::default(),
            Default::default(),
        )
        .unwrap();
        let request = passthrough_request(None, Some("sk-client-relay-42"));
        provider.complete(&request, &request.model).await.unwrap();
        let authorization = state.authorization.lock().unwrap();
        let x_api_key = state.x_api_key.lock().unwrap();
        assert!(authorization[0].is_none());
        assert_eq!(x_api_key[0].as_deref(), Some("sk-client-relay-42"));
        server.abort();
    }

    #[tokio::test]
    async fn static_key_provider_ignores_client_credential() {
        let (state, address, server) = passthrough_fixture().await;
        let provider = AnthropicProvider::new(
            "pool".into(),
            format!("http://{address}"),
            "sk-static-pool-7".into(),
            vec!["model".into()],
            true,
            2_000,
            CircuitBreakerConfig::default(),
            Default::default(),
        )
        .unwrap();
        let request = passthrough_request(Some("Bearer oauth-relay-91"), None);
        provider.complete(&request, &request.model).await.unwrap();
        let authorization = state.authorization.lock().unwrap();
        let x_api_key = state.x_api_key.lock().unwrap();
        assert!(
            authorization[0].is_none(),
            "a static-key provider must never relay the client credential"
        );
        assert_eq!(x_api_key[0].as_deref(), Some("sk-static-pool-7"));
        server.abort();
    }

    async fn unauthorized_handler() -> Response {
        (
            axum::http::StatusCode::UNAUTHORIZED,
            Json(json!({
                "type": "error",
                "error": {"type": "authentication_error", "message": "invalid x-api-key"}
            })),
        )
            .into_response()
    }

    async fn passthrough_401_fixture() -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
        let app = Router::new().route(
            "/v1/messages",
            post(|| async { unauthorized_handler().await }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (address, server)
    }

    /// A passthrough hop that relays a credential and still gets a 401 is a
    /// "this client is not a subscription OAuth caller" verdict — the hop
    /// can't serve, but the route may have static-key hops behind it, so
    /// the error must be retriable for failover.
    #[tokio::test]
    async fn passthrough_401_with_relayed_credential_is_retriable() {
        let (address, server) = passthrough_401_fixture().await;
        let provider = AnthropicProvider::new_passthrough(
            "sub".into(),
            format!("http://{address}"),
            vec!["model".into()],
            true,
            2_000,
            CircuitBreakerConfig::default(),
            Default::default(),
        )
        .unwrap();
        let request = passthrough_request(Some("Bearer not-an-oauth-token"), None);
        let error = provider
            .complete(&request, &request.model)
            .await
            .expect_err("upstream 401 must surface as an error");
        assert_eq!(error.status, Some(401));
        assert!(
            error.retriable,
            "passthrough 401 with a relayed credential must fail over to later hops"
        );
        server.abort();
    }

    /// Without a relayed credential the default 4xx classification must be
    /// unchanged (terminal) — the router is responsible for skipping such
    /// hops before dispatch, not the provider for masking real auth errors.
    #[tokio::test]
    async fn passthrough_401_without_credential_stays_terminal() {
        let (address, server) = passthrough_401_fixture().await;
        let provider = AnthropicProvider::new_passthrough(
            "sub".into(),
            format!("http://{address}"),
            vec!["model".into()],
            true,
            2_000,
            CircuitBreakerConfig::default(),
            Default::default(),
        )
        .unwrap();
        let mut request = passthrough_request(None, None);
        request.client_credential = None;
        let error = provider
            .complete(&request, &request.model)
            .await
            .expect_err("upstream 401 must surface as an error");
        assert_eq!(error.status, Some(401));
        assert!(
            !error.retriable,
            "401 without a relayed credential keeps the default terminal classification"
        );
        server.abort();
    }

    /// A static-key provider's 401 means the stored key is dead — that must
    /// stay terminal so the breaker can expire the credential rather than
    /// looping the route.
    #[tokio::test]
    async fn static_key_401_stays_terminal() {
        let (address, server) = passthrough_401_fixture().await;
        let provider = AnthropicProvider::new(
            "pool".into(),
            format!("http://{address}"),
            "sk-dead-key".into(),
            vec!["model".into()],
            true,
            2_000,
            CircuitBreakerConfig::default(),
            Default::default(),
        )
        .unwrap();
        let request = passthrough_request(Some("Bearer oauth-relay-91"), None);
        let error = provider
            .complete(&request, &request.model)
            .await
            .expect_err("upstream 401 must surface as an error");
        assert_eq!(error.status, Some(401));
        assert!(
            !error.retriable,
            "static-key 401 must stay terminal regardless of any client credential"
        );
        server.abort();
    }
}
