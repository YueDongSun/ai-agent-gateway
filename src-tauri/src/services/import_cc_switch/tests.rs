use super::*;
use crate::database::Database;
use rusqlite::Connection;
use std::sync::Arc;

/// Full cc-switch `providers` table schema (mirrors the real DB the
/// importer reads). Every fake DB in these tests uses this so the
/// expanded SELECT (all metadata columns) succeeds.
const FAKE_CC_SWITCH_SCHEMA: &str = "CREATE TABLE providers (
    id TEXT NOT NULL,
    app_type TEXT NOT NULL,
    name TEXT NOT NULL,
    settings_config TEXT NOT NULL,
    website_url TEXT,
    category TEXT,
    created_at INTEGER,
    sort_index INTEGER,
    notes TEXT,
    icon TEXT,
    icon_color TEXT,
    meta TEXT NOT NULL DEFAULT '{}',
    is_current BOOLEAN NOT NULL DEFAULT 0,
    in_failover_queue BOOLEAN NOT NULL DEFAULT 0,
    PRIMARY KEY (id, app_type)
)";

fn write_fake_cc_switch_db(path: &std::path::Path) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(FAKE_CC_SWITCH_SCHEMA).unwrap();
    // Two providers: one will be skipped (claude_code already in AAG),
    // one will be imported.
    conn.execute(
        "INSERT INTO providers (id, app_type, name, settings_config, is_current, in_failover_queue) \
         VALUES ('existing-claude', 'claude_code', 'Pre-existing', '{\"env\":{}}', 0, 0)",
        [],
    )
    .unwrap();
    // Use cc-switch's native app_type ids ("claude" not "claude_code")
    // so the mapping code path is exercised. This codex provider has a
    // valid key + base_url so it survives the no-key filter and imports.
    conn.execute(
        "INSERT INTO providers (id, app_type, name, settings_config, is_current, in_failover_queue) \
         VALUES ('imported-codex', 'codex', 'Imported Codex', \
          '{\"auth\":{\"OPENAI_API_KEY\":\"sk-fake\"},\"config\":\"base_url = \\\"https://api.fake.com\\\"\\nmodel=\\\"gpt-5\\\"\"}', 1, 0)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO providers (id, app_type, name, settings_config, is_current, in_failover_queue) \
         VALUES ('imported-grok', 'grokbuild', 'Imported Grok', '{}', 0, 1)",
        [],
    )
    .unwrap();
}
#[test]
fn opencode_mapping_preserves_identity_and_extracts_provider_options() {
    assert_eq!(map_app_type("opencode"), "opencode");
    let settings = serde_json::json!({
        "npm": "@ai-sdk/openai-compatible",
        "options": {"baseURL": "https://example.invalid/v1", "apiKey": "sk-private"},
        "models": {"model-a": {"name": "Model A"}}
    });
    assert_eq!(
        extract_upstream_from_settings("opencode", &settings, None),
        Some((
            "https://example.invalid/v1".into(),
            "sk-private".into(),
            "openai".into()
        ))
    );
    assert!(
        extract_upstream_from_settings("opencode", &serde_json::json!({"options": []}), None)
            .is_none()
    );
}
#[test]
fn opencode_extra_headers_are_extracted_and_opencode_session_is_injected() {
    // Explicit options.headers ride through; a non-opencode.ai host does
    // not gain an injected session header.
    let settings = serde_json::json!({
        "options": {
            "baseURL": "https://relay.example.invalid/v1",
            "headers": {"x-custom": "v1"}
        }
    });
    let headers = extract_extra_headers_from_settings(
        "opencode",
        &settings,
        "https://relay.example.invalid/v1",
    );
    assert_eq!(headers.get("x-custom").map(String::as_str), Some("v1"));
    assert!(!headers.contains_key("x-opencode-session"));

    // An opencode.ai host with no explicit session header gains a stable
    // x-opencode-session automatically.
    let bare = serde_json::json!({
        "options": {"baseURL": "https://opencode.ai/zen/go/v1/messages"}
    });
    let headers = extract_extra_headers_from_settings(
        "opencode",
        &bare,
        "https://opencode.ai/zen/go/v1/messages",
    );
    assert!(
        headers
            .get("x-opencode-session")
            .is_some_and(|s| s.starts_with("aag-")),
        "opencode.ai host must gain a session header: {headers:?}"
    );
}
#[test]
fn claude_custom_headers_json_is_extracted() {
    let settings = serde_json::json!({
        "env": {
            "ANTHROPIC_CUSTOM_HEADERS": "{\"x-rag\": \"on\", \"x-tenant\": \"t1\"}"
        }
    });
    let headers =
        extract_extra_headers_from_settings("claude_code", &settings, "https://relay.example");
    assert_eq!(headers.get("x-rag").map(String::as_str), Some("on"));
    assert_eq!(headers.get("x-tenant").map(String::as_str), Some("t1"));
}
#[test]
fn missing_db_is_noop() {
    let aag = Database::memory().unwrap();
    let report = import_from_db(
        &aag,
        std::path::Path::new("/tmp/definitely-does-not-exist-aag-test.db"),
    )
    .unwrap();
    assert_eq!(report.total_in_source, 0);
    assert!(report.imported.is_empty());
    assert!(report.skipped_existing.is_empty());
}

