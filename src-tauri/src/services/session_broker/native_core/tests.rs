use super::multiplex::Multiplexer;
use super::*;
use std::ffi::OsString;

fn limits() -> NativeCoreLimits {
    NativeCoreLimits {
        frame_bytes: 2048,
        outbound_bytes: 8192,
        outbound_frames: 8,
        response_bytes: 8192,
        pending_requests: 8,
        event_bytes: 8192,
        queued_events: 8,
        pending_server_requests: 8,
    }
}

fn reply(ticket: &RequestTicket, body: &str) -> NativeFrame {
    let id = match &ticket.id {
        RpcId::String(value) => serde_json::to_string(value).unwrap(),
        RpcId::Number(value) => value.to_string(),
    };
    NativeFrame::parse(format!("{{\"id\":{id},\"result\":{body}}}")).unwrap()
}

fn spec() -> NativeCoreSpec {
    NativeCoreSpec {
        binding: NativeBinding {
            canonical_session_id: CanonicalSessionId("fixture_source".into()),
            physical_generation: 7,
            role: CoreRole::ReadOnly,
        },
        executable: PathBuf::from(r"C:\fixture\codex.exe"),
        codex_home: PathBuf::from(r"C:\fixture\native_home"),
        sqlite_home: PathBuf::from(r"C:\fixture\metadata"),
        cwd: PathBuf::from(r"C:\fixture\workspace"),
        environment: vec![(OsString::from("SystemRoot"), OsString::from(r"C:\Windows"))],
        config_overrides: vec!["features.code_mode_host=false".into()],
        limits: limits(),
    }
}

#[test]
fn raw_params_keep_opaque_strings_and_large_numbers_while_becoming_one_line() {
    let input = "{\n \"encrypted_content\": \"opaque\\u0000\\n\", \"future\":123456789012345678901234567890 }";
    let raw = RawJson::parse(input).unwrap();
    assert_eq!(
        raw.as_str(),
        r#"{"encrypted_content":"opaque\u0000\n","future":123456789012345678901234567890}"#
    );
    assert!(RawJson::parse("{\"truncated\":").is_err());
}

#[test]
fn fragmented_jsonl_preserves_native_frame_bytes_and_crlf_boundaries() {
    let raw = r#"{"id":"one","result":{"encrypted_content":"opaque\u0000","future":123456789012345678901234567890}}"#;
    let mut decoder = FrameDecoder::new(2048).unwrap();
    assert!(decoder.feed(&raw.as_bytes()[..17]).unwrap().is_empty());
    let mut rest = raw.as_bytes()[17..].to_vec();
    rest.extend_from_slice(b"\r\n");
    let frames = decoder.feed(&rest).unwrap();
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].raw_json(), raw);
    assert_eq!(
        frames[0].kind(),
        &FrameKind::Response {
            id: RpcId::String("one".into()),
            is_error: false
        }
    );
    assert!(decoder.finish().unwrap().is_empty());
}

#[test]
fn decoder_rejects_oversize_invalid_utf8_and_incomplete_eof() {
    let mut oversized = FrameDecoder::new(8).unwrap();
    assert!(matches!(
        oversized.feed(b"123456789"),
        Err(NativeCoreError::FrameTooLarge)
    ));
    let mut utf8 = FrameDecoder::new(1024).unwrap();
    assert!(matches!(
        utf8.feed(&[0xff, b'\n']),
        Err(NativeCoreError::InvalidUtf8)
    ));
    let mut incomplete = FrameDecoder::new(1024).unwrap();
    incomplete.feed(b"{\"id\":1,").unwrap();
    assert!(matches!(
        incomplete.finish(),
        Err(NativeCoreError::IncompleteFrame)
    ));
}

#[test]
fn duplicate_routing_fields_and_ambiguous_rpc_envelopes_are_rejected() {
    for raw in [
        r#"{"id":1,"id":2,"result":{}}"#,
        r#"{"id":1,"result":{},"error":{}}"#,
        r#"{"id":1,"method":"tool/call","result":{}}"#,
        r#"{"result":{}}"#,
    ] {
        assert!(
            matches!(
                NativeFrame::parse(raw.into()),
                Err(NativeCoreError::MalformedFrame)
            ),
            "{raw}"
        );
    }
}

