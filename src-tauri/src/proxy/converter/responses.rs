//! Conversion between the OpenAI Responses API (the Codex `/v1/responses`
//! surface) and the canonical Chat Completions format: request, response,
//! and streaming. Extracted from the former converter.rs monolith.

use super::anthropic::rand_hex;
use super::responses_namespace::Responses_namespace_map;
use crate::proxy::canonical::{
    ChatCompletionRequest, ChatMessage, OAIChatResponse, OAIChoice, OAIMessage, OAIStreamChunk,
    ProviderError, Usage,
};
use crate::proxy::responses::{ResponsesRequest, ResponsesStreamEvent};
use futures_core::stream::Stream;
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::pin::Pin;

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
    // The canonical view may flatten supported namespaces for planning, while
    // the source sidecar always retains the unmodified native request.
    let namespace_map = Responses_namespace_map::from_request(req).ok();
    let normalized = namespace_map
        .as_ref()
        .and_then(|map| map.normalize_request(req).ok());
    let mut canonical = responses_to_chat_flat(normalized.as_deref().unwrap_or(req));
    canonical.source_responses_request = Some(req.clone());
    canonical
}

fn responses_to_chat_flat(req: &ResponsesRequest) -> ChatCompletionRequest {
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
        source_gemini_request: None,
        source_responses_request: None,
        request_id: None,
        client_credential: None,
    }
}

/// Convert a Responses request only when its source state and requested
/// features can be represented by a stateless Chat-shaped provider. Native
/// Responses hops should use the original request sidecar instead; the
/// cross-protocol guard is also exposed separately for router failover checks.
pub fn try_responses_to_chat(
    req: &ResponsesRequest,
) -> Result<ChatCompletionRequest, ProviderError> {
    let namespace_map = Responses_namespace_map::from_request(req)?;
    let normalized = namespace_map.normalize_request(req)?;
    let mut canonical = try_responses_to_chat_flat(&normalized)?;
    canonical.source_responses_request = Some(req.clone());
    Ok(canonical)
}

fn try_responses_to_chat_flat(
    req: &ResponsesRequest,
) -> Result<ChatCompletionRequest, ProviderError> {
    validate_responses_request_for_chat_flat(req)?;
    let mut canonical = responses_to_chat_flat(req);
    let mut messages = Vec::new();
    if let Some(instructions) = &req.instructions {
        if !instructions.is_empty() {
            messages.push(ChatMessage {
                role: "system".into(),
                content: Value::String(instructions.clone()),
                tool_calls: None,
                tool_call_id: None,
                extra: Default::default(),
            });
        }
    }
    match &req.input {
        Value::String(text) => messages.push(ChatMessage {
            role: "user".into(),
            content: Value::String(text.clone()),
            tool_calls: None,
            tool_call_id: None,
            extra: Default::default(),
        }),
        Value::Array(items) => {
            for item in items {
                let message = responses_input_item_to_chat_strict(item)?;
                append_responses_input_message(&mut messages, message);
            }
        }
        _ => return Err(responses_conversion_error("responses_invalid_request")),
    }
    canonical.messages = messages;
    canonical.extra = req.extra.clone();
    canonical.extra.remove("tool_choice");
    // Responses effort is nested; Chat Completions accepts only the flat
    // reasoning_effort field. The source sidecar remains unchanged for native
    // Responses hops.
    if let Some(reasoning) = canonical
        .extra
        .remove("reasoning")
        .filter(|value| !value.is_null())
    {
        let reasoning = reasoning
            .as_object()
            .ok_or_else(|| responses_conversion_error("responses_invalid_request"))?;
        if reasoning
            .keys()
            .any(|key| key != "effort" && key != "summary")
        {
            return Err(responses_conversion_error("responses_feature_unsupported"));
        }
        if let Some(effort) = reasoning.get("effort") {
            if (!effort.is_string() && !effort.is_null())
                || canonical.extra.contains_key("reasoning_effort")
            {
                return Err(responses_conversion_error("responses_invalid_request"));
            }
            canonical
                .extra
                .insert("reasoning_effort".into(), effort.clone());
        }
        if reasoning
            .get("summary")
            .is_some_and(|value| !value.is_null() && value.as_str() != Some("none"))
        {
            return Err(responses_conversion_error("responses_feature_unsupported"));
        }
    }
    // Codex requests this output projection even on a fresh stateless turn.
    // A converted Chat response has no encrypted Responses reasoning to return.
    if let Some(Value::Array(include)) = canonical.extra.get_mut("include") {
        include.retain(|value| value.as_str() != Some("reasoning.encrypted_content"));
        if include.is_empty() {
            canonical.extra.remove("include");
        }
    }
    if let Some(metadata) = &req.metadata {
        canonical.extra.insert("metadata".into(), metadata.clone());
    }
    Ok(canonical)
}

/// Validate fields whose meaning would be lost if a Responses request were
/// routed to a Chat/Anthropic/Gemini upstream. The native Responses provider
/// must not call this: its source request sidecar preserves those fields.
pub fn validate_responses_request_for_chat(req: &ResponsesRequest) -> Result<(), ProviderError> {
    let namespace_map = Responses_namespace_map::from_request(req)?;
    let normalized = namespace_map.normalize_request(req)?;
    validate_responses_request_for_chat_flat(&normalized)
}

