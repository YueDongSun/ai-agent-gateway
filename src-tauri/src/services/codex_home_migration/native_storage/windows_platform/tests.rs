use super::*;
use crate::{
    config::private_fs,
    services::codex_home_migration::{self, HomeSnapshot, Phase, Storage, WriterQuiescence},
};
use std::{cell::Cell, fs, io::Read, time::Duration};

#[test]
fn file_stream_records_preserve_named_bytes_and_exclude_the_unnamed_stream() {
    // FILE_STREAM_INFO documented layout: 24-byte header, UTF-16 byte length,
    // and an 8-byte aligned offset to each following record.
    let mut records = vec![0u8; 92];
    records[0..4].copy_from_slice(&40u32.to_le_bytes());
    records[4..8].copy_from_slice(&14u32.to_le_bytes());
    records[8..16].copy_from_slice(&27i64.to_le_bytes());
    for (part, unit) in records[24..38]
        .chunks_exact_mut(2)
        .zip("::$DATA".encode_utf16())
    {
        part.copy_from_slice(&unit.to_le_bytes());
    }
    records[44..48].copy_from_slice(&28u32.to_le_bytes());
    records[48..56].copy_from_slice(&12i64.to_le_bytes());
    for (part, unit) in records[64..92]
        .chunks_exact_mut(2)
        .zip(":fixture:$DATA".encode_utf16())
    {
        part.copy_from_slice(&unit.to_le_bytes());
    }
    assert_eq!(
        metadata::parse_streams(&records).unwrap(),
        vec![NamedStream {
            name: ":fixture:$DATA".into(),
            bytes: 12
        }]
    );
}

#[test]
fn malformed_stream_and_mount_point_records_are_rejected_without_truncation() {
    assert!(metadata::parse_streams(&[0; 24]).is_err());
    let mut streams = vec![0u8; 40];
    streams[0..4].copy_from_slice(&16u32.to_le_bytes());
    streams[4..8].copy_from_slice(&14u32.to_le_bytes());
    for (part, unit) in streams[24..38]
        .chunks_exact_mut(2)
        .zip("::$DATA".encode_utf16())
    {
        part.copy_from_slice(&unit.to_le_bytes());
    }
    assert!(metadata::parse_streams(&streams).is_err());
    let fixture = private_fs::private_temp_dir(&std::env::temp_dir()).unwrap();
    let target = fixture.path().join("version");
    let buffer = junction::buffer(&target, target.to_str().unwrap()).unwrap();
    assert_eq!(junction::parse(&buffer).unwrap().target, target);
    let mut wrong_tag = buffer.clone();
    wrong_tag[0..4].copy_from_slice(&0xA000_000Cu32.to_le_bytes());
    assert_eq!(
        junction::parse(&wrong_tag).unwrap_err(),
        MigrationError::ForbiddenLink
    );
    assert!(junction::parse(&buffer[..buffer.len() - 2]).is_err());
    assert!(junction::buffer(Path::new(r"\\.\fixture_device"), "device").is_err());
}

fn ea_record(name: &[u8], value: &[u8], next: bool) -> Vec<u8> {
    let used = 8 + name.len() + 1 + value.len();
    let length = if next { (used + 3) / 4 * 4 } else { used };
    let mut record = vec![0u8; length];
    if next {
        record[0..4].copy_from_slice(&(length as u32).to_le_bytes());
    }
    record[5] = name.len() as u8;
    record[6..8].copy_from_slice(&(value.len() as u16).to_le_bytes());
    record[8..8 + name.len()].copy_from_slice(name);
    record[9 + name.len()..used].copy_from_slice(value);
    record
}

