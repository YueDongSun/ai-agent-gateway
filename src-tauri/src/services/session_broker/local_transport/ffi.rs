//! Narrow Windows ABI boundary. Errors retain operation/code, never payloads.
use super::{KernelOperation, Sid, TransportError};
use std::{
    ffi::c_void,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle},
    ptr::null_mut,
};

pub(super) type Handle = *mut c_void;
pub(super) const INVALID_HANDLE: Handle = -1isize as Handle;
const MAX_TOKEN_BYTES: u32 = 64 * 1024;
const MAX_SID_CHARS: usize = 256;

#[repr(C)]
pub(super) struct SecurityAttributes {
    pub length: u32,
    pub descriptor: *mut c_void,
    pub inherit: i32,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct SidAndAttributes {
    pub sid: *mut c_void,
    pub attributes: u32,
}
#[cfg(test)]
#[repr(C)]
pub(super) struct TokenGroups {
    pub count: u32,
    pub groups: [SidAndAttributes; 1],
}
#[cfg(test)]
#[repr(C)]
pub(super) struct ObjectBasicInformation {
    pub attributes: u32,
    pub granted_access: u32,
    pub handle_count: u32,
    pub pointer_count: u32,
    pub reserved: [u32; 10],
}

#[link(name = "kernel32")]
extern "system" {
    pub fn GetLastError() -> u32;
    pub fn LocalFree(value: *mut c_void) -> *mut c_void;
    pub fn GetCurrentProcess() -> Handle;
    pub fn CreateFileW(
        name: *const u16,
        access: u32,
        share: u32,
        security: *mut SecurityAttributes,
        creation: u32,
        flags: u32,
        template: Handle,
    ) -> Handle;
    pub fn GetNamedPipeServerProcessId(pipe: Handle, process_id: *mut u32) -> i32;
    pub fn GetNamedPipeClientProcessId(pipe: Handle, process_id: *mut u32) -> i32;
    #[cfg(test)]
    pub fn CreateNamedPipeW(
        name: *const u16,
        open_mode: u32,
        pipe_mode: u32,
        maximum: u32,
        output_bytes: u32,
        input_bytes: u32,
        timeout: u32,
        security: *mut SecurityAttributes,
    ) -> Handle;
}
#[link(name = "advapi32")]
extern "system" {
    pub fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
        value: *const u16,
        revision: u32,
        descriptor: *mut *mut c_void,
        size: *mut u32,
    ) -> i32;
    pub fn GetSecurityInfo(
        handle: Handle,
        object_type: u32,
        information: u32,
        owner: *mut *mut c_void,
        group: *mut *mut c_void,
        dacl: *mut *mut c_void,
        sacl: *mut *mut c_void,
        descriptor: *mut *mut c_void,
    ) -> u32;
    pub fn IsValidSid(sid: *mut c_void) -> i32;
    pub fn ConvertSidToStringSidW(sid: *mut c_void, output: *mut *mut u16) -> i32;
    pub fn OpenProcessToken(process: Handle, access: u32, token: *mut Handle) -> i32;
    pub fn GetTokenInformation(
        token: Handle,
        class: u32,
        data: *mut c_void,
        size: u32,
        returned: *mut u32,
    ) -> i32;
    #[cfg(test)]
    pub fn CreateRestrictedToken(
        token: Handle,
        flags: u32,
        disable_count: u32,
        disable: *const SidAndAttributes,
        delete_count: u32,
        delete: *const c_void,
        restrict_count: u32,
        restrict: *const SidAndAttributes,
        output: *mut Handle,
    ) -> i32;
    #[cfg(test)]
    pub fn ImpersonateLoggedOnUser(token: Handle) -> i32;
    #[cfg(test)]
    pub fn RevertToSelf() -> i32;
}
#[cfg(test)]
#[link(name = "ntdll")]
extern "system" {
    pub fn NtQueryObject(
        handle: Handle,
        class: u32,
        data: *mut c_void,
        size: u32,
        returned: *mut u32,
    ) -> i32;
}

pub(super) fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}
pub(super) fn last_error(operation: KernelOperation) -> TransportError {
    TransportError::Kernel {
        operation,
        code: unsafe { GetLastError() },
    }
}
pub(super) struct LocalAllocation(pub *mut c_void);
impl Drop for LocalAllocation {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                LocalFree(self.0);
            }
        }
    }
}

