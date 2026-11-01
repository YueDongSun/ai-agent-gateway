use crate::database::dao::proxy_providers::CircuitBreakerConfig;
use crate::proxy::canonical::{
    self, ChatCompletionRequest, OAIChatResponse, OAIStreamChunk, ProviderError,
};
use crate::proxy::circuit_breaker::CircuitBreaker;
use crate::proxy::providers::http_client::{
    build_http_client, error_kind, read_captured_error_body,
};
use crate::proxy::providers::sse::SseDataStream;
use crate::proxy::providers::Provider;
use crate::proxy::routing_receipts::contract::{CredentialMode, ModelSource, TerminalSignal};
use crate::proxy::routing_receipts::{ProviderObservation, WireAttempt, WireResponse};
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
    response_byte_limit: Option<usize>,
    probe_response: crate::proxy::providers::http_client::ProbeResponseCapture,
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
            response_byte_limit: None,
            probe_response: Default::default(),
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
    ) -> Result<(Vec<u8>, String), ProviderError> {
        // Written once, straight to the request bytes: the previous shape
        // (`to_value` + patch + `.json(body)`) deep-copied the whole
        // conversation payload into a `serde_json::Value` on every hop.
        let upstream_model = crate::proxy::strip_one_m_suffix(upstream_model);
        let hop = crate::proxy::canonical::Hop_body {
            model: &upstream_model,
            messages: &req.messages,
            stream,
            temperature: req.temperature,
            max_tokens: req.max_tokens,
            tools: req.tools.as_deref(),
            tool_choice: req.tool_choice.as_ref(),
            extra: &req.extra,
            // Usage accounting is opt-out: a caller-supplied
            // `stream_options` (inside `extra`) wins untouched.
            stream_options: (stream && !req.extra.contains_key("stream_options")).then_some(
                crate::proxy::canonical::Stream_options {
                    include_usage: true,
                },
            ),
        };
        let body = serde_json::to_vec(&hop).map_err(|error| {
            let _ = error;
            crate::proxy::canonical::make_provider_error(
                "request_serialize_error",
                "request_serialize",
                None,
                Some(false),
                None,
            )
        })?;
        Ok((body, upstream_model))
    }

    async fn send_body(
        &self,
        body: Vec<u8>,
        wire_model: &str,
        stream: bool,
        request_id: &str,
        observation: &ProviderObservation,
    ) -> Result<WireResponse, ProviderError> {
        let started = Instant::now();
        let mut request = self
            .client
            .post(self.endpoint())
            .headers(self.headers())
            .body(body);
        if !stream {
            request = request.timeout(self.timeout);
        }
        let request = request.build().map_err(|error| {
            canonical::make_provider_error(
                "upstream_transport_error",
                error_kind(&error),
                None,
                Some(true),
                None,
            )
        })?;
        #[cfg(test)]
        crate::canary::before_upstream_send()?;
        let credential_mode = if self.api_key.is_empty() {
            CredentialMode::None
        } else {
            CredentialMode::StoredKey
        };
        let mut attempt = observation.begin_wire(wire_model, credential_mode)?;
        let send = self.client.execute(request);
        let response = if stream {
            tokio::time::timeout(self.timeout, send)
                .await
                .map_err(|_| {
                    attempt.failed("upstream_timeout");
                    canonical::make_provider_error(
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
            canonical::make_provider_error(
                "upstream_transport_error",
                error_kind(&error),
                None,
                Some(true),
                None,
            )
        })?;
        let status = response.status().as_u16();
        attempt.response_status(status);
        log::debug!(
            "provider: headers request_id={} provider={} protocol=openai stream={} elapsed_ms={}",
            request_id,
            crate::diagnostics::safe_reference(&self.id),
            stream,
            started.elapsed().as_millis()
        );
        if status >= 400 {
            attempt.failed("upstream_http_error");
            // Bounded error read so a misconfigured or hostile upstream
            // cannot buffer an arbitrarily large 4xx/5xx body into RAM (the
            // inbound request-size limit does not apply to responses).
            // Non-UTF-8 bytes decode lossily, which is fine — the classifier
            // returns None and the error stays unclassified (terminal at the
            // router).
            let (body, response_bytes) = read_captured_error_body(
                response,
                self.response_byte_limit,
                &self.probe_response,
                &self.api_key,
                &self.extra_headers,
            )
            .await;
            let kind = canonical::classify_upstream_4xx(status, &body);
            return Err(canonical::make_provider_error_with_kind(
                "upstream_http_error",
                "response_status",
                Some(status),
                canonical::upstream_quota_retriable(status, &body),
                Some(response_bytes),
                kind,
            ));
        }
        Ok(WireResponse { response, attempt })
    }
}

// placeholder impl
#[async_trait]
impl Provider for OpenAIProvider {
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
        "openai-chat"
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
        let (body, wire_model) = self.request_body(req, upstream_model, false)?;
        let request_id = req.request_id.as_deref().unwrap_or("unknown");
        let WireResponse {
            response,
            mut attempt,
        } = self
            .send_body(body, &wire_model, false, request_id, &observation)
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
        let response = serde_json::from_slice::<OAIChatResponse>(&bytes).map_err(|_| {
            attempt.failed("upstream_decode_error");
            canonical::make_provider_error(
                "upstream_decode_error",
                "response_decode",
                Some(502),
                Some(true),
                Some(bytes.len()),
            )
        })?;
        attempt.reported_model(ModelSource::Model, Some(&response.model));
        attempt.response_parsed();
        Ok(response)
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
        let (body, wire_model) = self.request_body(req, upstream_model, true)?;
        let provider_id = self.id.clone();
        // Eager send + status check: an upstream 4xx/5xx becomes a real
        // ProviderError with the status code, so the router can fail over
        // immediately (one RTT) instead of timing out on an empty stream.
        let request_id = req.request_id.clone().unwrap_or_else(|| "unknown".into());
        let WireResponse {
            response,
            mut attempt,
        } = self
            .send_body(body, &wire_model, true, &request_id, &observation)
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
        let model = crate::proxy::strip_one_m_suffix(upstream_model);
        let stream = async_stream::stream! {
            let mut events = std::pin::pin!(SseDataStream::new(byte_stream));
            let mut received_event = false;
            let mut finished = false;
            while let Some(event) = events.next().await {
                let data = match event {
                    Ok(data) => data,
                    Err(_) => {
                        attempt.failed("proxy_stream_error");
                        yield crate::proxy::canonical::stream_error_chunk(&model, 502, "proxy_stream_error");
                        return;
                    }
                };
                if data == "[DONE]" {
                    attempt.terminal(TerminalSignal::OpenaiDone);
                    if !finished && received_event {
                        attempt.failed("proxy_stream_error");
                        yield crate::proxy::canonical::stream_error_chunk(&model, 502, "proxy_stream_error");
                    } else {
                        attempt.stream_ended();
                    }
                    return;
                }
                let value: serde_json::Value = match serde_json::from_str(&data) {
                    Ok(value) => value,
                    Err(_) => {
                        attempt.failed("proxy_stream_error");
                        yield crate::proxy::canonical::stream_error_chunk(&model, 502, "proxy_stream_error");
                        return;
                    }
                };
                observe_model(&attempt, &value);
                if value.get("choices").and_then(serde_json::Value::as_array).is_some_and(|choices| {
                    choices.iter().any(has_supported_finish_reason)
                }) {
                    attempt.terminal(TerminalSignal::OpenaiFinish);
                }
                if value.get("error").is_some() {
                    attempt.failed("upstream_error");
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
                        attempt.failed("proxy_stream_error");
                        yield crate::proxy::canonical::stream_error_chunk(&model, 502, "proxy_stream_error");
                        return;
                    }
                }
            }
            if received_event && !finished {
                attempt.failed("proxy_stream_error");
                yield crate::proxy::canonical::stream_error_chunk(&model, 502, "proxy_stream_error");
            } else {
                attempt.stream_ended();
            }
        };
        Ok(Box::pin(stream))
    }
}

