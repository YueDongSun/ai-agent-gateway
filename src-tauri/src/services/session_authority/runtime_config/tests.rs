//! Scalar fixtures never mint VerifiedConfigBytes or production capabilities.

use super::*;
use serde_json::{json, Value};

const POLICY: &str = "S-1-5-80-101-102-103-104-105";
const READER: &str = "S-1-5-80-201-202-203-204-205";
const OPERATOR: &str = "S-1-5-21-10-20-30-1001";
const KEY_OLD: &str = "11111111111111111111111111111111";
const KEY_NEW: &str = "22222222222222222222222222222222";
const PROGRAM_FILES: &str = r"C:\Program Files";
const HOME: &str = r"C:\Users\operator_fixture";

fn source(key: &str, ordinal: u64) -> Value {
    let root = Path::new(HOME).join(".codex-cli");
    json!({
        "source_key":key, "canonical_id":format!("canonical-{ordinal}"),
        "native_session_id":format!("native-session-{ordinal}"),
        "native_thread_ids":[format!("native-thread-{ordinal}-one"),format!("native-thread-{ordinal}-two")],
        "native_home":root.join(r".authority\sources").join(key).join("native_home"),
        "reader_index":root.join(r".authority\indexes\reader").join(key),
        "runtime_index":root.join(r".authority\indexes\runtime").join(key).join("1"),
        "generation":1, "runtime_sid":format!("S-1-5-80-301-302-303-304-{}",305+ordinal),
        "root_identity":{"volume":7,"object":ordinal+100}
    })
}
fn config() -> Value {
    json!({
        "schema_version":1, "installation_id":"0123456789abcdef0123456789abcdef",
        "operator_home":HOME, "client_root":Path::new(HOME).join(".codex-cli"),
        "protected_shim":{"path":r"C:\Program Files\AI Agent Gateway\SessionAuthority\aag_session_authority.exe","object":{"volume":7,"object":20}},
        "pinned_native":{"path":r"C:\Program Files\WindowsApps\OpenAI.Codex_1.2.3.4_x64__2p2nqsd0c76g0\app\resources\codex.exe","object":{"volume":7,"object":21}},
        "policy_sid":POLICY,"reader_sid":READER,"operator_sid":OPERATOR,"gateway_port":8989,
        "sources":[source(KEY_OLD,1),source(KEY_NEW,2)]
    })
}
fn parse(
    value: &Value,
    role: ServiceRole,
    owner: &str,
) -> Result<FixtureRuntimeData, RuntimeConfigError> {
    parse_fixture(
        &serde_json::to_vec(value).unwrap(),
        role,
        owner,
        Path::new(PROGRAM_FILES),
    )
}
fn rejected(value: &Value, expected: RuntimeConfigError) {
    assert_eq!(
        parse(value, ServiceRole::Policy, POLICY).err(),
        Some(expected)
    );
}

#[test]
fn valid_inventory_binds_runtime_role_and_protected_policy_owner_without_readiness() {
    for role in [
        ServiceRole::Policy,
        ServiceRole::Reader,
        ServiceRole::runtime(KEY_OLD, 1).unwrap(),
    ] {
        let fixture = parse(&config(), role, POLICY).unwrap();
        assert_eq!(fixture.0.sources.len(), 2);
        assert_eq!(
            fixture.0.installation_id.as_str(),
            "0123456789abcdef0123456789abcdef"
        );
        assert_eq!(fixture.0.sources[0].canonical_id.as_str(), "canonical-1");
        assert_eq!(
            fixture.0.sources[1].native_thread_ids[1].as_str(),
            "native-thread-2-two"
        );
        assert_eq!(fixture.0.gateway_port.get(), 8989);
    }
    assert_eq!(
        parse(&config(), ServiceRole::runtime(KEY_OLD, 2).unwrap(), POLICY).err(),
        Some(RuntimeConfigError::RoleMismatch)
    );
    assert_eq!(
        parse(
            &config(),
            ServiceRole::runtime("33333333333333333333333333333333", 1).unwrap(),
            POLICY
        )
        .err(),
        Some(RuntimeConfigError::RoleMismatch)
    );
    assert_eq!(
        parse(&config(), ServiceRole::Reader, READER).err(),
        Some(RuntimeConfigError::OwnerMismatch)
    );
}

