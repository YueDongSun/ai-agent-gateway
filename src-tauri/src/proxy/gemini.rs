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
//! Private source envelopes retain native Gemini bodies and SSE data objects.
//! Foreign providers receive a strict projection that refuses unsupported
//! semantics instead of silently dropping native state or controls.

use crate::proxy::canonical::{
    ChatCompletionRequest, ChatMessage, OAIChatResponse, OAIChoice, OAIMessage, OAIStreamChunk,
    ProviderError, StreamChoice, StreamDelta, Usage,
};
use futures_core::Stream;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
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

fn gemini_conversion_error(code: &str) -> ProviderError {
    crate::proxy::canonical::make_provider_error(
        code,
        "request_conversion",
        Some(400),
        Some(false),
        None,
    )
}

fn known_gemini_fields(value: &Value, names: &[&str]) -> Result<(), ProviderError> {
    let object = value
        .as_object()
        .ok_or_else(|| gemini_conversion_error("gemini_invalid_request"))?;
    if object.keys().any(|key| !names.contains(&key.as_str())) {
        return Err(gemini_conversion_error("gemini_feature_unsupported"));
    }
    Ok(())
}

/// Admit only the Gemini subset that the canonical bridge can represent.
/// Unsupported data must fail before an upstream call, rather than disappear
/// during the tolerant projection retained for internal conversion callers.
pub fn try_gemini_req_to_oai_req(body: &Value) -> Result<ChatCompletionRequest, ProviderError> {
    known_gemini_fields(
        body,
        &[
            "contents",
            "systemInstruction",
            "tools",
            "toolConfig",
            "generationConfig",
        ],
    )?;
    let contents = body
        .get("contents")
        .and_then(Value::as_array)
        .filter(|contents| !contents.is_empty())
        .ok_or_else(|| gemini_conversion_error("gemini_invalid_request"))?;
    for content in contents {
        known_gemini_fields(content, &["role", "parts"])?;
        let role = content
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("user");
        if !matches!(role, "user" | "model") {
            return Err(gemini_conversion_error("gemini_invalid_request"));
        }
        let parts = content
            .get("parts")
            .and_then(Value::as_array)
            .filter(|parts| !parts.is_empty())
            .ok_or_else(|| gemini_conversion_error("gemini_invalid_request"))?;
        for part in parts {
            known_gemini_fields(
                part,
                &[
                    "text",
                    "thought",
                    "thoughtSignature",
                    "functionCall",
                    "functionResponse",
                    "inlineData",
                ],
            )?;
            let count = ["text", "functionCall", "functionResponse", "inlineData"]
                .iter()
                .filter(|name| part.get(**name).is_some())
                .count();
            if count != 1 {
                return Err(gemini_conversion_error("gemini_invalid_request"));
            }
            if let Some(text) = part.get("text") {
                if !text.is_string() || part.get("thought").is_some_and(|value| !value.is_boolean())
                {
                    return Err(gemini_conversion_error("gemini_invalid_request"));
                }
                // The bridge retains function-call signatures. A signature
                // attached to a text block has no canonical equivalent yet.
                if part.get("thoughtSignature").is_some() {
                    return Err(gemini_conversion_error("gemini_feature_unsupported"));
                }
                if part.get("thought").and_then(Value::as_bool) == Some(true) {
                    return Err(gemini_conversion_error("gemini_feature_unsupported"));
                }
            } else if let Some(call) = part.get("functionCall") {
                known_gemini_fields(call, &["name", "args"])?;
                if role != "model"
                    || call
                        .get("name")
                        .and_then(Value::as_str)
                        .is_none_or(str::is_empty)
                    || call.get("args").is_some_and(|args| !args.is_object())
                    || part
                        .get("thoughtSignature")
                        .is_some_and(|signature| !signature.is_string())
                {
                    return Err(gemini_conversion_error("gemini_invalid_request"));
                }
            } else if let Some(response) = part.get("functionResponse") {
                known_gemini_fields(response, &["name", "response"])?;
                if role != "user"
                    || response
                        .get("name")
                        .and_then(Value::as_str)
                        .is_none_or(str::is_empty)
                    || response
                        .get("response")
                        .is_none_or(|response| !response.is_object())
                {
                    return Err(gemini_conversion_error("gemini_invalid_request"));
                }
                if part.get("thoughtSignature").is_some() || part.get("thought").is_some() {
                    return Err(gemini_conversion_error("gemini_feature_unsupported"));
                }
            } else if let Some(inline) = part.get("inlineData") {
                known_gemini_fields(inline, &["mimeType", "data"])?;
                let mime = inline.get("mimeType").and_then(Value::as_str).unwrap_or("");
                if role != "user"
                    || !mime.starts_with("image/")
                    || inline
                        .get("data")
                        .and_then(Value::as_str)
                        .is_none_or(str::is_empty)
                    || part.get("thoughtSignature").is_some()
                    || part.get("thought").is_some()
                {
                    return Err(gemini_conversion_error("gemini_feature_unsupported"));
                }
            }
        }
    }
    if let Some(system) = body.get("systemInstruction") {
        known_gemini_fields(system, &["role", "parts"])?;
        let parts = system
            .get("parts")
            .and_then(Value::as_array)
            .ok_or_else(|| gemini_conversion_error("gemini_invalid_request"))?;
        for part in parts {
            known_gemini_fields(part, &["text"])?;
            if !part.get("text").is_some_and(Value::is_string) {
                return Err(gemini_conversion_error("gemini_invalid_request"));
            }
        }
    }
    if let Some(generation) = body.get("generationConfig") {
        known_gemini_fields(
            generation,
            &["temperature", "maxOutputTokens", "topP", "stopSequences"],
        )?;
        if generation
            .get("temperature")
            .is_some_and(|value| value.as_f64().is_none())
            || generation
                .get("topP")
                .is_some_and(|value| value.as_f64().is_none())
            || generation.get("maxOutputTokens").is_some_and(|value| {
                value
                    .as_u64()
                    .is_none_or(|number| number == 0 || number > u32::MAX as u64)
            })
            || generation.get("stopSequences").is_some_and(|value| {
                value
                    .as_array()
                    .is_none_or(|items| items.iter().any(|item| !item.is_string()))
            })
        {
            return Err(gemini_conversion_error("gemini_invalid_request"));
        }
    }
    if let Some(tools) = body.get("tools") {
        let tools = tools
            .as_array()
            .ok_or_else(|| gemini_conversion_error("gemini_invalid_request"))?;
        for tool in tools {
            known_gemini_fields(tool, &["functionDeclarations"])?;
            let declarations = tool
                .get("functionDeclarations")
                .and_then(Value::as_array)
                .ok_or_else(|| gemini_conversion_error("gemini_feature_unsupported"))?;
            for declaration in declarations {
                known_gemini_fields(declaration, &["name", "description", "parameters"])?;
                if declaration
                    .get("name")
                    .and_then(Value::as_str)
                    .is_none_or(str::is_empty)
                    || declaration
                        .get("description")
                        .is_some_and(|value| !value.is_string())
                    || declaration
                        .get("parameters")
                        .is_some_and(|value| !value.is_object())
                {
                    return Err(gemini_conversion_error("gemini_invalid_request"));
                }
            }
        }
    }
    if let Some(tool_config) = body.get("toolConfig") {
        known_gemini_fields(tool_config, &["functionCallingConfig"])?;
        if let Some(config) = tool_config.get("functionCallingConfig") {
            known_gemini_fields(config, &["mode", "allowedFunctionNames"])?;
            let mode = config.get("mode").and_then(Value::as_str).unwrap_or("AUTO");
            let allowed = config
                .get("allowedFunctionNames")
                .map(|value| {
                    value
                        .as_array()
                        .ok_or_else(|| gemini_conversion_error("gemini_invalid_request"))
                })
                .transpose()?;
            if !matches!(mode, "AUTO" | "NONE" | "ANY")
                || allowed.is_some_and(|names| {
                    names.len() > 1 || names.iter().any(|name| !name.is_string())
                })
                || (mode != "ANY" && allowed.is_some_and(|names| !names.is_empty()))
            {
                return Err(gemini_conversion_error("gemini_feature_unsupported"));
            }
        }
    }
    Ok(gemini_req_to_oai_req(body))
}

