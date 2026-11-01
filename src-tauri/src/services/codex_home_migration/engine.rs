use super::contract::*;
use super::plan::{same, validate_backup, validate_journal, validate_paths};
use std::path::Path;

fn absent(storage: &impl Storage, path: &Path) -> Result<()> {
    if storage.directory_identity(path)?.is_none() {
        Ok(())
    } else {
        Err(MigrationError::ObjectConflict)
    }
}
fn identity(storage: &impl Storage, path: &Path, expected: DirectoryIdentity) -> Result<()> {
    if storage.directory_identity(path)? == Some(expected) {
        Ok(())
    } else {
        Err(MigrationError::ObjectConflict)
    }
}
fn original_destination(storage: &impl Storage, journal: &Journal, path: &Path) -> Result<()> {
    match &journal.plan.destination {
        Some(expected) => {
            identity(storage, path, expected.root)?;
            storage.verify_snapshot(path, expected)
        }
        None => absent(storage, path),
    }
}
fn backup(storage: &impl Storage, journal: &Journal) -> Result<()> {
    let expected = journal
        .source_backup
        .as_ref()
        .ok_or(MigrationError::BackupUnverified)?;
    validate_backup(&journal.plan, expected)?;
    identity(storage, &journal.plan.paths.source_backup, expected.root)?;
    storage.verify_snapshot(&journal.plan.paths.source_backup, expected)
}
fn installed(storage: &impl Storage, journal: &Journal) -> Result<()> {
    absent(storage, &journal.plan.paths.source)?;
    identity(
        storage,
        &journal.plan.paths.destination,
        journal.plan.source.root,
    )?;
    original_destination(storage, journal, &journal.plan.paths.destination_archive)?;
    backup(storage, journal)
}

fn phase_state(storage: &impl Storage, journal: &Journal) -> Result<()> {
    let plan = &journal.plan;
    match journal.phase {
        Phase::Planned | Phase::BackedUp => {
            storage.verify_snapshot(&plan.paths.source, &plan.source)?;
            original_destination(storage, journal, &plan.paths.destination)?;
            absent(storage, &plan.paths.destination_archive)?;
            if journal.phase == Phase::BackedUp {
                backup(storage, journal)?;
            } else if journal.pending != Some(Intent::BackupSource) {
                absent(storage, &plan.paths.source_backup)?;
            }
            Ok(())
        }
        Phase::DestinationPreserved => {
            storage.verify_snapshot(&plan.paths.source, &plan.source)?;
            absent(storage, &plan.paths.destination)?;
            original_destination(storage, journal, &plan.paths.destination_archive)?;
            backup(storage, journal)
        }
        Phase::SourceInstalled => installed(storage, journal),
        Phase::Validated | Phase::Committed => {
            installed(storage, journal)?;
            storage.validate_installed(journal)
        }
    }
}

fn owned(storage: &impl Storage, journal: &Journal) -> Result<()> {
    validate_journal(storage, journal)?;
    if storage.load_journal(&journal.plan.paths.journal)?.as_ref() != Some(journal) {
        return Err(MigrationError::JournalConflict);
    }
    Ok(())
}

fn persist(
    storage: &mut impl Storage,
    writers: &mut impl WriterQuiescence,
    journal: Journal,
    previous: Option<u64>,
) -> Result<Journal> {
    validate_journal(storage, &journal)?;
    writers.verify(&journal.plan.paths)?;
    storage.store_journal(&journal.plan.paths.journal, &journal, previous)?;
    Ok(journal)
}

fn update(
    storage: &mut impl Storage,
    writers: &mut impl WriterQuiescence,
    mut journal: Journal,
    change: impl FnOnce(&mut Journal),
) -> Result<Journal> {
    let previous = journal.revision;
    journal.revision = previous
        .checked_add(1)
        .ok_or(MigrationError::JournalConflict)?;
    change(&mut journal);
    persist(storage, writers, journal, Some(previous))
}

fn intent(
    storage: &mut impl Storage,
    writers: &mut impl WriterQuiescence,
    journal: Journal,
    pending: Intent,
) -> Result<Journal> {
    update(storage, writers, journal, |record| {
        record.pending = Some(pending)
    })
}

fn finish(
    storage: &mut impl Storage,
    writers: &mut impl WriterQuiescence,
    journal: Journal,
    phase: Phase,
) -> Result<Journal> {
    update(storage, writers, journal, |record| {
        record.phase = phase;
        record.pending = None;
    })
}

