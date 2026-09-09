//! Bidirectional OpenAI <-> Anthropic format conversion.
//!
//! Ported from `src/gateway/converter.ts`. Two directions:
//! - Gateway edge: Anthropic client request -> canonical OpenAI -> provider,
//!   then OpenAI response -> Anthropic response back to client.
//! - Anthropic provider: canonical OpenAI request -> Anthropic backend request,
//!   then Anthropic response -> OpenAI response back to the router.

use crate::proxy::canonical::{
    ChatCompletionRequest, ChatMessage, OAIChatResponse, OAIChoice, OAIMessage, OAIStreamChunk,
    StreamChoice, StreamDelta, Usage,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Anthropic Messages API request shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicRequest {
    pub model: String,
    pub max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<Value>, // string or array of {type, text}
    pub messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "stop_sequences")]
    pub stop_sequences: Option<Vec<String>>,
    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicMessage {
    pub role: String, // "user" | "assistant"
    pub content: Value, // string or array of {type, text}
}

/// Anthropic Messages API response shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicResponse {
    pub id: String,
    #[serde(rename = "type")]
    pub resp_type: String, // "message"
    pub role: String, // "assistant"
    pub model: String,
    pub content: Vec<AnthropicContentBlock>,
    #[serde(rename = "stop_reason")]
    pub stop_reason: Option<String>,
    #[serde(rename = "stop_sequence")]
    pub stop_sequence: Option<String>,
    pub usage: AnthropicUsage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicContentBlock {
    #[serde(rename = "type")]
    pub block_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicUsage {
    #[serde(rename = "input_tokens")]
    pub input_tokens: u32,
    #[serde(rename = "output_tokens")]
    pub output_tokens: u32,
}

// ----- stop-reason mapping -----

fn stop_to_anthropic(stop: &str) -> &'static str {
    match stop {
        "stop" => "end_turn",
        "length" => "max_tokens",
        "tool_calls" => "tool_use",
        "content_filter" => "end_turn",
        _ => "end_turn",
    }
}

fn stop_from_anthropic(reason: &str) -> &'static str {
    match reason {
        "end_turn" => "stop",
        "max_tokens" => "length",
        "stop_sequence" => "stop",
        "tool_use" => "tool_calls",
        _ => "stop",
    }
}

// ----- Canonical OpenAI -> Anthropic backend (provider side) -----

pub fn oai_req_to_anthropic_req(req: &ChatCompletionRequest) -> AnthropicRequest {
    let mut system_parts: Vec<String> = Vec::new();
    let mut msgs: Vec<AnthropicMessage> = Vec::new();

    for m in &req.messages {
        match m.role.as_str() {
            "system" => system_parts.push(text_of(&m.content)),
            "user" | "assistant" => {
                let text = text_of(&m.content);
                msgs.push(AnthropicMessage {
                    role: m.role.clone(),
                    content: json!([{ "type": "text", "text": text }]),
                });
            }
            _ => {
                // tool/function messages merged as user text
                msgs.push(AnthropicMessage {
                    role: "user".into(),
                    content: json!([{ "type": "text", "text": text_of(&m.content) }]),
                });
            }
        }
    }

    let max_tokens = req.max_tokens.unwrap_or(4096);
    let mut out = AnthropicRequest {
        model: req.model.clone(),
        max_tokens,
        system: None,
        messages: msgs,
        temperature: req.temperature,
        stream: if req.stream { Some(true) } else { None },
        stop_sequences: None,
        extra: Default::default(),
    };
    if !system_parts.is_empty() {
        out.system = Some(Value::String(system_parts.join("\n\n")));
    }
    out
}

