//! Whole-object backup streams read from the already admitted source handles.
//! Destination descendants are created exclusively relative to held parents.

use super::*;
use crate::config::private_fs;
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::c_void,
    os::windows::{
        ffi::OsStrExt,
        fs::OpenOptionsExt,
        io::{AsRawHandle, FromRawHandle},
    },
    ptr,
};

const BUFFER_BYTES: usize = 64 * 1024;
const STREAM_HEADER_BYTES: usize = 20;
const MAX_METADATA_BYTES: u64 = 1024 * 1024;
const MAX_STREAM_NAME_BYTES: usize = 65534;
const DESTINATION_ACCESS: u32 = 0xC11E_0100;
const UNSUPPORTED_STORAGE_ATTRIBUTES: u32 = 0x0000_0A00;

// This adapter remains isolated to Windows test qualification. Production
// contracts and private-fs exports are deliberately unchanged.
struct BackupObject<'a> {
    relative: &'a Path,
    metadata: &'a EntryMetadata,
    file: &'a File,
}

#[repr(C)]
struct UnicodeString {
    length: u16,
    maximum_length: u16,
    buffer: *mut u16,
}
#[repr(C)]
struct ObjectAttributes {
    length: u32,
    root: *mut c_void,
    name: *mut UnicodeString,
    attributes: u32,
    security: *mut c_void,
    quality: *mut c_void,
}
#[repr(C)]
struct IoStatus {
    status: usize,
    information: usize,
}
#[repr(C)]
struct BasicInformation {
    created: i64,
    accessed: i64,
    written: i64,
    changed: i64,
    attributes: u32,
}

#[link(name = "ntdll")]
extern "system" {
    fn NtCreateFile(
        file: *mut *mut c_void,
        access: u32,
        attributes: *mut ObjectAttributes,
        status: *mut IoStatus,
        allocation: *const i64,
        file_attributes: u32,
        share: u32,
        disposition: u32,
        options: u32,
        ea: *const c_void,
        ea_bytes: u32,
    ) -> i32;
}
#[link(name = "kernel32")]
extern "system" {
    fn BackupRead(
        file: *mut c_void,
        buffer: *mut u8,
        bytes: u32,
        read: *mut u32,
        abort: i32,
        security: i32,
        context: *mut *mut c_void,
    ) -> i32;
    fn BackupWrite(
        file: *mut c_void,
        buffer: *const u8,
        bytes: u32,
        written: *mut u32,
        abort: i32,
        security: i32,
        context: *mut *mut c_void,
    ) -> i32;
    fn SetFileInformationByHandle(
        file: *mut c_void,
        class: u32,
        information: *const c_void,
        bytes: u32,
    ) -> i32;
}

struct BackupContext<'a> {
    file: &'a File,
    context: *mut c_void,
    writing: bool,
}
impl BackupContext<'_> {
    fn release_with(
        &mut self,
        abort: impl FnOnce(&File, &mut *mut c_void, bool) -> i32,
    ) -> Result<()> {
        let mut context = std::mem::replace(&mut self.context, ptr::null_mut());
        if context.is_null() {
            return Ok(());
        }
        // The abort consumes this wrapper's ownership even on an unknown
        // failure. Drop must never retry a possibly freed native pointer.
        if abort(self.file, &mut context, self.writing) == 0 {
            return Err(MigrationError::BackupUnverified);
        }
        Ok(())
    }
    fn release(&mut self) -> Result<()> {
        self.release_with(|file, context, writing| unsafe {
            if writing {
                BackupWrite(
                    file.as_raw_handle(),
                    ptr::null(),
                    0,
                    ptr::null_mut(),
                    1,
                    1,
                    context,
                )
            } else {
                BackupRead(
                    file.as_raw_handle(),
                    ptr::null_mut(),
                    0,
                    ptr::null_mut(),
                    1,
                    1,
                    context,
                )
            }
        })
    }
}
impl Drop for BackupContext<'_> {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

