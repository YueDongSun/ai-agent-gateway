use super::*;
use serde_json::{json, Value};

const REGISTRY_COMMIT: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const FETCHED_AT: &str = "2026-08-30T09:00:00.000Z";
const FIXTURE: &str = include_str!("../../../../scripts/dsh_market/test_feed.json");
fn fixture() -> Value {
    serde_json::from_str(FIXTURE).unwrap()
}
fn normalize(value: &Value) -> Result<Market_catalog, AppError> {
    normalize::normalize_feed(
        &serde_json::to_vec(value).unwrap(),
        REGISTRY_COMMIT,
        FETCHED_AT,
    )
}
struct Test_root(std::path::PathBuf);
impl Test_root {
    fn new() -> Self {
        let mut id = [0u8; 16];
        getrandom::getrandom(&mut id).unwrap();
        Self(std::env::temp_dir().join(format!("aag_market_test_{}", hex::encode(id))))
    }
}
impl Drop for Test_root {
    fn drop(&mut self) {
        if self.0.exists() {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }
}

#[test]
fn market_provenance_and_classification_are_explicit() {
    let catalog = normalize(&fixture()).unwrap();
    assert_eq!(catalog.entry_count, 2);
    assert_eq!(catalog.registry_commit, REGISTRY_COMMIT);
    assert_eq!(
        catalog.entries[0].source_commit.as_deref(),
        Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
    );
    assert!(catalog.entries[1].source_commit.is_none());
    assert!(catalog.entries[1].source_commit_url.is_none());
    assert!(catalog.entries[0]
        .use_cases
        .contains(&"claude_native".into()));
    assert!(catalog.entries[0]
        .use_cases
        .contains(&"desktop_workflow".into()));
    assert!(catalog.entries[0].categories.contains(&"models".into()));
    assert!(catalog.entries[1]
        .categories
        .contains(&"personalization".into()));
    assert!(catalog.entries[0].modules.contains(&"models".into()));
    assert!(catalog.entries[0].modules.contains(&"tools".into()));
    assert!(catalog.entries[1].modules.contains(&"appearance".into()));
    assert!(catalog.entries[1].modules.contains(&"knowledge".into()));
    assert!(catalog
        .entries
        .iter()
        .all(|entry| entry.classification_basis == "metadata_rules_v2"));
    assert!(catalog.entries[1].description_en.is_none());
    assert_eq!(catalog.entries[0].verification, "not_audited");
    assert_eq!(catalog.privacy_scrub.schema_version, 1);
    assert_eq!(catalog.privacy_scrub.source_entry_count, 2);
    assert_eq!(
        catalog.entries[0].install_spec.as_deref(),
        Some("@example/claude-desktop-tools@1.2.3")
    );
}

#[test]
fn market_scrubs_private_description_data_and_records_receipt() {
    let mut feed = fixture();
    let email = ["person", "example.test"].join("@");
    let authenticated_url = format!("https://{}:{}@{}", "user", "secret", "example.test");
    let user_path = "/home/fixture-user/config";
    let private_address = [100, 64, 1, 2].map(|part| part.to_string()).join(".");
    let tailnet_name = ["fixture-node", "ts.net"].join(".");
    let description = format!(
        "contact {email} {authenticated_url} {user_path} {private_address} {tailnet_name} http://127.0.0.1:9"
    );
    feed["plugins"][0]["description"] = json!(description);
    feed["plugins"][0]["descriptionEn"] = feed["plugins"][0]["description"].clone();
    let catalog = normalize(&feed).unwrap();
    let entry = catalog
        .entries
        .iter()
        .find(|entry| entry.full_name == "example/claude-desktop-tools")
        .unwrap();
    for private_value in [
        &email,
        "user:secret",
        user_path,
        &private_address,
        &tailnet_name,
    ] {
        assert!(!entry.description.contains(private_value));
        assert!(!entry
            .description_en
            .as_deref()
            .unwrap_or("")
            .contains(private_value));
    }
    assert!(entry.description.contains("http://127.0.0.1:9"));
    assert!(catalog.privacy_scrub.total_replacements >= 10);
    assert_eq!(catalog.privacy_scrub.modified_entries, 1);
    let encoded = serde_json::to_vec(&catalog).unwrap();
    normalize::parse_catalog(&encoded).unwrap();
    let mut unsafe_catalog = catalog;
    unsafe_catalog.entries[0].description = email;
    assert!(normalize::validate_catalog(&unsafe_catalog).is_err());
}

#[test]
fn market_scrubs_punctuated_private_networks_and_license_display_text() {
    let mut feed = fixture();
    let email = ["person", "example.test"].join("@");
    let private_ipv4 = [192, 168, 4, 5].map(|part| part.to_string()).join(".");
    let tailnet_ipv6 = ["fd7a", "115c", "a1e0", "", "42"].join(":");
    let link_local_ipv6 = ["fe80", "", "1"].join(":");
    let public_ipv6 = ["2001", "4860", "4860", "", "8888"].join(":");
    feed["plugins"][0]["description"] = json!(format!(
        "Contact ({email}). Hosts {private_ipv4}. [{tailnet_ipv6}]:443 and {link_local_ipv6}; public {public_ipv6}."
    ));
    feed["plugins"][0]["license"] = json!(format!("Custom terms: {email}."));

    let catalog = normalize(&feed).unwrap();
    let entry = catalog
        .entries
        .iter()
        .find(|entry| entry.full_name == "example/claude-desktop-tools")
        .unwrap();
    for private_value in [&email, &private_ipv4, &tailnet_ipv6, &link_local_ipv6] {
        assert!(!entry.description.contains(private_value));
        assert!(!entry
            .license
            .as_deref()
            .unwrap_or("")
            .contains(private_value));
    }
    assert!(entry.description.contains(&public_ipv6));
    assert!(catalog.privacy_scrub.fields.contains(&"license".into()));

    let mut unsafe_catalog = catalog;
    unsafe_catalog.entries[0].license = Some(format!("Owner {email}."));
    assert!(normalize::validate_catalog(&unsafe_catalog).is_err());
}

#[test]
fn market_scrubs_assignment_prefixed_substrings_and_mapped_private_ipv6() {
    let mut feed = fixture();
    let private_ipv4 = [10, 12, 34, 56].map(|part| part.to_string()).join(".");
    let user_path = ["", "home", "fixture-user", "private-config"].join("/");
    let private_ipv6 = ["fd7a", "115c", "a1e0", "", "5"].join(":");
    let mapped_ipv6 = format!("::ffff:{private_ipv4}");
    let value =
        format!("IP={private_ipv4} PATH={user_path} V6={private_ipv6} MAPPED={mapped_ipv6}");
    feed["plugins"][0]["description"] = json!(value);
    feed["plugins"][0]["license"] = feed["plugins"][0]["description"].clone();

    let catalog = normalize(&feed).unwrap();
    let entry = &catalog.entries[0];
    for private_value in [&private_ipv4, &user_path, &private_ipv6, &mapped_ipv6] {
        assert!(!entry.description.contains(private_value));
        assert!(!entry
            .license
            .as_deref()
            .unwrap_or("")
            .contains(private_value));
    }
    assert!(catalog.privacy_scrub.replacements["private_network"] >= 6);
    assert!(catalog.privacy_scrub.replacements["user_path"] >= 2);
    assert_eq!(catalog.privacy_scrub.modified_entries, 1);
}

#[test]
fn market_rejects_unsafe_package_identity_instead_of_redacting_it() {
    let mut feed = fixture();
    feed["plugins"][0]["packageName"] = json!("scope/../../private");
    assert!(normalize(&feed)
        .unwrap_err()
        .to_string()
        .contains("invalid_package"));
}
#[test]
fn market_rejects_bad_urls_duplicates_unknown_schemas_and_commits() {
    for bad in [
        "javascript:alert(1)",
        "https://github.com@evil.example/x",
        "https://github.com/example/claude-desktop-tools?token=x",
    ] {
        let mut feed = fixture();
        feed["plugins"][0]["htmlUrl"] = json!(bad);
        assert!(normalize(&feed)
            .unwrap_err()
            .to_string()
            .contains("invalid_repository_url"));
    }
    let mut duplicate = fixture();
    let entry = duplicate["plugins"][0].clone();
    duplicate["plugins"].as_array_mut().unwrap().push(entry);
    assert!(normalize(&duplicate)
        .unwrap_err()
        .to_string()
        .contains("duplicate_entry"));
    let mut bad_commit = fixture();
    bad_commit["plugins"][0]["verifiedCommit"] = json!("main");
    assert!(normalize(&bad_commit).is_err());
    let mut bad_schema = fixture();
    bad_schema["schemaVersion"] = json!(99);
    assert!(normalize(&bad_schema).is_err());
}
#[test]
fn market_rejects_empty_oversized_and_malformed_feeds() {
    let mut feed = fixture();
    feed["plugins"] = json!([]);
    assert!(normalize(&feed).is_err());
    feed["plugins"] = json!(vec![json!({}); MAX_ENTRIES + 1]);
    assert!(normalize(&feed).is_err());
    assert!(normalize::normalize_feed(
        &vec![b' '; MAX_FEED_BYTES + 1],
        REGISTRY_COMMIT,
        FETCHED_AT
    )
    .is_err());
    assert!(normalize::normalize_feed(b"{", REGISTRY_COMMIT, FETCHED_AT).is_err());
}
#[test]
fn market_site_feed_derives_repository_and_keeps_revision_unknown() {
    let mut feed = fixture();
    for item in feed["plugins"].as_array_mut().unwrap() {
        item.as_object_mut().unwrap().remove("htmlUrl");
        item.as_object_mut().unwrap().remove("verifiedCommit");
    }
    let catalog =
        normalize::normalize_site_feed(&serde_json::to_vec(&feed).unwrap(), FETCHED_AT).unwrap();
    assert_eq!(catalog.source_registry, "dsh.market");
    assert!(catalog.registry_commit.is_empty());
    assert_eq!(catalog.registry_url, "https://dsh.market/plugins.json");
    assert!(catalog.collector_url.is_empty());
    assert_eq!(
        catalog.entries[0].repo_url,
        format!("https://github.com/{}", catalog.entries[0].full_name)
    );
    assert!(catalog
        .entries
        .iter()
        .all(|entry| entry.source_commit.is_none()));
    assert!(normalize::validate_catalog(&catalog).is_ok());
    let mut invalid = catalog;
    invalid.collector_url = "https://evil.example/collector".into();
    assert!(normalize::validate_catalog(&invalid).is_err());
}
#[test]
fn market_site_surrogate_repair_matches_browser_json_behavior() {
    let malformed = br#"{"description":"flag \ud83c truncated"}"#;
    let repaired = normalize::replace_unpaired_surrogates(malformed);
    let parsed: Value = serde_json::from_slice(&repaired).unwrap();
    assert_eq!(parsed["description"], "flag � truncated");
    let valid_pair = br#"{"description":"\ud83c\uddec"}"#;
    assert_eq!(
        normalize::replace_unpaired_surrogates(valid_pair),
        valid_pair
    );
    let escaped_literal = br#"{"description":"\\ud83c"}"#;
    assert_eq!(
        normalize::replace_unpaired_surrogates(escaped_literal),
        escaped_literal
    );
}
#[test]
fn market_limits_sources_and_never_opens_arbitrary_urls() {
    let catalog = normalize(&fixture()).unwrap();
    assert_eq!(
        source_url(&catalog, &catalog.entries[0].id, "repository").unwrap(),
        catalog.entries[0].repo_url
    );
    assert!(source_url(&catalog, &catalog.entries[1].id, "commit").is_err());
    assert!(source_url(&catalog, "javascript:alert(1)", "repository").is_err());
    assert!(source_url(&catalog, "", "https://evil.example").is_err());
    assert!(fetch_url_allowed(&format!(
        "https://api.github.com/repos/{SOURCE_REGISTRY}/commits/main"
    )));
    assert!(fetch_url_allowed(SITE_FEED_URL));
    assert!(!fetch_url_allowed(
        "https://dsh.market/plugins.json?cache=1"
    ));
    assert!(fetch_url_allowed(&format!("https://raw.githubusercontent.com/{SOURCE_REGISTRY}/{REGISTRY_COMMIT}/registry/plugins.json")));
    assert!(!fetch_url_allowed(&format!(
        "https://raw.githubusercontent.com/{SOURCE_REGISTRY}/main/registry/plugins.json"
    )));
    assert!(!fetch_url_allowed(&format!(
        "https://api.github.com/repos/{SOURCE_REGISTRY}/commits/main?token=x"
    )));
}
#[test]
fn market_get_is_read_only_and_bad_cache_preserves_evidence() {
    let root = Test_root::new();
    assert!(get_catalog(&root.0)
        .unwrap_err()
        .to_string()
        .contains("catalog_unavailable"));
    assert!(!root.0.exists());
    fs::create_dir_all(&root.0).unwrap();
    fs::write(root.0.join(CACHE_FILE), b"broken evidence").unwrap();
    assert!(get_catalog(&root.0).is_err());
    assert_eq!(
        fs::read(root.0.join(CACHE_FILE)).unwrap(),
        b"broken evidence"
    );
}
#[test]
fn market_cache_is_validated_before_replace_and_preserved_on_write_failure() {
    let root = Test_root::new();
    let catalog = normalize(&fixture()).unwrap();
    replace_cache(&root.0, &catalog).unwrap();
    let before = fs::read(root.0.join(CACHE_FILE)).unwrap();
    let error = replace_cache_with(&root.0, &catalog, |_, _| {
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "injected failure",
        ))
    })
    .unwrap_err();
    assert!(error.to_string().contains("cache_replace_failed"));
    assert_eq!(fs::read(root.0.join(CACHE_FILE)).unwrap(), before);
    assert_eq!(fs::read_dir(&root.0).unwrap().count(), 1);
    let mut invalid = catalog.clone();
    invalid.entries[0].repo_url = "https://evil.example".into();
    assert!(replace_cache(&root.0, &invalid).is_err());
    assert_eq!(fs::read(root.0.join(CACHE_FILE)).unwrap(), before);
    let readback = get_catalog(&root.0).unwrap();
    assert_eq!(readback.cache_status, "cached");
    assert_eq!(readback.fetched_at, FETCHED_AT);
}
#[tokio::test]
async fn market_concurrent_refresh_fails_before_network_or_writes() {
    let _guard = REFRESH_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let root = Test_root::new();
    assert!(refresh_catalog(&root.0)
        .await
        .unwrap_err()
        .to_string()
        .contains("refresh_busy"));
    assert!(!root.0.exists());
}
#[tokio::test]
#[ignore = "explicit public dsh.market network acceptance; no credentials or plugin execution"]
async fn market_live_dsh_site_refresh_acceptance() {
    let root = Test_root::new();
    let catalog = refresh_catalog(&root.0).await.unwrap();
    assert_eq!(catalog.cache_status, "refreshed");
    assert_eq!(catalog.source_registry, "dsh.market");
    assert_eq!(catalog.registry_url, SITE_FEED_URL);
    assert_eq!(catalog.registry_commit, "");
    assert!(catalog.entry_count > 10_000);
    assert!(catalog
        .entries
        .iter()
        .any(|entry| entry.source_commit.is_none()));
    let cached = get_catalog(&root.0).unwrap();
    assert_eq!(cached.registry_commit, catalog.registry_commit);
    assert_eq!(cached.entry_count, catalog.entry_count);
}