pub(crate) fn begin(
    storage: &mut impl Storage,
    writers: &mut impl WriterQuiescence,
    plan: MigrationPlan,
) -> Result<Journal> {
    validate_paths(storage, &plan.operation_id, &plan.paths)?;
    if storage.load_journal(&plan.paths.journal)?.is_some() {
        return Err(MigrationError::JournalConflict);
    }
    let journal = Journal {
        schema_version: 1,
        revision: 1,
        plan,
        phase: Phase::Planned,
        pending: None,
        source_backup: None,
        rolled_back: false,
    };
    validate_journal(storage, &journal)?;
    phase_state(storage, &journal)?;
    persist(storage, writers, journal, None)
}

fn complete_backup(
    storage: &mut impl Storage,
    writers: &mut impl WriterQuiescence,
    journal: Journal,
) -> Result<Journal> {
    if journal.phase != Phase::Planned || journal.pending != Some(Intent::BackupSource) {
        return Err(MigrationError::JournalConflict);
    }
    // An interrupted copy can change source access times before completion.
    // Only an owned pending intent can reconcile that narrowly bound metadata.
    writers.verify(&journal.plan.paths)?;
    storage.reconcile_backup_source(&journal.plan)?;
    storage.verify_snapshot(&journal.plan.paths.source, &journal.plan.source)?;
    original_destination(storage, &journal, &journal.plan.paths.destination)?;
    absent(storage, &journal.plan.paths.destination_archive)?;
    writers.verify(&journal.plan.paths)?;
    // The adapter reconciles an operation-owned incomplete/verified backup.
    // An existing directory with matching contents alone is not ownership.
    let receipt = storage.backup_home_verified(&journal.plan)?;
    validate_backup(&journal.plan, &receipt)?;
    storage.verify_snapshot(&journal.plan.paths.source_backup, &receipt)?;
    storage.verify_snapshot(&journal.plan.paths.source, &journal.plan.source)?;
    update(storage, writers, journal, |record| {
        record.source_backup = Some(receipt);
        record.phase = Phase::BackedUp;
        record.pending = None;
    })
}

pub(crate) fn advance(
    storage: &mut impl Storage,
    writers: &mut impl WriterQuiescence,
    journal: Journal,
) -> Result<Journal> {
    owned(storage, &journal)?;
    if journal.rolled_back {
        return Err(MigrationError::JournalConflict);
    }
    if journal.phase == Phase::Committed {
        return Err(MigrationError::AlreadyCommitted);
    }
    if journal.pending.is_some() {
        return recover(storage, writers, &journal.plan.paths);
    }
    phase_state(storage, &journal)?;
    match journal.phase {
        Phase::Planned => {
            let journal = intent(storage, writers, journal, Intent::BackupSource)?;
            complete_backup(storage, writers, journal)
        }
        Phase::BackedUp => {
            let journal = intent(storage, writers, journal, Intent::PreserveDestination)?;
            if let Some(original) = &journal.plan.destination {
                writers.verify(&journal.plan.paths)?;
                storage.rename_no_replace(
                    &journal.plan.paths.destination,
                    &journal.plan.paths.destination_archive,
                    original.root,
                )?;
            }
            original_destination(storage, &journal, &journal.plan.paths.destination_archive)?;
            absent(storage, &journal.plan.paths.destination)?;
            finish(storage, writers, journal, Phase::DestinationPreserved)
        }
        Phase::DestinationPreserved => {
            let journal = intent(storage, writers, journal, Intent::InstallSource)?;
            writers.verify(&journal.plan.paths)?;
            storage.rename_no_replace(
                &journal.plan.paths.source,
                &journal.plan.paths.destination,
                journal.plan.source.root,
            )?;
            installed(storage, &journal)?;
            finish(storage, writers, journal, Phase::SourceInstalled)
        }
        Phase::SourceInstalled => {
            storage.verify_snapshot(&journal.plan.paths.destination, &journal.plan.source)?;
            let journal = if journal.plan.source.reparse_entries.is_empty() {
                journal
            } else {
                intent(storage, writers, journal, Intent::RetargetLatest)?
            };
            for alias in &journal.plan.source.reparse_entries {
                writers.verify(&journal.plan.paths)?;
                storage.retarget_latest(
                    &journal.plan.paths.destination,
                    journal.plan.source.root,
                    alias,
                    &journal.plan.paths.destination.join(&alias.target_relative),
                )?;
            }
            installed(storage, &journal)?;
            storage.validate_installed(&journal)?;
            finish(storage, writers, journal, Phase::Validated)
        }
        Phase::Validated => Ok(journal),
        Phase::Committed => unreachable!(),
    }
}

fn restored(storage: &impl Storage, journal: &Journal) -> Result<()> {
    storage.verify_snapshot(&journal.plan.paths.source, &journal.plan.source)?;
    original_destination(storage, journal, &journal.plan.paths.destination)?;
    absent(storage, &journal.plan.paths.destination_archive)?;
    if journal.source_backup.is_some() {
        backup(storage, journal)?;
    }
    Ok(())
}