fn validate_responses_request_for_chat_flat(req: &ResponsesRequest) -> Result<(), ProviderError> {
    if req.previous_response_id.is_some() {
        return Err(responses_conversion_error("responses_state_unsupported"));
    }
    if !req.input.is_string() && !req.input.is_array() {
        return Err(responses_conversion_error("responses_invalid_request"));
    }
    if let Some(items) = req.input.as_array() {
        for item in items {
            let item_type = item
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("message");
            match item_type {
                "message" => {
                    let role = item.get("role").and_then(Value::as_str).unwrap_or("user");
                    if !matches!(role, "user" | "assistant" | "developer" | "system") {
                        return Err(responses_conversion_error("responses_feature_unsupported"));
                    }
                    validate_responses_content_for_chat(
                        role,
                        item.get("content").unwrap_or(&Value::Null),
                    )?;
                }
                "function_call" => {
                    if item
                        .get("call_id")
                        .and_then(Value::as_str)
                        .is_none_or(str::is_empty)
                        || item
                            .get("name")
                            .and_then(Value::as_str)
                            .is_none_or(str::is_empty)
                    {
                        return Err(responses_conversion_error("responses_invalid_request"));
                    }
                    if let Some(status) = item.get("status").and_then(Value::as_str) {
                        if status != "completed" {
                            return Err(responses_conversion_error("responses_state_unsupported"));
                        }
                    }
                }
                "function_call_output" => {
                    if item
                        .get("call_id")
                        .or_else(|| item.get("id"))
                        .and_then(Value::as_str)
                        .is_none_or(str::is_empty)
                    {
                        return Err(responses_conversion_error("responses_invalid_request"));
                    }
                    validate_function_output_for_chat(item.get("output").unwrap_or(&Value::Null))?;
                }
                // Reasoning (including encrypted_content), item references,
                // computer/file/web-search calls and unknown future items are
                // upstream-owned state, not Chat messages.
                _ => return Err(responses_conversion_error("responses_state_unsupported")),
            }
        }
    }
    if let Some(tools) = &req.tools {
        let tools = match tools {
            Value::Array(items) => items.iter().collect::<Vec<_>>(),
            other => vec![other],
        };
        for tool in tools {
            if tool.get("type").and_then(Value::as_str) != Some("function")
                || tool
                    .get("name")
                    .and_then(Value::as_str)
                    .is_none_or(str::is_empty)
            {
                return Err(responses_conversion_error("responses_state_unsupported"));
            }
        }
    }
    if let Some(choice) = req.extra.get("tool_choice") {
        if responses_tool_choice_to_chat(&req.extra).is_none()
            || (choice.is_object()
                && choice.get("type").and_then(Value::as_str) != Some("function"))
        {
            return Err(responses_conversion_error("responses_feature_unsupported"));
        }
    }
    if req
        .extra
        .get("include")
        .and_then(Value::as_array)
        .is_some_and(|items| {
            items.iter().any(|item| {
                item.as_str().is_some_and(|name| {
                    name.contains("encrypted_content") && name != "reasoning.encrypted_content"
                })
            })
        })
    {
        return Err(responses_conversion_error("responses_state_unsupported"));
    }
    if req.extra.get("background").and_then(Value::as_bool) == Some(true) {
        return Err(responses_conversion_error("responses_state_unsupported"));
    }
    Ok(())
}

/// Build the conversion-side `ProviderError` with the stable code and a
/// non-retriable 400 classification; the HTTP edge maps this onto its public
/// error surface without the upstream payload.
fn responses_conversion_error(code: &str) -> ProviderError {
    crate::proxy::canonical::make_provider_error(
        code,
        "request_conversion",
        Some(400),
        Some(false),
        None,
    )
}

fn validate_responses_content_for_chat(role: &str, content: &Value) -> Result<(), ProviderError> {
    match content {
        Value::Null | Value::String(_) => Ok(()),
        Value::Array(parts) => {
            for part in parts {
                match part.get("type").and_then(Value::as_str).unwrap_or("") {
                    "input_text" | "output_text" | "text" => {
                        if part.get("text").and_then(Value::as_str).is_none() {
                            return Err(responses_conversion_error("responses_invalid_request"));
                        }
                    }
                    "input_image" if role == "user" => {
                        let image_url = part.get("image_url");
                        let url = image_url.and_then(Value::as_str).or_else(|| {
                            image_url
                                .and_then(|value| value.get("url"))
                                .and_then(Value::as_str)
                        });
                        if url.is_none_or(str::is_empty) || part.get("file_id").is_some() {
                            return Err(responses_conversion_error(
                                "responses_feature_unsupported",
                            ));
                        }
                    }
                    _ => return Err(responses_conversion_error("responses_feature_unsupported")),
                }
            }
            Ok(())
        }
        _ => Err(responses_conversion_error("responses_invalid_request")),
    }
}

