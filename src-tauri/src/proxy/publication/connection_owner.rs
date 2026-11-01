//! Frontend-owned upgrade work. The transport installs this extension; routes
//! submit their complete upgrade handler future instead of detaching it.

use super::{PublicationCancellation, MAX_PUBLICATION_CONNECTIONS};
use std::{
    future::Future,
    io,
    net::{Shutdown, TcpStream as StdTcpStream},
    sync::{Arc, Mutex},
};
use tokio::{net::TcpStream, sync::Notify, task::JoinSet};

struct OwnerState {
    closed: bool,
    sockets: Vec<Option<StdTcpStream>>,
    handlers: Option<JoinSet<()>>,
}

struct OwnerControl {
    state: Mutex<OwnerState>,
    capacity_changed: Notify,
    cancellation: PublicationCancellation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PublicationUpgradeError {
    Cancelled,
    CapacityExceeded,
}

/// Delivered as `Extension<PublicationUpgradeOwner>` only by the publication
/// frontend. The upgrade callback submits its socket-owning future here.
#[derive(Clone)]
pub(crate) struct PublicationUpgradeOwner {
    control: Arc<OwnerControl>,
}

impl PublicationUpgradeOwner {
    pub(super) fn new(cancellation: PublicationCancellation) -> Self {
        Self {
            control: Arc::new(OwnerControl {
                state: Mutex::new(OwnerState {
                    closed: false,
                    sockets: (0..MAX_PUBLICATION_CONNECTIONS).map(|_| None).collect(),
                    handlers: Some(JoinSet::new()),
                }),
                capacity_changed: Notify::new(),
                cancellation,
            }),
        }
    }

    /// Register the complete socket-owning future. Stop aborts and joins these
    /// futures; separately detached tasks are outside this ownership contract.
    pub fn spawn<F>(&self, handler: F) -> Result<(), PublicationUpgradeError>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let mut state = self
            .control
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if state.closed || self.control.cancellation.is_cancelled() {
            return Err(PublicationUpgradeError::Cancelled);
        }
        let handlers = state
            .handlers
            .as_mut()
            .ok_or(PublicationUpgradeError::Cancelled)?;
        while handlers.try_join_next().is_some() {}
        if handlers.len() >= MAX_PUBLICATION_CONNECTIONS {
            return Err(PublicationUpgradeError::CapacityExceeded);
        }
        handlers.spawn(handler);
        Ok(())
    }

    pub(super) fn has_capacity(&self) -> bool {
        let state = self
            .control
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        !state.closed
            && !self.control.cancellation.is_cancelled()
            && state.sockets.iter().any(Option::is_none)
    }

    pub(super) async fn capacity_available(&self) {
        loop {
            let changed = self.control.capacity_changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.has_capacity() || self.control.cancellation.is_cancelled() {
                return;
            }
            changed.await;
        }
    }

    pub(super) fn attach(&self, stream: TcpStream) -> io::Result<(TcpStream, ConnectionGuard)> {
        let stream = stream.into_std()?;
        // This duplicates a handle to the same accepted socket. Shutdown on it
        // closes the real upgraded connection even after Hyper transfers IO.
        let shutdown_handle = stream.try_clone()?;
        let stream = TcpStream::from_std(stream)?;
        let mut state = self
            .control
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if state.closed || self.control.cancellation.is_cancelled() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "publication stopped",
            ));
        }
        let slot = state
            .sockets
            .iter()
            .position(Option::is_none)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::WouldBlock, "publication capacity reached")
            })?;
        state.sockets[slot] = Some(shutdown_handle);
        Ok((
            stream,
            ConnectionGuard {
                owner: self.control.clone(),
                slot,
            },
        ))
    }

    /// Synchronous admission barrier and network cutoff, also used by Drop.
    pub(super) fn abort(&self) {
        self.control.cancellation.cancel();
        let mut state = self
            .control
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.closed = true;
        for socket in &mut state.sockets {
            if let Some(socket) = socket.take() {
                let _ = socket.shutdown(Shutdown::Both);
            }
        }
        if let Some(handlers) = state.handlers.as_mut() {
            handlers.abort_all();
        }
        self.control.capacity_changed.notify_waiters();
    }

    /// Only the frontend driver takes the handler set; no lock crosses await.
    pub(super) async fn join_handlers(&self) {
        self.abort();
        let handlers = self
            .control
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .handlers
            .take();
        if let Some(mut handlers) = handlers {
            handlers.abort_all();
            while handlers.join_next().await.is_some() {}
        }
    }
}

/// Travels inside PublicationIo, including when Hyper moves IO into Upgraded.
/// A slot is reusable only after this exact guard drops; stop closes admission.
pub(super) struct ConnectionGuard {
    owner: Arc<OwnerControl>,
    slot: usize,
}
impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.owner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .sockets[self.slot]
            .take();
        self.owner.capacity_changed.notify_waiters();
    }
}

/// Also runs if the frontend driver is aborted or unwinds before normal cleanup.
pub(super) struct FrontendGuard(pub PublicationUpgradeOwner);
impl Drop for FrontendGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}
