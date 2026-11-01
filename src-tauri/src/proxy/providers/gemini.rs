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
    gemini_resp_to_oai_resp, gemini_stream_to_oai_chunks_for_client, request_body,
    validate_response_for_foreign,
};
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
use serde::de::{IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::json;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
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
    response_byte_limit: Option<usize>,
    probe_response: crate::proxy::providers::http_client::ProbeResponseCapture,
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
            response_byte_limit: None,
            probe_response: Default::default(),
        })
    }

    fn method_target(&self, model: &str, method: &str) -> Result<(String, String), ProviderError> {
        let model = crate::proxy::strip_one_m_suffix(model);
        crate::proxy::endpoint::gemini_method_target(&self.base_url, &model, method).map_err(|_| {
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
        wire_model: &str,
        body: &serde_json::Value,
        stream: bool,
        request_id: &str,
        observation: &ProviderObservation,
    ) -> Result<WireResponse, ProviderError> {
        let started = Instant::now();
        let mut request = self.client.post(url).headers(self.headers()).json(body);
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
        let status = response.status().as_u16();
        attempt.response_status(status);
        log::debug!(
            "provider: headers request_id={} provider={} protocol=gemini stream={} elapsed_ms={}",
            request_id,
            crate::diagnostics::safe_reference(&self.id),
            stream,
            started.elapsed().as_millis()
        );
        if status >= 400 {
            attempt.failed("upstream_http_error");
            // Bounded read used only for its size — diagnostics never include
            // the upstream payload (it may echo private material), and the
            // read is capped so a huge error body is never buffered whole.
            let (_, response_bytes) = read_captured_error_body(
                response,
                self.response_byte_limit,
                &self.probe_response,
                &self.api_key,
                &self.extra_headers,
            )
            .await;
            return Err(crate::proxy::canonical::make_provider_error(
                "upstream_http_error",
                "response_status",
                Some(status),
                None,
                Some(response_bytes),
            ));
        }
        Ok(WireResponse { response, attempt })
    }
}

#[async_trait]
impl Provider for GeminiProvider {
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
        "gemini"
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
        let body = request_body(req)?;
        let request_id = req.request_id.as_deref().unwrap_or("unknown");
        let (url, wire_model) = self.method_target(upstream_model, "generateContent")?;
        let WireResponse {
            response,
            mut attempt,
        } = self
            .send(&url, &wire_model, &body, false, request_id, &observation)
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
        let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| {
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
            if let Some(model) = value.get("modelVersion") {
                match model.as_str() {
                    Some(model) => attempt.reported_model(ModelSource::ModelVersion, Some(model)),
                    None => attempt.rejected_model(ModelSource::ModelVersion),
                }
            }
        }
        if let Some(error) = value.get("error") {
            attempt.failed("upstream_http_error");
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
        if req.source_gemini_request.is_none() {
            validate_response_for_foreign(&value).map_err(|error| {
                attempt.failed(&error.code);
                error
            })?;
        }
        let response = gemini_resp_to_oai_resp(&value, upstream_model);
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
        let body = request_body(req)?;
        let native_client = req.source_gemini_request.is_some();
        let request_id = req.request_id.clone().unwrap_or_else(|| "unknown".into());
        let (mut url, wire_model) = self.method_target(upstream_model, "streamGenerateContent")?;
        // Gemini's SSE framing is opt-in via `alt=sse`.
        url.push_str("?alt=sse");
        let provider_id = self.id.clone();
        // Eager send + status check so the router can fail over on a 4xx/5xx
        // in one RTT instead of timing out on an empty stream.
        let WireResponse { response, attempt } = self
            .send(&url, &wire_model, &body, true, &request_id, &observation)
            .await?;
        let model = crate::proxy::strip_one_m_suffix(upstream_model);
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
                            json!({
                                "error": {
                                    "code": 502,
                                    "message": "Upstream stream transport failed",
                                    "status": "UNAVAILABLE",
                                }
                            }).to_string()
                        }
                    };
                    yield data;
                }
                tap.lock().unwrap_or_else(std::sync::PoisonError::into_inner).ended = true;
            };
            let oai_chunks = gemini_stream_to_oai_chunks_for_client(sse_stream, model.clone(), native_client);
            let mut oai_chunks = std::pin::pin!(oai_chunks);
            while let Some(chunk) = oai_chunks.next().await {
                if let Some(error) = chunk.extra.get("error") {
                    let code = if error.get("type").and_then(serde_json::Value::as_str) == Some("gemini_response_unsupported") {
                        "gemini_response_unsupported"
                    } else {
                        "proxy_stream_error"
                    };
                    capture.lock().unwrap_or_else(std::sync::PoisonError::into_inner).fail(code);
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
        let metadata: Gemini_event_metadata = match serde_json::from_str(data) {
            Ok(metadata) => metadata,
            Err(_) => {
                self.fail("proxy_stream_error");
                return;
            }
        };
        match metadata.model_version {
            Model_metadata::Reported(model) => self
                .attempt
                .reported_model(ModelSource::ModelVersion, Some(&model)),
            Model_metadata::Rejected | Model_metadata::Invalid_type => {
                self.attempt.rejected_model(ModelSource::ModelVersion);
            }
            Model_metadata::Missing => {}
        }
        if metadata.candidates.is_some_and(|candidates| candidates.0) {
            self.attempt.terminal(TerminalSignal::GeminiFinish);
        }
        if metadata.error_present {
            self.fail("upstream_error");
        }
    }
}