/// Strict projection for a foreign upstream, after a native Gemini request
/// has been admitted. Preserve routing context and any explicitly injected
/// system prefix while refusing to silently change the original body.
pub(crate) fn request_for_foreign_hop(
    req: &ChatCompletionRequest,
) -> Result<ChatCompletionRequest, ProviderError> {
    let Some(source) = req.source_gemini_request.as_ref() else {
        return Ok(req.clone());
    };
    let baseline = gemini_req_to_oai_req(source);
    let prefix_len = req.messages.len().saturating_sub(baseline.messages.len());
    if req.messages.len() < baseline.messages.len()
        || req.messages[..prefix_len]
            .iter()
            .any(|message| message.role != "system")
        || serde_json::to_value(&req.messages[prefix_len..]).ok()
            != serde_json::to_value(&baseline.messages).ok()
    {
        return Err(gemini_conversion_error("gemini_feature_unsupported"));
    }
    let mut strict = try_gemini_req_to_oai_req(source)?;
    let mut messages = req.messages[..prefix_len].to_vec();
    messages.append(&mut strict.messages);
    // Signatures bind Gemini-native function calls. Foreign schemas receive
    // the actual call identity/name/arguments without Gemini-only wire fields.
    for message in &mut messages {
        for call in message.tool_calls.iter_mut().flatten() {
            if let Some(call) = call.as_object_mut() {
                call.remove("thoughtSignature");
            }
        }
    }
    strict.messages = messages;
    strict.model = req.model.clone();
    strict.stream = req.stream;
    strict.request_id = req.request_id.clone();
    strict.client_credential = req.client_credential.clone();
    strict.source_gemini_request = None;
    Ok(strict)
}

