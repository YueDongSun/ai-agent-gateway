//! Bidirectional OpenAI <-> Anthropic format conversion.
//!
//! Two directions:
//! - Gateway edge: Anthropic client request -> canonical OpenAI -> provider,
//!   then OpenAI response -> Anthropic response back to client.
//! - Anthropic provider: canonical OpenAI request -> Anthropic backend request,
//!   then Anthropic response -> OpenAI response back to the router.
//!
//! Phase 21: this module gained full tool-use fidelity (tools,
//! tool_choice, tool_use / tool_result content blocks, streaming
//! tool_calls delta -> input_json_delta). See
//! `cc-switch/providers/transform.rs` and `cc-switch/providers/streaming.rs`
//! for the upstream reference design.

use crate::proxy::canonical::{
    ChatCompletionRequest, ChatMessage, OAIChatResponse, OAIChoice, OAIMessage, OAIStreamChunk,
    StreamChoice, StreamDelta, Usage,
};
use crate::proxy::responses::{ResponsesRequest, ResponsesStreamEvent};
use futures_core::stream::Stream;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::pin::Pin;

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
    pub role: String,   // "user" | "assistant"
    pub content: Value, // string or array of {type, text} / {type, tool_use} / {type, tool_result}
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
    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, Value>,
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
    #[serde(
        default,
        rename = "cache_creation_input_tokens",
        skip_serializing_if = "is_zero_u32"
    )]
    pub cache_creation_input_tokens: u32,
    #[serde(
        default,
        rename = "cache_read_input_tokens",
        skip_serializing_if = "is_zero_u32"
    )]
    pub cache_read_input_tokens: u32,
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

fn is_zero_u32(value: &u32) -> bool {
    *value == 0
}

// ----- stop-reason mapping -----

fn stop_to_anthropic(stop: &str) -> &'static str {
    match stop {
        "stop" => "end_turn",
        "length" => "max_tokens",
        "tool_calls" | "function_call" => "tool_use",
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

// ----- helpers -----

/// Canonical JSON serialisation so OpenAI-style `function.arguments`
/// (which is a JSON-encoded string) round-trips without whitespace/key
/// reordering noise. Mirrors `proxy/json_canonical.rs` in cc-switch.
pub fn canonical_json_string(v: &Value) -> String {
    // serde_json preserves object key order on serialise, but writes
    // them compactly; for nested values use serde_json::to_string then
    // strip the wrapping braces — but here we just want the string.
    serde_json::to_string(v).unwrap_or_else(|_| "null".into())
}

/// Anthropic adds a per-prompt billing header on the first system line:
///   `x-anthropic-billing-header: ...\nFor more information ...`
/// Forwarding that verbatim eats prompt-cache prefix and confuses some
/// upstreams. Strip it. Same logic as cc-switch.
fn strip_leading_anthropic_billing_header(s: &str) -> &str {
    if let Some(rest) = s.strip_prefix("x-anthropic-billing-header:") {
        if let Some(idx) = rest.find('\n') {
            return rest[idx + 1..].trim_start();
        }
    }
    s
}

fn source_anthropic_request_value(body: &AnthropicRequest) -> Option<Value> {
    let mut source = serde_json::to_value(body).ok()?;
    if let Some(system) = source.get_mut("system") {
        if let Some(text) = system.as_str() {
            *system = Value::String(strip_leading_anthropic_billing_header(text).to_string());
        }
    }
    Some(source)
}

fn text_of(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| {
                if let Value::String(s) = p {
                    Some(s.clone())
                } else {
                    p.get("text").and_then(|v| v.as_str()).map(String::from)
                }
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
        if i % 8 == 7 {
            v = v.wrapping_mul(0x9E3779B97F4A7C15).wrapping_add(0x1);
        }
    }
    out
}

fn sse(event: &str, data: Value) -> String {
    format!("event: {event}\ndata: {}\n\n", data)
}

// ============================================================================
// Canonical OpenAI -> Anthropic backend (provider side)
// ============================================================================

/// Build an Anthropic `tool_choice` from the canonical OpenAI form.
///
/// Anthropic accepts object variants:
///   {"type": "auto" | "any" | "none"}
///   {"type": "tool", "name": "<X>"}
///
/// OpenAI accepts:
///   "auto" / "none" / "required"
///   {"type": "function", "function": {"name": "<X>"}}
fn tool_choice_oai_to_anthropic(tc: &Value) -> Value {
    match tc {
        Value::String(s) => match s.as_str() {
            "required" => json!({ "type": "any" }),
            "auto" | "none" => json!({ "type": s }),
            _ => tc.clone(),
        },
        Value::Object(obj) => match obj.get("type").and_then(|v| v.as_str()) {
            Some("function") => {
                let Some(name) = obj
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|n| n.as_str())
                else {
                    return tc.clone();
                };
                json!({ "type": "tool", "name": name })
            }
            _ => tc.clone(),
        },
        _ => tc.clone(),
    }
}

/// Translate the canonical OpenAI function wrapper at the Anthropic boundary.
/// Native tools and invalid/unknown definitions remain intact so the provider
/// can reject them; never silently remove a requested tool.
fn oai_tool_to_anthropic_tool(tool: &Value) -> Value {
    if tool.get("type").and_then(Value::as_str) != Some("function") {
        return tool.clone();
    }
    let Some(function) = tool.get("function").and_then(Value::as_object) else {
        return tool.clone();
    };
    let Some(name) = function.get("name").and_then(Value::as_str) else {
        return tool.clone();
    };
    let mut converted = json!({
        "name": name,
        "input_schema": function.get("parameters").cloned()
            .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
    });
    if let Some(description) = function.get("description") {
        converted["description"] = description.clone();
    }
    converted
}

/// Anthropic requires parallel tool results together in the next user turn.
/// Only join adjacent result-only messages; preserve other turn boundaries.
fn append_anthropic_tool_result(messages: &mut Vec<AnthropicMessage>, block: Value) {
    if let Some(previous) = messages.last_mut().filter(|message| message.role == "user") {
        if let Some(blocks) = previous.content.as_array_mut().filter(|blocks| {
            blocks
                .iter()
                .all(|block| block.get("type").and_then(Value::as_str) == Some("tool_result"))
        }) {
            blocks.push(block);
            return;
        }
    }
    messages.push(AnthropicMessage {
        role: "user".into(),
        content: json!([block]),
    });
}

pub fn oai_req_to_anthropic_req(req: &ChatCompletionRequest) -> AnthropicRequest {
    if let Some(source) = &req.source_anthropic_request {
        if let Ok(mut body) = serde_json::from_value::<AnthropicRequest>(source.clone()) {
            body.model = crate::proxy::strip_one_m_suffix(&req.model);
            body.stream = Some(req.stream);
            return body;
        }
    }

    let mut system_parts: Vec<String> = Vec::new();
    let mut msgs: Vec<AnthropicMessage> = Vec::new();

    for m in &req.messages {
        match m.role.as_str() {
            "system" => system_parts.push(text_of(&m.content)),
            "tool" => {
                // OpenAI tool role -> Anthropic user message with tool_result block.
                let content_text = text_of(&m.content);
                let mut block = json!({
                    "type": "tool_result",
                    "tool_use_id": m.tool_call_id.clone().unwrap_or_default(),
                    "content": content_text,
                });
                // If content is array-shaped on the OAI side, preserve array form.
                if let Value::Array(_) = m.content {
                    block["content"] = m.content.clone();
                }
                append_anthropic_tool_result(&mut msgs, block);
            }
            "user" | "assistant" => {
                let mut blocks: Vec<Value> = Vec::new();
                let mut has_any = false;
                match &m.content {
                    Value::String(s) => {
                        blocks.push(json!({ "type": "text", "text": s }));
                        has_any = true;
                    }
                    Value::Array(parts) => {
                        for p in parts {
                            if let Some(t) = p.get("type").and_then(|v| v.as_str()) {
                                if t == "text" {
                                    if let Some(text) = p.get("text").and_then(|v| v.as_str()) {
                                        blocks.push(json!({ "type": "text", "text": text }));
                                        has_any = true;
                                    }
                                }
                                // image / audio parts on the OpenAI side map to
                                // Anthropic {type:image, source:{...}}; for now we
                                // skip them silently (AAG only handles text + tools).
                            }
                        }
                    }
                    Value::Null => {}
                    _ => {
                        blocks.push(json!({ "type": "text", "text": m.content.to_string() }));
                        has_any = true;
                    }
                }
                // If assistant, attach tool_calls as tool_use blocks.
                if m.role == "assistant" {
                    if let Some(tcs) = &m.tool_calls {
                        for tc in tcs {
                            let id = tc.get("id").and_then(|v| v.as_str()).unwrap_or("");
                            let name = tc
                                .get("function")
                                .and_then(|f| f.get("name"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            let args_str = tc
                                .get("function")
                                .and_then(|f| f.get("arguments"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("{}");
                            let input: Value = serde_json::from_str(args_str).unwrap_or(json!({}));
                            blocks.push(json!({
                                "type": "tool_use",
                                "id": id,
                                "name": name,
                                "input": input,
                            }));
                            has_any = true;
                        }
                    }
                }
                if has_any {
                    // Anthropic requires non-empty content arrays; if we ended
                    // up with zero blocks, attach an empty placeholder text.
                    if blocks.is_empty() {
                        blocks.push(json!({ "type": "text", "text": "" }));
                    }
                    msgs.push(AnthropicMessage {
                        role: m.role.clone(),
                        content: Value::Array(blocks),
                    });
                }
            }
            _ => {
                // Unknown roles fall back to user text.
                msgs.push(AnthropicMessage {
                    role: "user".into(),
                    content: json!([{ "type": "text", "text": text_of(&m.content) }]),
                });
            }
        }
    }

    let max_tokens = req.max_tokens.unwrap_or(4096);
    let mut out = AnthropicRequest {
        model: crate::proxy::strip_one_m_suffix(&req.model),
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
    // Native Anthropic source envelopes already returned above unchanged.
    if let Some(tools) = &req.tools {
        out.extra.insert(
            "tools".into(),
            Value::Array(tools.iter().map(oai_tool_to_anthropic_tool).collect()),
        );
    }
    if let Some(tc) = &req.tool_choice {
        out.extra
            .insert("tool_choice".into(), tool_choice_oai_to_anthropic(tc));
    }
    out
}

pub fn anthropic_resp_to_oai_resp(
    resp: &AnthropicResponse,
    requested_model: &str,
) -> OAIChatResponse {
    let mut text_acc = String::new();
    let mut tool_calls: Vec<Value> = Vec::new();
    let mut extra_blocks: Vec<Value> = Vec::new();
    for b in &resp.content {
        match b.block_type.as_str() {
            "text" => {
                if let Some(t) = &b.text {
                    if !t.is_empty() {
                        text_acc.push_str(t);
                    }
                }
            }
            "tool_use" => {
                // Extract id / name / input via the flatten'd `extra` map.
                let id = b.extra.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let name = b.extra.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let input = b.extra.get("input").cloned().unwrap_or(json!({}));
                tool_calls.push(json!({
                    "id": id,
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": canonical_json_string(&input),
                    }
                }));
            }
            // thinking / redacted_thinking / image / etc. — pass through under `extra`
            _ => {
                let mut block = json!({ "type": b.block_type });
                if let Some(t) = &b.text {
                    block
                        .as_object_mut()
                        .unwrap()
                        .insert("text".into(), json!(t));
                }
                for (k, v) in &b.extra {
                    block.as_object_mut().unwrap().insert(k.clone(), v.clone());
                }
                extra_blocks.push(block);
            }
        }
    }
    let stop_reason = resp
        .stop_reason
        .as_deref()
        .map(|r| stop_from_anthropic(r).to_string());

    let mut extra: HashMap<String, Value> = HashMap::new();
    if !extra_blocks.is_empty() {
        extra.insert("content_blocks".into(), Value::Array(extra_blocks));
    }

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
                content: if text_acc.is_empty() {
                    None
                } else {
                    Some(text_acc)
                },
                tool_calls: if tool_calls.is_empty() {
                    None
                } else {
                    Some(tool_calls)
                },
                refusal: None,
                extra,
            },
            finish_reason: stop_reason,
            extra: Default::default(),
        }],
        usage: Some(Usage {
            prompt_tokens: resp.usage.input_tokens,
            completion_tokens: resp.usage.output_tokens,
            total_tokens: resp.usage.input_tokens + resp.usage.output_tokens,
            extra: Default::default(),
        }),
        source_anthropic_response: None,
        extra: Default::default(),
    }
}

// ============================================================================
// Anthropic client -> Canonical OpenAI (gateway edge)
// ============================================================================

/// Map Anthropic `tool_choice` to OpenAI form.
fn tool_choice_anthropic_to_oai(tc: &Value) -> Value {
    match tc {
        Value::String(s) => match s.as_str() {
            "any" => json!("required"),
            other => json!(other),
        },
        Value::Object(obj) => match obj.get("type").and_then(|v| v.as_str()) {
            Some("tool") => {
                let name = obj.get("name").and_then(|v| v.as_str()).unwrap_or("");
                json!({ "type": "function", "function": { "name": name } })
            }
            Some(other) => json!({ "type": other }),
            _ => tc.clone(),
        },
        _ => tc.clone(),
    }
}

