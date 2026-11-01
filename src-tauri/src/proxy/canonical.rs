//! Canonical internal chat format = OpenAI Chat Completions.
//!
//! Every provider speaks this shape; the gateway edge converts to/from
//! Anthropic wire format. Ported from `src/gateway/canonical.ts`.

use serde::{Deserialize, Serialize};

const MAX_STRUCTURED_ERROR_BYTES: usize = 64 * 1024;

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
    /// Original OpenAI Responses request envelope. Internal routing context:
    /// preserved only for a native Responses upstream and never serialized on
    /// Chat Completions or Anthropic wires.
    #[serde(skip)]
    pub source_responses_request: Option<crate::proxy::responses::ResponsesRequest>,
    /// Original Gemini body, retained only for a Gemini-native upstream.
    #[serde(skip)]
    pub source_gemini_request: Option<serde_json::Value>,
    /// Correlation id assigned at the local HTTP edge. Internal only.
    #[serde(skip)]
    pub request_id: Option<String>,
    /// Client-supplied upstream credential captured at the HTTP edge for
    /// `anthropic_passthrough` providers (subscription OAuth relay).
    /// Internal routing context: never serialized, persisted, or logged.
    #[serde(skip)]
    pub client_credential: Option<Client_credential>,
}

/// The per-hop wire view of a canonical request: the same JSON with this
/// hop's upstream model and stream flag applied, and the conversation
/// payload borrowed rather than copied.
///
/// The router dispatches every hop against ONE shared request (see the
/// `Provider::complete` contract), so the hop-specific fields used to be
/// applied by serializing the request into a `serde_json::Value`, patching
/// two keys, and serializing that DOM again. That deep-copied the whole
/// message payload — base64 images included — on every hop and held the
/// copy alongside the final bytes. Serializing this view writes the request
/// body once.
///
/// `hop_body_matches_the_canonical_wire_shape` pins the two views to the
/// same JSON, so a canonical field added without a mirror here fails the
/// test instead of silently dropping off the wire.
#[derive(Debug, Serialize)]
#[allow(non_camel_case_types)] // repo style: snake_case type names are allowed explicitly
pub struct Hop_body<'a> {
    /// This hop's upstream model (the `[1M]`-stripped form the caller
    /// resolved), not the client-requested alias.
    pub model: &'a str,
    pub messages: &'a [ChatMessage],
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<&'a [serde_json::Value]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<&'a serde_json::Value>,
    #[serde(flatten, serialize_with = "serialize_hop_extra")]
    pub extra: &'a std::collections::HashMap<String, serde_json::Value>,
    /// Set by streaming hops that do not already carry a caller-supplied
    /// `stream_options`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_options: Option<Stream_options>,
}

fn serialize_hop_extra<S: serde::Serializer>(
    extra: &&std::collections::HashMap<String, serde_json::Value>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    use serde::ser::SerializeMap;
    let mut map = serializer.serialize_map(None)?;
    for (key, value) in *extra {
        if key != "model" && key != "stream" {
            map.serialize_entry(key, value)?;
        }
    }
    map.end()
}

/// OpenAI streaming usage accounting (`stream_options.include_usage`).
#[derive(Debug, Serialize)]
#[allow(non_camel_case_types)] // repo style: snake_case type names are allowed explicitly
pub struct Stream_options {
    pub include_usage: bool,
}

/// Raw inbound auth header values relayed verbatim to a passthrough
/// upstream. Never logged, persisted, or serialized — treat the contents
/// as secret.
#[derive(Debug, Clone, Default)]
#[allow(non_camel_case_types)] // repo style: snake_case types are allowed explicitly
pub struct Client_credential {
    /// Set only by the trusted HTTP edge when gateway authentication failed
    /// but loopback passthrough admission succeeded. Never a wire field.
    pub(crate) passthrough_only: bool,
    /// Raw `authorization` header value verbatim (e.g. `Bearer <oauth>`).
    pub authorization: Option<String>,
    /// Raw `x-api-key` header value verbatim.
    pub x_api_key: Option<String>,
    /// Raw `anthropic-beta` header value verbatim (the feature-flag list a
    /// subscription client sends, e.g. `context-1m,...`). Not a credential,
    /// but relayed verbatim on passthrough hops only so the static-key wire
    /// format stays byte-identical.
    pub anthropic_beta: Option<String>,
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
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_chat_tool_calls"
    )]
    pub tool_calls: Option<Vec<serde_json::Value>>,
    /// OpenAI-style `tool` role message id (`role: tool`, `tool_call_id`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

