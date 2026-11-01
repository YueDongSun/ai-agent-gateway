//! Anthropic Messages API shapes and non-streaming conversion between the
//! canonical OpenAI format and Anthropic requests/responses — both the
//! gateway edge (Anthropic client <-> canonical) and the Anthropic
//! provider side (canonical <-> Anthropic backend). Extracted from the
//! former converter.rs monolith; see the package docs in mod.rs.

use crate::proxy::canonical::{
    ChatCompletionRequest, ChatMessage, OAIChatResponse, OAIChoice, OAIMessage, ProviderError,
    Usage,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;

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

pub(crate) fn stop_to_anthropic(stop: &str) -> &'static str {
    match stop {
        "stop" => "end_turn",
        "length" => "max_tokens",
        "tool_calls" | "function_call" => "tool_use",
        "content_filter" => "end_turn",
        _ => "end_turn",
    }
}

pub(crate) fn stop_from_anthropic(reason: &str) -> &'static str {
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
pub(crate) fn strip_leading_anthropic_billing_header(s: &str) -> &str {
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

pub(crate) fn text_of(content: &Value) -> String {
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
pub(crate) fn tool_choice_oai_to_anthropic(tc: &Value) -> Value {
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
pub(crate) fn oai_tool_to_anthropic_tool(tool: &Value) -> Value {
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

/// Translate the Anthropic tool shape into the canonical OpenAI function
/// wrapper at the ingress boundary — the exact inverse of
/// `oai_tool_to_anthropic_tool`. Native/unknown definitions (custom tools,
/// already-canonical function wrappers) remain intact so the provider can
/// reject them; never silently remove a requested tool.
pub(crate) fn anthropic_tool_to_oai_tool(tool: &Value) -> Value {
    if tool.get("type").and_then(Value::as_str) == Some("function") {
        return tool.clone();
    }
    let Some(name) = tool.get("name").and_then(Value::as_str) else {
        return tool.clone();
    };
    let Some(input_schema) = tool.get("input_schema") else {
        return tool.clone();
    };
    let mut function = json!({
        "name": name,
        "parameters": input_schema.clone(),
    });
    if let Some(description) = tool.get("description") {
        function["description"] = description.clone();
    }
    json!({ "type": "function", "function": function })
}

fn anthropic_conversion_error(code: &str) -> ProviderError {
    crate::proxy::canonical::make_provider_error(
        code,
        "request_conversion",
        Some(400),
        Some(false),
        None,
    )
}

/// Map an OpenAI `image_url` payload to an Anthropic image source. Supports
/// `data:<media>;base64,<data>` payloads and `http(s)` URL sources; anything
/// else is unrepresentable and must be rejected by the caller, not dropped.
fn anthropic_image_source(url: &str) -> Option<Value> {
    if let Some(payload) = url.strip_prefix("data:") {
        let (meta, data) = payload.split_once(',')?;
        let (media_type, encoding) = meta.split_once(';').unwrap_or((meta, ""));
        if !encoding.split(';').any(|part| part == "base64")
            || !media_type.starts_with("image/")
            || data.is_empty()
        {
            return None;
        }
        return Some(json!({
            "type": "base64",
            "media_type": media_type,
            "data": data,
        }));
    }
    if url.starts_with("http://") || url.starts_with("https://") {
        return Some(json!({ "type": "url", "url": url }));
    }
    None
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

pub fn oai_req_to_anthropic_req(
    req: &ChatCompletionRequest,
) -> Result<AnthropicRequest, ProviderError> {
    if let Some(source) = &req.source_anthropic_request {
        // Borrowed parse: strings are copied once into the typed struct
        // instead of deep-cloning the whole source DOM per hop.
        if let Ok(mut body) = AnthropicRequest::deserialize(source) {
            body.model = crate::proxy::strip_one_m_suffix(&req.model);
            body.stream = Some(req.stream);
            return Ok(body);
        }
    }

    // Foreign clients carry these canonical controls in the extension map.
    // Project their representable values explicitly; a malformed control must
    // fail before send rather than silently change the requested sampling.
    let top_p = match req.extra.get("top_p") {
        None | Some(Value::Null) => None,
        Some(value) => {
            if value
                .as_f64()
                .is_none_or(|number| !number.is_finite() || !(0.0..=1.0).contains(&number))
            {
                return Err(anthropic_conversion_error("anthropic_invalid_request"));
            }
            Some(value.clone())
        }
    };
    let stop_sequences = match req.extra.get("stop") {
        None | Some(Value::Null) => None,
        Some(Value::String(stop)) if !stop.is_empty() => Some(vec![stop.clone()]),
        Some(Value::Array(stops)) => Some(
            stops
                .iter()
                .map(|stop| {
                    stop.as_str()
                        .filter(|stop| !stop.is_empty())
                        .map(str::to_string)
                })
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| anthropic_conversion_error("anthropic_invalid_request"))?,
        ),
        Some(_) => return Err(anthropic_conversion_error("anthropic_invalid_request")),
    };
    let reasoning_effort = match req.extra.get("reasoning_effort") {
        None | Some(Value::Null) => None,
        Some(Value::String(effort))
            if matches!(effort.as_str(), "low" | "medium" | "high" | "xhigh" | "max") =>
        {
            Some(effort)
        }
        Some(_) => return Err(anthropic_conversion_error("anthropic_invalid_request")),
    };

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
                                } else if t == "image_url" {
                                    // OpenAI image parts map to Anthropic
                                    // {type:image, source:{...}} — data URLs and
                                    // http(s) URLs both convert. Anything else
                                    // is rejected before send, never dropped.
                                    let image = p.get("image_url").unwrap_or(&Value::Null);
                                    let url = image
                                        .as_str()
                                        .or_else(|| image.get("url").and_then(|v| v.as_str()));
                                    match url.and_then(anthropic_image_source) {
                                        Some(source) => {
                                            blocks
                                                .push(json!({ "type": "image", "source": source }));
                                            has_any = true;
                                        }
                                        None => {
                                            return Err(anthropic_conversion_error(
                                                "anthropic_invalid_request",
                                            ))
                                        }
                                    }
                                } else {
                                    // Audio, file and unknown part shapes have
                                    // no Anthropic representation — reject
                                    // before send instead of silently dropping
                                    // a required modality.
                                    return Err(anthropic_conversion_error(
                                        "anthropic_feature_unsupported",
                                    ));
                                }
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
        stop_sequences,
        extra: Default::default(),
    };
    if let Some(top_p) = top_p {
        out.extra.insert("top_p".into(), top_p);
    }
    if let Some(effort) = reasoning_effort {
        out.extra
            .insert("output_config".into(), json!({"effort":effort}));
    }
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
    Ok(out)
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
        source_gemini_response: None,
        source_responses_response: None,
        extra: Default::default(),
    }
}

// ============================================================================
// Anthropic client -> Canonical OpenAI (gateway edge)
// ============================================================================

/// Map Anthropic `tool_choice` to OpenAI form.
pub(crate) fn tool_choice_anthropic_to_oai(tc: &Value) -> Value {
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
    // it to OpenAI's Option<Vec<Value>>, normalizing each Anthropic tool
    // definition into the canonical function wrapper so every downstream wire
    // (Chat, Responses, Gemini) sees real parameters instead of a bare
    // `input_schema` shape.
    let tools = match extra.remove("tools") {
        Some(Value::Array(arr)) => Some(arr.iter().map(anthropic_tool_to_oai_tool).collect()),
        Some(other) => Some(vec![anthropic_tool_to_oai_tool(&other)]),
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
        source_gemini_request: None,
        source_responses_request: None,
        request_id: None,
        client_credential: None,
    }
}

pub fn oai_resp_to_anthropic_resp(
    resp: &OAIChatResponse,
    requested_model: &str,
) -> AnthropicResponse {
    if let Some(source) = &resp.source_anthropic_response {
        // Borrowed parse: same semantics as from_value on a clone, without
        // duplicating the retained relay DOM.
        if let Ok(mut body) = AnthropicResponse::deserialize(source) {
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

#[cfg(test)]
mod generation_control_tests {
    use super::*;

    #[test]
    fn canonical_reasoning_effort_maps_supported_levels_to_anthropic_output_config() {
        for effort in ["low", "medium", "high", "xhigh", "max"] {
            let canonical = ChatCompletionRequest {
                model: "MiniMax-M3.1-Flash".into(),
                max_tokens: Some(4096),
                messages: vec![serde_json::from_value(
                    json!({"role":"user","content":"Fixture request."}),
                )
                .unwrap()],
                extra: [("reasoning_effort".into(), json!(effort))]
                    .into_iter()
                    .collect(),
                ..Default::default()
            };
            let wire = serde_json::to_value(oai_req_to_anthropic_req(&canonical).unwrap()).unwrap();
            assert_eq!(
                wire["output_config"],
                json!({"effort":effort}),
                "effort {effort}"
            );
            assert_eq!(wire["max_tokens"], 4096);
            assert!(wire.get("reasoning_effort").is_none());
            assert!(
                wire.get("thinking").is_none(),
                "effort projection must not disable or reconstruct thinking"
            );
        }
    }

    #[test]
    fn malformed_canonical_reasoning_effort_fails_before_anthropic_send() {
        for effort in [
            json!("none"),
            json!("minimal"),
            json!("LOW"),
            json!(""),
            json!(" low "),
            json!(1),
            json!(false),
            json!(["low"]),
            json!({"effort":"low"}),
        ] {
            let canonical = ChatCompletionRequest {
                extra: [("reasoning_effort".into(), effort)].into_iter().collect(),
                ..Default::default()
            };
            let error = oai_req_to_anthropic_req(&canonical).unwrap_err();
            assert_eq!(error.code, "anthropic_invalid_request");
            assert_eq!(error.stage, "request_conversion");
            assert_eq!(error.status, Some(400));
            assert!(!error.retriable);
        }
    }

    #[test]
    fn absent_or_null_canonical_reasoning_effort_leaves_vendor_defaults_unchanged() {
        for extra in [
            HashMap::new(),
            [("reasoning_effort".into(), Value::Null)]
                .into_iter()
                .collect(),
        ] {
            let canonical = ChatCompletionRequest {
                extra,
                ..Default::default()
            };
            let wire = serde_json::to_value(oai_req_to_anthropic_req(&canonical).unwrap()).unwrap();
            assert!(wire.get("output_config").is_none());
            assert!(wire.get("thinking").is_none());
        }
    }

    #[test]
    fn native_anthropic_envelope_retains_effort_thinking_and_signatures() {
        let native: AnthropicRequest = serde_json::from_value(json!({
            "model":"MiniMax-M3.1-Flash", "max_tokens":4096, "stream":false,
            "system":"Fixture native instructions.",
            "messages":[
                {"role":"assistant","content":[
                    {"type":"thinking","thinking":"Fixture reasoning summary.","signature":"opaque_fixture_signature"},
                    {"type":"text","text":"Fixture visible text."}
                ]},
                {"role":"user","content":"Continue the fixture."}
            ],
            "thinking":{"type":"adaptive"},
            "output_config":{"effort":"max","format":{"type":"json_schema","schema":{"type":"object"}}}
        })).unwrap();
        let original = serde_json::to_value(&native).unwrap();
        for effort in [json!("low"), json!({"malformed_foreign_control":true})] {
            let mut canonical = anthropic_req_to_oai_req(&native);
            canonical.extra.insert("reasoning_effort".into(), effort);
            let wire = serde_json::to_value(oai_req_to_anthropic_req(&canonical).unwrap()).unwrap();
            assert_eq!(
                wire, original,
                "native source envelope must take precedence over foreign control projection"
            );
        }
    }

    #[test]
    fn gemini_generation_controls_and_function_schema_reach_anthropic_wire() {
        let gemini = json!({
            "contents": [{"role":"user", "parts":[{"text":"Look up the requested item."}]}],
            "generationConfig": {
                "temperature":0.4, "maxOutputTokens":128,
                "topP":0.8, "stopSequences":["END", "DONE"]
            },
            "tools":[{"functionDeclarations":[{
                "name":"lookup", "description":"Find an item",
                "parameters":{"type":"object", "properties":{"query":{"type":"string"}}, "required":["query"]}
            }]}],
            "toolConfig":{"functionCallingConfig":{"mode":"ANY", "allowedFunctionNames":["lookup"]}}
        });
        let mut canonical = crate::proxy::gemini::try_gemini_req_to_oai_req(&gemini).unwrap();
        canonical.model = "MiniMax-M3.1-Flash".into();
        let wire = serde_json::to_value(oai_req_to_anthropic_req(&canonical).unwrap()).unwrap();

        assert_eq!(wire["top_p"], 0.8);
        assert_eq!(wire["stop_sequences"], json!(["END", "DONE"]));
        assert!(wire.get("stop").is_none());
        assert!(wire.get("generationConfig").is_none());
        assert_eq!(wire["temperature"], 0.4);
        assert_eq!(wire["max_tokens"], 128);
        assert_eq!(wire["tools"][0]["name"], "lookup");
        assert_eq!(
            wire["tools"][0]["input_schema"]["required"],
            json!(["query"])
        );
        assert_eq!(wire["tool_choice"], json!({"type":"tool", "name":"lookup"}));
    }

    #[test]
    fn canonical_stop_string_and_array_are_preserved_without_coercion() {
        for (stop, expected) in [
            (json!("END"), json!(["END"])),
            (json!(["END", "DONE"]), json!(["END", "DONE"])),
            (json!([]), json!([])),
        ] {
            let canonical = ChatCompletionRequest {
                extra: [("stop".into(), stop)].into_iter().collect(),
                ..Default::default()
            };
            let wire = serde_json::to_value(oai_req_to_anthropic_req(&canonical).unwrap()).unwrap();
            assert_eq!(wire["stop_sequences"], expected);
        }
        let canonical = ChatCompletionRequest {
            extra: [("stop".into(), Value::Null), ("top_p".into(), Value::Null)]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        let wire = serde_json::to_value(oai_req_to_anthropic_req(&canonical).unwrap()).unwrap();
        assert!(wire.get("stop_sequences").is_none());
        assert!(wire.get("top_p").is_none());
    }

    #[test]
    fn invalid_canonical_generation_controls_fail_before_anthropic_send() {
        for (key, value) in [
            ("top_p", json!(-0.1)),
            ("top_p", json!(1.1)),
            ("top_p", json!("0.5")),
            ("top_p", json!(true)),
            ("stop", json!(1)),
            ("stop", json!({"text":"END"})),
            ("stop", json!(["END", false])),
            ("stop", json!("")),
            ("stop", json!(["END", ""])),
        ] {
            let canonical = ChatCompletionRequest {
                extra: [(key.into(), value)].into_iter().collect(),
                ..Default::default()
            };
            let error = oai_req_to_anthropic_req(&canonical).unwrap_err();
            assert_eq!(error.code, "anthropic_invalid_request");
            assert_eq!(error.stage, "request_conversion");
            assert_eq!(error.status, Some(400));
            assert!(!error.retriable);
        }
    }
}