#[test]
fn extended_attribute_order_and_padding_do_not_hide_semantic_changes() {
    let mut one = ea_record(b"FIRST", b"one", true);
    one.extend(ea_record(b"SECOND", b"two", false));
    let mut two = ea_record(b"SECOND", b"two", true);
    two.extend(ea_record(b"FIRST", b"one", false));
    assert_eq!(
        extended_attributes::canonicalize(&one).unwrap(),
        extended_attributes::canonicalize(&two).unwrap()
    );
    let mut changed = ea_record(b"FIRST", b"different", true);
    changed.extend(ea_record(b"SECOND", b"two", false));
    assert_ne!(
        extended_attributes::canonicalize(&one).unwrap(),
        extended_attributes::canonicalize(&changed).unwrap()
    );
    let mut malformed = ea_record(b"FIRST", b"one", false);
    malformed[8 + 5] = 1;
    assert!(extended_attributes::canonicalize(&malformed).is_err());
}

#[test]
fn scoped_process_privileges_restore_prior_state_in_success_and_unavailable_cases() {
    let before = privileges::snapshot().unwrap();
    match privileges::ProcessPrivileges::acquire() {
        Ok(mut guard) => {
            assert!(privileges::snapshot()
                .unwrap()
                .iter()
                .all(|value| value.is_some_and(|attributes| attributes & 2 != 0)));
            guard.restore().unwrap();
        }
        Err(MigrationError::PlatformFailure) => {}
        Err(error) => panic!("unexpected privilege owner failure: {error:?}"),
    }
    assert_eq!(privileges::snapshot().unwrap(), before);
}

fn paths(root: &Path) -> MigrationPaths {
    let home = root.join("home");
    let source = home.join("source");
    let operation_root = home.join("operation_one");
    private_fs::prepare_private_dir(&source).unwrap();
    private_fs::prepare_private_dir(&operation_root).unwrap();
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

fn require_operator<'a>(
    paths: MigrationPaths,
    cancel: &'a AtomicBool,
    deadline: Instant,
    validator: &'a dyn Fn(&Journal) -> Result<()>,
) -> WindowsStoragePlatform<'a> {
    WindowsStoragePlatform::new(paths, cancel, deadline, validator).expect("full metadata qualification requires an operator process token with SeBackupPrivilege, SeRestorePrivilege, and SeSecurityPrivilege")
}

#[test]
fn operator_metadata_preserves_real_security_ea_ads_and_access_times() {
    let fixture = private_fs::private_temp_dir(&std::env::temp_dir()).unwrap();
    let paths = paths(fixture.path());
    let cancel = AtomicBool::new(false);
    let validator = |_: &Journal| Err(MigrationError::CallerCutoverRequired);
    let mut platform = require_operator(
        paths.clone(),
        &cancel,
        Instant::now() + Duration::from_secs(30),
        &validator,
    );
    let transcript = paths.source.join("history.jsonl");
    let writable = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&transcript)
        .unwrap();
    extended_attributes::set_fixture_ea(&writable, b"AAG_FIXTURE_EA", b"synthetic\0EA bytes")
        .unwrap();
    drop(writable);
    fs::write(
        paths.source.join("history.jsonl:fixture:$DATA"),
        b"named stream",
    )
    .unwrap();
    let before = platform.metadata(&transcript).unwrap();
    assert!(before.security.len() >= 20);
    assert!(!before.extended_attributes.is_empty());
    assert_eq!(
        before.streams,
        vec![NamedStream {
            name: ":fixture:$DATA".into(),
            bytes: 12
        }]
    );
    let mut content = Vec::new();
    platform
        .open_stream_for_inventory(&transcript)
        .unwrap()
        .read_to_end(&mut content)
        .unwrap();
    assert_eq!(content, b"synthetic complete history\n");
    assert!(platform.metadata(&transcript).unwrap() == before);
    let directory_before = platform.metadata(&paths.source).unwrap();
    assert_eq!(
        platform
            .directory_entries_for_inventory(&paths.source)
            .unwrap(),
        vec![OsString::from("history.jsonl")]
    );
    assert!(platform.metadata(&paths.source).unwrap() == directory_before);
    platform.restore_privileges().unwrap();
    assert_eq!(
        platform.metadata(&transcript).err(),
        Some(MigrationError::PlatformFailure)
    );
}

