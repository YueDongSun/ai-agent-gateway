//! In-memory contract fixtures only. Their copy emulator is not evidence of
//! Windows timestamp, ACL or alternate-stream backup fidelity.

use super::*;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Clone)]
struct Home {
    snapshot: HomeSnapshot,
    synthetic_history: Vec<u8>,
}

struct FixtureStorage {
    homes: BTreeMap<PathBuf, Home>,
    journals: BTreeMap<PathBuf, Journal>,
    backup_receipts: BTreeMap<String, HomeSnapshot>,
    redirects: BTreeMap<PathBuf, PathBuf>,
    mutations: Vec<&'static str>,
    crash_after_install: bool,
    interrupted_access_snapshot: Option<HomeSnapshot>,
    access_reconciliations: usize,
}

impl Storage for FixtureStorage {
    fn canonical_path(&self, path: &Path) -> Result<PathBuf> {
        for (prefix, target) in &self.redirects {
            if let Ok(suffix) = path.strip_prefix(prefix) {
                return Ok(target.join(suffix));
            }
        }
        Ok(path.components().collect())
    }
    fn directory_identity(&self, path: &Path) -> Result<Option<DirectoryIdentity>> {
        Ok(self.homes.get(path).map(|home| home.snapshot.root))
    }
    fn inventory(&self, path: &Path) -> Result<HomeSnapshot> {
        self.homes
            .get(path)
            .map(|home| home.snapshot.clone())
            .ok_or(MigrationError::SourceUnavailable)
    }
    fn load_journal(&self, path: &Path) -> Result<Option<Journal>> {
        Ok(self.journals.get(path).cloned())
    }
    fn verify_snapshot(&self, path: &Path, expected: &HomeSnapshot) -> Result<()> {
        if self.inventory(path)? == *expected {
            Ok(())
        } else {
            Err(MigrationError::SourceChanged)
        }
    }
    fn store_journal(
        &mut self,
        path: &Path,
        journal: &Journal,
        previous_revision: Option<u64>,
    ) -> Result<()> {
        if self.journals.get(path).map(|value| value.revision) != previous_revision {
            return Err(MigrationError::JournalConflict);
        }
        self.mutations.push("journal");
        self.journals.insert(path.into(), journal.clone());
        Ok(())
    }
    fn backup_home_verified(&mut self, plan: &MigrationPlan) -> Result<HomeSnapshot> {
        self.verify_snapshot(&plan.paths.source, &plan.source)?;
        if let Some(receipt) = self.backup_receipts.get(&plan.operation_id) {
            self.verify_snapshot(&plan.paths.source_backup, receipt)?;
            return Ok(receipt.clone());
        }
        if self.homes.contains_key(&plan.paths.source_backup) {
            return Err(MigrationError::ObjectConflict);
        }
        let mut backup = self.homes[&plan.paths.source].clone();
        backup.snapshot.root.object = 100;
        let receipt = backup.snapshot.clone();
        self.mutations.push("backup");
        self.homes.insert(plan.paths.source_backup.clone(), backup);
        self.backup_receipts
            .insert(plan.operation_id.clone(), receipt.clone());
        Ok(receipt)
    }
    fn reconcile_backup_source(&mut self, plan: &MigrationPlan) -> Result<()> {
        self.access_reconciliations += 1;
        let Some(original) = self.interrupted_access_snapshot.as_ref() else {
            return Ok(());
        };
        let source = self.homes.get_mut(&plan.paths.source).unwrap();
        if original != &plan.source
            || source.snapshot.root != original.root
            || source.snapshot.image != format!("{}_access_only", original.image)
        {
            return Err(MigrationError::SourceChanged);
        }
        source.snapshot = original.clone();
        self.interrupted_access_snapshot = None;
        Ok(())
    }
    fn rename_no_replace(
        &mut self,
        from: &Path,
        to: &Path,
        expected: DirectoryIdentity,
    ) -> Result<()> {
        if self.directory_identity(from)? != Some(expected) || self.homes.contains_key(to) {
            return Err(MigrationError::ObjectConflict);
        }
        let home = self.homes.remove(from).unwrap();
        self.homes.insert(to.into(), home);
        self.mutations.push("rename");
        if self.crash_after_install && to.file_name().is_some_and(|name| name == ".codex-cli") {
            self.crash_after_install = false;
            return Err(MigrationError::PlatformFailure);
        }
        Ok(())
    }
    fn retarget_latest(
        &mut self,
        _root: &Path,
        _expected: DirectoryIdentity,
        _alias: &ChromeLatest,
        _target: &Path,
    ) -> Result<()> {
        self.mutations.push("retarget");
        Ok(())
    }
    fn validate_installed(&self, journal: &Journal) -> Result<()> {
        let plan = &journal.plan;
        self.verify_snapshot(&plan.paths.destination, &plan.source)?;
        if let Some(original) = &plan.destination {
            self.verify_snapshot(&plan.paths.destination_archive, original)?;
        }
        let backup = journal
            .source_backup
            .as_ref()
            .ok_or(MigrationError::BackupUnverified)?;
        self.verify_snapshot(&plan.paths.source_backup, backup)
    }
}

