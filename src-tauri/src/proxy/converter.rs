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
use crate::proxy::responses::{
    ResponsesRequest, ResponsesResponse, ResponsesStreamEvent, ResponseOutputContent,
    ResponseOutputItem, ResponsesUsage,
};
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
    pub role: String, // "user" | "assistant"
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

// ============================================================================
// Canonical OpenAI -> Anthropic backend (provider side)
// ============================================================================

/// Build an Anthropic `tool_choice` from the canonical OpenAI form.
///
/// Anthropic accepts:
///   "auto" / "any" / "none"          (string enum)
///   {"type": "auto" | "any" | "none"}
///   {"type": "tool", "name": "<X>"}
///
/// OpenAI accepts:
///   "auto" / "none" / "required"
///   {"type": "function", "function": {"name": "<X>"}}
fn tool_choice_oai_to_anthropic(tc: &Value) -> Value {
    match tc {
        Value::String(s) => match s.as_str() {
            "required" => json!("any"),
            other => json!(other),
        },
        Value::Object(obj) => match obj.get("type").and_then(|v| v.as_str()) {
            Some("function") => {
                let name = obj
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|n| n.as_str())
                    .unwrap_or("");
                json!({ "type": "tool", "name": name })
            }
            Some(other) => json!({ "type": other }),
            _ => tc.clone(),
        },
        _ => tc.clone(),
    }
}