#[test]
fn schema_rejects_unknown_secret_readiness_and_mode_fields_without_echoing_contents() {
    for field in [
        "api_key",
        "env",
        "auth",
        "model_provider",
        "read_only",
        "committed",
        "ready",
    ] {
        let mut value = config();
        value[field] = json!("synthetic-secret-never-in-error");
        rejected(&value, RuntimeConfigError::InvalidJson);
        let mut value = config();
        value["sources"][0][field] = json!(true);
        rejected(&value, RuntimeConfigError::InvalidJson);
    }
    let mut value = config();
    value["protected_shim"]["api_key"] = json!("synthetic-secret");
    rejected(&value, RuntimeConfigError::InvalidJson);
    let mut value = config();
    value["schema_version"] = json!(2);
    rejected(&value, RuntimeConfigError::InvalidSchema);
    let duplicated = serde_json::to_string(&config()).unwrap().replacen(
        "\"schema_version\":1",
        "\"schema_version\":1,\"schema_version\":1",
        1,
    );
    assert_eq!(
        parse_fixture(
            duplicated.as_bytes(),
            ServiceRole::Policy,
            POLICY,
            Path::new(PROGRAM_FILES)
        )
        .err(),
        Some(RuntimeConfigError::InvalidJson)
    );
    assert!(!RuntimeConfigError::InvalidJson
        .to_string()
        .contains("synthetic-secret"));
}

#[test]
fn duplicate_sources_native_threads_principals_paths_and_objects_are_rejected() {
    for field in [
        "source_key",
        "canonical_id",
        "native_session_id",
        "runtime_sid",
        "root_identity",
    ] {
        let mut value = config();
        value["sources"][1][field] = value["sources"][0][field].clone();
        rejected(&value, RuntimeConfigError::Duplicate);
    }
    let mut value = config();
    value["sources"][1]["native_thread_ids"][0] =
        value["sources"][0]["native_thread_ids"][0].clone();
    rejected(&value, RuntimeConfigError::Duplicate);
    let mut value = config();
    value["sources"][0]["native_thread_ids"][1] =
        value["sources"][0]["native_thread_ids"][0].clone();
    rejected(&value, RuntimeConfigError::Duplicate);
    let mut value = config();
    value["sources"][1]["native_home"] = value["sources"][0]["native_home"].clone();
    assert!(parse(&value, ServiceRole::Policy, POLICY).is_err());
}

#[test]
fn all_paths_stay_in_the_exact_operator_inventory_and_images_in_vendor_layout() {
    for field in ["operator_home", "client_root"] {
        for bad in [
            r"\\server\share\user",
            r"\\?\C:\Users\user",
            r"C:\Users\operator_fixture\..\other",
            r"M:\Users\user",
            r"C:\Users\user:stream",
        ] {
            let mut value = config();
            value[field] = json!(bad);
            rejected(&value, RuntimeConfigError::InvalidPath);
        }
    }
    let mut value = config();
    value["client_root"] = json!(r"C:\Users\other_fixture_user\.codex-cli");
    rejected(&value, RuntimeConfigError::InvalidPath);
    for field in ["native_home", "reader_index", "runtime_index"] {
        let mut value = config();
        value["sources"][0][field] = json!(r"C:\Users\other_fixture_user\.codex-cli\.authority\foreign");
        rejected(&value, RuntimeConfigError::InvalidPath);
    }
    let mut value = config();
    value["sources"][0]["runtime_index"] = json!(Path::new(HOME)
        .join(r".codex-cli\.authority\indexes\runtime")
        .join(KEY_OLD)
        .join("2"));
    rejected(&value, RuntimeConfigError::InvalidPath);
    for bad in [
        r"C:\Users\operator_fixture\aag_session_authority.exe",
        r"C:\Program Files\AI Agent Gateway\other.exe",
    ] {
        let mut value = config();
        value["protected_shim"]["path"] = json!(bad);
        rejected(&value, RuntimeConfigError::InvalidImage);
    }
    for bad in [
        r"C:\Program Files\WindowsApps\OpenAI.Codex_1.2.3.4_x64__other\app\resources\codex.exe",
        r"C:\Program Files\WindowsApps\OpenAI.Codex_1.2.3.4_arm64__2p2nqsd0c76g0\app\resources\codex.exe",
        r"C:\Program Files\WindowsApps\OpenAI.Codex_1.2.3.4_x64__2p2nqsd0c76g0\app\resources\..\codex.exe",
    ] {
        let mut value = config();
        value["pinned_native"]["path"] = json!(bad);
        rejected(&value, RuntimeConfigError::InvalidImage);
    }
}

