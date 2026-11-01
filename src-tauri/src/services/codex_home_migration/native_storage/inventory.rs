use super::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NamedStream {
    /// Native stream name, including leading colon and trailing :$DATA.
    pub name: String,
    pub bytes: u64,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EntryMetadata {
    /// Object identity detects replacement during a scan. It is deliberately
    /// excluded from the image because backup objects have different IDs.
    pub object: DirectoryIdentity,
    pub attributes: u32,
    pub created: u64,
    pub accessed: u64,
    pub written: u64,
    pub unnamed_bytes: u64,
    /// Self-relative security descriptor including owner/group/DACL/SACL.
    /// A platform that cannot inspect required security metadata must fail.
    pub security: Vec<u8>,
    pub extended_attributes: Vec<u8>,
    pub streams: Vec<NamedStream>,
}

/// Parent-owned native helpers. Metadata is read-only and includes an exact
/// system-provided self-relative security descriptor, never a fabricated flag.
pub(crate) trait AccessTimeLease {
    fn metadata(&self) -> Result<EntryMetadata>;
    fn link_target(&self) -> Result<Option<PathBuf>>;
    fn restore_accessed(&mut self, expected: &EntryMetadata) -> Result<()>;
}

pub(crate) trait NativePlatform {
    fn metadata(&self, path: &Path) -> Result<EntryMetadata>;
    /// Keep NTFS access time unchanged on this read handle and validate the
    /// opened object/reparse type. Ordinary std::File::open is not sufficient.
    fn open_stream_for_inventory(&self, path: &Path) -> Result<fs::File>;
    /// Enumerate names through an admitted directory handle with access-time
    /// updates suppressed. Return actual direct child names, excluding dot entries.
    fn directory_entries_for_inventory(&self, path: &Path) -> Result<Vec<std::ffi::OsString>>;
    /// An admitted object held against namespace replacement throughout copy
    /// and reconciliation. Cleanup changes only a previously recorded atime.
    fn lease_access_time(&self, _path: &Path) -> Result<Box<dyn AccessTimeLease + '_>> {
        Err(MigrationError::PlatformFailure)
    }
    /// Platforms with enumeration that changes directory atime supply a held
    /// object. Absence retains strict no-change verification; errors propagate.
    fn lease_inventory_directory(
        &self,
        _path: &Path,
    ) -> Result<Option<Box<dyn AccessTimeLease + '_>>> {
        Ok(None)
    }
    fn robocopy_executable(&self) -> Result<PathBuf>;
    fn retarget_latest(
        &self,
        root: &Path,
        expected: DirectoryIdentity,
        alias: &ChromeLatest,
        target: &Path,
        alias_identity: DirectoryIdentity,
    ) -> Result<()>;
    /// Parent-owned qualification of any installed config/session transforms.
    fn validate_installed(&self, journal: &Journal) -> Result<()>;
}

use sha2::{Digest, Sha256};
use std::{ffi::OsStr, fs, io::Read, path::Component};

const MAX_ENTRIES: u64 = 2_000_000;
const MAX_BYTES: u64 = 4 * 1024 * 1024 * 1024 * 1024;
const MAX_DEPTH: usize = 256;
const MAX_METADATA_BYTES: usize = 1024 * 1024;
const MAX_NAMED_STREAMS: usize = 1024;
const REPARSE_ATTRIBUTE: u32 = 0x400;
const UNADMITTED_ATTRIBUTES: u32 = 0x0040_5000;
const CHROME_PACKAGE: &str = "plugins/cache/openai-bundled/chrome";

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AliasMetadata {
    pub alias: ChromeLatest,
    pub metadata: EntryMetadata,
}

pub(super) struct Scan {
    pub snapshot: HomeSnapshot,
    pub aliases: Vec<AliasMetadata>,
}