struct Writers {
    calls: usize,
    active: bool,
    deny_on: Option<usize>,
}
impl WriterQuiescence for Writers {
    fn verify(&mut self, _paths: &MigrationPaths) -> Result<()> {
        self.calls += 1;
        if self.active || self.deny_on == Some(self.calls) {
            Err(MigrationError::WritersActive)
        } else {
            Ok(())
        }
    }
}
struct Cutover(PathBuf);
impl CutoverState for Cutover {
    fn selected_home(&self) -> Result<PathBuf> {
        Ok(self.0.clone())
    }
}

fn home(object: u64, image: &str, data: &[u8]) -> Home {
    Home {
        snapshot: HomeSnapshot {
            root: DirectoryIdentity { volume: 7, object },
            image: image.into(),
            entries: 1,
            bytes: data.len() as u64,
            reparse_entries: Vec::new(),
        },
        synthetic_history: data.to_vec(),
    }
}

fn fixture() -> (FixtureStorage, Writers, MigrationPaths) {
    let root = std::env::temp_dir().join("codex_home_migration_contract_fixture");
    let operation = root.join(".aag/codex_home_migrations/operation_one");
    let paths = MigrationPaths {
        home: root.clone(),
        source: root.join(".aag/mirrors/codex"),
        destination: root.join(".codex-cli"),
        operation_root: operation.clone(),
        source_backup: operation.join("source_backup"),
        destination_archive: operation.join("destination_before"),
        journal: operation.join("migration.json"),
    };
    let storage = FixtureStorage {
        homes: BTreeMap::from([
            (root, home(1, "home_root", b"")),
            (
                paths.source.clone(),
                home(2, "source_image", b"synthetic_source_history"),
            ),
            (
                paths.destination.clone(),
                home(3, "destination_image", b"synthetic_destination_history"),
            ),
        ]),
        journals: BTreeMap::new(),
        backup_receipts: BTreeMap::new(),
        redirects: BTreeMap::new(),
        mutations: Vec::new(),
        crash_after_install: false,
        interrupted_access_snapshot: None,
        access_reconciliations: 0,
    };
    (
        storage,
        Writers {
            calls: 0,
            active: false,
            deny_on: None,
        },
        paths,
    )
}

#[test]
fn pending_backup_recovers_access_times_before_full_source_validation() {
    let (mut storage, mut writers, paths) = fixture();
    let plan = prepare_plan(&storage, "operation_one", paths.clone()).unwrap();
    let mut journal = begin(&mut storage, &mut writers, plan).unwrap();
    journal.pending = Some(Intent::BackupSource);
    journal.revision += 1;
    storage
        .journals
        .insert(paths.journal.clone(), journal.clone());
    storage.interrupted_access_snapshot = Some(journal.plan.source.clone());
    storage
        .homes
        .get_mut(&paths.source)
        .unwrap()
        .snapshot
        .image
        .push_str("_access_only");
    let recovered = recover(&mut storage, &mut writers, &paths).unwrap();
    assert_eq!(recovered.phase, Phase::BackedUp);
    assert_eq!(
        storage.inventory(&paths.source).unwrap(),
        journal.plan.source
    );
    assert_eq!(storage.access_reconciliations, 1);
}

