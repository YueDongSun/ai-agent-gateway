//! Exact owner/group/DACL preservation before writing credential-bearing bytes.
//! No privilege elevation, ownership takeover, or permissive fallback is used.
use std::{ffi::c_void, os::windows::ffi::OsStrExt, path::Path, ptr};

use super::failure;
use crate::error::AppError;

const OWNER_GROUP_DACL: u32 = 0x1 | 0x2 | 0x4;
const SE_FILE_OBJECT: u32 = 1;
const SE_DACL_PROTECTED: u16 = 0x1000;
const PROTECTED_DACL: u32 = 0x8000_0000;
const UNPROTECTED_DACL: u32 = 0x2000_0000;

#[link(name = "advapi32")]
extern "system" {
    fn GetNamedSecurityInfoW(
        name: *const u16,
        object_type: u32,
        information: u32,
        owner: *mut *mut c_void,
        group: *mut *mut c_void,
        dacl: *mut *mut c_void,
        sacl: *mut *mut c_void,
        descriptor: *mut *mut c_void,
    ) -> u32;
    fn SetNamedSecurityInfoW(
        name: *mut u16,
        object_type: u32,
        information: u32,
        owner: *mut c_void,
        group: *mut c_void,
        dacl: *mut c_void,
        sacl: *mut c_void,
    ) -> u32;
    fn GetSecurityDescriptorControl(
        descriptor: *const c_void,
        control: *mut u16,
        revision: *mut u32,
    ) -> i32;
    fn IsValidSecurityDescriptor(descriptor: *const c_void) -> i32;
    fn IsValidAcl(acl: *const c_void) -> i32;
    fn IsValidSid(sid: *const c_void) -> i32;
    fn EqualSid(one: *const c_void, two: *const c_void) -> i32;
}
#[link(name = "kernel32")]
extern "system" {
    fn LocalFree(memory: *mut c_void) -> *mut c_void;
}

#[allow(non_camel_case_types)]
pub(super) struct Windows_permissions {
    descriptor: *mut c_void,
    owner: *mut c_void,
    group: *mut c_void,
    dacl: *mut c_void,
    control: u16,
}

impl Drop for Windows_permissions {
    fn drop(&mut self) {
        if !self.descriptor.is_null() {
            // SAFETY: GetNamedSecurityInfoW allocates this single owning buffer
            // with LocalAlloc; owner/group/DACL are borrowed pointers into it.
            unsafe {
                LocalFree(self.descriptor);
            }
        }
    }
}

impl Windows_permissions {
    pub(super) fn read(path: &Path) -> Result<Self, AppError> {
        let name: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut permissions = Self {
            descriptor: ptr::null_mut(),
            owner: ptr::null_mut(),
            group: ptr::null_mut(),
            dacl: ptr::null_mut(),
            control: 0,
        };
        // SAFETY: the UTF-16 name is terminated, output pointers are writable,
        // and the returned allocation is immediately owned by the drop guard.
        let result = unsafe {
            GetNamedSecurityInfoW(
                name.as_ptr(),
                SE_FILE_OBJECT,
                OWNER_GROUP_DACL,
                &mut permissions.owner,
                &mut permissions.group,
                &mut permissions.dacl,
                ptr::null_mut(),
                &mut permissions.descriptor,
            )
        };
        if result != 0
            || permissions.descriptor.is_null()
            || permissions.owner.is_null()
            || permissions.group.is_null()
            || permissions.dacl.is_null()
        {
            return Err(failure("client_acl_unavailable_or_unsupported"));
        }
        let mut revision = 0;
        // SAFETY: all pointers are provided by the successful Windows call and
        // remain valid until this object's owning security descriptor is freed.
        let valid = unsafe {
            IsValidSecurityDescriptor(permissions.descriptor) != 0
                && IsValidSid(permissions.owner) != 0
                && IsValidSid(permissions.group) != 0
                && IsValidAcl(permissions.dacl) != 0
                && GetSecurityDescriptorControl(
                    permissions.descriptor,
                    &mut permissions.control,
                    &mut revision,
                ) != 0
        };
        if !valid {
            return Err(failure("client_acl_unavailable_or_unsupported"));
        }
        Ok(permissions)
    }

