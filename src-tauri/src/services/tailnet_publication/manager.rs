//! One owner for the publication frontend and its foreground native route.
//! Operations run in owned tasks so dropping an IPC waiter cannot orphan work.

use super::{
    NativePublication, PublicationConfig, PublicationError, PublicationPlan, PublicationState,
};
use crate::proxy::{
    publication::{publication_router, PublicationCancellation, PublicationFrontend},
    server::ProxyAppState,
};
use serde::Serialize;
use std::{
    path::{Path, PathBuf},
    sync::{atomic::AtomicBool, Arc, Mutex},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct PublicationSnapshot {
    pub state: PublicationState,
    pub port: Option<u16>,
    pub error: Option<String>,
}

trait PublicationProcess: Send {
    fn state(&mut self) -> Result<PublicationState, PublicationError>;
    fn stop(&mut self, cancelled: &AtomicBool) -> Result<(), PublicationError>;
}

impl PublicationProcess for NativePublication {
    fn state(&mut self) -> Result<PublicationState, PublicationError> {
        self.state()
    }
    fn stop(&mut self, cancelled: &AtomicBool) -> Result<(), PublicationError> {
        self.stop(cancelled)
    }
}

trait PublicationLauncher: Send + Sync {
    fn start(
        &self,
        executable: &Path,
        plan: PublicationPlan,
        cancelled: &AtomicBool,
    ) -> Result<Box<dyn PublicationProcess>, PublicationError>;
}

struct NativeLauncher;

impl PublicationLauncher for NativeLauncher {
    fn start(
        &self,
        executable: &Path,
        plan: PublicationPlan,
        cancelled: &AtomicBool,
    ) -> Result<Box<dyn PublicationProcess>, PublicationError> {
        NativePublication::start(executable, plan, cancelled)
            .map(|publication| Box::new(publication) as Box<dyn PublicationProcess>)
    }
}

#[derive(Default)]
struct ControlGate {
    generation: u64,
    current: Option<PublicationCancellation>,
}

/// Cancels admission immediately, including while native startup owns the lock.
/// Follow with `PublicationManager::stop` to await the complete cleanup barrier.
#[derive(Clone)]
pub(crate) struct PublicationShutdown {
    gate: Arc<Mutex<ControlGate>>,
}

impl PublicationShutdown {
    pub fn cancel(&self) {
        let mut gate = self.gate.lock().unwrap_or_else(|error| error.into_inner());
        gate.generation = gate.generation.wrapping_add(1);
        if let Some(current) = &gate.current {
            current.cancel();
        }
    }
}

struct ActivePublication {
    frontend: PublicationFrontend,
    // The slot retains this exact receipt while a blocking operation or its
    // async waiter is cancelled by runtime teardown.
    process: Arc<Mutex<Box<dyn PublicationProcess>>>,
    cancellation: PublicationCancellation,
    port: u16,
}

#[derive(Default)]
struct PublicationSlot {
    active: Option<ActivePublication>,
    // A failed native ownership proof must not be erased by another start.
    blocked: Option<PublicationSnapshot>,
}

struct ManagerInner {
    executable: PathBuf,
    launcher: Arc<dyn PublicationLauncher>,
    slot: tokio::sync::Mutex<PublicationSlot>,
    gate: Arc<Mutex<ControlGate>>,
}

#[derive(Clone)]
pub(crate) struct PublicationManager {
    inner: Arc<ManagerInner>,
}

struct CancelWaiter {
    cancellation: Option<PublicationCancellation>,
    inner: Arc<ManagerInner>,
}

impl Drop for CancelWaiter {
    fn drop(&mut self) {
        if let Some(cancellation) = self.cancellation.take() {
            cancellation.cancel();
            let inner = self.inner.clone();
            // The completion and waiter cancellation can cross just after
            // native admission. Cleanup remains owned even in that window.
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    let mut slot = inner.slot.lock().await;
                    if slot.active.as_ref().is_some_and(|active| {
                        std::ptr::eq(active.cancellation.flag(), cancellation.flag())
                    }) {
                        let _ = stop_slot(&mut slot).await;
                    }
                });
            }
            // Without a runnable context, cancellation still closes admission
            // and the slot keeps the receipt for explicit stop or owner drop.
        }
    }
}

