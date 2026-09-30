//! Google Gemini (Generative Language API) wire shapes and conversion.
//!
//! Gemini CLI speaks the native Gemini protocol
//! (`POST {base}/v1beta/models/{model}:generateContent` /
//! `:streamGenerateContent?alt=sse`). To make AAG a unified cc-switch-style
//! upstream for it, this module provides the third wire family alongside
//! OpenAI chat completions and Anthropic messages:
//!
//! 1. Entry surface: a Gemini-native client request is parsed into a
//!    canonical `ChatCompletionRequest` (`gemini_req_to_oai_req`), routed
//!    through the failover chain, and the canonical response is converted
//!    back to a `GenerateContentResponse` (`oai_resp_to_gemini_resp`).
//! 2. Upstream provider: a canonical request is converted to a Gemini
//!    `GenerateContentRequest` (`oai_req_to_gemini_req`) and sent to a
//!    Google-compatible upstream; the response / SSE stream is converted
//!    back to canonical (`gemini_resp_to_oai_resp`,
//!    `gemini_stream_to_oai_chunks`).
//!
//! Unlike the Anthropic edge there is no lossless same-protocol relay: the
//! canonical projection is always used (Gemini CLI tolerates the round trip —
//! it has no thinking signatures or opaque blocks that must survive).

use crate::proxy::canonical::{
    ChatCompletionRequest, ChatMessage, OAIChatResponse, OAIChoice, OAIMessage, OAIStreamChunk,
    StreamChoice, StreamDelta, Usage,
};
use futures_core::Stream;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::pin::Pin;

// ----- wire types -----

/// Gemini `GenerateContentRequest` (the subset AAG interprets; unknown keys
/// ride along in `extra` so custom proxies keep working).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeminiRequest {
    #[serde(default)]
    pub contents: Vec<GeminiContent>,
    /// `system_instruction` is camelCase `systemInstruction` on the wire.
    #[serde(
        rename = "systemInstruction",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub system_instruction: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Value>,
    #[serde(
        rename = "toolConfig",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub tool_config: Option<Value>,
    #[serde(
        rename = "generationConfig",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub generation_config: Option<Value>,
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeminiContent {
    /// `"user"` or `"model"` (Gemini has no system role here).
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub parts: Value,
}

/// Normalize a Gemini model id to its bare form: strip a leading `/` and the
/// `models/` resource-name prefix so it can be interpolated into
/// `/v1beta/models/{model}:generateContent` without doubling.
/// Mirrors cc-switch `gemini_url.rs::normalize_gemini_model_id`.
pub fn normalize_gemini_model_id(model: &str) -> &str {
    let trimmed = model.strip_prefix('/').unwrap_or(model);
    trimmed.strip_prefix("models/").unwrap_or(trimmed)
}

// ----- finish-reason mapping -----

fn finish_from_gemini(reason: Option<&str>, has_function_call: bool) -> Option<String> {
    if has_function_call {
        return Some("tool_calls".into());
    }
    reason.map(|reason| {
        match reason {
            "STOP" => "stop",
            "MAX_TOKENS" => "length",
            "SAFETY" | "PROHIBITED_CONTENT" | "RECITATION" | "BLOCKLIST" => "content_filter",
            _ => "other",
        }
        .to_string()
    })
}

fn finish_to_gemini(reason: Option<&str>) -> &'static str {
    match reason {
        Some("length") => "MAX_TOKENS",
        Some("content_filter") => "SAFETY",
        // Gemini reports STOP both for natural ends and completed function
        // calls — there is no separate tool-call stop reason.
        _ => "STOP",
    }
}

// ----- entry surface: Gemini request -> canonical -----

/// Convert a Gemini `GenerateContentRequest` (parsed as raw JSON for
/// tolerance) into the canonical chat-completions request.
///
/// - `systemInstruction` → a leading `system` message.
/// - `contents[].role`: `user` stays `user`, `model` becomes `assistant`.
/// - Parts: `text` → text; `inlineData` → image_url content part;
///   `functionCall` (model role) → assistant `tool_calls`;
///   `functionResponse` (user role) → `role: "tool"` message.
/// - `thought: true` parts become `reasoning_content` on the assistant side.
/// - `tools[].functionDeclarations[]` → nested chat tools;
///   `toolConfig.functionCallingConfig` → chat `tool_choice`.
/// - `generationConfig` maps `temperature` / `maxOutputTokens` / `topP` /
///   `stopSequences`.
pub fn gemini_req_to_oai_req(body: &Value) -> ChatCompletionRequest {
    let mut messages: Vec<ChatMessage> = Vec::new();

    if let Some(sys) = body.pointer("/systemInstruction") {
        let text = gemini_parts_text(sys.get("parts"));
        if !text.is_empty() {
            messages.push(ChatMessage {
                role: "system".into(),
                content: Value::String(text),
                tool_calls: None,
                tool_call_id: None,
                extra: Default::default(),
            });
        }
    }

    for content in body
        .get("contents")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        gemini_content_to_chat(content, &mut messages);
    }

    let generation = body.get("generationConfig");
    let temperature = generation
        .and_then(|g| g.get("temperature"))
        .and_then(Value::as_f64);
    let max_tokens = generation
        .and_then(|g| g.get("maxOutputTokens"))
        .and_then(Value::as_u64)
        .map(|v| v as u32);

    let mut extra: HashMap<String, Value> = HashMap::new();
    if let Some(top_p) = generation
        .and_then(|g| g.get("topP"))
        .and_then(Value::as_f64)
    {
        extra.insert("top_p".into(), json!(top_p));
    }
    if let Some(stop) = generation
        .and_then(|g| g.get("stopSequences"))
        .and_then(Value::as_array)
        .filter(|seqs| !seqs.is_empty())
    {
        extra.insert("stop".into(), Value::Array(stop.clone()));
    }

    ChatCompletionRequest {
        model: String::new(), // filled by the caller from the URL path
        messages,
        stream: false, // filled by the caller from the method / alt param
        temperature,
        max_tokens,
        tools: gemini_tools_to_chat(body.get("tools")),
        tool_choice: gemini_tool_choice_to_chat(body.get("toolConfig")),
        extra,
        source_anthropic_request: None,
        request_id: None,
        client_credential: None,
    }
}

