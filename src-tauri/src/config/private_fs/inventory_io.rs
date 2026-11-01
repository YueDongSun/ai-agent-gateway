//! Metadata-preserving, identity-bound reads for consumed migration inventories.

use super::{failure, AppError, Managed_directory_identity};
use std::{ffi::OsString, fs::File, path::Path};

#[allow(non_camel_case_types)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Inventory_object_metadata {
    pub object: Managed_directory_identity,
    pub attributes: u32,
    pub created: u64,
    pub accessed: u64,
    pub written: u64,
    pub unnamed_bytes: u64,
}

pub(crate) fn inventory_object_metadata(
    path: &Path,
) -> Result<Inventory_object_metadata, AppError> {
    #[cfg(windows)]
    {
        native::metadata(path)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Err(failure("inventory_platform_unsupported"))
    }
}

pub(crate) fn open_inventory_stream(path: &Path) -> Result<File, AppError> {
    #[cfg(windows)]
    {
        native::stream(path)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Err(failure("inventory_platform_unsupported"))
    }
}

pub(crate) fn directory_inventory_entries(path: &Path) -> Result<Vec<OsString>, AppError> {
    #[cfg(windows)]
    {
        native::entries(path)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Err(failure("inventory_platform_unsupported"))
    }
}

#[cfg(windows)]
pub(crate) fn inventory_object_metadata_from_file(
    file: &File,
) -> Result<Inventory_object_metadata, AppError> {
    native::from_file(file)
}

#[cfg(windows)]
pub(crate) fn open_inventory_security_object(path: &Path) -> Result<File, AppError> {
    // The owner separately enables and restores SeSecurityPrivilege. An
    // unavailable SACL is never represented by invented empty metadata.
    native::open(path, 0x0102_0008, 3, true)
}

#[cfg(all(test, windows))]
pub(crate) fn diagnose_inventory_security_object_open(
    path: &Path,
    observed: &File,
) -> Result<File, AppError> {
    native::observe_open(path, observed)
}

/// Write handle to a final directory reparse object, without traversing it.
/// The migration platform still admits the exact recorded Chrome alias and
/// checks its root, object identity and old/new pointer before using this.
#[cfg(windows)]
pub(crate) fn open_inventory_reparse_mutation_object(path: &Path) -> Result<File, AppError> {
    let file = native::open(path, 0x4102_0008, 3, true)?;
    let metadata = native::from_file(&file)?;
    if metadata.attributes & 0x410 != 0x410 {
        return Err(failure("inventory_directory_reparse_required"));
    }
    Ok(file)
}

#[cfg(all(test, windows))]
pub(crate) fn open_inventory_fixture_empty_directory_for_reparse(path: &Path) -> Result<File, AppError> {
    let file = native::open(path, 0x4102_0008, 3, false)?;
    let before = native::from_file(&file)?;
    if before.attributes & 0x410 != 0x10 || !native::entries(path)?.is_empty() {
        return Err(failure("inventory_empty_fixture_directory_required"));
    }
    if native::from_file(&file)? != before {
        return Err(failure("inventory_fixture_directory_changed"));
    }
    Ok(file)
}

#[cfg(windows)]
mod native {
    use super::*;
    use std::{
        ffi::c_void,
        mem::MaybeUninit,
        os::windows::{
            ffi::OsStringExt,
            io::{AsRawHandle, FromRawHandle},
        },
        ptr::null_mut,
    };

    const DIRECTORY: u32 = 0x10;
    const REPARSE: u32 = 0x400;
    const READ_ATTRIBUTES: u32 = 0x80;
    const WRITE_ATTRIBUTES: u32 = 0x100;
    const DIRECTORY_BUFFER_BYTES: usize = 64 * 1024;
    const MAX_DIRECTORY_ENTRIES: usize = 2_000_000;
    const FULL_DIR_NAME_OFFSET: usize = 68;

