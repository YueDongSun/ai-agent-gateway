//! Private filesystem boundary for credential-bearing AAG state.
//!
//! This module deliberately uses only operating-system facilities already
//! available to the application. It does not encrypt data at rest: callers
//! must still prefer secret references over copied plaintext credentials.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use crate::error::AppError;

const ERROR_PREFIX: &str = "private_storage";

fn failure(code: &str) -> AppError {
    AppError::Config(format!("{ERROR_PREFIX}:{code}"))
}

fn io_failure(code: &str, error: &std::io::Error) -> AppError {
    AppError::Config(format!("{ERROR_PREFIX}:{code}:{}", error.kind()))
}

fn metadata(path: &Path) -> Result<Option<fs::Metadata>, AppError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if platform::is_linked(&metadata) {
                Err(failure("linked_path"))
            } else {
                Ok(Some(metadata))
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io_failure("metadata_failed", &error)),
    }
}

fn reject_linked_existing_components(path: &Path) -> Result<(), AppError> {
    for component in path.ancestors() {
        if let Some(metadata) = metadata(component)? {
            if !metadata.is_dir() && component != path {
                return Err(failure("non_directory_component"));
            }
        }
    }
    Ok(())
}

/// Check a prospective client configuration file without creating files or
/// changing permissions. Check every ancestor before callers read credentials
/// or harden an existing target; a linked parent must never be followed first.
pub(crate) fn validate_external_file_path(path: &Path) -> Result<(), AppError> {
    if !path.is_absolute() {
        return Err(failure("file_not_absolute"));
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(failure("file_parent_traversal"));
    }
    if path.to_string_lossy().chars().any(char::is_control) {
        return Err(failure("file_control_character"));
    }
    if path.file_name().is_none() {
        return Err(failure("filename_invalid"));
    }
    for ancestor in path.ancestors() {
        if let Some(current) = metadata(ancestor)? {
            if ancestor == path {
                if !current.is_file() {
                    return Err(failure("file_expected"));
                }
            } else if !current.is_dir() {
                return Err(failure("non_directory_component"));
            }
        }
    }
    Ok(())
}

fn reject_broad_private_dir(path: &Path) -> Result<(), AppError> {
    if !path.is_absolute() {
        return Err(failure("directory_not_absolute"));
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(failure("directory_parent_traversal"));
    }
    if path.parent().is_none() {
        return Err(failure("directory_too_broad"));
    }
    let current = std::env::current_dir()
        .map_err(|error| io_failure("current_directory_unavailable", &error))?;
    if path.components().eq(current.components()) {
        return Err(failure("directory_too_broad"));
    }
    Ok(())
}

/// Create or harden an AAG-owned credential directory.
///
/// New POSIX directories are `0700`. Existing directories retain tighter
/// owner bits while group/other access is removed. Windows directories use a
/// protected DACL granting access only to the current owner, without elevation
/// or ownership takeover.
fn prepare_private_dir_with_policy(path: &Path, harden_existing: bool) -> Result<(), AppError> {
    reject_broad_private_dir(path)?;
    reject_linked_existing_components(path)?;
    let existed = metadata(path)?.is_some();
    if !existed {
        fs::create_dir_all(path).map_err(|error| io_failure("directory_create_failed", &error))?;
    }
    reject_linked_existing_components(path)?;
    let current = metadata(path)?.ok_or_else(|| failure("directory_missing_after_create"))?;
    if !current.is_dir() {
        return Err(failure("directory_expected"));
    }
    if !existed || harden_existing {
        platform::secure_directory(path, existed)?;
    }
    platform::verify_directory(path)
}

/// Create a new private directory or verify an existing external directory
/// without changing its ACL/mode.
pub(crate) fn prepare_private_dir(path: &Path) -> Result<(), AppError> {
    prepare_private_dir_with_policy(path, false)
}

/// Create or harden a directory that is unambiguously owned by AAG itself.
pub(crate) fn prepare_managed_private_dir(path: &Path) -> Result<(), AppError> {
    prepare_private_dir_with_policy(path, true)
}

/// Prepare an external application's parent directory without changing an
/// existing ACL/mode. Read/traverse grants are allowed, but linked paths,
/// owner mismatch, and untrusted write/delete grants fail closed.
fn prepare_external_parent_dir(path: &Path) -> Result<(), AppError> {
    reject_broad_private_dir(path)?;
    reject_linked_existing_components(path)?;
    let existed = metadata(path)?.is_some();
    if !existed {
        let mut existing_parent = None;
        for ancestor in path.ancestors().skip(1) {
            if metadata(ancestor)?.is_some() {
                existing_parent = Some(ancestor);
                break;
            }
        }
        let existing_parent = existing_parent.ok_or_else(|| failure("external_parent_missing"))?;
        platform::verify_external_directory(existing_parent)?;
        fs::create_dir_all(path).map_err(|error| io_failure("directory_create_failed", &error))?;
        platform::secure_directory(path, false)?;
        return platform::verify_directory(path);
    }
    let current = metadata(path)?.ok_or_else(|| failure("directory_missing"))?;
    if !current.is_dir() {
        return Err(failure("directory_expected"));
    }
    platform::verify_external_directory(path)
}

/// Create an empty private file, or harden an existing regular file.
/// Credential bytes must only be written after this function succeeds.
pub(crate) fn ensure_private_file(path: &Path) -> Result<(), AppError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| failure("parent_missing"))?;
    prepare_external_parent_dir(parent)?;
    match metadata(path)? {
        Some(current) => {
            if !current.is_file() {
                return Err(failure("file_expected"));
            }
            platform::secure_existing_file(path).map(|_| ())?;
        }
        None => {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            platform::configure_private_open(&mut options);
            let file = options
                .open(path)
                .map_err(|error| io_failure("file_create_failed", &error))?;
            if let Err(error) = platform::secure_new_file(path) {
                drop(file);
                let _ = fs::remove_file(path);
                return Err(error);
            }
            file.sync_all()
                .map_err(|error| io_failure("file_sync_failed", &error))?;
        }
    }
    platform::verify_file(path)
}

