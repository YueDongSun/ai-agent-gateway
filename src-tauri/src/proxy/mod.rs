//! Local proxy subsystem.
//!
//! Provides an axum HTTP server with OpenAI-compatible and Anthropic-compatible
//! endpoints, routing requests through provider failover chains with per-provider
//! circuit breakers. Ported from the Node `src/gateway/` layer.

pub mod canonical;
pub mod admission;
pub mod circuit_breaker;
pub(crate) mod claude_desktop;
pub mod converter;
pub mod cot;
pub mod endpoint;
pub mod gemini;
pub mod models;
pub(crate) mod model_health;
pub mod planner;
pub(crate) mod prompt_budget;
pub mod providers;
pub(crate) mod publication;
#[cfg(windows)]
pub(crate) mod session_gateway;
pub mod responses;
pub(crate) mod request_timeline;
pub(crate) mod request_status;
pub mod router;
pub mod routing_receipts;
pub mod self_loop;
pub mod server;
pub mod session_affinity;
pub mod thinking_compat;
pub mod usage_recorder;

#[cfg(test)]
mod protocol_matrix_tests;

#[cfg(test)]
mod routing_receipt_tests;
#[cfg(test)]
mod routing_receipt_protocol_tests;
#[cfg(test)]
mod routing_admission_tests;

use std::sync::Arc;
use tokio::task::JoinHandle;

/// The `[1M]` context marker that Claude Code uses to advertise 1M-context
/// support. It's a local capability flag — upstream Anthropic-compatible
/// endpoints don't recognize the suffix and will 404 (or return an empty
/// SSE body) if it reaches them. Strip before forwarding.
///
/// Case-insensitive match on the trailing bracket token (`[1M]` / `[1m]`).
/// Whitespace before the marker is tolerated; the result is re-trimmed.
pub const ONE_M_CONTEXT_MARKER: &str = "[1M]";

pub fn strip_one_m_suffix(model: &str) -> String {
    let trimmed = model.trim_end();
    let marker = ONE_M_CONTEXT_MARKER.as_bytes();
    let bytes = trimmed.as_bytes();
    if bytes.len() >= marker.len()
        && bytes[bytes.len() - marker.len()..].eq_ignore_ascii_case(marker)
    {
        return trimmed[..trimmed.len() - marker.len()]
            .trim_end()
            .to_string();
    }
    model.to_string()
}

/// Handle to a running proxy server. Call `stop()` to shut it down.
#[allow(dead_code)]
pub struct ProxyHandle {
    pub config: crate::database::dao::proxy_config::ProxyConfig,
    pub shutdown_tx: tokio::sync::oneshot::Sender<()>,
    pub join: JoinHandle<()>,
}

#[allow(dead_code)]
impl ProxyHandle {
    /// Stop the proxy server (best-effort, errors if already stopped).
    pub fn stop(self) -> Result<(), String> {
        self.shutdown_tx
            .send(())
            .map_err(|_| "already stopped".into())
    }
}

/// Shared, immutable server state: the router (with all providers and routes)
/// is built once at proxy_start from the DB snapshot.
#[allow(dead_code)]
pub type ServerRouter = Arc<router::Router>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_one_m_suffix_removes_marker() {
        assert_eq!(strip_one_m_suffix("claude-fable-5[1M]"), "claude-fable-5");
        assert_eq!(strip_one_m_suffix("MiniMax-M3[1M]"), "MiniMax-M3");
        assert_eq!(strip_one_m_suffix("deepseek-v4-pro[1m]"), "deepseek-v4-pro");
        // tolerant of trailing whitespace before the marker
        assert_eq!(
            strip_one_m_suffix("deepseek-v4-pro [1M]"),
            "deepseek-v4-pro"
        );
        assert_eq!(
            strip_one_m_suffix("deepseek-v4-pro  [1m]"),
            "deepseek-v4-pro"
        );
        // passthrough when no marker
        assert_eq!(strip_one_m_suffix("claude-fable-5"), "claude-fable-5");
        // empty / whitespace-only edge cases
        assert_eq!(strip_one_m_suffix(""), "");
        assert_eq!(strip_one_m_suffix("[1M]"), "");
        // marker without preceding name still strips
        assert_eq!(strip_one_m_suffix("foo"), "foo");
        // unrelated brackets are preserved
        assert_eq!(strip_one_m_suffix("foo[1.5M]"), "foo[1.5M]");
        assert_eq!(strip_one_m_suffix("foo[2M]"), "foo[2M]");
    }
}
