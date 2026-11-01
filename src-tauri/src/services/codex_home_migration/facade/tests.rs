//! Synthetic storage-contract tests; no native fidelity or ACL proof is claimed.

use super::*;
use std::collections::BTreeMap;

const SOURCE_KEY_OLD: &str = "11111111111111111111111111111111";
const SOURCE_KEY_NEW: &str = "22222222222222222222222222222222";

#[derive(Debug, Clone, PartialEq, Eq)]
struct ObjectId(u64);
#[derive(Debug, Clone, PartialEq, Eq)]
struct BackupProof(u64);
#[derive(Debug, Clone, PartialEq, Eq)]
struct NativeSeal([Vec<&'static str>; 2]);
#[derive(Debug, Clone, PartialEq, Eq)]
struct LogicalId(&'static str);

struct Fixture {
    objects: BTreeMap<PathBuf, ObjectId>,
    backups: Vec<BackupProof>,
    journal: Option<Record<Self>>,
    original_trees: [Vec<&'static str>; 2],
    staged_trees: [Vec<&'static str>; 2],
    owner_selection: OwnerSelection,
    acl_valid: bool,
    journal_owned: bool,
    crash_after_rename: Option<usize>,
    renames: usize,
    mutations: usize,
}

impl FacadeStorage for Fixture {
    type Identity = ObjectId;
    type Backup = BackupProof;
    type FacadeSeal = NativeSeal;
    type LogicalPayloadId = LogicalId;

    fn verify_admission(&self, _plan: &Plan<Self>) -> Result<()> {
        if self.journal_owned {
            Ok(())
        } else {
            Err(MigrationError::ObjectConflict)
        }
    }
    fn identity(&self, path: &Path) -> Result<Option<ObjectId>> {
        Ok(self.objects.get(path).cloned())
    }
    fn verify_backup(&self, home: &OriginalHome<ObjectId, BackupProof, LogicalId>) -> Result<()> {
        if self.backups.contains(&home.backup) && home.backup.0 == home.identity.0 {
            Ok(())
        } else {
            Err(MigrationError::BackupUnverified)
        }
    }
    fn verify_original(
        &self,
        path: &Path,
        home: &OriginalHome<ObjectId, BackupProof, LogicalId>,
    ) -> Result<()> {
        if self.identity(path)? == Some(home.identity.clone()) {
            Ok(())
        } else {
            Err(MigrationError::SourceChanged)
        }
    }
    fn verify_facade(&self, root: &Path, plan: &Plan<Self>) -> Result<()> {
        if self.identity(root)? == Some(plan.staging_identity.clone())
            && self.staged_trees == self.original_trees
            && plan.facade_seal.0 == self.original_trees
            && self.acl_valid
        {
            Ok(())
        } else {
            Err(MigrationError::SourceChanged)
        }
    }
    fn verify_owner_selection(&self, _plan: &Plan<Self>, selection: OwnerSelection) -> Result<()> {
        if self.owner_selection == selection {
            Ok(())
        } else {
            Err(MigrationError::CallerCutoverRequired)
        }
    }
    fn load_journal(&self, _path: &Path) -> Result<Option<Record<Self>>> {
        Ok(self.journal.clone())
    }
    fn store_journal(
        &mut self,
        record: &Record<Self>,
        previous_revision: Option<u64>,
    ) -> Result<()> {
        if !self.journal_owned
            || self.journal.as_ref().map(|entry| entry.revision) != previous_revision
        {
            return Err(MigrationError::JournalConflict);
        }
        self.mutations += 1;
        self.journal = Some(record.clone());
        Ok(())
    }
    fn rename_no_replace(&mut self, from: &Path, to: &Path, expected: &ObjectId) -> Result<()> {
        assert!(
            self.journal.as_ref().unwrap().pending.is_some(),
            "rename must have durable intent"
        );
        if self.identity(from)?.as_ref() != Some(expected) || self.objects.contains_key(to) {
            return Err(MigrationError::ObjectConflict);
        }
        let object = self.objects.remove(from).unwrap();
        self.objects.insert(to.into(), object);
        self.renames += 1;
        self.mutations += 1;
        if self.crash_after_rename == Some(self.renames) {
            self.crash_after_rename = None;
            return Err(MigrationError::PlatformFailure);
        }
        Ok(())
    }
}

#[derive(Default)]
struct Writers {
    calls: usize,
    deny_on: Option<usize>,
}
impl FacadeWriters<Fixture> for Writers {
    fn verify(&mut self, _plan: &Plan<Fixture>) -> Result<()> {
        self.calls += 1;
        if self.deny_on == Some(self.calls) {
            Err(MigrationError::WritersActive)
        } else {
            Ok(())
        }
    }
}

fn fixture() -> (Fixture, Writers, Plan<Fixture>) {
    let root = std::env::temp_dir().join("synthetic_two_home_facade");
    let operation = root.join("operation");
    let trees = [
        vec![
            "old-session",
            "old-message-1",
            "old-message-2",
            "old-attachment-ref",
        ],
        vec![
            "new-session",
            "new-message-1",
            "new-message-2",
            "new-attachment-ref",
        ],
    ];
    let plan = FacadePlan {
        journal: operation.join("journal.json"),
        old_home: OriginalHome {
            root: root.join(".aag/mirrors/codex"),
            quarantine: operation.join("old_quarantine"),
            identity: ObjectId(1),
            backup: BackupProof(1),
            logical_payload_id: LogicalId("old"),
        },
        new_home: OriginalHome {
            root: root.join(".codex-cli"),
            quarantine: operation.join("new_quarantine"),
            identity: ObjectId(2),
            backup: BackupProof(2),
            logical_payload_id: LogicalId("new"),
        },
        staging: operation.join("fresh_facade"),
        staging_identity: ObjectId(3),
        facade_seal: NativeSeal(trees.clone()),
        payloads: [
            FacadePayload {
                relative_path: Path::new(".authority")
                    .join("sources")
                    .join(SOURCE_KEY_OLD)
                    .join("native_home"),
                identity: ObjectId(4),
                logical_payload_id: LogicalId("old"),
            },
            FacadePayload {
                relative_path: Path::new(".authority")
                    .join("sources")
                    .join(SOURCE_KEY_NEW)
                    .join("native_home"),
                identity: ObjectId(5),
                logical_payload_id: LogicalId("new"),
            },
        ],
    };
    let storage = Fixture {
        objects: BTreeMap::from([
            (plan.old_home.root.clone(), ObjectId(1)),
            (plan.new_home.root.clone(), ObjectId(2)),
            (plan.staging.clone(), ObjectId(3)),
        ]),
        backups: vec![BackupProof(1), BackupProof(2)],
        journal: None,
        original_trees: trees.clone(),
        staged_trees: trees,
        owner_selection: OwnerSelection::OriginalHomes,
        acl_valid: true,
        journal_owned: true,
        crash_after_rename: None,
        renames: 0,
        mutations: 0,
    };
    (storage, Writers::default(), plan)
}

fn installed(storage: &mut Fixture, writers: &mut Writers, plan: Plan<Fixture>) -> Record<Fixture> {
    let mut record = begin(storage, writers, plan).unwrap();
    while record.phase != FacadePhase::Validated {
        record = advance(storage, writers, record).unwrap();
    }
    record
}

#[test]
fn both_native_histories_remain_distinct_and_commit_requires_readbacks() {
    let (mut storage, mut writers, plan) = fixture();
    let record = installed(&mut storage, &mut writers, plan.clone());
    assert_eq!(
        storage.identity(&plan.new_home.root).unwrap(),
        Some(ObjectId(3))
    );
    assert_eq!(
        storage.identity(&plan.old_home.quarantine).unwrap(),
        Some(ObjectId(1))
    );
    assert_eq!(
        storage.identity(&plan.new_home.quarantine).unwrap(),
        Some(ObjectId(2))
    );
    assert_eq!(storage.staged_trees[0][0], "old-session");
    assert_eq!(storage.staged_trees[1][0], "new-session");
    assert_eq!(
        commit(&mut storage, &mut writers, record.clone()),
        Err(MigrationError::CallerCutoverRequired)
    );
    storage.owner_selection = OwnerSelection::InstalledFacade;
    storage.acl_valid = false;
    assert!(commit(&mut storage, &mut writers, record.clone()).is_err());
    storage.acl_valid = true;
    storage.staged_trees[1].swap(1, 2);
    assert!(commit(&mut storage, &mut writers, record.clone()).is_err());
    storage.staged_trees = storage.original_trees.clone();
    let committed = commit(&mut storage, &mut writers, record).unwrap();
    assert_eq!(committed.phase, FacadePhase::Committed);
    assert_eq!(
        rollback(&mut storage, &mut writers, committed),
        Err(MigrationError::AlreadyCommitted)
    );
}

#[test]
fn crash_after_each_forward_and_reverse_rename_recovers_and_restores_both_originals() {
    for crash in 1..=6 {
        let (mut storage, mut writers, plan) = fixture();
        storage.crash_after_rename = Some(crash);
        let mut record = begin(&mut storage, &mut writers, plan.clone()).unwrap();
        while record.phase != FacadePhase::Validated {
            match advance(&mut storage, &mut writers, record) {
                Ok(next) => record = next,
                Err(MigrationError::PlatformFailure) => {
                    record = recover(&mut storage, &mut writers, &plan).unwrap()
                }
                Err(error) => panic!("unexpected forward error: {error:?}"),
            }
        }
        record = match rollback(&mut storage, &mut writers, record) {
            Ok(next) => next,
            Err(MigrationError::PlatformFailure) => {
                let recovered = recover(&mut storage, &mut writers, &plan).unwrap();
                rollback(&mut storage, &mut writers, recovered).unwrap()
            }
            Err(error) => panic!("unexpected rollback error: {error:?}"),
        };
        assert_eq!(record.phase, FacadePhase::RolledBack);
        assert_eq!(
            storage.identity(&plan.old_home.root).unwrap(),
            Some(ObjectId(1))
        );
        assert_eq!(
            storage.identity(&plan.new_home.root).unwrap(),
            Some(ObjectId(2))
        );
        assert_eq!(storage.identity(&plan.staging).unwrap(), Some(ObjectId(3)));
        assert!(storage
            .identity(&plan.old_home.quarantine)
            .unwrap()
            .is_none());
        assert!(storage
            .identity(&plan.new_home.quarantine)
            .unwrap()
            .is_none());
    }
}

#[test]
fn recovery_refuses_foreign_occupied_roots_and_foreign_journal_objects() {
    for foreign_journal in [false, true] {
        let (mut storage, mut writers, plan) = fixture();
        storage.crash_after_rename = Some(1);
        let record = begin(&mut storage, &mut writers, plan.clone()).unwrap();
        assert_eq!(
            advance(&mut storage, &mut writers, record),
            Err(MigrationError::PlatformFailure)
        );
        if foreign_journal {
            storage.journal_owned = false;
        } else {
            storage
                .objects
                .insert(plan.old_home.root.clone(), ObjectId(999));
        }
        let mutations = storage.mutations;
        assert!(recover(&mut storage, &mut writers, &plan).is_err());
        let record = storage.journal.clone().unwrap();
        assert!(rollback(&mut storage, &mut writers, record).is_err());
        assert_eq!(storage.mutations, mutations);
    }
}

#[test]
fn every_mutation_requires_a_fresh_old_home_writer_check() {
    let (mut baseline, mut writers, plan) = fixture();
    let record = installed(&mut baseline, &mut writers, plan);
    rollback(&mut baseline, &mut writers, record).unwrap();
    let total_mutations = baseline.mutations;
    for deny_on in 1..=total_mutations {
        let (mut storage, mut writers, plan) = fixture();
        writers.deny_on = Some(deny_on);
        let result = begin(&mut storage, &mut writers, plan).and_then(|mut record| {
            while record.phase != FacadePhase::Validated {
                record = advance(&mut storage, &mut writers, record)?;
            }
            rollback(&mut storage, &mut writers, record)
        });
        assert_eq!(result, Err(MigrationError::WritersActive));
        assert_eq!(
            storage.mutations,
            deny_on - 1,
            "writer refusal must stop its matching mutation"
        );
    }
}

#[test]
fn stale_cas_handle_cannot_advance_or_rollback_a_newer_record() {
    let (mut storage, mut writers, plan) = fixture();
    let stale = begin(&mut storage, &mut writers, plan).unwrap();
    advance(&mut storage, &mut writers, stale.clone()).unwrap();
    let mutations = storage.mutations;
    assert_eq!(
        advance(&mut storage, &mut writers, stale.clone()),
        Err(MigrationError::JournalConflict)
    );
    assert_eq!(
        rollback(&mut storage, &mut writers, stale),
        Err(MigrationError::JournalConflict)
    );
    assert_eq!(storage.mutations, mutations);
}

#[test]
fn both_complete_backup_proofs_and_exact_payload_mappings_are_required() {
    for missing in [BackupProof(1), BackupProof(2)] {
        let (mut storage, mut writers, plan) = fixture();
        storage.backups.retain(|proof| proof != &missing);
        assert_eq!(
            begin(&mut storage, &mut writers, plan),
            Err(MigrationError::BackupUnverified)
        );
        assert_eq!(storage.mutations, 0);
    }
    let (mut storage, mut writers, mut plan) = fixture();
    plan.payloads[1].logical_payload_id = LogicalId("old");
    assert!(begin(&mut storage, &mut writers, plan).is_err());
    assert_eq!(storage.mutations, 0);
}

#[test]
fn only_unique_canonical_source_key_native_home_paths_are_admitted() {
    let (mut storage, mut writers, mut plan) = fixture();
    plan.payloads[0].relative_path = ".authority/sources/sourceOld".into();
    plan.payloads[1].relative_path = ".authority/sources/sourceNew".into();
    assert_eq!(
        begin(&mut storage, &mut writers, plan),
        Err(MigrationError::InvalidPaths)
    );
    assert_eq!(storage.mutations, 0);
    for invalid in [
        ".authority/sources/ABCDEF0123456789ABCDEF0123456789/native_home",
        ".authority/sources/1111111111111111111111111111111/native_home",
        ".authority/sources/11111111111111111111111111111111/other_home",
        ".authority/sources/../11111111111111111111111111111111/native_home",
        ".authority/./sources/11111111111111111111111111111111/native_home",
        ".authority/sources/11111111111111111111111111111111/native_home/extra",
    ] {
        let (mut storage, mut writers, mut plan) = fixture();
        plan.payloads[0].relative_path = invalid.into();
        assert!(begin(&mut storage, &mut writers, plan).is_err());
        assert_eq!(storage.mutations, 0);
    }
    let (mut storage, mut writers, mut plan) = fixture();
    plan.payloads[1].relative_path = plan.payloads[0].relative_path.clone();
    assert_eq!(
        begin(&mut storage, &mut writers, plan),
        Err(MigrationError::ObjectConflict)
    );
    assert_eq!(storage.mutations, 0);
}
