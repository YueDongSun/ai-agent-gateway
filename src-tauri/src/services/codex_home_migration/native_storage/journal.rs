//! Private compare-and-swap journal persistence.

use super::*;
use crate::config::private_fs;
use serde::de::DeserializeOwned;
use std::fs;

pub(super) const MAX_RECORD_BYTES: u64 = 2 * 1024 * 1024;
pub(super) const MAX_REVISIONS: u64 = 128;

pub(super) struct Record {
    pub path: PathBuf,
    pub revision: u64,
    pub snapshot: MirrorFileSnapshot,
}

pub(super) fn revision_path(path: &Path, revision: u64) -> Result<PathBuf> {
    if revision == 0 || revision > MAX_REVISIONS {
        return Err(MigrationError::JournalConflict);
    }
    if revision == 1 {
        return Ok(path.to_path_buf());
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(MigrationError::InvalidPaths)?;
    Ok(path.with_file_name(format!("{name}.revision_{revision:020}")))
}

fn private_record(path: &Path) -> Result<MirrorFileSnapshot> {
    private_fs::validate_external_file_path(path).map_err(|_| MigrationError::JournalConflict)?;
    if fs::symlink_metadata(path)
        .map_err(|_| MigrationError::JournalConflict)?
        .len()
        > MAX_RECORD_BYTES
    {
        return Err(MigrationError::JournalConflict);
    }
    let snapshot =
        private_fs::read_mirror_file(path).map_err(|_| MigrationError::JournalConflict)?;
    if snapshot
        .contents
        .as_ref()
        .is_none_or(|bytes| bytes.len() as u64 > MAX_RECORD_BYTES)
        || snapshot.recovery_path.is_some()
    {
        return Err(MigrationError::JournalConflict);
    }
    Ok(snapshot)
}

/// Only committed immutable revision names enter the chain. Private-fs
/// temporary files never represent a committed revision and are not loaded.
pub(super) fn records<P: NativePlatform>(
    storage: &NativeStorage<'_, P>,
    path: &Path,
) -> Result<Vec<Record>> {
    storage.operation_path(path)?;
    let parent = path.parent().ok_or(MigrationError::InvalidPaths)?;
    let base = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(MigrationError::InvalidPaths)?;
    let prefix = format!("{base}.revision_");
    let mut versions = std::collections::BTreeSet::new();
    for item in fs::read_dir(parent).map_err(|_| MigrationError::JournalConflict)? {
        storage.check()?;
        let item = item.map_err(|_| MigrationError::JournalConflict)?;
        let name = item.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name == base {
            versions.insert(1);
        } else if let Some(suffix) = name.strip_prefix(&prefix) {
            if suffix.len() != 20 || !suffix.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(MigrationError::JournalConflict);
            }
            let version: u64 = suffix
                .parse()
                .map_err(|_| MigrationError::JournalConflict)?;
            if !(2..=MAX_REVISIONS).contains(&version)
                || revision_path(path, version)?.file_name() != Some(item.file_name().as_os_str())
            {
                return Err(MigrationError::JournalConflict);
            }
            versions.insert(version);
        }
    }
    let mut output = Vec::new();
    for (offset, revision) in versions.into_iter().enumerate() {
        if revision != offset as u64 + 1 {
            return Err(MigrationError::JournalConflict);
        }
        let path = revision_path(path, revision)?;
        let snapshot = private_record(&path)?;
        output.push(Record {
            path,
            revision,
            snapshot,
        });
    }
    Ok(output)
}

pub(super) fn decode<T: DeserializeOwned>(record: &Record) -> Result<T> {
    serde_json::from_slice(
        record
            .snapshot
            .contents
            .as_deref()
            .ok_or(MigrationError::JournalConflict)?,
    )
    .map_err(|_| MigrationError::JournalConflict)
}

pub(super) fn remember<P: NativePlatform>(storage: &NativeStorage<'_, P>, records: Vec<Record>) {
    let mut admitted = storage.journal_reads.borrow_mut();
    for record in records {
        admitted.insert(record.path, record.snapshot);
    }
}