/// Native Gemini call signatures remain in private source envelopes. The
/// canonical Chat wire must not expose those protocol-specific fields, while
/// all other provider extensions retain their original values without copies.
fn serialize_chat_tool_calls<S: serde::Serializer>(
    calls: &Option<Vec<serde_json::Value>>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    use serde::ser::SerializeSeq;
    let Some(calls) = calls else {
        return serializer.serialize_none();
    };
    let mut sequence = serializer.serialize_seq(Some(calls.len()))?;
    for call in calls {
        sequence.serialize_element(&Chat_tool_call_wire(call))?;
    }
    sequence.end()
}

#[allow(non_camel_case_types)]
struct Chat_tool_call_wire<'a>(&'a serde_json::Value);

impl Serialize for Chat_tool_call_wire<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let Some(object) = self.0.as_object() else {
            return self.0.serialize(serializer);
        };
        let mut map = serializer.serialize_map(None)?;
        for (key, value) in object {
            if key != "thoughtSignature" && key != "thought_signature" {
                map.serialize_entry(key, value)?;
            }
        }
        map.end()
    }
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
    /// Original OpenAI Responses response body for lossless same-protocol
    /// relay. Never serialized on a Chat Completions wire.
    #[serde(skip)]
    pub source_responses_response: Option<serde_json::Value>,
    /// Original Gemini response, retained only on the Gemini client surface.
    #[serde(skip)]
    pub source_gemini_response: Option<serde_json::Value>,
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
    /// Original OpenAI Responses SSE frame for lossless same-protocol relay.
    /// Includes event name, id, and the joined multiline data field. Never
    /// serialized on a Chat Completions wire.
    #[serde(skip)]
    pub source_responses_event: Option<crate::proxy::responses::ResponsesSseEvent>,
    /// Original Gemini SSE data object. Never serialized on foreign wires.
    #[serde(skip)]
    pub source_gemini_event: Option<serde_json::Value>,
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
    /// A structured rate rejection whose bucket is not known to cover the
    /// account. Another model on that account may still have capacity.
    RateLimited,
    /// An explicit account or plan allowance rejection. Protocol adapters
    /// sharing that account do not provide independent quota capacity.
    QuotaExceeded,
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
    /// unclassified: ordinary 4xx failures remain terminal rather than
    /// replaying rejected requests on another supplier.
    pub kind: Option<UpstreamErrorKind>,
    /// The upstream's own Anthropic-shaped `error.type` token when it is on
    /// the public allowlist (see [`anthropic_upstream_error_type`]). Relayed
    /// verbatim on the Anthropic surface so native CLI retry logic keys on
    /// the documented types; never synthesized for foreign upstreams.
    pub upstream_error_type: Option<&'static str>,
}

/// Anthropic's documented error `type` tokens. Only these relay upstream.
pub(crate) const ANTHROPIC_ERROR_TYPES: &[&str] = &[
    "invalid_request_error",
    "authentication_error",
    "permission_error",
    "not_found_error",
    "request_too_large",
    "rate_limit_error",
    "api_error",
    "overloaded_error",
];

/// Extract an allowlisted Anthropic `error.type` from a captured upstream
/// error body. Unknown spellings and unparseable bodies return `None` — the
/// surface then maps its own status instead of inventing a type.
pub fn anthropic_upstream_error_type(body: &str) -> Option<&'static str> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let token = value.get("error")?.get("type")?.as_str()?;
    ANTHROPIC_ERROR_TYPES.iter().find(|known| **known == token).copied()
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}
impl std::error::Error for ProviderError {}

fn default_retriable(status: Option<u16>) -> bool {
    status.is_none_or(|status| status == 429 || status >= 500)
}

