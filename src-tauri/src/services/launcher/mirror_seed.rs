//! Allowlisted native-to-mirror seeding with private, backed-up destinations.

use crate::config::private_fs;
use std::{fs, io::Read, path::Path};

const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 128 * 1024 * 1024;
const MAX_ENTRIES: usize = 4096;
const MAX_DEPTH: usize = 16;

#[derive(Default)]
#[allow(non_camel_case_types)]
struct copy_budget {
    entries: usize,
    bytes: u64,
}

fn metadata(path: &Path) -> Result<Option<fs::Metadata>, String> {
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                if meta.file_attributes() & 0x400 != 0 {
                    return Ok(None);
                }
            }
            if meta.file_type().is_symlink() {
                return Ok(None);
            }
            Ok(Some(meta))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err("codex_mirror_seed_metadata_failed".into()),
    }
}

fn copy_file(source: &Path, destination: &Path, budget: &mut copy_budget) -> Result<(), String> {
    let Some(meta) = metadata(source)?.filter(fs::Metadata::is_file) else {
        return Ok(());
    };
    if meta.len() > MAX_FILE_BYTES {
        return Err("codex_mirror_seed_size_limit".into());
    }
    let mut bytes = Vec::new();
    fs::File::open(source)
        .map_err(|_| "codex_mirror_seed_read_failed".to_string())?
        .take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "codex_mirror_seed_read_failed".to_string())?;
    budget.bytes += bytes.len() as u64;
    if bytes.len() as u64 > MAX_FILE_BYTES || budget.bytes > MAX_TOTAL_BYTES {
        return Err("codex_mirror_seed_size_limit".into());
    }
    if let Some(parent) = destination.parent() {
        private_fs::prepare_private_dir(parent).map_err(String::from)?;
    }
    match fs::symlink_metadata(destination) {
        Ok(_) => {
            let mut backup_name = destination.as_os_str().to_owned();
            backup_name.push(".aag_backup");
            private_fs::backup_file(destination, Path::new(&backup_name)).map_err(String::from)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err("codex_mirror_seed_metadata_failed".into()),
    }
    private_fs::atomic_write(destination, &bytes).map_err(String::from)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Skill helper scripts must remain runnable after seeding, while
        // their content stays accessible to the owner alone.
        if meta.permissions().mode() & 0o100 != 0 {
            fs::set_permissions(destination, fs::Permissions::from_mode(0o700))
                .map_err(|_| "codex_mirror_seed_permissions_failed".to_string())?;
        }
    }
    Ok(())
}

fn copy_tree(
    source: &Path,
    destination: &Path,
    depth: usize,
    budget: &mut copy_budget,
) -> Result<(), String> {
    if metadata(source)?.map_or(true, |meta| !meta.is_dir()) {
        return Ok(());
    }
    if depth > MAX_DEPTH {
        return Err("codex_mirror_seed_depth_limit".into());
    }
    private_fs::prepare_private_dir(destination).map_err(String::from)?;
    for entry in fs::read_dir(source).map_err(|_| "codex_mirror_seed_scan_failed".to_string())? {
        budget.entries += 1;
        if budget.entries > MAX_ENTRIES {
            return Err("codex_mirror_seed_entry_limit".into());
        }
        let entry = entry.map_err(|_| "codex_mirror_seed_scan_failed".to_string())?;
        let source = entry.path();
        let destination = destination.join(entry.file_name());
        let Some(meta) = metadata(&source)? else {
            continue;
        };
        if meta.is_dir() {
            copy_tree(&source, &destination, depth + 1, budget)?;
        } else if meta.is_file() {
            copy_file(&source, &destination, budget)?;
        }
    }
    Ok(())
}

pub(super) fn seed_mirror_from_native(native_dir: &Path, mirror_dir: &Path) -> Result<(), String> {
    if metadata(native_dir)?.map_or(true, |meta| !meta.is_dir()) {
        return Ok(());
    }
    let mut budget = copy_budget::default();
    copy_file(
        &native_dir.join("AGENTS.md"),
        &mirror_dir.join("AGENTS.md"),
        &mut budget,
    )?;
    for name in ["skills", "agents"] {
        copy_tree(
            &native_dir.join(name),
            &mirror_dir.join(name),
            0,
            &mut budget,
        )?;
    }
    Ok(())
}

/// Windows sandbox accounts are shared between Codex instances. Copy only
/// their two allowlisted setup files, never native ChatGPT login/session data.
pub(super) fn sync_mirror_sandbox_credentials(
    native_dir: &Path,
    mirror_dir: &Path,
) -> Result<(), String> {
    if metadata(native_dir)?.map_or(true, |meta| !meta.is_dir()) {
        return Ok(());
    }
    for name in [".sandbox", ".sandbox-secrets"] {
        if metadata(&native_dir.join(name))?.map_or(true, |meta| !meta.is_dir()) {
            return Ok(());
        }
    }
    let mut budget = copy_budget::default();
    for name in [
        ".sandbox/setup_marker.json",
        ".sandbox-secrets/sandbox_users.json",
    ] {
        copy_file(&native_dir.join(name), &mirror_dir.join(name), &mut budget)?;
    }
    Ok(())
}
