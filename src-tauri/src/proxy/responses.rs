//! OpenAI Responses API request/response types.
//!
//! The Responses API (`POST /v1/responses`) is the newer OpenAI endpoint
//! that unifies chat completions + tool use + structured outputs. Codex CLI
//! and the Codex Desktop app speak this shape natively, so a transparent
//! gateway needs to:
//! 1. Parse a `ResponsesRequest` from the client.
//! 2. Convert it to a canonical `ChatCompletionRequest`.
//! 3. Run it through the existing Router (failover, circuit breakers,
//!    per-provider adapters) — the response is back in canonical OAI form.
//! 4. Convert the canonical OAI response back to a `ResponsesResponse`.
//!
//! This file is the wire shape; the conversion logic lives in `converter.rs`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// OpenAI Responses API request body. Accepts both the simpler `input: string`
/// form (one-shot) and the array-of-items form (multi-turn).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponsesRequest {
    pub model: String,
    /// Either a plain string (treated as a single user message) or an array
    /// of `ResponseInputItem` (multi-turn — `role` + `content`).
    pub input: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(default)]
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "max_output_tokens")]
    pub max_output_tokens: Option<u32>,
    /// Responses-API tool definitions (flat `{type, name, parameters, ...}`
    /// shape). Translated to the Chat Completions nested
    /// `{type, function: {...}}` shape by `responses_to_chat`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Value>,
    /// Pass-through metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
    /// Dropped — we're stateless (no conversation server-side).
    #[serde(
        skip_serializing_if = "Option::is_none",
        rename = "previous_response_id"
    )]
    pub previous_response_id: Option<String>,
    /// Capture any extra fields to pass through (`tool_choice`,
    /// `parallel_tool_calls`, `reasoning`, `store`, ...).
    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, Value>,
}

