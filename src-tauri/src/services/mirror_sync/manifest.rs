use super::{scan, Client, Client_report};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

const MANIFEST_NAME: &str = ".aag_instruction_sync.json";
const MANIFEST_LIMIT: u64 = 4 * 1024 * 1024;
const MAX_DESTINATION_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_version: u8,
    client: String,
    source_root: PathBuf,
    managed_files: BTreeSet<PathBuf>,
}

fn validate_manifest(manifest: &Manifest, client: Client, native: &Path) -> Result<(), String> {
    if manifest.schema_version != 1
        || manifest.client != client.name()
        || !crate::config::same_dir(&manifest.source_root, native)
        || manifest.managed_files.len() > scan::MAX_ENTRIES * 2
        || manifest
            .managed_files
            .iter()
            .any(|path| !client.allows(path))
    {
        return Err("mirror_sync_manifest_invalid".into());
    }
    let mut unique = BTreeSet::new();
    for path in &manifest.managed_files {
        if !unique.insert(scan::path_key(path)) {
            return Err("mirror_sync_manifest_invalid".into());
        }
    }
    Ok(())
}

#[cfg(any(windows, test))]
pub(super) fn apply(
    client: Client,
    native: &Path,
    mirror: &Path,
    backups: &Path,
    inventory: scan::Inventory,
    stopped: &AtomicBool,
) -> Result<Client_report, String> {
    apply_with_hook(client, native, mirror, backups, inventory, stopped, || {})
}

