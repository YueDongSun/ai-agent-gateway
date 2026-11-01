//! Streaming conversion between canonical OpenAI chunks and Anthropic SSE
//! lines, including tool-call delta block state and stream usage
//! normalization. Extracted from the former converter.rs monolith.

use super::anthropic::{rand_hex, stop_from_anthropic, stop_to_anthropic};
use crate::proxy::canonical::{OAIStreamChunk, StreamChoice, StreamDelta};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

fn sse(event: &str, data: Value) -> String {
    format!("event: {event}\ndata: {}\n\n", data)
}


// ============================================================================
// SSE stream conversion: OAI chunks -> Anthropic SSE lines
// ============================================================================

/// State for one in-flight tool_call delta stream (keyed by OpenAI
/// `tool_calls.index`). Mirrors `cc-switch/providers/streaming.rs`'s
/// `ToolBlockState`. The `anthropic_index` is allocated when the
/// upstream first mentions the OpenAI index, *before* we have the id /
/// name — so we can't emit `content_block_start` yet. We buffer the
/// pre-start `function.arguments` fragments into `pending_args` and
/// flush them as `input_json_delta` once start is emitted.
#[derive(Debug, Default, Clone)]
struct ToolBlockState {
    anthropic_index: u32,
    id: String,
    name: String,
    started: bool,
    pending_args: String,
}