pub fn oai_req_to_anthropic_req(req: &ChatCompletionRequest) -> AnthropicRequest {
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
                msgs.push(AnthropicMessage {
                    role: "user".into(),
                    content: json!([block]),
                });
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
                            let input: Value =
                                serde_json::from_str(args_str).unwrap_or(json!({}));
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
    // Forward tools / tool_choice verbatim when present.
    if let Some(tools) = &req.tools {
        out.extra.insert("tools".into(), Value::Array(tools.clone()));
    }
    if let Some(tc) = &req.tool_choice {
        out.extra.insert("tool_choice".into(), tool_choice_oai_to_anthropic(tc));
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
                    block.as_object_mut().unwrap().insert("text".into(), json!(t));
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
                content: if text_acc.is_empty() { None } else { Some(text_acc) },
                tool_calls: if tool_calls.is_empty() { None } else { Some(tool_calls) },
                refusal: None,
                extra,
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
                            let id = b
                                .get("tool_use_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
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
                        tool_calls: if tool_calls.is_empty() { None } else { Some(tool_calls) },
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
    let tool_choice = extra.remove("tool_choice").map(|tc| tool_choice_anthropic_to_oai(&tc));

    ChatCompletionRequest {
        model: crate::proxy::strip_one_m_suffix(&body.model),
        messages,
        stream: body.stream.unwrap_or(false),
        temperature: body.temperature,
        max_tokens: Some(body.max_tokens),
        tools,
        tool_choice,
        extra,
    }
}

pub fn oai_resp_to_anthropic_resp(
    resp: &OAIChatResponse,
    requested_model: &str,
) -> AnthropicResponse {
    let mut content: Vec<AnthropicContentBlock> = Vec::new();
    for choice in &resp.choices {
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
                let input: Value =
                    serde_json::from_str(args_str).unwrap_or(json!({}));
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
            output_tokens: resp.usage.as_ref().map(|u| u.completion_tokens).unwrap_or(0),
        },
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
    let prompt = usage.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
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

        while let Some(ch) = chunks.next().await {
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

                // ---- reasoning / thinking delta (DeepSeek/Kimi/Anthropic
                // passthrough via `reasoning` field) ----
                if let Some(thinking) = &d.reasoning {
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

                // ---- tool_calls delta ----
                if let Some(tcs) = &d.tool_calls {
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
        yield sse("message_stop", json!({ "type": "message_stop" }));
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
    }
}

// ============================================================================
// Anthropic SSE events -> canonical OAI stream chunks
// ============================================================================

/// Parse Anthropic SSE event payloads (data strings without the `data:` prefix)
/// into canonical OAI stream chunks. Handles `tool_use` start/stop,
/// `input_json_delta`, text/thinking deltas, and `message_delta` finish reason.
pub fn anthropic_stream_to_oai_chunks(
    data_strings: impl futures_core::Stream<Item = String> + Send + 'static,
    model: String,
) -> std::pin::Pin<Box<dyn futures_core::Stream<Item = OAIStreamChunk> + Send>> {
    let id = format!("chatcmpl-{}", rand_hex(6));
    let created = chrono::Utc::now().timestamp();
    let id_for_closure = id.clone();
    let model_for_closure = model.clone();
    async_stream::stream! {
        let mut data_strings = std::pin::pin!(data_strings);
        let mut started = false;
        let mut tool_index_to_oai_index: HashMap<u32, usize> = HashMap::new();
        let mut next_oai_index: usize = 0;
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
                                delta: StreamDelta {
                                    role: Some("assistant".into()),
                                    content: None,
                                    tool_calls: None,
                                    reasoning: None,
                                },
                                finish_reason: None,
                                extra: Default::default(),
                            }],
                            extra: Default::default(),
                        };
                    }
                }
                "content_block_start" => {
                    let cb = evt.get("content_block").cloned().unwrap_or(Value::Null);
                    let cb_type = cb.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    if cb_type == "tool_use" {
                        let anthro_index = evt.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                        let tool_id = cb.get("id").and_then(|v| v.as_str()).unwrap_or("");
                        let name = cb.get("name").and_then(|v| v.as_str()).unwrap_or("");
                        let oai_index = *tool_index_to_oai_index.entry(anthro_index).or_insert_with(|| {
                            let idx = next_oai_index;
                            next_oai_index += 1;
                            idx
                        });
                        yield OAIStreamChunk {
                            id: id_for_closure.clone(),
                            object: "chat.completion.chunk".into(),
                            created,
                            model: model_for_closure.clone(),
                            choices: vec![StreamChoice {
                                index: 0,
                                delta: StreamDelta {
                                    role: None,
                                    content: None,
                                    tool_calls: Some(vec![json!({
                                        "index": oai_index,
                                        "id": tool_id,
                                        "type": "function",
                                        "function": { "name": name },
                                    })]),
                                    reasoning: None,
                                },
                                finish_reason: None,
                                extra: Default::default(),
                            }],
                            extra: Default::default(),
                        };
                    }
                }
                "content_block_delta" => {
                    let delta = evt.get("delta").cloned().unwrap_or(Value::Null);
                    let delta_type = delta.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    let anthro_index = evt.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                    let oai_index = *tool_index_to_oai_index.entry(anthro_index).or_insert_with(|| {
                        let idx = next_oai_index;
                        next_oai_index += 1;
                        idx
                    });
                    match delta_type {
                        "text_delta" => {
                            if let Some(text) = delta.get("text").and_then(|v| v.as_str()) {
                                yield OAIStreamChunk {
                                    id: id_for_closure.clone(),
                                    object: "chat.completion.chunk".into(),
                                    created,
                                    model: model_for_closure.clone(),
                                    choices: vec![StreamChoice {
                                        index: 0,
                                        delta: StreamDelta {
                                            role: None,
                                            content: Some(text.into()),
                                            tool_calls: None,
                                            reasoning: None,
                                        },
                                        finish_reason: None,
                                        extra: Default::default(),
                                    }],
                                    extra: Default::default(),
                                };
                            }
                        }
                        "thinking_delta" => {
                            if let Some(t) = delta.get("thinking").and_then(|v| v.as_str()) {
                                yield OAIStreamChunk {
                                    id: id_for_closure.clone(),
                                    object: "chat.completion.chunk".into(),
                                    created,
                                    model: model_for_closure.clone(),
                                    choices: vec![StreamChoice {
                                        index: 0,
                                        delta: StreamDelta {
                                            role: None,
                                            content: None,
                                            tool_calls: None,
                                            reasoning: Some(t.into()),
                                        },
                                        finish_reason: None,
                                        extra: Default::default(),
                                    }],
                                    extra: Default::default(),
                                };
                            }
                        }
                        "input_json_delta" => {
                            if let Some(pj) = delta.get("partial_json").and_then(|v| v.as_str()) {
                                yield OAIStreamChunk {
                                    id: id_for_closure.clone(),
                                    object: "chat.completion.chunk".into(),
                                    created,
                                    model: model_for_closure.clone(),
                                    choices: vec![StreamChoice {
                                        index: 0,
                                        delta: StreamDelta {
                                            role: None,
                                            content: None,
                                            tool_calls: Some(vec![json!({
                                                "index": oai_index,
                                                "function": { "arguments": pj },
                                            })]),
                                            reasoning: None,
                                        },
                                        finish_reason: None,
                                        extra: Default::default(),
                                    }],
                                    extra: Default::default(),
                                };
                            }
                        }
                        _ => {}
                    }
                }
                "content_block_stop" => {
                    // OpenAI Chat Completions has no per-tool block stop event;
                    // the finish_reason chunk tells the client we're done.
                    // Don't emit anything here.
                }
                "message_delta" => {
                    let stop = evt
                        .get("delta")
                        .and_then(|d| d.get("stop_reason"))
                        .and_then(|v| v.as_str())
                        .map(|s| stop_from_anthropic(s).to_string());
                    let mut extra = HashMap::new();
                    if let Some(usage) = evt.get("usage") {
                        extra.insert("usage".into(), usage.clone());
                    }
                    yield OAIStreamChunk {
                        id: id_for_closure.clone(),
                        object: "chat.completion.chunk".into(),
                        created,
                        model: model_for_closure.clone(),
                        choices: vec![StreamChoice {
                            index: 0,
                            delta: StreamDelta {
                                role: None,
                                content: None,
                                tool_calls: None,
                                reasoning: None,
                            },
                            finish_reason: stop,
                            extra: Default::default(),
                        }],
                        extra,
                    };
                }
                "message_stop" => { return; }
                _ => {}
            }
        }
    }
    .boxed()
}

