//! Independent whole-tree transfer verification. The recognized junction is
//! represented by its consumed recovery metadata, not a claimed physical copy.

use super::inventory::{self, AliasMetadata};
use super::*;
use crate::{
    config::private_fs,
    services::owned_process::{run_capture, SpawnSpec},
};
use serde::{Deserialize, Serialize};
use std::fs;

const RECEIPT_NAME: &str = "backup_receipt.json";
const COPY_OUTPUT_LIMIT: usize = 512 * 1024;
const COPY_OPTIONS: &[&str] = &[
    "/E",
    "/COPYALL",
    "/DCOPY:DATE",
    "/R:0",
    "/W:0",
    "/XJ",
    "/NFL",
    "/NDL",
    "/NJH",
    "/NJS",
    "/NP",
];

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BackupReceipt {
    schema_version: u8,
    revision: u64,
    operation_id: String,
    source: HomeSnapshot,
    backup_root: DirectoryIdentity,
    aliases: Vec<AliasMetadata>,
    #[serde(default)]
    access_memento: Option<access_times::Reference>,
    verified: bool,
}

fn receipt_path<P: NativePlatform>(storage: &NativeStorage<'_, P>) -> Result<PathBuf> {
    let path = storage.paths.operation_root.join(RECEIPT_NAME);
    if inventory::same(&path, &storage.paths.journal)
        || inventory::within(&path, &storage.paths.source_backup)
        || inventory::within(&path, &storage.paths.destination_archive)
    {
        return Err(MigrationError::InvalidPaths);
    }
    Ok(path)
}

fn validate_receipt<P: NativePlatform>(
    storage: &NativeStorage<'_, P>,
    receipt: &BackupReceipt,
    revision: u64,
) -> Result<()> {
    let operation = storage
        .paths
        .operation_root
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or(MigrationError::InvalidPaths)?;
    if receipt.schema_version != 1
        || receipt.revision != revision
        || receipt.operation_id != operation
        || receipt.aliases.len() > 1
        || receipt
            .aliases
            .iter()
            .map(|value| value.alias.clone())
            .collect::<Vec<_>>()
            != receipt.source.reparse_entries
        || receipt.backup_root.volume != receipt.source.root.volume
        || receipt.backup_root == receipt.source.root
    {
        return Err(MigrationError::BackupUnverified);
    }
    super::super::plan::validate_snapshot(&storage.paths.source, &receipt.source)?;
    Ok(())
}

fn load_receipt<P: NativePlatform>(
    storage: &NativeStorage<'_, P>,
) -> Result<Option<BackupReceipt>> {
    let path = receipt_path(storage)?;
    let records = journal::records(storage, &path)?;
    let mut latest: Option<BackupReceipt> = None;
    for record in &records {
        let receipt: BackupReceipt = journal::decode(record)?;
        validate_receipt(storage, &receipt, record.revision)?;
        if let Some(previous) = &latest {
            if previous.verified
                || !receipt.verified
                || receipt.revision != previous.revision + 1
                || receipt.schema_version != previous.schema_version
                || receipt.operation_id != previous.operation_id
                || receipt.source != previous.source
                || receipt.backup_root != previous.backup_root
                || receipt.aliases != previous.aliases
                || receipt.access_memento != previous.access_memento
            {
                return Err(MigrationError::BackupUnverified);
            }
        } else if receipt.revision != 1 || receipt.verified {
            return Err(MigrationError::BackupUnverified);
        }
        latest = Some(receipt);
    }
    journal::remember(storage, records);
    Ok(latest)
}

fn persist_receipt<P: NativePlatform>(
    storage: &NativeStorage<'_, P>,
    receipt: &BackupReceipt,
    previous: Option<u64>,
) -> Result<()> {
    validate_receipt(storage, receipt, receipt.revision)?;
    let path = receipt_path(storage)?;
    let data = serde_json::to_vec(receipt).map_err(|_| MigrationError::BackupUnverified)?;
    journal::append(storage, &path, &data, previous)
}

fn verify_tree<P: NativePlatform>(
    storage: &NativeStorage<'_, P>,
    receipt: &BackupReceipt,
) -> Result<HomeSnapshot> {
    if storage.directory_identity(&storage.paths.source_backup)? != Some(receipt.backup_root) {
        return Err(MigrationError::ObjectConflict);
    }
    let scan = inventory::scan(storage, &storage.paths.source_backup, &[], &receipt.aliases)?;
    let actual = scan.snapshot;
    if actual.root != receipt.backup_root
        || actual.image != receipt.source.image
        || actual.entries != receipt.source.entries
        || actual.bytes != receipt.source.bytes
    {
        #[cfg(test)]
        eprintln!(
            "backup_full_image_mismatch root={} image={} entries={} bytes={}",
            actual.root != receipt.backup_root,
            actual.image != receipt.source.image,
            actual.entries != receipt.source.entries,
            actual.bytes != receipt.source.bytes
        );
        return Err(MigrationError::BackupUnverified);
    }
    Ok(actual)
}

