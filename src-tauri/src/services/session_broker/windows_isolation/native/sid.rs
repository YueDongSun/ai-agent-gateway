use super::super::{IsolationError, Sid};
use super::ffi::*;
use std::{
    ffi::c_void,
    ptr::{null, null_mut},
    slice,
};

const MAX_DESCRIPTOR_BYTES: u32 = 1024 * 1024;
const MAX_SID_CHARACTERS: usize = 256;

pub(in super::super) struct LocalAllocation(pub *mut c_void);
impl Drop for LocalAllocation {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                LocalFree(self.0);
            }
        }
    }
}

pub(in super::super) fn wide(value: &str) -> Result<Vec<u16>, IsolationError> {
    if value.contains('\0') {
        return Err(IsolationError::InvalidInput);
    }
    Ok(value.encode_utf16().chain(Some(0)).collect())
}

pub(in super::super) struct BinarySid {
    allocation: LocalAllocation,
}
impl BinarySid {
    pub fn new(sid: &Sid) -> Result<Self, IsolationError> {
        let mut pointer = null_mut();
        if unsafe { ConvertStringSidToSidW(wide(sid.value())?.as_ptr(), &mut pointer) } == 0 {
            return Err(IsolationError::InvalidInput);
        }
        let allocation = LocalAllocation(pointer);
        if unsafe { IsValidSid(allocation.0) } == 0 {
            return Err(IsolationError::InvalidInput);
        }
        Ok(Self { allocation })
    }
    pub fn pointer(&self) -> Psid {
        self.allocation.0
    }
}

/// The caller must keep the native SID alive through this conversion.
pub(in super::super) unsafe fn sid_from_pointer(pointer: Psid) -> Result<Sid, IsolationError> {
    if pointer.is_null() || IsValidSid(pointer) == 0 {
        return Err(IsolationError::InvalidInput);
    }
    let mut text = null_mut();
    if ConvertSidToStringSidW(pointer, &mut text) == 0 {
        return Err(IsolationError::InvalidInput);
    }
    let allocation = LocalAllocation(text.cast());
    let mut length = 0;
    while length < MAX_SID_CHARACTERS && *text.add(length) != 0 {
        length += 1;
    }
    if length == MAX_SID_CHARACTERS {
        return Err(IsolationError::InvalidInput);
    }
    let value = String::from_utf16(slice::from_raw_parts(text, length))
        .map_err(|_| IsolationError::InvalidInput)?;
    drop(allocation);
    Ok(Sid { value })
}

pub(in super::super) fn validate_sid(value: &str) -> Result<Sid, IsolationError> {
    if !value.starts_with("S-1-")
        || value.len() > MAX_SID_CHARACTERS
        || !value
            .bytes()
            .all(|byte| byte == b'S' || byte == b'-' || byte.is_ascii_digit())
    {
        return Err(IsolationError::InvalidInput);
    }
    let candidate = Sid {
        value: value.to_owned(),
    };
    let binary = BinarySid::new(&candidate)?;
    let canonical = unsafe { sid_from_pointer(binary.pointer()) }?;
    if canonical.value != value {
        return Err(IsolationError::InvalidInput);
    }
    Ok(canonical)
}

pub(in super::super) fn derive_sid(name: &str) -> Result<Sid, IsolationError> {
    type Derive = unsafe extern "system" fn(
        *const u16,
        *mut *mut Psid,
        *mut u32,
        *mut *mut Psid,
        *mut u32,
    ) -> i32;
    let module = unsafe { GetModuleHandleW(wide("KernelBase.dll")?.as_ptr()) };
    if module.is_null() {
        return Err(IsolationError::Unsupported);
    }
    let pointer =
        unsafe { GetProcAddress(module, c"DeriveCapabilitySidsFromName".as_ptr().cast()) };
    if pointer.is_null() {
        return Err(IsolationError::Unsupported);
    }
    let derive: Derive = unsafe { std::mem::transmute(pointer) };
    let (mut groups, mut caps) = (null_mut(), null_mut());
    let (mut group_count, mut cap_count) = (0, 0);
    let success = unsafe {
        derive(
            wide(name)?.as_ptr(),
            &mut groups,
            &mut group_count,
            &mut caps,
            &mut cap_count,
        )
    };
    struct SidArray {
        pointer: *mut Psid,
        count: u32,
    }
    impl Drop for SidArray {
        fn drop(&mut self) {
            if !self.pointer.is_null() {
                // Counts and arrays are returned by the trusted Windows API.
                for index in 0..self.count {
                    unsafe {
                        LocalFree(*self.pointer.add(index as usize));
                    }
                }
                unsafe {
                    LocalFree(self.pointer.cast());
                }
            }
        }
    }
    let _groups = SidArray {
        pointer: groups,
        count: group_count,
    };
    let _caps = SidArray {
        pointer: caps,
        count: cap_count,
    };
    // Restricted desktop/service tokens consume the NT-authority group SID.
    // The AppAuthority capability SID is a different SID family: this host's
    // CreateRestrictedToken rejects it with ERROR_INVALID_PARAMETER.
    if success == 0 || groups.is_null() || group_count != 1 {
        return Err(IsolationError::TokenCreationFailed);
    }
    unsafe { sid_from_pointer(*groups) }
}