/// Append one Gemini content entry to the canonical message list. Consecutive
/// `functionCall` parts join a single assistant turn (chat completions expect
/// one `tool_calls` array), and `functionResponse` entries become `tool`
/// messages.
fn gemini_content_to_chat(content: &Value, messages: &mut Vec<ChatMessage>) {
    let role = match content.get("role").and_then(Value::as_str) {
        Some("model") => "assistant",
        _ => "user",
    };
    let parts = match content.get("parts").and_then(Value::as_array) {
        Some(parts) => parts,
        None => return,
    };

    let mut text_parts: Vec<Value> = Vec::new();
    let mut tool_calls: Vec<Value> = Vec::new();
    let mut reasoning = String::new();

    for part in parts {
        if let Some(text) = part.get("text").and_then(Value::as_str) {
            if part
                .get("thought")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                reasoning.push_str(text);
            } else {
                text_parts.push(json!({ "type": "text", "text": text }));
            }
            continue;
        }
        if let Some(call) = part.get("functionCall") {
            let name = call.get("name").and_then(Value::as_str).unwrap_or("");
            let arguments = match call.get("args") {
                Some(args) => crate::proxy::converter::canonical_json_string(args),
                None => "{}".to_string(),
            };
            // Gemini requires the thoughtSignature on every functionCall
            // replay in multi-turn tool exchanges — carry it in-band on the
            // canonical tool_call object (never serialized to OpenAI upstreams
            // because openai/anthropic converters read only the known keys).
            let signature = part
                .get("thoughtSignature")
                .and_then(Value::as_str)
                .unwrap_or("");
            tool_calls.push(json!({
                "id": format!("call_{}", crate::proxy::converter::rand_hex(12)),
                "type": "function",
                "function": { "name": name, "arguments": arguments },
                "thoughtSignature": signature,
            }));
            continue;
        }
        if let Some(response) = part.get("functionResponse") {
            let name = response.get("name").and_then(Value::as_str).unwrap_or("");
            let payload = match response.get("response") {
                Some(value) => crate::proxy::converter::canonical_json_string(value),
                None => "{}".to_string(),
            };
            messages.push(ChatMessage {
                role: "tool".into(),
                content: Value::String(payload),
                tool_calls: None,
                tool_call_id: Some(name.to_string()),
                extra: Default::default(),
            });
            continue;
        }
        if let Some(inline) = part.get("inlineData") {
            let mime = inline.get("mimeType").and_then(Value::as_str).unwrap_or("");
            let data = inline.get("data").and_then(Value::as_str).unwrap_or("");
            if !data.is_empty() {
                text_parts.push(json!({
                    "type": "image_url",
                    "image_url": { "url": format!("data:{mime};base64,{data}") },
                }));
            }
            continue;
        }
    }

    if role == "assistant" {
        let mut extra = HashMap::new();
        if !reasoning.is_empty() {
            extra.insert("reasoning_content".into(), Value::String(reasoning));
        }
        let content_value = if text_parts.is_empty() {
            Value::Null
        } else {
            text_parts_to_content(text_parts)
        };
        // Merge into a preceding assistant tool-call turn when this entry has
        // no text of its own (Gemini emits one part per function call).
        if tool_calls.is_empty() && content_value.is_null() && extra.is_empty() {
            return;
        }
        if content_value.is_null() {
            if let Some(previous) = messages.last_mut().filter(|m| m.role == "assistant") {
                if let Some(calls) = previous.tool_calls.as_mut() {
                    calls.extend(tool_calls);
                    return;
                }
            }
        }
        messages.push(ChatMessage {
            role: "assistant".into(),
            content: content_value,
            tool_calls: if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls)
            },
            tool_call_id: None,
            extra,
        });
        return;
    }

    if !text_parts.is_empty() {
        messages.push(ChatMessage {
            role: "user".into(),
            content: text_parts_to_content(text_parts),
            tool_calls: None,
            tool_call_id: None,
            extra: Default::default(),
        });
    }
}

fn text_parts_to_content(parts: Vec<Value>) -> Value {
    if parts.len() == 1 && parts[0].get("text").is_some() {
        return parts[0].get("text").cloned().unwrap_or(Value::Null);
    }
    Value::Array(parts)
}

/// Concatenate the plain text of a Gemini parts array (for system
/// instructions).
fn gemini_parts_text(parts: Option<&Value>) -> String {
    let mut out = String::new();
    if let Some(parts) = parts.and_then(Value::as_array) {
        for part in parts {
            if let Some(text) = part.get("text").and_then(Value::as_str) {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(text);
            }
        }
    }
    out
}

/// `tools: [{ functionDeclarations: [...] }]` → nested chat tools.
fn gemini_tools_to_chat(tools: Option<&Value>) -> Option<Vec<Value>> {
    let mut out = Vec::new();
    for tool in tools?.as_array()? {
        if let Some(declarations) = tool.get("functionDeclarations").and_then(Value::as_array) {
            for declaration in declarations {
                let name = declaration
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if name.is_empty() {
                    continue;
                }
                out.push(json!({
                    "type": "function",
                    "function": {
                        "name": name,
                        "description": declaration
                            .get("description")
                            .and_then(Value::as_str)
                            .unwrap_or(""),
                        "parameters": sanitize_gemini_schema(
                            declaration.get("parameters")
                                .cloned()
                                .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
                        ),
                    },
                }));
            }
        }
    }
    (!out.is_empty()).then_some(out)
}

