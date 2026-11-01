//! Windows child admission before any executable entrypoint can run.
//! The child starts suspended, enters a private kill-on-close job, and only
//! then resumes. AAG itself is never assigned to this job. Only explicit stdio
//! handles are inherited; the job handle remains exclusively parent-owned.

use super::{NativeProcessError, SpawnSpec};
use std::ffi::{c_void, OsStr, OsString};
use std::fs::File;
use std::os::windows::{
    ffi::OsStrExt,
    io::{AsRawHandle, FromRawHandle, OwnedHandle},
};
use std::path::Path;
use std::ptr::{null, null_mut};
use std::time::{Duration, Instant};

type Handle = *mut c_void;
const CREATE_SUSPENDED: u32 = 0x0000_0004;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const CREATE_UNICODE_ENVIRONMENT: u32 = 0x0000_0400;
const EXTENDED_STARTUPINFO_PRESENT: u32 = 0x0008_0000;
const STARTF_USESTDHANDLES: u32 = 0x0000_0100;
const PROC_THREAD_ATTRIBUTE_HANDLE_LIST: usize = 0x0002_0002;
const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x0000_2000;
const WAIT_OBJECT_0: u32 = 0;
const WAIT_TIMEOUT: u32 = 258;
const ERROR_BROKEN_PIPE: u32 = 109;
const MAX_ATTRIBUTE_BYTES: usize = 64 * 1024;
const DROP_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_ENVIRONMENT_UNITS: usize = 128 * 1024;
const MAX_JOB_MEMBERS: usize = 4096;

#[repr(C)]
struct SecurityAttributes {
    length: u32,
    descriptor: *mut c_void,
    inherit: i32,
}

#[repr(C)]
struct StartupInfo {
    size: u32,
    reserved: *mut u16,
    desktop: *mut u16,
    title: *mut u16,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    x_chars: u32,
    y_chars: u32,
    fill: u32,
    flags: u32,
    show: u16,
    reserved_size: u16,
    reserved_bytes: *mut u8,
    stdin: Handle,
    stdout: Handle,
    stderr: Handle,
}

#[repr(C)]
struct StartupInfoEx {
    startup: StartupInfo,
    attributes: *mut c_void,
}

#[repr(C)]
struct ProcessInformation {
    process: Handle,
    thread: Handle,
    process_id: u32,
    thread_id: u32,
}

#[repr(C)]
struct BasicLimits {
    process_time: i64,
    job_time: i64,
    flags: u32,
    minimum_working_set: usize,
    maximum_working_set: usize,
    active_process_limit: u32,
    affinity: usize,
    priority_class: u32,
    scheduling_class: u32,
}

#[repr(C)]
struct IoCounters {
    read_ops: u64,
    write_ops: u64,
    other_ops: u64,
    read_bytes: u64,
    write_bytes: u64,
    other_bytes: u64,
}

#[repr(C)]
struct ExtendedLimits {
    basic: BasicLimits,
    io: IoCounters,
    process_memory: usize,
    job_memory: usize,
    peak_process_memory: usize,
    peak_job_memory: usize,
}

#[repr(C)]
struct BasicAccounting {
    total_user_time: i64,
    total_kernel_time: i64,
    period_user_time: i64,
    period_kernel_time: i64,
    page_faults: u32,
    total_processes: u32,
    active_processes: u32,
    terminated_processes: u32,
}

#[link(name = "kernel32")]
extern "system" {
    fn CreateJobObjectW(attributes: *const SecurityAttributes, name: *const u16) -> Handle;
    fn SetInformationJobObject(job: Handle, class: i32, info: *const c_void, size: u32) -> i32;
    fn AssignProcessToJobObject(job: Handle, process: Handle) -> i32;
    fn TerminateJobObject(job: Handle, code: u32) -> i32;
    fn QueryInformationJobObject(job: Handle, class: i32, info: *mut c_void,
        size: u32, returned: *mut u32) -> i32;
    fn OpenProcess(access: u32, inherit: i32, id: u32) -> Handle;
    fn IsProcessInJob(process: Handle, job: Handle, in_job: *mut i32) -> i32;
    fn CreateFileW(
        name: *const u16,
        access: u32,
        share: u32,
        attributes: *const SecurityAttributes,
        disposition: u32,
        flags: u32,
        template: Handle,
    ) -> Handle;
    fn CreatePipe(
        read: *mut Handle,
        write: *mut Handle,
        attributes: *const SecurityAttributes,
        size: u32,
    ) -> i32;
    fn CreateNamedPipeW(
        name: *const u16,
        open_mode: u32,
        pipe_mode: u32,
        instances: u32,
        out_size: u32,
        in_size: u32,
        timeout: u32,
        attributes: *const SecurityAttributes,
    ) -> Handle;
    fn ConnectNamedPipe(pipe: Handle, overlapped: *mut c_void) -> i32;
    fn SetHandleInformation(handle: Handle, mask: u32, flags: u32) -> i32;
    fn InitializeProcThreadAttributeList(
        list: *mut c_void,
        count: u32,
        flags: u32,
        size: *mut usize,
    ) -> i32;
    fn UpdateProcThreadAttribute(
        list: *mut c_void,
        flags: u32,
        attribute: usize,
        value: *const c_void,
        size: usize,
        previous: *mut c_void,
        returned_size: *mut usize,
    ) -> i32;
    fn DeleteProcThreadAttributeList(list: *mut c_void);
    fn CreateProcessW(
        application: *const u16,
        command: *mut u16,
        process_attributes: *const SecurityAttributes,
        thread_attributes: *const SecurityAttributes,
        inherit: i32,
        flags: u32,
        environment: *const c_void,
        directory: *const u16,
        startup: *const StartupInfo,
        process: *mut ProcessInformation,
    ) -> i32;
    fn ResumeThread(thread: Handle) -> u32;
    fn TerminateProcess(process: Handle, code: u32) -> i32;
    fn WaitForSingleObject(handle: Handle, millis: u32) -> u32;
    fn GetExitCodeProcess(process: Handle, code: *mut u32) -> i32;
    fn PeekNamedPipe(
        pipe: Handle,
        buffer: *mut c_void,
        size: u32,
        read: *mut u32,
        available: *mut u32,
        left: *mut u32,
    ) -> i32;
    fn ReadFile(
        file: Handle,
        buffer: *mut c_void,
        size: u32,
        read: *mut u32,
        overlapped: *mut c_void,
    ) -> i32;
    fn GetLastError() -> u32;
    fn GetCurrentProcess() -> Handle;
    fn GetProcessId(process: Handle) -> u32;
    fn DuplicateHandle(
        source_process: Handle,
        source: Handle,
        target_process: Handle,
        target: *mut Handle,
        access: u32,
        inherit: i32,
        options: u32,
    ) -> i32;
    fn CompareStringOrdinal(
        first: *const u16,
        first_len: i32,
        second: *const u16,
        second_len: i32,
        ignore_case: i32,
    ) -> i32;
}

