//! Child-process ownership and bounded execution shared by DSH and pnpm.
//! Dropping the owner reaps its descendants; raw output stays private.
#[cfg(test)]
use super::config;
use super::config::failure;
use crate::error::AppError;
#[cfg(test)]
use std::fs;
use std::{
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

pub(super) struct Owned_process {
    child: Child,
    #[cfg(windows)]
    job: windows_job::Job_handle,
}

impl Owned_process {
    pub(super) fn spawn(mut command: Command) -> Result<Self, AppError> {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(test)]
        if let Some(path) = std::env::var_os("AAG_DSH_TEST_STDERR_PATH") {
            let path = std::path::PathBuf::from(path);
            config::safe_path(&path)?;
            let output = fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(&path)
                .map_err(|_| failure("test_capture_unavailable"))?;
            command.stderr(Stdio::from(output));
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command
            .spawn()
            .map_err(|_| failure("process_spawn_failed"))?;
        #[cfg(windows)]
        let job = match windows_job::Job_handle::assign(&child) {
            Ok(job) => job,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        Ok(Self {
            child,
            #[cfg(windows)]
            job,
        })
    }
    pub(super) fn id(&self) -> u32 {
        self.child.id()
    }
    pub(super) fn try_wait(&mut self) -> Result<Option<std::process::ExitStatus>, AppError> {
        self.child
            .try_wait()
            .map_err(|_| failure("process_status_failed"))
    }
}

impl Drop for Owned_process {
    fn drop(&mut self) {
        #[cfg(windows)]
        self.job.terminate();
        #[cfg(unix)]
        {
            extern "C" {
                fn kill(pid: i32, signal: i32) -> i32;
            }
            // A new process group was explicitly assigned at spawn.
            unsafe {
                kill(-(self.child.id() as i32), 9);
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub(super) fn run_bounded(
    command: Command,
    timeout_secs: u64,
    cancellation: &AtomicBool,
) -> Result<(), AppError> {
    if cancellation.load(Ordering::SeqCst) {
        return Err(failure("cancelled"));
    }
    let mut child = Owned_process::spawn(command)?;
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        if cancellation.load(Ordering::SeqCst) {
            return Err(failure("cancelled"));
        }
        if let Some(status) = child.try_wait()? {
            return if status.success() {
                Ok(())
            } else {
                Err(failure("package_manager_failed"))
            };
        }
        if Instant::now() >= deadline {
            return Err(failure("timed_out"));
        }
        thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(windows)]
mod windows_job {
    use super::failure;
    use crate::error::AppError;
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
    #[allow(non_camel_case_types)]
    pub(super) struct Job_handle(*mut c_void);
    // A Windows handle is safe to move; ownership remains unique and protected
    // by the manager mutex when used for a running DSH instance.
    unsafe impl Send for Job_handle {}
    impl Job_handle {
        pub(super) fn assign(child: &Child) -> Result<Self, AppError> {
            unsafe {
                let handle = Self(CreateJobObjectW(std::ptr::null(), std::ptr::null()));
                if handle.0.is_null() {
                    return Err(failure("process_job_create_failed"));
                }
                let mut limits: Extended_limits = std::mem::zeroed();
                limits.basic.flags = 0x2000;
                if SetInformationJobObject(
                    handle.0,
                    9,
                    &limits as *const _ as *const c_void,
                    std::mem::size_of::<Extended_limits>() as u32,
                ) == 0
                    || AssignProcessToJobObject(handle.0, child.as_raw_handle() as *mut c_void) == 0
                {
                    return Err(failure("process_job_assign_failed"));
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
