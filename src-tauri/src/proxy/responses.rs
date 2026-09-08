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
    /// Pass-through. Not interpreted by the gateway.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Value>,
    /// Pass-through metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
    /// Dropped — we're stateless (no conversation server-side).
    #[serde(skip_serializing_if = "Option::is_none", rename = "previous_response_id")]
    pub previous_response_id: Option<String>,
    /// Capture any extra fields to pass through.
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
    pub item_type: String, // "message"
    pub role: String,
    pub content: Vec<ResponseOutputContent>,
    /// Pass-through for future item types.
    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseOutputContent {
    #[serde(rename = "type")]
    pub content_type: String, // "output_text"
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponsesUsage {
    #[serde(rename = "input_tokens")]
    pub input_tokens: u32,
    #[serde(rename = "output_tokens")]
    pub output_tokens: u32,
    #[serde(rename = "total_tokens")]
    pub total_tokens: u32,
}

/// Events emitted on the Responses API SSE stream. The wire shape is
/// `event: <type>\ndata: <json>\n\n` (NOT the chat-completions-style
/// `data: <json>` blob). Event types match the OpenAI spec.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponsesStreamEvent {
    ResponseCreated {
        response: serde_json::Value,
    },
    ResponseOutputItemAdded {
        output_index: u32,
        item: serde_json::Value,
    },
    ResponseOutputTextDelta {
        output_index: u32,
        content_index: u32,
        delta: String,
    },
    ResponseOutputItemDone {
        output_index: u32,
        item: serde_json::Value,
    },
    ResponseCompleted {
        response: serde_json::Value,
    },
}

/// Format a `ResponsesStreamEvent` as an SSE wire line.
///
/// Returns `event: <type>\ndata: <json>\n\n`. The Responses API uses named
/// events (not the unnamed `data: ...` events of chat-completions).
pub fn stream_event_to_sse(event: &ResponsesStreamEvent) -> String {
    let event_name = match event {
        ResponsesStreamEvent::ResponseCreated { .. } => "response.created",
        ResponsesStreamEvent::ResponseOutputItemAdded { .. } => "response.output_item.added",
        ResponsesStreamEvent::ResponseOutputTextDelta { .. } => "response.output_text.delta",
        ResponsesStreamEvent::ResponseOutputItemDone { .. } => "response.output_item.done",
        ResponsesStreamEvent::ResponseCompleted { .. } => "response.completed",
    };
    let json = serde_json::to_string(event).unwrap_or_else(|_| "{}".into());
    format!("event: {event_name}\ndata: {json}\n\n")
}

/// Streaming completion sentinel. The Responses API uses bare
/// `data: [DONE]\n\n` (not the chat-completions undefined-event variant).
pub const STREAM_DONE: &str = "data: [DONE]\n\n";
