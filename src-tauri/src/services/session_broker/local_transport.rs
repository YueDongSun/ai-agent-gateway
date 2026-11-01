//! Private Windows pipe transport. Identity/framing/lifecycle only: no session
//! policy, business dispatch, approvals, model calls, or protocol-body logging.

mod codec;
mod ffi;
#[cfg(test)]
mod fixture;
mod lifecycle;
mod pipe;
#[cfg(test)]
mod tests;

/// Synthetic gateway transport only; this creates no service identity proof.
#[cfg(test)]
pub(crate) fn fixture_duplex_for_gateway(
    stream: tokio::io::DuplexStream,
    limits: TransportLimits,
) -> Result<LocalConnection, TransportError> {
    fixture::fixture_duplex(stream, limits)
}

use super::service_host::{ServiceContext, ServiceRole};
use super::windows_isolation::Sid;
pub(crate) use codec::{EnvelopeKind, TransportFrame};
use std::{
    fmt,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Instant,
};
use tokio::sync::{mpsc, Mutex, Notify, Semaphore};

const MAX_FRAME_BYTES: usize = 32 * 1024 * 1024;
const MAX_QUEUE_BYTES: usize = 64 * 1024 * 1024;
const MAX_QUEUE_FRAMES: usize = 16;
const MAX_CONNECTIONS: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TransportError {
    InvalidEndpoint,
    InvalidLimits,
    ServiceContextRequired,
    CreatorIdentityMismatch,
    PeerOwnerMismatch,
    NamespaceOccupied,
    EndpointUnavailable,
    PipeBusy,
    InvalidFrame,
    InvalidUtf8,
    FrameTooLarge,
    TruncatedFrame,
    Closed,
    Cancelled,
    DeadlineExceeded,
    CapacityExceeded,
    RuntimeUnavailable,
    Kernel {
        operation: KernelOperation,
        code: u32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KernelOperation {
    Identity,
    Descriptor,
    Create,
    Open,
    Attest,
    Accept,
    Read,
    Write,
    Join,
}

impl fmt::Display for TransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let code = match self {
            Self::InvalidEndpoint => "local_pipe_endpoint_invalid",
            Self::InvalidLimits => "local_pipe_limits_invalid",
            Self::ServiceContextRequired => "local_pipe_service_context_required",
            Self::CreatorIdentityMismatch => "local_pipe_creator_identity_mismatch",
            Self::PeerOwnerMismatch => "local_pipe_peer_owner_mismatch",
            Self::NamespaceOccupied => "local_pipe_namespace_occupied",
            Self::EndpointUnavailable => "local_pipe_endpoint_unavailable",
            Self::PipeBusy => "local_pipe_busy",
            Self::InvalidFrame => "local_pipe_frame_invalid",
            Self::InvalidUtf8 => "local_pipe_utf8_invalid",
            Self::FrameTooLarge => "local_pipe_frame_too_large",
            Self::TruncatedFrame => "local_pipe_frame_truncated",
            Self::Closed => "local_pipe_closed",
            Self::Cancelled => "local_pipe_cancelled",
            Self::DeadlineExceeded => "local_pipe_deadline_exceeded",
            Self::CapacityExceeded => "local_pipe_capacity_exceeded",
            Self::RuntimeUnavailable => "local_pipe_runtime_unavailable",
            Self::Kernel { .. } => "local_pipe_kernel_failure",
        };
        formatter.write_str(code)
    }
}
impl std::error::Error for TransportError {}

#[derive(Debug, Clone, Copy)]
pub(crate) struct TransportLimits {
    pub frame_bytes: usize,
    pub queued_bytes: usize,
    pub queued_frames: usize,
    pub connections: usize,
}
impl Default for TransportLimits {
    fn default() -> Self {
        Self {
            frame_bytes: MAX_FRAME_BYTES,
            queued_bytes: MAX_QUEUE_BYTES,
            queued_frames: 8,
            connections: 8,
        }
    }
}
impl TransportLimits {
    fn validate(&self) -> Result<(), TransportError> {
        if self.frame_bytes == 0
            || self.frame_bytes > MAX_FRAME_BYTES
            || self.queued_bytes < self.frame_bytes + 4
            || self.queued_bytes > MAX_QUEUE_BYTES
            || self.queued_frames == 0
            || self.queued_frames > MAX_QUEUE_FRAMES
            || self.connections == 0
            || self.connections > MAX_CONNECTIONS
        {
            return Err(TransportError::InvalidLimits);
        }
        Ok(())
    }
}

/// Names come only from the already validated service role/source/generation.
#[derive(Clone)]
pub(crate) struct PipeName {
    value: String,
}
impl PipeName {
    pub fn for_role(role: ServiceRole) -> Result<Self, TransportError> {
        Ok(Self {
            value: format!(r"\\.\pipe\{}_rpc_v1", role.service_name()),
        })
    }
    pub fn as_str(&self) -> &str {
        &self.value
    }
}

/// Expected identity supplied by the protected catalog, not a peer's message.
/// This is not an attestation until connect reads the actual pipe object owner.
pub(crate) struct ServicePeer {
    role: ServiceRole,
    owner_sid: Sid,
    name: PipeName,
}
impl ServicePeer {
    pub fn new(role: ServiceRole, owner_sid: Sid) -> Result<Self, TransportError> {
        let valid = owner_sid
            .value()
            .strip_prefix("S-1-5-80-")
            .is_some_and(|suffix| {
                let components: Vec<_> = suffix.split('-').collect();
                components.len() == 5 && components.iter().all(|part| part.parse::<u32>().is_ok())
            });
        if !valid {
            return Err(TransportError::InvalidEndpoint);
        }
        Ok(Self {
            role,
            owner_sid,
            name: PipeName::for_role(role)?,
        })
    }
    pub fn role(&self) -> ServiceRole {
        self.role
    }
    pub fn owner_sid(&self) -> &Sid {
        &self.owner_sid
    }
    pub fn pipe_name(&self) -> &PipeName {
        &self.name
    }
}

