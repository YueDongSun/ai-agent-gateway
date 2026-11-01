//! Owned files, strict input validation and concurrent-edit detection.
use super::types::Dsh_settings;
use crate::error::AppError;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

pub(super) const SETTINGS_FILE: &str = "aag_settings.json";
pub(super) const MANAGED_PROFILE: &str = "profiles/web";
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub(super) fn failure(code: &str) -> AppError {
    AppError::Message(format!("dsh:{code}"))
}
pub(super) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
pub(super) fn operation_id() -> String {
    digest(
        format!(
            "{}:{}:{}",
            std::process::id(),
            now_ms(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        )
        .as_bytes(),
    )[..24]
        .into()
}
pub(super) fn digest(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// Refuse linked ancestors and final components. Package-manager-owned links
/// inside node_modules are validated separately and never recursively deleted.
pub(super) fn safe_path(path: &Path) -> Result<(), AppError> {
    for ancestor in path.ancestors() {
        reject_linked(ancestor)?;
    }
    Ok(())
}

/// Reject a single linked path component. Callers that validate many paths
/// sharing an ancestor chain cache already-checked components and call this
/// directly instead of repeating full `safe_path` walks.
pub(super) fn reject_linked(path: &Path) -> Result<(), AppError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                if metadata.file_attributes() & 0x400 != 0 {
                    return Err(failure("linked_managed_path"));
                }
            }
            if metadata.file_type().is_symlink() {
                return Err(failure("linked_managed_path"));
            }
            Ok(())
        }
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(())
        }
        Err(_) => Err(failure("managed_path_unreadable")),
    }
}

pub(super) fn read_optional(path: &Path) -> Result<Option<Vec<u8>>, AppError> {
    safe_path(path)?;
    match fs::File::open(path) {
        Ok(file) => {
            let mut data = Vec::new();
            file.take(4 * 1024 * 1024 + 1)
                .read_to_end(&mut data)
                .map_err(|_| failure("managed_file_unreadable"))?;
            if data.len() > 4 * 1024 * 1024 {
                Err(failure("managed_file_too_large"))
            } else {
                Ok(Some(data))
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(failure("managed_file_unreadable")),
    }
}
pub(super) fn read_json(path: &Path) -> Result<Option<Value>, AppError> {
    read_optional(path)?
        .map(|data| serde_json::from_slice(&data).map_err(|_| failure("invalid_managed_json")))
        .transpose()
}

pub(super) fn read_settings(root: &Path) -> Result<(Dsh_settings, String), AppError> {
    match read_optional(&root.join(SETTINGS_FILE))? {
        Some(data) => {
            let settings =
                serde_json::from_slice(&data).map_err(|_| failure("invalid_settings"))?;
            Ok((settings, digest(&data)))
        }
        None => Ok((Dsh_settings::default(), digest(b"missing"))),
    }
}

pub(super) fn validate_settings(settings: &Dsh_settings) -> Result<(), AppError> {
    if settings.port < 1024
        || !(5..=180).contains(&settings.startup_timeout_secs)
        || !(5..=600).contains(&settings.operation_timeout_secs)
        || !["system", "light", "dark"].contains(&settings.theme.as_str())
    {
        return Err(failure("invalid_settings"));
    }
    for (value, is_node) in [
        (&settings.node_path, true),
        (&settings.dsh_entry_path, false),
        (&settings.pnpm_entry_path, false),
    ] {
        if value.is_empty() {
            continue;
        }
        let path = Path::new(value);
        if value.len() > 4096
            || value.chars().any(char::is_control)
            || !path.is_absolute()
            || !path.is_file()
        {
            return Err(failure("invalid_runtime_path"));
        }
        if !is_node
            && !matches!(
                path.extension().and_then(|extension| extension.to_str()),
                Some("js" | "mjs" | "cjs")
            )
        {
            return Err(failure("entry_must_be_javascript"));
        }
        if is_node
            && !matches!(
                path.file_name().and_then(|name| name.to_str()),
                Some("node" | "node.exe")
            )
        {
            return Err(failure("node_binary_required"));
        }
    }
    for value in [&settings.upstream_id, &settings.model_id]
        .into_iter()
        .flatten()
    {
        if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
            return Err(failure("invalid_selection"));
        }
    }
    Ok(())
}

pub(super) fn write_new(path: &Path, data: &[u8]) -> Result<(), AppError> {
    safe_path(path)?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|_| failure("managed_file_create_failed"))?;
    file.write_all(data)
        .and_then(|()| file.sync_all())
        .map_err(|_| failure("managed_file_write_failed"))
}

pub(super) fn read_optional_recoverable(path: &Path) -> Result<Option<Vec<u8>>, AppError> {
    match read_optional(path)? {
        Some(data) => Ok(Some(data)),
        None => {
            let backup = stable_backup_path(path);
            if backup.exists() {
                read_optional(&backup)
            } else {
                Ok(None)
            }
        }
    }
}