/// A native Gemini upstream consumes the preserved source body, including
/// signatures and provider-specific native controls. Cross-protocol callers
/// use the explicit canonical request conversion.
pub(crate) fn request_body(req: &ChatCompletionRequest) -> Result<Value, ProviderError> {
    // Codex effort has no supported projection in this Gemini adapter. Refuse
    // that explicit control before sending instead of silently ignoring it.
    if let Some(source) = &req.source_responses_request {
        let source_effort = source
            .extra
            .get("reasoning")
            .and_then(|value| value.get("effort"));
        if source_effort.is_some_and(|value| !value.is_null())
            || req
                .extra
                .get("reasoning_effort")
                .is_some_and(|value| !value.is_null())
        {
            return Err(gemini_conversion_error("responses_feature_unsupported"));
        }
    }
    if let Some(source) = &req.source_gemini_request {
        let baseline = gemini_req_to_oai_req(source);
        if serde_json::to_value(&req.messages).ok() != serde_json::to_value(&baseline.messages).ok()
        {
            return Err(gemini_conversion_error("gemini_feature_unsupported"));
        }
        return Ok(source.clone());
    }
    serde_json::to_value(oai_req_to_gemini_req(req))
        .map_err(|_| gemini_conversion_error("gemini_invalid_request"))
}

/// Foreign client surfaces cannot express native media/code parts or several
/// Gemini candidates. Refuse those responses before projection; native Gemini
/// callers retain the complete source envelope instead.
pub(crate) fn validate_response_for_foreign(body: &Value) -> Result<(), ProviderError> {
    let candidates = body.get("candidates").and_then(Value::as_array);
    let unsupported = candidates.is_some_and(|candidates| {
        candidates.len() > 1
            || candidates.iter().any(|candidate| {
                candidate.get("groundingMetadata").is_some()
                    || candidate.get("citationMetadata").is_some()
                    || candidate
                        .pointer("/content/parts")
                        .and_then(Value::as_array)
                        .is_some_and(|parts| {
                            parts.iter().any(|part| {
                                part.as_object().is_none_or(|part| {
                                    part.keys().any(|key| {
                                        !matches!(key.as_str(), "text" | "thought" | "functionCall")
                                    })
                                })
                            })
                        })
            })
    });
    if unsupported {
        return Err(crate::proxy::canonical::make_provider_error(
            "gemini_response_unsupported",
            "response_conversion",
            Some(502),
            Some(true),
            None,
        ));
    }
    Ok(())
}

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

    // function name → queue of generated call ids not yet matched by a
    // functionResponse. Gemini round-trips only the function NAME, but
    // foreign upstreams (OpenAI/Anthropic) correlate a tool result through
    // `tool_call_id` == the assistant call's `id`; deterministic per-name
    // queues keep that correlation intact across the canonical projection.
    let mut pending_calls: HashMap<String, VecDeque<String>> = HashMap::new();
    let mut call_counter: u64 = 0;
    for content in body
        .get("contents")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        gemini_content_to_chat(
            content,
            &mut messages,
            &mut pending_calls,
            &mut call_counter,
        );
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
        source_responses_request: None,
        source_gemini_request: Some(body.clone()),
        request_id: None,
        client_credential: None,
    }
}