// ============================================================================
// OpenAI Responses API <-> canonical OpenAI Chat Completions
// ============================================================================

/// Flatten a `ResponsesRequest.input` into the canonical `messages` array.
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
                let role = item
                    .get("role")
                    .and_then(|v| v.as_str())
                    .unwrap_or("user")
                    .to_string();
                let content = item.get("content").cloned().unwrap_or(Value::Null);
                messages.push(ChatMessage {
                    role,
                    content: input_content_to_chat(&content),
                    tool_calls: None,
                    tool_call_id: None,
                    extra: Default::default(),
                });
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
        tools: match req.tools.clone() {
            Some(Value::Array(arr)) => Some(arr),
            Some(other) => Some(vec![other]),
            None => None,
        },
        tool_choice: None,
        extra: Default::default(),
    }
}

fn input_content_to_chat(content: &Value) -> Value {
    match content {
        Value::String(s) => Value::String(s.clone()),
        Value::Array(parts) => {
            let mut out: Vec<Value> = Vec::new();
            for p in parts {
                if let Some(text) = p.get("text").and_then(|v| v.as_str()) {
                    out.push(json!({ "type": "text", "text": text }));
                }
            }
            Value::Array(out)
        }
        other => other.clone(),
    }
}

pub fn chat_to_responses(
    resp: &OAIChatResponse,
    requested_model: &str,
) -> ResponsesResponse {
    let model = if resp.model.is_empty() {
        requested_model.to_string()
    } else {
        resp.model.clone()
    };

    let mut output: Vec<ResponseOutputItem> = Vec::new();
    for choice in &resp.choices {
        let text = choice
            .message
            .content
            .clone()
            .unwrap_or_default();
        output.push(ResponseOutputItem {
            item_type: "message".into(),
            role: choice.message.role.clone(),
            content: vec![ResponseOutputContent {
                content_type: "output_text".into(),
                text,
            }],
            extra: Default::default(),
        });
    }

    let usage = resp.usage.clone().unwrap_or(Usage {
        prompt_tokens: 0,
        completion_tokens: 0,
        total_tokens: 0,
    });

    ResponsesResponse {
        id: resp.id.clone(),
        object: "response".into(),
        created_at: resp.created,
        model,
        status: "completed".into(),
        output,
        usage: ResponsesUsage {
            input_tokens: usage.prompt_tokens,
            output_tokens: usage.completion_tokens,
            total_tokens: usage.total_tokens,
        },
        instructions: None,
    }
}

