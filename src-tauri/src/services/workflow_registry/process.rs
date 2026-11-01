//! Bounded fixed-argv child execution for local policy and version reads.
//!
//! File-backed stdio avoids an inherited pipe keeping a reader or writer
//! thread blocked after the direct child exits. The files are task-owned and
//! removed when capture returns; a directory the host will not release is
//! reported and never replaces the capture outcome. This is local process
//! ownership, not a sandbox for hostile programs.
use std::{
    ffi::{OsStr, OsString},
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
    process::{Child, Command, Stdio},
    sync::{Arc, Condvar, Mutex},
    thread,
    time::{Duration, Instant},
};

const POLL_INTERVAL: Duration = Duration::from_millis(20);
const REAP_GRACE: Duration = Duration::from_millis(100);
const CLEANUP_GRACE: Duration = Duration::from_millis(200);
// Runtime discovery, owned temporary files, and home discovery only. No
// provider credentials, proxy settings, or interpreter customization survives.
const COMMON_ENVIRONMENT: &[&str] = &["PATH", "HOME", "TMPDIR", "TMP", "TEMP"];
#[cfg(windows)]
const PLATFORM_ENVIRONMENT: &[&str] = &[
    "SYSTEMROOT",
    "WINDIR",
    "COMSPEC",
    "PATHEXT",
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "APPDATA",
    "LOCALAPPDATA",
    "PROGRAMDATA",
    "PROGRAMFILES",
    "PROGRAMFILES(X86)",
];
#[cfg(not(windows))]
const PLATFORM_ENVIRONMENT: &[&str] = &[];

fn permitted_environment(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    COMMON_ENVIRONMENT
        .iter()
        .chain(PLATFORM_ENVIRONMENT)
        .any(|allowed| {
            if cfg!(windows) {
                name.eq_ignore_ascii_case(allowed)
            } else {
                name == *allowed
            }
        })
}

fn isolate_environment(
    command: &mut Command,
    environment: impl IntoIterator<Item = (OsString, OsString)>,
) {
    command.env_clear().envs(
        environment
            .into_iter()
            .filter(|(name, _)| permitted_environment(name)),
    );
}

pub fn python_arguments<'a>(script: &'a str, arguments: &[&'a str]) -> Vec<&'a str> {
    // Unlike -I, -E -s preserves sibling imports from the reviewed script's
    // directory while ignoring PYTHON* variables and user site customization.
    let mut result = vec!["-E", "-s", "-B", "-X", "utf8", script];
    result.extend_from_slice(arguments);
    result
}

enum Reaper_request {
    Pending,
    Reap(Child),
    Cancelled,
}

struct Child_reaper {
    request: Arc<(Mutex<Reaper_request>, Condvar)>,
}

impl Child_reaper {
    fn reserve() -> Result<Self, String> {
        let request = Arc::new((Mutex::new(Reaper_request::Pending), Condvar::new()));
        let worker_request = Arc::clone(&request);
        thread::Builder::new()
            .name("workflow_child_reaper".into())
            .spawn(move || {
                let (slot, wake) = &*worker_request;
                let mut request = slot.lock().unwrap_or_else(|error| error.into_inner());
                while matches!(*request, Reaper_request::Pending) {
                    request = wake
                        .wait(request)
                        .unwrap_or_else(|error| error.into_inner());
                }
                let action = std::mem::replace(&mut *request, Reaper_request::Cancelled);
                drop(request);
                // Release the handoff lock before waiting on a potentially
                // delayed exit; capture never waits for the reaper itself.
                if let Reaper_request::Reap(child) = action {
                    wait_for_owned_child(child);
                }
            })
            .map_err(|_| "workflow:process_reaper_unavailable".to_string())?;
        Ok(Self { request })
    }

    fn reap(&self, mut child: Child) {
        let deadline = Instant::now() + REAP_GRACE;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => return,
                Err(error) if child_already_reaped(&error) => return,
                Err(error) if error.kind() != std::io::ErrorKind::Interrupted => break,
                _ => {}
            }
            if Instant::now() >= deadline {
                break;
            }
            thread::sleep(POLL_INTERVAL);
        }
        let (slot, wake) = &*self.request;
        *slot.lock().unwrap_or_else(|error| error.into_inner()) = Reaper_request::Reap(child);
        wake.notify_one();
    }
}