struct Reader<'a, 'b, 'c> {
    platform: &'a WindowsStoragePlatform<'b>,
    context: BackupContext<'c>,
    buffer: Vec<u8>,
    position: usize,
    available: usize,
    eof: bool,
}
impl Reader<'_, '_, '_> {
    fn exact(&mut self, output: &mut [u8], allow_eof: bool) -> Result<bool> {
        let mut copied = 0;
        while copied < output.len() {
            self.platform.check()?;
            if self.position == self.available {
                if self.eof {
                    return if copied == 0 && allow_eof {
                        Ok(false)
                    } else {
                        Err(MigrationError::BackupUnverified)
                    };
                }
                let mut read = 0u32;
                if unsafe {
                    BackupRead(
                        self.context.file.as_raw_handle(),
                        self.buffer.as_mut_ptr(),
                        self.buffer.len() as u32,
                        &mut read,
                        0,
                        1,
                        &mut self.context.context,
                    )
                } == 0
                    || read as usize > self.buffer.len()
                {
                    return Err(MigrationError::BackupUnverified);
                }
                self.position = 0;
                self.available = read as usize;
                self.eof = read == 0;
                continue;
            }
            let count = (self.available - self.position).min(output.len() - copied);
            output[copied..copied + count]
                .copy_from_slice(&self.buffer[self.position..self.position + count]);
            self.position += count;
            copied += count;
        }
        Ok(true)
    }
}

fn write(
    platform: &WindowsStoragePlatform<'_>,
    context: &mut BackupContext<'_>,
    bytes: &[u8],
) -> Result<()> {
    let mut position = 0;
    while position < bytes.len() {
        platform.check()?;
        let mut written = 0u32;
        if unsafe {
            BackupWrite(
                context.file.as_raw_handle(),
                bytes[position..].as_ptr(),
                (bytes.len() - position) as u32,
                &mut written,
                0,
                1,
                &mut context.context,
            )
        } == 0
            || written == 0
            || written as usize > bytes.len() - position
        {
            return Err(MigrationError::BackupUnverified);
        }
        position += written as usize;
    }
    Ok(())
}

fn transfer(
    platform: &WindowsStoragePlatform<'_>,
    source: &File,
    destination: &File,
    expected: &EntryMetadata,
) -> Result<()> {
    let mut reader = Reader {
        platform,
        context: BackupContext {
            file: source,
            context: ptr::null_mut(),
            writing: false,
        },
        buffer: vec![0; BUFFER_BYTES],
        position: 0,
        available: 0,
        eof: false,
    };
    let mut writer = BackupContext {
        file: destination,
        context: ptr::null_mut(),
        writing: true,
    };
    let mut seen = BTreeSet::new();
    let mut body = vec![0u8; BUFFER_BYTES];
    loop {
        let mut header = [0u8; STREAM_HEADER_BYTES];
        if !reader.exact(&mut header, true)? {
            break;
        }
        let (kind, size, name_bytes) = stream_header(&header)?;
        let mut name = vec![0u8; name_bytes];
        reader.exact(&mut name, false)?;
        let units: Vec<u16> = name
            .chunks_exact(2)
            .map(|value| u16::from_le_bytes([value[0], value[1]]))
            .collect();
        let name = String::from_utf16(&units).map_err(|_| MigrationError::BackupUnverified)?;
        let admitted = match kind {
            1 => name.is_empty() && size == expected.unnamed_bytes,
            2 => name.is_empty() && size <= MAX_METADATA_BYTES,
            3 => name.is_empty() && (20..=MAX_METADATA_BYTES).contains(&size),
            4 => expected
                .streams
                .iter()
                .any(|stream| stream.name == name && stream.bytes == size),
            _ => false,
        };
        if !admitted || !seen.insert((kind, name.clone())) {
            return Err(MigrationError::BackupUnverified);
        }
        write(platform, &mut writer, &header)?;
        let native_name: Vec<u8> = units.iter().flat_map(|value| value.to_le_bytes()).collect();
        write(platform, &mut writer, &native_name)?;
        let mut remaining = size;
        while remaining != 0 {
            let count = remaining.min(body.len() as u64) as usize;
            reader.exact(&mut body[..count], false)?;
            write(platform, &mut writer, &body[..count])?;
            remaining -= count as u64;
        }
    }
    if !seen.contains(&(3, String::new()))
        || (expected.unnamed_bytes != 0 && !seen.contains(&(1, String::new())))
        || (!expected.extended_attributes.is_empty() && !seen.contains(&(2, String::new())))
        || expected
            .streams
            .iter()
            .any(|stream| !seen.contains(&(4, stream.name.clone())))
    {
        return Err(MigrationError::BackupUnverified);
    }
    reader.context.release()?;
    writer.release()?;
    Ok(())
}