#[link(name = "advapi32")]
extern "system" {
    fn OpenProcessToken(process: Handle, access: u32, token: *mut Handle) -> i32;
    fn IsTokenRestricted(token: Handle) -> i32;
    fn SetKernelObjectSecurity(object: Handle, info: u32, descriptor: *const c_void) -> i32;
    fn CreateProcessAsUserW(
        token: Handle,
        application: *const u16,
        command: *mut u16,
        process_attributes: *const SecurityAttributes,
        thread_attributes: *const SecurityAttributes,
        inherit: i32,
        flags: u32,
        environment: *const c_void,
        directory: *const u16,
        startup: *const StartupInfo,
        process: *mut ProcessInformation,
    ) -> i32;
}

/// The child always receives the client end. It therefore cannot impersonate
/// the parent through a server-side stdin handle. Both pipe objects use the
/// authority-only descriptor, and the client explicitly requests Identification.
fn restricted_stdio_pipe(
    child_reads: bool,
    attributes: &SecurityAttributes,
) -> Result<(OwnedHandle, OwnedHandle), NativeProcessError> {
    let mut random = [0u8; 16];
    getrandom::getrandom(&mut random).map_err(|_| NativeProcessError::StartFailed)?;
    let nonce: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    let name = wide(OsStr::new(&format!(r"\\.\pipe\aag_owned_stdio_{nonce}")))?;
    let server = own(
        unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                if child_reads { 2 } else { 1 } | 0x0008_0000,
                0,
                1,
                64 * 1024,
                64 * 1024,
                0,
                attributes,
            )
        },
        NativeProcessError::StartFailed,
    )?;
    if unsafe { SetHandleInformation(server.as_raw_handle(), 1, 0) } == 0 {
        return Err(NativeProcessError::StartFailed);
    }
    let client = own(
        unsafe {
            CreateFileW(
                name.as_ptr(),
                if child_reads {
                    0x8000_0000
                } else {
                    0x4000_0080
                },
                0,
                attributes,
                3,
                0x0011_0000,
                null_mut(),
            )
        },
        NativeProcessError::StartFailed,
    )?;
    if unsafe { ConnectNamedPipe(server.as_raw_handle(), null_mut()) } == 0
        && unsafe { GetLastError() } != 535
    {
        return Err(NativeProcessError::StartFailed);
    }
    Ok((server, client))
}