pub fn anthropic_req_to_oai_req(body: &AnthropicRequest) -> ChatCompletionRequest {
    let mut messages: Vec<ChatMessage> = Vec::new();
    if let Some(sys) = &body.system {
        let sys_text = match sys {
            Value::String(s) => strip_leading_anthropic_billing_header(s).to_string(),
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
                tool_calls: None,
                tool_call_id: None,
                extra: Default::default(),
            });
        }
    }
    for m in &body.messages {
        let role = m.role.clone();
        match &m.content {
            Value::String(s) => {
                messages.push(ChatMessage {
                    role,
                    content: Value::String(s.clone()),
                    tool_calls: None,
                    tool_call_id: None,
                    extra: Default::default(),
                });
            }
            Value::Array(blocks) => {
                let mut text_parts: Vec<Value> = Vec::new();
                let mut tool_calls: Vec<Value> = Vec::new();
                // For Anthropic `user` messages, `tool_result` blocks must
                // become separate OpenAI `role: tool` messages.
                let mut pending_tool_results: Vec<ChatMessage> = Vec::new();
                for b in blocks {
                    let t = b.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    match t {
                        "text" => {
                            if let Some(text) = b.get("text").and_then(|v| v.as_str()) {
                                text_parts.push(json!({ "type": "text", "text": text }));
                            }
                        }
                        "image" => {
                            // Forward as an OpenAI image_url block.
                            if let Some(src) = b.get("source") {
                                let mt = src
                                    .get("media_type")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("image/png");
                                let data = src.get("data").and_then(|v| v.as_str()).unwrap_or("");
                                text_parts.push(json!({
                                    "type": "image_url",
                                    "image_url": {
                                        "url": format!("data:{};base64,{}", mt, data),
                                    },
                                }));
                            }
                        }
                        "tool_use" => {
                            let id = b.get("id").and_then(|v| v.as_str()).unwrap_or("");
                            let name = b.get("name").and_then(|v| v.as_str()).unwrap_or("");
                            let input = b.get("input").cloned().unwrap_or(json!({}));
                            tool_calls.push(json!({
                                "id": id,
                                "type": "function",
                                "function": {
                                    "name": name,
                                    "arguments": canonical_json_string(&input),
                                }
                            }));
                        }
                        "tool_result" => {
                            let id = b.get("tool_use_id").and_then(|v| v.as_str()).unwrap_or("");
                            // The tool result content can be a string or
                            // an array of content blocks — preserve the
                            // exact shape upstream expects.
                            let content = b.get("content").cloned().unwrap_or(Value::Null);
                            pending_tool_results.push(ChatMessage {
                                role: "tool".into(),
                                content,
                                tool_calls: None,
                                tool_call_id: Some(id.to_string()),
                                extra: Default::default(),
                            });
                        }
                        // thinking / redacted_thinking — drop (we don't surface
                        // them as canonical content yet; could pass via
                        // message.extra in a future iteration).
                        _ => {}
                    }
                }
                // For assistant messages, attach tool_calls; for user/system
                // they would be ignored by OpenAI servers, so we drop them.
                let msg = if role == "assistant" {
                    ChatMessage {
                        role,
                        // Single text -> string; multi-part -> array.
                        content: if text_parts.len() == 1
                            && text_parts[0].get("type").and_then(|v| v.as_str()) == Some("text")
                        {
                            Value::String(
                                text_parts[0]
                                    .get("text")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string(),
                            )
                        } else if text_parts.is_empty() && tool_calls.is_empty() {
                            Value::String(String::new())
                        } else {
                            Value::Array(text_parts)
                        },
                        tool_calls: if tool_calls.is_empty() {
                            None
                        } else {
                            Some(tool_calls)
                        },
                        tool_call_id: None,
                        extra: Default::default(),
                    }
                } else {
                    // user messages: never carry tool_calls in OAI form.
                    ChatMessage {
                        role,
                        content: if text_parts.is_empty() {
                            Value::String(String::new())
                        } else if text_parts.len() == 1
                            && text_parts[0].get("type").and_then(|v| v.as_str()) == Some("text")
                        {
                            Value::String(
                                text_parts[0]
                                    .get("text")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string(),
                            )
                        } else {
                            Value::Array(text_parts)
                        },
                        tool_calls: None,
                        tool_call_id: None,
                        extra: Default::default(),
                    }
                };
                // If a non-tool message ended up empty (e.g. a user msg
                // whose content was entirely tool_result blocks), skip it
                // — we already pushed the tool results separately.
                let msg_empty = !matches!(msg.content, Value::String(ref s) if !s.is_empty())
                    && !matches!(msg.content, Value::Array(ref a) if !a.is_empty());
                if !(msg.role == "user" && msg_empty && !pending_tool_results.is_empty()) {
                    messages.push(msg);
                }
                for tr in pending_tool_results {
                    messages.push(tr);
                }
            }
            _ => {
                messages.push(ChatMessage {
                    role,
                    content: m.content.clone(),
                    tool_calls: None,
                    tool_call_id: None,
                    extra: Default::default(),
                });
            }
        }
    }

    let mut extra = body.extra.clone();
    // Tools + tool_choice are carried in `AnthropicRequest.extra` (the flatten
    // bag) because the Anthropic wire type doesn't expose them as named fields.
    // Anthropic stores tools as a top-level array under the same key; promote
    // it to OpenAI's Option<Vec<Value>>.
    let tools = match extra.remove("tools") {
        Some(Value::Array(arr)) => Some(arr),
        Some(other) => Some(vec![other]), // unknown shape — pass through as-is
        None => None,
    };
    let tool_choice = extra
        .remove("tool_choice")
        .map(|tc| tool_choice_anthropic_to_oai(&tc));

    ChatCompletionRequest {
        model: crate::proxy::strip_one_m_suffix(&body.model),
        messages,
        stream: body.stream.unwrap_or(false),
        temperature: body.temperature,
        max_tokens: Some(body.max_tokens),
        tools,
        tool_choice,
        extra,
        source_anthropic_request: source_anthropic_request_value(body),
        request_id: None,
        client_credential: None,
    }
}