    #[repr(C)]
    struct FileTime {
        low: u32,
        high: u32,
    }
    #[repr(C)]
    struct FileInformation {
        attributes: u32,
        created: FileTime,
        accessed: FileTime,
        written: FileTime,
        volume: u32,
        size_high: u32,
        size_low: u32,
        links: u32,
        index_high: u32,
        index_low: u32,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateFileW(
            name: *const u16,
            access: u32,
            share: u32,
            security: *mut c_void,
            creation: u32,
            flags: u32,
            template: *mut c_void,
        ) -> *mut c_void;
        fn SetFileTime(
            file: *mut c_void,
            created: *const FileTime,
            accessed: *const FileTime,
            written: *const FileTime,
        ) -> i32;
        fn GetFileInformationByHandle(file: *mut c_void, information: *mut FileInformation) -> i32;
        fn GetFileInformationByHandleEx(
            file: *mut c_void,
            class: u32,
            information: *mut c_void,
            bytes: u32,
        ) -> i32;
    }

    fn validate(path: &Path, allow_final_reparse: bool) -> Result<(), AppError> {
        if !path.is_absolute()
            || path.components().any(|part| {
                matches!(
                    part,
                    std::path::Component::ParentDir | std::path::Component::CurDir
                )
            })
        {
            return Err(failure("inventory_path_invalid"));
        }
        // This is a filesystem-only API. Never admit a device namespace.
        let mut components = path.components();
        match components.next() {
            Some(std::path::Component::Prefix(prefix))
                if matches!(
                    prefix.kind(),
                    std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_)
                ) => {}
            _ => return Err(failure("inventory_local_disk_path_required")),
        }
        let parent = path
            .parent()
            .ok_or_else(|| failure("inventory_parent_missing"))?;
        super::super::reject_linked_existing_components(parent)?;
        if !allow_final_reparse {
            super::super::reject_linked_existing_components(path)?;
        }
        Ok(())
    }

    pub(super) fn open(
        path: &Path,
        access: u32,
        share: u32,
        allow_final_reparse: bool,
    ) -> Result<File, AppError> {
        validate(path, allow_final_reparse)?;
        let name = super::super::windows_path::encode_native_path(path)?;
        let handle = unsafe {
            CreateFileW(
                name.as_ptr(),
                access | READ_ATTRIBUTES | WRITE_ATTRIBUTES,
                share,
                null_mut(),
                3,
                0x0220_0000,
                null_mut(),
            )
        };
        if handle as isize == -1 || handle.is_null() {
            return Err(super::super::io_failure(
                "inventory_open_failed",
                &std::io::Error::last_os_error(),
            ));
        }
        let file = unsafe { File::from_raw_handle(handle) };
        let no_update = FileTime {
            low: u32::MAX,
            high: u32::MAX,
        };
        if unsafe {
            SetFileTime(
                file.as_raw_handle(),
                std::ptr::null(),
                &no_update,
                std::ptr::null(),
            )
        } == 0
        {
            return Err(super::super::io_failure(
                "inventory_suppress_access_time_failed",
                &std::io::Error::last_os_error(),
            ));
        }
        let information = from_file(&file)?;
        if !allow_final_reparse && information.attributes & REPARSE != 0 {
            return Err(failure("inventory_reparse_refused"));
        }
        validate(path, allow_final_reparse)?;
        Ok(file)
    }

    #[cfg(test)]
    pub(super) fn observe_open(path: &Path, observed: &File) -> Result<File, AppError> {
        fn stage(
            stage: &str,
            observed: &File,
            before: &Inventory_object_metadata,
        ) -> Result<(), AppError> {
            let after = from_file(observed)?;
            eprintln!("inventory_open_stage={stage} directory={} reparse={} object={} attrs={} created={} accessed={} written={} size={}", before.attributes & DIRECTORY != 0, before.attributes & REPARSE != 0, before.object != after.object, before.attributes != after.attributes, before.created != after.created, before.accessed != after.accessed, before.written != after.written, before.unnamed_bytes != after.unnamed_bytes);
            if before != &after {
                return Err(failure("inventory_open_observer_changed"));
            }
            Ok(())
        }
        let before = from_file(observed)?;
        stage("before_validate", observed, &before)?;
        validate(path, true)?;
        stage("after_validate", observed, &before)?;
        let name = super::super::windows_path::encode_native_path(path)?;
        stage("before_create_file", observed, &before)?;
        let handle = unsafe {
            CreateFileW(
                name.as_ptr(), 0x0102_0008 | READ_ATTRIBUTES | WRITE_ATTRIBUTES,
                3, null_mut(), 3, 0x0220_0000, null_mut(),
            )
        };
        if handle as isize == -1 || handle.is_null() {
            return Err(super::super::io_failure("inventory_open_failed", &std::io::Error::last_os_error()));
        }
        let file = unsafe { File::from_raw_handle(handle) };
        stage("after_create_file", observed, &before)?;
        let no_update = FileTime { low: u32::MAX, high: u32::MAX };
        if unsafe {
            SetFileTime(file.as_raw_handle(), std::ptr::null(), &no_update, std::ptr::null())
        } == 0 {
            return Err(super::super::io_failure("inventory_suppress_access_time_failed", &std::io::Error::last_os_error()));
        }
        stage("after_set_file_time", observed, &before)?;
        from_file(&file)?;
        stage("after_new_handle_metadata", observed, &before)?;
        validate(path, true)?;
        stage("after_post_validate", observed, &before)?;
        Ok(file)
    }

    fn time(value: &FileTime) -> u64 {
        (u64::from(value.high) << 32) | u64::from(value.low)
    }

    pub(super) fn from_file(file: &File) -> Result<Inventory_object_metadata, AppError> {
        let mut information = MaybeUninit::<FileInformation>::zeroed();
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), information.as_mut_ptr()) }
            == 0
        {
            return Err(super::super::io_failure(
                "inventory_information_failed",
                &std::io::Error::last_os_error(),
            ));
        }
        let information = unsafe { information.assume_init() };
        Ok(Inventory_object_metadata {
            object: Managed_directory_identity {
                volume: u64::from(information.volume),
                object: (u64::from(information.index_high) << 32)
                    | u64::from(information.index_low),
            },
            attributes: information.attributes,
            created: time(&information.created),
            accessed: time(&information.accessed),
            written: time(&information.written),
            unnamed_bytes: if information.attributes & DIRECTORY != 0 {
                0
            } else {
                (u64::from(information.size_high) << 32) | u64::from(information.size_low)
            },
        })
    }

    pub(super) fn metadata(path: &Path) -> Result<Inventory_object_metadata, AppError> {
        from_file(&open(path, 0, 3, true)?)
    }

    pub(super) fn stream(path: &Path) -> Result<File, AppError> {
        let file = open(path, 0x8000_0000, 1, false)?;
        if from_file(&file)?.attributes & DIRECTORY != 0 {
            return Err(failure("inventory_stream_file_required"));
        }
        Ok(file)
    }

    fn u32_at(bytes: &[u8], offset: usize) -> Result<u32, AppError> {
        let value: [u8; 4] = bytes
            .get(offset..offset + 4)
            .ok_or_else(|| failure("inventory_directory_record_invalid"))?
            .try_into()
            .map_err(|_| failure("inventory_directory_record_invalid"))?;
        Ok(u32::from_le_bytes(value))
    }

    pub(super) fn entries(path: &Path) -> Result<Vec<OsString>, AppError> {
        let file = open(path, 1, 3, false)?;
        let before = from_file(&file)?;
        if before.attributes & DIRECTORY == 0 {
            return Err(failure("inventory_directory_required"));
        }
        let mut buffer = vec![0u64; DIRECTORY_BUFFER_BYTES / 8];
        let mut entries = Vec::new();
        let mut information_class = 15;
        loop {
            buffer.fill(0);
            if unsafe {
                GetFileInformationByHandleEx(
                    file.as_raw_handle(),
                    information_class,
                    buffer.as_mut_ptr().cast(),
                    DIRECTORY_BUFFER_BYTES as u32,
                )
            } == 0
            {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() == Some(18) {
                    break;
                }
                return Err(super::super::io_failure(
                    "inventory_enumeration_failed",
                    &error,
                ));
            }
            information_class = 14;
            let bytes = unsafe {
                std::slice::from_raw_parts(buffer.as_ptr().cast::<u8>(), DIRECTORY_BUFFER_BYTES)
            };
            let mut offset: usize = 0;
            loop {
                let record = bytes
                    .get(offset..)
                    .ok_or_else(|| failure("inventory_directory_record_invalid"))?;
                let next = u32_at(record, 0)? as usize;
                let name_bytes = u32_at(record, 60)? as usize;
                if name_bytes == 0 || name_bytes % 2 != 0 || name_bytes > 65534 {
                    return Err(failure("inventory_directory_record_invalid"));
                }
                let name_data = record
                    .get(FULL_DIR_NAME_OFFSET..FULL_DIR_NAME_OFFSET + name_bytes)
                    .ok_or_else(|| failure("inventory_directory_record_invalid"))?;
                let units: Vec<u16> = name_data
                    .chunks_exact(2)
                    .map(|part| u16::from_le_bytes([part[0], part[1]]))
                    .collect();
                if units.iter().any(|unit| matches!(*unit, 0 | 47 | 92)) {
                    return Err(failure("inventory_directory_name_invalid"));
                }
                let name = OsString::from_wide(&units);
                if name != "." && name != ".." {
                    entries.push(name);
                    if entries.len() > MAX_DIRECTORY_ENTRIES {
                        return Err(failure("inventory_directory_limit"));
                    }
                }
                if next == 0 {
                    break;
                }
                if next % 8 != 0 || next < FULL_DIR_NAME_OFFSET + name_bytes {
                    return Err(failure("inventory_directory_record_invalid"));
                }
                offset = offset
                    .checked_add(next)
                    .ok_or_else(|| failure("inventory_directory_record_invalid"))?;
            }
        }
        if from_file(&file)? != before {
            return Err(failure("inventory_directory_changed"));
        }
        Ok(entries)
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::{fs, io::Read};

    #[test]
    fn protected_inventory_preserves_timestamps_and_exact_native_names() {
        let fixture = super::super::private_temp_dir(&std::env::temp_dir()).unwrap();
        let root = fixture.path().join("tree");
        super::super::prepare_private_dir(&root).unwrap();
        let name = OsString::from("synthetic_会话.jsonl");
        let file = root.join(&name);
        fs::write(&file, b"complete synthetic session\n").unwrap();
        let before = inventory_object_metadata(&file).unwrap();
        let before_dir = inventory_object_metadata(&root).unwrap();
        let entries = directory_inventory_entries(&root).unwrap();
        assert_eq!(entries, vec![name]);
        let mut opened = open_inventory_stream(&file).unwrap();
        let mut bytes = Vec::new();
        opened.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"complete synthetic session\n");
        drop(opened);
        assert_eq!(inventory_object_metadata(&file).unwrap(), before);
        assert_eq!(inventory_object_metadata(&root).unwrap(), before_dir);
    }

    #[test]
    fn stream_guard_excludes_concurrent_writes_and_directory_enumeration_rejects_files() {
        let fixture = super::super::private_temp_dir(&std::env::temp_dir()).unwrap();
        let file = fixture.path().join("fixture");
        fs::write(&file, b"complete synthetic session\n").unwrap();
        let opened = open_inventory_stream(&file).unwrap();
        assert!(fs::OpenOptions::new().write(true).open(&file).is_err());
        assert!(fs::rename(&file, file.with_extension("other")).is_err());
        assert!(directory_inventory_entries(&file).is_err());
        drop(opened);
        assert!(fs::OpenOptions::new().write(true).open(&file).is_ok());
    }
}