#[test]
fn terminal_turn_is_correlated_metadata_without_rewriting_the_payload() {
    let raw = r#"{"method":"turn/completed","params":{"threadId":"thread_a","turn":{"id":"turn_a","status":"interrupted","future":{"opaque":"retained"}}}}"#;
    let frame = NativeFrame::parse(raw.into()).unwrap();
    let terminal = frame.terminal_turn().unwrap();
    assert_eq!(terminal.native_thread_id, "thread_a");
    assert_eq!(terminal.native_turn_id, "turn_a");
    assert_eq!(terminal.status, "interrupted");
    assert_eq!(frame.raw_json(), raw);
}

#[test]
fn outbound_ids_are_broker_assigned_and_requests_leave_in_fifo_order() {
    let mut mux = Multiplexer::new(limits()).unwrap();
    let params = RawJson::parse(r#"{"id":"caller_value","opaque":"unchanged"}"#).unwrap();
    let first = mux.request("thread/read", Some(&params)).unwrap();
    let second = mux.request("model/list", None).unwrap();
    assert_ne!(first.id, second.id);
    let a = mux.take_outbound().unwrap();
    let b = mux.take_outbound().unwrap();
    let a_json: serde_json::Value = serde_json::from_slice(&a.bytes).unwrap();
    let b_json: serde_json::Value = serde_json::from_slice(&b.bytes).unwrap();
    assert_eq!(a_json["method"], "thread/read");
    assert_eq!(b_json["method"], "model/list");
    assert_ne!(a_json["id"], a_json["params"]["id"]);
    assert_eq!(a_json["params"]["opaque"], "unchanged");
    assert!(mux.take_outbound().is_none());
    mux.written(a.sequence).unwrap();
    mux.written(b.sequence).unwrap();
}

#[test]
fn out_of_order_responses_are_delivered_only_to_the_matching_ticket() {
    let mut mux = Multiplexer::new(limits()).unwrap();
    let first = mux.request("thread/read", None).unwrap();
    let second = mux.request("model/list", None).unwrap();
    // Native replies can race the writer's completion report, but only after
    // the frames were handed to that writer.
    mux.take_outbound().unwrap();
    mux.take_outbound().unwrap();
    mux.receive(reply(&second, r#"{"opaque":"second"}"#))
        .unwrap();
    assert!(mux.take_response(&first).unwrap().is_none());
    assert!(mux
        .take_response(&second)
        .unwrap()
        .unwrap()
        .frame
        .raw_json()
        .contains("second"));
    mux.receive(reply(&first, r#"{"opaque":"first"}"#)).unwrap();
    assert!(mux
        .take_response(&first)
        .unwrap()
        .unwrap()
        .frame
        .raw_json()
        .contains("first"));
}

#[test]
fn timeout_retains_unknown_outcome_and_accepts_a_late_response_without_retry() {
    let mut mux = Multiplexer::new(limits()).unwrap();
    let ticket = mux.request("turn/start", None).unwrap();
    let frame = mux.take_outbound().unwrap();
    mux.written(frame.sequence).unwrap();
    let unknown = mux.timeout(&ticket).unwrap();
    assert_eq!(unknown.unknown_reason, Some(UnknownReason::Deadline));
    assert!(matches!(
        mux.progress(&ticket).unwrap(),
        RequestProgress::OutcomeUnknown(_)
    ));
    assert!(mux.take_outbound().is_none());
    assert_eq!(mux.pending_outcomes().len(), 1);
    mux.receive(reply(&ticket, r#"{"turn":{"id":"turn_a"}}"#))
        .unwrap();
    assert!(mux.take_response(&ticket).unwrap().unwrap().after_timeout);
    assert!(mux.pending_outcomes().is_empty());
}

#[test]
fn native_server_requests_are_queued_without_implicit_reply_or_execution() {
    let mut mux = Multiplexer::new(limits()).unwrap();
    let raw = r#"{"id":17,"method":"item/commandExecution/requestApproval","params":{"opaque":"preserve"}}"#;
    mux.receive(NativeFrame::parse(raw.into()).unwrap())
        .unwrap();
    assert!(mux.take_outbound().is_none());
    let NativeEvent::ServerRequest { ticket, frame } = mux.pop_event().unwrap() else {
        panic!("server request expected")
    };
    assert_eq!(frame.raw_json(), raw);
    mux.reply(
        &ticket,
        ServerReply::Result(RawJson::parse(r#"{"decision":"decline"}"#).unwrap()),
    )
    .unwrap();
    let reply = mux.take_outbound().unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&reply.bytes).unwrap();
    assert_eq!(parsed["id"], 17);
    assert_eq!(parsed["result"]["decision"], "decline");
    assert!(mux
        .reply(&ticket, ServerReply::Result(RawJson::parse("{}").unwrap()))
        .is_err());
}

#[test]
fn server_request_ticket_cannot_cross_native_core_instances() {
    let mut first = Multiplexer::new(limits()).unwrap();
    let mut second = Multiplexer::new(limits()).unwrap();
    let raw = r#"{"id":1,"method":"item/tool/requestUserInput","params":{}}"#;
    first
        .receive(NativeFrame::parse(raw.into()).unwrap())
        .unwrap();
    second
        .receive(NativeFrame::parse(raw.into()).unwrap())
        .unwrap();
    let NativeEvent::ServerRequest { ticket, .. } = first.pop_event().unwrap() else {
        panic!("server request expected")
    };
    assert!(matches!(
        second.reply(&ticket, ServerReply::Result(RawJson::parse("{}").unwrap())),
        Err(NativeCoreError::UnknownServerRequest)
    ));
    assert!(second.take_outbound().is_none());
}

#[test]
fn response_for_an_undispatched_request_is_not_accepted_as_completion() {
    let mut mux = Multiplexer::new(limits()).unwrap();
    let ticket = mux.request("turn/start", None).unwrap();
    assert!(matches!(
        mux.receive(reply(&ticket, "{}")),
        Err(NativeCoreError::MalformedFrame)
    ));
    assert!(matches!(
        mux.progress(&ticket).unwrap(),
        RequestProgress::Pending(_)
    ));
}

#[test]
fn bounded_pending_and_event_queues_fail_explicitly_without_evicting_old_events() {
    let mut limits = limits();
    limits.pending_requests = 1;
    limits.queued_events = 1;
    let mut mux = Multiplexer::new(limits).unwrap();
    mux.request("thread/read", None).unwrap();
    assert!(matches!(
        mux.request("model/list", None),
        Err(NativeCoreError::CapacityExceeded)
    ));
    mux.receive(NativeFrame::parse(r#"{"method":"first","params":{}}"#.into()).unwrap())
        .unwrap();
    assert!(matches!(
        mux.receive(NativeFrame::parse(r#"{"method":"second","params":{}}"#.into()).unwrap()),
        Err(NativeCoreError::CapacityExceeded)
    ));
    let NativeEvent::Notification(frame) = mux.pop_event().unwrap() else {
        panic!("notification expected")
    };
    assert_eq!(
        frame.kind(),
        &FrameKind::Notification {
            method: "first".into()
        }
    );
}

#[test]
fn disconnect_preserves_uncertain_requests_and_forbids_new_admission() {
    let mut mux = Multiplexer::new(limits()).unwrap();
    let ticket = mux.request("turn/start", None).unwrap();
    mux.close(UnknownReason::Transport);
    assert!(matches!(
        mux.progress(&ticket).unwrap(),
        RequestProgress::OutcomeUnknown(_)
    ));
    assert_eq!(mux.pending_outcomes().len(), 1);
    assert!(matches!(
        mux.request("thread/read", None),
        Err(NativeCoreError::Stopped)
    ));
}

#[test]
fn launch_preparation_binds_source_generation_and_role_and_forces_sqlite_home() {
    let spec = spec();
    let prepared = spec.prepare(&spec.binding).unwrap();
    assert!(prepared
        .args
        .windows(2)
        .any(|pair| pair == ["--listen", "stdio://"]));
    assert!(prepared
        .args
        .iter()
        .any(|value| value == r#"sqlite_home="C:\\fixture\\metadata""#));
    let environment = String::from_utf16(&prepared.environment).unwrap();
    assert!(environment.contains("CODEX_HOME=C:\\fixture\\native_home\0"));
    assert!(environment.ends_with("\0\0"));
    let mut changed = spec.binding.clone();
    changed.physical_generation += 1;
    assert!(matches!(
        spec.prepare(&changed),
        Err(NativeCoreError::SourceMismatch)
    ));
    changed = spec.binding.clone();
    changed.canonical_session_id = CanonicalSessionId("other_source".into());
    assert!(matches!(
        spec.prepare(&changed),
        Err(NativeCoreError::SourceMismatch)
    ));
    changed = spec.binding.clone();
    changed.role = CoreRole::ReadWrite;
    assert!(matches!(
        spec.prepare(&changed),
        Err(NativeCoreError::SourceMismatch)
    ));
}

#[test]
fn launch_preparation_rejects_authority_environment_and_home_overrides() {
    let mut spec = spec();
    spec.environment
        .push(("AAG_BROKER_TOKEN".into(), "fixture_secret".into()));
    assert!(matches!(
        spec.prepare(&spec.binding),
        Err(NativeCoreError::InvalidEnvironment)
    ));
    spec.environment.pop();
    spec.config_overrides
        .push(r#"sqlite_home="C:\\other""#.into());
    assert!(matches!(
        spec.prepare(&spec.binding),
        Err(NativeCoreError::ConflictingOverride)
    ));
    spec.config_overrides.pop();
    spec.config_overrides
        .push("sandbox_mode=\"danger-full-access\"".into());
    assert!(matches!(
        spec.prepare(&spec.binding),
        Err(NativeCoreError::ConflictingOverride)
    ));
    spec.config_overrides.pop();
    spec.cwd = PathBuf::from("relative");
    assert!(matches!(
        spec.prepare(&spec.binding),
        Err(NativeCoreError::InvalidConfiguration)
    ));
}

#[test]
fn normal_runtime_principal_requires_exact_source_generation_and_write_role() {
    let mut expected = spec().binding;
    expected.role = CoreRole::ReadWrite;
    let role = NormalServiceRole::RuntimeGeneration {
        source_id: expected.canonical_session_id.clone(),
        physical_generation: expected.physical_generation,
    };
    assert_eq!(binding_for_service(&expected, &role).unwrap(), expected);
}

#[test]
fn normal_runtime_principal_cannot_be_rebound_or_used_as_a_reader() {
    let mut expected = spec().binding;
    expected.role = CoreRole::ReadWrite;
    let role = NormalServiceRole::RuntimeGeneration {
        source_id: expected.canonical_session_id.clone(),
        physical_generation: expected.physical_generation,
    };
    let mut changed = expected.clone();
    changed.physical_generation += 1;
    assert!(matches!(
        binding_for_service(&changed, &role),
        Err(NativeCoreError::SourceMismatch)
    ));
    changed = expected.clone();
    changed.canonical_session_id = CanonicalSessionId("different_source".into());
    assert!(matches!(
        binding_for_service(&changed, &role),
        Err(NativeCoreError::SourceMismatch)
    ));
    changed = expected;
    changed.role = CoreRole::ReadOnly;
    assert!(matches!(
        binding_for_service(&changed, &role),
        Err(NativeCoreError::SourceMismatch)
    ));
}

#[test]
fn fixed_reader_principal_preserves_catalog_binding_and_rejects_write_role() {
    let mut expected = spec().binding;
    assert_eq!(
        binding_for_service(&expected, &NormalServiceRole::TrustedReader).unwrap(),
        expected
    );
    expected.role = CoreRole::ReadWrite;
    assert!(matches!(
        binding_for_service(&expected, &NormalServiceRole::TrustedReader),
        Err(NativeCoreError::SourceMismatch)
    ));
}

#[test]
fn trusted_reader_forces_confirmed_feature_disables_without_changing_runtime_history() {
    let mut spec = spec();
    let prepared = spec.prepare_reader(&spec.binding).unwrap();
    for feature in [
        "plugins",
        "hooks",
        "remote_plugin",
        "code_mode_host",
        "skill_mcp_dependency_install",
    ] {
        assert!(
            prepared
                .args
                .windows(2)
                .any(|pair| pair == ["--disable", feature]),
            "{feature}"
        );
    }
    assert!(prepared
        .args
        .iter()
        .any(|value| value == "history.persistence=\"none\""));
    let normal = spec.prepare(&spec.binding).unwrap();
    assert!(!normal
        .args
        .iter()
        .any(|value| value == "history.persistence=\"none\""));
    spec.config_overrides = vec!["features.plugins=true".into()];
    assert!(matches!(
        spec.prepare_reader(&spec.binding),
        Err(NativeCoreError::ConflictingOverride)
    ));
    spec.config_overrides = vec!["features={plugins=true}".into()];
    assert!(matches!(
        spec.prepare_reader(&spec.binding),
        Err(NativeCoreError::ConflictingOverride)
    ));
    spec.config_overrides = vec!["history.persistence=\"save-all\"".into()];
    assert!(matches!(
        spec.prepare_reader(&spec.binding),
        Err(NativeCoreError::ConflictingOverride)
    ));
}
