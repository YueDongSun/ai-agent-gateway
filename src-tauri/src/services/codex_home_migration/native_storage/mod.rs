//! Native storage behind the migration engine. Platform metadata/alias helpers
//! are supplied by the parent; this layer owns no process implementation.

mod access_times;
mod backup;
mod inventory;
mod journal;
#[cfg(test)]
mod tests;
#[cfg(windows)]
mod windows_platform;

use super::*;
use crate::config::private_fs::MirrorFileSnapshot;
use std::{
    cell::RefCell,
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::Instant,
};

pub(crate) use inventory::{AccessTimeLease, EntryMetadata, NamedStream, NativePlatform};
#[cfg(windows)]
pub(crate) use windows_platform::WindowsStoragePlatform;

pub(crate) struct NativeStorage<'a, P: NativePlatform> {
    paths: MigrationPaths,
    platform: &'a P,
    cancelled: &'a AtomicBool,
    deadline: Instant,
    home_identity: DirectoryIdentity,
    operation_identity: DirectoryIdentity,
    journal_reads: RefCell<BTreeMap<PathBuf, MirrorFileSnapshot>>,
}

impl<'a, P: NativePlatform> NativeStorage<'a, P> {
    /// The parent has exclusively created/verified operation_root before this
    /// read-only constructor. Existing source/destination ACLs are not adopted.
    pub(crate) fn new(
        paths: MigrationPaths,
        platform: &'a P,
        cancelled: &'a AtomicBool,
        deadline: Instant,
    ) -> Result<Self> {
        let home_identity =
            inventory::identity(&paths.home)?.ok_or(MigrationError::SourceUnavailable)?;
        let operation_identity =
            inventory::identity(&paths.operation_root)?.ok_or(MigrationError::SourceUnavailable)?;
        let storage = Self {
            paths,
            platform,
            cancelled,
            deadline,
            home_identity,
            operation_identity,
            journal_reads: RefCell::new(BTreeMap::new()),
        };
        let operation = storage
            .paths
            .operation_root
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(MigrationError::InvalidPaths)?;
        super::plan::validate_paths(&storage, operation, &storage.paths)?;
        Ok(storage)
    }

    fn check(&self) -> Result<()> {
        if self.cancelled.load(std::sync::atomic::Ordering::SeqCst)
            || Instant::now() >= self.deadline
        {
            Err(MigrationError::PlatformFailure)
        } else {
            Ok(())
        }
    }

    fn home_path(&self, path: &Path) -> Result<PathBuf> {
        self.check()?;
        if !inventory::plain(path) || !inventory::within(path, &self.paths.home) {
            return Err(MigrationError::InvalidPaths);
        }
        if inventory::identity(&self.paths.home)? != Some(self.home_identity) {
            return Err(MigrationError::ObjectConflict);
        }
        let canonical_home = inventory::canonical(&self.paths.home)?;
        let canonical = inventory::canonical(path)?;
        if !inventory::within(&canonical, &canonical_home) {
            return Err(MigrationError::InvalidPaths);
        }
        Ok(canonical)
    }

    fn operation_path(&self, path: &Path) -> Result<()> {
        self.home_path(path)?;
        if !inventory::within(path, &self.paths.operation_root)
            || inventory::identity(&self.paths.operation_root)? != Some(self.operation_identity)
        {
            return Err(MigrationError::ObjectConflict);
        }
        Ok(())
    }
}

impl<P: NativePlatform> Storage for NativeStorage<'_, P> {
    fn canonical_path(&self, path: &Path) -> Result<PathBuf> {
        self.home_path(path)
    }
    fn directory_identity(&self, path: &Path) -> Result<Option<DirectoryIdentity>> {
        self.home_path(path)?;
        inventory::identity(path)
    }
    fn inventory(&self, path: &Path) -> Result<HomeSnapshot> {
        if inventory::same(path, &self.paths.source_backup) {
            return backup::verified_snapshot(self);
        }
        if ![
            &self.paths.source,
            &self.paths.destination,
            &self.paths.destination_archive,
        ]
        .iter()
        .any(|allowed| inventory::same(path, allowed))
        {
            return Err(MigrationError::InvalidPaths);
        }
        Ok(inventory::scan(self, path, &[], &[])?.snapshot)
    }
    fn load_journal(&self, path: &Path) -> Result<Option<Journal>> {
        journal::load(self, path)
    }
    fn verify_snapshot(&self, path: &Path, expected: &HomeSnapshot) -> Result<()> {
        let actual = if inventory::same(path, &self.paths.source_backup) {
            backup::verified_snapshot(self)?
        } else {
            if ![
                &self.paths.source,
                &self.paths.destination,
                &self.paths.destination_archive,
            ]
            .iter()
            .any(|allowed| inventory::same(path, allowed))
            {
                return Err(MigrationError::InvalidPaths);
            }
            inventory::scan(self, path, &expected.reparse_entries, &[])?.snapshot
        };
        if actual.root != expected.root
            || actual.image != expected.image
            || actual.entries != expected.entries
            || actual.bytes != expected.bytes
        {
            #[cfg(test)]
            eprintln!(
                "native_snapshot_mismatch source={} root={} image={} entries={} bytes={}",
                inventory::same(path, &self.paths.source),
                actual.root != expected.root,
                actual.image != expected.image,
                actual.entries != expected.entries,
                actual.bytes != expected.bytes
            );
            return Err(MigrationError::SourceChanged);
        }
        Ok(())
    }
    fn store_journal(
        &mut self,
        path: &Path,
        journal: &Journal,
        previous_revision: Option<u64>,
    ) -> Result<()> {
        journal::store(self, path, journal, previous_revision)
    }
    fn backup_home_verified(&mut self, plan: &MigrationPlan) -> Result<HomeSnapshot> {
        backup::create_or_recover(self, plan)
    }
    fn reconcile_backup_source(&mut self, plan: &MigrationPlan) -> Result<()> {
        backup::reconcile_source(self, plan)
    }
    fn rename_no_replace(
        &mut self,
        from: &Path,
        to: &Path,
        expected: DirectoryIdentity,
    ) -> Result<()> {
        self.home_path(from)?;
        self.home_path(to)?;
        let pairs = [
            (&self.paths.destination, &self.paths.destination_archive),
            (&self.paths.source, &self.paths.destination),
            (&self.paths.destination, &self.paths.source),
            (&self.paths.destination_archive, &self.paths.destination),
        ];
        if !pairs.iter().any(|(allowed_from, allowed_to)| {
            inventory::same(from, allowed_from) && inventory::same(to, allowed_to)
        }) {
            return Err(MigrationError::InvalidPaths);
        }
        crate::config::private_fs::rename_directory_object_no_replace(from, to, expected)
            .map_err(|_| MigrationError::ObjectConflict)
    }
    fn retarget_latest(
        &mut self,
        root: &Path,
        expected: DirectoryIdentity,
        alias: &ChromeLatest,
        target: &Path,
    ) -> Result<()> {
        backup::retarget(self, root, expected, alias, target)
    }
    fn validate_installed(&self, journal: &Journal) -> Result<()> {
        self.check()?;
        if journal.plan.paths != self.paths
            || self.directory_identity(&self.paths.source)?.is_some()
            || self.directory_identity(&self.paths.destination)? != Some(journal.plan.source.root)
        {
            return Err(MigrationError::ObjectConflict);
        }
        self.platform.validate_installed(journal)
    }
}
