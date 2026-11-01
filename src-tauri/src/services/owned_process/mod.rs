//! Narrow process/stdio ownership shared by native copy and broker consumers.
//! No service, provider, authentication, or publication policy belongs here.

use std::{
    fmt,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub(crate) use windows::{encode_environment, NativeChild};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativeProcessError {
    ExecutableUnavailable,
    SpecificationInvalid,
    StartFailed,
    OwnershipUnavailable,
    ResumeFailed,
    StatusFailed,
    StopTimeout,
    OutputLimitExceeded,
    OperationCancelled,
    DeadlineExceeded,
    PlatformUnsupported,
}

impl NativeProcessError {
    pub fn code(self) -> &'static str {
        match self {
            Self::ExecutableUnavailable => "owned_process_executable_unavailable",
            Self::SpecificationInvalid => "owned_process_specification_invalid",
            Self::StartFailed => "owned_process_start_failed",
            Self::OwnershipUnavailable => "owned_process_ownership_unavailable",
            Self::ResumeFailed => "owned_process_resume_failed",
            Self::StatusFailed => "owned_process_status_failed",
            Self::StopTimeout => "owned_process_stop_timeout",
            Self::OutputLimitExceeded => "owned_process_output_limit_exceeded",
            Self::OperationCancelled => "owned_process_cancelled",
            Self::DeadlineExceeded => "owned_process_deadline_exceeded",
            Self::PlatformUnsupported => "owned_process_platform_unsupported",
        }
    }
}

impl fmt::Display for NativeProcessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code())
    }
}
impl std::error::Error for NativeProcessError {}

/// Borrowed spawn inputs, consumed only by process creation. None inherits the
/// environment/CWD; an explicit environment is a double-NUL UTF-16 block.
/// No Debug implementation can accidentally print arguments or environment.
#[derive(Clone, Copy)]
pub(crate) struct SpawnSpec<'a> {
    pub executable: &'a Path,
    pub args: &'a [String],
    pub capture_stdout: bool,
    pub pipe_stdin: bool,
    pub environment: Option<&'a [u16]>,
    pub cwd: Option<&'a Path>,
    pub primary_token: Option<&'a OwnedToken>,
}

impl<'a> SpawnSpec<'a> {
    pub fn new(executable: &'a Path, args: &'a [String]) -> Self {
        Self {
            executable,
            args,
            capture_stdout: false,
            pipe_stdin: false,
            environment: None,
            cwd: None,
            primary_token: None,
        }
    }
}

/// An owned primary token produced by the caller-restriction boundary. It is
/// intentionally neither Clone, Debug nor Serde; it cannot be an IPC value.
#[cfg(windows)]
pub(crate) struct OwnedToken {
    handle: std::os::windows::io::OwnedHandle,
    kernel_descriptor: Vec<u8>,
    child_token_descriptor: Vec<u8>,
    #[cfg(test)]
    fixture_process_descriptor: Option<Vec<u8>>,
    #[cfg(test)]
    fixture_thread_descriptor: Option<Vec<u8>>,
}

#[cfg(windows)]
impl OwnedToken {
    /// # Safety
    /// The caller must have created and verified this as a restricted primary
    /// token derived from the current caller's primary token. Alternate-account
    /// and unrestricted handles must not cross this constructor.
    /// Both descriptors must be valid self-relative security descriptors with
    /// the object-specific rights required by this verified token boundary.
    pub(crate) unsafe fn from_caller_derived_restricted(
        handle: std::os::windows::io::OwnedHandle,
        kernel_descriptor: Vec<u8>,
        child_token_descriptor: Vec<u8>,
    ) -> Self {
        Self {
            handle,
            kernel_descriptor,
            child_token_descriptor,
            #[cfg(test)]
            fixture_process_descriptor: None,
            #[cfg(test)]
            fixture_thread_descriptor: None,
        }
    }
    pub(crate) fn as_handle(&self) -> std::os::windows::io::BorrowedHandle<'_> {
        use std::os::windows::io::AsHandle;
        self.handle.as_handle()
    }
    pub(crate) fn kernel_descriptor(&self) -> &[u8] {
        &self.kernel_descriptor
    }
    pub(crate) fn child_token_descriptor(&self) -> &[u8] { &self.child_token_descriptor }
    pub(crate) fn process_descriptor(&self) -> &[u8] {
        #[cfg(test)]
        if let Some(descriptor) = &self.fixture_process_descriptor { return descriptor; }
        &self.kernel_descriptor
    }
    pub(crate) fn thread_descriptor(&self) -> &[u8] {
        #[cfg(test)]
        if let Some(descriptor) = &self.fixture_thread_descriptor { return descriptor; }
        &self.kernel_descriptor
    }
    /// # Safety
    /// Only the isolated token fixture may supply valid self-relative process
    /// and thread descriptors. No authority/default/pipe ACL is changed.
    #[cfg(test)]
    pub(crate) unsafe fn with_fixture_child_object_descriptors(
        mut self, process: Vec<u8>, thread: Vec<u8>,
    ) -> Self {
        self.fixture_process_descriptor = Some(process);
        self.fixture_thread_descriptor = Some(thread);
        self
    }
}

