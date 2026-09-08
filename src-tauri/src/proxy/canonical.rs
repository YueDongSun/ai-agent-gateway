//! Canonical internal chat format = OpenAI Chat Completions.
//!
//! Every provider speaks this shape; the gateway edge converts to/from
//! Anthropic wire format. Ported from `src/gateway/canonical.ts`.

use serde::{Deserialize, Serialize};

/// A chat completion request in canonical OpenAI format.
///
/// `tools` and `tool_choice` are stored as raw `serde_json::Value` arrays
/// because tool definition shapes differ between OpenAI / Anthropic / Codex
/// Responses API and we don't want to maintain three near-identical
/// structs. The converter layer is responsible for translating between
/// shapes (see `proxy::converter`).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// OpenAI-format tool definitions. Each entry is a JSON object whose
    /// concrete shape depends on the upstream protocol.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<serde_json::Value>>,
    /// OpenAI-format tool selector ("auto" / "none" / "required" /
    /// `{"type":"function","function":{"name":"X"}}`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<serde_json::Value>,
    /// Capture any extra fields to pass through.
    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

/// One message in a chat completion request.
///
/// `tool_calls` is stored as raw values (OpenAI's `tool_calls` array shape
/// varies between providers — index, function name + JSON-string args). The
/// canonical encoder preserves order; converters normalise to the target
/// protocol.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: serde_json::Value,
    /// OpenAI-style assistant tool invocations: array of
    /// `{id, type, function:{name, arguments}}` objects.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<serde_json::Value>>,
    /// OpenAI-style `tool` role message id (`role: tool`, `tool_call_id`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

/// Non-streaming chat completion response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAIChatResponse {
    pub id: String,
    pub object: String,
    pub created: i64,
    pub model: String,
    pub choices: Vec<OAIChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAIChoice {
    pub index: u32,
    pub message: OAIMessage,
    #[serde(rename = "finish_reason", skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAIMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// OpenAI `tool_calls` array on an assistant message (id / type /
    /// function name + JSON-string arguments).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<serde_json::Value>>,
    /// OpenAI `refusal` field — surfaced verbatim.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refusal: Option<String>,
    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    #[serde(rename = "prompt_tokens")]
    pub prompt_tokens: u32,
    #[serde(rename = "completion_tokens")]
    pub completion_tokens: u32,
    #[serde(rename = "total_tokens")]
    pub total_tokens: u32,
}

/// A single streaming chunk (the JSON object inside an SSE `data:` line).
///
/// Top-level flatten captures `object: "chat.completion.chunk"` already in
/// the explicit field, plus any provider-specific top-level keys
/// (`system_fingerprint`, etc).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAIStreamChunk {
    pub id: String,
    pub object: String,
    pub created: i64,
    pub model: String,
    pub choices: Vec<StreamChoice>,
    /// Captures `usage` (often the last chunk) plus other top-level fields.
    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamChoice {
    pub index: u32,
    pub delta: StreamDelta,
    #[serde(rename = "finish_reason", skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
    /// Provider-specific extras (e.g. `logprobs`).
    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamDelta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// OpenAI `tool_calls` array delta — usually with `index`, sometimes
    /// `id`/`function.name` only, sometimes `function.arguments` fragment.
    /// Critical for streaming tool use; without this field the entire
    /// tool-call payload was being silently dropped by serde.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tool_calls: Option<Vec<serde_json::Value>>,
    /// DeepSeek/Kimi-style `reasoning_content` (or Anthropic `thinking`
    /// delta passthrough). Stored under the same key on the wire as the
    /// upstream sent it.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub reasoning: Option<String>,
}

/// Error from a provider during request execution.
#[derive(Debug)]
pub struct ProviderError {
    pub message: String,
    pub status: Option<u16>,
    pub retriable: bool,
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}
impl std::error::Error for ProviderError {}

pub fn make_error(message: impl Into<String>, status: Option<u16>, retriable: Option<bool>) -> ProviderError {
    let msg = message.into();
    let retr = retriable.unwrap_or_else(|| status.map_or(true, |s| s >= 500));
    ProviderError {
        message: msg,
        status,
        retriable: retr,
    }
}

/// Extract the last user message's text content from a request.
pub fn extract_text(req: &ChatCompletionRequest) -> String {
    let last = match req.messages.last() {
        Some(m) => m,
        None => return String::new(),
    };
    match &last.content {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(parts) => {
            let mut out = String::new();
            for p in parts {
                if let Some(s) = p.as_str() {
                    out.push_str(s);
                } else if let Some(text) = p.get("text").and_then(|v| v.as_str()) {
                    out.push_str(text);
                }
            }
            out
        }
        other => other.to_string(),
    }
}