//! Dedicated Windows SCM host for independent authority, reader, and per-source
//! physical-generation runtime virtual-account principals.
//! Installation, broker work, profiles, homes, and ACL changes belong to callers.
//! The SCM bootstrap stores only the role and function pointers. Callback
//! context belongs to SCM's process lifetime and carries no broker data.

#[cfg(windows)]
#[path = "service_host/ffi.rs"]
mod ffi;
#[cfg(windows)]
#[path = "service_host/identity.rs"]
mod identity;
#[cfg(windows)]
#[path = "service_host/native.rs"]
mod native;
#[cfg(windows)]
#[path = "service_host/protected_process.rs"]
pub(crate) mod protected_process;
#[cfg(test)]
#[path = "service_host/tests.rs"]
mod tests;

use std::{
    fmt,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

const CLEANUP_TIMEOUT: Duration = Duration::from_secs(10);
const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const CANCELLATION_POLL: Duration = Duration::from_millis(25);
const MINIMAL_TOKEN_PRIVILEGES: &[&str] = &["SeChangeNotifyPrivilege"];
const POLICY_TOKEN_PRIVILEGES: &[&str] = &[
    "SeChangeNotifyPrivilege",
    "SeBackupPrivilege",
    "SeRestorePrivilege",
    "SeSecurityPrivilege",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Syntax validation only. The caller supplies the owner-generated source key;
/// physical-generation allocation and non-reuse belong to its durable fence.
pub(crate) struct RuntimeSourceKey([u8; 32]);

impl RuntimeSourceKey {
    pub fn parse(value: &str) -> Result<Self, ServiceHostError> {
        if value.len() != 32
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(ServiceHostError::RoleInvalid);
        }
        let mut key = [0; 32];
        key.copy_from_slice(value.as_bytes());
        Ok(Self(key))
    }
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).expect("validated ASCII source key")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ServiceRole {
    Policy,
    Reader,
    Runtime {
        source_key: RuntimeSourceKey,
        generation: std::num::NonZeroU64,
    },
}

impl ServiceRole {
    /// Presence allowlist only. Business operations must enable privileges in
    /// scoped guards and restore their previous enabled states afterwards.
    pub fn allowed_privileges(self) -> &'static [&'static str] {
        match self {
            Self::Policy => POLICY_TOKEN_PRIVILEGES,
            Self::Reader | Self::Runtime { .. } => MINIMAL_TOKEN_PRIVILEGES,
        }
    }
    pub fn runtime(source_key: &str, generation: u64) -> Result<Self, ServiceHostError> {
        Ok(Self::Runtime {
            source_key: RuntimeSourceKey::parse(source_key)?,
            generation: std::num::NonZeroU64::new(generation)
                .ok_or(ServiceHostError::RoleInvalid)?,
        })
    }
    pub fn service_name(self) -> String {
        match self {
            Self::Policy => "aag_session_authority".into(),
            Self::Reader => "aag_session_reader".into(),
            Self::Runtime {
                source_key,
                generation,
            } => format!(
                "aag_session_runtime_{}_g{}",
                source_key.as_str(),
                generation
            ),
        }
    }

    pub fn account_name(self) -> String {
        format!("NT SERVICE\\{}", self.service_name())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ServiceHostError {
    Unsupported,
    AlreadyDispatched,
    DispatcherUnavailable,
    HandlerRegistrationFailed,
    StatusReportFailed,
    IdentityUnavailable,
    IdentityMismatch,
    RoleInvalid,
    PrivilegeForbidden,
    AdministratorForbidden,
    ServiceNameMismatch,
    SessionNotZero,
    BusinessNotReady,
    BusinessFailed,
    BusinessPanicked,
    UnexpectedExit,
    StopRequested,
    CleanupTimeout,
    StartupTimeout,
    CallbackPanicked,
    StateInvalid,
    PolicyProtectionFailed,
}

impl ServiceHostError {
    pub fn code(self) -> &'static str {
        match self {
            Self::Unsupported => "session_service_host_unsupported",
            Self::AlreadyDispatched => "session_service_host_already_dispatched",
            Self::DispatcherUnavailable => "session_service_host_dispatcher_unavailable",
            Self::HandlerRegistrationFailed => "session_service_host_handler_registration_failed",
            Self::StatusReportFailed => "session_service_host_status_report_failed",
            Self::IdentityUnavailable => "session_service_host_identity_unavailable",
            Self::IdentityMismatch => "session_service_host_identity_mismatch",
            Self::RoleInvalid => "session_service_host_role_invalid",
            Self::PrivilegeForbidden => "session_service_host_privilege_forbidden",
            Self::AdministratorForbidden => "session_service_host_administrator_forbidden",
            Self::ServiceNameMismatch => "session_service_host_service_name_mismatch",
            Self::SessionNotZero => "session_service_host_session_not_zero",
            Self::BusinessNotReady => "session_service_host_business_not_ready",
            Self::BusinessFailed => "session_service_host_business_failed",
            Self::BusinessPanicked => "session_service_host_business_panicked",
            Self::UnexpectedExit => "session_service_host_unexpected_exit",
            Self::StopRequested => "session_service_host_stop_requested",
            Self::CleanupTimeout => "session_service_host_cleanup_timeout",
            Self::StartupTimeout => "session_service_host_startup_timeout",
            Self::CallbackPanicked => "session_service_host_callback_panicked",
            Self::StateInvalid => "session_service_host_state_invalid",
            Self::PolicyProtectionFailed => "session_service_host_policy_protection_failed",
        }
    }
}

impl fmt::Display for ServiceHostError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code())
    }
}
impl std::error::Error for ServiceHostError {}

