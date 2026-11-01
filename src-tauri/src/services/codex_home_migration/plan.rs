use super::contract::*;
use std::path::{Component, Path, PathBuf};

pub(super) fn key(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let native: PathBuf = path.components().collect();
        let text = native.to_string_lossy().to_lowercase();
        PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text))
    }
    #[cfg(not(windows))]
    {
        path.components().collect()
    }
}

pub(super) fn same(left: &Path, right: &Path) -> bool {
    key(left) == key(right)
}
fn inside(path: &Path, root: &Path) -> bool {
    key(path).starts_with(key(root)) && !same(path, root)
}
fn overlap(left: &Path, right: &Path) -> bool {
    same(left, right) || inside(left, right) || inside(right, left)
}
fn absolute(path: &Path) -> bool {
    path.is_absolute()
        && !path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
        && !path.to_string_lossy().chars().any(char::is_control)
}
fn relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
        && !path.to_string_lossy().chars().any(char::is_control)
}
fn token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
}

pub(super) fn validate_paths(
    storage: &impl Storage,
    operation: &str,
    paths: &MigrationPaths,
) -> Result<()> {
    if !token(operation) || paths.home.parent().is_none() {
        return Err(MigrationError::InvalidPaths);
    }
    let all = [
        &paths.home,
        &paths.source,
        &paths.destination,
        &paths.operation_root,
        &paths.source_backup,
        &paths.destination_archive,
        &paths.journal,
    ];
    if all.iter().any(|path| !absolute(path)) {
        return Err(MigrationError::InvalidPaths);
    }
    let canonical: Vec<_> = all
        .iter()
        .map(|path| storage.canonical_path(path))
        .collect::<Result<_>>()?;
    let [home, source, destination, operation_root, backup, archive, journal] =
        canonical.as_slice()
    else {
        unreachable!()
    };
    if [source, destination, operation_root]
        .iter()
        .any(|path| !inside(path, home))
        || overlap(source, destination)
        || overlap(source, operation_root)
        || overlap(destination, operation_root)
        || [backup, archive, journal]
            .iter()
            .any(|path| !inside(path, operation_root))
        || overlap(backup, archive)
        || overlap(backup, journal)
        || overlap(archive, journal)
        || paths
            .operation_root
            .file_name()
            .is_none_or(|name| name.to_string_lossy() != operation)
    {
        return Err(MigrationError::InvalidPaths);
    }
    for native in [home.join(".codex"), home.join(".claude")] {
        if [source, destination, operation_root]
            .iter()
            .any(|path| overlap(path, &native))
        {
            return Err(MigrationError::InvalidPaths);
        }
    }
    Ok(())
}

pub(super) fn validate_snapshot(root: &Path, snapshot: &HomeSnapshot) -> Result<()> {
    if !token(&snapshot.image) || snapshot.reparse_entries.len() > 1 {
        return Err(MigrationError::ForbiddenLink);
    }
    let package = Path::new("plugins/cache/openai-bundled/chrome");
    for alias in &snapshot.reparse_entries {
        if !relative(&alias.relative_path)
            || !same(&alias.relative_path, &package.join("latest"))
            || !relative(&alias.target_relative)
            || alias
                .target_relative
                .parent()
                .is_none_or(|parent| !same(parent, package))
            || alias
                .target_relative
                .file_name()
                .is_none_or(|name| name.to_string_lossy().eq_ignore_ascii_case("latest"))
            || !absolute(&alias.raw_target)
            || !same(&alias.raw_target, &root.join(&alias.target_relative))
            || alias.target_identity.volume != snapshot.root.volume
        {
            return Err(MigrationError::ForbiddenLink);
        }
    }
    Ok(())
}

fn check_alias_targets(storage: &impl Storage, root: &Path, snapshot: &HomeSnapshot) -> Result<()> {
    validate_snapshot(root, snapshot)?;
    let canonical_root = storage.canonical_path(root)?;
    for alias in &snapshot.reparse_entries {
        if !same(
            &storage.canonical_path(&alias.raw_target)?,
            &canonical_root.join(&alias.target_relative),
        ) || storage.directory_identity(&root.join(&alias.target_relative))?
            != Some(alias.target_identity)
        {
            return Err(MigrationError::ForbiddenLink);
        }
    }
    Ok(())
}