impl Drop for Child_reaper {
    fn drop(&mut self) {
        let (slot, wake) = &*self.request;
        let mut request = slot.lock().unwrap_or_else(|error| error.into_inner());
        if matches!(*request, Reaper_request::Pending) {
            *request = Reaper_request::Cancelled;
        }
        wake.notify_one();
    }
}

fn child_already_reaped(error: &std::io::Error) -> bool {
    // ECHILD refers to this retained direct-child wait identity. Never infer
    // ownership or send a signal from a recycled numeric process identifier.
    cfg!(unix) && error.raw_os_error() == Some(10)
}

fn wait_for_owned_child(mut child: Child) {
    loop {
        match child.wait() {
            Ok(_) => return,
            Err(error) if child_already_reaped(&error) => return,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => {
                // Unknown wait failures are not terminal evidence. Quarantine
                // the retained Child on its reserved owner without retrying a
                // permanent error or blocking the capture caller.
                loop {
                    thread::park();
                }
            }
        }
    }
}

struct Owned_child {
    child: Option<Child>,
    reaper: Child_reaper,
    #[cfg(windows)]
    job: windows_job::Job_handle,
    #[cfg(unix)]
    group_id: i32,
}

impl Owned_child {
    fn spawn(command: Command) -> Result<Self, String> {
        Self::spawn_with_reaper(command, Child_reaper::reserve)
    }

    fn spawn_with_reaper(
        mut command: Command,
        reserve_reaper: impl FnOnce() -> Result<Child_reaper, String>,
    ) -> Result<Self, String> {
        // Reaper availability is a launch prerequisite, not a cleanup fallback.
        let reaper = reserve_reaper()?;
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command
            .spawn()
            .map_err(|_| "workflow:process_spawn_failed".to_string())?;
        #[cfg(windows)]
        let job = match windows_job::Job_handle::assign(&child) {
            Ok(job) => job,
            Err(error) => {
                let _ = child.kill();
                reaper.reap(child);
                return Err(error);
            }
        };
        #[cfg(unix)]
        let group_id = child.id() as i32;
        Ok(Self {
            child: Some(child),
            reaper,
            #[cfg(windows)]
            job,
            #[cfg(unix)]
            group_id,
        })
    }
}

impl Drop for Owned_child {
    fn drop(&mut self) {
        #[cfg(windows)]
        self.job.terminate();
        #[cfg(unix)]
        unsafe {
            // The group was created by this command before exec.
            kill(-self.group_id, 9);
        }
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            self.reaper.reap(child);
        }
    }
}

#[cfg(unix)]
extern "C" {
    fn kill(pid: i32, signal: i32) -> i32;
}

fn private_stdio() -> Result<(tempfile::TempDir, File, File), String> {
    let parent = std::fs::canonicalize(std::env::temp_dir())
        .map_err(|_| "workflow:process_temp_unavailable".to_string())?;
    let directory = tempfile::Builder::new()
        .prefix("aag_workflow_")
        .tempdir_in(parent)
        .map_err(|_| "workflow:process_temp_unavailable".to_string())?;
    crate::config::private_fs::prepare_managed_private_dir(directory.path())
        .map_err(|_| "workflow:process_temp_unavailable".to_string())?;
    let input_path = directory.path().join("stdin");
    let output_path = directory.path().join("stdout");
    for path in [&input_path, &output_path] {
        crate::config::private_fs::ensure_private_file(path)
            .map_err(|_| "workflow:process_temp_unavailable".to_string())?;
    }
    let input = OpenOptions::new()
        .read(true)
        .write(true)
        .open(input_path)
        .map_err(|_| "workflow:process_temp_unavailable".to_string())?;
    let output = OpenOptions::new()
        .read(true)
        .write(true)
        .open(output_path)
        .map_err(|_| "workflow:process_temp_unavailable".to_string())?;
    Ok((directory, input, output))
}