/// A fatal result issued only inside an attached dedicated service process.
/// Its configured nonreturning handler must immediately exit that process.
pub(crate) struct FatalServiceExit {
    reason: ServiceHostError,
}

impl FatalServiceExit {
    pub fn reason(&self) -> ServiceHostError {
        self.reason
    }

    pub fn exit_code(&self) -> i32 {
        1
    }
}

#[derive(Clone, Copy)]
pub(crate) struct ServiceHostConfig {
    pub role: ServiceRole,
    pub fatal_exit: fn(FatalServiceExit) -> !,
}

type ServiceEntry = fn(ServiceContext) -> Result<(), ServiceHostError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ServicePhase {
    StartPending,
    Running,
    StopPending,
    Stopped,
}

#[derive(Clone)]
struct ObservedIdentity {
    user_sid: String,
    session_id: u32,
    privileges: Vec<String>,
    administrator_member: bool,
}

struct ServiceMachine {
    phase: ServicePhase,
    identity_verified: bool,
    was_ready: bool,
    stop_started: Option<Instant>,
    failure: Option<ServiceHostError>,
    checkpoint: u32,
}

impl ServiceMachine {
    fn new() -> Self {
        Self {
            phase: ServicePhase::StartPending,
            identity_verified: false,
            was_ready: false,
            stop_started: None,
            failure: None,
            checkpoint: 1,
        }
    }

    fn verify_identity(
        &mut self,
        role: ServiceRole,
        observed: &ObservedIdentity,
        expected: &str,
    ) -> Result<(), ServiceHostError> {
        if self.phase != ServicePhase::StartPending || self.identity_verified {
            return Err(ServiceHostError::StateInvalid);
        }
        let valid = expected.strip_prefix("S-1-5-80-").is_some_and(|suffix| {
            let parts: Vec<_> = suffix.split('-').collect();
            parts.len() == 5 && parts.iter().all(|part| part.parse::<u32>().is_ok())
        });
        if !valid || observed.user_sid != expected {
            return Err(ServiceHostError::IdentityMismatch);
        }
        if observed.session_id != 0 {
            return Err(ServiceHostError::SessionNotZero);
        }
        if observed.administrator_member {
            return Err(ServiceHostError::AdministratorForbidden);
        }
        if observed
            .privileges
            .iter()
            .any(|privilege| !role.allowed_privileges().contains(&privilege.as_str()))
        {
            return Err(ServiceHostError::PrivilegeForbidden);
        }
        self.identity_verified = true;
        Ok(())
    }