pub(super) fn plain(path: &Path) -> bool {
    path.is_absolute()
        && !path
            .components()
            .any(|part| matches!(part, Component::CurDir | Component::ParentDir))
        && !path.to_string_lossy().chars().any(char::is_control)
}

pub(super) fn same(left: &Path, right: &Path) -> bool {
    super::super::plan::same(left, right)
}

pub(super) fn within(path: &Path, root: &Path) -> bool {
    let path = super::super::plan::key(path);
    let root = super::super::plan::key(root);
    path.starts_with(root)
}

pub(super) fn linked(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & REPARSE_ATTRIBUTE != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

pub(super) fn canonical(path: &Path) -> Result<PathBuf> {
    if !plain(path) {
        return Err(MigrationError::InvalidPaths);
    }
    let ancestors: Vec<_> = path.ancestors().collect();
    for ancestor in ancestors.into_iter().rev() {
        match fs::symlink_metadata(ancestor) {
            Ok(meta) if linked(&meta) => return Err(MigrationError::ForbiddenLink),
            Ok(meta) if ancestor != path && !meta.is_dir() => {
                return Err(MigrationError::ObjectConflict)
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(MigrationError::PlatformFailure),
        }
    }
    let mut suffix = Vec::new();
    let mut existing = path;
    loop {
        match fs::symlink_metadata(existing) {
            Ok(_) => break,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                suffix.push(
                    existing
                        .file_name()
                        .ok_or(MigrationError::InvalidPaths)?
                        .to_os_string(),
                );
                existing = existing.parent().ok_or(MigrationError::InvalidPaths)?;
            }
            Err(_) => return Err(MigrationError::PlatformFailure),
        }
    }
    let mut result = fs::canonicalize(existing).map_err(|_| MigrationError::PlatformFailure)?;
    for name in suffix.iter().rev() {
        result.push(name);
    }
    Ok(result)
}

pub(super) fn identity(path: &Path) -> Result<Option<DirectoryIdentity>> {
    canonical(path)?;
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() && !linked(&meta) => {
            crate::config::private_fs::inspect_directory_object(path)
                .map(Some)
                .map_err(|_| MigrationError::ObjectConflict)
        }
        Ok(_) => Err(MigrationError::ObjectConflict),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(MigrationError::PlatformFailure),
    }
}

fn push_bytes(hash: &mut Sha256, value: &[u8]) {
    hash.update((value.len() as u64).to_le_bytes());
    hash.update(value);
}

fn native_bytes(value: &OsStr) -> Vec<u8> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        value.encode_wide().flat_map(u16::to_le_bytes).collect()
    }
    #[cfg(not(windows))]
    {
        value.to_string_lossy().as_bytes().to_vec()
    }
}

fn metadata_hash(hash: &mut Sha256, value: &EntryMetadata) -> Result<()> {
    if value.security.is_empty()
        || value.security.len() > MAX_METADATA_BYTES
        || value.extended_attributes.len() > MAX_METADATA_BYTES
        || value.streams.len() > MAX_NAMED_STREAMS
    {
        return Err(MigrationError::PlatformFailure);
    }
    hash.update(value.attributes.to_le_bytes());
    hash.update(value.created.to_le_bytes());
    hash.update(value.accessed.to_le_bytes());
    hash.update(value.written.to_le_bytes());
    push_bytes(hash, &value.security);
    push_bytes(hash, &value.extended_attributes);
    Ok(())
}

fn stream_name(name: &str) -> bool {
    name.starts_with(':')
        && name.ends_with(":$DATA")
        && name.len() > 7
        && name.len() <= 32768
        && !name[1..name.len() - 6]
            .chars()
            .any(|ch| ch == ':' || ch == '/' || ch == '\\' || ch.is_control())
}