pub fn oai_resp_to_anthropic_resp(
    resp: &OAIChatResponse,
    requested_model: &str,
) -> AnthropicResponse {
    if let Some(source) = &resp.source_anthropic_response {
        if let Ok(mut body) = serde_json::from_value::<AnthropicResponse>(source.clone()) {
            body.model = requested_model.to_string();
            return body;
        }
    }

    let mut content: Vec<AnthropicContentBlock> = Vec::new();
    for choice in &resp.choices {
        // Only forward reasoning the upstream actually returned. Cross-protocol
        // text has no Anthropic signature and must never be assigned a fake one.
        if let Some(thinking) = choice
            .message
            .extra
            .get("reasoning_content")
            .or_else(|| choice.message.extra.get("reasoning"))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            content.push(AnthropicContentBlock {
                block_type: "thinking".into(),
                text: None,
                extra: [("thinking".into(), json!(thinking))].into_iter().collect(),
            });
        }
        // 1. text / refusal
        if let Some(text) = &choice.message.content {
            if !text.is_empty() {
                content.push(AnthropicContentBlock {
                    block_type: "text".into(),
                    text: Some(text.clone()),
                    extra: Default::default(),
                });
            }
        }
        if let Some(refusal) = &choice.message.refusal {
            if !refusal.is_empty() {
                content.push(AnthropicContentBlock {
                    block_type: "text".into(),
                    text: Some(refusal.clone()),
                    extra: Default::default(),
                });
            }
        }
        // 2. tool_calls -> tool_use blocks
        if let Some(tcs) = &choice.message.tool_calls {
            for tc in tcs {
                let id = tc.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let name = tc
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let args_str = tc
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("{}");
                let input: Value = serde_json::from_str(args_str).unwrap_or(json!({}));
                let mut extra: HashMap<String, Value> = HashMap::new();
                extra.insert("id".into(), json!(id));
                extra.insert("name".into(), json!(name));
                extra.insert("input".into(), input);
                content.push(AnthropicContentBlock {
                    block_type: "tool_use".into(),
                    text: None,
                    extra,
                });
            }
        }
        // 3. extra_blocks (thinking etc.) forwarded as raw blocks.
        if let Some(Value::Array(blocks)) = choice.message.extra.get("content_blocks") {
            for b in blocks {
                let block_type = b
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let text = b.get("text").and_then(|v| v.as_str()).map(String::from);
                let mut extra: HashMap<String, Value> = HashMap::new();
                if let Some(map) = b.as_object() {
                    for (k, v) in map {
                        if k != "type" && k != "text" {
                            extra.insert(k.clone(), v.clone());
                        }
                    }
                }
                content.push(AnthropicContentBlock {
                    block_type,
                    text,
                    extra,
                });
            }
        }
    }

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
        content,
        stop_reason: Some(stop_to_anthropic(stop).into()),
        stop_sequence: None,
        usage: AnthropicUsage {
            input_tokens: resp.usage.as_ref().map(|u| u.prompt_tokens).unwrap_or(0),
            output_tokens: resp
                .usage
                .as_ref()
                .map(|u| u.completion_tokens)
                .unwrap_or(0),
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            extra: Default::default(),
        },
        extra: Default::default(),
    }
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

        while let Some(ch) = chunks.next().await {
            if let Some(source_event) = &ch.source_anthropic_event {
                let mut event = source_event.clone();
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

// ============================================================================
// OpenAI Responses API <-> canonical OpenAI Chat Completions
// ============================================================================

/// Flatten a `ResponsesRequest.input` into the canonical `messages` array.
///
/// Input items are converted by `type`, not flattened indiscriminately:
/// - `message` (or a bare role/content item) → a chat message whose content
///   parts (`input_text` / `output_text` / `text`) collapse to text parts.
/// - `function_call` → an assistant message carrying the OpenAI `tool_calls`
///   entry (`call_id` → `id`, flat `name`/`arguments` → nested `function`).
///   Adjacent calls join the same assistant turn before any tool results.
///   Dropping these broke every multi-turn tool conversation: the upstream
///   saw a dangling tool result with no matching assistant invocation.
/// - `function_call_output` → a `role: "tool"` message with `tool_call_id`.
/// - `reasoning`, `item_reference` and other gateway-opaque item types are
///   stateless for a chat-completions upstream and are dropped.
pub fn responses_to_chat(req: &ResponsesRequest) -> ChatCompletionRequest {
    let mut messages: Vec<ChatMessage> = Vec::new();

    if let Some(sys) = &req.instructions {
        if !sys.is_empty() {
            messages.push(ChatMessage {
                role: "system".into(),
                content: Value::String(sys.clone()),
                tool_calls: None,
                tool_call_id: None,
                extra: Default::default(),
            });
        }
    }

    match &req.input {
        Value::String(s) => {
            messages.push(ChatMessage {
                role: "user".into(),
                content: Value::String(s.clone()),
                tool_calls: None,
                tool_call_id: None,
                extra: Default::default(),
            });
        }
        Value::Array(items) => {
            for item in items {
                if let Some(message) = responses_input_item_to_chat(item) {
                    append_responses_input_message(&mut messages, message);
                }
            }
        }
        _ => {
            messages.push(ChatMessage {
                role: "user".into(),
                content: Value::String(String::new()),
                tool_calls: None,
                tool_call_id: None,
                extra: Default::default(),
            });
        }
    }

    ChatCompletionRequest {
        model: req.model.clone(),
        messages,
        stream: req.stream,
        temperature: req.temperature,
        max_tokens: req.max_output_tokens,
        tools: req.tools.as_ref().and_then(responses_tools_to_chat),
        tool_choice: responses_tool_choice_to_chat(&req.extra),
        extra: Default::default(),
        source_anthropic_request: None,
        request_id: None,
        client_credential: None,
    }
}

/// Responses emits a separate item per call, while Chat Completions expects
/// one assistant tool_calls array followed by all of that turn's tool results.
fn append_responses_input_message(messages: &mut Vec<ChatMessage>, mut message: ChatMessage) {
    if message.role == "assistant" && message.content.is_null() {
        if let (Some(previous), Some(calls)) = (
            messages
                .last_mut()
                .filter(|previous| previous.role == "assistant"),
            message.tool_calls.as_mut(),
        ) {
            previous
                .tool_calls
                .get_or_insert_with(Vec::new)
                .append(calls);
            return;
        }
    }
    messages.push(message);
}

/// Translate one Responses-API input item into a canonical chat message.
/// Returns `None` for item types without a Chat Completions equivalent
/// (dropping them is safe; forwarding them as bogus messages is not —
/// items like `function_call` have no `role`/`content` keys and used to
/// flatten into `{"role":"user","content":null}`, a guaranteed 400).
fn responses_input_item_to_chat(item: &Value) -> Option<ChatMessage> {
    let item_type = item
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("message");
    match item_type {
        "function_call" => {
            let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let arguments = match item.get("arguments") {
                Some(Value::String(s)) => s.clone(),
                Some(other) => other.to_string(),
                None => String::new(),
            };
            let call_id = item
                .get("call_id")
                .and_then(|v| v.as_str())
                .or_else(|| item.get("id").and_then(|v| v.as_str()))
                .unwrap_or("call_0")
                .to_string();
            Some(ChatMessage {
                role: "assistant".into(),
                // Chat Completions permits null content on assistant
                // tool-call messages; every other combination is replaced
                // by a string in `input_content_to_chat`.
                content: Value::Null,
                tool_calls: Some(vec![json!({
                    "id": call_id,
                    "type": "function",
                    "function": { "name": name, "arguments": arguments },
                })]),
                tool_call_id: None,
                extra: Default::default(),
            })
        }
        "function_call_output" => {
            let output = function_call_output_text(item.get("output").unwrap_or(&Value::Null));
            let call_id = item
                .get("call_id")
                .and_then(|v| v.as_str())
                .or_else(|| item.get("id").and_then(|v| v.as_str()))
                .unwrap_or("call_0")
                .to_string();
            Some(ChatMessage {
                role: "tool".into(),
                content: output,
                tool_calls: None,
                tool_call_id: Some(call_id),
                extra: Default::default(),
            })
        }
        "message" => {
            let role = item
                .get("role")
                .and_then(|v| v.as_str())
                .unwrap_or("user")
                .to_string();
            // Responses-API "developer" is the new name for the Chat
            // Completions "system" role; most OpenAI-compatible relays
            // reject "developer" outright.
            let role = if role == "developer" {
                "system".to_string()
            } else {
                role
            };
            let content = item.get("content").cloned().unwrap_or(Value::Null);
            Some(ChatMessage {
                role,
                content: input_content_to_chat(&content),
                tool_calls: None,
                tool_call_id: None,
                extra: Default::default(),
            })
        }
        // reasoning blocks, item references, computer/file calls, ...
        _ => None,
    }
}

/// `function_call_output.output` is a string or an array of content parts;
/// chat upstreams need a plain string.
fn function_call_output_text(output: &Value) -> Value {
    match output {
        Value::String(s) => Value::String(s.clone()),
        Value::Array(parts) => {
            let mut text = String::new();
            for part in parts {
                if let Some(s) = part.get("text").and_then(Value::as_str) {
                    text.push_str(s);
                }
            }
            Value::String(text)
        }
        Value::Null => Value::String(String::new()),
        other => other.clone(),
    }
}

fn input_content_to_chat(content: &Value) -> Value {
    match content {
        Value::String(s) => Value::String(s.clone()),
        Value::Array(parts) => {
            let mut out: Vec<Value> = Vec::new();
            for p in parts {
                if let Some(text) = p.get("text").and_then(Value::as_str) {
                    out.push(json!({ "type": "text", "text": text }));
                }
            }
            if out.is_empty() {
                // Image/file-only parts (or an empty array): strict
                // upstreams reject empty content arrays, so collapse to "".
                Value::String(String::new())
            } else {
                Value::Array(out)
            }
        }
        Value::Null => Value::String(String::new()),
        other => other.clone(),
    }
}

/// Translate Responses-API tool definitions into the Chat Completions
/// shape. Responses function tools are flat
/// (`{type, name, description?, parameters?, strict?}`); Chat Completions
/// nests the payload under `function`. Tool types without a Chat
/// Completions equivalent (`web_search`, `local_shell`, `mcp`, ...) are
/// dropped rather than forwarded — forwarding them gets the whole request
/// rejected. Returns `None` when nothing usable remains so we never emit
/// an empty `tools` array.
fn responses_tools_to_chat(tools: &Value) -> Option<Vec<Value>> {
    let items: Vec<&Value> = match tools {
        Value::Array(arr) => arr.iter().collect(),
        other => vec![other],
    };
    let mut out: Vec<Value> = Vec::new();
    for tool in items {
        let obj = match tool.as_object() {
            Some(obj) => obj,
            None => continue,
        };
        if obj.get("function").map_or(false, Value::is_object) {
            // Already in the Chat Completions shape.
            out.push(tool.clone());
            continue;
        }
        let tool_type = obj
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("function");
        if tool_type != "function" {
            continue;
        }
        let name = match obj.get("name").and_then(|v| v.as_str()) {
            Some(name) => name,
            None => continue,
        };
        let mut function = serde_json::Map::new();
        function.insert("name".into(), Value::String(name.to_string()));
        if let Some(description) = obj.get("description") {
            function.insert("description".into(), description.clone());
        }
        // Codex's apply_patch tool ships without a `parameters` key; many
        // upstreams require one, so default to an empty object schema.
        function.insert(
            "parameters".into(),
            obj.get("parameters")
                .cloned()
                .unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
        );
        if let Some(strict) = obj.get("strict") {
            function.insert("strict".into(), strict.clone());
        }
        out.push(json!({ "type": "function", "function": Value::Object(function) }));
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Translate the Responses-API `tool_choice` (captured in `extra` because
/// it is not a named field) into the Chat Completions shape. Strings pass
/// through; the object form `{type:"function", name}` becomes
/// `{type:"function", function:{name}}`. Anything else is dropped — a
/// missing `tool_choice` is safe, an untranslatable one is a 400.
fn responses_tool_choice_to_chat(
    extra: &std::collections::HashMap<String, Value>,
) -> Option<Value> {
    match extra.get("tool_choice")? {
        Value::String(s) => Some(Value::String(s.clone())),
        Value::Object(obj) => {
            if obj.get("type").and_then(|v| v.as_str()) == Some("function") {
                let name = obj.get("name").and_then(|v| v.as_str())?;
                Some(json!({ "type": "function", "function": { "name": name } }))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Map a chat finish reason to Responses `status` and optional
/// `incomplete_details`.
fn responses_finish_status(reason: Option<&str>) -> (&'static str, Option<Value>) {
    match reason {
        Some("length") => ("incomplete", Some(json!({ "reason": "max_output_tokens" }))),
        Some("content_filter") => ("incomplete", Some(json!({ "reason": "content_filter" }))),
        _ => ("completed", None),
    }
}

/// Convert a canonical chat completion into the Responses envelope (the
/// non-streaming `POST /v1/responses` body). Message text becomes a
/// `message` output item; assistant `tool_calls` become `function_call`
/// output items — Codex CLI executes tools from exactly these items, so
/// dropping them (the previous behavior) made every tool turn unusable.
pub fn chat_to_responses(resp: &OAIChatResponse, requested_model: &str) -> Value {
    let model = if resp.model.is_empty() {
        requested_model.to_string()
    } else {
        resp.model.clone()
    };

    let mut output: Vec<Value> = Vec::new();
    for choice in &resp.choices {
        if let Some(text) = choice.message.content.as_deref().filter(|t| !t.is_empty()) {
            output.push(json!({
                "type": "message",
                "id": format!("msg_{}", rand_hex(12)),
                "role": choice.message.role,
                "status": "completed",
                "content": [{
                    "type": "output_text",
                    "text": text,
                    "annotations": [],
                }],
            }));
        }
        if let Some(tool_calls) = &choice.message.tool_calls {
            for call in tool_calls {
                let name = call
                    .pointer("/function/name")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let arguments = call
                    .pointer("/function/arguments")
                    .and_then(Value::as_str)
                    .unwrap_or("{}");
                let call_id = call.get("id").and_then(Value::as_str).unwrap_or("");
                output.push(json!({
                    "type": "function_call",
                    "id": format!("fc_{}", rand_hex(12)),
                    "call_id": call_id,
                    "name": name,
                    "arguments": arguments,
                    "status": "completed",
                }));
            }
        }
    }

    // Compute status from the first choice's finish_reason. A non-success
    // reason like "length" must surface as status="incomplete" so Codex CLI
    // treats the response as truncated rather than fully successful.
    let finish_reason = resp
        .choices
        .first()
        .and_then(|c| c.finish_reason.as_deref());
    let (status, incomplete_details) = responses_finish_status(finish_reason);

    let usage = resp
        .usage
        .as_ref()
        .map(|usage| crate::proxy::responses::ResponsesUsage {
            input_tokens: usage.prompt_tokens,
            output_tokens: usage.completion_tokens,
            total_tokens: usage.total_tokens,
            input_tokens_details: usage.extra.get("prompt_tokens_details").cloned(),
            output_tokens_details: usage.extra.get("completion_tokens_details").cloned(),
        });

    crate::proxy::responses::response_envelope(
        &resp.id,
        resp.created,
        &model,
        status,
        output,
        usage,
        None,
        incomplete_details,
        None,
    )
}

/// One in-flight streamed tool call: the Responses item that accumulates
/// the chat-completions `tool_calls` deltas for a single `index`.
#[allow(non_camel_case_types)]
struct Stream_tool_call {
    item_id: String,
    call_id: String,
    name: String,
    arguments: String,
    emitted_argument_bytes: usize,
    output_index: u32,
    added: bool,
}

/// Convert a canonical chat-completions chunk stream into the Responses SSE
/// event stream.
///
/// Emits the event sequence Codex CLI depends on, with the payload `type`
/// matching each SSE event name. Items open lazily on their first delta and
/// take output indices in first-seen order, so a tool-only turn carries no
/// phantom empty message item:
/// - `response.created` → (message item added on first text)
///   `response.output_text.delta` → item/completed events.
/// - Tool-call deltas accumulate per chat `index` into `function_call`
///   items (`output_item.added` → `function_call_arguments.delta` →
///   `.done` → `output_item.done`), so streamed tool use survives the hop.
/// - Reasoning deltas get their own `reasoning` output item.
/// - Upstream usage (the final chunk, requested via `stream_options`)
///   populates `response.completed.usage` instead of the previous
///   always-zero stub.
/// - An error chunk (router `proxy_stream_error` or mid-stream upstream
///   error) becomes `response.failed` with a failed status envelope.
pub fn chat_chunks_to_responses_stream(
    stream: Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>>,
    requested_model: &str,
) -> Pin<Box<dyn Stream<Item = String> + Send>> {
    use async_stream::stream;
    let mut stream = stream;
    let id = format!("resp_{}", crate::proxy::converter::rand_hex(16));
    let message_id = format!("msg_{}", crate::proxy::converter::rand_hex(12));
    let reasoning_id = format!("rs_{}", crate::proxy::converter::rand_hex(12));
    let created = chrono::Utc::now().timestamp();
    let model = requested_model.to_string();
    let mut first = true;

    // Output state. Every item is opened lazily on its first delta and
    // takes the next output index in first-seen order — a tool-only turn
    // (Codex's most common shape) produces no phantom empty message item,
    // matching the native Responses API.
    let mut next_output_index: u32 = 0;
    let mut message_index: Option<u32> = None;
    let mut accumulated_text = String::new();
    let mut message_role = "assistant".to_string();
    let mut reasoning_index: Option<u32> = None;
    let mut tool_calls: Vec<Stream_tool_call> = Vec::new();
    let mut usage: Option<crate::proxy::responses::ResponsesUsage> = None;
    let mut upstream_model = String::new();
    let mut final_finish_reason: Option<String> = None;

    let out = stream! {
        while let Some(chunk) = stream.next().await {
            if let Some(error) = chunk.extra.get("error") {
                let code = error.get("type").and_then(Value::as_str).unwrap_or("proxy_stream_error");
                let message = error.get("message").and_then(Value::as_str).unwrap_or("Upstream stream failed before completion");
                let failed = crate::proxy::responses::response_envelope(
                    &id, created, &model, "failed", vec![], usage.clone(),
                    None, None,
                    Some(json!({ "code": code, "message": message })),
                );
                let evt = ResponsesStreamEvent::ResponseFailed { response: failed };
                yield crate::proxy::responses::stream_event_to_sse(&evt);
                // Terminal: no [DONE] success terminator after a failure.
                return;
            }
            if !chunk.model.is_empty() {
                upstream_model = chunk.model.clone();
            }
            if first {
                if let Some(choice) = chunk.choices.first() {
                    if let Some(r) = &choice.delta.role {
                        message_role = r.clone();
                    }
                }
                let stub = crate::proxy::responses::response_envelope(
                    &id, created, &model, "in_progress", vec![], None, None, None, None,
                );
                let evt = ResponsesStreamEvent::ResponseCreated { response: stub };
                yield crate::proxy::responses::stream_event_to_sse(&evt);
                first = false;
            }

            if let Some(u) = chunk.extra.get("usage") {
                let input = u.get("prompt_tokens").or_else(|| u.get("input_tokens")).and_then(Value::as_u64);
                let output_tokens = u.get("completion_tokens").or_else(|| u.get("output_tokens")).and_then(Value::as_u64);
                if input.is_some() || output_tokens.is_some() {
                    let input = input.unwrap_or(0) as u32;
                    let output = output_tokens.unwrap_or(0) as u32;
                    usage = Some(crate::proxy::responses::ResponsesUsage {
                        input_tokens: input,
                        output_tokens: output,
                        total_tokens: input.saturating_add(output),
                        input_tokens_details: u.get("prompt_tokens_details").cloned(),
                        output_tokens_details: u.get("completion_tokens_details").cloned(),
                    });
                }
            }

            for choice in &chunk.choices {
                if let Some(reason) = &choice.finish_reason {
                    final_finish_reason = Some(reason.clone());
                }
                if let Some(reasoning) = choice.delta.reasoning_text() {
                    if !reasoning.is_empty() {
                        let index = match reasoning_index {
                            Some(index) => index,
                            None => {
                                let index = next_output_index;
                                next_output_index += 1;
                                reasoning_index = Some(index);
                                let item = json!({
                                    "type": "reasoning",
                                    "id": reasoning_id,
                                    "summary": [],
                                    "status": "in_progress",
                                });
                                let evt = ResponsesStreamEvent::ResponseOutputItemAdded {
                                    output_index: index,
                                    item,
                                };
                                yield crate::proxy::responses::stream_event_to_sse(&evt);
                                index
                            }
                        };
                        let evt = ResponsesStreamEvent::ResponseReasoningTextDelta {
                            item_id: reasoning_id.clone(),
                            output_index: index,
                            content_index: 0,
                            delta: reasoning.to_string(),
                        };
                        yield crate::proxy::responses::stream_event_to_sse(&evt);
                    }
                }
                if let Some(text) = &choice.delta.content {
                    if !text.is_empty() {
                        let index = match message_index {
                            Some(index) => index,
                            None => {
                                let index = next_output_index;
                                next_output_index += 1;
                                message_index = Some(index);
                                let item = json!({
                                    "type": "message",
                                    "id": message_id,
                                    "role": message_role,
                                    "content": [],
                                    "status": "in_progress",
                                });
                                let evt = ResponsesStreamEvent::ResponseOutputItemAdded {
                                    output_index: index,
                                    item,
                                };
                                yield crate::proxy::responses::stream_event_to_sse(&evt);
                                index
                            }
                        };
                        accumulated_text.push_str(text);
                        let evt = ResponsesStreamEvent::ResponseOutputTextDelta {
                            item_id: message_id.clone(),
                            output_index: index,
                            content_index: 0,
                            delta: text.clone(),
                        };
                        yield crate::proxy::responses::stream_event_to_sse(&evt);
                    }
                }
                if let Some(deltas) = &choice.delta.tool_calls {
                    for delta in deltas {
                        let index = delta.get("index").and_then(Value::as_u64).unwrap_or(tool_calls.len() as u64) as usize;
                        if index >= tool_calls.len() {
                            // Grow with placeholders so indices stay aligned.
                            for _ in tool_calls.len()..=index {
                                let output_index = next_output_index;
                                next_output_index += 1;
                                tool_calls.push(Stream_tool_call {
                                    item_id: format!("fc_{}", crate::proxy::converter::rand_hex(12)),
                                    call_id: String::new(),
                                    name: String::new(),
                                    arguments: String::new(),
                                    emitted_argument_bytes: 0,
                                    output_index,
                                    added: false,
                                });
                            }
                        }
                        let state = &mut tool_calls[index];
                        if let Some(call_id) = delta.get("id").and_then(Value::as_str) {
                            if !call_id.is_empty() {
                                state.call_id = call_id.to_string();
                            }
                        }
                        if let Some(name) = delta.pointer("/function/name").and_then(Value::as_str) {
                            if !name.is_empty() {
                                state.name = name.to_string();
                            }
                        }
                        if !state.added && !state.name.is_empty() && !state.call_id.is_empty() {
                            state.added = true;
                            let item = json!({
                                "type": "function_call",
                                "id": state.item_id,
                                "call_id": state.call_id,
                                "name": state.name,
                                "arguments": "",
                                "status": "in_progress",
                            });
                            let evt = ResponsesStreamEvent::ResponseOutputItemAdded {
                                output_index: state.output_index,
                                item,
                            };
                            yield crate::proxy::responses::stream_event_to_sse(&evt);
                        }
                        if let Some(fragment) = delta.pointer("/function/arguments").and_then(Value::as_str) {
                            state.arguments.push_str(fragment);
                        }
                        // Identity can arrive after argument fragments. Open the
                        // item first, then flush every previously buffered byte.
                        if state.added && state.emitted_argument_bytes < state.arguments.len() {
                            let fragment = state.arguments[state.emitted_argument_bytes..].to_string();
                            state.emitted_argument_bytes = state.arguments.len();
                            let evt = ResponsesStreamEvent::ResponseFunctionCallArgumentsDelta {
                                item_id: state.item_id.clone(),
                                output_index: state.output_index,
                                delta: fragment,
                            };
                            yield crate::proxy::responses::stream_event_to_sse(&evt);
                        }
                    }
                }
                // A terminal chunk (finish_reason / message_stop) is
                // guaranteed by the router before the stream ends — any
                // missing-terminal stream arrives here as an error chunk.
            }
        }

        // A payload without both identity fields cannot become an executable
        // Responses function call. Do not hide it behind a successful response.
        if tool_calls.iter().any(|state| !state.added
            && (!state.name.is_empty() || !state.call_id.is_empty() || !state.arguments.is_empty()))
        {
            let failed = crate::proxy::responses::response_envelope(
                &id, created, &model, "failed", vec![], usage, None, None,
                Some(json!({"code": "proxy_stream_error", "message": "Upstream tool call ended without a name or call ID"})),
            );
            yield crate::proxy::responses::stream_event_to_sse(&ResponsesStreamEvent::ResponseFailed { response: failed });
            return;
        }

        if !first {
            // Close opened items in output-index order, then assemble the
            // final output array the same way. Items that never opened
            // (a text-free turn has no message item) are simply absent —
            // matching the native Responses API.
            let mut items: Vec<(u32, Value)> = Vec::new();
            if let Some(index) = message_index {
                let evt = ResponsesStreamEvent::ResponseOutputTextDone {
                    item_id: message_id.clone(),
                    output_index: index,
                    content_index: 0,
                    text: accumulated_text.clone(),
                };
                yield crate::proxy::responses::stream_event_to_sse(&evt);
                let message_item = json!({
                    "type": "message",
                    "id": message_id,
                    "role": message_role,
                    "status": "completed",
                    "content": [{
                        "type": "output_text",
                        "text": accumulated_text,
                        "annotations": [],
                    }],
                });
                let evt = ResponsesStreamEvent::ResponseOutputItemDone { output_index: index, item: message_item.clone() };
                yield crate::proxy::responses::stream_event_to_sse(&evt);
                items.push((index, message_item));
            }
            if let Some(index) = reasoning_index {
                let item = json!({
                    "type": "reasoning",
                    "id": reasoning_id,
                    "summary": [],
                    "status": "completed",
                });
                let evt = ResponsesStreamEvent::ResponseOutputItemDone { output_index: index, item: item.clone() };
                yield crate::proxy::responses::stream_event_to_sse(&evt);
                items.push((index, item));
            }
            for state in tool_calls.iter().filter(|state| state.added) {
                let evt = ResponsesStreamEvent::ResponseFunctionCallArgumentsDone {
                    item_id: state.item_id.clone(),
                    output_index: state.output_index,
                    arguments: state.arguments.clone(),
                };
                yield crate::proxy::responses::stream_event_to_sse(&evt);
                let item = json!({
                    "type": "function_call",
                    "id": state.item_id,
                    "call_id": state.call_id,
                    "name": state.name,
                    "arguments": state.arguments,
                    "status": "completed",
                });
                let evt = ResponsesStreamEvent::ResponseOutputItemDone {
                    output_index: state.output_index,
                    item: item.clone(),
                };
                yield crate::proxy::responses::stream_event_to_sse(&evt);
                items.push((state.output_index, item));
            }
            items.sort_by_key(|(index, _)| *index);
            let output: Vec<Value> = items.into_iter().map(|(_, item)| item).collect();

            let final_model = if upstream_model.is_empty() { model.clone() } else { upstream_model };
            let (final_status, final_incomplete) = responses_finish_status(
                final_finish_reason.as_deref()
            );
            let final_response = crate::proxy::responses::response_envelope(
                &id, created, &final_model, final_status, output, usage, None, final_incomplete, None,
            );
            let evt = ResponsesStreamEvent::ResponseCompleted { response: final_response };
            yield crate::proxy::responses::stream_event_to_sse(&evt);
            yield crate::proxy::responses::STREAM_DONE.to_string();
        }
    };

    Box::pin(out)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::stream;

    pub fn empty_extra() -> HashMap<String, Value> {
        HashMap::new()
    }

    fn make_user_msg(content: &str) -> ChatMessage {
        ChatMessage {
            role: "user".into(),
            content: Value::String(content.into()),
            tool_calls: None,
            tool_call_id: None,
            extra: empty_extra(),
        }
    }

    #[test]
    fn roundtrip_anthropic_req_basic() {
        let oai = ChatCompletionRequest {
            model: "claude-3-5-sonnet".into(),
            messages: vec![
                ChatMessage {
                    role: "system".into(),
                    content: Value::String("be brief".into()),
                    tool_calls: None,
                    tool_call_id: None,
                    extra: empty_extra(),
                },
                make_user_msg("hi"),
            ],
            stream: false,
            temperature: Some(0.5),
            max_tokens: Some(100),
            tools: None,
            tool_choice: None,
            extra: empty_extra(),
            source_anthropic_request: None,
            request_id: None,
            client_credential: None,
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
        assert_eq!(stop_to_anthropic("tool_calls"), "tool_use");
        assert_eq!(stop_from_anthropic("tool_use"), "tool_calls");
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
                message: OAIMessage {
                    role: "assistant".into(),
                    content: Some("hello".into()),
                    tool_calls: None,
                    refusal: None,
                    extra: empty_extra(),
                },
                finish_reason: Some("stop".into()),
                extra: Default::default(),
            }],
            usage: Some(Usage {
                prompt_tokens: 5,
                completion_tokens: 5,
                total_tokens: 10,
                extra: Default::default(),
            }),
            source_anthropic_response: None,
            extra: Default::default(),
        };
        let ant = oai_resp_to_anthropic_resp(&oai, "claude");
        assert_eq!(ant.content[0].text.as_deref(), Some("hello"));
        assert_eq!(ant.stop_reason.as_deref(), Some("end_turn"));
        assert_eq!(ant.usage.input_tokens, 5);
    }

    // ----- tool-aware tests (Phase 21) -----

    #[test]
    fn roundtrip_anthropic_req_with_tools() {
        // Build an Anthropic request with one tool_use in an assistant
        // message and one tool_result in a user message; convert it to
        // canonical OpenAI; convert back; verify the tool calls survive.
        let mut tool_use_extra: HashMap<String, Value> = HashMap::new();
        tool_use_extra.insert("id".into(), json!("toolu_01"));
        tool_use_extra.insert("name".into(), json!("get_weather"));
        tool_use_extra.insert("input".into(), json!({"city": "SF"}));
        let ant_req = AnthropicRequest {
            model: "claude-opus-4-8".into(),
            max_tokens: 1024,
            system: Some(json!("you can call tools")),
            messages: vec![
                AnthropicMessage {
                    role: "assistant".into(),
                    content: json!([
                        { "type": "text", "text": "calling weather" },
                        { "type": "tool_use", "id": "toolu_01",
                          "name": "get_weather", "input": {"city": "SF"} },
                    ]),
                },
                AnthropicMessage {
                    role: "user".into(),
                    content: json!([
                        { "type": "tool_result",
                          "tool_use_id": "toolu_01",
                          "content": "72F sunny" },
                    ]),
                },
            ],
            temperature: None,
            stream: None,
            stop_sequences: None,
            extra: {
                let mut e = HashMap::new();
                e.insert(
                    "tools".into(),
                    json!([
                        { "name": "get_weather",
                          "description": "Get the weather",
                          "input_schema": { "type": "object",
                                             "properties": { "city": { "type": "string" } } } }
                    ]),
                );
                e.insert(
                    "tool_choice".into(),
                    json!({"type": "tool", "name": "get_weather"}),
                );
                e
            },
        };
        let oai = anthropic_req_to_oai_req(&ant_req);
        assert_eq!(oai.model, "claude-opus-4-8");
        assert!(oai.tools.is_some(), "tools should propagate via extra");
        assert!(oai.tool_choice.is_some(), "tool_choice should propagate");
        // Three messages: system, assistant (with tool_calls), tool (tool_call_id).
        // The original Anthropic user message that contained only a
        // tool_result block was elided (its content moved into the
        // separate role:tool message).
        assert_eq!(oai.messages.len(), 3);
        assert_eq!(oai.messages[0].role, "system");
        let asst = &oai.messages[1];
        assert_eq!(asst.role, "assistant");
        let tcs = asst.tool_calls.as_ref().expect("assistant tool_calls");
        assert_eq!(tcs.len(), 1);
        assert_eq!(tcs[0]["id"], "toolu_01");
        assert_eq!(tcs[0]["function"]["name"], "get_weather");
        // tool_result block should have been promoted to a separate
        // role:tool message.
        let tool_msg = &oai.messages[2];
        assert_eq!(tool_msg.role, "tool");
        assert_eq!(tool_msg.tool_call_id.as_deref(), Some("toolu_01"));
        assert_eq!(tool_msg.content, json!("72F sunny"));

        // Round-trip back to Anthropic; verify the tool_use block survives.
        let ant_back = oai_req_to_anthropic_req(&oai);
        assert_eq!(ant_back.messages.len(), 2);
        let asst_back = &ant_back.messages[0];
        let arr = asst_back
            .content
            .as_array()
            .expect("assistant content array");
        let tool_use_block = arr
            .iter()
            .find(|b| b.get("type").and_then(|v| v.as_str()) == Some("tool_use"))
            .expect("tool_use block present after round-trip");
        assert_eq!(tool_use_block["id"], "toolu_01");
        assert_eq!(tool_use_block["name"], "get_weather");
        assert_eq!(tool_use_block["input"]["city"], "SF");
        let tool_result_msg = &ant_back.messages[1];
        let arr2 = tool_result_msg.content.as_array().unwrap();
        assert_eq!(arr2[0]["type"], "tool_result");
        assert_eq!(arr2[0]["tool_use_id"], "toolu_01");
        assert_eq!(arr2[0]["content"], "72F sunny");
    }

    #[test]
    fn roundtrip_anthropic_resp_with_tool_use() {
        // Build an Anthropic response with one tool_use block; verify it
        // becomes an OpenAI tool_call and the finish_reason maps to
        // tool_calls.
        let mut tool_use = HashMap::new();
        tool_use.insert("id".into(), json!("toolu_42"));
        tool_use.insert("name".into(), json!("search"));
        tool_use.insert("input".into(), json!({"q": "rust"}));
        let ant = AnthropicResponse {
            id: "msg_x".into(),
            resp_type: "message".into(),
            role: "assistant".into(),
            model: "claude".into(),
            content: vec![
                AnthropicContentBlock {
                    block_type: "text".into(),
                    text: Some("let me search".into()),
                    extra: empty_extra(),
                },
                AnthropicContentBlock {
                    block_type: "tool_use".into(),
                    text: None,
                    extra: tool_use,
                },
            ],
            stop_reason: Some("tool_use".into()),
            stop_sequence: None,
            usage: AnthropicUsage {
                input_tokens: 10,
                output_tokens: 5,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
                extra: Default::default(),
            },
            extra: Default::default(),
        };
        let oai = anthropic_resp_to_oai_resp(&ant, "claude");
        let choice = &oai.choices[0];
        assert_eq!(choice.message.content.as_deref(), Some("let me search"));
        let tcs = choice
            .message
            .tool_calls
            .as_ref()
            .expect("tool_calls present");
        assert_eq!(tcs.len(), 1);
        assert_eq!(tcs[0]["id"], "toolu_42");
        assert_eq!(tcs[0]["function"]["name"], "search");
        // canonical_json_string keeps the JSON canonical (no spaces), so
        // we compare the parsed value rather than the literal string.
        let args: Value =
            serde_json::from_str(tcs[0]["function"]["arguments"].as_str().unwrap()).unwrap();
        assert_eq!(args["q"], "rust");
        assert_eq!(choice.finish_reason.as_deref(), Some("tool_calls"));
    }

    #[test]
    fn tool_choice_mapping_round_trip() {
        // Anthropic {"type":"tool","name":"foo"} -> OAI {type:function, ...} ->
        // back to Anthropic {"type":"tool","name":"foo"}.
        let ant_in = json!({"type": "tool", "name": "foo"});
        let oai = tool_choice_anthropic_to_oai(&ant_in);
        assert_eq!(oai["type"], "function");
        assert_eq!(oai["function"]["name"], "foo");
        let ant_back = tool_choice_oai_to_anthropic(&oai);
        assert_eq!(ant_back["type"], "tool");
        assert_eq!(ant_back["name"], "foo");

        // "any" <-> "required"
        assert_eq!(
            tool_choice_oai_to_anthropic(&json!("required")),
            json!({"type": "any"})
        );
        assert_eq!(tool_choice_anthropic_to_oai(&json!("any")), "required");
    }

    /// Parse an Anthropic SSE event payload (data string after `data:` strip)
    /// by extracting just the JSON. We feed JSON strings into the stream
    /// directly — the converter parses them with serde_json::from_str.
    fn sse_chunk_from_data_json(json_str: &str) -> String {
        json_str.to_string()
    }

    #[tokio::test]
    async fn streaming_oai_tool_calls_to_anthropic() {
        // Feed three OAI chunks: (1) role + tool_calls index 0 with id+name,
        // (2) tool_calls index 0 with arguments fragment, (3) finish_reason
        // tool_calls. Expect: message_start, content_block_start tool_use,
        // input_json_delta, content_block_stop, message_delta, message_stop.
        let chunks = vec![
            OAIStreamChunk {
                id: "x".into(),
                object: "chat.completion.chunk".into(),
                created: 1,
                model: "m".into(),
                choices: vec![StreamChoice {
                    index: 0,
                    delta: StreamDelta {
                        role: Some("assistant".into()),
                        content: None,
                        tool_calls: Some(vec![json!({
                            "index": 0,
                            "id": "toolu_1",
                            "type": "function",
                            "function": { "name": "lookup" },
                        })]),
                        reasoning: None,
                        extra: Default::default(),
                    },
                    finish_reason: None,
                    extra: empty_extra(),
                }],
                extra: empty_extra(),
                source_anthropic_event: None,
            },
            OAIStreamChunk {
                id: "x".into(),
                object: "chat.completion.chunk".into(),
                created: 1,
                model: "m".into(),
                choices: vec![StreamChoice {
                    index: 0,
                    delta: StreamDelta {
                        role: None,
                        content: None,
                        tool_calls: Some(vec![json!({
                            "index": 0,
                            "function": { "arguments": "{\"q\":" },
                        })]),
                        reasoning: None,
                        extra: Default::default(),
                    },
                    finish_reason: None,
                    extra: empty_extra(),
                }],
                extra: empty_extra(),
                source_anthropic_event: None,
            },
            OAIStreamChunk {
                id: "x".into(),
                object: "chat.completion.chunk".into(),
                created: 1,
                model: "m".into(),
                choices: vec![StreamChoice {
                    index: 0,
                    delta: StreamDelta {
                        role: None,
                        content: None,
                        tool_calls: Some(vec![json!({
                            "index": 0,
                            "function": { "arguments": "\"rust\"}" },
                        })]),
                        reasoning: None,
                        extra: Default::default(),
                    },
                    finish_reason: Some("tool_calls".into()),
                    extra: empty_extra(),
                }],
                extra: empty_extra(),
                source_anthropic_event: None,
            },
        ];
        let s = stream::iter(chunks);
        let boxed: Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>> = Box::pin(s);
        let mut out = oai_chunks_to_anthropic_stream(boxed, "claude".to_string());
        let mut events: Vec<String> = Vec::new();
        while let Some(line) = out.next().await {
            events.push(line);
        }
        // Expect (at minimum) these event types in order:
        let types: Vec<&str> = events
            .iter()
            .filter_map(|s| {
                if s.starts_with("event: ") {
                    Some(s.trim_start_matches("event: ").lines().next().unwrap_or(""))
                } else {
                    None
                }
            })
            .collect();
        assert!(types.contains(&"message_start"), "events: {types:?}");
        let tool_start_idx = types
            .iter()
            .position(|t| *t == "content_block_start")
            .expect("content_block_start");
        let tool_data: &str = events[tool_start_idx]
            .split("data: ")
            .nth(1)
            .unwrap_or("")
            .split("\n\n")
            .next()
            .unwrap_or("");
        let tool_data: Value = serde_json::from_str(tool_data).unwrap();
        assert_eq!(tool_data["type"], "content_block_start");
        assert_eq!(tool_data["content_block"]["type"], "tool_use");
        assert_eq!(tool_data["content_block"]["id"], "toolu_1");
        assert_eq!(tool_data["content_block"]["name"], "lookup");
        // Two input_json_delta events from the two argument fragments.
        let deltas: Vec<&String> = events
            .iter()
            .filter(|s| s.starts_with("event: content_block_delta"))
            .collect();
        assert_eq!(deltas.len(), 2, "events: {events:?}");
        // Combined arguments should reassemble to {"q":"rust"}.
        let mut combined = String::new();
        for d in &deltas {
            let payload = d
                .split("data: ")
                .nth(1)
                .unwrap_or("")
                .split("\n\n")
                .next()
                .unwrap_or("");
            let v: Value = serde_json::from_str(payload).unwrap();
            if let Some(pj) = v["delta"]["partial_json"].as_str() {
                combined.push_str(pj);
            }
        }
        let parsed: Value = serde_json::from_str(&combined).unwrap();
        assert_eq!(parsed["q"], "rust");
        // message_delta + message_stop
        assert!(types.contains(&"message_delta"));
        assert!(types.contains(&"message_stop"));
        let md_data: Value = serde_json::from_str(
            events
                .iter()
                .find(|s| s.starts_with("event: message_delta"))
                .unwrap()
                .split("data: ")
                .nth(1)
                .unwrap()
                .split("\n\n")
                .next()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(md_data["delta"]["stop_reason"], "tool_use");
    }

    #[tokio::test]
    async fn streaming_oai_text_then_tool_calls() {
        // OpenAI often emits text content first then a separate chunk
        // carrying the tool_calls (id, name, arguments). The text block
        // should close before the tool block opens.
        let chunks = vec![
            OAIStreamChunk {
                id: "x".into(),
                object: "chat.completion.chunk".into(),
                created: 1,
                model: "m".into(),
                choices: vec![StreamChoice {
                    index: 0,
                    delta: StreamDelta {
                        role: Some("assistant".into()),
                        content: Some("hold on".into()),
                        tool_calls: None,
                        reasoning: None,
                        extra: Default::default(),
                    },
                    finish_reason: None,
                    extra: empty_extra(),
                }],
                extra: empty_extra(),
                source_anthropic_event: None,
            },
            OAIStreamChunk {
                id: "x".into(),
                object: "chat.completion.chunk".into(),
                created: 1,
                model: "m".into(),
                choices: vec![StreamChoice {
                    index: 0,
                    delta: StreamDelta {
                        role: None,
                        content: None,
                        tool_calls: Some(vec![json!({
                            "index": 0,
                            "id": "toolu_1",
                            "type": "function",
                            "function": { "name": "lookup", "arguments": "{}" },
                        })]),
                        reasoning: None,
                        extra: Default::default(),
                    },
                    finish_reason: Some("tool_calls".into()),
                    extra: empty_extra(),
                }],
                extra: empty_extra(),
                source_anthropic_event: None,
            },
        ];
        let s = stream::iter(chunks);
        let boxed: Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>> = Box::pin(s);
        let mut out = oai_chunks_to_anthropic_stream(boxed, "claude".to_string());
        let mut events: Vec<String> = Vec::new();
        while let Some(line) = out.next().await {
            events.push(line);
        }
        let types: Vec<&str> = events
            .iter()
            .filter_map(|s| {
                if s.starts_with("event: ") {
                    Some(s.trim_start_matches("event: ").lines().next().unwrap_or(""))
                } else {
                    None
                }
            })
            .collect();
        // text block start, text delta, text block stop, tool block start.
        let text_start = types
            .iter()
            .position(|t| *t == "content_block_start")
            .expect("first content_block_start");
        // Make sure the text block opens first (text_delta appears before
        // the second content_block_start).
        let second_start = types
            .iter()
            .skip(text_start + 1)
            .position(|t| *t == "content_block_start")
            .map(|p| p + text_start + 1)
            .expect("second content_block_start");
        assert!(second_start > text_start);
        // Verify we got at least one text_delta and one input_json_delta.
        assert!(events
            .iter()
            .any(|s| s.starts_with("event: content_block_delta") && s.contains("text_delta")));
        assert!(
            events
                .iter()
                .any(|s| s.starts_with("event: content_block_delta")
                    && s.contains("input_json_delta"))
        );
    }

    #[tokio::test]
    async fn streaming_anthropic_tool_use_to_oai() {
        // Feed Anthropic SSE events: message_start, content_block_start
        // tool_use, content_block_delta input_json_delta, content_block_stop,
        // message_delta (tool_use), message_stop. Expect an OAI delta
        // carrying tool_calls[0] with id + name + arguments.
        let events = vec![
            json!({ "type": "message_start", "message": { "id": "m1", "type": "message", "role": "assistant", "model": "claude", "content": [], "usage": { "input_tokens": 0, "output_tokens": 0 } } }).to_string(),
            json!({ "type": "content_block_start", "index": 1,
                   "content_block": { "type": "tool_use", "id": "toolu_1", "name": "lookup" } }).to_string(),
            json!({ "type": "content_block_delta", "index": 1,
                   "delta": { "type": "input_json_delta", "partial_json": "{\"q\":" } }).to_string(),
            json!({ "type": "content_block_delta", "index": 1,
                   "delta": { "type": "input_json_delta", "partial_json": "\"rust\"}" } }).to_string(),
            json!({ "type": "content_block_stop", "index": 1 }).to_string(),
            json!({ "type": "message_delta", "delta": { "stop_reason": "tool_use", "stop_sequence": null }, "usage": { "input_tokens": 1, "output_tokens": 2 } }).to_string(),
            json!({ "type": "message_stop" }).to_string(),
        ];
        let s = stream::iter(events.into_iter().map(|s| sse_chunk_from_data_json(&s)));
        let boxed: Pin<Box<dyn Stream<Item = String> + Send>> = Box::pin(s);
        let mut out = anthropic_stream_to_oai_chunks(boxed, "claude".to_string());
        let mut chunks = Vec::new();
        while let Some(c) = out.next().await {
            chunks.push(c);
        }
        // First chunk should be the role chunk.
        assert_eq!(
            chunks[0].choices[0].delta.role.as_deref(),
            Some("assistant")
        );
        // Find the chunk carrying tool_calls[0].id
        let id_chunk = chunks
            .iter()
            .find(|c| {
                c.choices.iter().any(|ch| {
                    ch.delta
                        .tool_calls
                        .as_ref()
                        .map(|tcs| tcs.iter().any(|tc| tc.get("id").is_some()))
                        .unwrap_or(false)
                })
            })
            .expect("a chunk with tool_calls[0].id");
        let tcs = id_chunk.choices[0].delta.tool_calls.as_ref().unwrap();
        assert_eq!(tcs[0]["id"], "toolu_1");
        assert_eq!(tcs[0]["type"], "function");
        assert_eq!(tcs[0]["function"]["name"], "lookup");
        // Find the chunk carrying arguments (will be the next chunk).
        let arg_chunks: Vec<&OAIStreamChunk> = chunks
            .iter()
            .filter(|c| {
                c.choices.iter().any(|ch| {
                    ch.delta
                        .tool_calls
                        .as_ref()
                        .map(|tcs| {
                            tcs.iter().any(|tc| {
                                tc.get("function")
                                    .and_then(|f| f.get("arguments"))
                                    .is_some()
                            })
                        })
                        .unwrap_or(false)
                })
            })
            .collect();
        let mut combined = String::new();
        for c in &arg_chunks {
            for ch in &c.choices {
                if let Some(tcs) = &ch.delta.tool_calls {
                    for tc in tcs {
                        if let Some(args) = tc
                            .get("function")
                            .and_then(|f| f.get("arguments"))
                            .and_then(|v| v.as_str())
                        {
                            combined.push_str(args);
                        }
                    }
                }
            }
        }
        let parsed: Value = serde_json::from_str(&combined).unwrap();
        assert_eq!(parsed["q"], "rust");
        let fin = chunks
            .iter()
            .find(|chunk| {
                chunk
                    .choices
                    .first()
                    .and_then(|choice| choice.finish_reason.as_deref())
                    == Some("tool_calls")
            })
            .expect("finish chunk");
        assert_eq!(fin.choices[0].finish_reason.as_deref(), Some("tool_calls"));
    }

    #[test]
    fn anthropic_request_source_preserves_thinking_and_block_order() {
        let request = AnthropicRequest {
            model: "claude-opus-4-8".into(),
            max_tokens: 4096,
            system: Some(json!([
                {"type": "text", "text": "system", "cache_control": {"type": "ephemeral"}}
            ])),
            messages: vec![
                AnthropicMessage {
                    role: "assistant".into(),
                    content: json!([
                        {"type": "thinking", "thinking": "summary", "signature": "opaque-signature"},
                        {"type": "text", "text": "working"},
                        {"type": "tool_use", "id": "tool_1", "name": "lookup", "input": {"q": "rust"}}
                    ]),
                },
                AnthropicMessage {
                    role: "user".into(),
                    content: json!([
                        {"type": "tool_result", "tool_use_id": "tool_1", "content": "result"}
                    ]),
                },
            ],
            temperature: None,
            stream: Some(false),
            stop_sequences: Some(vec!["done".into()]),
            extra: [
                (
                    "thinking".into(),
                    json!({"type": "enabled", "budget_tokens": 2048}),
                ),
                ("metadata".into(), json!({"user_id": "local-user"})),
            ]
            .into_iter()
            .collect(),
        };

        let mut canonical = anthropic_req_to_oai_req(&request);
        canonical.model = "MiniMax-M3".into();
        canonical.stream = true;
        let forwarded = oai_req_to_anthropic_req(&canonical);

        assert_eq!(forwarded.model, "MiniMax-M3");
        assert_eq!(forwarded.stream, Some(true));
        assert_eq!(forwarded.system, request.system);
        assert_eq!(
            serde_json::to_value(&forwarded.messages).unwrap(),
            serde_json::to_value(&request.messages).unwrap()
        );
        assert_eq!(forwarded.stop_sequences, request.stop_sequences);
        assert_eq!(forwarded.extra["thinking"], request.extra["thinking"]);
        assert_eq!(forwarded.extra["metadata"], request.extra["metadata"]);
    }

    #[test]
    fn anthropic_response_source_preserves_unknown_fields_and_order() {
        let source = json!({
            "id": "msg_source",
            "type": "message",
            "role": "assistant",
            "model": "MiniMax-M3",
            "content": [
                {"type": "thinking", "thinking": "summary", "signature": "opaque"},
                {"type": "text", "text": "answer"}
            ],
            "stop_reason": "end_turn",
            "stop_sequence": null,
            "usage": {"input_tokens": 5, "output_tokens": 7, "cache_read_input_tokens": 2},
            "base_resp": {"status_code": 0}
        });
        let parsed: AnthropicResponse = serde_json::from_value(source.clone()).unwrap();
        let mut canonical = anthropic_resp_to_oai_resp(&parsed, "claude-opus-4-8");
        canonical.source_anthropic_response = Some(source);

        let relayed = oai_resp_to_anthropic_resp(&canonical, "claude-opus-4-8");
        let relayed_value = serde_json::to_value(relayed).unwrap();
        assert_eq!(relayed_value["model"], "claude-opus-4-8");
        assert_eq!(relayed_value["content"][0]["signature"], "opaque");
        assert_eq!(relayed_value["content"][1]["type"], "text");
        assert_eq!(relayed_value["base_resp"]["status_code"], 0);
        assert_eq!(relayed_value["usage"]["cache_read_input_tokens"], 2);
    }

    #[tokio::test]
    async fn anthropic_stream_relay_preserves_signature_and_usage() {
        let source_events = vec![
            json!({"type": "message_start", "message": {
                "id": "msg_1", "type": "message", "role": "assistant", "model": "MiniMax-M3",
                "content": [], "stop_reason": null, "stop_sequence": null,
                "usage": {"input_tokens": 5, "output_tokens": 0, "cache_read_input_tokens": 2, "cache_creation_input_tokens": 1}
            }}),
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": ""}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "reason"}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "opaque-signature"}}),
            json!({"type": "content_block_stop", "index": 0}),
            json!({"type": "content_block_start", "index": 1, "content_block": {"type": "text", "text": ""}}),
            json!({"type": "content_block_delta", "index": 1, "delta": {"type": "text_delta", "text": "answer"}}),
            json!({"type": "content_block_stop", "index": 1}),
            json!({"type": "message_delta", "delta": {"stop_reason": "end_turn", "stop_sequence": null}, "usage": {"output_tokens": 7}}),
            json!({"type": "message_stop"}),
        ];
        let source_stream = stream::iter(
            source_events
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>(),
        );
        let chunks: Vec<OAIStreamChunk> =
            anthropic_stream_to_oai_chunks(source_stream, "MiniMax-M3".into())
                .collect()
                .await;

        let usage = chunks
            .iter()
            .filter_map(|chunk| chunk.extra.get("usage"))
            .last()
            .expect("normalized usage");
        assert_eq!(usage["prompt_tokens"], 8);
        assert_eq!(usage["completion_tokens"], 7);
        assert_eq!(usage["total_tokens"], 15);

        let mut relayed =
            oai_chunks_to_anthropic_stream(stream::iter(chunks), "claude-opus-4-8".into());
        let mut relayed_values = Vec::new();
        while let Some(frame) = relayed.next().await {
            let payload = frame
                .split("data: ")
                .nth(1)
                .unwrap()
                .split("\n\n")
                .next()
                .unwrap();
            relayed_values.push(serde_json::from_str::<Value>(payload).unwrap());
        }
        let relayed_types: Vec<&str> = relayed_values
            .iter()
            .filter_map(|value| value.get("type").and_then(Value::as_str))
            .collect();
        let source_types: Vec<&str> = source_events
            .iter()
            .filter_map(|value| value.get("type").and_then(Value::as_str))
            .collect();
        assert_eq!(relayed_types, source_types);
        assert_eq!(relayed_values[0]["message"]["model"], "claude-opus-4-8");
        assert_eq!(relayed_values[3]["delta"]["signature"], "opaque-signature");
        assert_eq!(relayed_values[8]["usage"]["output_tokens"], 7);
    }

    #[test]
    fn stream_delta_accepts_reasoning_content_alias() {
        let delta: StreamDelta = serde_json::from_value(json!({
            "reasoning_content": "thinking"
        }))
        .unwrap();
        assert_eq!(delta.reasoning.as_deref(), Some("thinking"));
        let serialized = serde_json::to_value(delta).unwrap();
        assert_eq!(serialized["reasoning_content"], "thinking");
        assert!(serialized.get("reasoning").is_none());
    }

    #[test]
    fn strip_billing_header() {
        assert_eq!(
            strip_leading_anthropic_billing_header(
                "x-anthropic-billing-header: key=abc\nreal system prompt"
            ),
            "real system prompt"
        );
        assert_eq!(
            strip_leading_anthropic_billing_header("no header here"),
            "no header here"
        );
    }
}

#[cfg(test)]
mod responses_tests {
    use super::*;
    use crate::proxy::responses::ResponsesRequest;
    use futures_util::StreamExt;

    fn empty_extra() -> HashMap<String, Value> {
        HashMap::new()
    }

    #[test]
    fn responses_request_string_input_to_chat() {
        let req = ResponsesRequest {
            model: "gpt-5".into(),
            input: serde_json::json!("hello"),
            instructions: Some("be brief".into()),
            stream: false,
            temperature: Some(0.5),
            max_output_tokens: Some(64),
            tools: None,
            metadata: None,
            previous_response_id: None,
            extra: empty_extra(),
        };
        let chat = responses_to_chat(&req);
        assert_eq!(chat.model, "gpt-5");
        assert_eq!(chat.messages.len(), 2);
        assert_eq!(chat.messages[0].role, "system");
        assert_eq!(chat.messages[0].content, serde_json::json!("be brief"));
        assert_eq!(chat.messages[1].role, "user");
        assert_eq!(chat.messages[1].content, serde_json::json!("hello"));
        assert_eq!(chat.temperature, Some(0.5));
        assert_eq!(chat.max_tokens, Some(64));
    }

    #[test]
    fn responses_request_array_input_to_chat() {
        let req = ResponsesRequest {
            model: "gpt-5".into(),
            input: serde_json::json!([
                { "role": "user", "content": "first" },
                { "role": "assistant", "content": "ok" },
                { "role": "user", "content": [
                    { "type": "text", "text": "second" },
                ]},
            ]),
            instructions: None,
            stream: false,
            temperature: None,
            max_output_tokens: None,
            tools: None,
            metadata: None,
            previous_response_id: None,
            extra: empty_extra(),
        };
        let chat = responses_to_chat(&req);
        assert_eq!(chat.messages.len(), 3);
        assert_eq!(chat.messages[0].role, "user");
        assert_eq!(chat.messages[1].role, "assistant");
        assert_eq!(chat.messages[2].role, "user");
        let text = chat.messages[2].content.as_array().unwrap();
        assert_eq!(text[0]["text"], "second");
    }

    #[test]
    fn chat_response_to_responses_envelope() {
        let chat = OAIChatResponse {
            id: "chatcmpl-1".into(),
            object: "chat.completion".into(),
            created: 1_700_000_000,
            model: "gpt-5".into(),
            choices: vec![OAIChoice {
                index: 0,
                message: OAIMessage {
                    role: "assistant".into(),
                    content: Some("hi there".into()),
                    tool_calls: None,
                    refusal: None,
                    extra: empty_extra(),
                },
                finish_reason: Some("stop".into()),
                extra: Default::default(),
            }],
            usage: Some(Usage {
                prompt_tokens: 5,
                completion_tokens: 7,
                total_tokens: 12,
                extra: Default::default(),
            }),
            source_anthropic_response: None,
            extra: Default::default(),
        };
        let r = chat_to_responses(&chat, "gpt-5");
        assert_eq!(r["object"], "response");
        assert_eq!(r["status"], "completed");
        assert_eq!(r["id"], "chatcmpl-1");
        assert!(
            r["error"].is_null(),
            "stateless envelope carries a null error"
        );
        let output = r["output"].as_array().unwrap();
        assert_eq!(output.len(), 1);
        assert_eq!(output[0]["type"], "message");
        assert_eq!(output[0]["role"], "assistant");
        assert_eq!(output[0]["content"][0]["type"], "output_text");
        assert_eq!(output[0]["content"][0]["text"], "hi there");
        assert_eq!(r["usage"]["input_tokens"], 5);
        assert_eq!(r["usage"]["output_tokens"], 7);
        assert_eq!(r["usage"]["total_tokens"], 12);
    }

    #[test]
    fn chat_response_tool_calls_become_function_call_items() {
        let chat = OAIChatResponse {
            id: "chatcmpl-2".into(),
            object: "chat.completion".into(),
            created: 1_700_000_000,
            model: "gpt-5".into(),
            choices: vec![OAIChoice {
                index: 0,
                message: OAIMessage {
                    role: "assistant".into(),
                    content: None,
                    tool_calls: Some(vec![serde_json::json!({
                        "id": "call_abc",
                        "type": "function",
                        "function": { "name": "shell", "arguments": "{\"cmd\":\"ls\"}" },
                    })]),
                    refusal: None,
                    extra: empty_extra(),
                },
                finish_reason: Some("tool_calls".into()),
                extra: Default::default(),
            }],
            usage: None,
            source_anthropic_response: None,
            extra: Default::default(),
        };
        let r = chat_to_responses(&chat, "gpt-5");
        let output = r["output"].as_array().unwrap();
        assert_eq!(output.len(), 1, "tool-only response has no message item");
        assert_eq!(output[0]["type"], "function_call");
        assert_eq!(output[0]["call_id"], "call_abc");
        assert_eq!(output[0]["name"], "shell");
        assert_eq!(output[0]["arguments"], "{\"cmd\":\"ls\"}");
        assert_eq!(output[0]["status"], "completed");
    }

    #[test]
    fn responses_input_tool_items_convert_round_trip() {
        let req = ResponsesRequest {
            model: "gpt-5".into(),
            input: serde_json::json!([
                { "type": "message", "role": "user", "content": "list files" },
                { "type": "function_call", "call_id": "call_1", "name": "shell", "arguments": "{\"cmd\":\"ls\"}" },
                { "type": "function_call_output", "call_id": "call_1", "output": "a.txt" },
                { "type": "reasoning", "summary": [] },
            ]),
            instructions: None,
            stream: false,
            temperature: None,
            max_output_tokens: None,
            tools: Some(serde_json::json!([
                { "type": "function", "name": "shell", "description": "run a command", "parameters": { "type": "object", "properties": {} } },
                { "type": "web_search" },
            ])),
            metadata: None,
            previous_response_id: None,
            extra: [(
                "tool_choice".to_string(),
                serde_json::json!({"type": "function", "name": "shell"}),
            )]
            .into_iter()
            .collect(),
        };
        let chat = responses_to_chat(&req);
        assert_eq!(chat.messages.len(), 3, "reasoning item is dropped");

        assert_eq!(chat.messages[1].role, "assistant");
        let calls = chat.messages[1].tool_calls.as_ref().unwrap();
        assert_eq!(calls[0]["id"], "call_1");
        assert_eq!(calls[0]["function"]["name"], "shell");
        assert_eq!(calls[0]["function"]["arguments"], "{\"cmd\":\"ls\"}");

        assert_eq!(chat.messages[2].role, "tool");
        assert_eq!(chat.messages[2].tool_call_id.as_deref(), Some("call_1"));
        assert_eq!(chat.messages[2].content, serde_json::json!("a.txt"));

        // Flat Responses tools -> nested chat tools; non-function tools dropped.
        let tools = chat.tools.as_ref().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[0]["function"]["name"], "shell");
        // {type:function,name} tool_choice -> nested form.
        assert_eq!(chat.tool_choice.unwrap()["function"]["name"], "shell");
    }

    #[tokio::test]
    async fn responses_stream_emits_created_delta_done_completed() {
        let chunks = vec![
            OAIStreamChunk {
                id: "x".into(),
                object: "chat.completion.chunk".into(),
                created: 1,
                model: "gpt-5".into(),
                choices: vec![StreamChoice {
                    index: 0,
                    delta: StreamDelta {
                        role: Some("assistant".into()),
                        content: Some("hel".into()),
                        tool_calls: None,
                        reasoning: None,
                        extra: Default::default(),
                    },
                    finish_reason: None,
                    extra: empty_extra(),
                }],
                extra: empty_extra(),
                source_anthropic_event: None,
            },
            OAIStreamChunk {
                id: "x".into(),
                object: "chat.completion.chunk".into(),
                created: 1,
                model: "gpt-5".into(),
                choices: vec![StreamChoice {
                    index: 0,
                    delta: StreamDelta {
                        role: None,
                        content: Some("lo".into()),
                        tool_calls: None,
                        reasoning: None,
                        extra: Default::default(),
                    },
                    finish_reason: Some("stop".into()),
                    extra: empty_extra(),
                }],
                extra: empty_extra(),
                source_anthropic_event: None,
            },
        ];
        let s = futures_util::stream::iter(chunks);
        let boxed: Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>> = Box::pin(s);
        let mut out = chat_chunks_to_responses_stream(boxed, "gpt-5");
        let mut events: Vec<String> = Vec::new();
        while let Some(line) = out.next().await {
            events.push(line);
        }
        assert_eq!(events.len(), 8, "got: {events:?}");
        assert!(events[0].starts_with("event: response.created\ndata: "));
        assert!(events[1].starts_with("event: response.output_item.added\ndata: "));
        assert!(
            events[2].contains("event: response.output_text.delta"),
            "{}",
            events[2]
        );
        assert!(events[2].contains("\"delta\":\"hel\""));
        assert!(events[3].contains("\"delta\":\"lo\""));
        assert!(events[4].starts_with("event: response.output_text.done\ndata: "));
        assert!(events[5].starts_with("event: response.output_item.done\ndata: "));
        assert!(events[6].starts_with("event: response.completed\ndata: "));
        assert_eq!(events[7], crate::proxy::responses::STREAM_DONE);
        // The completed envelope must carry the upstream usage, not zeros.
        let completed: Value =
            serde_json::from_str(events[6].split("data: ").nth(1).unwrap().trim_end()).unwrap();
        assert_eq!(completed["response"]["status"], "completed");
        assert_eq!(completed["response"]["output"][0]["type"], "message");
    }

    /// Streaming tool calls must surface as function_call items with
    /// argument deltas — Codex CLI executes tools from exactly these
    /// events, so dropping the chat `tool_calls` deltas (the previous
    /// behavior) made every streamed tool turn unusable.
    #[tokio::test]
    async fn responses_stream_relays_tool_call_deltas_and_usage() {
        let chunks = vec![
            OAIStreamChunk {
                id: "x".into(),
                object: "chat.completion.chunk".into(),
                created: 1,
                model: "gpt-5".into(),
                choices: vec![StreamChoice {
                    index: 0,
                    delta: StreamDelta {
                        role: Some("assistant".into()),
                        content: None,
                        tool_calls: Some(vec![serde_json::json!({
                            "index": 0,
                            "id": "call_1",
                            "type": "function",
                            "function": { "name": "shell", "arguments": "" },
                        })]),
                        reasoning: None,
                        extra: Default::default(),
                    },
                    finish_reason: None,
                    extra: empty_extra(),
                }],
                extra: empty_extra(),
                source_anthropic_event: None,
            },
            OAIStreamChunk {
                id: "x".into(),
                object: "chat.completion.chunk".into(),
                created: 1,
                model: "gpt-5".into(),
                choices: vec![StreamChoice {
                    index: 0,
                    delta: StreamDelta {
                        role: None,
                        content: None,
                        tool_calls: Some(vec![serde_json::json!({
                            "index": 0,
                            "function": { "arguments": "{\"cmd\":\"ls\"}" },
                        })]),
                        reasoning: None,
                        extra: Default::default(),
                    },
                    finish_reason: Some("tool_calls".into()),
                    extra: empty_extra(),
                }],
                extra: [(
                    "usage".to_string(),
                    serde_json::json!({
                        "prompt_tokens": 9,
                        "completion_tokens": 4,
                        "total_tokens": 13,
                    }),
                )]
                .into_iter()
                .collect(),
                source_anthropic_event: None,
            },
        ];
        let boxed: Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>> =
            Box::pin(futures_util::stream::iter(chunks));
        let mut out = chat_chunks_to_responses_stream(boxed, "gpt-5");
        let mut events: Vec<String> = Vec::new();
        while let Some(line) = out.next().await {
            events.push(line);
        }
        let types: Vec<&str> = events
            .iter()
            .filter_map(|line| line.strip_prefix("event: "))
            .filter_map(|rest| rest.split('\n').next())
            .collect();
        assert!(types.contains(&"response.output_item.added"), "{types:?}");
        assert!(
            types.contains(&"response.function_call_arguments.delta"),
            "{types:?}"
        );
        assert!(
            types.contains(&"response.function_call_arguments.done"),
            "{types:?}"
        );

        let added = events
            .iter()
            .find(|line| {
                line.contains("response.output_item.added") && line.contains("function_call")
            })
            .expect("function_call output_item.added event");
        assert!(added.contains("\"type\":\"function_call\""), "{added}");
        assert!(added.contains("\"name\":\"shell\""), "{added}");

        let args_delta = events
            .iter()
            .find(|line| line.contains("response.function_call_arguments.delta"))
            .unwrap();
        assert!(
            args_delta.contains("\"delta\":\"{\\\"cmd\\\""),
            "{args_delta}"
        );

        let completed_line = events
            .iter()
            .find(|line| line.starts_with("event: response.completed\n"))
            .expect("completed event");
        let completed: Value =
            serde_json::from_str(completed_line.split("data: ").nth(1).unwrap().trim_end())
                .unwrap();
        assert_eq!(completed["response"]["usage"]["input_tokens"], 9);
        assert_eq!(completed["response"]["usage"]["output_tokens"], 4);
        let output = completed["response"]["output"].as_array().unwrap();
        assert_eq!(
            output.len(),
            1,
            "tool-only turn has no phantom message item"
        );
        assert!(
            output
                .iter()
                .any(|item| item["type"] == "function_call"
                    && item["arguments"] == "{\"cmd\":\"ls\"}"),
            "final output carries the full function call: {output:?}"
        );
    }

    /// An in-stream error chunk must become `response.failed` with the
    /// failed status envelope (not a chat-completions style `error` event).
    #[tokio::test]
    async fn responses_stream_error_becomes_response_failed() {
        let error_chunk =
            crate::proxy::canonical::stream_error_chunk("m", 502, "proxy_stream_error");
        let boxed: Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>> =
            Box::pin(futures_util::stream::iter(vec![error_chunk]));
        let mut out = chat_chunks_to_responses_stream(boxed, "gpt-5");
        let mut events: Vec<String> = Vec::new();
        while let Some(line) = out.next().await {
            events.push(line);
        }
        assert_eq!(events.len(), 1, "failed event is terminal, got: {events:?}");
        assert!(
            events[0].starts_with("event: response.failed\ndata: "),
            "{}",
            events[0]
        );
        let failed: Value =
            serde_json::from_str(events[0].split("data: ").nth(1).unwrap().trim_end()).unwrap();
        assert_eq!(failed["response"]["status"], "failed");
        assert_eq!(failed["response"]["error"]["code"], "proxy_stream_error");
        assert!(
            !events[0].contains("[DONE]"),
            "no success terminator after failure"
        );
    }

    #[test]
    fn responses_request_drop_previous_response_id() {
        let req = ResponsesRequest {
            model: "gpt-5".into(),
            input: serde_json::json!("hi"),
            instructions: None,
            stream: false,
            temperature: None,
            max_output_tokens: None,
            tools: None,
            metadata: None,
            previous_response_id: Some("resp_xxx".into()),
            extra: empty_extra(),
        };
        let chat = responses_to_chat(&req);
        assert_eq!(chat.model, "gpt-5");
        assert_eq!(chat.messages.len(), 1);
    }

    #[test]
    fn responses_request_drops_non_text_parts() {
        // Test that input_content_to_chat drops non-text parts
        let req = ResponsesRequest {
            model: "gpt-5".into(),
            input: serde_json::json!([
                { "role": "user", "content": [
                    { "type": "input_image", "image_url": "https://example.com/image.png" },
                    { "type": "input_text", "text": "describe this" },
                ]},
            ]),
            instructions: None,
            stream: false,
            temperature: None,
            max_output_tokens: None,
            tools: None,
            metadata: None,
            previous_response_id: None,
            extra: empty_extra(),
        };
        let chat = responses_to_chat(&req);
        assert_eq!(chat.messages.len(), 1);
        let content = &chat.messages[0].content;
        // The image part should be dropped, only text remains
        let arr = content.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["type"], "text");
        assert_eq!(arr[0]["text"], "describe this");
    }

    #[test]
    fn responses_request_image_only_collapses_to_empty_string() {
        // Image-only content (no text parts to keep) used to round-trip as
        // an empty `content` array, which strict OpenAI-compatible upstreams
        // reject with 400. Collapse to an empty string instead.
        let req = ResponsesRequest {
            model: "gpt-5".into(),
            input: serde_json::json!([
                { "role": "user", "content": [
                    { "type": "input_image", "image_url": "https://example.com/image.png" },
                ]},
            ]),
            instructions: None,
            stream: false,
            temperature: None,
            max_output_tokens: None,
            tools: None,
            metadata: None,
            previous_response_id: None,
            extra: empty_extra(),
        };
        let chat = responses_to_chat(&req);
        assert_eq!(chat.messages.len(), 1);
        assert_eq!(chat.messages[0].content, serde_json::json!(""));
    }

    #[test]
    fn responses_request_missing_content_becomes_empty_string() {
        // Items with no `content` key used to flatten into null content,
        // which upstreams reject. Empty string is always valid.
        let req = ResponsesRequest {
            model: "gpt-5".into(),
            input: serde_json::json!([
                { "role": "user" },
            ]),
            instructions: None,
            stream: false,
            temperature: None,
            max_output_tokens: None,
            tools: None,
            metadata: None,
            previous_response_id: None,
            extra: empty_extra(),
        };
        let chat = responses_to_chat(&req);
        assert_eq!(chat.messages.len(), 1);
        assert_eq!(chat.messages[0].content, serde_json::json!(""));
    }

    #[test]
    fn responses_request_image_only_serializes_as_empty_string() {
        let req = ResponsesRequest {
            model: "gpt-5".into(),
            input: serde_json::json!([
                { "role": "user", "content": [
                    { "type": "input_image", "image_url": "https://example.com/image.png" },
                ]},
            ]),
            instructions: None,
            stream: false,
            temperature: None,
            max_output_tokens: None,
            tools: None,
            metadata: None,
            previous_response_id: None,
            extra: empty_extra(),
        };
        let chat = responses_to_chat(&req);
        let serialized = serde_json::to_value(&chat).unwrap();
        let messages = serialized["messages"].as_array().unwrap();
        assert_eq!(messages[0]["content"], serde_json::json!(""));
    }

    #[test]
    fn responses_tools_flat_get_wrapped_for_chat_completions() {
        // The Responses API sends tools in a flat shape; Chat Completions
        // requires the function payload nested under `function`. Without
        // this conversion every Codex request is a 400 upstream.
        let req = ResponsesRequest {
            model: "gpt-5".into(),
            input: serde_json::json!("hi"),
            instructions: None,
            stream: false,
            temperature: None,
            max_output_tokens: None,
            tools: Some(serde_json::json!([
                {
                    "type": "function",
                    "name": "shell",
                    "description": "run shell",
                    "parameters": { "type": "object", "properties": { "cmd": {"type": "string"} } },
                    "strict": false
                },
                {
                    "type": "function",
                    "name": "apply_patch",
                    "description": "apply a patch",
                    "strict": false
                }
            ])),
            metadata: None,
            previous_response_id: None,
            extra: empty_extra(),
        };
        let chat = responses_to_chat(&req);
        let tools = chat.tools.expect("tools must survive conversion");
        assert_eq!(tools.len(), 2);
        let shell = &tools[0];
        assert_eq!(shell["type"], "function");
        assert_eq!(shell["function"]["name"], "shell");
        assert_eq!(shell["function"]["description"], "run shell");
        assert_eq!(shell["function"]["strict"], false);
        assert_eq!(shell["function"]["parameters"]["type"], "object");
        let patch = &tools[1];
        assert_eq!(patch["function"]["name"], "apply_patch");
        // apply_patch ships without parameters; default an empty schema so
        // strict upstreams don't reject the request.
        assert_eq!(patch["function"]["parameters"]["type"], "object");
        assert_eq!(
            patch["function"]["parameters"]["properties"],
            serde_json::json!({})
        );
    }

    #[test]
    fn responses_tools_non_function_types_are_dropped() {
        let req = ResponsesRequest {
            model: "gpt-5".into(),
            input: serde_json::json!("hi"),
            instructions: None,
            stream: false,
            temperature: None,
            max_output_tokens: None,
            tools: Some(serde_json::json!([
                { "type": "web_search" },
                { "type": "function", "name": "shell", "parameters": {} },
            ])),
            metadata: None,
            previous_response_id: None,
            extra: empty_extra(),
        };
        let chat = responses_to_chat(&req);
        let tools = chat.tools.expect("function tool should survive");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["function"]["name"], "shell");
    }

    #[test]
    fn responses_tools_already_in_chat_shape_pass_through() {
        let req = ResponsesRequest {
            model: "gpt-5".into(),
            input: serde_json::json!("hi"),
            instructions: None,
            stream: false,
            temperature: None,
            max_output_tokens: None,
            tools: Some(serde_json::json!([
                { "type": "function", "function": { "name": "shell", "parameters": {} } }
            ])),
            metadata: None,
            previous_response_id: None,
            extra: empty_extra(),
        };
        let chat = responses_to_chat(&req);
        let tools = chat.tools.expect("already-shaped tool must survive");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["function"]["name"], "shell");
    }

    #[test]
    fn responses_tools_all_unusable_becomes_none() {
        // Don't emit `"tools": []` on the wire — that is a guaranteed 400
        // from strict upstreams.
        let req = ResponsesRequest {
            model: "gpt-5".into(),
            input: serde_json::json!("hi"),
            instructions: None,
            stream: false,
            temperature: None,
            max_output_tokens: None,
            tools: Some(serde_json::json!([
                { "type": "web_search" },
                { "type": "local_shell" }
            ])),
            metadata: None,
            previous_response_id: None,
            extra: empty_extra(),
        };
        let chat = responses_to_chat(&req);
        assert!(chat.tools.is_none());
    }

    #[test]
    fn responses_function_call_items_become_assistant_tool_calls() {
        let req = ResponsesRequest {
            model: "gpt-5".into(),
            input: serde_json::json!([
                { "role": "user", "content": "list files" },
                { "type": "function_call",
                  "call_id": "call_1",
                  "name": "shell",
                  "arguments": "{\"cmd\":\"ls\"}" },
                { "type": "function_call_output",
                  "call_id": "call_1",
                  "output": "file1.txt\nfile2.txt" },
                { "type": "reasoning",
                  "id": "r_1",
                  "summary": [{ "type": "summary_text", "text": "thinking" }] }
            ]),
            instructions: None,
            stream: false,
            temperature: None,
            max_output_tokens: None,
            tools: None,
            metadata: None,
            previous_response_id: None,
            extra: empty_extra(),
        };
        let chat = responses_to_chat(&req);
        // user + assistant(tool_calls) + tool — reasoning is dropped.
        assert_eq!(chat.messages.len(), 3);
        assert_eq!(chat.messages[0].role, "user");
        assert_eq!(chat.messages[1].role, "assistant");
        assert!(
            chat.messages[1].content.is_null(),
            "assistant tool_calls message content must be null, got {:?}",
            chat.messages[1].content
        );
        let tc = chat.messages[1]
            .tool_calls
            .as_ref()
            .expect("assistant tool_calls must be set");
        assert_eq!(tc.len(), 1);
        assert_eq!(tc[0]["id"], "call_1");
        assert_eq!(tc[0]["type"], "function");
        assert_eq!(tc[0]["function"]["name"], "shell");
        assert_eq!(tc[0]["function"]["arguments"], "{\"cmd\":\"ls\"}");
        assert_eq!(chat.messages[2].role, "tool");
        assert_eq!(chat.messages[2].tool_call_id.as_deref(), Some("call_1"));
        assert_eq!(
            chat.messages[2].content,
            serde_json::json!("file1.txt\nfile2.txt")
        );
    }

    #[test]
    fn responses_developer_role_maps_to_system() {
        let req = ResponsesRequest {
            model: "gpt-5".into(),
            input: serde_json::json!([
                { "role": "developer", "content": "be terse" },
                { "role": "developer", "content": [
                    { "type": "input_text", "text": "be terse with parts" }
                ]},
            ]),
            instructions: None,
            stream: false,
            temperature: None,
            max_output_tokens: None,
            tools: None,
            metadata: None,
            previous_response_id: None,
            extra: empty_extra(),
        };
        let chat = responses_to_chat(&req);
        assert_eq!(chat.messages.len(), 2);
        assert_eq!(chat.messages[0].role, "system");
        assert_eq!(chat.messages[0].content, serde_json::json!("be terse"));
        assert_eq!(chat.messages[1].role, "system");
        let arr = chat.messages[1].content.as_array().unwrap();
        assert_eq!(arr[0]["text"], "be terse with parts");
    }

    #[test]
    fn responses_tool_choice_string_passes_through() {
        let mut extra = empty_extra();
        extra.insert("tool_choice".into(), Value::String("auto".into()));
        let req = ResponsesRequest {
            model: "gpt-5".into(),
            input: serde_json::json!("hi"),
            instructions: None,
            stream: false,
            temperature: None,
            max_output_tokens: None,
            tools: None,
            metadata: None,
            previous_response_id: None,
            extra,
        };
        let chat = responses_to_chat(&req);
        assert_eq!(chat.tool_choice, Some(serde_json::json!("auto")));
    }

    #[test]
    fn responses_tool_choice_function_object_is_rewrapped() {
        let mut extra = empty_extra();
        extra.insert(
            "tool_choice".into(),
            serde_json::json!({ "type": "function", "name": "shell" }),
        );
        let req = ResponsesRequest {
            model: "gpt-5".into(),
            input: serde_json::json!("hi"),
            instructions: None,
            stream: false,
            temperature: None,
            max_output_tokens: None,
            tools: None,
            metadata: None,
            previous_response_id: None,
            extra,
        };
        let chat = responses_to_chat(&req);
        assert_eq!(
            chat.tool_choice,
            Some(serde_json::json!({ "type": "function", "function": { "name": "shell" } }))
        );
    }

    #[test]
    fn chat_response_with_tool_calls_emits_function_call_items() {
        let chat = OAIChatResponse {
            id: "chatcmpl-1".into(),
            object: "chat.completion".into(),
            created: 1_700_000_000,
            model: "gpt-5".into(),
            choices: vec![OAIChoice {
                index: 0,
                message: OAIMessage {
                    role: "assistant".into(),
                    content: None,
                    tool_calls: Some(vec![serde_json::json!({
                        "id": "call_1",
                        "type": "function",
                        "function": { "name": "shell", "arguments": "{\"cmd\":\"ls\"}" }
                    })]),
                    refusal: None,
                    extra: empty_extra(),
                },
                finish_reason: Some("tool_calls".into()),
                extra: Default::default(),
            }],
            usage: None,
            source_anthropic_response: None,
            extra: Default::default(),
        };
        let r = chat_to_responses(&chat, "gpt-5");
        assert_eq!(r["object"], "response");
        // A tool-only assistant turn must not gain a phantom empty message.
        let output = r["output"].as_array().unwrap();
        assert_eq!(output.len(), 1);
        assert_eq!(output[0]["type"], "function_call");
        assert_eq!(output[0]["call_id"], "call_1");
        assert_eq!(output[0]["name"], "shell");
        assert_eq!(output[0]["arguments"], "{\"cmd\":\"ls\"}");
        assert_eq!(output[0]["status"], "completed");
    }

    #[tokio::test]
    async fn responses_stream_with_tool_call_deltas_emits_function_call_events() {
        let chunks = vec![
            OAIStreamChunk {
                id: "x".into(),
                object: "chat.completion.chunk".into(),
                created: 1,
                model: "gpt-5".into(),
                choices: vec![StreamChoice {
                    index: 0,
                    delta: StreamDelta {
                        role: Some("assistant".into()),
                        content: None,
                        tool_calls: Some(vec![serde_json::json!({
                            "index": 0,
                            "id": "call_1",
                            "type": "function",
                            "function": { "name": "shell", "arguments": "{\"cmd\":" }
                        })]),
                        reasoning: None,
                        extra: Default::default(),
                    },
                    finish_reason: None,
                    extra: empty_extra(),
                }],
                extra: empty_extra(),
                source_anthropic_event: None,
            },
            OAIStreamChunk {
                id: "x".into(),
                object: "chat.completion.chunk".into(),
                created: 1,
                model: "gpt-5".into(),
                choices: vec![StreamChoice {
                    index: 0,
                    delta: StreamDelta {
                        role: None,
                        content: None,
                        tool_calls: Some(vec![serde_json::json!({
                            "index": 0,
                            "function": { "arguments": "\"ls\"}" }
                        })]),
                        reasoning: None,
                        extra: Default::default(),
                    },
                    finish_reason: Some("tool_calls".into()),
                    extra: empty_extra(),
                }],
                extra: empty_extra(),
                source_anthropic_event: None,
            },
        ];
        let s = futures_util::stream::iter(chunks);
        let boxed: Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>> = Box::pin(s);
        let mut out = chat_chunks_to_responses_stream(boxed, "gpt-5");
        let mut events: Vec<String> = Vec::new();
        while let Some(line) = out.next().await {
            events.push(line);
        }
        // A tool-only turn opens only the function_call item: created,
        // item added, argument deltas, arguments done, item done,
        // response.completed, [DONE].
        assert_eq!(events.len(), 8, "got: {events:?}");
        assert!(events[0].starts_with("event: response.created\ndata: "));
        assert!(events[1].starts_with("event: response.output_item.added\ndata: "));
        assert!(events[1].contains("\"output_index\":0"));
        assert!(events[1].contains("\"type\":\"function_call\""));
        assert!(events[1].contains("\"call_id\":\"call_1\""));
        assert!(events[2].starts_with("event: response.function_call_arguments.delta\ndata: "));
        assert!(events[3].starts_with("event: response.function_call_arguments.delta\ndata: "));
        assert!(events[4].starts_with("event: response.function_call_arguments.done\ndata: "));
        assert!(events[4].contains("{\\\"cmd\\\":\\\"ls\\\"}"));
        assert!(events[5].starts_with("event: response.output_item.done\ndata: "));
        assert!(events[5].contains("\"output_index\":0"));
        assert!(events[5].contains("\"type\":\"function_call\""));
        // event 6: response.completed containing the function_call item.
        assert!(events[6].starts_with("event: response.completed\ndata: "));
        let completed: serde_json::Value = serde_json::from_str(
            events[6]
                .trim_start_matches("event: response.completed\ndata: ")
                .trim(),
        )
        .unwrap();
        let output = completed["response"]["output"].as_array().unwrap();
        assert_eq!(output.len(), 1);
        assert_eq!(output[0]["type"], "function_call");
        assert_eq!(output[0]["call_id"], "call_1");
        assert_eq!(output[0]["arguments"], "{\"cmd\":\"ls\"}");
        assert_eq!(events[7], crate::proxy::responses::STREAM_DONE);
    }
}

#[cfg(test)]
#[path = "converter_protocol_tests.rs"]
mod anthropic_tool_boundary_tests;
