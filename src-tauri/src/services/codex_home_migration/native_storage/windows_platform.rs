//! Windows metadata and qualified junction operations for whole-home migration.
//! The caller retains writer exclusion and supplies installed-state validation.

mod extended_attributes;
mod file_times;
mod junction;
mod metadata;
#[cfg(all(test, windows))]
mod backup_transfer;
mod privileges;
#[cfg(test)]
mod tests;

use super::super::{
    ChromeLatest, DirectoryIdentity, Journal, MigrationError, MigrationPaths, Result,
};
use super::{AccessTimeLease, EntryMetadata, NamedStream, NativePlatform};
use std::{
    ffi::OsString,
    fs::File,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::Instant,
};

pub(crate) struct WindowsStoragePlatform<'a> {
    paths: MigrationPaths,
    cancelled: &'a AtomicBool,
    deadline: Instant,
    installed_validator: &'a dyn Fn(&Journal) -> Result<()>,
    privileges: privileges::ProcessPrivileges,
    robocopy: PathBuf,
}

impl<'a> WindowsStoragePlatform<'a> {
    pub(crate) fn new(
        paths: MigrationPaths,
        cancelled: &'a AtomicBool,
        deadline: Instant,
        installed_validator: &'a dyn Fn(&Journal) -> Result<()>,
    ) -> Result<Self> {
        let robocopy = metadata::qualified_robocopy(cancelled, deadline)?;
        let privileges = privileges::ProcessPrivileges::acquire()?;
        Ok(Self {
            paths,
            cancelled,
            deadline,
            installed_validator,
            privileges,
            robocopy,
        })
    }

    fn check(&self) -> Result<()> {
        self.check_authority()?;
        if self.cancelled.load(std::sync::atomic::Ordering::SeqCst)
            || Instant::now() >= self.deadline
        {
            return Err(MigrationError::PlatformFailure);
        }
        Ok(())
    }
    fn check_authority(&self) -> Result<()> {
        if self.privileges.active() {
            Ok(())
        } else {
            Err(MigrationError::PlatformFailure)
        }
    }

    fn scope(&self, path: &Path) -> Result<()> {
        self.check()?;
        if !super::inventory::plain(path)
            || !matches!(path.components().next(), Some(std::path::Component::Prefix(prefix)) if matches!(prefix.kind(), std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_)))
            || ![
                &self.paths.source,
                &self.paths.destination,
                &self.paths.source_backup,
                &self.paths.destination_archive,
            ]
            .iter()
            .any(|root| super::inventory::within(path, root))
        {
            return Err(MigrationError::InvalidPaths);
        }
        Ok(())
    }

    /// Call before resuming writers to report restoration failures explicitly;
    /// Drop supplies the same restoration as a cleanup backstop.
    pub(crate) fn restore_privileges(&mut self) -> Result<()> {
        self.privileges.restore()
    }
}

impl NativePlatform for WindowsStoragePlatform<'_> {
    fn metadata(&self, path: &Path) -> Result<EntryMetadata> {
        self.scope(path)?;
        metadata::capture(self, path)
    }
    fn open_stream_for_inventory(&self, path: &Path) -> Result<File> {
        self.scope(path)?;
        crate::config::private_fs::open_inventory_stream(path)
            .map_err(|_| MigrationError::SourceUnavailable)
    }
    fn directory_entries_for_inventory(&self, path: &Path) -> Result<Vec<OsString>> {
        self.scope(path)?;
        crate::config::private_fs::directory_inventory_entries(path)
            .map_err(|_| MigrationError::SourceUnavailable)
    }
    fn lease_access_time(&self, path: &Path) -> Result<Box<dyn AccessTimeLease + '_>> {
        self.scope(path)?;
        Ok(Box::new(metadata::TimeLease::new(self, path)?))
    }
    fn lease_inventory_directory(
        &self,
        path: &Path,
    ) -> Result<Option<Box<dyn AccessTimeLease + '_>>> {
        self.scope(path)?;
        let lease = metadata::TimeLease::new(self, path)?;
        if lease.metadata()?.attributes & 0x410 != 0x10 {
            return Err(MigrationError::ObjectConflict);
        }
        Ok(Some(Box::new(lease)))
    }
    fn robocopy_executable(&self) -> Result<PathBuf> {
        self.check()?;
        if metadata::system_robocopy()? != self.robocopy {
            return Err(MigrationError::PlatformFailure);
        }
        Ok(self.robocopy.clone())
    }
    fn retarget_latest(
        &self,
        root: &Path,
        expected: DirectoryIdentity,
        alias: &ChromeLatest,
        target: &Path,
        alias_identity: DirectoryIdentity,
    ) -> Result<()> {
        self.scope(root)?;
        junction::retarget(self, root, expected, alias, target, alias_identity)
    }
    fn validate_installed(&self, journal: &Journal) -> Result<()> {
        self.check()?;
        if journal.plan.paths != self.paths {
            return Err(MigrationError::InvalidPaths);
        }
        (self.installed_validator)(journal)
    }
}
