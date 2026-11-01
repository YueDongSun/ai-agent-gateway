use super::*;

#[test]
fn reader_rejects_mutation_execution_and_unknown_even_from_owner() {
    for method in [
        "thread/read",
        "thread/list",
        "thread/turns/list",
        "thread/items/list",
    ] {
        assert!(reader_permits(method, Some(r#"{"threadId":"native"}"#)).unwrap());
    }
    for method in [
        "initialize",
        "thread/resume",
        "thread/start",
        "turn/start",
        "turn/interrupt",
        "command/exec",
        "fs/readFile",
        "config/write",
        "tools/call",
        "thread/newReadApi",
    ] {
        assert!(!reader_permits(method, Some("{}")).unwrap(), "{method}");
    }
    assert!(!reader_permits("thread/read", Some(r#"{"path":"C:\\private"}"#)).unwrap());
    assert!(!reader_permits("thread/read", Some(r#"{"history":[]}"#)).unwrap());
    assert!(reader_permits("thread/read", Some(r#"{"path":null}"#)).unwrap());
}

#[test]
fn policy_role_and_stale_generation_are_rejected_before_native_admission() {
    let key = RuntimeSourceKey::parse("0123456789abcdef0123456789abcdef").unwrap();
    assert_eq!(
        admitted_core_role(ServiceRole::Policy, key, 7),
        Err(NativeRoleError::RoleMismatch)
    );
    assert_eq!(
        admitted_core_role(ServiceRole::runtime(key.as_str(), 8).unwrap(), key, 7),
        Err(NativeRoleError::SourceMismatch)
    );
    assert_eq!(
        admitted_core_role(ServiceRole::Reader, key, 7),
        Ok(CoreRole::ReadOnly)
    );
    assert_eq!(
        admitted_core_role(ServiceRole::runtime(key.as_str(), 7).unwrap(), key, 7),
        Ok(CoreRole::ReadWrite)
    );
}

#[test]
fn opaque_payloads_survive_header_rewrite_and_bounded_correlation() {
    let raw = r#"{ "id":"native", "result" : {"id":"nested", "big":18446744073709551615, "signature":"a\\b", "opaque":[1, {"x":" y "}]} }"#;
    let owner = OwnerId::parse(r#""owner\u005fid""#).unwrap();
    let rewritten = rewrite_id(raw, &owner).unwrap();
    let expected = raw.replacen(r#""native""#, r#""owner\u005fid""#, 1);
    assert_eq!(std::str::from_utf8(rewritten.bytes()).unwrap(), expected);
    let params = raw_members(r#"{"method":"thread/read","id":2,"params":{"big":18446744073709551615,"signature":"a\\b"}}"#).unwrap();
    assert_eq!(
        params.get("params").unwrap(),
        r#"{"big":18446744073709551615,"signature":"a\\b"}"#
    );
    let mut correlations = Correlations::new(1);
    correlations.insert(owner.clone(), 17).unwrap();
    assert_eq!(
        correlations.insert(OwnerId::parse(r#""owner_id""#).unwrap(), 18),
        Err(NativeRoleError::DuplicateId)
    );
    assert_eq!(
        correlations.insert(OwnerId::parse("3").unwrap(), 19),
        Err(NativeRoleError::Capacity)
    );
    assert_eq!(correlations.remove(&owner).unwrap(), 17);
    assert_eq!(correlations.remove(&owner), Err(NativeRoleError::UnknownId));
}

#[test]
fn cancellation_requires_terminal_pipe_and_native_cleanup() {
    assert_eq!(
        cleanup_result(
            None,
            TransportClosed::Closed,
            NativeShutdown::Stopped { exit_code: Some(1) }
        ),
        Ok(())
    );
    assert_eq!(
        cleanup_result(
            None,
            TransportClosed::OutcomeUnknown(TransportError::DeadlineExceeded),
            NativeShutdown::Stopped { exit_code: Some(1) }
        ),
        Err(NativeRoleError::CleanupUnknown)
    );
    assert_eq!(
        cleanup_result(
            None,
            TransportClosed::Closed,
            NativeShutdown::OutcomeUnknown {
                error: NativeCoreError::TransportFailed
            }
        ),
        Err(NativeRoleError::CleanupUnknown)
    );
    assert_eq!(
        cleanup_result(
            Some(NativeRoleError::ReaderServerRequest),
            TransportClosed::Closed,
            NativeShutdown::Stopped { exit_code: Some(1) }
        ),
        Err(NativeRoleError::ReaderServerRequest)
    );
}