#[test]
fn access_recovery_requires_pending_backup_and_fresh_writer_exclusion() {
    let (mut storage, mut writers, paths) = fixture();
    let plan = prepare_plan(&storage, "operation_one", paths.clone()).unwrap();
    let mut journal = begin(&mut storage, &mut writers, plan).unwrap();
    storage.interrupted_access_snapshot = Some(journal.plan.source.clone());
    storage
        .homes
        .get_mut(&paths.source)
        .unwrap()
        .snapshot
        .image
        .push_str("_access_only");
    assert_eq!(
        recover(&mut storage, &mut writers, &paths),
        Err(MigrationError::SourceChanged)
    );
    assert_eq!(storage.access_reconciliations, 0);
    journal.pending = Some(Intent::BackupSource);
    journal.revision += 1;
    storage.journals.insert(paths.journal.clone(), journal);
    writers.active = true;
    assert_eq!(
        recover(&mut storage, &mut writers, &paths),
        Err(MigrationError::WritersActive)
    );
    assert_eq!(storage.access_reconciliations, 0);
    assert!(storage.interrupted_access_snapshot.is_some());
}

fn until_phase(
    storage: &mut FixtureStorage,
    writers: &mut Writers,
    mut journal: Journal,
    phase: Phase,
) -> Journal {
    for _ in 0..8 {
        if journal.phase == phase {
            return journal;
        }
        journal = advance(storage, writers, journal).unwrap();
    }
    panic!("migration did not reach requested phase");
}

#[test]
fn cutover_backs_up_whole_source_and_preserves_distinct_destination_history() {
    let (mut storage, mut writers, paths) = fixture();
    let plan = prepare_plan(&storage, "operation_one", paths.clone()).unwrap();
    assert!(storage.mutations.is_empty(), "planning is read-only");
    let journal = begin(&mut storage, &mut writers, plan).unwrap();
    let journal = until_phase(&mut storage, &mut writers, journal, Phase::Validated);
    assert_eq!(
        storage.homes[&paths.destination].synthetic_history,
        b"synthetic_source_history"
    );
    assert_eq!(
        storage.homes[&paths.destination_archive].synthetic_history,
        b"synthetic_destination_history"
    );
    assert_eq!(
        storage.homes[&paths.source_backup].synthetic_history,
        b"synthetic_source_history"
    );
    assert!(!storage.homes.contains_key(&paths.source));
    assert!(
        storage
            .mutations
            .iter()
            .position(|op| *op == "backup")
            .unwrap()
            < storage
                .mutations
                .iter()
                .position(|op| *op == "rename")
                .unwrap()
    );
    let journal = commit(
        &mut storage,
        &mut writers,
        &Cutover(paths.destination.clone()),
        journal,
    )
    .unwrap();
    assert_eq!(journal.phase, Phase::Committed);
    assert_eq!(
        writers.calls,
        storage.mutations.len() + storage.access_reconciliations,
        "every mutation and access-recovery admission requires a fresh writer check"
    );
}

#[test]
fn crash_phases_roll_back_without_discarding_either_history_or_verified_backup() {
    for phase in [
        Phase::Planned,
        Phase::BackedUp,
        Phase::DestinationPreserved,
        Phase::SourceInstalled,
        Phase::Validated,
    ] {
        let (mut storage, mut writers, paths) = fixture();
        let plan = prepare_plan(&storage, "operation_one", paths.clone()).unwrap();
        let journal = begin(&mut storage, &mut writers, plan).unwrap();
        let journal = until_phase(&mut storage, &mut writers, journal, phase);
        let rolled_back = rollback(
            &mut storage,
            &mut writers,
            &Cutover(paths.source.clone()),
            journal,
        )
        .unwrap();
        assert!(rolled_back.rolled_back);
        assert_eq!(
            storage.homes[&paths.source].synthetic_history,
            b"synthetic_source_history"
        );
        assert_eq!(
            storage.homes[&paths.destination].synthetic_history,
            b"synthetic_destination_history"
        );
        if phase != Phase::Planned {
            assert_eq!(
                storage.homes[&paths.source_backup].synthetic_history,
                b"synthetic_source_history"
            );
        }
    }
}

#[test]
fn interrupted_install_resumes_by_object_identity_instead_of_repeating_rename() {
    let (mut storage, mut writers, paths) = fixture();
    let plan = prepare_plan(&storage, "operation_one", paths.clone()).unwrap();
    let journal = begin(&mut storage, &mut writers, plan).unwrap();
    let journal = until_phase(
        &mut storage,
        &mut writers,
        journal,
        Phase::DestinationPreserved,
    );
    storage.crash_after_install = true;
    assert_eq!(
        advance(&mut storage, &mut writers, journal).unwrap_err(),
        MigrationError::PlatformFailure
    );
    let recovered = recover(&mut storage, &mut writers, &paths).unwrap();
    assert_eq!(recovered.phase, Phase::SourceInstalled);
    assert!(recovered.pending.is_none());
    let recovered = until_phase(&mut storage, &mut writers, recovered, Phase::Validated);
    assert_eq!(
        commit(
            &mut storage,
            &mut writers,
            &Cutover(paths.destination),
            recovered
        )
        .unwrap()
        .phase,
        Phase::Committed
    );
}

