//! Fixture tests for the cc-switch supplier import preview/apply pair.
//!
//! Everything runs against disposable SQLite fixtures: a synthetic
//! cc-switch source database and an in-memory AAG database. No external network,
//! no real credential files, no reads of a live cc-switch installation.

use super::{apply_import, preview_import};
use crate::database::dao::suppliers::{endpoint_id_for, legacy_supplier_id};
use crate::database::Database;
use crate::services::suppliers::{
    list_suppliers, resolved_supplier_snapshot, save_supplier, supplier_status,
    SupplierEndpointInput, SupplierModelInput, SupplierSaveInput,
};
use rusqlite::Connection;
use std::path::PathBuf;

const FIXTURE_SCHEMA: &str = "CREATE TABLE providers (
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
);
CREATE TABLE provider_endpoints (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    provider_id TEXT NOT NULL,
    app_type TEXT NOT NULL,
    url TEXT NOT NULL,
    added_at INTEGER
)";

#[tokio::test]
async fn imported_nonstandard_endpoint_discovers_models_and_remains_idempotent() {
    use axum::{
        http::{HeaderMap, StatusCode},
        routing::{get, post},
        Json, Router,
    };
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    let catalog_calls = Arc::new(AtomicUsize::new(0));
    let inference_calls = Arc::new(AtomicUsize::new(0));
    let catalogs = catalog_calls.clone();
    let inferences = inference_calls.clone();
    let app = Router::new().route(
        "/gateway/anthropic/v1/models",
        get(move |headers: HeaderMap| {
            let catalogs = catalogs.clone();
            async move {
                catalogs.fetch_add(1, Ordering::SeqCst);
                let accepted = headers.get("authorization").is_some_and(|value| value == "Bearer fixture_key") && !headers.contains_key("x-api-key");
                if accepted {
                    (StatusCode::OK, Json(serde_json::json!({"data":[{"id":"configured-model"},{"id":"new-family-model"}]})))
                } else {
                    (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error":"fixture auth rejected"})))
                }
            }
        }),
    ).route("/gateway/anthropic/v1/messages", post(move |headers: HeaderMap, Json(body): Json<serde_json::Value>| {
        let inferences = inferences.clone();
        async move {
            inferences.fetch_add(1, Ordering::SeqCst);
            let accepted = headers.get("authorization").is_some_and(|value| value == "Bearer fixture_key") && !headers.contains_key("x-api-key");
            if accepted {
                (StatusCode::OK, Json(serde_json::json!({"id":"fixture","type":"message","role":"assistant","model":body["model"],"content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1}})))
            } else {
                (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error":"fixture auth rejected"})))
            }
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!(
        "http://{}/gateway/anthropic/v1/messages",
        listener.local_addr().unwrap()
    );
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let fixture = Fixture::new("discovery");
    let settings = serde_json::json!({"env":{"ANTHROPIC_BASE_URL":base,"ANTHROPIC_AUTH_TOKEN":"fixture_key","ANTHROPIC_MODEL":"configured-model"}}).to_string();
    insert_provider(&fixture, "fixture", "claude", "fixture", &settings, "{}");
    let db = Database::memory().unwrap();
    let preview = preview_import(&db, &fixture.path).unwrap();
    assert!(!super::source_is_materialized(&db, &fixture.path).unwrap());
    // Match the production IPC/operator sequence. The synchronous apply owner
    // only registers prepared rows; screening owns the exact source-path binding.
    let screened = super::screen_import(
        &db,
        &fixture.path,
        &preview.plan_id,
        &["claude_code:fixture".into()],
    )
    .await
    .unwrap();
    assert_eq!(screened.len(), 1);
    assert!(screened[0].eligible);
    apply_import(&db, &fixture.path, &preview.plan_id).unwrap();
    assert!(super::source_is_materialized(&db, &fixture.path).unwrap());
    let supplier_id = preview.plans[0].supplier_id.clone();
    crate::services::suppliers::revalidate_suppliers(&db, &[supplier_id.clone()])
        .await
        .unwrap();
    let view = crate::services::suppliers::get_supplier(&db, &supplier_id)
        .unwrap()
        .supplier
        .unwrap();
    assert!(view.endpoints[0]
        .models
        .iter()
        .any(|model| model.model_id == "new-family-model"));
    assert_eq!(
        view.endpoints[0].discovery_health.as_ref().unwrap().status,
        "catalog_available"
    );
    assert!(
        apply_import(&db, &fixture.path, &preview.plan_id)
            .unwrap()
            .already_applied
    );
    assert_eq!(catalog_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        inference_calls.load(Ordering::SeqCst),
        1,
        "repeated apply does not repeat the completed check"
    );
    let provider_id = db
        .get_supplier_record_snapshot(&supplier_id)
        .unwrap()
        .1
        .unwrap()
        .endpoints[0]
        .proxy_provider_id
        .clone()
        .unwrap();
    let provider = db.get_proxy_provider(&provider_id).unwrap().unwrap();
    let (bound_path, source_ref, _) =
        crate::services::provider_admission::source_for_provider(&db, &provider).unwrap();
    assert_eq!(
        bound_path.canonicalize().unwrap(),
        fixture.path.canonicalize().unwrap()
    );
    assert_eq!(source_ref, "claude_code:fixture");
    assert_eq!(
        crate::services::provider_admission::auth_scheme(&db, &provider).unwrap(),
        crate::proxy::providers::AnthropicAuthScheme::Bearer
    );
    assert_eq!(
        db.provider_admission_record(&provider_id)
            .unwrap()
            .unwrap()
            .probe
            .unwrap()
            .probe_model,
        "configured-model"
    );
    // A same-byte credential moved to the other field changes source identity.
    // It must fail before another catalog request, never fall back to API-key auth.
    let changed = serde_json::json!({"env":{"ANTHROPIC_BASE_URL":base,"ANTHROPIC_API_KEY":"fixture_key","ANTHROPIC_MODEL":"configured-model"}}).to_string();
    fixture.insert("UPDATE providers SET settings_config=?1", &[&changed]);
    assert_eq!(
        crate::services::suppliers::revalidate_suppliers(&db, &[supplier_id])
            .await
            .unwrap_err(),
        "supplier_discovery_partial:provider_subscription_source_changed"
    );
    assert_eq!(catalog_calls.load(Ordering::SeqCst), 1);
    assert_eq!(inference_calls.load(Ordering::SeqCst), 1);
    server.abort();
    let _ = server.await;
}

struct Fixture {
    path: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "aag_supplier_import_{}_{}_{}.db",
            label,
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        let _ = std::fs::remove_file(&path);
        let conn = Connection::open(&path).expect("open fixture");
        conn.execute_batch(FIXTURE_SCHEMA).expect("fixture schema");
        drop(conn);
        Self { path }
    }

    fn insert(&self, sql: &str, params: &[&dyn rusqlite::ToSql]) {
        let conn = Connection::open(&self.path).expect("reopen fixture");
        conn.execute(sql, params).expect("fixture insert");
    }

    fn execute(&self, sql: &str) {
        let conn = Connection::open(&self.path).expect("reopen fixture");
        conn.execute_batch(sql).expect("fixture execute");
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn insert_provider(
    fixture: &Fixture,
    id: &str,
    app_type: &str,
    name: &str,
    settings: &str,
    meta: &str,
) {
    fixture.insert(
        "INSERT INTO providers (id, app_type, name, settings_config, meta)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        &[&id, &app_type, &name, &settings, &meta],
    );
}

fn claude_settings(base: &str, key: &str) -> String {
    format!(
        "{{\"env\":{{\"ANTHROPIC_BASE_URL\":\"{base}\",\"ANTHROPIC_AUTH_TOKEN\":\"{key}\",\
         \"ANTHROPIC_DEFAULT_SONNET_MODEL\":\"vendor-sonnet\",\"ANTHROPIC_DEFAULT_OPUS_MODEL\":\"vendor-opus\"}}}}"
    )
}

fn codex_settings(base: &str, key: &str, wire_api: &str) -> String {
    format!(
        "{{\"auth\":{{\"OPENAI_API_KEY\":\"{key}\"}},\
         \"config\":\"base_url = \\\"{base}\\\"\\nwire_api = \\\"{wire_api}\\\"\\n\",\
         \"modelCatalog\":{{\"models\":[{{\"model\":\"m0\"}},{{\"model\":\"m1\"}}]}}}}"
    )
}

fn supplier_count(db: &Database) -> i64 {
    let conn = db.conn.lock().expect("database lock");
    conn.query_row("SELECT COUNT(*) FROM suppliers", [], |row| row.get(0))
        .expect("count suppliers")
}

fn endpoint_count(db: &Database) -> i64 {
    let conn = db.conn.lock().expect("database lock");
    conn.query_row("SELECT COUNT(*) FROM supplier_endpoints", [], |row| {
        row.get(0)
    })
    .expect("count endpoints")
}

fn generation(db: &Database) -> u64 {
    crate::services::suppliers::supplier_config_generation(db).expect("generation")
}

fn reset_fixture_target() -> Database {
    let db = Database::memory().unwrap();
    db.seed_proxy_if_empty().unwrap();
    save_supplier(&db,serde_json::from_value(serde_json::json!({"id":"old_manual","expectedGeneration":0,"name":"Old manual supplier",
        "baseUrl":"https://old.example.invalid","apiKey":"old_fixture_key","endpoints":[{"protocol":"anthropic","models":[{"modelId":"old-model"}]}]})).unwrap()).unwrap();
    let conn = db.conn.lock().unwrap();
    conn.execute_batch("INSERT INTO providers(id,app_type,name,settings_config) VALUES('old_legacy','codex','Old legacy','{}');
        INSERT INTO provider_endpoints(provider_id,app_type,url) VALUES('old_legacy','codex','https://old.example.invalid');
        INSERT INTO vendors(id,name) VALUES('old_vendor','Old vendor');
        INSERT INTO mcp_servers(id,name,server_config) VALUES('retained_mcp','Retained','{}');
        INSERT INTO sessions(id,app_id,session_id,title) VALUES('retained_session','codex','fixture','Keep history');
        INSERT INTO usage_records(ts,app_id,provider_id,model) VALUES(1,'codex','old_legacy','old-model');
        INSERT INTO supplier_catalog_cache(source,raw_json,fetched_at) VALUES('models_dev','{}',1);").unwrap();
    drop(conn);
    for (key, value) in [
        ("theme", "dark"),
        ("prompt_enhance_en_system", "Keep prompt template"),
        ("proxy_pinned_provider", "old_legacy"),
        ("supplier_discovery_health:old_legacy", "{}"),
        ("supplier_import_setup_pending", "1"),
        ("sync_with_cc_switch_enabled", "1"),
    ] {
        db.set_setting(key, value).unwrap();
    }
    db
}

fn static_reset_source(label: &str) -> Fixture {
    let fixture = Fixture::new(label);
    insert_provider(
        &fixture,
        "new_static",
        "claude",
        "New static",
        &claude_settings("https://new.example.invalid", "new_fixture_key"),
        "{}",
    );
    fixture
}

#[test]
fn atomic_reset_import_removes_all_old_supply_and_preserves_unrelated_data_and_source() {
    let db = reset_fixture_target();
    let source = static_reset_source("reset_success");
    insert_provider(
        &source,
        "excluded_account",
        "codex",
        "Account",
        &codex_settings(
            "https://account.example.invalid",
            "fixture_account_key",
            "responses",
        ),
        "{\"authBinding\":{\"source\":\"managed_account\"}}",
    );
    let source_before = std::fs::read(&source.path).unwrap();
    let config_before = serde_json::to_value(db.get_proxy_config().unwrap()).unwrap();
    let old_generation = generation(&db);
    let before_suppliers = supplier_count(&db);
    let before_routes = db.get_all_routes().unwrap().len();
    let preview = super::preview_reset_import(&db, &source.path).unwrap();
    assert!(preview.plan_id.starts_with("reset_v1:"));
    assert!(preview.plans.iter().all(|entry| entry.change == "create"));
    assert_eq!(generation(&db), old_generation);
    let root =
        crate::config::private_fs::private_temp_dir(&std::env::temp_dir().canonicalize().unwrap())
            .unwrap();
    let backup = root.path().join("before_reset.db");
    let result = super::apply_reset_import(&db, &source.path, &preview.plan_id, &backup).unwrap();
    assert_eq!(result.generation, old_generation + 1);
    assert_eq!(result.removed_rows["suppliers"], before_suppliers as u64);
    assert_eq!(result.removed_rows["providers"], 1);
    assert_eq!(result.removed_rows["routes"], before_routes as u64);
    assert_eq!(result.created_suppliers, 1);
    assert_eq!(result.created_endpoints, 1);
    assert_eq!(result.excluded_oauth_count, 1);
    let suppliers = list_suppliers(&db).unwrap();
    assert_eq!(suppliers.suppliers.len(), 1);
    assert_eq!(suppliers.suppliers[0].source, "cc_switch");
    assert_eq!(suppliers.suppliers[0].name, "New static");
    assert!(db.get_all_routes().unwrap().is_empty());
    assert!(db.get_all_providers("codex").unwrap().is_empty());
    assert!(db.get_all_vendors().unwrap().is_empty());
    assert_eq!(db.get_all_proxy_providers().unwrap().len(), 1);
    for key in [
        "proxy_pinned_provider",
        "supplier_discovery_health:old_legacy",
    ] {
        assert!(db.get_setting(key).unwrap().is_none());
    }
    assert_eq!(
        db.get_setting("supplier_import_setup_pending")
            .unwrap()
            .as_deref(),
        Some("1")
    );
    assert_eq!(
        db.get_setting("provider_seed_disabled").unwrap().as_deref(),
        Some("1")
    );
    assert_eq!(
        db.get_setting("sync_with_cc_switch_enabled")
            .unwrap()
            .as_deref(),
        Some("0")
    );
    assert_eq!(db.get_setting("theme").unwrap().as_deref(), Some("dark"));
    assert_eq!(
        db.get_setting("prompt_enhance_en_system")
            .unwrap()
            .as_deref(),
        Some("Keep prompt template")
    );
    assert_eq!(
        serde_json::to_value(db.get_proxy_config().unwrap()).unwrap(),
        config_before
    );
    for table in [
        "mcp_servers",
        "sessions",
        "usage_records",
        "supplier_catalog_cache",
    ] {
        let count: i64 = db
            .conn
            .lock()
            .unwrap()
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 1);
    }
    assert_eq!(std::fs::read(&source.path).unwrap(), source_before);
    let snapshot =
        Connection::open_with_flags(&backup, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert_eq!(
        snapshot
            .query_row("SELECT COUNT(*) FROM suppliers", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        before_suppliers
    );
    assert_eq!(
        snapshot
            .query_row("SELECT COUNT(*) FROM routes", [], |row| row
                .get::<_, usize>(0))
            .unwrap(),
        before_routes
    );
    let again = root.path().join("must_not_exist.db");
    assert!(super::apply_reset_import(&db, &source.path, &preview.plan_id, &again).is_err());
    assert!(
        !again.exists(),
        "an old reset plan cannot reset the rebuilt state again"
    );
}

#[test]
fn atomic_reset_import_insertion_failure_rolls_back_deletion_and_retains_backup() {
    let db = reset_fixture_target();
    let source = static_reset_source("reset_failure");
    db.conn.lock().unwrap().execute_batch("CREATE TRIGGER refuse_new_import BEFORE INSERT ON supplier_models WHEN NEW.source='cc_switch' BEGIN SELECT RAISE(ABORT,'fixture insert failure'); END;").unwrap();
    let before = serde_json::to_value(list_suppliers(&db).unwrap()).unwrap();
    let before_routes = serde_json::to_value(db.get_all_routes().unwrap()).unwrap();
    let before_providers = db.get_all_proxy_providers().unwrap();
    let preview = super::preview_reset_import(&db, &source.path).unwrap();
    let root =
        crate::config::private_fs::private_temp_dir(&std::env::temp_dir().canonicalize().unwrap())
            .unwrap();
    let backup = root.path().join("rollback.db");
    assert!(super::apply_reset_import(&db, &source.path, &preview.plan_id, &backup).is_err());
    assert!(backup.is_file());
    assert_eq!(
        serde_json::to_value(list_suppliers(&db).unwrap()).unwrap(),
        before
    );
    assert_eq!(
        serde_json::to_value(db.get_all_routes().unwrap()).unwrap(),
        before_routes
    );
    assert_eq!(db.get_all_proxy_providers().unwrap(), before_providers);
    assert_eq!(
        db.get_setting("supplier_import_setup_pending")
            .unwrap()
            .as_deref(),
        Some("1")
    );
    assert_eq!(
        db.get_setting("supplier_discovery_health:old_legacy")
            .unwrap()
            .as_deref(),
        Some("{}")
    );
    assert_eq!(
        db.get_setting("sync_with_cc_switch_enabled")
            .unwrap()
            .as_deref(),
        Some("1")
    );
    assert!(db.get_setting("provider_seed_disabled").unwrap().is_none());
}

#[test]
fn reset_import_refuses_empty_sources_and_stale_source_or_legacy_target_binding() {
    let db = reset_fixture_target();
    let empty = Fixture::new("reset_empty");
    assert!(super::preview_reset_import(&db, &empty.path).is_err());
    insert_provider(&empty, "unusable", "claude", "Unusable", "{}", "{}");
    assert!(super::preview_reset_import(&db, &empty.path).is_err());
    let root =
        crate::config::private_fs::private_temp_dir(&std::env::temp_dir().canonicalize().unwrap())
            .unwrap();
    let backup = root.path().join("must_not_exist.db");
    let source = static_reset_source("reset_binding");
    let ordinary = preview_import(&db, &source.path).unwrap();
    assert!(super::apply_reset_import(&db, &source.path, &ordinary.plan_id, &backup).is_err());
    let preview = super::preview_reset_import(&db, &source.path).unwrap();
    let old_generation = generation(&db);
    db.save_route(&crate::database::dao::routes::Route {
        alias: "concurrent_legacy_edit".into(),
        description: String::new(),
        hops: vec![],
    })
    .unwrap();
    assert_eq!(
        generation(&db),
        old_generation,
        "legacy route edits do not bump supplier generation"
    );
    assert!(super::apply_reset_import(&db, &source.path, &preview.plan_id, &backup).is_err());
    let preview = super::preview_reset_import(&db, &source.path).unwrap();
    source.execute("UPDATE providers SET name='Source changed after approval'");
    assert!(super::apply_reset_import(&db, &source.path, &preview.plan_id, &backup).is_err());
    assert!(!backup.exists());
    assert!(db.get_proxy_provider("mock-primary").unwrap().is_some());
}

#[test]
fn metadata_refresh_preserves_model_owners_capabilities_routes_and_unchanged_entries() {
    let db = Database::memory().unwrap();
    let fixture = Fixture::new("preserve_models");
    for (id, name, key) in [
        ("changed", "Changed", "fixture_a"),
        ("untouched", "Untouched", "fixture_b"),
    ] {
        insert_provider(
            &fixture,
            id,
            "claude",
            name,
            &claude_settings("https://fixture.example.invalid", key),
            "{}",
        );
    }
    let preview = preview_import(&db, &fixture.path).unwrap();
    apply_import(&db, &fixture.path, &preview.plan_id).unwrap();
    let first_id = preview
        .plans
        .iter()
        .find(|entry| entry.source_id == "changed")
        .unwrap()
        .supplier_id
        .clone();
    let other_id = preview
        .plans
        .iter()
        .find(|entry| entry.source_id == "untouched")
        .unwrap()
        .supplier_id
        .clone();
    crate::services::suppliers::update_supplier_models(&db,serde_json::from_value(serde_json::json!({
        "supplierId":first_id,"protocol":"anthropic","expectedGeneration":generation(&db),"models":[
            {"modelId":"vendor-sonnet","source":"manual","enabled":false,"supportsTools":true,"contextWindow":128000,"maxOutputTokens":32000,"tierOverride":3,"displayName":"Manual override"},
            {"modelId":"vendor-opus","source":"cc_switch","enabled":true,"supportsImages":true},
            {"modelId":"discovered","source":"provider_catalog","enabled":true,"supportsTools":true,"contextWindow":64000},
            {"modelId":"unlisted","source":"provider_catalog_unlisted","enabled":false},
            {"modelId":"manual-extra","source":"manual","enabled":true}
        ]
    })).unwrap()).unwrap();
    save_supplier(&db,serde_json::from_value(serde_json::json!({
        "id":first_id,"expectedGeneration":generation(&db),"name":"Changed","enabled":false,"priority":37,
        "endpoints":[{"protocol":"anthropic","enabled":false,"priority":29}]
    })).unwrap()).unwrap();
    let (_, before_record) = db.get_supplier_record_snapshot(&first_id).unwrap();
    let before_record = before_record.unwrap();
    let endpoint_id = before_record.endpoints[0].id.clone();
    let provider_id = before_record.endpoints[0]
        .proxy_provider_id
        .clone()
        .unwrap();
    let models_before = serde_json::to_value(&before_record.endpoints[0].models).unwrap();
    db.save_route(&crate::database::dao::routes::Route {
        alias: "preserved-route".into(),
        description: "Keep exact route".into(),
        hops: vec![crate::database::dao::routes::RouteHop {
            provider: provider_id.clone(),
            model: "discovered".into(),
        }],
    })
    .unwrap();
    let routes_before = serde_json::to_value(db.get_all_routes().unwrap()).unwrap();
    // Any replay of the unchanged endpoint aborts, independently proving that
    // import selected only the changed entry rather than restoring its contents.
    let other_endpoint = endpoint_id_for(&other_id, "anthropic");
    db.conn.lock().unwrap().execute_batch(&format!("CREATE TRIGGER reject_unchanged_endpoint BEFORE UPDATE ON supplier_endpoints WHEN OLD.id='{other_endpoint}' BEGIN SELECT RAISE(ABORT,'unchanged endpoint replayed'); END;")).unwrap();
    fixture.execute("UPDATE providers SET name='Changed display',icon='openai' WHERE id='changed'");
    let updated = preview_import(&db, &fixture.path).unwrap();
    assert_eq!(
        updated
            .plans
            .iter()
            .find(|entry| entry.source_id == "changed")
            .unwrap()
            .change,
        "refresh"
    );
    assert_eq!(
        updated
            .plans
            .iter()
            .find(|entry| entry.source_id == "untouched")
            .unwrap()
            .change,
        "unchanged"
    );
    let receipt = apply_import(&db, &fixture.path, &updated.plan_id).unwrap();
    assert_eq!(receipt.refreshed_count, 1);
    let (_, after_record) = db.get_supplier_record_snapshot(&first_id).unwrap();
    let after_record = after_record.unwrap();
    assert_eq!(after_record.name, "Changed display");
    assert_eq!(after_record.icon.as_deref(), Some("knot"));
    assert!(!after_record.enabled);
    assert_eq!(after_record.priority, 37);
    assert!(!after_record.endpoints[0].enabled);
    assert_eq!(after_record.endpoints[0].priority, 29);
    assert_eq!(after_record.endpoints[0].id, endpoint_id);
    assert_eq!(
        serde_json::to_value(&after_record.endpoints[0].models).unwrap(),
        models_before
    );
    assert_eq!(
        serde_json::to_value(db.get_all_routes().unwrap()).unwrap(),
        routes_before
    );
    let projected = db
        .get_all_proxy_providers()
        .unwrap()
        .into_iter()
        .find(|provider| provider.id == provider_id)
        .unwrap();
    assert!(projected.models.contains(&"discovered".to_string()));
    assert!(projected.models.contains(&"manual-extra".to_string()));
    assert!(!projected.models.contains(&"vendor-sonnet".to_string()));
    assert!(!projected.models.contains(&"unlisted".to_string()));
    assert!(!projected.enabled);
    assert_eq!(projected.weight, 135);
    assert!(
        apply_import(&db, &fixture.path, &updated.plan_id)
            .unwrap()
            .already_applied
    );
    assert!(preview_import(&db, &fixture.path)
        .unwrap()
        .plans
        .iter()
        .all(|entry| entry.change == "unchanged"));
}

#[test]
fn metadata_refresh_preserves_independent_supplier_endpoint_and_variant_admission() {
    for (supplier_enabled, endpoint_enabled, variant_enabled) in [
        (false, true, true),
        (true, false, true),
        (true, true, false),
    ] {
        let db = Database::memory().unwrap();
        let fixture = Fixture::new("preserve_admission");
        for (id, url) in [
            ("primary", "https://open.bigmodel.cn/first"),
            ("alternate", "https://open.bigmodel.cn/second"),
        ] {
            insert_provider(
                &fixture,
                id,
                "claude",
                "GLM",
                &claude_settings(url, "fixture_same"),
                "{}",
            );
        }
        let preview = preview_import(&db, &fixture.path).unwrap();
        apply_import(&db, &fixture.path, &preview.plan_id).unwrap();
        let supplier_id = preview.plans[0].supplier_id.clone();
        let (_, before) = db.get_supplier_record_snapshot(&supplier_id).unwrap();
        let before = before.unwrap();
        let endpoint = &before.endpoints[0];
        assert_eq!(endpoint.alternatives.len(), 1);
        let mut variants = endpoint.alternatives.clone();
        variants[0].enabled = variant_enabled;
        variants[0].priority = 13;
        let mut discovered = variants[0].models[0].clone();
        discovered.model_id = "variant-discovered".into();
        discovered.id =
            crate::database::dao::suppliers::model_id_for(&variants[0].id, &discovered.model_id);
        discovered.source = "provider_catalog".into();
        discovered.supports_tools = Some(true);
        discovered.sort_index = 5;
        variants[0].models.push(discovered);
        let variant_models = serde_json::to_value(&variants[0].models).unwrap();
        let variant_provider = variants[0].legacy_proxy_provider_id.clone().unwrap();
        db.conn
            .lock()
            .unwrap()
            .execute(
                "UPDATE supplier_endpoints SET alternatives_json=?1 WHERE id=?2",
                rusqlite::params![serde_json::to_string(&variants).unwrap(), endpoint.id],
            )
            .unwrap();
        save_supplier(&db,serde_json::from_value(serde_json::json!({"id":supplier_id,"expectedGeneration":generation(&db),"name":"GLM",
            "enabled":supplier_enabled,"priority":37,"endpoints":[{"protocol":"anthropic","enabled":endpoint_enabled,"priority":29}]})).unwrap()).unwrap();
        fixture.execute("UPDATE providers SET icon='openai'");
        let updated = preview_import(&db, &fixture.path).unwrap();
        apply_import(&db, &fixture.path, &updated.plan_id).unwrap();
        let (_, after) = db.get_supplier_record_snapshot(&supplier_id).unwrap();
        let after = after.unwrap();
        assert_eq!((after.enabled, after.priority), (supplier_enabled, 37));
        let endpoint = &after.endpoints[0];
        assert_eq!(
            (endpoint.enabled, endpoint.priority),
            (endpoint_enabled, 29)
        );
        assert_eq!(
            (
                endpoint.alternatives[0].enabled,
                endpoint.alternatives[0].priority
            ),
            (variant_enabled, 13)
        );
        assert_eq!(
            serde_json::to_value(&endpoint.alternatives[0].models).unwrap(),
            variant_models
        );
        let primary = db
            .get_proxy_provider(endpoint.proxy_provider_id.as_ref().unwrap())
            .unwrap()
            .unwrap();
        let alternate = db.get_proxy_provider(&variant_provider).unwrap().unwrap();
        assert_eq!(primary.enabled, supplier_enabled && endpoint_enabled);
        assert_eq!(alternate.enabled, supplier_enabled && variant_enabled);
        assert_eq!(primary.weight, 135);
        assert_eq!(alternate.weight, 151);
        assert!(alternate.models.contains(&"variant-discovered".into()));
        assert!(
            apply_import(&db, &fixture.path, &updated.plan_id)
                .unwrap()
                .already_applied
        );
    }
}

#[test]
fn changed_endpoint_credentials_are_refused_atomically_before_reusing_catalog_claims() {
    let db = Database::memory().unwrap();
    let fixture = Fixture::new("changed_connection");
    insert_provider(
        &fixture,
        "source",
        "claude",
        "Connection",
        &claude_settings("https://fixture.example.invalid", "old_fixture_key"),
        "{}",
    );
    let preview = preview_import(&db, &fixture.path).unwrap();
    apply_import(&db, &fixture.path, &preview.plan_id).unwrap();
    let supplier_id = preview.plans[0].supplier_id.clone();
    let (_, record) = db.get_supplier_record_snapshot(&supplier_id).unwrap();
    let record = record.unwrap();
    let provider_id = record.endpoints[0].proxy_provider_id.clone().unwrap();
    let health_key = format!("supplier_discovery_health:{provider_id}");
    let health = "{\"status\":\"catalog_available\",\"checkedAt\":1,\"lastSuccessAt\":1,\"latencyMs\":1,\"consecutiveFailures\":0,\"error\":null,\"listedModelIds\":[\"vendor-sonnet\"],\"inferenceStatus\":\"unverified\"}";
    db.set_setting(&health_key, health).unwrap();
    let before = serde_json::to_value(list_suppliers(&db).unwrap()).unwrap();
    insert_provider(
        &fixture,
        "new_source",
        "claude",
        "New source",
        &claude_settings("https://new.example.invalid", "new_fixture_key"),
        "{}",
    );
    for settings in [
        claude_settings("https://fixture.example.invalid", "changed_fixture_key"),
        claude_settings("https://changed.example.invalid", "old_fixture_key"),
    ] {
        fixture.insert(
            "UPDATE providers SET settings_config=?1 WHERE id='source'",
            &[&settings],
        );
        let changed = preview_import(&db, &fixture.path).unwrap();
        let error = apply_import(&db, &fixture.path, &changed.plan_id).unwrap_err();
        assert!(error
            .to_string()
            .contains("supplier_import_endpoint_change_requires_review"));
        assert_eq!(
            serde_json::to_value(list_suppliers(&db).unwrap()).unwrap(),
            before
        );
        assert_eq!(
            db.get_setting(&health_key).unwrap().as_deref(),
            Some(health)
        );
        assert_eq!(
            supplier_count(&db),
            1,
            "refusal must also defer unrelated new imports atomically"
        );
    }
}

/// Cross-app rows sharing one cc-switch id must never collide: identity
/// includes the app_type.
#[test]
fn crossapp_same_source_id_yields_distinct_suppliers() {
    let db = Database::memory().expect("memory db");
    let fixture = Fixture::new("crossapp");
    insert_provider(
        &fixture,
        "shared-id",
        "claude",
        "Claude Side",
        &claude_settings("https://relay-a.example.invalid", "sk-claude"),
        "{}",
    );
    insert_provider(
        &fixture,
        "shared-id",
        "codex",
        "Codex Side",
        &codex_settings("https://relay-b.example.invalid", "sk-codex", "responses"),
        "{}",
    );

    let preview = preview_import(&db, &fixture.path).expect("preview");
    assert_eq!(preview.plans.len(), 2);
    let claude_entry = preview
        .plans
        .iter()
        .find(|entry| entry.source_app_type == "claude_code")
        .expect("claude entry");
    let codex_entry = preview
        .plans
        .iter()
        .find(|entry| entry.source_app_type == "codex")
        .expect("codex entry");
    assert_ne!(
        claude_entry.supplier_id, codex_entry.supplier_id,
        "app_type must be part of the identity"
    );
    assert_eq!(
        claude_entry.supplier_id,
        legacy_supplier_id("cc_switch", "claude_code:shared-id"),
        "identity is stable and app-scoped"
    );
    assert_eq!(
        codex_entry.supplier_id,
        legacy_supplier_id("cc_switch", "codex:shared-id")
    );

    let applied = apply_import(&db, &fixture.path, &preview.plan_id).expect("apply");
    assert_eq!(applied.created_count, 2);
    assert_eq!(supplier_count(&db), 2);
}

/// Same display name with different keys must not merge records or keys.
#[test]
fn same_name_with_different_keys_stays_independent() {
    let db = Database::memory().expect("memory db");
    let fixture = Fixture::new("samename");
    insert_provider(
        &fixture,
        "row-one",
        "claude",
        "Twin Relay",
        &claude_settings("https://one.example.invalid", "sk-one"),
        "{}",
    );
    insert_provider(
        &fixture,
        "row-two",
        "claude",
        "Twin Relay",
        &claude_settings("https://two.example.invalid", "sk-two"),
        "{}",
    );

    let preview = preview_import(&db, &fixture.path).expect("preview");
    assert_eq!(preview.plans.len(), 2);
    apply_import(&db, &fixture.path, &preview.plan_id).expect("apply");

    let snapshot = resolved_supplier_snapshot(&db).expect("runtime snapshot");
    let keys: Vec<&str> = snapshot
        .endpoints
        .iter()
        .map(|endpoint| endpoint.api_key.as_str())
        .collect();
    assert!(
        keys.contains(&"sk-one") && keys.contains(&"sk-two"),
        "{keys:?}"
    );
    assert_eq!(supplier_count(&db), 2, "same name must not merge suppliers");
}

/// Protocol comes from real declarations — meta.apiFormat and the active
/// Codex TOML wire_api — never from URL guessing. Responses-native
/// endpoints are recorded as `openai-responses` and get no legacy
/// projection.
#[test]
fn protocol_comes_from_api_format_and_active_wire_api() {
    let db = Database::memory().expect("memory db");
    let fixture = Fixture::new("protocol");
    insert_provider(
        &fixture,
        "claude-openai",
        "claude",
        "Claude via Chat",
        &claude_settings("https://chat-relay.example.invalid", "sk-a"),
        "{\"apiFormat\":\"openai_chat\"}",
    );
    insert_provider(
        &fixture,
        "codex-responses",
        "codex",
        "Codex Responses",
        &codex_settings("https://resp.example.invalid", "sk-b", "responses"),
        "{}",
    );
    // Declared chat wire despite an "anthropic"-looking URL: the
    // declaration wins over URL guessing.
    insert_provider(
        &fixture,
        "codex-chat",
        "codex",
        "Codex Chat",
        &codex_settings(
            "https://looks-anthropic.example.invalid/anthropic",
            "sk-c",
            "chat",
        ),
        "{}",
    );

    let preview = preview_import(&db, &fixture.path).expect("preview");
    let by_id = |source_id: &str| {
        preview
            .plans
            .iter()
            .find(|entry| entry.source_id == source_id)
            .unwrap_or_else(|| panic!("missing plan entry {source_id}"))
    };
    assert_eq!(by_id("claude-openai").protocol, "openai-chat");
    assert_eq!(by_id("codex-responses").protocol, "openai-responses");
    assert_eq!(by_id("codex-chat").protocol, "openai-chat");
    assert!(by_id("claude-openai").legacy_projection_planned);
    assert!(by_id("codex-responses").legacy_projection_planned);

    apply_import(&db, &fixture.path, &preview.plan_id).expect("apply");
    let (_, records) = db.get_supplier_snapshot().expect("snapshot");
    let protocols: Vec<String> = records
        .iter()
        .flat_map(|record| record.endpoints.iter().map(|e| e.protocol.clone()))
        .collect();
    assert!(protocols.contains(&"openai-chat".to_string()));
    assert!(protocols.contains(&"openai-responses".to_string()));
    // A known-unknown declaration is explained, not guessed.
    let conn = db.conn.lock().expect("lock");
    let proxies: i64 = conn
        .query_row("SELECT COUNT(*) FROM proxy_providers", [], |row| row.get(0))
        .expect("count proxies");
    drop(conn);
    assert_eq!(
        proxies, 3,
        "all three supported protocols have native runtime projections"
    );
}

/// An unrecognized protocol declaration becomes a problem entry and the
/// row is not imported.
#[test]
fn unknown_protocol_declaration_is_explained_not_guessed() {
    let db = Database::memory().expect("memory db");
    let fixture = Fixture::new("unknownproto");
    insert_provider(
        &fixture,
        "weird",
        "claude",
        "Weird",
        &claude_settings("https://weird.example.invalid", "sk-w"),
        "{\"apiFormat\":\"carrier_pigeon\"}",
    );

    let preview = preview_import(&db, &fixture.path).expect("preview");
    assert!(preview.plans.is_empty());
    assert!(preview.problems.iter().any(|problem| {
        problem.code == "protocol_declaration_unknown"
            && problem.source_id.as_deref() == Some("weird")
    }));
    let applied = apply_import(&db, &fixture.path, &preview.plan_id).expect("apply");
    assert_eq!(applied.created_count, 0);
    assert_eq!(supplier_count(&db), 0);
}

/// Preview must not write configuration: no suppliers, endpoints,
/// models, proxy projections, legacy providers, or routes appear, and
/// the generation is untouched.
#[test]
fn preview_has_zero_configuration_side_effects() {
    let db = Database::memory().expect("memory db");
    let fixture = Fixture::new("zeroside");
    insert_provider(
        &fixture,
        "one",
        "claude",
        "One",
        &claude_settings("https://one.example.invalid", "sk-one"),
        "{}",
    );
    insert_provider(
        &fixture,
        "two",
        "codex",
        "Two",
        &codex_settings("https://two.example.invalid", "sk-two", "responses"),
        "{}",
    );

    let before_generation = generation(&db);
    let before_suppliers = supplier_count(&db);
    let before_endpoints = endpoint_count(&db);
    let before_proxies: i64 = {
        let conn = db.conn.lock().expect("lock");
        conn.query_row("SELECT COUNT(*) FROM proxy_providers", [], |row| row.get(0))
            .expect("proxies")
    };
    let before_routes = db.get_all_routes().expect("routes").len();

    let preview = preview_import(&db, &fixture.path).expect("preview");
    assert_eq!(preview.plans.len(), 2);
    assert!(!preview.plan_id.is_empty());

    assert_eq!(generation(&db), before_generation);
    assert_eq!(supplier_count(&db), before_suppliers);
    assert_eq!(endpoint_count(&db), before_endpoints);
    let after_proxies: i64 = {
        let conn = db.conn.lock().expect("lock");
        conn.query_row("SELECT COUNT(*) FROM proxy_providers", [], |row| row.get(0))
            .expect("proxies")
    };
    assert_eq!(after_proxies, before_proxies);
    assert_eq!(db.get_all_routes().expect("routes").len(), before_routes);
}

/// A plan bound to an older generation is refused without writes.
#[test]
fn stale_preview_is_refused_without_writes() {
    let db = Database::memory().expect("memory db");
    let fixture = Fixture::new("stale");
    insert_provider(
        &fixture,
        "one",
        "claude",
        "One",
        &claude_settings("https://one.example.invalid", "sk-one"),
        "{}",
    );
    let preview = preview_import(&db, &fixture.path).expect("preview");

    // A concurrent configuration write advances the generation.
    save_supplier(
        &db,
        SupplierSaveInput {
            id: Some("manual_intruder".into()),
            expected_generation: preview.target_generation,
            name: "Manual".into(),
            base_url: Some("https://manual.example.invalid".into()),
            api_key: Some("sk-manual".into()),
            enabled: true,
            priority: 100,
            source: None,
            provenance: None,
            icon: None,
            icon_color: None,
            endpoints: Some(vec![SupplierEndpointInput {
                protocol: "anthropic".into(),
                endpoint_url: None,
                api_key_override: None,
                use_shared_api_key: false,
                enabled: Some(true),
                priority: Some(0),
                extra_headers: None,
                source: None,
                provenance: None,
                models: Some(vec![SupplierModelInput {
                    model_id: "m".into(),
                    display_name: None,
                    enabled: true,
                    supports_tools: None,
                    supports_images: None,
                    context_window: None,
                    max_output_tokens: None,
                    tier_override: None,
                    source: None,
                    provenance: None,
                }]),
            }]),
        },
    )
    .expect("concurrent save");

    let error = apply_import(&db, &fixture.path, &preview.plan_id).unwrap_err();
    assert!(
        error.to_string().contains("supplier_import_stale_preview"),
        "unexpected error: {error}"
    );
    let (_, records) = db.get_supplier_snapshot().expect("snapshot");
    assert_eq!(records.len(), 1, "only the manual row exists");
    assert_eq!(records[0].id, "manual_intruder");
}

/// A mid-transaction failure rolls the whole apply back: earlier rows in
/// the same batch must not survive.
#[test]
fn apply_rolls_back_every_row_on_midway_failure() {
    let db = Database::memory().expect("memory db");
    let fixture = Fixture::new("rollback");
    insert_provider(
        &fixture,
        "first",
        "claude",
        "First",
        &claude_settings("https://first.example.invalid", "sk-first"),
        "{}",
    );
    insert_provider(
        &fixture,
        "second",
        "claude",
        "Second",
        &claude_settings("https://second.example.invalid", "sk-second"),
        "{}",
    );
    let preview = preview_import(&db, &fixture.path).expect("preview");
    assert_eq!(preview.plans.len(), 2);

    // Sabotage the SECOND row's legacy projection id so its projection
    // insert fails after the first row has already been written inside
    // the transaction.
    let second_supplier = legacy_supplier_id("cc_switch", "claude_code:second");
    let second_endpoint = endpoint_id_for(&second_supplier, "anthropic");
    let conflicting_provider = super::supplier_import::projection_id_for(&second_endpoint);
    {
        let conn = db.conn.lock().expect("lock");
        conn.execute(
            "INSERT INTO proxy_providers (
                 id, name, type, api_key, base_url, models, enabled, timeout_ms, weight,
                 circuit_breaker, model_mapping, extra_headers, auto_track_models, created_at
             ) VALUES (?1, 'squatter', 'anthropic', '', 'https://squatter.example.invalid',
                       '[]', 1, 60000, 1, '{}', '{}', '{}', 1, 1)",
            [&conflicting_provider],
        )
        .expect("insert squatter");
    }
    let before_generation = generation(&db);

    let error = apply_import(&db, &fixture.path, &preview.plan_id).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("supplier_projection_id_conflict"),
        "unexpected error: {error}"
    );
    assert_eq!(supplier_count(&db), 0, "rolled back: no supplier rows");
    assert_eq!(endpoint_count(&db), 0, "rolled back: no endpoint rows");
    assert_eq!(generation(&db), before_generation, "generation unchanged");
}

/// Repeated applies are idempotent: same plan, then a fresh preview of an
/// unchanged source, then apply again — nothing duplicates and nothing
/// errors.
#[test]
fn apply_is_idempotent_across_repeats_and_repreviews() {
    let db = Database::memory().expect("memory db");
    let fixture = Fixture::new("idempotent");
    insert_provider(
        &fixture,
        "one",
        "claude",
        "One",
        &claude_settings("https://one.example.invalid", "sk-one"),
        "{}",
    );
    let preview = preview_import(&db, &fixture.path).expect("preview");
    let first = apply_import(&db, &fixture.path, &preview.plan_id).expect("first apply");
    assert!(!first.already_applied);
    assert_eq!(first.applied_generation, preview.target_generation + 1);

    let repeat = apply_import(&db, &fixture.path, &preview.plan_id).expect("repeat apply");
    assert!(repeat.already_applied, "same plan re-apply is a no-op");
    assert_eq!(repeat.applied_generation, first.applied_generation);
    assert_eq!(supplier_count(&db), 1);

    let repreview = preview_import(&db, &fixture.path).expect("re-preview");
    assert_eq!(repreview.plans[0].change, "unchanged");
    let again = apply_import(&db, &fixture.path, &repreview.plan_id).expect("re-preview apply");
    assert!(again.already_applied);
    assert_eq!(supplier_count(&db), 1);
    assert_eq!(endpoint_count(&db), 1);
}

/// Official accounts and managed auth are recorded as non-sensitive
/// pending associations only — never as usable API suppliers, never with
/// credentials, never in the runtime snapshot.
#[test]
fn official_and_managed_auth_are_association_only() {
    let db = Database::memory().expect("memory db");
    let fixture = Fixture::new("official");
    // Official Anthropic subscription row: official base, empty key.
    insert_provider(
        &fixture,
        "claude-official",
        "claude",
        "Claude Subscription",
        &claude_settings("https://api.anthropic.com", ""),
        "{}",
    );
    // Managed-account binding (e.g. a Copilot OAuth account).
    insert_provider(
        &fixture,
        "managed-one",
        "codex",
        "Managed Account",
        &codex_settings("https://managed.example.invalid", "sk-ignored", "responses"),
        "{\"authBinding\":{\"source\":\"managed_account\",\"authProvider\":\"github_copilot\"}}",
    );
    // And a normal static-key row to prove the others are not confused
    // with it.
    insert_provider(
        &fixture,
        "normal",
        "claude",
        "Normal",
        &claude_settings("https://normal.example.invalid", "sk-normal"),
        "{}",
    );

    let preview = preview_import(&db, &fixture.path).expect("preview");
    assert_eq!(preview.plans.len(), 1);
    assert_eq!(preview.plans[0].source_id, "normal");
    assert!(preview.pending_unsupported.is_empty());
    assert_eq!(preview.excluded_oauth_count, 2);
    let applied = apply_import(&db, &fixture.path, &preview.plan_id).expect("apply");
    assert_eq!(applied.pending_unsupported_count, 0);
    let status = supplier_status(&db).expect("status");
    assert_eq!(status.pending_unsupported_count, 0);
    assert_eq!(supplier_count(&db), 1);
    // Runtime snapshot must not expose the pending rows at all.
    let snapshot = resolved_supplier_snapshot(&db).expect("runtime snapshot");
    assert_eq!(snapshot.endpoints.len(), 1);
    assert_eq!(
        snapshot.endpoints[0].supplier_id,
        preview.plans[0].supplier_id
    );
    // No credential material was stored for the managed row.
    let (_, records) = db.get_supplier_snapshot().expect("snapshot");
    for record in &records {
        if record.source == "official_auth_pending" {
            assert!(record.api_key.is_empty(), "pending rows carry no key");
        }
    }
}

/// Editing the source after preview invalidates the plan.
#[test]
fn source_change_invalidates_plan() {
    let db = Database::memory().expect("memory db");
    let fixture = Fixture::new("sourcechange");
    insert_provider(
        &fixture,
        "one",
        "claude",
        "One",
        &claude_settings("https://one.example.invalid", "sk-one"),
        "{}",
    );
    let preview = preview_import(&db, &fixture.path).expect("preview");

    fixture.execute("UPDATE providers SET name = 'One Renamed' WHERE id = 'one'");

    let error = apply_import(&db, &fixture.path, &preview.plan_id).unwrap_err();
    assert!(
        error.to_string().contains("supplier_import_source_changed"),
        "unexpected error: {error}"
    );
    assert_eq!(supplier_count(&db), 0);
}

/// The preview is redacted: no keys, no query-string URLs, no source
/// paths in any serialized field.
#[test]
fn preview_is_redacted_and_bound_to_generation() {
    let db = Database::memory().expect("memory db");
    let fixture = Fixture::new("redacted");
    insert_provider(
        &fixture,
        "one",
        "claude",
        "One",
        &claude_settings(
            "https://user:pass@one.example.invalid/v1?token=secret#f",
            "sk-secret-123",
        ),
        "{}",
    );
    fixture.insert(
        "INSERT INTO provider_endpoints (provider_id, app_type, url, added_at)
         VALUES ('one', 'claude', 'https://user:pass@edge.example.invalid/v1?k=x', 5)",
        &[],
    );

    let preview = preview_import(&db, &fixture.path).expect("preview");
    let serialized = serde_json::to_string(&preview).expect("serialize");
    assert!(
        !serialized.contains("sk-secret-123"),
        "key leaked: {serialized}"
    );
    assert!(!serialized.contains("token=secret"), "query leaked");
    assert!(!serialized.contains("user:pass"), "userinfo leaked");
    assert!(
        !serialized.contains(&fixture.path.display().to_string()),
        "path leaked"
    );
    assert_eq!(preview.target_generation, generation(&db));
    assert_eq!(preview.source_label, "cc-switch");
    // The unsafe URL cannot become a planned base URL either.
    assert!(preview.plans.is_empty(), "unsafe URL row is not planned");
    assert!(preview
        .problems
        .iter()
        .any(|problem| problem.code == "base_url_invalid"));
}

/// Malformed source rows are reported one by one, and the healthy rows
/// still import.
#[test]
fn malformed_rows_are_explained_while_healthy_rows_import() {
    let db = Database::memory().expect("memory db");
    let fixture = Fixture::new("malformed");
    insert_provider(
        &fixture,
        "good",
        "claude",
        "Good",
        &claude_settings("https://good.example.invalid", "sk-good"),
        "{}",
    );
    insert_provider(
        &fixture,
        "bad-json",
        "claude",
        "Bad",
        "not json at all",
        "{}",
    );
    insert_provider(
        &fixture,
        "bad-meta",
        "claude",
        "Bad Meta",
        &claude_settings("https://meta.example.invalid", "sk-meta"),
        "not json",
    );

    let preview = preview_import(&db, &fixture.path).expect("preview");
    assert_eq!(preview.total_rows, 3);
    assert_eq!(preview.plans.len(), 1);
    assert!(preview
        .problems
        .iter()
        .any(|problem| problem.code == "settings_config_invalid"
            && problem.source_id.as_deref() == Some("bad-json")));
    assert!(preview
        .problems
        .iter()
        .any(|problem| problem.code == "meta_invalid"
            && problem.source_id.as_deref() == Some("bad-meta")));
    let applied = apply_import(&db, &fixture.path, &preview.plan_id).expect("apply");
    assert_eq!(applied.created_count, 1);
    assert_eq!(applied.problem_count, 2);
    assert_eq!(supplier_count(&db), 1);
}

/// A malformed plan id is rejected as its own stable error code.
#[test]
fn malformed_plan_id_is_rejected() {
    let db = Database::memory().expect("memory db");
    let fixture = Fixture::new("badplan");
    let error = apply_import(&db, &fixture.path, "ccsplan_zz_not-a-plan").unwrap_err();
    assert!(
        error.to_string().contains("supplier_import_plan_invalid"),
        "unexpected error: {error}"
    );
}

#[test]
fn merges_same_vendor_channels_without_losing_endpoint_urls() {
    let db = Database::memory().unwrap();
    let fixture = Fixture::new("merged-channels");
    insert_provider(
        &fixture,
        "glm-claude",
        "claude",
        "GLM",
        &claude_settings("https://open.bigmodel.cn/api/anthropic", "fixture-shared"),
        "{}",
    );
    insert_provider(
        &fixture,
        "glm-codex",
        "codex",
        "GLM Responses",
        &codex_settings(
            "https://open.bigmodel.cn/api/paas/v4",
            "fixture-shared",
            "responses",
        ),
        "{}",
    );
    let preview = preview_import(&db, &fixture.path).unwrap();
    assert_eq!(preview.plans.len(), 2);
    assert_eq!(preview.plans[0].supplier_id, preview.plans[1].supplier_id);
    let applied = apply_import(&db, &fixture.path, &preview.plan_id).unwrap();
    assert_eq!(applied.created_count, 1);
    let suppliers = list_suppliers(&db).unwrap().suppliers;
    assert_eq!(suppliers.len(), 1);
    assert_eq!(suppliers[0].endpoints.len(), 2);
    let urls: Vec<_> = suppliers[0]
        .endpoints
        .iter()
        .filter_map(|e| e.effective_base_url.as_deref())
        .collect();
    assert!(urls.contains(&"https://open.bigmodel.cn/api/anthropic"));
    assert!(urls.contains(&"https://open.bigmodel.cn/api/paas/v4"));
    assert!(
        apply_import(&db, &fixture.path, &preview.plan_id)
            .unwrap()
            .already_applied
    );
}

#[test]
fn keeps_distinct_credentials_separate_even_for_same_vendor() {
    let db = Database::memory().unwrap();
    let fixture = Fixture::new("distinct-accounts");
    insert_provider(
        &fixture,
        "one",
        "claude",
        "GLM",
        &claude_settings("https://open.bigmodel.cn/api/anthropic", "fixture-one"),
        "{}",
    );
    insert_provider(
        &fixture,
        "two",
        "codex",
        "GLM",
        &codex_settings(
            "https://open.bigmodel.cn/api/paas/v4",
            "fixture-two",
            "responses",
        ),
        "{}",
    );
    let preview = preview_import(&db, &fixture.path).unwrap();
    assert_ne!(preview.plans[0].supplier_id, preview.plans[1].supplier_id);
}

#[test]
fn excludes_account_login_rows_without_materializing_pending_options() {
    let db = Database::memory().unwrap();
    let fixture = Fixture::new("account-exclusion");
    insert_provider(
        &fixture,
        "login",
        "codex",
        "Account",
        &codex_settings(
            "https://account.example.invalid",
            "fixture-ignored",
            "responses",
        ),
        "{\"authBinding\":{\"source\":\"managed_account\"}}",
    );
    let preview = preview_import(&db, &fixture.path).unwrap();
    assert!(preview.plans.is_empty());
    assert!(preview.pending_unsupported.is_empty());
    assert_eq!(preview.excluded_oauth_count, 1);
    apply_import(&db, &fixture.path, &preview.plan_id).unwrap();
    assert_eq!(supplier_count(&db), 0);
}

/// Display icons ride through the supplier import onto the legacy
/// projection rows: brand keys become designed glyphs and colors pass
/// through; unknown keys fall back to the monogram (no icon) while the
/// color still becomes the tile background.
#[test]
fn projection_rows_carry_mapped_display_icons() {
    let db = Database::memory().expect("memory db");
    let fixture = Fixture::new("icons");
    fixture.insert(
        "INSERT INTO providers (id, app_type, name, settings_config, meta, icon, icon_color) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        &[
            &"iconed",
            &"claude",
            &"Iconed Relay",
            &claude_settings("https://relay.example.invalid", "sk-icon"),
            &"{}",
            &"deepseek",
            &"#1E88E5",
        ],
    );

    let preview = preview_import(&db, &fixture.path).expect("preview");
    let applied = apply_import(&db, &fixture.path, &preview.plan_id).expect("apply");
    assert_eq!(applied.created_count, 1);

    let conn = db.conn.lock().expect("database lock");
    let (icon, color): (Option<String>, Option<String>) = conn
        .query_row(
            "SELECT icon, icon_color FROM proxy_providers WHERE id LIKE 'supplier_%'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("projection row");
    assert_eq!(icon.as_deref(), Some("whale"));
    assert_eq!(color.as_deref(), Some("#1E88E5"));
    // The supplier row itself owns the same display icons; the projection
    // only mirrors them.
    let (supplier_icon, supplier_color): (Option<String>, Option<String>) = conn
        .query_row(
            "SELECT icon, icon_color FROM suppliers WHERE source = 'cc_switch'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("supplier row");
    assert_eq!(supplier_icon.as_deref(), Some("whale"));
    assert_eq!(supplier_color.as_deref(), Some("#1E88E5"));
}

/// Icon fields are plan content: an icon-only source edit must invalidate
/// a previously previewed plan instead of silently applying stale data.
#[test]
fn icon_edits_change_the_plan_binding() {
    let db = Database::memory().expect("memory db");
    let fixture = Fixture::new("iconedit");
    insert_provider(
        &fixture,
        "icon-edit",
        "claude",
        "Icon Edit",
        &claude_settings("https://edit.example.invalid", "sk-edit"),
        "{}",
    );
    fixture.execute("UPDATE providers SET icon = 'deepseek'");

    let preview = preview_import(&db, &fixture.path).expect("preview");
    fixture.execute("UPDATE providers SET icon = 'openai'");
    let repreview = preview_import(&db, &fixture.path).expect("repreview");
    assert_ne!(
        preview.plan_id, repreview.plan_id,
        "an icon-only source edit must change the plan binding"
    );
    let error = apply_import(&db, &fixture.path, &preview.plan_id).unwrap_err();
    assert!(
        error.to_string().contains("supplier_import_source_changed"),
        "unexpected error: {error}"
    );
}

/// Icon-only source edits must re-apply: the projection rows refresh
/// their display icons instead of hiding behind supplier-level content
/// idempotency, and the refreshed plan then settles into a stable
/// already-applied no-op (even though applying advanced the generation).
#[test]
fn icon_only_source_edits_refresh_projection_icons() {
    let db = Database::memory().expect("memory db");
    let fixture = Fixture::new("iconrefresh");
    fixture.insert(
        "INSERT INTO providers (id, app_type, name, settings_config, meta, icon, icon_color) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        &[
            &"icon-refresh",
            &"claude",
            &"Icon Refresh",
            &claude_settings("https://refresh.example.invalid", "sk-refresh"),
            &"{}",
            &"deepseek",
            &"#1E88E5",
        ],
    );

    let preview = preview_import(&db, &fixture.path).expect("preview");
    let first = apply_import(&db, &fixture.path, &preview.plan_id).expect("first apply");
    assert!(!first.already_applied);

    // Only the icon changes in the source; every substantive field is
    // untouched.
    fixture.execute("UPDATE providers SET icon = 'openai'");
    let repreview = preview_import(&db, &fixture.path).expect("repreview");
    let second = apply_import(&db, &fixture.path, &repreview.plan_id).expect("second apply");
    assert!(
        !second.already_applied,
        "icon-only edits must not report already applied"
    );

    let conn = db.conn.lock().expect("database lock");
    let icon: Option<String> = conn
        .query_row(
            "SELECT icon FROM proxy_providers WHERE id LIKE 'supplier_%'",
            [],
            |row| row.get(0),
        )
        .expect("projection row");
    assert_eq!(icon.as_deref(), Some("knot"));
    let supplier_icon: Option<String> = conn
        .query_row(
            "SELECT icon FROM suppliers WHERE source = 'cc_switch'",
            [],
            |row| row.get(0),
        )
        .expect("supplier row");
    assert_eq!(supplier_icon.as_deref(), Some("knot"));
    drop(conn);

    let third = apply_import(&db, &fixture.path, &repreview.plan_id).expect("third apply");
    assert!(
        third.already_applied,
        "the refreshed plan must settle into a stable no-op"
    );
}

#[test]
fn same_protocol_urls_are_preserved_as_routable_variants_on_one_supplier() {
    let db = Database::memory().unwrap();
    let fixture = Fixture::new("alternate-url");
    insert_provider(
        &fixture,
        "first",
        "claude",
        "GLM",
        &claude_settings("https://open.bigmodel.cn/first", "fixture_same"),
        "{}",
    );
    insert_provider(
        &fixture,
        "second",
        "claude",
        "GLM",
        &claude_settings("https://open.bigmodel.cn/second", "fixture_same"),
        "{}",
    );
    let preview = preview_import(&db, &fixture.path).unwrap();
    assert_eq!(preview.plans.len(), 1);
    apply_import(&db, &fixture.path, &preview.plan_id).unwrap();
    let list = list_suppliers(&db).unwrap();
    assert_eq!(list.suppliers.len(), 1);
    assert_eq!(list.suppliers[0].endpoints[0].alternatives.len(), 1);
    let runtime = resolved_supplier_snapshot(&db).unwrap();
    assert_eq!(runtime.endpoints.len(), 2);
    assert!(runtime
        .endpoints
        .iter()
        .all(|endpoint| endpoint.legacy_proxy_provider_id.is_some()));
    assert!(
        apply_import(&db, &fixture.path, &preview.plan_id)
            .unwrap()
            .already_applied
    );
}

#[test]
fn maintenance_preserves_static_manual_ccs_row_and_repointed_source_endpoint() {
    let db = Database::memory().unwrap();
    let fixture = Fixture::new("safe-maintenance");
    insert_provider(
        &fixture,
        "account",
        "claude",
        "Account",
        &claude_settings("https://account.example.invalid", "fixture_same"),
        "{\"authBinding\":{\"source\":\"managed_account\"}}",
    );
    let manual=serde_json::from_value(serde_json::json!({"id":"ccs-manual","name":"Valid static manual","type":"anthropic_passthrough","baseURL":"https://manual.example.invalid","apiKey":"fixture_static","models":["valid"]})).unwrap();
    db.save_proxy_provider(&manual).unwrap();
    let input:SupplierSaveInput=serde_json::from_value(serde_json::json!({"id":"repointed","expectedGeneration":0,"name":"Retained","baseUrl":"https://new-static.example.invalid","apiKey":"fixture_same","source":"cc_switch","provenance":{"sourceKind":"cc_switch","sourceRecordRef":"claude_code:account"},"endpoints":[{"protocol":"anthropic","provenance":{"sourceKind":"cc_switch","sourceRecordRef":"claude_code:account"},"models":[{"modelId":"valid"}]}]})).unwrap();
    save_supplier(&db, input).unwrap();
    let root =
        crate::config::private_fs::private_temp_dir(&std::env::temp_dir().canonicalize().unwrap())
            .unwrap();
    let result =
        super::maintenance::maintain_imported_suppliers_at(&db, &fixture.path, root.path())
            .unwrap();
    assert!(!result.backup_created);
    assert!(db.get_proxy_provider("ccs-manual").unwrap().is_some());
    assert!(db
        .get_supplier_record_snapshot("repointed")
        .unwrap()
        .1
        .is_some());
}

#[test]
fn maintenance_converts_valid_static_pending_row_and_cleans_its_original_account_proxy() {
    let db = Database::memory().unwrap();
    let fixture = Fixture::new("converted-pending");
    let proxy_id = "ccs-original-account";
    let original=serde_json::from_value(serde_json::json!({"id":proxy_id,"name":"Old account","type":"anthropic_passthrough","baseURL":"https://old-account.example.invalid","models":["old"]})).unwrap();
    db.save_proxy_provider(&original).unwrap();
    let supplier_id = legacy_supplier_id("passthrough", proxy_id);
    let input:SupplierSaveInput=serde_json::from_value(serde_json::json!({"id":supplier_id,"expectedGeneration":0,"name":"Converted API","baseUrl":"https://valid.example.invalid","apiKey":"fixture_valid","source":"official_auth_pending","provenance":{"sourceKind":"official_auth_pending","sourceRecordRef":supplier_id},"endpoints":[{"protocol":"openai-chat","models":[{"modelId":"valid"}]}]})).unwrap();
    save_supplier(&db, input).unwrap();
    let root =
        crate::config::private_fs::private_temp_dir(&std::env::temp_dir().canonicalize().unwrap())
            .unwrap();
    let result =
        super::maintenance::maintain_imported_suppliers_at(&db, &fixture.path, root.path())
            .unwrap();
    assert_eq!(result.preserved_static_suppliers, 1);
    let valid = db
        .get_supplier_record_snapshot(&supplier_id)
        .unwrap()
        .1
        .unwrap();
    assert_eq!(valid.source, "manual");
    assert_eq!(valid.api_key, "fixture_valid");
    assert_eq!(valid.endpoints[0].models[0].model_id, "valid");
    assert!(db.get_proxy_provider(proxy_id).unwrap().is_none());
}

#[test]
fn maintenance_removes_only_the_account_endpoint_from_a_mixed_supplier() {
    let db = Database::memory().unwrap();
    let fixture = Fixture::new("mixed-parent");
    insert_provider(
        &fixture,
        "bad",
        "claude",
        "Account",
        &claude_settings("https://account.example.invalid", "fixture_shared"),
        "{\"authBinding\":{\"source\":\"managed_account\"}}",
    );
    let input:SupplierSaveInput=serde_json::from_value(serde_json::json!({"id":"mixed","expectedGeneration":0,"name":"Retained API","baseUrl":"https://account.example.invalid","apiKey":"fixture_shared","source":"cc_switch","endpoints":[{"protocol":"anthropic","endpointUrl":"https://account.example.invalid","provenance":{"sourceKind":"cc_switch","sourceRecordRef":"claude_code:bad"},"models":[{"modelId":"old"}]},{"protocol":"openai-chat","endpointUrl":"https://valid.example.invalid","apiKeyOverride":"fixture_valid","models":[{"modelId":"valid"}]}]})).unwrap();
    save_supplier(&db, input).unwrap();
    let root =
        crate::config::private_fs::private_temp_dir(&std::env::temp_dir().canonicalize().unwrap())
            .unwrap();
    let result =
        super::maintenance::maintain_imported_suppliers_at(&db, &fixture.path, root.path())
            .unwrap();
    assert_eq!(result.removed_oauth_suppliers, 0);
    assert!(result.backup_created);
    let saved = db.get_supplier_record_snapshot("mixed").unwrap().1.unwrap();
    assert_eq!(saved.endpoints.len(), 1);
    assert_eq!(saved.endpoints[0].protocol, "openai-chat");
    assert_eq!(saved.base_url, "https://valid.example.invalid");
    assert_eq!(saved.api_key, "fixture_valid");
}

#[test]
fn cleanup_promotes_a_valid_alternate_when_the_imported_primary_becomes_account_auth() {
    let db = Database::memory().unwrap();
    let fixture = Fixture::new("promote-static-alternate");
    insert_provider(
        &fixture,
        "first",
        "claude",
        "GLM",
        &claude_settings("https://open.bigmodel.cn/first", "fixture_shared"),
        "{}",
    );
    insert_provider(
        &fixture,
        "second",
        "claude",
        "GLM",
        &claude_settings("https://open.bigmodel.cn/second", "fixture_shared"),
        "{}",
    );
    let preview = preview_import(&db, &fixture.path).unwrap();
    apply_import(&db, &fixture.path, &preview.plan_id).unwrap();
    let id = preview.plans[0].supplier_id.clone();
    fixture.execute("UPDATE providers SET meta='{\"authBinding\":{\"source\":\"managed_account\"}}' WHERE id='first'");
    let root =
        crate::config::private_fs::private_temp_dir(&std::env::temp_dir().canonicalize().unwrap())
            .unwrap();
    let result =
        super::maintenance::maintain_imported_suppliers_at(&db, &fixture.path, root.path())
            .unwrap();
    assert_eq!(result.removed_oauth_suppliers, 0);
    assert!(result.removed_oauth_providers > 0);
    let saved = db.get_supplier_record_snapshot(&id).unwrap().1.unwrap();
    assert_eq!(saved.endpoints.len(), 1);
    assert!(saved.endpoints[0].alternatives.is_empty());
    assert_eq!(saved.base_url, "https://open.bigmodel.cn/second");
    let runtime = resolved_supplier_snapshot(&db).unwrap();
    assert_eq!(runtime.endpoints.len(), 1);
    assert_eq!(
        runtime.endpoints[0].base_url,
        "https://open.bigmodel.cn/second"
    );
    assert!(runtime.endpoints[0].legacy_proxy_provider_id.is_some());
}

#[test]
fn a_preexisting_manual_conversion_with_the_same_url_and_key_is_not_account_cleanup() {
    let db = Database::memory().unwrap();
    let fixture = Fixture::new("manual-conversion-same-coordinate");
    let mut source: serde_json::Value = serde_json::from_str(&codex_settings(
        "https://gateway.example.invalid",
        "fixture_same",
        "responses",
    ))
    .unwrap();
    source["auth"]["refresh_token"] = serde_json::json!("fixture_refresh");
    insert_provider(
        &fixture,
        "account",
        "codex",
        "Original account",
        &source.to_string(),
        "{}",
    );
    let mut converted = source.clone();
    converted["auth"]
        .as_object_mut()
        .unwrap()
        .remove("refresh_token");
    converted["model"] = serde_json::json!("valid-static-model");
    let saved = crate::provider::Provider::new(
        "account".into(),
        "Converted valid API".into(),
        converted.clone(),
    );
    db.save_provider("codex", &saved).unwrap();
    db.set_current_provider("account", "codex").unwrap();
    let proxy=serde_json::from_value(serde_json::json!({"id":"ccs-account","name":"Converted valid API","type":"openai-responses","baseURL":"https://gateway.example.invalid","apiKey":"fixture_same","models":["valid-static-model"]})).unwrap();
    db.save_proxy_provider(&proxy).unwrap();
    let root =
        crate::config::private_fs::private_temp_dir(&std::env::temp_dir().canonicalize().unwrap())
            .unwrap();
    let result =
        super::maintenance::maintain_imported_suppliers_at(&db, &fixture.path, root.path())
            .unwrap();
    assert!(!result.backup_created);
    assert_eq!(
        db.get_provider_by_id("account", "codex")
            .unwrap()
            .unwrap()
            .settings_config,
        converted
    );
    assert!(db.get_proxy_provider("ccs-account").unwrap().is_some());
    assert_eq!(
        db.get_current_provider("codex").unwrap().as_deref(),
        Some("account")
    );
}

#[test]
fn automatic_merge_preserves_distinct_effective_accounts_and_newer_manual_anchor_data() {
    let db = Database::memory().unwrap();
    let fixture = Fixture::new("divergent-merge");
    let mut generation = 0;
    for (id, override_key) in [("first", None), ("second", Some("fixture_other"))] {
        let input:SupplierSaveInput=serde_json::from_value(serde_json::json!({"id":id,"expectedGeneration":generation,"name":"GLM","baseUrl":"https://open.bigmodel.cn/anthropic","apiKey":"fixture_shared","source":"cc_switch","endpoints":[{"protocol":"anthropic","apiKeyOverride":override_key,"models":[{"modelId":"source-model"}]}]})).unwrap();
        generation = save_supplier(&db, input).unwrap().generation;
    }
    let root =
        crate::config::private_fs::private_temp_dir(&std::env::temp_dir().canonicalize().unwrap())
            .unwrap();
    let result =
        super::maintenance::maintain_imported_suppliers_at(&db, &fixture.path, root.path())
            .unwrap();
    assert!(!result.backup_created);
    assert_eq!(result.merge_conflict_count, 1);
    assert!(result
        .merge_conflict_codes
        .contains(&"supplier_merge_credential_divergence".into()));
    assert_eq!(db.get_supplier_snapshot().unwrap().1.len(), 2);
    let input:SupplierSaveInput=serde_json::from_value(serde_json::json!({"id":"second","expectedGeneration":generation,"name":"GLM","endpoints":[{"protocol":"anthropic","useSharedApiKey":true}]})).unwrap();
    save_supplier(&db, input).unwrap();
    let proxy_id = db
        .get_supplier_record_snapshot("second")
        .unwrap()
        .1
        .unwrap()
        .endpoints[0]
        .proxy_provider_id
        .clone()
        .unwrap();
    let mut proxy = db.get_proxy_provider(&proxy_id).unwrap().unwrap();
    proxy.models = vec!["manual-upgraded-model".into()];
    db.save_proxy_provider(&proxy).unwrap();
    let result =
        super::maintenance::maintain_imported_suppliers_at(&db, &fixture.path, root.path())
            .unwrap();
    assert!(!result.backup_created);
    assert_eq!(result.merge_conflict_count, 1);
    assert!(result
        .merge_conflict_codes
        .contains(&"supplier_merge_manual_projection_divergence".into()));
    assert_eq!(
        db.get_proxy_provider(&proxy_id).unwrap().unwrap().models,
        vec!["manual-upgraded-model"]
    );
    assert_eq!(db.get_supplier_snapshot().unwrap().1.len(), 2);
}

#[test]
fn renaming_or_locking_an_unchanged_oauth_record_does_not_make_it_a_static_conversion() {
    let db = Database::memory().unwrap();
    let fixture = Fixture::new("renamed-account");
    let mut source: serde_json::Value = serde_json::from_str(&codex_settings(
        "https://account.example.invalid",
        "fixture_proxy",
        "responses",
    ))
    .unwrap();
    source["auth"]["refresh_token"] = serde_json::json!("fixture_refresh");
    insert_provider(
        &fixture,
        "renamed",
        "codex",
        "Original account",
        &source.to_string(),
        "{}",
    );
    let saved = crate::provider::Provider::new("renamed".into(), "Renamed UI label".into(), source);
    db.save_provider("codex", &saved).unwrap();
    db.conn.lock().unwrap().execute("UPDATE providers SET category='renamed',sync_locked=1 WHERE id='renamed' AND app_type='codex'",[]).unwrap();
    let proxy=serde_json::from_value(serde_json::json!({"id":"ccs-renamed","name":"Renamed UI label","type":"openai-responses","baseURL":"https://account.example.invalid","apiKey":"fixture_proxy","models":["old"]})).unwrap();
    db.save_proxy_provider(&proxy).unwrap();
    let root =
        crate::config::private_fs::private_temp_dir(&std::env::temp_dir().canonicalize().unwrap())
            .unwrap();
    let result =
        super::maintenance::maintain_imported_suppliers_at(&db, &fixture.path, root.path())
            .unwrap();
    assert!(result.backup_created);
    assert!(db.get_provider_by_id("renamed", "codex").unwrap().is_none());
    assert!(db.get_proxy_provider("ccs-renamed").unwrap().is_none());
}

#[test]
fn pending_account_placeholder_keys_are_removed_instead_of_retagged_manual() {
    let db = Database::memory().unwrap();
    let fixture = Fixture::new("placeholder-pending");
    for (index, key) in ["dummy", "sk-local", "cliproxyapi", "placeholder"]
        .into_iter()
        .enumerate()
    {
        db.conn.lock().unwrap().execute("INSERT INTO suppliers(id,name,base_url,api_key,enabled,priority,source,provenance_json) VALUES (?1,'Account','http://127.0.0.1:8317',?2,0,100,'official_auth_pending','{}')",rusqlite::params![format!("pending-{index}"),key]).unwrap();
    }
    let root =
        crate::config::private_fs::private_temp_dir(&std::env::temp_dir().canonicalize().unwrap())
            .unwrap();
    let result =
        super::maintenance::maintain_imported_suppliers_at(&db, &fixture.path, root.path())
            .unwrap();
    assert_eq!(result.removed_oauth_suppliers, 4);
    assert_eq!(result.preserved_static_suppliers, 0);
    assert!(db.get_supplier_snapshot().unwrap().1.is_empty());
}

#[test]
fn maintenance_removes_exact_imported_accounts_without_a_pool_projection() {
    let db = Database::memory().unwrap();
    let fixture = Fixture::new("poolless-account");
    insert_provider(&fixture, "official", "claude", "Official", "{}", "{}");
    fixture.execute("UPDATE providers SET category='official' WHERE id='official'");
    let saved = crate::provider::Provider::new(
        "official".into(),
        "Renamed official".into(),
        serde_json::json!({}),
    );
    db.save_provider("claude_code", &saved).unwrap();
    db.set_current_provider("official", "claude_code").unwrap();
    insert_provider(&fixture, "converted", "claude", "Official", "{}", "{}");
    fixture.execute("UPDATE providers SET category='official' WHERE id='converted'");
    let converted = crate::provider::Provider::new(
        "converted".into(),
        "Valid API".into(),
        serde_json::from_str(&claude_settings(
            "https://static.example.invalid",
            "fixture_static",
        ))
        .unwrap(),
    );
    db.save_provider("claude_code", &converted).unwrap();
    let root =
        crate::config::private_fs::private_temp_dir(&std::env::temp_dir().canonicalize().unwrap())
            .unwrap();
    let result =
        super::maintenance::maintain_imported_suppliers_at(&db, &fixture.path, root.path())
            .unwrap();
    assert!(result.backup_created);
    assert_eq!(result.removed_oauth_providers, 1);
    assert!(db
        .get_provider_by_id("official", "claude_code")
        .unwrap()
        .is_none());
    assert!(db.get_current_provider("claude_code").unwrap().is_none());
    assert_eq!(
        db.get_provider_by_id("converted", "claude_code")
            .unwrap()
            .unwrap()
            .settings_config,
        converted.settings_config
    );
}

#[test]
fn maintenance_follows_stale_import_ids_only_for_unowned_account_only_projections() {
    let db = Database::memory().unwrap();
    let fixture = Fixture::new("stale-account-id");
    let account_url = "https://chatgpt.com/backend-api/codex";
    insert_provider(
        &fixture,
        "recreated-account",
        "claude",
        "Current account",
        &claude_settings(account_url, ""),
        "{\"authBinding\":{\"source\":\"managed_account\"}}",
    );
    for (id, app, parent_url, parent_key) in [
        ("old-account", "claude_code", account_url, ""),
        (
            "manual-static",
            "claude_code",
            account_url,
            "fixture_static",
        ),
        (
            "repointed",
            "claude_code",
            "https://manual.example.invalid",
            "",
        ),
        ("different-app", "claude_desktop", account_url, ""),
        ("owned-account", "claude_code", account_url, ""),
    ] {
        let mut settings: serde_json::Value =
            serde_json::from_str(&claude_settings(parent_url, parent_key)).unwrap();
        settings["model"] = serde_json::json!("locally-selected-model");
        db.save_provider(
            app,
            &crate::provider::Provider::new(id.into(), "Legacy label".into(), settings),
        )
        .unwrap();
        let proxy = serde_json::from_value(serde_json::json!({
            "id":format!("ccs-{id}"),"name":"Legacy label","type":"anthropic",
            "baseURL":account_url,"apiKey":parent_key,"models":["old"],"enabled":false
        }))
        .unwrap();
        db.save_proxy_provider(&proxy).unwrap();
    }
    let unlinked = serde_json::from_value(serde_json::json!({
        "id":"ccs-unlinked","name":"Unlinked","type":"anthropic",
        "baseURL":account_url,"apiKey":"","models":["old"]
    }))
    .unwrap();
    db.save_proxy_provider(&unlinked).unwrap();
    db.save_route(&crate::database::dao::routes::Route {
        alias: "retained-static-route".into(),
        description: String::new(),
        hops: vec![
            crate::database::dao::routes::RouteHop {
                provider: "ccs-old-account".into(),
                model: "old".into(),
            },
            crate::database::dao::routes::RouteHop {
                provider: "ccs-manual-static".into(),
                model: "valid".into(),
            },
        ],
    })
    .unwrap();
    let input: SupplierSaveInput = serde_json::from_value(serde_json::json!({
        "id":"manual-owner","expectedGeneration":0,"name":"Retained owner",
        "baseUrl":"https://valid.example.invalid","apiKey":"fixture_static",
        "endpoints":[{"protocol":"anthropic","models":[{"modelId":"valid"}]}]
    }))
    .unwrap();
    save_supplier(&db, input).unwrap();
    db.conn.lock().unwrap().execute(
        "UPDATE supplier_runtime_projections SET proxy_provider_id='ccs-owned-account' WHERE endpoint_id=?1",
        [endpoint_id_for("manual-owner", "anthropic")],
    ).unwrap();
    db.set_current_provider("old-account", "claude_code")
        .unwrap();
    let root =
        crate::config::private_fs::private_temp_dir(&std::env::temp_dir().canonicalize().unwrap())
            .unwrap();
    let result =
        super::maintenance::maintain_imported_suppliers_at(&db, &fixture.path, root.path())
            .unwrap();
    assert!(result.backup_created);
    assert_eq!(result.removed_oauth_providers, 2);
    assert!(db
        .get_provider_by_id("old-account", "claude_code")
        .unwrap()
        .is_none());
    assert!(db.get_proxy_provider("ccs-old-account").unwrap().is_none());
    assert!(db.get_current_provider("claude_code").unwrap().is_none());
    let route = db.get_route("retained-static-route").unwrap().unwrap();
    assert_eq!(route.hops.len(), 1);
    assert_eq!(route.hops[0].provider, "ccs-manual-static");
    for id in [
        "manual-static",
        "repointed",
        "different-app",
        "owned-account",
        "unlinked",
    ] {
        assert!(
            db.get_proxy_provider(&format!("ccs-{id}"))
                .unwrap()
                .is_some(),
            "{id}"
        );
    }
    assert!(db
        .get_provider_by_id("manual-static", "claude_code")
        .unwrap()
        .is_some());
    assert!(db
        .get_provider_by_id("owned-account", "claude_code")
        .unwrap()
        .is_some());
}