pub fn anthropic_resp_to_oai_resp(
    resp: &AnthropicResponse,
    requested_model: &str,
) -> OAIChatResponse {
    let text: String = resp
        .content
        .iter()
        .filter_map(|b| {
            if b.block_type == "text" {
                b.text.clone()
            } else {
                None
            }
        })
        .collect();
    let stop_reason = resp
        .stop_reason
        .as_deref()
        .map(|r| stop_from_anthropic(r).to_string());
    OAIChatResponse {
        id: if resp.id.is_empty() {
            format!("chatcmpl-{}", rand_hex(6))
        } else {
            resp.id.clone()
        },
        object: "chat.completion".into(),
        created: chrono::Utc::now().timestamp(),
        model: requested_model.into(),
        choices: vec![OAIChoice {
            index: 0,
            message: OAIMessage {
                role: "assistant".into(),
                content: Some(text),
                extra: Default::default(),
            },
            finish_reason: stop_reason,
        }],
        usage: Some(Usage {
            prompt_tokens: resp.usage.input_tokens,
            completion_tokens: resp.usage.output_tokens,
            total_tokens: resp.usage.input_tokens + resp.usage.output_tokens,
        }),
    }
}

// ----- Anthropic client -> Canonical OpenAI (gateway edge) -----

pub fn anthropic_req_to_oai_req(body: &AnthropicRequest) -> ChatCompletionRequest {
    let mut messages: Vec<ChatMessage> = Vec::new();
    if let Some(sys) = &body.system {
        let sys_text = match sys {
            Value::String(s) => s.clone(),
            Value::Array(parts) => parts
                .iter()
                .filter_map(|p| p.get("text").and_then(|v| v.as_str()).map(String::from))
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        };
        if !sys_text.is_empty() {
            messages.push(ChatMessage {
                role: "system".into(),
                content: Value::String(sys_text),
                extra: Default::default(),
            });
        }
    }
    for m in &body.messages {
        let text = match &m.content {
            Value::String(s) => s.clone(),
            Value::Array(parts) => parts
                .iter()
                .filter_map(|p| p.get("text").and_then(|v| v.as_str()).map(String::from))
                .collect::<Vec<_>>()
                .join(""),
            _ => m.content.to_string(),
        };
        messages.push(ChatMessage {
            role: m.role.clone(),
            content: Value::String(text),
            extra: Default::default(),
        });
    }
    ChatCompletionRequest {
        model: body.model.clone(),
        messages,
        stream: body.stream.unwrap_or(false),
        temperature: body.temperature,
        max_tokens: Some(body.max_tokens),
        extra: Default::default(),
    }
}

pub fn oai_resp_to_anthropic_resp(
    resp: &OAIChatResponse,
    requested_model: &str,
) -> AnthropicResponse {
    let text = resp
        .choices
        .first()
        .and_then(|c| c.message.content.clone())
        .unwrap_or_default();
    let stop = resp
        .choices
        .first()
        .and_then(|c| c.finish_reason.as_deref())
        .unwrap_or("stop");
    AnthropicResponse {
        id: resp.id.clone(),
        resp_type: "message".into(),
        role: "assistant".into(),
        model: requested_model.into(),
        content: vec![AnthropicContentBlock {
            block_type: "text".into(),
            text: Some(text),
            extra: Default::default(),
        }],
        stop_reason: Some(stop_to_anthropic(stop).into()),
        stop_sequence: None,
        usage: AnthropicUsage {
            input_tokens: resp.usage.as_ref().map(|u| u.prompt_tokens).unwrap_or(0),
            output_tokens: resp.usage.as_ref().map(|u| u.completion_tokens).unwrap_or(0),
        },
    }
}

// ----- SSE stream conversion: OAI chunks -> Anthropic SSE lines -----