/// `toolConfig.functionCallingConfig` → chat `tool_choice`:
/// AUTO → "auto", NONE → "none", ANY → "required" (an `allowedFunctionNames`
/// singleton pins one function).
fn gemini_tool_choice_to_chat(tool_config: Option<&Value>) -> Option<Value> {
    let config = tool_config?.pointer("/functionCallingConfig")?;
    match config.get("mode").and_then(Value::as_str) {
        Some("NONE") => Some(json!("none")),
        Some("ANY") => {
            let allowed = config
                .get("allowedFunctionNames")
                .and_then(Value::as_array)
                .and_then(|names| names.first())
                .and_then(Value::as_str);
            match allowed {
                Some(name) => Some(json!({"type": "function", "function": {"name": name}})),
                None => Some(json!("required")),
            }
        }
        // AUTO (and Gemini 3's dynamic modes) map to the chat default.
        _ => Some(json!("auto")),
    }
}

// ----- upstream provider: canonical request -> Gemini -----

/// Convert a canonical chat-completions request into a Gemini
/// `GenerateContentRequest`.
pub fn oai_req_to_gemini_req(req: &ChatCompletionRequest) -> GeminiRequest {
    let mut system_parts: Vec<Value> = Vec::new();
    let mut contents: Vec<GeminiContent> = Vec::new();
    // tool_call id -> function name, so tool results can carry the Gemini
    // functionResponse name Gemini requires.
    let mut call_names: HashMap<String, String> = HashMap::new();

    for message in &req.messages {
        match message.role.as_str() {
            "system" | "developer" => {
                if let Some(text) = message_text(message) {
                    if !text.is_empty() {
                        system_parts.push(json!({ "text": text }));
                    }
                }
            }
            "assistant" => {
                let mut parts: Vec<Value> = Vec::new();
                if let Some(reasoning) = message
                    .extra
                    .get("reasoning_content")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    parts.push(json!({ "thought": true, "text": reasoning }));
                }
                if let Some(text) = message_text(message) {
                    if !text.is_empty() {
                        parts.push(json!({ "text": text }));
                    }
                }
                if let Some(calls) = &message.tool_calls {
                    for call in calls {
                        let name = call
                            .pointer("/function/name")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        let id = call.get("id").and_then(Value::as_str).unwrap_or("");
                        if !id.is_empty() && !name.is_empty() {
                            call_names.insert(id.to_string(), name.to_string());
                        }
                        let args = call
                            .pointer("/function/arguments")
                            .and_then(Value::as_str)
                            .and_then(|s| serde_json::from_str::<Value>(s).ok())
                            .unwrap_or_else(|| json!({}));
                        let mut function_call =
                            json!({ "functionCall": { "name": name, "args": args } });
                        // Replay the thoughtSignature Gemini attached to this
                        // call when it was originally produced (multi-turn
                        // tool exchanges require it verbatim).
                        if let Some(signature) =
                            call.get("thoughtSignature").and_then(Value::as_str)
                        {
                            if !signature.is_empty() {
                                function_call["thoughtSignature"] = json!(signature);
                            }
                        }
                        parts.push(function_call);
                    }
                }
                if !parts.is_empty() {
                    contents.push(GeminiContent {
                        role: "model".into(),
                        parts: Value::Array(parts),
                    });
                }
            }
            "tool" => {
                let id = message.tool_call_id.as_deref().unwrap_or("");
                let name = call_names
                    .get(id)
                    .cloned()
                    .unwrap_or_else(|| id.to_string());
                let response = message
                    .content
                    .as_str()
                    .and_then(|s| serde_json::from_str::<Value>(s).ok())
                    .filter(|v| v.is_object())
                    .unwrap_or_else(|| json!({ "result": message.content }));
                contents.push(GeminiContent {
                    role: "user".into(),
                    parts: json!([{ "functionResponse": { "name": name, "response": response } }]),
                });
            }
            _ => {
                // user (and anything unknown degrades to user)
                if let Some(content) = user_content_to_parts(message) {
                    if !content.is_empty() {
                        contents.push(GeminiContent {
                            role: "user".into(),
                            parts: Value::Array(content),
                        });
                    }
                }
            }
        }
    }

    let mut generation: Value = json!({});
    if let Some(temperature) = req.temperature {
        generation["temperature"] = json!(temperature);
    }
    if let Some(max_tokens) = req.max_tokens {
        generation["maxOutputTokens"] = json!(max_tokens);
    }
    if let Some(top_p) = req.extra.get("top_p").and_then(Value::as_f64) {
        generation["topP"] = json!(top_p);
    }
    if let Some(stop) = req
        .extra
        .get("stop")
        .and_then(Value::as_array)
        .filter(|seqs| !seqs.is_empty())
    {
        generation["stopSequences"] = Value::Array(stop.clone());
    }

    GeminiRequest {
        contents,
        system_instruction: (!system_parts.is_empty()).then(|| json!({ "parts": system_parts })),
        tools: chat_tools_to_gemini(req.tools.as_deref()),
        tool_config: chat_tool_choice_to_gemini(req.tool_choice.as_ref()),
        generation_config: Some(generation),
        extra: Default::default(),
    }
}

fn message_text(message: &ChatMessage) -> Option<String> {
    match &message.content {
        Value::String(s) => Some(s.clone()),
        Value::Array(parts) => {
            let mut out = String::new();
            for part in parts {
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str(text);
                }
            }
            Some(out)
        }
        Value::Null => None,
        other => Some(other.to_string()),
    }
}

fn user_content_to_parts(message: &ChatMessage) -> Option<Vec<Value>> {
    match &message.content {
        Value::String(s) => Some(vec![json!({ "text": s })]),
        Value::Array(parts) => {
            let mut out = Vec::new();
            for part in parts {
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    out.push(json!({ "text": text }));
                    continue;
                }
                if part.get("type").and_then(Value::as_str) == Some("image_url") {
                    if let Some(url) = part.pointer("/image_url/url").and_then(Value::as_str) {
                        if let Some((mime, data)) = url.split_once(",") {
                            let mime = mime
                                .strip_prefix("data:")
                                .unwrap_or(mime)
                                .trim_end_matches(";base64");
                            out.push(json!({
                                "inlineData": { "mimeType": mime, "data": data }
                            }));
                        }
                    }
                }
            }
            Some(out)
        }
        Value::Null => Some(Vec::new()),
        other => Some(vec![json!({ "text": other.to_string() })]),
    }
}

