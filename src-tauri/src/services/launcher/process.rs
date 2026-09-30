//! Bounded probes and owned child reaping. Never surface child output in errors.

use std::{
    io::{Read, Seek, SeekFrom},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

pub(super) const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_OUTPUT_BYTES: u64 = 1024 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(20);
const STARTUP_GRACE: Duration = Duration::from_millis(150);

#[allow(non_camel_case_types)]
struct child_guard(Child);

impl Drop for child_guard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub(super) fn private_temp_dir() -> Result<tempfile::TempDir, String> {
    // macOS's system TMPDIR commonly passes through the /var symlink.
    let parent = std::fs::canonicalize(std::env::temp_dir())
        .map_err(|_| "launcher_temp_directory_unavailable".to_string())?;
    let directory = tempfile::Builder::new()
        .prefix("aag_launcher_")
        .tempdir_in(parent)
        .map_err(|_| "launcher_temp_directory_unavailable".to_string())?;
    crate::config::private_fs::prepare_managed_private_dir(directory.path())
        .map_err(String::from)?;
    Ok(directory)
}

pub(super) fn background_command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut command = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW for probes only.
    }
    command
}

pub(super) fn capture_stdout(command: &mut Command, timeout: Duration) -> Result<Vec<u8>, String> {
    let directory = private_temp_dir()?;
    let output_path = directory.path().join("stdout");
    crate::config::private_fs::ensure_private_file(&output_path).map_err(String::from)?;
    let mut output = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(output_path)
        .map_err(|_| "launcher_probe_output_failed".to_string())?;
    command.stdin(Stdio::null()).stderr(Stdio::null()).stdout(
        output
            .try_clone()
            .map_err(|_| "launcher_probe_output_failed".to_string())?,
    );
    let mut child = child_guard(
        command
            .spawn()
            .map_err(|_| "launcher_probe_spawn_failed".to_string())?,
    );
    let deadline = Instant::now() + timeout;
    loop {
        if output
            .metadata()
            .map_err(|_| "launcher_probe_output_failed".to_string())?
            .len()
            > MAX_OUTPUT_BYTES
        {
            return Err("launcher_probe_output_limit".into());
        }
        if let Some(status) = child
            .0
            .try_wait()
            .map_err(|_| "launcher_probe_wait_failed".to_string())?
        {
            if !status.success() {
                return Err("launcher_probe_failed".into());
            }
            output
                .seek(SeekFrom::Start(0))
                .map_err(|_| "launcher_probe_output_failed".to_string())?;
            let mut bytes = Vec::new();
            output
                .take(MAX_OUTPUT_BYTES + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| "launcher_probe_output_failed".to_string())?;
            if bytes.len() as u64 > MAX_OUTPUT_BYTES {
                return Err("launcher_probe_output_limit".into());
            }
            return Ok(bytes);
        }
        if Instant::now() >= deadline {
            return Err("launcher_probe_timeout".into());
        }
        thread::sleep(POLL_INTERVAL);
    }
}

/// Detect immediate launch errors, then reap the external application's child
/// after it exits. A successful dispatch is not a claim that its UI is ready.
pub(super) fn launch(command: &mut Command) -> Result<(), String> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    launch_owned(command)
}

fn launch_owned(command: &mut Command) -> Result<(), String> {
    let mut child = child_guard(
        command
            .spawn()
            .map_err(|_| "launcher_spawn_failed".to_string())?,
    );
    let deadline = Instant::now() + STARTUP_GRACE;
    while Instant::now() < deadline {
        if let Some(status) = child
            .0
            .try_wait()
            .map_err(|_| "launcher_wait_failed".to_string())?
        {
            return if status.success() {
                Ok(())
            } else {
                Err("launcher_process_failed".into())
            };
        }
        thread::sleep(POLL_INTERVAL);
    }
    thread::Builder::new()
        .name("aag_launcher_reaper".into())
        .spawn(move || {
            let _ = child.0.wait();
        })
        .map_err(|_| "launcher_reaper_failed".to_string())?;
    Ok(())
}

pub(super) fn validate_environment(environment: &[(String, String)]) -> Result<(), String> {
    for (name, value) in environment {
        if name.is_empty()
            || !name.bytes().enumerate().all(|(index, byte)| {
                byte == b'_' || byte.is_ascii_alphabetic() || (index > 0 && byte.is_ascii_digit())
            })
            || value.contains('\0')
        {
            return Err("launcher_environment_invalid".into());
        }
    }
    Ok(())
}

pub(super) fn working_directory(value: Option<&str>) -> Result<Option<std::path::PathBuf>, String> {
    let Some(value) = value.filter(|value| !value.trim().is_empty()) else {
        return Ok(None);
    };
    let path = std::path::Path::new(value);
    if !path.is_absolute() || value.chars().any(char::is_control) || !path.is_dir() {
        return Err("launcher_working_directory_invalid".into());
    }
    Ok(Some(path.to_path_buf()))
}