#[test]
fn missing_providers_table_errors() {
    let aag = Database::memory().unwrap();
    let tmp = std::env::temp_dir().join(format!("aag_ccswitch_noprov_{}.db", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    // Create a SQLite file with only a different table.
    let conn = Connection::open(&tmp).unwrap();
    conn.execute_batch("CREATE TABLE something_else (x INTEGER)")
        .unwrap();
    drop(conn);

    let err = import_from_db(&aag, &tmp).unwrap_err();
    assert!(
        err.to_string().contains("no `providers` table"),
        "expected schema probe error, got: {err}"
    );
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn import_inserts_missing_and_skips_existing() {
    let aag = Database::memory().unwrap();
    // Seed AAG with one provider that overlaps the source.
    aag.save_provider(
        "claude_code",
        &Provider::new(
            "existing-claude".into(),
            "Pre-existing".into(),
            serde_json::json!({"env": {}}),
        ),
    )
    .unwrap();

    let tmp = std::env::temp_dir().join(format!("aag_ccswitch_real_{}.db", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    write_fake_cc_switch_db(&tmp);

    let report = import_from_db(&aag, &tmp).unwrap();
    assert_eq!(report.total_in_source, 3);
    // existing-claude was already in AAG → skipped.
    assert_eq!(report.skipped_existing.len(), 1);
    assert_eq!(report.skipped_existing[0].id, "existing-claude");
    // imported-codex + imported-grok → imported.
    assert_eq!(report.imported.len(), 2);
    let ids: Vec<&str> = report.imported.iter().map(|r| r.id.as_str()).collect();
    assert!(ids.contains(&"imported-codex"));
    assert!(ids.contains(&"imported-grok"));

    // Verify AAG now has the rows.
    let codex = aag
        .get_provider_by_id("imported-codex", "codex")
        .unwrap()
        .expect("imported-codex should exist");
    assert_eq!(codex.name, "Imported Codex");
    assert!(
        codex.settings_config["config"]
            .as_str()
            .unwrap_or("")
            .contains("gpt-5"),
        "settings_config should be preserved verbatim"
    );

    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn auto_sync_imports_claude_rows_and_skips_other_app_types() {
    // Regression: sync_claude_code_from_cc_switch used to filter rows on
    // cc-switch's native `claude` app_type, but read_cc_switch_providers
    // already maps rows to AAG ids — the filter matched nothing and
    // auto-sync silently imported zero providers.
    let aag = Database::memory().unwrap();
    let tmp = std::env::temp_dir().join(format!("aag_ccswitch_autosync_{}.db", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let conn = Connection::open(&tmp).unwrap();
    conn.execute_batch(FAKE_CC_SWITCH_SCHEMA).unwrap();
    conn.execute(
        "INSERT INTO providers (id, app_type, name, settings_config, in_failover_queue) VALUES \
         ('glm-auto', 'claude', 'GLM Auto', \
          '{\"env\":{\
             \"ANTHROPIC_BASE_URL\":\"https://open.bigmodel.cn/api/anthropic\",\
             \"ANTHROPIC_AUTH_TOKEN\":\"x\"\
          }}', 1)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO providers (id, app_type, name, settings_config, in_failover_queue) VALUES \
         ('codex-x', 'codex', 'Codex X', \
          '{\"auth\":{\"OPENAI_API_KEY\":\"sk-fake\"},\"config\":\"base_url = \\\"https://api.fake.com\\\"\"}', 0)",
        [],
    )
    .unwrap();
    drop(conn);

    let report = sync_claude_code_from_cc_switch(&aag, &tmp).unwrap();
    assert_eq!(report.imported.len(), 1);
    assert_eq!(report.imported[0].id, "glm-auto");
    assert_eq!(report.imported[0].app_type, "claude_code");
    assert!(aag
        .get_provider_by_id("glm-auto", "claude_code")
        .unwrap()
        .is_some());
    assert!(aag
        .get_provider_by_id("codex-x", "codex")
        .unwrap()
        .is_none());

    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn auto_sync_honours_sync_locked_rows() {
    // A row marked sync_locked in AAG is manual tuning: auto-sync must
    // leave it alone (not overwritten, not re-queued), while a manual
    // one-shot import (respect_sync_locked=false) is unaffected.
    let aag = Database::memory().unwrap();
    let locked = Provider::new(
        "locked-one".into(),
        "Locked".into(),
        serde_json::json!({"env": {
            "ANTHROPIC_BASE_URL": "https://old.invalid",
            "ANTHROPIC_AUTH_TOKEN": "old-key"
        }}),
    );
    aag.save_provider("claude_code", &locked).unwrap();
    // save_provider deliberately inserts sync_locked=0 regardless of the
    // struct field (existing rows and lock bits are never clobbered), so the
    // dedicated toggle command path is what sets the bit.
    aag.set_sync_locked("locked-one", "claude_code", true)
        .unwrap();

    let tmp = std::env::temp_dir().join(format!("aag_ccswitch_synclock_{}.db", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let conn = Connection::open(&tmp).unwrap();
    conn.execute_batch(FAKE_CC_SWITCH_SCHEMA).unwrap();
    conn.execute(
        "INSERT INTO providers (id, app_type, name, settings_config, in_failover_queue) VALUES \
         ('locked-one', 'claude', 'Locked New', \
          '{\"env\":{\
             \"ANTHROPIC_BASE_URL\":\"https://new.invalid\",\
             \"ANTHROPIC_AUTH_TOKEN\":\"new-key\"\
          }}', 1)",
        [],
    )
    .unwrap();
    drop(conn);

    let report = sync_claude_code_from_cc_switch(&aag, &tmp).unwrap();
    assert!(report.imported.is_empty());
    assert!(report.skipped_existing.is_empty());

    let after = aag
        .get_provider_by_id("locked-one", "claude_code")
        .unwrap()
        .expect("locked row still present");
    assert_eq!(
        after.settings_config["env"]["ANTHROPIC_BASE_URL"],
        serde_json::json!("https://old.invalid"),
        "locked row must keep its manually tuned settings"
    );
    assert!(after.sync_locked);

    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn reimporting_is_idempotent() {
    let aag = Database::memory().unwrap();
    let tmp = std::env::temp_dir().join(format!("aag_ccswitch_idem_{}.db", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    write_fake_cc_switch_db(&tmp);

    let r1 = import_from_db(&aag, &tmp).unwrap();
    let r2 = import_from_db(&aag, &tmp).unwrap();
    assert_eq!(r1.imported.len(), 3);
    assert_eq!(r2.imported.len(), 0);
    assert_eq!(r2.skipped_existing.len(), 3);

    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn aag_self_loop_entry_is_skipped() {
    let aag = Database::memory().unwrap();
    // Database::memory runs the seed, which writes a default proxy_config
    // at 127.0.0.1:8989. Any cc-switch entry whose settings_config points
    // there must be dropped before insertion — pulling AAG back into its
    // own queue would route traffic through itself in a loop.
    let tmp = std::env::temp_dir().join(format!("aag_ccswitch_selfloop_{}.db", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let conn = Connection::open(&tmp).unwrap();
    conn.execute_batch(FAKE_CC_SWITCH_SCHEMA).unwrap();
    // AAG itself — the entry cc-switch would create when the user adds
    // AAG as an upstream provider.
    conn.execute(
        "INSERT INTO providers (id, app_type, name, settings_config) VALUES \
         ('aag-self', 'claude', 'AI Agent Gateway', \
          '{\"env\":{\"ANTHROPIC_BASE_URL\":\"http://127.0.0.1:8989\",\"ANTHROPIC_AUTH_TOKEN\":\"\"}}')",
        [],
    )
    .unwrap();
    // A normal upstream — must still import.
    conn.execute(
        "INSERT INTO providers (id, app_type, name, settings_config) VALUES \
         ('real-upstream', 'claude', 'Real Upstream', \
          '{\"env\":{\"ANTHROPIC_BASE_URL\":\"https://api.example.com\",\"ANTHROPIC_AUTH_TOKEN\":\"x\"}}')",
        [],
    )
    .unwrap();
    // A loose 8989 in a non-URL field — must still import (the detector
    // walks all string fields but a base URL is what matters in practice;
    // the only "8989" we drop is one inside a URL-shaped value).
    conn.execute(
        "INSERT INTO providers (id, app_type, name, settings_config) VALUES \
         ('unrelated', 'claude', 'Unrelated', \
          '{\"env\":{\"ANTHROPIC_BASE_URL\":\"https://api.real.com\",\"ANTHROPIC_AUTH_TOKEN\":\"token-8989\"}}')",
        [],
    )
    .unwrap();
    drop(conn);

    let report = import_from_db(&aag, &tmp).unwrap();
    let ids: Vec<&str> = report.imported.iter().map(|r| r.id.as_str()).collect();
    assert!(
        !ids.contains(&"aag-self"),
        "AAG self entry must not be imported, got: {ids:?}"
    );
    assert!(ids.contains(&"real-upstream"));
    assert!(ids.contains(&"unrelated"));
    // Verify AAG did not end up with the self entry.
    assert!(
        aag.get_provider_by_id("aag-self", "claude_code")
            .unwrap()
            .is_none(),
        "aag-self must not have been written to the providers table"
    );
    let _ = std::fs::remove_file(&tmp);
}

/// Each imported Claude provider's `ANTHROPIC_DEFAULT_*_MODEL` env
/// keys should auto-seed failover routes for every official Claude
/// model id. The hop count = number of official models × number of
/// imported Claude providers (when every tier key is present).
#[test]
fn import_seeds_routes_from_anthropic_default_models() {
    let aag = Database::memory().unwrap();
    let tmp = std::env::temp_dir().join(format!("aag_ccswitch_seedrt_{}.db", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let conn = Connection::open(&tmp).unwrap();
    conn.execute_batch(FAKE_CC_SWITCH_SCHEMA).unwrap();
    // A Claude provider with full per-tier mapping. cc-switch's
    // `claude` app_type is mapped to AAG `claude_code`.
    conn.execute(
        "INSERT INTO providers (id, app_type, name, settings_config, in_failover_queue) VALUES \
         ('zhipu', 'claude', 'Zhipu GLM', \
          '{\"env\":{\
             \"ANTHROPIC_BASE_URL\":\"https://open.bigmodel.cn/api/anthropic\",\
             \"ANTHROPIC_AUTH_TOKEN\":\"x\",\
             \"ANTHROPIC_DEFAULT_HAIKU_MODEL\":\"glm-5.2\",\
             \"ANTHROPIC_DEFAULT_SONNET_MODEL\":\"glm-5.2[1M]\",\
             \"ANTHROPIC_DEFAULT_OPUS_MODEL\":\"glm-5.3[1M]\",\
             \"ANTHROPIC_DEFAULT_FABLE_MODEL\":\"glm-5.3\"\
          }}', 1)",
        [],
    )
    .unwrap();
    drop(conn);

    let report = import_from_db(&aag, &tmp).unwrap();
    assert_eq!(report.imported.len(), 1);

    // OFFICIAL_MODELS has 7 entries (fable-5 + opus-4-8/7/6 + sonnet-5/4-6 + haiku-4-5)
    // and each one should now have a route seeded with a single hop
    // pointing at the imported `ccs-zhipu` proxy provider.
    let routes = aag.get_all_routes().unwrap();
    assert_eq!(
        routes.len(),
        crate::proxy::models::OFFICIAL_MODELS.len(),
        "expected one route per official model, got {} routes",
        routes.len()
    );
    for r in &routes {
        assert_eq!(r.hops.len(), 1, "route {} should have 1 hop", r.alias);
        assert_eq!(r.hops[0].provider, "ccs-zhipu");
        let expected = match r.alias.as_str() {
            "claude-haiku-4-5" => "glm-5.2",
            "claude-sonnet-5" | "claude-sonnet-4-6" => "glm-5.2",
            "claude-opus-4-8" | "claude-opus-4-7" | "claude-opus-4-6" => "glm-5.3",
            "claude-fable-5" => "glm-5.3",
            _ => panic!("unexpected alias {}", r.alias),
        };
        assert_eq!(
            r.hops[0].model, expected,
            "wrong model for route {}",
            r.alias
        );
    }

    // imported_routes report field must mirror what we just wrote.
    assert_eq!(
        report.imported_routes.len(),
        crate::proxy::models::OFFICIAL_MODELS.len()
    );
    let _ = std::fs::remove_file(&tmp);
}

/// Codex import: upstream models from `modelCatalog.models[].model` are
/// evenly distributed across the default Codex client models, and
/// providers without an API key (ChatGPT-login / default) are skipped.
#[test]
fn import_codex_distributes_models_and_skips_no_key() {
    let aag = Database::memory().unwrap();
    let tmp = std::env::temp_dir().join(format!("aag_ccswitch_codex_{}.db", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let conn = Connection::open(&tmp).unwrap();
    conn.execute_batch(FAKE_CC_SWITCH_SCHEMA).unwrap();
    // One upstream model → all four client models use it.
    conn.execute(
        "INSERT INTO providers (id, app_type, name, settings_config, sort_index, in_failover_queue) VALUES \
         ('codex-a', 'codex', 'Codex A', \
          '{\"auth\":{\"OPENAI_API_KEY\":\"key-a\"},\
            \"config\":\"base_url = \\\"https://api.a.com\\\"\\nwire_api = \\\"responses\\\"\\n\",\
            \"modelCatalog\":{\"models\":[{\"model\":\"glm-5.2\"}]}}', 0, 1)",
        [],
    )
    .unwrap();
    // Two upstream models → two-and-two (Sol+Terra→m0, Luna+5.5→m1).
    conn.execute(
        "INSERT INTO providers (id, app_type, name, settings_config, sort_index, in_failover_queue) VALUES \
         ('codex-b', 'codex', 'Codex B', \
          '{\"auth\":{\"OPENAI_API_KEY\":\"key-b\"},\
            \"config\":\"base_url = \\\"https://api.b.com/anthropic\\\"\\n\",\
            \"modelCatalog\":{\"models\":[{\"model\":\"m0\"},{\"model\":\"m1\"}]}}', 1, 1)",
        [],
    )
    .unwrap();
    // No API key (ChatGPT-login) → must be skipped entirely.
    conn.execute(
        "INSERT INTO providers (id, app_type, name, settings_config, sort_index, in_failover_queue) VALUES \
         ('codex-nokey', 'codex', 'Codex NoKey', \
          '{\"auth\":{\"OPENAI_API_KEY\":null},\
            \"config\":\"base_url = \\\"https://api.nokey.com\\\"\\n\",\
            \"modelCatalog\":{\"models\":[{\"model\":\"x\"}]}}', 2, 1)",
        [],
    )
    .unwrap();
    drop(conn);

    let report = import_from_db(&aag, &tmp).unwrap();
    assert_eq!(
        report.imported.len(),
        2,
        "two codex providers with keys import"
    );
    assert_eq!(report.skipped_no_key.len(), 1, "no-key provider is skipped");
    assert_eq!(report.skipped_no_key[0].id, "codex-nokey");
    assert!(
        aag.get_provider_by_id("codex-nokey", "codex")
            .unwrap()
            .is_none(),
        "no-key codex provider must not be stored"
    );

    // Client-model → upstream mapping per the even-distribution rule
    // (two upstream models → Sol+Luna→m0, Terra+5.5→m1).
    let expected: &[(&str, &[&str])] = &[
        ("codex-5-6-sol", &["glm-5.2", "m0"]),
        ("codex-5-6-luna", &["glm-5.2", "m0"]),
        ("codex-5-6-terra", &["glm-5.2", "m1"]),
        ("codex-5-5", &["glm-5.2", "m1"]),
    ];
    for (alias, models) in expected {
        let route = aag
            .get_route(alias)
            .unwrap()
            .unwrap_or_else(|| panic!("route {} missing", alias));
        let hops: Vec<(String, String)> = route
            .hops
            .iter()
            .map(|h| (h.provider.clone(), h.model.clone()))
            .collect();
        assert_eq!(
            hops,
            vec![
                ("ccs-codex-a".into(), models[0].to_string()),
                ("ccs-codex-b".into(), models[1].to_string()),
            ],
            "route {alias} hop mapping wrong"
        );
    }

    // Anthropic-compatible base_url → anthropic proxy_provider type.
    let pp_b = aag
        .get_all_proxy_providers()
        .unwrap()
        .into_iter()
        .find(|p| p.id == "ccs-codex-b")
        .expect("ccs-codex-b exists");
    assert_eq!(
        pp_b.provider_type, "anthropic",
        "codex-b has /anthropic base_url"
    );
    let pp_a = aag
        .get_all_proxy_providers()
        .unwrap()
        .into_iter()
        .find(|p| p.id == "ccs-codex-a")
        .expect("ccs-codex-a exists");
    assert_eq!(pp_a.provider_type, "openai");
    assert_eq!(pp_a.api_key, "key-a");

    let _ = std::fs::remove_file(&tmp);
}

/// A re-import where every provider already exists in AAG (so none is
/// newly inserted) must STILL build/repair the failover routes — this is
/// the "already imported before the route builder existed" upgrade path.
#[test]
fn reimport_fills_hops_for_existing_providers() {
    let aag = Database::memory().unwrap();
    let tmp = std::env::temp_dir().join(format!("aag_ccswitch_repair_{}.db", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let conn = Connection::open(&tmp).unwrap();
    conn.execute_batch(FAKE_CC_SWITCH_SCHEMA).unwrap();
    conn.execute(
        "INSERT INTO providers (id, app_type, name, settings_config, in_failover_queue) VALUES \
         ('deepseek', 'claude', 'DeepSeek', \
          '{\"env\":{\
             \"ANTHROPIC_BASE_URL\":\"https://api.deepseek.com/anthropic\",\
             \"ANTHROPIC_AUTH_TOKEN\":\"x\",\
             \"ANTHROPIC_DEFAULT_FABLE_MODEL\":\"deepseek-v4-pro[1M]\"\
          }}', 1)",
        [],
    )
    .unwrap();
    drop(conn);

    // First import builds providers + routes.
    let r1 = import_from_db(&aag, &tmp).unwrap();
    assert_eq!(r1.imported.len(), 1);
    let fable = aag
        .get_route("claude-fable-5")
        .unwrap()
        .expect("fable route");
    assert_eq!(fable.hops[0].provider, "ccs-deepseek");
    // The `[1M]` context marker is stripped — upstream endpoints reject
    // it as not-a-real-model; the Anthropic provider injects the
    // 1M-context beta header instead.
    assert_eq!(fable.hops[0].model, "deepseek-v4-pro");

    // Simulate pre-route-builder state: wipe the fable route, then
    // re-import. Every provider is now a skip, but the route must be
    // rebuilt with the hop model filled from the provider's mapping.
    aag.delete_route("claude-fable-5").expect("delete route");
    let r2 = import_from_db(&aag, &tmp).unwrap();
    assert_eq!(r2.imported.len(), 0, "nothing new to insert on re-import");
    let fable2 = aag
        .get_route("claude-fable-5")
        .unwrap()
        .expect("fable route repaired");
    assert_eq!(fable2.hops[0].provider, "ccs-deepseek");
    assert_eq!(
        fable2.hops[0].model, "deepseek-v4-pro",
        "re-import must fill the hop model from cc-switch's tier mapping ([1M] stripped)"
    );

    let _ = std::fs::remove_file(&tmp);
}

/// If a route already exists for an official model, a re-import
/// must prepend the new provider as the head hop rather than
/// clobbering the existing chain (preserves user-tuned order).
#[test]
fn reimport_prepends_new_provider_to_existing_route() {
    let aag = Database::memory().unwrap();
    // Seed an existing user-tuned route: opus tier hops to provider-A first.
    aag.save_route(&crate::database::dao::routes::Route {
        alias: "claude-opus-4-8".into(),
        description: "user-tuned".into(),
        hops: vec![
            crate::database::dao::routes::RouteHop {
                provider: "existing-a".into(),
                model: "user-model-a".into(),
            },
            crate::database::dao::routes::RouteHop {
                provider: "existing-b".into(),
                model: "user-model-b".into(),
            },
        ],
    })
    .unwrap();

    let tmp = std::env::temp_dir().join(format!(
        "aag_ccswitch_reimport_rt_{}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&tmp);
    let conn = Connection::open(&tmp).unwrap();
    conn.execute_batch(FAKE_CC_SWITCH_SCHEMA).unwrap();
    conn.execute(
        "INSERT INTO providers (id, app_type, name, settings_config, in_failover_queue) VALUES \
         ('newp', 'claude', 'New', \
          '{\"env\":{\"ANTHROPIC_BASE_URL\":\"https://x\",\"ANTHROPIC_AUTH_TOKEN\":\"k\",\
            \"ANTHROPIC_DEFAULT_OPUS_MODEL\":\"newp-opus\"}}', 1)",
        [],
    )
    .unwrap();
    drop(conn);

    let report = import_from_db(&aag, &tmp).unwrap();
    assert_eq!(report.imported.len(), 1);

    // The opus-4-8 route must now have 3 hops with `ccs-newp` first.
    let route = aag.get_route("claude-opus-4-8").unwrap().unwrap();
    assert_eq!(route.hops.len(), 3);
    assert_eq!(route.hops[0].provider, "ccs-newp");
    assert_eq!(route.hops[0].model, "newp-opus");
    assert_eq!(route.hops[1].provider, "existing-a");
    assert_eq!(route.hops[2].provider, "existing-b");

    // Re-importing must be idempotent (head hop unchanged, no extra hops).
    let r2 = import_from_db(&aag, &tmp).unwrap();
    assert_eq!(r2.imported.len(), 0, "nothing should import on re-run");
    let route2 = aag.get_route("claude-opus-4-8").unwrap().unwrap();
    assert_eq!(route2.hops.len(), 3);
    let _ = std::fs::remove_file(&tmp);
}

/// Auto-queue: a NEW row with a usable upstream lands in the failover
/// queue even when cc-switch had it at in_failover_queue=0. Existing
/// rows (re-import) keep their stored bit — the force only applies to
/// new inserts.
#[test]
fn import_autoqueues_usable_upstream_rows() {
    let aag = Database::memory().unwrap();
    let tmp =
        std::env::temp_dir().join(format!("aag_ccswitch_autoqueue_{}.db", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let conn = Connection::open(&tmp).unwrap();
    conn.execute_batch(FAKE_CC_SWITCH_SCHEMA).unwrap();
    // Queue bit = 0 in cc-switch, but the upstream is fully usable →
    // AAG force-queues it so the import needs no manual tuning.
    conn.execute(
        "INSERT INTO providers (id, app_type, name, settings_config, in_failover_queue) VALUES \
         ('relay', 'claude', 'Relay', \
          '{\"env\":{\
             \"ANTHROPIC_BASE_URL\":\"https://relay.example.com\",\
             \"ANTHROPIC_AUTH_TOKEN\":\"k\",\
             \"ANTHROPIC_DEFAULT_SONNET_MODEL\":\"relay-sonnet\"\
          }}', 0)",
        [],
    )
    .unwrap();
    drop(conn);

    let report = import_from_db(&aag, &tmp).unwrap();
    assert_eq!(report.imported.len(), 1);
    assert_eq!(report.queued_providers.len(), 1);
    assert_eq!(report.queued_providers[0].id, "relay");
    let stored = aag
        .get_provider_by_id("relay", "claude_code")
        .unwrap()
        .unwrap();
    assert!(
        stored.in_failover_queue,
        "usable upstream must be force-queued"
    );
    // The forced queue bit feeds the route builder: sonnet-5 carries
    // the relay hop even though cc-switch had the row unqueued.
    let sonnet = aag.get_route("claude-sonnet-5").unwrap().unwrap();
    assert_eq!(sonnet.hops[0].provider, "ccs-relay");
    assert_eq!(sonnet.hops[0].model, "relay-sonnet");
    let _ = std::fs::remove_file(&tmp);
}

/// Pool-row fill: a claude import's ccs-* pool row carries the tier
/// model list and the official-alias model_mapping derived from the
/// row's own ANTHROPIC_DEFAULT_* keys.
#[test]
fn import_fills_claude_pool_models_and_mapping() {
    let aag = Database::memory().unwrap();
    let tmp = std::env::temp_dir().join(format!("aag_ccswitch_poolfill_{}.db", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let conn = Connection::open(&tmp).unwrap();
    conn.execute_batch(FAKE_CC_SWITCH_SCHEMA).unwrap();
    conn.execute(
        "INSERT INTO providers (id, app_type, name, settings_config, in_failover_queue) VALUES \
         ('glm', 'claude', 'GLM', \
          '{\"env\":{\
             \"ANTHROPIC_BASE_URL\":\"https://open.bigmodel.cn/api/anthropic\",\
             \"ANTHROPIC_AUTH_TOKEN\":\"x\",\
             \"ANTHROPIC_DEFAULT_HAIKU_MODEL\":\"glm-5.2\",\
             \"ANTHROPIC_DEFAULT_SONNET_MODEL\":\"glm-5.2\",\
             \"ANTHROPIC_DEFAULT_OPUS_MODEL\":\"glm-5.3\",\
             \"ANTHROPIC_DEFAULT_FABLE_MODEL\":\"glm-5.3\"\
          }}', 1)",
        [],
    )
    .unwrap();
    drop(conn);

    import_from_db(&aag, &tmp).unwrap();
    let pp = aag
        .get_all_proxy_providers()
        .unwrap()
        .into_iter()
        .find(|p| p.id == "ccs-glm")
        .expect("pool row exists");
    // Tier models deduped: glm-5.2 + glm-5.3 only.
    assert_eq!(
        pp.models,
        vec!["glm-5.2".to_string(), "glm-5.3".to_string()]
    );
    assert_eq!(
        pp.model_mapping.get("claude-haiku-4-5").map(String::as_str),
        Some("glm-5.2")
    );
    assert_eq!(
        pp.model_mapping.get("claude-fable-5").map(String::as_str),
        Some("glm-5.3")
    );
    assert_eq!(
        pp.model_mapping
            .get("claude-sonnet-4-6")
            .map(String::as_str),
        Some("glm-5.2")
    );
    let _ = std::fs::remove_file(&tmp);
}

/// Gemini imports: pool row carries [GEMINI_MODEL] + the row's
/// modelMapping verbatim, and each mapping entry seeds a route alias
/// (plus a bare alias route when no mapping exists).
#[test]
fn import_gemini_row_seeds_model_route() {
    let aag = Database::memory().unwrap();
    let tmp = std::env::temp_dir().join(format!("aag_ccswitch_gemini_{}.db", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let conn = Connection::open(&tmp).unwrap();
    conn.execute_batch(FAKE_CC_SWITCH_SCHEMA).unwrap();
    conn.execute(
        "INSERT INTO providers (id, app_type, name, settings_config, in_failover_queue) VALUES \
         ('g1', 'gemini', 'Gemini Relay', \
          '{\"env\":{\
             \"GOOGLE_GEMINI_BASE_URL\":\"https://relay.example.com\",\
             \"GEMINI_API_KEY\":\"k\",\
             \"GEMINI_MODEL\":\"gemini-2.5-pro\"\
          },\
            \"modelMapping\":{\"gemini-2.5-pro\":\"gemini-2.5-pro-preview-06-05\"}}', 1)",
        [],
    )
    .unwrap();
    // A gemini row WITHOUT a GEMINI_MODEL still gets a pool row (empty
    // models — the user fills the model in the pool UI); only the route
    // seeding needs a concrete model.
    conn.execute(
        "INSERT INTO providers (id, app_type, name, settings_config, in_failover_queue) VALUES \
         ('g2', 'gemini', 'Gemini No Model', \
          '{\"env\":{\
             \"GOOGLE_GEMINI_BASE_URL\":\"https://relay2.example.com\",\
             \"GEMINI_API_KEY\":\"k2\"\
          }}', 0)",
        [],
    )
    .unwrap();
    drop(conn);

    let report = import_from_db(&aag, &tmp).unwrap();
    assert_eq!(report.imported.len(), 2);
    let pp = aag
        .get_all_proxy_providers()
        .unwrap()
        .into_iter()
        .find(|p| p.id == "ccs-g1")
        .expect("pool row exists");
    assert_eq!(pp.models, vec!["gemini-2.5-pro".to_string()]);
    let pp2 = aag
        .get_all_proxy_providers()
        .unwrap()
        .into_iter()
        .find(|p| p.id == "ccs-g2")
        .expect("no-model gemini row still gets a pool row");
    assert!(pp2.models.is_empty());
    // Route seeded from the modelMapping entry.
    let route = aag
        .get_route("gemini-2.5-pro")
        .unwrap()
        .expect("gemini route seeded");
    assert_eq!(route.hops[0].provider, "ccs-g1");
    assert_eq!(route.hops[0].model, "gemini-2.5-pro-preview-06-05");
    // The provider row is auto-queued (usable upstream).
    let stored = aag.get_provider_by_id("g1", "gemini").unwrap().unwrap();
    assert!(stored.in_failover_queue);
    let _ = std::fs::remove_file(&tmp);
}

/// The official-host check must be an EXACT host comparison: an
/// unbounded prefix would classify `api.anthropic.com.evil.tld` as
/// official and silently relay client OAuth tokens to the attacker
/// host. A non-default port is not the official endpoint either.
#[test]
fn official_anthropic_host_check_is_exact() {
    for base in [
        "https://api.anthropic.com",
        "https://api.anthropic.com/",
        "https://api.anthropic.com/v1/messages",
        "api.anthropic.com",
        "https://API.Anthropic.Com",
    ] {
        assert!(
            base_host_is_official_anthropic(base),
            "{base:?} should classify as the official Anthropic host"
        );
    }
    for base in [
        "https://api.anthropic.com.evil.tld",
        "https://api.anthropic.com.attacker.example/v1/messages",
        "https://api.anthropic.com:8443",
        "https://api.anthropic.companion.io",
        "https://api.anthropic.com.evil.tld:443",
        "https://relay.anthropic.com",
        "",
        "https://token-plan.cn-beijing.maas.aliyuncs.com/apps/anthropic",
        "not a url",
    ] {
        assert!(
            !base_host_is_official_anthropic(base),
            "{base:?} must NOT classify as the official Anthropic host"
        );
    }
}

/// Subscription rows (official api.anthropic.com base + empty key)
/// become anthropic_passthrough pool rows: no key, models = official
/// claude aliases, reported in passthrough_providers, and prepended
/// to the official claude-* routes.
#[test]
fn import_subscription_row_is_excluded_without_config_remnants() {
    let aag = Database::memory().unwrap();
    let tmp = std::env::temp_dir().join(format!("aag_ccswitch_sub_{}.db", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let conn = Connection::open(&tmp).unwrap();
    conn.execute_batch(FAKE_CC_SWITCH_SCHEMA).unwrap();
    conn.execute(
        "INSERT INTO providers (id, app_type, name, settings_config, in_failover_queue) VALUES \
         ('sub', 'claude', 'Anthropic Subscription', \
          '{\"env\":{\"ANTHROPIC_BASE_URL\":\"https://api.anthropic.com\",\"ANTHROPIC_AUTH_TOKEN\":\"\"}}', 0)",
        [],
    )
    .unwrap();
    drop(conn);

    let report = import_from_db(&aag, &tmp).unwrap();
    assert!(report.imported.is_empty());
    assert!(report.passthrough_providers.is_empty());
    assert!(report.queued_providers.is_empty());
    assert!(aag
        .get_provider_by_id("sub", "claude_code")
        .unwrap()
        .is_none());
    assert!(aag.get_proxy_provider("ccs-sub").unwrap().is_none());
    assert!(aag
        .get_all_routes()
        .unwrap()
        .iter()
        .all(|route| route.hops.iter().all(|hop| hop.provider != "ccs-sub")));
    let _ = std::fs::remove_file(&tmp);
}

/// Static rows keep the historic path: a real key on a non-anthropic
/// base stays a normal anthropic pool row and is never reported as
/// passthrough.
#[test]
fn import_static_rows_unaffected_by_passthrough() {
    let aag = Database::memory().unwrap();
    let tmp = std::env::temp_dir().join(format!("aag_ccswitch_static_{}.db", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let conn = Connection::open(&tmp).unwrap();
    conn.execute_batch(FAKE_CC_SWITCH_SCHEMA).unwrap();
    conn.execute(
        "INSERT INTO providers (id, app_type, name, settings_config, in_failover_queue) VALUES \
         ('static', 'claude', 'Static Relay', \
          '{\"env\":{\
             \"ANTHROPIC_BASE_URL\":\"https://relay.example.com/api/anthropic\",\
             \"ANTHROPIC_AUTH_TOKEN\":\"real-key\",\
             \"ANTHROPIC_DEFAULT_SONNET_MODEL\":\"relay-sonnet\"\
          }}', 0)",
        [],
    )
    .unwrap();
    drop(conn);

    let report = import_from_db(&aag, &tmp).unwrap();
    assert_eq!(report.imported.len(), 1);
    assert!(report.passthrough_providers.is_empty());
    let pp = aag
        .get_all_proxy_providers()
        .unwrap()
        .into_iter()
        .find(|p| p.id == "ccs-static")
        .expect("pool row exists");
    assert_eq!(pp.provider_type, "anthropic");
    assert_eq!(pp.api_key, "real-key");
    assert_eq!(
        pp.model_mapping.get("claude-sonnet-5").map(String::as_str),
        Some("relay-sonnet")
    );
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn malformed_source_row_is_reported() {
    let aag = Database::memory().unwrap();
    let tmp =
        std::env::temp_dir().join(format!("aag_ccswitch_malformed_{}.db", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let conn = Connection::open(&tmp).unwrap();
    conn.execute_batch(
        "CREATE TABLE providers (
            id BLOB,
            app_type TEXT,
            name TEXT,
            settings_config BLOB,
            website_url TEXT,
            category TEXT,
            created_at INTEGER,
            sort_index INTEGER,
            notes TEXT,
            icon TEXT,
            icon_color TEXT,
            meta TEXT,
            is_current BOOLEAN,
            in_failover_queue BOOLEAN
        )",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO providers (id, app_type, name, settings_config) VALUES (X'00', 'claude', 'Malformed', '{}')",
        [],
    )
    .unwrap();
    drop(conn);

    let err = import_from_db(&aag, &tmp).unwrap_err();
    assert!(
        err.to_string().contains("row decode"),
        "unexpected error: {err}"
    );
    let _ = std::fs::remove_file(&tmp);
}

/// Opt-in end-to-end test against an external cc-switch SQLite DB.
/// Gated behind `AAG_TEST_CCSWITCH_DB` env var so CI / other devs
/// don't accidentally read a live external database. Run with:
///
///   AAG_TEST_CCSWITCH_DB="/absolute/path/to/cc-switch.db" \
///     cargo test --lib -- --test-threads=1 real_cc_switch
///
/// Reports aggregate counts only. Provider names, IDs, model mappings,
/// endpoints, paths, credentials, and response content stay private.
#[test]
fn real_cc_switch_db_smoke_test() {
    let Ok(path) = std::env::var("AAG_TEST_CCSWITCH_DB") else {
        eprintln!(
            "real_cc_switch_db_smoke_test skipped: set \
             AAG_TEST_CCSWITCH_DB=/path/to/cc-switch.db to enable"
        );
        return;
    };
    let path = std::path::PathBuf::from(&path);
    if !path.exists() {
        panic!("AAG_TEST_CCSWITCH_DB points at a non-existent file");
    }
    let aag = Database::memory().unwrap();
    let report = import_from_db(&aag, &path).expect("import_from_db failed");

    eprintln!(
        "\n=== real_cc_switch_db_smoke_test ===\n\
         total in source: {}\n\
         imported:        {}\n\
         skipped:         {}",
        report.total_in_source,
        report.imported.len(),
        report.skipped_existing.len(),
    );
    // The ChatGPT-login codex providers must have been filtered out.
    assert!(
        report
            .skipped_no_key
            .iter()
            .any(|r| r.id == "codex-official"),
        "codex-official (login) must be skipped"
    );
    assert!(
        report.skipped_no_key.iter().any(|r| r.id == "default"),
        "default-chatgpt must be skipped"
    );

    // ── Codex client-model routes: every default codex model got a chain
    // from the imported providers' modelCatalog, and the hops preserve
    // cc-switch queue order (current entry first). ──
    let codex_route_aliases = [
        "codex-5-6-sol",
        "codex-5-6-luna",
        "codex-5-6-terra",
        "codex-5-5",
    ];
    for alias in codex_route_aliases {
        let route = aag
            .get_route(alias)
            .expect("get_route")
            .unwrap_or_else(|| panic!("codex route {} missing after import", alias));
        assert!(
            !route.hops.is_empty(),
            "codex route {} should have hops",
            alias
        );
        assert!(
            route.hops.iter().all(|h| !h.model.is_empty()),
            "codex route {} must have a mapped upstream model per hop",
            alias
        );
    }

    // ── Verify per-tier model mappings for every imported Claude provider ──
    let claude_imported: Vec<&str> = report
        .imported
        .iter()
        .filter(|r| r.app_type == "claude_code")
        .map(|r| r.id.as_str())
        .collect();
    assert!(
        claude_imported.len() >= 4,
        "expected several claude providers from cc-switch, got {}",
        claude_imported.len()
    );
    // Queue members → (id, fable model), for the verbatim-hop check below.
    let mut fable_by_provider: Vec<(String, String)> = Vec::new();
    for id in &claude_imported {
        let prov = aag
            .get_provider_by_id(id, "claude_code")
            .expect("get_provider")
            .expect("provider exists");
        let env = prov
            .settings_config
            .get("env")
            .and_then(|e| e.as_object())
            .cloned()
            .unwrap_or_default();
        let pick = |k: &str| env.get(k).and_then(|v| v.as_str()).unwrap_or("");
        let fable_val = pick("ANTHROPIC_DEFAULT_FABLE_MODEL");
        // Queue members (in_failover_queue=1) must have their fable
        // mapping present verbatim (the [1M] suffix preserved).
        if prov.in_failover_queue && !fable_val.is_empty() {
            fable_by_provider.push((id.to_string(), fable_val.to_string()));
        }
    }

    // ── Every queue member's fable mapping appears in the claude-fable-5
    // route hop (chain head = cc-switch current), with the local `[1M]`
    // context marker stripped (upstream endpoints reject it). ──
    let fable_route = aag
        .get_route("claude-fable-5")
        .expect("get_route")
        .expect("claude-fable-5 route should exist after import");
    for (id, fable_val) in &fable_by_provider {
        let pp_id = format!("ccs-{}", id);
        let hop = fable_route
            .hops
            .iter()
            .find(|h| h.provider == pp_id)
            .expect("route must contain every imported queue provider");
        let expected = crate::proxy::strip_one_m_suffix(fable_val);
        assert_eq!(
            &hop.model, &expected,
            "fable hop must use the source model id without its local context marker"
        );
    }

    // ── Verify routes were built with imported providers in queue order ──
    let routes = aag.get_all_routes().expect("get_all_routes");
    let imported_pp_ids: Vec<String> = claude_imported
        .iter()
        .map(|id| format!("ccs-{}", id))
        .collect();
    for alias in [
        "claude-fable-5",
        "claude-opus-4-8",
        "claude-sonnet-5",
        "claude-haiku-4-5",
    ] {
        let route = aag
            .get_route(alias)
            .expect("get_route")
            .unwrap_or_else(|| panic!("route {} should exist after import", alias));
        // The imported cc-s providers must appear in the SAME order as
        // the cc-switch queue (current first, then sort_index). The
        // mock seed hops trail after the real providers.
        let ccs_in_chain: Vec<&str> = route
            .hops
            .iter()
            .filter(|h| h.provider.starts_with("ccs-"))
            .map(|h| h.provider.as_str())
            .collect();
        assert!(
            !ccs_in_chain.is_empty(),
            "route {} should contain imported cc-s providers",
            alias
        );
        // imported_pp_ids is in SELECT ORDER BY is_current, sort_index
        // order, so the chain must list them in the same relative order.
        let expected: Vec<&str> = imported_pp_ids.iter().map(|s| s.as_str()).collect();
        let filtered_expected: Vec<&str> = expected
            .iter()
            .filter(|id| ccs_in_chain.contains(id))
            .copied()
            .collect();
        assert_eq!(
            ccs_in_chain, filtered_expected,
            "route {} hops must preserve cc-switch queue order",
            alias
        );
    }

    // ── imported_routes report must cover every official model × provider ──
    assert!(
        !report.imported_routes.is_empty(),
        "imported_routes should be populated"
    );
    eprintln!("imported route records: {}", report.imported_routes.len());

    // ── Auto-queue + passthrough classification report integrity: every
    // force-queued row is stored queued, and every passthrough row is a
    // key-less anthropic_passthrough pool entry. ──
    for entry in &report.queued_providers {
        let stored = aag
            .get_provider_by_id(&entry.id, &entry.app_type)
            .expect("get_provider")
            .unwrap_or_else(|| panic!("queued row {} must be stored", entry.id));
        assert!(
            stored.in_failover_queue,
            "auto-queued row {} must be stored queued",
            entry.id
        );
    }
    eprintln!(
        "auto-queued: {} passthrough: {}",
        report.queued_providers.len(),
        report.passthrough_providers.len()
    );
    for entry in &report.passthrough_providers {
        let pp_id = format!("ccs-{}", entry.id);
        let pp = aag
            .get_all_proxy_providers()
            .expect("get_all_proxy_providers")
            .into_iter()
            .find(|p| p.id == pp_id)
            .unwrap_or_else(|| panic!("passthrough pool row {} must exist", pp_id));
        assert_eq!(
            pp.provider_type, "anthropic_passthrough",
            "subscription row {} must import as passthrough",
            entry.id
        );
        assert!(
            pp.api_key.is_empty(),
            "passthrough row {} must carry no key",
            entry.id
        );
    }

    // ── Build a Router from the imported data and verify every official
    // model resolves to a chain of registered providers (no 404 at route
    // resolution time). Deterministic — no upstream requests are sent. ──
    let mut providers: std::collections::HashMap<
        String,
        Arc<dyn crate::proxy::providers::Provider>,
    > = std::collections::HashMap::new();
    for pp in aag
        .get_all_proxy_providers()
        .expect("get_all_proxy_providers")
    {
        match crate::proxy::providers::build_provider(&pp) {
            Ok(p) => {
                providers.insert(pp.id.clone(), Arc::from(p));
            }
            Err(_) => eprintln!("  [warn] one imported provider could not be built"),
        }
    }
    let route_map: std::collections::HashMap<String, Vec<crate::database::dao::routes::RouteHop>> =
        routes
            .iter()
            .map(|r| (r.alias.clone(), r.hops.clone()))
            .collect();
    let router = crate::proxy::router::Router::new(providers.clone(), route_map);
    for m in crate::proxy::models::OFFICIAL_MODELS {
        let hops = router
            .resolve_route_for(crate::proxy::router::SURFACE_CLAUDE, m.id)
            .unwrap_or_else(|| panic!("official model {} must resolve after import", m.id));
        let head = &hops[0];
        assert!(
            providers.contains_key(&head.provider),
            "resolved route head must reference a registered provider"
        );
        assert!(!hops.is_empty(), "resolved chain must not be empty");
    }
}

/// Real end-to-end failover: send one minimal completion through the
/// imported chain and observe which upstream actually serves it. Gated on
/// `AAG_TEST_CCSWITCH_DB` (like the smoke test above). `max_tokens=8` so
/// the real upstream call is effectively free. The router walks hops in
/// queue order; failing upstreams are skipped via the circuit breaker and
/// the next hop is tried. Run with `RUST_LOG=warn` to see per-hop
/// `router: FAIL` lines.
#[tokio::test]
async fn real_cc_switch_failover_request() {
    let Ok(path) = std::env::var("AAG_TEST_CCSWITCH_DB") else {
        eprintln!("real_cc_switch_failover_request skipped: set AAG_TEST_CCSWITCH_DB");
        return;
    };
    let path = std::path::PathBuf::from(&path);
    if !path.exists() {
        panic!("AAG_TEST_CCSWITCH_DB points at a non-existent file");
    }
    let aag = Database::memory().unwrap();
    let _ = import_from_db(&aag, &path).expect("import_from_db failed");

    let mut providers: std::collections::HashMap<
        String,
        Arc<dyn crate::proxy::providers::Provider>,
    > = std::collections::HashMap::new();
    let mut build_error_count = 0usize;
    for pp in aag
        .get_all_proxy_providers()
        .expect("get_all_proxy_providers")
    {
        match crate::proxy::providers::build_provider(&pp) {
            Ok(p) => {
                providers.insert(pp.id.clone(), Arc::from(p));
            }
            Err(_) => build_error_count += 1,
        }
    }
    let route_map: std::collections::HashMap<String, Vec<crate::database::dao::routes::RouteHop>> =
        aag.get_all_routes()
            .expect("get_all_routes")
            .into_iter()
            .map(|r| (r.alias, r.hops))
            .collect();
    let router = crate::proxy::router::Router::new(providers, route_map);

    let req = crate::proxy::canonical::ChatCompletionRequest {
        model: "claude-fable-5".into(),
        messages: vec![crate::proxy::canonical::ChatMessage {
            role: "user".into(),
            content: serde_json::json!("hi"),
            tool_calls: None,
            tool_call_id: None,
            extra: Default::default(),
        }],
        stream: false,
        temperature: None,
        max_tokens: Some(8),
        tools: None,
        tool_choice: None,
        extra: Default::default(),
        source_anthropic_request: None,
        source_gemini_request: None,
        source_responses_request: None,
        request_id: None,
        client_credential: None,
    };
    eprintln!("\n=== real_cc_switch_failover_request ===\n  build_error_count={build_error_count}");
    // Claude chain (SURFACE_CLAUDE /v1/messages): claude-fable-5.
    eprintln!("  checking Claude surface");
    match tokio::time::timeout(
        std::time::Duration::from_secs(180),
        router.complete_for(crate::proxy::router::SURFACE_CLAUDE, "claude_code", &req),
    )
    .await
    {
        Ok(Ok(resp)) => {
            let text = resp.choices[0].message.content.as_deref().unwrap_or("");
            eprintln!("  served response bytes={}", text.len());
        }
        Ok(Err(e)) => {
            eprintln!(
                "  exhausted (status={:?}, retriable={})",
                e.status, e.retriable
            );
        }
        Err(_) => {
            eprintln!("  TIMEOUT after 180s — chain hung");
        }
    }
    // Codex chain (SURFACE_CODEX /v1/responses): codex-5-6-sol maps to
    // each imported provider's modelCatalog via the even-distribution
    // rule. Verifies the direct-route forwarding mapping end-to-end.
    let codex_req = crate::proxy::canonical::ChatCompletionRequest {
        model: "codex-5-6-sol".into(),
        messages: req.messages.clone(),
        stream: false,
        temperature: None,
        max_tokens: Some(8),
        tools: None,
        tool_choice: None,
        extra: Default::default(),
        source_anthropic_request: None,
        source_gemini_request: None,
        source_responses_request: None,
        request_id: None,
        client_credential: None,
    };
    eprintln!("  checking Codex surface");
    match tokio::time::timeout(
        std::time::Duration::from_secs(180),
        router.complete_for(crate::proxy::router::SURFACE_CODEX, "codex", &codex_req),
    )
    .await
    {
        Ok(Ok(resp)) => {
            let text = resp.choices[0].message.content.as_deref().unwrap_or("");
            eprintln!("  served response bytes={}", text.len());
        }
        Ok(Err(e)) => {
            eprintln!(
                "  exhausted (status={:?}, retriable={})",
                e.status, e.retriable
            );
        }
        Err(_) => {
            eprintln!("  TIMEOUT after 180s — codex chain hung");
        }
    }
}

/// Trace the resolved failover chain for a real imported route and then
/// send one real request through it. Gated on `AAG_TEST_CCSWITCH_DB`.
/// This test is for observing chain scheduling, not for assertions.
#[tokio::test]
async fn real_request_chain_trace() {
    let Ok(path) = std::env::var("AAG_TEST_CCSWITCH_DB") else {
        eprintln!("real_request_chain_trace skipped: set AAG_TEST_CCSWITCH_DB");
        return;
    };
    let path = std::path::PathBuf::from(&path);
    if !path.exists() {
        panic!("AAG_TEST_CCSWITCH_DB points at a non-existent file");
    }
    let aag = Database::memory().unwrap();
    let _ = import_from_db(&aag, &path).expect("import_from_db failed");

    let mut providers: std::collections::HashMap<
        String,
        Arc<dyn crate::proxy::providers::Provider>,
    > = std::collections::HashMap::new();
    let proxy_providers = aag
        .get_all_proxy_providers()
        .expect("get_all_proxy_providers");
    for pp in &proxy_providers {
        match crate::proxy::providers::build_provider(pp) {
            Ok(p) => {
                providers.insert(pp.id.clone(), Arc::from(p));
            }
            Err(e) => eprintln!("  [warn] build_provider {} failed: {e}", pp.id),
        }
    }
    let route_map: std::collections::HashMap<String, Vec<crate::database::dao::routes::RouteHop>> =
        aag.get_all_routes()
            .expect("get_all_routes")
            .into_iter()
            .map(|r| (r.alias, r.hops))
            .collect();
    let router = crate::proxy::router::Router::new(providers, route_map);

    let model = "claude-fable-5";
    eprintln!("\n=== real_request_chain_trace ===");
    eprintln!("model: {model}");
    if let Some(chain) = router.resolve_route_for(crate::proxy::router::SURFACE_CLAUDE, model) {
        eprintln!("resolved chain ({} hops):", chain.len());
        for (idx, hop) in chain.iter().enumerate() {
            let enabled = router
                .health()
                .iter()
                .any(|h| h.id == hop.provider && h.enabled);
            let base = proxy_providers
                .iter()
                .find(|p| p.id == hop.provider)
                .map(|p| p.base_url.as_str())
                .unwrap_or("(unknown)");
            eprintln!(
                "  hop {idx}: provider={} model={} enabled={} base_url={}",
                hop.provider, hop.model, enabled, base
            );
        }
    } else {
        eprintln!("no resolved chain for {model}");
    }

    let req = crate::proxy::canonical::ChatCompletionRequest {
        model: model.into(),
        messages: vec![crate::proxy::canonical::ChatMessage {
            role: "user".into(),
            content: serde_json::json!("hi, explain the routing chain in one sentence"),
            tool_calls: None,
            tool_call_id: None,
            extra: Default::default(),
        }],
        stream: false,
        temperature: None,
        max_tokens: Some(64),
        tools: None,
        tool_choice: None,
        extra: Default::default(),
        source_anthropic_request: None,
        source_gemini_request: None,
        source_responses_request: None,
        request_id: Some("trace-001".into()),
        client_credential: None,
    };

    match tokio::time::timeout(
        std::time::Duration::from_secs(120),
        router.complete_for(crate::proxy::router::SURFACE_CLAUDE, "claude_code", &req),
    )
    .await
    {
        Ok(Ok(resp)) => {
            let text = resp.choices[0].message.content.as_deref().unwrap_or("");
            eprintln!("response content ({} bytes): {}", text.len(), text);
            if let Ok(lu) = router.last_used.lock() {
                eprintln!("last_used winner: {:?}", lu.get("claude_code"));
            }
        }
        Ok(Err(e)) => {
            eprintln!(
                "chain exhausted: status={:?} retriable={} code={} stage={} kind={:?}",
                e.status, e.retriable, e.code, e.stage, e.kind
            );
        }
        Err(_) => eprintln!("TIMEOUT after 120s"),
    }
}

#[test]
fn cc_switch_icons_map_to_aag_vocabulary() {
    // Brand-library keys translate to designed glyphs (case-insensitive).
    assert_eq!(
        map_cc_switch_icon(Some("anthropic")).as_deref(),
        Some("spark")
    );
    assert_eq!(
        map_cc_switch_icon(Some("DeepSeek")).as_deref(),
        Some("whale")
    );
    assert_eq!(map_cc_switch_icon(Some("openai")).as_deref(), Some("knot"));
    assert_eq!(map_cc_switch_icon(Some("zhipu")).as_deref(), Some("nodes"));
    assert_eq!(
        map_cc_switch_icon(Some("claudeapi")).as_deref(),
        Some("spark")
    );
    // Literal characters pass through verbatim.
    assert_eq!(map_cc_switch_icon(Some("🚀")).as_deref(), Some("🚀"));
    assert_eq!(map_cc_switch_icon(Some("A")).as_deref(), Some("A"));
    assert_eq!(map_cc_switch_icon(Some("中")).as_deref(), Some("中"));
    // Unknown library keys drop to the monogram fallback instead of
    // rendering multi-character text on the tile.
    assert_eq!(map_cc_switch_icon(Some("aicodemirror")), None);
    assert_eq!(map_cc_switch_icon(Some("azure")), None);
    // Empty / whitespace / absent icons map to nothing.
    assert_eq!(map_cc_switch_icon(None), None);
    assert_eq!(map_cc_switch_icon(Some("  ")), None);
    // Colors pass through trimmed; empty normalises to None.
    assert_eq!(
        map_cc_switch_icon_color(Some(" #00A67E ")).as_deref(),
        Some("#00A67E")
    );
    assert_eq!(map_cc_switch_icon_color(Some("")), None);
    assert_eq!(map_cc_switch_icon_color(None), None);
}

#[test]
fn import_maps_cc_switch_icons_onto_provider_and_pool_rows() {
    let aag = Database::memory().unwrap();
    let tmp = std::env::temp_dir().join(format!("aag_ccswitch_icons_{}.db", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let conn = Connection::open(&tmp).unwrap();
    conn.execute_batch(FAKE_CC_SWITCH_SCHEMA).unwrap();
    // Static-key upstream: brand icon maps to a designed glyph.
    conn.execute(
        "INSERT INTO providers (id, app_type, name, settings_config, icon, icon_color, in_failover_queue) VALUES \
         ('spark-vendor', 'claude', 'Spark Vendor', \
          '{\"env\":{\"ANTHROPIC_BASE_URL\":\"https://relay.invalid\",\"ANTHROPIC_AUTH_TOKEN\":\"x\"}}', \
          'anthropic', '#D4915D', 1)",
        [],
    )
    .unwrap();
    // Unknown library key: icon drops to the monogram, color still rides.
    conn.execute(
        "INSERT INTO providers (id, app_type, name, settings_config, icon, icon_color, in_failover_queue) VALUES \
         ('icon-drop', 'claude', 'No Glyph Vendor', \
          '{\"env\":{\"ANTHROPIC_BASE_URL\":\"https://other.invalid\",\"ANTHROPIC_AUTH_TOKEN\":\"y\"}}', \
          'aicodemirror', '#E4906E', 0)",
        [],
    )
    .unwrap();
    // Subscription passthrough row: the passthrough pool-row arm must map
    // icons too.
    conn.execute(
        "INSERT INTO providers (id, app_type, name, settings_config, icon, icon_color, in_failover_queue) VALUES \
         ('sub-row', 'claude', 'Subscription', \
          '{\"env\":{\"ANTHROPIC_BASE_URL\":\"https://api.anthropic.com\",\"ANTHROPIC_AUTH_TOKEN\":\"\"}}', \
          'kimi', '#6366F1', 0)",
        [],
    )
    .unwrap();
    drop(conn);

    import_from_db(&aag, &tmp).unwrap();

    // Preset rows carry the translated glyph and the source color.
    let brand = aag
        .get_provider_by_id("spark-vendor", "claude_code")
        .unwrap()
        .unwrap();
    assert_eq!(brand.icon.as_deref(), Some("spark"));
    assert_eq!(brand.icon_color.as_deref(), Some("#D4915D"));
    let dropped = aag
        .get_provider_by_id("icon-drop", "claude_code")
        .unwrap()
        .unwrap();
    assert_eq!(dropped.icon, None);
    assert_eq!(dropped.icon_color.as_deref(), Some("#E4906E"));
    // The ccs-* pool rows get the same identity icons on both builder arms.
    let pool = aag.get_proxy_provider("ccs-spark-vendor").unwrap().unwrap();
    assert_eq!(pool.icon.as_deref(), Some("spark"));
    assert_eq!(pool.icon_color.as_deref(), Some("#D4915D"));
    let pool_drop = aag.get_proxy_provider("ccs-icon-drop").unwrap().unwrap();
    assert_eq!(pool_drop.icon, None);
    assert_eq!(pool_drop.icon_color.as_deref(), Some("#E4906E"));
    assert!(aag.get_proxy_provider("ccs-sub-row").unwrap().is_none());
    assert!(aag
        .get_provider_by_id("sub-row", "claude_code")
        .unwrap()
        .is_none());

    let _ = std::fs::remove_file(&tmp);
}
