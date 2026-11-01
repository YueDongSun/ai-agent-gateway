//! One native SDK stdio owner for one canonical source and physical generation.
//!
//! The caller owns source admission, policy/write permits, OS qualification, and
//! model-call authority. This module does not approve tools, reconstruct history,
//! copy native payloads, or log protocol bodies. All stdio is private in memory.

mod framing;
mod multiplex;
#[cfg(test)]
mod tests;
mod windows_io;
mod writer;

pub(crate) use framing::{FrameDecoder, FrameKind, NativeFrame, RawJson, RpcId, TerminalTurn};
pub(crate) use multiplex::{
    DeliveryState, NativeEvent, NativeResponse, RequestProgress, RequestSnapshot, RequestTicket,
    ServerReply, ServerRequestTicket, UnknownReason,
};

use super::windows_isolation::{
    CoreRole, IsolationError, NormalServiceRole, RoleToken, VerifiedNormalServicePrincipal,
};
use super::CanonicalSessionId;
use crate::services::owned_process::{
    encode_environment, NativeChild, NativeProcessError, SpawnSpec,
};
use multiplex::Multiplexer;
use std::{
    ffi::OsString,
    fs::File,
    path::{Component, Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::TrySendError,
        Arc,
    },
    time::{Duration, Instant},
};
use writer::{WriteReport, WriterOwner};