pub(super) fn verified_snapshot<P: NativePlatform>(
    storage: &NativeStorage<'_, P>,
) -> Result<HomeSnapshot> {
    let receipt = load_receipt(storage)?.ok_or(MigrationError::BackupUnverified)?;
    if !receipt.verified {
        return Err(MigrationError::BackupUnverified);
    }
    verify_tree(storage, &receipt)
}

fn copy_arguments(source: &Path, destination: &Path) -> Result<Vec<String>> {
    let mut args = vec![
        source
            .to_str()
            .ok_or(MigrationError::InvalidPaths)?
            .to_owned(),
        destination
            .to_str()
            .ok_or(MigrationError::InvalidPaths)?
            .to_owned(),
    ];
    args.extend(COPY_OPTIONS.iter().map(|option| (*option).to_owned()));
    Ok(args)
}

fn copy<P: NativePlatform>(storage: &NativeStorage<'_, P>, executable: &Path) -> Result<()> {
    storage.check()?;
    let args = copy_arguments(&storage.paths.source, &storage.paths.source_backup)?;
    let output = run_capture(
        &SpawnSpec::new(executable, &args),
        storage.cancelled,
        storage.deadline,
        COPY_OUTPUT_LIMIT,
    )
    .map_err(|_| MigrationError::BackupUnverified)?;
    // Never print output: even errors can contain private filenames/paths.
    if output.exit_code >= 8 {
        return Err(MigrationError::BackupUnverified);
    }
    Ok(())
}

pub(super) fn create_or_recover<P: NativePlatform>(
    storage: &NativeStorage<'_, P>,
    plan: &MigrationPlan,
) -> Result<HomeSnapshot> {
    storage.check()?;
    if plan.paths != storage.paths {
        return Err(MigrationError::InvalidPaths);
    }
    storage.operation_path(&storage.paths.source_backup)?;
    if let Some(mut receipt) = load_receipt(storage)? {
        if receipt.operation_id != plan.operation_id || receipt.source != plan.source {
            return Err(MigrationError::BackupUnverified);
        }
        storage.verify_snapshot(&storage.paths.source, &plan.source)?;
        if let Some(entries) = access_times::load(storage, plan, receipt.access_memento.as_ref())? {
            let mut backup = access_times::collect(storage, &storage.paths.source_backup)?;
            access_times::restore(&mut backup, &entries, Some(receipt.backup_root))?;
        }
        // Reconcile the unknown outcome by verifying, never by overwriting a
        // partial tree. A retained incomplete copy needs parent-owned recovery.
        let snapshot = verify_tree(storage, &receipt)?;
        if !receipt.verified {
            receipt.revision += 1;
            receipt.verified = true;
            persist_receipt(storage, &receipt, Some(receipt.revision - 1))?;
        }
        storage.verify_snapshot(&storage.paths.source, &plan.source)?;
        return Ok(snapshot);
    }
    if storage
        .directory_identity(&storage.paths.source_backup)?
        .is_some()
    {
        return Err(MigrationError::ObjectConflict);
    }
    let executable = storage.platform.robocopy_executable()?;
    let executable_metadata =
        fs::symlink_metadata(&executable).map_err(|_| MigrationError::PlatformFailure)?;
    if !inventory::plain(&executable)
        || inventory::linked(&executable_metadata)
        || !executable_metadata.is_file()
        || executable
            .file_name()
            .is_none_or(|name| !name.to_string_lossy().eq_ignore_ascii_case("robocopy.exe"))
        || executable
            .parent()
            .and_then(Path::file_name)
            .is_none_or(|name| !name.to_string_lossy().eq_ignore_ascii_case("System32"))
    {
        return Err(MigrationError::PlatformFailure);
    }
    let source = inventory::scan(
        storage,
        &storage.paths.source,
        &plan.source.reparse_entries,
        &[],
    )?;
    if source.snapshot != plan.source {
        return Err(MigrationError::SourceChanged);
    }
    let mut source_handles = access_times::collect(storage, &storage.paths.source)?;
    let original_metadata = access_times::records(&source_handles);
    storage.verify_snapshot(&storage.paths.source, &plan.source)?;
    // Exclusive creation: an existing directory is never adopted. If a crash
    // precedes receipt installation, recovery preserves the unknown object.
    private_fs::create_private_dir(&storage.paths.source_backup)
        .map_err(|_| MigrationError::PlatformFailure)?;
    let backup_root = storage
        .directory_identity(&storage.paths.source_backup)?
        .ok_or(MigrationError::BackupUnverified)?;
    if backup_root.volume != plan.work_volume
        || backup_root == plan.source.root
        || plan
            .destination
            .as_ref()
            .is_some_and(|home| home.root == backup_root)
    {
        return Err(MigrationError::BackupUnverified);
    }
    let access_memento = access_times::persist(storage, plan, &source_handles)?;
    let mut receipt = BackupReceipt {
        schema_version: 1,
        revision: 1,
        operation_id: plan.operation_id.clone(),
        source: plan.source.clone(),
        backup_root,
        aliases: source.aliases,
        access_memento: Some(access_memento),
        verified: false,
    };
    persist_receipt(storage, &receipt, None)?;
    let copied = copy(storage, &executable);
    #[cfg(test)]
    eprintln!("backup_gate=source_memento_restore");
    let source_restored = access_times::restore(&mut source_handles, &original_metadata, None);
    if copied.is_err() {
        log::error!("migration_backup_copy_failed");
    }
    source_restored?;
    copied?;
    #[cfg(test)]
    eprintln!("backup_gate=source_full_image");
    #[cfg(test)]
    let _ = access_times::diagnose_held(
        "before",
        &source_handles,
        &original_metadata,
        &plan.source.reparse_entries,
    );
    let source_checked = storage.verify_snapshot(&storage.paths.source, &plan.source);
    #[cfg(test)]
    if source_checked.is_err() {
        let _ = access_times::diagnose_held(
            "after",
            &source_handles,
            &original_metadata,
            &plan.source.reparse_entries,
        );
    }
    source_checked?;
    let mut backup_handles = access_times::collect(storage, &storage.paths.source_backup)?;
    #[cfg(test)]
    eprintln!("backup_gate=backup_memento_restore");
    access_times::restore(
        &mut backup_handles,
        &original_metadata,
        Some(receipt.backup_root),
    )?;
    #[cfg(test)]
    eprintln!("backup_gate=backup_full_image");
    let snapshot = verify_tree(storage, &receipt)?;
    #[cfg(test)]
    eprintln!("backup_gate=source_final_image");
    storage.verify_snapshot(&storage.paths.source, &plan.source)?;
    receipt.revision = 2;
    receipt.verified = true;
    persist_receipt(storage, &receipt, Some(1))?;
    Ok(snapshot)
}