fn known_rollback_locations(storage: &impl Storage, journal: &Journal) -> Result<()> {
    let plan = &journal.plan;
    let source = storage.directory_identity(&plan.paths.source)?;
    let destination = storage.directory_identity(&plan.paths.destination)?;
    let original = plan.destination.as_ref().map(|value| value.root);
    if !((source == Some(plan.source.root) && (destination.is_none() || destination == original))
        || (source.is_none() && destination == Some(plan.source.root)))
    {
        return Err(MigrationError::ObjectConflict);
    }
    let archive = storage.directory_identity(&plan.paths.destination_archive)?;
    if archive.is_some() && archive != original {
        return Err(MigrationError::ObjectConflict);
    }
    if journal.source_backup.is_some() {
        backup(storage, journal)?;
    }
    Ok(())
}

pub(crate) fn recover(
    storage: &mut impl Storage,
    writers: &mut impl WriterQuiescence,
    paths: &MigrationPaths,
) -> Result<Journal> {
    let journal = storage
        .load_journal(&paths.journal)?
        .ok_or(MigrationError::SourceUnavailable)?;
    if journal.plan.paths != *paths {
        return Err(MigrationError::JournalConflict);
    }
    owned(storage, &journal)?;
    if journal.rolled_back {
        restored(storage, &journal)?;
        return Ok(journal);
    }
    let plan = &journal.plan;
    match journal.pending {
        None => {
            phase_state(storage, &journal)?;
            Ok(journal)
        }
        Some(Intent::BackupSource) => complete_backup(storage, writers, journal),
        Some(Intent::PreserveDestination) => {
            storage.verify_snapshot(&plan.paths.source, &plan.source)?;
            backup(storage, &journal)?;
            let current = storage.directory_identity(&plan.paths.destination)?;
            let archived = storage.directory_identity(&plan.paths.destination_archive)?;
            let original = plan.destination.as_ref().map(|value| value.root);
            if current.is_none() && archived == original {
                original_destination(storage, &journal, &plan.paths.destination_archive)?;
                finish(storage, writers, journal, Phase::DestinationPreserved)
            } else if current == original && archived.is_none() {
                original_destination(storage, &journal, &plan.paths.destination)?;
                update(storage, writers, journal, |record| record.pending = None)
            } else {
                Err(MigrationError::ObjectConflict)
            }
        }
        Some(Intent::InstallSource) => {
            backup(storage, &journal)?;
            original_destination(storage, &journal, &plan.paths.destination_archive)?;
            let source = storage.directory_identity(&plan.paths.source)?;
            let destination = storage.directory_identity(&plan.paths.destination)?;
            if source.is_none() && destination == Some(plan.source.root) {
                storage.verify_snapshot(&plan.paths.destination, &plan.source)?;
                finish(storage, writers, journal, Phase::SourceInstalled)
            } else if source == Some(plan.source.root) && destination.is_none() {
                storage.verify_snapshot(&plan.paths.source, &plan.source)?;
                update(storage, writers, journal, |record| record.pending = None)
            } else {
                Err(MigrationError::ObjectConflict)
            }
        }
        Some(Intent::RetargetLatest) => {
            installed(storage, &journal)?;
            let current = storage.inventory(&plan.paths.destination)?;
            let old = current.reparse_entries == plan.source.reparse_entries;
            let relocated = current.reparse_entries.len() == plan.source.reparse_entries.len()
                && current
                    .reparse_entries
                    .iter()
                    .zip(&plan.source.reparse_entries)
                    .all(|(now, original)| {
                        now.relative_path == original.relative_path
                            && now.target_relative == original.target_relative
                            && now.target_identity == original.target_identity
                            && same(
                                &now.raw_target,
                                &plan.paths.destination.join(&original.target_relative),
                            )
                    });
            if relocated {
                storage.validate_installed(&journal)?;
                finish(storage, writers, journal, Phase::Validated)
            } else if old {
                storage.verify_snapshot(&plan.paths.destination, &plan.source)?;
                update(storage, writers, journal, |record| record.pending = None)
            } else {
                Err(MigrationError::ObjectConflict)
            }
        }
        Some(Intent::RestoreSource | Intent::RestoreDestination) => {
            // The parent must first read/restore its DB pointer. Recovery
            // reports this owned pending rollback without publishing a home.
            known_rollback_locations(storage, &journal)?;
            Ok(journal)
        }
    }
}

