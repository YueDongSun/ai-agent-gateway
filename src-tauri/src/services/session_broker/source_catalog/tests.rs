use super::*;
use crate::services::session_broker::{NativeSessionId, NativeThreadId, PayloadObjectId};

const SID_ONE: &str = "S-1-5-80-1-2-3-4-5";
const SID_THREE: &str = "S-1-5-80-31-32-33-34-35";

fn key() -> SourceKey {
    SourceKey::parse("0123456789abcdef0123456789abcdef").unwrap()
}
fn reference() -> ThreadReference {
    ThreadReference {
        canonical_session_id: CanonicalSessionId("canonical_fixture_tree".into()),
        owner_instance_id: "authority_fixture".into(),
        native_session_id: NativeSessionId::new("native_fixture_tree").unwrap(),
        native_thread_id: NativeThreadId::new("native_fixture_thread").unwrap(),
        payload_object_id: PayloadObjectId::new("native://fixture/root").unwrap(),
    }
}
fn root(object: u64, mode: SourceMode, sid: Option<&str>) -> VerifiedSourceRoot {
    VerifiedSourceRoot {
        path: PathBuf::from(r"C:\fixture\canonical_source"),
        identity: Managed_directory_identity { volume: 1, object },
        mode,
        runtime_sid: sid.map(str::to_owned),
    }
}
fn populated() -> SourceCatalog {
    let catalog = SourceCatalog::from_connection(Connection::open_in_memory().unwrap()).unwrap();
    catalog
        .register(
            &key(),
            &reference(),
            root(11, SourceMode::Writable, Some(SID_ONE)),
        )
        .unwrap();
    catalog
}

#[test]
fn a_native_tree_and_physical_root_have_one_authoritative_binding() {
    let catalog = populated();
    let first = catalog.active(&key()).unwrap();
    assert_eq!(
        catalog
            .register(
                &key(),
                &reference(),
                root(11, SourceMode::Writable, Some(SID_ONE))
            )
            .unwrap(),
        first
    );
    let other_key = SourceKey::parse("1123456789abcdef0123456789abcdef").unwrap();
    assert!(catalog
        .register(
            &other_key,
            &reference(),
            root(99, SourceMode::Writable, Some(SID_THREE))
        )
        .is_err());
    let mut other_reference = reference();
    other_reference.canonical_session_id = CanonicalSessionId("different_canonical_tree".into());
    other_reference.native_session_id = NativeSessionId::new("different_native_tree").unwrap();
    assert!(catalog
        .register(
            &other_key,
            &other_reference,
            root(11, SourceMode::Writable, Some(SID_THREE))
        )
        .is_err());
    assert_eq!(catalog.active(&key()).unwrap(), first);
}

#[test]
fn an_idempotent_registration_cannot_change_the_native_owner_or_tree() {
    let catalog = populated();
    let mut other = reference();
    other.owner_instance_id = "different_owner".into();
    assert!(matches!(
        catalog.register(
            &key(),
            &other,
            root(11, SourceMode::Writable, Some(SID_ONE))
        ),
        Err(CatalogError::Conflict)
    ));
    other = reference();
    other.native_session_id = NativeSessionId::new("different_tree").unwrap();
    assert!(catalog
        .register(
            &key(),
            &other,
            root(11, SourceMode::Writable, Some(SID_ONE))
        )
        .is_err());
}

#[test]
fn source_transition_blocks_admission_and_survives_database_reopening() {
    let fixture = crate::config::private_fs::private_temp_dir(&std::env::temp_dir()).unwrap();
    let database = fixture.path().join("source_catalog.sqlite");
    let intent;
    {
        let catalog = SourceCatalog::from_connection(Connection::open(&database).unwrap()).unwrap();
        catalog
            .register(
                &key(),
                &reference(),
                root(11, SourceMode::Writable, Some(SID_ONE)),
            )
            .unwrap();
        intent = catalog.begin(&key(), 1, SourceMode::ReadOnly).unwrap();
        assert!(matches!(
            catalog.active(&key()),
            Err(CatalogError::RecoveryRequired)
        ));
        assert!(matches!(
            catalog.begin(&key(), 1, SourceMode::ReadOnly),
            Err(CatalogError::RecoveryRequired)
        ));
    }
    let reopened = SourceCatalog::from_connection(Connection::open(database).unwrap()).unwrap();
    assert_eq!(reopened.pending(&key()).unwrap(), Some(intent));
    assert!(matches!(
        reopened.active(&key()),
        Err(CatalogError::RecoveryRequired)
    ));
}

