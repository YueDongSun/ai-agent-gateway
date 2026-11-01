//! Synthetic policy/backend checks. These make no Windows or live-service claim.

use super::super::{NativeSessionId, OperatorAuthorization, PayloadObjectId};
use super::*;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

#[derive(Clone, Debug)]
struct Call {
    kind: &'static str,
    method: String,
    reference: Option<ThreadReference>,
    params: String,
}
#[derive(Clone)]
struct FixtureKey(u64);
struct FixtureState {
    calls: Vec<Call>,
    source_valid: bool,
    write_unknown: bool,
    write_timeout: bool,
    operation: Option<WriteOperation>,
    events: VecDeque<BackendEvent<FixtureKey>>,
    replies: Vec<u64>,
}
impl Default for FixtureState {
    fn default() -> Self {
        Self {
            calls: Vec::new(),
            source_valid: true,
            write_unknown: false,
            write_timeout: false,
            operation: None,
            events: VecDeque::new(),
            replies: Vec::new(),
        }
    }
}
struct FixtureBackend(Arc<Mutex<FixtureState>>);
fn result(value: Value) -> BackendReply {
    BackendReply::Result(RawJson::parse(&value.to_string()).unwrap())
}
impl DispatchBackend for FixtureBackend {
    type ServerRequestKey = FixtureKey;
    fn verify_source(&mut self, _: &SourceGeneration) -> Result<(), BackendError> {
        if self.0.lock().unwrap().source_valid {
            Ok(())
        } else {
            Err(BackendError::SourceMismatch)
        }
    }
    fn metadata(
        &mut self,
        _: &SourceGeneration,
        method: &str,
        params: &RawJson,
    ) -> Result<BackendReply, BackendError> {
        self.0.lock().unwrap().calls.push(Call {
            kind: "metadata",
            method: method.into(),
            reference: None,
            params: params.as_str().into(),
        });
        Ok(result(json!({"data":[]})))
    }
    fn project_history(
        &mut self,
        _: &SourceGeneration,
        reference: &ThreadReference,
        method: &str,
        params: &RawJson,
    ) -> Result<BackendReply, BackendError> {
        self.0.lock().unwrap().calls.push(Call {
            kind: "history",
            method: method.into(),
            reference: Some(reference.clone()),
            params: params.as_str().into(),
        });
        Ok(result(
            json!({"thread":{"id":reference.native_thread_id.as_str()},
            "canonicalId":reference.canonical_session_id.as_str(), "historyRef":reference.payload_object_id.as_str()}),
        ))
    }
    fn write(
        &mut self,
        _: &SourceGeneration,
        reference: &ThreadReference,
        operation: &WriteOperation,
        method: &str,
        params: &RawJson,
    ) -> Result<WriteOutcome, BackendError> {
        let mut state = self.0.lock().unwrap();
        state.calls.push(Call {
            kind: "write",
            method: method.into(),
            reference: Some(reference.clone()),
            params: params.as_str().into(),
        });
        state.operation = Some(operation.clone());
        if state.write_timeout {
            return Err(BackendError::Timeout);
        }
        if state.write_unknown {
            return Ok(WriteOutcome::OutcomeUnknown);
        }
        Ok(WriteOutcome::Running {
            native_turn_id: "turn_active".into(),
            reply: result(json!({"turn":{"id":"turn_active"}})),
        })
    }
    fn inflight(
        &mut self,
        _: &SourceGeneration,
        reference: &ThreadReference,
        _: &WriteOperation,
        method: &str,
        params: &RawJson,
    ) -> Result<BackendReply, BackendError> {
        self.0.lock().unwrap().calls.push(Call {
            kind: "control",
            method: method.into(),
            reference: Some(reference.clone()),
            params: params.as_str().into(),
        });
        Ok(result(json!({})))
    }
    fn reply_to_server(
        &mut self,
        _: &SourceGeneration,
        native_key: &FixtureKey,
        _: ServerReply,
    ) -> Result<(), BackendError> {
        self.0.lock().unwrap().replies.push(native_key.0);
        Ok(())
    }
    fn poll_event(&mut self) -> Result<Option<BackendEvent<FixtureKey>>, BackendError> {
        Ok(self.0.lock().unwrap().events.pop_front())
    }
}

