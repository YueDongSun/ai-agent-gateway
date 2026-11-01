//! Consumed, private metadata mementos and identity-bound access-time cleanup.
//! No content/timestamp mismatch is waived by the final full-image verifier.

use super::*;
use crate::config::private_fs;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs, path::Component};

const DIRECTORY_NAME: &str = "access_memento";
const HEADER_NAME: &str = "manifest.json";
const PAGE_BYTES: usize = 1024 * 1024;
const MAX_RECORD_BYTES: usize = 2 * 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 512 * 1024 * 1024;
const MAX_ENTRIES: usize = 2_000_000;
const MAX_PAGES: usize = 4096;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Entry {
    relative: PathBuf,
    metadata: EntryMetadata,
    link: Option<PathBuf>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    schema_version: u8,
    operation_id: String,
    source: HomeSnapshot,
    root: DirectoryIdentity,
    pages: usize,
    entries: usize,
    image: String,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Reference {
    root: DirectoryIdentity,
    image: String,
}

pub(super) struct Held<'a> {
    entry: Entry,
    lease: Box<dyn AccessTimeLease + 'a>,
}

fn base<P: NativePlatform>(storage: &NativeStorage<'_, P>) -> Result<PathBuf> {
    let path = storage.paths.operation_root.join(DIRECTORY_NAME);
    storage.operation_path(&path)?;
    if [
        &storage.paths.source_backup,
        &storage.paths.destination_archive,
        &storage.paths.journal,
    ]
    .iter()
    .any(|reserved| inventory::within(&path, reserved) || inventory::within(reserved, &path))
    {
        return Err(MigrationError::InvalidPaths);
    }
    Ok(path)
}

fn relative(path: &Path) -> bool {
    path.as_os_str().is_empty()
        || (path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
            && !path.to_string_lossy().chars().any(char::is_control))
}

fn same_link(left: &Option<PathBuf>, right: &Option<PathBuf>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => inventory::same(left, right),
        _ => false,
    }
}

fn same_non_access(actual: &EntryMetadata, expected: &EntryMetadata) -> bool {
    let mut normalized = actual.clone();
    normalized.accessed = expected.accessed;
    normalized == *expected
}

fn put<P: NativePlatform>(storage: &NativeStorage<'_, P>, path: &Path, bytes: &[u8]) -> Result<()> {
    storage.operation_path(path)?;
    if bytes.len() > MAX_RECORD_BYTES {
        return Err(MigrationError::BackupUnverified);
    }
    let absent = private_fs::read_mirror_file(path).map_err(|_| MigrationError::JournalConflict)?;
    if absent.contents.is_some() {
        return Err(MigrationError::ObjectConflict);
    }
    #[cfg(any(windows, all(unix, test)))]
    {
        let written = private_fs::replace_mirror_file(path, bytes, &absent)
            .map_err(|_| MigrationError::JournalConflict)?;
        if written.contents.as_deref() != Some(bytes) || written.recovery_path.is_some() {
            return Err(MigrationError::JournalConflict);
        }
        Ok(())
    }
    #[cfg(not(any(windows, all(unix, test))))]
    {
        Err(MigrationError::PlatformFailure)
    }
}

fn get<P: NativePlatform>(storage: &NativeStorage<'_, P>, path: &Path) -> Result<Vec<u8>> {
    storage.operation_path(path)?;
    if fs::symlink_metadata(path)
        .map_err(|_| MigrationError::BackupUnverified)?
        .len()
        > MAX_RECORD_BYTES as u64
    {
        return Err(MigrationError::BackupUnverified);
    }
    let snapshot =
        private_fs::read_mirror_file(path).map_err(|_| MigrationError::BackupUnverified)?;
    let bytes = snapshot.contents.ok_or(MigrationError::BackupUnverified)?;
    if bytes.len() > MAX_RECORD_BYTES || snapshot.recovery_path.is_some() {
        return Err(MigrationError::BackupUnverified);
    }
    Ok(bytes)
}