/// OpenAI Responses API non-streaming response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponsesResponse {
    pub id: String,
    pub object: String, // "response"
    #[serde(rename = "created_at")]
    pub created_at: i64,
    pub model: String,
    pub status: String, // "completed" | "incomplete" | "failed"
    pub output: Vec<ResponseOutputItem>,
    pub usage: ResponsesUsage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseOutputItem {
    #[serde(rename = "type")]
    pub item_type: String, // "message" | "function_call"
    /// Present on `message` items; omitted on `function_call` items (the
    /// Responses wire shape has no role there).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub role: Option<String>,
    /// `message` content parts. Empty (and omitted on the wire) for
    /// `function_call` items.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub content: Vec<ResponseOutputContent>,
    /// Pass-through for future item types. Also carries the
    /// `function_call` fields (`call_id` / `name` / `arguments` / `status`).
    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseOutputContent {
    #[serde(rename = "type")]
    pub content_type: String, // "output_text"
    pub text: String,
}

/// Usage numbers on a Responses API response (`input_tokens` /
/// `output_tokens` / `total_tokens`). Optional detail objects are echoed
/// through when the upstream supplied them.

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponsesUsage {
    #[serde(rename = "input_tokens")]
    pub input_tokens: u32,
    #[serde(rename = "output_tokens")]
    pub output_tokens: u32,
    #[serde(rename = "total_tokens")]
    pub total_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens_details: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens_details: Option<Value>,
}

/// Build the `response` object shared by `response.created`,
/// `response.completed` and `response.failed` payloads, plus the
/// non-streaming response body.
///
/// Emits the documented Response-object fields (with explicit nulls where
/// the gateway is stateless) so strict clients — Codex CLI included — can
/// deserialize the envelope without `#[serde(default)]` leniency.
#[allow(clippy::too_many_arguments)]
pub fn response_envelope(
    id: &str,
    created_at: i64,
    model: &str,
    status: &str,
    output: Vec<Value>,
    usage: Option<ResponsesUsage>,
    instructions: Option<&str>,
    incomplete_details: Option<Value>,
    error: Option<Value>,
) -> Value {
    serde_json::json!({
        "id": id,
        "object": "response",
        "created_at": created_at,
        "status": status,
        "error": error,
        "incomplete_details": incomplete_details.unwrap_or(Value::Null),
        "instructions": instructions,
        "metadata": {},
        "model": model,
        "output": output,
        "parallel_tool_calls": true,
        "temperature": Value::Null,
        "tool_choice": "auto",
        "tools": [],
        "top_p": Value::Null,
        "background": false,
        "previous_response_id": Value::Null,
        "reasoning": { "effort": Value::Null, "summary": Value::Null },
        "store": false,
        "usage": usage.map(|u| serde_json::to_value(u).unwrap_or(Value::Null)),
        "user": Value::Null,
    })
}

/// Events emitted on the Responses API SSE stream. The wire shape is
/// `event: <type>\ndata: <json>\n\n` (NOT the chat-completions-style
/// `data: <json>` blob). The JSON payload's `type` field must equal the
/// event name — clients (Codex CLI included) deserialize the data payload
/// with `type` as the tag, so dotted wire names are applied per variant.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ResponsesStreamEvent {
    #[serde(rename = "response.created")]
    ResponseCreated { response: Value },
    #[serde(rename = "response.in_progress")]
    ResponseInProgress { response: Value },
    #[serde(rename = "response.output_item.added")]
    ResponseOutputItemAdded { output_index: u32, item: Value },
    #[serde(rename = "response.output_text.delta")]
    ResponseOutputTextDelta {
        item_id: String,
        output_index: u32,
        content_index: u32,
        delta: String,
    },
    #[serde(rename = "response.output_text.done")]
    ResponseOutputTextDone {
        item_id: String,
        output_index: u32,
        content_index: u32,
        text: String,
    },
    #[serde(rename = "response.reasoning_text.delta")]
    ResponseReasoningTextDelta {
        item_id: String,
        output_index: u32,
        content_index: u32,
        delta: String,
    },
    #[serde(rename = "response.function_call_arguments.delta")]
    ResponseFunctionCallArgumentsDelta {
        item_id: String,
        output_index: u32,
        delta: String,
    },
    #[serde(rename = "response.function_call_arguments.done")]
    ResponseFunctionCallArgumentsDone {
        item_id: String,
        output_index: u32,
        arguments: String,
    },
    #[serde(rename = "response.output_item.done")]
    ResponseOutputItemDone { output_index: u32, item: Value },
    #[serde(rename = "response.completed")]
    ResponseCompleted { response: Value },
    #[serde(rename = "response.failed")]
    ResponseFailed { response: Value },
}

/// Format a `ResponsesStreamEvent` as an SSE wire line.
///
/// Returns `event: <type>\ndata: <json>\n\n`. The Responses API uses named
/// events (not the unnamed `data: ...` events of chat-completions); the
/// event name matches the payload's `type` field.
pub fn stream_event_to_sse(event: &ResponsesStreamEvent) -> String {
    let event_name = match event {
        ResponsesStreamEvent::ResponseCreated { .. } => "response.created",
        ResponsesStreamEvent::ResponseInProgress { .. } => "response.in_progress",
        ResponsesStreamEvent::ResponseOutputItemAdded { .. } => "response.output_item.added",
        ResponsesStreamEvent::ResponseOutputTextDelta { .. } => "response.output_text.delta",
        ResponsesStreamEvent::ResponseOutputTextDone { .. } => "response.output_text.done",
        ResponsesStreamEvent::ResponseReasoningTextDelta { .. } => "response.reasoning_text.delta",
        ResponsesStreamEvent::ResponseFunctionCallArgumentsDelta { .. } => {
            "response.function_call_arguments.delta"
        }
        ResponsesStreamEvent::ResponseFunctionCallArgumentsDone { .. } => {
            "response.function_call_arguments.done"
        }
        ResponsesStreamEvent::ResponseOutputItemDone { .. } => "response.output_item.done",
        ResponsesStreamEvent::ResponseCompleted { .. } => "response.completed",
        ResponsesStreamEvent::ResponseFailed { .. } => "response.failed",
    };
    let json = serde_json::to_string(event).unwrap_or_else(|_| "{}".into());
    format!("event: {event_name}\ndata: {json}\n\n")
}