/// Removes an owned private temp directory, retrying until the deadline because
/// a killed child can release a Windows file handle just after termination.
/// A directory already gone counts as removed: the remaining failure mode is a
/// host that will not release the directory at all, and that is reported to the
/// caller instead of being retried indefinitely.
fn remove_owned_temp_dir(
    path: &Path,
    deadline: Instant,
    remove: &mut impl FnMut(&Path) -> std::io::Result<()>,
) -> Result<(), String> {
    loop {
        match remove(path) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(_) => {}
        }
        if Instant::now() >= deadline {
            return Err("workflow:process_temp_cleanup_failed".into());
        }
        thread::sleep(POLL_INTERVAL);
    }
}

pub fn capture(
    program: &Path,
    args: &[&str],
    input: &[u8],
    cwd: Option<&Path>,
    timeout: Duration,
    max_output: usize,
) -> Result<Vec<u8>, String> {
    capture_with_remover(program, args, input, cwd, timeout, max_output, |path| {
        std::fs::remove_dir_all(path)
    })
}

fn capture_with_remover(
    program: &Path,
    args: &[&str],
    input: &[u8],
    cwd: Option<&Path>,
    timeout: Duration,
    max_output: usize,
    mut remove_dir: impl FnMut(&Path) -> std::io::Result<()>,
) -> Result<Vec<u8>, String> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or("workflow:process_deadline_overflow")?;
    let (directory, mut stdin, mut stdout) = private_stdio()?;
    let result = (|| {
        stdin
            .write_all(input)
            .and_then(|_| stdin.flush())
            .and_then(|_| stdin.seek(SeekFrom::Start(0)).map(|_| ()))
            .map_err(|_| "workflow:process_stdin_failed".to_string())?;
        if Instant::now() >= deadline {
            return Err("workflow:process_timeout".into());
        }
        let mut command = Command::new(program);
        isolate_environment(&mut command, std::env::vars_os());
        command
            .args(args)
            .stdin(Stdio::from(stdin))
            .stdout(Stdio::from(
                stdout
                    .try_clone()
                    .map_err(|_| "workflow:process_output_failed".to_string())?,
            ))
            .stderr(Stdio::null());
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        let mut owned = Owned_child::spawn(command)?;
        loop {
            if Instant::now() >= deadline {
                return Err("workflow:process_timeout".into());
            }
            if stdout
                .metadata()
                .map_err(|_| "workflow:process_output_failed".to_string())?
                .len()
                > max_output as u64
            {
                return Err("workflow:process_output_limit".into());
            }
            if let Some(status) = owned
                .child
                .as_mut()
                .expect("owned direct child")
                .try_wait()
                .map_err(|_| "workflow:process_wait_failed".to_string())?
            {
                if !status.success() {
                    return Err("workflow:process_failed".into());
                }
                // Drop kills any still-owned descendant before the file is read.
                drop(owned);
                if Instant::now() >= deadline {
                    return Err("workflow:process_timeout".into());
                }
                stdout
                    .seek(SeekFrom::Start(0))
                    .map_err(|_| "workflow:process_output_failed".to_string())?;
                let mut bytes = Vec::new();
                Read::by_ref(&mut stdout)
                    .take(max_output.saturating_add(1) as u64)
                    .read_to_end(&mut bytes)
                    .map_err(|_| "workflow:process_output_failed".to_string())?;
                if bytes.len() > max_output {
                    return Err("workflow:process_output_limit".into());
                }
                return Ok(bytes);
            }
            thread::sleep(POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())));
        }
    })();
    drop(stdout);
    let cleanup_deadline = Instant::now() + CLEANUP_GRACE;
    // A retained private temp directory is reported, never substituted for the
    // capture result. A cleanup race on a slow host must not turn a working CLI
    // into an unavailable observation, nor hide a real timeout or output limit.
    if remove_owned_temp_dir(directory.path(), cleanup_deadline, &mut remove_dir).is_err() {
        log::warn!("workflow: temp directory retained code=process_temp_cleanup_failed");
    }
    drop(directory);
    result
}

#[cfg(windows)]
mod windows_job {
    // Matches the existing DSH-owned Job Object layout and close-on-drop policy.
    // std::process does not expose atomic create-in-job: assignment immediately
    // after spawn has a small race. Assignment failure kills the direct child
    // and returns an error; this is not a hostile-code containment guarantee.
    use std::{ffi::c_void, os::windows::io::AsRawHandle, process::Child};