    fn ready(&mut self, cancelled: bool) -> Result<(), ServiceHostError> {
        if cancelled || self.phase == ServicePhase::StopPending {
            return Err(ServiceHostError::StopRequested);
        }
        if self.phase != ServicePhase::StartPending || !self.identity_verified {
            return Err(ServiceHostError::StateInvalid);
        }
        self.phase = ServicePhase::Running;
        self.was_ready = true;
        self.checkpoint = 0;
        Ok(())
    }

    fn request_stop(&mut self, now: Instant) -> Result<(), ServiceHostError> {
        if self.phase == ServicePhase::Stopped {
            return Err(ServiceHostError::StateInvalid);
        }
        if self.phase != ServicePhase::StopPending {
            self.phase = ServicePhase::StopPending;
            self.stop_started = Some(now);
            self.checkpoint = 1;
        }
        Ok(())
    }

    fn check_cleanup_deadline(&self, now: Instant) -> Result<(), ServiceHostError> {
        if self
            .stop_started
            .is_some_and(|started| now.saturating_duration_since(started) >= CLEANUP_TIMEOUT)
        {
            return Err(ServiceHostError::CleanupTimeout);
        }
        Ok(())
    }

    fn finish(&mut self, result: Result<(), ServiceHostError>) -> Result<(), ServiceHostError> {
        if self.phase == ServicePhase::Stopped {
            return Err(ServiceHostError::StateInvalid);
        }
        if result == Err(ServiceHostError::BusinessPanicked) {
            self.failure = Some(ServiceHostError::BusinessPanicked);
            self.request_stop(Instant::now())?;
            return result;
        }
        let result = match result {
            Ok(()) if self.phase != ServicePhase::StopPending => Err(if self.was_ready {
                ServiceHostError::UnexpectedExit
            } else {
                ServiceHostError::BusinessNotReady
            }),
            Err(ServiceHostError::StopRequested) if self.phase == ServicePhase::StopPending => {
                Ok(())
            }
            other => other,
        };
        if self.failure.is_none() {
            self.failure = result.err();
        }
        self.phase = ServicePhase::Stopped;
        self.checkpoint = 0;
        self.failure.map_or(Ok(()), Err)
    }
}

#[derive(Clone, Copy)]
struct StatusSnapshot {
    phase: ServicePhase,
    checkpoint: u32,
    failure: Option<ServiceHostError>,
}

trait StatusReporter: Send + Sync {
    fn report(&self, status: StatusSnapshot) -> Result<(), ServiceHostError>;
}

struct ServiceControl {
    role: ServiceRole,
    cancelled: AtomicBool,
    machine: Mutex<ServiceMachine>,
    started: Instant,
    stop_elapsed_ms: std::sync::atomic::AtomicU64,
    callback_panicked: AtomicBool,
    reporter: Arc<dyn StatusReporter>,
}

impl ServiceControl {
    fn cancel(&self) {
        let elapsed = self
            .started
            .elapsed()
            .as_millis()
            .min((u64::MAX - 1) as u128) as u64;
        let _ = self.stop_elapsed_ms.compare_exchange(
            u64::MAX,
            elapsed,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
        self.cancelled.store(true, Ordering::SeqCst);
    }
    fn stop_started(&self) -> Instant {
        let elapsed = self.stop_elapsed_ms.load(Ordering::SeqCst);
        if elapsed == u64::MAX {
            Instant::now()
        } else {
            self.started + Duration::from_millis(elapsed)
        }
    }
    fn report(&self, machine: &ServiceMachine) -> Result<(), ServiceHostError> {
        self.reporter.report(StatusSnapshot {
            phase: machine.phase,
            checkpoint: machine.checkpoint,
            failure: machine.failure,
        })
    }
}

#[derive(Clone)]
pub(crate) struct ServiceContext {
    control: Arc<ServiceControl>,
}

impl ServiceContext {
    pub fn role(&self) -> ServiceRole {
        self.control.role
    }

    /// Re-read the current process identity; never return a claimed/config SID
    /// as proof. Membership in the role's service group is insufficient.
    pub fn verified_user_sid(&self) -> Result<String, ServiceHostError> {
        let machine = self
            .control
            .machine
            .lock()
            .map_err(|_| ServiceHostError::StateInvalid)?;
        if self.is_cancelled()
            || !machine.identity_verified
            || !matches!(
                machine.phase,
                ServicePhase::StartPending | ServicePhase::Running
            )
        {
            return Err(ServiceHostError::StateInvalid);
        }
        let sid = self.current_role_user_sid()?;
        if self.is_cancelled() {
            return Err(ServiceHostError::StopRequested);
        }
        Ok(sid)
    }