pub(super) fn collect<'a, P: NativePlatform>(
    storage: &'a NativeStorage<'_, P>,
    root: &Path,
) -> Result<Vec<Held<'a>>> {
    fn walk<'a, P: NativePlatform>(
        storage: &'a NativeStorage<'_, P>,
        root: &Path,
        path: PathBuf,
        output: &mut Vec<Held<'a>>,
        depth: usize,
    ) -> Result<()> {
        storage.check()?;
        if output.len() >= MAX_ENTRIES || depth > 256 {
            return Err(MigrationError::BackupUnverified);
        }
        let absolute = root.join(&path);
        let lease = storage.platform.lease_access_time(&absolute)?;
        let metadata = lease.metadata()?;
        let link = lease.link_target()?;
        let descend = metadata.attributes & 0x410 == 0x10;
        output.push(Held {
            entry: Entry {
                relative: path.clone(),
                metadata,
                link,
            },
            lease,
        });
        if descend {
            let mut names = storage
                .platform
                .directory_entries_for_inventory(&absolute)?;
            names.sort();
            for name in names {
                let next = path.join(name);
                if !relative(&next) {
                    return Err(MigrationError::InvalidPaths);
                }
                walk(storage, root, next, output, depth + 1)?;
            }
        }
        Ok(())
    }
    let mut output = Vec::new();
    walk(storage, root, PathBuf::new(), &mut output, 0)?;
    output.sort_by(|left, right| left.entry.relative.cmp(&right.entry.relative));
    Ok(output)
}

fn validate_entries(plan: &MigrationPlan, entries: &[Entry]) -> Result<()> {
    if entries.len() as u64 != plan.source.entries
        || entries.len() > MAX_ENTRIES
        || entries.first().is_none_or(|entry| {
            !entry.relative.as_os_str().is_empty() || entry.metadata.object != plan.source.root
        })
    {
        return Err(MigrationError::BackupUnverified);
    }
    let mut previous: Option<&PathBuf> = None;
    let mut links = 0;
    for entry in entries {
        if !relative(&entry.relative)
            || previous.is_some_and(|previous| previous >= &entry.relative)
            || entry.metadata.object.volume != plan.work_volume
        {
            return Err(MigrationError::BackupUnverified);
        }
        previous = Some(&entry.relative);
        if entry.metadata.attributes & 0x400 != 0 {
            let expected = plan
                .source
                .reparse_entries
                .iter()
                .find(|alias| inventory::same(&alias.relative_path, &entry.relative))
                .ok_or(MigrationError::ForbiddenLink)?;
            if entry
                .link
                .as_ref()
                .is_none_or(|link| !inventory::same(link, &expected.raw_target))
            {
                return Err(MigrationError::ForbiddenLink);
            }
            links += 1;
        } else if entry.link.is_some() {
            return Err(MigrationError::ForbiddenLink);
        }
    }
    if links != plan.source.reparse_entries.len() {
        return Err(MigrationError::ForbiddenLink);
    }
    Ok(())
}