/// Recognize explicit quota rejections returned as otherwise terminal 4xx.
/// Only structured error fields participate; messages, request echoes and
/// unrelated numeric values cannot turn a rejected request into a retry.
/// MiniMax 2056 is the exhausted five-hour window; 2062 and 2067 are its
/// documented rate/usage limits. The caller retains the ordinary HTTP policy
/// when this returns `None`, including terminal authentication failures.
pub(crate) fn upstream_quota_retriable(status: u16, body: &str) -> Option<bool> {
    if !matches!(status, 400 | 402 | 403 | 429) || body.len() > MAX_STRUCTURED_ERROR_BYTES {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let error = value.get("error").filter(|error| error.is_object());
    let error_type = error
        .and_then(|error| error.get("type"))
        .and_then(serde_json::Value::as_str);
    let error_code = error
        .and_then(|error| error.get("code"))
        .and_then(serde_json::Value::as_str);
    let auth_rejection = error_type.is_some_and(|kind| {
        matches!(
            kind,
            "authentication_error" | "authorization_error" | "permission_error"
        )
    }) || error_code.is_some_and(|code| {
        matches!(
            code,
            "invalid_api_key"
                | "invalid_token"
                | "invalid_grant"
                | "permission_denied"
                | "access_denied"
        )
    });
    if auth_rejection {
        return None;
    }
    let quota_error = error_type == Some("rate_limit_error")
        || error_code.is_some_and(|code| {
            matches!(
                code,
                "rate_limit_exceeded"
                    | "usage_limit_reached"
                    | "insufficient_quota"
                    | "quota_exceeded"
            )
        });
    let minimax_quota = value
        .pointer("/base_resp/status_code")
        .and_then(serde_json::Value::as_u64)
        .is_some_and(|code| matches!(code, 2056 | 2062 | 2067));
    (quota_error || minimax_quota).then_some(true)
}

/// Require an explicit allowance signal before skipping every model and
/// protocol sharing the credential. Ordinary 429/RPM/TPM errors and MiniMax
/// 2062 rate/concurrency limits do not identify an exhausted account bucket.
fn account_allowance_exhausted(body: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return false;
    };
    value
        .pointer("/error/code")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|code| matches!(code, "usage_limit_reached" | "insufficient_quota"))
        || value
            .pointer("/base_resp/status_code")
            .and_then(serde_json::Value::as_u64)
            .is_some_and(|code| matches!(code, 2056 | 2067))
}