    fn current_role_user_sid(&self) -> Result<String, ServiceHostError> {
        #[cfg(windows)]
        {
            let expected = identity::expected_user_sid(self.role())?;
            let observed = identity::current_identity()?;
            ServiceMachine::new().verify_identity(self.role(), &observed, &expected)?;
            Ok(observed.user_sid)
        }
        #[cfg(not(windows))]
        {
            Err(ServiceHostError::Unsupported)
        }
    }
    pub fn is_cancelled(&self) -> bool {
        self.control.cancelled.load(Ordering::SeqCst)
    }

    /// Trusted business code may request its own ordered shutdown, for example
    /// after the authenticated owner closes its control channel.
    pub fn request_stop(&self) {
        self.control.cancel();
    }

    /// Call only after the business layer has completed all startup checks.
    pub fn ready(&self) -> Result<(), ServiceHostError> {
        let mut machine = self
            .control
            .machine
            .lock()
            .map_err(|_| ServiceHostError::StateInvalid)?;
        if self.is_cancelled()
            || machine.phase != ServicePhase::StartPending
            || !machine.identity_verified
        {
            return machine.ready(self.is_cancelled());
        }
        if self.control.started.elapsed() >= STARTUP_TIMEOUT {
            self.control.cancel();
            machine.failure = Some(ServiceHostError::StartupTimeout);
            machine.request_stop(self.control.stop_started())?;
            self.control.report(&machine)?;
            return Err(ServiceHostError::StartupTimeout);
        }
        machine.ready(self.is_cancelled())?;
        if let Err(error) = self.control.report(&machine) {
            self.control.cancel();
            machine.failure = Some(error);
            machine.request_stop(self.control.stop_started())?;
            return Err(error);
        }
        Ok(())
    }

    pub fn phase(&self) -> Result<ServicePhase, ServiceHostError> {
        Ok(self
            .control
            .machine
            .lock()
            .map_err(|_| ServiceHostError::StateInvalid)?
            .phase)
    }

    pub fn wait_cancelled(&self, timeout: Duration) -> bool {
        let started = Instant::now();
        while !self.is_cancelled() {
            let remaining = timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return false;
            }
            std::thread::sleep(remaining.min(CANCELLATION_POLL));
        }
        true
    }

    /// Explicit progress only; this does not extend the hard host budget.
    pub fn startup_progress(&self) -> Result<(), ServiceHostError> {
        self.progress(ServicePhase::StartPending)
    }
    pub fn cleanup_progress(&self) -> Result<(), ServiceHostError> {
        self.progress(ServicePhase::StopPending)
    }
    fn progress(&self, expected: ServicePhase) -> Result<(), ServiceHostError> {
        let mut machine = self
            .control
            .machine
            .lock()
            .map_err(|_| ServiceHostError::StateInvalid)?;
        if machine.phase != expected {
            return Err(ServiceHostError::StateInvalid);
        }
        if expected == ServicePhase::StartPending && self.is_cancelled() {
            return Err(ServiceHostError::StopRequested);
        }
        machine.check_cleanup_deadline(Instant::now())?;
        machine.checkpoint = machine
            .checkpoint
            .checked_add(1)
            .ok_or(ServiceHostError::StateInvalid)?;
        self.control.report(&machine)
    }
    pub fn cleanup_remaining(&self) -> Option<Duration> {
        self.is_cancelled()
            .then(|| CLEANUP_TIMEOUT.saturating_sub(self.control.stop_started().elapsed()))
    }
}

/// Call only from a dedicated service entrypoint. The callback owns business
/// work and must return only after its tasks/children have actually quiesced.
pub(crate) fn dispatch(
    config: ServiceHostConfig,
    entry: ServiceEntry,
) -> Result<(), ServiceHostError> {
    #[cfg(windows)]
    {
        native::dispatch_native(config, entry)
    }
    #[cfg(not(windows))]
    {
        let _ = (config, entry);
        Err(ServiceHostError::Unsupported)
    }
}
