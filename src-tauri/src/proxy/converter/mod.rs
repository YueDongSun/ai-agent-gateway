//! Bidirectional format conversion for the proxy's canonical OpenAI
//! Chat-Completions shape.
//!
//! Package layout (split out of the former 4,034-line converter.rs):
//! - [`anthropic`]: Anthropic Messages API shapes and non-streaming
//!   request/response conversion. Two directions: the gateway edge
//!   (Anthropic client request -> canonical OpenAI -> provider, then
//!   OpenAI response -> Anthropic response back to the client) and the
//!   Anthropic provider side (canonical -> Anthropic backend and back).
//! - [`anthropic_stream`]: streaming conversion between canonical OAI
//!   chunks and Anthropic SSE lines, with full tool-use fidelity
//!   (tool_calls delta -> input_json_delta; see
//!   `cc-switch/providers/streaming.rs` for the upstream reference
//!   design).
//! - [`responses`]: OpenAI Responses API (Codex `/v1/responses`) <->
//!   Chat Completions conversion.
//!
//! All historical public paths (`crate::proxy::converter::X`) are
//! preserved through glob re-exports.

mod anthropic;
mod anthropic_stream;
mod responses;
mod responses_namespace;

pub use anthropic::*;
#[cfg(test)]
pub(crate) use anthropic::{
    oai_tool_to_anthropic_tool, text_of, tool_choice_anthropic_to_oai, tool_choice_oai_to_anthropic,
};
pub use anthropic_stream::*;
pub use responses::*;
pub use responses_namespace::Responses_namespace_map;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod responses_tests;

#[cfg(test)]
#[path = "../converter_protocol_tests.rs"]
mod anthropic_tool_boundary_tests;