/// Build the Anthropic `usage` JSON from a top-level usage block, undoing
/// OpenAI's "cache tokens counted in prompt_tokens" convention.
fn build_anthropic_usage_from_stream_extra(extra: &HashMap<String, Value>) -> Option<Value> {
    let usage = extra.get("usage")?;
    if usage.get("input_tokens").is_some() || usage.get("output_tokens").is_some() {
        let mut normalized = json!({
            "input_tokens": usage.get("input_tokens").and_then(Value::as_u64).unwrap_or(0),
            "output_tokens": usage.get("output_tokens").and_then(Value::as_u64).unwrap_or(0),
        });
        for field in ["cache_read_input_tokens", "cache_creation_input_tokens"] {
            if let Some(value) = usage.get(field).and_then(Value::as_u64) {
                if value > 0 {
                    normalized[field] = json!(value);
                }
            }
        }
        return Some(normalized);
    }
    let prompt = usage
        .get("prompt_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    let completion = usage
        .get("completion_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    let cached = usage
        .get("prompt_tokens_details")
        .and_then(|d| d.get("cached_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    let cache_creation = usage
        .get("prompt_tokens_details")
        .and_then(|d| d.get("cache_write_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    let input = prompt.saturating_sub(cached).saturating_sub(cache_creation);
    let mut u = json!({
        "input_tokens": input,
        "output_tokens": completion,
    });
    if cached > 0 {
        u["cache_read_input_tokens"] = json!(cached);
    }
    if cache_creation > 0 {
        u["cache_creation_input_tokens"] = json!(cache_creation);
    }
    Some(u)
}

fn anthropic_usage_to_oai_usage(usage: &Value, previous: Option<&Value>) -> Value {
    let previous_prompt = previous
        .and_then(|value| value.get("prompt_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let previous_completion = previous
        .and_then(|value| value.get("completion_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let previous_cached = previous
        .and_then(|value| value.get("prompt_tokens_details"))
        .and_then(|value| value.get("cached_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let previous_cache_write = previous
        .and_then(|value| value.get("prompt_tokens_details"))
        .and_then(|value| value.get("cache_write_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);

    let input = usage
        .get("input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or_else(|| previous_prompt.saturating_sub(previous_cached + previous_cache_write));
    let completion = usage
        .get("output_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(previous_completion);
    let cached = usage
        .get("cache_read_input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(previous_cached);
    let cache_write = usage
        .get("cache_creation_input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(previous_cache_write);
    let prompt = input + cached + cache_write;

    json!({
        "prompt_tokens": prompt,
        "completion_tokens": completion,
        "total_tokens": prompt + completion,
        "prompt_tokens_details": {
            "cached_tokens": cached,
            "cache_write_tokens": cache_write,
        },
    })
}

pub fn oai_chunks_to_anthropic_stream(
    chunks: impl futures_core::Stream<Item = OAIStreamChunk> + Send + 'static,
    model: String,
) -> std::pin::Pin<Box<dyn futures_core::Stream<Item = String> + Send>> {
    let msg_id = format!("msg_{}", rand_hex(12));
    async_stream::stream! {
        let mut chunks = std::pin::pin!(chunks);
        let mut message_started = false;
        let mut text_block_open = false;
        let mut thinking_block_open = false;
        let mut next_content_index: u32 = 0;
        // Some upstreams send multiple finish_reason chunks; only emit one
        // message_delta. Cache it until we see [DONE] or stream end so
        // usage (if any) can be filled in.
        let mut pending_message_delta: Option<Value> = None;
        let mut has_emitted_message_delta = false;
        let mut tool_blocks: HashMap<usize, ToolBlockState> = HashMap::new();
        let mut open_tool_indices: HashSet<u32> = HashSet::new();
        let mut latest_usage: Option<Value> = None;
        let mut source_message_stop_emitted = false;

        while let Some(mut ch) = chunks.next().await {
            if let Some(mut event) = ch.source_anthropic_event.take() {
                let event_type = event
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("message")
                    .to_string();
                if event_type == "message_start" {
                    message_started = true;
                    if let Some(message) = event.get_mut("message") {
                        message["model"] = json!(model.clone());
                    }
                }
                if event_type == "message_stop" {
                    source_message_stop_emitted = true;
                }
                yield sse(&event_type, event);
                if event_type == "error" {
                    return;
                }
                continue;
            }

            if let Some(error) = ch.extra.get("error") {
                yield sse("error", json!({"type": "error", "error": error}));
                return;
            }

            // Capture latest usage so the final message_delta can carry it.
            if let Some(u) = build_anthropic_usage_from_stream_extra(&ch.extra) {
                latest_usage = Some(u.clone());
                if let Some(existing) = pending_message_delta.as_mut() {
                    if let Some(usage_field) = existing.get_mut("usage") {
                        *usage_field = u;
                    }
                }
            }

            // message_start on first chunk.
            if !message_started {
                message_started = true;
                let mut start_usage = json!({ "input_tokens": 0, "output_tokens": 0 });
                if let Some(u) = build_anthropic_usage_from_stream_extra(&ch.extra) {
                    start_usage = u;
                }
                yield sse("message_start", json!({
                    "type": "message_start",
                    "message": {
                        "id": msg_id.clone(),
                        "type": "message",
                        "role": "assistant",
                        "model": model.clone(),
                        "content": [],
                        "stop_reason": null,
                        "stop_sequence": null,
                        "usage": start_usage,
                    }
                }));
            }

            let finish_reason = ch
                .choices
                .first()
                .and_then(|c| c.finish_reason.clone());

            for choice in &ch.choices {
                let d = &choice.delta;

                // ---- role (just signals first chunk in OAI) ----
                if let Some(role) = &d.role {
                    if role == "assistant" && !text_block_open && !thinking_block_open {
                        // Don't pre-open; let content drive the block type.
                    }
                }

                // ---- reasoning / thinking delta (DeepSeek/Kimi/Anthropic
                // passthrough via `reasoning` field) ----
                if let Some(thinking) = d.reasoning_text() {
                    if !thinking.is_empty() {
                        if text_block_open {
                            yield sse("content_block_stop", json!({
                                "type": "content_block_stop",
                                "index": next_content_index - 1,
                            }));
                            text_block_open = false;
                        }
                        if !thinking_block_open {
                            let idx = next_content_index;
                            next_content_index += 1;
                            yield sse("content_block_start", json!({
                                "type": "content_block_start",
                                "index": idx,
                                "content_block": { "type": "thinking", "thinking": "" },
                            }));
                            thinking_block_open = true;
                        }
                        yield sse("content_block_delta", json!({
                            "type": "content_block_delta",
                            "index": next_content_index - 1,
                            "delta": { "type": "thinking_delta", "thinking": thinking },
                        }));
                    }
                }

                // ---- text delta ----
                if let Some(text) = &d.content {
                    if !text.is_empty() {
                        if thinking_block_open {
                            yield sse("content_block_stop", json!({
                                "type": "content_block_stop",
                                "index": next_content_index - 1,
                            }));
                            thinking_block_open = false;
                        }
                        if !text_block_open {
                            let idx = next_content_index;
                            next_content_index += 1;
                            yield sse("content_block_start", json!({
                                "type": "content_block_start",
                                "index": idx,
                                "content_block": { "type": "text", "text": "" },
                            }));
                            text_block_open = true;
                        }
                        yield sse("content_block_delta", json!({
                            "type": "content_block_delta",
                            "index": next_content_index - 1,
                            "delta": { "type": "text_delta", "text": text },
                        }));
                    }
                }

                // ---- tool_calls delta ----
                if let Some(tcs) = &d.tool_calls {
                    // A tool index must never replace an open text/thinking
                    // index: close those blocks before allocating the tool.
                    if !tcs.is_empty() && (text_block_open || thinking_block_open) {
                        yield sse("content_block_stop", json!({
                            "type": "content_block_stop", "index": next_content_index - 1,
                        }));
                        text_block_open = false;
                        thinking_block_open = false;
                    }
                    for tc in tcs {
                        // OpenAI tool_calls entries may have `index` as a
                        // usize or as a string ("0") in some providers.
                        let oai_index = tc
                            .get("index")
                            .and_then(|v| match v {
                                Value::Number(n) => n.as_u64(),
                                Value::String(s) => s.parse::<u64>().ok(),
                                _ => None,
                            })
                            .unwrap_or(0) as usize;
                        let state = tool_blocks.entry(oai_index).or_insert_with(|| {
                            let idx = next_content_index;
                            next_content_index += 1;
                            ToolBlockState {
                                anthropic_index: idx,
                                ..Default::default()
                            }
                        });
                        let id = tc
                            .get("id")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                        let name = tc
                            .get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                        let args_delta = tc
                            .get("function")
                            .and_then(|f| f.get("arguments"))
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                        if let Some(id) = id { state.id = id; }
                        if let Some(name) = name { state.name = name; }
                        if !state.started && !state.id.is_empty() && !state.name.is_empty() {
                            // Emit content_block_start + buffered args.
                            yield sse("content_block_start", json!({
                                "type": "content_block_start",
                                "index": state.anthropic_index,
                                "content_block": {
                                    "type": "tool_use",
                                    "id": state.id.clone(),
                                    "name": state.name.clone(),
                                    "input": {},
                                }
                            }));
                            open_tool_indices.insert(state.anthropic_index);
                            state.started = true;
                            if !state.pending_args.is_empty() {
                                let pending = std::mem::take(&mut state.pending_args);
                                yield sse("content_block_delta", json!({
                                    "type": "content_block_delta",
                                    "index": state.anthropic_index,
                                    "delta": {
                                        "type": "input_json_delta",
                                        "partial_json": pending,
                                    }
                                }));
                            }
                        }
                        if let Some(args) = args_delta {
                            if state.started {
                                yield sse("content_block_delta", json!({
                                    "type": "content_block_delta",
                                    "index": state.anthropic_index,
                                    "delta": {
                                        "type": "input_json_delta",
                                        "partial_json": args,
                                    }
                                }));
                            } else {
                                state.pending_args.push_str(&args);
                            }
                        }
                    }
                }
            }

            // ---- finish_reason handling ----
            if let Some(fr) = finish_reason {
                let stop_reason = stop_to_anthropic(&fr).to_string();
                if has_emitted_message_delta {
                    continue;
                }
                has_emitted_message_delta = true;

                // Close any open text/thinking block.
                if text_block_open {
                    yield sse("content_block_stop", json!({
                        "type": "content_block_stop",
                        "index": next_content_index - 1,
                    }));
                    text_block_open = false;
                }
                if thinking_block_open {
                    yield sse("content_block_stop", json!({
                        "type": "content_block_stop",
                        "index": next_content_index - 1,
                    }));
                    thinking_block_open = false;
                }

                // Late-start any tool blocks that never got id/name.
                let mut late_starts: Vec<(u32, String, String, String)> = Vec::new();
                for (idx, state) in tool_blocks.iter_mut() {
                    if state.started { continue; }
                    let has_payload = !state.pending_args.is_empty()
                        || !state.id.is_empty()
                        || !state.name.is_empty();
                    if !has_payload { continue; }
                    let fallback_id = if state.id.is_empty() {
                        format!("tool_call_{idx}")
                    } else { state.id.clone() };
                    let fallback_name = if state.name.is_empty() {
                        "unknown_tool".to_string()
                    } else { state.name.clone() };
                    state.started = true;
                    let pending = std::mem::take(&mut state.pending_args);
                    late_starts.push((
                        state.anthropic_index,
                        fallback_id,
                        fallback_name,
                        pending,
                    ));
                }
                late_starts.sort_by_key(|(idx, _, _, _)| *idx);
                for (idx, id, name, pending) in late_starts {
                    yield sse("content_block_start", json!({
                        "type": "content_block_start",
                        "index": idx,
                        "content_block": {
                            "type": "tool_use",
                            "id": id,
                            "name": name,
                        }
                    }));
                    open_tool_indices.insert(idx);
                    if !pending.is_empty() {
                        yield sse("content_block_delta", json!({
                            "type": "content_block_delta",
                            "index": idx,
                            "delta": {
                                "type": "input_json_delta",
                                "partial_json": pending,
                            }
                        }));
                    }
                }

                // Close all open tool blocks.
                let mut sorted_indices: Vec<u32> = open_tool_indices.iter().copied().collect();
                sorted_indices.sort_unstable();
                for idx in sorted_indices {
                    yield sse("content_block_stop", json!({
                        "type": "content_block_stop",
                        "index": idx,
                    }));
                }
                open_tool_indices.clear();

                // Build message_delta; defer until [DONE] so usage is final.
                let mut usage_field = latest_usage.clone().unwrap_or(json!({
                    "input_tokens": 0,
                    "output_tokens": 0,
                }));
                if usage_field.get("input_tokens").is_none() {
                    usage_field["input_tokens"] = json!(0);
                }
                if usage_field.get("output_tokens").is_none() {
                    usage_field["output_tokens"] = json!(0);
                }
                let md = json!({
                    "type": "message_delta",
                    "delta": { "stop_reason": stop_reason, "stop_sequence": null },
                    "usage": usage_field,
                });
                pending_message_delta = Some(md);
            }
        }

        // Stream ended — flush deferred message_delta + message_stop.
        if let Some(md) = pending_message_delta.take() {
            yield sse("message_delta", md);
        }
        if !source_message_stop_emitted {
            yield sse("message_stop", json!({ "type": "message_stop" }));
        }
    }
    .boxed()
}

/// Convenience: build an OAIStreamChunk (used by mock streaming).
pub fn make_stream_chunk(
    id: &str,
    model: &str,
    delta: StreamDelta,
    finish_reason: Option<String>,
) -> OAIStreamChunk {
    OAIStreamChunk {
        id: id.into(),
        object: "chat.completion.chunk".into(),
        created: chrono::Utc::now().timestamp(),
        model: model.into(),
        choices: vec![StreamChoice {
            index: 0,
            delta,
            finish_reason,
            extra: Default::default(),
        }],
        extra: Default::default(),
        source_anthropic_event: None,
        source_gemini_event: None,
        source_responses_event: None,
    }
}

// ============================================================================
// Anthropic SSE events -> canonical OAI stream chunks
// ============================================================================

/// Parse Anthropic SSE event payloads (data strings without the `data:` prefix)
/// into canonical OAI stream chunks. Handles `tool_use` start/stop,
/// `input_json_delta`, text/thinking deltas, and `message_delta` finish reason.
fn anthropic_source_chunk(
    id: &str,
    created: i64,
    model: &str,
    source_event: Value,
    delta: Option<StreamDelta>,
    finish_reason: Option<String>,
    extra: HashMap<String, Value>,
) -> OAIStreamChunk {
    let choices = delta
        .map(|delta| {
            vec![StreamChoice {
                index: 0,
                delta,
                finish_reason,
                extra: Default::default(),
            }]
        })
        .unwrap_or_default();
    OAIStreamChunk {
        id: id.to_string(),
        object: "chat.completion.chunk".into(),
        created,
        model: model.to_string(),
        choices,
        extra,
        source_anthropic_event: Some(source_event),
        source_gemini_event: None,
        source_responses_event: None,
    }
}

pub fn anthropic_stream_to_oai_chunks(
    data_strings: impl futures_core::Stream<Item = String> + Send + 'static,
    model: String,
) -> std::pin::Pin<Box<dyn futures_core::Stream<Item = OAIStreamChunk> + Send>> {
    let id = format!("chatcmpl-{}", rand_hex(6));
    let created = chrono::Utc::now().timestamp();
    async_stream::stream! {
        let mut data_strings = std::pin::pin!(data_strings);
        let mut tool_index_to_oai_index: HashMap<u32, usize> = HashMap::new();
        let mut next_oai_index: usize = 0;
        let mut latest_usage: Option<Value> = None;
        let mut received_event = false;

        while let Some(data) = data_strings.next().await {
            let event: Value = match serde_json::from_str(&data) {
                Ok(value) => value,
                Err(_) => {
                    yield crate::proxy::canonical::stream_error_chunk(&model, 502, "proxy_stream_error");
                    return;
                }
            };
            let event_type = event
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            received_event = true;
            if event_type == "error" {
                let status = crate::proxy::canonical::stream_error_status(&event);
                let mut chunk = crate::proxy::canonical::stream_error_chunk(&model, status, "upstream_error");
                chunk.source_anthropic_event = Some(event);
                yield chunk;
                return;
            }
            let mut extra = HashMap::new();
            let mut delta: Option<StreamDelta> = None;
            let mut finish_reason: Option<String> = None;

            match event_type.as_str() {
                "message_start" => {
                    if let Some(usage) = event.get("message").and_then(|message| message.get("usage")) {
                        let normalized = anthropic_usage_to_oai_usage(usage, latest_usage.as_ref());
                        latest_usage = Some(normalized.clone());
                        extra.insert("usage".into(), normalized);
                    }
                    delta = Some(StreamDelta {
                        role: Some("assistant".into()),
                        content: None,
                        tool_calls: None,
                        reasoning: None,
                        extra: Default::default(),
                    });
                }
                "content_block_start" => {
                    let content_block = event.get("content_block").cloned().unwrap_or(Value::Null);
                    if content_block.get("type").and_then(Value::as_str) == Some("tool_use") {
                        let anthro_index = event.get("index").and_then(Value::as_u64).unwrap_or(0) as u32;
                        let oai_index = *tool_index_to_oai_index.entry(anthro_index).or_insert_with(|| {
                            let index = next_oai_index;
                            next_oai_index += 1;
                            index
                        });
                        delta = Some(StreamDelta {
                            role: None,
                            content: None,
                            tool_calls: Some(vec![json!({
                                "index": oai_index,
                                "id": content_block.get("id").and_then(Value::as_str).unwrap_or(""),
                                "type": "function",
                                "function": {
                                    "name": content_block.get("name").and_then(Value::as_str).unwrap_or(""),
                                },
                            })]),
                            reasoning: None,
                            extra: Default::default(),
                        });
                    }
                }
                "content_block_delta" => {
                    let source_delta = event.get("delta").cloned().unwrap_or(Value::Null);
                    let anthro_index = event.get("index").and_then(Value::as_u64).unwrap_or(0) as u32;
                    match source_delta.get("type").and_then(Value::as_str).unwrap_or("") {
                        "text_delta" => {
                            delta = source_delta.get("text").and_then(Value::as_str).map(|text| StreamDelta {
                                role: None,
                                content: Some(text.to_string()),
                                tool_calls: None,
                                reasoning: None,
                                extra: Default::default(),
                            });
                        }
                        "thinking_delta" => {
                            delta = source_delta.get("thinking").and_then(Value::as_str).map(|thinking| StreamDelta {
                                role: None,
                                content: None,
                                tool_calls: None,
                                reasoning: Some(thinking.to_string()),
                                extra: Default::default(),
                            });
                        }
                        "input_json_delta" => {
                            let oai_index = *tool_index_to_oai_index.entry(anthro_index).or_insert_with(|| {
                                let index = next_oai_index;
                                next_oai_index += 1;
                                index
                            });
                            delta = source_delta.get("partial_json").and_then(Value::as_str).map(|partial_json| StreamDelta {
                                role: None,
                                content: None,
                                tool_calls: Some(vec![json!({
                                    "index": oai_index,
                                    "function": { "arguments": partial_json },
                                })]),
                                reasoning: None,
                                extra: Default::default(),
                            });
                        }
                        _ => {}
                    }
                }
                "message_delta" => {
                    finish_reason = event
                        .get("delta")
                        .and_then(|value| value.get("stop_reason"))
                        .and_then(Value::as_str)
                        .map(|reason| stop_from_anthropic(reason).to_string());
                    if let Some(usage) = event.get("usage") {
                        let normalized = anthropic_usage_to_oai_usage(usage, latest_usage.as_ref());
                        latest_usage = Some(normalized.clone());
                        extra.insert("usage".into(), normalized);
                    }
                    delta = Some(StreamDelta {
                        role: None,
                        content: None,
                        tool_calls: None,
                        reasoning: None,
                        extra: Default::default(),
                    });
                }
                _ => {}
            }

            let should_stop = event_type == "message_stop";
            yield anthropic_source_chunk(
                &id,
                created,
                &model,
                event,
                delta,
                finish_reason,
                extra,
            );
            if should_stop {
                return;
            }
        }
        if received_event {
            yield crate::proxy::canonical::stream_error_chunk(&model, 502, "proxy_stream_error");
        }
    }
    .boxed()
}