impl PublicationManager {
    pub fn new(executable: PathBuf) -> Self {
        Self::with_launcher(executable, Arc::new(NativeLauncher))
    }

    fn with_launcher(executable: PathBuf, launcher: Arc<dyn PublicationLauncher>) -> Self {
        Self {
            inner: Arc::new(ManagerInner {
                executable,
                launcher,
                slot: tokio::sync::Mutex::new(PublicationSlot::default()),
                gate: Arc::new(Mutex::new(ControlGate::default())),
            }),
        }
    }

    pub fn shutdown_handle(&self) -> PublicationShutdown {
        PublicationShutdown {
            gate: self.inner.gate.clone(),
        }
    }

    pub async fn start(
        &self,
        config: PublicationConfig,
        state: ProxyAppState,
    ) -> Result<PublicationSnapshot, String> {
        let router = publication_router(state.clone())?;
        // Validate before opening a listener. The actual bound endpoint is
        // validated again before any native process can be created.
        config
            .plan(&state.auth_token, "127.0.0.1:1")
            .map_err(|error| error.to_string())?;
        let cancellation = PublicationCancellation::new();
        let mut waiter = CancelWaiter {
            cancellation: Some(cancellation.clone()),
            inner: self.inner.clone(),
        };
        let generation = self
            .inner
            .gate
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .generation;
        let inner = self.inner.clone();
        let result = tokio::spawn(async move {
            let mut slot = inner.slot.lock().await;
            if slot.blocked.is_some() {
                return Err(PublicationError::OwnershipUnknown.to_string());
            }
            if slot.active.is_some() {
                return Err(PublicationError::SlotOccupied.to_string());
            }
            {
                let mut gate = inner.gate.lock().unwrap_or_else(|error| error.into_inner());
                if generation != gate.generation || cancellation.is_cancelled() {
                    return Err(PublicationError::OperationCancelled.to_string());
                }
                gate.current = Some(cancellation.clone());
            }
            let mut frontend = PublicationFrontend::start(router, cancellation.clone()).await?;
            let plan = config
                .plan(&state.auth_token, &frontend.address().to_string())
                .map_err(|error| error.to_string())?;
            let launcher = inner.launcher.clone();
            let executable = inner.executable.clone();
            let native_cancellation = cancellation.clone();
            let started = tokio::task::spawn_blocking(move || {
                launcher.start(&executable, plan, native_cancellation.flag())
            })
            .await
            .map_err(|_| PublicationError::OwnershipUnknown);
            let started = match started {
                Ok(result) => result,
                Err(error) => Err(error),
            };
            match started {
                Ok(process) => {
                    slot.active = Some(ActivePublication {
                        frontend,
                        process: Arc::new(Mutex::new(process)),
                        cancellation: cancellation.clone(),
                        port: config.port,
                    });
                    if cancellation.is_cancelled() {
                        stop_slot(&mut slot).await?;
                        return Err(PublicationError::OperationCancelled.to_string());
                    }
                    Ok(snapshot(&slot))
                }
                Err(error) => {
                    // Only this operation's listener and foreground child are
                    // involved; NativePublication performs its own rollback.
                    let _ = frontend.stop().await;
                    if matches!(
                        error,
                        PublicationError::OwnershipUnknown | PublicationError::OwnershipLost
                    ) {
                        slot.blocked = Some(PublicationSnapshot {
                            state: PublicationState::OutcomeUnknown,
                            port: Some(config.port),
                            error: Some(error.to_string()),
                        });
                    }
                    Err(error.to_string())
                }
            }
        })
        .await
        .map_err(|_| PublicationError::OwnershipUnknown.to_string())?;
        waiter.cancellation.take();
        result
    }

