//! Initial allowlisted native-to-mirror population. Existing instruction roots
//! belong to the mirror; optional mirror_sync owns subsequent refreshes.

use crate::config::private_fs;
use std::{fs, io::Read, path::Path};

const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 128 * 1024 * 1024;
const MAX_ENTRIES: usize = 4096;
const MAX_DEPTH: usize = 16;
const SANDBOX_SETUP_FILES: [&str; 2] = [
    ".sandbox/setup_marker.json",
    ".sandbox-secrets/sandbox_users.json",
];

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

/// Inspect an existing destination without following a link, traversing an
/// instruction tree, reading its contents or changing its runtime-owned ACL.
fn destination_exists(path: &Path, directory: bool) -> Result<bool, String> {
    let current = match fs::symlink_metadata(path) {
        Ok(current) => current,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err("codex_mirror_seed_metadata_failed".into()),
    };
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if current.file_attributes() & 0x400 != 0 {
            return Err("codex_mirror_seed_destination_linked".into());
        }
    }
    if current.file_type().is_symlink() {
        return Err("codex_mirror_seed_destination_linked".into());
    }
    let valid_shape = if directory {
        current.is_dir()
    } else {
        current.is_file()
    };
    if !valid_shape {
        return Err("codex_mirror_seed_destination_type_invalid".into());
    }
    Ok(true)
}

fn read_source_file(
    source: &Path,
    budget: &mut copy_budget,
) -> Result<Option<(Vec<u8>, fs::Metadata)>, String> {
    let Some(meta) = metadata(source)?.filter(fs::Metadata::is_file) else {
        return Ok(None);
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
    Ok(Some((bytes, meta)))
}

fn copy_file(
    source: &Path,
    destination: &Path,
    app_type: &'static str,
    budget: &mut copy_budget,
) -> Result<(), String> {
    let Some((bytes, _source_metadata)) = read_source_file(source, budget)? else {
        return Ok(());
    };
    if let Some(parent) = destination.parent() {
        private_fs::prepare_private_dir(parent).map_err(String::from)?;
    }
    let previous = private_fs::read_mirror_file(destination).map_err(String::from)?;
    if previous.contents.is_some() {
        return Err("codex_mirror_seed_destination_changed".into());
    }
    // A CLI-created file that appears after initial admission remains its own
    // configuration. The absent snapshot also guards the final install race.
    crate::services::provider::config_writes::write_mirror_files(&[
        crate::services::provider::config_writes::MirrorConfigFileUpdate {
            path: destination,
            contents: &bytes,
            app_type,
            previous: &previous,
        },
    ])
    .map_err(String::from)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Skill helper scripts must remain runnable after seeding, while
        // their content stays accessible to the owner alone.
        if _source_metadata.permissions().mode() & 0o100 != 0 {
            fs::set_permissions(destination, fs::Permissions::from_mode(0o700))
                .map_err(|_| "codex_mirror_seed_permissions_failed".to_string())?;
        }
    }
    Ok(())
}

fn copy_tree(
    source: &Path,
    destination: &Path,
    app_type: &'static str,
    depth: usize,
    budget: &mut copy_budget,
) -> Result<(), String> {
    if metadata(source)?.is_none_or(|meta| !meta.is_dir()) {
        return Ok(());
    }
    if depth > MAX_DEPTH {
        return Err("codex_mirror_seed_depth_limit".into());
    }
    if let Some(parent) = destination.parent() {
        private_fs::prepare_private_dir(parent).map_err(String::from)?;
    }
    // Never adopt a root or child directory created after admission. Keeping
    // creation exclusive preserves its contents and runtime-owned permissions.
    private_fs::create_private_dir(destination).map_err(String::from)?;
    for entry in fs::read_dir(source).map_err(|_| "codex_mirror_seed_scan_failed".to_string())? {
        budget.entries += 1;
        if budget.entries > MAX_ENTRIES {
            return Err("codex_mirror_seed_entry_limit".into());
        }
        let entry = entry.map_err(|_| "codex_mirror_seed_scan_failed".to_string())?;
        // Codex installs and updates its bundled system skills inside its own
        // home. They are vendor runtime state, not user personalization to seed.
        if depth == 0
            && source.file_name().is_some_and(|name| name == "skills")
            && entry.file_name() == ".system"
        {
            continue;
        }
        let source = entry.path();
        let destination = destination.join(entry.file_name());
        let Some(meta) = metadata(&source)? else {
            continue;
        };
        if meta.is_dir() {
            copy_tree(&source, &destination, app_type, depth + 1, budget)?;
        } else if meta.is_file() {
            copy_file(&source, &destination, app_type, budget)?;
        }
    }
    Ok(())
}

/// Initial allowlisted population for one client's mirror: the primary
/// instruction file plus the client's seed roots (a subset of what continuous
/// sync manages — see `mirror_sync::Client::seed_roots`).
pub(super) fn seed_mirror_from_native(
    client: crate::services::mirror_sync::Client,
    native_dir: &Path,
    mirror_dir: &Path,
) -> Result<(), String> {
    let instructions = mirror_dir.join(client.seed_instruction_file());
    // Check the mirror's ancestors and all destination shapes before any
    // copy. Existing client-created roots may have legitimate inherited ACLs.
    private_fs::validate_external_file_path(&instructions).map_err(String::from)?;
    let preserve_instructions = destination_exists(&instructions, false)?;
    let mut roots = Vec::new();
    for name in client.seed_roots() {
        roots.push((*name, destination_exists(&mirror_dir.join(name), true)?));
    }
    if preserve_instructions && roots.iter().all(|(_, exists)| *exists) {
        return Ok(());
    }
    if metadata(native_dir)?.is_none_or(|meta| !meta.is_dir()) {
        return Ok(());
    }
    let app_type = client.instructions_app_type();
    let mut budget = copy_budget::default();
    if !preserve_instructions {
        copy_file(
            &native_dir.join(client.seed_instruction_file()),
            &instructions,
            app_type,
            &mut budget,
        )?;
    }
    for (name, exists) in roots {
        if !exists {
            copy_tree(
                &native_dir.join(name),
                &mirror_dir.join(name),
                app_type,
                0,
                &mut budget,
            )?;
        }
    }
    Ok(())
}