fn stream_header(header: &[u8; STREAM_HEADER_BYTES]) -> Result<(u32, u64, usize)> {
    let kind = u32::from_le_bytes(
        header[0..4]
            .try_into()
            .map_err(|_| MigrationError::BackupUnverified)?,
    );
    let attributes = u32::from_le_bytes(
        header[4..8]
            .try_into()
            .map_err(|_| MigrationError::BackupUnverified)?,
    );
    let size = i64::from_le_bytes(
        header[8..16]
            .try_into()
            .map_err(|_| MigrationError::BackupUnverified)?,
    );
    let name_bytes = u32::from_le_bytes(
        header[16..20]
            .try_into()
            .map_err(|_| MigrationError::BackupUnverified)?,
    ) as usize;
    // The qualification supports normal ordinary data/EA/ADS and the SDK's
    // security flag only on a security stream. No control header is forwarded.
    let ordinary = matches!(kind, 1 | 2 | 4) && attributes == 0;
    let security = kind == 3 && matches!(attributes, 0 | 2);
    if size < 0
        || name_bytes > MAX_STREAM_NAME_BYTES
        || name_bytes % 2 != 0
        || !(ordinary || security)
    {
        return Err(MigrationError::BackupUnverified);
    }
    Ok((kind, size as u64, name_bytes))
}

fn child_units(name: &std::ffi::OsStr) -> Result<Vec<u16>> {
    let units: Vec<u16> = name.encode_wide().collect();
    if units.is_empty()
        || units.len() > MAX_STREAM_NAME_BYTES / 2
        || name == "."
        || name == ".."
        || units.iter().any(|value| matches!(*value, 0 | 47 | 58 | 92))
    {
        return Err(MigrationError::InvalidPaths);
    }
    Ok(units)
}

fn create_child(parent: &File, name: &std::ffi::OsStr, directory: bool) -> Result<File> {
    let mut units = child_units(name)?;
    let mut name = UnicodeString {
        length: (units.len() * 2) as u16,
        maximum_length: (units.len() * 2) as u16,
        buffer: units.as_mut_ptr(),
    };
    let mut attributes = ObjectAttributes {
        length: std::mem::size_of::<ObjectAttributes>() as u32,
        root: parent.as_raw_handle(),
        name: &mut name,
        attributes: 0x40,
        security: ptr::null_mut(),
        quality: ptr::null_mut(),
    };
    let mut status = IoStatus {
        status: 0,
        information: 0,
    };
    let mut raw = ptr::null_mut();
    let result = unsafe {
        NtCreateFile(
            &mut raw,
            DESTINATION_ACCESS,
            &mut attributes,
            &mut status,
            ptr::null(),
            if directory { 0x10 } else { 0x80 },
            1,
            2,
            0x0020_4020 | if directory { 1 } else { 0x40 },
            ptr::null(),
            0,
        )
    };
    let file = if raw.is_null() || raw as isize == -1 {
        None
    } else {
        Some(unsafe { File::from_raw_handle(raw) })
    };
    if result < 0 || status.information != 2 {
        return Err(MigrationError::ObjectConflict);
    }
    let file = file.ok_or(MigrationError::ObjectConflict)?;
    let metadata = private_fs::inventory_object_metadata_from_file(&file)
        .map_err(|_| MigrationError::BackupUnverified)?;
    if metadata.attributes & 0x400 != 0 || (metadata.attributes & 0x10 != 0) != directory {
        return Err(MigrationError::ObjectConflict);
    }
    Ok(file)
}

fn restore_metadata(file: &File, expected: &EntryMetadata) -> Result<()> {
    let basic = BasicInformation {
        created: expected.created as i64,
        accessed: expected.accessed as i64,
        written: expected.written as i64,
        changed: 0,
        attributes: expected.attributes,
    };
    if unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            0,
            (&basic as *const BasicInformation).cast(),
            std::mem::size_of::<BasicInformation>() as u32,
        )
    } == 0
    {
        return Err(MigrationError::BackupUnverified);
    }
    file_times::restore_all(file, expected)
}

