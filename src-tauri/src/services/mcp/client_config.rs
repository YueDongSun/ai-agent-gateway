//! Client adapters never launch commands, connect to servers, or alter trust policy.
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

use serde_json::{json, Value};

use super::{
    failure,
    validation::{redact_config, validate_config, validate_existing_id},
    Mcp_sync_change,
};
use crate::{database::dao::mcp::McpServer, error::AppError};

const MAX_CLIENT_BYTES: u64 = 2 * 1024 * 1024;

pub(super) fn check_plain_path(path: &Path) -> Result<(), AppError> {
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) => {
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    if metadata.file_attributes() & 0x400 != 0 {
                        return Err(failure("linked_config_path_not_allowed"));
                    }
                }
                if metadata.file_type().is_symlink() {
                    return Err(failure("linked_config_path_not_allowed"));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(_) => return Err(failure("client_config_unreadable")),
        }
    }
    Ok(())
}

pub(super) fn read_client(path: &Path) -> Result<Option<Vec<u8>>, AppError> {
    check_plain_path(path)?;
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(failure("client_config_unreadable")),
    };
    let metadata = file
        .metadata()
        .map_err(|_| failure("client_config_unreadable"))?;
    if !metadata.is_file() || metadata.len() > MAX_CLIENT_BYTES {
        return Err(failure("client_config_too_large_or_not_file"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_CLIENT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| failure("client_config_unreadable"))?;
    if bytes.len() as u64 > MAX_CLIENT_BYTES {
        return Err(failure("client_config_too_large_or_not_file"));
    }
    Ok(Some(bytes))
}

pub(super) fn claude_plan(
    original: Option<&[u8]>,
    servers: &[McpServer],
) -> Result<(Vec<u8>, String, Vec<Mcp_sync_change>), AppError> {
    // Never return parse diagnostics from a credential-bearing client file.
    let mut document: Value = match original {
        Some(bytes) => {
            serde_json::from_slice(bytes).map_err(|_| failure("client_config_invalid_json"))?
        }
        None => json!({}),
    };
    let root = document
        .as_object_mut()
        .ok_or_else(|| failure("client_config_must_be_object"))?;
    if !root.contains_key("mcpServers") {
        root.insert("mcpServers".into(), json!({}));
    }
    let entries = root
        .get_mut("mcpServers")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| failure("client_mcp_servers_must_be_object"))?;
    let mut changes = Vec::new();
    let mut redacted = serde_json::Map::new();
    for server in servers {
        validate_existing_id(&server.id)?;
        let key = format!("aag_{}", server.id);
        let action = if server.enabled_claude {
            let config = validate_config(&server.server_config)?;
            let action = match entries.get(&key) {
                None => "add",
                Some(existing) if existing == &config => "unchanged",
                Some(_) => "replace",
            };
            redacted.insert(key.clone(), redact_config(&config));
            entries.insert(key, config);
            action
        } else if entries.remove(&key).is_some() {
            "remove"
        } else {
            "unchanged"
        };
        changes.push(Mcp_sync_change {
            server_id: server.id.clone(),
            action: action.into(),
        });
    }
    let mut bytes = serde_json::to_vec_pretty(&document)?;
    bytes.push(b'\n');
    if bytes.len() as u64 > MAX_CLIENT_BYTES {
        return Err(failure("client_config_too_large_or_not_file"));
    }
    let preview = serde_json::to_string_pretty(&json!({"mcpServers": redacted}))?;
    Ok((bytes, preview, changes))
}

pub(super) fn codex_preview(servers: &[McpServer]) -> Result<String, AppError> {
    let mut text = String::from("# Redacted review only. Restore private values before manual use.\n# AAG does not edit Codex configuration.\n");
    for server in servers.iter().filter(|server| server.enabled_codex) {
        validate_existing_id(&server.id)?;
        let config = validate_config(&server.server_config)?;
        let redacted = redact_config(&config);
        text.push_str(&format!("\n[mcp_servers.aag_{}]\n", server.id));
        for key in ["command", "args", "url"] {
            if let Some(value) = redacted.get(key) {
                // JSON strings/arrays are valid for this conservative TOML subset.
                text.push_str(&format!("{key} = {}\n", serde_json::to_string(value)?));
            }
        }
        for (source_key, target_key) in [("env", "env"), ("headers", "http_headers")] {
            if let Some(values) = redacted.get(source_key).and_then(Value::as_object) {
                text.push_str(&format!("\n[mcp_servers.aag_{}.{target_key}]\n", server.id));
                for (key, value) in values {
                    text.push_str(&format!(
                        "{} = {}\n",
                        serde_json::to_string(key)?,
                        serde_json::to_string(value)?
                    ));
                }
            }
        }
    }
    Ok(text)
}

#[allow(non_camel_case_types)]
struct Owned_file {
    path: PathBuf,
    handle: Option<fs::File>,
    remove_on_drop: bool,
}
impl Owned_file {
    fn create(path: PathBuf) -> Result<Self, AppError> {
        let handle = create_private(&path)?;
        Ok(Self {
            path,
            handle: Some(handle),
            remove_on_drop: true,
        })
    }