    #[repr(C)]
    struct Basic_limits {
        process_user_time: i64,
        job_user_time: i64,
        flags: u32,
        minimum_working_set: usize,
        maximum_working_set: usize,
        active_process_limit: u32,
        affinity: usize,
        priority_class: u32,
        scheduling_class: u32,
    }
    #[repr(C)]
    struct Io_counters {
        read_operations: u64,
        write_operations: u64,
        other_operations: u64,
        read_bytes: u64,
        write_bytes: u64,
        other_bytes: u64,
    }
    #[repr(C)]
    struct Extended_limits {
        basic: Basic_limits,
        io: Io_counters,
        process_memory_limit: usize,
        job_memory_limit: usize,
        peak_process_memory: usize,
        peak_job_memory: usize,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn CreateJobObjectW(attributes: *const c_void, name: *const u16) -> *mut c_void;
        fn SetInformationJobObject(
            job: *mut c_void,
            class: i32,
            info: *const c_void,
            size: u32,
        ) -> i32;
        fn AssignProcessToJobObject(job: *mut c_void, process: *mut c_void) -> i32;
        fn TerminateJobObject(job: *mut c_void, exit_code: u32) -> i32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }
    pub(super) struct Job_handle(*mut c_void);
    impl Job_handle {
        pub(super) fn assign(child: &Child) -> Result<Self, String> {
            unsafe {
                let handle = Self(CreateJobObjectW(std::ptr::null(), std::ptr::null()));
                if handle.0.is_null() {
                    return Err("workflow:process_job_create_failed".into());
                }
                let mut limits: Extended_limits = std::mem::zeroed();
                limits.basic.flags = 0x2000; // JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
                if SetInformationJobObject(
                    handle.0,
                    9, // JobObjectExtendedLimitInformation
                    &limits as *const _ as *const c_void,
                    std::mem::size_of::<Extended_limits>() as u32,
                ) == 0
                    || AssignProcessToJobObject(handle.0, child.as_raw_handle()) == 0
                {
                    return Err("workflow:process_job_assign_failed".into());
                }
                Ok(handle)
            }
        }
        pub(super) fn terminate(&self) {
            unsafe {
                TerminateJobObject(self.0, 1);
            }
        }
    }
    impl Drop for Job_handle {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    CloseHandle(self.0);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::{env, fs};

    const FIXTURE_NAME: &str = "services::workflow_registry::process::tests::capture_fixture";

    // Executed only in a separate copy of the test binary with an exact test
    // filter. The descendant inherits the direct child's stdout file handle.
    #[test]
    #[ignore]
    fn capture_fixture() {
        let envelope: serde_json::Value = serde_json::from_reader(std::io::stdin()).unwrap();
        let role = envelope["role"].as_str().unwrap();
        let marker = envelope["marker"].as_str().unwrap();
        match role {
            "parent" | "parent_wait" => {
                let wait_for_timeout = role == "parent_wait";
                let mut descendant = Command::new(env::current_exe().expect("test binary"))
                    .args(["--ignored", "--exact", FIXTURE_NAME])
                    .stdin(Stdio::piped())
                    .stdout(Stdio::inherit())
                    .stderr(Stdio::null())
                    .spawn()
                    .expect("spawn fixture descendant");
                let descendant_input = serde_json::to_vec(&serde_json::json!({
                    "role": "descendant", "marker": marker
                }))
                .unwrap();
                descendant
                    .stdin
                    .take()
                    .unwrap()
                    .write_all(&descendant_input)
                    .unwrap();
                let marker_path = Path::new(marker);
                let deadline = Instant::now() + Duration::from_secs(2);
                while !marker_path.exists() && Instant::now() < deadline {
                    thread::sleep(Duration::from_millis(10));
                }
                assert!(marker_path.exists(), "descendant did not start");
                let _ = descendant.try_wait();
                if wait_for_timeout {
                    thread::sleep(Duration::from_secs(20));
                }
            }
            "descendant" => {
                fs::write(marker, std::process::id().to_string()).expect("write fixture pid");
                thread::sleep(Duration::from_secs(20));
            }
            "output" => {
                fs::write(marker, std::process::id().to_string()).unwrap();
                std::io::stdout()
                    .write_all(&vec![b'x'; 16 * 1024])
                    .expect("write output");
                thread::sleep(Duration::from_secs(20));
            }
            "echo" => {
                let input: Vec<u8> = serde_json::from_value(envelope["input"].clone()).unwrap();
                std::io::stdout()
                    .write_all(&input)
                    .expect("write fixture output");
            }
            "environment" => {
                for name in [
                    "WORKFLOW_TEST_SECRET",
                    "PYTHONPATH",
                    "PYTHONHOME",
                    "PYTHONUSERBASE",
                ] {
                    assert!(
                        env::var_os(name).is_none(),
                        "ambient setting reached fixture"
                    );
                }
                std::io::stdout()
                    .write_all(b"environment_isolated")
                    .unwrap();
            }
            "wait" => {
                fs::write(marker, std::process::id().to_string()).unwrap();
                thread::sleep(Duration::from_secs(20));
            }
            _ => panic!("fixture role missing"),
        }
    }

