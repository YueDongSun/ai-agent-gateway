//! Authenticated WebSocket facade for the protected native session authority.
//! The relay carries opaque RPC envelopes and never supplies business authority.

use super::{publication::PublicationUpgradeOwner, server::ProxyAppState};
use crate::services::session_broker::local_transport::{
    LocalConnection, SendOutcome, TransportClosed, TransportError, TransportFrame,
};
use axum::{
    extract::{ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade}, FromRequestParts, Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{any, MethodRouter},
};
use futures_util::{SinkExt, StreamExt};
use std::{
    borrow::Cow,
    future::Future,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
#[cfg(test)]
use std::sync::Mutex;
use tokio::{sync::watch, task::JoinSet};

pub(crate) const PATH: &str = "/aag/codex";
const FRAME_BYTES: usize = 32 * 1024 * 1024;
const MAX_LOCAL_HANDLERS: usize = 64;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);
const RECEIVE_INTERVAL: Duration = Duration::from_secs(30);
#[cfg(not(test))]
const SEND_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(test)]
const SEND_TIMEOUT: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionGatewayClosed {
    Closed,
    OutcomeUnknown,
}
struct GatewayState {
    handlers: JoinSet<SessionGatewayClosed>,
    completed: Option<SessionGatewayClosed>,
    failed: bool,
}
struct GatewayControl {
    cancelled: AtomicBool,
    signal: watch::Sender<bool>,
    state: tokio::sync::Mutex<GatewayState>,
}

/// The listener supplies this extension and cancels it before waiting for HTTP
/// graceful shutdown. Local relay tasks stay owned until close() reconciles them.
#[derive(Clone)]
pub(crate) struct SessionGatewayCancellation {
    control: Arc<GatewayControl>,
}
impl SessionGatewayCancellation {
    pub fn new() -> Self {
        let (signal, _) = watch::channel(false);
        Self {
            control: Arc::new(GatewayControl {
                cancelled: AtomicBool::new(false),
                signal,
                state: tokio::sync::Mutex::new(GatewayState {
                    handlers: JoinSet::new(),
                    completed: None,
                    failed: false,
                }),
            }),
        }
    }
    pub fn cancel(&self) {
        self.control.cancelled.store(true, Ordering::SeqCst);
        self.control.signal.send_replace(true);
    }
    pub fn is_cancelled(&self) -> bool {
        self.control.cancelled.load(Ordering::SeqCst)
    }
    pub async fn close(&self, deadline: Instant) -> SessionGatewayClosed {
        self.cancel();
        let Ok(mut state) = tokio::time::timeout_at(deadline.into(), self.control.state.lock()).await else {
            return SessionGatewayClosed::OutcomeUnknown;
        };
        if let Some(completed) = state.completed { return completed; }
        while !state.handlers.is_empty() {
            match tokio::time::timeout_at(deadline.into(), state.handlers.join_next()).await {
                Ok(Some(Ok(SessionGatewayClosed::Closed))) => {},
                Ok(Some(_)) => state.failed = true,
                Ok(None) => break,
                Err(_) => {
                    // Keep the aborted JoinSet in its owner for a later close
                    // reconciliation. A timeout never fabricates joined work.
                    state.handlers.abort_all();
                    return SessionGatewayClosed::OutcomeUnknown;
                }
            }
        }
        let completed = if state.failed { SessionGatewayClosed::OutcomeUnknown } else { SessionGatewayClosed::Closed };
        state.completed = Some(completed);
        completed
    }
    async fn cancelled(&self) {
        let mut signal = self.control.signal.subscribe();
        while !*signal.borrow_and_update() {
            if signal.changed().await.is_err() { break; }
        }
    }
    async fn spawn<F>(&self, handler: F) -> Result<(), ()>
    where F: Future<Output = SessionGatewayClosed> + Send + 'static {
        if self.is_cancelled() { return Err(()); }
        let mut state = self.control.state.lock().await;
        if self.is_cancelled() || state.completed.is_some() { return Err(()); }
        while let Some(result) = state.handlers.try_join_next() {
            if !matches!(result, Ok(SessionGatewayClosed::Closed)) { state.failed = true; }
        }
        if state.handlers.len() >= MAX_LOCAL_HANDLERS { return Err(()); }
        state.handlers.spawn(handler);
        Ok(())
    }
}

pub(crate) fn route() -> MethodRouter<ProxyAppState> {
    any(upgrade)
}
async fn upgrade(State(state): State<ProxyAppState>, request: Request) -> Response {
    let state = state.capture_snapshot();
    // The native facade always needs the configured gateway key. The local
    // Anthropic subscription exception does not apply to this route.
    if state.auth_token.trim().is_empty() { return unavailable(); }
    if let Err(response) = super::server::check_auth(request.headers(), &state.auth_token) { return *response; }
    let Some(cancel) = request.extensions().get::<SessionGatewayCancellation>().cloned() else { return unavailable(); };
    if cancel.is_cancelled() { return unavailable(); }
    let publication = request.extensions().get::<PublicationUpgradeOwner>().cloned();
    let (mut parts, _) = request.into_parts();
    let upgrade = match WebSocketUpgrade::from_request_parts(&mut parts, &state).await {
        Ok(upgrade) => upgrade,
        Err(rejection) => return rejection.into_response(),
    };
    let connection = tokio::select! {
        biased;
        _ = cancel.cancelled() => return unavailable(),
        result = tokio::time::timeout(CONNECT_TIMEOUT, connect(&parts.extensions)) => match result {
            Ok(Ok(connection)) => connection,
            _ => return unavailable(),
        },
    };
    if cancel.is_cancelled() {
        let _ = connection.close(Instant::now() + CLOSE_TIMEOUT).await;
        return unavailable();
    }
    upgrade.max_message_size(FRAME_BYTES).max_frame_size(FRAME_BYTES)
        .write_buffer_size(0).max_write_buffer_size(FRAME_BYTES * 2)
        .on_upgrade(move |socket| async move {
            let relay_cancel = cancel.clone();
            let relay = async move { relay(socket, connection, relay_cancel).await };
            if let Some(owner) = publication {
                // Submit the whole socket/pipe-owning future. Publication stop
                // aborts and joins it; LocalConnection Drop aborts its driver
                // if that exceptional path interrupts asynchronous cleanup.
                let _ = owner.spawn(async move { let _ = relay.await; });
            } else {
                let _ = cancel.spawn(relay).await;
            }
        })
}