fn has_supported_finish_reason(choice: &serde_json::Value) -> bool {
    matches!(
        choice
            .get("finish_reason")
            .and_then(serde_json::Value::as_str),
        Some("stop" | "length" | "content_filter" | "tool_calls" | "function_call")
    )
}

fn observe_model(attempt: &WireAttempt, value: &serde_json::Value) {
    if !attempt.enabled() {
        return;
    }
    if let Some(model) = value.get("model") {
        match model.as_str() {
            Some(model) => attempt.reported_model(ModelSource::Model, Some(model)),
            None => attempt.rejected_model(ModelSource::Model),
        }
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
            source_responses_request: None,
            source_gemini_request: None,
            request_id: None,
            client_credential: None,
        }
    }

    #[test]
    fn receipt_finish_reasons_accept_only_protocol_values() {
        for reason in [
            "stop",
            "length",
            "content_filter",
            "tool_calls",
            "function_call",
        ] {
            assert!(
                has_supported_finish_reason(&json!({"finish_reason": reason})),
                "{reason}"
            );
        }
        for choice in [
            json!({}),
            json!({"finish_reason": null}),
            json!({"finish_reason": 42}),
            json!({"finish_reason": ["stop"]}),
            json!({"finish_reason": {"reason": "stop"}}),
            json!({"finish_reason": ""}),
            json!({"finish_reason": "STOP"}),
            json!({"finish_reason": "unknown"}),
            json!({"finish_reason": " stop"}),
            json!({"finish_reason": "stop "}),
            json!({"finish_reason": "max_tokens"}),
        ] {
            assert!(!has_supported_finish_reason(&choice), "{choice}");
        }
    }

    #[test]
    fn streaming_body_requests_usage_without_overwriting_caller_value() {
        let provider = provider();
        let body: serde_json::Value =
            serde_json::from_slice(&provider.request_body(&request(), "model", true).unwrap().0)
                .expect("hop body is json");
        assert_eq!(body["stream_options"]["include_usage"], true);

        let mut explicit = request();
        explicit.extra.insert(
            "stream_options".into(),
            json!({"include_usage": false, "custom": "keep"}),
        );
        let body: serde_json::Value =
            serde_json::from_slice(&provider.request_body(&explicit, "gpt-5", true).unwrap().0)
                .expect("hop body is json");
        assert_eq!(body["stream_options"]["include_usage"], false);
        assert_eq!(body["stream_options"]["custom"], "keep");
    }

    /// The hop body must reach the upstream with the hop's upstream model,
    /// not the client-requested alias, and the `[1M]` marker stripped.
    #[test]
    fn hop_body_carries_the_hop_model_and_strips_the_context_marker() {
        let provider = provider();
        let (bytes, wire_model) = provider
            .request_body(&request(), "gpt-5[1M]", false)
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).expect("hop body is json");
        assert_eq!(wire_model, "gpt-5");
        assert_eq!(body["model"], "gpt-5");
        assert_eq!(body["stream"], false);
        // Non-streaming hops ask for no usage accounting.
        assert!(body.get("stream_options").is_none());
    }

    #[test]
    fn foreign_chat_request_does_not_serialize_native_gemini_signatures() {
        let mut req = request();
        req.messages[0].role = "assistant".into();
        req.messages[0].tool_calls = Some(vec![json!({
            "id":"gemini_call_fixture","type":"function","function":{"name":"lookup","arguments":"{}"},
            "thoughtSignature":"native_gemini_signature","vendor_extra":"keep"
        })]);
        let (bytes, _) = provider()
            .request_body(&req, "foreign-model", false)
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let call = &body["messages"][0]["tool_calls"][0];
        assert!(call.get("thoughtSignature").is_none());
        assert_eq!(call["vendor_extra"], "keep");
        assert_eq!(call["function"]["name"], "lookup");
    }

    #[test]
    fn hop_overrides_cannot_be_shadowed_by_internal_extra_fields() {
        let mut req = request();
        req.extra.insert("model".into(), json!("not_the_hop"));
        req.extra.insert("stream".into(), json!(true));
        let (bytes, wire_model) = provider().request_body(&req, "hop_model", false).unwrap();
        assert_eq!(wire_model, "hop_model");
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["model"], "hop_model");
        assert_eq!(body["stream"], false);
        assert_eq!(
            String::from_utf8(bytes)
                .unwrap()
                .matches("\"model\"")
                .count(),
            1
        );
    }

    #[test]
    #[ignore = "manual local serialization timing comparison"]
    fn request_serialization_comparison() {
        use std::hint::black_box;
        let mut req = request();
        req.messages[0].content = json!("x".repeat(2 * 1024 * 1024));
        let encoded = serde_json::to_vec(&req).unwrap();
        let provider = provider();
        let mut ingress_old = Vec::new();
        let mut ingress_new = Vec::new();
        let mut egress_old = Vec::new();
        let mut egress_new = Vec::new();
        for iteration in 0..12 {
            let start = Instant::now();
            let dom: serde_json::Value = serde_json::from_slice(black_box(&encoded)).unwrap();
            let old_req: ChatCompletionRequest = serde_json::from_value(dom).unwrap();
            black_box(old_req);
            let old_ingress_time = start.elapsed().as_micros();
            let start = Instant::now();
            black_box(
                serde_json::from_slice::<ChatCompletionRequest>(black_box(&encoded)).unwrap(),
            );
            let new_ingress_time = start.elapsed().as_micros();
            let start = Instant::now();
            let mut dom = serde_json::to_value(&req).unwrap();
            dom["model"] = json!("upstream");
            dom["stream"] = json!(false);
            let old_bytes = serde_json::to_vec(&dom).unwrap();
            let old_egress_time = start.elapsed().as_micros();
            let start = Instant::now();
            let (new_bytes, _) = provider.request_body(&req, "upstream", false).unwrap();
            let new_egress_time = start.elapsed().as_micros();
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&old_bytes).unwrap(),
                serde_json::from_slice::<serde_json::Value>(&new_bytes).unwrap()
            );
            if iteration >= 3 {
                ingress_old.push(old_ingress_time);
                ingress_new.push(new_ingress_time);
                egress_old.push(old_egress_time);
                egress_new.push(new_egress_time);
            }
        }
        for (label, mut samples) in [
            ("ingress_old", ingress_old),
            ("ingress_new", ingress_new),
            ("egress_old", egress_old),
            ("egress_new", egress_new),
        ] {
            samples.sort_unstable();
            println!(
                "{label} bytes={} n={} min_us={} median_us={} max_us={}",
                encoded.len(),
                samples.len(),
                samples[0],
                samples[samples.len() / 2],
                samples[samples.len() - 1]
            );
        }
    }

    #[test]
    #[ignore = "manual isolated-process memory comparison"]
    fn request_serialization_memory_probe() {
        use std::hint::black_box;
        let mode =
            std::env::var("AAG_TEST_SERIALIZATION_MODE").unwrap_or_else(|_| "borrowed".into());
        assert!(matches!(mode.as_str(), "value" | "borrowed"));
        let mut req = request();
        req.stream = false;
        req.messages[0].content = json!("x".repeat(16 * 1024 * 1024));
        let provider = provider();
        let encoded = if mode == "value" {
            let mut dom = serde_json::to_value(&req).unwrap();
            dom["model"] = json!("upstream");
            dom["stream"] = json!(false);
            let encoded = serde_json::to_vec(&dom).unwrap();
            black_box(dom);
            encoded
        } else {
            provider.request_body(&req, "upstream", false).unwrap().0
        };
        println!(
            "serialization_path={mode} payload_bytes={} wire_bytes={}",
            16 * 1024 * 1024,
            encoded.len()
        );
        black_box(encoded);
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
    async fn duplicate_known_response_fields_remain_retriable_decode_errors() {
        async fn duplicate_model() -> impl IntoResponse {
            (
                [("content-type", "application/json")],
                r#"{"id":"fixture","object":"chat.completion","created":1,"model":"first","model":"second","choices":[]}"#,
            )
        }
        let app = Router::new().route("/v1/chat/completions", post(duplicate_model));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let provider = OpenAIProvider::new(
            "fixture".into(),
            format!("http://{address}/v1"),
            String::new(),
            vec!["model".into()],
            true,
            1_000,
            CircuitBreakerConfig::default(),
            Default::default(),
        )
        .unwrap();
        let error = provider
            .complete_observed(&request(), "model", ProviderObservation::default())
            .await
            .unwrap_err();
        server.abort();
        assert_eq!(error.code, "upstream_decode_error");
        assert_eq!(error.status, Some(502));
        assert!(error.retriable);
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
            .send_body(
                b"{}".to_vec(),
                "model",
                false,
                "req_test",
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

    /// A hostile upstream may return an arbitrarily large error body: the
    /// provider must cap the buffered read while (1) keeping classification
    /// from the capped text and (2) reporting the observed (capped) size.
    #[tokio::test]
    async fn oversized_upstream_error_body_is_capped_but_still_classified() {
        async fn reject() -> impl IntoResponse {
            // Auth keyword inside the classifier's 8 KiB window, then padding
            // far beyond the 64 KiB read cap.
            let mut body =
                String::from("HTTP 401: invalid_grant — access token could not be refreshed");
            body.push_str(&"x".repeat(256 * 1024));
            (StatusCode::UNAUTHORIZED, body)
        }
        let app = Router::new().route("/v1/chat/completions", post(reject));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let provider = OpenAIProvider::new(
            "test".into(),
            format!("http://{address}/v1"),
            String::new(),
            vec!["model".into()],
            true,
            1_000,
            CircuitBreakerConfig::default(),
            Default::default(),
        )
        .unwrap();
        let error = provider
            .send_body(
                b"{}".to_vec(),
                "model",
                false,
                "req_test",
                &ProviderObservation::default(),
            )
            .await
            .err()
            .expect("upstream rejection must fail");
        server.abort();
        assert_eq!(error.status, Some(401));
        assert_eq!(
            error.kind,
            Some(crate::proxy::canonical::UpstreamErrorKind::AuthExpired),
            "classification must survive the capped read"
        );
        assert_eq!(
            error.response_bytes,
            Some(crate::proxy::providers::http_client::MAX_ERROR_BODY_BYTES),
            "the recorded size is the observed (capped) byte count"
        );
    }
}