fn check_observed_metadata(actual: &fs::Metadata, expected: &EntryMetadata) -> Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if actual.file_attributes() != expected.attributes
            || actual.creation_time() != expected.created
            || actual.last_access_time() != expected.accessed
            || actual.last_write_time() != expected.written
        {
            #[cfg(test)]
            eprintln!("inventory_internal stage=observed_stream_metadata attributes={} created={} accessed={} written={}", actual.file_attributes() != expected.attributes, actual.creation_time() != expected.created, actual.last_access_time() != expected.accessed, actual.last_write_time() != expected.written);
            return Err(MigrationError::SourceChanged);
        }
    }
    if linked(actual) {
        return Err(MigrationError::ForbiddenLink);
    }
    Ok(())
}

struct InventoryDirectoryLease<'a> {
    depth: usize,
    metadata: EntryMetadata,
    lease: Box<dyn AccessTimeLease + 'a>,
}

struct Scanner<'a, 'b, P: NativePlatform> {
    storage: &'a NativeStorage<'b, P>,
    root: &'a Path,
    expected_aliases: &'a [ChromeLatest],
    virtual_aliases: &'a [AliasMetadata],
    hash: Sha256,
    entries: u64,
    bytes: u64,
    aliases: Vec<AliasMetadata>,
    root_original: Option<EntryMetadata>,
    root_lease: Option<Box<dyn AccessTimeLease + 'a>>,
    directory_leases: Vec<InventoryDirectoryLease<'a>>,
}

impl<P: NativePlatform> Scanner<'_, '_, P> {
    fn add_bytes(&mut self, bytes: u64) -> Result<()> {
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .filter(|count| *count <= MAX_BYTES)
            .ok_or(MigrationError::PlatformFailure)?;
        Ok(())
    }

    fn stream(
        &mut self,
        path: &Path,
        expected_bytes: u64,
        ordinary: Option<&EntryMetadata>,
    ) -> Result<()> {
        self.storage.check()?;
        if expected_bytes > MAX_BYTES - self.bytes {
            return Err(MigrationError::PlatformFailure);
        }
        let mut file = self.storage.platform.open_stream_for_inventory(path)?;
        let before = file
            .metadata()
            .map_err(|_| MigrationError::SourceUnavailable)?;
        if linked(&before) || before.len() != expected_bytes {
            #[cfg(test)]
            eprintln!(
                "inventory_internal stage=stream_before reparse={} size={}",
                linked(&before),
                before.len() != expected_bytes
            );
            return Err(MigrationError::SourceChanged);
        }
        if let Some(metadata) = ordinary {
            check_observed_metadata(&before, metadata)?;
        }
        self.hash.update(expected_bytes.to_le_bytes());
        let mut copied = 0u64;
        let mut buffer = vec![0; 64 * 1024];
        loop {
            self.storage.check()?;
            let count = file
                .read(&mut buffer)
                .map_err(|_| MigrationError::SourceUnavailable)?;
            if count == 0 {
                break;
            }
            copied = copied
                .checked_add(count as u64)
                .filter(|count| *count <= expected_bytes)
                .ok_or(MigrationError::SourceChanged)?;
            self.hash.update(&buffer[..count]);
        }
        if copied != expected_bytes {
            #[cfg(test)]
            eprintln!("inventory_internal stage=stream_read size=true");
            return Err(MigrationError::SourceChanged);
        }
        let after = file
            .metadata()
            .map_err(|_| MigrationError::SourceUnavailable)?;
        if after.len() != expected_bytes {
            #[cfg(test)]
            eprintln!("inventory_internal stage=stream_after size=true");
            return Err(MigrationError::SourceChanged);
        }
        if let Some(metadata) = ordinary {
            check_observed_metadata(&after, metadata)?;
        }
        self.add_bytes(expected_bytes)
    }

