//! Bounded canonical extended-attribute inventory from an admitted handle.

use super::*;
use std::{ffi::c_void, os::windows::io::AsRawHandle, ptr};

const MAX_EA_BYTES: usize = 1024 * 1024;
const MAX_EA_ENTRIES: usize = 4096;

#[repr(C)]
struct IoStatus {
    status: usize,
    information: usize,
}
#[link(name = "ntdll")]
extern "system" {
    fn NtQueryEaFile(
        file: *mut c_void,
        status: *mut IoStatus,
        buffer: *mut c_void,
        bytes: u32,
        single: u8,
        list: *const c_void,
        list_bytes: u32,
        index: *const u32,
        restart: u8,
    ) -> i32;
    #[cfg(test)]
    fn NtSetEaFile(
        file: *mut c_void,
        status: *mut IoStatus,
        buffer: *const c_void,
        bytes: u32,
    ) -> i32;
}

fn u32_at(bytes: &[u8], offset: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(
        bytes
            .get(offset..offset + 4)
            .ok_or(MigrationError::PlatformFailure)?
            .try_into()
            .map_err(|_| MigrationError::PlatformFailure)?,
    ))
}

/// Ignore linked-record offsets/padding, preserve every semantic EA byte, and
/// order by exact name so two equivalent kernel enumerations compare equally.
pub(super) fn canonicalize(bytes: &[u8]) -> Result<Vec<u8>> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    let mut entries = Vec::new();
    let mut offset: usize = 0;
    loop {
        if entries.len() >= MAX_EA_ENTRIES {
            return Err(MigrationError::PlatformFailure);
        }
        let record = bytes.get(offset..).ok_or(MigrationError::PlatformFailure)?;
        let next = u32_at(record, 0)? as usize;
        let flags = *record.get(4).ok_or(MigrationError::PlatformFailure)?;
        let name_length = usize::from(*record.get(5).ok_or(MigrationError::PlatformFailure)?);
        let value_length = u16::from_le_bytes(
            record
                .get(6..8)
                .ok_or(MigrationError::PlatformFailure)?
                .try_into()
                .map_err(|_| MigrationError::PlatformFailure)?,
        ) as usize;
        let value_start = 8usize
            .checked_add(name_length + 1)
            .ok_or(MigrationError::PlatformFailure)?;
        let end = value_start
            .checked_add(value_length)
            .ok_or(MigrationError::PlatformFailure)?;
        let name = record
            .get(8..8 + name_length)
            .ok_or(MigrationError::PlatformFailure)?;
        if name.is_empty()
            || name.contains(&0)
            || record.get(value_start - 1) != Some(&0)
            || flags & !0x80 != 0
        {
            return Err(MigrationError::PlatformFailure);
        }
        let value = record
            .get(value_start..end)
            .ok_or(MigrationError::PlatformFailure)?;
        entries.push((name.to_vec(), flags, value.to_vec()));
        if next == 0 {
            break;
        }
        if next % 4 != 0 || next < end {
            return Err(MigrationError::PlatformFailure);
        }
        offset = offset
            .checked_add(next)
            .ok_or(MigrationError::PlatformFailure)?;
    }
    entries.sort_by(|left, right| {
        left.0
            .to_ascii_uppercase()
            .cmp(&right.0.to_ascii_uppercase())
            .then_with(|| left.0.cmp(&right.0))
    });
    if entries
        .windows(2)
        .any(|pair| pair[0].0.eq_ignore_ascii_case(&pair[1].0))
    {
        return Err(MigrationError::PlatformFailure);
    }
    let mut output = b"aag_ea_v1".to_vec();
    output.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for (name, flags, value) in entries {
        output.push(flags);
        output.push(name.len() as u8);
        output.extend_from_slice(&(value.len() as u16).to_le_bytes());
        output.extend_from_slice(&name);
        output.extend_from_slice(&value);
    }
    Ok(output)
}

pub(super) fn capture(platform: &WindowsStoragePlatform<'_>, file: &File) -> Result<Vec<u8>> {
    let mut length = 4096;
    loop {
        platform.check_authority()?;
        let mut words = vec![0u64; length / 8];
        let mut status = IoStatus {
            status: 0,
            information: 0,
        };
        let result = unsafe {
            NtQueryEaFile(
                file.as_raw_handle(),
                &mut status,
                words.as_mut_ptr().cast(),
                length as u32,
                0,
                ptr::null(),
                0,
                ptr::null(),
                1,
            )
        } as u32;
        match result {
            0 => {
                if status.information > length {
                    return Err(MigrationError::PlatformFailure);
                }
                let bytes = unsafe {
                    std::slice::from_raw_parts(words.as_ptr().cast::<u8>(), status.information)
                };
                return canonicalize(bytes);
            }
            0xC000_0052 | 0x8000_0012 => return Ok(Vec::new()),
            0xC000_0023 | 0x8000_0005 if length < MAX_EA_BYTES => length *= 2,
            _ => return Err(MigrationError::PlatformFailure),
        }
    }
}

#[cfg(test)]
pub(super) fn set_fixture_ea(file: &File, name: &[u8], value: &[u8]) -> Result<()> {
    if name.is_empty() || name.len() > 255 || value.len() > u16::MAX as usize {
        return Err(MigrationError::InvalidPaths);
    }
    let mut bytes = vec![0u8; 8];
    bytes[5] = name.len() as u8;
    bytes[6..8].copy_from_slice(&(value.len() as u16).to_le_bytes());
    bytes.extend_from_slice(name);
    bytes.push(0);
    bytes.extend_from_slice(value);
    let mut status = IoStatus {
        status: 0,
        information: 0,
    };
    if unsafe {
        NtSetEaFile(
            file.as_raw_handle(),
            &mut status,
            bytes.as_ptr().cast(),
            bytes.len() as u32,
        )
    } != 0
    {
        return Err(MigrationError::PlatformFailure);
    }
    Ok(())
}