fn own(handle: Handle, error: NativeProcessError) -> Result<OwnedHandle, NativeProcessError> {
    if handle.is_null() || handle as isize == -1 {
        return Err(error);
    }
    // Every accepted handle was created exclusively in this module.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

struct Attributes {
    storage: Vec<usize>,
    _handles: Vec<Handle>,
}

impl Attributes {
    fn new(handles: Vec<Handle>) -> Result<Self, NativeProcessError> {
        let mut size = 0;
        unsafe {
            InitializeProcThreadAttributeList(null_mut(), 1, 0, &mut size);
        }
        if size == 0 || size > MAX_ATTRIBUTE_BYTES {
            return Err(NativeProcessError::StartFailed);
        }
        let mut storage = vec![0usize; size.div_ceil(std::mem::size_of::<usize>())];
        let pointer = storage.as_mut_ptr().cast();
        if unsafe { InitializeProcThreadAttributeList(pointer, 1, 0, &mut size) } == 0 {
            return Err(NativeProcessError::StartFailed);
        }
        let mut attributes = Self {
            storage,
            _handles: handles,
        };
        let values = attributes._handles.as_ptr();
        let values_size = attributes._handles.len() * std::mem::size_of::<Handle>();
        if unsafe {
            UpdateProcThreadAttribute(
                attributes.pointer(),
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
                values.cast(),
                values_size,
                null_mut(),
                null_mut(),
            )
        } == 0
        {
            return Err(NativeProcessError::StartFailed);
        }
        Ok(attributes)
    }
    fn pointer(&mut self) -> *mut c_void {
        self.storage.as_mut_ptr().cast()
    }
}

impl Drop for Attributes {
    fn drop(&mut self) {
        unsafe {
            DeleteProcThreadAttributeList(self.pointer());
        }
    }
}

pub(crate) struct NativeChild {
    process: OwnedHandle,
    primary_thread: Option<OwnedHandle>,
    job: OwnedHandle,
    stdin: Option<OwnedHandle>,
    stdout: Option<OwnedHandle>,
    admitted: bool,
    resumed: bool,
    stop_observers: Vec<OwnedHandle>,
    stop_observation_failed: bool,
}

impl NativeChild {
    #[cfg(test)]
    pub(crate) fn process_id(&self) -> u32 {
        unsafe { GetProcessId(self.process.as_raw_handle()) }
    }
    pub fn create_suspended(spec: &SpawnSpec<'_>) -> Result<Self, NativeProcessError> {
        let executable = spec.executable;
        let args = spec.args;
        let capture_stdout = spec.capture_stdout;
        let environment = spec.environment;
        if !executable.is_absolute()
            || executable
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
            || !std::fs::metadata(executable).is_ok_and(|metadata| metadata.is_file())
        {
            return Err(NativeProcessError::ExecutableUnavailable);
        }
        if environment.is_some_and(|block| {
            block.len() < 2
                || block.len() > MAX_ENVIRONMENT_UNITS
                || block[block.len() - 2..] != [0, 0]
        }) {
            return Err(NativeProcessError::SpecificationInvalid);
        }
        let cwd = spec
            .cwd
            .map(|path| {
                if !path.is_absolute()
                    || path
                        .components()
                        .any(|part| matches!(part, std::path::Component::ParentDir))
                    || !std::fs::metadata(path).is_ok_and(|metadata| metadata.is_dir())
                {
                    return Err(NativeProcessError::SpecificationInvalid);
                }
                wide(path.as_os_str()).map_err(|_| NativeProcessError::SpecificationInvalid)
            })
            .transpose()?;
        let application = wide(executable.as_os_str())?;
        let mut command = command_line(executable, args)?;
        let job = own(
            unsafe { CreateJobObjectW(null(), null()) },
            NativeProcessError::OwnershipUnavailable,
        )?;
        let mut limits: ExtendedLimits = unsafe { std::mem::zeroed() };
        limits.basic.flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if unsafe {
            SetInformationJobObject(
                job.as_raw_handle(),
                9,
                (&limits as *const ExtendedLimits).cast(),
                std::mem::size_of::<ExtendedLimits>() as u32,
            )
        } == 0
        {
            return Err(NativeProcessError::OwnershipUnavailable);
        }
        let security = SecurityAttributes {
            length: std::mem::size_of::<SecurityAttributes>() as u32,
            descriptor: spec
                .primary_token
                .map(|token| token.kernel_descriptor().as_ptr().cast_mut().cast())
                .unwrap_or(null_mut()),
            inherit: 1,
        };
        let nul = own(
            unsafe {
                CreateFileW(
                    wide(OsStr::new("NUL"))?.as_ptr(),
                    0xC000_0000,
                    3,
                    &security,
                    3,
                    0,
                    null_mut(),
                )
            },
            NativeProcessError::StartFailed,
        )?;
        let (stdout_read, stdout_write) = if capture_stdout && spec.primary_token.is_some() {
            let (server, client) = restricted_stdio_pipe(false, &security)?;
            (Some(server), Some(client))
        } else if capture_stdout {
            let (mut read, mut write) = (null_mut(), null_mut());
            if unsafe { CreatePipe(&mut read, &mut write, &security, 0) } == 0 {
                return Err(NativeProcessError::StartFailed);
            }
            let read = own(read, NativeProcessError::StartFailed)?;
            let write = own(write, NativeProcessError::StartFailed)?;
            if unsafe { SetHandleInformation(read.as_raw_handle(), 1, 0) } == 0 {
                return Err(NativeProcessError::StartFailed);
            }
            (Some(read), Some(write))
        } else {
            (None, None)
        };
        let (stdin_read, stdin_write) = if spec.pipe_stdin && spec.primary_token.is_some() {
            let (server, client) = restricted_stdio_pipe(true, &security)?;
            (Some(client), Some(server))
        } else if spec.pipe_stdin {
            let (mut read, mut write) = (null_mut(), null_mut());
            if unsafe { CreatePipe(&mut read, &mut write, &security, 0) } == 0 {
                return Err(NativeProcessError::StartFailed);
            }
            let read = own(read, NativeProcessError::StartFailed)?;
            let write = own(write, NativeProcessError::StartFailed)?;
            if unsafe { SetHandleInformation(write.as_raw_handle(), 1, 0) } == 0 {
                return Err(NativeProcessError::StartFailed);
            }
            (Some(read), Some(write))
        } else {
            (None, None)
        };
        let output = stdout_write
            .as_ref()
            .map(AsRawHandle::as_raw_handle)
            .unwrap_or_else(|| nul.as_raw_handle());
        let mut handles = vec![nul.as_raw_handle()];
        if stdout_write.is_some() {
            handles.push(output);
        }
        let input = stdin_read
            .as_ref()
            .map(AsRawHandle::as_raw_handle)
            .unwrap_or_else(|| nul.as_raw_handle());
        if stdin_read.is_some() {
            handles.push(input);
        }
        let mut attributes = Attributes::new(handles)?;
        let mut startup: StartupInfoEx = unsafe { std::mem::zeroed() };
        startup.startup.size = std::mem::size_of::<StartupInfoEx>() as u32;
        startup.startup.flags = STARTF_USESTDHANDLES;
        startup.startup.stdin = input;
        startup.startup.stdout = output;
        startup.startup.stderr = nul.as_raw_handle();
        startup.attributes = attributes.pointer();
        let mut info: ProcessInformation = unsafe { std::mem::zeroed() };
        let environment = environment
            .map(|value| value.as_ptr().cast())
            .unwrap_or(null());
        let flags = CREATE_SUSPENDED
            | CREATE_NO_WINDOW
            | CREATE_UNICODE_ENVIRONMENT
            | EXTENDED_STARTUPINFO_PRESENT;
        let directory = cwd.as_ref().map(|value| value.as_ptr()).unwrap_or(null());
        let process_security = SecurityAttributes {
            length: std::mem::size_of::<SecurityAttributes>() as u32,
            descriptor: spec.primary_token
                .map(|token| token.process_descriptor().as_ptr().cast_mut().cast())
                .unwrap_or(null_mut()),
            inherit: 0,
        };
        let thread_security = SecurityAttributes {
            length: std::mem::size_of::<SecurityAttributes>() as u32,
            descriptor: spec.primary_token
                .map(|token| token.thread_descriptor().as_ptr().cast_mut().cast())
                .unwrap_or(null_mut()),
            inherit: 0,
        };
        let created = if let Some(token) = spec.primary_token {
            unsafe {
                CreateProcessAsUserW(
                    token.as_handle().as_raw_handle(),
                    application.as_ptr(),
                    command.as_mut_ptr(),
                    &process_security,
                    &thread_security,
                    1,
                    flags,
                    environment,
                    directory,
                    &startup.startup,
                    &mut info,
                )
            }
        } else {
            unsafe {
                CreateProcessW(
                    application.as_ptr(),
                    command.as_mut_ptr(),
                    null(),
                    null(),
                    1,
                    flags,
                    environment,
                    directory,
                    &startup.startup,
                    &mut info,
                )
            }
        };
        if created == 0 {
            return Err(NativeProcessError::StartFailed);
        }
        // Successful CreateProcessW guarantees both owned handles. Install
        // their RAII owners before any other fallible operation can occur.
        let process = unsafe { OwnedHandle::from_raw_handle(info.process) };
        let primary_thread = unsafe { OwnedHandle::from_raw_handle(info.thread) };
        // The local inheritable write handle drops here. Only the child keeps
        // its copy, so stdout reaches EOF when the private job is terminated.
        drop(stdout_write);
        drop(stdin_read);
        let child = Self {
            process,
            primary_thread: Some(primary_thread),
            job,
            stdin: stdin_write,
            stdout: stdout_read,
            admitted: false,
            resumed: false,
            stop_observers: Vec::new(),
            stop_observation_failed: false,
        };
        if let Some(token) = spec.primary_token {
            child.protect_role_child_token(token.child_token_descriptor())?;
        }
        Ok(child)
    }

    /// Token-object rights have a different mapping from process/thread/pipe
    /// rights. Apply the caller's token-only descriptor to the exact suspended
    /// child's primary token; never to the caller or a shared default DACL.
    fn protect_role_child_token(&self, descriptor: &[u8]) -> Result<(), NativeProcessError> {
        if self.admitted || self.resumed || descriptor.is_empty() { return Err(NativeProcessError::OwnershipUnavailable); }
        let mut token = null_mut();
        if unsafe { OpenProcessToken(self.process.as_raw_handle(), 0x000e_0008, &mut token) } == 0 { return Err(NativeProcessError::StartFailed); }
        let token = own(token, NativeProcessError::StartFailed)?;
        if unsafe { IsTokenRestricted(token.as_raw_handle()) } == 0
            || unsafe { SetKernelObjectSecurity(token.as_raw_handle(), 5, descriptor.as_ptr().cast()) } == 0
        { return Err(NativeProcessError::StartFailed); }
        Ok(())
    }

    pub fn admit_and_resume(&mut self) -> Result<(), NativeProcessError> {
        if self.resumed || self.admitted {
            return Err(NativeProcessError::OwnershipUnavailable);
        }
        if unsafe {
            AssignProcessToJobObject(self.job.as_raw_handle(), self.process.as_raw_handle())
        } == 0
        {
            return Err(NativeProcessError::OwnershipUnavailable);
        }
        self.admitted = true;
        let thread = self
            .primary_thread
            .as_ref()
            .ok_or(NativeProcessError::ResumeFailed)?;
        if unsafe { ResumeThread(thread.as_raw_handle()) } != 1 {
            return Err(NativeProcessError::ResumeFailed);
        }
        self.resumed = true;
        self.primary_thread.take();
        Ok(())
    }

    pub fn try_exit(&self) -> Result<Option<u32>, NativeProcessError> {
        match unsafe { WaitForSingleObject(self.process.as_raw_handle(), 0) } {
            WAIT_TIMEOUT => Ok(None),
            WAIT_OBJECT_0 => {
                let mut code = 0;
                if unsafe { GetExitCodeProcess(self.process.as_raw_handle(), &mut code) } == 0 {
                    Err(NativeProcessError::StatusFailed)
                } else {
                    Ok(Some(code))
                }
            }
            _ => Err(NativeProcessError::StatusFailed),
        }
    }

    fn job_accounting(&self) -> Result<BasicAccounting, NativeProcessError> {
        let mut accounting: BasicAccounting = unsafe { std::mem::zeroed() };
        if unsafe { QueryInformationJobObject(self.job.as_raw_handle(), 1,
            (&mut accounting as *mut BasicAccounting).cast(),
            std::mem::size_of::<BasicAccounting>() as u32, null_mut()) } == 0 {
            return Err(NativeProcessError::StatusFailed);
        }
        Ok(accounting)
    }

    #[cfg(test)]
    fn job_active_processes(&self) -> Result<u32, NativeProcessError> {
        Ok(self.job_accounting()?.active_processes)
    }

    fn observe_job_members(&mut self) -> Result<u32, NativeProcessError> {
        let initial_total = self.job_accounting()?.total_processes;
        let mut capacity = 16usize;
        let ids = loop {
            // JOBOBJECT_BASIC_PROCESS_ID_LIST starts with two DWORD counts,
            // followed by pointer-sized process IDs at offset eight.
            let bytes = 8 + capacity * std::mem::size_of::<usize>();
            let mut buffer = vec![0usize; bytes.div_ceil(std::mem::size_of::<usize>())];
            let success = unsafe { QueryInformationJobObject(self.job.as_raw_handle(), 3,
                buffer.as_mut_ptr().cast(), bytes as u32, null_mut()) };
            let assigned = unsafe { *buffer.as_ptr().cast::<u32>() } as usize;
            let listed = unsafe { *buffer.as_ptr().cast::<u32>().add(1) } as usize;
            if success == 0 {
                if unsafe { GetLastError() } != 234 || capacity == MAX_JOB_MEMBERS {
                    return Err(NativeProcessError::StatusFailed);
                }
                capacity = capacity.saturating_mul(2).max(assigned).min(MAX_JOB_MEMBERS);
                continue;
            }
            if assigned != listed || listed > capacity || listed > MAX_JOB_MEMBERS {
                return Err(NativeProcessError::StatusFailed);
            }
            let values = unsafe { std::slice::from_raw_parts(
                buffer.as_ptr().cast::<u8>().add(8).cast::<usize>(), listed) };
            break values.to_vec();
        };
        for id in ids {
            let id = u32::try_from(id).map_err(|_| NativeProcessError::StatusFailed)?;
            if id == 0 { return Err(NativeProcessError::StatusFailed); }
            if id == unsafe { GetProcessId(self.process.as_raw_handle()) }
                || self.stop_observers.iter().any(|known| unsafe { GetProcessId(known.as_raw_handle()) } == id)
            { continue; }
            if self.stop_observers.len() == MAX_JOB_MEMBERS {
                return Err(NativeProcessError::StatusFailed);
            }
            let observed = own(unsafe { OpenProcess(0x0010_1000, 0, id) },
                NativeProcessError::StatusFailed)?;
            let mut in_job = 0;
            if unsafe { GetProcessId(observed.as_raw_handle()) } != id
                || unsafe { IsProcessInJob(observed.as_raw_handle(), self.job.as_raw_handle(), &mut in_job) } == 0
                || in_job == 0
            { return Err(NativeProcessError::StatusFailed); }
            self.stop_observers.push(observed);
        }
        let captured_total = self.job_accounting()?.total_processes;
        if captured_total != initial_total { return Err(NativeProcessError::StatusFailed); }
        Ok(captured_total)
    }

    fn observed_members_exited(&self) -> Result<bool, NativeProcessError> {
        let mut exited = true;
        for process in &self.stop_observers {
            match unsafe { WaitForSingleObject(process.as_raw_handle(), 0) } {
                WAIT_OBJECT_0 => {}
                WAIT_TIMEOUT => exited = false,
                _ => return Err(NativeProcessError::StatusFailed),
            }
        }
        Ok(exited)
    }

    pub fn terminate(&mut self, deadline: Instant) -> Result<(), NativeProcessError> {
        let observed_total = if self.admitted {
            Some(if self.stop_observation_failed { Err(NativeProcessError::StatusFailed) }
                else { self.observe_job_members() })
        } else { None };
        if matches!(observed_total, Some(Err(_))) {
            // Losing a member observation cannot be repaired merely by seeing
            // an empty accounting list on a later stop attempt.
            self.stop_observation_failed = true;
        }
        if self.admitted {
            // Root exit does not imply job completion: descendants can remain
            // alive. Always cancel the admitted job, including after root exit.
            if unsafe { TerminateJobObject(self.job.as_raw_handle(), 1) } == 0 {
                return Err(NativeProcessError::StatusFailed);
            }
        } else if unsafe { TerminateProcess(self.process.as_raw_handle(), 1) } == 0
            && self.try_exit()?.is_none()
        {
            return Err(NativeProcessError::StatusFailed);
        }
        // Even a failed observation above must not skip the cancellation call.
        let observed_total = observed_total.transpose()?;
        loop {
            let root_exited = self.try_exit()?.is_some();
            // A failed query is not proof of an empty job. Both conditions are
            // required before a caller can publish a completed stop receipt.
            let job_empty = if let Some(total) = observed_total {
                let accounting = self.job_accounting()?;
                if accounting.total_processes != total {
                    // A member joined between observation and cancellation.
                    // Its rundown was not observed; never manufacture success.
                    self.stop_observation_failed = true;
                    return Err(NativeProcessError::StatusFailed);
                }
                accounting.active_processes == 0
            } else { true };
            if root_exited && job_empty && self.observed_members_exited()? {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(NativeProcessError::StopTimeout);
            }
            let remaining = deadline
                .saturating_duration_since(Instant::now())
                .as_millis()
                .min(50) as u32;
            if root_exited {
                std::thread::sleep(Duration::from_millis(remaining as u64));
            } else {
                match unsafe { WaitForSingleObject(self.process.as_raw_handle(), remaining) } {
                    WAIT_OBJECT_0 | WAIT_TIMEOUT => {}
                    _ => return Err(NativeProcessError::StatusFailed),
                }
            }
        }
    }

    pub fn read_available(
        &self,
        output: &mut Vec<u8>,
        limit: usize,
    ) -> Result<bool, NativeProcessError> {
        let Some(pipe) = &self.stdout else {
            return Ok(true);
        };
        loop {
            let mut available = 0;
            if unsafe {
                PeekNamedPipe(
                    pipe.as_raw_handle(),
                    null_mut(),
                    0,
                    null_mut(),
                    &mut available,
                    null_mut(),
                )
            } == 0
            {
                return if unsafe { GetLastError() } == ERROR_BROKEN_PIPE {
                    Ok(true)
                } else {
                    Err(NativeProcessError::StatusFailed)
                };
            }
            if available == 0 {
                return Ok(false);
            }
            let mut buffer = [0u8; 4096];
            let wanted = available.min(buffer.len() as u32);
            let mut read = 0;
            if unsafe {
                ReadFile(
                    pipe.as_raw_handle(),
                    buffer.as_mut_ptr().cast(),
                    wanted,
                    &mut read,
                    null_mut(),
                )
            } == 0
            {
                return Err(NativeProcessError::StatusFailed);
            }
            if output.len().saturating_add(read as usize) > limit {
                return Err(NativeProcessError::OutputLimitExceeded);
            }
            output.extend_from_slice(&buffer[..read as usize]);
            if read == 0 {
                return Ok(false);
            }
        }
    }

    /// Transfer stdio endpoint ownership to the designated caller. The job and
    /// process handles remain private, so pipe consumers cannot extend them.
    pub fn take_stdin(&mut self) -> Option<File> {
        self.stdin.take().map(File::from)
    }
    pub fn take_stdout(&mut self) -> Option<File> {
        self.stdout.take().map(File::from)
    }

    /// Duplicates are non-inheritable and share one underlying byte stream.
    /// Consumers must keep one stdout reader and serialize stdin writes.
    pub fn try_clone_stdin(&self) -> Result<Option<File>, NativeProcessError> {
        self.stdin.as_ref().map(duplicate_file).transpose()
    }
    pub fn try_clone_stdout(&self) -> Result<Option<File>, NativeProcessError> {
        self.stdout.as_ref().map(duplicate_file).transpose()
    }
}

impl Drop for NativeChild {
    fn drop(&mut self) {
        let _ = self.terminate(Instant::now() + DROP_TIMEOUT);
    }
}

fn duplicate_file(handle: &OwnedHandle) -> Result<File, NativeProcessError> {
    let mut duplicated = null_mut();
    let current = unsafe { GetCurrentProcess() };
    if unsafe {
        DuplicateHandle(
            current,
            handle.as_raw_handle(),
            current,
            &mut duplicated,
            0,
            0,
            2,
        )
    } == 0
    {
        return Err(NativeProcessError::StatusFailed);
    }
    own(duplicated, NativeProcessError::StatusFailed).map(File::from)
}

pub(crate) fn encode_environment(
    pairs: &[(OsString, OsString)],
) -> Result<Vec<u16>, NativeProcessError> {
    let mut entries = Vec::with_capacity(pairs.len().min(MAX_ENVIRONMENT_UNITS));
    let mut total = 2usize;
    for (key, value) in pairs {
        let key: Vec<_> = key.encode_wide().take(MAX_ENVIRONMENT_UNITS + 1).collect();
        let value: Vec<_> = value
            .encode_wide()
            .take(MAX_ENVIRONMENT_UNITS + 1)
            .collect();
        let hidden_drive = key.len() == 3
            && key[0] == b'=' as u16
            && key[2] == b':' as u16
            && ((b'A' as u16..=b'Z' as u16).contains(&key[1])
                || (b'a' as u16..=b'z' as u16).contains(&key[1]));
        if key.is_empty()
            || key.contains(&0)
            || value.contains(&0)
            || (!hidden_drive && key.contains(&(b'=' as u16)))
        {
            return Err(NativeProcessError::SpecificationInvalid);
        }
        total = total
            .checked_add(key.len())
            .and_then(|sum| sum.checked_add(value.len() + 2))
            .ok_or(NativeProcessError::SpecificationInvalid)?;
        if total > MAX_ENVIRONMENT_UNITS {
            return Err(NativeProcessError::SpecificationInvalid);
        }
        entries.push((key, value));
    }
    let invalid = std::cell::Cell::new(false);
    entries.sort_by(|left, right| match compare_names(&left.0, &right.0) {
        Ok(order) => order,
        Err(_) => {
            invalid.set(true);
            std::cmp::Ordering::Equal
        }
    });
    if invalid.get() {
        return Err(NativeProcessError::SpecificationInvalid);
    }
    for pair in entries.windows(2) {
        if compare_names(&pair[0].0, &pair[1].0)? == std::cmp::Ordering::Equal {
            return Err(NativeProcessError::SpecificationInvalid);
        }
    }
    let mut block = Vec::with_capacity(total);
    for (key, value) in entries {
        block.extend(key);
        block.push(b'=' as u16);
        block.extend(value);
        block.push(0);
    }
    if block.is_empty() {
        block.push(0);
    }
    block.push(0);
    Ok(block)
}

fn compare_names(first: &[u16], second: &[u16]) -> Result<std::cmp::Ordering, NativeProcessError> {
    match unsafe {
        CompareStringOrdinal(
            first.as_ptr(),
            first.len() as i32,
            second.as_ptr(),
            second.len() as i32,
            1,
        )
    } {
        1 => Ok(std::cmp::Ordering::Less),
        2 => Ok(std::cmp::Ordering::Equal),
        3 => Ok(std::cmp::Ordering::Greater),
        _ => Err(NativeProcessError::SpecificationInvalid),
    }
}

fn wide(value: &OsStr) -> Result<Vec<u16>, NativeProcessError> {
    let mut value: Vec<_> = value.encode_wide().collect();
    if value.contains(&0) {
        return Err(NativeProcessError::StartFailed);
    }
    value.push(0);
    Ok(value)
}

fn quoted(value: &OsStr, output: &mut Vec<u16>) -> Result<(), NativeProcessError> {
    let value: Vec<_> = value.encode_wide().collect();
    if value.contains(&0) {
        return Err(NativeProcessError::StartFailed);
    }
    output.push(b'"' as u16);
    let mut slashes = 0;
    for ch in value {
        if ch == b'\\' as u16 {
            slashes += 1;
            continue;
        }
        let count = if ch == b'"' as u16 {
            slashes * 2 + 1
        } else {
            slashes
        };
        output.extend(std::iter::repeat_n(b'\\' as u16, count));
        output.push(ch);
        slashes = 0;
    }
    output.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2));
    output.push(b'"' as u16);
    Ok(())
}