/// Harden and verify an existing regular credential file.
pub(crate) fn secure_existing_file(path: &Path) -> Result<(), AppError> {
    reject_linked_existing_components(path)?;
    let current = metadata(path)?.ok_or_else(|| failure("file_missing"))?;
    if !current.is_file() {
        return Err(failure("file_expected"));
    }
    platform::secure_existing_file(path).map(|_| ())?;
    platform::verify_file(path)
}

#[cfg(test)]
pub(crate) fn verify_private_dir(path: &Path) -> Result<(), AppError> {
    platform::verify_directory(path)
}

#[cfg(test)]
pub(crate) fn verify_private_file(path: &Path) -> Result<(), AppError> {
    platform::verify_file(path)
}

fn random_suffix() -> Result<String, AppError> {
    let mut nonce = [0u8; 16];
    getrandom::getrandom(&mut nonce).map_err(|_| failure("random_unavailable"))?;
    Ok(hex::encode(nonce))
}

fn temporary_path(path: &Path, suffix: &str) -> Result<PathBuf, AppError> {
    let parent = path.parent().ok_or_else(|| failure("parent_missing"))?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| failure("filename_invalid"))?;
    Ok(parent.join(format!(".{name}.aag_tmp_{suffix}")))
}

fn cleanup_owned_temporary(path: &Path, identity: &platform::FileIdentity) {
    let Ok(Some(current)) = metadata(path) else {
        return;
    };
    if current.is_file()
        && platform::file_identity(path)
            .map(|current| current == *identity)
            .unwrap_or(false)
    {
        let _ = fs::remove_file(path);
    }
}

fn atomic_write_with_suffix<F>(
    path: &Path,
    data: &[u8],
    inherited_permissions: Option<platform::FilePermissions>,
    suffix: &str,
    before_replace: F,
) -> Result<(), AppError>
where
    F: FnOnce(&Path) -> Result<(), AppError>,
{
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| failure("parent_missing"))?;
    prepare_external_parent_dir(parent)?;

    let original_identity = match metadata(path)? {
        Some(current) => {
            if !current.is_file() {
                return Err(failure("file_expected"));
            }
            Some(platform::file_identity(path)?)
        }
        None => None,
    };

    let temporary = temporary_path(path, suffix)?;
    if metadata(&temporary)?.is_some() {
        return Err(failure("temporary_collision"));
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    platform::configure_private_open(&mut options);
    let mut file = Some(
        options
            .open(&temporary)
            .map_err(|error| io_failure("temporary_create_failed", &error))?,
    );
    let owned_identity = platform::file_identity_from_file(
        file.as_ref()
            .ok_or_else(|| failure("temporary_closed_early"))?,
    )?;

    let result = (|| {
        platform::secure_new_file(&temporary)?;
        if let Some(permissions) = &inherited_permissions {
            platform::apply_file_permissions(permissions, &temporary)?;
        }
        file.as_mut()
            .ok_or_else(|| failure("temporary_closed_early"))?
            .write_all(data)
            .map_err(|error| io_failure("temporary_write_failed", &error))?;
        file.as_ref()
            .ok_or_else(|| failure("temporary_closed_early"))?
            .sync_all()
            .map_err(|error| io_failure("temporary_sync_failed", &error))?;
        platform::verify_file(&temporary)?;
        drop(file.take());

        before_replace(&temporary)?;
        let current_identity = match metadata(path)? {
            Some(current) if current.is_file() => Some(platform::file_identity(path)?),
            Some(_) => return Err(failure("target_changed")),
            None => None,
        };
        match (original_identity, current_identity) {
            (None, None) => {}
            (Some(expected), Some(current)) if expected == current => {}
            _ => return Err(failure("target_changed")),
        }
        platform::replace_file(&temporary, path)?;
        platform::verify_file(path)?;
        platform::sync_parent(parent)?;
        Ok::<_, AppError>(())
    })();

    match result {
        Ok(()) => Ok(()),
        Err(error) => {
            drop(file.take());
            cleanup_owned_temporary(&temporary, &owned_identity);
            Err(error)
        }
    }
}

fn atomic_write_with_permissions(
    path: &Path,
    data: &[u8],
    permissions: Option<platform::FilePermissions>,
) -> Result<(), AppError> {
    let suffix = random_suffix()?;
    atomic_write_with_suffix(path, data, permissions, &suffix, |_| Ok(()))
}

/// Atomically replace a credential file using a private, exclusive temporary
/// file in the same directory. Existing safe permissions are preserved when
/// they are tighter than the default.
pub(crate) fn atomic_write(path: &Path, data: &[u8]) -> Result<(), AppError> {
    validate_external_file_path(path)?;
    let permissions = match metadata(path)? {
        Some(current) => {
            if !current.is_file() {
                return Err(failure("file_expected"));
            }
            Some(platform::secure_existing_file(path)?)
        }
        None => None,
    };
    atomic_write_with_permissions(path, data, permissions)
}