    fn alias(
        &mut self,
        relative: &Path,
        metadata: EntryMetadata,
        recorded: Option<ChromeLatest>,
    ) -> Result<()> {
        if !same(relative, &Path::new(CHROME_PACKAGE).join("latest"))
            || !metadata.streams.is_empty()
        {
            return Err(MigrationError::ForbiddenLink);
        }
        let alias = if let Some(recorded) = recorded {
            recorded
        } else {
            let path = self.root.join(relative);
            let raw_target = fs::read_link(&path).map_err(|_| MigrationError::ForbiddenLink)?;
            let (target_relative, expected_target) = if let Some(expected) = self
                .expected_aliases
                .iter()
                .find(|item| same(&item.relative_path, relative))
            {
                let relocated = self.root.join(&expected.target_relative);
                if !same(&raw_target, &expected.raw_target) && !same(&raw_target, &relocated) {
                    return Err(MigrationError::ForbiddenLink);
                }
                (
                    expected.target_relative.clone(),
                    Some(expected.target_identity),
                )
            } else {
                if !plain(&raw_target) || !within(&raw_target, self.root) {
                    return Err(MigrationError::ForbiddenLink);
                }
                let target_relative = super::super::plan::key(&raw_target)
                    .strip_prefix(super::super::plan::key(self.root))
                    .map_err(|_| MigrationError::ForbiddenLink)?
                    .to_path_buf();
                (target_relative, None)
            };
            if target_relative
                .parent()
                .is_none_or(|parent| !same(parent, Path::new(CHROME_PACKAGE)))
                || target_relative
                    .file_name()
                    .is_none_or(|name| name.to_string_lossy().eq_ignore_ascii_case("latest"))
            {
                return Err(MigrationError::ForbiddenLink);
            }
            let target_identity = identity(&self.root.join(&target_relative))?
                .ok_or(MigrationError::ForbiddenLink)?;
            if expected_target.is_some_and(|expected| expected != target_identity) {
                return Err(MigrationError::ObjectConflict);
            }
            if fs::read_link(&path).map_err(|_| MigrationError::ForbiddenLink)? != raw_target {
                #[cfg(test)]
                eprintln!("inventory_internal stage=alias_pointer changed=true");
                return Err(MigrationError::SourceChanged);
            }
            ChromeLatest {
                relative_path: relative.to_path_buf(),
                raw_target,
                target_relative,
                target_identity,
            }
        };
        self.hash.update([2]);
        push_bytes(&mut self.hash, &native_bytes(relative.as_os_str()));
        metadata_hash(&mut self.hash, &metadata)?;
        push_bytes(
            &mut self.hash,
            &native_bytes(alias.target_relative.as_os_str()),
        );
        self.aliases.push(AliasMetadata { alias, metadata });
        Ok(())
    }