#[test]
fn changed_source_foreign_destination_and_active_writers_are_never_adopted() {
    let (mut storage, mut writers, paths) = fixture();
    let plan = prepare_plan(&storage, "operation_one", paths.clone()).unwrap();
    let journal = begin(&mut storage, &mut writers, plan).unwrap();
    storage.homes.get_mut(&paths.source).unwrap().snapshot.image = "changed_source".into();
    assert_eq!(
        advance(&mut storage, &mut writers, journal).unwrap_err(),
        MigrationError::SourceChanged
    );
    assert!(storage.homes.contains_key(&paths.destination));
    let (mut storage, mut writers, paths) = fixture();
    let plan = prepare_plan(&storage, "operation_one", paths.clone()).unwrap();
    let journal = begin(&mut storage, &mut writers, plan).unwrap();
    let journal = until_phase(&mut storage, &mut writers, journal, Phase::BackedUp);
    storage
        .homes
        .get_mut(&paths.destination)
        .unwrap()
        .snapshot
        .root
        .object = 900;
    assert_eq!(
        advance(&mut storage, &mut writers, journal).unwrap_err(),
        MigrationError::ObjectConflict
    );
    assert_eq!(storage.homes[&paths.destination].snapshot.root.object, 900);
    let (mut storage, mut writers, paths) = fixture();
    let plan = prepare_plan(&storage, "operation_one", paths).unwrap();
    writers.active = true;
    assert_eq!(
        begin(&mut storage, &mut writers, plan).unwrap_err(),
        MigrationError::WritersActive
    );
    assert!(storage.mutations.is_empty());
}

#[test]
fn canonical_nested_escape_and_cross_volume_targets_fail_before_mutation() {
    let (mut storage, _writers, mut paths) = fixture();
    storage.redirects.insert(
        paths.operation_root.clone(),
        paths.home.parent().unwrap().join("outside_operation"),
    );
    assert_eq!(
        prepare_plan(&storage, "operation_one", paths.clone()).unwrap_err(),
        MigrationError::InvalidPaths
    );
    storage.redirects.clear();
    storage
        .homes
        .get_mut(&paths.destination)
        .unwrap()
        .snapshot
        .root
        .volume = 8;
    assert_eq!(
        prepare_plan(&storage, "operation_one", paths.clone()).unwrap_err(),
        MigrationError::DifferentVolume
    );
    paths.destination = paths.source.join("nested_destination");
    assert_eq!(
        prepare_plan(&storage, "operation_one", paths).unwrap_err(),
        MigrationError::InvalidPaths
    );
    assert!(storage.mutations.is_empty());
}

#[test]
fn only_same_package_chrome_latest_alias_is_admitted() {
    let (mut storage, _writers, paths) = fixture();
    let relative = PathBuf::from("plugins/cache/openai-bundled/chrome/26.1021.31345");
    let version = paths.source.join(&relative);
    let target = home(20, "chrome_version", b"");
    let target_identity = target.snapshot.root;
    storage.homes.insert(version.clone(), target);
    let alias = ChromeLatest {
        relative_path: PathBuf::from("plugins/cache/openai-bundled/chrome/latest"),
        raw_target: version,
        target_relative: relative,
        target_identity,
    };
    storage
        .homes
        .get_mut(&paths.source)
        .unwrap()
        .snapshot
        .reparse_entries = vec![alias.clone()];
    assert_eq!(
        prepare_plan(&storage, "operation_one", paths.clone())
            .unwrap()
            .source
            .reparse_entries,
        [alias.clone()]
    );
    storage
        .homes
        .get_mut(&paths.source)
        .unwrap()
        .snapshot
        .reparse_entries[0]
        .relative_path = PathBuf::from("skills/unrecognized_alias");
    assert_eq!(
        prepare_plan(&storage, "operation_one", paths.clone()).unwrap_err(),
        MigrationError::ForbiddenLink
    );
    storage
        .homes
        .get_mut(&paths.source)
        .unwrap()
        .snapshot
        .reparse_entries = vec![alias];
    storage
        .homes
        .get_mut(&paths.source)
        .unwrap()
        .snapshot
        .reparse_entries[0]
        .raw_target = paths.home.parent().unwrap().join("outside_version");
    assert_eq!(
        prepare_plan(&storage, "operation_one", paths).unwrap_err(),
        MigrationError::ForbiddenLink
    );
    assert!(storage.mutations.is_empty());
}