fn command_line(executable: &Path, args: &[String]) -> Result<Vec<u16>, NativeProcessError> {
    let mut command = Vec::new();
    quoted(executable.as_os_str(), &mut command)?;
    for arg in args {
        command.push(b' ' as u16);
        quoted(OsStr::new(arg), &mut command)?;
    }
    if command.len() >= 32_767 {
        return Err(NativeProcessError::StartFailed);
    }
    command.push(0);
    Ok(command)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::ffi::OsString;
    use std::sync::atomic::AtomicBool;

    const FIXTURE: &str = "services::owned_process::windows::tests::owned_child_fixture";
    const FIXTURE_OUTPUT_LIMIT: usize = 512 * 1024;

    fn create_suspended(
        executable: &Path,
        args: &[String],
        capture_stdout: bool,
        environment: Option<&[u16]>,
    ) -> Result<NativeChild, NativeProcessError> {
        NativeChild::create_suspended(&SpawnSpec {
            capture_stdout,
            environment,
            ..SpawnSpec::new(executable, args)
        })
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> Handle;
        fn DuplicateHandle(
            source_process: Handle,
            source: Handle,
            target_process: Handle,
            target: *mut Handle,
            access: u32,
            inherit: i32,
            options: u32,
        ) -> i32;
    }

    fn environment(mode: &str, marker: Option<&Path>) -> Vec<u16> {
        // The fixture needs no inherited provider/API credentials or user home.
        let mut values: BTreeMap<String, OsString> = BTreeMap::new();
        values.insert("AAG_OWNED_PROCESS_FIXTURE_MODE".into(), mode.into());
        if let Some(marker) = marker {
            values.insert(
                "AAG_OWNED_PROCESS_FIXTURE_MARKER".into(),
                marker.as_os_str().to_owned(),
            );
        }
        if let Some(root) = std::env::var_os("SystemRoot") {
            values.insert("SystemRoot".into(), root);
        }
        let mut block = Vec::new();
        for (key, value) in values {
            block.extend(OsStr::new(&key).encode_wide());
            block.push(b'=' as u16);
            block.extend(value.encode_wide());
            block.push(0);
        }
        block.push(0);
        block
    }

    fn fixture_args() -> Vec<String> {
        vec![
            "--exact".into(),
            FIXTURE.into(),
            "--ignored".into(),
            "--nocapture".into(),
        ]
    }

    fn observer(child: &NativeChild) -> OwnedHandle {
        let mut duplicate = null_mut();
        let current = unsafe { GetCurrentProcess() };
        assert_ne!(
            unsafe {
                DuplicateHandle(
                    current,
                    child.process.as_raw_handle(),
                    current,
                    &mut duplicate,
                    0x0010_1000,
                    0,
                    0,
                )
            },
            0
        );
        own(duplicate, NativeProcessError::StatusFailed).unwrap()
    }

    fn workspace_fixture_directory() -> crate::config::private_fs::PrivateTempDir {
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/owned_process_fixtures");
        std::fs::create_dir_all(&base).unwrap();
        crate::config::private_fs::private_temp_dir(&base).unwrap()
    }

    #[test]
    fn termination_does_not_claim_drain_without_job_accounting() {
        let directory = workspace_fixture_directory();
        let marker = directory.path().join("started.txt");
        let environment = environment("hold", Some(&marker));
        let mut child = create_suspended(&std::env::current_exe().unwrap(),
            &fixture_args(), false, Some(&environment)).unwrap();
        child.admit_and_resume().unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !marker.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(marker.exists());
        let mut terminate_only = null_mut();
        let current = unsafe { GetCurrentProcess() };
        assert_ne!(unsafe { DuplicateHandle(current, child.job.as_raw_handle(), current,
            &mut terminate_only, 0x8, 0, 0) }, 0);
        // Keep a private full-access guard so cleanup remains deterministic.
        // This duplicate can terminate the exact same job, but cannot QUERY it.
        let full_job = std::mem::replace(&mut child.job,
            own(terminate_only, NativeProcessError::StatusFailed).unwrap());
        assert_eq!(child.terminate(Instant::now() + Duration::from_secs(3)),
            Err(NativeProcessError::StatusFailed));
        drop(full_job);
    }

    #[test]
    fn stopping_after_root_exit_drains_the_owned_descendant() {
        let directory = workspace_fixture_directory();
        let marker = directory.path().join("descendant.pid");
        let ready = directory.path().join("descendant.ready");
        let environment = environment("spawn_then_exit", Some(&marker));
        let mut child = create_suspended(&std::env::current_exe().unwrap(),
            &fixture_args(), false, Some(&environment)).unwrap();
        child.admit_and_resume().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if child.try_exit().unwrap().is_some() && ready.exists() { break; }
            assert!(Instant::now() < deadline, "owned descendant fixture did not become ready");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(child.try_exit().unwrap(), Some(0));
        assert!(child.job_active_processes().unwrap() > 0,
            "root completion must precede descendant completion");
        let pid: u32 = std::fs::read_to_string(&marker).unwrap().parse().unwrap();
        let observed = own(unsafe { OpenProcess(0x0010_1000, 0, pid) },
            NativeProcessError::StatusFailed).unwrap();
        let mut in_job = 0;
        assert_ne!(unsafe { IsProcessInJob(observed.as_raw_handle(), child.job.as_raw_handle(), &mut in_job) }, 0);
        assert_ne!(in_job, 0, "observer must refer to our exact private job member");
        assert_eq!(unsafe { WaitForSingleObject(observed.as_raw_handle(), 0) }, WAIT_TIMEOUT);
        child.terminate(Instant::now() + Duration::from_secs(3)).unwrap();
        assert_eq!(child.job_active_processes().unwrap(), 0);
        assert_eq!(unsafe { WaitForSingleObject(observed.as_raw_handle(), 0) }, WAIT_OBJECT_0);
    }

    #[test]
    fn owned_fixture_cannot_run_before_job_admission_and_dies_on_owner_drop() {
        let directory = crate::config::private_fs::private_temp_dir(&std::env::temp_dir()).unwrap();
        let marker = directory.path().join("started.txt");
        let environment = environment("hold", Some(&marker));
        let mut child = create_suspended(
            &std::env::current_exe().unwrap(),
            &fixture_args(),
            false,
            Some(&environment),
        )
        .unwrap();
        assert!(!child.admitted && !child.resumed);
        assert!(child.try_exit().unwrap().is_none());
        std::thread::sleep(Duration::from_millis(100));
        assert!(!marker.exists(), "the suspended entrypoint must not run");
        child.admit_and_resume().unwrap();
        assert!(child.admitted && child.resumed);
        let deadline = Instant::now() + Duration::from_secs(3);
        while !marker.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), "ready");
        let observed = observer(&child);
        drop(child);
        assert_eq!(
            unsafe { WaitForSingleObject(observed.as_raw_handle(), 3000) },
            WAIT_OBJECT_0
        );
    }

    #[test]
    fn dropping_an_unadmitted_fixture_never_runs_its_entrypoint() {
        let directory = crate::config::private_fs::private_temp_dir(&std::env::temp_dir()).unwrap();
        let marker = directory.path().join("never_started.txt");
        let environment = environment("hold", Some(&marker));
        let child = create_suspended(
            &std::env::current_exe().unwrap(),
            &fixture_args(),
            false,
            Some(&environment),
        )
        .unwrap();
        let observed = observer(&child);
        drop(child);
        assert_eq!(
            unsafe { WaitForSingleObject(observed.as_raw_handle(), 3000) },
            WAIT_OBJECT_0
        );
        assert!(!marker.exists());
    }

    #[test]
    fn closing_the_private_job_handle_alone_kills_the_admitted_fixture() {
        let directory = crate::config::private_fs::private_temp_dir(&std::env::temp_dir()).unwrap();
        let marker = directory.path().join("job_close.txt");
        let environment = environment("hold", Some(&marker));
        let mut child = create_suspended(
            &std::env::current_exe().unwrap(),
            &fixture_args(),
            false,
            Some(&environment),
        )
        .unwrap();
        child.admit_and_resume().unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !marker.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(marker.exists());
        let observed = observer(&child);
        let empty_job = own(
            unsafe { CreateJobObjectW(null(), null()) },
            NativeProcessError::OwnershipUnavailable,
        )
        .unwrap();
        let admitted_job = std::mem::replace(&mut child.job, empty_job);
        drop(admitted_job);
        // The explicit NativeChild drop/TerminateJobObject path has not run.
        // This also proves the child did not inherit a second job handle.
        assert_eq!(
            unsafe { WaitForSingleObject(observed.as_raw_handle(), 3000) },
            WAIT_OBJECT_0
        );
        drop(child);
    }

    #[test]
    fn command_capture_is_bounded_and_cancellation_precedes_process_creation() {
        let executable = std::env::current_exe().unwrap();
        let environment = environment("flood", None);
        let args = fixture_args();
        let spec = SpawnSpec {
            environment: Some(&environment),
            ..SpawnSpec::new(&executable, &args)
        };
        assert_eq!(
            super::super::run_capture(
                &spec,
                &AtomicBool::new(false),
                Instant::now() + Duration::from_secs(5),
                128
            )
            .err(),
            Some(NativeProcessError::OutputLimitExceeded)
        );
        let args = Vec::new();
        let spec = SpawnSpec::new(Path::new("not-an-executable"), &args);
        assert_eq!(
            super::super::run_capture(
                &spec,
                &AtomicBool::new(true),
                Instant::now() + Duration::from_secs(1),
                128
            )
            .err(),
            Some(NativeProcessError::OperationCancelled)
        );
    }

    #[test]
    fn windows_argv_quoting_keeps_quotes_spaces_and_trailing_slashes_literal() {
        let command = command_line(
            Path::new(r"C:\Program Files\Tailscale\tailscale.exe"),
            &[
                String::new(),
                "two words".into(),
                "a\"b".into(),
                "tail\\".into(),
            ],
        )
        .unwrap();
        let text = String::from_utf16(&command[..command.len() - 1]).unwrap();
        assert_eq!(text, "\"C:\\Program Files\\Tailscale\\tailscale.exe\" \"\" \"two words\" \"a\\\"b\" \"tail\\\\\"");
    }

    #[test]
    fn piped_stdio_supports_explicit_environment_and_working_directory() {
        use std::io::{BufRead, Write};
        let directory = crate::config::private_fs::private_temp_dir(&std::env::temp_dir()).unwrap();
        let mut pairs = vec![
            (
                OsString::from("AAG_OWNED_PROCESS_FIXTURE_MODE"),
                OsString::from("rpc"),
            ),
            (
                OsString::from("AAG_OWNED_PROCESS_FIXTURE_VALUE"),
                OsString::from("fixture-env-value"),
            ),
        ];
        if let Some(root) = std::env::var_os("SystemRoot") {
            pairs.push((OsString::from("SystemRoot"), root));
        }
        let environment = encode_environment(&pairs).unwrap();
        let executable = std::env::current_exe().unwrap();
        let args = fixture_args();
        let spec = SpawnSpec {
            pipe_stdin: true,
            capture_stdout: true,
            environment: Some(&environment),
            cwd: Some(directory.path()),
            ..SpawnSpec::new(&executable, &args)
        };
        let mut child = NativeChild::create_suspended(&spec).unwrap();
        child.admit_and_resume().unwrap();
        let mut writer = child.try_clone_stdin().unwrap().unwrap();
        let original_writer = child.take_stdin().unwrap();
        let duplicate_reader = child.try_clone_stdout().unwrap().unwrap();
        drop(duplicate_reader);
        let output = child.take_stdout().unwrap();
        writer.write_all(b"fixture-input\n").unwrap();
        drop(writer);
        drop(original_writer);
        let (sender, receiver) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut input = std::io::BufReader::new(output);
            let mut total = 0;
            let result = loop {
                let mut line = String::new();
                match input.read_line(&mut line) {
                    Ok(0) | Err(_) => break None,
                    Ok(bytes) => total += bytes,
                }
                if total > 64 * 1024 {
                    break None;
                }
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) {
                    if value
                        .get("fixture_rpc")
                        .and_then(serde_json::Value::as_bool)
                        == Some(true)
                    {
                        break Some(value);
                    }
                }
            };
            let _ = sender.send(result);
        });
        let reply = receiver.recv_timeout(Duration::from_secs(3));
        let _ = child.terminate(Instant::now() + Duration::from_secs(2));
        drop(child);
        reader.join().unwrap();
        let reply = reply.unwrap().unwrap();
        assert_eq!(reply["input"], "fixture-input\n");
        assert_eq!(reply["value"], "fixture-env-value");
        assert_eq!(
            std::fs::canonicalize(reply["cwd"].as_str().unwrap()).unwrap(),
            std::fs::canonicalize(directory.path()).unwrap()
        );
    }

    #[test]
    fn environment_encoding_rejects_ambiguous_names_nuls_and_invalid_cwd() {
        assert_eq!(encode_environment(&[]).unwrap(), [0, 0]);
        assert_eq!(
            encode_environment(&[("Path".into(), "one".into()), ("PATH".into(), "two".into())])
                .err(),
            Some(NativeProcessError::SpecificationInvalid)
        );
        assert_eq!(
            encode_environment(&[("BAD=NAME".into(), "one".into())]).err(),
            Some(NativeProcessError::SpecificationInvalid)
        );
        assert_eq!(
            encode_environment(&[("KEY".into(), OsString::from("nul\0value"))]).err(),
            Some(NativeProcessError::SpecificationInvalid)
        );
        let executable = std::env::current_exe().unwrap();
        let args = fixture_args();
        let spec = SpawnSpec {
            cwd: Some(Path::new("relative")),
            ..SpawnSpec::new(&executable, &args)
        };
        assert_eq!(
            NativeChild::create_suspended(&spec).err(),
            Some(NativeProcessError::SpecificationInvalid)
        );
    }

    #[test]
    fn capture_preserves_exit_codes_for_the_consumer_to_classify() {
        let executable = std::env::current_exe().unwrap();
        let args = fixture_args();
        let environment = environment("exit7", None);
        let spec = SpawnSpec {
            environment: Some(&environment),
            ..SpawnSpec::new(&executable, &args)
        };
        let output = super::super::run_capture(
            &spec,
            &AtomicBool::new(false),
            Instant::now() + Duration::from_secs(5),
            FIXTURE_OUTPUT_LIMIT,
        )
        .unwrap_or_else(|error| panic!("fixture capture failed: {error}"));
        assert_eq!(output.exit_code, 7);
    }

    #[test]
    #[ignore = "owned child-process fixture; launched only by the native ownership tests"]
    fn owned_child_fixture() {
        let Ok(mode) = std::env::var("AAG_OWNED_PROCESS_FIXTURE_MODE") else {
            return;
        };
        match mode.as_str() {
            "spawn_then_exit" => {
                use std::process::{Command, Stdio};
                let marker = std::path::PathBuf::from(
                    std::env::var_os("AAG_OWNED_PROCESS_FIXTURE_MARKER").unwrap());
                let ready = marker.parent().unwrap().join("descendant.ready");
                let mut command = Command::new(std::env::current_exe().unwrap());
                command.args(fixture_args()).env_clear()
                    .env("AAG_OWNED_PROCESS_FIXTURE_MODE", "hold")
                    .env("AAG_OWNED_PROCESS_FIXTURE_MARKER", ready)
                    .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
                if let Some(root) = std::env::var_os("SystemRoot") { command.env("SystemRoot", root); }
                let descendant = command.spawn().unwrap();
                std::fs::write(marker, descendant.id().to_string()).unwrap();
                // Windows job membership is inherited. The std Child is not
                // an execution owner; the outer NativeChild retains that job.
                drop(descendant);
            }
            "hold" => {
                let marker = std::env::var_os("AAG_OWNED_PROCESS_FIXTURE_MARKER").unwrap();
                std::fs::write(Path::new(&marker), b"ready").unwrap();
                loop {
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
            "flood" => {
                use std::io::Write;
                let _ = std::io::stdout()
                    .lock()
                    .write_all(&vec![b'x'; FIXTURE_OUTPUT_LIMIT + 4096]);
            }
            "rpc" => {
                use std::io::Read;
                let mut input = String::new();
                std::io::stdin().read_to_string(&mut input).unwrap();
                println!(
                    "\n{}",
                    serde_json::json!({"fixture_rpc":true,"input":input,
                    "value":std::env::var("AAG_OWNED_PROCESS_FIXTURE_VALUE").unwrap(),
                    "cwd":std::env::current_dir().unwrap().to_string_lossy()})
                );
            }
            "exit7" => std::process::exit(7),
            _ => panic!("unknown owned fixture mode"),
        }
    }
}
