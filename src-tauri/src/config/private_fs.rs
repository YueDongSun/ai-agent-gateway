//! Private filesystem boundary for credential-bearing AAG state.
//!
//! This module deliberately uses only operating-system facilities already
//! available to the application. It does not encrypt data at rest: callers
//! must still prefer secret references over copied plaintext credentials.

use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use crate::error::AppError;

mod directory_ops;
pub(crate) use directory_ops::{inspect_directory_object, rename_directory_object_no_replace};
#[cfg(windows)]
mod namespace_lease;
#[cfg(windows)]
pub(crate) use namespace_lease::NamespaceLease;
mod inventory_io;
pub(crate) use inventory_io::{directory_inventory_entries, inventory_object_metadata, open_inventory_stream, Inventory_object_metadata};
#[cfg(windows)]
pub(crate) use inventory_io::{inventory_object_metadata_from_file, open_inventory_reparse_mutation_object, open_inventory_security_object};
#[cfg(all(test, windows))]
pub(crate) use inventory_io::open_inventory_fixture_empty_directory_for_reparse;
#[cfg(all(test, windows))]
pub(crate) use inventory_io::diagnose_inventory_security_object_open;

const ERROR_PREFIX: &str = "private_storage";

/// Durable metadata identity used by the migration journal. Inspecting an
/// identity does not adopt a directory or change its owner or permissions.
#[allow(non_camel_case_types)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Managed_directory_identity {
    pub(crate) volume: u64,
    pub(crate) object: u64,
}

#[cfg(windows)]
pub(crate) mod windows_path;

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
        platform::create_directory_tree(path)?;
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

