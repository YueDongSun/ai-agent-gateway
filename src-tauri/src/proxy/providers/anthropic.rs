use crate::database::dao::proxy_providers::CircuitBreakerConfig;
use crate::proxy::canonical::{
    ChatCompletionRequest, Client_credential, OAIChatResponse, OAIStreamChunk, ProviderError,
};
use crate::proxy::circuit_breaker::CircuitBreaker;
use crate::proxy::converter::{
    anthropic_resp_to_oai_resp, anthropic_stream_to_oai_chunks, oai_req_to_anthropic_req,
    AnthropicResponse,
};
use crate::proxy::providers::http_client::{
    apply_extra_headers, build_http_client, error_kind, read_captured_error_body,
};
use crate::proxy::providers::sse::SseDataStream;
use crate::proxy::providers::{AnthropicAuthScheme, Provider};
use crate::proxy::routing_receipts::contract::{CredentialMode, ModelSource, TerminalSignal};
use crate::proxy::routing_receipts::{ProviderObservation, WireAttempt, WireResponse};
use crate::proxy::thinking_compat::{
    rectify_anthropic_request, response_rejects_thinking_config, should_retry_without_thinking,
};
use async_trait::async_trait;
use futures_core::Stream;
use futures_util::StreamExt;
use serde::de::{IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub struct AnthropicProvider {
    id: String,
    endpoint: String,
    api_key: String,
    auth_scheme: AnthropicAuthScheme,
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
    response_byte_limit: Option<usize>,
    probe_response: crate::proxy::providers::http_client::ProbeResponseCapture,
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
            auth_scheme: AnthropicAuthScheme::default(),
            passthrough: false,
            extra_headers,
            models,
            enabled,
            timeout: Duration::from_millis(timeout_ms),
            client,
            breaker: CircuitBreaker::new(cb_cfg),
            response_byte_limit: None,
            probe_response: Default::default(),
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
            auth_scheme: AnthropicAuthScheme::default(),
            passthrough: true,
            extra_headers,
            models,
            enabled,
            timeout: Duration::from_millis(timeout_ms),
            client,
            breaker: CircuitBreaker::new(cb_cfg),
            response_byte_limit: None,
            probe_response: Default::default(),
        })
    }

    /// Select static authentication without changing subscription passthrough.
    pub fn with_auth_scheme(mut self, auth_scheme: AnthropicAuthScheme) -> Self {
        self.auth_scheme = auth_scheme;
        self
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
                // The client's beta-flag list rides along so a subscription
                // login keeps its negotiated features (context management,
                // token-efficient tools, …) through the relay.
                if let Some(beta) = credential.anthropic_beta.as_deref() {
                    if let Ok(value) = beta.parse() {
                        h.insert("anthropic-beta", value);
                    }
                }
            }
        } else if !self.api_key.is_empty() {
            match self.auth_scheme {
                AnthropicAuthScheme::ApiKey => {
                    h.insert("x-api-key", self.api_key.parse().unwrap());
                }
                AnthropicAuthScheme::Bearer => {
                    h.insert(
                        "authorization",
                        format!("Bearer {}", self.api_key).parse().unwrap(),
                    );
                }
            }
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
        observation: &ProviderObservation,
    ) -> Result<WireResponse, ProviderError> {
        let started = Instant::now();
        let headers = self.headers(client_credential);
        let credential_mode = if self.passthrough {
            if headers.contains_key("authorization") || headers.contains_key("x-api-key") {
                CredentialMode::ClientPassthrough
            } else {
                CredentialMode::None
            }
        } else if self.api_key.is_empty() {
            CredentialMode::None
        } else {
            CredentialMode::StoredKey
        };
        let mut request = self
            .client
            .post(self.endpoint())
            .headers(headers)
            .json(body);
        if !stream {
            request = request.timeout(self.timeout);
        }
        let request = request.build().map_err(|error| {
            crate::proxy::canonical::make_provider_error(
                "upstream_transport_error",
                error_kind(&error),
                None,
                Some(true),
                None,
            )
        })?;
        #[cfg(test)]
        crate::canary::before_upstream_send()?;
        let mut attempt = observation.begin_wire(&body.model, credential_mode)?;
        let send = self.client.execute(request);
        let response = if stream {
            tokio::time::timeout(self.timeout, send)
                .await
                .map_err(|_| {
                    attempt.failed("upstream_timeout");
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
            attempt.failed("upstream_transport_error");
            crate::proxy::canonical::make_provider_error(
                "upstream_transport_error",
                error_kind(&error),
                None,
                Some(true),
                None,
            )
        })?;
        attempt.response_status(response.status().as_u16());
        log::debug!(
            "provider: headers request_id={} provider={} protocol=anthropic stream={} elapsed_ms={}",
            request_id,
            crate::diagnostics::safe_reference(&self.id),
            stream,
            started.elapsed().as_millis()
        );
        Ok(WireResponse { response, attempt })
    }

    async fn send_with_thinking_compatibility(
        &self,
        body: &crate::proxy::converter::AnthropicRequest,
        stream: bool,
        request_id: &str,
        client_credential: Option<&Client_credential>,
        observation: &ProviderObservation,
    ) -> Result<WireResponse, ProviderError> {
        let WireResponse {
            response,
            mut attempt,
        } = self
            .send_once(body, stream, request_id, client_credential, observation)
            .await?;
        let status = response.status().as_u16();
        if status < 400 {
            return Ok(WireResponse { response, attempt });
        }
        attempt.failed("upstream_http_error");
        // Bounded error read: classification only needs the capped window,
        // and the inbound request-size limit does not apply to upstream
        // responses, so a huge error body must never be buffered whole.
        let (response_text, response_bytes) = read_captured_error_body(
            response,
            self.response_byte_limit,
            &self.probe_response,
            &self.api_key,
            &self.extra_headers,
        )
        .await;
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
                return Err(upstream_typed_error(
                    "upstream_http_error",
                    "response_status",
                    Some(status),
                    self.passthrough_auth_retriable(status, client_credential)
                        .or_else(|| {
                            crate::proxy::canonical::upstream_quota_retriable(
                                status,
                                &response_text,
                            )
                        }),
                    Some(response_bytes),
                    kind,
                    &response_text,
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
            let WireResponse {
                response: retry_response,
                attempt: mut retry_attempt,
            } = self
                .send_once(
                    &retry_body,
                    stream,
                    request_id,
                    client_credential,
                    observation,
                )
                .await?;
            let retry_status = retry_response.status().as_u16();
            if retry_status < 400 {
                return Ok(WireResponse {
                    response: retry_response,
                    attempt: retry_attempt,
                });
            }
            retry_attempt.failed("upstream_http_error");
            let (retry_text, retry_response_bytes) = read_captured_error_body(
                retry_response,
                self.response_byte_limit,
                &self.probe_response,
                &self.api_key,
                &self.extra_headers,
            )
            .await;
            let retry_kind =
                crate::proxy::canonical::classify_upstream_4xx(retry_status, &retry_text);
            return Err(upstream_typed_error(
                "upstream_http_error",
                "compatibility_retry_status",
                Some(retry_status),
                self.passthrough_auth_retriable(retry_status, client_credential)
                    .or_else(|| {
                        crate::proxy::canonical::upstream_quota_retriable(retry_status, &retry_text)
                    }),
                Some(retry_response_bytes),
                retry_kind,
                &retry_text,
            ));
        }
        Err(upstream_typed_error(
            "upstream_http_error",
            "response_status",
            Some(status),
            self.passthrough_auth_retriable(status, client_credential)
                .or_else(|| {
                    crate::proxy::canonical::upstream_quota_retriable(status, &response_text)
                }),
            Some(response_bytes),
            kind,
            &response_text,
        ))
    }
}

/// Attach the upstream's own allowlisted Anthropic error type to a provider
/// error so the Claude surface can relay it verbatim; the body text itself
/// is never carried beyond this classification.
fn upstream_typed_error(
    code: &str,
    stage: &str,
    status: Option<u16>,
    retriable: Option<bool>,
    response_bytes: Option<usize>,
    kind: Option<crate::proxy::canonical::UpstreamErrorKind>,
    body: &str,
) -> crate::proxy::canonical::ProviderError {
    let mut error = crate::proxy::canonical::make_provider_error_with_kind(
        code, stage, status, retriable, response_bytes, kind,
    );
    error.upstream_error_type =
        crate::proxy::canonical::anthropic_upstream_error_type(body);
    error
}

#[async_trait]
impl Provider for AnthropicProvider {
    fn take_probe_response(&self) -> Option<String> {
        self.probe_response.take()
    }
    fn probe_response_flags(&self) -> (bool, bool) {
        self.probe_response.flags()
    }
    fn set_response_byte_limit(&mut self, limit: usize) -> bool {
        if limit == 0 {
            return false;
        }
        self.response_byte_limit = Some(limit);
        true
    }

    fn wire_protocol(&self) -> &'static str {
        "anthropic"
    }

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

    /// Exact upstream count: relay the Anthropic-shaped body to the
    /// provider's `messages/count_tokens` endpoint. Passthrough relays the
    /// client credential (and its beta-flag header) verbatim; the static-key
    /// path uses the stored key.
    async fn count_tokens(
        &self,
        mut body: serde_json::Value,
        client_credential: Option<&Client_credential>,
    ) -> Result<u32, ProviderError> {
        let url =
            crate::proxy::endpoint::anthropic_count_tokens_url(&self.endpoint).map_err(|_| {
                crate::proxy::canonical::make_provider_error(
                    "count_tokens_endpoint_invalid",
                    "request_construction",
                    None,
                    Some(false),
                    None,
                )
            })?;
        // The body arrives owned (the router's per-hop clone is the only
        // whole-body copy), so normalize the model in place instead of
        // cloning again. The `[1M]` strip must stay: hop.model can carry the
        // marker via fallback chains and upstream endpoints reject it.
        if let Some(model) = body.get_mut("model") {
            if let Some(model_name) = model.as_str() {
                *model = serde_json::Value::String(crate::proxy::strip_one_m_suffix(model_name));
            }
        }
        let response = self
            .client
            .post(url)
            .headers(self.headers(client_credential))
            .timeout(self.timeout)
            .json(&body)
            .send()
            .await
            .map_err(|error| {
                crate::proxy::canonical::make_provider_error(
                    "upstream_transport_error",
                    error_kind(&error),
                    Some(502),
                    Some(true),
                    None,
                )
            })?;
        let status = response.status().as_u16();
        if status >= 400 {
            let (response_text, response_bytes) = read_captured_error_body(
                response,
                self.response_byte_limit,
                &self.probe_response,
                &self.api_key,
                &self.extra_headers,
            )
            .await;
            let retriable = self
                .passthrough_auth_retriable(status, client_credential)
                .or_else(|| {
                    crate::proxy::canonical::upstream_quota_retriable(status, &response_text)
                })
                .unwrap_or_else(|| (500..600).contains(&status) || status == 429);
            return Err(upstream_typed_error(
                "upstream_error",
                "upstream_status",
                Some(status),
                Some(retriable),
                Some(response_bytes),
                None,
                &response_text,
            ));
        }
        let bytes = response.bytes().await.map_err(|error| {
            crate::proxy::canonical::make_provider_error(
                "upstream_transport_error",
                error_kind(&error),
                Some(502),
                Some(true),
                None,
            )
        })?;
        serde_json::from_slice::<serde_json::Value>(&bytes)
            .ok()
            .and_then(|value| value.get("input_tokens").and_then(|t| t.as_u64()))
            .map(|tokens| tokens as u32)
            .ok_or_else(|| {
                crate::proxy::canonical::make_provider_error(
                    "upstream_decode_error",
                    "response_shape",
                    Some(502),
                    Some(true),
                    Some(bytes.len()),
                )
            })
    }

    async fn complete(
        &self,
        req: &ChatCompletionRequest,
        upstream_model: &str,
    ) -> Result<OAIChatResponse, ProviderError> {
        self.complete_observed(req, upstream_model, ProviderObservation::default())
            .await
    }

    async fn complete_observed(
        &self,
        req: &ChatCompletionRequest,
        upstream_model: &str,
        observation: ProviderObservation,
    ) -> Result<OAIChatResponse, ProviderError> {
        if self.response_byte_limit.is_some() {
            self.probe_response.clear();
        }
        let mut body = oai_req_to_anthropic_req(req)?;
        // The route hop's upstream model wins over the client's requested
        // name. Defensive: the client may send `model: claude-fable-5[1M]`
        // even when the route hop is unannotated. Strip the local 1M-context
        // marker before forwarding — upstream endpoints reject the suffix.
        body.model = crate::proxy::strip_one_m_suffix(upstream_model);
        body.stream = Some(false);
        let request_id = req.request_id.as_deref().unwrap_or("unknown");
        let WireResponse {
            response,
            mut attempt,
        } = self
            .send_with_thinking_compatibility(
                &body,
                false,
                request_id,
                req.client_credential.as_ref(),
                &observation,
            )
            .await?;
        let bytes = crate::proxy::providers::http_client::read_completion_body(
            response,
            self.response_byte_limit,
        )
        .await
        .map_err(|error| {
            attempt.failed(&error.code);
            error
        })?;
        self.probe_response.record(
            &bytes,
            self.response_byte_limit,
            &self.api_key,
            &self.extra_headers,
            false,
        );
        let source_response: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| {
            attempt.failed("upstream_decode_error");
            crate::proxy::canonical::make_provider_error(
                "upstream_decode_error",
                "response_decode",
                Some(502),
                Some(true),
                Some(bytes.len()),
            )
        })?;
        if attempt.enabled() {
            if let Some(model) = source_response.get("model") {
                match model.as_str() {
                    Some(model) => attempt.reported_model(ModelSource::MessageModel, Some(model)),
                    None => attempt.rejected_model(ModelSource::MessageModel),
                }
            }
        }
        // Borrowed parse: same semantics as from_value on a clone, without
        // duplicating the response DOM (the DOM itself is retained below for
        // the same-protocol relay path).
        let resp: AnthropicResponse =
            AnthropicResponse::deserialize(&source_response).map_err(|_| {
                attempt.failed("upstream_decode_error");
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
        attempt.response_parsed();
        Ok(canonical)
    }

    async fn stream(
        &self,
        req: &ChatCompletionRequest,
        upstream_model: &str,
    ) -> Result<Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>>, ProviderError> {
        self.stream_observed(req, upstream_model, ProviderObservation::default())
            .await
    }

    async fn stream_observed(
        &self,
        req: &ChatCompletionRequest,
        upstream_model: &str,
        observation: ProviderObservation,
    ) -> Result<Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>>, ProviderError> {
        // Streaming requests get a near-unbounded total timeout (the response
        // body may stream for many minutes); the pre-stream phase is bounded
        // by the Router's first-byte timeout instead, so a dead upstream
        // can't hold the chain hostage. Matches cc-switch's 24h stream cap.
        let mut body = oai_req_to_anthropic_req(req)?;
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
        let WireResponse { response, attempt } = self
            .send_with_thinking_compatibility(
                &body,
                true,
                &request_id,
                req.client_credential.as_ref(),
                &observation,
            )
            .await?;
        // `bytes_stream()` consumes the response by value, so build the byte
        // stream eagerly and move it into the generator (keeps `res` alive
        // for as long as the stream lives).
        let byte_stream = response.bytes_stream().map(move |r| {
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
            let capture = Arc::new(Mutex::new(Stream_observation {
                attempt,
                failed: false,
                ended: false,
            }));
            let tap = Arc::clone(&capture);
            let sse_stream = async_stream::stream! {
                let mut events = std::pin::pin!(SseDataStream::new(byte_stream));
                while let Some(event) = events.next().await {
                    let data = match event {
                        Ok(data) => {
                            tap.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
                                .observe_event(&data);
                            data
                        }
                        Err(_) => {
                            tap.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
                                .fail("proxy_stream_error");
                            serde_json::json!({
                                "type": "error",
                                "error": {"type": "proxy_stream_error", "message": "Upstream stream transport failed"}
                            }).to_string()
                        }
                    };
                    yield data;
                }
                tap.lock().unwrap_or_else(std::sync::PoisonError::into_inner).ended = true;
            };
            let oai_chunks = anthropic_stream_to_oai_chunks(sse_stream, model.clone());
            let mut oai_chunks = std::pin::pin!(oai_chunks);
            while let Some(chunk) = oai_chunks.next().await {
                if chunk.extra.contains_key("error") {
                    capture.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
                        .fail("proxy_stream_error");
                }
                yield chunk;
            }
            let mut capture = capture.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if capture.ended && !capture.failed {
                capture.attempt.stream_ended();
            }
        };
        Ok(Box::pin(stream))
    }
}

#[allow(non_camel_case_types)]
struct Stream_observation {
    attempt: WireAttempt,
    failed: bool,
    ended: bool,
}

impl Stream_observation {
    fn fail(&mut self, code: &str) {
        if !self.failed {
            self.attempt.failed(code);
            self.failed = true;
        }
    }

    fn observe_event(&mut self, data: &str) {
        if !self.attempt.enabled() {
            return;
        }
        let metadata: Anthropic_event_metadata = match serde_json::from_str(data) {
            Ok(metadata) => metadata,
            Err(_) => {
                self.fail("proxy_stream_error");
                return;
            }
        };
        match metadata.event_kind {
            Anthropic_event_kind::Message_start => {
                if let Some(message) = metadata.message {
                    match message.model {
                        Model_metadata::Reported(model) => self
                            .attempt
                            .reported_model(ModelSource::MessageModel, Some(&model)),
                        Model_metadata::Rejected => {
                            self.attempt.rejected_model(ModelSource::MessageModel)
                        }
                        Model_metadata::Missing => {}
                    }
                }
            }
            Anthropic_event_kind::Message_stop => {
                self.attempt.terminal(TerminalSignal::AnthropicMessageStop);
                self.ended = true;
            }
            Anthropic_event_kind::Error => self.fail("upstream_error"),
            Anthropic_event_kind::Other => {}
        }
    }
}

#[derive(Deserialize)]
#[allow(non_camel_case_types)]
struct Anthropic_event_metadata {
    #[serde(default, rename = "type")]
    event_kind: Anthropic_event_kind,
    #[serde(default)]
    message: Option<Anthropic_message_metadata>,
}

#[derive(Default, Deserialize)]
#[allow(non_camel_case_types)]
enum Anthropic_event_kind {
    #[serde(rename = "message_start")]
    Message_start,
    #[serde(rename = "message_stop")]
    Message_stop,
    #[serde(rename = "error")]
    Error,
    #[default]
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
#[allow(non_camel_case_types)]
struct Anthropic_message_metadata {
    #[serde(default)]
    model: Model_metadata,
}

#[derive(Default)]
#[allow(non_camel_case_types)]
enum Model_metadata {
    #[default]
    Missing,
    Reported(String),
    Rejected,
}

impl<'de> Deserialize<'de> for Model_metadata {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[allow(non_camel_case_types)]
        struct Model_visitor;
        impl<'de> Visitor<'de> for Model_visitor {
            type Value = Model_metadata;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("model metadata")
            }

            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(crate::proxy::routing_receipts::contract::safe_model(value)
                    .map(Model_metadata::Reported)
                    .unwrap_or(Model_metadata::Rejected))
            }

            fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(Model_metadata::Rejected)
            }

            fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(Model_metadata::Rejected)
            }

            fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(Model_metadata::Rejected)
            }

            fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(Model_metadata::Rejected)
            }

            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(Model_metadata::Rejected)
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut values: A) -> Result<Self::Value, A::Error> {
                while values.next_element::<IgnoredAny>()?.is_some() {}
                Ok(Model_metadata::Rejected)
            }

            fn visit_map<A: MapAccess<'de>>(self, mut values: A) -> Result<Self::Value, A::Error> {
                while values.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(Model_metadata::Rejected)
            }
        }
        deserializer.deserialize_any(Model_visitor)
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

    #[test]
    fn metadata_tap_keeps_model_claims_bounded_and_ignores_content() {
        let metadata: Anthropic_event_metadata = serde_json::from_value(json!({
            "type": "message_start",
            "message": {"model": "reported_model", "content": [{"text": "not_metadata"}]},
            "delta": {"text": "not_metadata"}
        }))
        .unwrap();
        assert!(matches!(
            metadata.event_kind,
            Anthropic_event_kind::Message_start
        ));
        assert!(matches!(metadata.message.unwrap().model,
            Model_metadata::Reported(model) if model == "reported_model"));

        let metadata: Anthropic_event_metadata =
            serde_json::from_str(r#"{"type":"message_start","message":{}}"#).unwrap();
        assert!(matches!(
            metadata.message.unwrap().model,
            Model_metadata::Missing
        ));
        for model in [
            json!(null),
            json!(42),
            json!({"text":"not_metadata"}),
            json!(["not_metadata"]),
            json!("sk-not_metadata"),
            json!("m".repeat(crate::proxy::routing_receipts::contract::MAX_MODEL_BYTES + 1)),
        ] {
            let metadata: Anthropic_event_metadata = serde_json::from_value(json!({
                "type":"message_start", "message":{"model":model}
            }))
            .unwrap();
            assert!(matches!(
                metadata.message.unwrap().model,
                Model_metadata::Rejected
            ));
        }
    }

    #[test]
    fn disabled_stream_observation_does_not_parse_metadata() {
        let attempt = ProviderObservation::default()
            .begin_wire("wire_model", CredentialMode::None)
            .unwrap();
        let mut capture = Stream_observation {
            attempt,
            failed: false,
            ended: false,
        };
        capture.observe_event("not json");
        assert!(!capture.failed);
    }

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
            .send_with_thinking_compatibility(
                &body,
                false,
                "req_test",
                None,
                &ProviderObservation::default(),
            )
            .await
            .err()
            .expect("upstream rejection must fail");
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

    #[tokio::test]
    async fn structured_five_hour_limit_fails_over_but_auth_and_invalid_params_do_not() {
        use axum::http::StatusCode;

        for (status, code, expected_retriable) in [
            (StatusCode::BAD_REQUEST, 2056, true),
            (StatusCode::FORBIDDEN, 2067, true),
            (StatusCode::BAD_REQUEST, 2013, false),
            (StatusCode::UNAUTHORIZED, 1004, false),
            (StatusCode::UNAUTHORIZED, 2056, false),
        ] {
            let reject = move || async move {
                (
                    status,
                    Json(
                        json!({"base_resp":{"status_code":code,"status_msg":"fixture_private_error"}}),
                    ),
                )
            };
            let app = Router::new()
                .route("/v1/messages", post(reject))
                .route("/v1/messages/count_tokens", post(reject));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            let provider = AnthropicProvider::new(
                "quota-fixture".into(),
                format!("http://{address}"),
                String::new(),
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
                    content: json!("fixture"),
                }],
                temperature: None,
                stream: Some(false),
                stop_sequences: None,
                extra: Default::default(),
            };
            for stream in [false, true] {
                let error = provider
                    .send_with_thinking_compatibility(
                        &body,
                        stream,
                        "quota_test",
                        None,
                        &ProviderObservation::default(),
                    )
                    .await
                    .err()
                    .unwrap();
                assert_eq!(
                    error.retriable, expected_retriable,
                    "{status}/{code}, stream={stream}"
                );
                assert_eq!(error.status, Some(status.as_u16()));
                assert!(!format!("{error:?}").contains("fixture_private_error"));
            }
            let error = provider
                .count_tokens(json!({"model":"model","messages":[]}), None)
                .await
                .unwrap_err();
            assert_eq!(
                error.retriable, expected_retriable,
                "count_tokens {status}/{code}"
            );
            assert!(error.response_bytes.is_some());
            server.abort();
        }
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
        anthropic_beta: Arc<Mutex<Vec<Option<String>>>>,
        message_bodies: Arc<Mutex<Vec<serde_json::Value>>>,
        count_tokens_paths: Arc<Mutex<Vec<String>>>,
        count_tokens_bodies: Arc<Mutex<Vec<serde_json::Value>>>,
    }

    async fn header_capture_handler(
        State(state): State<Header_capture_state>,
        headers: axum::http::HeaderMap,
        Json(body): Json<serde_json::Value>,
    ) -> Response {
        state.message_bodies.lock().unwrap().push(body);
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
        state.anthropic_beta.lock().unwrap().push(
            headers
                .get("anthropic-beta")
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

    async fn count_tokens_capture_handler(
        State(state): State<Header_capture_state>,
        uri: axum::http::Uri,
        headers: axum::http::HeaderMap,
        axum::Json(body): axum::Json<serde_json::Value>,
    ) -> Response {
        state
            .count_tokens_paths
            .lock()
            .unwrap()
            .push(uri.path().to_string());
        state.count_tokens_bodies.lock().unwrap().push(body);
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
        state.anthropic_beta.lock().unwrap().push(
            headers
                .get("anthropic-beta")
                .and_then(|v| v.to_str().ok().map(String::from)),
        );
        Json(json!({ "input_tokens": 4242 })).into_response()
    }

    async fn passthrough_fixture() -> (
        Header_capture_state,
        std::net::SocketAddr,
        tokio::task::JoinHandle<()>,
    ) {
        let state = Header_capture_state::default();
        let app = Router::new()
            .route("/v1/messages", post(header_capture_handler))
            .route(
                "/v1/messages/count_tokens",
                post(count_tokens_capture_handler),
            )
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
            passthrough_only: false,
            authorization: authorization.map(String::from),
            x_api_key: x_api_key.map(String::from),
            anthropic_beta: None,
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

    #[tokio::test]
    async fn static_auth_scheme_selects_one_header_and_cannot_be_overridden() {
        use crate::proxy::providers::{
            build_provider, build_provider_with_auth, AnthropicAuthScheme,
        };

        let (state, address, server) = passthrough_fixture().await;
        let row = serde_json::from_value(json!({
            "id": "static_auth_fixture", "name": "Fixture", "type": "anthropic",
            "baseURL": format!("http://{address}"), "apiKey": "fixture_static_key",
            "models": ["model"], "extraHeaders": {
                "Authorization": "Bearer forbidden_extra", "X-Api-Key": "forbidden_extra"
            }
        }))
        .unwrap();
        let request =
            passthrough_request(Some("Bearer forbidden_client"), Some("forbidden_client"));
        // Existing callers retain API-key authentication; explicit selection
        // reaches both completion and count_tokens through the same builder.
        build_provider(&row)
            .unwrap()
            .complete(&request, "model")
            .await
            .unwrap();
        for scheme in [AnthropicAuthScheme::ApiKey, AnthropicAuthScheme::Bearer] {
            let provider = build_provider_with_auth(&row, scheme).unwrap();
            provider.complete(&request, "model").await.unwrap();
            provider
                .count_tokens(
                    json!({"model":"model","messages":[]}),
                    request.client_credential.as_ref(),
                )
                .await
                .unwrap();
        }
        assert_eq!(
            *state.authorization.lock().unwrap(),
            vec![
                None,
                None,
                None,
                Some("Bearer fixture_static_key".into()),
                Some("Bearer fixture_static_key".into())
            ]
        );
        assert_eq!(
            *state.x_api_key.lock().unwrap(),
            vec![
                Some("fixture_static_key".into()),
                Some("fixture_static_key".into()),
                Some("fixture_static_key".into()),
                None,
                None
            ]
        );
        server.abort();
    }

    #[tokio::test]
    async fn static_auth_scheme_does_not_change_passthrough_credentials() {
        let (state, address, server) = passthrough_fixture().await;
        let provider = AnthropicProvider::new_passthrough(
            "passthrough_auth_fixture".into(),
            format!("http://{address}"),
            vec!["model".into()],
            true,
            2_000,
            CircuitBreakerConfig::default(),
            Default::default(),
        )
        .unwrap()
        .with_auth_scheme(crate::proxy::providers::AnthropicAuthScheme::Bearer);
        provider
            .complete(
                &passthrough_request(None, Some("fixture_client_key")),
                "model",
            )
            .await
            .unwrap();
        assert_eq!(*state.authorization.lock().unwrap(), vec![None]);
        assert_eq!(
            *state.x_api_key.lock().unwrap(),
            vec![Some("fixture_client_key".into())]
        );
        server.abort();
    }

    fn beta_credential_request(beta: Option<&str>) -> ChatCompletionRequest {
        let mut request = passthrough_request(Some("Bearer oauth-relay-91"), None);
        if let Some(credential) = request.client_credential.as_mut() {
            credential.anthropic_beta = beta.map(String::from);
        }
        request
    }

    /// The client's `anthropic-beta` feature-flag list rides along on
    /// passthrough hops so a subscription login keeps its negotiated beta
    /// features through the relay.
    #[tokio::test]
    async fn passthrough_relays_anthropic_beta_verbatim() {
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
        let request = beta_credential_request(Some("context-1m,claude-code-20250219"));
        provider.complete(&request, &request.model).await.unwrap();
        let beta = state.anthropic_beta.lock().unwrap();
        assert_eq!(
            beta[0].as_deref(),
            Some("context-1m,claude-code-20250219"),
            "the beta header must be relayed verbatim"
        );
        server.abort();
    }

    /// The static-key wire format stays byte-identical: no beta relay there.
    #[tokio::test]
    async fn static_key_path_does_not_relay_anthropic_beta() {
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
        let mut request = beta_credential_request(Some("context-1m"));
        request.client_credential = request.client_credential.map(|mut credential| {
            credential.anthropic_beta = Some("context-1m".into());
            credential
        });
        provider.complete(&request, &request.model).await.unwrap();
        let beta = state.anthropic_beta.lock().unwrap();
        assert!(
            beta[0].is_none(),
            "the static-key hop must not gain a beta header"
        );
        server.abort();
    }

    /// count_tokens relays to the derived endpoint with the client's
    /// credential and beta list; the hop's upstream model already rides the
    /// body (the router rewrote it).
    #[tokio::test]
    async fn count_tokens_relays_credential_beta_and_body() {
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
        let credential = Client_credential {
            passthrough_only: false,
            authorization: Some("Bearer oauth-relay-91".into()),
            x_api_key: None,
            anthropic_beta: Some("context-1m".into()),
        };
        let body = json!({
            "model": "claude-sonnet-4-5",
            "messages": [{"role": "user", "content": "hello"}],
        });
        let tokens = provider
            .count_tokens(body, Some(&credential))
            .await
            .expect("count_tokens relay");
        assert_eq!(tokens, 4242);
        let paths = state.count_tokens_paths.lock().unwrap();
        assert_eq!(
            paths[0].as_str(),
            "/v1/messages/count_tokens",
            "the derived count endpoint must be used"
        );
        let bodies = state.count_tokens_bodies.lock().unwrap();
        assert_eq!(bodies[0]["model"], "claude-sonnet-4-5");
        assert_eq!(
            state.authorization.lock().unwrap()[0].as_deref(),
            Some("Bearer oauth-relay-91")
        );
        assert_eq!(
            state.anthropic_beta.lock().unwrap()[0].as_deref(),
            Some("context-1m")
        );
        server.abort();
    }

    #[tokio::test]
    async fn count_tokens_and_messages_normalize_local_model_suffix_identically() {
        let (state, address, server) = passthrough_fixture().await;
        for passthrough in [false, true] {
            let provider = if passthrough {
                AnthropicProvider::new_passthrough(
                    "relay".into(),
                    format!("http://{address}"),
                    vec![],
                    true,
                    2_000,
                    CircuitBreakerConfig::default(),
                    Default::default(),
                )
            } else {
                AnthropicProvider::new(
                    "pool".into(),
                    format!("http://{address}"),
                    "synthetic_key".into(),
                    vec![],
                    true,
                    2_000,
                    CircuitBreakerConfig::default(),
                    Default::default(),
                )
            }
            .unwrap();
            for model in ["model[1M]", "model [1m]  ", "model"] {
                let request = passthrough_request(Some("Bearer synthetic_relay"), None);
                provider.complete(&request, model).await.unwrap();
                let body = json!({"model": model, "messages": [], "system": "preserved"});
                assert_eq!(
                    provider
                        .count_tokens(body.clone(), request.client_credential.as_ref())
                        .await
                        .unwrap(),
                    4242
                );
                let messages = state.message_bodies.lock().unwrap();
                let counts = state.count_tokens_bodies.lock().unwrap();
                assert_eq!(messages.last().unwrap()["model"], "model");
                assert_eq!(
                    counts.last().unwrap()["model"],
                    messages.last().unwrap()["model"]
                );
                assert_eq!(counts.last().unwrap()["system"], "preserved");
                assert_eq!(body["model"], model, "caller body remains unchanged");
            }
        }
        server.abort();
    }

    /// A static-key provider counts with the stored key and no relay.
    #[tokio::test]
    async fn count_tokens_static_key_uses_stored_credential() {
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
        let tokens = provider
            .count_tokens(json!({"model": "m", "messages": []}), None)
            .await
            .expect("count_tokens static");
        assert_eq!(tokens, 4242);
        assert_eq!(
            state.x_api_key.lock().unwrap()[0].as_deref(),
            Some("sk-static-pool-7")
        );
        assert!(state.authorization.lock().unwrap()[0].is_none());
        server.abort();
    }

    /// count_tokens 401 classification mirrors the messages surface: a
    /// relayed credential that the upstream rejects is retriable so the
    /// route can fall through to static-key hops.
    #[tokio::test]
    async fn count_tokens_passthrough_401_with_credential_is_retriable() {
        let app = Router::new().route(
            "/v1/messages/count_tokens",
            post(|| async { unauthorized_handler().await }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
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
        let credential = Client_credential {
            passthrough_only: false,
            authorization: Some("Bearer not-an-oauth-token".into()),
            x_api_key: None,
            anthropic_beta: None,
        };
        let error = provider
            .count_tokens(json!({"model": "m", "messages": []}), Some(&credential))
            .await
            .expect_err("401 must surface");
        assert_eq!(error.status, Some(401));
        assert!(error.retriable, "must fail over to later hops");
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