fn chat_tools_to_gemini(tools: Option<&[Value]>) -> Option<Value> {
    let mut declarations = Vec::new();
    for tool in tools? {
        // Accept both nested chat tools and flat Gemini declarations.
        let declaration = match tool.pointer("/function") {
            Some(function) => function.clone(),
            None if tool.get("name").is_some() => tool.clone(),
            _ => continue,
        };
        let name = declaration
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("");
        if name.is_empty() {
            continue;
        }
        declarations.push(json!({
            "name": name,
            "description": declaration
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or(""),
            "parameters": sanitize_gemini_schema(
                declaration.get("parameters").cloned()
                    .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
            ),
        }));
    }
    (!declarations.is_empty()).then(|| json!([{ "functionDeclarations": declarations }]))
}

/// Chat `tool_choice` → Gemini `functionCallingConfig`.
fn chat_tool_choice_to_gemini(tool_choice: Option<&Value>) -> Option<Value> {
    match tool_choice {
        Some(Value::String(mode)) => match mode.as_str() {
            "none" => Some(json!({
                "functionCallingConfig": { "mode": "NONE" }
            })),
            "required" | "any" => Some(json!({
                "functionCallingConfig": { "mode": "ANY" }
            })),
            _ => None, // "auto"
        },
        Some(Value::Object(selector)) => {
            let name = selector
                .get("function")
                .and_then(|function| function.get("name"))
                .and_then(Value::as_str)
                .or_else(|| selector.get("name").and_then(Value::as_str));
            match name {
                Some(name) => Some(json!({
                    "functionCallingConfig": {
                        "mode": "ANY",
                        "allowedFunctionNames": [name],
                    }
                })),
                None => Some(json!({
                    "functionCallingConfig": { "mode": "ANY" }
                })),
            }
        }
        _ => None,
    }
}

/// Recursively sanitize a JSON Schema for Gemini's `parameters` field.
///
/// The Generative Language API accepts an OpenAPI subset and rejects several
/// common JSON-Schema keywords (`$schema`, `additionalProperties`,
/// `exclusiveMinimum`/`exclusiveMaximum` in boolean form, `default`).
/// Unknown keys are dropped rather than forwarded — a 400 from a strict
/// upstream would otherwise kill the whole request. Mirrors the intent of
/// cc-switch `providers/gemini_schema.rs`.
pub fn sanitize_gemini_schema(schema: Value) -> Value {
    const ALLOWED: &[&str] = &[
        "type",
        "description",
        "properties",
        "required",
        "items",
        "enum",
        "format",
        "anyOf",
        "nullable",
        "minimum",
        "maximum",
        "title",
    ];
    match schema {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (key, value) in map {
                if !ALLOWED.contains(&key.as_str()) {
                    continue;
                }
                let sanitized = match key.as_str() {
                    "properties" => match value {
                        Value::Object(properties) => Value::Object(
                            properties
                                .into_iter()
                                .map(|(name, prop)| (name, sanitize_gemini_schema(prop)))
                                .collect(),
                        ),
                        other => other,
                    },
                    "items" => sanitize_gemini_schema(value),
                    "anyOf" => match value {
                        Value::Array(variants) => {
                            Value::Array(variants.into_iter().map(sanitize_gemini_schema).collect())
                        }
                        other => other,
                    },
                    _ => value,
                };
                out.insert(key, sanitized);
            }
            Value::Object(out)
        }
        other => other,
    }
}

// ----- upstream provider: Gemini response -> canonical -----

/// Convert a Gemini `GenerateContentResponse` (raw JSON) into the canonical
/// chat-completions response. `requested_model` is echoed when the upstream
/// omits `modelVersion`.
pub fn gemini_resp_to_oai_resp(body: &Value, requested_model: &str) -> OAIChatResponse {
    let candidate = body
        .get("candidates")
        .and_then(Value::as_array)
        .and_then(|candidates| candidates.first());

    let mut content = String::new();
    let mut reasoning = String::new();
    let mut tool_calls: Vec<Value> = Vec::new();
    let mut has_function_call = false;

    if let Some(parts) = candidate
        .and_then(|c| c.pointer("/content/parts"))
        .and_then(Value::as_array)
    {
        for part in parts {
            if let Some(text) = part.get("text").and_then(Value::as_str) {
                if part
                    .get("thought")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    reasoning.push_str(text);
                } else {
                    content.push_str(text);
                }
                continue;
            }
            if let Some(call) = part.get("functionCall") {
                has_function_call = true;
                let name = call.get("name").and_then(Value::as_str).unwrap_or("");
                let arguments = call
                    .get("args")
                    .map(crate::proxy::converter::canonical_json_string)
                    .unwrap_or_else(|| "{}".to_string());
                let mut tool_call = json!({
                    "id": format!("call_{}", crate::proxy::converter::rand_hex(12)),
                    "type": "function",
                    "function": { "name": name, "arguments": arguments },
                });
                if let Some(signature) = part.get("thoughtSignature").and_then(Value::as_str) {
                    tool_call["thoughtSignature"] = json!(signature);
                }
                tool_calls.push(tool_call);
            }
        }
    }

    let finish_reason = finish_from_gemini(
        candidate
            .and_then(|c| c.get("finishReason"))
            .and_then(Value::as_str),
        has_function_call,
    );

    let mut message_extra = HashMap::new();
    if !reasoning.is_empty() {
        message_extra.insert("reasoning_content".into(), Value::String(reasoning));
    }
    let message = OAIMessage {
        role: "assistant".into(),
        content: (!content.is_empty()).then_some(content),
        tool_calls: (!tool_calls.is_empty()).then_some(tool_calls),
        refusal: None,
        extra: message_extra,
    };

    let usage = body.get("usageMetadata").map(|meta| {
        let prompt = meta
            .get("promptTokenCount")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32;
        let completion = meta
            .get("candidatesTokenCount")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32;
        let total = meta
            .get("totalTokenCount")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32;
        let mut extra = HashMap::new();
        if let Some(thoughts) = meta.get("thoughtsTokenCount").and_then(Value::as_u64) {
            extra.insert(
                "completion_tokens_details".into(),
                json!({ "reasoning_tokens": thoughts }),
            );
        }
        Usage {
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: total,
            extra,
        }
    });

    OAIChatResponse {
        id: body
            .get("responseId")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("gemini-{}", crate::proxy::converter::rand_hex(12))),
        object: "chat.completion".into(),
        created: chrono::Utc::now().timestamp(),
        model: body
            .get("modelVersion")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| requested_model.to_string()),
        choices: vec![OAIChoice {
            index: 0,
            message,
            finish_reason,
            extra: Default::default(),
        }],
        usage,
        extra: Default::default(),
        source_anthropic_response: None,
    }
}

