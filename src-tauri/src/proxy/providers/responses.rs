//! OpenAI Responses API upstream provider.
//!
//! Speaks the native `POST /v1/responses` wire shape. Two request paths:
//!
//! - A native Responses client request (sidecar `source_responses_request`)
//!   is relayed as the source of truth: only the routed model and stream
//!   flag change, so response IDs, `previous_response_id` linkage,
//!   encrypted reasoning and built-in tool state survive untouched.
//! - A Chat-originating request is explicitly mapped through
//!   `canonical_to_responses_request_value`; fields that cannot be
//!   represented fail before any network request instead of being dropped.
//!
//! Responses attach the full SSE frame (event name, id, joined data) to
//! every canonical chunk as `source_responses_event`, so the edge can replay
//! them losslessly on the native client surface while other surfaces receive
//! the explicit canonical conversion (or an explicit refusal when a
//! necessary field cannot cross).

use crate::database::dao::proxy_providers::CircuitBreakerConfig;
use crate::proxy::canonical::{
    self, ChatCompletionRequest, OAIChatResponse, OAIStreamChunk, ProviderError, StreamChoice,
    StreamDelta,
};
use crate::proxy::circuit_breaker::CircuitBreaker;
use crate::proxy::converter::{
    canonical_to_responses_request_value, responses_failure_error,
    responses_request_body_with_model, responses_response_to_chat,
};
use crate::proxy::providers::http_client::{
    apply_extra_headers, build_http_client, error_kind, read_captured_error_body,
};
use crate::proxy::providers::sse::SseEventStream;
use crate::proxy::providers::Provider;
use crate::proxy::responses::ResponsesSseEvent;
use crate::proxy::routing_receipts::contract::{CredentialMode, ModelSource, TerminalSignal};
use crate::proxy::routing_receipts::{ProviderObservation, WireAttempt, WireResponse};
use async_trait::async_trait;
use futures_core::Stream;
use futures_util::StreamExt;
use serde_json::Value;
use std::collections::HashMap;
use std::pin::Pin;
use std::time::{Duration, Instant};

pub struct ResponsesProvider {
    id: String,
    endpoint: String,
    api_key: String,
    /// Provider-mandated extra request headers, merged in `headers()` after
    /// the trusted set (reserved headers never overridden).
    extra_headers: HashMap<String, String>,
    models: Vec<String>,
    enabled: bool,
    timeout: Duration,
    client: reqwest::Client,
    breaker: CircuitBreaker,
    response_byte_limit: Option<usize>,
    probe_response: crate::proxy::providers::http_client::ProbeResponseCapture,
    /// MiniMax documents a stateless Responses subset. It ignores these
    /// linkage fields, so admitting them would silently lose conversation state.
    stateless_minimax: bool,
}

impl ResponsesProvider {
    pub fn new(
        id: String,
        base_url: String,
        api_key: String,
        models: Vec<String>,
        enabled: bool,
        timeout_ms: u64,
        cb_cfg: CircuitBreakerConfig,
        extra_headers: HashMap<String, String>,
    ) -> Result<Self, String> {
        // Resolve the request endpoint once at construction (see openai.rs).
        let endpoint = crate::proxy::endpoint::endpoint_url(
            if base_url.is_empty() {
                "https://api.openai.com/v1"
            } else {
                &base_url
            },
            crate::proxy::endpoint::Protocol::Responses,
        )
        .map_err(|error| format!("invalid openai-responses base_url: {error}"))?;
        let stateless_minimax = reqwest::Url::parse(&endpoint)
            .ok()
            .and_then(|url| url.host_str().map(str::to_owned))
            .is_some_and(|host| matches!(host.as_str(), "api.minimax.io" | "api.minimaxi.com"));
        Ok(Self {
            id,
            endpoint,
            api_key,
            extra_headers,
            models,
            enabled,
            timeout: Duration::from_millis(timeout_ms),
            client: build_http_client("OpenAI-Responses")?,
            breaker: CircuitBreaker::new(cb_cfg),
            response_byte_limit: None,
            probe_response: Default::default(),
            stateless_minimax,
        })
    }

    fn endpoint(&self) -> &str {
        &self.endpoint
    }