/// Copy an exact plaintext backup into a private atomic destination. A safe,
/// tighter source permission set is retained; unsafe source grants never
/// propagate into the backup.
pub(crate) fn backup_file(source: &Path, destination: &Path) -> Result<(), AppError> {
    reject_linked_existing_components(source)?;
    let source_metadata = metadata(source)?.ok_or_else(|| failure("backup_source_missing"))?;
    if !source_metadata.is_file() {
        return Err(failure("backup_source_not_file"));
    }
    let permissions = platform::capture_safe_file_permissions(source)?;
    let data = fs::read(source).map_err(|error| io_failure("backup_read_failed", &error))?;
    atomic_write_with_permissions(destination, &data, permissions)
}

/// Harden SQLite's main file and any live WAL/SHM sidecars. The containing
/// directory is already private, so newly created sidecars are never exposed
/// while SQLite is starting; this call makes their own ACL/mode explicit.
fn sqlite_sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

/// Reject linked/reparse sidecars before SQLite can follow them. Existing
/// regular sidecars are hardened before the connection is opened.
pub(crate) fn preflight_sqlite_files(path: &Path) -> Result<(), AppError> {
    secure_existing_file(path)?;
    for suffix in ["-wal", "-shm"] {
        let sidecar = sqlite_sidecar_path(path, suffix);
        if metadata(&sidecar)?.is_some() {
            secure_existing_file(&sidecar)?;
        }
    }
    Ok(())
}

#[cfg(unix)]
mod platform {
    use super::{failure, io_failure};
    use crate::error::AppError;
    use std::fs::{self, File, Metadata, OpenOptions};
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    use std::path::Path;

    #[derive(Clone, Copy)]
    pub(super) struct FilePermissions {
        mode: u32,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(super) struct FileIdentity {
        device: u64,
        inode: u64,
    }

    extern "C" {
        fn geteuid() -> u32;
    }

    fn current_uid() -> u32 {
        // SAFETY: geteuid has no arguments and no memory safety preconditions.
        unsafe { geteuid() }
    }

    fn verify_owner(metadata: &Metadata) -> Result<(), AppError> {
        if metadata.uid() != current_uid() {
            return Err(failure("owner_mismatch"));
        }
        Ok(())
    }

    pub(super) fn is_linked(metadata: &Metadata) -> bool {
        metadata.file_type().is_symlink()
    }

    pub(super) fn secure_directory(path: &Path, existed: bool) -> Result<(), AppError> {
        let metadata = fs::symlink_metadata(path)
            .map_err(|error| io_failure("directory_metadata_failed", &error))?;
        verify_owner(&metadata)?;
        let current = metadata.permissions().mode() & 0o777;
        let desired = if existed { current & 0o700 } else { 0o700 };
        if current != desired {
            fs::set_permissions(path, fs::Permissions::from_mode(desired))
                .map_err(|error| io_failure("directory_permissions_failed", &error))?;
        }
        Ok(())
    }

    pub(super) fn verify_directory(path: &Path) -> Result<(), AppError> {
        let metadata = fs::symlink_metadata(path)
            .map_err(|error| io_failure("directory_metadata_failed", &error))?;
        verify_owner(&metadata)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(failure("directory_expected"));
        }
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(failure("directory_permissions_unsafe"));
        }
        Ok(())
    }

    pub(super) fn verify_external_directory(path: &Path) -> Result<(), AppError> {
        let metadata = fs::symlink_metadata(path)
            .map_err(|error| io_failure("directory_metadata_failed", &error))?;
        verify_owner(&metadata)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(failure("directory_expected"));
        }
        if metadata.permissions().mode() & 0o022 != 0 {
            return Err(failure("external_directory_write_unsafe"));
        }
        Ok(())
    }

    pub(super) fn configure_private_open(options: &mut OpenOptions) {
        options.mode(0o600);
    }

    pub(super) fn secure_new_file(path: &Path) -> Result<(), AppError> {
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .map_err(|error| io_failure("file_permissions_failed", &error))?;
        verify_file(path)
    }

    pub(super) fn secure_existing_file(path: &Path) -> Result<FilePermissions, AppError> {
        let metadata = fs::symlink_metadata(path)
            .map_err(|error| io_failure("file_metadata_failed", &error))?;
        verify_owner(&metadata)?;
        let current = metadata.permissions().mode() & 0o777;
        let desired = current & 0o600;
        if current != desired {
            fs::set_permissions(path, fs::Permissions::from_mode(desired))
                .map_err(|error| io_failure("file_permissions_failed", &error))?;
        }
        verify_file(path)?;
        Ok(FilePermissions { mode: desired })
    }

    pub(super) fn capture_safe_file_permissions(
        path: &Path,
    ) -> Result<Option<FilePermissions>, AppError> {
        let metadata = fs::symlink_metadata(path)
            .map_err(|error| io_failure("file_metadata_failed", &error))?;
        verify_owner(&metadata)?;
        Ok(Some(FilePermissions {
            mode: metadata.permissions().mode() & 0o600,
        }))
    }

    pub(super) fn apply_file_permissions(
        permissions: &FilePermissions,
        path: &Path,
    ) -> Result<(), AppError> {
        fs::set_permissions(path, fs::Permissions::from_mode(permissions.mode))
            .map_err(|error| io_failure("file_permissions_failed", &error))?;
        verify_file(path)
    }

    pub(super) fn verify_file(path: &Path) -> Result<(), AppError> {
        let metadata = fs::symlink_metadata(path)
            .map_err(|error| io_failure("file_metadata_failed", &error))?;
        verify_owner(&metadata)?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(failure("file_expected"));
        }
        if metadata.permissions().mode() & 0o177 != 0 {
            return Err(failure("file_permissions_unsafe"));
        }
        Ok(())
    }

    pub(super) fn file_identity(path: &Path) -> Result<FileIdentity, AppError> {
        let metadata = fs::symlink_metadata(path)
            .map_err(|error| io_failure("file_identity_failed", &error))?;
        verify_owner(&metadata)?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(failure("file_expected"));
        }
        Ok(FileIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }

    pub(super) fn file_identity_from_file(file: &File) -> Result<FileIdentity, AppError> {
        let metadata = file
            .metadata()
            .map_err(|error| io_failure("file_identity_failed", &error))?;
        Ok(FileIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }

    pub(super) fn replace_file(source: &Path, destination: &Path) -> Result<(), AppError> {
        fs::rename(source, destination).map_err(|error| io_failure("atomic_replace_failed", &error))
    }

    pub(super) fn sync_parent(parent: &Path) -> Result<(), AppError> {
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| io_failure("directory_sync_failed", &error))
    }
}