// ----- entry surface: canonical response -> Gemini -----

/// Convert a canonical chat-completions response into a Gemini
/// `GenerateContentResponse` body.
pub fn oai_resp_to_gemini_resp(resp: &OAIChatResponse, requested_model: &str) -> Value {
    let model = if resp.model.is_empty() {
        requested_model.to_string()
    } else {
        resp.model.clone()
    };
    let mut parts: Vec<Value> = Vec::new();
    let mut finish_reason: Option<&str> = None;
    for choice in &resp.choices {
        if let Some(reasoning) = choice
            .message
            .extra
            .get("reasoning_content")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            parts.push(json!({ "thought": true, "text": reasoning }));
        }
        if let Some(text) = choice.message.content.as_deref().filter(|t| !t.is_empty()) {
            parts.push(json!({ "text": text }));
        }
        if let Some(calls) = &choice.message.tool_calls {
            for call in calls {
                let name = call
                    .pointer("/function/name")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let arguments = call
                    .pointer("/function/arguments")
                    .and_then(Value::as_str)
                    .and_then(|s| serde_json::from_str::<Value>(s).ok())
                    .unwrap_or_else(|| json!({}));
                parts.push(json!({ "functionCall": { "name": name, "args": arguments } }));
            }
        }
        finish_reason = choice.finish_reason.as_deref().or(finish_reason);
    }

    let usage = resp.usage.as_ref().map(|usage| {
        let mut meta = json!({
            "promptTokenCount": usage.prompt_tokens,
            "candidatesTokenCount": usage.completion_tokens,
            "totalTokenCount": usage.total_tokens,
        });
        if let Some(reasoning) = usage
            .extra
            .get("completion_tokens_details")
            .and_then(|details| details.get("reasoning_tokens"))
            .and_then(Value::as_u64)
        {
            meta["thoughtsTokenCount"] = json!(reasoning);
        }
        meta
    });

    json!({
        "candidates": [{
            "content": { "role": "model", "parts": parts },
            "finishReason": finish_to_gemini(finish_reason),
            "index": 0,
        }],
        "usageMetadata": usage.unwrap_or(json!({
            "promptTokenCount": 0, "candidatesTokenCount": 0, "totalTokenCount": 0,
        })),
        "modelVersion": model,
    })
}

// ----- upstream provider: Gemini SSE -> canonical chunks -----

/// Convert a stream of Gemini SSE data strings into canonical OAI chunks.
///
/// Each Gemini chunk contributes the text of its non-thought parts as
/// content deltas, thought parts as `reasoning_content`, complete
/// `functionCall` parts as `tool_calls` deltas, the `finishReason` on the
/// chunk that carries it, and `usageMetadata` (usually the final chunk) as
/// the chat usage payload.
pub fn gemini_stream_to_oai_chunks<S>(
    data_strings: S,
    model: String,
) -> Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>>
where
    S: Stream<Item = String> + Send + 'static,
{
    use async_stream::stream;
    let mut data_strings = Box::pin(data_strings);
    Box::pin(stream! {
        while let Some(data) = data_strings.next().await {
            let event: Value = match serde_json::from_str(&data) {
                Ok(value) => value,
                Err(_) => continue,
            };
            if let Some(error) = event.get("error") {
                let status = crate::proxy::canonical::stream_error_status(error);
                yield crate::proxy::canonical::stream_error_chunk(&model, status, "upstream_error");
                continue;
            }
            let candidate = event
                .get("candidates")
                .and_then(Value::as_array)
                .and_then(|candidates| candidates.first());
            let parts = candidate
                .and_then(|c| c.pointer("/content/parts"))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();

            let mut text = String::new();
            let mut reasoning = String::new();
            let mut tool_calls: Vec<Value> = Vec::new();
            let mut has_function_call = false;
            for part in &parts {
                if let Some(part_text) = part.get("text").and_then(Value::as_str) {
                    if part.get("thought").and_then(Value::as_bool).unwrap_or(false) {
                        reasoning.push_str(part_text);
                    } else {
                        text.push_str(part_text);
                    }
                    continue;
                }
                if let Some(call) = part.get("functionCall") {
                    has_function_call = true;
                    tool_calls.push(json!({
                        "index": tool_calls.len(),
                        "id": format!("call_{}", crate::proxy::converter::rand_hex(12)),
                        "type": "function",
                        "function": {
                            "name": call.get("name").and_then(Value::as_str).unwrap_or(""),
                            "arguments": call
                                .get("args")
                                .map(crate::proxy::converter::canonical_json_string)
                                .unwrap_or_else(|| "{}".to_string()),
                        },
                    }));
                }
            }

            let finish = finish_from_gemini(
                candidate
                    .and_then(|c| c.get("finishReason"))
                    .and_then(Value::as_str),
                has_function_call,
            );

            if text.is_empty() && reasoning.is_empty() && tool_calls.is_empty() && finish.is_none()
                && event.get("usageMetadata").is_none()
            {
                continue;
            }

            let mut delta = StreamDelta::default();
            if !text.is_empty() {
                delta.content = Some(text);
            }
            if !reasoning.is_empty() {
                delta.reasoning = Some(reasoning);
            }
            if !tool_calls.is_empty() {
                delta.tool_calls = Some(tool_calls);
            }
            let mut extra: HashMap<String, Value> = HashMap::new();
            if let Some(meta) = event.get("usageMetadata") {
                extra.insert("usage".into(), json!({
                    "prompt_tokens": meta.get("promptTokenCount").and_then(Value::as_u64).unwrap_or(0),
                    "completion_tokens": meta.get("candidatesTokenCount").and_then(Value::as_u64).unwrap_or(0),
                    "total_tokens": meta.get("totalTokenCount").and_then(Value::as_u64).unwrap_or(0),
                }));
            }
            yield OAIStreamChunk {
                id: "gemini_stream".into(),
                object: "chat.completion.chunk".into(),
                created: chrono::Utc::now().timestamp(),
                model: model.clone(),
                choices: vec![StreamChoice {
                    index: 0,
                    delta,
                    finish_reason: finish,
                    extra: Default::default(),
                }],
                extra,
                source_anthropic_event: None,
            };
        }
    })
}