#[cfg(any(windows, test))]
fn apply_with_hook(
    client: Client,
    native: &Path,
    mirror: &Path,
    backups: &Path,
    inventory: scan::Inventory,
    stopped: &AtomicBool,
    before_finish: impl FnOnce(),
) -> Result<Client_report, String> {
    use crate::config::private_fs;
    use std::fs;
    super::require_unmanaged_client(client)?;
    struct Update {
        relative: PathBuf,
        previous: private_fs::MirrorFileSnapshot,
        contents: Option<Vec<u8>>,
    }
    let safe = |_| "mirror_sync_private_storage_refused".to_string();
    scan::check_cancelled(stopped)?;
    private_fs::prepare_managed_private_dir(mirror).map_err(safe)?;
    let manifest_path = mirror.join(MANIFEST_NAME);
    if fs::symlink_metadata(&manifest_path).is_ok_and(|metadata| metadata.len() > MANIFEST_LIMIT) {
        return Err("mirror_sync_manifest_invalid".into());
    }
    let mut prior_manifest = private_fs::read_mirror_file(&manifest_path).map_err(safe)?;
    let mut old = match prior_manifest.contents.as_deref() {
        Some(contents) => {
            let manifest: Manifest =
                serde_json::from_slice(contents).map_err(|_| "mirror_sync_manifest_invalid")?;
            validate_manifest(&manifest, client, native)?;
            manifest.managed_files
        }
        None => BTreeSet::new(),
    };
    let managed: BTreeSet<_> = inventory.files.keys().cloned().collect();
    let managed_keys: BTreeSet<_> = managed.iter().map(|path| scan::path_key(path)).collect();
    let destination_files = scan::destination_files(client, mirror)?;
    // Enabling this service explicitly selects strict source authority for
    // these instruction roots. Adopt obsolete mirror-only instruction files
    // into the backed-up batch before pruning them. Runtime and vendor paths
    // never enter destination_files or the ownership manifest.
    let mut adopted: BTreeMap<_, _> = old
        .into_iter()
        .map(|path| (scan::path_key(&path), path))
        .collect();
    adopted.extend(
        destination_files
            .into_iter()
            .map(|path| (scan::path_key(&path), path)),
    );
    old = adopted.into_values().collect();
    let mut report = Client_report {
        status: "synced".into(),
        ..Client_report::default()
    };
    let mut pending = Vec::new();
    let mut destination_bytes = 0;
    for (relative, contents) in inventory.files {
        scan::check_cancelled(stopped)?;
        if !client.allows(&relative) {
            return Err("mirror_sync_manifest_invalid".into());
        }
        let target = mirror.join(&relative);
        private_fs::prepare_managed_private_dir(
            target
                .parent()
                .ok_or("mirror_sync_destination_type_invalid")?,
        )
        .map_err(safe)?;
        if let Ok(metadata) = fs::symlink_metadata(&target) {
            destination_bytes += metadata.len();
            if metadata.len() > scan::MAX_FILE_BYTES || destination_bytes > MAX_DESTINATION_BYTES {
                return Err("mirror_sync_size_limit".into());
            }
        }
        let previous = private_fs::read_mirror_file(&target).map_err(safe)?;
        if previous.contents.as_deref() == Some(contents.as_slice()) {
            report.unchanged += 1;
            continue;
        }
        pending.push(Update {
            relative,
            previous,
            contents: Some(contents),
        });
    }
    for relative in old
        .iter()
        .filter(|path| !managed_keys.contains(&scan::path_key(path)))
    {
        scan::check_cancelled(stopped)?;
        let target = mirror.join(relative);
        match fs::symlink_metadata(&target) {
            Ok(metadata) => {
                destination_bytes += metadata.len();
                if metadata.len() > scan::MAX_FILE_BYTES
                    || destination_bytes > MAX_DESTINATION_BYTES
                {
                    return Err("mirror_sync_size_limit".into());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err("mirror_sync_destination_unavailable".into()),
        }
        let previous = private_fs::read_mirror_file(&target).map_err(safe)?;
        pending.push(Update {
            relative: relative.clone(),
            previous,
            contents: None,
        });
    }
    // Inventory and every target snapshot are admitted before any replacement
    // or removal. Recovery copies live outside client instruction discovery.
    let backup_root = if pending
        .iter()
        .any(|update| update.previous.contents.is_some())
    {
        let mut nonce = [0u8; 12];
        getrandom::getrandom(&mut nonce).map_err(|_| "mirror_sync_random_unavailable")?;
        let path = backups.join(client.name()).join(hex::encode(nonce));
        private_fs::prepare_managed_private_dir(&path).map_err(safe)?;
        Some(path)
    } else {
        None
    };
    for update in &pending {
        scan::check_cancelled(stopped)?;
        private_fs::verify_mirror_snapshot(&mirror.join(&update.relative), &update.previous)
            .map_err(safe)?;
        if update.previous.contents.is_some() {
            let backup = backup_root.as_ref().unwrap().join(&update.relative);
            private_fs::prepare_managed_private_dir(backup.parent().unwrap()).map_err(safe)?;
            private_fs::backup_mirror_file(&update.previous, &backup).map_err(safe)?;
        }
    }
    // Admit ownership before installing new paths. If the app stops or a later
    // target changes during this batch, the next reconciliation still knows
    // which partially applied additions belong to this source. Keep old paths
    // until their removals have completed. Platform-equivalent spellings share
    // one entry so a Windows case-only rename cannot delete the desired file.
    let mut admitted: BTreeMap<_, _> = old
        .iter()
        .map(|path| (scan::path_key(path), path.clone()))
        .collect();
    admitted.extend(
        managed
            .iter()
            .map(|path| (scan::path_key(path), path.clone())),
    );
    let admitted = Manifest {
        schema_version: 1,
        client: client.name().into(),
        source_root: native.to_path_buf(),
        managed_files: admitted.into_values().collect(),
    };
    validate_manifest(&admitted, client, native)?;
    let admitted_contents =
        serde_json::to_vec_pretty(&admitted).map_err(|_| "mirror_sync_manifest_invalid")?;
    if admitted_contents.len() as u64 > MANIFEST_LIMIT {
        return Err("mirror_sync_manifest_invalid".into());
    }
    if prior_manifest.contents.as_deref() != Some(admitted_contents.as_slice()) {
        prior_manifest =
            private_fs::replace_mirror_file(&manifest_path, &admitted_contents, &prior_manifest)
                .map_err(safe)?;
    }
    for update in &pending {
        scan::check_cancelled(stopped)?;
        let path = mirror.join(&update.relative);
        if let Some(contents) = &update.contents {
            private_fs::replace_mirror_file(&path, contents, &update.previous).map_err(safe)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if inventory.executable.contains(&update.relative) {
                    fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
                        .map_err(|_| "mirror_sync_private_storage_refused")?;
                }
            }
            report.copied += 1;
        } else {
            private_fs::remove_mirror_file(&path, &update.previous).map_err(safe)?;
            report.deleted += 1;
        }
    }
    before_finish();
    scan::check_cancelled(stopped)?;
    let manifest = Manifest {
        schema_version: 1,
        client: client.name().into(),
        source_root: native.to_path_buf(),
        managed_files: managed,
    };
    let contents =
        serde_json::to_vec_pretty(&manifest).map_err(|_| "mirror_sync_manifest_invalid")?;
    if prior_manifest.contents.as_deref() != Some(contents.as_slice()) {
        private_fs::replace_mirror_file(&manifest_path, &contents, &prior_manifest)
            .map_err(safe)?;
    }
    Ok(report)
}

#[cfg(test)]
pub(super) fn apply_interrupted(
    client: Client,
    native: &Path,
    mirror: &Path,
    backups: &Path,
    inventory: scan::Inventory,
    stopped: &AtomicBool,
) -> Result<Client_report, String> {
    apply_with_hook(client, native, mirror, backups, inventory, stopped, || {
        stopped.store(true, std::sync::atomic::Ordering::SeqCst);
    })
}

#[cfg(not(any(windows, test)))]
pub(super) fn apply(
    _client: Client,
    _native: &Path,
    _mirror: &Path,
    _backups: &Path,
    _inventory: scan::Inventory,
    _stopped: &AtomicBool,
) -> Result<Client_report, String> {
    Err("mirror_sync_platform_unsupported".into())
}
