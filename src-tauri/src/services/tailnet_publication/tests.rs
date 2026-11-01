use super::*;
use serde_json::json;
use std::{path::Path, sync::atomic::AtomicBool};

fn plan() -> PublicationPlan {
    let config: PublicationConfig =
        serde_json::from_value(json!({"enabled":true,"port":8989})).unwrap();
    config
        .plan("fixture-existing-key", "127.0.0.1:45678")
        .unwrap()
}

fn background() -> serde_json::Value {
    json!({"TCP":{"443":{"HTTPS":true},"8443":{"HTTPS":true}},"Foreground":{
        "existing-fg":{"TCP":{"8990":{"TCPForward":"127.0.0.1:45679"}}}
    }})
}

fn with_owned() -> serde_json::Value {
    let mut status = background();
    status["Foreground"]["new-owner"] = json!({"TCP":{"8989":{"TCPForward":"127.0.0.1:45678"}}});
    status
}

fn status(value: serde_json::Value) -> ServeStatus {
    ServeStatus::parse(&value.to_string()).unwrap()
}

#[test]
fn configuration_rejects_zero_overflow_unknown_fields_and_missing_existing_key() {
    for value in [
        json!({"enabled":true,"port":0}),
        json!({"enabled":true,"port":65536}),
        json!({"enabled":true,"port":8989,"funnel":true}),
    ] {
        assert!(serde_json::from_value::<PublicationConfig>(value).is_err());
    }
    let config: PublicationConfig =
        serde_json::from_value(json!({"enabled":true,"port":65535})).unwrap();
    assert_eq!(
        config.plan("   ", "127.0.0.1:45678").err(),
        Some(PublicationError::ExistingKeyRequired)
    );
    assert_eq!(
        config.plan("fixture-existing-key", "localhost:45678").err(),
        Some(PublicationError::BackendInvalid)
    );
    assert_eq!(
        config.plan("fixture-existing-key", "0.0.0.0:45678").err(),
        Some(PublicationError::BackendInvalid)
    );
    assert_eq!(
        config
            .plan("fixture-existing-key", "100.64.1.2:45678")
            .err(),
        Some(PublicationError::BackendInvalid)
    );
}

#[test]
fn foreground_arguments_never_persist_funnel_reset_or_include_the_key() {
    let plan = plan();
    assert_eq!(
        plan.foreground_args(),
        ["serve", "--tcp=8989", "tcp://127.0.0.1:45678"]
    );
    assert_eq!(
        PublicationPlan::status_args(),
        ["serve", "status", "--json"]
    );
    let text = plan.foreground_args().join(" ");
    for forbidden in ["--bg", "funnel", "reset", "fixture-existing-key"] {
        assert!(!text.contains(forbidden));
    }
    assert!(!format!("{plan:?}").contains("127.0.0.1"));
}

#[test]
fn existing_background_foreground_and_own_port_funnel_routes_are_conflicts() {
    let plan = plan();
    assert!(status(background()).preflight(&plan).is_ok());
    let mut existing = background();
    existing["TCP"]["8989"] = json!({"TCPForward":"127.0.0.1:45678"});
    assert_eq!(
        status(existing).preflight(&plan).err().map(|e| e.code()),
        Some("tailnet_publication_slot_occupied")
    );
    assert_eq!(
        status(with_owned())
            .preflight(&plan)
            .err()
            .map(|e| e.code()),
        Some("tailnet_publication_slot_occupied")
    );
    let mut funnel = background();
    funnel["AllowFunnel"] = json!({"fixture.ts.net:8989":true});
    assert_eq!(
        status(funnel).preflight(&plan).err().map(|e| e.code()),
        Some("tailnet_publication_funnel_conflict")
    );
}

#[test]
fn ownership_binds_only_the_new_exact_foreground_session_and_scopes_stop() {
    let plan = plan();
    let before = status(background());
    let preflight = before.preflight(&plan).unwrap();
    let after = status(with_owned());
    let owned = preflight.bind(&plan, &after).unwrap();
    assert!(owned.matches(&after));
    assert_eq!(
        owned.targeted_stop_args(&after).unwrap(),
        ["serve", "--tcp=8989", "off"]
    );
    assert!(owned.verify_removed(&after, &before).is_ok());
    let mut changed = background();
    changed["TCP"]["443"] = json!({"HTTPS":false,"HTTP":true});
    assert_eq!(
        owned.verify_removed(&after, &status(changed)),
        Err(PublicationError::OwnershipUnknown)
    );
}