pub(in super::super) struct Descriptor {
    allocation: LocalAllocation,
    size: u32,
}
impl Descriptor {
    pub fn new(sddl: &str) -> Result<Self, IsolationError> {
        let (mut pointer, mut size) = (null_mut(), 0);
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide(sddl)?.as_ptr(),
                1,
                &mut pointer,
                &mut size,
            )
        } == 0
        {
            return Err(IsolationError::InvalidInput);
        }
        let allocation = LocalAllocation(pointer);
        if size == 0 || size > MAX_DESCRIPTOR_BYTES {
            return Err(IsolationError::InvalidInput);
        }
        Ok(Self { allocation, size })
    }
    pub fn pointer(&self) -> *mut c_void {
        self.allocation.0
    }
    pub fn bytes(&self) -> Vec<u8> {
        unsafe { slice::from_raw_parts(self.pointer().cast::<u8>(), self.size as usize).to_vec() }
    }
    pub fn acl(&self) -> Result<*mut c_void, IsolationError> {
        unsafe { descriptor_acl(self.pointer()) }
    }
}

pub(in super::super) fn validate_descriptor(sddl: &str) -> Result<(), IsolationError> {
    Descriptor::new(sddl).map(|_| ())
}

pub(in super::super) unsafe fn descriptor_acl(
    descriptor: *mut c_void,
) -> Result<*mut c_void, IsolationError> {
    let (mut present, mut defaulted, mut acl) = (0, 0, null_mut());
    if GetSecurityDescriptorDacl(descriptor, &mut present, &mut acl, &mut defaulted) == 0
        || present == 0
        || acl.is_null()
    {
        return Err(IsolationError::StorageBoundaryInvalid);
    }
    Ok(acl)
}

pub(in super::super) fn security_descriptor(object: Handle) -> Result<Vec<usize>, IsolationError> {
    let mut needed = 0;
    unsafe {
        GetKernelObjectSecurity(object, 0x5, null_mut(), 0, &mut needed);
    }
    if needed == 0 || needed > MAX_DESCRIPTOR_BYTES {
        return Err(IsolationError::StorageAccessFailed);
    }
    let mut buffer = vec![0usize; (needed as usize).div_ceil(std::mem::size_of::<usize>())];
    if unsafe {
        GetKernelObjectSecurity(object, 0x5, buffer.as_mut_ptr().cast(), needed, &mut needed)
    } == 0
    {
        return Err(IsolationError::StorageAccessFailed);
    }
    Ok(buffer)
}

pub(in super::super) fn descriptor_owner(descriptor: *mut c_void) -> Result<Sid, IsolationError> {
    let (mut owner, mut defaulted) = (null_mut(), 0);
    if unsafe { GetSecurityDescriptorOwner(descriptor, &mut owner, &mut defaulted) } == 0 {
        return Err(IsolationError::StorageAccessFailed);
    }
    unsafe { sid_from_pointer(owner) }
}

pub(in super::super) fn set_file_acl(
    object: Handle,
    descriptor: &Descriptor,
) -> Result<(), IsolationError> {
    // No owner adoption: only a complete protected DACL is installed on an
    // already verified owner-A file/directory handle.
    if unsafe {
        SetSecurityInfo(
            object,
            1,
            0x8000_0004,
            null_mut(),
            null_mut(),
            descriptor.acl()?,
            null_mut(),
        )
    } != 0
    {
        return Err(IsolationError::StorageAccessFailed);
    }
    Ok(())
}