    fn headers(&self) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("content-type", "application/json".parse().unwrap());
        if !self.api_key.is_empty() {
            headers.insert(
                "authorization",
                format!("Bearer {}", self.api_key).parse().unwrap(),
            );
        }
        apply_extra_headers(&mut headers, &self.extra_headers);
        headers
    }

    fn request_body(
        &self,
        req: &ChatCompletionRequest,
        upstream_model: &str,
        stream: bool,
    ) -> Result<(Vec<u8>, String), ProviderError> {
        if self.stateless_minimax
            && req.source_responses_request.as_ref().is_some_and(|source| {
                source
                    .previous_response_id
                    .as_ref()
                    .is_some_and(|id| !id.is_empty())
                    || source
                        .extra
                        .get("conversation")
                        .is_some_and(|value| !value.is_null())
            })
        {
            return Err(canonical::make_provider_error(
                "responses_upstream_state_unsupported",
                "request_construction",
                Some(400),
                Some(false),
                None,
            ));
        }
        responses_request_body_with_model(req, upstream_model, stream)
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
            "provider: headers request_id={} provider={} protocol=responses stream={} elapsed_ms={}",
            request_id,
            crate::diagnostics::safe_reference(&self.id),
            stream,
            started.elapsed().as_millis()
        );
        if status >= 400 {
            attempt.failed("upstream_http_error");
            // Bounded error read so a misconfigured or hostile upstream
            // cannot buffer an arbitrarily large 4xx/5xx body into RAM.
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

#[async_trait]
impl Provider for ResponsesProvider {
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
        "openai-responses"
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
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| {
            attempt.failed("upstream_decode_error");
            canonical::make_provider_error(
                "upstream_decode_error",
                "response_decode",
                Some(502),
                Some(true),
                Some(bytes.len()),
            )
        })?;
        observe_model(&attempt, &value);
        // The sidecar preserves the exact native envelope; the canonical
        // shape carries what other surfaces can express.
        let response =
            responses_response_to_chat(&value, upstream_model, true).map_err(|error| {
                attempt.failed(&error.code);
                error
            })?;
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
        let byte_stream = response.bytes_stream().map(move |result| {
            result.map_err(|error| {
                log::warn!(
                    "provider: stream body failed request_id={} provider={} kind={}",
                    request_id,
                    crate::diagnostics::safe_reference(&provider_id),
                    error_kind(&error)
                );
                std::io::Error::other(error_kind(&error))
            })
        });
        let model = crate::proxy::strip_one_m_suffix(upstream_model);
        let stream = async_stream::stream! {
            let mut events = std::pin::pin!(SseEventStream::new(byte_stream));
            let mut state = Responses_stream_state::default();
            while let Some(event) = events.next().await {
                let event = match event {
                    Ok(event) => event,
                    Err(_) => {
                        attempt.failed("proxy_stream_error");
                        yield error_chunk(&model, "proxy_stream_error");
                        return;
                    }
                };
                let frame = ResponsesSseEvent {
                    event: event.event.clone(),
                    id: event.id.clone(),
                    retry: None,
                    data: event.data.clone(),
                };
                // The Responses wire uses named events, never a `[DONE]`
                // sentinel; a sentinel here is a relay artifact, not a
                // protocol frame.
                if event.data.trim() == "[DONE]" {
                    continue;
                }
                let value: Value = match serde_json::from_str(&event.data) {
                    Ok(value) => value,
                    Err(_) => {
                        attempt.failed("proxy_stream_error");
                        yield error_chunk(&model, "proxy_stream_error");
                        return;
                    }
                };
                let event_type = value
                    .get("type")
                    .and_then(Value::as_str)
                    .or(frame.event.as_deref())
                    .unwrap_or_default();
                if let Some(response) = value.get("response") {
                    observe_model(&attempt, response);
                }
                if let Some(signal) = terminal_signal(event_type) {
                    attempt.terminal(signal);
                }
                if let Some(status) = stream_failure_status(event_type, &value) {
                    attempt.failed("upstream_error");
                    let mut chunk = canonical::stream_error_chunk(&model, status, "upstream_error");
                    chunk.source_responses_event = Some(frame);
                    yield chunk;
                    return;
                }
                let terminal = frame.is_terminal();
                match state.source_chunk(&model, frame, &value) {
                    Ok(chunk) => yield chunk,
                    Err(()) => {
                        attempt.failed("proxy_stream_error");
                        yield error_chunk(&model, "proxy_stream_error");
                        return;
                    }
                }
                if terminal {
                    attempt.stream_ended();
                    return;
                }
            }
            // A transport EOF is never a successful Responses terminal event.
            attempt.failed("proxy_stream_error");
            yield error_chunk(&model, "proxy_stream_error");
        };
        Ok(Box::pin(stream))
    }
}