pub fn make_error(
    message: impl Into<String>,
    status: Option<u16>,
    retriable: Option<bool>,
) -> ProviderError {
    let msg = message.into();
    let retr = retriable.unwrap_or_else(|| default_retriable(status));
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
        upstream_error_type: None,
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
        retriable: retriable.unwrap_or_else(|| default_retriable(status)),
        code: code.to_string(),
        stage: stage.to_string(),
        response_bytes,
        kind: None,
        upstream_error_type: None,
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
/// - `Some(UpstreamErrorKind::QuotaExceeded)` for a structured allowance
///   rejection — the router skips other adapters sharing that quota resource.
/// - `Some(UpstreamErrorKind::RateLimited)` for a structured rate rejection
///   whose account scope is unknown — other models remain eligible.
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
    let structured_kind = serde_json::from_str::<serde_json::Value>(head)
        .ok()
        .and_then(|value| classify_from_json(&value));
    if structured_kind == Some(UpstreamErrorKind::AuthExpired) {
        return structured_kind;
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
    if upstream_quota_retriable(status, body) == Some(true) {
        return Some(if account_allowance_exhausted(body) {
            UpstreamErrorKind::QuotaExceeded
        } else {
            UpstreamErrorKind::RateLimited
        });
    }
    if structured_kind.is_some() {
        return structured_kind;
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
        "model_alias_no_candidates" => "No eligible model can satisfy the requested routing tier.",
        "model_precise_unavailable" => {
            "No eligible provider is available for the requested exact model."
        }
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
        "responses_state_unsupported" => {
            "The request contains Responses state this upstream cannot preserve."
        }
        "responses_feature_unsupported" => {
            "The request uses a Responses feature this upstream cannot represent."
        }
        "responses_invalid_request" => "The Responses request is invalid.",
        "anthropic_feature_unsupported" => {
            "The request uses a feature this Anthropic upstream cannot represent."
        }
        "anthropic_invalid_request" => "The Anthropic request is invalid.",
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
        source_responses_event: None,
        source_gemini_event: None,
    }
}

pub fn stream_chunk_error(chunk: &OAIStreamChunk) -> Option<ProviderError> {
    chunk.extra.get("error").map(|error| {
        let status = stream_error_status(error);
        let body = if error.get("error").is_some() || error.get("base_resp").is_some() {
            serde_json::to_string(error)
        } else {
            serde_json::to_string(&serde_json::json!({"error":error}))
        }
        .unwrap_or_default();
        make_provider_error_with_kind(
            "upstream_stream_error",
            "stream_event",
            Some(status),
            upstream_quota_retriable(status, &body),
            None,
            classify_upstream_4xx(status, &body),
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

#[cfg(test)]
mod retriable_tests {
    use super::{
        classify_upstream_4xx, default_retriable, make_provider_error, upstream_quota_retriable,
        ProviderError, UpstreamErrorKind,
    };

    #[test]
    fn structured_quota_rejections_allow_another_configured_supplier() {
        for status in [400, 402, 403, 429] {
            for code in [2056, 2062, 2067] {
                let body = serde_json::json!({"base_resp":{"status_code":code}}).to_string();
                assert_eq!(upstream_quota_retriable(status, &body), Some(true));
                assert_eq!(
                    classify_upstream_4xx(status, &body),
                    Some(if code == 2062 {
                        UpstreamErrorKind::RateLimited
                    } else {
                        UpstreamErrorKind::QuotaExceeded
                    })
                );
            }
            for (body, expected) in [
                (
                    r#"{"error":{"type":"rate_limit_error"}}"#,
                    UpstreamErrorKind::RateLimited,
                ),
                (
                    r#"{"error":{"code":"usage_limit_reached"}}"#,
                    UpstreamErrorKind::QuotaExceeded,
                ),
                (
                    r#"{"error":{"code":"rate_limit_exceeded"}}"#,
                    UpstreamErrorKind::RateLimited,
                ),
                (
                    r#"{"error":{"code":"quota_exceeded"}}"#,
                    UpstreamErrorKind::RateLimited,
                ),
                (
                    r#"{"error":{"code":"insufficient_quota"}}"#,
                    UpstreamErrorKind::QuotaExceeded,
                ),
            ] {
                assert_eq!(upstream_quota_retriable(status, body), Some(true));
                assert_eq!(classify_upstream_4xx(status, body), Some(expected));
            }
        }
    }

    #[test]
    fn structured_quota_recognition_keeps_auth_and_input_rejections_terminal() {
        for (status, body) in [
            (401, r#"{"base_resp":{"status_code":2056}}"#),
            (
                403,
                r#"{"error":{"type":"authentication_error","code":"usage_limit_reached"}}"#,
            ),
            (
                403,
                r#"{"error":{"type":"rate_limit_error","code":"invalid_api_key"}}"#,
            ),
            (
                400,
                r#"{"base_resp":{"status_code":2013,"status_msg":"usage limit exceeded"}}"#,
            ),
            (
                400,
                r#"{"error":{"message":"usage_limit_reached rate_limit_error 2056"}}"#,
            ),
            (
                400,
                r#"{"request":{"base_resp":{"status_code":2056},"error":{"type":"rate_limit_error"}}}"#,
            ),
            (404, r#"{"error":{"type":"rate_limit_error"}}"#),
            (200, r#"{"base_resp":{"status_code":2056}}"#),
            (400, "usage_limit_reached"),
        ] {
            assert_eq!(
                upstream_quota_retriable(status, body),
                None,
                "{status}: {body}"
            );
            assert_ne!(
                classify_upstream_4xx(status, body),
                Some(UpstreamErrorKind::QuotaExceeded)
            );
            let error = make_provider_error(
                "upstream_http_error",
                "response_status",
                Some(status),
                upstream_quota_retriable(status, body),
                None,
            );
            assert!(!error.retriable);
        }
    }

    #[test]
    fn ordinary_4xx_failures_stay_terminal() {
        for status in [400, 401, 402, 403, 404, 409, 413, 415, 422] {
            assert!(
                !default_retriable(Some(status)),
                "{status} must not be replayed on another supplier"
            );
        }
        // Rate limits and upstream outages always fail over.
        assert!(default_retriable(Some(429)));
        assert!(default_retriable(Some(500)));
        assert!(default_retriable(Some(503)));
        // A transport error carries no status and fails over.
        assert!(default_retriable(None));
    }

    /// The default applies only when the caller did not decide; an explicit
    /// decision always wins.
    #[test]
    fn explicit_retry_decisions_win_over_the_default() {
        let forced: ProviderError = make_provider_error(
            "upstream_http_error",
            "response_status",
            Some(400),
            Some(true),
            None,
        );
        assert!(forced.retriable, "an explicit decision is never overridden");
        let forced_off: ProviderError = make_provider_error(
            "upstream_http_error",
            "response_status",
            Some(503),
            Some(false),
            None,
        );
        assert!(!forced_off.retriable);
        // Without an explicit decision the status decides.
        let defaulted: ProviderError = make_provider_error(
            "upstream_http_error",
            "response_status",
            Some(402),
            None,
            None,
        );
        assert!(!defaulted.retriable);
        assert_eq!(defaulted.status, Some(402));
    }
}

#[cfg(test)]
mod hop_body_tests {
    use super::{ChatCompletionRequest, ChatMessage, Client_credential, Hop_body, Stream_options};

    #[test]
    fn canonical_chat_wire_excludes_only_native_gemini_call_signatures() {
        let message = ChatMessage {
            role: "assistant".into(),
            content: serde_json::Value::Null,
            tool_calls: Some(vec![serde_json::json!({
                "id":"call_fixture","type":"function","function":{"name":"lookup","arguments":"{}"},
                "thoughtSignature":"native_signature","thought_signature":"native_sdk_signature",
                "vendor_extension":{"preserve":true}
            })]),
            tool_call_id: None,
            extra: Default::default(),
        };
        let request = ChatCompletionRequest {
            messages: vec![message],
            source_gemini_request: Some(serde_json::json!({"opaque":"request"})),
            ..Default::default()
        };
        let wire = serde_json::to_value(&request).unwrap();
        let call = &wire["messages"][0]["tool_calls"][0];
        assert!(call.get("thoughtSignature").is_none());
        assert!(call.get("thought_signature").is_none());
        assert_eq!(call["vendor_extension"]["preserve"], true);
        assert_eq!(call["id"], "call_fixture");
        assert!(wire.get("source_gemini_request").is_none());
        assert_eq!(
            request.messages[0].tool_calls.as_ref().unwrap()[0]["thoughtSignature"],
            "native_signature"
        );
    }

    /// The hop view must serialize to exactly the canonical request, field
    /// for field. It carries the same payload by reference and adds only
    /// the per-hop `model` / `stream` / `stream_options`; a canonical field
    /// that is not mirrored here would silently stop reaching the wire, so
    /// this comparison is the drift guard for that mirror.
    #[test]
    fn hop_body_matches_the_canonical_wire_shape() {
        let mut request = ChatCompletionRequest {
            model: "client-alias".into(),
            messages: vec![
                ChatMessage {
                    role: "system".into(),
                    content: serde_json::json!("be brief"),
                    tool_calls: None,
                    tool_call_id: None,
                    extra: Default::default(),
                },
                ChatMessage {
                    role: "user".into(),
                    content: serde_json::json!([
                        {"type": "text", "text": "hi"},
                        {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"}},
                    ]),
                    tool_calls: Some(vec![serde_json::json!({
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "lookup", "arguments": "{}"},
                    })]),
                    tool_call_id: Some("call_1".into()),
                    extra: [("name".to_string(), serde_json::json!("assistant"))]
                        .into_iter()
                        .collect(),
                },
            ],
            stream: true,
            temperature: Some(0.25),
            max_tokens: Some(256),
            tools: Some(vec![serde_json::json!({
                "type": "function",
                "function": {"name": "lookup"},
            })]),
            tool_choice: Some(serde_json::json!("auto")),
            extra: [
                ("top_p".to_string(), serde_json::json!(0.9)),
                ("reasoning_effort".to_string(), serde_json::json!("high")),
            ]
            .into_iter()
            .collect(),
            // Internal routing context: never part of either wire shape.
            source_anthropic_request: Some(serde_json::json!({"model": "claude"})),
            source_responses_request: None,
            source_gemini_request: None,
            request_id: Some("req_1".into()),
            client_credential: Some(Client_credential::default()),
        };
        // Every optional wire field is populated above; assert that, so a
        // future field added as `Option` cannot silently go uncovered here.
        assert!(request.tools.is_some() && request.tool_choice.is_some());

        let canonical = serde_json::to_value(&request).expect("canonical serializes");
        let hop = Hop_body {
            model: &request.model,
            messages: &request.messages,
            stream: request.stream,
            temperature: request.temperature,
            max_tokens: request.max_tokens,
            tools: request.tools.as_deref(),
            tool_choice: request.tool_choice.as_ref(),
            extra: &request.extra,
            stream_options: None,
        };
        let mirrored = serde_json::to_value(hop).expect("hop body serializes");

        assert_eq!(mirrored, canonical);
        // The per-hop overrides are the ONLY intended difference.
        request.stream = false;
        let overridden = serde_json::to_value(Hop_body {
            model: "upstream-model",
            messages: &request.messages,
            stream: true,
            temperature: request.temperature,
            max_tokens: request.max_tokens,
            tools: request.tools.as_deref(),
            tool_choice: request.tool_choice.as_ref(),
            extra: &request.extra,
            stream_options: Some(Stream_options {
                include_usage: true,
            }),
        })
        .expect("hop body serializes");
        assert_eq!(overridden["model"], serde_json::json!("upstream-model"));
        assert_eq!(overridden["stream"], serde_json::json!(true));
        assert_eq!(
            overridden["stream_options"],
            serde_json::json!({"include_usage": true})
        );
    }
}