fn validate_function_output_for_chat(output: &Value) -> Result<(), ProviderError> {
    match output {
        Value::Null | Value::String(_) | Value::Object(_) | Value::Number(_) | Value::Bool(_) => {
            Ok(())
        }
        Value::Array(parts) => {
            if parts.iter().all(|part| {
                matches!(
                    part.get("type").and_then(Value::as_str),
                    Some("input_text" | "output_text" | "text")
                ) && part.get("text").and_then(Value::as_str).is_some()
            }) {
                Ok(())
            } else {
                Err(responses_conversion_error("responses_feature_unsupported"))
            }
        }
    }
}

fn responses_input_item_to_chat_strict(item: &Value) -> Result<ChatMessage, ProviderError> {
    match item
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("message")
    {
        "function_call" => {
            let arguments = match item.get("arguments") {
                Some(Value::String(value)) => value.clone(),
                Some(value @ Value::Object(_)) | Some(value @ Value::Array(_)) => value.to_string(),
                Some(Value::Null) | None => String::new(),
                _ => return Err(responses_conversion_error("responses_invalid_request")),
            };
            let call_id = item
                .get("call_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let name = item.get("name").and_then(Value::as_str).unwrap_or_default();
            Ok(ChatMessage {
                role: "assistant".into(),
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
            let output = item.get("output").unwrap_or(&Value::Null);
            let content = match output {
                Value::String(value) => Value::String(value.clone()),
                Value::Array(parts) => Value::Array(
                    parts
                        .iter()
                        .map(|part| json!({"type":"text", "text":part["text"]}))
                        .collect(),
                ),
                Value::Null => Value::String(String::new()),
                other => Value::String(other.to_string()),
            };
            Ok(ChatMessage {
                role: "tool".into(),
                content,
                tool_calls: None,
                tool_call_id: item
                    .get("call_id")
                    .or_else(|| item.get("id"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                extra: Default::default(),
            })
        }
        "message" => {
            let role = item.get("role").and_then(Value::as_str).unwrap_or("user");
            let role = if role == "developer" { "system" } else { role };
            let content =
                strict_input_content_to_chat(item.get("content").unwrap_or(&Value::Null), role)?;
            Ok(ChatMessage {
                role: role.to_owned(),
                content,
                tool_calls: None,
                tool_call_id: None,
                extra: Default::default(),
            })
        }
        _ => Err(responses_conversion_error("responses_state_unsupported")),
    }
}

fn strict_input_content_to_chat(content: &Value, role: &str) -> Result<Value, ProviderError> {
    match content {
        Value::String(value) => Ok(Value::String(value.clone())),
        Value::Null => Ok(Value::String(String::new())),
        Value::Array(parts) => {
            let mut output = Vec::with_capacity(parts.len());
            for part in parts {
                match part.get("type").and_then(Value::as_str).unwrap_or("") {
                    "input_text" | "output_text" | "text" => {
                        output.push(json!({"type":"text", "text":part["text"]}));
                    }
                    "input_image" if role == "user" => {
                        let image = part.get("image_url").unwrap_or(&Value::Null);
                        let url = image
                            .as_str()
                            .or_else(|| image.get("url").and_then(Value::as_str));
                        let mut image_url = json!({"url": url.unwrap_or_default()});
                        if let Some(detail) = part.get("detail") {
                            image_url["detail"] = detail.clone();
                        } else if let Some(detail) = image.get("detail") {
                            image_url["detail"] = detail.clone();
                        }
                        output.push(json!({"type":"image_url", "image_url":image_url}));
                    }
                    _ => return Err(responses_conversion_error("responses_feature_unsupported")),
                }
            }
            if output.is_empty() {
                Ok(Value::String(String::new()))
            } else {
                Ok(Value::Array(output))
            }
        }
        _ => Err(responses_conversion_error("responses_invalid_request")),
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
        if obj.get("function").is_some_and(Value::is_object) {
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
    // A native Responses response is opaque source state: IDs, previous-
    // response linkage, encrypted reasoning and built-in-tool outputs must
    // never be reconstructed through the lossy Chat shape.
    if let Some(source) = &resp.source_responses_response {
        return source.clone();
    }

    let model = if resp.model.is_empty() {
        requested_model.to_string()
    } else {
        resp.model.clone()
    };

    let mut output: Vec<Value> = Vec::new();
    for choice in &resp.choices {
        // DeepSeek/Kimi-style reasoning arrives as a message extension. The
        // streaming path surfaces it as a reasoning output item; the
        // non-streaming envelope must not silently drop it — Codex shows
        // model thinking in both modes.
        if let Some(reasoning) = choice
            .message
            .extra
            .get("reasoning_content")
            .and_then(Value::as_str)
            .or_else(|| {
                choice
                    .message
                    .extra
                    .get("reasoning")
                    .and_then(Value::as_str)
            })
            .filter(|text| !text.is_empty())
        {
            output.push(json!({
                "type": "reasoning",
                "id": format!("rs_{}", rand_hex(12)),
                "summary": [],
                "content": [{
                    "type": "reasoning_text",
                    "text": reasoning,
                }],
                "status": "completed",
            }));
        }
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

/// Restore namespace identity only on a synthesized Responses envelope.
pub fn chat_to_responses_with_namespaces(
    resp: &OAIChatResponse,
    requested_model: &str,
    namespace_map: Option<&Responses_namespace_map>,
) -> Value {
    let mut response = chat_to_responses(resp, requested_model);
    if resp.source_responses_response.is_none() {
        if let Some(namespace_map) = namespace_map {
            namespace_map.restore_response(&mut response);
        }
    }
    response
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
    responses_stream_to_sse(stream, requested_model)
}

pub fn chat_chunks_to_responses_stream_with_namespaces(
    stream: Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>>,
    requested_model: &str,
    namespace_map: Option<Responses_namespace_map>,
) -> Pin<Box<dyn Stream<Item = String> + Send>> {
    responses_stream_to_sse_with_namespaces(stream, requested_model, namespace_map)
}

/// Convert a canonical stream to the Responses wire. When the stream carries
/// native Responses sidecar frames, replay their event name, id and data
/// instead of reconstructing them through Chat Completions. Otherwise retain
/// the existing canonical-to-Responses event conversion.
pub fn responses_stream_to_sse(
    stream: Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>>,
    requested_model: &str,
) -> Pin<Box<dyn Stream<Item = String> + Send>> {
    responses_stream_to_sse_with_namespaces(stream, requested_model, None)
}

fn responses_stream_to_sse_with_namespaces(
    stream: Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>>,
    requested_model: &str,
    namespace_map: Option<Responses_namespace_map>,
) -> Pin<Box<dyn Stream<Item = String> + Send>> {
    use async_stream::stream;
    let mut input = stream;
    let model = requested_model.to_string();
    let output = stream! {
        let first = input.next().await;
        let native = first
            .as_ref()
            .is_some_and(|chunk| chunk.source_responses_event.is_some());
        if native {
            let frames = futures_util::stream::iter(first).chain(input);
            let mut frames = std::pin::pin!(frames);
            let mut terminal_seen = false;
            while let Some(chunk) = frames.next().await {
                if let Some(source_event) = chunk.source_responses_event {
                    let terminal = source_event.is_terminal();
                    terminal_seen |= terminal;
                    yield source_event.to_sse();
                    // The surface contract terminates streams with
                    // `data: [DONE]` after a successful terminal frame;
                    // failed/cancelled/error terminals end without the
                    // success sentinel (same rule as the synthesized path).
                    if terminal && source_event.is_success_terminal() {
                        yield crate::proxy::responses::STREAM_DONE.to_string();
                    }
                    continue;
                }

                // A router-generated error has no upstream envelope to relay.
                // Reuse the established safe `response.failed` conversion and
                // do not invent a success terminator after it.
                let error = if chunk.extra.contains_key("error") {
                    chunk
                } else {
                    crate::proxy::canonical::stream_error_chunk(
                        &model,
                        502,
                        "proxy_stream_error",
                    )
                };
                let failure = chat_chunks_to_responses_stream_generated(
                    Box::pin(futures_util::stream::iter([error])),
                    &model,
                );
                let mut failure = std::pin::pin!(failure);
                while let Some(frame) = failure.next().await {
                    yield frame;
                }
                return;
            }
            if !terminal_seen {
                let error = crate::proxy::canonical::stream_error_chunk(
                    &model,
                    502,
                    "proxy_stream_error",
                );
                let failure = chat_chunks_to_responses_stream_generated(
                    Box::pin(futures_util::stream::iter([error])),
                    &model,
                );
                let mut failure = std::pin::pin!(failure);
                while let Some(frame) = failure.next().await {
                    yield frame;
                }
            }
        } else {
            let joined = futures_util::stream::iter(first).chain(input);
            let canonical = chat_chunks_to_responses_stream_generated(
                Box::pin(joined),
                &model,
            );
            let mut canonical = std::pin::pin!(canonical);
            while let Some(frame) = canonical.next().await {
                yield match &namespace_map {
                    Some(namespace_map) => namespace_map.restore_generated_frame(frame),
                    None => frame,
                };
            }
        }
    };
    Box::pin(output)
}

fn chat_chunks_to_responses_stream_generated(
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

/// Construct an OpenAI Responses wire request from the canonical Chat shape.
/// This is the opposite direction from `chat_to_responses`, which is only a
/// client-facing response converter.
pub fn canonical_to_responses_request_value(
    req: &ChatCompletionRequest,
    upstream_model: &str,
    stream: bool,
) -> Result<Value, ProviderError> {
    let mut input = Vec::new();
    for message in &req.messages {
        match message.role.as_str() {
            "tool" => {
                let call_id = message
                    .tool_call_id
                    .as_deref()
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| responses_conversion_error("responses_invalid_request"))?;
                input.push(json!({
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": message.content,
                }));
            }
            "system" | "developer" | "user" | "assistant" => {
                if let Some(content) = chat_content_to_responses(&message.content, &message.role)? {
                    input.push(json!({
                        "type": "message",
                        "role": message.role,
                        "content": content,
                    }));
                }
                if let Some(calls) = &message.tool_calls {
                    if message.role != "assistant" {
                        return Err(responses_conversion_error("responses_feature_unsupported"));
                    }
                    for call in calls {
                        input.push(chat_tool_call_to_responses(call)?);
                    }
                }
                if !message.extra.is_empty() {
                    return Err(responses_conversion_error("responses_feature_unsupported"));
                }
            }
            _ => return Err(responses_conversion_error("responses_feature_unsupported")),
        }
    }

    let mut body = serde_json::Map::new();
    body.insert(
        "model".into(),
        Value::String(crate::proxy::strip_one_m_suffix(upstream_model)),
    );
    body.insert("input".into(), Value::Array(input));
    body.insert("stream".into(), Value::Bool(stream));
    if let Some(temperature) = req.temperature {
        body.insert("temperature".into(), json!(temperature));
    }
    if let Some(max_tokens) = req.max_tokens {
        body.insert("max_output_tokens".into(), json!(max_tokens));
    }
    if let Some(tools) = &req.tools {
        let tools = tools
            .iter()
            .map(chat_tool_to_responses)
            .collect::<Result<Vec<_>, _>>()?;
        body.insert("tools".into(), Value::Array(tools));
    }
    if let Some(choice) = &req.tool_choice {
        body.insert("tool_choice".into(), chat_tool_choice_to_responses(choice)?);
    }

    for (key, value) in &req.extra {
        match key.as_str() {
            "model" | "stream" | "tool_choice" => {}
            "stream_options" => {
                // Responses always returns usage in its final response, so the
                // Chat include_usage=true request is already satisfied. Do not
                // pretend to support other Chat-only stream options.
                let equivalent = value.as_object().is_some_and(|options| {
                    options.len() == 1 && options.get("include_usage") == Some(&Value::Bool(true))
                });
                if !equivalent {
                    return Err(responses_conversion_error("responses_feature_unsupported"));
                }
            }
            "reasoning_effort" => {
                let reasoning: &mut serde_json::Value =
                    body.entry("reasoning").or_insert_with(|| json!({}));
                if !reasoning.is_object() {
                    return Err(responses_conversion_error("responses_feature_unsupported"));
                }
                reasoning["effort"] = value.clone();
            }
            "response_format" => {
                if body.contains_key("text") {
                    return Err(responses_conversion_error("responses_feature_unsupported"));
                }
                body.insert(
                    "text".into(),
                    json!({"format": chat_response_format_to_responses(value)?}),
                );
            }
            "max_completion_tokens" | "max_output_tokens" => {
                if body.contains_key("max_output_tokens") {
                    return Err(responses_conversion_error("responses_invalid_request"));
                }
                body.insert("max_output_tokens".into(), value.clone());
            }
            "n" => {
                if value.as_u64() != Some(1) {
                    return Err(responses_conversion_error("responses_feature_unsupported"));
                }
            }
            "top_p"
            | "user"
            | "metadata"
            | "store"
            | "background"
            | "service_tier"
            | "parallel_tool_calls"
            | "reasoning"
            | "text"
            | "truncation"
            | "include"
            | "prompt_cache_key"
            | "safety_identifier"
            | "max_tool_calls"
            | "instructions" => {
                if body.insert(key.clone(), value.clone()).is_some() {
                    return Err(responses_conversion_error("responses_invalid_request"));
                }
            }
            // These Chat-only controls have no Responses equivalent. Refuse
            // instead of returning a successful request with changed meaning.
            _ => return Err(responses_conversion_error("responses_feature_unsupported")),
        }
    }
    Ok(Value::Object(body))
}

/// Build the exact HTTP body for a Responses upstream. A native Responses
/// request is re-used as the source of truth and only the routed model and
/// stream flag are overlaid; a Chat-originating request is explicitly mapped.
pub fn responses_request_body(
    req: &ChatCompletionRequest,
    upstream_model: &str,
    stream: bool,
) -> Result<Vec<u8>, ProviderError> {
    responses_request_body_with_model(req, upstream_model, stream).map(|(body, _)| body)
}

/// Return the wire model from the same body that is serialized, so private
/// routing observations cannot drift from native envelope overlays.
pub fn responses_request_body_with_model(
    req: &ChatCompletionRequest,
    upstream_model: &str,
    stream: bool,
) -> Result<(Vec<u8>, String), ProviderError> {
    let body = if let Some(source) = &req.source_responses_request {
        let mut source = source.clone();
        let baseline = responses_to_chat(&source);
        if serde_json::to_value(&req.messages).ok() != serde_json::to_value(&baseline.messages).ok()
        {
            let prefix_len = req.messages.len().saturating_sub(baseline.messages.len());
            let prefix_matches = req.messages.len() >= baseline.messages.len()
                && serde_json::to_value(&req.messages[prefix_len..]).ok()
                    == serde_json::to_value(&baseline.messages).ok()
                && req.messages[..prefix_len]
                    .iter()
                    .all(|message| message.role == "system");
            if !prefix_matches {
                return Err(responses_conversion_error("responses_state_unsupported"));
            }
            let injected = req.messages[..prefix_len]
                .iter()
                .map(|message| match &message.content {
                    Value::String(text) => Ok(text.clone()),
                    _ => Err(responses_conversion_error("responses_state_unsupported")),
                })
                .collect::<Result<Vec<_>, _>>()?
                .join("\n\n");
            if !injected.is_empty() {
                source.instructions = Some(match source.instructions.take() {
                    Some(existing) if !existing.is_empty() => format!("{injected}\n\n{existing}"),
                    _ => injected,
                });
            }
        }
        source.model = crate::proxy::strip_one_m_suffix(upstream_model);
        source.stream = stream;
        if req.temperature != baseline.temperature {
            source.temperature = req.temperature;
        }
        if req.max_tokens != baseline.max_tokens {
            source.max_output_tokens = req.max_tokens;
        }
        serde_json::to_value(source)
            .map_err(|_| responses_conversion_error("responses_invalid_request"))?
    } else {
        canonical_to_responses_request_value(req, upstream_model, stream)?
    };
    let wire_model = body.get("model").and_then(Value::as_str)
        .ok_or_else(|| responses_conversion_error("responses_invalid_request"))?.to_owned();
    let encoded = serde_json::to_vec(&body).map_err(|_| {
        crate::proxy::canonical::make_provider_error(
            "request_serialize_error",
            "request_serialize",
            None,
            Some(false),
            None,
        )
    })?;
    Ok((encoded, wire_model))
}

fn chat_content_to_responses(content: &Value, role: &str) -> Result<Option<Value>, ProviderError> {
    match content {
        Value::Null => Ok(None),
        Value::String(text) => Ok(Some(Value::String(text.clone()))),
        Value::Array(parts) => {
            let mut output = Vec::with_capacity(parts.len());
            for part in parts {
                match part.get("type").and_then(Value::as_str).unwrap_or("") {
                    "text" => {
                        let text = part.get("text").and_then(Value::as_str).ok_or_else(|| {
                            responses_conversion_error("responses_invalid_request")
                        })?;
                        output.push(json!({
                            "type": if role == "assistant" { "output_text" } else { "input_text" },
                            "text": text,
                        }));
                    }
                    "image_url" if role == "user" => {
                        let image = part.get("image_url").unwrap_or(&Value::Null);
                        let url = image
                            .as_str()
                            .or_else(|| image.get("url").and_then(Value::as_str))
                            .filter(|value| !value.is_empty())
                            .ok_or_else(|| {
                                responses_conversion_error("responses_feature_unsupported")
                            })?;
                        let mut output_image = json!({"type":"input_image", "image_url":url});
                        if let Some(detail) = image.get("detail") {
                            output_image["detail"] = detail.clone();
                        }
                        output.push(output_image);
                    }
                    _ => return Err(responses_conversion_error("responses_feature_unsupported")),
                }
            }
            Ok(Some(Value::Array(output)))
        }
        _ => Err(responses_conversion_error("responses_feature_unsupported")),
    }
}

fn chat_tool_call_to_responses(call: &Value) -> Result<Value, ProviderError> {
    if call
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("function")
        != "function"
    {
        return Err(responses_conversion_error("responses_feature_unsupported"));
    }
    let call_id = call
        .get("id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| responses_conversion_error("responses_invalid_request"))?;
    let function = call
        .get("function")
        .and_then(Value::as_object)
        .ok_or_else(|| responses_conversion_error("responses_invalid_request"))?;
    let name = function
        .get("name")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| responses_conversion_error("responses_invalid_request"))?;
    let arguments = match function.get("arguments") {
        Some(Value::String(value)) => value.clone(),
        Some(value @ Value::Object(_)) | Some(value @ Value::Array(_)) => value.to_string(),
        Some(Value::Null) | None => "{}".into(),
        _ => return Err(responses_conversion_error("responses_invalid_request")),
    };
    Ok(json!({
        "type": "function_call",
        "call_id": call_id,
        "name": name,
        "arguments": arguments,
    }))
}

fn chat_tool_to_responses(tool: &Value) -> Result<Value, ProviderError> {
    if tool.get("type").and_then(Value::as_str) != Some("function") {
        if tool.get("type").and_then(Value::as_str).is_some() {
            return Ok(tool.clone());
        }
        return Err(responses_conversion_error("responses_feature_unsupported"));
    }
    let function = tool
        .get("function")
        .and_then(Value::as_object)
        .ok_or_else(|| responses_conversion_error("responses_invalid_request"))?;
    let name = function
        .get("name")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| responses_conversion_error("responses_invalid_request"))?;
    let mut output = serde_json::Map::new();
    output.insert("type".into(), Value::String("function".into()));
    for (key, value) in tool
        .as_object()
        .into_iter()
        .flat_map(|object| object.iter())
    {
        if key != "type" && key != "function" {
            output.insert(key.clone(), value.clone());
        }
    }
    for (key, value) in function {
        if key != "name" && key != "description" && key != "parameters" && key != "strict" {
            return Err(responses_conversion_error("responses_feature_unsupported"));
        }
        output.insert(key.clone(), value.clone());
    }
    output.insert("name".into(), Value::String(name.to_owned()));
    Ok(Value::Object(output))
}

fn chat_tool_choice_to_responses(choice: &Value) -> Result<Value, ProviderError> {
    match choice {
        Value::String(value) if matches!(value.as_str(), "auto" | "none" | "required") => {
            Ok(choice.clone())
        }
        Value::Object(object) if object.get("type").and_then(Value::as_str) == Some("function") => {
            let name = object
                .get("function")
                .and_then(|function| function.get("name"))
                .or_else(|| object.get("name"))
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| responses_conversion_error("responses_invalid_request"))?;
            Ok(json!({"type":"function", "name":name}))
        }
        _ => Err(responses_conversion_error("responses_feature_unsupported")),
    }
}

fn chat_response_format_to_responses(format: &Value) -> Result<Value, ProviderError> {
    match format.get("type").and_then(Value::as_str) {
        Some("text") => Ok(json!({"type":"text"})),
        Some("json_object") => Ok(json!({"type":"json_object"})),
        Some("json_schema") => {
            let schema = format
                .get("json_schema")
                .ok_or_else(|| responses_conversion_error("responses_invalid_request"))?;
            let name = schema
                .get("name")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| responses_conversion_error("responses_invalid_request"))?;
            let mut output = json!({"type":"json_schema", "name":name});
            if let Some(value) = schema.get("schema") {
                output["schema"] = value.clone();
            }
            if let Some(value) = schema.get("strict") {
                output["strict"] = value.clone();
            }
            Ok(output)
        }
        _ => Err(responses_conversion_error("responses_feature_unsupported")),
    }
}

/// Convert a canonical Chat Completions request to the OpenAI Responses wire
/// shape for an upstream Responses provider. This delegates to the shared
/// `canonical_to_responses_request_value` implementation; a native Responses
/// request sidecar is handled by the provider (it never enters this
/// lossy-direction converter).
pub fn canonical_to_responses_request(
    req: &ChatCompletionRequest,
    upstream_model: &str,
    stream: bool,
) -> Result<Value, ProviderError> {
    canonical_to_responses_request_value(req, upstream_model, stream)
}

/// Parse a Responses response into the canonical shape used by the router.
/// Native Responses callers receive the exact source envelope from the
/// serde-skipped sidecar; callers on other surfaces are rejected when output
/// contains built-ins, encrypted reasoning or annotations they cannot express.
pub fn responses_response_to_chat(
    response: &Value,
    requested_model: &str,
    allow_native_opaque: bool,
) -> Result<OAIChatResponse, ProviderError> {
    use crate::proxy::canonical::make_provider_error;

    let decode_error = || {
        make_provider_error(
            "upstream_decode_error",
            "response_shape",
            Some(502),
            Some(true),
            None,
        )
    };
    let unsupported = || {
        make_provider_error(
            "upstream_decode_error",
            "responses_output_conversion",
            Some(502),
            Some(false),
            None,
        )
    };
    if response.get("error").is_some_and(|error| !error.is_null()) {
        return Err(responses_failure_error(response));
    }
    let status = response
        .get("status")
        .and_then(Value::as_str)
        .ok_or_else(decode_error)?;
    if status == "failed" {
        return Err(responses_failure_error(response));
    }
    if status == "cancelled" {
        return Err(make_provider_error(
            "upstream_stream_error",
            "response_cancelled",
            Some(499),
            Some(false),
            None,
        ));
    }
    if !matches!(status, "completed" | "incomplete") {
        return Err(decode_error());
    }
    let id = response
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(decode_error)?;
    let created = response
        .get("created_at")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let model = response
        .get("model")
        .and_then(Value::as_str)
        .filter(|model| !model.is_empty())
        .unwrap_or(requested_model)
        .to_string();
    let output = response
        .get("output")
        .and_then(Value::as_array)
        .ok_or_else(decode_error)?;

    let mut text = String::new();
    let mut reasoning = String::new();
    let mut refusal = String::new();
    let mut tool_calls = Vec::new();
    let mut message_items = 0usize;
    for item in output {
        match item.get("type").and_then(Value::as_str).unwrap_or("") {
            "message" => {
                message_items += 1;
                if message_items > 1 && !allow_native_opaque {
                    return Err(unsupported());
                }
                let role = item
                    .get("role")
                    .and_then(Value::as_str)
                    .unwrap_or("assistant");
                if role != "assistant" && !allow_native_opaque {
                    return Err(unsupported());
                }
                let Some(content) = item.get("content") else {
                    if !allow_native_opaque {
                        return Err(decode_error());
                    }
                    continue;
                };
                let parts = match content {
                    Value::Array(parts) => parts.as_slice(),
                    Value::String(value) => {
                        text.push_str(value);
                        continue;
                    }
                    _ if allow_native_opaque => continue,
                    _ => return Err(decode_error()),
                };
                for part in parts {
                    match part.get("type").and_then(Value::as_str).unwrap_or("") {
                        "output_text" => {
                            if !allow_native_opaque
                                && part.get("annotations").is_some_and(|value| {
                                    value.as_array().is_some_and(|items| !items.is_empty())
                                })
                            {
                                return Err(unsupported());
                            }
                            text.push_str(
                                part.get("text")
                                    .and_then(Value::as_str)
                                    .ok_or_else(decode_error)?,
                            );
                        }
                        "refusal" => refusal.push_str(
                            part.get("refusal")
                                .or_else(|| part.get("text"))
                                .and_then(Value::as_str)
                                .ok_or_else(decode_error)?,
                        ),
                        _ if allow_native_opaque => {}
                        _ => return Err(unsupported()),
                    }
                }
            }
            "function_call" => {
                let call_id = item
                    .get("call_id")
                    .and_then(Value::as_str)
                    .ok_or_else(decode_error)?;
                let name = item
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(decode_error)?;
                let arguments = match item.get("arguments") {
                    Some(Value::String(arguments)) => arguments.clone(),
                    Some(Value::Object(_) | Value::Array(_)) => item["arguments"].to_string(),
                    Some(Value::Null) | None => String::new(),
                    _ => return Err(decode_error()),
                };
                tool_calls.push(json!({
                    "id": call_id,
                    "type": "function",
                    "function": { "name": name, "arguments": arguments },
                }));
            }
            "reasoning" => {
                if item.get("encrypted_content").is_some() && !allow_native_opaque {
                    return Err(unsupported());
                }
                if let Some(summary) = item.get("summary").and_then(Value::as_array) {
                    for part in summary {
                        if let Some(value) = part.get("text").and_then(Value::as_str) {
                            reasoning.push_str(value);
                        }
                    }
                }
            }
            "web_search_call"
            | "file_search_call"
            | "computer_call"
            | "code_interpreter_call"
            | "image_generation_call"
            | "local_shell_call"
            | "mcp_call"
            | "apply_patch_call"
                if allow_native_opaque => {}
            _ if allow_native_opaque => {}
            _ => return Err(unsupported()),
        }
    }

    let incomplete_reason = response
        .pointer("/incomplete_details/reason")
        .and_then(Value::as_str);
    let finish_reason = if status == "incomplete" {
        Some(if incomplete_reason == Some("content_filter") {
            "content_filter".to_string()
        } else {
            "length".to_string()
        })
    } else if !tool_calls.is_empty() {
        Some("tool_calls".to_string())
    } else {
        Some("stop".to_string())
    };

    let usage = response
        .get("usage")
        .map(|usage_value| {
            let input = usage_value
                .get("input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let output = usage_value
                .get("output_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let total = usage_value
                .get("total_tokens")
                .and_then(Value::as_u64)
                .unwrap_or_else(|| input.saturating_add(output));
            let (Ok(input), Ok(output), Ok(total)) = (
                u32::try_from(input),
                u32::try_from(output),
                u32::try_from(total),
            ) else {
                return Err(decode_error());
            };
            let mut extra = std::collections::HashMap::new();
            if let Some(details) = usage_value.get("input_tokens_details") {
                extra.insert("prompt_tokens_details".into(), details.clone());
            }
            if let Some(details) = usage_value.get("output_tokens_details") {
                extra.insert("completion_tokens_details".into(), details.clone());
            }
            Ok(Usage {
                prompt_tokens: input,
                completion_tokens: output,
                total_tokens: total,
                extra,
            })
        })
        .transpose()?;

    let mut message_extra = std::collections::HashMap::new();
    if !reasoning.is_empty() {
        message_extra.insert("reasoning_content".into(), Value::String(reasoning));
    }
    let message = OAIMessage {
        role: "assistant".into(),
        content: (!text.is_empty()).then_some(text),
        tool_calls: (!tool_calls.is_empty()).then_some(tool_calls),
        refusal: (!refusal.is_empty()).then_some(refusal),
        extra: message_extra,
    };
    Ok(OAIChatResponse {
        id: id.to_string(),
        object: "chat.completion".into(),
        created,
        model,
        choices: vec![OAIChoice {
            index: 0,
            message,
            finish_reason,
            extra: Default::default(),
        }],
        usage,
        extra: Default::default(),
        source_anthropic_response: None,
        source_gemini_response: None,
        source_responses_response: Some(response.clone()),
    })
}

pub(crate) fn responses_failure_error(response: &Value) -> ProviderError {
    let error = response
        .get("error")
        .filter(|error| !error.is_null())
        .unwrap_or(response);
    let status = ["status", "status_code", "code"]
        .iter()
        .filter_map(|key| error.get(*key))
        .find_map(|value| {
            value
                .as_u64()
                .or_else(|| value.as_str().and_then(|text| text.parse::<u64>().ok()))
        })
        .filter(|status| (400..=599).contains(status))
        .and_then(|status| u16::try_from(status).ok())
        .unwrap_or(502);
    crate::proxy::canonical::make_provider_error(
        "upstream_http_error",
        "response_status",
        Some(status),
        None,
        None,
    )
}
