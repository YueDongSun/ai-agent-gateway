//! Resolve a fixed virtual account and read the process's normal TokenUser.
use super::{ffi::*, ObservedIdentity, ServiceHostError, ServiceRole};
use std::{
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    ptr::{null, null_mut},
};

const MAX_IDENTITY_BYTES: u32 = 64 * 1024;
const MAX_SID_CHARACTERS: usize = 256;

pub(super) fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

pub(super) unsafe fn sid_text(sid: *mut std::ffi::c_void) -> Result<String, ServiceHostError> {
    if sid.is_null() || IsValidSid(sid) == 0 {
        return Err(ServiceHostError::IdentityUnavailable);
    }
    let mut output = null_mut();
    if ConvertSidToStringSidW(sid, &mut output) == 0 || output.is_null() {
        return Err(ServiceHostError::IdentityUnavailable);
    }
    let mut length = 0;
    while length < MAX_SID_CHARACTERS && *output.add(length) != 0 {
        length += 1;
    }
    let value = if length < MAX_SID_CHARACTERS {
        String::from_utf16(std::slice::from_raw_parts(output, length))
            .map_err(|_| ServiceHostError::IdentityUnavailable)
    } else {
        Err(ServiceHostError::IdentityUnavailable)
    };
    LocalFree(output.cast());
    value
}

pub(super) fn expected_user_sid(role: ServiceRole) -> Result<String, ServiceHostError> {
    let account = wide(&role.account_name());
    let (mut sid_bytes, mut domain_chars, mut use_kind) = (0, 0, 0);
    unsafe {
        LookupAccountNameW(
            null(),
            account.as_ptr(),
            null_mut(),
            &mut sid_bytes,
            null_mut(),
            &mut domain_chars,
            &mut use_kind,
        );
    }
    if sid_bytes == 0 || sid_bytes > MAX_IDENTITY_BYTES || domain_chars > 256 {
        return Err(ServiceHostError::IdentityUnavailable);
    }
    let mut sid = vec![0usize; (sid_bytes as usize).div_ceil(std::mem::size_of::<usize>())];
    let mut domain = vec![0u16; domain_chars.max(1) as usize];
    if unsafe {
        LookupAccountNameW(
            null(),
            account.as_ptr(),
            sid.as_mut_ptr().cast(),
            &mut sid_bytes,
            domain.as_mut_ptr(),
            &mut domain_chars,
            &mut use_kind,
        )
    } == 0
    {
        return Err(ServiceHostError::IdentityUnavailable);
    }
    unsafe { sid_text(sid.as_mut_ptr().cast()) }
}

pub(super) fn current_identity() -> Result<ObservedIdentity, ServiceHostError> {
    let mut raw_token = null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), 0x8, &mut raw_token) } == 0
        || raw_token.is_null()
    {
        return Err(ServiceHostError::IdentityUnavailable);
    }
    let token = unsafe { OwnedHandle::from_raw_handle(raw_token) };
    let user = token_information(token.as_raw_handle(), 1)?;
    if user.bytes < std::mem::size_of::<SidAndAttributes>() {
        return Err(ServiceHostError::IdentityUnavailable);
    }
    let user_sid = unsafe { sid_text((*user.words.as_ptr().cast::<SidAndAttributes>()).sid) }?;
    let session = token_information(token.as_raw_handle(), 12)?;
    if session.bytes != std::mem::size_of::<u32>() {
        return Err(ServiceHostError::IdentityUnavailable);
    }
    let session_id = unsafe { *session.words.as_ptr().cast::<u32>() };
    let privileges = token_privileges(token.as_raw_handle())?;
    let administrator_member = token_administrator_member(token.as_raw_handle())?;
    Ok(ObservedIdentity {
        user_sid,
        session_id,
        privileges,
        administrator_member,
    })
}

pub(super) struct TokenBuffer {
    pub words: Vec<usize>,
    pub bytes: usize,
}

pub(super) fn token_information(
    token: Handle,
    class: u32,
) -> Result<TokenBuffer, ServiceHostError> {
    let mut needed = 0;
    unsafe {
        GetTokenInformation(token, class, null_mut(), 0, &mut needed);
    }
    if needed == 0 || needed > MAX_IDENTITY_BYTES {
        return Err(ServiceHostError::IdentityUnavailable);
    }
    let capacity = needed;
    let mut words = vec![0usize; (needed as usize).div_ceil(std::mem::size_of::<usize>())];
    if unsafe {
        GetTokenInformation(
            token,
            class,
            words.as_mut_ptr().cast(),
            capacity,
            &mut needed,
        )
    } == 0
        || needed > capacity
    {
        return Err(ServiceHostError::IdentityUnavailable);
    }
    Ok(TokenBuffer {
        words,
        bytes: needed as usize,
    })
}

fn array_count(
    buffer: &TokenBuffer,
    offset: usize,
    item_size: usize,
) -> Result<usize, ServiceHostError> {
    if buffer.bytes < std::mem::size_of::<u32>() {
        return Err(ServiceHostError::IdentityUnavailable);
    }
    let count = unsafe { *buffer.words.as_ptr().cast::<u32>() } as usize;
    if count
        .checked_mul(item_size)
        .and_then(|bytes| bytes.checked_add(offset))
        .is_none_or(|bytes| bytes > buffer.bytes)
    {
        return Err(ServiceHostError::IdentityUnavailable);
    }
    Ok(count)
}

fn token_privileges(token: Handle) -> Result<Vec<String>, ServiceHostError> {
    let buffer = token_information(token, 3)?;
    let offset = std::mem::offset_of!(TokenPrivileges, privileges);
    let count = array_count(&buffer, offset, std::mem::size_of::<LuidAndAttributes>())?;
    let privileges = unsafe {
        std::slice::from_raw_parts(
            buffer
                .words
                .as_ptr()
                .cast::<u8>()
                .add(offset)
                .cast::<LuidAndAttributes>(),
            count,
        )
    };
    let mut names = Vec::with_capacity(count);
    for privilege in privileges {
        let mut chars = 0;
        unsafe {
            LookupPrivilegeNameW(null(), &privilege.luid, null_mut(), &mut chars);
        }
        if chars == 0 || chars > 256 {
            return Err(ServiceHostError::IdentityUnavailable);
        }
        let mut name = vec![0u16; chars as usize + 1];
        let mut capacity = name.len() as u32;
        if unsafe {
            LookupPrivilegeNameW(null(), &privilege.luid, name.as_mut_ptr(), &mut capacity)
        } == 0
            || capacity as usize >= name.len()
        {
            return Err(ServiceHostError::IdentityUnavailable);
        }
        names.push(
            String::from_utf16(&name[..capacity as usize])
                .map_err(|_| ServiceHostError::IdentityUnavailable)?,
        );
    }
    Ok(names)
}

fn token_administrator_member(token: Handle) -> Result<bool, ServiceHostError> {
    let buffer = token_information(token, 2)?;
    let offset = std::mem::offset_of!(TokenGroups, groups);
    let count = array_count(&buffer, offset, std::mem::size_of::<SidAndAttributes>())?;
    let groups = unsafe {
        std::slice::from_raw_parts(
            buffer
                .words
                .as_ptr()
                .cast::<u8>()
                .add(offset)
                .cast::<SidAndAttributes>(),
            count,
        )
    };
    // Group membership never establishes service identity. Here it can only
    // reject an elevated/filtered administrator token, including deny-only SID.
    for group in groups {
        if unsafe { sid_text(group.sid) }? == "S-1-5-32-544" {
            return Ok(true);
        }
    }
    Ok(false)
}