#[test]
fn sealing_requires_a_new_object_and_reconciles_only_the_exact_committed_transition() {
    let catalog = populated();
    let intent = catalog.begin(&key(), 1, SourceMode::ReadOnly).unwrap();
    assert!(matches!(
        catalog.commit(&intent, root(11, SourceMode::ReadOnly, None)),
        Err(CatalogError::Conflict)
    ));
    assert_eq!(catalog.pending(&key()).unwrap(), Some(intent.clone()));
    let sealed = catalog
        .commit(&intent, root(22, SourceMode::ReadOnly, None))
        .unwrap();
    assert_eq!(sealed.physical_generation, 2);
    assert_eq!(sealed.mode, SourceMode::ReadOnly);
    assert!(sealed.runtime_sid.is_none());
    assert_eq!(
        catalog
            .commit(&intent, root(22, SourceMode::ReadOnly, None))
            .unwrap(),
        sealed
    );
    let mut foreign = intent;
    foreign.transition_id = "not_this_transition".into();
    assert!(matches!(
        catalog.commit(&foreign, root(22, SourceMode::ReadOnly, None)),
        Err(CatalogError::StaleGeneration)
    ));
}

#[test]
fn reopening_never_regrants_an_old_runtime_principal() {
    let catalog = populated();
    let seal = catalog.begin(&key(), 1, SourceMode::ReadOnly).unwrap();
    catalog
        .commit(&seal, root(22, SourceMode::ReadOnly, None))
        .unwrap();
    let open = catalog.begin(&key(), 2, SourceMode::Writable).unwrap();
    assert!(matches!(
        catalog.commit(&open, root(22, SourceMode::Writable, Some(SID_ONE))),
        Err(CatalogError::Conflict)
    ));
    assert_eq!(catalog.pending(&key()).unwrap(), Some(open.clone()));
    let reopened = catalog
        .commit(&open, root(22, SourceMode::Writable, Some(SID_THREE)))
        .unwrap();
    assert_eq!(reopened.physical_generation, 3);
    assert_eq!(reopened.runtime_sid.as_deref(), Some(SID_THREE));
    assert!(catalog.pending(&key()).unwrap().is_none());
}

#[test]
fn stale_and_foreign_cutovers_cannot_replace_a_pending_source() {
    let catalog = populated();
    assert!(matches!(
        catalog.begin(&key(), 0, SourceMode::ReadOnly),
        Err(CatalogError::StaleGeneration)
    ));
    let intent = catalog.begin(&key(), 1, SourceMode::ReadOnly).unwrap();
    let mut wrong = intent.clone();
    wrong.target_generation = 3;
    assert!(catalog
        .commit(&wrong, root(22, SourceMode::ReadOnly, None))
        .is_err());
    let mut redirected = root(22, SourceMode::ReadOnly, None);
    redirected.path = PathBuf::from(r"C:\fixture\foreign");
    assert!(catalog.commit(&intent, redirected).is_err());
    assert_eq!(catalog.pending(&key()).unwrap(), Some(intent));
}

#[test]
fn concurrent_cutovers_share_one_persistent_admission_fence() {
    let catalog = populated();
    let left = catalog.clone();
    let right = catalog.clone();
    let first = std::thread::spawn(move || left.begin(&key(), 1, SourceMode::ReadOnly));
    let second = std::thread::spawn(move || right.begin(&key(), 1, SourceMode::ReadOnly));
    let results = [first.join().unwrap(), second.join().unwrap()];
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(CatalogError::RecoveryRequired)))
            .count(),
        1
    );
    assert!(catalog.pending(&key()).unwrap().is_some());
}

#[test]
fn generic_sids_and_ambiguous_source_keys_cannot_be_storage_authority() {
    for invalid in [
        "",
        "ABCDEF0123456789abcdef0123456789ab",
        "../0123456789abcdef0123456789abcdef",
    ] {
        assert!(SourceKey::parse(invalid).is_err());
    }
    for invalid in [
        "S-1-1-0",
        "S-1-5-33",
        "S-1-5-5-1-2",
        "S-1-5-80-01-2-3-4-5",
        "S-1-5-80-1-2-3-4",
    ] {
        assert!(validate_root(&root(11, SourceMode::Writable, Some(invalid))).is_err());
    }
    assert!(validate_root(&root(11, SourceMode::ReadOnly, Some(SID_ONE))).is_err());
    assert!(validate_root(&root(11, SourceMode::Writable, None)).is_err());
}