    fn write_durable(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        let handle = self.handle.as_mut().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::BrokenPipe, "transaction file is closed")
        })?;
        handle.write_all(bytes)?;
        handle.sync_all()
    }

    fn close(&mut self) {
        drop(self.handle.take());
    }

    fn retain(mut self) {
        self.close();
        self.remove_on_drop = false;
    }
}
impl Drop for Owned_file {
    fn drop(&mut self) {
        // Windows exclusive sharing also denies deletion through a second
        // handle. Close our handle before removing this owned temporary/lock.
        self.close();
        if self.remove_on_drop {
            let _ = fs::remove_file(&self.path);
        }
    }
}

pub(super) fn create_private(path: &Path) -> Result<fs::File, AppError> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // Deny even an owner read handle from the instant of creation. This
        // closes the inherited-ACL gap before restrictive ACLs are applied;
        // no reader may retain a handle that later observes secret bytes.
        options.share_mode(0);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .map_err(|_| failure("client_write_preparation_failed"))
}

/// Remove regular files whose name starts with an AAG-owned sidecar prefix.
/// Best-effort with a fixed warning on failure. Links are never followed and
/// directories are never removed, so a foreign entry is left in place.
fn prune_orphaned_sidecars(parent: &Path, prefix: &str) {
    let Ok(entries) = fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let matches_prefix = entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(prefix));
        if matches_prefix && entry.file_type().is_ok_and(|kind| kind.is_file()) {
            if let Err(error) = fs::remove_file(entry.path()) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    log::warn!("client: sidecar prune failed code=remove_failed");
                }
            }
        }
    }
}

/// Serialize AAG writers with an exclusive lock, detect stale content, retain a
/// private backup, and atomically replace only after the full new file is durable.
/// Clients must be closed: an uncooperative external writer is not lock-aware.
pub(super) fn apply_claude(
    path: &Path,
    original: &Option<Vec<u8>>,
    replacement: &[u8],
    nonce: &str,
) -> Result<bool, AppError> {
    check_plain_path(path)?;
    let parent = path
        .parent()
        .ok_or_else(|| failure("invalid_client_path"))?;
    if !parent.is_dir() {
        return Err(failure("client_home_missing"));
    }
    if original.is_some()
        && fs::metadata(path)
            .map_err(|_| failure("client_config_unreadable"))?
            .permissions()
            .readonly()
    {
        // In particular on Windows, copying the read-only flag onto the temp
        // would prevent its deterministic cleanup after a refused replacement.
        return Err(failure("client_readonly"));
    }
    let lock_path = parent.join(".claude.json.aag_lock");
    let _lock =
        Owned_file::create(lock_path).map_err(|_| failure("client_sync_busy_or_interrupted"))?;
    // The exclusive lock is held, so any pending temporary still in the home
    // directory is an orphan of a crashed apply that holds no lock, and the
    // per-review backup copies written by older builds are superseded by the
    // single recovery copy this apply is about to write. Reclaiming those
    // credential-bearing files bounds them even when the apply later aborts.
    prune_orphaned_sidecars(parent, ".claude.json.aag_pending_");
    prune_orphaned_sidecars(parent, ".claude.json.aag_backup_");
    if &read_client(path)? != original {
        return Err(failure("client_changed_since_preview"));
    }
    #[cfg(windows)]
    let source_acl = original
        .as_ref()
        .map(|_| super::windows_permissions::Windows_permissions::read(path))
        .transpose()?;
    let temporary = parent.join(format!(".claude.json.aag_pending_{nonce}"));
    let mut pending = Owned_file::create(temporary.clone())?;
    #[cfg(windows)]
    if let Some(permissions) = &source_acl {
        permissions.apply(&temporary)?;
    } else {
        super::windows_permissions::restrict_to_owner(&temporary)?;
    }
    pending
        .write_durable(replacement)
        .map_err(|_| failure("client_write_failed"))?;
    if let Ok(metadata) = fs::metadata(path) {
        fs::set_permissions(&temporary, metadata.permissions())
            .map_err(|_| failure("client_permissions_failed"))?;
    }
    pending.close();
    if let Some(previous) = original {
        // One latest recovery copy per file. The earlier copy is superseded
        // before the live replacement, so removing a token-bearing server
        // definition cannot leave its token behind in a growing backup
        // history; a crash before the replacement leaves the live file
        // intact, which is the state the copy protected.
        let backup_path = parent.join(".claude.json.aag_backup");
        match fs::remove_file(&backup_path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(failure("client_backup_failed")),
        }
        let mut backup = Owned_file::create(backup_path.clone())?;
        #[cfg(windows)]
        if let Some(permissions) = &source_acl {
            permissions.apply(&backup_path)?;
        }
        backup
            .write_durable(previous)
            .map_err(|_| failure("client_backup_failed"))?;
        backup.retain();
    }
    if &read_client(path)? != original {
        return Err(failure("client_changed_since_preview"));
    }
    fs::rename(&temporary, path).map_err(|_| failure("client_atomic_replace_failed"))?;
    if read_client(path)?.as_deref() != Some(replacement) {
        return Err(failure("client_changed_after_apply_review_backup"));
    }
    Ok(original.is_some())
}