fn volume(storage: &impl Storage, path: &Path) -> Result<u64> {
    for ancestor in path.ancestors() {
        if let Some(identity) = storage.directory_identity(ancestor)? {
            return Ok(identity.volume);
        }
    }
    Err(MigrationError::SourceUnavailable)
}

pub(crate) fn prepare_plan(
    storage: &impl Storage,
    operation_id: &str,
    paths: MigrationPaths,
) -> Result<MigrationPlan> {
    validate_paths(storage, operation_id, &paths)?;
    if storage.load_journal(&paths.journal)?.is_some() {
        return Err(MigrationError::JournalConflict);
    }
    if storage.directory_identity(&paths.source_backup)?.is_some()
        || storage
            .directory_identity(&paths.destination_archive)?
            .is_some()
    {
        return Err(MigrationError::ObjectConflict);
    }
    let source_identity = storage
        .directory_identity(&paths.source)?
        .ok_or(MigrationError::SourceUnavailable)?;
    let source = storage.inventory(&paths.source)?;
    if source.root != source_identity {
        return Err(MigrationError::SourceChanged);
    }
    check_alias_targets(storage, &paths.source, &source)?;
    let destination = match storage.directory_identity(&paths.destination)? {
        Some(identity) => {
            let snapshot = storage.inventory(&paths.destination)?;
            if snapshot.root != identity {
                return Err(MigrationError::ObjectConflict);
            }
            check_alias_targets(storage, &paths.destination, &snapshot)?;
            Some(snapshot)
        }
        None => None,
    };
    let work_volume = volume(storage, &paths.operation_root)?;
    if work_volume != source.root.volume
        || volume(storage, &paths.destination)? != source.root.volume
        || destination
            .as_ref()
            .is_some_and(|target| target.root.volume != source.root.volume)
    {
        return Err(MigrationError::DifferentVolume);
    }
    if destination
        .as_ref()
        .is_some_and(|target| target.root == source.root)
    {
        return Err(MigrationError::InvalidPaths);
    }
    Ok(MigrationPlan {
        operation_id: operation_id.into(),
        paths,
        source,
        destination,
        work_volume,
    })
}

pub(super) fn validate_journal(storage: &impl Storage, journal: &Journal) -> Result<()> {
    let plan = &journal.plan;
    validate_paths(storage, &plan.operation_id, &plan.paths)?;
    validate_snapshot(&plan.paths.source, &plan.source)?;
    if let Some(destination) = &plan.destination {
        validate_snapshot(&plan.paths.destination, destination)?;
    }
    if journal.schema_version != 1
        || journal.revision == 0
        || plan.work_volume != plan.source.root.volume
        || plan.destination.as_ref().is_some_and(|target| {
            target.root.volume != plan.work_volume || target.root == plan.source.root
        })
        || (journal.rolled_back && (journal.phase == Phase::Committed || journal.pending.is_some()))
        || (journal.phase != Phase::Planned && journal.source_backup.is_none())
    {
        return Err(MigrationError::JournalConflict);
    }
    let valid_intent = match journal.pending {
        None => true,
        Some(Intent::BackupSource) => journal.phase == Phase::Planned,
        Some(Intent::PreserveDestination) => journal.phase == Phase::BackedUp,
        Some(Intent::InstallSource) => journal.phase == Phase::DestinationPreserved,
        Some(Intent::RetargetLatest) => journal.phase == Phase::SourceInstalled,
        Some(Intent::RestoreSource | Intent::RestoreDestination) => {
            journal.phase != Phase::Committed
        }
    };
    if !valid_intent {
        return Err(MigrationError::JournalConflict);
    }
    if let Some(backup) = &journal.source_backup {
        validate_backup(plan, backup)?;
    }
    Ok(())
}

pub(super) fn validate_backup(plan: &MigrationPlan, backup: &HomeSnapshot) -> Result<()> {
    if backup.root.volume != plan.work_volume
        || backup.root == plan.source.root
        || plan
            .destination
            .as_ref()
            .is_some_and(|target| backup.root == target.root)
        || backup.image != plan.source.image
        || backup.entries != plan.source.entries
        || backup.bytes != plan.source.bytes
    {
        return Err(MigrationError::BackupUnverified);
    }
    Ok(())
}