    fn walk(&mut self, relative: &Path, depth: usize) -> Result<()> {
        self.storage.check()?;
        if depth > MAX_DEPTH || self.entries >= MAX_ENTRIES {
            return Err(MigrationError::PlatformFailure);
        }
        self.entries += 1;
        if let Some(value) = self
            .virtual_aliases
            .iter()
            .find(|value| same(&value.alias.relative_path, relative))
            .cloned()
        {
            if fs::symlink_metadata(self.root.join(relative)).is_ok() {
                return Err(MigrationError::ObjectConflict);
            }
            return self.alias(relative, value.metadata, Some(value.alias));
        }
        let path = self.root.join(relative);
        let is_root = relative.as_os_str().is_empty();
        let mut metadata = if is_root && self.root_original.is_some() {
            self.root_original
                .clone()
                .ok_or(MigrationError::SourceChanged)?
        } else {
            self.storage.platform.metadata(&path)?
        };
        metadata
            .streams
            .sort_by(|left, right| left.name.cmp(&right.name));
        if metadata.attributes & REPARSE_ATTRIBUTE != 0 {
            self.alias(relative, metadata.clone(), None)?;
            let after = self.storage.platform.metadata(&path)?;
            if after != metadata {
                #[cfg(test)]
                diagnostic_metadata("alias_after_read_link", &metadata, &after);
                return Err(MigrationError::SourceChanged);
            }
            return Ok(());
        }
        if metadata.attributes & (REPARSE_ATTRIBUTE | UNADMITTED_ATTRIBUTES) != 0 {
            return Err(MigrationError::PlatformFailure);
        }
        let is_directory = metadata.attributes & 0x10 != 0;
        let mut directory_lease = if is_directory && !is_root {
            self.storage.platform.lease_inventory_directory(&path)?
        } else {
            None
        };
        if let Some(lease) = &directory_lease {
            if lease.metadata()?.object != metadata.object {
                return Err(MigrationError::ObjectConflict);
            }
        }
        if is_directory && metadata.unnamed_bytes != 0 {
            return Err(MigrationError::PlatformFailure);
        }
        self.hash.update([u8::from(!is_directory)]);
        push_bytes(&mut self.hash, &native_bytes(relative.as_os_str()));
        metadata_hash(&mut self.hash, &metadata)?;
        if !is_directory {
            self.stream(&path, metadata.unnamed_bytes, Some(&metadata))?;
        }
        self.hash
            .update((metadata.streams.len() as u64).to_le_bytes());
        let mut previous = None;
        for stream in &metadata.streams {
            if !stream_name(&stream.name) || previous == Some(&stream.name) {
                return Err(MigrationError::PlatformFailure);
            }
            previous = Some(&stream.name);
            push_bytes(&mut self.hash, stream.name.as_bytes());
            let mut stream_path = path.as_os_str().to_os_string();
            stream_path.push(&stream.name);
            self.stream(Path::new(&stream_path), stream.bytes, None)?;
        }
        if is_directory {
            let directory = identity(&path)?.ok_or(MigrationError::SourceChanged)?;
            if metadata.object != directory {
                #[cfg(test)]
                eprintln!("inventory_internal stage=directory_identity changed=true");
                return Err(MigrationError::SourceChanged);
            }
            let mut names = self
                .storage
                .platform
                .directory_entries_for_inventory(&path)?;
            if names.iter().any(|name| {
                let mut parts = Path::new(name).components();
                !matches!(parts.next(), Some(Component::Normal(_))) || parts.next().is_some()
            }) {
                return Err(MigrationError::InvalidPaths);
            }
            let original_names = names.clone();
            for value in self.virtual_aliases {
                if value
                    .alias
                    .relative_path
                    .parent()
                    .is_some_and(|parent| same(parent, relative))
                {
                    names.push(
                        value
                            .alias
                            .relative_path
                            .file_name()
                            .ok_or(MigrationError::ForbiddenLink)?
                            .to_os_string(),
                    );
                }
            }
            names.sort();
            for name in names {
                self.walk(&relative.join(name), depth + 1)?;
            }
            let mut after_names = self
                .storage
                .platform
                .directory_entries_for_inventory(&path)?;
            let mut original_names = original_names;
            original_names.sort();
            after_names.sort();
            if after_names != original_names || identity(&path)? != Some(directory) {
                #[cfg(test)]
                eprintln!(
                    "inventory_internal stage=directory_final names={} object={}",
                    after_names != original_names,
                    identity(&path)? != Some(directory)
                );
                return Err(MigrationError::SourceChanged);
            }
        }
        let mut after = if let Some(lease) = directory_lease.take() {
            let after = lease.metadata()?;
            let mut non_access = after.clone();
            non_access.accessed = metadata.accessed;
            if non_access != metadata {
                return Err(MigrationError::SourceChanged);
            }
            // Later alias/namespace checks can revisit an already walked
            // directory. Keep its admitted object and original atime until
            // every such read has completed.
            self.directory_leases.push(InventoryDirectoryLease {
                depth,
                metadata,
                lease,
            });
            return Ok(());
        } else if is_root && self.root_lease.is_some() {
            let after = self
                .root_lease
                .as_ref()
                .ok_or(MigrationError::SourceChanged)?
                .metadata()?;
            let mut non_access = after.clone();
            non_access.accessed = metadata.accessed;
            if non_access != metadata {
                return Err(MigrationError::SourceChanged);
            }
            // The root lease spans the scanner's final namespace identity check.
            return Ok(());
        } else {
            self.storage.platform.metadata(&path)?
        };
        after
            .streams
            .sort_by(|left, right| left.name.cmp(&right.name));
        if after != metadata {
            #[cfg(test)]
            diagnostic_metadata("ordinary_final", &metadata, &after);
            return Err(MigrationError::SourceChanged);
        }
        Ok(())
    }
}

