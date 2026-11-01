use super::*;
use std::{
    ffi::c_void,
    mem::size_of,
    ptr,
    sync::{Mutex, MutexGuard, TryLockError},
};

// One owner changes this process token at a time. A failed restoration poisons
// this authority boundary until the process is restarted by its owner.
static PRIVILEGE_OWNER: Mutex<bool> = Mutex::new(true);
const TOKEN_QUERY: u32 = 0x8;
const TOKEN_ADJUST_PRIVILEGES: u32 = 0x20;
const PRIVILEGE_NAMES: [&str; 3] = [
    "SeBackupPrivilege",
    "SeRestorePrivilege",
    "SeSecurityPrivilege",
];

#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Luid {
    low: u32,
    high: i32,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct LuidAttributes {
    luid: Luid,
    attributes: u32,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct TokenPrivileges {
    count: u32,
    values: [LuidAttributes; 3],
}

#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentProcess() -> *mut c_void;
    fn GetCurrentThread() -> *mut c_void;
    fn CloseHandle(handle: *mut c_void) -> i32;
    fn GetLastError() -> u32;
    fn SetLastError(error: u32);
}
#[link(name = "advapi32")]
extern "system" {
    fn OpenProcessToken(process: *mut c_void, access: u32, token: *mut *mut c_void) -> i32;
    fn OpenThreadToken(
        thread: *mut c_void,
        access: u32,
        open_as_self: i32,
        token: *mut *mut c_void,
    ) -> i32;
    fn LookupPrivilegeValueW(system: *const u16, name: *const u16, luid: *mut Luid) -> i32;
    fn AdjustTokenPrivileges(
        token: *mut c_void,
        disable_all: i32,
        state: *const TokenPrivileges,
        previous_bytes: u32,
        previous: *mut TokenPrivileges,
        returned_bytes: *mut u32,
    ) -> i32;
    #[cfg(test)]
    fn GetTokenInformation(
        token: *mut c_void,
        class: u32,
        information: *mut c_void,
        bytes: u32,
        returned: *mut u32,
    ) -> i32;
}

struct Token(*mut c_void);
impl Drop for Token {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

fn process_token(access: u32) -> Result<Token> {
    let mut handle = ptr::null_mut();
    // SAFETY: the output is one owned token handle; pseudo-process handles are
    // borrowed and are never closed.
    if unsafe { OpenProcessToken(GetCurrentProcess(), access, &mut handle) } == 0
        || handle.is_null()
    {
        return Err(MigrationError::PlatformFailure);
    }
    Ok(Token(handle))
}

fn luids() -> Result<[Luid; 3]> {
    let mut values = [Luid::default(); 3];
    for (name, value) in PRIVILEGE_NAMES.iter().zip(values.iter_mut()) {
        let name: Vec<_> = name.encode_utf16().chain(Some(0)).collect();
        if unsafe { LookupPrivilegeValueW(ptr::null(), name.as_ptr(), value) } == 0 {
            return Err(MigrationError::PlatformFailure);
        }
    }
    Ok(values)
}

pub(super) struct ProcessPrivileges {
    owner: MutexGuard<'static, bool>,
    token: Token,
    previous: TokenPrivileges,
    active: bool,
}
impl ProcessPrivileges {
    pub(super) fn acquire() -> Result<Self> {
        let owner = match PRIVILEGE_OWNER.try_lock() {
            Ok(owner) if *owner => owner,
            Ok(_) => return Err(MigrationError::PlatformFailure),
            Err(TryLockError::WouldBlock) => return Err(MigrationError::WritersActive),
            Err(TryLockError::Poisoned(_)) => return Err(MigrationError::PlatformFailure),
        };
        // Filesystem access and CreateProcess inheritance must use the same
        // process principal. This adapter never adopts an impersonation token.
        let mut thread = ptr::null_mut();
        if unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut thread) } != 0 {
            drop(Token(thread));
            return Err(MigrationError::PlatformFailure);
        }
        if unsafe { GetLastError() } != 1008 {
            return Err(MigrationError::PlatformFailure);
        }
        let token = process_token(TOKEN_QUERY | TOKEN_ADJUST_PRIVILEGES)?;
        let mut requested = TokenPrivileges {
            count: 3,
            ..Default::default()
        };
        for (entry, luid) in requested.values.iter_mut().zip(luids()?) {
            *entry = LuidAttributes {
                luid,
                attributes: 2,
            };
        }
        let mut guard = Self {
            owner,
            token,
            previous: TokenPrivileges::default(),
            active: false,
        };
        let mut returned = 0;
        unsafe {
            SetLastError(0);
        }
        let adjusted = unsafe {
            AdjustTokenPrivileges(
                guard.token.0,
                0,
                &requested,
                size_of::<TokenPrivileges>() as u32,
                &mut guard.previous,
                &mut returned,
            )
        };
        let status = unsafe { GetLastError() };
        if guard.previous.count > 3 {
            *guard.owner = false;
            return Err(MigrationError::PlatformFailure);
        }
        guard.active = adjusted != 0;
        // ERROR_NOT_ALL_ASSIGNED may have enabled a subset. Drop restores every
        // returned prior value before this failure leaves the constructor.
        if adjusted == 0 || status != 0 {
            return Err(MigrationError::PlatformFailure);
        }
        Ok(guard)
    }

    pub(super) fn active(&self) -> bool {
        self.active
    }

    pub(super) fn restore(&mut self) -> Result<()> {
        if !self.active {
            return Ok(());
        }
        unsafe {
            SetLastError(0);
        }
        let restored = unsafe {
            AdjustTokenPrivileges(
                self.token.0,
                0,
                &self.previous,
                0,
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };
        if restored == 0 || unsafe { GetLastError() } != 0 {
            *self.owner = false;
            return Err(MigrationError::PlatformFailure);
        }
        self.active = false;
        Ok(())
    }
}