fn observe_model(attempt: &WireAttempt, value: &Value) {
    if !attempt.enabled() {
        return;
    }
    if let Some(model) = value.get("model") {
        match model.as_str() {
            Some(model) => attempt.reported_model(ModelSource::ResponseModel, Some(model)),
            None => attempt.rejected_model(ModelSource::ResponseModel),
        }
    }
}

fn terminal_signal(event_type: &str) -> Option<TerminalSignal> {
    match event_type {
        "response.completed" => Some(TerminalSignal::ResponsesCompleted),
        "response.incomplete" => Some(TerminalSignal::ResponsesIncomplete),
        "response.failed" => Some(TerminalSignal::ResponsesFailed),
        "response.cancelled" => Some(TerminalSignal::ResponsesCancelled),
        _ => None,
    }
}

#[derive(Default)]
#[allow(non_camel_case_types)]
struct Responses_stream_state {
    response_id: Option<String>,
    items: Vec<Responses_item_state>,
    output_indices: HashMap<u64, usize>,
    item_ids: HashMap<String, usize>,
    next_tool_index: usize,
}

#[derive(Default)]
#[allow(non_camel_case_types)]
struct Responses_item_state {
    parts: HashMap<(&'static str, u64), Responses_part_state>,
    tool: Option<Responses_tool_state>,
}

#[derive(Default)]
#[allow(non_camel_case_types)]
struct Responses_part_state {
    delta_seen: bool,
    done: bool,
}

#[derive(Default)]
#[allow(non_camel_case_types)]
struct Responses_tool_state {
    index: usize,
    call_id: Option<String>,
    name: Option<String>,
    arguments: Responses_part_state,
}

impl Responses_stream_state {
    fn item_index(&mut self, value: &Value, item: Option<&Value>) -> Result<usize, ()> {
        let output_index = value.get("output_index").and_then(Value::as_u64);
        let item_id = value
            .get("item_id")
            .or_else(|| item.and_then(|item| item.get("id")))
            .and_then(Value::as_str);
        if output_index.is_none() && item_id.is_none() {
            return Err(());
        }
        let by_output = output_index.and_then(|index| self.output_indices.get(&index).copied());
        let by_id = item_id.and_then(|id| self.item_ids.get(id).copied());
        if matches!((by_output, by_id), (Some(left), Some(right)) if left != right) {
            return Err(());
        }
        let index = by_output.or(by_id).unwrap_or_else(|| {
            self.items.push(Responses_item_state::default());
            self.items.len() - 1
        });
        if let Some(output_index) = output_index {
            self.output_indices.insert(output_index, index);
        }
        if let Some(item_id) = item_id {
            self.item_ids.insert(item_id.to_string(), index);
        }
        Ok(index)
    }

    fn project_part(
        &mut self,
        index: usize,
        channel: &'static str,
        part_index: u64,
        text: &str,
        done: bool,
        delta: &mut StreamDelta,
    ) -> Result<(), ()> {
        let part = self.items[index]
            .parts
            .entry((channel, part_index))
            .or_default();
        let emit = project_fragment(part, text, done)?;
        if !emit || text.is_empty() {
            return Ok(());
        }
        match channel {
            "content" => delta.content.get_or_insert_with(String::new).push_str(text),
            "refusal" => {
                let value = delta
                    .extra
                    .entry("refusal".into())
                    .or_insert_with(|| Value::String(String::new()));
                let mut content = value.as_str().ok_or(())?.to_string();
                content.push_str(text);
                *value = Value::String(content);
            }
            _ => delta
                .reasoning
                .get_or_insert_with(String::new)
                .push_str(text),
        }
        Ok(())
    }