/// Append one Gemini content entry to the canonical message list. Consecutive
/// `functionCall` parts join a single assistant turn (chat completions expect
/// one `tool_calls` array), and `functionResponse` entries become `tool`
/// messages whose `tool_call_id` is the id of the earliest unmatched call of
/// the same function name (falling back to the bare name when the request
/// history contains no matching call).
fn gemini_content_to_chat(
    content: &Value,
    messages: &mut Vec<ChatMessage>,
    pending_calls: &mut HashMap<String, VecDeque<String>>,
    call_counter: &mut u64,
) {
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
            let id = format!("gemini_call_{name}_{call_counter}");
            *call_counter += 1;
            pending_calls
                .entry(name.to_string())
                .or_default()
                .push_back(id.clone());
            tool_calls.push(json!({
                "id": id,
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
            // Correlate with the earliest unmatched call of this function;
            // a response whose call is not in this request keeps the bare
            // name (foreign upstreams will reject the mismatch explicitly).
            let tool_call_id = pending_calls
                .get_mut(name)
                .and_then(VecDeque::pop_front)
                .unwrap_or_else(|| name.to_string());
            messages.push(ChatMessage {
                role: "tool".into(),
                content: Value::String(payload),
                tool_calls: None,
                tool_call_id: Some(tool_call_id),
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
                        "parameters": gemini_schema_to_chat(
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

/// Gemini uses uppercase type names and an OpenAPI nullable flag. Translate
/// those two conventions while retaining JSON Schema constraints for foreign
/// providers; the outbound Gemini sanitizer has a different responsibility.
fn gemini_schema_to_chat(schema: Value) -> Value {
    let Value::Object(mut object) = schema else {
        return schema;
    };
    let nullable = object.get("nullable").and_then(Value::as_bool);
    if nullable.is_some() {
        object.remove("nullable");
    }
    for (key, value) in &mut object {
        match key.as_str() {
            "type" if value.is_string() => {
                *value = Value::String(value.as_str().unwrap().to_ascii_lowercase());
            }
            "properties" | "$defs" | "definitions" => {
                if let Some(properties) = value.as_object_mut() {
                    for property in properties.values_mut() {
                        *property = gemini_schema_to_chat(std::mem::take(property));
                    }
                }
            }
            "items" | "additionalProperties" | "not" => {
                *value = gemini_schema_to_chat(std::mem::take(value));
            }
            "anyOf" | "oneOf" | "allOf" => {
                if let Some(variants) = value.as_array_mut() {
                    for variant in variants {
                        *variant = gemini_schema_to_chat(std::mem::take(variant));
                    }
                }
            }
            _ => {}
        }
    }
    let schema = Value::Object(object);
    if nullable == Some(true) {
        json!({"anyOf":[schema,{"type":"null"}]})
    } else {
        schema
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
        source_responses_response: None,
        source_gemini_response: Some(body.clone()),
    }
}

// ----- entry surface: canonical response -> Gemini -----

/// Convert a canonical chat-completions response into a Gemini
/// `GenerateContentResponse` body.
pub fn oai_resp_to_gemini_resp(resp: &OAIChatResponse, requested_model: &str) -> Value {
    if let Some(source) = &resp.source_gemini_response {
        return source.clone();
    }
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
    gemini_stream_to_oai_chunks_for_client(data_strings, model, true)
}

pub(crate) fn gemini_stream_to_oai_chunks_for_client<S>(
    data_strings: S,
    model: String,
    native_client: bool,
) -> Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>>
where
    S: Stream<Item = String> + Send + 'static,
{
    use async_stream::stream;
    let mut data_strings = Box::pin(data_strings);
    Box::pin(stream! {
        // Stream-wide call counter: each Gemini functionCall part is a
        // complete, distinct call, so its chat `index` must be unique across
        // the whole stream — a per-frame 0-based index would make consumers
        // merging deltas by index collapse two calls into one.
        let mut next_tool_index: u64 = 0;
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
            if !native_client && validate_response_for_foreign(&event).is_err() {
                yield crate::proxy::canonical::stream_error_chunk(&model, 502, "gemini_response_unsupported");
                return;
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
                        "index": next_tool_index,
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
                    next_tool_index += 1;
                }
            }

            let finish = finish_from_gemini(
                candidate
                    .and_then(|c| c.get("finishReason"))
                    .and_then(Value::as_str),
                has_function_call,
            );

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
                source_responses_event: None,
                source_gemini_event: Some(event),
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
    name: String,
    arguments: String,
    /// False for index-gap placeholders: they must never be emitted as
    /// empty-name function calls.
    touched: bool,
}

/// Drain accumulated calls into Gemini `functionCall` parts in stream-index
/// order. Untouched index-gap placeholders are skipped. A touched call that
/// never carried a function name is an upstream protocol error, not a valid
/// Gemini part, and fails the drain explicitly.
fn drain_pending_calls(
    pending: &mut Vec<Gemini_stream_tool_call>,
    parts: &mut Vec<Value>,
) -> Result<(), ()> {
    let mut malformed = false;
    for call in pending.drain(..) {
        if !call.touched {
            continue;
        }
        if call.name.is_empty() {
            malformed = true;
            continue;
        }
        let args: Value = serde_json::from_str(&call.arguments).unwrap_or_else(|_| json!({}));
        parts.push(json!({ "functionCall": { "name": call.name, "args": args } }));
    }
    if malformed {
        Err(())
    } else {
        Ok(())
    }
}

fn gemini_stream_error_frame() -> String {
    serde_json::to_string(&json!({
        "error": {
            "code": 502,
            "message": crate::proxy::canonical::public_error_message("upstream_stream_error", Some(502)),
            "status": "INTERNAL",
        }
    }))
    .unwrap_or_default()
}

/// Canonical chunk usage -> Gemini `usageMetadata` (stream and metadata-only
/// trailing frames share this shape).
fn gemini_usage_metadata(usage: &Value) -> Value {
    json!({
        "promptTokenCount": usage.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(0),
        "candidatesTokenCount": usage.get("completion_tokens").and_then(Value::as_u64).unwrap_or(0),
        "totalTokenCount": usage.get("total_tokens").and_then(Value::as_u64).unwrap_or(0),
    })
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
            if let Some(source) = chunk.source_gemini_event {
                yield serde_json::to_string(&source).unwrap_or_default();
                continue;
            }
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
            // The OpenAI `stream_options.include_usage` terminal chunk carries
            // `"choices": []` with a trailing usage payload — relay it as a
            // metadata-only Gemini frame instead of dropping the usage.
            if chunk.choices.is_empty() {
                if let Some(usage) = chunk.extra.get("usage") {
                    let body = json!({
                        "modelVersion": model,
                        "usageMetadata": gemini_usage_metadata(usage),
                    });
                    yield serde_json::to_string(&body).unwrap_or_default();
                }
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

                // Accumulate argument deltas per call index. A new index does
                // NOT complete the previous call — upstreams may interleave
                // parallel calls — so calls flush only with the terminal
                // chunk (or an unterminated stream end), never mid-stream.
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
                        while pending_calls.len() <= index {
                            pending_calls.push(Gemini_stream_tool_call {
                                name: String::new(),
                                arguments: String::new(),
                                touched: false,
                            });
                        }
                        let slot = &mut pending_calls[index];
                        slot.touched = true;
                        if !name.is_empty() {
                            slot.name = name;
                        }
                        slot.arguments.push_str(&arguments);
                    }
                }

                let mut body = json!({
                    "candidates": [{
                        "content": { "role": "model", "parts": parts },
                        "index": 0,
                    }],
                    "modelVersion": model,
                });
                let mut malformed_tools = false;
                if let Some(reason) = choice.finish_reason.as_deref() {
                    // Flush every accumulated call with the terminal chunk,
                    // whatever the finish reason — completed calls must not
                    // be dropped when the finish is `length`/`content_filter`.
                    let terminal_parts = body
                        .pointer_mut("/candidates/0/content/parts")
                        .and_then(Value::as_array_mut);
                    if let Some(terminal_parts) = terminal_parts {
                        malformed_tools =
                            drain_pending_calls(&mut pending_calls, terminal_parts).is_err();
                    }
                    body["candidates"][0]["finishReason"] = json!(finish_to_gemini(Some(reason)));
                }
                if malformed_tools {
                    yield gemini_stream_error_frame();
                    continue;
                }
                if let Some(usage) = chunk.extra.get("usage") {
                    body["usageMetadata"] = gemini_usage_metadata(usage);
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
        // The stream ended without a terminal chunk: flush accumulated calls
        // truthfully, without fabricating a finishReason.
        if pending_calls.iter().any(|call| call.touched) {
            let mut parts: Vec<Value> = Vec::new();
            if drain_pending_calls(&mut pending_calls, &mut parts).is_err() {
                yield gemini_stream_error_frame();
            } else if !parts.is_empty() {
                let body = json!({
                    "candidates": [{
                        "content": { "role": "model", "parts": parts },
                        "index": 0,
                    }],
                    "modelVersion": model,
                });
                yield serde_json::to_string(&body).unwrap_or_default();
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inbound_schema_translation_preserves_constraints_and_enum_values() {
        let schema = json!({"type":"OBJECT","additionalProperties":false,"properties":{
            "value":{"type":"STRING","nullable":true,"default":"fallback"},
            "kind":{"enum":[{"type":"LITERAL_VALUE"}]}
        }});
        let translated = gemini_schema_to_chat(schema);
        assert_eq!(translated["type"], "object");
        assert_eq!(translated["additionalProperties"], false);
        assert_eq!(
            translated["properties"]["value"]["anyOf"][0]["type"],
            "string"
        );
        assert_eq!(
            translated["properties"]["value"]["anyOf"][0]["default"],
            "fallback"
        );
        assert_eq!(
            translated["properties"]["value"]["anyOf"][1]["type"],
            "null"
        );
        assert_eq!(
            translated["properties"]["kind"]["enum"][0]["type"],
            "LITERAL_VALUE"
        );
    }

    #[test]
    fn native_gemini_request_preserves_opaque_controls_but_foreign_projection_refuses_them() {
        let body = json!({
            "contents":[{"role":"user","parts":[{"fileData":{"mimeType":"application/pdf","fileUri":"fixture://document"}}]}],
            "cachedContent":"cachedContents/fixture",
            "generationConfig":{"responseMimeType":"application/json","responseSchema":{"type":"OBJECT"}},
        });
        let req = gemini_req_to_oai_req(&body);
        assert_eq!(request_body(&req).unwrap(), body);
        let error = request_for_foreign_hop(&req).unwrap_err();
        assert_eq!(error.code, "gemini_feature_unsupported");
        assert_eq!(error.status, Some(400));
        assert!(!error.retriable);
    }

    #[test]
    fn foreign_projection_preserves_context_and_removes_only_native_call_metadata() {
        let body = json!({"contents":[
            {"role":"model","parts":[{"functionCall":{"name":"lookup","args":{"query":"fixture"}},"thoughtSignature":"native_signature"}]},
            {"role":"user","parts":[{"functionResponse":{"name":"lookup","response":{"ok":true}}}]}
        ]});
        let mut req = gemini_req_to_oai_req(&body);
        req.model = "client-role".into();
        req.stream = true;
        req.request_id = Some("request_fixture".into());
        req.messages.insert(
            0,
            ChatMessage {
                role: "system".into(),
                content: json!("explicit prefix"),
                tool_calls: None,
                tool_call_id: None,
                extra: Default::default(),
            },
        );
        let projected = request_for_foreign_hop(&req).unwrap();
        assert_eq!(projected.model, "client-role");
        assert!(projected.stream);
        assert_eq!(projected.request_id.as_deref(), Some("request_fixture"));
        assert_eq!(projected.messages[0].content, "explicit prefix");
        let call = &projected.messages[1].tool_calls.as_ref().unwrap()[0];
        assert!(call.get("thoughtSignature").is_none());
        assert_eq!(
            call["id"],
            projected.messages[2].tool_call_id.as_deref().unwrap()
        );
        assert!(projected.source_gemini_request.is_none());
        assert_eq!(request_body(&gemini_req_to_oai_req(&body)).unwrap(), body);
    }

    #[test]
    fn foreign_projection_refuses_silent_tool_choice_and_media_changes() {
        for patch in [
            json!({"tools":[{"googleSearch":{}}]}),
            json!({"generationConfig":{"responseMimeType":"application/json"}}),
            json!({"toolConfig":{"functionCallingConfig":{"mode":"ANY","allowedFunctionNames":["one","two"]}}}),
            json!({"contents":[{"parts":[{"inlineData":{"mimeType":"audio/wav","data":"fixture"}}]}]}),
        ] {
            let mut body = json!({"contents":[{"role":"user","parts":[{"text":"fixture"}]}]});
            body.as_object_mut()
                .unwrap()
                .extend(patch.as_object().unwrap().clone());
            assert_eq!(
                request_for_foreign_hop(&gemini_req_to_oai_req(&body))
                    .unwrap_err()
                    .code,
                "gemini_feature_unsupported"
            );
        }
    }

    #[tokio::test]
    async fn native_gemini_response_and_events_preserve_signatures_and_opaque_parts() {
        let body = json!({
            "candidates":[{"content":{"role":"model","parts":[
                {"text":"fixture","thoughtSignature":"text_signature"},
                {"functionCall":{"name":"lookup","args":{}},"thoughtSignature":"function_signature"},
                {"inlineData":{"mimeType":"image/png","data":"fixture"}},
            ]},"finishReason":"STOP","groundingMetadata":{"fixture":true}}],
            "usageMetadata":{"promptTokenCount":1,"candidatesTokenCount":2,"totalTokenCount":3},
            "modelVersion":"native-model","nativeExtension":{"preserve":true}
        });
        let canonical = gemini_resp_to_oai_resp(&body, "requested-model");
        assert_eq!(oai_resp_to_gemini_resp(&canonical, "requested-model"), body);
        let opaque = json!({"nativeSignatureChunk":{"preserve":true}});
        let frames = futures_util::stream::iter(vec![opaque.to_string(), body.to_string()]);
        let projected = gemini_stream_to_oai_chunks(frames, "native-model".into());
        let restored: Vec<_> = oai_chunks_to_gemini_stream(projected, "native-model".into())
            .collect()
            .await;
        assert_eq!(restored.len(), 2);
        assert_eq!(serde_json::from_str::<Value>(&restored[0]).unwrap(), opaque);
        assert_eq!(serde_json::from_str::<Value>(&restored[1]).unwrap(), body);
        assert!(serde_json::to_value(&canonical)
            .unwrap()
            .get("source_gemini_response")
            .is_none());
    }

    #[tokio::test]
    async fn foreign_clients_refuse_native_only_gemini_response_parts() {
        let body = json!({"candidates":[{"content":{"parts":[{"inlineData":{"mimeType":"image/png","data":"fixture"}}]}}]});
        assert_eq!(
            validate_response_for_foreign(&body).unwrap_err().code,
            "gemini_response_unsupported"
        );
        let chunks: Vec<_> = gemini_stream_to_oai_chunks_for_client(
            futures_util::stream::iter(vec![body.to_string()]),
            "model".into(),
            false,
        )
        .collect()
        .await;
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].extra.get("error").is_some());
        assert!(chunks[0].source_gemini_event.is_none());
    }

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
        // The tool result carries the generated call id, not the bare
        // function name, so foreign upstreams can correlate it.
        let call_id = req.messages[2].tool_calls.as_ref().unwrap()[0]
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string);
        assert_eq!(req.messages[3].tool_call_id, call_id);
        assert_eq!(req.temperature, Some(0.5));
        assert_eq!(req.max_tokens, Some(1024));
        assert_eq!(req.extra.get("top_p"), Some(&json!(0.9)));
        let tools = req.tools.unwrap();
        assert_eq!(
            tools[0].pointer("/function/name"),
            Some(&json!("get_weather"))
        );
        // Foreign schemas retain constraints; only outbound Gemini schema
        // normalization strips fields unsupported by the native API.
        let params = tools[0].pointer("/function/parameters").unwrap();
        assert_eq!(
            params.pointer("/properties/city/$schema"),
            Some(&json!("x"))
        );
        assert_eq!(
            params.pointer("/properties/city/additionalProperties"),
            Some(&json!(false))
        );
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
    fn gemini_tool_results_correlate_with_their_call_ids() {
        let body = json!({
            "contents": [
                { "role": "user", "parts": [{ "text": "weather?" }] },
                { "role": "model", "parts": [
                    { "functionCall": { "name": "get_weather", "args": { "city": "sf" } } },
                    { "functionCall": { "name": "get_weather", "args": { "city": "nyc" } } },
                ]},
                { "role": "user", "parts": [
                    { "functionResponse": { "name": "get_weather", "response": { "temp": 18 } } },
                    { "functionResponse": { "name": "get_weather", "response": { "temp": 20 } } },
                    { "functionResponse": { "name": "unknown_fn", "response": { "x": 1 } } },
                ]},
            ],
        });
        let req = gemini_req_to_oai_req(&body);
        assert_eq!(req.messages[1].role, "assistant");
        let calls = req.messages[1].tool_calls.as_ref().unwrap();
        assert_eq!(calls.len(), 2);
        let id0 = calls[0]
            .get("id")
            .and_then(Value::as_str)
            .unwrap()
            .to_string();
        let id1 = calls[1]
            .get("id")
            .and_then(Value::as_str)
            .unwrap()
            .to_string();
        assert_ne!(id0, id1);
        // Same-name responses correlate in call order; a response with no
        // matching call in this request keeps the bare name.
        assert_eq!(req.messages[2].tool_call_id.as_deref(), Some(id0.as_str()));
        assert_eq!(req.messages[3].tool_call_id.as_deref(), Some(id1.as_str()));
        assert_eq!(req.messages[4].tool_call_id.as_deref(), Some("unknown_fn"));

        // Round trip back to Gemini: names resolve from the correlated ids.
        let back = oai_req_to_gemini_req(&req);
        assert_eq!(
            back.contents[2].parts.pointer("/0/functionResponse/name"),
            Some(&json!("get_weather"))
        );
        assert_eq!(
            back.contents[3]
                .parts
                .pointer("/0/functionResponse/response"),
            Some(&json!({ "temp": 20 }))
        );
    }

    #[tokio::test]
    async fn stream_tool_call_indices_are_stable_across_frames() {
        use futures_util::stream;
        let upstream = stream::iter(vec![
            json!({
                "candidates": [{ "content": { "role": "model", "parts": [
                    { "functionCall": { "name": "first", "args": { "a": 1 } } },
                ]}, "index": 0 }]
            })
            .to_string(),
            json!({
                "candidates": [{ "content": { "role": "model", "parts": [
                    { "functionCall": { "name": "second", "args": { "b": 2 } } },
                ]}, "finishReason": "STOP", "index": 0 }]
            })
            .to_string(),
        ]);
        let chunks: Vec<OAIStreamChunk> = gemini_stream_to_oai_chunks(upstream, "m".into())
            .collect()
            .await;
        assert_eq!(chunks.len(), 2);
        let first = &chunks[0].choices[0].delta.tool_calls.as_ref().unwrap()[0];
        let second = &chunks[1].choices[0].delta.tool_calls.as_ref().unwrap()[0];
        assert_eq!(first.get("index"), Some(&json!(0)));
        assert_eq!(second.get("index"), Some(&json!(1)));
        assert_eq!(second.pointer("/function/name"), Some(&json!("second")));
    }

    fn tool_delta_chunk(
        index: u64,
        name: Option<&str>,
        arguments: &str,
        finish: Option<&str>,
    ) -> OAIStreamChunk {
        let mut call = json!({ "index": index, "function": { "arguments": arguments } });
        if let Some(name) = name {
            call["id"] = json!(format!("call_{index}"));
            call["type"] = json!("function");
            call["function"]["name"] = json!(name);
        }
        OAIStreamChunk {
            id: "t".into(),
            object: "chat.completion.chunk".into(),
            created: 0,
            model: "m".into(),
            choices: vec![StreamChoice {
                index: 0,
                delta: StreamDelta {
                    tool_calls: Some(vec![call]),
                    ..Default::default()
                },
                finish_reason: finish.map(str::to_string),
                extra: Default::default(),
            }],
            extra: Default::default(),
            source_anthropic_event: None,
            source_responses_event: None,
            source_gemini_event: None,
        }
    }

    async fn gemini_frames(chunks: Vec<OAIStreamChunk>) -> Vec<Value> {
        use futures_util::stream;
        oai_chunks_to_gemini_stream(Box::pin(stream::iter(chunks)), "m".into())
            .filter_map(|frame| async move { serde_json::from_str(&frame).ok() })
            .collect()
            .await
    }

    fn function_call_parts(frames: &[Value]) -> Vec<Value> {
        frames
            .iter()
            .flat_map(|frame| {
                frame
                    .pointer("/candidates/0/content/parts")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
            })
            .filter(|part| part.get("functionCall").is_some())
            .collect()
    }

    #[tokio::test]
    async fn interleaved_tool_deltas_flush_complete_calls_once() {
        let frames = gemini_frames(vec![
            tool_delta_chunk(0, Some("first"), "{\"x\":", None),
            tool_delta_chunk(1, Some("second"), "{\"y\":", None),
            tool_delta_chunk(0, None, "1}", None),
            tool_delta_chunk(1, None, "2}", Some("tool_calls")),
        ])
        .await;
        let calls = function_call_parts(&frames);
        assert_eq!(calls.len(), 2);
        assert_eq!(
            calls[0].pointer("/functionCall/name"),
            Some(&json!("first"))
        );
        assert_eq!(
            calls[0].pointer("/functionCall/args"),
            Some(&json!({ "x": 1 }))
        );
        assert_eq!(
            calls[1].pointer("/functionCall/name"),
            Some(&json!("second"))
        );
        assert_eq!(
            calls[1].pointer("/functionCall/args"),
            Some(&json!({ "y": 2 }))
        );
    }

    #[tokio::test]
    async fn tool_index_gaps_never_emit_empty_placeholder_calls() {
        let frames = gemini_frames(vec![
            tool_delta_chunk(2, Some("only"), "{\"a\":1}", None),
            tool_delta_chunk(2, None, "", Some("tool_calls")),
        ])
        .await;
        let calls = function_call_parts(&frames);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].pointer("/functionCall/name"), Some(&json!("only")));
    }

    #[tokio::test]
    async fn nameless_tool_call_surfaces_stream_error_instead_of_invalid_call() {
        let frames = gemini_frames(vec![
            tool_delta_chunk(0, None, "{}", None),
            tool_delta_chunk(0, None, "", Some("tool_calls")),
        ])
        .await;
        assert!(function_call_parts(&frames).is_empty());
        assert!(frames.iter().any(|frame| frame.get("error").is_some()));
    }

    #[tokio::test]
    async fn unterminated_tool_stream_flushes_without_fabricated_finish() {
        let frames = gemini_frames(vec![tool_delta_chunk(0, Some("f"), "{\"a\":1}", None)]).await;
        let calls = function_call_parts(&frames);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].pointer("/functionCall/name"), Some(&json!("f")));
        assert!(frames
            .iter()
            .all(|frame| frame.pointer("/candidates/0/finishReason").is_none()));
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

    /// The OpenAI `stream_options.include_usage` terminal chunk carries
    /// `"choices": []` with a trailing usage payload — the Gemini surface
    /// must relay it as a metadata-only frame instead of dropping the usage
    /// (P3 regression).
    #[tokio::test]
    async fn gemini_stream_surfaces_usage_only_terminal_chunk() {
        let mut extra = HashMap::new();
        extra.insert(
            "usage".to_string(),
            json!({ "prompt_tokens": 3, "completion_tokens": 2, "total_tokens": 5 }),
        );
        let usage_only = OAIStreamChunk {
            id: "m".into(),
            object: "chat.completion.chunk".into(),
            created: 0,
            model: "m".into(),
            choices: vec![],
            extra,
            source_anthropic_event: None,
            source_responses_event: None,
            source_gemini_event: None,
        };
        let frames = gemini_frames(vec![usage_only]).await;
        assert_eq!(frames.len(), 1);
        assert!(frames[0].get("candidates").is_none());
        assert_eq!(
            frames[0].pointer("/usageMetadata/promptTokenCount"),
            Some(&json!(3))
        );
        assert_eq!(
            frames[0].pointer("/usageMetadata/candidatesTokenCount"),
            Some(&json!(2))
        );
        assert_eq!(
            frames[0].pointer("/usageMetadata/totalTokenCount"),
            Some(&json!(5))
        );
    }
}