#[cfg(test)]
fn diagnostic_metadata(stage: &str, before: &EntryMetadata, after: &EntryMetadata) {
    eprintln!("inventory_internal stage={stage} directory={} reparse={} object={} attrs={} created={} accessed={} written={} size={} security={} ea={} streams={}", before.attributes & 0x10 != 0, before.attributes & 0x400 != 0, before.object != after.object, before.attributes != after.attributes, before.created != after.created, before.accessed != after.accessed, before.written != after.written, before.unnamed_bytes != after.unnamed_bytes, before.security != after.security, before.extended_attributes != after.extended_attributes, before.streams != after.streams);
}

pub(super) fn scan<P: NativePlatform>(
    storage: &NativeStorage<'_, P>,
    root: &Path,
    expected_aliases: &[ChromeLatest],
    virtual_aliases: &[AliasMetadata],
) -> Result<Scan> {
    storage.check()?;
    let root_lease = storage.platform.lease_inventory_directory(root)?;
    let root_original = root_lease
        .as_ref()
        .map(|lease| lease.metadata())
        .transpose()?;
    if root_original
        .as_ref()
        .is_some_and(|metadata| metadata.attributes & 0x410 != 0x10)
    {
        return Err(MigrationError::SourceUnavailable);
    }
    storage.home_path(root)?;
    let root_identity = match &root_original {
        Some(metadata) => metadata.object,
        None => identity(root)?.ok_or(MigrationError::SourceUnavailable)?,
    };
    let mut scanner = Scanner {
        storage,
        root,
        expected_aliases,
        virtual_aliases,
        hash: Sha256::new(),
        entries: 0,
        bytes: 0,
        aliases: Vec::new(),
        root_original,
        root_lease,
        directory_leases: Vec::new(),
    };
    scanner.hash.update(b"aag_codex_home_image_v1");
    scanner.walk(Path::new(""), 0)?;
    if identity(root)? != Some(root_identity) || scanner.aliases.len() > 1 {
        return Err(MigrationError::SourceChanged);
    }
    scanner
        .directory_leases
        .sort_by(|left, right| right.depth.cmp(&left.depth));
    for mut directory in scanner.directory_leases.drain(..) {
        directory.lease.restore_accessed(&directory.metadata)?;
        if directory.lease.metadata()? != directory.metadata {
            return Err(MigrationError::SourceChanged);
        }
        // Close descendants before restoring their still-held parent. The
        // root lease remains held until every descendant has been released.
        drop(directory);
    }
    if let Some(lease) = scanner.root_lease.as_mut() {
        lease.restore_accessed(
            scanner
                .root_original
                .as_ref()
                .ok_or(MigrationError::SourceChanged)?,
        )?;
        if lease.metadata()?
            != *scanner
                .root_original
                .as_ref()
                .ok_or(MigrationError::SourceChanged)?
        {
            return Err(MigrationError::SourceChanged);
        }
    }
    drop(scanner.root_lease.take());
    let reparse_entries = scanner
        .aliases
        .iter()
        .map(|value| value.alias.clone())
        .collect();
    let image = hex::encode(scanner.hash.finalize());
    Ok(Scan {
        snapshot: HomeSnapshot {
            root: root_identity,
            image,
            entries: scanner.entries,
            bytes: scanner.bytes,
            reparse_entries,
        },
        aliases: scanner.aliases,
    })
}
