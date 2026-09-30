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
    /// Original Anthropic request body when `/v1/messages` is the client
    /// surface. This is internal routing context and must never appear on an
    /// OpenAI-compatible wire.
    #[serde(skip)]
    pub source_anthropic_request: Option<serde_json::Value>,
    /// Correlation id assigned at the local HTTP edge. Internal only.
    #[serde(skip)]
    pub request_id: Option<String>,
    /// Client-supplied upstream credential captured at the HTTP edge for
    /// `anthropic_passthrough` providers (subscription OAuth relay).
    /// Internal routing context: never serialized, persisted, or logged.
    #[serde(skip)]
    pub client_credential: Option<Client_credential>,
}

/// Raw inbound auth header values relayed verbatim to a passthrough
/// upstream. Never logged, persisted, or serialized — treat the contents
/// as secret.
#[derive(Debug, Clone, Default)]
#[allow(non_camel_case_types)] // repo style: snake_case types are allowed explicitly
pub struct Client_credential {
    /// Raw `authorization` header value verbatim (e.g. `Bearer <oauth>`).
    pub authorization: Option<String>,
    /// Raw `x-api-key` header value verbatim.
    pub x_api_key: Option<String>,
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
    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
    /// Original Anthropic response body for lossless same-protocol relay.
    #[serde(skip)]
    pub source_anthropic_response: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAIChoice {
    pub index: u32,
    pub message: OAIMessage,
    #[serde(rename = "finish_reason", skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
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
    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
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
    /// Original Anthropic SSE data object for lossless same-protocol relay.
    #[serde(skip)]
    pub source_anthropic_event: Option<serde_json::Value>,
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
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
    #[serde(
        rename = "reasoning_content",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub reasoning: Option<String>,
    /// Preserve provider extensions and the distinct `reasoning` wire key.
    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

impl StreamDelta {
    pub fn reasoning_text(&self) -> Option<&str> {
        self.reasoning.as_deref().or_else(|| {
            self.extra
                .get("reasoning")
                .and_then(serde_json::Value::as_str)
        })
    }
}

/// Stable classification for upstream 4xx response bodies. `None` means
/// unclassified (legacy default — the router treats an unclassified 4xx as
/// terminal, matching the doc-comment invariant that authentication and
/// malformed-input failures must not be replayed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpstreamErrorKind {
    /// OAuth refresh / session-revoked response from the upstream
    /// (e.g. `refresh_token` invalidated, `invalid_grant`, "sign in again").
    /// The Codex mirror auto-heal triggers on this; the router MUST NOT
    /// fail over.
    AuthExpired,
    /// "Tool schema not supported by this upstream" response. The router
    /// SHOULD fail over to the next provider.
    ToolCapability,
}

/// Error from a provider during request execution.
#[derive(Debug)]
pub struct ProviderError {
    pub message: String,
    pub status: Option<u16>,
    pub retriable: bool,
    pub code: String,
    pub stage: String,
    pub response_bytes: Option<usize>,
    /// Stable classification for upstream 4xx bodies. `None` means
    /// unclassified (legacy default — the router treats an unclassified
    /// 4xx as terminal, matching the doc-comment invariant that
    /// authentication and malformed-input failures must not be replayed).
    pub kind: Option<UpstreamErrorKind>,
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}
impl std::error::Error for ProviderError {}

pub fn make_error(
    message: impl Into<String>,
    status: Option<u16>,
    retriable: Option<bool>,
) -> ProviderError {
    let msg = message.into();
    let retr = retriable.unwrap_or_else(|| status.map_or(true, |s| s == 429 || s >= 500));
    let code = stable_error_code(&msg)
        .unwrap_or("provider_error")
        .to_string();
    ProviderError {
        message: public_error_message(&code, status).to_string(),
        status,
        retriable: retr,
        code,
        stage: "provider".into(),
        response_bytes: None,
        kind: None,
    }
}

pub fn make_provider_error(
    code: &str,
    stage: &str,
    status: Option<u16>,
    retriable: Option<bool>,
    response_bytes: Option<usize>,
) -> ProviderError {
    let code = stable_error_code(code).unwrap_or("provider_error");
    let stage = stable_error_code(stage).unwrap_or("provider");
    ProviderError {
        message: public_error_message(code, status).to_string(),
        status,
        retriable: retriable
            .unwrap_or_else(|| status.map_or(true, |value| value == 429 || value >= 500)),
        code: code.to_string(),
        stage: stage.to_string(),
        response_bytes,
        kind: None,
    }
}

/// Build a `ProviderError` with a classified upstream-4xx kind. Use this
/// when the caller already inspected the response body via
/// `classify_upstream_4xx`. The body itself is never carried — only a
/// stable enum variant.
pub fn make_provider_error_with_kind(
    code: &str,
    stage: &str,
    status: Option<u16>,
    retriable: Option<bool>,
    response_bytes: Option<usize>,
    kind: Option<UpstreamErrorKind>,
) -> ProviderError {
    let mut err = make_provider_error(code, stage, status, retriable, response_bytes);
    err.kind = kind;
    err
}

/// Stable classification for upstream 4xx response bodies.
///
/// Returns:
/// - `Some(UpstreamErrorKind::AuthExpired)` for OAuth refresh / session-
///   revoked responses — the router must NOT fail over, and the Codex
///   mirror auto-heal triggers.
/// - `Some(UpstreamErrorKind::ToolCapability)` for "tool schema not
///   supported by this upstream" responses — the router SHOULD fail over.
/// - `None` otherwise; the router treats an unclassified 4xx as terminal.
///
/// Body inspection order:
/// 1. Try `serde_json::from_str::<Value>(head)` and look at the JSON
///    `error.message` / `error.code` / `error.type` (and nested
///    `error.error.code`) first — real OpenAI / Anthropic errors parse.
/// 2. Fall back to lower-case substring matching against the first 8 KiB.
///
/// `tools[0]`-style JSON-path strings are deliberately excluded from the
/// tool-capability keyword set: a naïve substring match would also hit
/// on a request-echo body, which is not a tool-capability rejection.
pub fn classify_upstream_4xx(status: u16, body: &str) -> Option<UpstreamErrorKind> {
    if !(400..500).contains(&status) {
        return None;
    }
    let head: &str = &body[..body.len().min(8192)];

    // JSON-first: the structured fields are more reliable than the
    // free-form message, and `error.code == "invalid_grant"` is the
    // canonical OAuth refresh-token failure code.
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(head) {
        if let Some(kind) = classify_from_json(&value) {
            return Some(kind);
        }
    }

    // Substring fallback (non-JSON bodies, older Codex error pages, etc.).
    let lower: String = head
        .bytes()
        .map(|byte| byte.to_ascii_lowercase() as char)
        .collect();
    const AUTH_KEYWORDS: &[&str] = &[
        "invalid_grant",
        "access token could not be refreshed",
        "sign in again",
        "signed in to another account",
        // Spaced form: OAuth RFC and OpenAI messages use this; the
        // underscored form `refresh_token` also appears in many JSON
        // examples / request echoes, so we use the spaced one to avoid
        // false positives.
        "refresh token",
    ];
    for keyword in AUTH_KEYWORDS {
        if lower.contains(keyword) {
            return Some(UpstreamErrorKind::AuthExpired);
        }
    }
    const TOOL_KEYWORDS: &[&str] = &[
        "unsupported tool",
        "unknown tool",
        "tool not supported",
        "tools are not supported",
        "invalid function",
        "invalid schema",
        "function name",
    ];
    for keyword in TOOL_KEYWORDS {
        if lower.contains(keyword) {
            return Some(UpstreamErrorKind::ToolCapability);
        }
    }
    None
}

/// Inspect a parsed JSON error body for the two classification buckets.
/// Returns `None` if the JSON does not carry a recognizable structured
/// signal — the caller falls back to substring matching.
fn classify_from_json(value: &serde_json::Value) -> Option<UpstreamErrorKind> {
    let error = value.get("error").unwrap_or(value);

    // Nested `{error: {code, message, type}}` (OpenAI / Anthropic shape).
    let code = error.get("code").and_then(|v| v.as_str()).unwrap_or("");
    let kind = error.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let message = error.get("message").and_then(|v| v.as_str()).unwrap_or("");

    // OpenAI / OAuth: `error.code == "invalid_grant"`, sometimes nested.
    let nested_code = error
        .get("error")
        .and_then(|inner| inner.get("code"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let lower_code = code.to_ascii_lowercase();
    let lower_kind = kind.to_ascii_lowercase();
    let lower_message = message.to_ascii_lowercase();
    let lower_nested = nested_code.to_ascii_lowercase();

    if lower_code == "invalid_grant"
        || lower_nested == "invalid_grant"
        || lower_code.contains("refresh_token")
        || lower_code.contains("invalid_api_key")
        || lower_code.contains("authentication")
        || lower_message.contains("refresh token")
        || lower_message.contains("sign in again")
        || lower_message.contains("access token could not be refreshed")
    {
        return Some(UpstreamErrorKind::AuthExpired);
    }

    if lower_code.contains("tool")
        || lower_kind.contains("tool")
        || lower_message.contains("unsupported tool")
        || lower_message.contains("tool not supported")
        || lower_message.contains("tools are not supported")
        || lower_message.contains("invalid function")
        || lower_message.contains("invalid schema")
    {
        return Some(UpstreamErrorKind::ToolCapability);
    }
    None
}

pub fn public_error_message(code: &str, status: Option<u16>) -> &'static str {
    match code {
        "no_route" => "No route is available for the requested model.",
        "upstream_http_error" => match status {
            Some(429) => "The upstream provider is rate limited.",
            Some(400..=499) => "The upstream provider rejected the request.",
            _ => "The upstream provider is unavailable.",
        },
        "upstream_decode_error" => "The upstream provider returned an invalid response.",
        "upstream_stream_error" => "The upstream stream failed before completion.",
        "upstream_timeout" | "upstream_transport_error" => {
            "The upstream provider could not be reached."
        }
        "all_hops_exhausted" => "No configured provider completed the request.",
        "invalid_request" => "The request is invalid.",
        _ => "The gateway could not complete the request.",
    }
}

fn stable_error_code(value: &str) -> Option<&str> {
    (!value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'))
    .then_some(value)
}

/// Classify an upstream SSE error without including its payload in diagnostics.
/// Unknown application errors are terminal; only known overload/rate failures
/// or explicit 5xx statuses can advance the configured provider chain.
pub fn stream_error_status(event: &serde_json::Value) -> u16 {
    let error = event.get("error").unwrap_or(event);
    for key in ["status", "status_code", "code"] {
        let value = error.get(key).or_else(|| event.get(key));
        let numeric = value.and_then(|value| {
            value
                .as_u64()
                .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
        });
        if let Some(status) = numeric.filter(|status| (400..=599).contains(status)) {
            return status as u16;
        }
    }
    match error
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
    {
        "rate_limit_error" => 429,
        "overloaded_error" => 529,
        "api_error" | "proxy_stream_error" => 502,
        "authentication_error" => 401,
        "permission_error" => 403,
        _ => 400,
    }
}

pub fn stream_error_chunk(_model: &str, status: u16, kind: &str) -> OAIStreamChunk {
    OAIStreamChunk {
        id: "proxy_error".into(),
        object: "chat.completion.chunk".into(),
        created: chrono::Utc::now().timestamp(),
        // A client-supplied model identifier can itself contain private
        // material. Error envelopes use a fixed value instead of echoing it.
        model: "unavailable".into(),
        choices: Vec::new(),
        extra: [(
            "error".into(),
            serde_json::json!({
                "type": kind,
                "message": "Upstream stream failed before completion",
                "status": status,
            }),
        )]
        .into_iter()
        .collect(),
        source_anthropic_event: None,
    }
}

pub fn stream_chunk_error(chunk: &OAIStreamChunk) -> Option<ProviderError> {
    chunk.extra.get("error").map(|error| {
        let status = stream_error_status(error);
        make_provider_error(
            "upstream_stream_error",
            "stream_event",
            Some(status),
            None,
            None,
        )
    })
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

#[cfg(test)]
mod classify_tests {
    use super::{classify_upstream_4xx, UpstreamErrorKind};

    #[test]
    fn auth_expired_from_openai_invalid_grant_json() {
        let body = r#"{"error":{"message":"Your access token could not be refreshed because you have since logged out or signed in to another account. Please sign in again.","type":"invalid_request_error","code":"invalid_grant"}}"#;
        assert_eq!(
            classify_upstream_4xx(400, body),
            Some(UpstreamErrorKind::AuthExpired)
        );
    }

    #[test]
    fn auth_expired_from_substring_when_message_mentions_sign_in_again() {
        // Non-JSON body, e.g. an HTML error page.
        let body = "<html><body>Sign in again to continue.</body></html>";
        assert_eq!(
            classify_upstream_4xx(401, body),
            Some(UpstreamErrorKind::AuthExpired)
        );
    }

    #[test]
    fn auth_expired_is_case_insensitive() {
        let body = r#"{"error":{"message":"ACCESS TOKEN COULD NOT BE REFRESHED"}}"#;
        assert_eq!(
            classify_upstream_4xx(400, body),
            Some(UpstreamErrorKind::AuthExpired)
        );
    }

    #[test]
    fn tool_capability_from_openai_unsupported_tool_message() {
        let body = r#"{"error":{"message":"tools[0].function.name: Unsupported tool: shell. Supported tools: web_search, file_read.","type":"invalid_request_error","code":"invalid_value"}}"#;
        assert_eq!(
            classify_upstream_4xx(400, body),
            Some(UpstreamErrorKind::ToolCapability)
        );
    }

    #[test]
    fn tool_capability_from_substring_keyword() {
        let body = "Tool not supported by this model";
        assert_eq!(
            classify_upstream_4xx(400, body),
            Some(UpstreamErrorKind::ToolCapability)
        );
    }

    #[test]
    fn generic_400_without_keywords_is_unclassified() {
        let body =
            r#"{"error":{"message":"Invalid value: 'foo'.","type":"invalid_request_error"}}"#;
        assert_eq!(classify_upstream_4xx(400, body), None);
    }

    #[test]
    fn empty_body_is_unclassified() {
        assert_eq!(classify_upstream_4xx(400, ""), None);
    }

    #[test]
    fn non_4xx_status_is_unclassified() {
        let body = "Your access token could not be refreshed";
        assert_eq!(classify_upstream_4xx(500, body), None);
    }

    #[test]
    fn truncated_body_still_matches_prefix() {
        let body = "Your access token could not be refreshed because";
        assert_eq!(
            classify_upstream_4xx(400, body),
            Some(UpstreamErrorKind::AuthExpired)
        );
    }

    #[test]
    fn request_echo_with_tools_path_is_not_misclassified_as_tool_capability() {
        // A 400 body that echoes the request payload — contains "tools[0]"
        // in a JSON path, but the error message itself says nothing about
        // tool support. Must stay unclassified.
        let body = r#"{"error":{"message":"Invalid request: see request","request":{"model":"gpt-5","tools":[{"type":"function","function":{"name":"shell"}}]}}}"#;
        assert_eq!(classify_upstream_4xx(400, body), None);
    }

    #[test]
    fn oversized_body_does_not_panic() {
        // 100 KiB of filler followed by a tool-capability keyword INSIDE
        // the first 8 KiB window — must classify as ToolCapability and
        // must not panic on the long input.
        let mut body = String::with_capacity(100_000 + 64);
        body.push_str("invalid function "); // inside the 8 KiB head
        body.push_str(&"x".repeat(100_000));
        assert_eq!(
            classify_upstream_4xx(400, &body),
            Some(UpstreamErrorKind::ToolCapability)
        );
    }

    #[test]
    fn oversized_body_with_keyword_beyond_head_is_unclassified() {
        // Keyword past the 8 KiB head is intentionally NOT classified —
        // the cap bounds CPU cost on adversarial upstream bodies.
        let mut body = String::with_capacity(20_000);
        body.push_str(&"x".repeat(9000));
        body.push_str(" invalid function ");
        assert_eq!(classify_upstream_4xx(400, &body), None);
    }
}