pub(super) fn append<P: NativePlatform>(
    storage: &NativeStorage<'_, P>,
    path: &Path,
    data: &[u8],
    previous: Option<u64>,
) -> Result<()> {
    storage.check()?;
    if data.len() as u64 > MAX_RECORD_BYTES {
        return Err(MigrationError::JournalConflict);
    }
    let records = records(storage, path)?;
    if records.last().map(|record| record.revision) != previous {
        return Err(MigrationError::JournalConflict);
    }
    {
        let admitted = storage.journal_reads.borrow();
        for record in &records {
            let expected = admitted
                .get(&record.path)
                .ok_or(MigrationError::JournalConflict)?;
            private_fs::verify_mirror_snapshot(&record.path, expected)
                .map_err(|_| MigrationError::JournalConflict)?;
        }
    }
    let revision = previous
        .unwrap_or(0)
        .checked_add(1)
        .ok_or(MigrationError::JournalConflict)?;
    let next = revision_path(path, revision)?;
    storage.operation_path(&next)?;
    let empty = private_fs::read_mirror_file(&next).map_err(|_| MigrationError::JournalConflict)?;
    if empty.contents.is_some() {
        return Err(MigrationError::JournalConflict);
    }
    storage.check()?;
    #[cfg(any(windows, all(unix, test)))]
    {
        // Absent-target branch installs the complete synced temporary with
        // NO_REPLACE. It never moves aside a prior committed revision.
        let written = private_fs::replace_mirror_file(&next, data, &empty)
            .map_err(|_| MigrationError::JournalConflict)?;
        if written.contents.as_deref() != Some(data) || written.recovery_path.is_some() {
            return Err(MigrationError::JournalConflict);
        }
        storage.journal_reads.borrow_mut().insert(next, written);
        Ok(())
    }
    #[cfg(not(any(windows, all(unix, test))))]
    {
        Err(MigrationError::PlatformFailure)
    }
}

fn transition(previous: Option<&Journal>, next: &Journal) -> Result<()> {
    let phase = |value: Phase| match value {
        Phase::Planned => 0,
        Phase::BackedUp => 1,
        Phase::DestinationPreserved => 2,
        Phase::SourceInstalled => 3,
        Phase::Validated => 4,
        Phase::Committed => 5,
    };
    match previous {
        None if next.revision == 1
            && next.phase == Phase::Planned
            && next.pending.is_none()
            && next.source_backup.is_none()
            && !next.rolled_back =>
        {
            Ok(())
        }
        Some(previous)
            if !previous.rolled_back
                && previous.phase != Phase::Committed
                && next.revision == previous.revision + 1
                && next.plan == previous.plan
                && phase(next.phase) >= phase(previous.phase)
                && phase(next.phase) <= phase(previous.phase) + 1
                && previous
                    .source_backup
                    .as_ref()
                    .is_none_or(|backup| next.source_backup.as_ref() == Some(backup)) =>
        {
            Ok(())
        }
        _ => Err(MigrationError::JournalConflict),
    }
}

fn decode_chain<P: NativePlatform>(
    storage: &NativeStorage<'_, P>,
    records: &[Record],
) -> Result<Option<Journal>> {
    let mut latest = None;
    for record in records {
        let journal: Journal = decode(record)?;
        if journal.revision != record.revision || journal.plan.paths != storage.paths {
            return Err(MigrationError::JournalConflict);
        }
        super::super::plan::validate_journal(storage, &journal)?;
        transition(latest.as_ref(), &journal)?;
        latest = Some(journal);
    }
    Ok(latest)
}

pub(super) fn load<P: NativePlatform>(
    storage: &NativeStorage<'_, P>,
    path: &Path,
) -> Result<Option<Journal>> {
    if !inventory::same(path, &storage.paths.journal) {
        return Err(MigrationError::InvalidPaths);
    }
    let records = records(storage, path)?;
    let latest = decode_chain(storage, &records)?;
    remember(storage, records);
    Ok(latest)
}

pub(super) fn store<P: NativePlatform>(
    storage: &NativeStorage<'_, P>,
    path: &Path,
    journal: &Journal,
    previous: Option<u64>,
) -> Result<()> {
    if !inventory::same(path, &storage.paths.journal) || journal.plan.paths != storage.paths {
        return Err(MigrationError::InvalidPaths);
    }
    super::super::plan::validate_journal(storage, journal)?;
    // Decode without replacing the caller's original object/byte admissions.
    let records = records(storage, path)?;
    let latest = decode_chain(storage, &records)?;
    if latest.as_ref().map(|value| value.revision) != previous {
        return Err(MigrationError::JournalConflict);
    }
    transition(latest.as_ref(), journal)?;
    let data = serde_json::to_vec(journal).map_err(|_| MigrationError::JournalConflict)?;
    append(storage, path, &data, previous)
}