#[derive(Deserialize)]
#[allow(non_camel_case_types)]
struct Gemini_event_metadata {
    #[serde(default, rename = "modelVersion")]
    model_version: Model_metadata,
    #[serde(default)]
    candidates: Option<Candidate_terminals>,
    #[serde(default, rename = "error", deserialize_with = "error_present")]
    error_present: bool,
}

fn error_present<'de, D: Deserializer<'de>>(deserializer: D) -> Result<bool, D::Error> {
    IgnoredAny::deserialize(deserializer).map(|_| true)
}

#[derive(Deserialize)]
#[allow(non_camel_case_types)]
struct Candidate_metadata {
    #[serde(
        default,
        rename = "finishReason",
        deserialize_with = "deserialize_finish_reason"
    )]
    finish_reason: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[allow(non_camel_case_types)]
enum Gemini_finish_reason {
    Stop,
    Max_tokens,
    Safety,
    Recitation,
    Language,
    Other,
    Blocklist,
    Prohibited_content,
    Spii,
    Malformed_function_call,
}

#[derive(Deserialize)]
#[serde(untagged)]
#[allow(non_camel_case_types)]
enum Parsed_finish_reason {
    Supported(Gemini_finish_reason),
    Rejected(IgnoredAny),
}

fn deserialize_finish_reason<'de, D: Deserializer<'de>>(deserializer: D) -> Result<bool, D::Error> {
    Ok(matches!(
        Parsed_finish_reason::deserialize(deserializer)?,
        Parsed_finish_reason::Supported(_reason)
    ))
}

#[allow(non_camel_case_types)]
struct Candidate_terminals(bool);

impl<'de> Deserialize<'de> for Candidate_terminals {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[allow(non_camel_case_types)]
        struct Candidates_visitor;
        impl<'de> Visitor<'de> for Candidates_visitor {
            type Value = Candidate_terminals;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("candidate metadata")
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut values: A) -> Result<Self::Value, A::Error> {
                let mut finished = false;
                while let Some(candidate) = values.next_element::<Candidate_metadata>()? {
                    finished |= candidate.finish_reason;
                }
                Ok(Candidate_terminals(finished))
            }
        }
        deserializer.deserialize_seq(Candidates_visitor)
    }
}