    fn project_tool(
        &mut self,
        index: usize,
        item: &Value,
        arguments: Option<&str>,
        done: bool,
        delta: &mut StreamDelta,
    ) -> Result<(), ()> {
        let tool = self.items[index].tool.get_or_insert_with(|| {
            let index = self.next_tool_index;
            self.next_tool_index += 1;
            Responses_tool_state {
                index,
                ..Default::default()
            }
        });
        let mut call = serde_json::json!({"index": tool.index});
        let mut function = serde_json::Map::new();
        if let Some(call_id) = item.get("call_id").and_then(Value::as_str) {
            match &tool.call_id {
                Some(previous) if previous != call_id => return Err(()),
                None => {
                    call["id"] = Value::String(call_id.to_string());
                    call["type"] = Value::String("function".into());
                    tool.call_id = Some(call_id.to_string());
                }
                _ => {}
            }
        }
        if let Some(name) = item.get("name").and_then(Value::as_str) {
            match &tool.name {
                Some(previous) if previous != name => return Err(()),
                None => {
                    function.insert("name".into(), Value::String(name.to_string()));
                    tool.name = Some(name.to_string());
                }
                _ => {}
            }
        }
        if let Some(arguments) = arguments {
            if !arguments.is_empty() && (tool.call_id.is_none() || tool.name.is_none()) {
                return Err(());
            }
            if project_fragment(&mut tool.arguments, arguments, done)? && !arguments.is_empty() {
                function.insert("arguments".into(), Value::String(arguments.to_string()));
            }
        }
        if !function.is_empty() {
            call["function"] = Value::Object(function);
        }
        if call.as_object().is_some_and(|call| call.len() > 1) {
            delta.tool_calls.get_or_insert_with(Vec::new).push(call);
        }
        Ok(())
    }