fn unavailable() -> Response {
    (StatusCode::SERVICE_UNAVAILABLE, "session_authority_unavailable").into_response()
}
async fn connect(_extensions: &axum::http::Extensions) -> Result<LocalConnection, String> {
    #[cfg(test)]
    if let Some(connector) = _extensions.get::<TestConnector>() { return connector.0.connect().await; }
    crate::services::session_authority::connect_gateway_client().await
}

#[derive(Clone, Copy)]
enum RelayEnd { Stopped, PeerClosed, InvalidFrame, BinaryFrame, PipeUnavailable, DeliveryUncertain, SocketUnavailable }
impl RelayEnd {
    fn close_frame(self) -> CloseFrame<'static> {
        let (code, reason) = match self {
            Self::Stopped => (1001, "gateway_stopped"),
            Self::PeerClosed => (1000, "peer_closed"),
            Self::InvalidFrame => (1008, "invalid_rpc_frame"),
            Self::BinaryFrame => (1003, "text_rpc_required"),
            Self::DeliveryUncertain => (1011, "pipe_delivery_uncertain"),
            Self::PipeUnavailable => (1011, "authority_disconnected"),
            Self::SocketUnavailable => (1011, "websocket_unavailable"),
        };
        CloseFrame { code, reason: Cow::Borrowed(reason) }
    }
}

async fn relay(socket: WebSocket, connection: LocalConnection, cancel: SessionGatewayCancellation) -> SessionGatewayClosed {
    let (mut outgoing, mut incoming) = socket.split();
    let ended = {
        let from_websocket = async {
            loop {
                let text = match incoming.next().await {
                    Some(Ok(Message::Text(text))) => text,
                    Some(Ok(Message::Binary(_))) => return RelayEnd::BinaryFrame,
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
                    Some(Ok(Message::Close(_))) | None => return RelayEnd::PeerClosed,
                    Some(Err(_)) => return RelayEnd::SocketUnavailable,
                };
                let frame = match TransportFrame::parse(text.into_bytes(), FRAME_BYTES) {
                    Ok(frame) => frame,
                    Err(_) => return RelayEnd::InvalidFrame,
                };
                match connection.send(frame, Instant::now() + SEND_TIMEOUT).await {
                    SendOutcome::Written => {},
                    SendOutcome::NotSent(_) => return RelayEnd::PipeUnavailable,
                    // Never replay a possibly admitted native request.
                    SendOutcome::OutcomeUnknown(_) => return RelayEnd::DeliveryUncertain,
                }
            }
        };
        let to_websocket = async {
            loop {
                let frame = match connection.receive(Instant::now() + RECEIVE_INTERVAL).await {
                    Ok(frame) => frame,
                    Err(TransportError::DeadlineExceeded) => continue,
                    Err(_) => return RelayEnd::PipeUnavailable,
                };
                let text = match std::str::from_utf8(frame.bytes()) {
                    Ok(text) => text.to_owned(),
                    Err(_) => return RelayEnd::InvalidFrame,
                };
                match tokio::time::timeout(SEND_TIMEOUT, outgoing.send(Message::Text(text))).await {
                    Ok(Ok(())) => {},
                    _ => return RelayEnd::SocketUnavailable,
                }
            }
        };
        tokio::pin!(from_websocket, to_websocket);
        tokio::select! {
            biased;
            _ = cancel.cancelled() => RelayEnd::Stopped,
            ended = &mut from_websocket => ended,
            ended = &mut to_websocket => ended,
        }
    };
    // Both directional futures have been dropped before cleanup. Closing never
    // creates another relay task or retries an uncertain client request.
    let deadline = Instant::now() + CLOSE_TIMEOUT;
    connection.cancellation().cancel();
    let (closed, _) = tokio::join!(
        connection.close(deadline),
        tokio::time::timeout_at(deadline.into(), outgoing.send(Message::Close(Some(ended.close_frame())))),
    );
    drop(outgoing);
    drop(incoming);
    match closed { TransportClosed::Closed => SessionGatewayClosed::Closed, _ => SessionGatewayClosed::OutcomeUnknown }
}

#[cfg(test)]
#[async_trait::async_trait]
trait TestConnectionFactory: Send + Sync {
    async fn connect(&self) -> Result<LocalConnection, String>;
}
#[cfg(test)]
#[derive(Clone)]
struct TestConnector(Arc<dyn TestConnectionFactory>);

#[cfg(test)]
mod tests;