#[test]
fn role_sids_must_be_canonical_distinct_normal_services_and_operator_account_shaped() {
    for field in ["policy_sid", "reader_sid"] {
        for bad in [
            "S-1-5-80-0",
            "S-1-5-18",
            OPERATOR,
            "S-1-5-80-01-2-3-4-5",
            "S-1-5-80-1-2-3-4-4294967296",
        ] {
            let mut value = config();
            value[field] = json!(bad);
            rejected(&value, RuntimeConfigError::InvalidSid);
        }
    }
    for bad in ["S-1-5-18", "S-1-5-32-544", POLICY, "S-1-5-21-1-2-3-0001"] {
        let mut value = config();
        value["operator_sid"] = json!(bad);
        rejected(&value, RuntimeConfigError::InvalidSid);
    }
    let mut value = config();
    value["reader_sid"] = json!(POLICY);
    rejected(&value, RuntimeConfigError::Duplicate);
    for bad in [POLICY, READER, "S-1-5-80-0", OPERATOR] {
        let mut value = config();
        value["sources"][0]["runtime_sid"] = json!(bad);
        assert!(parse(&value, ServiceRole::Policy, POLICY).is_err());
    }
}

#[test]
fn ports_inventory_counts_identifiers_generations_and_identity_are_bounded() {
    for port in [0, 65536, u64::MAX] {
        let mut value = config();
        value["gateway_port"] = json!(port);
        rejected(&value, RuntimeConfigError::InvalidPort);
    }
    for count in [0, 1, 129] {
        let mut value = config();
        value["sources"] = json!((0..count).map(|_| source(KEY_OLD, 1)).collect::<Vec<_>>());
        rejected(&value, RuntimeConfigError::InvalidBounds);
    }
    for field in ["canonical_id", "native_session_id"] {
        for bad in [String::new(), "x".repeat(257), "has\ncontrol".into()] {
            let mut value = config();
            value["sources"][0][field] = json!(bad);
            rejected(&value, RuntimeConfigError::InvalidIdentifier);
        }
    }
    let mut value = config();
    value["sources"][0]["native_thread_ids"] = json!([]);
    rejected(&value, RuntimeConfigError::InvalidBounds);
    let mut value = config();
    value["sources"][0]["generation"] = json!(0);
    rejected(&value, RuntimeConfigError::InvalidBounds);
    let mut value = config();
    value["sources"][0]["source_key"] = json!("../foreign");
    rejected(&value, RuntimeConfigError::InvalidIdentifier);
    let mut value = config();
    value["installation_id"] = json!("not-an-installation-id");
    rejected(&value, RuntimeConfigError::InvalidIdentifier);
    for field in ["protected_shim", "pinned_native"] {
        let mut value = config();
        value[field]["object"]["object"] = json!(0);
        rejected(&value, RuntimeConfigError::InvalidImage);
    }
    let mut value = config();
    value["sources"][0]["root_identity"]["object"] = json!(0);
    rejected(&value, RuntimeConfigError::InvalidBounds);
}
