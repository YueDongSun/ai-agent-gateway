//! Timestamp restoration on already admitted user filesystem object handles.

use super::*;
use std::{ffi::c_void, os::windows::io::AsRawHandle, ptr};

#[repr(C)]
struct FileTime {
    low: u32,
    high: u32,
}
#[link(name = "kernel32")]
extern "system" {
    fn SetFileTime(
        file: *mut c_void,
        created: *const FileTime,
        accessed: *const FileTime,
        written: *const FileTime,
    ) -> i32;
}
fn time(value: u64) -> FileTime {
    FileTime {
        low: value as u32,
        high: (value >> 32) as u32,
    }
}
fn suppress(file: &File) -> Result<()> {
    let disabled = FileTime {
        low: u32::MAX,
        high: u32::MAX,
    };
    if unsafe { SetFileTime(file.as_raw_handle(), ptr::null(), &disabled, ptr::null()) } == 0 {
        return Err(MigrationError::PlatformFailure);
    }
    Ok(())
}
pub(super) fn restore_all(file: &File, metadata: &EntryMetadata) -> Result<()> {
    if unsafe {
        SetFileTime(
            file.as_raw_handle(),
            &time(metadata.created),
            &time(metadata.accessed),
            &time(metadata.written),
        )
    } == 0
    {
        return Err(MigrationError::PlatformFailure);
    }
    suppress(file)
}
pub(super) fn restore_accessed(file: &File, value: u64) -> Result<()> {
    if unsafe { SetFileTime(file.as_raw_handle(), ptr::null(), &time(value), ptr::null()) } == 0 {
        return Err(MigrationError::PlatformFailure);
    }
    suppress(file)
}