/// Convert a stream of canonical OAI chunks into Anthropic SSE event lines.
/// Each yielded String is a complete SSE block: `event: X\ndata: {...}\n\n`.
pub fn oai_chunks_to_anthropic_stream(
    chunks: impl futures_core::Stream<Item = OAIStreamChunk> + Send + 'static,
    model: String,
) -> std::pin::Pin<Box<dyn futures_core::Stream<Item = String> + Send>> {
    use futures_util::StreamExt;
    let msg_id = format!("msg_{}", rand_hex(12));
    async_stream::stream! {
        let mut chunks = std::pin::pin!(chunks);
        let mut block_open = false;
        let mut first = true;
        while let Some(ch) = chunks.next().await {
            let delta = ch.choices.first().map(|c| &c.delta);
            let finish = ch.choices.first().and_then(|c| c.finish_reason.clone());
            if first {
                first = false;
                yield sse("message_start", json!({
                    "type": "message_start",
                    "message": {
                        "id": msg_id, "type": "message", "role": "assistant", "model": model,
                        "content": [], "stop_reason": null, "stop_sequence": null,
                        "usage": { "input_tokens": 0, "output_tokens": 0 }
                    }
                }));
            }
            if delta.and_then(|d| d.role.as_deref()) == Some("assistant") && !block_open {
                yield sse("content_block_start", json!({
                    "type": "content_block_start", "index": 0,
                    "content_block": { "type": "text", "text": "" }
                }));
                block_open = true;
            }
            if let Some(d) = delta {
                if let Some(content) = &d.content {
                    if !block_open {
                        yield sse("content_block_start", json!({
                            "type": "content_block_start", "index": 0,
                            "content_block": { "type": "text", "text": "" }
                        }));
                        block_open = true;
                    }
                    yield sse("content_block_delta", json!({
                        "type": "content_block_delta", "index": 0,
                        "delta": { "type": "text_delta", "text": content }
                    }));
                }
            }
            if let Some(finish_reason) = finish {
                if block_open {
                    yield sse("content_block_stop", json!({ "type": "content_block_stop", "index": 0 }));
                    block_open = false;
                }
                let stop = stop_to_anthropic(&finish_reason);
                yield sse("message_delta", json!({
                    "type": "message_delta",
                    "delta": { "stop_reason": stop, "stop_sequence": null },
                    "usage": { "output_tokens": 0 }
                }));
                yield sse("message_stop", json!({ "type": "message_stop" }));
            }
        }
        if first {
            // no chunks at all - emit empty message
            yield sse("message_start", json!({
                "type": "message_start",
                "message": { "id": msg_id, "type": "message", "role": "assistant", "model": model, "content": [], "stop_reason": null, "usage": { "input_tokens": 0, "output_tokens": 0 } }
            }));
            yield sse("content_block_start", json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "text", "text": "" } }));
            yield sse("content_block_stop", json!({ "type": "content_block_stop", "index": 0 }));
            yield sse("message_delta", json!({ "type": "message_delta", "delta": { "stop_reason": "end_turn", "stop_sequence": null }, "usage": { "output_tokens": 0 } }));
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
        }],
    }
}

// ----- utilities -----

fn text_of(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| {
                if let Value::String(s) = p { Some(s.clone()) }
                else { p.get("text").and_then(|v| v.as_str()).map(String::from) }
            })
            .collect::<Vec<_>>()
            .join(""),
        _ => content.to_string(),
    }
}

pub fn rand_hex(n_bytes: usize) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    // Simple non-crypto random: timestamp + counter. Good enough for IDs.
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let c = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mixed = ts.wrapping_mul(0x9E3779B97F4A7C15).wrapping_add(c);
    let mut out = String::with_capacity(n_bytes * 2);
    let mut v = mixed;
    for i in 0..n_bytes {
        let byte = ((v >> ((i % 8) * 8)) & 0xFF) as u8;
        out.push_str(&format!("{:02x}", byte));
        if i % 8 == 7 { v = v.wrapping_mul(0x9E3779B97F4A7C15).wrapping_add(0x1); }
    }
    out
}