    pub(super) fn apply(&self, path: &Path) -> Result<(), AppError> {
        let mut name: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let target = Self::read(path)?;
        // Setting an unchanged owner/group can still require WRITE_OWNER on
        // Windows. Verify the creator inherited the exact identities first,
        // then update only the DACL that the caller is authorized to change.
        let same_owner_group = unsafe {
            EqualSid(self.owner, target.owner) != 0 && EqualSid(self.group, target.group) != 0
        };
        if !same_owner_group {
            return Err(failure("client_acl_preservation_failed"));
        }
        let protection = if self.control & SE_DACL_PROTECTED != 0 {
            PROTECTED_DACL
        } else {
            UNPROTECTED_DACL
        };
        // SAFETY: descriptor-owned pointers are valid for this bounded call;
        // only the newly created AAG-owned file named by the caller is changed.
        let result = unsafe {
            SetNamedSecurityInfoW(
                name.as_mut_ptr(),
                SE_FILE_OBJECT,
                0x4 | protection,
                ptr::null_mut(),
                ptr::null_mut(),
                self.dacl,
                ptr::null_mut(),
            )
        };
        if result != 0 {
            return Err(failure("client_acl_preservation_failed"));
        }
        let actual = Self::read(path)?;
        if !self.matches(&actual) {
            return Err(failure("client_acl_verification_failed"));
        }
        Ok(())
    }

    pub(super) fn matches(&self, other: &Self) -> bool {
        // SAFETY: both objects validated their descriptor/ACL/SID pointers. The
        // ACL header's second u16 is AclSize, and Windows bounds it on validation.
        unsafe {
            let self_size = *((self.dacl as *const u16).add(1)) as usize;
            let other_size = *((other.dacl as *const u16).add(1)) as usize;
            EqualSid(self.owner, other.owner) != 0
                && EqualSid(self.group, other.group) != 0
                && (self.control & SE_DACL_PROTECTED) == (other.control & SE_DACL_PROTECTED)
                && std::slice::from_raw_parts(self.dacl as *const u8, self_size)
                    == std::slice::from_raw_parts(other.dacl as *const u8, other_size)
        }
    }
}

/// Secure a newly created, still exclusively open transaction file. The owner
/// was assigned by Windows at creation; no foreign owner or privilege is used.
pub(super) fn restrict_to_owner(path: &Path) -> Result<(), AppError> {
    #[link(name = "advapi32")]
    extern "system" {
        fn GetLengthSid(sid: *const c_void) -> u32;
        fn InitializeAcl(acl: *mut c_void, length: u32, revision: u32) -> i32;
        fn AddAccessAllowedAce(
            acl: *mut c_void,
            revision: u32,
            mask: u32,
            sid: *const c_void,
        ) -> i32;
    }
    let permissions = Windows_permissions::read(path)?;
    // SAFETY: owner SID was validated; aligned u32 storage holds a Windows ACL.
    let acl_size = unsafe { 8 + 8 + GetLengthSid(permissions.owner) };
    let mut acl = vec![0u32; ((acl_size + 3) / 4) as usize];
    let acl_pointer = acl.as_mut_ptr() as *mut c_void;
    let mut name: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: the buffer is large enough for one ACCESS_ALLOWED_ACE and SID;
    // only the new transaction file's DACL is changed, preserving owner/group.
    let ok = unsafe {
        InitializeAcl(acl_pointer, acl_size, 2) != 0
            && AddAccessAllowedAce(acl_pointer, 2, 0x001f_01ff, permissions.owner) != 0
            && SetNamedSecurityInfoW(
                name.as_mut_ptr(),
                SE_FILE_OBJECT,
                0x4 | PROTECTED_DACL,
                ptr::null_mut(),
                ptr::null_mut(),
                acl_pointer,
                ptr::null_mut(),
            ) == 0
    };
    if !ok {
        return Err(failure("client_acl_initialization_failed"));
    }
    let actual = Windows_permissions::read(path)?;
    // SAFETY: validated descriptors own both SIDs and the ACL; the expected ACL
    // uses the validated owner SID and an exactly sized aligned local buffer.
    let matches = unsafe {
        EqualSid(permissions.owner, actual.owner) != 0
            && EqualSid(permissions.group, actual.group) != 0
            && actual.control & SE_DACL_PROTECTED != 0
            && *((actual.dacl as *const u16).add(1)) as u32 == acl_size
            && std::slice::from_raw_parts(actual.dacl as *const u8, acl_size as usize)
                == std::slice::from_raw_parts(acl_pointer as *const u8, acl_size as usize)
    };
    if !matches {
        return Err(failure("client_acl_verification_failed"));
    }
    Ok(())
}