pub(super) fn descriptor(value: &str) -> Result<LocalAllocation, TransportError> {
    let text = wide(value);
    let mut raw = null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(text.as_ptr(), 1, &mut raw, null_mut())
    } == 0
    {
        return Err(last_error(KernelOperation::Descriptor));
    }
    Ok(LocalAllocation(raw))
}
pub(super) unsafe fn sid_text(raw: *mut c_void) -> Result<Sid, TransportError> {
    if raw.is_null() || IsValidSid(raw) == 0 {
        return Err(TransportError::InvalidEndpoint);
    }
    let mut text = null_mut();
    if ConvertSidToStringSidW(raw, &mut text) == 0 {
        return Err(last_error(KernelOperation::Identity));
    }
    let _allocation = LocalAllocation(text.cast());
    let mut length = 0;
    while length < MAX_SID_CHARS && *text.add(length) != 0 {
        length += 1;
    }
    if length == MAX_SID_CHARS {
        return Err(TransportError::InvalidEndpoint);
    }
    let value = String::from_utf16(std::slice::from_raw_parts(text, length))
        .map_err(|_| TransportError::InvalidEndpoint)?;
    Sid::parse(&value).map_err(|_| TransportError::InvalidEndpoint)
}
pub(super) fn object_owner(handle: RawHandle) -> Result<Sid, TransportError> {
    let (mut owner, mut descriptor) = (null_mut(), null_mut());
    let code = unsafe {
        GetSecurityInfo(
            handle,
            1,
            1,
            &mut owner,
            null_mut(),
            null_mut(),
            null_mut(),
            &mut descriptor,
        )
    };
    let _allocation = LocalAllocation(descriptor);
    if code != 0 {
        return Err(TransportError::Kernel {
            operation: KernelOperation::Attest,
            code,
        });
    }
    unsafe { sid_text(owner) }
}
pub(super) fn process_id(handle: RawHandle, server: bool) -> Result<u32, TransportError> {
    let mut value = 0;
    let ok = unsafe {
        if server {
            GetNamedPipeServerProcessId(handle, &mut value)
        } else {
            GetNamedPipeClientProcessId(handle, &mut value)
        }
    };
    if ok == 0 || value == 0 {
        return Err(last_error(KernelOperation::Attest));
    }
    Ok(value)
}
pub(super) fn open(name: &str, access: u32, flags: u32) -> Result<OwnedHandle, u32> {
    let text = wide(name);
    let handle = unsafe { CreateFileW(text.as_ptr(), access, 0, null_mut(), 3, flags, null_mut()) };
    if handle == INVALID_HANDLE {
        Err(unsafe { GetLastError() })
    } else {
        Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
    }
}
pub(super) struct TokenBuffer {
    pub words: Vec<usize>,
    pub bytes: usize,
}
pub(super) fn token_information(
    token: &OwnedHandle,
    class: u32,
) -> Result<TokenBuffer, TransportError> {
    let mut needed = 0;
    unsafe {
        GetTokenInformation(token.as_raw_handle(), class, null_mut(), 0, &mut needed);
    }
    if needed == 0 || needed > MAX_TOKEN_BYTES {
        return Err(last_error(KernelOperation::Identity));
    }
    let capacity = needed;
    let mut words = vec![0usize; (needed as usize).div_ceil(std::mem::size_of::<usize>())];
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            class,
            words.as_mut_ptr().cast(),
            capacity,
            &mut needed,
        )
    } == 0
        || needed > capacity
    {
        return Err(last_error(KernelOperation::Identity));
    }
    Ok(TokenBuffer {
        words,
        bytes: needed as usize,
    })
}
pub(super) fn current_token(access: u32) -> Result<OwnedHandle, TransportError> {
    let mut token = null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), access, &mut token) } == 0 {
        return Err(last_error(KernelOperation::Identity));
    }
    Ok(unsafe { OwnedHandle::from_raw_handle(token) })
}
pub(super) fn current_user_sid() -> Result<Sid, TransportError> {
    let token = current_token(0x8)?;
    let buffer = token_information(&token, 1)?;
    if buffer.bytes < std::mem::size_of::<SidAndAttributes>() {
        return Err(TransportError::InvalidEndpoint);
    }
    unsafe { sid_text((*buffer.words.as_ptr().cast::<SidAndAttributes>()).sid) }
}