struct Fixture {
    dispatcher: ClientDispatcher<FixtureBackend>,
    registry: SessionRegistry,
    owner: VerifiedOwner,
    reference: ThreadReference,
    source: SourceGeneration,
    state: Arc<Mutex<FixtureState>>,
}
fn fixture(registry: SessionRegistry, suffix: &str, read_only: bool) -> Fixture {
    let owner = VerifiedOwner::from_verified_transport("synthetic_owner").unwrap();
    let reference = registry
        .register_thread(
            &owner,
            &NativeSessionId::new(format!("tree_{suffix}")).unwrap(),
            &NativeThreadId::new(format!("thread_{suffix}")).unwrap(),
            &PayloadObjectId::new(format!("history_{suffix}")).unwrap(),
        )
        .unwrap();
    if read_only {
        let snapshot = registry
            .session_snapshot(&owner, &reference.canonical_session_id)
            .unwrap();
        let idle = owner.attest_native_idle(&snapshot).unwrap();
        let authorization =
            OperatorAuthorization::from_verified_os_principal("synthetic_operator").unwrap();
        registry.set_read_only(&authorization, &idle, true).unwrap();
    }
    let source = SourceGeneration {
        source_key: SourceKey::parse(if suffix == "a" {
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        } else {
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
        })
        .unwrap(),
        canonical_session_id: reference.canonical_session_id.clone(),
        physical_generation: 1,
        mode: if read_only {
            SourceMode::ReadOnly
        } else {
            SourceMode::Writable
        },
    };
    let state = Arc::new(Mutex::new(FixtureState::default()));
    let dispatcher = ClientDispatcher::new(
        registry.clone(),
        owner.clone(),
        source.clone(),
        FixtureBackend(state.clone()),
    )
    .unwrap();
    Fixture {
        dispatcher,
        registry,
        owner,
        reference,
        source,
        state,
    }
}
fn registry() -> SessionRegistry {
    SessionRegistry::from_connection(Connection::open_in_memory().unwrap()).unwrap()
}
fn client(value: &str) -> ClientTag {
    ClientTag::new(value).unwrap()
}
fn frame(raw: &str) -> TransportFrame {
    TransportFrame::parse(raw.as_bytes().to_vec(), 1024 * 1024).unwrap()
}
fn request(method: &str, params: Value) -> TransportFrame {
    frame(&json!({"id":1,"method":method,"params":params}).to_string())
}
fn response(result: DispatchResult) -> Value {
    match result {
        DispatchResult::Response(frame) => serde_json::from_slice(frame.bytes()).unwrap(),
        _ => panic!("response required"),
    }
}
fn busy(fixture: &Fixture) -> bool {
    fixture
        .registry
        .session_snapshot(&fixture.owner, &fixture.reference.canonical_session_id)
        .unwrap()
        .admitted_work
}
fn queue_terminal(fixture: &Fixture, thread: &str, turn: &str, status: &str) {
    let raw = json!({"method":"turn/completed","params":{"threadId":thread,"turn":{"id":turn,"status":status}}}).to_string();
    fixture
        .state
        .lock()
        .unwrap()
        .events
        .push_back(BackendEvent {
            source: fixture.source.clone(),
            kind: BackendEventKind::Notification(NativeFrame::parse(raw).unwrap()),
        });
}
fn start(fixture: &mut Fixture) -> Result<DispatchResult, DispatchError> {
    fixture.dispatcher.dispatch(
        &client("cli"),
        &request(
            "turn/start",
            json!({"threadId":fixture.reference.native_thread_id.as_str(),"input":[]}),
        ),
    )
}

#[test]
fn two_sources_keep_identical_canonical_and_history_references_across_client_tags() {
    let registry = registry();
    let mut first = fixture(registry.clone(), "a", false);
    let mut second = fixture(registry.clone(), "b", true);
    assert_ne!(
        first.reference.canonical_session_id,
        second.reference.canonical_session_id
    );
    for fixture in [&mut first, &mut second] {
        let request = request(
            "thread/read",
            json!({"threadId":fixture.reference.native_thread_id.as_str()}),
        );
        let cli = response(
            fixture
                .dispatcher
                .dispatch(&client("cli"), &request)
                .unwrap(),
        );
        let desktop = response(
            fixture
                .dispatcher
                .dispatch(&client("desktop"), &request)
                .unwrap(),
        );
        assert_eq!(cli, desktop);
        assert_eq!(
            cli["result"]["canonicalId"],
            fixture.reference.canonical_session_id.as_str()
        );
        assert_eq!(
            cli["result"]["historyRef"],
            fixture.reference.payload_object_id.as_str()
        );
        assert_eq!(
            fixture
                .registry
                .list_threads(&fixture.owner, &fixture.reference.canonical_session_id)
                .unwrap(),
            vec![fixture.reference.clone()]
        );
    }
}