struct CancelState {
    cancelled: AtomicBool,
    wake: Notify,
}
#[derive(Clone)]
pub(crate) struct TransportCancellation {
    inner: Arc<CancelState>,
}
impl TransportCancellation {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(CancelState {
                cancelled: AtomicBool::new(false),
                wake: Notify::new(),
            }),
        }
    }
    pub fn cancel(&self) {
        self.inner.cancelled.store(true, Ordering::SeqCst);
        self.inner.wake.notify_waiters();
    }
    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::SeqCst)
    }
    async fn cancelled(&self) {
        loop {
            let notified = self.inner.wake.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }
}

pub(crate) struct PeerAttestation {
    owner_sid: Sid,
    role: Option<ServiceRole>,
    peer_process_id: Option<u32>,
}
impl PeerAttestation {
    /// Kernel pipe-object owner. For an accepted connection this is the local
    /// listener owner, never a claim about the connecting client's TokenUser.
    pub fn owner_sid(&self) -> &Sid {
        &self.owner_sid
    }
    /// Protected-catalog endpoint role, not the identity of an incoming caller.
    pub fn role(&self) -> Option<ServiceRole> {
        self.role
    }
    /// Kernel observation only; matching SCM PID/source epoch belongs to caller.
    pub fn peer_process_id(&self) -> Option<u32> {
        self.peer_process_id
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SendOutcome {
    /// Every frame byte was written, not a business/RPC success acknowledgement.
    Written,
    NotSent(TransportError),
    OutcomeUnknown(TransportError),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TransportClosed {
    Closed,
    OutcomeUnknown(TransportError),
}

pub(crate) struct LocalListener {
    control: Arc<lifecycle::TaskControl>,
    incoming: Arc<Mutex<mpsc::Receiver<LocalConnection>>>,
}
impl LocalListener {
    pub fn bind(
        context: &ServiceContext,
        peer: ServicePeer,
        clients: &[Sid],
        limits: TransportLimits,
    ) -> Result<Self, TransportError> {
        pipe::bind(context, peer, clients, limits)
    }
    pub async fn accept(&self, deadline: Instant) -> Result<LocalConnection, TransportError> {
        tokio::select! {
            biased;
            _ = self.control.cancel.cancelled() => Err(self.control.error()),
            _ = tokio::time::sleep_until(deadline.into()) => Err(TransportError::DeadlineExceeded),
            value = async { self.incoming.lock().await.recv().await } => value.ok_or_else(|| self.control.error()),
        }
    }
    pub fn cancellation(&self) -> TransportCancellation {
        self.control.cancel.clone()
    }
    pub async fn close(&self, deadline: Instant) -> TransportClosed {
        self.control.close(deadline).await
    }
}
impl Drop for LocalListener {
    fn drop(&mut self) {
        self.control.abort();
    }
}

pub(crate) struct LocalConnection {
    peer: PeerAttestation,
    limits: TransportLimits,
    control: Arc<lifecycle::TaskControl>,
    outgoing: mpsc::Sender<lifecycle::OutgoingFrame>,
    outgoing_bytes: Arc<Semaphore>,
    incoming: Mutex<mpsc::Receiver<lifecycle::IncomingFrame>>,
}
impl LocalConnection {
    /// The supplied cancellation bounds connection establishment. Once returned,
    /// the caller owns the connection and its separate cancellation handle.
    pub async fn connect(
        peer: &ServicePeer,
        limits: TransportLimits,
        deadline: Instant,
        cancel: &TransportCancellation,
    ) -> Result<Self, TransportError> {
        pipe::connect(
            &peer.name,
            &peer.owner_sid,
            Some(peer.role),
            limits,
            deadline,
            cancel,
        )
        .await
    }
    pub fn peer(&self) -> &PeerAttestation {
        &self.peer
    }
    pub async fn send(&self, frame: TransportFrame, deadline: Instant) -> SendOutcome {
        lifecycle::send(self, frame, deadline).await
    }
    pub async fn receive(&self, deadline: Instant) -> Result<TransportFrame, TransportError> {
        // Drain an already admitted frame before reporting the terminal read error.
        let mut incoming = tokio::select! {
            biased;
            value = self.incoming.lock() => value,
            _ = tokio::time::sleep_until(deadline.into()) => return Err(TransportError::DeadlineExceeded),
        };
        match incoming.try_recv() {
            Ok(value) => return Ok(value.frame),
            Err(mpsc::error::TryRecvError::Disconnected) => return Err(self.control.error()),
            Err(mpsc::error::TryRecvError::Empty) => {}
        }
        tokio::select! {
            biased;
            value = incoming.recv() => value.map(|value| value.frame).ok_or_else(|| self.control.error()),
            _ = self.control.cancel.cancelled() => Err(self.control.error()),
            _ = tokio::time::sleep_until(deadline.into()) => Err(TransportError::DeadlineExceeded),
        }
    }
    pub fn cancellation(&self) -> TransportCancellation {
        self.control.cancel.clone()
    }
    pub async fn close(&self, deadline: Instant) -> TransportClosed {
        self.control.close(deadline).await
    }
}
impl Drop for LocalConnection {
    fn drop(&mut self) {
        self.control.abort();
    }
}