pub(super) fn persist<P: NativePlatform>(
    storage: &NativeStorage<'_, P>,
    plan: &MigrationPlan,
    held: &[Held<'_>],
) -> Result<Reference> {
    let entries: Vec<_> = held.iter().map(|value| value.entry.clone()).collect();
    validate_entries(plan, &entries)?;
    let directory = base(storage)?;
    private_fs::create_private_dir(&directory).map_err(|_| MigrationError::ObjectConflict)?;
    let root = private_fs::inspect_directory_object(&directory)
        .map_err(|_| MigrationError::ObjectConflict)?;
    let mut pages = 0usize;
    let mut total_bytes = 0usize;
    let mut page = Vec::new();
    let mut page_bytes = 2usize;
    let mut image = Sha256::new();
    for entry in entries {
        let size = serde_json::to_vec(&entry)
            .map_err(|_| MigrationError::BackupUnverified)?
            .len();
        if size > PAGE_BYTES {
            return Err(MigrationError::BackupUnverified);
        }
        if !page.is_empty() && page_bytes + size + 1 > PAGE_BYTES {
            pages += 1;
            let bytes = serde_json::to_vec(&page).map_err(|_| MigrationError::BackupUnverified)?;
            total_bytes += bytes.len();
            if pages > MAX_PAGES || total_bytes > MAX_TOTAL_BYTES {
                return Err(MigrationError::BackupUnverified);
            }
            image.update((bytes.len() as u64).to_le_bytes());
            image.update(&bytes);
            put(
                storage,
                &directory.join(format!("page_{pages:08}.json")),
                &bytes,
            )?;
            page.clear();
            page_bytes = 2;
        }
        page_bytes += size + 1;
        page.push(entry);
    }
    if !page.is_empty() {
        pages += 1;
        let bytes = serde_json::to_vec(&page).map_err(|_| MigrationError::BackupUnverified)?;
        total_bytes += bytes.len();
        if pages > MAX_PAGES || total_bytes > MAX_TOTAL_BYTES {
            return Err(MigrationError::BackupUnverified);
        }
        image.update((bytes.len() as u64).to_le_bytes());
        image.update(&bytes);
        put(
            storage,
            &directory.join(format!("page_{pages:08}.json")),
            &bytes,
        )?;
    }
    // Commit the header last. An incomplete set never authorizes restoration.
    let image = hex::encode(image.finalize());
    let reference = Reference {
        root,
        image: image.clone(),
    };
    let header = Header {
        schema_version: 1,
        operation_id: plan.operation_id.clone(),
        source: plan.source.clone(),
        root,
        pages,
        entries: held.len(),
        image,
    };
    put(
        storage,
        &directory.join(HEADER_NAME),
        &serde_json::to_vec(&header).map_err(|_| MigrationError::BackupUnverified)?,
    )?;
    Ok(reference)
}

pub(super) fn load<P: NativePlatform>(
    storage: &NativeStorage<'_, P>,
    plan: &MigrationPlan,
    expected: Option<&Reference>,
) -> Result<Option<Vec<Entry>>> {
    let Some(expected) = expected else {
        return Ok(None);
    };
    let directory = base(storage)?;
    if storage.directory_identity(&directory)?.is_none() {
        return Err(MigrationError::BackupUnverified);
    }
    let header: Header = serde_json::from_slice(&get(storage, &directory.join(HEADER_NAME))?)
        .map_err(|_| MigrationError::BackupUnverified)?;
    if header.schema_version != 1
        || header.operation_id != plan.operation_id
        || header.source != plan.source
        || header.pages == 0
        || header.pages > MAX_PAGES
        || header.entries > MAX_ENTRIES
        || header.root != expected.root
        || header.image != expected.image
        || private_fs::inspect_directory_object(&directory)
            .map_err(|_| MigrationError::ObjectConflict)?
            != header.root
    {
        return Err(MigrationError::BackupUnverified);
    }
    let mut output = Vec::new();
    let mut total = 0;
    let mut image = Sha256::new();
    for page in 1..=header.pages {
        let bytes = get(storage, &directory.join(format!("page_{page:08}.json")))?;
        total += bytes.len();
        if total > MAX_TOTAL_BYTES {
            return Err(MigrationError::BackupUnverified);
        }
        image.update((bytes.len() as u64).to_le_bytes());
        image.update(&bytes);
        let entries: Vec<Entry> =
            serde_json::from_slice(&bytes).map_err(|_| MigrationError::BackupUnverified)?;
        output.extend(entries);
        if output.len() > header.entries {
            return Err(MigrationError::BackupUnverified);
        }
    }
    if output.len() != header.entries || hex::encode(image.finalize()) != expected.image {
        return Err(MigrationError::BackupUnverified);
    }
    validate_entries(plan, &output)?;
    Ok(Some(output))
}

pub(super) fn restore_source<P: NativePlatform>(
    storage: &NativeStorage<'_, P>,
    plan: &MigrationPlan,
    entries: &[Entry],
) -> Result<()> {
    validate_entries(plan, entries)?;
    if storage.directory_identity(&plan.paths.source)? != Some(plan.source.root) {
        return Err(MigrationError::ObjectConflict);
    }
    let mut held = collect(storage, &plan.paths.source)?;
    restore(&mut held, entries, None)?;
    storage.verify_snapshot(&plan.paths.source, &plan.source)
}

pub(super) fn restore(
    held: &mut [Held<'_>],
    entries: &[Entry],
    backup_root: Option<DirectoryIdentity>,
) -> Result<()> {
    let backup = backup_root.is_some();
    if backup_root.is_some_and(|root| {
        held.first().is_none_or(|entry| {
            !entry.entry.relative.as_os_str().is_empty() || entry.entry.metadata.object != root
        })
    }) {
        return Err(MigrationError::ObjectConflict);
    }
    let expected: BTreeMap<_, _> = entries
        .iter()
        .filter(|entry| !backup || entry.link.is_none())
        .map(|entry| (entry.relative.clone(), entry))
        .collect();
    if held.len() != expected.len() {
        #[cfg(test)]
        eprintln!("atime_memento_mismatch backup={backup} shape=true");
        return Err(MigrationError::SourceChanged);
    }
    let mut admitted = Vec::with_capacity(held.len());
    // Validate the entire set before the first timestamp mutation.
    for value in held.iter() {
        let original = expected
            .get(&value.entry.relative)
            .ok_or(MigrationError::SourceChanged)?;
        let current = value.lease.metadata()?;
        let mut wanted = original.metadata.clone();
        if backup {
            wanted.object = value.entry.metadata.object;
        }
        if !same_non_access(&current, &wanted)
            || !same_link(&value.lease.link_target()?, &original.link)
        {
            #[cfg(test)]
            eprintln!("atime_memento_mismatch backup={backup} root={} object={} attrs={} created={} accessed={} written={} size={} security={} ea={} streams={} link={}", value.entry.relative.as_os_str().is_empty(), current.object != wanted.object, current.attributes != wanted.attributes, current.created != wanted.created, current.accessed != wanted.accessed, current.written != wanted.written, current.unnamed_bytes != wanted.unnamed_bytes, current.security != wanted.security, current.extended_attributes != wanted.extended_attributes, current.streams != wanted.streams, !same_link(&value.lease.link_target()?, &original.link));
            return Err(MigrationError::SourceChanged);
        }
        admitted.push(wanted);
    }
    for (value, wanted) in held.iter_mut().zip(admitted) {
        value.lease.restore_accessed(&wanted).map_err(|error| {
            #[cfg(test)]
            eprintln!(
                "atime_memento_set_failed backup={backup} root={} kind={error:?}",
                value.entry.relative.as_os_str().is_empty()
            );
            error
        })?;
    }
    Ok(())
}

pub(super) fn records(held: &[Held<'_>]) -> Vec<Entry> {
    held.iter().map(|value| value.entry.clone()).collect()
}

#[cfg(test)]
pub(super) fn diagnose_held(
    stage: &str,
    held: &[Held<'_>],
    expected: &[Entry],
    aliases: &[ChromeLatest],
) -> Result<()> {
    let expected: BTreeMap<_, _> = expected
        .iter()
        .map(|entry| (&entry.relative, &entry.metadata))
        .collect();
    let mut changed = [false; 9];
    let mut categories = [false; 5];
    let mut access_categories = [false; 5];
    for value in held {
        let Some(before) = expected.get(&value.entry.relative) else {
            continue;
        };
        let after = value.lease.metadata()?;
        let flags = [
            before.object != after.object,
            before.attributes != after.attributes,
            before.created != after.created,
            before.accessed != after.accessed,
            before.written != after.written,
            before.unnamed_bytes != after.unnamed_bytes,
            before.security != after.security,
            before.extended_attributes != after.extended_attributes,
            before.streams != after.streams,
        ];
        for (target, flag) in changed.iter_mut().zip(flags) {
            *target |= flag;
        }
        let kind = if value.entry.relative.as_os_str().is_empty() {
            0
        } else if before.attributes & 0x400 != 0 {
            2
        } else if before.attributes & 0x10 != 0 {
            1
        } else {
            3
        };
        let any = flags.iter().any(|value| *value);
        categories[kind] |= any;
        access_categories[kind] |= flags[3];
        if aliases
            .iter()
            .any(|alias| alias.target_identity == before.object)
        {
            categories[4] |= any;
            access_categories[4] |= flags[3];
        }
    }
    eprintln!("source_memento_scan stage={stage} object={} attrs={} created={} accessed={} written={} size={} security={} ea={} streams={} root={} directory={} reparse={} file={} alias_target={} root_access={} directory_access={} reparse_access={} file_access={} alias_target_access={}", changed[0], changed[1], changed[2], changed[3], changed[4], changed[5], changed[6], changed[7], changed[8], categories[0], categories[1], categories[2], categories[3], categories[4], access_categories[0], access_categories[1], access_categories[2], access_categories[3], access_categories[4]);
    Ok(())
}
