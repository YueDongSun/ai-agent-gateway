//! Real import orchestration against disposable sources and loopback providers.
use super::{apply_import, preview_import, screen_import};
use crate::{
    database::Database,
    services::{provider_admission, provider_screening::SCREENING_CONFIG_KEY},
};
use axum::{
    extract::{OriginalUri, State},
    routing::post,
    Json, Router,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

struct Source_fixture {
    _root: crate::config::private_fs::PrivateTempDir,
    path: PathBuf,
}

impl Source_fixture {
    fn new() -> Self {
        let root = crate::config::private_fs::private_temp_dir(
            &std::env::temp_dir().canonicalize().unwrap(),
        )
        .unwrap();
        let path = root.path().join("cc_switch.db");
        rusqlite::Connection::open(&path).unwrap().execute_batch(
            "CREATE TABLE providers(id TEXT NOT NULL,app_type TEXT NOT NULL,name TEXT NOT NULL,settings_config TEXT NOT NULL,website_url TEXT,category TEXT,created_at INTEGER,sort_index INTEGER,notes TEXT,icon TEXT,icon_color TEXT,meta TEXT NOT NULL DEFAULT '{}',is_current BOOLEAN DEFAULT 0,in_failover_queue BOOLEAN DEFAULT 0,PRIMARY KEY(id,app_type));
             CREATE TABLE provider_endpoints(id INTEGER PRIMARY KEY AUTOINCREMENT,provider_id TEXT NOT NULL,app_type TEXT NOT NULL,url TEXT NOT NULL,added_at INTEGER);"
        ).unwrap();
        Self { _root: root, path }
    }

    fn insert(&self, id: &str, base: &str, key: &str, alternative: Option<&str>) {
        let conn = rusqlite::Connection::open(&self.path).unwrap();
        let settings = json!({"env":{"ANTHROPIC_BASE_URL":base,"ANTHROPIC_AUTH_TOKEN":key,
            "ANTHROPIC_MODEL":"shared-fixture-model","ANTHROPIC_DEFAULT_SONNET_MODEL":"shared-fixture-model","ANTHROPIC_DEFAULT_OPUS_MODEL":"fixture-plus"}});
        conn.execute(
            "INSERT INTO providers(id,app_type,name,settings_config) VALUES(?1,'claude',?2,?3)",
            rusqlite::params![id, format!("Fixture {id}"), settings.to_string()],
        )
        .unwrap();
        if let Some(url) = alternative {
            conn.execute("INSERT INTO provider_endpoints(provider_id,app_type,url,added_at) VALUES(?1,'claude',?2,1)", rusqlite::params![id,url]).unwrap();
        }
    }
}

#[derive(Clone)]
struct Server_state {
    db: Database,
    calls: Arc<Mutex<BTreeMap<String, usize>>>,
    rejected: bool,
    require_unregistered: bool,
}

struct Probe_server {
    base: String,
    calls: Arc<Mutex<BTreeMap<String, usize>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Probe_server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn assert_no_registration(db: &Database) {
    assert!(db.get_all_proxy_providers().unwrap().is_empty());
    let (_, suppliers) = db.get_supplier_snapshot().unwrap();
    assert!(suppliers.is_empty());
    let conn = db.conn.lock().unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM supplier_models", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 0);
}

impl Probe_server {
    async fn new(db: &Database, rejected: bool) -> Self {
        Self::with_registration_check(db, rejected, true).await
    }

    async fn with_registration_check(
        db: &Database,
        rejected: bool,
        require_unregistered: bool,
    ) -> Self {
        let calls = Arc::new(Mutex::new(BTreeMap::new()));
        let state = Server_state {
            db: db.clone(),
            calls: calls.clone(),
            rejected,
            require_unregistered,
        };
        let app = Router::new().route("/*path", post(
            |State(state): State<Server_state>, OriginalUri(uri): OriginalUri, Json(body): Json<Value>| async move {
                // Assert the actual send occurs before any provider/model is registered.
                if state.require_unregistered { assert_no_registration(&state.db); }
                *state.calls.lock().unwrap().entry(uri.path().to_string()).or_default() += 1;
                assert_eq!(body["messages"], json!([{"role":"user","content":[{"type":"text","text":"Reply with exactly: ok"}]}]));
                assert!(body.get("system").is_none());
                if uri.path().contains("timeout") { tokio::time::sleep(Duration::from_millis(500)).await; }
                Json(json!({"id":"fixture","type":"message","role":"assistant","model":body["model"],
                    "content":[{"type":"text","text":if state.rejected {"actual non-ok import fixture"} else {"ok"}}],
                    "stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1}}))
            })).with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self { base, calls, task }
    }
    fn count(&self) -> usize {
        self.calls.lock().unwrap().values().sum()
    }
}

#[tokio::test]
async fn screening_precedes_registration_and_keeps_source_and_alternative_model_pairs() {
    let db = Database::memory().unwrap();
    let first = Probe_server::new(&db, false).await;
    let second = Probe_server::new(&db, false).await;
    let source = Source_fixture::new();
    source.insert(
        "first",
        &format!("{}/primary", first.base),
        "fixture_first_key",
        Some(&format!("{}/alternative", first.base)),
    );
    source.insert("second", &second.base, "fixture_second_key", None);
    let preview = preview_import(&db, &source.path).unwrap();
    assert_eq!(preview.plans.len(), 2);
    assert_no_registration(&db);
    let screened = screen_import(
        &db,
        &source.path,
        &preview.plan_id,
        &["claude_code:first".into(), "claude_code:second".into()],
    )
    .await
    .unwrap();
    assert_eq!(screened.len(), 3);
    assert!(screened.iter().all(|outcome| outcome.eligible));
    assert_no_registration(&db);
    assert_eq!(first.count(), 2);
    assert_eq!(second.count(), 1);
    let applied = apply_import(&db, &source.path, &preview.plan_id).unwrap();
    assert_eq!(applied.created_count, 2);
    let providers = db.get_all_proxy_providers().unwrap();
    assert_eq!(providers.len(), 3);
    let pairs: std::collections::BTreeSet<_> = providers
        .iter()
        .flat_map(|provider| {
            provider
                .models
                .iter()
                .map(move |model| (provider.id.clone(), model.clone()))
        })
        .collect();
    assert_eq!(pairs.len(), 6);
    for provider in &providers {
        assert_eq!(provider_admission::admitted_models(&db, provider).len(), 2);
        provider_admission::ensure_provider_admitted(&db, provider, "shared-fixture-model")
            .await
            .unwrap();
    }
    assert_eq!(db.get_supplier_snapshot().unwrap().1.len(), 2);
    assert_eq!(
        first.count() + second.count(),
        3,
        "apply and admission lookups must not re-probe"
    );
}

#[tokio::test]
async fn reimport_screens_each_materialized_provider_once_and_merges_duplicate_source_models() {
    let db = Database::memory().unwrap();
    let server = Probe_server::with_registration_check(&db, false, false).await;
    let source = Source_fixture::new();
    for (id, base) in [("first", "primary"), ("second", "other")] {
        source.insert(
            id,
            &format!("{}/{base}", server.base),
            "fixture_shared_key",
            Some(&format!("{}/shared-alternative", server.base)),
        );
    }
    let conn = rusqlite::Connection::open(&source.path).unwrap();
    conn.execute("UPDATE providers SET name='Fixture Shared'", [])
        .unwrap();
    let settings: String = conn
        .query_row(
            "SELECT settings_config FROM providers WHERE id='second'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let mut settings: Value = serde_json::from_str(&settings).unwrap();
    settings["env"]["ANTHROPIC_DEFAULT_OPUS_MODEL"] = json!("second-extra");
    conn.execute(
        "UPDATE providers SET settings_config=?1 WHERE id='second'",
        [settings.to_string()],
    )
    .unwrap();
    drop(conn);

    // Materialize the same shared alternative from both CC records. The owner
    // importer keeps one actual provider for that connection.
    let initial = preview_import(&db, &source.path).unwrap();
    apply_import(&db, &source.path, &initial.plan_id).unwrap();
    let providers = db.get_all_proxy_providers().unwrap();
    assert_eq!(providers.len(), 3);
    let shared = providers
        .iter()
        .find(|provider| provider.base_url.ends_with("/shared-alternative"))
        .unwrap();
    let resolved = super::screening::candidate_by_id(&db, &source.path, &shared.id).unwrap();
    assert_eq!(
        resolved.models,
        ["fixture-plus", "shared-fixture-model", "second-extra"]
    );

    for expected_calls in [2, 4] {
        let preview = preview_import(&db, &source.path).unwrap();
        // Only the later duplicate source has an explicit local assertion.
        // It must be selected for the shared connection, without exempting
        // the unrelated primary connection from the first source.
        let outcomes = screen_import(
            &db,
            &source.path,
            &preview.plan_id,
            &["claude_code:second".into()],
        )
        .await
        .unwrap();
        assert_eq!(outcomes.len(), 3);
        assert_eq!(
            outcomes
                .iter()
                .map(|outcome| &outcome.provider_id)
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            3
        );
        let outcome = outcomes
            .iter()
            .find(|outcome| outcome.provider_id == shared.id)
            .unwrap();
        assert_eq!(outcome.source_record_ref, "claude_code:second");
        assert!(outcome.eligible);
        assert_eq!(
            outcomes.iter().filter(|outcome| outcome.eligible).count(),
            2
        );
        assert_eq!(server.count(), expected_calls);
        assert_eq!(provider_admission::admitted_models(&db, &resolved).len(), 3);
        let record = db.provider_admission_record(&shared.id).unwrap().unwrap();
        assert_eq!(record.offered_models.len(), 3);
        apply_import(&db, &source.path, &preview.plan_id).unwrap();
        let registered = db.get_proxy_provider(&shared.id).unwrap().unwrap();
        assert_eq!(
            registered.models,
            ["fixture-plus", "shared-fixture-model", "second-extra"]
        );
        assert_eq!(
            provider_admission::admitted_models(&db, &registered).len(),
            3
        );
        let repeated = apply_import(&db, &source.path, &preview.plan_id).unwrap();
        assert!(repeated.already_applied);
        assert_eq!(repeated.created_count, 0);
        assert_eq!(repeated.refreshed_count, 0);
        assert_eq!(db.get_all_proxy_providers().unwrap().len(), 3);
        assert_eq!(
            server.count(),
            expected_calls,
            "registration must not issue another probe"
        );
    }
    assert_eq!(
        server
            .calls
            .lock()
            .unwrap()
            .get("/shared-alternative/v1/messages"),
        Some(&2)
    );
    assert_eq!(
        server.calls.lock().unwrap().get("/other/v1/messages"),
        Some(&2)
    );
    assert!(!server
        .calls
        .lock()
        .unwrap()
        .contains_key("/primary/v1/messages"));

    // Removing a declaration from every current source must not resurrect its
    // old importer-owned row when the duplicate alternatives are coalesced.
    let conn = rusqlite::Connection::open(&source.path).unwrap();
    let raw: String = conn
        .query_row(
            "SELECT settings_config FROM providers WHERE id='first'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let mut settings: Value = serde_json::from_str(&raw).unwrap();
    settings["env"]["ANTHROPIC_DEFAULT_OPUS_MODEL"] = json!("shared-fixture-model");
    conn.execute(
        "UPDATE providers SET settings_config=?1 WHERE id='first'",
        [settings.to_string()],
    )
    .unwrap();
    drop(conn);
    let preview = preview_import(&db, &source.path).unwrap();
    let screened = screen_import(
        &db,
        &source.path,
        &preview.plan_id,
        &["claude_code:second".into()],
    )
    .await
    .unwrap();
    assert_eq!(screened.len(), 3);
    apply_import(&db, &source.path, &preview.plan_id).unwrap();
    let registered = db.get_proxy_provider(&shared.id).unwrap().unwrap();
    assert_eq!(registered.models, ["shared-fixture-model", "second-extra"]);
    assert_eq!(server.count(), 6);
    assert!(
        apply_import(&db, &source.path, &preview.plan_id)
            .unwrap()
            .already_applied
    );
}

#[test]
fn duplicate_candidate_identity_conflicts_fail_before_binding_and_models_keep_first_order() {
    use crate::database::dao::proxy_providers::ProxyProvider;
    let first: ProxyProvider = serde_json::from_value(json!({"id":"duplicate_fixture",
        "name":"Fixture", "type":"anthropic", "baseURL":"http://127.0.0.1:9876",
        "apiKey":"fixture_key", "models":["first","shared","first"]}))
    .unwrap();
    let mut second = first.clone();
    second.models = vec!["shared".into(), "last".into()];
    let raw = vec![
        (first.clone(), "claude_code:first".into()),
        (second.clone(), "claude_code:second".into()),
    ];
    let merged = super::screening::coalesce_candidates(&raw, &[]).unwrap();
    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].0.models, ["first", "shared", "last"]);
    assert_eq!(merged[0].1, "claude_code:first");
    assert_eq!(
        super::screening::coalesce_candidates(&raw, &["claude_code:second".into()]).unwrap()[0].1,
        "claude_code:second"
    );
    second.api_key = "different_fixture_key".into();
    assert_eq!(
        super::screening::coalesce_candidates(
            &[
                (first, "claude_code:first".into()),
                (second, "claude_code:second".into())
            ],
            &["claude_code:second".into()]
        )
        .err()
        .as_deref(),
        Some("supplier_import_candidate_conflict")
    );
}

#[test]
fn admitted_alternatives_merge_into_primary_or_first_matching_target_only() {
    let db = Database::memory().unwrap();
    let source = Source_fixture::new();
    source.insert(
        "first",
        "http://127.0.0.1:9876/primary",
        "fixture_key",
        Some("http://127.0.0.1:9876/alternative"),
    );
    let rows = super::preview::read_source_rows(&source.path).unwrap();
    let mut entry = super::preview::derive_plan(&rows, &[], &db)
        .unwrap()
        .entries
        .remove(0);
    let template = entry.alternatives[0].clone();
    entry.models = vec!["primary".into()];
    entry.alternatives = (0..4)
        .map(|index| {
            let mut alternative = template.clone();
            alternative.source_id = format!("source_{index}");
            alternative.models = match index {
                0 => vec!["primary".into(), "shared".into(), "not_admitted".into()],
                1 => vec!["second".into()],
                2 => vec!["third".into(), "second".into()],
                _ => vec!["second".into()],
            };
            alternative
        })
        .collect();
    super::screening::coalesce_admitted_alternatives(
        &mut entry,
        Some("primary_id"),
        vec![
            (
                Some("primary_id".into()),
                vec!["primary".into(), "shared".into()],
            ),
            (Some("alternative_id".into()), vec!["second".into()]),
            (
                Some("alternative_id".into()),
                vec!["second".into(), "third".into()],
            ),
            (Some("distinct_id".into()), vec!["second".into()]),
        ],
    );
    assert_eq!(entry.models, ["primary", "shared"]);
    assert_eq!(entry.alternatives.len(), 2);
    assert_eq!(entry.alternatives[0].source_id, "source_1");
    assert_eq!(entry.alternatives[0].models, ["second", "third"]);
    assert_eq!(entry.alternatives[1].source_id, "source_3");
    assert_eq!(entry.alternatives[1].models, ["second"]);
}

#[tokio::test]
async fn invalid_explicit_local_reference_rejects_before_any_binding_or_probe() {
    let db = Database::memory().unwrap();
    let server = Probe_server::new(&db, false).await;
    let source = Source_fixture::new();
    source.insert("first", &server.base, "fixture_local_key", None);
    source.insert(
        "second",
        "https://remote.invalid/anthropic",
        "fixture_remote_key",
        None,
    );
    let preview = preview_import(&db, &source.path).unwrap();
    let result = screen_import(
        &db,
        &source.path,
        &preview.plan_id,
        &["claude_code:first".into(), "claude_code:second".into()],
    )
    .await;
    assert_eq!(
        result.err().as_deref(),
        Some("provider_subscription_local_binding_invalid")
    );
    assert_eq!(server.count(), 0);
    assert!(!provider_admission::enforcement_enabled(&db));
    let conn = db.conn.lock().unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM app_meta WHERE key LIKE 'provider_subscription:record:%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        count, 0,
        "read-only validation must not bind the earlier valid reference"
    );
}

#[tokio::test]
async fn rejected_and_unknown_imports_stay_unregistered_and_cached_inactive_reimport_sends_nothing()
{
    let db = Database::memory().unwrap();
    db.set_setting(SCREENING_CONFIG_KEY, &json!({"max_latency_ms":100,"probe_deadline_ms":200,"max_output_tokens":64,"max_response_chars":1024}).to_string()).unwrap();
    let server = Probe_server::new(&db, true).await;
    let source = Source_fixture::new();
    for id in ["bad_reply", "timeout", "unknown"] {
        source.insert(
            id,
            &format!("{}/{id}", server.base),
            &format!("fixture_{id}_key"),
            None,
        );
    }
    let preview = preview_import(&db, &source.path).unwrap();
    let free = vec!["claude_code:bad_reply".into(), "claude_code:timeout".into()];
    let outcomes = screen_import(&db, &source.path, &preview.plan_id, &free)
        .await
        .unwrap();
    assert_eq!(outcomes.len(), 3);
    assert!(outcomes.iter().all(|outcome| !outcome.eligible));
    assert_eq!(
        outcomes
            .iter()
            .find(|entry| entry.source_record_ref == "claude_code:bad_reply")
            .unwrap()
            .reason,
        "abnormal_response"
    );
    assert_eq!(
        outcomes
            .iter()
            .find(|entry| entry.source_record_ref == "claude_code:timeout")
            .unwrap()
            .reason,
        "timeout"
    );
    assert_eq!(
        outcomes
            .iter()
            .find(|entry| entry.source_record_ref == "claude_code:unknown")
            .unwrap()
            .reason,
        "subscription_unknown"
    );
    assert_eq!(server.count(), 2);
    apply_import(&db, &source.path, &preview.plan_id).unwrap();
    assert_no_registration(&db);
    for outcome in outcomes {
        let old = db
            .provider_subscription_value(&outcome.provider_id)
            .unwrap()
            .unwrap();
        let mut cache: Value = serde_json::from_str(&old).unwrap();
        cache["observation"]["evidence"]["state"] = json!("inactive");
        cache["observation"]["probeAllowed"] = json!(false);
        cache["observation"]["reason"] = json!("no_subscription");
        assert!(db
            .compare_exchange_provider_subscription(
                &outcome.provider_id,
                Some(&old),
                &cache.to_string()
            )
            .unwrap());
    }
    let preview = preview_import(&db, &source.path).unwrap();
    let again = screen_import(&db, &source.path, &preview.plan_id, &free)
        .await
        .unwrap();
    assert!(again
        .iter()
        .all(|outcome| !outcome.eligible && outcome.reason == "subscription_inactive"));
    apply_import(&db, &source.path, &preview.plan_id).unwrap();
    assert_no_registration(&db);
    assert_eq!(server.count(), 2);
}