    fn run_fixture(
        role: &str,
        marker: &Path,
        input: &[u8],
        timeout: Duration,
        max_output: usize,
    ) -> Result<Vec<u8>, String> {
        run_fixture_with_remover(role, marker, input, timeout, max_output, &mut |path| {
            std::fs::remove_dir_all(path)
        })
    }

    /// Single fixture entry point. `remove` is the temp-directory seam: the
    /// cleanup-failure regression tests pass a remover that performs the real
    /// removal but reports failure, so the regression is exercised without
    /// leaving an aag_workflow_* directory behind.
    fn run_fixture_with_remover(
        role: &str,
        marker: &Path,
        input: &[u8],
        timeout: Duration,
        max_output: usize,
        remove: &mut impl FnMut(&Path) -> std::io::Result<()>,
    ) -> Result<Vec<u8>, String> {
        let envelope = serde_json::to_vec(&serde_json::json!({
            "role": role, "marker": marker, "input": input
        }))
        .unwrap();
        capture_with_remover(
            &env::current_exe().expect("test binary"),
            &["--ignored", "--exact", FIXTURE_NAME],
            &envelope,
            None,
            timeout,
            max_output,
            remove,
        )
    }

    #[test]
    fn reaper_setup_failure_prevents_child_launch() {
        let directory = tempfile::tempdir().unwrap();
        // A non-existent executable must still report the reaper preflight
        // failure, proving Command::spawn was not reached.
        let result = Owned_child::spawn_with_reaper(
            Command::new(directory.path().join("missing_executable")),
            || Err("workflow:process_reaper_unavailable".into()),
        );
        assert!(matches!(result, Err(error) if error == "workflow:process_reaper_unavailable"));
    }

    #[test]
    fn wait_error_classification_requires_already_reaped_identity() {
        assert_eq!(
            child_already_reaped(&std::io::Error::from_raw_os_error(10)),
            cfg!(unix)
        );
        assert!(!child_already_reaped(&std::io::Error::from(
            std::io::ErrorKind::Interrupted
        )));
        assert!(!child_already_reaped(&std::io::Error::from(
            std::io::ErrorKind::PermissionDenied
        )));
    }

    #[test]
    fn child_environment_excludes_credentials_proxies_and_python_poisoning() {
        let mut command = Command::new("fixture_program");
        let environment = [
            ("PATH", "runtime_path"),
            ("HOME", "owned_home"),
            ("OPENAI_API_KEY", "synthetic_secret"),
            ("TYPESAFE_API_KEY", "synthetic_secret"),
            ("HTTP_PROXY", "synthetic_proxy"),
            ("HTTPS_PROXY", "synthetic_proxy"),
            ("ALL_PROXY", "synthetic_proxy"),
            ("PYTHONPATH", "poisoned_module_path"),
            ("PYTHONHOME", "poisoned_python_home"),
            ("PYTHONUSERBASE", "poisoned_user_site"),
            ("UNRECOGNIZED_SETTING", "synthetic_value"),
        ]
        .map(|(name, value)| (OsString::from(name), OsString::from(value)));
        isolate_environment(&mut command, environment);
        let entries: Vec<_> = command.get_envs().collect();
        assert_eq!(entries.len(), 2);
        assert!(entries
            .iter()
            .all(|(name, _)| *name == OsStr::new("PATH") || *name == OsStr::new("HOME")));
        let arguments = python_arguments("reviewed_script.py", &["--policy-only"]);
        assert_eq!(
            &arguments[..6],
            &["-E", "-s", "-B", "-X", "utf8", "reviewed_script.py"]
        );
    }