/// Create one new private directory. An existing path always fails without
/// changing its ownership, permissions, or contents.
pub(crate) fn create_private_dir(path: &Path) -> Result<(), AppError> {
    reject_broad_private_dir(path)?;
    reject_linked_existing_components(path)?;
    platform::create_directory(path)?;
    platform::verify_directory(path)
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
        platform::create_directory_tree(path)?;
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
            let file = platform::create_new_file(path, "file_create_failed")?;
            let identity = platform::file_identity_from_file(&file)?;
            if let Err(error) = platform::secure_new_file(path) {
                drop(file);
                cleanup_owned_temporary(path, &identity);
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

#[cfg(unix)]
pub(crate) fn validate_mirror_parent(path: &Path) -> Result<(), AppError> {
    validate_external_file_path(path)?;
    let parent = path.parent().ok_or_else(|| failure("parent_missing"))?;
    platform::verify_directory(parent)
}

pub(crate) fn verify_private_file(path: &Path) -> Result<(), AppError> {
    platform::verify_file(path)
}

fn random_suffix() -> Result<String, AppError> {
    let mut nonce = [0u8; 16];
    getrandom::getrandom(&mut nonce).map_err(|_| failure("random_unavailable"))?;
    Ok(hex::encode(nonce))
}

/// An exclusively created private temporary directory. Cleanup only applies
/// while the original, non-linked directory still occupies the owned path.
#[derive(Debug)]
pub(crate) struct PrivateTempDir {
    path: PathBuf,
    identity: platform::FileIdentity,
}

impl PrivateTempDir {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for PrivateTempDir {
    fn drop(&mut self) {
        let Ok(Some(current)) = metadata(&self.path) else {
            return;
        };
        if current.is_dir()
            && platform::directory_identity(&self.path)
                .map(|identity| identity == self.identity)
                .unwrap_or(false)
        {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

pub(crate) fn private_temp_dir(parent: &Path) -> Result<PrivateTempDir, AppError> {
    let path = parent.join(format!("aag_launcher_{}", random_suffix()?));
    reject_broad_private_dir(&path)?;
    reject_linked_existing_components(&path)?;
    // This is exclusive creation: an existing path is never adopted or removed.
    platform::create_directory(&path)?;
    let identity = platform::directory_identity(&path)?;
    let directory = PrivateTempDir { path, identity };
    platform::verify_directory(directory.path())?;
    Ok(directory)
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
    let mut file = Some(platform::create_new_file(
        &temporary,
        "temporary_create_failed",
    )?);
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

/// An admitted version of a managed mirror file. Native desktop clients can
/// replace these files using their token's default owner on Windows; this
/// exception is confined to a strictly private, current-user-owned parent.
pub(crate) struct MirrorFileSnapshot {
    pub(crate) contents: Option<Vec<u8>>,
    /// A private prior object retained if cleanup failed after installation.
    pub(crate) recovery_path: Option<PathBuf>,
    parent_identity: platform::FileIdentity,
    identity: Option<platform::FileIdentity>,
    permissions: Option<platform::MirrorPermissions>,
}

pub(crate) fn read_mirror_file(path: &Path) -> Result<MirrorFileSnapshot, AppError> {
    validate_external_file_path(path)?;
    let parent = path.parent().ok_or_else(|| failure("parent_missing"))?;
    platform::verify_directory(parent)?;
    let parent_identity = platform::directory_identity(parent)?;
    let (identity, permissions, contents) = match metadata(path)? {
        Some(current) => {
            if !current.is_file() {
                return Err(failure("file_expected"));
            }
            let identity = platform::file_identity(path)?;
            let permissions = platform::mirror_file_permissions(path)?;
            let contents =
                fs::read(path).map_err(|error| io_failure("mirror_read_failed", &error))?;
            (Some(identity), Some(permissions), Some(contents))
        }
        None => (None, None, None),
    };
    let snapshot = MirrorFileSnapshot {
        contents,
        recovery_path: None,
        parent_identity,
        identity,
        permissions,
    };
    verify_mirror_snapshot(path, &snapshot)?;
    Ok(snapshot)
}

pub(crate) fn verify_mirror_snapshot(
    path: &Path,
    expected: &MirrorFileSnapshot,
) -> Result<(), AppError> {
    validate_external_file_path(path)?;
    let parent = path.parent().ok_or_else(|| failure("parent_missing"))?;
    platform::verify_directory(parent)?;
    if platform::directory_identity(parent)? != expected.parent_identity {
        return Err(failure("mirror_parent_changed"));
    }
    match (
        metadata(path)?,
        expected.identity,
        &expected.permissions,
        &expected.contents,
    ) {
        (None, None, None, None) => Ok(()),
        (Some(current), Some(identity), Some(permissions), Some(contents)) if current.is_file() => {
            if platform::file_identity(path)? != identity {
                return Err(failure("target_changed"));
            }
            let actual = platform::mirror_file_permissions(path)?;
            if !platform::mirror_permissions_match(permissions, &actual)
                || fs::read(path).map_err(|error| io_failure("mirror_read_failed", &error))?
                    != *contents
                || platform::file_identity(path)? != identity
            {
                return Err(failure("target_changed"));
            }
            Ok(())
        }
        _ => Err(failure("target_changed")),
    }
}

/// Persist the exact admitted prior bytes to an exclusive private backup.
/// No existing backup can be overwritten, and failed cleanup is identity-bound.
#[cfg(any(windows, test))]
pub(crate) fn backup_mirror_file(
    snapshot: &MirrorFileSnapshot,
    destination: &Path,
) -> Result<(), AppError> {
    let contents = snapshot
        .contents
        .as_ref()
        .ok_or_else(|| failure("backup_source_missing"))?;
    validate_external_file_path(destination)?;
    let parent = destination
        .parent()
        .ok_or_else(|| failure("parent_missing"))?;
    prepare_external_parent_dir(parent)?;
    let mut file = platform::create_new_file(destination, "backup_create_failed")?;
    let identity = platform::file_identity_from_file(&file)?;
    let result = (|| {
        platform::secure_new_file(destination)?;
        file.write_all(contents)
            .map_err(|error| io_failure("backup_write_failed", &error))?;
        file.sync_all()
            .map_err(|error| io_failure("backup_sync_failed", &error))?;
        platform::verify_file(destination)
    })();
    drop(file);
    if result.is_err() {
        cleanup_owned_temporary(destination, &identity);
    }
    result
}

#[cfg(all(unix, test))]
pub(crate) fn replace_mirror_file(
    path: &Path,
    contents: &[u8],
    expected: &MirrorFileSnapshot,
) -> Result<MirrorFileSnapshot, AppError> {
    verify_mirror_snapshot(path, expected)?;
    let mut replacement_identity = None;
    let permissions = platform::mirror_replacement_permissions(expected.permissions.as_ref());
    atomic_write_with_suffix(
        path,
        contents,
        permissions,
        &random_suffix()?,
        |temporary| {
            verify_mirror_snapshot(path, expected)?;
            replacement_identity = Some(platform::file_identity(temporary)?);
            Ok(())
        },
    )?;
    let written = read_mirror_file(path)?;
    if written.identity != replacement_identity || written.contents.as_deref() != Some(contents) {
        return Err(failure("target_changed"));
    }
    Ok(written)
}

#[cfg(all(unix, test))]
pub(crate) fn remove_mirror_file(
    path: &Path,
    expected: &MirrorFileSnapshot,
) -> Result<(), AppError> {
    verify_mirror_snapshot(path, expected)?;
    fs::remove_file(path).map_err(|error| io_failure("mirror_remove_failed", &error))
}

#[cfg(windows)]
struct MirrorTemporary {
    file: fs::File,
    installed: bool,
}

#[cfg(windows)]
impl Drop for MirrorTemporary {
    fn drop(&mut self) {
        if !self.installed {
            // Delete only the object represented by our exclusive creation handle.
            let _ = platform::delete_mirror_handle(&self.file);
        }
    }
}

#[cfg(windows)]
fn verify_mirror_parent_identity(
    path: &Path,
    expected: &MirrorFileSnapshot,
) -> Result<(), AppError> {
    validate_external_file_path(path)?;
    let parent = path.parent().ok_or_else(|| failure("parent_missing"))?;
    platform::verify_directory(parent)?;
    if platform::directory_identity(parent)? != expected.parent_identity {
        return Err(failure("mirror_parent_changed"));
    }
    Ok(())
}

#[cfg(windows)]
fn verify_locked_mirror(
    path: &Path,
    file: &mut fs::File,
    expected: &MirrorFileSnapshot,
) -> Result<(), AppError> {
    verify_mirror_parent_identity(path, expected)?;
    let actual_permissions = platform::mirror_handle_permissions(file)?;
    if Some(platform::file_identity_from_file(file)?) != expected.identity
        || !expected.permissions.as_ref().is_some_and(|permissions| {
            platform::mirror_permissions_match(permissions, &actual_permissions)
        })
        || Some(platform::read_mirror_handle(file)?) != expected.contents
    {
        return Err(failure("target_changed"));
    }
    Ok(())
}

/// Hold the admitted object against writes/deletion, rename that handle aside,
/// then atomically install the completed new object with NO_REPLACE. The two
/// renames have a brief path gap: a client creating a file in that gap wins and
/// is never overwritten. Failed restoration retains the private recovery hold.
#[cfg(windows)]
fn replace_mirror_file_with_hook(
    path: &Path,
    contents: &[u8],
    expected: &MirrorFileSnapshot,
    after_hold: impl FnOnce() -> Result<(), AppError>,
) -> Result<MirrorFileSnapshot, AppError> {
    verify_mirror_snapshot(path, expected)?;
    let new_path = temporary_path(path, &format!("mirror_{}", random_suffix()?))?;
    let hold_path = temporary_path(path, &format!("recovery_{}", random_suffix()?))?;
    let mut temporary = MirrorTemporary {
        file: platform::create_mirror_temporary(&new_path)?,
        installed: false,
    };
    temporary
        .file
        .write_all(contents)
        .map_err(|error| io_failure("temporary_write_failed", &error))?;
    temporary
        .file
        .sync_all()
        .map_err(|error| io_failure("temporary_sync_failed", &error))?;
    // Build the receipt from the locked, fully written new object before commit.
    // There are no fallible path reads after successful installation.
    let mut written = MirrorFileSnapshot {
        contents: Some(contents.to_vec()),
        recovery_path: None,
        parent_identity: expected.parent_identity,
        identity: Some(platform::file_identity_from_file(&temporary.file)?),
        permissions: Some(platform::mirror_handle_permissions(&temporary.file)?),
    };
    let mut original = if expected.identity.is_some() {
        let mut original = platform::open_locked_mirror(path)?;
        verify_locked_mirror(path, &mut original, expected)?;
        platform::rename_mirror_handle(&original, &hold_path)?;
        Some(original)
    } else {
        verify_mirror_snapshot(path, expected)?;
        None
    };

    let installed = after_hold()
        .and_then(|_| verify_mirror_parent_identity(path, expected))
        .and_then(|_| platform::rename_mirror_handle(&temporary.file, path));
    if let Err(error) = installed {
        if let Some(original) = &mut original {
            // NO_REPLACE applies to restoration as well. A concurrent client
            // object is preserved and the admitted old object stays in the hold.
            if verify_mirror_parent_identity(path, expected).is_err()
                || platform::rename_mirror_handle(original, path).is_err()
            {
                return Err(failure("mirror_recovery_retained"));
            }
        }
        return Err(error);
    }
    temporary.installed = true;
    if let Some(original) = original {
        if platform::delete_mirror_handle(&original).is_err() {
            // Installation succeeded. Surface this retained recovery object as
            // a backup instead of misreporting the committed write as failure.
            written.recovery_path = Some(hold_path);
        }
    }
    Ok(written)
}

#[cfg(windows)]
pub(crate) fn replace_mirror_file(
    path: &Path,
    contents: &[u8],
    expected: &MirrorFileSnapshot,
) -> Result<MirrorFileSnapshot, AppError> {
    replace_mirror_file_with_hook(path, contents, expected, || Ok(()))
}

#[cfg(windows)]
pub(crate) fn remove_mirror_file(
    path: &Path,
    expected: &MirrorFileSnapshot,
) -> Result<(), AppError> {
    let mut file = platform::open_locked_mirror(path)?;
    verify_locked_mirror(path, &mut file, expected)?;
    platform::delete_mirror_handle(&file)
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
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
    use std::path::Path;

    #[derive(Clone, Copy)]
    pub(super) struct FilePermissions {
        mode: u32,
    }

    pub(super) type MirrorPermissions = FilePermissions;

    pub(super) fn mirror_file_permissions(path: &Path) -> Result<MirrorPermissions, AppError> {
        let metadata = fs::symlink_metadata(path)
            .map_err(|error| io_failure("file_metadata_failed", &error))?;
        verify_owner(&metadata)?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(failure("file_expected"));
        }
        Ok(FilePermissions {
            mode: metadata.permissions().mode() & 0o777,
        })
    }

    pub(super) fn mirror_permissions_match(
        expected: &MirrorPermissions,
        actual: &MirrorPermissions,
    ) -> bool {
        expected.mode == actual.mode
    }

    #[cfg(test)]
    pub(super) fn mirror_replacement_permissions(
        permissions: Option<&MirrorPermissions>,
    ) -> Option<FilePermissions> {
        permissions.copied()
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(super) struct FileIdentity {
        device: u64,
        inode: u64,
    }

    pub(super) fn durable_identity(identity: FileIdentity) -> super::Managed_directory_identity {
        super::Managed_directory_identity {
            volume: identity.device,
            object: identity.inode,
        }
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

    pub(super) fn create_directory(path: &Path) -> Result<(), AppError> {
        fs::DirBuilder::new()
            .mode(0o700)
            .create(path)
            .map_err(|error| io_failure("directory_create_failed", &error))
    }

    pub(super) fn create_directory_tree(path: &Path) -> Result<(), AppError> {
        fs::create_dir_all(path).map_err(|error| io_failure("directory_create_failed", &error))
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

    pub(super) fn create_new_file(path: &Path, error_code: &str) -> Result<File, AppError> {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .map_err(|error| io_failure(error_code, &error))
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

    pub(super) fn directory_identity(path: &Path) -> Result<FileIdentity, AppError> {
        let metadata = fs::symlink_metadata(path)
            .map_err(|error| io_failure("directory_identity_failed", &error))?;
        verify_owner(&metadata)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(failure("directory_expected"));
        }
        Ok(FileIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }

    pub(super) fn sync_parent(parent: &Path) -> Result<(), AppError> {
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| io_failure("directory_sync_failed", &error))
    }
}

#[cfg(windows)]
mod platform {
    use super::windows_path::encode_native_path;
    use super::{failure, io_failure};
    use crate::error::AppError;
    use std::ffi::c_void;
    use std::fs::{self, File, Metadata};
    use std::io::{Read, Seek, SeekFrom};
    use std::os::windows::fs::MetadataExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use std::path::Path;
    use std::ptr;

    const OWNER_GROUP_DACL: u32 = 0x1 | 0x2 | 0x4;
    const SE_FILE_OBJECT: u32 = 1;
    const SE_DACL_PROTECTED: u16 = 0x1000;
    const PROTECTED_DACL: u32 = 0x8000_0000;
    const UNPROTECTED_DACL: u32 = 0x2000_0000;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;
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
    const TOKEN_OWNER_CLASS: u32 = 4;
    const DELETE_ACCESS: u32 = 0x0001_0000;
    const WRITE_DAC: u32 = 0x0004_0000;
    const WRITE_OWNER: u32 = 0x0008_0000;
    const GENERIC_ALL: u32 = 0x1000_0000;
    const GENERIC_READ: u32 = 0x8000_0000;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const DIRECTORY_WRITE_ACCESS: u32 = 0x0000_0156;
    const WIN_LOCAL_SYSTEM_SID: i32 = 22;
    const WIN_BUILTIN_ADMINISTRATORS_SID: i32 = 26;
    const FILE_SHARE_READ: u32 = 0x1;
    const FILE_SHARE_WRITE: u32 = 0x2;
    const FILE_SHARE_DELETE: u32 = 0x4;
    const OPEN_EXISTING: u32 = 3;
    const CREATE_NEW: u32 = 1;

    #[repr(C)]
    struct SecurityDescriptor {
        revision: u8,
        sbz1: u8,
        control: u16,
        owner: *mut c_void,
        group: *mut c_void,
        sacl: *mut c_void,
        dacl: *mut c_void,
    }

    #[repr(C)]
    struct SecurityAttributes {
        length: u32,
        descriptor: *mut c_void,
        inherit_handle: i32,
    }

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

    #[repr(C)]
    struct FileRenameInformation {
        replace_if_exists: u32,
        root_directory: *mut c_void,
        name_length: u32,
        name: [u16; 1],
    }

    #[link(name = "advapi32")]
    extern "system" {
        fn GetSecurityInfo(
            handle: *mut c_void,
            object_type: u32,
            information: u32,
            owner: *mut *mut c_void,
            group: *mut *mut c_void,
            dacl: *mut *mut c_void,
            sacl: *mut *mut c_void,
            descriptor: *mut *mut c_void,
        ) -> u32;
        fn InitializeSecurityDescriptor(descriptor: *mut c_void, revision: u32) -> i32;
        fn SetSecurityDescriptorOwner(
            descriptor: *mut c_void,
            owner: *mut c_void,
            defaulted: i32,
        ) -> i32;
        fn SetSecurityDescriptorDacl(
            descriptor: *mut c_void,
            present: i32,
            dacl: *mut c_void,
            defaulted: i32,
        ) -> i32;
        fn SetSecurityDescriptorControl(
            descriptor: *mut c_void,
            bits_of_interest: u16,
            bits_to_set: u16,
        ) -> i32;
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
        #[cfg(test)]
        fn CreateWellKnownSid(
            sid_type: i32,
            domain: *const c_void,
            sid: *mut c_void,
            size: *mut u32,
        ) -> i32;
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
        fn CreateDirectoryW(name: *const u16, security_attributes: *mut c_void) -> i32;
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
        fn SetFileInformationByHandle(
            handle: *mut c_void,
            information_class: u32,
            information: *const c_void,
            size: u32,
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

    fn current_user_information() -> Result<Vec<usize>, AppError> {
        current_token_sid_information(TOKEN_USER_CLASS)
    }

    fn current_token_sid_information(information_class: u32) -> Result<Vec<usize>, AppError> {
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
            GetTokenInformation(
                token.0,
                information_class,
                ptr::null_mut(),
                0,
                &mut required,
            );
        }
        if required == 0 {
            return Err(failure("current_owner_unavailable"));
        }
        let words = (required as usize).div_ceil(std::mem::size_of::<usize>());
        let mut buffer = vec![0usize; words];
        // SAFETY: the aligned buffer has the exact byte capacity Windows asked for.
        let ok = unsafe {
            GetTokenInformation(
                token.0,
                information_class,
                buffer.as_mut_ptr() as *mut c_void,
                required,
                &mut required,
            )
        };
        if ok == 0 {
            return Err(failure("current_owner_unavailable"));
        }
        // SAFETY: TOKEN_USER and TOKEN_OWNER both begin with the requested SID pointer.
        let current = unsafe { *(buffer.as_ptr() as *const *mut c_void) };
        if current.is_null() || unsafe { IsValidSid(current) } == 0 {
            return Err(failure("current_owner_unavailable"));
        }
        Ok(buffer)
    }

    fn owner_is_current(owner: *const c_void) -> Result<bool, AppError> {
        let buffer = current_user_information()?;
        // SAFETY: current_user_information validated the returned TOKEN_USER SID.
        let current = unsafe { (*(buffer.as_ptr() as *const TokenUser)).user.sid };
        // SAFETY: both SIDs have been validated.
        Ok(unsafe { EqualSid(owner, current) != 0 })
    }

    fn with_private_creation_security<T>(
        directory: bool,
        create: impl FnOnce(*mut c_void) -> Result<T, AppError>,
    ) -> Result<T, AppError> {
        let user_information = current_user_information()?;
        // SAFETY: the validated SID remains in this live, unchanged buffer.
        let owner = unsafe { (*(user_information.as_ptr() as *const TokenUser)).user.sid };
        let acl_size = unsafe { 8 + 8 + GetLengthSid(owner) };
        let mut acl = vec![0u32; acl_size.div_ceil(4) as usize];
        let acl_pointer = acl.as_mut_ptr() as *mut c_void;
        let inheritance = if directory {
            OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE
        } else {
            0
        };
        let mut descriptor = std::mem::MaybeUninit::<SecurityDescriptor>::uninit();
        let descriptor_pointer = descriptor.as_mut_ptr() as *mut c_void;
        // SAFETY: the descriptor and ACL are aligned and correctly sized. All
        // pointers refer to local buffers that outlive the synchronous callback.
        // Explicit TOKEN_USER ownership avoids the token's default owner, which
        // may be Administrators even when running as the interactive user.
        let initialized = unsafe {
            InitializeAcl(acl_pointer, acl_size, 2) != 0
                && AddAccessAllowedAceEx(acl_pointer, 2, inheritance, FILE_ALL_ACCESS, owner) != 0
                && InitializeSecurityDescriptor(descriptor_pointer, 1) != 0
                && SetSecurityDescriptorOwner(descriptor_pointer, owner, 0) != 0
                && SetSecurityDescriptorDacl(descriptor_pointer, 1, acl_pointer, 0) != 0
                && SetSecurityDescriptorControl(
                    descriptor_pointer,
                    SE_DACL_PROTECTED,
                    SE_DACL_PROTECTED,
                ) != 0
                && IsValidSecurityDescriptor(descriptor_pointer) != 0
        };
        if !initialized {
            return Err(failure("creation_acl_initialization_failed"));
        }
        let mut attributes = SecurityAttributes {
            length: std::mem::size_of::<SecurityAttributes>() as u32,
            descriptor: descriptor_pointer,
            inherit_handle: 0,
        };
        create(&mut attributes as *mut SecurityAttributes as *mut c_void)
    }

    pub(super) fn create_directory(path: &Path) -> Result<(), AppError> {
        let name = encode_native_path(path)?;
        with_private_creation_security(true, |attributes| {
            // SAFETY: the terminated name and creation descriptor remain live.
            // CreateDirectoryW fails if the target already exists.
            if unsafe { CreateDirectoryW(name.as_ptr(), attributes) } == 0 {
                return Err(io_failure(
                    "directory_create_failed",
                    &std::io::Error::last_os_error(),
                ));
            }
            Ok(())
        })
    }

    pub(super) fn create_directory_tree(path: &Path) -> Result<(), AppError> {
        let mut missing = Vec::new();
        for ancestor in path.ancestors() {
            if let Some(current) = super::metadata(ancestor)? {
                if ancestor == path {
                    return Err(io_failure(
                        "directory_create_failed",
                        &std::io::Error::from(std::io::ErrorKind::AlreadyExists),
                    ));
                }
                if !current.is_dir() {
                    return Err(failure("non_directory_component"));
                }
                break;
            }
            missing.push(ancestor);
        }
        for directory in missing.into_iter().rev() {
            super::reject_linked_existing_components(directory)?;
            create_directory(directory)?;
            verify_directory(directory)?;
        }
        Ok(())
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
            let permissions = Self::read_unchecked_owner(path)?;
            if !owner_is_current(permissions.owner)? {
                return Err(failure("owner_mismatch"));
            }
            Ok(permissions)
        }

        fn read_unchecked_owner(path: &Path) -> Result<Self, AppError> {
            let name: Vec<u16> = encode_native_path(path)?;
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
            Self::validate_read(permissions, result)
        }

        fn read_handle_unchecked_owner(file: &File) -> Result<Self, AppError> {
            let mut permissions = Self {
                descriptor: ptr::null_mut(),
                owner: ptr::null_mut(),
                group: ptr::null_mut(),
                dacl: ptr::null_mut(),
                control: 0,
            };
            // SAFETY: the File owns a live handle and all output pointers are writable.
            let result = unsafe {
                GetSecurityInfo(
                    file.as_raw_handle(),
                    SE_FILE_OBJECT,
                    OWNER_GROUP_DACL,
                    &mut permissions.owner,
                    &mut permissions.group,
                    &mut permissions.dacl,
                    ptr::null_mut(),
                    &mut permissions.descriptor,
                )
            };
            Self::validate_read(permissions, result)
        }

        fn validate_read(mut permissions: Self, result: u32) -> Result<Self, AppError> {
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
            let mut name: Vec<u16> = encode_native_path(path)?;
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

    pub(super) type MirrorPermissions = WindowsPermissions;

    fn mirror_owner_is_current_or_default(
        owner: *const c_void,
        current_user: *const c_void,
    ) -> Result<bool, AppError> {
        if unsafe { EqualSid(owner, current_user) } == 0 {
            let owner_information = current_token_sid_information(TOKEN_OWNER_CLASS)?;
            // SAFETY: TOKEN_OWNER begins with the validated SID pointer.
            let default_owner = unsafe { *(owner_information.as_ptr() as *const *mut c_void) };
            if unsafe { IsWellKnownSid(default_owner, WIN_BUILTIN_ADMINISTRATORS_SID) } == 0
                || unsafe { EqualSid(owner, default_owner) } == 0
            {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub(super) fn mirror_file_permissions(path: &Path) -> Result<MirrorPermissions, AppError> {
        let permissions = WindowsPermissions::read_unchecked_owner(path)?;
        validate_mirror_permissions(permissions)
    }

    pub(super) fn mirror_handle_permissions(file: &File) -> Result<MirrorPermissions, AppError> {
        validate_mirror_permissions(WindowsPermissions::read_handle_unchecked_owner(file)?)
    }

    fn validate_mirror_permissions(
        permissions: WindowsPermissions,
    ) -> Result<MirrorPermissions, AppError> {
        let user_information = current_user_information()?;
        // SAFETY: current_user_information validated this SID and its buffer is live.
        let current_user = unsafe { (*(user_information.as_ptr() as *const TokenUser)).user.sid };
        if !mirror_owner_is_current_or_default(permissions.owner, current_user)? {
            return Err(failure("owner_mismatch"));
        }
        // SAFETY: raw read validated the ACL and SID. Inheritance is permitted
        // only in this mirror path, whose caller verifies the strict parent.
        unsafe {
            let header = &*(permissions.dacl as *const AclHeader);
            for index in 0..header.ace_count as u32 {
                let mut raw_ace = ptr::null_mut();
                if GetAce(permissions.dacl, index, &mut raw_ace) == 0 || raw_ace.is_null() {
                    return Err(failure("mirror_permissions_unsafe"));
                }
                let ace = &*(raw_ace as *const AceHeader);
                match ace.ace_type {
                    ACCESS_ALLOWED_ACE_TYPE => {
                        let sid = (raw_ace as *const u8).add(8) as *const c_void;
                        if IsValidSid(sid) == 0 || EqualSid(current_user, sid) == 0 {
                            return Err(failure("mirror_permissions_unsafe"));
                        }
                    }
                    ACCESS_DENIED_ACE_TYPE => {}
                    _ => return Err(failure("mirror_permissions_unsafe")),
                }
            }
        }
        Ok(permissions)
    }

    pub(super) fn mirror_permissions_match(
        expected: &MirrorPermissions,
        actual: &MirrorPermissions,
    ) -> bool {
        expected.matches(actual)
    }

    pub(super) fn open_locked_mirror(path: &Path) -> Result<File, AppError> {
        let name = encode_native_path(path)?;
        // SAFETY: a terminated path and valid flags. Sharing permits readers,
        // but denies all competing writer/deletion handles until this File drops.
        let handle = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | DELETE_ACCESS,
                FILE_SHARE_READ,
                ptr::null_mut(),
                OPEN_EXISTING,
                FILE_FLAG_OPEN_REPARSE_POINT,
                ptr::null_mut(),
            )
        };
        if handle == (-1isize as *mut c_void) {
            return Err(io_failure(
                "mirror_lock_failed",
                &std::io::Error::last_os_error(),
            ));
        }
        // SAFETY: ownership of this newly opened handle transfers once.
        let file = unsafe { File::from_raw_handle(handle) };
        let metadata = file
            .metadata()
            .map_err(|error| io_failure("file_metadata_failed", &error))?;
        if !metadata.is_file() || is_linked(&metadata) {
            return Err(failure("file_expected"));
        }
        Ok(file)
    }

    pub(super) fn create_mirror_temporary(path: &Path) -> Result<File, AppError> {
        let name = encode_native_path(path)?;
        with_private_creation_security(false, |attributes| {
            // SAFETY: descriptor/path remain live. CREATE_NEW never adopts a
            // collision; READ+WRITE+DELETE supports handle-bound rename/cleanup.
            let handle = unsafe {
                CreateFileW(
                    name.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE | DELETE_ACCESS,
                    FILE_SHARE_READ,
                    attributes,
                    CREATE_NEW,
                    FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
                    ptr::null_mut(),
                )
            };
            if handle == (-1isize as *mut c_void) {
                return Err(io_failure(
                    "temporary_create_failed",
                    &std::io::Error::last_os_error(),
                ));
            }
            // SAFETY: this File is the only owning wrapper for the new handle.
            Ok(unsafe { File::from_raw_handle(handle) })
        })
    }

    pub(super) fn read_mirror_handle(file: &mut File) -> Result<Vec<u8>, AppError> {
        file.seek(SeekFrom::Start(0))
            .map_err(|error| io_failure("mirror_read_failed", &error))?;
        let mut contents = Vec::new();
        file.read_to_end(&mut contents)
            .map_err(|error| io_failure("mirror_read_failed", &error))?;
        Ok(contents)
    }

    pub(super) fn rename_mirror_handle(file: &File, destination: &Path) -> Result<(), AppError> {
        let name = encode_native_path(destination)?;
        let name_bytes = (name.len() - 1)
            .checked_mul(2)
            .and_then(|length| u32::try_from(length).ok())
            .ok_or_else(|| failure("path_too_long"))?;
        let offset = std::mem::offset_of!(FileRenameInformation, name);
        let size = offset + name.len() * 2;
        let mut buffer = vec![0usize; size.div_ceil(std::mem::size_of::<usize>())];
        let information = buffer.as_mut_ptr() as *mut FileRenameInformation;
        // SAFETY: the aligned zeroed allocation fits the fixed header and the
        // entire terminated UTF-16 name. ReplaceIfExists is deliberately false.
        unsafe {
            (*information).replace_if_exists = 0;
            (*information).root_directory = ptr::null_mut();
            (*information).name_length = name_bytes;
            ptr::copy_nonoverlapping(
                name.as_ptr(),
                (information as *mut u8).add(offset) as *mut u16,
                name.len(),
            );
            if SetFileInformationByHandle(
                file.as_raw_handle(),
                3,
                information as *const c_void,
                size as u32,
            ) == 0
            {
                return Err(io_failure(
                    "mirror_install_failed",
                    &std::io::Error::last_os_error(),
                ));
            }
        }
        Ok(())
    }

    pub(super) fn delete_mirror_handle(file: &File) -> Result<(), AppError> {
        let delete_file: u8 = 1; // FILE_DISPOSITION_INFO::DeleteFile (BOOLEAN).
                                 // SAFETY: the live handle was opened with DELETE; disposition affects
                                 // that exact object, never whichever object later occupies its pathname.
        if unsafe {
            SetFileInformationByHandle(
                file.as_raw_handle(),
                4,
                &delete_file as *const u8 as *const c_void,
                1,
            )
        } == 0
        {
            return Err(io_failure(
                "mirror_remove_failed",
                &std::io::Error::last_os_error(),
            ));
        }
        Ok(())
    }

    #[cfg(test)]
    fn well_known_sid_for_test(kind: i32) -> Vec<u32> {
        let mut buffer = vec![0u32; 17]; // SECURITY_MAX_SID_SIZE (68 bytes).
        let mut size = 68;
        // SAFETY: the aligned buffer has the maximum supported SID size.
        assert_ne!(
            unsafe {
                CreateWellKnownSid(
                    kind,
                    ptr::null(),
                    buffer.as_mut_ptr() as *mut c_void,
                    &mut size,
                )
            },
            0
        );
        buffer
    }

    #[cfg(test)]
    pub(super) fn mirror_rejects_foreign_owner_for_test() -> bool {
        let system = well_known_sid_for_test(WIN_LOCAL_SYSTEM_SID);
        let world = well_known_sid_for_test(1); // WinWorldSid.
        !mirror_owner_is_current_or_default(
            system.as_ptr() as *const c_void,
            world.as_ptr() as *const c_void,
        )
        .unwrap()
    }

    #[cfg(test)]
    pub(super) fn add_broad_grant_for_test(path: &Path) -> Result<(), AppError> {
        let permissions = WindowsPermissions::read(path)?;
        let world = well_known_sid_for_test(1);
        let world_sid = world.as_ptr() as *const c_void;
        let acl_size =
            unsafe { 8 + 8 + GetLengthSid(permissions.owner) + 8 + GetLengthSid(world_sid) };
        let mut acl = vec![0u32; acl_size.div_ceil(4) as usize];
        let acl_pointer = acl.as_mut_ptr() as *mut c_void;
        let mut name = encode_native_path(path)?;
        // SAFETY: test-only, validated owned fixture; mutates its DACL, never its owner.
        let ok = unsafe {
            InitializeAcl(acl_pointer, acl_size, 2) != 0
                && AddAccessAllowedAceEx(acl_pointer, 2, 0, FILE_ALL_ACCESS, permissions.owner) != 0
                && AddAccessAllowedAceEx(acl_pointer, 2, 0, 0x0012_0089, world_sid) != 0
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
            return Err(failure("test_acl_failed"));
        }
        Ok(())
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(super) struct FileIdentity {
        volume_serial_number: u32,
        file_index: u64,
    }

    pub(super) fn durable_identity(identity: FileIdentity) -> super::Managed_directory_identity {
        super::Managed_directory_identity {
            volume: identity.volume_serial_number as u64,
            object: identity.file_index,
        }
    }

    pub(super) fn open_directory_guard(path: &Path, delete: bool) -> Result<File, AppError> {
        let name = encode_native_path(path)?;
        // READ_ATTRIBUTES only inspects the object; DELETE is required solely
        // on the source object that this handle will rename. Excluding share
        // DELETE keeps competing rename/delete handles out until this File drops.
        let handle = unsafe {
            CreateFileW(
                name.as_ptr(),
                0x80 | if delete { DELETE_ACCESS } else { 0 },
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                ptr::null_mut(),
                OPEN_EXISTING,
                FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS,
                ptr::null_mut(),
            )
        };
        if handle == (-1isize as *mut c_void) {
            return Err(io_failure(
                "directory_lock_failed",
                &std::io::Error::last_os_error(),
            ));
        }
        // SAFETY: transfer ownership of the single newly opened handle once.
        let file = unsafe { File::from_raw_handle(handle) };
        let metadata = file
            .metadata()
            .map_err(|error| io_failure("directory_metadata_failed", &error))?;
        if !metadata.is_dir() || is_linked(&metadata) {
            return Err(failure("directory_expected"));
        }
        Ok(file)
    }

    fn restrict_to_owner_with_mask(
        path: &Path,
        directory: bool,
        access_mask: u32,
    ) -> Result<(), AppError> {
        let permissions = WindowsPermissions::read(path)?;
        let acl_size = unsafe { 8 + 8 + GetLengthSid(permissions.owner) };
        let mut acl = vec![0u32; acl_size.div_ceil(4) as usize];
        let acl_pointer = acl.as_mut_ptr() as *mut c_void;
        let mut name: Vec<u16> = encode_native_path(path)?;
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

    pub(super) fn create_new_file(path: &Path, error_code: &str) -> Result<File, AppError> {
        let name = encode_native_path(path)?;
        with_private_creation_security(false, |attributes| {
            // SAFETY: all referenced buffers remain live for this call. CREATE_NEW
            // preserves the exclusive-create contract and never opens an existing
            // file or reparse point. The returned handle is non-inheritable.
            let handle = unsafe {
                CreateFileW(
                    name.as_ptr(),
                    GENERIC_WRITE,
                    0,
                    attributes,
                    CREATE_NEW,
                    FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
                    ptr::null_mut(),
                )
            };
            if handle == (-1isize as *mut c_void) {
                return Err(io_failure(error_code, &std::io::Error::last_os_error()));
            }
            // SAFETY: this successful CreateFileW handle has one owned File.
            Ok(unsafe { File::from_raw_handle(handle) })
        })
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

    fn path_identity(path: &Path, directory: bool) -> Result<FileIdentity, AppError> {
        let name: Vec<u16> = encode_native_path(path)?;
        // Desired access zero requests metadata only. OPEN_REPARSE_POINT makes
        // any last-component link observable instead of following it.
        let handle = unsafe {
            CreateFileW(
                name.as_ptr(),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                ptr::null_mut(),
                OPEN_EXISTING,
                FILE_FLAG_OPEN_REPARSE_POINT
                    | if directory {
                        FILE_FLAG_BACKUP_SEMANTICS
                    } else {
                        0
                    },
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

    pub(super) fn file_identity(path: &Path) -> Result<FileIdentity, AppError> {
        path_identity(path, false)
    }

    pub(super) fn directory_identity(path: &Path) -> Result<FileIdentity, AppError> {
        path_identity(path, true)
    }

    pub(super) fn file_identity_from_file(file: &File) -> Result<FileIdentity, AppError> {
        identity_from_handle(file.as_raw_handle())
    }

    pub(super) fn replace_file(source: &Path, destination: &Path) -> Result<(), AppError> {
        let source = encode_native_path(source)?;
        let destination = encode_native_path(destination)?;
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
            platform::create_directory(&path).expect("create private test root");
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
        assert!(read_mirror_file(&linked.join("secret.json")).is_err());
        assert!(!real.join("secret.json").exists());
    }

    #[cfg(windows)]
    #[test]
    fn windows_mirror_competing_gap_writer_is_preserved_with_recovery_hold() {
        let root = TestDir::new("mirror_gap_writer");
        let target = root.join("config.json");
        fs::write(&target, b"admitted native version").unwrap();
        let previous = read_mirror_file(&target).unwrap();
        let error = replace_mirror_file_with_hook(&target, b"our replacement", &previous, || {
            fs::write(&target, b"competing native version")
                .map_err(|error| io_failure("test_write_failed", &error))
        })
        .err()
        .unwrap();
        assert!(error.to_string().contains("mirror_recovery_retained"));
        assert_eq!(fs::read(&target).unwrap(), b"competing native version");
        let holds: Vec<_> = fs::read_dir(&root.0)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .contains("aag_tmp_recovery_")
            })
            .collect();
        assert_eq!(holds.len(), 1);
        assert_eq!(fs::read(&holds[0]).unwrap(), b"admitted native version");
        assert_eq!(
            fs::read_dir(&root.0).unwrap().count(),
            2,
            "failed new object was removed by handle"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_mirror_failed_install_restores_the_held_object_when_target_is_empty() {
        let root = TestDir::new("mirror_restore_hold");
        let target = root.join("config.json");
        fs::write(&target, b"original native version").unwrap();
        let previous = read_mirror_file(&target).unwrap();
        assert!(
            replace_mirror_file_with_hook(&target, b"replacement", &previous, || {
                Err(failure("test_install_failed"))
            })
            .is_err()
        );
        verify_mirror_snapshot(&target, &previous).unwrap();
        assert_eq!(fs::read_dir(&root.0).unwrap().count(), 1);
    }

    #[cfg(windows)]
    #[test]
    fn windows_mirror_missing_target_install_never_overwrites_a_new_client_file() {
        let root = TestDir::new("mirror_missing_race");
        let target = root.join("config.json");
        let previous = read_mirror_file(&target).unwrap();
        assert!(
            replace_mirror_file_with_hook(&target, b"our replacement", &previous, || {
                fs::write(&target, b"native created")
                    .map_err(|error| io_failure("test_write_failed", &error))
            })
            .is_err()
        );
        assert_eq!(fs::read(&target).unwrap(), b"native created");
        assert_eq!(fs::read_dir(&root.0).unwrap().count(), 1);
    }

    #[cfg(windows)]
    #[test]
    fn windows_mirror_lock_denies_competing_writes_deletes_and_commit_attempts() {
        let root = TestDir::new("mirror_locked");
        let target = root.join("config.json");
        atomic_write(&target, b"original").unwrap();
        let previous = read_mirror_file(&target).unwrap();
        let locked = platform::open_locked_mirror(&target).unwrap();
        assert!(fs::write(&target, b"competing write").is_err());
        assert!(fs::remove_file(&target).is_err());
        assert!(replace_mirror_file(&target, b"competing commit", &previous).is_err());
        drop(locked);
        verify_mirror_snapshot(&target, &previous).unwrap();
        remove_mirror_file(&target, &previous).unwrap();
        assert!(!target.exists());
    }

    #[cfg(windows)]
    #[test]
    fn windows_managed_mirror_accepts_private_native_rewrite_and_keeps_generic_owner_rule() {
        let root = TestDir::new("native_mirror");
        let target = root.join("config.json");
        fs::write(&target, b"native rewrite").unwrap();
        let strict_error = verify_private_file(&target).unwrap_err();
        let snapshot = read_mirror_file(&target).unwrap();
        let backup = root.join("backup.json");
        backup_mirror_file(&snapshot, &backup).unwrap();
        platform::verify_file(&backup).unwrap();
        assert_eq!(fs::read(&backup).unwrap(), b"native rewrite");
        // The native object was only read; no hardening/ownership takeover occurred.
        assert_eq!(
            verify_private_file(&target).unwrap_err().to_string(),
            strict_error.to_string()
        );
        replace_mirror_file(&target, b"managed replacement", &snapshot).unwrap();
        platform::verify_file(&target).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"managed replacement");
        assert!(backup_mirror_file(&snapshot, &backup).is_err());
        assert_eq!(fs::read(&backup).unwrap(), b"native rewrite");
    }

    #[cfg(windows)]
    #[test]
    fn windows_managed_mirror_rejects_broad_grants_unsafe_parent_and_foreign_owner() {
        let root = TestDir::new("mirror_reject");
        let target = root.join("config.json");
        atomic_write(&target, b"original").unwrap();
        platform::add_broad_grant_for_test(&target).unwrap();
        assert!(read_mirror_file(&target)
            .err()
            .unwrap()
            .to_string()
            .contains("mirror_permissions_unsafe"));
        let parent = root.join("unsafe_parent");
        create_private_dir(&parent).unwrap();
        platform::add_broad_grant_for_test(&parent).unwrap();
        assert!(read_mirror_file(&parent.join("absent.json")).is_err());
        assert!(platform::mirror_rejects_foreign_owner_for_test());
        assert_eq!(fs::read(&target).unwrap(), b"original");
    }

    #[test]
    fn managed_mirror_rechecks_identity_and_contents_before_replacement() {
        let root = TestDir::new("mirror_cas");
        let target = root.join("config.json");
        atomic_write(&target, b"original").unwrap();
        let snapshot = read_mirror_file(&target).unwrap();
        atomic_write(&target, b"third party").unwrap();
        assert!(replace_mirror_file(&target, b"replacement", &snapshot).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"third party");
        let snapshot = read_mirror_file(&target).unwrap();
        fs::write(&target, b"in place").unwrap();
        assert!(replace_mirror_file(&target, b"replacement", &snapshot).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"in place");
    }

    #[cfg(windows)]
    #[test]
    fn windows_managed_mirror_rechecks_acl_before_replacement() {
        let root = TestDir::new("mirror_acl_cas");
        let target = root.join("config.json");
        atomic_write(&target, b"original").unwrap();
        let snapshot = read_mirror_file(&target).unwrap();
        platform::add_broad_grant_for_test(&target).unwrap();
        assert!(replace_mirror_file(&target, b"replacement", &snapshot).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"original");
    }

    #[cfg(windows)]
    #[test]
    fn windows_fresh_objects_are_current_user_owned_and_private() {
        let root = TestDir::new("explicit_owner");
        let directory = root.join("parent/private");
        prepare_private_dir(&directory).unwrap();
        platform::verify_directory(&root.0).unwrap();
        platform::verify_directory(&root.join("parent")).unwrap();
        platform::verify_directory(&directory).unwrap();

        let file = directory.join("created.json");
        ensure_private_file(&file).unwrap();
        platform::verify_file(&file).unwrap();
        assert!(fs::read(&file).unwrap().is_empty());
        atomic_write(&file, b"replacement").unwrap();
        platform::verify_file(&file).unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"replacement");

        let other = directory.join("atomic.json");
        atomic_write(&other, b"fresh").unwrap();
        platform::verify_file(&other).unwrap();
        assert_eq!(fs::read(&other).unwrap(), b"fresh");
    }

    #[cfg(windows)]
    #[test]
    fn windows_exclusive_creation_preserves_existing_objects() {
        let root = TestDir::new("exclusive_owner");
        let directory = root.join("private");
        prepare_private_dir(&directory).unwrap();
        let identity = platform::directory_identity(&directory).unwrap();
        assert!(create_private_dir(&directory).is_err());
        assert_eq!(platform::directory_identity(&directory).unwrap(), identity);
        platform::verify_directory(&directory).unwrap();

        let file = directory.join("existing.json");
        atomic_write(&file, b"original").unwrap();
        platform::set_tighter_owner_access_for_test(&file).unwrap();
        let expected = platform::capture_permissions_for_test(&file).unwrap();
        assert!(platform::create_new_file(&file, "test_create_failed").is_err());
        assert!(platform::permissions_match_for_test(&expected, &file).unwrap());
        assert_eq!(fs::read(&file).unwrap(), b"original");
    }

    #[cfg(windows)]
    #[test]
    fn windows_existing_foreign_default_owner_is_never_adopted() {
        let root = TestDir::new("foreign_default_owner");
        let directory = root.join("default_owner");
        // Use the OS default owner without changing a token, taking ownership,
        // or enabling privileges. On this host it is Administrators, not USER.
        fs::create_dir(&directory).unwrap();
        let is_foreign = platform::verify_directory(&directory)
            .is_err_and(|error| error.to_string().contains("private_storage:owner_mismatch"));
        if !is_foreign {
            eprintln!(
                "foreign default-owner fixture unavailable on this token; no ownership changed"
            );
            return;
        }
        let identity = platform::directory_identity(&directory).unwrap();
        let error = prepare_managed_private_dir(&directory).unwrap_err();
        assert!(error.to_string().contains("private_storage:owner_mismatch"));
        assert_eq!(platform::directory_identity(&directory).unwrap(), identity);

        let file = root.join("default_owner.json");
        fs::write(&file, b"foreign content").unwrap();
        let identity = platform::file_identity(&file).unwrap();
        for result in [
            ensure_private_file(&file),
            atomic_write(&file, b"replacement"),
        ] {
            assert!(result
                .unwrap_err()
                .to_string()
                .contains("private_storage:owner_mismatch"));
        }
        assert_eq!(platform::file_identity(&file).unwrap(), identity);
        assert_eq!(fs::read(&file).unwrap(), b"foreign content");
    }

    #[test]
    fn temporary_directory_cleanup_preserves_a_replacement() {
        let root = TestDir::new("temp_identity");
        let temporary = private_temp_dir(&root.0).unwrap();
        let original_path = temporary.path().to_path_buf();
        let moved = root.join("moved");
        fs::rename(&original_path, &moved).unwrap();
        platform::create_directory(&original_path).unwrap();
        fs::write(original_path.join("foreign"), b"preserve").unwrap();
        drop(temporary);
        assert_eq!(
            fs::read(original_path.join("foreign")).unwrap(),
            b"preserve"
        );
        assert!(moved.is_dir());
    }

    #[test]
    fn temporary_directory_cleanup_removes_the_owned_directory() {
        let root = TestDir::new("temp_owned");
        let temporary = private_temp_dir(&root.0).unwrap();
        let path = temporary.path().to_path_buf();
        platform::verify_directory(&path).unwrap();
        drop(temporary);
        assert!(!path.exists());
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
        assert!(read_mirror_file(&linked.join("secret.json")).is_err());
        assert!(!real.join("secret.json").exists());
        fs::remove_dir(&linked).unwrap();
    }

    #[cfg(windows)]
    fn long_windows_directory(root: &TestDir) -> PathBuf {
        use std::os::windows::ffi::OsStrExt;

        let mut directory = root.join("nested");
        while directory.as_os_str().encode_wide().count() <= 300 {
            directory = directory.join("nested_component_with_a_bounded_name");
        }
        directory
    }

    #[cfg(windows)]
    #[test]
    fn windows_long_paths_preserve_private_replacement_and_backup() {
        let root = TestDir::new("long_path");
        let directory = long_windows_directory(&root);
        assert!(directory.is_absolute());
        prepare_private_dir(&directory).unwrap();
        let source = directory.join("state.json");
        let backup = directory.join("state.json.backup");
        atomic_write(&source, b"original").unwrap();
        platform::set_tighter_owner_access_for_test(&source).unwrap();
        let expected = platform::capture_permissions_for_test(&source).unwrap();

        backup_file(&source, &backup).unwrap();
        atomic_write(&source, b"replacement").unwrap();

        platform::verify_directory(&directory).unwrap();
        platform::verify_file(&source).unwrap();
        platform::verify_file(&backup).unwrap();
        assert!(platform::permissions_match_for_test(&expected, &source).unwrap());
        assert!(platform::permissions_match_for_test(&expected, &backup).unwrap());
        assert_eq!(fs::read(&source).unwrap(), b"replacement");
        assert_eq!(fs::read(&backup).unwrap(), b"original");
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 2);
    }

    #[cfg(windows)]
    #[test]
    fn windows_backup_handles_temporary_path_beyond_legacy_limit() {
        use std::os::windows::ffi::OsStrExt;

        let root = TestDir::new("long_backup");
        let directory = root.join("private");
        prepare_private_dir(&directory).unwrap();
        let source = directory.join("source.json");
        atomic_write(&source, b"original").unwrap();
        let parent_length = directory.as_os_str().encode_wide().count() + 1;
        let filename_length = 245usize.saturating_sub(parent_length).max(12);
        let destination = directory.join("b".repeat(filename_length));
        if parent_length + 12 < 260 {
            assert!(destination.as_os_str().encode_wide().count() < 260);
        }
        let temporary = temporary_path(&destination, &"0".repeat(32)).unwrap();
        assert!(temporary.as_os_str().encode_wide().count() > 260);
        assert!(!destination.exists());

        backup_file(&source, &destination).unwrap();

        assert_eq!(fs::read(&destination).unwrap(), b"original");
        platform::verify_file(&destination).unwrap();
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 2);
    }

    #[cfg(windows)]
    #[test]
    fn windows_long_paths_preserve_collision_and_identity_cleanup() {
        let root = TestDir::new("long_guards");
        let directory = long_windows_directory(&root);
        prepare_private_dir(&directory).unwrap();
        let target = directory.join("state.json");
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

        let absent_target = directory.join("absent.json");
        let error = atomic_write_with_suffix(&absent_target, b"replacement", None, "owned", |_| {
            fs::create_dir(&absent_target)
                .map_err(|error| io_failure("test_target_create_failed", &error))?;
            Ok(())
        })
        .unwrap_err();
        assert!(error.to_string().contains("private_storage:target_changed"));
        assert!(!temporary_path(&absent_target, "owned").unwrap().exists());
        assert!(absent_target.is_dir());
        assert_eq!(fs::read(&collision).unwrap(), b"foreign");
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