fn copy(
    platform: &WindowsStoragePlatform<'_>,
    destination: &Path,
    expected_root: DirectoryIdentity,
    objects: &[BackupObject<'_>],
) -> Result<()> {
    platform.scope(destination)?;
    if !super::super::inventory::same(destination, &platform.paths.source_backup)
        || objects
            .first()
            .is_none_or(|object| !object.relative.as_os_str().is_empty())
    {
        return Err(MigrationError::InvalidPaths);
    }
    let mut previous: Option<&Path> = None;
    let mut directories = BTreeSet::new();
    for object in objects {
        platform.check()?;
        if object.metadata.attributes & (0x400 | UNSUPPORTED_STORAGE_ATTRIBUTES) != 0
            || object
                .relative
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
            || previous.is_some_and(|previous| previous >= object.relative)
            || (!object.relative.as_os_str().is_empty()
                && object
                    .relative
                    .parent()
                    .is_none_or(|parent| !directories.contains(parent)))
        {
            return Err(MigrationError::InvalidPaths);
        }
        for part in object.relative.components() {
            let std::path::Component::Normal(name) = part else {
                return Err(MigrationError::InvalidPaths);
            };
            child_units(name)?;
        }
        if metadata::capture_file(platform, object.file)? != *object.metadata {
            return Err(MigrationError::SourceChanged);
        }
        if object.metadata.attributes & 0x10 != 0 {
            directories.insert(object.relative.to_path_buf());
        }
        previous = Some(object.relative);
    }
    if objects[0].metadata.attributes & 0x410 != 0x10 {
        return Err(MigrationError::InvalidPaths);
    }
    let root = std::fs::OpenOptions::new()
        .access_mode(DESTINATION_ACCESS)
        .share_mode(1)
        .custom_flags(0x0220_0000)
        .open(destination)
        .map_err(|_| MigrationError::BackupUnverified)?;
    let root_metadata = private_fs::inventory_object_metadata_from_file(&root)
        .map_err(|_| MigrationError::BackupUnverified)?;
    if root_metadata.object != expected_root || root_metadata.attributes & 0x410 != 0x10 {
        return Err(MigrationError::ObjectConflict);
    }
    let mut destinations = BTreeMap::new();
    destinations.insert(PathBuf::new(), root);
    // Keep every destination handle, including each parent, against replacement
    // until all stream writes and bottom-up metadata restoration are complete.
    for object in objects {
        platform.check()?;
        if object.relative.as_os_str().is_empty() {
            continue;
        }
        let parent = object
            .relative
            .parent()
            .ok_or(MigrationError::InvalidPaths)?;
        let parent = destinations
            .get(parent)
            .ok_or(MigrationError::InvalidPaths)?;
        let file = create_child(
            parent,
            object
                .relative
                .file_name()
                .ok_or(MigrationError::InvalidPaths)?,
            object.metadata.attributes & 0x10 != 0,
        )?;
        let actual = private_fs::inventory_object_metadata_from_file(&file)
            .map_err(|_| MigrationError::BackupUnverified)?;
        if actual.object.volume != expected_root.volume
            || destinations
                .insert(object.relative.to_path_buf(), file)
                .is_some()
        {
            return Err(MigrationError::ObjectConflict);
        }
    }
    for object in objects {
        platform.check()?;
        let destination = destinations
            .get(object.relative)
            .ok_or(MigrationError::BackupUnverified)?;
        transfer(platform, object.file, destination, object.metadata)?;
        if metadata::capture_file(platform, object.file)? != *object.metadata {
            return Err(MigrationError::SourceChanged);
        }
    }
    for object in objects.iter().rev() {
        platform.check()?;
        let destination = destinations
            .get(object.relative)
            .ok_or(MigrationError::BackupUnverified)?;
        restore_metadata(destination, object.metadata)?;
        let actual = metadata::capture_file(platform, destination)?;
        let mut expected = object.metadata.clone();
        expected.object = actual.object;
        if actual != expected {
            return Err(MigrationError::BackupUnverified);
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "backup_transfer_tests.rs"]
mod tests;