/// Crash-in-gap recovery: a target absent while its latest recovery copy
/// exists means write_atomic died between the two renames. Reserved for
/// AAG-internal sidecars with no user-editable content; user-visible managed
/// files keep strict reads so a deliberate deletion stays a reset.
pub(super) fn read_json_recoverable(path: &Path) -> Result<Option<Value>, AppError> {
    read_optional_recoverable(path)?
        .map(|data| serde_json::from_slice(&data).map_err(|_| failure("invalid_managed_json")))
        .transpose()
}

/// One recovery copy per managed file, named beside the target so it can
/// never collide with the legacy per-operation sidecar names. The launcher
/// mirror seed (services/launcher/mirror_seed.rs) independently keeps this
/// same `<file>.aag_backup` convention for its own disjoint directories.
fn stable_backup_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!("{file_name}.aag_backup"))
}

/// Replace a managed file, retaining only the latest recovery copy beside it.
/// The caller holds both the in-process mutex and per-instance filesystem
/// lock. A failed write never leaks its temporary file; a successful write
/// prunes superseded legacy sidecars.
pub(super) fn write_atomic(path: &Path, data: &[u8]) -> Result<(), AppError> {
    safe_path(path)?;
    let parent = path
        .parent()
        .ok_or_else(|| failure("invalid_managed_path"))?;
    fs::create_dir_all(parent).map_err(|_| failure("managed_directory_create_failed"))?;
    let temporary = parent.join(format!(".aag_write_{}", operation_id()));
    let result = write_new(&temporary, data)
        .and_then(|()| replace_with_backup(path, &temporary, &stable_backup_path(path)));
    if result.is_err() {
        // A leaked temporary is never a recovery source.
        let _ = fs::remove_file(&temporary);
    } else {
        prune_legacy_sidecars(parent);
    }
    result
}

/// The stable recovery copy replaces the previous one. A recovery copy that
/// cannot be replaced fails the write before the target is moved.
fn replace_with_backup(path: &Path, temporary: &Path, backup: &Path) -> Result<(), AppError> {
    reject_linked(backup)?;
    if backup.exists() && fs::remove_file(backup).is_err() {
        return Err(failure("backup_replace_failed"));
    }
    let had_original = path.exists();
    if had_original {
        fs::rename(path, backup).map_err(|_| failure("backup_failed"))?;
    }
    if fs::rename(temporary, path).is_err() {
        if had_original && fs::rename(backup, path).is_err() {
            return Err(failure("rollback_failed"));
        }
        return Err(failure("atomic_write_failed"));
    }
    Ok(())
}

/// Legacy per-operation backups (.aag_backup_<id>) and leaked temporaries
/// (.aag_write_<id>) are superseded by construction once a newer write
/// commits. Removal is best-effort and limited to regular files.
fn prune_legacy_sidecars(parent: &Path) {
    let Ok(entries) = fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !(name.starts_with(".aag_backup_") || name.starts_with(".aag_write_")) {
            continue;
        }
        if entry.file_type().is_ok_and(|kind| kind.is_file()) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

pub(super) fn profile_manifest() -> Value {
    json!({"name":"aag_dsh_web_profile","private":true,"dsh":{"profile":{"bundles":["@deepseek-ai/dsh-base","@deepseek-ai/dsh-web-app"]}},"dependencies":{}})
}

pub(super) fn state_digest(root: &Path) -> Result<String, AppError> {
    let mut hasher = Sha256::new();
    for path in [
        root.join(SETTINGS_FILE),
        root.join(MANAGED_PROFILE).join("package.json"),
        root.join(MANAGED_PROFILE).join("pnpm-lock.yaml"),
        root.join(MANAGED_PROFILE).join("pnpm-workspace.yaml"),
    ] {
        match read_optional(&path)? {
            Some(data) => {
                hasher.update([1]);
                hasher.update(data);
            }
            None => hasher.update([0]),
        }
    }
    Ok(hex::encode(hasher.finalize()))
}

/// The lock file is persistent; the operating system owns exclusivity and
/// releases it if AAG exits or crashes. Never unlink an active lock inode.
#[allow(non_camel_case_types)]
pub(super) struct Instance_lock {
    _file: fs::File,
}
impl Instance_lock {
    pub(super) fn acquire(root: &Path) -> Result<Self, AppError> {
        safe_path(root)?;
        fs::create_dir_all(root).map_err(|_| failure("managed_directory_create_failed"))?;
        // Keep the exclusive handle outside DSH_HOME: DSH config watchers
        // inspect sibling files and cannot watch an exclusively opened file.
        let path = root
            .parent()
            .ok_or_else(|| failure("invalid_managed_path"))?
            .join("dsh_instance.lock");
        safe_path(&path)?;
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(0);
        }
        let mut file = options.open(&path).map_err(|error| {
            if cfg!(windows) && matches!(error.raw_os_error(), Some(32 | 33)) {
                failure("instance_locked")
            } else {
                failure("lock_create_failed")
            }
        })?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            extern "C" {
                fn flock(fd: i32, operation: i32) -> i32;
            }
            if unsafe { flock(file.as_raw_fd(), 2 | 4) } != 0 {
                return Err(failure("instance_locked"));
            }
        }
        file.set_len(0).map_err(|_| failure("lock_write_failed"))?;
        file.write_all(operation_id().as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|_| failure("lock_write_failed"))?;
        Ok(Self { _file: file })
    }
}
