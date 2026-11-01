//! Platform-neutral migration contract. The platform owner supplies private
//! filesystem admission, complete backup fidelity, and no-replace mutations.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub(crate) use crate::config::private_fs::Managed_directory_identity as DirectoryIdentity;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MigrationPaths {
    pub home: PathBuf,
    pub source: PathBuf,
    pub destination: PathBuf,
    pub operation_root: PathBuf,
    pub source_backup: PathBuf,
    pub destination_archive: PathBuf,
    pub journal: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChromeLatest {
    pub relative_path: PathBuf,
    pub raw_target: PathBuf,
    pub target_relative: PathBuf,
    pub target_identity: DirectoryIdentity,
}

/// `image` is an adapter-owned durable inventory receipt, not a client flag
/// or a content dump. It must bind all bytes, timestamps, attributes, security
/// metadata and alternate streams required by the backup contract. Its root
/// identity is separate because a verified backup contains different objects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HomeSnapshot {
    pub root: DirectoryIdentity,
    pub image: String,
    pub entries: u64,
    pub bytes: u64,
    pub reparse_entries: Vec<ChromeLatest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MigrationPlan {
    pub operation_id: String,
    pub paths: MigrationPaths,
    pub source: HomeSnapshot,
    pub destination: Option<HomeSnapshot>,
    pub work_volume: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Phase {
    Planned,
    BackedUp,
    DestinationPreserved,
    SourceInstalled,
    Validated,
    Committed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Intent {
    BackupSource,
    PreserveDestination,
    InstallSource,
    RetargetLatest,
    RestoreSource,
    RestoreDestination,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Journal {
    pub schema_version: u8,
    pub revision: u64,
    pub plan: MigrationPlan,
    pub phase: Phase,
    pub pending: Option<Intent>,
    pub source_backup: Option<HomeSnapshot>,
    pub rolled_back: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MigrationError {
    InvalidPaths,
    DifferentVolume,
    SourceUnavailable,
    SourceChanged,
    ForbiddenLink,
    JournalConflict,
    ObjectConflict,
    BackupUnverified,
    WritersActive,
    CallerCutoverRequired,
    AlreadyCommitted,
    PlatformFailure,
    NotImplemented,
}

pub(crate) type Result<T> = std::result::Result<T, MigrationError>;

/// The caller performs a fresh real writer check on every invocation. A
/// remembered UI confirmation or cached boolean is not an implementation.
/// The owning service must additionally exclude all managed writers for the
/// entire operation. Platform mutations keep their admitted handles/leases.
pub(crate) trait WriterQuiescence {
    fn verify(&mut self, paths: &MigrationPaths) -> Result<()>;
}

/// Readback of the parent-owned database cutover. This module never writes
/// AAG's settings, edits session databases, or claims two histories are merged.
pub(crate) trait CutoverState {
    fn selected_home(&self) -> Result<PathBuf>;
}

pub(crate) trait Storage {
    /// Canonicalize existing ancestors and append missing normal components;
    /// reject reparse traversal. Only recorded Chrome aliases inside inventory
    /// receive the narrower explicit admission below.
    fn canonical_path(&self, path: &Path) -> Result<PathBuf>;
    /// `None` means the path is absent. An existing file or unadmitted link
    /// is a conflict, not an absent directory that the engine may replace.
    fn directory_identity(&self, path: &Path) -> Result<Option<DirectoryIdentity>>;
    fn inventory(&self, path: &Path) -> Result<HomeSnapshot>;
    fn load_journal(&self, path: &Path) -> Result<Option<Journal>>;
    fn verify_snapshot(&self, path: &Path, expected: &HomeSnapshot) -> Result<()>;

    /// Atomic, private, durable compare-and-swap. `previous_revision=None`
    /// creates exclusively and never replaces an unrelated journal.
    fn store_journal(
        &mut self,
        path: &Path,
        journal: &Journal,
        previous_revision: Option<u64>,
    ) -> Result<()>;

    /// Reconcile only access timestamps changed by an owned interrupted backup.
    /// Called after verifying the persisted pending-backup journal and fresh
    /// writer exclusion, before the ordinary complete-source equality check.
    /// An adapter that can change source access times must durably record the
    /// original per-object metadata before copying and restore only after same
    /// object identity and every non-access field are independently verified.
    fn reconcile_backup_source(&mut self, _plan: &MigrationPlan) -> Result<()> {
        Ok(())
    }

    /// Return only after a complete backup is independently verified against
    /// the admitted source image. The platform adapter owns timestamp/ADS/ACL
    /// fidelity and recovery of any operation-owned partial backup. No generic
    /// `fs::copy` implementation may claim this qualification implicitly.
    fn backup_home_verified(&mut self, plan: &MigrationPlan) -> Result<HomeSnapshot>;

    /// Identity-bound same-volume directory rename with NO replacement. A
    /// changed source or an occupied destination is a conflict, never cleanup.
    fn rename_no_replace(
        &mut self,
        from: &Path,
        to: &Path,
        expected: DirectoryIdentity,
    ) -> Result<()>;

    /// Only the already admitted same-package `chrome/latest` alias may move
    /// from old-home to new-home addressing, or back during owned rollback.
    /// Admit only the recorded old or operation-owned new pointer; qualified
    /// idempotent recovery must not adopt a newly replaced unknown alias.
    /// Never traverse/remove its target.
    fn retarget_latest(
        &mut self,
        root: &Path,
        expected: DirectoryIdentity,
        alias: &ChromeLatest,
        target: &Path,
    ) -> Result<()>;

    /// The parent adapter verifies the installed source and preserved target,
    /// including any separately owned session/path reconciliation. This is not
    /// permission to merge databases, discard target history, or reset auth.
    fn validate_installed(&self, journal: &Journal) -> Result<()>;
}