/// Initially copy only the two allowlisted Windows sandbox setup files.
/// Once both exist, Codex owns their contents and sandbox-specific ACLs.
pub(super) fn sync_mirror_sandbox_credentials(
    native_dir: &Path,
    mirror_dir: &Path,
) -> Result<(), String> {
    let mut initialized_count = 0;
    for name in SANDBOX_SETUP_FILES {
        let destination = mirror_dir.join(name);
        private_fs::validate_external_file_path(&destination).map_err(String::from)?;
        initialized_count += usize::from(destination_exists(&destination, false)?);
    }
    if initialized_count == SANDBOX_SETUP_FILES.len() {
        return Ok(());
    }
    if initialized_count != 0 {
        return Err("codex_mirror_sandbox_setup_partial".into());
    }
    if metadata(native_dir)?.is_none_or(|meta| !meta.is_dir()) {
        return Ok(());
    }
    for name in [".sandbox", ".sandbox-secrets"] {
        if metadata(&native_dir.join(name))?.is_none_or(|meta| !meta.is_dir()) {
            return Ok(());
        }
    }
    let mut budget = copy_budget::default();
    let mut pending = Vec::new();
    for name in SANDBOX_SETUP_FILES {
        let Some((contents, _)) = read_source_file(&native_dir.join(name), &mut budget)? else {
            return Err("codex_mirror_sandbox_setup_incomplete".into());
        };
        pending.push((mirror_dir.join(name), contents));
    }
    // Both source reads and destination checks precede the first file write.
    // Client-created sandbox parents keep their runtime ACLs and fail closed
    // here if initialization is incomplete and AAG cannot safely populate them.
    for (path, _) in &pending {
        private_fs::prepare_private_dir(path.parent().unwrap()).map_err(String::from)?;
    }
    let previous: Vec<_> = pending
        .iter()
        .map(|(path, _)| private_fs::read_mirror_file(path))
        .collect::<Result<_, _>>()
        .map_err(String::from)?;
    if previous.iter().any(|snapshot| snapshot.contents.is_some()) {
        return Err("codex_mirror_sandbox_setup_changed".into());
    }
    let updates: Vec<_> = pending
        .iter()
        .zip(&previous)
        .map(|((path, contents), snapshot)| {
            crate::services::provider::config_writes::MirrorConfigFileUpdate {
                path,
                contents,
                app_type: "codex",
                previous: snapshot,
            }
        })
        .collect();
    crate::services::provider::config_writes::write_mirror_files(&updates).map_err(String::from)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instruction_file_created_after_preflight_is_preserved_without_backup() {
        let home = crate::services::provider::test_support::isolated_home("instruction-file-race");
        let source = home.root.join("native-instructions.md");
        let mirror = home.root.join("mirror");
        private_fs::prepare_managed_private_dir(&mirror).unwrap();
        fs::write(&source, "native instructions").unwrap();
        let destination = mirror.join("AGENTS.md");
        assert!(!destination_exists(&destination, false).unwrap());
        assert!(read_source_file(&source, &mut copy_budget::default())
            .unwrap()
            .is_some());
        // The CLI creates this valid file after initial destination/source
        // admission, before seeding reaches its write boundary.
        private_fs::atomic_write(&destination, b"CLI instructions").unwrap();
        assert!(copy_file(&source, &destination, "codex_instructions", &mut copy_budget::default()).is_err());
        assert_eq!(
            fs::read_to_string(&destination).unwrap(),
            "CLI instructions"
        );
        assert!(!mirror.join("AGENTS.md.aag_backup").exists());
    }

    #[test]
    fn instruction_root_created_after_preflight_is_preserved_without_traversal() {
        let home = crate::services::provider::test_support::isolated_home("instruction-root-race");
        let source = home.root.join("native-skills");
        let mirror = home.root.join("mirror");
        private_fs::prepare_managed_private_dir(&source).unwrap();
        private_fs::prepare_managed_private_dir(&mirror).unwrap();
        fs::write(source.join("keep.md"), "native skill").unwrap();
        fs::write(source.join("new.md"), "new native skill").unwrap();
        let destination = mirror.join("skills");
        assert!(!destination_exists(&destination, true).unwrap());
        assert!(metadata(&source).unwrap().unwrap().is_dir());
        private_fs::prepare_managed_private_dir(&destination).unwrap();
        private_fs::atomic_write(&destination.join("keep.md"), b"CLI skill").unwrap();
        assert!(copy_tree(&source, &destination, "codex_instructions", 0, &mut copy_budget::default()).is_err());
        assert_eq!(
            fs::read_to_string(destination.join("keep.md")).unwrap(),
            "CLI skill"
        );
        assert!(!destination.join("keep.md.aag_backup").exists());
        assert!(!destination.join("new.md").exists());
    }
}