impl Drop for ProcessPrivileges {
    fn drop(&mut self) {
        if self.restore().is_err() {
            log::error!("migration_process_privilege_restore_failed");
        }
    }
}

#[cfg(test)]
pub(super) fn snapshot() -> Result<Vec<Option<u32>>> {
    let token = process_token(TOKEN_QUERY)?;
    let mut returned = 0;
    unsafe {
        GetTokenInformation(token.0, 3, ptr::null_mut(), 0, &mut returned);
    }
    if !(4..=65536).contains(&returned) {
        return Err(MigrationError::PlatformFailure);
    }
    let mut words = vec![0u64; (returned as usize + 7) / 8];
    if unsafe {
        GetTokenInformation(
            token.0,
            3,
            words.as_mut_ptr().cast(),
            (words.len() * 8) as u32,
            &mut returned,
        )
    } == 0
    {
        return Err(MigrationError::PlatformFailure);
    }
    if returned < 4 || returned as usize > words.len() * 8 {
        return Err(MigrationError::PlatformFailure);
    }
    let bytes =
        unsafe { std::slice::from_raw_parts(words.as_ptr().cast::<u8>(), returned as usize) };
    let count = u32::from_le_bytes(
        bytes[0..4]
            .try_into()
            .map_err(|_| MigrationError::PlatformFailure)?,
    ) as usize;
    if count > (bytes.len() - 4) / 12 {
        return Err(MigrationError::PlatformFailure);
    }
    let wanted = luids()?;
    let mut result = vec![None; 3];
    for entry in bytes[4..4 + count * 12].chunks_exact(12) {
        let luid = Luid {
            low: u32::from_le_bytes(
                entry[0..4]
                    .try_into()
                    .map_err(|_| MigrationError::PlatformFailure)?,
            ),
            high: i32::from_le_bytes(
                entry[4..8]
                    .try_into()
                    .map_err(|_| MigrationError::PlatformFailure)?,
            ),
        };
        if let Some(index) = wanted.iter().position(|value| *value == luid) {
            result[index] = Some(u32::from_le_bytes(
                entry[8..12]
                    .try_into()
                    .map_err(|_| MigrationError::PlatformFailure)?,
            ));
        }
    }
    Ok(result)
}
