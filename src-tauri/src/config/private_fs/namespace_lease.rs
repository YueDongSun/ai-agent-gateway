//! Directory namespace leases below an owner-admitted configuration root.
//! A path string is not admission authority. The caller supplies recorded root
//! and leaf object identities from its protected catalog.

use super::{failure, inspect_directory_object, platform, Managed_directory_identity};
use crate::error::AppError;
use std::{
    fs::{File, OpenOptions},
    os::windows::fs::OpenOptionsExt,
    path::{Component, Path, PathBuf, Prefix},
};

// Attribute-only opens are exempt from Windows sharing checks. Listing access
// makes this retained handle participate in the requested share restrictions.
const DIRECTORY_LEASE_ACCESS: u32 = 0x1 | 0x80;
const SHARE_READ: u32 = 1;
const OPEN_DIRECTORY_NO_FOLLOW: u32 = 0x0220_0000;

struct DirectoryGuard {
    path: PathBuf,
    identity: Managed_directory_identity,
    file: File,
}

pub(crate) struct NamespaceLease {
    admitted_root: PathBuf,
    path: PathBuf,
    root_identity: Managed_directory_identity,
    leaf_identity: Managed_directory_identity,
    guards: Vec<DirectoryGuard>,
}

impl NamespaceLease {
    /// Holds each ordinary directory from the admitted root to the leaf.
    /// Sharing excludes new data-write/delete handles and rename operations.
    /// Attribute-only handles are outside Windows share checks: callers must
    /// separately admit ACLs and fence prior writers before relying on them.
    pub fn acquire(
        admitted_root: &Path,
        path: &Path,
        expected_root: Managed_directory_identity,
        expected_leaf: Managed_directory_identity,
    ) -> Result<Self, AppError> {
        validate_path(admitted_root)?;
        validate_path(path)?;
        let relative = path
            .strip_prefix(admitted_root)
            .map_err(|_| failure("namespace_outside_admitted_root"))?;
        let mut paths = vec![admitted_root.to_owned()];
        let mut current = admitted_root.to_owned();
        for part in relative.components() {
            let Component::Normal(name) = part else {
                return Err(failure("namespace_relative_path_invalid"));
            };
            current.push(name);
            paths.push(current.clone());
        }
        let mut guards = Vec::with_capacity(paths.len());
        for path in paths {
            // Reuse the established no-follow directory/type boundary, then
            // retain a stricter sharing handle for this lease's lifetime.
            let existing = platform::open_directory_guard(&path, false)?;
            let identity =
                platform::durable_identity(platform::file_identity_from_file(&existing)?);
            let file = OpenOptions::new()
                .access_mode(DIRECTORY_LEASE_ACCESS)
                .share_mode(SHARE_READ)
                .custom_flags(OPEN_DIRECTORY_NO_FOLLOW)
                .open(&path)
                .map_err(|error| super::io_failure("namespace_guard_open_failed", &error))?;
            verify_handle(&file, identity)?;
            if identity.volume != expected_root.volume {
                return Err(failure("namespace_volume_changed"));
            }
            guards.push(DirectoryGuard {
                path,
                identity,
                file,
            });
        }
        if guards
            .first()
            .is_none_or(|guard| guard.identity != expected_root)
            || guards
                .last()
                .is_none_or(|guard| guard.identity != expected_leaf)
        {
            return Err(failure("namespace_object_changed"));
        }
        let lease = Self {
            admitted_root: admitted_root.to_owned(),
            path: path.to_owned(),
            root_identity: expected_root,
            leaf_identity: expected_leaf,
            guards,
        };
        lease.verify()?;
        Ok(lease)
    }
    pub fn verify(&self) -> Result<(), AppError> {
        for guard in &self.guards {
            verify_handle(&guard.file, guard.identity)?;
            if inspect_directory_object(&guard.path)? != guard.identity {
                return Err(failure("namespace_path_changed"));
            }
        }
        Ok(())
    }
    pub fn admitted_root(&self) -> &Path {
        &self.admitted_root
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn root_identity(&self) -> Managed_directory_identity {
        self.root_identity
    }
    pub fn leaf_identity(&self) -> Managed_directory_identity {
        self.leaf_identity
    }
    /// Resolve a nonempty relative child without permitting aliases or devices.
    pub fn child_path(&self, relative: &Path) -> Result<PathBuf, AppError> {
        self.verify()?;
        if relative.as_os_str().is_empty()
            || relative.is_absolute()
            || relative
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err(failure("namespace_relative_path_invalid"));
        }
        let path = self.path.join(relative);
        validate_path(&path)?;
        Ok(path)
    }
    pub fn directory_objects(&self) -> impl Iterator<Item = (&Path, Managed_directory_identity)> {
        self.guards
            .iter()
            .map(|guard| (guard.path.as_path(), guard.identity))
    }
}

fn verify_handle(file: &File, expected: Managed_directory_identity) -> Result<(), AppError> {
    let metadata = file
        .metadata()
        .map_err(|error| super::io_failure("namespace_metadata_failed", &error))?;
    if !metadata.is_dir()
        || platform::is_linked(&metadata)
        || platform::durable_identity(platform::file_identity_from_file(file)?) != expected
    {
        return Err(failure("namespace_object_changed"));
    }
    Ok(())
}