#[cfg(not(windows))]
pub(crate) struct OwnedToken {
    _private: (),
}

/// Output stays private to the caller. Exit-code success policy belongs to the
/// consumer (for example, Robocopy and a JSON status command differ).
pub(crate) struct CommandOutput {
    pub exit_code: u32,
    pub stdout: Vec<u8>,
}

#[cfg(windows)]
pub(crate) fn run_capture(
    spec: &SpawnSpec<'_>,
    cancelled: &AtomicBool,
    deadline: Instant,
    limit: usize,
) -> Result<CommandOutput, NativeProcessError> {
    if cancelled.load(Ordering::SeqCst) {
        return Err(NativeProcessError::OperationCancelled);
    }
    if Instant::now() >= deadline {
        return Err(NativeProcessError::DeadlineExceeded);
    }
    if spec.pipe_stdin {
        return Err(NativeProcessError::SpecificationInvalid);
    }
    let mut spec = *spec;
    spec.capture_stdout = true;
    let mut child = NativeChild::create_suspended(&spec)?;
    if cancelled.load(Ordering::SeqCst) {
        return Err(NativeProcessError::OperationCancelled);
    }
    if Instant::now() >= deadline {
        return Err(NativeProcessError::DeadlineExceeded);
    }
    child.admit_and_resume()?;
    let mut stdout = Vec::new();
    loop {
        if cancelled.load(Ordering::SeqCst) {
            return Err(NativeProcessError::OperationCancelled);
        }
        if Instant::now() >= deadline {
            return Err(NativeProcessError::DeadlineExceeded);
        }
        let eof = child.read_available(&mut stdout, limit)?;
        if let Some(exit_code) = child.try_exit()? {
            if eof {
                return Ok(CommandOutput { exit_code, stdout });
            }
        }
        std::thread::sleep(
            Duration::from_millis(20).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
}

#[cfg(not(windows))]
mod unsupported {
    use super::*;
    use std::{ffi::OsString, fs::File};
    pub(crate) struct NativeChild {
        _private: (),
    }
    impl NativeChild {
        pub fn create_suspended(_spec: &SpawnSpec<'_>) -> Result<Self, NativeProcessError> {
            Err(NativeProcessError::PlatformUnsupported)
        }
        pub fn admit_and_resume(&mut self) -> Result<(), NativeProcessError> {
            Err(NativeProcessError::PlatformUnsupported)
        }
        pub fn try_exit(&self) -> Result<Option<u32>, NativeProcessError> {
            Err(NativeProcessError::PlatformUnsupported)
        }
        pub fn terminate(&mut self, _deadline: Instant) -> Result<(), NativeProcessError> {
            Err(NativeProcessError::PlatformUnsupported)
        }
        pub fn read_available(
            &self,
            _output: &mut Vec<u8>,
            _limit: usize,
        ) -> Result<bool, NativeProcessError> {
            Err(NativeProcessError::PlatformUnsupported)
        }
        pub fn take_stdin(&mut self) -> Option<File> {
            None
        }
        pub fn take_stdout(&mut self) -> Option<File> {
            None
        }
        pub fn try_clone_stdin(&self) -> Result<Option<File>, NativeProcessError> {
            Err(NativeProcessError::PlatformUnsupported)
        }
        pub fn try_clone_stdout(&self) -> Result<Option<File>, NativeProcessError> {
            Err(NativeProcessError::PlatformUnsupported)
        }
    }
    pub(crate) fn encode_environment(
        _pairs: &[(OsString, OsString)],
    ) -> Result<Vec<u16>, NativeProcessError> {
        Err(NativeProcessError::PlatformUnsupported)
    }
}
#[cfg(not(windows))]
pub(crate) use unsupported::{encode_environment, NativeChild};

#[cfg(not(windows))]
pub(crate) fn run_capture(
    _spec: &SpawnSpec<'_>,
    cancelled: &AtomicBool,
    _deadline: Instant,
    _limit: usize,
) -> Result<CommandOutput, NativeProcessError> {
    if cancelled.load(Ordering::SeqCst) {
        Err(NativeProcessError::OperationCancelled)
    } else {
        Err(NativeProcessError::PlatformUnsupported)
    }
}