fn sse(event: &str, data: Value) -> String {
    format!("event: {event}\ndata: {}\n\n", data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_anthropic_req_basic() {
        let oai = ChatCompletionRequest {
            model: "claude-3-5-sonnet".into(),
            messages: vec![
                ChatMessage { role: "system".into(), content: Value::String("be brief".into()), extra: Default::default() },
                ChatMessage { role: "user".into(), content: Value::String("hi".into()), extra: Default::default() },
            ],
            stream: false,
            temperature: Some(0.5),
            max_tokens: Some(100),
            extra: Default::default(),
        };
        let ant = oai_req_to_anthropic_req(&oai);
        assert_eq!(ant.max_tokens, 100);
        assert_eq!(ant.temperature, Some(0.5));
        assert_eq!(ant.system, Some(Value::String("be brief".into())));
        assert_eq!(ant.messages.len(), 1);
        assert_eq!(ant.messages[0].role, "user");
    }

    #[test]
    fn stop_reason_mapping() {
        assert_eq!(stop_to_anthropic("stop"), "end_turn");
        assert_eq!(stop_from_anthropic("end_turn"), "stop");
        assert_eq!(stop_to_anthropic("length"), "max_tokens");
        assert_eq!(stop_from_anthropic("max_tokens"), "length");
    }

    #[test]
    fn oai_to_anthropic_resp_text() {
        let oai = OAIChatResponse {
            id: "chatcmpl-x".into(),
            object: "chat.completion".into(),
            created: 1,
            model: "m".into(),
            choices: vec![OAIChoice {
                index: 0,
                message: OAIMessage { role: "assistant".into(), content: Some("hello".into()), extra: Default::default() },
                finish_reason: Some("stop".into()),
            }],
            usage: Some(Usage { prompt_tokens: 5, completion_tokens: 5, total_tokens: 10 }),
        };
        let ant = oai_resp_to_anthropic_resp(&oai, "claude");
        assert_eq!(ant.content[0].text.as_deref(), Some("hello"));
        assert_eq!(ant.stop_reason.as_deref(), Some("end_turn"));
        assert_eq!(ant.usage.input_tokens, 5);
    }
}
/// Parse Anthropic SSE event payloads (data strings without the `data:` prefix)
/// into canonical OAI stream chunks. Mirrors `anthropicStreamToOAIChunks` from
/// `src/gateway/converter.ts:112`. Consumed by the Anthropic provider's stream
/// method - input is the output of `SseDataStream` (raw JSON payload strings).
pub fn anthropic_stream_to_oai_chunks(
    data_strings: impl futures_core::Stream<Item = String> + Send + 'static,
    model: String,
) -> std::pin::Pin<Box<dyn futures_core::Stream<Item = OAIStreamChunk> + Send>> {
    use futures_util::StreamExt;
    let id = format!("chatcmpl-{}", rand_hex(6));
    let created = chrono::Utc::now().timestamp();
    let model_for_closure = model.clone();
    let id_for_closure = id.clone();
    async_stream::stream! {
        let mut data_strings = std::pin::pin!(data_strings);
        let mut started = false;
        while let Some(data) = data_strings.next().await {
            let evt: serde_json::Value = match serde_json::from_str(&data) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let evt_type = evt.get("type").and_then(|v| v.as_str()).unwrap_or("");
            match evt_type {
                "message_start" => {
                    if !started {
                        started = true;
                        yield OAIStreamChunk {
                            id: id_for_closure.clone(),
                            object: "chat.completion.chunk".into(),
                            created,
                            model: model_for_closure.clone(),
                            choices: vec![StreamChoice {
                                index: 0,
                                delta: StreamDelta { role: Some("assistant".into()), content: None },
                                finish_reason: None,
                            }],
                        };
                    }
                }
                "content_block_delta" => {
                    let delta = evt.get("delta");
                    if delta.and_then(|d| d.get("type")).and_then(|v| v.as_str()) == Some("text_delta") {
                        if let Some(text) = delta.and_then(|d| d.get("text")).and_then(|v| v.as_str()) {
                            yield OAIStreamChunk {
                                id: id_for_closure.clone(),
                                object: "chat.completion.chunk".into(),
                                created,
                                model: model_for_closure.clone(),
                                choices: vec![StreamChoice {
                                    index: 0,
                                    delta: StreamDelta { role: None, content: Some(text.into()) },
                                    finish_reason: None,
                                }],
                            };
                        }
                    }
                }
                "message_delta" => {
                    if let Some(stop_reason) = evt.get("delta").and_then(|d| d.get("stop_reason")).and_then(|v| v.as_str()) {
                        let mapped = stop_from_anthropic(stop_reason).to_string();
                        yield OAIStreamChunk {
                            id: id_for_closure.clone(),
                            object: "chat.completion.chunk".into(),
                            created,
                            model: model_for_closure.clone(),
                            choices: vec![StreamChoice {
                                index: 0,
                                delta: StreamDelta { role: None, content: None },
                                finish_reason: Some(mapped),
                            }],
                        };
                    }
                }
                "message_stop" => { return; }
                _ => {}
            }
        }
    }
    .boxed()
}