pub(crate) fn commit(
    storage: &mut impl Storage,
    writers: &mut impl WriterQuiescence,
    cutover: &impl CutoverState,
    journal: Journal,
) -> Result<Journal> {
    owned(storage, &journal)?;
    if journal.phase == Phase::Committed {
        return Err(MigrationError::AlreadyCommitted);
    }
    if journal.rolled_back || journal.phase != Phase::Validated || journal.pending.is_some() {
        return Err(MigrationError::JournalConflict);
    }
    if !same(&cutover.selected_home()?, &journal.plan.paths.destination) {
        return Err(MigrationError::CallerCutoverRequired);
    }
    phase_state(storage, &journal)?;
    finish(storage, writers, journal, Phase::Committed)
}

fn restore_aliases(
    storage: &mut impl Storage,
    writers: &mut impl WriterQuiescence,
    journal: &Journal,
) -> Result<()> {
    if journal.plan.source.reparse_entries.is_empty() {
        return Ok(());
    }
    let current = storage.inventory(&journal.plan.paths.source)?;
    if current.reparse_entries.len() != journal.plan.source.reparse_entries.len() {
        return Err(MigrationError::ObjectConflict);
    }
    for (now, original) in current
        .reparse_entries
        .iter()
        .zip(&journal.plan.source.reparse_entries)
    {
        if now.relative_path != original.relative_path
            || now.target_relative != original.target_relative
            || now.target_identity != original.target_identity
        {
            return Err(MigrationError::ObjectConflict);
        }
        if same(&now.raw_target, &original.raw_target) {
            continue;
        }
        if !same(
            &now.raw_target,
            &journal
                .plan
                .paths
                .destination
                .join(&original.target_relative),
        ) {
            return Err(MigrationError::ObjectConflict);
        }
        writers.verify(&journal.plan.paths)?;
        storage.retarget_latest(
            &journal.plan.paths.source,
            journal.plan.source.root,
            original,
            &original.raw_target,
        )?;
    }
    Ok(())
}

pub(crate) fn rollback(
    storage: &mut impl Storage,
    writers: &mut impl WriterQuiescence,
    cutover: &impl CutoverState,
    mut journal: Journal,
) -> Result<Journal> {
    owned(storage, &journal)?;
    if journal.phase == Phase::Committed {
        return Err(MigrationError::AlreadyCommitted);
    }
    if !same(&cutover.selected_home()?, &journal.plan.paths.source) {
        return Err(MigrationError::CallerCutoverRequired);
    }
    if journal.rolled_back {
        restored(storage, &journal)?;
        return Ok(journal);
    }
    if journal
        .pending
        .is_some_and(|intent| !matches!(intent, Intent::RestoreSource | Intent::RestoreDestination))
    {
        journal = recover(storage, writers, &journal.plan.paths)?;
    }
    known_rollback_locations(storage, &journal)?;
    if storage.directory_identity(&journal.plan.paths.destination)?
        == Some(journal.plan.source.root)
    {
        // Preserve unrecognized changes. The parent must reverse/qualify any
        // separately owned session/config reconciliation before rollback.
        if storage
            .verify_snapshot(&journal.plan.paths.destination, &journal.plan.source)
            .is_err()
        {
            storage.validate_installed(&journal)?;
        }
        absent(storage, &journal.plan.paths.source)?;
        journal = intent(storage, writers, journal, Intent::RestoreSource)?;
        writers.verify(&journal.plan.paths)?;
        storage.rename_no_replace(
            &journal.plan.paths.destination,
            &journal.plan.paths.source,
            journal.plan.source.root,
        )?;
    }
    identity(
        storage,
        &journal.plan.paths.source,
        journal.plan.source.root,
    )?;
    restore_aliases(storage, writers, &journal)?;
    storage.verify_snapshot(&journal.plan.paths.source, &journal.plan.source)?;
    if let Some(original) = journal.plan.destination.as_ref().map(|value| value.root) {
        match (
            storage.directory_identity(&journal.plan.paths.destination)?,
            storage.directory_identity(&journal.plan.paths.destination_archive)?,
        ) {
            (None, Some(archived)) if archived == original => {
                original_destination(storage, &journal, &journal.plan.paths.destination_archive)?;
                journal = intent(storage, writers, journal, Intent::RestoreDestination)?;
                writers.verify(&journal.plan.paths)?;
                storage.rename_no_replace(
                    &journal.plan.paths.destination_archive,
                    &journal.plan.paths.destination,
                    original,
                )?;
            }
            (Some(current), None) if current == original => {}
            _ => return Err(MigrationError::ObjectConflict),
        }
    } else {
        absent(storage, &journal.plan.paths.destination)?;
        absent(storage, &journal.plan.paths.destination_archive)?;
    }
    restored(storage, &journal)?;
    update(storage, writers, journal, |record| {
        record.rolled_back = true;
        record.pending = None;
    })
}