    #[test]
    fn capture_clears_actual_ambient_environment() {
        struct Restore_environment(Vec<(&'static str, Option<OsString>)>);
        impl Drop for Restore_environment {
            fn drop(&mut self) {
                for (name, value) in &self.0 {
                    match value {
                        Some(value) => env::set_var(name, value),
                        None => env::remove_var(name),
                    }
                }
            }
        }
        let names = [
            "WORKFLOW_TEST_SECRET",
            "PYTHONPATH",
            "PYTHONHOME",
            "PYTHONUSERBASE",
        ];
        let restore = Restore_environment(
            names
                .iter()
                .map(|name| (*name, env::var_os(name)))
                .collect(),
        );
        for name in names {
            env::set_var(name, "synthetic_poisoned_environment");
        }
        let directory = tempfile::tempdir().unwrap();
        let result = run_fixture(
            "environment",
            &directory.path().join("unused"),
            &[],
            Duration::from_secs(2),
            4096,
        );
        drop(restore);
        let output = result.expect("isolated capture");
        assert!(output
            .windows(b"environment_isolated".len())
            .any(|window| window == b"environment_isolated"));
    }

    #[cfg(unix)]
    #[test]
    fn timeout_and_overflow_reap_the_direct_child() {
        extern "C" {
            fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
        }
        for (role, expected_error, output_limit) in [
            ("wait", "workflow:process_timeout", 4096),
            ("output", "workflow:process_output_limit", 256),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let marker = directory.path().join("direct_child_pid");
            let result = run_fixture(role, &marker, &[], Duration::from_secs(2), output_limit);
            assert_eq!(result.unwrap_err(), expected_error);
            let pid: i32 = fs::read_to_string(marker).unwrap().parse().unwrap();
            // ECHILD proves the owned direct process has already been waited on;
            // observing its PID absent alone would not prove zombie cleanup.
            let mut status = 0;
            assert_eq!(unsafe { waitpid(pid, &mut status, 1) }, -1);
            assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(10));
        }
    }

    #[test]
    fn inherited_stdout_descendant_cannot_block_capture() {
        let directory = tempfile::tempdir().expect("fixture directory");
        let marker = directory.path().join("descendant_pid");
        let timeout = Duration::from_secs(3);
        let started = Instant::now();
        let result = run_fixture("parent", &marker, &[], timeout, 4096);
        assert!(
            started.elapsed() < timeout + Duration::from_millis(300),
            "capture exceeded the bound"
        );
        assert!(result.is_ok(), "{result:?}");
        let pid: u32 = fs::read_to_string(&marker)
            .expect("descendant pid marker")
            .parse()
            .expect("pid");
        let cleanup_deadline = Instant::now() + Duration::from_secs(2);
        while process_is_running(pid) && Instant::now() < cleanup_deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !process_is_running(pid),
            "owned descendant survived capture"
        );
    }

    #[test]
    fn output_limit_stops_running_child() {
        let directory = tempfile::tempdir().expect("fixture directory");
        let started = Instant::now();
        let result = run_fixture(
            "output",
            &directory.path().join("unused"),
            &[],
            Duration::from_secs(2),
            256,
        );
        assert_eq!(result.unwrap_err(), "workflow:process_output_limit");
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "output cap did not stop execution"
        );
    }

