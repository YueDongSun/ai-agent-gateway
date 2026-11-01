//! Two-home facade cutover. The parent mints the opaque admission/backup/seal
//! types and owns the native backend, writer lease and configuration writes.
//! This core neither reconstructs native histories nor copies them to clients.

use super::contract::{MigrationError, Result};
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OriginalHome<I, B, L> {
    pub root: PathBuf,
    pub quarantine: PathBuf,
    pub identity: I,
    pub backup: B,
    pub logical_payload_id: L,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FacadePayload<I, L> {
    pub relative_path: PathBuf,
    pub identity: I,
    pub logical_payload_id: L,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FacadePlan<I, B, F, L> {
    pub journal: PathBuf,
    pub old_home: OriginalHome<I, B, L>,
    pub new_home: OriginalHome<I, B, L>,
    pub staging: PathBuf,
    pub staging_identity: I,
    pub facade_seal: F,
    /// Fixed order: old-home payload, existing-new-home payload. Fresh A-owned
    /// trees live only at .authority/sources/<32-lowercase-hex-key>/native_home;
    /// the parent seal binds each key to its original logical payload.
    pub payloads: [FacadePayload<I, L>; 2],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FacadePhase {
    Ready,
    SourcePreserved,
    BothPreserved,
    Installed,
    Validated,
    Committed,
    RolledBack,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FacadeIntent {
    PreserveSource,
    PreserveTarget,
    InstallFacade,
    RemoveFacade,
    RestoreTarget,
    RestoreSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FacadeJournal<I, B, F, L> {
    pub revision: u64,
    pub plan: FacadePlan<I, B, F, L>,
    pub phase: FacadePhase,
    pub pending: Option<FacadeIntent>,
    pub rolling_back: bool,
}

pub(crate) type Plan<S> = FacadePlan<
    <S as FacadeStorage>::Identity,
    <S as FacadeStorage>::Backup,
    <S as FacadeStorage>::FacadeSeal,
    <S as FacadeStorage>::LogicalPayloadId,
>;
pub(crate) type Record<S> = FacadeJournal<
    <S as FacadeStorage>::Identity,
    <S as FacadeStorage>::Backup,
    <S as FacadeStorage>::FacadeSeal,
    <S as FacadeStorage>::LogicalPayloadId,
>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OwnerSelection {
    OriginalHomes,
    InstalledFacade,
}

pub(crate) trait FacadeStorage: Sized {
    type Identity: Debug + Clone + Eq;
    type Backup: Debug + Clone + Eq;
    type FacadeSeal: Debug + Clone + Eq;
    type LogicalPayloadId: Debug + Clone + Eq;

    /// Admit exact roots, same-volume nonoverlapping paths, operation identity
    /// and parent-minted proofs. Reject reparse traversal and unknown occupied
    /// journals; never adopt one by parsing matching contents. Both backup
    /// proofs bind complete pre-migration native homes, including metadata.
    fn verify_admission(&self, plan: &Plan<Self>) -> Result<()>;
    fn identity(&self, path: &Path) -> Result<Option<Self::Identity>>;
    fn verify_backup(
        &self,
        home: &OriginalHome<Self::Identity, Self::Backup, Self::LogicalPayloadId>,
    ) -> Result<()>;
    fn verify_original(
        &self,
        path: &Path,
        home: &OriginalHome<Self::Identity, Self::Backup, Self::LogicalPayloadId>,
    ) -> Result<()>;
    /// Independently verify BOTH mapped native trees against their originals:
    /// exact sessions/messages/attachments/order/references, and full data and
    /// metadata except the declared owner/grant policy change. Verify fresh
    /// A-owned objects, A-only authority ACLs, Native B/C read-only ACL readback,
    /// and metadata-only client directories. Backups/quarantines stay inactive.
    fn verify_facade(&self, root: &Path, plan: &Plan<Self>) -> Result<()>;
    /// Actual owner configuration readback under the parent lease, binding the
    /// installed facade identity or the admitted original configuration.
    fn verify_owner_selection(&self, plan: &Plan<Self>, selection: OwnerSelection) -> Result<()>;
    fn load_journal(&self, path: &Path) -> Result<Option<Record<Self>>>;
    /// Private, durable CAS. None creates exclusively; replacement requires the
    /// same admitted journal object plus its previous revision, never revision
    /// equality alone. Unknown outcomes must be reconciled before another CAS.
    fn store_journal(
        &mut self,
        record: &Record<Self>,
        previous_revision: Option<u64>,
    ) -> Result<()>;
    /// Same-volume, identity-bound no-replace rename using admitted handles.
    fn rename_no_replace(
        &mut self,
        from: &Path,
        to: &Path,
        expected: &Self::Identity,
    ) -> Result<()>;
}

pub(crate) trait FacadeWriters<S: FacadeStorage> {
    /// A fresh real check of every old/new/facade writer. The parent also holds
    /// continuous exclusion and stable admission handles for the whole call.
    fn verify(&mut self, plan: &Plan<S>) -> Result<()>;
}

fn payload_source_key(path: &Path) -> Result<&str> {
    let text = path.to_str().ok_or(MigrationError::InvalidPaths)?;
    if text.chars().any(char::is_control)
        || text
            .split(['\\', '/'])
            .any(|part| matches!(part, "" | "." | ".."))
    {
        return Err(MigrationError::InvalidPaths);
    }
    let mut parts = path.components();
    if !matches!(parts.next(), Some(Component::Normal(name)) if name == ".authority")
        || !matches!(parts.next(), Some(Component::Normal(name)) if name == "sources")
    {
        return Err(MigrationError::InvalidPaths);
    }
    let Some(Component::Normal(key)) = parts.next() else {
        return Err(MigrationError::InvalidPaths);
    };
    let key = key.to_str().ok_or(MigrationError::InvalidPaths)?;
    if key.len() != 32
        || !key
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || !matches!(parts.next(), Some(Component::Normal(name)) if name == "native_home")
        || parts.next().is_some()
    {
        return Err(MigrationError::InvalidPaths);
    }
    Ok(key)
}

fn admitted<S: FacadeStorage>(storage: &S, plan: &Plan<S>) -> Result<()> {
    for (payload, home) in [
        (&plan.payloads[0], &plan.old_home),
        (&plan.payloads[1], &plan.new_home),
    ] {
        if payload.logical_payload_id != home.logical_payload_id {
            return Err(MigrationError::InvalidPaths);
        }
    }
    if payload_source_key(&plan.payloads[0].relative_path)?
        == payload_source_key(&plan.payloads[1].relative_path)?
    {
        return Err(MigrationError::ObjectConflict);
    }
    let identities = [
        &plan.old_home.identity,
        &plan.new_home.identity,
        &plan.staging_identity,
        &plan.payloads[0].identity,
        &plan.payloads[1].identity,
    ];
    if plan.old_home.logical_payload_id == plan.new_home.logical_payload_id
        || identities
            .iter()
            .enumerate()
            .any(|(index, value)| identities[..index].contains(value))
    {
        return Err(MigrationError::ObjectConflict);
    }
    storage.verify_admission(plan)
}

fn layout<S: FacadeStorage>(storage: &S, plan: &Plan<S>, phase: FacadePhase) -> Result<()> {
    let old_moved = !matches!(phase, FacadePhase::Ready | FacadePhase::RolledBack);
    let new_moved = matches!(
        phase,
        FacadePhase::BothPreserved
            | FacadePhase::Installed
            | FacadePhase::Validated
            | FacadePhase::Committed
    );
    let installed = matches!(
        phase,
        FacadePhase::Installed | FacadePhase::Validated | FacadePhase::Committed
    );
    let expected = [
        (
            &plan.old_home.root,
            (!old_moved).then_some(&plan.old_home.identity),
        ),
        (
            &plan.old_home.quarantine,
            old_moved.then_some(&plan.old_home.identity),
        ),
        (
            &plan.new_home.root,
            if installed {
                Some(&plan.staging_identity)
            } else {
                (!new_moved).then_some(&plan.new_home.identity)
            },
        ),
        (
            &plan.new_home.quarantine,
            new_moved.then_some(&plan.new_home.identity),
        ),
        (
            &plan.staging,
            (!installed).then_some(&plan.staging_identity),
        ),
    ];
    for (path, identity) in expected {
        if storage.identity(path)?.as_ref() != identity {
            return Err(MigrationError::ObjectConflict);
        }
    }
    for (home, moved) in [(&plan.old_home, old_moved), (&plan.new_home, new_moved)] {
        storage.verify_backup(home)?;
        storage.verify_original(if moved { &home.quarantine } else { &home.root }, home)?;
    }
    storage.verify_facade(
        if installed {
            &plan.new_home.root
        } else {
            &plan.staging
        },
        plan,
    )
}

fn rename_spec<S: FacadeStorage>(
    record: &Record<S>,
    intent: FacadeIntent,
) -> Result<(&Path, &Path, &S::Identity, FacadePhase)> {
    use FacadeIntent::*;
    use FacadePhase::*;
    let plan = &record.plan;
    let result = match (record.phase, intent) {
        (Ready, PreserveSource) => (
            &plan.old_home.root,
            &plan.old_home.quarantine,
            &plan.old_home.identity,
            SourcePreserved,
        ),
        (SourcePreserved, PreserveTarget) => (
            &plan.new_home.root,
            &plan.new_home.quarantine,
            &plan.new_home.identity,
            BothPreserved,
        ),
        (BothPreserved, InstallFacade) => (
            &plan.staging,
            &plan.new_home.root,
            &plan.staging_identity,
            Installed,
        ),
        (Installed | Validated, RemoveFacade) => (
            &plan.new_home.root,
            &plan.staging,
            &plan.staging_identity,
            BothPreserved,
        ),
        (BothPreserved, RestoreTarget) => (
            &plan.new_home.quarantine,
            &plan.new_home.root,
            &plan.new_home.identity,
            SourcePreserved,
        ),
        (SourcePreserved, RestoreSource) => (
            &plan.old_home.quarantine,
            &plan.old_home.root,
            &plan.old_home.identity,
            Ready,
        ),
        _ => return Err(MigrationError::JournalConflict),
    };
    if record.rolling_back != matches!(intent, RemoveFacade | RestoreTarget | RestoreSource) {
        return Err(MigrationError::JournalConflict);
    }
    Ok((result.0.as_path(), result.1.as_path(), result.2, result.3))
}

fn owned<S: FacadeStorage>(storage: &S, record: &Record<S>) -> Result<()> {
    admitted(storage, &record.plan)?;
    if record.revision == 0
        || (record.phase == FacadePhase::Committed && record.rolling_back)
        || (record.phase == FacadePhase::RolledBack && !record.rolling_back)
        || storage.load_journal(&record.plan.journal)?.as_ref() != Some(record)
    {
        return Err(MigrationError::JournalConflict);
    }
    if let Some(intent) = record.pending {
        rename_spec::<S>(record, intent)?;
    }
    Ok(())
}

fn persist<S: FacadeStorage>(
    storage: &mut S,
    writers: &mut impl FacadeWriters<S>,
    record: Record<S>,
    previous: Option<u64>,
) -> Result<Record<S>> {
    admitted(storage, &record.plan)?;
    writers.verify(&record.plan)?;
    storage.store_journal(&record, previous)?;
    Ok(record)
}

fn update<S: FacadeStorage>(
    storage: &mut S,
    writers: &mut impl FacadeWriters<S>,
    mut record: Record<S>,
    change: impl FnOnce(&mut Record<S>),
) -> Result<Record<S>> {
    let previous = record.revision;
    record.revision = previous
        .checked_add(1)
        .ok_or(MigrationError::JournalConflict)?;
    change(&mut record);
    persist(storage, writers, record, Some(previous))
}

fn rename<S: FacadeStorage>(
    storage: &mut S,
    writers: &mut impl FacadeWriters<S>,
    record: Record<S>,
    intent: FacadeIntent,
) -> Result<Record<S>> {
    layout(storage, &record.plan, record.phase)?;
    rename_spec::<S>(&record, intent)?;
    let record = update(storage, writers, record, |entry| {
        entry.pending = Some(intent)
    })?;
    owned(storage, &record)?;
    let (from, to, identity, next) = rename_spec::<S>(&record, intent)?;
    writers.verify(&record.plan)?;
    storage.rename_no_replace(from, to, identity)?;
    layout(storage, &record.plan, next)?;
    update(storage, writers, record, |entry| {
        entry.phase = next;
        entry.pending = None;
    })
}

pub(crate) fn begin<S: FacadeStorage>(
    storage: &mut S,
    writers: &mut impl FacadeWriters<S>,
    plan: Plan<S>,
) -> Result<Record<S>> {
    admitted(storage, &plan)?;
    if storage.load_journal(&plan.journal)?.is_some() {
        return Err(MigrationError::JournalConflict);
    }
    layout(storage, &plan, FacadePhase::Ready)?;
    persist(
        storage,
        writers,
        FacadeJournal {
            revision: 1,
            plan,
            phase: FacadePhase::Ready,
            pending: None,
            rolling_back: false,
        },
        None,
    )
}

pub(crate) fn advance<S: FacadeStorage>(
    storage: &mut S,
    writers: &mut impl FacadeWriters<S>,
    record: Record<S>,
) -> Result<Record<S>> {
    owned(storage, &record)?;
    if record.phase == FacadePhase::Committed {
        return Err(MigrationError::AlreadyCommitted);
    }
    if record.rolling_back {
        return Err(MigrationError::JournalConflict);
    }
    if record.pending.is_some() {
        return recover(storage, writers, &record.plan);
    }
    use FacadePhase::*;
    match record.phase {
        Ready => rename(storage, writers, record, FacadeIntent::PreserveSource),
        SourcePreserved => rename(storage, writers, record, FacadeIntent::PreserveTarget),
        BothPreserved => rename(storage, writers, record, FacadeIntent::InstallFacade),
        Installed => {
            layout(storage, &record.plan, Installed)?;
            update(storage, writers, record, |entry| entry.phase = Validated)
        }
        Validated => {
            layout(storage, &record.plan, Validated)?;
            Ok(record)
        }
        _ => Err(MigrationError::JournalConflict),
    }
}

pub(crate) fn recover<S: FacadeStorage>(
    storage: &mut S,
    writers: &mut impl FacadeWriters<S>,
    plan: &Plan<S>,
) -> Result<Record<S>> {
    let record = storage
        .load_journal(&plan.journal)?
        .ok_or(MigrationError::SourceUnavailable)?;
    if record.plan != *plan {
        return Err(MigrationError::JournalConflict);
    }
    owned(storage, &record)?;
    let Some(intent) = record.pending else {
        layout(storage, plan, record.phase)?;
        return Ok(record);
    };
    let (_, _, _, next) = rename_spec::<S>(&record, intent)?;
    // Reconcile the complete layout, never infer ownership from equal contents.
    // A pre-rename layout clears its intent without retrying the rename.
    let phase = if layout(storage, plan, next).is_ok() {
        next
    } else {
        layout(storage, plan, record.phase)?;
        record.phase
    };
    update(storage, writers, record, |entry| {
        entry.phase = phase;
        entry.pending = None;
    })
}

pub(crate) fn commit<S: FacadeStorage>(
    storage: &mut S,
    writers: &mut impl FacadeWriters<S>,
    record: Record<S>,
) -> Result<Record<S>> {
    owned(storage, &record)?;
    if record.phase == FacadePhase::Committed {
        return Err(MigrationError::AlreadyCommitted);
    }
    if record.phase != FacadePhase::Validated || record.pending.is_some() || record.rolling_back {
        return Err(MigrationError::JournalConflict);
    }
    layout(storage, &record.plan, FacadePhase::Validated)?;
    storage.verify_owner_selection(&record.plan, OwnerSelection::InstalledFacade)?;
    update(storage, writers, record, |entry| {
        entry.phase = FacadePhase::Committed
    })
}

pub(crate) fn rollback<S: FacadeStorage>(
    storage: &mut S,
    writers: &mut impl FacadeWriters<S>,
    mut record: Record<S>,
) -> Result<Record<S>> {
    owned(storage, &record)?;
    if record.phase == FacadePhase::Committed {
        return Err(MigrationError::AlreadyCommitted);
    }
    storage.verify_owner_selection(&record.plan, OwnerSelection::OriginalHomes)?;
    if record.pending.is_some() {
        record = recover(storage, writers, &record.plan)?;
    }
    layout(storage, &record.plan, record.phase)?;
    if record.phase == FacadePhase::RolledBack {
        return Ok(record);
    }
    if !record.rolling_back {
        record = update(storage, writers, record, |entry| entry.rolling_back = true)?;
    }
    while record.phase != FacadePhase::Ready {
        let intent = match record.phase {
            FacadePhase::Installed | FacadePhase::Validated => FacadeIntent::RemoveFacade,
            FacadePhase::BothPreserved => FacadeIntent::RestoreTarget,
            FacadePhase::SourcePreserved => FacadeIntent::RestoreSource,
            _ => return Err(MigrationError::JournalConflict),
        };
        record = rename(storage, writers, record, intent)?;
    }
    layout(storage, &record.plan, FacadePhase::Ready)?;
    update(storage, writers, record, |entry| {
        entry.phase = FacadePhase::RolledBack
    })
}

#[cfg(test)]
mod tests;