#[test]
fn read_only_resume_alternate_reads_and_tool_denials_never_call_a_mutating_backend() {
    let mut fixture = fixture(registry(), "a", true);
    for method in [
        "thread/resume",
        "thread/read",
        "thread/items/read",
        "thread/timeline/list",
    ] {
        let request = request(method, json!({"threadId":"thread_a"}));
        response(
            fixture
                .dispatcher
                .dispatch(&client("desktop"), &request)
                .unwrap(),
        );
    }
    for method in [
        "turn/start",
        "thread/fork",
        "thread/revert",
        "thread/name/set",
        "thread/queue/start",
        "fs/writeFile",
        "process/spawn",
        "command/exec",
        "config/value/write",
        "thread/start",
        "future/unknown",
    ] {
        assert!(
            fixture
                .dispatcher
                .dispatch(
                    &client("cli"),
                    &request(method, json!({"threadId":"thread_a"}))
                )
                .is_err(),
            "{method}"
        );
    }
    assert!(fixture
        .dispatcher
        .dispatch(
            &client("desktop"),
            &frame(r#"{"id":"unassociated_tool","result":{}}"#)
        )
        .is_err());
    let state = fixture.state.lock().unwrap();
    assert_eq!(state.calls.len(), 4);
    assert!(state.calls.iter().all(|call| call.kind == "history"));
    assert!(state.replies.is_empty());
    assert!(!busy(&fixture));
}

#[test]
fn duplicate_routes_and_owner_overrides_are_denied_while_raw_numbers_survive() {
    let mut fixture = fixture(registry(), "a", false);
    for raw in [
        r#"{"id":1,"method":"turn/start","params":{"threadId":"thread_a","threadId":"thread_b"}}"#,
        r#"{"id":1,"method":"turn/start","params":{"threadId":"thread_a","\u0074hreadId":"thread_b"}}"#,
        r#"{"id":1,"method":"thread/resume","params":{"threadId":"thread_a","history":[]}}"#,
        r#"{"id":1,"method":"thread/resume","params":{"threadId":"thread_a","path":""}}"#,
        r#"{"id":1,"method":"turn/start","params":{"threadId":"thread_a","cwd":"C:/synthetic"}}"#,
    ] {
        assert!(fixture
            .dispatcher
            .dispatch(&client("cli"), &frame(raw))
            .is_err());
    }
    assert!(fixture.state.lock().unwrap().calls.is_empty());
    for tag in ["cli", "desktop"] {
        response(
            fixture
                .dispatcher
                .dispatch(
                    &client(tag),
                    &request("initialize", json!({"clientInfo":{"name":"fixture"}})),
                )
                .unwrap(),
        );
        assert!(matches!(
            fixture
                .dispatcher
                .dispatch(
                    &client(tag),
                    &frame(r#"{"method":"initialized","params":{}}"#)
                )
                .unwrap(),
            DispatchResult::NotificationAccepted
        ));
    }
    let raw = r#"{"id":9,"method":"turn/start","params": {"threadId":"thread_a","input":[],"opaque":900719925474099312345,"decimal":1.234567890123456789}}"#;
    response(
        fixture
            .dispatcher
            .dispatch(&client("cli"), &frame(raw))
            .unwrap(),
    );
    let state = fixture.state.lock().unwrap();
    assert_eq!(
        state
            .calls
            .iter()
            .filter(|call| call.kind == "metadata")
            .count(),
        2
    );
    let call = state
        .calls
        .iter()
        .find(|call| call.kind == "write")
        .unwrap();
    assert_eq!(
        call.params,
        r#"{"threadId":"thread_a","input":[],"opaque":900719925474099312345,"decimal":1.234567890123456789}"#
    );
}

#[test]
fn uncertainty_retains_admission_and_only_exact_source_reconciliation_can_bind_a_turn() {
    let mut fixture = fixture(registry(), "a", false);
    fixture.state.lock().unwrap().write_timeout = true;
    assert!(matches!(
        start(&mut fixture),
        Err(DispatchError::Backend(BackendError::Timeout))
    ));
    assert!(busy(&fixture));
    assert!(matches!(
        start(&mut fixture),
        Err(DispatchError::Policy(BrokerError::Busy))
    ));
    let operation = fixture.state.lock().unwrap().operation.clone().unwrap();
    let mut stale_source = fixture.source.clone();
    stale_source.physical_generation += 1;
    fixture
        .state
        .lock()
        .unwrap()
        .events
        .push_back(BackendEvent {
            source: stale_source,
            kind: BackendEventKind::WriteReconciled {
                operation: operation.clone(),
                native_thread_id: fixture.reference.native_thread_id.clone(),
                outcome: WriteOutcome::Running {
                    native_turn_id: "turn_late".into(),
                    reply: result(json!({})),
                },
            },
        });
    assert!(matches!(
        fixture.dispatcher.poll(),
        Err(DispatchError::SourceMismatch)
    ));
    queue_terminal(&fixture, "thread_a", "turn_late", "completed");
    fixture.dispatcher.poll().unwrap();
    assert!(busy(&fixture));
    fixture
        .state
        .lock()
        .unwrap()
        .events
        .push_back(BackendEvent {
            source: fixture.source.clone(),
            kind: BackendEventKind::WriteReconciled {
                operation,
                native_thread_id: fixture.reference.native_thread_id.clone(),
                outcome: WriteOutcome::Running {
                    native_turn_id: "turn_late".into(),
                    reply: result(json!({})),
                },
            },
        });
    fixture.dispatcher.poll().unwrap();
    queue_terminal(&fixture, "thread_a", "turn_late", "completed");
    fixture.dispatcher.poll().unwrap();
    assert!(!busy(&fixture));
    fixture.state.lock().unwrap().source_valid = false;
    assert!(fixture
        .dispatcher
        .dispatch(&client("cli"), &request("model/list", json!({})))
        .is_err());
}

#[test]
fn terminal_events_release_only_the_exact_active_native_thread_and_turn() {
    let mut fixture = fixture(registry(), "a", false);
    response(start(&mut fixture).unwrap());
    for (thread, turn, status) in [
        ("thread_b", "turn_active", "completed"),
        ("thread_a", "turn_old", "completed"),
        ("thread_a", "turn_active", "inProgress"),
    ] {
        queue_terminal(&fixture, thread, turn, status);
        fixture.dispatcher.poll().unwrap();
        assert!(busy(&fixture));
    }
    queue_terminal(&fixture, "thread_a", "turn_active", "interrupted");
    fixture.dispatcher.poll().unwrap();
    assert!(!busy(&fixture));
}

#[test]
fn server_replies_and_turn_controls_require_the_active_client_thread_and_turn() {
    let mut fixture = fixture(registry(), "a", false);
    response(start(&mut fixture).unwrap());
    let native = NativeFrame::parse(r#"{"id":"native_tool_1","method":"item/tool/call","params":{"threadId":"thread_a","turnId":"turn_active","itemId":"synthetic_item"}}"#.into()).unwrap();
    fixture
        .state
        .lock()
        .unwrap()
        .events
        .push_back(BackendEvent {
            source: fixture.source.clone(),
            kind: BackendEventKind::ServerRequest {
                native_key: FixtureKey(42),
                frame: native,
            },
        });
    let deliveries = fixture.dispatcher.poll().unwrap();
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0].client, client("cli"));
    let reply = frame(r#"{"id":"native_tool_1","result":{"value":900719925474099312345}}"#);
    assert!(fixture
        .dispatcher
        .dispatch(&client("desktop"), &reply)
        .is_err());
    for (method, params) in [
        (
            "turn/interrupt",
            json!({"threadId":"thread_a","turnId":"turn_old"}),
        ),
        (
            "turn/steer",
            json!({"threadId":"thread_b","expectedTurnId":"turn_active","input":[]}),
        ),
    ] {
        assert!(fixture
            .dispatcher
            .dispatch(&client("cli"), &request(method, params))
            .is_err());
    }
    response(
        fixture
            .dispatcher
            .dispatch(
                &client("cli"),
                &request(
                    "turn/steer",
                    json!({"threadId":"thread_a","expectedTurnId":"turn_active","input":[]}),
                ),
            )
            .unwrap(),
    );
    assert!(matches!(
        fixture.dispatcher.dispatch(&client("cli"), &reply).unwrap(),
        DispatchResult::NotificationAccepted
    ));
    assert!(fixture.dispatcher.dispatch(&client("cli"), &reply).is_err());
    let state = fixture.state.lock().unwrap();
    assert_eq!(state.replies, vec![42]);
    assert_eq!(
        state
            .calls
            .iter()
            .filter(|call| call.kind == "control")
            .count(),
        1
    );
    drop(state);
    assert!(busy(&fixture));
    queue_terminal(&fixture, "thread_a", "turn_active", "completed");
    fixture.dispatcher.poll().unwrap();
    assert!(!busy(&fixture));
}