#[test]
fn commit_requires_parent_cutover_readback_and_committed_migrations_do_not_auto_rollback() {
    let (mut storage, mut writers, paths) = fixture();
    let plan = prepare_plan(&storage, "operation_one", paths.clone()).unwrap();
    let journal = begin(&mut storage, &mut writers, plan).unwrap();
    let journal = until_phase(&mut storage, &mut writers, journal, Phase::Validated);
    assert_eq!(
        commit(
            &mut storage,
            &mut writers,
            &Cutover(paths.source.clone()),
            journal.clone()
        )
        .unwrap_err(),
        MigrationError::CallerCutoverRequired
    );
    let committed = commit(
        &mut storage,
        &mut writers,
        &Cutover(paths.destination.clone()),
        journal,
    )
    .unwrap();
    assert_eq!(
        rollback(
            &mut storage,
            &mut writers,
            &Cutover(paths.destination),
            committed
        )
        .unwrap_err(),
        MigrationError::AlreadyCommitted
    );
}

#[test]
fn writer_check_is_repeated_after_intent_before_directory_mutation() {
    let (mut storage, mut writers, paths) = fixture();
    let plan = prepare_plan(&storage, "operation_one", paths.clone()).unwrap();
    let journal = begin(&mut storage, &mut writers, plan).unwrap();
    let journal = until_phase(&mut storage, &mut writers, journal, Phase::BackedUp);
    writers.deny_on = Some(writers.calls + 2);
    assert_eq!(
        advance(&mut storage, &mut writers, journal).unwrap_err(),
        MigrationError::WritersActive
    );
    assert_eq!(storage.homes[&paths.source].snapshot.root.object, 2);
    assert_eq!(storage.homes[&paths.destination].snapshot.root.object, 3);
    assert!(!storage.homes.contains_key(&paths.destination_archive));
    assert_eq!(
        storage.journals[&paths.journal].pending,
        Some(Intent::PreserveDestination)
    );
}

#[test]
fn stale_journals_and_new_destination_objects_never_overwrite_unrelated_data() {
    let (mut storage, mut writers, paths) = fixture();
    let plan = prepare_plan(&storage, "operation_one", paths.clone()).unwrap();
    let journal = begin(&mut storage, &mut writers, plan).unwrap();
    let backed_up = advance(&mut storage, &mut writers, journal.clone()).unwrap();
    assert_eq!(
        advance(&mut storage, &mut writers, journal).unwrap_err(),
        MigrationError::JournalConflict
    );
    let preserved = advance(&mut storage, &mut writers, backed_up).unwrap();
    storage.homes.insert(
        paths.destination.clone(),
        home(900, "foreign_image", b"unrelated_history"),
    );
    assert_eq!(
        advance(&mut storage, &mut writers, preserved.clone()).unwrap_err(),
        MigrationError::ObjectConflict
    );
    assert_eq!(
        rollback(
            &mut storage,
            &mut writers,
            &Cutover(paths.source.clone()),
            preserved
        )
        .unwrap_err(),
        MigrationError::ObjectConflict
    );
    assert_eq!(
        storage.homes[&paths.destination].synthetic_history,
        b"unrelated_history"
    );
    assert_eq!(
        storage.homes[&paths.source].synthetic_history,
        b"synthetic_source_history"
    );
    assert_eq!(
        storage.homes[&paths.destination_archive].synthetic_history,
        b"synthetic_destination_history"
    );
}

#[test]
fn absent_destination_installs_source_without_inventing_a_second_history() {
    let (mut storage, mut writers, paths) = fixture();
    storage.homes.remove(&paths.destination);
    let plan = prepare_plan(&storage, "operation_one", paths.clone()).unwrap();
    assert!(plan.destination.is_none());
    let journal = begin(&mut storage, &mut writers, plan).unwrap();
    let journal = until_phase(&mut storage, &mut writers, journal, Phase::Validated);
    assert!(!storage.homes.contains_key(&paths.destination_archive));
    assert_eq!(
        storage.homes[&paths.destination].synthetic_history,
        b"synthetic_source_history"
    );
    assert_eq!(
        commit(
            &mut storage,
            &mut writers,
            &Cutover(paths.destination),
            journal
        )
        .unwrap()
        .phase,
        Phase::Committed
    );
}