// ----- entry surface: canonical chunks -> Gemini SSE -----

/// Accumulates one streamed function call until its arguments are complete,
/// because Gemini emits each `functionCall` part whole rather than as
/// argument deltas.
#[allow(non_camel_case_types)]
struct Gemini_stream_tool_call {
    id: String,
    name: String,
    arguments: String,
    /// chat tool_calls index of the call already flushed before this one.
    flushed: bool,
}

/// Convert a canonical chunk stream into a Gemini SSE `data:` string stream
/// (`alt=sse` framing). Gemini has no terminal sentinel — the stream simply
/// ends after the chunk carrying `usageMetadata`.
pub fn oai_chunks_to_gemini_stream(
    chunks: Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>>,
    model: String,
) -> Pin<Box<dyn Stream<Item = String> + Send>> {
    use async_stream::stream;
    let mut chunks = chunks;
    Box::pin(stream! {
        let mut pending_calls: Vec<Gemini_stream_tool_call> = Vec::new();
        while let Some(chunk) = chunks.next().await {
            if let Some(error) = chunk.extra.get("error") {
                let status = crate::proxy::canonical::stream_error_status(error);
                yield serde_json::to_string(&json!({
                    "error": {
                        "code": status,
                        "message": crate::proxy::canonical::public_error_message("upstream_stream_error", Some(status)),
                        "status": "INTERNAL",
                    }
                }))
                .unwrap_or_default();
                continue;
            }
            for choice in &chunk.choices {
                let delta = &choice.delta;
                let mut parts: Vec<Value> = Vec::new();
                if let Some(reasoning) = delta.reasoning_text().filter(|s| !s.is_empty()) {
                    parts.push(json!({ "thought": true, "text": reasoning }));
                }
                if let Some(content) = delta.content.as_deref().filter(|s| !s.is_empty()) {
                    parts.push(json!({ "text": content }));
                }

                // Flush any completed earlier calls once a new one starts.
                if let Some(calls) = &delta.tool_calls {
                    for call in calls {
                        let index = call.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                        let name = call
                            .pointer("/function/name")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        let arguments = call
                            .pointer("/function/arguments")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        if index >= pending_calls.len() {
                            // Flush everything before this new index.
                            let completed: Vec<Gemini_stream_tool_call> =
                                pending_calls.drain(..).collect();
                            for mut completed_call in completed {
                                completed_call.flushed = true;
                                let args: Value = serde_json::from_str(&completed_call.arguments)
                                    .unwrap_or_else(|_| json!({}));
                                parts.push(json!({
                                    "functionCall": {
                                        "name": completed_call.name,
                                        "args": args,
                                    }
                                }));
                            }
                            while pending_calls.len() <= index {
                                pending_calls.push(Gemini_stream_tool_call {
                                    id: String::new(),
                                    name: String::new(),
                                    arguments: String::new(),
                                    flushed: false,
                                });
                            }
                        }
                        let slot = &mut pending_calls[index];
                        if !name.is_empty() {
                            slot.name = name;
                        }
                        slot.arguments.push_str(&arguments);
                        if let Some(id) = call.get("id").and_then(Value::as_str) {
                            slot.id = id.to_string();
                        }
                    }
                }

                let mut body = json!({
                    "candidates": [{
                        "content": { "role": "model", "parts": parts },
                        "index": 0,
                    }],
                    "modelVersion": model,
                });
                if let Some(reason) = choice.finish_reason.as_deref() {
                    if reason == "tool_calls" || reason == "stop" {
                        // Flush remaining calls with the terminal chunk.
                        let completed: Vec<Gemini_stream_tool_call> =
                            pending_calls.drain(..).collect();
                        let parts = body
                            .pointer_mut("/candidates/0/content/parts")
                            .and_then(Value::as_array_mut);
                        if let Some(parts) = parts {
                            for call in completed {
                                let args: Value = serde_json::from_str(&call.arguments)
                                    .unwrap_or_else(|_| json!({}));
                                parts.push(json!({
                                    "functionCall": { "name": call.name, "args": args }
                                }));
                            }
                        }
                    }
                    body["candidates"][0]["finishReason"] = json!(finish_to_gemini(Some(reason)));
                }
                if let Some(usage) = chunk.extra.get("usage") {
                    body["usageMetadata"] = json!({
                        "promptTokenCount": usage.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(0),
                        "candidatesTokenCount": usage.get("completion_tokens").and_then(Value::as_u64).unwrap_or(0),
                        "totalTokenCount": usage.get("total_tokens").and_then(Value::as_u64).unwrap_or(0),
                    });
                }
                let has_parts = body
                    .pointer("/candidates/0/content/parts")
                    .and_then(Value::as_array)
                    .is_some_and(|parts| !parts.is_empty());
                if has_parts
                    || body.pointer("/candidates/0/finishReason").is_some()
                    || body.get("usageMetadata").is_some()
                {
                    yield serde_json::to_string(&body).unwrap_or_default();
                }
            }
        }
        // Any calls that never saw a finish_reason still need flushing.
        if !pending_calls.is_empty() {
            let parts: Vec<Value> = pending_calls
                .drain(..)
                .map(|call| {
                    let args: Value =
                        serde_json::from_str(&call.arguments).unwrap_or_else(|_| json!({}));
                    json!({ "functionCall": { "name": call.name, "args": args } })
                })
                .collect();
            let body = json!({
                "candidates": [{
                    "content": { "role": "model", "parts": parts },
                    "finishReason": "STOP",
                    "index": 0,
                }],
                "modelVersion": model,
            });
            yield serde_json::to_string(&body).unwrap_or_default();
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gemini_request_to_chat_maps_system_tools_and_history() {
        let body = json!({
            "systemInstruction": { "parts": [{ "text": "be brief" }] },
            "contents": [
                { "role": "user", "parts": [{ "text": "hi" }] },
                { "role": "model", "parts": [{ "functionCall": { "name": "get_weather", "args": { "city": "sf" } } }] },
                { "role": "user", "parts": [{ "functionResponse": { "name": "get_weather", "response": { "temp": 18 } } }] },
            ],
            "tools": [{ "functionDeclarations": [{
                "name": "get_weather",
                "description": "weather",
                "parameters": {
                    "type": "object",
                    "properties": { "city": { "type": "string", "$schema": "x", "additionalProperties": false } },
                    "required": ["city"],
                },
            }]}],
            "toolConfig": { "functionCallingConfig": { "mode": "AUTO" } },
            "generationConfig": { "temperature": 0.5, "maxOutputTokens": 1024, "topP": 0.9 },
        });
        let req = gemini_req_to_oai_req(&body);
        assert_eq!(req.messages[0].role, "system");
        assert_eq!(req.messages[0].content, json!("be brief"));
        assert_eq!(req.messages[1].role, "user");
        assert_eq!(req.messages[1].content, json!("hi"));
        assert_eq!(req.messages[2].role, "assistant");
        assert_eq!(
            req.messages[2].tool_calls.as_ref().unwrap()[0].pointer("/function/name"),
            Some(&json!("get_weather"))
        );
        assert_eq!(req.messages[3].role, "tool");
        assert_eq!(req.messages[3].tool_call_id.as_deref(), Some("get_weather"));
        assert_eq!(req.temperature, Some(0.5));
        assert_eq!(req.max_tokens, Some(1024));
        assert_eq!(req.extra.get("top_p"), Some(&json!(0.9)));
        let tools = req.tools.unwrap();
        assert_eq!(
            tools[0].pointer("/function/name"),
            Some(&json!("get_weather"))
        );
        // $schema and additionalProperties stripped from the schema.
        let params = tools[0].pointer("/function/parameters").unwrap();
        assert!(params.pointer("/properties/city/$schema").is_none());
        assert!(params
            .pointer("/properties/city/additionalProperties")
            .is_none());
        assert_eq!(params.get("required"), Some(&json!(["city"])));
        assert_eq!(req.tool_choice, Some(json!("auto")));
    }

    #[test]
    fn chat_request_to_gemini_round_trip_tool_history() {
        let req: ChatCompletionRequest = serde_json::from_value(json!({
            "model": "gemini-2.5-pro",
            "messages": [
                { "role": "system", "content": "be brief" },
                { "role": "user", "content": "weather in sf?" },
                { "role": "assistant", "content": null, "tool_calls": [{
                    "id": "call_abc", "type": "function",
                    "function": { "name": "get_weather", "arguments": "{\"city\":\"sf\"}" }
                }]},
                { "role": "tool", "tool_call_id": "call_abc", "content": "{\"temp\":18}" },
            ],
            "tools": [{ "type": "function", "function": {
                "name": "get_weather", "description": "w",
                "parameters": { "type": "object", "properties": { "city": { "type": "string" } } },
            }}],
            "tool_choice": "auto",
        }))
        .unwrap();
        let gemini = oai_req_to_gemini_req(&req);
        assert_eq!(
            gemini
                .system_instruction
                .as_ref()
                .unwrap()
                .pointer("/parts/0/text"),
            Some(&json!("be brief"))
        );
        assert_eq!(gemini.contents.len(), 3);
        assert_eq!(gemini.contents[0].role, "user");
        assert_eq!(
            gemini.contents[1].parts.pointer("/0/functionCall/name"),
            Some(&json!("get_weather"))
        );
        assert_eq!(
            gemini.contents[1].parts.pointer("/0/functionCall/args"),
            Some(&json!({ "city": "sf" }))
        );
        // tool result carries the function NAME resolved from call id.
        assert_eq!(
            gemini.contents[2].parts.pointer("/0/functionResponse/name"),
            Some(&json!("get_weather"))
        );
        assert_eq!(
            gemini.contents[2]
                .parts
                .pointer("/0/functionResponse/response"),
            Some(&json!({ "temp": 18 }))
        );
        let tools = gemini.tools.unwrap();
        assert_eq!(
            tools.pointer("/0/functionDeclarations/0/name"),
            Some(&json!("get_weather"))
        );
        assert!(gemini.tool_config.is_none());
    }

    #[test]
    fn gemini_response_to_chat_maps_parts_and_usage() {
        let body = json!({
            "candidates": [{
                "content": { "role": "model", "parts": [
                    { "text": "let me check" },
                    { "functionCall": { "name": "get_weather", "args": { "city": "sf" } } },
                ]},
                "finishReason": "STOP",
            }],
            "usageMetadata": {
                "promptTokenCount": 10, "candidatesTokenCount": 5,
                "totalTokenCount": 15, "thoughtsTokenCount": 3,
            },
            "modelVersion": "gemini-2.5-pro",
        });
        let resp = gemini_resp_to_oai_resp(&body, "gemini-2.5-pro");
        let choice = &resp.choices[0];
        assert_eq!(choice.message.content.as_deref(), Some("let me check"));
        assert_eq!(choice.finish_reason.as_deref(), Some("tool_calls"));
        let usage = resp.usage.unwrap();
        assert_eq!(usage.prompt_tokens, 10);
        assert_eq!(usage.completion_tokens, 5);
        assert_eq!(usage.total_tokens, 15);
        assert_eq!(resp.model, "gemini-2.5-pro");
    }

    #[test]
    fn chat_response_to_gemini_includes_function_calls() {
        let resp: OAIChatResponse = serde_json::from_value(json!({
            "id": "chatcmpl-1", "object": "chat.completion", "created": 1,
            "model": "gemini-2.5-pro",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant", "content": "checking",
                    "tool_calls": [{
                        "id": "call_1", "type": "function",
                        "function": { "name": "get_weather", "arguments": "{\"city\":\"sf\"}" }
                    }],
                },
                "finish_reason": "tool_calls",
            }],
            "usage": { "prompt_tokens": 3, "completion_tokens": 2, "total_tokens": 5 },
        }))
        .unwrap();
        let body = oai_resp_to_gemini_resp(&resp, "gemini-2.5-pro");
        assert_eq!(
            body.pointer("/candidates/0/content/parts/0/text"),
            Some(&json!("checking"))
        );
        assert_eq!(
            body.pointer("/candidates/0/content/parts/1/functionCall/name"),
            Some(&json!("get_weather"))
        );
        assert_eq!(
            body.pointer("/candidates/0/finishReason"),
            Some(&json!("STOP"))
        );
        assert_eq!(
            body.pointer("/usageMetadata/promptTokenCount"),
            Some(&json!(3))
        );
    }

    #[tokio::test]
    async fn gemini_stream_round_trip_preserves_text_tools_usage() {
        use futures_util::stream;
        let upstream = stream::iter(vec![
            json!({
                "candidates": [{ "content": { "role": "model", "parts": [{ "text": "hello " }] }, "index": 0 }]
            })
            .to_string(),
            json!({
                "candidates": [{ "content": { "role": "model", "parts": [
                    { "text": "world" },
                    { "functionCall": { "name": "get_weather", "args": { "city": "sf" } } },
                ]}, "finishReason": "STOP", "index": 0 }],
                "usageMetadata": { "promptTokenCount": 4, "candidatesTokenCount": 6, "totalTokenCount": 10 },
            })
            .to_string(),
        ]);
        let chunks = gemini_stream_to_oai_chunks(upstream, "gemini-2.5-pro".into());
        let collected: Vec<OAIStreamChunk> = chunks.collect().await;
        assert_eq!(collected.len(), 2);
        assert_eq!(
            collected[0].choices[0].delta.content.as_deref(),
            Some("hello ")
        );
        assert_eq!(
            collected[1].choices[0].delta.content.as_deref(),
            Some("world")
        );
        let calls = collected[1].choices[0].delta.tool_calls.as_ref().unwrap();
        assert_eq!(
            calls[0].pointer("/function/name"),
            Some(&json!("get_weather"))
        );
        assert_eq!(
            collected[1].choices[0].finish_reason.as_deref(),
            Some("tool_calls")
        );
        assert_eq!(
            collected[1].extra.get("usage").unwrap().get("total_tokens"),
            Some(&json!(10))
        );

        // Now the entry direction: chunk stream -> Gemini SSE frames.
        let chunk_stream = Box::pin(stream::iter(collected));
        let sse = oai_chunks_to_gemini_stream(chunk_stream, "gemini-2.5-pro".into());
        let frames: Vec<Value> = sse
            .filter_map(|frame| async move { serde_json::from_str(&frame).ok() })
            .collect()
            .await;
        assert!(frames.len() >= 2);
        assert_eq!(
            frames[0].pointer("/candidates/0/content/parts/0/text"),
            Some(&json!("hello "))
        );
        let last = frames.last().unwrap();
        assert_eq!(
            last.pointer("/candidates/0/finishReason"),
            Some(&json!("STOP"))
        );
        assert_eq!(
            last.pointer("/candidates/0/content/parts")
                .and_then(Value::as_array)
                .unwrap()
                .iter()
                .any(|p| p.get("functionCall").is_some()),
            true
        );
        assert_eq!(
            last.pointer("/usageMetadata/totalTokenCount"),
            Some(&json!(10))
        );
    }

    #[test]
    fn normalize_gemini_model_id_strips_prefix() {
        assert_eq!(
            normalize_gemini_model_id("models/gemini-2.5-pro"),
            "gemini-2.5-pro"
        );
        assert_eq!(
            normalize_gemini_model_id("/models/gemini-2.5-pro"),
            "gemini-2.5-pro"
        );
        assert_eq!(
            normalize_gemini_model_id("gemini-2.5-pro"),
            "gemini-2.5-pro"
        );
    }

    #[test]
    fn sanitize_schema_drops_unsupported_keywords_recursively() {
        let schema = json!({
            "type": "object",
            "$schema": "http://json-schema.org/draft-07/schema#",
            "additionalProperties": false,
            "properties": {
                "city": {
                    "type": "string",
                    "default": "sf",
                    "enum": ["sf", "nyc"],
                },
                "nested": {
                    "type": "object",
                    "properties": { "x": { "type": "integer", "exclusiveMinimum": 0 } },
                },
            },
            "required": ["city"],
        });
        let sanitized = sanitize_gemini_schema(schema);
        assert!(sanitized.get("$schema").is_none());
        assert!(sanitized.get("additionalProperties").is_none());
        assert!(sanitized.pointer("/properties/city/default").is_none());
        assert!(sanitized.pointer("/properties/city/enum").is_some());
        assert!(sanitized
            .pointer("/properties/nested/properties/x/exclusiveMinimum")
            .is_none());
        assert_eq!(sanitized.get("required"), Some(&json!(["city"])));
    }
}