    #[test]
    fn timeout_terminates_owned_descendant() {
        let directory = tempfile::tempdir().expect("fixture directory");
        let marker = directory.path().join("descendant_pid");
        let timeout = Duration::from_secs(2);
        let started = Instant::now();
        let result = run_fixture("parent_wait", &marker, &[], timeout, 4096);
        assert_eq!(result.unwrap_err(), "workflow:process_timeout");
        assert!(
            started.elapsed() < timeout + Duration::from_millis(300),
            "timeout cleanup exceeded the bound: elapsed={:?} timeout={:?}",
            started.elapsed(),
            timeout
        );
        let pid: u32 = fs::read_to_string(&marker)
            .expect("descendant pid marker")
            .parse()
            .expect("pid");
        let cleanup_deadline = Instant::now() + Duration::from_secs(2);
        while process_is_running(pid) && Instant::now() < cleanup_deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !process_is_running(pid),
            "owned descendant survived timeout"
        );
    }

    #[test]
    fn private_file_stdin_round_trips_without_a_writer_thread() {
        let directory = tempfile::tempdir().expect("fixture directory");
        let output = run_fixture(
            "echo",
            &directory.path().join("unused"),
            b"fixed_input_payload",
            Duration::from_secs(2),
            4096,
        )
        .expect("capture");
        assert!(output
            .windows(b"fixed_input_payload".len())
            .any(|window| window == b"fixed_input_payload"));
    }

    #[test]
    fn temp_cleanup_failure_does_not_mask_a_successful_capture() {
        // The injected remover always fails so it exercises cleanup failure, but
        // still performs the real removal so no aag_workflow_* directory survives.
        let directory = tempfile::tempdir().unwrap();
        let result = run_fixture_with_remover(
            "echo",
            &directory.path().join("unused"),
            b"fixed_input_payload",
            Duration::from_secs(2),
            4096,
            &mut |path| {
                let _ = std::fs::remove_dir_all(path);
                Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
            },
        );
        assert!(
            result.as_ref().is_ok_and(|bytes| {
                bytes
                    .windows(b"fixed_input_payload".len())
                    .any(|window| window == b"fixed_input_payload")
            }),
            "expected Ok with payload, got {result:?}"
        );
    }

    #[test]
    fn temp_cleanup_failure_does_not_mask_a_timeout() {
        let directory = tempfile::tempdir().unwrap();
        let result = run_fixture_with_remover(
            "wait",
            &directory.path().join("pid_marker"),
            &[],
            Duration::from_secs(2),
            4096,
            &mut |path| {
                let _ = std::fs::remove_dir_all(path);
                Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
            },
        );
        assert_eq!(result.unwrap_err(), "workflow:process_timeout");
    }

    #[test]
    fn cleanup_grace_is_bounded_and_reports_failure() {
        let directory = tempfile::tempdir().unwrap();
        let path_buf = directory.path().to_path_buf();
        let result = remove_owned_temp_dir(&path_buf, Instant::now(), &mut |_| {
            Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
        });
        assert_eq!(result.unwrap_err(), "workflow:process_temp_cleanup_failed");
    }

    #[test]
    fn cleanup_tolerates_an_already_removed_directory() {
        let directory = tempfile::tempdir().unwrap();
        let path_buf = directory.path().to_path_buf();
        // Simulate a race: the remover reports NotFound.
        let result = remove_owned_temp_dir(
            &path_buf,
            Instant::now() + Duration::from_secs(1),
            &mut |_| Err(std::io::Error::from(std::io::ErrorKind::NotFound)),
        );
        assert_eq!(result, Ok(()));
    }

    #[cfg(windows)]
    fn process_is_running(pid: u32) -> bool {
        use std::ffi::c_void;
        #[link(name = "kernel32")]
        extern "system" {
            fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
            fn WaitForSingleObject(handle: *mut c_void, milliseconds: u32) -> u32;
            fn CloseHandle(handle: *mut c_void) -> i32;
            fn GetLastError() -> u32;
        }
        unsafe {
            let handle = OpenProcess(0x0010_0000, 0, pid); // SYNCHRONIZE
            if handle.is_null() {
                assert_eq!(GetLastError(), 87, "cannot inspect fixture descendant");
                return false;
            }
            let wait_result = WaitForSingleObject(handle, 0);
            CloseHandle(handle);
            assert!(
                matches!(wait_result, 0 | 0x0000_0102),
                "cannot wait on fixture descendant"
            );
            wait_result == 0x0000_0102 // WAIT_TIMEOUT
        }
    }

    #[cfg(unix)]
    fn process_is_running(pid: u32) -> bool {
        unsafe { kill(pid as i32, 0) == 0 }
    }
}
