use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PublicationError {
    Disabled,
    PortInvalid,
    ExistingKeyRequired,
    BackendInvalid,
    StatusInvalid,
    SlotOccupied,
    FunnelConflict,
    OwnershipUnknown,
    OwnershipLost,
    ProcessUnavailable,
    ProcessStartFailed,
    ProcessOwnershipUnavailable,
    ProcessResumeFailed,
    ProcessExited,
    ProcessStatusFailed,
    StartupTimeout,
    StopTimeout,
    OutputLimitExceeded,
    CommandFailed,
    CommandTimeout,
    OperationCancelled,
    PlatformUnsupported,
}

impl PublicationError {
    pub fn code(self) -> &'static str {
        match self {
            Self::Disabled => "tailnet_publication_disabled",
            Self::PortInvalid => "tailnet_publication_port_invalid",
            Self::ExistingKeyRequired => "tailnet_publication_existing_key_required",
            Self::BackendInvalid => "tailnet_publication_backend_invalid",
            Self::StatusInvalid => "tailnet_publication_status_invalid",
            Self::SlotOccupied => "tailnet_publication_slot_occupied",
            Self::FunnelConflict => "tailnet_publication_funnel_conflict",
            Self::OwnershipUnknown => "tailnet_publication_ownership_unknown",
            Self::OwnershipLost => "tailnet_publication_ownership_lost",
            Self::ProcessUnavailable => "tailnet_publication_process_unavailable",
            Self::ProcessStartFailed => "tailnet_publication_process_start_failed",
            Self::ProcessOwnershipUnavailable => {
                "tailnet_publication_process_ownership_unavailable"
            }
            Self::ProcessResumeFailed => "tailnet_publication_process_resume_failed",
            Self::ProcessExited => "tailnet_publication_process_exited",
            Self::ProcessStatusFailed => "tailnet_publication_process_status_failed",
            Self::StartupTimeout => "tailnet_publication_startup_timeout",
            Self::StopTimeout => "tailnet_publication_stop_timeout",
            Self::OutputLimitExceeded => "tailnet_publication_output_limit_exceeded",
            Self::CommandFailed => "tailnet_publication_command_failed",
            Self::CommandTimeout => "tailnet_publication_command_timeout",
            Self::OperationCancelled => "tailnet_publication_cancelled",
            Self::PlatformUnsupported => "tailnet_publication_platform_unsupported",
        }
    }
}

impl fmt::Display for PublicationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code())
    }
}

impl std::error::Error for PublicationError {}

impl From<crate::services::owned_process::NativeProcessError> for PublicationError {
    fn from(error: crate::services::owned_process::NativeProcessError) -> Self {
        use crate::services::owned_process::NativeProcessError as Native;
        match error {
            Native::ExecutableUnavailable => Self::ProcessUnavailable,
            Native::SpecificationInvalid | Native::StartFailed => Self::ProcessStartFailed,
            Native::OwnershipUnavailable => Self::ProcessOwnershipUnavailable,
            Native::ResumeFailed => Self::ProcessResumeFailed,
            Native::StatusFailed => Self::ProcessStatusFailed,
            Native::StopTimeout => Self::StopTimeout,
            Native::OutputLimitExceeded => Self::OutputLimitExceeded,
            Native::OperationCancelled => Self::OperationCancelled,
            Native::DeadlineExceeded => Self::CommandTimeout,
            Native::PlatformUnsupported => Self::PlatformUnsupported,
        }
    }
}