/// Streaming completion sentinel. The Responses API uses bare
/// `data: [DONE]\n\n` (not the chat-completions undefined-event variant).
pub const STREAM_DONE: &str = "data: [DONE]\n\n";

#[cfg(test)]
mod tests {
    use super::*;

    /// The payload's `type` field must equal the SSE event name for every
    /// variant — Codex CLI deserializes the data payload with `type` as the
    /// tag, so a snake_case `"response_created"` (the pre-fix serialization)
    /// silently dropped every event.
    #[test]
    fn event_payload_type_matches_event_name() {
        let events = vec![
            ResponsesStreamEvent::ResponseCreated {
                response: serde_json::json!({}),
            },
            ResponsesStreamEvent::ResponseInProgress {
                response: serde_json::json!({}),
            },
            ResponsesStreamEvent::ResponseOutputItemAdded {
                output_index: 0,
                item: serde_json::json!({}),
            },
            ResponsesStreamEvent::ResponseOutputTextDelta {
                item_id: "msg_1".into(),
                output_index: 0,
                content_index: 0,
                delta: "a".into(),
            },
            ResponsesStreamEvent::ResponseOutputTextDone {
                item_id: "msg_1".into(),
                output_index: 0,
                content_index: 0,
                text: "a".into(),
            },
            ResponsesStreamEvent::ResponseReasoningTextDelta {
                item_id: "rs_1".into(),
                output_index: 0,
                content_index: 0,
                delta: "a".into(),
            },
            ResponsesStreamEvent::ResponseFunctionCallArgumentsDelta {
                item_id: "fc_1".into(),
                output_index: 1,
                delta: "{\"a\"".into(),
            },
            ResponsesStreamEvent::ResponseFunctionCallArgumentsDone {
                item_id: "fc_1".into(),
                output_index: 1,
                arguments: "{\"a\":1}".into(),
            },
            ResponsesStreamEvent::ResponseOutputItemDone {
                output_index: 1,
                item: serde_json::json!({}),
            },
            ResponsesStreamEvent::ResponseCompleted {
                response: serde_json::json!({}),
            },
            ResponsesStreamEvent::ResponseFailed {
                response: serde_json::json!({}),
            },
        ];
        for event in events {
            let wire = stream_event_to_sse(&event);
            let event_name = wire
                .strip_prefix("event: ")
                .and_then(|rest| rest.split('\n').next())
                .expect("event line present")
                .to_string();
            let data = wire
                .split("data: ")
                .nth(1)
                .and_then(|rest| rest.split('\n').next())
                .expect("data line present");
            let payload: Value = serde_json::from_str(data).expect("valid json payload");
            assert_eq!(
                payload.get("type").and_then(Value::as_str),
                Some(event_name.as_str()),
                "payload type must equal event name for {event_name}"
            );
        }
    }

    #[test]
    fn response_envelope_includes_stateless_nulls() {
        let envelope = response_envelope(
            "resp_1",
            1_700_000_000,
            "gpt-5",
            "completed",
            vec![serde_json::json!({"type": "message"})],
            Some(ResponsesUsage {
                input_tokens: 5,
                output_tokens: 7,
                total_tokens: 12,
                input_tokens_details: None,
                output_tokens_details: None,
            }),
            Some("be brief"),
            None,
            None,
        );
        assert_eq!(envelope["object"], "response");
        assert_eq!(envelope["status"], "completed");
        assert!(
            envelope["error"].is_null(),
            "stateless gateway has no error"
        );
        assert_eq!(envelope["usage"]["input_tokens"], 5);
        assert_eq!(envelope["output"].as_array().unwrap().len(), 1);
    }
}
