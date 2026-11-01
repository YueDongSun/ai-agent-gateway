//! Construction boundary for a protected, committed authority installation.
//! A setting, path, client flag, or successful TCP connection cannot mint it.

use super::client_config::VerifiedClientAuthority;
use std::path::Path;

#[cfg(windows)]
mod protocol;
#[cfg(windows)]
mod windows;

#[cfg(all(windows, test))]
pub(super) use windows::check_config_access_ace;
#[cfg(windows)]
pub(super) use windows::program_files_folder;
#[cfg(windows)]
pub(super) use windows::{canonical_config_path, program_data_folder, protected_object_security};
#[cfg(windows)]
pub(super) use windows::{gateway_policy_peer, verify_gateway_peer};

#[cfg(windows)]
pub(super) fn validate_disk_path(path: &Path) -> Result<(), &'static str> {
    protocol::validate_disk_path(path).map_err(|_| "session_config_path_invalid")
}

#[cfg(windows)]
pub(super) fn validate_runtime_image_paths(
    root: &Path,
    shim: &Path,
    native: &Path,
    program_files: &Path,
) -> Result<(), &'static str> {
    protocol::validate_image_layout(root, shim, native, program_files)
        .map_err(|_| "session_config_image_layout_invalid")
}

/// Called from the launcher's blocking pool. Service identity, a fresh owner
/// response and protected image objects are required; a gateway key is absent
/// from this interface. Missing deployment/commit state remains unavailable.
pub(crate) fn verify_client_authority(root: &Path) -> Result<VerifiedClientAuthority, String> {
    if !root.is_absolute() {
        return Err("session_client_authority_root_invalid".into());
    }
    #[cfg(windows)]
    {
        windows::verify(root).map_err(|error| error.code().into())
    }
    #[cfg(not(windows))]
    {
        Err("session_client_authority_unavailable".into())
    }
}