#[cfg(windows)]
mod platform {
    use super::{failure, io_failure};
    use crate::error::AppError;
    use std::ffi::c_void;
    use std::fs::{self, File, Metadata, OpenOptions};
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    use std::os::windows::io::AsRawHandle;
    use std::path::Path;
    use std::ptr;

    const OWNER_GROUP_DACL: u32 = 0x1 | 0x2 | 0x4;
    const SE_FILE_OBJECT: u32 = 1;
    const SE_DACL_PROTECTED: u16 = 0x1000;
    const PROTECTED_DACL: u32 = 0x8000_0000;
    const UNPROTECTED_DACL: u32 = 0x2000_0000;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_ALL_ACCESS: u32 = 0x001f_01ff;
    const INHERITED_ACE: u8 = 0x10;
    const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
    const ACCESS_DENIED_ACE_TYPE: u8 = 1;
    const OBJECT_INHERIT_ACE: u32 = 0x1;
    const CONTAINER_INHERIT_ACE: u32 = 0x2;
    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
    const TOKEN_QUERY: u32 = 0x0008;
    const TOKEN_USER_CLASS: u32 = 1;
    const DELETE_ACCESS: u32 = 0x0001_0000;
    const WRITE_DAC: u32 = 0x0004_0000;
    const WRITE_OWNER: u32 = 0x0008_0000;
    const GENERIC_ALL: u32 = 0x1000_0000;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const DIRECTORY_WRITE_ACCESS: u32 = 0x0000_0156;
    const WIN_LOCAL_SYSTEM_SID: i32 = 22;
    const WIN_BUILTIN_ADMINISTRATORS_SID: i32 = 26;
    const FILE_SHARE_READ: u32 = 0x1;
    const FILE_SHARE_WRITE: u32 = 0x2;
    const FILE_SHARE_DELETE: u32 = 0x4;
    const OPEN_EXISTING: u32 = 3;

    #[repr(C)]
    struct AclHeader {
        revision: u8,
        sbz1: u8,
        size: u16,
        ace_count: u16,
        sbz2: u16,
    }

    #[repr(C)]
    struct AceHeader {
        ace_type: u8,
        ace_flags: u8,
        ace_size: u16,
    }

    #[repr(C)]
    struct SidAndAttributes {
        sid: *mut c_void,
        attributes: u32,
    }

    #[repr(C)]
    struct TokenUser {
        user: SidAndAttributes,
    }

    #[repr(C)]
    struct FileTime {
        low: u32,
        high: u32,
    }