fn validate_path(path: &Path) -> Result<(), AppError> {
    let raw = path
        .to_str()
        .ok_or_else(|| failure("namespace_path_invalid"))?;
    if raw.chars().any(char::is_control)
        || raw
            .split(['\\', '/'])
            .any(|part| part == "." || part == "..")
    {
        return Err(failure("namespace_path_invalid"));
    }
    let mut components = path.components();
    let drive = match components.next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => drive,
            _ => return Err(failure("namespace_disk_path_required")),
        },
        _ => return Err(failure("namespace_disk_path_required")),
    };
    if drive.eq_ignore_ascii_case(&b'M') || components.next() != Some(Component::RootDir) {
        return Err(failure("namespace_disk_path_forbidden"));
    }
    let mut count = 0;
    for part in components {
        let Component::Normal(name) = part else {
            return Err(failure("namespace_path_invalid"));
        };
        let name = name
            .to_str()
            .ok_or_else(|| failure("namespace_path_invalid"))?;
        if name.is_empty()
            || name.ends_with(['.', ' '])
            || name
                .chars()
                .any(|value| value.is_control() || "<>:\"/\\|?*".contains(value))
        {
            return Err(failure("namespace_component_invalid"));
        }
        let base = name
            .split('.')
            .next()
            .unwrap_or("")
            .trim_end_matches(' ')
            .to_uppercase();
        let reserved = matches!(
            base.as_str(),
            "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$" | "CLOCK$"
        ) || ["COM", "LPT"].iter().any(|prefix| {
            base.strip_prefix(*prefix).is_some_and(|suffix| {
                matches!(
                    suffix,
                    "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                )
            })
        });
        if reserved {
            return Err(failure("namespace_device_component_forbidden"));
        }
        count += 1;
    }
    if count == 0 {
        return Err(failure("namespace_root_too_broad"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::{inspect_directory_object, private_temp_dir, PrivateTempDir};
    use super::*;
    use std::fs;
    use std::os::windows::fs::OpenOptionsExt;

    fn fixture() -> PrivateTempDir {
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/namespace_lease_fixtures");
        fs::create_dir_all(&base).unwrap();
        private_temp_dir(&base).unwrap()
    }
    fn lease(root: &Path, leaf: &Path) -> NamespaceLease {
        NamespaceLease::acquire(
            root,
            leaf,
            inspect_directory_object(root).unwrap(),
            inspect_directory_object(leaf).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn leases_all_components_and_keeps_child_file_io_available() {
        let fixture = fixture();
        let branch = fixture.path().join("branch");
        let leaf = branch.join("leaf");
        fs::create_dir_all(&leaf).unwrap();
        let guard = lease(fixture.path(), &leaf);
        fs::write(leaf.join("history"), b"synthetic history").unwrap();
        assert_eq!(
            fs::read(leaf.join("history")).unwrap(),
            b"synthetic history"
        );
        guard.verify().unwrap();
        assert!(fs::rename(&branch, fixture.path().join("moved")).is_err());
        assert!(fs::rename(&leaf, branch.join("moved_leaf")).is_err());
        assert!(fs::rename(fixture.path(), fixture.path().with_extension("moved")).is_err());
        drop(guard);
        fs::rename(&leaf, branch.join("moved_leaf")).unwrap();
    }

    #[test]
    fn rejects_wrong_identities_outside_paths_and_replaced_leaves() {
        let fixture = fixture();
        let leaf = fixture.path().join("leaf");
        let other = fixture.path().join("other");
        fs::create_dir(&leaf).unwrap();
        fs::create_dir(&other).unwrap();
        let root_id = inspect_directory_object(fixture.path()).unwrap();
        let leaf_id = inspect_directory_object(&leaf).unwrap();
        drop(lease(fixture.path(), &leaf));
        assert!(NamespaceLease::acquire(
            fixture.path(),
            &leaf,
            root_id,
            inspect_directory_object(&other).unwrap()
        )
        .is_err());
        fs::rename(&leaf, fixture.path().join("old_leaf")).unwrap();
        fs::create_dir(&leaf).unwrap();
        assert!(NamespaceLease::acquire(fixture.path(), &leaf, root_id, leaf_id).is_err());
        assert!(NamespaceLease::acquire(&leaf, fixture.path(), leaf_id, root_id).is_err());
    }

    #[test]
    fn directory_write_handles_conflict_in_both_acquisition_orders() {
        let fixture = fixture();
        let guard = lease(fixture.path(), fixture.path());
        let writable = || {
            fs::OpenOptions::new()
                .access_mode(0x4000_0000)
                .share_mode(7)
                .custom_flags(0x0220_0000)
                .open(fixture.path())
        };
        assert!(writable().is_err());
        guard.verify().unwrap();
        drop(guard);
        let write_handle = writable().unwrap();
        let id = inspect_directory_object(fixture.path()).unwrap();
        assert!(NamespaceLease::acquire(fixture.path(), fixture.path(), id, id).is_err());
        drop(write_handle);
        lease(fixture.path(), fixture.path()).verify().unwrap();
    }

    #[test]
    fn invalid_device_and_alias_paths_are_rejected_before_native_open() {
        let fixture = fixture();
        drop(lease(fixture.path(), fixture.path()));
        let id = inspect_directory_object(fixture.path()).unwrap();
        for path in [
            r"M:\forbidden",
            r"\\server\share\root",
            r"\\.\C:\root",
            r"\\?\GLOBALROOT\Device\HarddiskVolume1\root",
            r"I:\root\NUL",
            r"I:\root\file:stream",
            r"I:\root\trailing.",
            r"I:\root\..\other",
        ] {
            assert!(NamespaceLease::acquire(Path::new(path), Path::new(path), id, id).is_err());
        }
    }
}