    fn project_item(
        &mut self,
        value: &Value,
        item: &Value,
        done: bool,
        delta: &mut StreamDelta,
    ) -> Result<(), ()> {
        let index = self.item_index(value, Some(item))?;
        match item.get("type").and_then(Value::as_str) {
            Some("function_call") => self.project_tool(
                index,
                item,
                item.get("arguments")
                    .and_then(Value::as_str)
                    .filter(|arguments| done || !arguments.is_empty()),
                done,
                delta,
            )?,
            Some("message") if done => {
                if let Some(parts) = item.get("content").and_then(Value::as_array) {
                    for (part_index, part) in parts.iter().enumerate() {
                        let (channel, field) = match part.get("type").and_then(Value::as_str) {
                            Some("output_text") => ("content", "text"),
                            Some("refusal") => ("refusal", "refusal"),
                            _ => continue,
                        };
                        let text = part.get(field).and_then(Value::as_str).ok_or(())?;
                        self.project_part(index, channel, part_index as u64, text, true, delta)?;
                    }
                }
            }
            Some("reasoning") if done => {
                if let Some(parts) = item.get("summary").and_then(Value::as_array) {
                    for (part_index, part) in parts.iter().enumerate() {
                        let text = part.get("text").and_then(Value::as_str).ok_or(())?;
                        self.project_part(
                            index,
                            "reasoning_summary",
                            part_index as u64,
                            text,
                            true,
                            delta,
                        )?;
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn source_chunk(
        &mut self,
        model: &str,
        frame: ResponsesSseEvent,
        value: &Value,
    ) -> Result<OAIStreamChunk, ()> {
        let event_type = value
            .get("type")
            .and_then(Value::as_str)
            .or(frame.event.as_deref())
            .unwrap_or_default();
        if let Some(id) = value
            .pointer("/response/id")
            .or_else(|| value.get("response_id"))
            .and_then(Value::as_str)
        {
            self.response_id = Some(id.to_string());
        }
        let mut delta = StreamDelta::default();
        match event_type {
            "response.output_item.added" | "response.output_item.done" => {
                let item = value.get("item").ok_or(())?;
                self.project_item(value, item, event_type.ends_with(".done"), &mut delta)?;
            }
            "response.output_text.delta"
            | "response.output_text.done"
            | "response.refusal.delta"
            | "response.refusal.done"
            | "response.reasoning_text.delta"
            | "response.reasoning_text.done"
            | "response.reasoning_summary_text.delta"
            | "response.reasoning_summary_text.done" => {
                let index = self.item_index(value, None)?;
                let done = event_type.ends_with(".done");
                let channel = if event_type.starts_with("response.output_text.") {
                    "content"
                } else if event_type.starts_with("response.refusal.") {
                    "refusal"
                } else if event_type.starts_with("response.reasoning_summary_text.") {
                    "reasoning_summary"
                } else {
                    "reasoning"
                };
                let field = if !done {
                    "delta"
                } else if channel == "refusal" {
                    "refusal"
                } else {
                    "text"
                };
                let text = value.get(field).and_then(Value::as_str).ok_or(())?;
                let part_index = value
                    .get("content_index")
                    .or_else(|| value.get("summary_index"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                self.project_part(index, channel, part_index, text, done, &mut delta)?;
            }
            "response.content_part.done" | "response.reasoning_summary_part.done" => {
                let index = self.item_index(value, None)?;
                let part = value.get("part").ok_or(())?;
                let (channel, field) = match part.get("type").and_then(Value::as_str) {
                    Some("output_text") => ("content", "text"),
                    Some("refusal") => ("refusal", "refusal"),
                    Some("summary_text") => ("reasoning_summary", "text"),
                    _ => ("reasoning", "text"),
                };
                let text = part.get(field).and_then(Value::as_str).ok_or(())?;
                let part_index = value
                    .get("content_index")
                    .or_else(|| value.get("summary_index"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                self.project_part(index, channel, part_index, text, true, &mut delta)?;
            }
            "response.function_call_arguments.delta" | "response.function_call_arguments.done" => {
                let index = self.item_index(value, None)?;
                let done = event_type.ends_with(".done");
                let field = if done { "arguments" } else { "delta" };
                let arguments = value.get(field).and_then(Value::as_str).ok_or(())?;
                self.project_tool(index, value, Some(arguments), done, &mut delta)?;
            }
            "response.completed" | "response.incomplete" => {
                if let Some(output) = value.pointer("/response/output").and_then(Value::as_array) {
                    for (index, item) in output.iter().enumerate() {
                        self.project_item(
                            &serde_json::json!({"output_index": index}),
                            item,
                            true,
                            &mut delta,
                        )?;
                    }
                }
            }
            _ => {}
        }
        let finish_reason = match event_type {
            "response.completed" | "response.incomplete" => {
                let response = value.get("response").ok_or(())?;
                let status = response.get("status").and_then(Value::as_str).ok_or(())?;
                if status == "incomplete" || event_type == "response.incomplete" {
                    Some(
                        if response
                            .pointer("/incomplete_details/reason")
                            .and_then(Value::as_str)
                            == Some("content_filter")
                        {
                            "content_filter"
                        } else {
                            "length"
                        }
                        .to_string(),
                    )
                } else if status == "completed" {
                    Some(
                        if self.next_tool_index > 0 {
                            "tool_calls"
                        } else {
                            "stop"
                        }
                        .to_string(),
                    )
                } else {
                    return Err(());
                }
            }
            _ => None,
        };
        let mut extra = HashMap::new();
        if let Some(usage) = value
            .pointer("/response/usage")
            .or_else(|| value.get("usage"))
            .filter(|usage| !usage.is_null())
        {
            let input = usage
                .get("input_tokens")
                .and_then(Value::as_u64)
                .ok_or(())?;
            let output = usage
                .get("output_tokens")
                .and_then(Value::as_u64)
                .ok_or(())?;
            let total = usage
                .get("total_tokens")
                .and_then(Value::as_u64)
                .unwrap_or_else(|| input.saturating_add(output));
            let mut canonical_usage = serde_json::json!({"prompt_tokens": input, "completion_tokens": output, "total_tokens": total});
            for (source, target) in [
                ("input_tokens_details", "prompt_tokens_details"),
                ("output_tokens_details", "completion_tokens_details"),
            ] {
                if let Some(details) = usage.get(source) {
                    canonical_usage[target] = details.clone();
                }
            }
            extra.insert("usage".into(), canonical_usage);
        }
        Ok(OAIStreamChunk {
            id: self
                .response_id
                .clone()
                .unwrap_or_else(|| "responses_stream".into()),
            object: "chat.completion.chunk".into(),
            created: chrono::Utc::now().timestamp(),
            model: model.to_string(),
            choices: vec![StreamChoice {
                index: 0,
                delta,
                finish_reason,
                extra: Default::default(),
            }],
            extra,
            source_anthropic_event: None,
            source_responses_event: Some(frame),
            source_gemini_event: None,
        })
    }
}

fn project_fragment(part: &mut Responses_part_state, text: &str, done: bool) -> Result<bool, ()> {
    if part.done {
        return if done { Ok(false) } else { Err(()) };
    }
    if done {
        part.done = true;
        // Done frames contain the whole part, not an additional text delta.
        Ok(!part.delta_seen)
    } else {
        part.delta_seen |= !text.is_empty();
        Ok(true)
    }
}

fn stream_failure_status(event_type: &str, value: &Value) -> Option<u16> {
    let response = value.get("response").unwrap_or(value);
    let status = response.get("status").and_then(Value::as_str);
    if event_type == "response.cancelled" || status == Some("cancelled") {
        return Some(499);
    }
    if matches!(event_type, "response.failed" | "error")
        || status == Some("failed")
        || response.get("error").is_some_and(|error| !error.is_null())
        || value.get("error").is_some_and(|error| !error.is_null())
    {
        let failed = if value.get("error").is_some_and(|error| !error.is_null()) {
            value
        } else {
            response
        };
        return responses_failure_error(failed).status;
    }
    None
}

fn error_chunk(model: &str, kind: &str) -> OAIStreamChunk {
    let mut chunk = canonical::stream_error_chunk(model, 502, kind);
    chunk.source_responses_event = None;
    chunk
}

/// A Chat-originating request cannot use the native sidecar: the lossy
/// direction is explicit and fails before any network request.
pub fn validate_chat_request_for_responses(
    req: &ChatCompletionRequest,
) -> Result<(), ProviderError> {
    if req.source_responses_request.is_some() {
        return Ok(());
    }
    canonical_to_responses_request_value(req, &req.model, req.stream).map(|_| ())
}

#[cfg(test)]
mod minimax_state_tests {
    use super::*;
    use serde_json::json;

    fn provider(base: &str) -> ResponsesProvider {
        ResponsesProvider::new(
            "fixture".into(),
            base.into(),
            "fixture".into(),
            vec!["MiniMax-M3.1-Flash".into()],
            true,
            1000,
            CircuitBreakerConfig::default(),
            HashMap::new(),
        )
        .unwrap()
    }

    #[test]
    fn stateless_minimax_rejects_ignored_linkage_before_any_send() {
        let provider = provider("https://api.minimaxi.com/v1");
        for state in [
            json!({"previous_response_id":"resp_previous"}),
            json!({"conversation":"conv_previous"}),
        ] {
            let mut body = json!({"model":"gpt-5.6-sol","input":"continue"});
            body.as_object_mut()
                .unwrap()
                .extend(state.as_object().unwrap().clone());
            let source = serde_json::from_value(body).unwrap();
            let request = ChatCompletionRequest {
                source_responses_request: Some(source),
                ..Default::default()
            };
            let error = provider
                .request_body(&request, "MiniMax-M3.1-Flash", false)
                .unwrap_err();
            assert_eq!(error.code, "responses_upstream_state_unsupported");
            assert!(!error.retriable);
        }
    }

    #[test]
    fn stateless_minimax_accepts_materialized_history_and_other_providers_keep_linkage() {
        let mut source: crate::proxy::responses::ResponsesRequest = serde_json::from_value(json!({
            "model":"gpt-5.6-sol","input":[{"role":"user","content":"hello"}],"store":false
        }))
        .unwrap();
        let request = crate::proxy::converter::responses_to_chat(&source);
        assert!(provider("https://api.minimax.io/v1")
            .request_body(&request, "MiniMax-M3.1-Flash", false)
            .is_ok());
        source.previous_response_id = Some("resp_previous".into());
        let request = crate::proxy::converter::responses_to_chat(&source);
        let (bytes, wire_model) = provider("https://example.invalid/v1")
            .request_body(&request, "model[1M]", false)
            .unwrap();
        assert_eq!(wire_model, "model");
        assert_eq!(
            serde_json::from_slice::<Value>(&bytes).unwrap()["model"],
            "model"
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&bytes).unwrap()["previous_response_id"],
            "resp_previous"
        );
    }
}
