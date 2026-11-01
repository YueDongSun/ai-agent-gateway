//! Win32 SCM and read-only process-token calls. No installer surface is exposed.
use std::ffi::c_void;
pub(super) type Handle = *mut c_void;

#[repr(C)]
pub(super) struct ServiceTableEntry {
    pub name: *mut u16,
    pub service_main: Option<unsafe extern "system" fn(u32, *mut *mut u16)>,
}
#[repr(C)]
pub(super) struct ServiceStatus {
    pub service_type: u32,
    pub current_state: u32,
    pub controls_accepted: u32,
    pub win32_exit_code: u32,
    pub service_specific_exit_code: u32,
    pub checkpoint: u32,
    pub wait_hint: u32,
}
#[repr(C)]
pub(super) struct SidAndAttributes {
    pub sid: *mut c_void,
    pub attributes: u32,
}
#[repr(C)]
pub(super) struct Luid {
    pub low: u32,
    pub high: i32,
}
#[repr(C)]
pub(super) struct LuidAndAttributes {
    pub luid: Luid,
    pub attributes: u32,
}
#[repr(C)]
pub(super) struct TokenPrivileges {
    pub count: u32,
    pub privileges: [LuidAndAttributes; 1],
}
#[repr(C)]
pub(super) struct TokenGroups {
    pub count: u32,
    pub groups: [SidAndAttributes; 1],
}

#[link(name = "advapi32")]
extern "system" {
    pub fn StartServiceCtrlDispatcherW(table: *const ServiceTableEntry) -> i32;
    pub fn RegisterServiceCtrlHandlerExW(
        name: *const u16,
        handler: Option<unsafe extern "system" fn(u32, u32, *mut c_void, *mut c_void) -> u32>,
        context: *mut c_void,
    ) -> Handle;
    pub fn SetServiceStatus(handle: Handle, status: *const ServiceStatus) -> i32;
    pub fn OpenProcessToken(process: Handle, access: u32, token: *mut Handle) -> i32;
    pub fn GetTokenInformation(
        token: Handle,
        class: u32,
        buffer: *mut c_void,
        length: u32,
        needed: *mut u32,
    ) -> i32;
    pub fn LookupAccountNameW(
        system: *const u16,
        account: *const u16,
        sid: *mut c_void,
        sid_bytes: *mut u32,
        domain: *mut u16,
        domain_chars: *mut u32,
        use_kind: *mut u32,
    ) -> i32;
    pub fn IsValidSid(sid: *mut c_void) -> i32;
    pub fn LookupPrivilegeNameW(
        system: *const u16,
        luid: *const Luid,
        name: *mut u16,
        chars: *mut u32,
    ) -> i32;
    pub fn ConvertSidToStringSidW(sid: *mut c_void, output: *mut *mut u16) -> i32;
}
#[link(name = "kernel32")]
extern "system" {
    pub fn GetCurrentProcess() -> Handle;
    pub fn LocalFree(pointer: *mut c_void) -> *mut c_void;
}
