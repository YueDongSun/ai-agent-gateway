use super::*;
use std::{
    fs,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

struct FixturePlatform;
impl NativePlatform for FixturePlatform {
    fn metadata(&self, path: &Path) -> Result<EntryMetadata> {
        let native = crate::config::private_fs::inventory_object_metadata(path)
            .map_err(|_| MigrationError::SourceUnavailable)?;
        let mut stream_path = path.as_os_str().to_os_string();
        stream_path.push(":fixture:$DATA");
        let streams =
            match crate::config::private_fs::open_inventory_stream(Path::new(&stream_path)) {
                Ok(stream) => vec![NamedStream {
                    name: ":fixture:$DATA".into(),
                    bytes: stream
                        .metadata()
                        .map_err(|_| MigrationError::SourceUnavailable)?
                        .len(),
                }],
                Err(_) => Vec::new(),
            };
        Ok(EntryMetadata {
            object: native.object,
            attributes: native.attributes,
            created: native.created,
            accessed: native.accessed,
            written: native.written,
            unnamed_bytes: native.unnamed_bytes,
            security: b"synthetic security only".to_vec(),
            extended_attributes: Vec::new(),
            streams,
        })
    }
    fn open_stream_for_inventory(&self, path: &Path) -> Result<fs::File> {
        crate::config::private_fs::open_inventory_stream(path)
            .map_err(|_| MigrationError::SourceUnavailable)
    }
    fn directory_entries_for_inventory(&self, path: &Path) -> Result<Vec<std::ffi::OsString>> {
        crate::config::private_fs::directory_inventory_entries(path)
            .map_err(|_| MigrationError::SourceUnavailable)
    }
    fn robocopy_executable(&self) -> Result<PathBuf> {
        Err(MigrationError::PlatformFailure)
    }
    fn retarget_latest(
        &self,
        _root: &Path,
        _expected: DirectoryIdentity,
        _alias: &ChromeLatest,
        _target: &Path,
        _alias_identity: DirectoryIdentity,
    ) -> Result<()> {
        Err(MigrationError::PlatformFailure)
    }
    fn validate_installed(&self, _journal: &Journal) -> Result<()> {
        Ok(())
    }
}

#[cfg(windows)]
fn fixture_paths(root: &Path) -> MigrationPaths {
    let home = root.join("home");
    crate::config::private_fs::prepare_private_dir(&home).unwrap();
    let source = home.join("source");
    crate::config::private_fs::prepare_private_dir(&source).unwrap();
    let operation_root = home.join("operation_one");
    crate::config::private_fs::prepare_private_dir(&operation_root).unwrap();
    fs::write(
        source.join("history.jsonl"),
        b"synthetic complete history\n",
    )
    .unwrap();
    MigrationPaths {
        home: home.clone(),
        source,
        destination: home.join("destination"),
        source_backup: operation_root.join("source_backup"),
        destination_archive: operation_root.join("destination_archive"),
        journal: operation_root.join("journal.json"),
        operation_root,
    }
}

#[cfg(windows)]
#[test]
fn inventory_detects_real_file_changes_without_modifying_the_tree() {
    let fixture = crate::config::private_fs::private_temp_dir(&std::env::temp_dir()).unwrap();
    let home = fixture.path().join("home");
    fs::create_dir(&home).unwrap();
    let source = home.join("source");
    fs::create_dir(&source).unwrap();
    crate::config::private_fs::prepare_private_dir(&home.join("operation_one")).unwrap();
    fs::write(
        source.join("history.jsonl"),
        b"synthetic complete history\n",
    )
    .unwrap();
    let paths = MigrationPaths {
        home: home.clone(),
        source: source.clone(),
        destination: home.join("destination"),
        operation_root: home.join("operation_one"),
        source_backup: home.join("operation_one/source_backup"),
        destination_archive: home.join("operation_one/destination_archive"),
        journal: home.join("operation_one/journal.json"),
    };
    let cancel = AtomicBool::new(false);
    let storage = NativeStorage::new(
        paths,
        &FixturePlatform,
        &cancel,
        Instant::now() + Duration::from_secs(30),
    )
    .unwrap();
    let initial = storage.inventory(&source).unwrap();
    assert_eq!(initial.entries, 2);
    assert_eq!(initial.bytes, 27);
    assert!(storage.verify_snapshot(&source, &initial).is_ok());
    fs::write(
        source.join("history.jsonl"),
        b"synthetic changed history!\n",
    )
    .unwrap();
    assert_eq!(
        storage.verify_snapshot(&source, &initial),
        Err(MigrationError::SourceChanged)
    );
    assert_eq!(fs::read_dir(&source).unwrap().count(), 1);
}

#[cfg(windows)]
#[test]
fn named_stream_bytes_are_part_of_the_consumed_backup_image() {
    let fixture = crate::config::private_fs::private_temp_dir(&std::env::temp_dir()).unwrap();
    let paths = fixture_paths(fixture.path());
    let stream = paths.source.join("history.jsonl:fixture:$DATA");
    fs::write(&stream, b"first stream").unwrap();
    let cancelled = AtomicBool::new(false);
    let storage = NativeStorage::new(
        paths.clone(),
        &FixturePlatform,
        &cancelled,
        Instant::now() + Duration::from_secs(30),
    )
    .unwrap();
    let initial = storage.inventory(&paths.source).unwrap();
    assert_eq!(initial.bytes, 39);
    assert_eq!(initial.entries, 2);
    fs::write(&stream, b"other stream").unwrap();
    assert_eq!(
        storage.verify_snapshot(&paths.source, &initial),
        Err(MigrationError::SourceChanged)
    );
    assert_eq!(
        fs::read(paths.source.join("history.jsonl")).unwrap(),
        b"synthetic complete history\n"
    );
    assert_eq!(fs::read_dir(&paths.source).unwrap().count(), 1);
}

#[cfg(windows)]
#[test]
fn inventory_preserves_file_and_directory_access_times_through_protected_handles() {
    let fixture = crate::config::private_fs::private_temp_dir(&std::env::temp_dir()).unwrap();
    let paths = fixture_paths(fixture.path());
    let transcript = paths.source.join("history.jsonl");
    let source_before =
        crate::config::private_fs::inventory_object_metadata(&paths.source).unwrap();
    let transcript_before =
        crate::config::private_fs::inventory_object_metadata(&transcript).unwrap();
    let cancelled = AtomicBool::new(false);
    let storage = NativeStorage::new(
        paths.clone(),
        &FixturePlatform,
        &cancelled,
        Instant::now() + Duration::from_secs(30),
    )
    .unwrap();
    let image = storage.inventory(&paths.source).unwrap();
    storage.verify_snapshot(&paths.source, &image).unwrap();
    let source_after = crate::config::private_fs::inventory_object_metadata(&paths.source).unwrap();
    let transcript_after =
        crate::config::private_fs::inventory_object_metadata(&transcript).unwrap();
    assert_eq!(source_before.object, source_after.object);
    assert_eq!(source_before.created, source_after.created);
    assert_eq!(source_before.accessed, source_after.accessed);
    assert_eq!(source_before.written, source_after.written);
    assert_eq!(transcript_before.object, transcript_after.object);
    assert_eq!(transcript_before.created, transcript_after.created);
    assert_eq!(transcript_before.accessed, transcript_after.accessed);
    assert_eq!(transcript_before.written, transcript_after.written);
}

#[cfg(windows)]
#[test]
fn journal_reopens_the_contiguous_chain_and_stale_writers_cannot_replace_a_revision() {
    let fixture = crate::config::private_fs::private_temp_dir(&std::env::temp_dir()).unwrap();
    let paths = fixture_paths(fixture.path());
    let cancelled = AtomicBool::new(false);
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut first =
        NativeStorage::new(paths.clone(), &FixturePlatform, &cancelled, deadline).unwrap();
    let mut stale =
        NativeStorage::new(paths.clone(), &FixturePlatform, &cancelled, deadline).unwrap();
    let plan = super::super::prepare_plan(&first, "operation_one", paths.clone()).unwrap();
    let initial = Journal {
        schema_version: 1,
        revision: 1,
        plan,
        phase: Phase::Planned,
        pending: None,
        source_backup: None,
        rolled_back: false,
    };
    assert_eq!(first.load_journal(&paths.journal).unwrap(), None);
    first.store_journal(&paths.journal, &initial, None).unwrap();
    assert_eq!(
        stale.load_journal(&paths.journal).unwrap(),
        Some(initial.clone())
    );
    let mut pending = initial.clone();
    pending.revision = 2;
    pending.pending = Some(Intent::BackupSource);
    first
        .store_journal(&paths.journal, &pending, Some(1))
        .unwrap();
    assert_eq!(
        stale.store_journal(&paths.journal, &pending, Some(1)),
        Err(MigrationError::JournalConflict)
    );
    assert_eq!(
        serde_json::from_slice::<Journal>(&fs::read(&paths.journal).unwrap()).unwrap(),
        initial
    );
    let reopened =
        NativeStorage::new(paths.clone(), &FixturePlatform, &cancelled, deadline).unwrap();
    assert_eq!(
        reopened.load_journal(&paths.journal).unwrap(),
        Some(pending)
    );
    assert!(journal::revision_path(&paths.journal, 2).unwrap().is_file());
    assert!(!journal::revision_path(&paths.journal, 3).unwrap().exists());
}

#[cfg(windows)]
#[test]
fn journal_rejects_gaps_and_invalid_revision_names_but_ignores_uncommitted_private_temporaries() {
    let fixture = crate::config::private_fs::private_temp_dir(&std::env::temp_dir()).unwrap();
    let paths = fixture_paths(fixture.path());
    let cancelled = AtomicBool::new(false);
    let mut storage = NativeStorage::new(
        paths.clone(),
        &FixturePlatform,
        &cancelled,
        Instant::now() + Duration::from_secs(30),
    )
    .unwrap();
    let plan = super::super::prepare_plan(&storage, "operation_one", paths.clone()).unwrap();
    let initial = Journal {
        schema_version: 1,
        revision: 1,
        plan,
        phase: Phase::Planned,
        pending: None,
        source_backup: None,
        rolled_back: false,
    };
    storage
        .store_journal(&paths.journal, &initial, None)
        .unwrap();
    let temporary = paths.operation_root.join(".journal.json.revision_00000000000000000002.aag_tmp_mirror_00000000000000000000000000000000");
    crate::config::private_fs::atomic_write(&temporary, b"uncommitted temporary bytes").unwrap();
    assert_eq!(storage.load_journal(&paths.journal).unwrap(), Some(initial));
    let gap = journal::revision_path(&paths.journal, 3).unwrap();
    crate::config::private_fs::atomic_write(&gap, b"invalid incomplete record").unwrap();
    assert_eq!(
        storage.load_journal(&paths.journal),
        Err(MigrationError::JournalConflict)
    );
    fs::remove_file(&gap).unwrap();
    let malformed = paths.operation_root.join("journal.json.revision_unknown");
    crate::config::private_fs::atomic_write(&malformed, b"unrelated private bytes").unwrap();
    assert_eq!(
        storage.load_journal(&paths.journal),
        Err(MigrationError::JournalConflict)
    );
    assert_eq!(fs::read(&malformed).unwrap(), b"unrelated private bytes");
}

#[cfg(windows)]
#[test]
fn exact_admitted_journal_object_is_required_even_when_replacement_bytes_are_identical() {
    let fixture = crate::config::private_fs::private_temp_dir(&std::env::temp_dir()).unwrap();
    let paths = fixture_paths(fixture.path());
    let cancelled = AtomicBool::new(false);
    let mut storage = NativeStorage::new(
        paths.clone(),
        &FixturePlatform,
        &cancelled,
        Instant::now() + Duration::from_secs(30),
    )
    .unwrap();
    let plan = super::super::prepare_plan(&storage, "operation_one", paths.clone()).unwrap();
    let initial = Journal {
        schema_version: 1,
        revision: 1,
        plan,
        phase: Phase::Planned,
        pending: None,
        source_backup: None,
        rolled_back: false,
    };
    storage
        .store_journal(&paths.journal, &initial, None)
        .unwrap();
    let exact = fs::read(&paths.journal).unwrap();
    crate::config::private_fs::atomic_write(&paths.journal, &exact).unwrap();
    let mut next = initial;
    next.revision = 2;
    next.pending = Some(Intent::BackupSource);
    assert_eq!(
        storage.store_journal(&paths.journal, &next, Some(1)),
        Err(MigrationError::JournalConflict)
    );
    assert!(!journal::revision_path(&paths.journal, 2).unwrap().exists());
    assert_eq!(fs::read(&paths.journal).unwrap(), exact);
}

#[cfg(windows)]
#[test]
fn an_unknown_existing_backup_is_retained_and_never_adopted_by_matching_contents() {
    let fixture = crate::config::private_fs::private_temp_dir(&std::env::temp_dir()).unwrap();
    let paths = fixture_paths(fixture.path());
    let cancelled = AtomicBool::new(false);
    let mut storage = NativeStorage::new(
        paths.clone(),
        &FixturePlatform,
        &cancelled,
        Instant::now() + Duration::from_secs(30),
    )
    .unwrap();
    let plan = super::super::prepare_plan(&storage, "operation_one", paths.clone()).unwrap();
    crate::config::private_fs::prepare_private_dir(&paths.source_backup).unwrap();
    fs::write(
        paths.source_backup.join("independent_history"),
        b"retain this independent tree",
    )
    .unwrap();
    assert_eq!(
        storage.backup_home_verified(&plan),
        Err(MigrationError::ObjectConflict)
    );
    assert_eq!(
        fs::read(paths.source_backup.join("independent_history")).unwrap(),
        b"retain this independent tree"
    );
    assert_eq!(
        fs::read(paths.source.join("history.jsonl")).unwrap(),
        b"synthetic complete history\n"
    );
    assert!(!paths.operation_root.join("backup_receipt.json").exists());
}

#[cfg(windows)]
#[test]
fn cancellation_deadline_and_outside_paths_fail_without_creating_migration_artifacts() {
    let fixture = crate::config::private_fs::private_temp_dir(&std::env::temp_dir()).unwrap();
    let paths = fixture_paths(fixture.path());
    let cancelled = AtomicBool::new(true);
    assert!(matches!(
        NativeStorage::new(
            paths.clone(),
            &FixturePlatform,
            &cancelled,
            Instant::now() + Duration::from_secs(30)
        ),
        Err(MigrationError::PlatformFailure)
    ));
    cancelled.store(false, std::sync::atomic::Ordering::SeqCst);
    assert!(matches!(
        NativeStorage::new(paths.clone(), &FixturePlatform, &cancelled, Instant::now()),
        Err(MigrationError::PlatformFailure)
    ));
    let storage = NativeStorage::new(
        paths.clone(),
        &FixturePlatform,
        &cancelled,
        Instant::now() + Duration::from_secs(30),
    )
    .unwrap();
    assert_eq!(
        storage.inventory(fixture.path()),
        Err(MigrationError::InvalidPaths)
    );
    assert_eq!(
        storage.canonical_path(&paths.home.join("../outside")),
        Err(MigrationError::InvalidPaths)
    );
    assert!(!paths.source_backup.exists());
    assert!(!paths.journal.exists());
}
