use super::*;
use rusqlite::Connection;
use serde_json::json;
use std::sync::{Arc, Barrier};

fn owner(identity: &str) -> VerifiedOwner {
    VerifiedOwner::from_verified_transport(identity).unwrap()
}

fn operator() -> OperatorAuthorization {
    OperatorAuthorization::from_verified_os_principal("fixture_os_operator").unwrap()
}

fn registry() -> SessionRegistry {
    SessionRegistry::from_connection(Connection::open_in_memory().unwrap()).unwrap()
}

fn register(
    registry: &SessionRegistry,
    owner: &VerifiedOwner,
    native_session_id: &str,
    native_thread_id: &str,
    payload_object_id: &str,
) -> ThreadReference {
    registry
        .register_thread(
            owner,
            &NativeSessionId::new(native_session_id).unwrap(),
            &NativeThreadId::new(native_thread_id).unwrap(),
            &PayloadObjectId::new(payload_object_id).unwrap(),
        )
        .unwrap()
}

fn mark_read_only(
    registry: &SessionRegistry,
    owner: &VerifiedOwner,
    reference: &ThreadReference,
    read_only: bool,
) -> SessionSnapshot {
    let snapshot = registry
        .session_snapshot(owner, &reference.canonical_session_id)
        .unwrap();
    let attestation = owner.attest_native_idle(&snapshot).unwrap();
    registry
        .set_read_only(&operator(), &attestation, read_only)
        .unwrap()
}

fn request(
    registry: &SessionRegistry,
    owner: &VerifiedOwner,
    method: &str,
    params: serde_json::Value,
) -> Result<BrokerDecision, BrokerError> {
    registry.assess_client_request(
        owner,
        ClientRequest {
            method,
            params: &params,
        },
    )
}

#[test]
fn exact_native_reference_is_idempotent_without_a_second_history() {
    let registry = registry();
    let owner = owner("owner_a");
    let first = register(&registry, &owner, "tree_a", "thread_a", "native_object_a");
    let repeated = register(&registry, &owner, "tree_a", "thread_a", "native_object_a");
    assert_eq!(first, repeated);
    assert_eq!(
        registry
            .list_threads(&owner, &first.canonical_session_id)
            .unwrap(),
        vec![first]
    );
}

