#[cfg(windows)]
use super::windows_process::{create_suspended, NativeChild};
use super::{OwnedRoute, PublicationError, PublicationPlan, ServeStatus};
use serde::Serialize;
#[cfg(windows)]
use std::time::{Duration, Instant};
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

#[cfg(windows)]
const STATUS_TIMEOUT: Duration = Duration::from_secs(3);
#[cfg(windows)]
const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
#[cfg(windows)]
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(windows)]
const STOP_TIMEOUT: Duration = Duration::from_secs(10);
#[cfg(windows)]
const CHILD_STOP_TIMEOUT: Duration = Duration::from_secs(2);
#[cfg(windows)]
const POLL_INTERVAL: Duration = Duration::from_millis(75);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PublicationState {
    Running,
    Stopped,
    OutcomeUnknown,
}

pub(crate) struct NativePublication {
    pub(super) plan: PublicationPlan,
    pub(super) owned: OwnedRoute,
    executable: PathBuf,
    phase: PublicationState,
    #[cfg(windows)]
    child: Option<NativeChild>,
}

impl NativePublication {
    pub fn start(
        executable: &Path,
        plan: PublicationPlan,
        cancelled: &AtomicBool,
    ) -> Result<Self, PublicationError> {
        if cancelled.load(Ordering::SeqCst) {
            return Err(PublicationError::OperationCancelled);
        }
        #[cfg(windows)]
        {
            Self::start_windows(executable, plan, cancelled)
        }
        #[cfg(not(windows))]
        {
            let _ = (executable, plan);
            Err(PublicationError::PlatformUnsupported)
        }
    }
    pub fn state(&mut self) -> Result<PublicationState, PublicationError> {
        if self.phase != PublicationState::Running {
            return Ok(self.phase);
        }
        #[cfg(windows)]
        {
            let child = self
                .child
                .as_ref()
                .ok_or(PublicationError::OwnershipUnknown)?;
            match child.try_exit() {
                Ok(Some(_)) => {
                    self.phase = PublicationState::OutcomeUnknown;
                    return Ok(self.phase);
                }
                Err(_) => {
                    self.phase = PublicationState::OutcomeUnknown;
                    return Err(PublicationError::OwnershipUnknown);
                }
                Ok(None) => {}
            }
            let current = read_serve_status(&self.executable, &AtomicBool::new(false));
            match current {
                Ok(current) if self.owned.matches(&current) => Ok(PublicationState::Running),
                Ok(_) => {
                    self.phase = PublicationState::OutcomeUnknown;
                    Err(PublicationError::OwnershipLost)
                }
                Err(_) => {
                    self.phase = PublicationState::OutcomeUnknown;
                    Err(PublicationError::OwnershipUnknown)
                }
            }
        }
        #[cfg(not(windows))]
        {
            Err(PublicationError::PlatformUnsupported)
        }
    }
    pub fn owned_route(&self) -> &OwnedRoute {
        &self.owned
    }
    pub fn port(&self) -> u16 {
        self.plan.port()
    }
    pub fn request_stop(&mut self) -> Result<(), PublicationError> {
        if self.phase == PublicationState::Stopped {
            return Ok(());
        }
        self.phase = PublicationState::OutcomeUnknown;
        #[cfg(windows)]
        {
            self.child
                .as_mut()
                .ok_or(PublicationError::OwnershipUnknown)?
                .terminate(Instant::now() + CHILD_STOP_TIMEOUT)
                .map_err(Into::into)
        }
        #[cfg(not(windows))]
        {
            Err(PublicationError::PlatformUnsupported)
        }
    }
    pub fn stop(&mut self, cancelled: &AtomicBool) -> Result<(), PublicationError> {
        if self.phase == PublicationState::Stopped {
            return Ok(());
        }
        #[cfg(windows)]
        {
            self.stop_windows(cancelled)
        }
        #[cfg(not(windows))]
        {
            let _ = cancelled;
            Err(PublicationError::PlatformUnsupported)
        }
    }
}

pub(crate) fn read_serve_status(
    executable: &Path,
    cancelled: &AtomicBool,
) -> Result<ServeStatus, PublicationError> {
    if cancelled.load(Ordering::SeqCst) {
        return Err(PublicationError::OperationCancelled);
    }
    #[cfg(windows)]
    {
        read_status_before(executable, cancelled, Instant::now() + STATUS_TIMEOUT)
    }
    #[cfg(not(windows))]
    {
        let _ = executable;
        Err(PublicationError::PlatformUnsupported)
    }
}