#[test]
fn operator_known_chrome_junction_retargets_in_place_and_rolls_back_with_metadata() {
    let fixture = private_fs::private_temp_dir(&std::env::temp_dir()).unwrap();
    let paths = paths(fixture.path());
    let cancel = AtomicBool::new(false);
    let validator = |_: &Journal| Err(MigrationError::CallerCutoverRequired);
    let platform = require_operator(
        paths.clone(),
        &cancel,
        Instant::now() + Duration::from_secs(30),
        &validator,
    );
    let relative = Path::new("plugins/cache/openai-bundled/chrome/fixture_version");
    let alias_relative = Path::new("plugins/cache/openai-bundled/chrome/latest");
    let version = paths.source.join(relative);
    fs::create_dir_all(&version).unwrap();
    fs::write(version.join("fixture.bin"), b"concrete version bytes").unwrap();
    let alias_path = paths.source.join(alias_relative);
    fs::create_dir(&alias_path).unwrap();
    let alias_id = junction::create_fixture(&alias_path, &version).unwrap();
    let original = platform.metadata(&alias_path).unwrap();
    let root_id = private_fs::inspect_directory_object(&paths.source).unwrap();
    let alias = ChromeLatest {
        relative_path: alias_relative.to_path_buf(),
        raw_target: version.clone(),
        target_relative: relative.to_path_buf(),
        target_identity: private_fs::inspect_directory_object(&version).unwrap(),
    };
    let foreign_id = DirectoryIdentity {
        volume: alias_id.volume,
        object: alias_id.object.wrapping_add(100),
    };
    assert_eq!(
        platform.retarget_latest(&paths.source, root_id, &alias, &version, foreign_id),
        Err(MigrationError::ObjectConflict)
    );
    private_fs::rename_directory_object_no_replace(&paths.source, &paths.destination, root_id)
        .unwrap();
    let new_target = paths.destination.join(relative);
    platform
        .retarget_latest(&paths.destination, root_id, &alias, &new_target, alias_id)
        .unwrap();
    let moved_alias = paths.destination.join(alias_relative);
    assert!(platform.metadata(&moved_alias).unwrap() == original);
    assert!(super::super::inventory::same(
        &fs::read_link(&moved_alias).unwrap(),
        &new_target
    ));
    private_fs::rename_directory_object_no_replace(&paths.destination, &paths.source, root_id)
        .unwrap();
    platform
        .retarget_latest(&paths.source, root_id, &alias, &version, alias_id)
        .unwrap();
    assert!(platform.metadata(&alias_path).unwrap() == original);
    assert!(super::super::inventory::same(
        &fs::read_link(&alias_path).unwrap(),
        &version
    ));
}

struct FixtureWriters;
impl WriterQuiescence for FixtureWriters {
    fn verify(&mut self, _paths: &MigrationPaths) -> Result<()> {
        Ok(())
    }
}

#[test]
fn operator_robocopy_backup_is_independently_verified_with_ea_ads_and_omitted_alias_receipt() {
    let fixture = private_fs::private_temp_dir(&std::env::temp_dir()).unwrap();
    let paths = paths(fixture.path());
    let cancel = AtomicBool::new(false);
    let validator = |_: &Journal| Err(MigrationError::CallerCutoverRequired);
    let platform = require_operator(
        paths.clone(),
        &cancel,
        Instant::now() + Duration::from_secs(60),
        &validator,
    );
    let writable = fs::OpenOptions::new()
        .write(true)
        .open(paths.source.join("history.jsonl"))
        .unwrap();
    extended_attributes::set_fixture_ea(&writable, b"AAG_FIXTURE_EA", b"backup EA bytes").unwrap();
    drop(writable);
    fs::write(
        paths.source.join("history.jsonl:fixture:$DATA"),
        b"named stream",
    )
    .unwrap();
    let relative = Path::new("plugins/cache/openai-bundled/chrome/fixture_version");
    let alias_relative = Path::new("plugins/cache/openai-bundled/chrome/latest");
    fs::create_dir_all(paths.source.join(relative)).unwrap();
    fs::write(
        paths.source.join(relative).join("fixture.bin"),
        b"preserve concrete version bytes",
    )
    .unwrap();
    fs::create_dir(paths.source.join(alias_relative)).unwrap();
    junction::create_fixture(
        &paths.source.join(alias_relative),
        &paths.source.join(relative),
    )
    .unwrap();
    let mut storage = super::super::NativeStorage::new(
        paths.clone(),
        &platform,
        &cancel,
        Instant::now() + Duration::from_secs(60),
    )
    .unwrap();
    let plan =
        codex_home_migration::prepare_plan(&storage, "operation_one", paths.clone()).unwrap();
    let mut writers = FixtureWriters;
    let journal = codex_home_migration::begin(&mut storage, &mut writers, plan.clone()).unwrap();
    let backed_up = codex_home_migration::advance(&mut storage, &mut writers, journal).unwrap();
    assert_eq!(backed_up.phase, Phase::BackedUp);
    storage
        .verify_snapshot(&paths.source, &plan.source)
        .unwrap();
    storage
        .verify_snapshot(
            &paths.source_backup,
            backed_up.source_backup.as_ref().unwrap(),
        )
        .unwrap();
    assert!(fs::symlink_metadata(paths.source_backup.join(alias_relative)).is_err());
    assert!(paths
        .source_backup
        .join(relative)
        .join("fixture.bin")
        .is_file());
    assert!(paths.operation_root.join("backup_receipt.json").is_file());
}