pub fn chat_chunks_to_responses_stream(
    stream: Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>>,
    requested_model: &str,
) -> Pin<Box<dyn Stream<Item = String> + Send>> {
    use async_stream::stream;
    let mut stream = stream;
    let id = crate::proxy::converter::rand_hex(16);
    let created = chrono::Utc::now().timestamp();
    let model = requested_model.to_string();
    let mut first = true;
    let mut last_chunk: Option<OAIStreamChunk> = None;
    let mut accumulated_text = String::new();
    let mut role = "assistant".to_string();

    let out = stream! {
        while let Some(chunk) = stream.next().await {
            if first {
                if let Some(choice) = chunk.choices.first() {
                    if let Some(r) = &choice.delta.role {
                        role = r.clone();
                    }
                }
            }
            if first {
                let stub = ResponsesResponse {
                    id: id.clone(),
                    object: "response".into(),
                    created_at: created,
                    model: model.clone(),
                    status: "in_progress".into(),
                    output: vec![],
                    usage: ResponsesUsage {
                        input_tokens: 0,
                        output_tokens: 0,
                        total_tokens: 0,
                    },
                    instructions: None,
                };
                let evt = ResponsesStreamEvent::ResponseCreated {
                    response: serde_json::to_value(&stub).unwrap_or(Value::Null),
                };
                yield crate::proxy::responses::stream_event_to_sse(&evt);
                let evt = ResponsesStreamEvent::ResponseOutputItemAdded {
                    output_index: 0,
                    item: serde_json::json!({
                        "type": "message",
                        "role": role,
                        "content": [],
                        "status": "in_progress",
                    }),
                };
                yield crate::proxy::responses::stream_event_to_sse(&evt);
                first = false;
            }

            for choice in &chunk.choices {
                if let Some(text) = &choice.delta.content {
                    if !text.is_empty() {
                        accumulated_text.push_str(text);
                        let evt = ResponsesStreamEvent::ResponseOutputTextDelta {
                            output_index: 0,
                            content_index: 0,
                            delta: text.clone(),
                        };
                        yield crate::proxy::responses::stream_event_to_sse(&evt);
                    }
                }
            }

            last_chunk = Some(chunk);
        }

        let last = last_chunk.unwrap_or_else(|| OAIStreamChunk {
            id: id.clone(),
            object: "chat.completion.chunk".into(),
            created,
            model: model.clone(),
            choices: vec![],
            extra: Default::default(),
        });

        let done_item = ResponseOutputItem {
            item_type: "message".into(),
            role: role.clone(),
            content: vec![ResponseOutputContent {
                content_type: "output_text".into(),
                text: accumulated_text,
            }],
            extra: Default::default(),
        };
        let evt = ResponsesStreamEvent::ResponseOutputItemDone {
            output_index: 0,
            item: serde_json::to_value(&done_item).unwrap_or(Value::Null),
        };
        yield crate::proxy::responses::stream_event_to_sse(&evt);

        let final_response = ResponsesResponse {
            id: id.clone(),
            object: "response".into(),
            created_at: created,
            model: if last.model.is_empty() { model.clone() } else { last.model.clone() },
            status: "completed".into(),
            output: vec![done_item],
            usage: ResponsesUsage {
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
            },
            instructions: None,
        };
        let evt = ResponsesStreamEvent::ResponseCompleted {
            response: serde_json::to_value(&final_response).unwrap_or(Value::Null),
        };
        yield crate::proxy::responses::stream_event_to_sse(&evt);
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
            }],
            usage: Some(Usage { prompt_tokens: 5, completion_tokens: 5, total_tokens: 10 }),
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
                e.insert("tools".into(), json!([
                    { "name": "get_weather",
                      "description": "Get the weather",
                      "input_schema": { "type": "object",
                                         "properties": { "city": { "type": "string" } } } }
                ]));
                e.insert("tool_choice".into(), json!({"type": "tool", "name": "get_weather"}));
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
        let arr = asst_back.content.as_array().expect("assistant content array");
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
            usage: AnthropicUsage { input_tokens: 10, output_tokens: 5 },
        };
        let oai = anthropic_resp_to_oai_resp(&ant, "claude");
        let choice = &oai.choices[0];
        assert_eq!(choice.message.content.as_deref(), Some("let me search"));
        let tcs = choice.message.tool_calls.as_ref().expect("tool_calls present");
        assert_eq!(tcs.len(), 1);
        assert_eq!(tcs[0]["id"], "toolu_42");
        assert_eq!(tcs[0]["function"]["name"], "search");
        // canonical_json_string keeps the JSON canonical (no spaces), so
        // we compare the parsed value rather than the literal string.
        let args: Value = serde_json::from_str(
            tcs[0]["function"]["arguments"].as_str().unwrap(),
        ).unwrap();
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
        assert_eq!(tool_choice_oai_to_anthropic(&json!("required")), "any");
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
                    },
                    finish_reason: None,
                    extra: empty_extra(),
                }],
                extra: empty_extra(),
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
                    },
                    finish_reason: None,
                    extra: empty_extra(),
                }],
                extra: empty_extra(),
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
                    },
                    finish_reason: Some("tool_calls".into()),
                    extra: empty_extra(),
                }],
                extra: empty_extra(),
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
                } else { None }
            })
            .collect();
        assert!(types.contains(&"message_start"), "events: {types:?}");
        let tool_start_idx = types.iter().position(|t| *t == "content_block_start")
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
            let payload = d.split("data: ").nth(1).unwrap_or("").split("\n\n").next().unwrap_or("");
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
            events.iter()
                .find(|s| s.starts_with("event: message_delta"))
                .unwrap()
                .split("data: ").nth(1).unwrap().split("\n\n").next().unwrap()
        ).unwrap();
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
                    },
                    finish_reason: None,
                    extra: empty_extra(),
                }],
                extra: empty_extra(),
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
                    },
                    finish_reason: Some("tool_calls".into()),
                    extra: empty_extra(),
                }],
                extra: empty_extra(),
            },
        ];
        let s = stream::iter(chunks);
        let boxed: Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>> = Box::pin(s);
        let mut out = oai_chunks_to_anthropic_stream(boxed, "claude".to_string());
        let mut events: Vec<String> = Vec::new();
        while let Some(line) = out.next().await { events.push(line); }
        let types: Vec<&str> = events
            .iter()
            .filter_map(|s| {
                if s.starts_with("event: ") {
                    Some(s.trim_start_matches("event: ").lines().next().unwrap_or(""))
                } else { None }
            })
            .collect();
        // text block start, text delta, text block stop, tool block start.
        let text_start = types.iter().position(|t| *t == "content_block_start")
            .expect("first content_block_start");
        // Make sure the text block opens first (text_delta appears before
        // the second content_block_start).
        let second_start = types.iter().skip(text_start + 1)
            .position(|t| *t == "content_block_start")
            .map(|p| p + text_start + 1)
            .expect("second content_block_start");
        assert!(second_start > text_start);
        // Verify we got at least one text_delta and one input_json_delta.
        assert!(events.iter().any(|s| s.starts_with("event: content_block_delta")
            && s.contains("text_delta")));
        assert!(events.iter().any(|s| s.starts_with("event: content_block_delta")
            && s.contains("input_json_delta")));
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
        while let Some(c) = out.next().await { chunks.push(c); }
        // First chunk should be the role chunk.
        assert_eq!(chunks[0].choices[0].delta.role.as_deref(), Some("assistant"));
        // Find the chunk carrying tool_calls[0].id
        let id_chunk = chunks.iter().find(|c| {
            c.choices.iter().any(|ch| {
                ch.delta.tool_calls.as_ref()
                    .map(|tcs| tcs.iter().any(|tc| tc.get("id").is_some()))
                    .unwrap_or(false)
            })
        }).expect("a chunk with tool_calls[0].id");
        let tcs = id_chunk.choices[0].delta.tool_calls.as_ref().unwrap();
        assert_eq!(tcs[0]["id"], "toolu_1");
        assert_eq!(tcs[0]["type"], "function");
        assert_eq!(tcs[0]["function"]["name"], "lookup");
        // Find the chunk carrying arguments (will be the next chunk).
        let arg_chunks: Vec<&OAIStreamChunk> = chunks.iter().filter(|c| {
            c.choices.iter().any(|ch| {
                ch.delta.tool_calls.as_ref()
                    .map(|tcs| tcs.iter().any(|tc| tc.get("function")
                        .and_then(|f| f.get("arguments")).is_some()))
                    .unwrap_or(false)
            })
        }).collect();
        let mut combined = String::new();
        for c in &arg_chunks {
            for ch in &c.choices {
                if let Some(tcs) = &ch.delta.tool_calls {
                    for tc in tcs {
                        if let Some(args) = tc.get("function")
                            .and_then(|f| f.get("arguments")).and_then(|v| v.as_str()) {
                            combined.push_str(args);
                        }
                    }
                }
            }
        }
        let parsed: Value = serde_json::from_str(&combined).unwrap();
        assert_eq!(parsed["q"], "rust");
        // Final chunk should carry finish_reason "tool_calls".
        let fin = chunks.last().unwrap();
        assert_eq!(fin.choices[0].finish_reason.as_deref(), Some("tool_calls"));
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
            }],
            usage: Some(Usage {
                prompt_tokens: 5,
                completion_tokens: 7,
                total_tokens: 12,
            }),
        };
        let r = chat_to_responses(&chat, "gpt-5");
        assert_eq!(r.object, "response");
        assert_eq!(r.status, "completed");
        assert_eq!(r.id, "chatcmpl-1");
        assert_eq!(r.output.len(), 1);
        assert_eq!(r.output[0].role, "assistant");
        assert_eq!(r.output[0].content[0].content_type, "output_text");
        assert_eq!(r.output[0].content[0].text, "hi there");
        assert_eq!(r.usage.input_tokens, 5);
        assert_eq!(r.usage.output_tokens, 7);
        assert_eq!(r.usage.total_tokens, 12);
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
                    },
                    finish_reason: None,
                    extra: empty_extra(),
                }],
                extra: empty_extra(),
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
                    },
                    finish_reason: Some("stop".into()),
                    extra: empty_extra(),
                }],
                extra: empty_extra(),
            },
        ];
        let s = futures_util::stream::iter(chunks);
        let boxed: Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>> =
            Box::pin(s);
        let mut out = chat_chunks_to_responses_stream(boxed, "gpt-5");
        let mut events: Vec<String> = Vec::new();
        while let Some(line) = out.next().await {
            events.push(line);
        }
        assert_eq!(events.len(), 6, "got: {events:?}");
        assert!(events[0].starts_with("event: response.created\ndata: "));
        assert!(events[1].starts_with("event: response.output_item.added\ndata: "));
        assert!(events[2].contains("event: response.output_text.delta"), "{}", events[2]);
        assert!(events[2].contains("\"delta\":\"hel\""));
        assert!(events[3].contains("\"delta\":\"lo\""));
        assert!(events[4].starts_with("event: response.output_item.done\ndata: "));
        assert!(events[5].starts_with("event: response.completed\ndata: "));
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
}