    #[repr(C)]
    struct ByHandleFileInformation {
        file_attributes: u32,
        creation_time: FileTime,
        last_access_time: FileTime,
        last_write_time: FileTime,
        volume_serial_number: u32,
        file_size_high: u32,
        file_size_low: u32,
        number_of_links: u32,
        file_index_high: u32,
        file_index_low: u32,
    }

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
        fn IsWellKnownSid(sid: *const c_void, sid_type: i32) -> i32;
        fn GetAce(acl: *const c_void, index: u32, ace: *mut *mut c_void) -> i32;
        fn GetLengthSid(sid: *const c_void) -> u32;
        fn InitializeAcl(acl: *mut c_void, length: u32, revision: u32) -> i32;
        fn AddAccessAllowedAceEx(
            acl: *mut c_void,
            revision: u32,
            flags: u32,
            mask: u32,
            sid: *const c_void,
        ) -> i32;
        fn OpenProcessToken(
            process: *mut c_void,
            desired_access: u32,
            token: *mut *mut c_void,
        ) -> i32;
        fn GetTokenInformation(
            token: *mut c_void,
            information_class: u32,
            information: *mut c_void,
            information_length: u32,
            return_length: *mut u32,
        ) -> i32;
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn LocalFree(memory: *mut c_void) -> *mut c_void;
        fn MoveFileExW(existing: *const u16, replacement: *const u16, flags: u32) -> i32;
        fn GetCurrentProcess() -> *mut c_void;
        fn CloseHandle(handle: *mut c_void) -> i32;
        fn CreateFileW(
            name: *const u16,
            desired_access: u32,
            share_mode: u32,
            security_attributes: *mut c_void,
            creation_disposition: u32,
            flags_and_attributes: u32,
            template: *mut c_void,
        ) -> *mut c_void;
        fn GetFileInformationByHandle(
            handle: *mut c_void,
            information: *mut ByHandleFileInformation,
        ) -> i32;
    }

    struct TokenHandle(*mut c_void);

    impl Drop for TokenHandle {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: OpenProcessToken returned this owned handle.
                unsafe {
                    CloseHandle(self.0);
                }
            }
        }
    }

    fn owner_is_current(owner: *const c_void) -> Result<bool, AppError> {
        let mut token = ptr::null_mut();
        // SAFETY: the pseudo-process handle is valid and the output is writable.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0
            || token.is_null()
        {
            return Err(failure("current_owner_unavailable"));
        }
        let token = TokenHandle(token);
        let mut required = 0u32;
        // SAFETY: the first call intentionally supplies no buffer to obtain size.
        unsafe {
            GetTokenInformation(token.0, TOKEN_USER_CLASS, ptr::null_mut(), 0, &mut required);
        }
        if required == 0 {
            return Err(failure("current_owner_unavailable"));
        }
        let words =
            (required as usize + std::mem::size_of::<usize>() - 1) / std::mem::size_of::<usize>();
        let mut buffer = vec![0usize; words];
        // SAFETY: the aligned buffer has the exact byte capacity Windows asked for.
        let ok = unsafe {
            GetTokenInformation(
                token.0,
                TOKEN_USER_CLASS,
                buffer.as_mut_ptr() as *mut c_void,
                required,
                &mut required,
            )
        };
        if ok == 0 {
            return Err(failure("current_owner_unavailable"));
        }
        // SAFETY: a successful TOKEN_USER result begins with TOKEN_USER.
        let current = unsafe { (*(buffer.as_ptr() as *const TokenUser)).user.sid };
        if current.is_null() || unsafe { IsValidSid(current) } == 0 {
            return Err(failure("current_owner_unavailable"));
        }
        // SAFETY: both SIDs have been validated.
        Ok(unsafe { EqualSid(owner, current) != 0 })
    }

    pub(super) struct WindowsPermissions {
        descriptor: *mut c_void,
        owner: *mut c_void,
        group: *mut c_void,
        dacl: *mut c_void,
        control: u16,
    }

    impl Drop for WindowsPermissions {
        fn drop(&mut self) {
            if !self.descriptor.is_null() {
                // SAFETY: GetNamedSecurityInfoW returns one LocalAlloc buffer.
                unsafe {
                    LocalFree(self.descriptor);
                }
            }
        }
    }

    impl WindowsPermissions {
        fn read(path: &Path) -> Result<Self, AppError> {
            let name: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
            let mut permissions = Self {
                descriptor: ptr::null_mut(),
                owner: ptr::null_mut(),
                group: ptr::null_mut(),
                dacl: ptr::null_mut(),
                control: 0,
            };
            // SAFETY: the UTF-16 path is terminated and all outputs are valid.
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
                return Err(failure("acl_unavailable"));
            }
            let mut revision = 0;
            // SAFETY: all pointers come from the successful Windows call.
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
                return Err(failure("acl_invalid"));
            }
            if !owner_is_current(permissions.owner)? {
                return Err(failure("owner_mismatch"));
            }
            Ok(permissions)
        }

        fn owner_only(&self) -> bool {
            if self.control & SE_DACL_PROTECTED == 0 {
                return false;
            }
            // SAFETY: read() validated the ACL and owner SID. GetAce bounds
            // each returned pointer by the ACL's validated ACE count.
            unsafe {
                let header = &*(self.dacl as *const AclHeader);
                for index in 0..header.ace_count as u32 {
                    let mut raw_ace = ptr::null_mut();
                    if GetAce(self.dacl, index, &mut raw_ace) == 0 || raw_ace.is_null() {
                        return false;
                    }
                    let ace = &*(raw_ace as *const AceHeader);
                    if ace.ace_flags & INHERITED_ACE != 0 {
                        return false;
                    }
                    match ace.ace_type {
                        ACCESS_ALLOWED_ACE_TYPE => {
                            let sid = (raw_ace as *const u8).add(8) as *const c_void;
                            if IsValidSid(sid) == 0 || EqualSid(self.owner, sid) == 0 {
                                return false;
                            }
                        }
                        ACCESS_DENIED_ACE_TYPE => {}
                        _ => return false,
                    }
                }
            }
            true
        }

        fn external_parent_safe(&self) -> bool {
            let dangerous = DELETE_ACCESS
                | WRITE_DAC
                | WRITE_OWNER
                | GENERIC_ALL
                | GENERIC_WRITE
                | DIRECTORY_WRITE_ACCESS;
            // SAFETY: read() validated the ACL, owner SID, and ACE count.
            unsafe {
                let header = &*(self.dacl as *const AclHeader);
                for index in 0..header.ace_count as u32 {
                    let mut raw_ace = ptr::null_mut();
                    if GetAce(self.dacl, index, &mut raw_ace) == 0 || raw_ace.is_null() {
                        return false;
                    }
                    let ace = &*(raw_ace as *const AceHeader);
                    match ace.ace_type {
                        ACCESS_ALLOWED_ACE_TYPE => {
                            let mask =
                                ptr::read_unaligned((raw_ace as *const u8).add(4) as *const u32);
                            let sid = (raw_ace as *const u8).add(8) as *const c_void;
                            if IsValidSid(sid) == 0 {
                                return false;
                            }
                            let trusted_writer = EqualSid(self.owner, sid) != 0
                                || IsWellKnownSid(sid, WIN_LOCAL_SYSTEM_SID) != 0
                                || IsWellKnownSid(sid, WIN_BUILTIN_ADMINISTRATORS_SID) != 0;
                            if !trusted_writer && mask & dangerous != 0 {
                                return false;
                            }
                        }
                        ACCESS_DENIED_ACE_TYPE => {}
                        _ => return false,
                    }
                }
            }
            true
        }

        fn apply(&self, path: &Path) -> Result<(), AppError> {
            let mut name: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
            let protection = if self.control & SE_DACL_PROTECTED != 0 {
                PROTECTED_DACL
            } else {
                UNPROTECTED_DACL
            };
            // SAFETY: descriptor-owned pointers remain valid for this call.
            let result = unsafe {
                SetNamedSecurityInfoW(
                    name.as_mut_ptr(),
                    SE_FILE_OBJECT,
                    OWNER_GROUP_DACL | protection,
                    self.owner,
                    self.group,
                    self.dacl,
                    ptr::null_mut(),
                )
            };
            if result != 0 {
                return Err(failure("acl_apply_failed"));
            }
            let actual = Self::read(path)?;
            if !self.matches(&actual) || !actual.owner_only() {
                return Err(failure("acl_verification_failed"));
            }
            Ok(())
        }

        fn matches(&self, other: &Self) -> bool {
            // SAFETY: validated ACLs expose an in-bounds size in their header.
            unsafe {
                let self_size = (*(self.dacl as *const AclHeader)).size as usize;
                let other_size = (*(other.dacl as *const AclHeader)).size as usize;
                EqualSid(self.owner, other.owner) != 0
                    && EqualSid(self.group, other.group) != 0
                    && (self.control & SE_DACL_PROTECTED) == (other.control & SE_DACL_PROTECTED)
                    && std::slice::from_raw_parts(self.dacl as *const u8, self_size)
                        == std::slice::from_raw_parts(other.dacl as *const u8, other_size)
            }
        }
    }

    pub(super) struct FilePermissions {
        acl: WindowsPermissions,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(super) struct FileIdentity {
        volume_serial_number: u32,
        file_index: u64,
    }

    fn restrict_to_owner_with_mask(
        path: &Path,
        directory: bool,
        access_mask: u32,
    ) -> Result<(), AppError> {
        let permissions = WindowsPermissions::read(path)?;
        let acl_size = unsafe { 8 + 8 + GetLengthSid(permissions.owner) };
        let mut acl = vec![0u32; ((acl_size + 3) / 4) as usize];
        let acl_pointer = acl.as_mut_ptr() as *mut c_void;
        let mut name: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let inheritance = if directory {
            OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE
        } else {
            0
        };
        // SAFETY: the aligned buffer fits one ACCESS_ALLOWED_ACE plus the SID.
        let ok = unsafe {
            InitializeAcl(acl_pointer, acl_size, 2) != 0
                && AddAccessAllowedAceEx(
                    acl_pointer,
                    2,
                    inheritance,
                    access_mask,
                    permissions.owner,
                ) != 0
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
            return Err(failure("acl_initialization_failed"));
        }
        let actual = WindowsPermissions::read(path)?;
        if !actual.owner_only() {
            return Err(failure("acl_verification_failed"));
        }
        Ok(())
    }

    fn restrict_to_owner(path: &Path, directory: bool) -> Result<(), AppError> {
        restrict_to_owner_with_mask(path, directory, FILE_ALL_ACCESS)
    }

    #[cfg(test)]
    pub(super) fn set_tighter_owner_access_for_test(path: &Path) -> Result<(), AppError> {
        // FILE_GENERIC_READ | FILE_GENERIC_WRITE, deliberately excluding
        // delete/ownership/DACL rights while retaining normal config access.
        restrict_to_owner_with_mask(path, false, 0x0012_019f)
    }

    #[cfg(test)]
    pub(super) fn capture_permissions_for_test(path: &Path) -> Result<FilePermissions, AppError> {
        secure_existing_file(path)
    }

    #[cfg(test)]
    pub(super) fn permissions_match_for_test(
        expected: &FilePermissions,
        path: &Path,
    ) -> Result<bool, AppError> {
        Ok(expected.acl.matches(&WindowsPermissions::read(path)?))
    }

    pub(super) fn is_linked(metadata: &Metadata) -> bool {
        metadata.file_type().is_symlink()
            || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }

    pub(super) fn secure_directory(path: &Path, _existed: bool) -> Result<(), AppError> {
        let permissions = WindowsPermissions::read(path)?;
        if !permissions.owner_only() {
            restrict_to_owner(path, true)?;
        }
        Ok(())
    }

    pub(super) fn verify_directory(path: &Path) -> Result<(), AppError> {
        let metadata = fs::symlink_metadata(path)
            .map_err(|error| io_failure("directory_metadata_failed", &error))?;
        if !metadata.is_dir() || is_linked(&metadata) {
            return Err(failure("directory_expected"));
        }
        if !WindowsPermissions::read(path)?.owner_only() {
            return Err(failure("directory_permissions_unsafe"));
        }
        Ok(())
    }

    pub(super) fn verify_external_directory(path: &Path) -> Result<(), AppError> {
        let metadata = fs::symlink_metadata(path)
            .map_err(|error| io_failure("directory_metadata_failed", &error))?;
        if !metadata.is_dir() || is_linked(&metadata) {
            return Err(failure("directory_expected"));
        }
        if !WindowsPermissions::read(path)?.external_parent_safe() {
            return Err(failure("external_directory_write_unsafe"));
        }
        Ok(())
    }

    pub(super) fn configure_private_open(options: &mut OpenOptions) {
        options
            .share_mode(0)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }

    pub(super) fn secure_new_file(path: &Path) -> Result<(), AppError> {
        restrict_to_owner(path, false)?;
        verify_file(path)
    }

    pub(super) fn secure_existing_file(path: &Path) -> Result<FilePermissions, AppError> {
        let permissions = WindowsPermissions::read(path)?;
        if !permissions.owner_only() {
            restrict_to_owner(path, false)?;
        }
        let acl = WindowsPermissions::read(path)?;
        if !acl.owner_only() {
            return Err(failure("file_permissions_unsafe"));
        }
        Ok(FilePermissions { acl })
    }

    pub(super) fn capture_safe_file_permissions(
        path: &Path,
    ) -> Result<Option<FilePermissions>, AppError> {
        let acl = WindowsPermissions::read(path)?;
        if acl.owner_only() {
            Ok(Some(FilePermissions { acl }))
        } else {
            Ok(None)
        }
    }

    pub(super) fn apply_file_permissions(
        permissions: &FilePermissions,
        path: &Path,
    ) -> Result<(), AppError> {
        permissions.acl.apply(path)
    }

    pub(super) fn verify_file(path: &Path) -> Result<(), AppError> {
        let metadata = fs::symlink_metadata(path)
            .map_err(|error| io_failure("file_metadata_failed", &error))?;
        if !metadata.is_file() || is_linked(&metadata) {
            return Err(failure("file_expected"));
        }
        if !WindowsPermissions::read(path)?.owner_only() {
            return Err(failure("file_permissions_unsafe"));
        }
        Ok(())
    }

    fn identity_from_handle(handle: *mut c_void) -> Result<FileIdentity, AppError> {
        let mut information = std::mem::MaybeUninit::<ByHandleFileInformation>::uninit();
        // SAFETY: the handle is live and the output points to writable storage.
        if unsafe { GetFileInformationByHandle(handle, information.as_mut_ptr()) } == 0 {
            let error = std::io::Error::last_os_error();
            return Err(io_failure("file_identity_failed", &error));
        }
        // SAFETY: the successful Windows call initialized every field.
        let information = unsafe { information.assume_init() };
        if information.file_attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(failure("linked_path"));
        }
        Ok(FileIdentity {
            volume_serial_number: information.volume_serial_number,
            file_index: ((information.file_index_high as u64) << 32)
                | information.file_index_low as u64,
        })
    }

    pub(super) fn file_identity(path: &Path) -> Result<FileIdentity, AppError> {
        let name: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        // Desired access zero requests metadata only. OPEN_REPARSE_POINT makes
        // any last-component link observable instead of following it.
        let handle = unsafe {
            CreateFileW(
                name.as_ptr(),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                ptr::null_mut(),
                OPEN_EXISTING,
                FILE_FLAG_OPEN_REPARSE_POINT,
                ptr::null_mut(),
            )
        };
        if handle == (-1isize as *mut c_void) {
            let error = std::io::Error::last_os_error();
            return Err(io_failure("file_identity_failed", &error));
        }
        let owned = TokenHandle(handle);
        identity_from_handle(owned.0)
    }

    pub(super) fn file_identity_from_file(file: &File) -> Result<FileIdentity, AppError> {
        identity_from_handle(file.as_raw_handle() as *mut c_void)
    }

    pub(super) fn replace_file(source: &Path, destination: &Path) -> Result<(), AppError> {
        let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
        let destination: Vec<u16> = destination
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect();
        // SAFETY: both path buffers are immutable and NUL-terminated.
        let ok = unsafe {
            MoveFileExW(
                source.as_ptr(),
                destination.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if ok == 0 {
            let error = std::io::Error::last_os_error();
            return Err(io_failure("atomic_replace_failed", &error));
        }
        Ok(())
    }

    pub(super) fn sync_parent(_parent: &Path) -> Result<(), AppError> {
        // MOVEFILE_WRITE_THROUGH flushes the replacement metadata on Windows.
        Ok(())
    }
}

#[cfg(not(any(unix, windows)))]
compile_error!("AAG private storage requires an ACL-capable Unix or Windows target");

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(label: &str) -> Self {
            let mut nonce = [0u8; 8];
            getrandom::getrandom(&mut nonce).expect("test nonce");
            let path = std::env::temp_dir().join(format!(
                "aag_private_fs_{label}_{}_{}",
                std::process::id(),
                hex::encode(nonce)
            ));
            fs::create_dir(&path).expect("create test root");
            Self(path)
        }

        fn join(&self, value: &str) -> PathBuf {
            self.0.join(value)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn atomic_write_rejects_a_deterministic_temporary_collision() {
        let root = TestDir::new("collision");
        let directory = root.join("config");
        prepare_private_dir(&directory).unwrap();
        let target = directory.join("config.json");
        atomic_write(&target, b"original").unwrap();
        let collision = temporary_path(&target, "fixed").unwrap();
        fs::write(&collision, b"foreign").unwrap();

        let error = atomic_write_with_suffix(&target, b"replacement", None, "fixed", |_| Ok(()))
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("private_storage:temporary_collision"));
        assert_eq!(fs::read(&target).unwrap(), b"original");
        assert_eq!(fs::read(&collision).unwrap(), b"foreign");
    }

    #[test]
    fn failed_replace_cleans_only_the_owned_temporary_file() {
        let root = TestDir::new("cleanup");
        let directory = root.join("config");
        prepare_private_dir(&directory).unwrap();
        let target = directory.join("config.json");
        let error = atomic_write_with_suffix(&target, b"secret", None, "owned", |_| {
            fs::create_dir(&target)
                .map_err(|error| io_failure("test_target_create_failed", &error))?;
            Ok(())
        })
        .unwrap_err();
        assert!(error.to_string().contains("private_storage:target_changed"));
        assert!(!temporary_path(&target, "owned").unwrap().exists());
        assert!(target.is_dir());
    }

    #[test]
    fn backup_is_exact_private_and_does_not_create_a_backup_chain() {
        let root = TestDir::new("backup");
        let directory = root.join("config");
        prepare_private_dir(&directory).unwrap();
        let source = directory.join("source.json");
        let destination = directory.join("source.json.backup");
        atomic_write(&source, br#"{\"token\":\"private\"}"#).unwrap();
        backup_file(&source, &destination).unwrap();
        assert_eq!(fs::read(&destination).unwrap(), fs::read(&source).unwrap());
        platform::verify_file(&destination).unwrap();
        assert!(!directory.join("source.json.backup.backup").exists());
    }

    #[test]
    fn private_directories_reject_relative_current_and_root_targets() {
        let relative = PathBuf::from("aag-relative-state");
        assert!(prepare_private_dir(&relative)
            .unwrap_err()
            .to_string()
            .contains("directory_not_absolute"));

        let current = std::env::current_dir().unwrap();
        assert!(prepare_private_dir(&current)
            .unwrap_err()
            .to_string()
            .contains("directory_too_broad"));

        let root = current.ancestors().last().unwrap();
        assert!(prepare_private_dir(root)
            .unwrap_err()
            .to_string()
            .contains("directory_too_broad"));
    }

    #[test]
    fn existing_external_directory_is_rejected_without_permission_mutation() {
        let root = TestDir::new("external_directory");
        let directory = root.join("existing");
        fs::create_dir(&directory).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();
            let before = fs::metadata(&directory).unwrap().permissions().mode() & 0o777;
            assert!(prepare_private_dir(&directory).is_err());
            assert_eq!(
                fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
                before
            );
        }
        #[cfg(windows)]
        {
            assert!(prepare_private_dir(&directory).is_err());
        }
        assert!(!directory.join("secret.json").exists());
        prepare_managed_private_dir(&directory).unwrap();
        verify_private_dir(&directory).unwrap();
    }

    #[test]
    fn live_write_accepts_safe_external_parent_without_mutating_it() {
        let root = TestDir::new("safe_external_parent");
        let directory = root.join("client_config");
        fs::create_dir(&directory).unwrap();
        prepare_managed_private_dir(&directory).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();
        }
        #[cfg(unix)]
        let before = fs::metadata(&directory).unwrap().permissions();
        let target = directory.join("settings.json");
        atomic_write(&target, b"private client state").unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"private client state");
        verify_private_file(&target).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
                before.mode() & 0o777
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn live_write_rejects_group_writable_external_parent_without_mutation() {
        use std::os::unix::fs::PermissionsExt;

        let root = TestDir::new("unsafe_external_parent");
        let directory = root.join("client_config");
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o775)).unwrap();
        let target = directory.join("settings.json");
        assert!(atomic_write(&target, b"private client state")
            .unwrap_err()
            .to_string()
            .contains("external_directory_write_unsafe"));
        assert!(!target.exists());
        assert_eq!(
            fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o775
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_permissions_ignore_umask_and_preserve_tighter_files() {
        use std::os::unix::fs::PermissionsExt;

        extern "C" {
            fn umask(mask: u32) -> u32;
        }
        let root = TestDir::new("umask");
        fs::set_permissions(&root.0, fs::Permissions::from_mode(0o777)).unwrap();
        // SAFETY: serialized Rust tests restore the process umask below.
        let previous = unsafe { umask(0) };
        let directory = root.join("private");
        prepare_private_dir(&directory).unwrap();
        let file = directory.join("state.json");
        atomic_write(&file, b"one").unwrap();
        // SAFETY: restore the exact process state captured above.
        unsafe { umask(previous) };

        assert_eq!(
            fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::set_permissions(&file, fs::Permissions::from_mode(0o400)).unwrap();
        atomic_write(&file, b"two").unwrap();
        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o400
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_symlinked_config_directory_is_rejected() {
        use std::os::unix::fs::symlink;

        let root = TestDir::new("symlink");
        let real = root.join("real");
        fs::create_dir(&real).unwrap();
        let linked = root.join("linked");
        symlink(&real, &linked).unwrap();
        let error = atomic_write(&linked.join("secret.json"), b"secret").unwrap_err();
        assert!(error.to_string().contains("private_storage:linked_path"));
        assert!(!real.join("secret.json").exists());
    }

    #[cfg(windows)]
    #[test]
    fn windows_owner_only_dacl_is_applied_and_reparse_directory_is_rejected() {
        let root = TestDir::new("windows_acl");
        let directory = root.join("private");
        prepare_private_dir(&directory).unwrap();
        let file = directory.join("state.json");
        atomic_write(&file, b"secret").unwrap();
        platform::verify_directory(&directory).unwrap();
        platform::verify_file(&file).unwrap();

        let real = root.join("real");
        fs::create_dir(&real).unwrap();
        let linked = root.join("linked");
        let output = std::process::Command::new("cmd.exe")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(&linked)
            .arg(&real)
            .output()
            .expect("create junction");
        assert!(
            output.status.success(),
            "junction creation must be available"
        );
        let error = atomic_write(&linked.join("secret.json"), b"secret").unwrap_err();
        assert!(error.to_string().contains("private_storage:linked_path"));
        assert!(!real.join("secret.json").exists());
        fs::remove_dir(&linked).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_tighter_owner_dacl_survives_replacement_and_backup() {
        let root = TestDir::new("windows_tighter");
        let directory = root.join("private");
        prepare_private_dir(&directory).unwrap();
        let source = directory.join("state.json");
        let backup = directory.join("state.json.backup");
        atomic_write(&source, b"first secret").unwrap();
        platform::set_tighter_owner_access_for_test(&source).unwrap();
        let expected = platform::capture_permissions_for_test(&source).unwrap();

        backup_file(&source, &backup).unwrap();
        atomic_write(&source, b"second secret").unwrap();

        assert!(platform::permissions_match_for_test(&expected, &source).unwrap());
        assert!(platform::permissions_match_for_test(&expected, &backup).unwrap());
        assert_eq!(fs::read(&source).unwrap(), b"second secret");
        assert_eq!(fs::read(&backup).unwrap(), b"first secret");
    }
}