#[test]
fn canonical_identity_distinguishes_tree_aliases_forks_and_owners() {
    let registry = registry();
    let owner_a = owner("owner_a");
    let root = register(&registry, &owner_a, "tree_a", "root", "object_root");
    let child = register(&registry, &owner_a, "tree_a", "child", "object_child");
    let fork = register(&registry, &owner_a, "tree_b", "fork", "object_fork");
    let other_owner = register(
        &registry,
        &owner("owner_b"),
        "tree_a",
        "root",
        "object_root",
    );
    assert_eq!(root.canonical_session_id, child.canonical_session_id);
    assert_ne!(root.canonical_session_id, fork.canonical_session_id);
    assert_ne!(root.canonical_session_id, other_owner.canonical_session_id);
    assert_eq!(
        registry
            .list_threads(&owner_a, &root.canonical_session_id)
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn conflicting_alias_or_payload_never_replaces_the_authoritative_reference() {
    let registry = registry();
    let owner = owner("owner_a");
    let original = register(&registry, &owner, "tree_a", "thread_a", "object_a");
    let payload_conflict = registry.register_thread(
        &owner,
        &original.native_session_id,
        &original.native_thread_id,
        &PayloadObjectId::new("object_b").unwrap(),
    );
    assert!(matches!(
        payload_conflict,
        Err(BrokerError::PayloadConflict)
    ));
    let alias_conflict = registry.register_thread(
        &owner,
        &NativeSessionId::new("tree_b").unwrap(),
        &original.native_thread_id,
        &original.payload_object_id,
    );
    assert!(matches!(alias_conflict, Err(BrokerError::AliasConflict)));
    assert_eq!(
        registry
            .resolve_thread(&owner, &original.native_thread_id)
            .unwrap(),
        original
    );
}

#[test]
fn opaque_identifiers_remain_case_sensitive_and_reject_invalid_values() {
    let registry = registry();
    let owner = owner("owner_a");
    let upper = register(&registry, &owner, "Tree_A", "Thread_A", "Object_A");
    let lower = register(&registry, &owner, "tree_a", "thread_a", "object_a");
    assert_ne!(upper.canonical_session_id, lower.canonical_session_id);
    assert!(NativeThreadId::new("").is_err());
    assert!(NativeSessionId::new("tree\0suffix").is_err());
    assert!(PayloadObjectId::new("object\nother").is_err());
    assert!(VerifiedOwner::from_verified_transport("").is_err());
    assert!(OperatorAuthorization::from_verified_os_principal("").is_err());
}

#[test]
fn read_only_covers_all_aliases_and_resume_becomes_a_read_projection() {
    let registry = registry();
    let owner = owner("owner_a");
    let root = register(&registry, &owner, "tree_a", "root", "object_root");
    let child = register(&registry, &owner, "tree_a", "child", "object_child");
    let BrokerDecision::Write(permit) =
        request(&registry, &owner, "turn/start", json!({"threadId": "root"})).unwrap()
    else {
        panic!("expected write permit")
    };
    registry.finish_write(&permit).unwrap();
    mark_read_only(&registry, &owner, &root, true);
    for thread_id in ["root", "child"] {
        for method in [
            "thread/read",
            "thread/turns/list",
            "thread/items/list",
            "thread/attachment/list",
        ] {
            assert!(
                matches!(
                    request(&registry, &owner, method, json!({"threadId": thread_id})),
                    Ok(BrokerDecision::Read { .. })
                ),
                "{method}"
            );
        }
        assert!(matches!(
            request(
                &registry,
                &owner,
                "thread/resume",
                json!({"threadId": thread_id})
            ),
            Ok(BrokerDecision::ReadProjection { .. })
        ));
        assert!(matches!(
            request(
                &registry,
                &owner,
                "turn/start",
                json!({"threadId": thread_id, "input": []})
            ),
            Err(BrokerError::ReadOnly)
        ));
    }
    assert!(matches!(
        registry.authorize_tool_reply(&owner, &child.native_thread_id, &permit),
        Err(BrokerError::ReadOnly)
    ));
    assert!(
        !registry
            .session_snapshot(&owner, &root.canonical_session_id)
            .unwrap()
            .admitted_work
    );
}

#[test]
fn read_only_rejects_mutations_forks_acknowledgements_and_client_operator_claims() {
    let registry = registry();
    let owner = owner("owner_a");
    let reference = register(&registry, &owner, "tree_a", "thread_a", "object_a");
    mark_read_only(&registry, &owner, &reference, true);
    for method in [
        "turn/start",
        "turn/steer",
        "turn/interrupt",
        "turn/settings/update",
        "thread/fork",
        "thread/delete",
        "thread/archive",
        "thread/unarchive",
        "thread/name/set",
        "thread/metadata/update",
        "thread/settings/update",
        "thread/readState/update",
        "thread/attachment/add",
        "thread/attachment/remove",
        "thread/queue/add",
        "thread/queue/update",
        "thread/queue/delete",
        "thread/queue/start",
        "thread/queue/reorder",
        "thread/goal/set",
        "thread/goal/clear",
        "thread/revert",
        "thread/inject_items",
        "thread/compact/start",
        "review/start",
        "thread/realtime/start",
    ] {
        assert!(
            request(
                &registry,
                &owner,
                method,
                json!({"threadId": "thread_a", "isOperator": true, "readOnly": false})
            )
            .is_err(),
            "{method}"
        );
    }
    assert!(request(
        &registry,
        &owner,
        "session/readOnly/set",
        json!({"threadId": "thread_a", "isOperator": true, "readOnly": false})
    )
    .is_err());
    assert!(
        registry
            .session_snapshot(&owner, &reference.canonical_session_id)
            .unwrap()
            .read_only
    );
    assert!(!mark_read_only(&registry, &owner, &reference, false).read_only);
}

#[test]
fn generic_host_mutation_privileged_configuration_and_unknown_methods_are_always_denied() {
    let registry = registry();
    let owner = owner("owner_a");
    register(&registry, &owner, "tree_a", "thread_a", "object_a");
    for method in [
        "command/exec",
        "command/exec/write",
        "process/spawn",
        "process/writeStdin",
        "fs/writeFile",
        "fs/remove",
        "fs/copy",
        "fs/readFile",
        "thread/shellCommand",
        "config/value/write",
        "config/batchWrite",
        "externalAgentConfig/import",
        "codex_mirror_session_reset",
        "thread/approveGuardianDeniedAction",
        "future/write",
    ] {
        assert!(
            matches!(
                request(&registry, &owner, method, json!({"threadId": "thread_a"})),
                Err(BrokerError::ForbiddenMethod)
            ),
            "{method}"
        );
    }
    assert!(matches!(
        request(&registry, &owner, "thread/list", json!({})),
        Ok(BrokerDecision::Read { reference: None })
    ));
}

#[test]
fn native_path_history_and_configuration_overrides_cannot_escape_alias_resolution() {
    let registry = registry();
    let owner = owner("owner_a");
    register(&registry, &owner, "tree_a", "thread_a", "object_a");
    for params in [
        json!({"threadId": "thread_a", "path": std::env::temp_dir().join("different_rollout")}),
        json!({"threadId": "thread_a", "history": []}),
        json!({"threadId": "thread_a", "config": {"sandbox_mode": "danger-full-access"}}),
    ] {
        assert!(matches!(
            request(&registry, &owner, "thread/resume", params),
            Err(BrokerError::ForbiddenOverride)
        ));
    }
    assert!(matches!(
        request(
            &registry,
            &owner,
            "thread/read",
            json!({"threadId": "unknown"})
        ),
        Err(BrokerError::UnknownThread)
    ));
    assert!(request(&registry, &owner, "thread/read", json!({"threadId": 123})).is_err());
}

#[test]
fn lock_requires_idle_and_rejects_stale_attestations_and_write_completions() {
    let registry = registry();
    let owner = owner("owner_a");
    let reference = register(&registry, &owner, "tree_a", "thread_a", "object_a");
    let before = registry
        .session_snapshot(&owner, &reference.canonical_session_id)
        .unwrap();
    let stale_idle = owner.attest_native_idle(&before).unwrap();
    let BrokerDecision::Write(permit) = request(
        &registry,
        &owner,
        "turn/start",
        json!({"threadId": "thread_a", "input": []}),
    )
    .unwrap() else {
        panic!("mutation must issue a permit")
    };
    registry.validate_write_permit(&permit).unwrap();
    let busy = registry
        .session_snapshot(&owner, &reference.canonical_session_id)
        .unwrap();
    let busy_attestation = owner.attest_native_idle(&busy).unwrap();
    assert!(matches!(
        registry.set_read_only(&operator(), &busy_attestation, true),
        Err(BrokerError::Busy)
    ));
    registry.finish_write(&permit).unwrap();
    assert!(matches!(
        registry.set_read_only(&operator(), &stale_idle, true),
        Err(BrokerError::StaleFence)
    ));
    mark_read_only(&registry, &owner, &reference, true);
    assert!(matches!(
        registry.validate_write_permit(&permit),
        Err(BrokerError::StaleFence)
    ));
    assert!(matches!(
        registry.finish_write(&permit),
        Err(BrokerError::StaleFence)
    ));
}

#[test]
fn owner_attestation_cannot_be_rebound_to_another_owner() {
    let registry = registry();
    let owner_a = owner("owner_a");
    let reference = register(&registry, &owner_a, "tree_a", "thread_a", "object_a");
    let snapshot = registry
        .session_snapshot(&owner_a, &reference.canonical_session_id)
        .unwrap();
    assert!(matches!(
        owner("owner_b").attest_native_idle(&snapshot),
        Err(BrokerError::OwnerMismatch)
    ));
    assert!(registry
        .session_snapshot(&owner("owner_b"), &reference.canonical_session_id)
        .is_err());
}

#[test]
fn tool_reply_requires_the_existing_turn_admission_and_its_native_thread() {
    let registry = registry();
    let owner = owner("owner_a");
    let root = register(&registry, &owner, "tree_a", "root", "object_root");
    let other = register(&registry, &owner, "tree_b", "other", "object_other");
    let BrokerDecision::Write(permit) =
        request(&registry, &owner, "turn/start", json!({"threadId": "root"})).unwrap()
    else {
        panic!("expected write permit")
    };
    registry
        .authorize_tool_reply(&owner, &root.native_thread_id, &permit)
        .unwrap();
    assert!(registry
        .authorize_tool_reply(&owner, &other.native_thread_id, &permit)
        .is_err());
    registry.finish_write(&permit).unwrap();
    assert!(matches!(
        registry.authorize_tool_reply(&owner, &root.native_thread_id, &permit),
        Err(BrokerError::StaleFence)
    ));
}

#[test]
fn inflight_steer_and_interrupt_retain_the_existing_turn_admission() {
    let registry = registry();
    let owner = owner("owner_a");
    let reference = register(&registry, &owner, "tree_a", "root", "object_root");
    let BrokerDecision::Write(permit) =
        request(&registry, &owner, "turn/start", json!({"threadId": "root"})).unwrap()
    else {
        panic!("expected write permit")
    };
    let before = registry
        .session_snapshot(&owner, &reference.canonical_session_id)
        .unwrap();
    for method in ["turn/steer", "turn/interrupt"] {
        let params = json!({"threadId": "root", "turnId": "active_turn"});
        registry
            .authorize_inflight_request(
                &owner,
                &reference.native_thread_id,
                ClientRequest {
                    method,
                    params: &params,
                },
                &permit,
            )
            .unwrap();
        registry.validate_write_permit(&permit).unwrap();
        assert_eq!(
            registry
                .session_snapshot(&owner, &reference.canonical_session_id)
                .unwrap(),
            before
        );
    }
    assert!(matches!(
        request(&registry, &owner, "turn/start", json!({"threadId": "root"})),
        Err(BrokerError::Busy)
    ));
    registry.finish_write(&permit).unwrap();
}

#[test]
fn inflight_controls_reject_other_methods_and_share_override_validation() {
    let registry = registry();
    let owner = owner("owner_a");
    let reference = register(&registry, &owner, "tree_a", "root", "object_root");
    let BrokerDecision::Write(permit) =
        request(&registry, &owner, "turn/start", json!({"threadId": "root"})).unwrap()
    else {
        panic!("expected write permit")
    };
    let params = json!({"threadId": "root"});
    for method in [
        "turn/start",
        "turn/settings/update",
        "thread/read",
        "thread/delete",
        "thread/shellCommand",
        "process/spawn",
        "future/control",
    ] {
        assert!(
            matches!(
                registry.authorize_inflight_request(
                    &owner,
                    &reference.native_thread_id,
                    ClientRequest {
                        method,
                        params: &params
                    },
                    &permit
                ),
                Err(BrokerError::ForbiddenMethod)
            ),
            "{method}"
        );
    }
    for method in ["turn/steer", "turn/interrupt"] {
        for params in [
            json!({"threadId": "root", "path": std::env::temp_dir().join("different_rollout")}),
            json!({"threadId": "root", "history": []}),
            json!({"threadId": "root", "config": {"sandbox_mode": "danger-full-access"}}),
            json!({"threadId": "root", "sandboxPolicy": {"type": "dangerFullAccess"}}),
        ] {
            assert!(matches!(
                registry.authorize_inflight_request(
                    &owner,
                    &reference.native_thread_id,
                    ClientRequest {
                        method,
                        params: &params
                    },
                    &permit
                ),
                Err(BrokerError::ForbiddenOverride)
            ));
        }
    }
    registry.finish_write(&permit).unwrap();
}

#[test]
fn inflight_controls_require_matching_trusted_owner_thread_and_request_target() {
    let registry = registry();
    let owner = owner("owner_a");
    let reference = register(&registry, &owner, "tree_a", "root", "object_root");
    let sibling = register(&registry, &owner, "tree_a", "child", "object_child");
    let other_owner = VerifiedOwner::from_verified_transport("owner_b").unwrap();
    register(
        &registry,
        &other_owner,
        "tree_a",
        "root",
        "other_object_root",
    );
    let BrokerDecision::Write(permit) =
        request(&registry, &owner, "turn/start", json!({"threadId": "root"})).unwrap()
    else {
        panic!("expected write permit")
    };
    let root_params = json!({"threadId": "root"});
    assert!(registry
        .authorize_inflight_request(
            &other_owner,
            &reference.native_thread_id,
            ClientRequest {
                method: "turn/steer",
                params: &root_params
            },
            &permit
        )
        .is_err());
    let child_params = json!({"threadId": "child"});
    assert!(registry
        .authorize_inflight_request(
            &owner,
            &sibling.native_thread_id,
            ClientRequest {
                method: "turn/steer",
                params: &child_params
            },
            &permit
        )
        .is_err());
    assert!(registry
        .authorize_inflight_request(
            &owner,
            &reference.native_thread_id,
            ClientRequest {
                method: "turn/steer",
                params: &child_params
            },
            &permit
        )
        .is_err());
    let missing_target = json!({"isOperator": true});
    assert!(registry
        .authorize_inflight_request(
            &owner,
            &reference.native_thread_id,
            ClientRequest {
                method: "turn/interrupt",
                params: &missing_target
            },
            &permit
        )
        .is_err());
    registry.finish_write(&permit).unwrap();
}

#[test]
fn inflight_controls_reject_read_only_and_completed_or_stale_permits() {
    let registry = registry();
    let owner = owner("owner_a");
    let reference = register(&registry, &owner, "tree_a", "root", "object_root");
    let BrokerDecision::Write(permit) =
        request(&registry, &owner, "turn/start", json!({"threadId": "root"})).unwrap()
    else {
        panic!("expected write permit")
    };
    let params = json!({"threadId": "root"});
    registry.finish_write(&permit).unwrap();
    for method in ["turn/steer", "turn/interrupt"] {
        assert!(matches!(
            registry.authorize_inflight_request(
                &owner,
                &reference.native_thread_id,
                ClientRequest {
                    method,
                    params: &params
                },
                &permit
            ),
            Err(BrokerError::StaleFence)
        ));
    }
    mark_read_only(&registry, &owner, &reference, true);
    for method in ["turn/steer", "turn/interrupt"] {
        assert!(matches!(
            registry.authorize_inflight_request(
                &owner,
                &reference.native_thread_id,
                ClientRequest {
                    method,
                    params: &params
                },
                &permit
            ),
            Err(BrokerError::ReadOnly)
        ));
    }
    mark_read_only(&registry, &owner, &reference, false);
    let BrokerDecision::Write(current_permit) =
        request(&registry, &owner, "turn/start", json!({"threadId": "root"})).unwrap()
    else {
        panic!("expected current permit")
    };
    assert!(matches!(
        registry.authorize_inflight_request(
            &owner,
            &reference.native_thread_id,
            ClientRequest {
                method: "turn/interrupt",
                params: &params
            },
            &permit
        ),
        Err(BrokerError::StaleFence)
    ));
    registry.validate_write_permit(&current_permit).unwrap();
    registry.finish_write(&current_permit).unwrap();
}

#[test]
fn sqlite_reopen_preserves_reference_policy_and_unresolved_admission() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("broker.sqlite");
    let owner = owner("owner_a");
    let first = SessionRegistry::from_connection(Connection::open(&path).unwrap()).unwrap();
    let reference = register(&first, &owner, "tree_a", "thread_a", "object_a");
    let BrokerDecision::Write(permit) = request(
        &first,
        &owner,
        "turn/start",
        json!({"threadId": "thread_a"}),
    )
    .unwrap() else {
        panic!("expected write permit")
    };
    drop(first);
    let reopened = SessionRegistry::from_connection(Connection::open(&path).unwrap()).unwrap();
    assert_eq!(
        reopened
            .resolve_thread(&owner, &reference.native_thread_id)
            .unwrap(),
        reference
    );
    assert!(
        reopened
            .session_snapshot(&owner, &reference.canonical_session_id)
            .unwrap()
            .admitted_work
    );
    reopened.validate_write_permit(&permit).unwrap();
    reopened.finish_write(&permit).unwrap();
    let locked = mark_read_only(&reopened, &owner, &reference, true);
    drop(reopened);
    let reopened = SessionRegistry::from_connection(Connection::open(&path).unwrap()).unwrap();
    assert_eq!(
        reopened
            .session_snapshot(&owner, &reference.canonical_session_id)
            .unwrap(),
        locked
    );
    assert!(matches!(
        reopened.validate_write_permit(&permit),
        Err(BrokerError::StaleFence)
    ));
}

#[test]
fn separate_sqlite_connections_serialize_lock_with_mutation_admission() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("broker.sqlite");
    let owner = owner("owner_a");
    let writer = SessionRegistry::from_connection(Connection::open(&path).unwrap()).unwrap();
    let reference = register(&writer, &owner, "tree_a", "thread_a", "object_a");
    let locker = SessionRegistry::from_connection(Connection::open(&path).unwrap()).unwrap();
    let idle = owner
        .attest_native_idle(
            &writer
                .session_snapshot(&owner, &reference.canonical_session_id)
                .unwrap(),
        )
        .unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let writer_barrier = barrier.clone();
    let write_owner = owner.clone();
    let write = std::thread::spawn(move || {
        writer_barrier.wait();
        request(
            &writer,
            &write_owner,
            "turn/start",
            json!({"threadId": "thread_a"}),
        )
    });
    barrier.wait();
    let locked = locker.set_read_only(&operator(), &idle, true);
    let admitted = write.join().unwrap();
    match (locked, admitted) {
        (Ok(snapshot), Err(BrokerError::ReadOnly)) => assert!(snapshot.read_only),
        (Err(BrokerError::Busy), Ok(BrokerDecision::Write(permit))) => {
            locker.validate_write_permit(&permit).unwrap();
            locker.finish_write(&permit).unwrap();
        }
        other => panic!("lock/write race must have one winner: {other:?}"),
    }
}