#[test]
fn background_adoption_target_changes_and_foreign_takeover_are_never_owned() {
    let plan = plan();
    let before = status(background());
    let preflight = before.preflight(&plan).unwrap();
    let mut background_match = background();
    background_match["TCP"]["8989"] = json!({"TCPForward":"127.0.0.1:45678"});
    assert_eq!(
        preflight
            .bind(&plan, &status(background_match))
            .err()
            .map(|e| e.code()),
        Some("tailnet_publication_ownership_unknown")
    );
    let after = status(with_owned());
    let owned = preflight.bind(&plan, &after).unwrap();
    let mut takeover = with_owned();
    takeover["Foreground"]["new-owner"]["TCP"]["8989"]["TCPForward"] = json!("127.0.0.1:45680");
    let takeover = status(takeover);
    assert!(!owned.matches(&takeover));
    assert_eq!(
        owned.targeted_stop_args(&takeover),
        Err(PublicationError::OwnershipLost)
    );
    assert_eq!(
        owned.verify_removed(&after, &takeover),
        Err(PublicationError::OwnershipLost)
    );
}

#[test]
fn malformed_status_and_errors_never_echo_private_status_content() {
    let private = "fixture-secret-value";
    let error = ServeStatus::parse(&json!({"TCP":{private:{"TCPForward":private}}}).to_string())
        .err()
        .unwrap();
    assert_eq!(error, PublicationError::StatusInvalid);
    assert!(!format!("{error:?} {error}").contains(private));
}

#[test]
fn cancellation_before_start_runs_no_command_and_generates_no_key() {
    let cancelled = AtomicBool::new(true);
    assert_eq!(
        NativePublication::start(Path::new("fixture-not-an-executable"), plan(), &cancelled)
            .err()
            .map(|e| e.code()),
        Some("tailnet_publication_cancelled")
    );
}

#[test]
fn web_only_and_foreground_funnel_uses_reserve_the_requested_port() {
    let plan = plan();
    let mut web = background();
    web["Web"] =
        json!({"fixture.ts.net:8989":{"Handlers":{"/":{"Proxy":"http://127.0.0.1:45678"}}}});
    assert_eq!(
        status(web).preflight(&plan).err(),
        Some(PublicationError::SlotOccupied)
    );
    let mut funnel = background();
    funnel["Foreground"]["existing-fg"]["AllowFunnel"] = json!({"fixture.ts.net:8989":true});
    assert_eq!(
        status(funnel).preflight(&plan).err(),
        Some(PublicationError::FunnelConflict)
    );
}

#[test]
fn tls_proxy_protocol_and_competing_foreground_sessions_are_not_adopted() {
    let plan = plan();
    let preflight = status(background()).preflight(&plan).unwrap();
    for field in ["TerminateTLS", "ProxyProtocol", "HTTPS"] {
        let mut after = with_owned();
        after["Foreground"]["new-owner"]["TCP"]["8989"][field] = match field {
            "TerminateTLS" => json!("fixture.ts.net"),
            "ProxyProtocol" => json!(1),
            _ => json!(true),
        };
        assert_eq!(
            preflight.bind(&plan, &status(after)).err(),
            Some(PublicationError::OwnershipUnknown)
        );
    }
    let mut competing = with_owned();
    competing["Foreground"]["another-new-owner"] =
        json!({"TCP":{"8989":{"TCPForward":"127.0.0.1:45678"}}});
    assert_eq!(
        preflight.bind(&plan, &status(competing)).err(),
        Some(PublicationError::OwnershipUnknown)
    );
}

#[test]
fn an_existing_foreground_session_id_is_not_adopted_for_a_new_target() {
    let plan = plan();
    let mut before = background();
    before["Foreground"]["new-owner"] = json!({"TCP":{"8991":{"TCPForward":"127.0.0.1:45678"}}});
    let preflight = status(before).preflight(&plan).unwrap();
    assert_eq!(
        preflight.bind(&plan, &status(with_owned())).err(),
        Some(PublicationError::OwnershipUnknown)
    );
}

#[test]
fn numeric_ipv6_loopback_is_valid_but_zero_backend_and_disabled_config_are_not() {
    let enabled: PublicationConfig =
        serde_json::from_value(json!({"enabled":true,"port":8989})).unwrap();
    assert_eq!(
        enabled
            .plan("fixture-existing-key", "[::1]:45678")
            .unwrap()
            .foreground_args(),
        ["serve", "--tcp=8989", "tcp://[::1]:45678"]
    );
    assert_eq!(
        enabled.plan("fixture-existing-key", "127.0.0.1:0").err(),
        Some(PublicationError::BackendInvalid)
    );
    let disabled: PublicationConfig =
        serde_json::from_value(json!({"enabled":false,"port":8989})).unwrap();
    assert_eq!(
        disabled.plan("", "127.0.0.1:45678").err(),
        Some(PublicationError::Disabled)
    );
}
