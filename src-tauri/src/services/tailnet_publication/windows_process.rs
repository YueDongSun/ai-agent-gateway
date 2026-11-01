//! Tailnet's OS-process boundary. Native ownership lives in owned_process;
//! publication-specific errors and CLI policy remain in this module.

use super::PublicationError;
use crate::services::owned_process::SpawnSpec;
use std::path::Path;

pub(super) use crate::services::owned_process::NativeChild;

pub(super) fn create_suspended(
    executable: &Path,
    args: &[String],
    capture_stdout: bool,
    environment: Option<&[u16]>,
) -> Result<NativeChild, PublicationError> {
    NativeChild::create_suspended(&SpawnSpec {
        capture_stdout,
        environment,
        ..SpawnSpec::new(executable, args)
    })
    .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        ffi::OsString,
        sync::atomic::AtomicBool,
        time::{Duration, Instant},
    };

    #[test]
    fn nonzero_native_exit_keeps_the_publication_command_failure_policy() {
        let executable = std::env::current_exe().unwrap();
        let mut pairs = vec![(
            OsString::from("AAG_OWNED_PROCESS_FIXTURE_MODE"),
            OsString::from("exit7"),
        )];
        if let Some(root) = std::env::var_os("SystemRoot") {
            pairs.push((OsString::from("SystemRoot"), root));
        }
        let environment = crate::services::owned_process::encode_environment(&pairs).unwrap();
        let args = vec![
            "--exact".into(),
            "services::owned_process::windows::tests::owned_child_fixture".into(),
            "--ignored".into(),
            "--nocapture".into(),
        ];
        assert_eq!(
            super::super::process::run_command(
                &executable,
                &args,
                Some(&environment),
                &AtomicBool::new(false),
                Instant::now() + Duration::from_secs(5),
                512 * 1024
            )
            .err(),
            Some(PublicationError::CommandFailed)
        );
    }
}