/// Called only after the engine admits its owned pending BackupSource journal
/// and repeats writer quiescence. The memento is never a snapshot waiver.
pub(super) fn reconcile_source<P: NativePlatform>(
    storage: &NativeStorage<'_, P>,
    plan: &MigrationPlan,
) -> Result<()> {
    if plan.paths != storage.paths {
        return Err(MigrationError::InvalidPaths);
    }
    let journal = storage
        .load_journal(&storage.paths.journal)?
        .ok_or(MigrationError::JournalConflict)?;
    if journal.plan != *plan
        || journal.phase != Phase::Planned
        || journal.pending != Some(Intent::BackupSource)
        || journal.rolled_back
    {
        return Err(MigrationError::JournalConflict);
    }
    let Some(receipt) = load_receipt(storage)? else {
        return Ok(());
    };
    if receipt.source != plan.source || receipt.operation_id != plan.operation_id {
        return Err(MigrationError::BackupUnverified);
    }
    if let Some(entries) = access_times::load(storage, plan, receipt.access_memento.as_ref())? {
        access_times::restore_source(storage, plan, &entries)?;
    }
    Ok(())
}

pub(super) fn retarget<P: NativePlatform>(
    storage: &NativeStorage<'_, P>,
    root: &Path,
    expected: DirectoryIdentity,
    alias: &ChromeLatest,
    target: &Path,
) -> Result<()> {
    storage.check()?;
    let receipt = load_receipt(storage)?.ok_or(MigrationError::BackupUnverified)?;
    if !receipt.verified
        || expected != receipt.source.root
        || ![&storage.paths.source, &storage.paths.destination]
            .iter()
            .any(|path| inventory::same(root, path))
        || storage.directory_identity(root)? != Some(expected)
        || !inventory::same(target, &root.join(&alias.target_relative))
    {
        return Err(MigrationError::ObjectConflict);
    }
    let recorded = receipt
        .aliases
        .iter()
        .find(|value| value.alias == *alias)
        .ok_or(MigrationError::ForbiddenLink)?;
    let path = root.join(&alias.relative_path);
    let raw = fs::read_link(&path).map_err(|_| MigrationError::ForbiddenLink)?;
    if !inventory::same(&raw, &alias.raw_target)
        && !inventory::same(
            &raw,
            &storage.paths.destination.join(&alias.target_relative),
        )
    {
        return Err(MigrationError::ForbiddenLink);
    }
    if storage.platform.metadata(&path)? != recorded.metadata {
        return Err(MigrationError::SourceChanged);
    }
    let mut current_alias = alias.clone();
    current_alias.raw_target = raw;
    let before = inventory::scan(storage, root, &[current_alias], &[])?.snapshot;
    if before.image != receipt.source.image
        || before.entries != receipt.source.entries
        || before.bytes != receipt.source.bytes
    {
        return Err(MigrationError::SourceChanged);
    }
    storage
        .platform
        .retarget_latest(root, expected, alias, target, recorded.metadata.object)?;
    if storage.platform.metadata(&path)? != recorded.metadata
        || storage.directory_identity(root)? != Some(expected)
        || !inventory::same(
            &fs::read_link(path).map_err(|_| MigrationError::ForbiddenLink)?,
            target,
        )
    {
        return Err(MigrationError::SourceChanged);
    }
    storage.verify_snapshot(root, &receipt.source)
}