#[cfg(windows)]
impl NativePublication {
    fn start_windows(
        executable: &Path,
        plan: PublicationPlan,
        cancelled: &AtomicBool,
    ) -> Result<Self, PublicationError> {
        let deadline = Instant::now() + STARTUP_TIMEOUT;
        let before = read_status_before(executable, cancelled, deadline)?;
        let preflight = before.preflight(&plan)?;
        let mut child = create_suspended(executable, &plan.foreground_args(), false, None)?;
        if cancelled.load(Ordering::SeqCst) {
            return Err(PublicationError::OperationCancelled);
        }
        if let Err(error) = child.admit_and_resume() {
            return Err(cleanup_start(
                executable,
                &plan,
                &before,
                &mut child,
                error.into(),
            ));
        }
        let result = loop {
            if cancelled.load(Ordering::SeqCst) {
                break Err(PublicationError::OperationCancelled);
            }
            if Instant::now() >= deadline {
                break Err(PublicationError::StartupTimeout);
            }
            match child.try_exit() {
                Ok(Some(_)) => break Err(PublicationError::ProcessExited),
                Err(error) => break Err(error.into()),
                Ok(None) => {}
            }
            let after = match read_status_before(executable, cancelled, deadline) {
                Ok(status) => status,
                Err(error) => break Err(error),
            };
            match preflight.bind(&plan, &after) {
                Ok(owned) => {
                    break match child.try_exit() {
                        Ok(None) => Ok(owned),
                        Ok(Some(_)) => Err(PublicationError::ProcessExited),
                        Err(error) => Err(error.into()),
                    };
                }
                Err(_) if after.port_free(plan.port()) && after.equivalent_to(&before) => {}
                Err(error) => break Err(error),
            }
            std::thread::sleep(
                POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())),
            );
        };
        match result {
            Ok(owned) => Ok(Self {
                plan,
                owned,
                executable: executable.to_path_buf(),
                phase: PublicationState::Running,
                child: Some(child),
            }),
            Err(error) => Err(cleanup_start(executable, &plan, &before, &mut child, error)),
        }
    }

    fn stop_windows(&mut self, cancelled: &AtomicBool) -> Result<(), PublicationError> {
        let deadline = Instant::now() + STOP_TIMEOUT;
        let before = match read_status_before(&self.executable, cancelled, deadline) {
            Ok(status) => status,
            Err(_) => {
                let _ = self.request_stop();
                return Err(PublicationError::OwnershipUnknown);
            }
        };
        // Even after a foreign takeover, terminate only our process handle.
        // Never use port-wide Serve off/reset to compensate for lost ownership.
        let admitted = self.owned.matches(&before)
            || (before.port_free(self.plan.port()) && !before.has_session(&self.owned));
        self.request_stop()?;
        if !admitted {
            return Err(PublicationError::OwnershipLost);
        }
        loop {
            if cancelled.load(Ordering::SeqCst) || Instant::now() >= deadline {
                return Err(PublicationError::OwnershipUnknown);
            }
            let after = read_status_before(&self.executable, cancelled, deadline)
                .map_err(|_| PublicationError::OwnershipUnknown)?;
            match self.owned.verify_removed(&before, &after) {
                Ok(()) => {
                    self.child.take();
                    self.phase = PublicationState::Stopped;
                    return Ok(());
                }
                Err(PublicationError::OwnershipLost)
                    if self.owned.matches(&after)
                        || (after.port_free(self.plan.port())
                            && after.has_session(&self.owned)) => {}
                Err(error) => return Err(error),
            }
            std::thread::sleep(
                POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())),
            );
        }
    }
}

#[cfg(windows)]
fn cleanup_start(
    executable: &Path,
    plan: &PublicationPlan,
    before: &ServeStatus,
    child: &mut NativeChild,
    error: PublicationError,
) -> PublicationError {
    let deadline = Instant::now() + CLEANUP_TIMEOUT;
    if child
        .terminate(deadline.min(Instant::now() + CHILD_STOP_TIMEOUT))
        .is_err()
    {
        return PublicationError::OwnershipUnknown;
    }
    let cleanup = AtomicBool::new(false);
    loop {
        if Instant::now() >= deadline {
            return PublicationError::OwnershipUnknown;
        }
        match read_status_before(executable, &cleanup, deadline) {
            Ok(after) if after.port_free(plan.port()) && after.equivalent_to(before) => {
                return error
            }
            Ok(_) => {}
            Err(_) => return PublicationError::OwnershipUnknown,
        }
        std::thread::sleep(POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())));
    }
}

#[cfg(windows)]
fn read_status_before(
    executable: &Path,
    cancelled: &AtomicBool,
    deadline: Instant,
) -> Result<ServeStatus, PublicationError> {
    let raw = run_command(
        executable,
        &PublicationPlan::status_args(),
        None,
        cancelled,
        deadline.min(Instant::now() + STATUS_TIMEOUT),
        super::status::STATUS_LIMIT,
    )?;
    let raw = String::from_utf8(raw).map_err(|_| PublicationError::StatusInvalid)?;
    ServeStatus::parse(&raw)
}

#[cfg(windows)]
pub(super) fn run_command(
    executable: &Path,
    args: &[String],
    environment: Option<&[u16]>,
    cancelled: &AtomicBool,
    deadline: Instant,
    limit: usize,
) -> Result<Vec<u8>, PublicationError> {
    if cancelled.load(Ordering::SeqCst) {
        return Err(PublicationError::OperationCancelled);
    }
    if Instant::now() >= deadline {
        return Err(PublicationError::CommandTimeout);
    }
    let mut child = create_suspended(executable, args, true, environment)?;
    if cancelled.load(Ordering::SeqCst) {
        return Err(PublicationError::OperationCancelled);
    }
    if Instant::now() >= deadline {
        return Err(PublicationError::CommandTimeout);
    }
    child.admit_and_resume()?;
    let mut output = Vec::new();
    loop {
        if cancelled.load(Ordering::SeqCst) {
            return Err(PublicationError::OperationCancelled);
        }
        if Instant::now() >= deadline {
            return Err(PublicationError::CommandTimeout);
        }
        let eof = child.read_available(&mut output, limit)?;
        if let Some(code) = child.try_exit()? {
            if code != 0 {
                return Err(PublicationError::CommandFailed);
            }
            if eof {
                return Ok(output);
            }
        }
        std::thread::sleep(
            Duration::from_millis(20).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
}