#[test]
fn operator_installed_validation_delegates_the_exact_journal_to_parent() {
    let fixture = private_fs::private_temp_dir(&std::env::temp_dir()).unwrap();
    let paths = paths(fixture.path());
    let cancel = AtomicBool::new(false);
    let calls = Cell::new(0);
    let validator = |journal: &Journal| {
        assert_eq!(journal.plan.paths, paths);
        calls.set(calls.get() + 1);
        Err(MigrationError::CallerCutoverRequired)
    };
    let platform = require_operator(
        paths.clone(),
        &cancel,
        Instant::now() + Duration::from_secs(30),
        &validator,
    );
    let root = private_fs::inspect_directory_object(&paths.source).unwrap();
    let journal = Journal {
        schema_version: 1,
        revision: 1,
        plan: codex_home_migration::MigrationPlan {
            operation_id: "operation_one".into(),
            paths: paths.clone(),
            source: HomeSnapshot {
                root,
                image: "fixture_image".into(),
                entries: 0,
                bytes: 0,
                reparse_entries: Vec::new(),
            },
            destination: None,
            work_volume: root.volume,
        },
        phase: Phase::Validated,
        pending: None,
        source_backup: None,
        rolled_back: false,
    };
    assert_eq!(
        platform.validate_installed(&journal),
        Err(MigrationError::CallerCutoverRequired)
    );
    assert_eq!(calls.get(), 1);
}

fn completed_copy_with_pending_journal(
    storage: &mut impl Storage,
    paths: &MigrationPaths,
) -> codex_home_migration::MigrationPlan {
    let plan = codex_home_migration::prepare_plan(storage, "operation_one", paths.clone()).unwrap();
    let mut writers = FixtureWriters;
    let mut pending = codex_home_migration::begin(storage, &mut writers, plan.clone()).unwrap();
    pending.revision += 1;
    pending.pending = Some(codex_home_migration::Intent::BackupSource);
    storage
        .store_journal(&paths.journal, &pending, Some(1))
        .unwrap();
    writers.verify(paths).unwrap();
    storage.backup_home_verified(&plan).unwrap();
    plan
}