const MAX_FRAME_BYTES: usize = 32 * 1024 * 1024;
const MAX_BUFFER_BYTES: usize = 64 * 1024 * 1024;
const MAX_PENDING_REQUESTS: usize = 64;
const MAX_QUEUED_EVENTS: usize = 512;
const MAX_PENDING_SERVER_REQUESTS: usize = 64;
const MAX_OUTBOUND_FRAMES: usize = 64;
const READ_CHUNK_BYTES: usize = 16 * 1024;
const READ_CHUNKS_PER_POLL: usize = 16;
const POLL_INTERVAL: Duration = Duration::from_millis(10);
const DEFAULT_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_CONFIG_OVERRIDES: usize = 64;
const MAX_CONFIG_OVERRIDE_BYTES: usize = 16 * 1024;
const READER_DISABLED_FEATURES: &[&str] = &[
    "plugins",
    "hooks",
    "remote_plugin",
    "code_mode_host",
    "skill_mcp_dependency_install",
];
const ALLOWED_ENVIRONMENT: &[&str] = &[
    "SYSTEMROOT",
    "WINDIR",
    "SYSTEMDRIVE",
    "COMSPEC",
    "PATH",
    "PATHEXT",
    "TEMP",
    "TMP",
    "USERPROFILE",
    "HOME",
    "HOMEDRIVE",
    "HOMEPATH",
    "APPDATA",
    "LOCALAPPDATA",
    "PROGRAMDATA",
    "PROGRAMFILES",
    "PROGRAMFILES(X86)",
    "COMMONPROGRAMFILES",
    "COMMONPROGRAMFILES(X86)",
    "LANG",
    "LC_ALL",
    "TZ",
    "TERM",
    "NO_COLOR",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativeIoOperation {
    Peek,
    Read,
    Write,
    Cancel,
    Join,
    ThreadStart,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativeCoreError {
    InvalidConfiguration,
    SourceMismatch,
    InvalidEnvironment,
    ConflictingOverride,
    InvalidJson,
    InvalidUtf8,
    MalformedFrame,
    IncompleteFrame,
    FrameTooLarge,
    InputChunkTooLarge,
    CapacityExceeded,
    UnknownRequest,
    UnknownServerRequest,
    DuplicateResponse,
    DuplicateServerRequest,
    InvalidWriteReport,
    TransportFailed,
    WriterUnavailable,
    RandomIdentity,
    Stopped,
    Process(NativeProcessError),
    Principal(IsolationError),
    Io {
        operation: NativeIoOperation,
        os_code: Option<u32>,
    },
}

impl std::fmt::Display for NativeCoreError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let code = match self {
            Self::InvalidConfiguration => "native_core_configuration_invalid",
            Self::SourceMismatch => "native_core_source_mismatch",
            Self::InvalidEnvironment => "native_core_environment_invalid",
            Self::ConflictingOverride => "native_core_override_conflict",
            Self::InvalidJson => "native_core_json_invalid",
            Self::InvalidUtf8 => "native_core_utf8_invalid",
            Self::MalformedFrame => "native_core_frame_invalid",
            Self::IncompleteFrame => "native_core_frame_incomplete",
            Self::FrameTooLarge => "native_core_frame_too_large",
            Self::InputChunkTooLarge => "native_core_input_chunk_too_large",
            Self::CapacityExceeded => "native_core_capacity_exceeded",
            Self::UnknownRequest => "native_core_request_unknown",
            Self::UnknownServerRequest => "native_core_server_request_unknown",
            Self::DuplicateResponse => "native_core_duplicate_response",
            Self::DuplicateServerRequest => "native_core_duplicate_server_request",
            Self::InvalidWriteReport => "native_core_write_report_invalid",
            Self::TransportFailed => "native_core_transport_failed",
            Self::WriterUnavailable => "native_core_writer_unavailable",
            Self::RandomIdentity => "native_core_random_identity_failed",
            Self::Stopped => "native_core_stopped",
            Self::Process(error) => error.code(),
            Self::Principal(error) => error.code(),
            Self::Io { .. } => "native_core_io_failed",
        };
        formatter.write_str(code)
    }
}
impl std::error::Error for NativeCoreError {}
impl From<NativeProcessError> for NativeCoreError {
    fn from(value: NativeProcessError) -> Self {
        Self::Process(value)
    }
}
impl From<IsolationError> for NativeCoreError {
    fn from(value: IsolationError) -> Self {
        Self::Principal(value)
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct NativeCoreLimits {
    pub frame_bytes: usize,
    pub outbound_bytes: usize,
    pub outbound_frames: usize,
    pub response_bytes: usize,
    pub pending_requests: usize,
    pub event_bytes: usize,
    pub queued_events: usize,
    pub pending_server_requests: usize,
}
impl Default for NativeCoreLimits {
    fn default() -> Self {
        Self {
            frame_bytes: MAX_FRAME_BYTES,
            outbound_bytes: MAX_BUFFER_BYTES,
            outbound_frames: MAX_OUTBOUND_FRAMES,
            response_bytes: MAX_BUFFER_BYTES,
            pending_requests: MAX_PENDING_REQUESTS,
            event_bytes: MAX_BUFFER_BYTES,
            queued_events: MAX_QUEUED_EVENTS,
            pending_server_requests: MAX_PENDING_SERVER_REQUESTS,
        }
    }
}
impl NativeCoreLimits {
    fn validate(&self) -> Result<(), NativeCoreError> {
        for (value, maximum) in [
            (self.frame_bytes, MAX_FRAME_BYTES),
            (self.outbound_bytes, MAX_BUFFER_BYTES),
            (self.outbound_frames, MAX_OUTBOUND_FRAMES),
            (self.response_bytes, MAX_BUFFER_BYTES),
            (self.pending_requests, MAX_PENDING_REQUESTS),
            (self.event_bytes, MAX_BUFFER_BYTES),
            (self.queued_events, MAX_QUEUED_EVENTS),
            (self.pending_server_requests, MAX_PENDING_SERVER_REQUESTS),
        ] {
            if value == 0 || value > maximum {
                return Err(NativeCoreError::InvalidConfiguration);
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NativeBinding {
    pub canonical_session_id: CanonicalSessionId,
    pub physical_generation: u64,
    pub role: CoreRole,
}

/// No Debug/Serde: paths, configuration, and environment are caller-private.
pub(crate) struct NativeCoreSpec {
    pub binding: NativeBinding,
    pub executable: PathBuf,
    pub codex_home: PathBuf,
    pub sqlite_home: PathBuf,
    pub cwd: PathBuf,
    pub environment: Vec<(OsString, OsString)>,
    pub config_overrides: Vec<String>,
    pub limits: NativeCoreLimits,
}

struct PreparedSpec {
    executable: PathBuf,
    args: Vec<String>,
    environment: Vec<u16>,
    cwd: PathBuf,
}
impl NativeCoreSpec {
    fn prepare(&self, observed: &NativeBinding) -> Result<PreparedSpec, NativeCoreError> {
        if &self.binding != observed {
            return Err(NativeCoreError::SourceMismatch);
        }
        self.limits.validate()?;
        for path in [
            &self.executable,
            &self.codex_home,
            &self.sqlite_home,
            &self.cwd,
        ] {
            validate_path(path)?;
        }
        if self.config_overrides.len() > MAX_CONFIG_OVERRIDES {
            return Err(NativeCoreError::InvalidConfiguration);
        }
        let mut args = Vec::new();
        for entry in &self.config_overrides {
            if entry.len() > MAX_CONFIG_OVERRIDE_BYTES || entry.contains(['\0', '\n', '\r']) {
                return Err(NativeCoreError::InvalidConfiguration);
            }
            let (key, value) = entry
                .split_once('=')
                .ok_or(NativeCoreError::InvalidConfiguration)?;
            let key = key.trim();
            if key.is_empty()
                || key.split('.').any(|part| part.is_empty())
                || !key
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
            {
                return Err(NativeCoreError::InvalidConfiguration);
            }
            if key == "sqlite_home"
                || key.starts_with("sqlite_home.")
                || key == "codex_home"
                || key == "model_providers"
                || key.starts_with("model_providers.")
                || key == "mcp_servers"
                || key.starts_with("mcp_servers.")
                || matches!(
                    key,
                    "sandbox_mode"
                        | "default_permissions"
                        | "approval_policy"
                        | "approvals_reviewer"
                )
                || key == "permissions"
                || key.starts_with("permissions.")
                || key == "windows.sandbox"
                || key.split('.').any(|part| {
                    matches!(
                        part,
                        "api_key"
                            | "token"
                            | "bearer_token"
                            | "experimental_bearer_token"
                            | "password"
                            | "secret"
                    )
                })
            {
                return Err(NativeCoreError::ConflictingOverride);
            }
            format!("{key} = {}", value.trim())
                .parse::<toml_edit::DocumentMut>()
                .map_err(|_| NativeCoreError::InvalidConfiguration)?;
            args.extend(["-c".into(), entry.clone()]);
        }
        let sqlite = serde_json::to_string(
            self.sqlite_home
                .to_str()
                .ok_or(NativeCoreError::InvalidConfiguration)?,
        )
        .map_err(|_| NativeCoreError::InvalidConfiguration)?;
        args.extend([
            "-c".into(),
            format!("sqlite_home={sqlite}"),
            "app-server".into(),
            "--listen".into(),
            "stdio://".into(),
        ]);

        let mut environment = Vec::new();
        let mut names = std::collections::HashSet::new();
        for (key, value) in &self.environment {
            let key_text = key.to_str().ok_or(NativeCoreError::InvalidEnvironment)?;
            let normalized = key_text.to_ascii_uppercase();
            if !ALLOWED_ENVIRONMENT.contains(&normalized.as_str()) || !names.insert(normalized) {
                return Err(NativeCoreError::InvalidEnvironment);
            }
            environment.push((key.clone(), value.clone()));
        }
        if !names.contains("SYSTEMROOT") {
            return Err(NativeCoreError::InvalidEnvironment);
        }
        if self.environment.iter().any(|(key, value)| {
            key.to_str()
                .is_some_and(|key| key.eq_ignore_ascii_case("SYSTEMROOT"))
                && validate_path(Path::new(value)).is_err()
        }) {
            return Err(NativeCoreError::InvalidEnvironment);
        }
        environment.extend([
            (
                OsString::from("CODEX_HOME"),
                self.codex_home.as_os_str().to_owned(),
            ),
            (
                OsString::from("CODEX_SQLITE_HOME"),
                self.sqlite_home.as_os_str().to_owned(),
            ),
            (OsString::from("LOG_FORMAT"), OsString::from("json")),
            (OsString::from("RUST_LOG"), OsString::from("warn")),
        ]);
        let environment = encode_environment(&environment)?;
        Ok(PreparedSpec {
            executable: self.executable.clone(),
            args,
            environment,
            cwd: self.cwd.clone(),
        })
    }

    /// Feature disables are additional startup constraints, not proof that the
    /// native reader cannot execute tools. Its empty trusted home, neutral cwd,
    /// method policy, and installed-runtime behavior remain caller-qualified.
    fn prepare_reader(&self, observed: &NativeBinding) -> Result<PreparedSpec, NativeCoreError> {
        if observed.role != CoreRole::ReadOnly {
            return Err(NativeCoreError::SourceMismatch);
        }
        let mut prepared = self.prepare(observed)?;
        for entry in &self.config_overrides {
            let (key, value) = entry
                .split_once('=')
                .ok_or(NativeCoreError::InvalidConfiguration)?;
            let key = key.trim();
            if key == "features" || key == "history" {
                return Err(NativeCoreError::ConflictingOverride);
            }
            if let Some(feature) = key.strip_prefix("features.") {
                let parsed = format!("value = {}", value.trim())
                    .parse::<toml_edit::DocumentMut>()
                    .map_err(|_| NativeCoreError::InvalidConfiguration)?;
                if !READER_DISABLED_FEATURES.contains(&feature)
                    || parsed.get("value").and_then(|item| item.as_bool()) != Some(false)
                {
                    return Err(NativeCoreError::ConflictingOverride);
                }
            }
            if key == "history.persistence" {
                let parsed = format!("value = {}", value.trim())
                    .parse::<toml_edit::DocumentMut>()
                    .map_err(|_| NativeCoreError::InvalidConfiguration)?;
                if parsed.get("value").and_then(|item| item.as_str()) != Some("none") {
                    return Err(NativeCoreError::ConflictingOverride);
                }
            }
        }
        let position = prepared
            .args
            .len()
            .checked_sub(3)
            .ok_or(NativeCoreError::InvalidConfiguration)?;
        let mut enforced = Vec::new();
        for feature in READER_DISABLED_FEATURES {
            enforced.extend(["--disable".to_owned(), (*feature).to_owned()]);
        }
        enforced.extend(["-c".into(), "history.persistence=\"none\"".into()]);
        prepared.args.splice(position..position, enforced);
        Ok(prepared)
    }
}

/// Kept for the entire child lifetime. No Debug/Clone/Serde and no unqualified
/// inherited-token variant exist at this boundary.
enum ManagedCredential {
    Restricted(RoleToken),
    NormalService(VerifiedNormalServicePrincipal),
}
impl ManagedCredential {
    fn prepare(&self, spec: &NativeCoreSpec) -> Result<PreparedSpec, NativeCoreError> {
        match self {
            Self::Restricted(role) => spec.prepare(&NativeBinding {
                canonical_session_id: role.source_id().clone(),
                physical_generation: role.generation(),
                role: role.role(),
            }),
            Self::NormalService(principal) => {
                let observed = binding_for_service(&spec.binding, principal.role())?;
                // Reject stale/impersonating callers before any path metadata IO.
                principal.verify_current()?;
                if matches!(principal.role(), NormalServiceRole::TrustedReader) {
                    spec.prepare_reader(&observed)
                } else {
                    spec.prepare(&observed)
                }
            }
        }
    }

    fn create_child(&self, prepared: &PreparedSpec) -> Result<NativeChild, NativeCoreError> {
        match self {
            Self::Restricted(role) => Ok(NativeChild::create_suspended(&SpawnSpec {
                executable: &prepared.executable,
                args: &prepared.args,
                capture_stdout: true,
                pipe_stdin: true,
                environment: Some(&prepared.environment),
                cwd: Some(&prepared.cwd),
                primary_token: Some(role.token()),
            })?),
            Self::NormalService(principal) => {
                // This is the only inherited-token branch. The proof rechecks
                // current PID, SID, primary-token binding and service conditions
                // immediately before creation; failure never selects a fallback.
                principal.verify_current()?;
                Ok(NativeChild::create_suspended(&SpawnSpec {
                    executable: &prepared.executable,
                    args: &prepared.args,
                    capture_stdout: true,
                    pipe_stdin: true,
                    environment: Some(&prepared.environment),
                    cwd: Some(&prepared.cwd),
                    primary_token: None,
                })?)
            }
        }
    }
}

/// May be cloned to request cancellation without acquiring the actor's lock.
#[derive(Clone)]
pub(crate) struct NativeCancellation {
    requested: Arc<AtomicBool>,
}
impl NativeCancellation {
    pub fn cancel(&self) {
        self.requested.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.requested.load(Ordering::SeqCst)
    }
}

/// A start failure can retain its exact process owner if cleanup is uncertain.
pub(crate) struct NativeStartFailure {
    pub error: NativeCoreError,
    pub retained_owner: Option<NativeCore>,
}
impl std::fmt::Debug for NativeStartFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NativeStartFailure")
            .field("error", &self.error)
            .field("has_retained_owner", &self.retained_owner.is_some())
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativeShutdown {
    Stopped {
        exit_code: Option<u32>,
    },
    /// The same owner must stay in its source slot and be reconciled again.
    OutcomeUnknown {
        error: NativeCoreError,
    },
}

pub(crate) enum NativeWait {
    Response(NativeResponse),
    OutcomeUnknown { request: RequestSnapshot },
}

/// Actor-owned: callers serialize access; cancellation uses its separate handle.
/// A failed shutdown retains this value and forbids replacement admission.
#[must_use]
pub(crate) struct NativeCore {
    binding: NativeBinding,
    child: Option<NativeChild>,
    stdout: Option<File>,
    writer: Option<WriterOwner>,
    credential: Option<ManagedCredential>,
    mux: Multiplexer,
    decoder: FrameDecoder,
    cancellation: NativeCancellation,
    fault: Option<NativeCoreError>,
    exit_code: Option<u32>,
    stdout_eof: bool,
    stopped: bool,
}
impl NativeCore {
    /// The principal is captured in B-generation or C, never in owner A. The
    /// source tuple for a fixed reader comes from A's protected catalog/IPC.
    pub fn spawn_for_service(
        spec: NativeCoreSpec,
        principal: VerifiedNormalServicePrincipal,
    ) -> Result<Self, NativeStartFailure> {
        Self::spawn_managed(spec, ManagedCredential::NormalService(principal))
    }

    pub fn spawn(spec: NativeCoreSpec, role: RoleToken) -> Result<Self, NativeStartFailure> {
        Self::spawn_managed(spec, ManagedCredential::Restricted(role))
    }

    fn spawn_managed(
        spec: NativeCoreSpec,
        credential: ManagedCredential,
    ) -> Result<Self, NativeStartFailure> {
        let prepare = || -> Result<_, NativeCoreError> {
            let prepared = credential.prepare(&spec)?;
            for path in [&spec.codex_home, &spec.sqlite_home, &spec.cwd] {
                if !std::fs::metadata(path).is_ok_and(|metadata| metadata.is_dir()) {
                    return Err(NativeCoreError::InvalidConfiguration);
                }
            }
            let mux = Multiplexer::new(spec.limits)?;
            let decoder = FrameDecoder::new(spec.limits.frame_bytes)?;
            let child = credential.create_child(&prepared)?;
            Ok((child, mux, decoder))
        };
        let (child, mux, decoder) = prepare().map_err(|error| NativeStartFailure {
            error,
            retained_owner: None,
        })?;
        let mut core = Self {
            binding: spec.binding,
            child: Some(child),
            stdout: None,
            writer: None,
            credential: Some(credential),
            mux,
            decoder,
            cancellation: NativeCancellation {
                requested: Arc::new(AtomicBool::new(false)),
            },
            fault: None,
            exit_code: None,
            stdout_eof: false,
            stopped: false,
        };
        let mut start = || -> Result<(), NativeCoreError> {
            let child = core
                .child
                .as_mut()
                .ok_or(NativeCoreError::TransportFailed)?;
            core.stdout = Some(
                child
                    .take_stdout()
                    .ok_or(NativeCoreError::TransportFailed)?,
            );
            let stdin = child.take_stdin().ok_or(NativeCoreError::TransportFailed)?;
            core.writer = Some(WriterOwner::start(stdin, spec.limits.outbound_frames)?);
            child.admit_and_resume()?;
            Ok(())
        };
        if let Err(error) = start() {
            core.mark_fault(error);
            let stopped = matches!(
                core.shutdown(Instant::now() + DEFAULT_SHUTDOWN_TIMEOUT),
                NativeShutdown::Stopped { .. }
            );
            return Err(NativeStartFailure {
                error,
                retained_owner: if stopped { None } else { Some(core) },
            });
        }
        Ok(core)
    }
    pub fn binding(&self) -> &NativeBinding {
        &self.binding
    }
    pub fn cancellation(&self) -> NativeCancellation {
        self.cancellation.clone()
    }
    pub fn request(
        &mut self,
        method: &str,
        params: Option<&RawJson>,
    ) -> Result<RequestTicket, NativeCoreError> {
        if self.cancellation.is_cancelled() {
            return Err(NativeCoreError::Stopped);
        }
        self.mux.request(method, params)
    }
    pub fn notify_initialized(&mut self) -> Result<(), NativeCoreError> {
        if self.cancellation.is_cancelled() {
            return Err(NativeCoreError::Stopped);
        }
        self.mux.initialized()
    }
    pub fn reply_to_server_request(
        &mut self,
        ticket: &ServerRequestTicket,
        reply: ServerReply,
    ) -> Result<(), NativeCoreError> {
        if self.cancellation.is_cancelled() {
            return Err(NativeCoreError::Stopped);
        }
        self.mux.reply(ticket, reply)
    }
    pub fn poll(&mut self) -> Result<(), NativeCoreError> {
        if self.stopped {
            return Ok(());
        }
        if self.cancellation.is_cancelled() {
            return match self.shutdown(Instant::now() + DEFAULT_SHUTDOWN_TIMEOUT) {
                NativeShutdown::Stopped { .. } => Err(NativeCoreError::Stopped),
                NativeShutdown::OutcomeUnknown { error } => Err(error),
            };
        }
        if let Some(error) = self.fault {
            return Err(error);
        }
        let result = self.pump();
        if let Err(error) = result {
            self.mark_fault(error);
        }
        result
    }
    pub fn take_response(
        &mut self,
        ticket: &RequestTicket,
    ) -> Result<Option<NativeResponse>, NativeCoreError> {
        self.mux.take_response(ticket)
    }
    pub fn wait_response(
        &mut self,
        ticket: &RequestTicket,
        deadline: Instant,
    ) -> Result<NativeWait, NativeCoreError> {
        loop {
            if let Some(response) = self.mux.take_response(ticket)? {
                return Ok(NativeWait::Response(response));
            }
            if let RequestProgress::OutcomeUnknown(request) = self.mux.progress(ticket)? {
                if request.unknown_reason != Some(UnknownReason::Deadline) {
                    return Ok(NativeWait::OutcomeUnknown { request });
                }
            }
            if Instant::now() >= deadline {
                return Ok(NativeWait::OutcomeUnknown {
                    request: self.mux.timeout(ticket)?,
                });
            }
            if self.poll().is_err() {
                if let Some(response) = self.mux.take_response(ticket)? {
                    return Ok(NativeWait::Response(response));
                }
                let request = self
                    .mux
                    .pending_outcomes()
                    .into_iter()
                    .find(|request| &request.ticket == ticket)
                    .ok_or(NativeCoreError::UnknownRequest)?;
                return Ok(NativeWait::OutcomeUnknown { request });
            }
            std::thread::sleep(
                POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())),
            );
        }
    }
    pub fn pop_event(&mut self) -> Option<NativeEvent> {
        self.mux.pop_event()
    }
    pub fn pending_outcomes(&self) -> Vec<RequestSnapshot> {
        self.mux.pending_outcomes()
    }
    pub fn process_exit_code(&self) -> Option<u32> {
        self.exit_code
    }
    pub fn shutdown(&mut self, deadline: Instant) -> NativeShutdown {
        if self.stopped {
            return NativeShutdown::Stopped {
                exit_code: self.exit_code,
            };
        }
        self.cancellation.cancel();
        self.mux.close(UnknownReason::Cancelled);
        if let Some(writer) = &mut self.writer {
            writer.stop();
        }
        let process_result = match &mut self.child {
            Some(child) => child.terminate(deadline).map_err(NativeCoreError::from),
            None => Ok(()),
        };
        // Try writer cleanup even when process termination is uncertain. Neither
        // a cancel acknowledgement nor root exit substitutes for these checks.
        let writer_result = match &mut self.writer {
            Some(writer) => writer.join_until(deadline),
            None => Ok(()),
        };
        if let Err(error) = process_result.and(writer_result) {
            return NativeShutdown::OutcomeUnknown { error };
        }
        if let Some(child) = &self.child {
            match child.try_exit() {
                Ok(Some(code)) => self.exit_code = Some(code),
                Ok(None) => {
                    return NativeShutdown::OutcomeUnknown {
                        error: NativeCoreError::TransportFailed,
                    }
                }
                Err(error) => {
                    return NativeShutdown::OutcomeUnknown {
                        error: error.into(),
                    }
                }
            }
        }
        self.stdout.take();
        self.writer.take();
        self.child.take();
        self.credential.take();
        self.stopped = true;
        NativeShutdown::Stopped {
            exit_code: self.exit_code,
        }
    }
    fn pump(&mut self) -> Result<(), NativeCoreError> {
        let writer = self
            .writer
            .as_ref()
            .ok_or(NativeCoreError::WriterUnavailable)?;
        while let Some(report) = writer.report()? {
            match report {
                WriteReport::Written(sequence) => self.mux.written(sequence)?,
                WriteReport::Failed { sequence: _, error } => return Err(error),
            }
        }
        if let Some(error) = writer.failure() {
            return Err(error);
        }
        while let Some(frame) = self.mux.take_outbound() {
            match writer.try_send(frame) {
                Ok(()) => {}
                Err(TrySendError::Full(frame)) => {
                    self.mux.requeue_front(frame)?;
                    break;
                }
                Err(TrySendError::Disconnected(_)) => {
                    return Err(NativeCoreError::WriterUnavailable)
                }
            }
        }
        if !self.stdout_eof {
            for _ in 0..READ_CHUNKS_PER_POLL {
                match windows_io::read_chunk(
                    self.stdout
                        .as_ref()
                        .ok_or(NativeCoreError::TransportFailed)?,
                )? {
                    windows_io::PipeRead::Data(bytes) => {
                        for frame in self.decoder.feed(&bytes)? {
                            self.mux.receive(frame)?;
                        }
                    }
                    windows_io::PipeRead::Idle => break,
                    windows_io::PipeRead::Eof => {
                        self.stdout_eof = true;
                        for frame in self.decoder.finish()? {
                            self.mux.receive(frame)?;
                        }
                        break;
                    }
                }
            }
        }
        self.exit_code = self
            .child
            .as_ref()
            .ok_or(NativeCoreError::Stopped)?
            .try_exit()?;
        if self.exit_code.is_some() {
            self.mux.close(UnknownReason::NativeExit);
            if let Some(writer) = &mut self.writer {
                writer.stop();
            }
        } else if self.stdout_eof {
            return Err(NativeCoreError::TransportFailed);
        }
        Ok(())
    }
    fn mark_fault(&mut self, error: NativeCoreError) {
        self.fault.get_or_insert(error);
        self.mux.close(UnknownReason::Transport);
        if let Some(writer) = &mut self.writer {
            writer.stop();
        }
    }
}

impl Drop for NativeCore {
    fn drop(&mut self) {
        // Best-effort emergency cleanup is not an acceptance receipt. Production
        // callers retain OutcomeUnknown owners and use explicit shutdown again.
        let _ = self.shutdown(Instant::now() + DEFAULT_SHUTDOWN_TIMEOUT);
    }
}

fn validate_path(path: &Path) -> Result<(), NativeCoreError> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir))
        || path.to_str().is_none_or(|value| value.contains('\0'))
    {
        return Err(NativeCoreError::InvalidConfiguration);
    }
    Ok(())
}

fn binding_for_service(
    expected: &NativeBinding,
    role: &NormalServiceRole,
) -> Result<NativeBinding, NativeCoreError> {
    match role {
        NormalServiceRole::RuntimeGeneration {
            source_id,
            physical_generation,
        } if expected.role == CoreRole::ReadWrite
            && &expected.canonical_session_id == source_id
            && expected.physical_generation == *physical_generation =>
        {
            Ok(expected.clone())
        }
        NormalServiceRole::TrustedReader if expected.role == CoreRole::ReadOnly => {
            Ok(expected.clone())
        }
        _ => Err(NativeCoreError::SourceMismatch),
    }
}