    pub async fn stop(&self) -> Result<PublicationSnapshot, String> {
        self.shutdown_handle().cancel();
        let inner = self.inner.clone();
        tokio::spawn(async move {
            let mut slot = inner.slot.lock().await;
            stop_slot(&mut slot).await?;
            Ok(snapshot(&slot))
        })
        .await
        .map_err(|_| PublicationError::OwnershipUnknown.to_string())?
    }

    pub async fn status(&self) -> PublicationSnapshot {
        let inner = self.inner.clone();
        tokio::spawn(async move {
            let mut slot = inner.slot.lock().await;
            if let Some(active) = slot.active.as_ref() {
                let frontend_finished = active.frontend.is_finished();
                let process = active.process.clone();
                let result = tokio::task::spawn_blocking(move || {
                    let mut receipt = process
                        .lock()
                        .map_err(|_| PublicationError::OwnershipUnknown)?;
                    receipt.state()
                })
                .await;
                match result {
                    Ok(result) => {
                        if frontend_finished || !matches!(result, Ok(PublicationState::Running)) {
                            let error = result
                                .err()
                                .map(|error| error.to_string())
                                .unwrap_or_else(|| PublicationError::OwnershipUnknown.to_string());
                            if let Some(active) = &mut slot.active {
                                active.cancellation.cancel();
                                let _ = active.frontend.stop().await;
                            }
                            slot.blocked = Some(PublicationSnapshot {
                                state: PublicationState::OutcomeUnknown,
                                port: slot.active.as_ref().map(|active| active.port),
                                error: Some(error),
                            });
                        }
                    }
                    Err(_) => slot.blocked = Some(unknown_snapshot(None)),
                }
            }
            snapshot(&slot)
        })
        .await
        .unwrap_or_else(|_| unknown_snapshot(None))
    }
}

fn snapshot(slot: &PublicationSlot) -> PublicationSnapshot {
    if let Some(blocked) = &slot.blocked {
        return blocked.clone();
    }
    match &slot.active {
        Some(active) => PublicationSnapshot {
            state: if active.cancellation.is_cancelled() {
                PublicationState::OutcomeUnknown
            } else {
                PublicationState::Running
            },
            port: Some(active.port),
            error: None,
        },
        None => PublicationSnapshot {
            state: PublicationState::Stopped,
            port: None,
            error: None,
        },
    }
}

fn unknown_snapshot(port: Option<u16>) -> PublicationSnapshot {
    PublicationSnapshot {
        state: PublicationState::OutcomeUnknown,
        port,
        error: Some(PublicationError::OwnershipUnknown.to_string()),
    }
}

async fn stop_slot(slot: &mut PublicationSlot) -> Result<(), String> {
    let Some(active) = slot.active.as_mut() else {
        return slot.blocked.as_ref().map_or(Ok(()), |_| {
            Err(PublicationError::OwnershipUnknown.to_string())
        });
    };
    active.cancellation.cancel();
    let frontend_result = active.frontend.stop().await;
    let process = active.process.clone();
    // Stop has its own bounded native cleanup budget. Reusing the cancelled
    // startup flag would skip the ownership verification that stop must perform.
    let result = tokio::task::spawn_blocking(move || {
        let mut receipt = process
            .lock()
            .map_err(|_| PublicationError::OwnershipUnknown)?;
        receipt.stop(&AtomicBool::new(false))
    })
    .await;
    match result {
        Ok(Ok(())) => {
            slot.active.take();
            slot.blocked = None;
            frontend_result
        }
        Ok(Err(error)) => {
            slot.blocked = Some(PublicationSnapshot {
                state: PublicationState::OutcomeUnknown,
                port: slot.active.as_ref().map(|active| active.port),
                error: Some(error.to_string()),
            });
            Err(error.to_string())
        }
        Err(_) => {
            slot.blocked = Some(unknown_snapshot(None));
            Err(PublicationError::OwnershipUnknown.to_string())
        }
    }
}

#[cfg(test)]
#[path = "manager_tests.rs"]
mod tests;