#[test]
fn operator_durable_memento_recovers_access_times_after_adapter_restart() {
    let fixture = private_fs::private_temp_dir(&std::env::temp_dir()).unwrap();
    let paths = paths(fixture.path());
    let cancel = AtomicBool::new(false);
    let deadline = Instant::now() + Duration::from_secs(60);
    let validator = |_: &Journal| Err(MigrationError::CallerCutoverRequired);
    let platform = require_operator(paths.clone(), &cancel, deadline, &validator);
    let mut storage =
        super::super::NativeStorage::new(paths.clone(), &platform, &cancel, deadline).unwrap();
    let plan = completed_copy_with_pending_journal(&mut storage, &paths);
    let transcript = paths.source.join("history.jsonl");
    let original = platform.metadata(&transcript).unwrap();
    let file = private_fs::open_inventory_security_object(&transcript).unwrap();
    file_times::restore_accessed(&file, original.accessed - 10_000_000).unwrap();
    drop(file);
    assert_eq!(
        storage.verify_snapshot(&paths.source, &plan.source),
        Err(MigrationError::SourceChanged)
    );
    drop(storage);
    let mut reopened =
        super::super::NativeStorage::new(paths.clone(), &platform, &cancel, deadline).unwrap();
    reopened.reconcile_backup_source(&plan).unwrap();
    reopened
        .verify_snapshot(&paths.source, &plan.source)
        .unwrap();
    assert!(platform.metadata(&transcript).unwrap() == original);
    assert!(paths
        .operation_root
        .join("access_memento/manifest.json")
        .is_file());
}

#[test]
fn operator_access_recovery_rejects_non_access_changes_before_any_timestamp_write() {
    let fixture = private_fs::private_temp_dir(&std::env::temp_dir()).unwrap();
    let paths = paths(fixture.path());
    let cancel = AtomicBool::new(false);
    let deadline = Instant::now() + Duration::from_secs(60);
    let validator = |_: &Journal| Err(MigrationError::CallerCutoverRequired);
    let platform = require_operator(paths.clone(), &cancel, deadline, &validator);
    let mut storage =
        super::super::NativeStorage::new(paths.clone(), &platform, &cancel, deadline).unwrap();
    let plan = completed_copy_with_pending_journal(&mut storage, &paths);
    let root = private_fs::open_inventory_security_object(&paths.source).unwrap();
    let root_before = platform.metadata(&paths.source).unwrap();
    let changed_access = root_before.accessed - 10_000_000;
    file_times::restore_accessed(&root, changed_access).unwrap();
    drop(root);
    let transcript = paths.source.join("history.jsonl");
    let file = private_fs::open_inventory_security_object(&transcript).unwrap();
    let mut changed = platform.metadata(&transcript).unwrap();
    changed.written += 10_000_000;
    file_times::restore_all(&file, &changed).unwrap();
    drop(file);
    assert_eq!(
        storage.reconcile_backup_source(&plan),
        Err(MigrationError::SourceChanged)
    );
    assert_eq!(
        platform.metadata(&paths.source).unwrap().accessed,
        changed_access
    );
    assert_eq!(
        platform.metadata(&transcript).unwrap().written,
        changed.written
    );
}

#[test]
fn operator_old_directory_access_times_survive_repeated_whole_home_inventory() {
    let fixture = private_fs::private_temp_dir(&std::env::temp_dir()).unwrap();
    let paths = paths(fixture.path());
    let cancel = AtomicBool::new(false);
    let deadline = Instant::now() + Duration::from_secs(60);
    let validator = |_: &Journal| Err(MigrationError::CallerCutoverRequired);
    let platform = require_operator(paths.clone(), &cancel, deadline, &validator);
    let nested = paths.source.join("old_directory");
    fs::create_dir(&nested).unwrap();
    fs::write(
        nested.join("fixture.bin"),
        b"inventory must preserve old atime",
    )
    .unwrap();
    let storage =
        super::super::NativeStorage::new(paths.clone(), &platform, &cancel, deadline).unwrap();
    let mut expected = Vec::new();
    for path in [&nested, &paths.source] {
        let file = private_fs::open_inventory_security_object(path).unwrap();
        let old = private_fs::inventory_object_metadata_from_file(&file)
            .unwrap()
            .accessed
            - 864_000_000_000;
        file_times::restore_accessed(&file, old).unwrap();
        expected.push((file, old));
    }
    let snapshot = storage.inventory(&paths.source).unwrap();
    storage.verify_snapshot(&paths.source, &snapshot).unwrap();
    storage.verify_snapshot(&paths.source, &snapshot).unwrap();
    for (file, old) in expected {
        assert_eq!(
            private_fs::inventory_object_metadata_from_file(&file)
                .unwrap()
                .accessed,
            old
        );
    }
}
