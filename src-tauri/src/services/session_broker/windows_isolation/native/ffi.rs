//! Small Win32 boundary. No path, SID, descriptor, or token is logged.
use std::ffi::c_void;
pub(in super::super) type Handle = *mut c_void;
pub(in super::super) type Psid = *mut c_void;

#[repr(C)]
#[derive(Clone, Copy)]
pub(in super::super) struct SidAndAttributes {
    pub sid: Psid,
    pub attributes: u32,
}
#[repr(C)]
pub(in super::super) struct TokenGroups {
    pub count: u32,
    pub groups: [SidAndAttributes; 1],
}
#[repr(C)]
#[derive(Clone, Copy)]
pub(in super::super) struct Luid {
    pub low: u32,
    pub high: i32,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub(in super::super) struct LuidAndAttributes {
    pub luid: Luid,
    pub attributes: u32,
}
#[repr(C)]
pub(in super::super) struct TokenPrivileges {
    pub count: u32,
    pub privileges: [LuidAndAttributes; 1],
}
#[repr(C)]
pub(in super::super) struct TokenDefaultDacl {
    pub acl: *mut c_void,
}
#[repr(C)]
pub(in super::super) struct FileInformation {
    pub attributes: u32,
    pub creation: [u32; 2],
    pub access: [u32; 2],
    pub write: [u32; 2],
    pub volume: u32,
    pub size_high: u32,
    pub size_low: u32,
    pub links: u32,
    pub index_high: u32,
    pub index_low: u32,
}
#[repr(C)]
pub(in super::super) struct AclSize {
    pub count: u32,
    pub in_use: u32,
    pub free: u32,
}
#[repr(C)]
pub(in super::super) struct AceHeader {
    pub kind: u8,
    pub flags: u8,
    pub size: u16,
}
#[repr(C)]
pub(in super::super) struct AllowedAce {
    pub header: AceHeader,
    pub mask: u32,
    pub sid_start: u32,
}

#[link(name = "kernel32")]
extern "system" {
    pub fn GetCurrentProcess() -> Handle;
    pub fn GetCurrentProcessId() -> u32;
    pub fn GetCurrentThreadId() -> u32;
    pub fn OpenThread(access: u32, inherit: i32, id: u32) -> Handle;
    pub fn OpenProcess(access: u32, inherit: i32, id: u32) -> Handle;
    pub fn GetLastError() -> u32;
    pub fn SetLastError(value: u32);
    pub fn LocalFree(value: *mut c_void) -> *mut c_void;
    pub fn GetModuleHandleW(name: *const u16) -> Handle;
    pub fn GetProcAddress(module: Handle, name: *const u8) -> *mut c_void;
    pub fn CreateFileW(
        name: *const u16,
        access: u32,
        share: u32,
        attributes: *const c_void,
        disposition: u32,
        flags: u32,
        template: Handle,
    ) -> Handle;
    pub fn CreateDirectoryW(name: *const u16, attributes: *const c_void) -> i32;
    pub fn GetFileInformationByHandle(file: Handle, info: *mut FileInformation) -> i32;
    pub fn GetNamedPipeInfo(
        pipe: Handle,
        flags: *mut u32,
        out_size: *mut u32,
        in_size: *mut u32,
        max_instances: *mut u32,
    ) -> i32;
    pub fn GetStdHandle(which: u32) -> Handle;
}
#[link(name = "advapi32")]
extern "system" {
    pub fn ConvertStringSidToSidW(value: *const u16, sid: *mut Psid) -> i32;
    pub fn ConvertSidToStringSidW(sid: Psid, value: *mut *mut u16) -> i32;
    pub fn IsValidSid(sid: Psid) -> i32;
    pub fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
        value: *const u16,
        revision: u32,
        descriptor: *mut *mut c_void,
        size: *mut u32,
    ) -> i32;
    pub fn GetSecurityDescriptorDacl(
        descriptor: *mut c_void,
        present: *mut i32,
        acl: *mut *mut c_void,
        defaulted: *mut i32,
    ) -> i32;
    pub fn GetSecurityDescriptorOwner(
        descriptor: *mut c_void,
        owner: *mut Psid,
        defaulted: *mut i32,
    ) -> i32;
    pub fn GetAclInformation(acl: *mut c_void, info: *mut c_void, size: u32, class: u32) -> i32;
    pub fn GetAce(acl: *mut c_void, index: u32, ace: *mut *mut c_void) -> i32;
    pub fn GetKernelObjectSecurity(
        object: Handle,
        info: u32,
        descriptor: *mut c_void,
        length: u32,
        needed: *mut u32,
    ) -> i32;
    pub fn SetKernelObjectSecurity(object: Handle, info: u32, descriptor: *const c_void) -> i32;
    pub fn SetSecurityInfo(
        object: Handle,
        kind: u32,
        info: u32,
        owner: Psid,
        group: Psid,
        dacl: *mut c_void,
        sacl: *mut c_void,
    ) -> u32;
    pub fn OpenProcessToken(process: Handle, access: u32, token: *mut Handle) -> i32;
    pub fn GetTokenInformation(
        token: Handle,
        class: u32,
        information: *mut c_void,
        length: u32,
        needed: *mut u32,
    ) -> i32;
    pub fn SetTokenInformation(
        token: Handle,
        class: u32,
        information: *const c_void,
        length: u32,
    ) -> i32;
    pub fn CreateRestrictedToken(
        existing: Handle,
        flags: u32,
        disable_count: u32,
        disable: *const SidAndAttributes,
        delete_count: u32,
        delete: *const LuidAndAttributes,
        restrict_count: u32,
        restrict: *const SidAndAttributes,
        new_token: *mut Handle,
    ) -> i32;
    pub fn IsTokenRestricted(token: Handle) -> i32;
    pub fn ImpersonateLoggedOnUser(token: Handle) -> i32;
    pub fn RevertToSelf() -> i32;
    pub fn ImpersonateNamedPipeClient(pipe: Handle) -> i32;
    pub fn AdjustTokenPrivileges(
        token: Handle,
        disable_all: i32,
        new_state: *const c_void,
        length: u32,
        previous: *mut c_void,
        returned: *mut u32,
    ) -> i32;
    pub fn LookupPrivilegeValueW(system: *const u16, name: *const u16, luid: *mut Luid) -> i32;
}