#[derive(Default)]
#[allow(non_camel_case_types)]
enum Model_metadata {
    #[default]
    Missing,
    Reported(String),
    Rejected,
    Invalid_type,
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
                Ok(Model_metadata::Invalid_type)
            }

            fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(Model_metadata::Invalid_type)
            }

            fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(Model_metadata::Invalid_type)
            }

            fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(Model_metadata::Invalid_type)
            }

            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(Model_metadata::Invalid_type)
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut values: A) -> Result<Self::Value, A::Error> {
                while values.next_element::<IgnoredAny>()?.is_some() {}
                Ok(Model_metadata::Invalid_type)
            }

            fn visit_map<A: MapAccess<'de>>(self, mut values: A) -> Result<Self::Value, A::Error> {
                while values.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(Model_metadata::Invalid_type)
            }
        }
        deserializer.deserialize_any(Model_visitor)
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

    #[test]
    fn finish_reason_parser_accepts_only_closed_protocol_values() {
        for reason in [
            "STOP",
            "MAX_TOKENS",
            "SAFETY",
            "RECITATION",
            "LANGUAGE",
            "OTHER",
            "BLOCKLIST",
            "PROHIBITED_CONTENT",
            "SPII",
            "MALFORMED_FUNCTION_CALL",
        ] {
            let metadata: Gemini_event_metadata = serde_json::from_value(json!({
                "candidates":[{"finishReason":reason}]
            }))
            .unwrap();
            assert!(metadata.candidates.unwrap().0, "{reason}");
        }
        for finish_reason in [
            json!(null),
            json!(42),
            json!(["STOP"]),
            json!({"reason":"STOP"}),
            json!(""),
            json!("stop"),
            json!(" STOP"),
            json!("STOP "),
            json!("UNKNOWN"),
            json!("FINISH_REASON_UNSPECIFIED"),
        ] {
            let metadata: Gemini_event_metadata = serde_json::from_value(json!({
                "candidates":[{"finishReason":finish_reason}]
            }))
            .unwrap();
            assert!(!metadata.candidates.unwrap().0, "{finish_reason}");
        }
    }

    #[test]
    fn metadata_tap_distinguishes_raw_finish_from_tool_call_projection() {
        let metadata: Gemini_event_metadata = serde_json::from_value(json!({
            "modelVersion":"reported_model",
            "candidates":[{"content":{"parts":[{"functionCall":{"name":"lookup","args":{}}}]}}]
        }))
        .unwrap();
        assert!(
            matches!(metadata.model_version, Model_metadata::Reported(model) if model == "reported_model")
        );
        assert!(!metadata.candidates.unwrap().0);

        let metadata: Gemini_event_metadata = serde_json::from_value(json!({
            "candidates":[{}, {"finishReason":"STOP"}], "error":null
        }))
        .unwrap();
        assert!(metadata.candidates.unwrap().0);
        assert!(metadata.error_present);
        assert!(matches!(metadata.model_version, Model_metadata::Missing));
    }

    #[test]
    fn metadata_tap_rejects_invalid_models_without_retaining_nested_content() {
        for model in [
            json!(null),
            json!(42),
            json!({"text":"not_metadata"}),
            json!(["not_metadata"]),
            json!("sk-not_metadata"),
            json!("m".repeat(crate::proxy::routing_receipts::contract::MAX_MODEL_BYTES + 1)),
        ] {
            let metadata: Gemini_event_metadata = serde_json::from_value(json!({
                "modelVersion":model,
                "candidates":[],
            }))
            .unwrap();
            assert!(matches!(
                metadata.model_version,
                Model_metadata::Rejected | Model_metadata::Invalid_type
            ));
        }
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

    #[test]
    fn method_target_returns_the_model_embedded_in_the_url() {
        let provider = GeminiProvider::new(
            "fixture".into(),
            "https://example.invalid".into(),
            String::new(),
            vec![],
            true,
            1_000,
            CircuitBreakerConfig::default(),
            Default::default(),
        )
        .unwrap();
        let (url, wire_model) = provider
            .method_target("/models/gemini_fixture[1M]", "generateContent")
            .unwrap();
        assert_eq!(wire_model, "gemini_fixture");
        assert_eq!(
            url,
            "https://example.invalid/v1beta/models/gemini_fixture:generateContent"
        );
    }

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

    #[tokio::test]
    async fn native_gemini_http_preserves_signed_bodies_responses_and_stream_events() {
        let source = json!({
            "contents":[{"role":"model","parts":[{"text":"native history","thoughtSignature":"history_signature"}]}],
            "generationConfig":{"responseMimeType":"application/json"},
            "cachedContent":"cachedContents/fixture",
        });
        let expected = json!({
            "candidates":[{"content":{"role":"model","parts":[
                {"text":"native answer","thoughtSignature":"text_signature"},
                {"functionCall":{"name":"lookup","args":{}},"thoughtSignature":"call_signature"}
            ]},"finishReason":"STOP","groundingMetadata":{"fixture":true}}],
            "usageMetadata":{"promptTokenCount":1,"candidatesTokenCount":2,"totalTokenCount":3},
            "modelVersion":"native-model","nativeExtension":{"preserve":true}
        });
        async fn handler(
            axum::extract::State((source, response)): axum::extract::State<(Value, Value)>,
            axum::extract::Query(query): axum::extract::Query<
                std::collections::HashMap<String, String>,
            >,
            Json(body): Json<Value>,
        ) -> Response {
            assert_eq!(body, source);
            if query.get("alt").map(String::as_str) == Some("sse") {
                return (
                    [("content-type", "text/event-stream")],
                    format!("data: {response}\n\n"),
                )
                    .into_response();
            }
            Json(response).into_response()
        }
        let app = Router::new()
            .route("/v1beta/models/*tail", post(handler))
            .with_state((source.clone(), expected.clone()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let provider = GeminiProvider::new(
            "native_fixture".into(),
            format!("http://{address}"),
            "fixture_key".into(),
            vec!["native-model".into()],
            true,
            2_000,
            CircuitBreakerConfig::default(),
            Default::default(),
        )
        .unwrap();
        let req = crate::proxy::gemini::gemini_req_to_oai_req(&source);
        let response = provider.complete(&req, "native-model").await.unwrap();
        assert_eq!(
            crate::proxy::gemini::oai_resp_to_gemini_resp(&response, "client-alias"),
            expected
        );
        let stream = provider.stream(&req, "native-model").await.unwrap();
        let frames: Vec<_> =
            crate::proxy::gemini::oai_chunks_to_gemini_stream(stream, "client-alias".into())
                .collect()
                .await;
        assert_eq!(frames.len(), 1);
        assert_eq!(serde_json::from_str::<Value>(&frames[0]).unwrap(), expected);
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
