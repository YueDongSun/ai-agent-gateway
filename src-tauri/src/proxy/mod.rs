//! Local proxy subsystem.
//!
//! Provides an axum HTTP server with OpenAI-compatible and Anthropic-compatible
//! endpoints, routing requests through provider failover chains with per-provider
//! circuit breakers. Ported from the Node `src/gateway/` layer.

pub mod canonical;
pub mod circuit_breaker;
pub mod converter;
pub mod models;
pub mod providers;
pub mod responses;
pub mod router;
pub mod server;

use std::sync::Arc;
use tokio::task::JoinHandle;

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
        self.shutdown_tx.send(()).map_err(|_| "already stopped".into())
    }
}

/// Shared, immutable server state: the router (with all providers and routes)
/// is built once at proxy_start from the DB snapshot.
#[allow(dead_code)]
pub type ServerRouter = Arc<router::Router>;
