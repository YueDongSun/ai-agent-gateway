use super::*;
use crate::services::suppliers::{save_supplier, SupplierSaveInput};
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode, Uri},
    routing::get,
    Json, Router,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

type Requests = Arc<Mutex<Vec<(String, HeaderMap)>>>;

#[tokio::test]
async fn refresh_adds_new_model_families_to_supplier_and_runtime_without_enabling_disabled_models()
{
    let server = CatalogServer::start(StatusCode::OK, json!({"data": [
        {"id":"existing-model"}, {"id":"new-coding-family"}, {"id":"disabled-model"}, {"id":"new-coding-family"}
    ]})).await;
    let db = Database::memory().unwrap();
    let saved = save_supplier(&db, serde_json::from_value(json!({
        "expectedGeneration":0,"name":"fixture","baseUrl": server.base_url,"apiKey":"fixture_key",
        "endpoints":[{"protocol":"anthropic","models":[{"modelId":"existing-model"},{"modelId":"disabled-model","enabled":false}]}]
    })).unwrap()).unwrap();
    revalidate_suppliers(&db, &[saved.supplier_id.clone()])
        .await
        .unwrap();
    let view = crate::services::suppliers::get_supplier(&db, &saved.supplier_id).unwrap();
    let endpoint = &view.supplier.as_ref().unwrap().endpoints[0];
    assert_eq!(endpoint.models.len(), 3);
    assert!(
        !endpoint
            .models
            .iter()
            .find(|model| model.model_id == "disabled-model")
            .unwrap()
            .enabled
    );
    let new_model = endpoint
        .models
        .iter()
        .find(|model| model.model_id == "new-coding-family")
        .unwrap();
    assert_eq!(
        new_model.provenance.source_kind.as_deref(),
        Some("provider_catalog")
    );
    assert_eq!(
        endpoint.discovery_health.as_ref().unwrap().status,
        "catalog_available"
    );
    assert_eq!(
        endpoint.discovery_health.as_ref().unwrap().inference_status,
        "unverified"
    );
    let runtime = crate::services::suppliers::resolved_supplier_snapshot(&db).unwrap();
    assert!(runtime.endpoints[0]
        .models
        .iter()
        .any(|model| model.model_id == "new-coding-family"));
    let generation = view.generation;
    revalidate_suppliers(&db, &[saved.supplier_id])
        .await
        .unwrap();
    assert_eq!(
        db.supplier_config_generation().unwrap(),
        generation,
        "unchanged catalogs must not churn configuration"
    );
}

#[tokio::test]
async fn catalog_auth_failure_preserves_models_and_independent_endpoint_health() {
    let available =
        CatalogServer::start(StatusCode::OK, json!({"data":[{"id":"available-model"}]})).await;
    let denied =
        CatalogServer::start(StatusCode::UNAUTHORIZED, json!({"error":"fixture only"})).await;
    let db = Database::memory().unwrap();
    let saved = save_supplier(&db,serde_json::from_value(json!({"expectedGeneration":0,"name":"partial","baseUrl":available.base_url,
        "endpoints":[{"protocol":"openai-chat","models":[]},{"protocol":"anthropic","endpointUrl":denied.base_url,"models":[{"modelId":"retained-model"}]}]})).unwrap()).unwrap();
    let error = revalidate_suppliers(&db, &[saved.supplier_id.clone()])
        .await
        .unwrap_err();
    assert_eq!(
        error,
        "supplier_discovery_partial:proxy_models_http_error_401"
    );
    let view = crate::services::suppliers::get_supplier(&db, &saved.supplier_id)
        .unwrap()
        .supplier
        .unwrap();
    let denied_endpoint = view
        .endpoints
        .iter()
        .find(|endpoint| endpoint.protocol == "anthropic")
        .unwrap();
    assert_eq!(denied_endpoint.models[0].model_id, "retained-model");
    assert_eq!(
        denied_endpoint.discovery_health.as_ref().unwrap().status,
        "authentication_failed"
    );
    let available_endpoint = view
        .endpoints
        .iter()
        .find(|endpoint| endpoint.protocol == "openai-chat")
        .unwrap();
    assert_eq!(available_endpoint.models[0].model_id, "available-model");
    assert_eq!(
        available_endpoint.discovery_health.as_ref().unwrap().status,
        "catalog_available"
    );
    assert_eq!(
        denied.requests.lock().unwrap().len(),
        1,
        "auth failure must not fan out requests"
    );
}

#[tokio::test]
async fn changing_configuration_invalidates_catalog_health() {
    let server = CatalogServer::start(StatusCode::OK, json!({"data":[{"id":"listed"}]})).await;
    let db = Database::memory().unwrap();
    let saved = save_supplier(
        &db,
        serde_json::from_value(
            json!({"expectedGeneration":0,"name":"fixture","baseUrl":server.base_url,
        "endpoints":[{"protocol":"openai-chat","models":[]}]}),
        )
        .unwrap(),
    )
    .unwrap();
    revalidate_suppliers(&db, &[saved.supplier_id.clone()])
        .await
        .unwrap();
    let generation = db.supplier_config_generation().unwrap();
    save_supplier(&db,serde_json::from_value(json!({"id":saved.supplier_id,"expectedGeneration":generation,"name":"fixture","apiKey":"replacement_fixture_key"})).unwrap()).unwrap();
    let view = crate::services::suppliers::get_supplier(&db, &saved.supplier_id)
        .unwrap()
        .supplier
        .unwrap();
    assert!(view.endpoints[0].discovery_health.is_none());
}

#[tokio::test]
async fn discovery_failure_history_retains_last_success_and_rejects_stale_credentials() {
    let server = CatalogServer::start(StatusCode::OK, json!({"data":[{"id":"listed"}]})).await;
    let db = Database::memory().unwrap();
    let saved = save_supplier(
        &db,
        serde_json::from_value(
            json!({"expectedGeneration":0,"name":"fixture","baseUrl":server.base_url,
        "apiKey":"old_fixture_key","endpoints":[{"protocol":"openai-chat","models":[]}]}),
        )
        .unwrap(),
    )
    .unwrap();
    revalidate_suppliers(&db, &[saved.supplier_id.clone()])
        .await
        .unwrap();
    let provider = db.get_all_proxy_providers().unwrap().remove(0);
    let healthy = db.supplier_discovery_health(&provider.id).unwrap().unwrap();
    for _ in 0..2 {
        let mut failure = healthy.clone();
        failure.status = "rate_limited".into();
        failure.error = Some("proxy_models_http_error_429".into());
        failure.last_success_at = None;
        failure.listed_model_ids.clear();
        db.record_supplier_discovery_failure(&provider, failure)
            .unwrap();
    }
    let failed = db.supplier_discovery_health(&provider.id).unwrap().unwrap();
    assert_eq!(failed.last_success_at, healthy.last_success_at);
    assert_eq!(failed.listed_model_ids, ["listed"]);
    assert_eq!(failed.consecutive_failures, 2);
    let generation = db.supplier_config_generation().unwrap();
    save_supplier(&db,serde_json::from_value(json!({"id":saved.supplier_id,"expectedGeneration":generation,"name":"fixture","apiKey":"new_fixture_key"})).unwrap()).unwrap();
    assert!(db.reconcile_supplier_catalog(&provider, healthy).is_err());
    assert!(db
        .supplier_discovery_health(&provider.id)
        .unwrap()
        .is_none());
}

struct CatalogServer {
    base_url: String,
    requests: Requests,
    task: tokio::task::JoinHandle<()>,
}

impl CatalogServer {
    async fn mutable_catalog(body: Value) -> (Self, Arc<Mutex<Value>>) {
        let body = Arc::new(Mutex::new(body));
        let app = Router::new()
            .fallback(get(|State(body): State<Arc<Mutex<Value>>>| async move {
                Json(body.lock().unwrap().clone())
            }))
            .with_state(body.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (
            Self {
                base_url,
                requests: Requests::default(),
                task,
            },
            body,
        )
    }

    async fn start(status: StatusCode, body: Value) -> Self {
        let requests = Requests::default();
        let app = Router::new()
            .fallback(get(
                |State((requests, status, body)): State<(Requests, StatusCode, Value)>,
                 uri: Uri,
                 headers: HeaderMap| async move {
                    requests.lock().unwrap().push((uri.path().into(), headers));
                    (status, Json(body))
                },
            ))
            .with_state((requests.clone(), status, body));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            base_url,
            requests,
            task,
        }
    }
}

#[tokio::test]
async fn malformed_pagination_preserves_previous_catalog_and_never_demotes_models() {
    let (server, body) =
        CatalogServer::mutable_catalog(json!({"data":[{"id":"verified-listing"}]})).await;
    let db = Database::memory().unwrap();
    let saved = save_supplier(
        &db,
        serde_json::from_value(
            json!({"expectedGeneration":0,"name":"fixture","baseUrl":server.base_url,
        "endpoints":[{"protocol":"openai-chat","models":[]}]}),
        )
        .unwrap(),
    )
    .unwrap();
    revalidate_suppliers(&db, &[saved.supplier_id.clone()])
        .await
        .unwrap();
    let provider_id = db.get_all_proxy_providers().unwrap()[0].id.clone();
    let previous = db.supplier_discovery_health(&provider_id).unwrap().unwrap();
    let generation = db.supplier_config_generation().unwrap();
    for pagination in [
        json!({"has_more":"true"}),
        json!({"has_more":null}),
        json!({"has_more":1}),
        json!({"has_more":true}),
        json!({"has_more":true,"last_id":null}),
        json!({"has_more":true,"last_id":123}),
        json!({"has_more":false,"last_id":123}),
        json!({"nextPageToken":[]}),
        json!({"nextPageToken":null}),
        json!({"has_more":false,"nextPageToken":"next"}),
        json!({"last_id":"orphan"}),
    ] {
        let mut malformed = pagination;
        malformed["data"] = json!([{"id":"partial-page-model"}]);
        *body.lock().unwrap() = malformed;
        let error = revalidate_suppliers(&db, &[saved.supplier_id.clone()])
            .await
            .unwrap_err();
        assert!(error.contains("proxy_models_pagination_invalid"), "{error}");
        let health = db.supplier_discovery_health(&provider_id).unwrap().unwrap();
        assert_eq!(health.last_success_at, previous.last_success_at);
        assert_eq!(health.listed_model_ids, ["verified-listing"]);
        let view = crate::services::suppliers::get_supplier(&db, &saved.supplier_id)
            .unwrap()
            .supplier
            .unwrap();
        assert_eq!(view.endpoints[0].models.len(), 1);
        assert!(view.endpoints[0].models[0].enabled);
        assert_eq!(db.supplier_config_generation().unwrap(), generation);
    }
}

#[tokio::test]
async fn complete_catalog_demotes_and_restores_only_discovery_owned_models() {
    let (server, body) = CatalogServer::mutable_catalog(
        json!({"data":[{"id":"auto-model"},{"id":"manually-disabled"}]}),
    )
    .await;
    let db = Database::memory().unwrap();
    let saved = save_supplier(
        &db,
        serde_json::from_value(
            json!({"expectedGeneration":0,"name":"fixture","baseUrl":server.base_url,
            "endpoints":[{"protocol":"openai-chat","models":[
                {"modelId":"manual-absent","enabled":true},
                {"modelId":"manually-disabled","enabled":false,"source":"provider_catalog"}
            ]}]}),
        )
        .unwrap(),
    )
    .unwrap();
    revalidate_suppliers(&db, &[saved.supplier_id.clone()])
        .await
        .unwrap();
    let provider_id = db.get_all_proxy_providers().unwrap()[0].id.clone();
    *body.lock().unwrap() = json!({"data":[],"has_more":false,"last_id":null});
    let demotion = crate::services::model_discovery::run_refresh(
        &db,
        crate::services::model_discovery::Trigger::Manual,
        true,
        Some(&provider_id),
    )
    .await
    .unwrap();
    assert!(demotion.has_changes);
    let view = crate::services::suppliers::get_supplier(&db, &saved.supplier_id)
        .unwrap()
        .supplier
        .unwrap();
    let models = &view.endpoints[0].models;
    let auto_model = models
        .iter()
        .find(|model| model.model_id == "auto-model")
        .unwrap();
    assert!(!auto_model.enabled);
    assert_eq!(auto_model.source, "provider_catalog_unlisted");
    assert_eq!(
        auto_model.provenance.source_kind.as_deref(),
        Some("provider_catalog_unlisted")
    );
    assert!(
        models
            .iter()
            .find(|model| model.model_id == "manual-absent")
            .unwrap()
            .enabled
    );
    assert!(
        !models
            .iter()
            .find(|model| model.model_id == "manually-disabled")
            .unwrap()
            .enabled
    );
    assert!(!db.get_all_proxy_providers().unwrap()[0]
        .models
        .contains(&"auto-model".to_string()));
    *body.lock().unwrap() = json!({"data":[{"id":"auto-model"},{"id":"manually-disabled"}]});
    let restored = crate::services::model_discovery::run_refresh(
        &db,
        crate::services::model_discovery::Trigger::Manual,
        true,
        Some(&provider_id),
    )
    .await
    .unwrap();
    assert!(restored.has_changes);
    let view = crate::services::suppliers::get_supplier(&db, &saved.supplier_id)
        .unwrap()
        .supplier
        .unwrap();
    assert!(
        view.endpoints[0]
            .models
            .iter()
            .find(|model| model.model_id == "auto-model")
            .unwrap()
            .enabled
    );
    assert!(
        !view.endpoints[0]
            .models
            .iter()
            .find(|model| model.model_id == "manually-disabled")
            .unwrap()
            .enabled
    );
    let repeated = crate::services::model_discovery::run_refresh(
        &db,
        crate::services::model_discovery::Trigger::Manual,
        true,
        Some(&provider_id),
    )
    .await
    .unwrap();
    assert!(!repeated.has_changes);
}

impl Drop for CatalogServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Read (enabled, retire_misses, retired_at, retire_reason) for one model row.
fn retire_state(db: &Database, model_id: &str) -> (bool, i64, Option<i64>, Option<String>) {
    let conn = db.conn.lock().unwrap();
    conn.query_row(
        "SELECT enabled, retire_misses, retired_at, retire_reason
         FROM supplier_models WHERE model_id = ?1",
        [model_id],
        |row| {
            Ok((
                row.get::<_, bool>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        },
    )
    .unwrap()
}

#[tokio::test]
async fn import_owned_rows_retire_after_miss_streak_while_manual_rows_only_flag() {
    let (server, body) =
        CatalogServer::mutable_catalog(json!({"data":[{"id":"kept-model"}]})).await;
    let db = Database::memory().unwrap();
    let saved = save_supplier(&db, serde_json::from_value(json!({
        "expectedGeneration":0,"name":"fixture","baseUrl":server.base_url,
        "endpoints":[{"protocol":"openai-chat","models":[
            {"modelId":"kept-model"},
            {"modelId":"import-gone","source":"cc_switch"},
            {"modelId":"manual-gone"}
        ]}]
    })).unwrap()).unwrap();
    // The revalidation itself is the first unlisted observation (miss #1 of
    // the default threshold 2): retirement must not land after one miss.
    revalidate_suppliers(&db, &[saved.supplier_id.clone()])
        .await
        .unwrap();
    let provider_id = db.get_all_proxy_providers().unwrap()[0].id.clone();
    let (enabled, misses, retired_at, _) = retire_state(&db, "import-gone");
    assert!(enabled, "one unlisted fetch must not retire import-owned rows");
    assert_eq!(misses, 1);
    assert!(retired_at.is_none());
    assert!(retire_state(&db, "manual-gone").0);

    let retired = crate::services::model_discovery::run_refresh(
        &db,
        crate::services::model_discovery::Trigger::Manual,
        true,
        Some(&provider_id),
    )
    .await
    .unwrap();
    assert!(retired.has_changes, "retirement is an inventory change");
    let (enabled, misses, retired_at, reason) = retire_state(&db, "import-gone");
    assert!(!enabled, "second unlisted fetch retires the import row");
    assert_eq!(misses, 2);
    assert!(retired_at.is_some());
    assert_eq!(reason.as_deref(), Some("catalog_unlisted"));
    assert!(
        !db.get_all_proxy_providers().unwrap()[0]
            .models
            .contains(&"import-gone".to_string()),
        "retired rows leave the projected pool"
    );
    // Manual rows are flagged stale but never disabled.
    let (enabled, misses, retired_at, reason) = retire_state(&db, "manual-gone");
    assert!(enabled, "manual rows are never auto-disabled");
    assert_eq!(misses, 2);
    assert!(retired_at.is_some());
    assert_eq!(reason.as_deref(), Some("catalog_unlisted"));
    let (kept_enabled, _, _, _) = retire_state(&db, "kept-model");
    assert!(kept_enabled);

    // Relisting restores the retired import row and clears the manual flag.
    *body.lock().unwrap() = json!({"data":[{"id":"kept-model"},{"id":"import-gone"},{"id":"manual-gone"}]});
    crate::services::model_discovery::run_refresh(
        &db,
        crate::services::model_discovery::Trigger::Manual,
        true,
        Some(&provider_id),
    )
    .await
    .unwrap();
    let (enabled, misses, retired_at, reason) = retire_state(&db, "import-gone");
    assert!(enabled, "relisting restores the retired import row");
    assert_eq!(misses, 0);
    assert!(retired_at.is_none());
    assert!(reason.is_none());
    let (enabled, misses, retired_at, _) = retire_state(&db, "manual-gone");
    assert!(enabled);
    assert_eq!(misses, 0);
    assert!(retired_at.is_none());
}

fn draft(protocol: &str, base_url: &str) -> SupplierDiscoverModelsInput {
    serde_json::from_value(json!({
        "protocol": protocol,
        "baseUrl": base_url,
        "apiKey": "fixture_shared_key"
    }))
    .unwrap()
}

fn total_changes(db: &Database) -> i64 {
    db.conn
        .lock()
        .unwrap()
        .query_row("SELECT total_changes()", [], |row| row.get(0))
        .unwrap()
}

struct BoundCatalog {
    _root: crate::config::private_fs::PrivateTempDir,
    source_path: std::path::PathBuf,
    server: CatalogServer,
    supplier_id: String,
    provider: ProxyProvider,
}

impl BoundCatalog {
    async fn new(db: &Database, bearer: bool) -> Self {
        use axum::routing::post;
        let requests = Requests::default();
        let check = move |headers: &HeaderMap| {
            if bearer {
                headers
                    .get("authorization")
                    .is_some_and(|value| value == "Bearer fixture_bound_key")
                    && !headers.contains_key("x-api-key")
            } else {
                headers
                    .get("x-api-key")
                    .is_some_and(|value| value == "fixture_bound_key")
                    && !headers.contains_key("authorization")
            }
        };
        let app = Router::new()
            .route("/v1/models", get(move |State(requests): State<Requests>, headers: HeaderMap| async move {
                let accepted = check(&headers);
                requests.lock().unwrap().push(("/v1/models".into(), headers));
                if accepted { (StatusCode::OK, Json(json!({"data":[{"id":"listed-model"}]}))) }
                else { (StatusCode::UNAUTHORIZED, Json(json!({"error":"fixture auth rejected"}))) }
            }))
            .route("/v1/messages", post(move |State(requests): State<Requests>, headers: HeaderMap, Json(body): Json<Value>| async move {
                let accepted = check(&headers);
                requests.lock().unwrap().push(("/v1/messages".into(), headers));
                if accepted { (StatusCode::OK, Json(json!({"id":"fixture","type":"message","role":"assistant","model":body["model"],
                    "content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1}}))) }
                else { (StatusCode::UNAUTHORIZED, Json(json!({"error":"fixture auth rejected"}))) }
            }))
            .with_state(requests.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let server = CatalogServer {
            base_url,
            requests,
            task,
        };
        let saved = save_supplier(db, serde_json::from_value(json!({
            "expectedGeneration":db.supplier_config_generation().unwrap(),"name":"bound fixture","baseUrl":server.base_url,
            "apiKey":"fixture_bound_key","endpoints":[{"protocol":"anthropic","models":[{"modelId":"tested-model"}]}]
        })).unwrap()).unwrap();
        let endpoint_id = db
            .get_supplier_record_snapshot(&saved.supplier_id)
            .unwrap()
            .1
            .unwrap()
            .endpoints[0]
            .proxy_provider_id
            .clone()
            .unwrap();
        let provider = db.get_proxy_provider(&endpoint_id).unwrap().unwrap();
        let root = crate::config::private_fs::private_temp_dir(
            &std::env::temp_dir().canonicalize().unwrap(),
        )
        .unwrap();
        let source_path = root.path().join("cc_switch.db");
        let conn = rusqlite::Connection::open(&source_path).unwrap();
        conn.execute_batch("CREATE TABLE providers(id TEXT,app_type TEXT,name TEXT,settings_config TEXT,website_url TEXT,category TEXT,created_at INTEGER,sort_index INTEGER,notes TEXT,icon TEXT,icon_color TEXT,meta TEXT,is_current INTEGER,in_failover_queue INTEGER);").unwrap();
        let key_field = if bearer {
            "ANTHROPIC_AUTH_TOKEN"
        } else {
            "ANTHROPIC_API_KEY"
        };
        let mut settings = json!({"env":{"ANTHROPIC_BASE_URL":server.base_url}});
        settings["env"][key_field] = json!("fixture_bound_key");
        conn.execute("INSERT INTO providers(id,app_type,name,settings_config,meta,is_current,in_failover_queue) VALUES('catalog_source','claude','Fixture',?1,'{}',0,0)", [settings.to_string()]).unwrap();
        let screened = crate::services::provider_admission::screen_bound_provider(
            db,
            &provider,
            "tested-model",
            &source_path,
            "claude_code:catalog_source",
            true,
            true,
        )
        .await
        .unwrap();
        assert!(
            screened.eligible,
            "local inference must accept the exact source credential scheme"
        );
        Self {
            _root: root,
            source_path,
            server,
            supplier_id: saved.supplier_id,
            provider,
        }
    }

    fn input(&self) -> SupplierDiscoverModelsInput {
        serde_json::from_value(json!({"supplierId":self.supplier_id,"protocol":"anthropic"}))
            .unwrap()
    }

    fn change_source(&self) {
        rusqlite::Connection::open(&self.source_path)
            .unwrap()
            .execute(
                "UPDATE providers SET meta=?1",
                [json!({"changed":"source identity"}).to_string()],
            )
            .unwrap();
    }
}

#[tokio::test]
async fn saved_discovery_uses_its_exact_projection_despite_identical_unadmitted_records() {
    let db = Database::memory().unwrap();
    db.set_provider_admission_enforced(true).unwrap();
    let fixture = BoundCatalog::new(&db, false).await;
    for enabled in [false, true] {
        let mut other = fixture.provider.clone();
        other.id = "000_unselected_identical_connection".into();
        other.weight = fixture.provider.weight + 1;
        other.enabled = enabled;
        db.save_proxy_provider(&other).unwrap();
        assert_eq!(db.get_all_proxy_providers().unwrap()[0].id, other.id);
        assert_eq!(
            discover_supplier_models(&db, fixture.input())
                .await
                .unwrap(),
            ["listed-model"]
        );
        assert!(db.provider_admission_record(&other.id).unwrap().is_none());
    }
}

#[tokio::test]
async fn saved_discovery_cannot_borrow_admission_from_another_enabled_projection() {
    let db = Database::memory().unwrap();
    db.set_provider_admission_enforced(true).unwrap();
    let fixture = BoundCatalog::new(&db, false).await;
    let mut other = fixture.provider.clone();
    other.id = "000_admitted_identical_connection".into();
    other.weight = fixture.provider.weight + 1;
    db.save_proxy_provider(&other).unwrap();
    assert!(
        crate::services::provider_admission::screen_bound_provider(
            &db,
            &other,
            "tested-model",
            &fixture.source_path,
            "claude_code:catalog_source",
            true,
            true
        )
        .await
        .unwrap()
        .eligible
    );
    let mut disabled = fixture.provider.clone();
    disabled.enabled = false;
    db.save_proxy_provider(&disabled).unwrap();
    let calls = fixture.server.requests.lock().unwrap().len();
    assert_eq!(
        crate::services::suppliers::ipc_error(
            discover_supplier_models(&db, fixture.input())
                .await
                .unwrap_err()
        ),
        "supplier_subscription_screening_required"
    );
    assert_eq!(fixture.server.requests.lock().unwrap().len(), calls);
}

#[tokio::test]
async fn changed_and_unbound_drafts_cannot_borrow_an_admitted_connections_coordinates() {
    let db = Database::memory().unwrap();
    db.set_provider_admission_enforced(true).unwrap();
    let selected = BoundCatalog::new(&db, false).await;
    let other = BoundCatalog::new(&db, false).await;
    let mut changed = selected.input();
    changed.endpoint_url = Some(other.server.base_url.clone());
    let mut unbound = draft("anthropic", &other.server.base_url);
    unbound.api_key = Some("fixture_bound_key".into());
    let calls = other.server.requests.lock().unwrap().len();
    for input in [changed, unbound] {
        assert_eq!(
            crate::services::suppliers::ipc_error(
                discover_supplier_models(&db, input).await.unwrap_err()
            ),
            "supplier_subscription_screening_required"
        );
    }
    assert_eq!(other.server.requests.lock().unwrap().len(), calls);
}

#[tokio::test]
async fn bound_catalog_uses_source_auth_for_saved_manual_and_background_paths() {
    for bearer in [false, true] {
        let db = Database::memory().unwrap();
        db.set_provider_admission_enforced(true).unwrap();
        let fixture = BoundCatalog::new(&db, bearer).await;
        assert_eq!(
            discover_supplier_models(&db, fixture.input())
                .await
                .unwrap(),
            ["listed-model"]
        );
        let scheme =
            crate::services::provider_admission::auth_scheme(&db, &fixture.provider).unwrap();
        // The manual command shares this fetch seam; its IPC call supplies the
        // same validated scheme rather than inferring it from the protocol.
        assert_eq!(
            fetch_models(&fixture.provider, scheme)
                .await
                .unwrap()
                .models[0]
                .id,
            "listed-model"
        );
        let refreshed = crate::services::model_discovery::run_refresh(
            &db,
            crate::services::model_discovery::Trigger::Periodic,
            true,
            Some(&fixture.provider.id),
        )
        .await
        .unwrap();
        assert!(
            refreshed.providers[0].ok,
            "{:?}",
            refreshed.providers[0].error
        );
        assert!(fixture
            .server
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|(_, headers)| {
                headers.contains_key("authorization") == bearer
                    && headers.contains_key("x-api-key") != bearer
            }));
        assert_eq!(
            db.provider_admission_record(&fixture.provider.id)
                .unwrap()
                .unwrap()
                .probe
                .unwrap()
                .probe_model,
            "tested-model"
        );
        assert_eq!(
            db.supplier_discovery_health(&fixture.provider.id)
                .unwrap()
                .unwrap()
                .inference_status,
            "unverified"
        );
    }
}

#[tokio::test]
async fn known_changed_source_cannot_fall_back_to_api_key_when_enforcement_is_off() {
    let db = Database::memory().unwrap();
    let fixture = BoundCatalog::new(&db, true).await;
    fixture.change_source();
    assert!(!crate::services::provider_admission::enforcement_enabled(
        &db
    ));
    assert_eq!(
        crate::services::provider_admission::auth_scheme(&db, &fixture.provider).unwrap_err(),
        "provider_subscription_source_changed"
    );
    let calls = fixture.server.requests.lock().unwrap().len();
    assert!(discover_supplier_models(&db, fixture.input())
        .await
        .is_err());
    let refreshed = crate::services::model_discovery::run_refresh(
        &db,
        crate::services::model_discovery::Trigger::Manual,
        true,
        Some(&fixture.provider.id),
    )
    .await
    .unwrap();
    assert!(!refreshed.providers[0].ok);
    assert_eq!(fixture.server.requests.lock().unwrap().len(), calls);
}

#[tokio::test]
async fn retained_runtime_rejects_changed_source_auth_identity_while_manual_provider_remains_usable(
) {
    for clear_observation in [false, true] {
        let db = Database::memory().unwrap();
        let fixture = BoundCatalog::new(&db, true).await;
        if clear_observation {
            let raw = db
                .provider_subscription_value(&fixture.provider.id)
                .unwrap()
                .unwrap();
            let mut source: Value = serde_json::from_str(&raw).unwrap();
            source["observation"] = Value::Null;
            assert!(db
                .compare_exchange_provider_subscription(
                    &fixture.provider.id,
                    Some(&raw),
                    &source.to_string()
                )
                .unwrap());
            assert!(
                crate::services::provider_admission::source_identity(&db, &fixture.provider)
                    .is_some()
            );
            db.set_provider_admission_enforced(true).unwrap();
            assert!(
                crate::services::provider_admission::admitted_models(&db, &fixture.provider)
                    .is_empty(),
                "a source binding without subscription evidence grants no admission"
            );
            db.set_provider_admission_enforced(false).unwrap();
        }
        let manual: ProxyProvider =
            serde_json::from_value(json!({"id":"manual_fixture","name":"Manual",
        "type":"mock","baseURL":"mock://manual","models":["manual-model"],"enabled":true}))
            .unwrap();
        db.save_proxy_provider(&manual).unwrap();
        let config = db.get_proxy_config().unwrap();
        let old = crate::services::proxy_runtime::snapshot::build_runtime_snapshot(&db, &config)
            .await
            .unwrap();
        let request: crate::proxy::canonical::ChatCompletionRequest = serde_json::from_value(
            json!({"model":"tested-model","messages":[{"role":"user","content":"fixture"}]}),
        )
        .unwrap();
        old.router
            .complete_with_provider("messages", "claude", &fixture.provider.id, &request)
            .await
            .unwrap();
        let calls = fixture.server.requests.lock().unwrap().len();
        // Same key bytes and coordinates, but a different source credential field
        // changes the connection's auth contract and invalidates the old snapshot.
        rusqlite::Connection::open(&fixture.source_path).unwrap().execute("UPDATE providers SET settings_config=?1", [json!({"env":{"ANTHROPIC_BASE_URL":fixture.server.base_url,"ANTHROPIC_API_KEY":"fixture_bound_key"}}).to_string()]).unwrap();
        assert!(
            crate::services::proxy_runtime::snapshot::build_runtime_snapshot_from(
                &db,
                &config,
                Some(&old)
            )
            .await
            .is_err()
        );
        assert!(old
            .router
            .complete_with_provider("messages", "claude", &fixture.provider.id, &request)
            .await
            .is_err());
        assert!(discover_supplier_models(&db, fixture.input())
            .await
            .is_err());
        assert_eq!(fixture.server.requests.lock().unwrap().len(), calls);
        let manual_request = crate::proxy::canonical::ChatCompletionRequest {
            model: "manual-model".into(),
            ..request
        };
        old.router
            .complete_with_provider("chat", "manual", &manual.id, &manual_request)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn non_anthropic_catalog_also_rejects_changed_known_source_before_network() {
    let db = Database::memory().unwrap();
    let fixture = BoundCatalog::new(&db, true).await;
    let mut provider = fixture.provider.clone();
    provider.id = "openai_bound_fixture".into();
    provider.provider_type = "openai".into();
    db.save_proxy_provider(&provider).unwrap();
    crate::services::provider_admission::bind_source(
        &db,
        &provider,
        &fixture.source_path,
        "claude_code:catalog_source",
        true,
    )
    .unwrap();
    let scheme = crate::services::provider_admission::auth_scheme(&db, &provider).unwrap();
    assert_eq!(
        fetch_models(&provider, scheme).await.unwrap().models[0].id,
        "listed-model"
    );
    let calls = fixture.server.requests.lock().unwrap().len();
    fixture.change_source();
    assert_eq!(
        crate::services::provider_admission::auth_scheme(&db, &provider).unwrap_err(),
        "provider_subscription_source_changed"
    );
    let refreshed = crate::services::model_discovery::run_refresh(
        &db,
        crate::services::model_discovery::Trigger::Manual,
        true,
        Some(&provider.id),
    )
    .await
    .unwrap();
    assert!(!refreshed.providers[0].ok);
    assert_eq!(
        refreshed.providers[0].error.as_deref(),
        Some("provider_subscription_source_changed")
    );
    assert_eq!(fixture.server.requests.lock().unwrap().len(), calls);
    let report = serde_json::to_string(&refreshed).unwrap();
    assert!(!report.contains("fixture_bound_key"));
    assert!(!report.contains(&fixture.server.base_url));
}

#[tokio::test]
async fn draft_discovery_uses_protocol_url_and_auth_without_persisting() {
    let server = CatalogServer::start(
        StatusCode::OK,
        json!({"data": [{"id": "zeta"}, {"id": "alpha"}, {"id": "zeta"}]}),
    )
    .await;
    let db = Database::memory().unwrap();
    let before = total_changes(&db);
    for (protocol, suffix, path, auth_name, auth_value) in [
        (
            "anthropic",
            "/relay/v1/messages",
            "/relay/v1/models",
            "x-api-key",
            "fixture_shared_key",
        ),
        (
            "openai-chat",
            "/relay/v1/chat/completions",
            "/relay/v1/models",
            "authorization",
            "Bearer fixture_shared_key",
        ),
        (
            "openai-responses",
            "/relay/v1/responses",
            "/relay/v1/models",
            "authorization",
            "Bearer fixture_shared_key",
        ),
        (
            "gemini",
            "/v1beta/models/gemini-pro:generateContent",
            "/v1beta/models",
            "x-goog-api-key",
            "fixture_shared_key",
        ),
    ] {
        let mut input = draft(protocol, &format!("{}{suffix}", server.base_url));
        input.extra_headers = Some([("x-provider-mode".into(), "fixture".into())].into());
        assert_eq!(
            discover_supplier_models(&db, input).await.unwrap(),
            ["alpha", "zeta"]
        );
        let requests = server.requests.lock().unwrap();
        let (request_path, headers) = requests.last().unwrap();
        assert_eq!(request_path, path);
        assert_eq!(headers[auth_name], auth_value);
        assert_eq!(headers["x-provider-mode"], "fixture");
        assert_eq!(
            headers.contains_key("anthropic-version"),
            protocol == "anthropic"
        );
    }
    assert_eq!(total_changes(&db), before);
    assert!(db.get_supplier_snapshot().unwrap().1.is_empty());
    assert!(db.get_all_proxy_providers().unwrap().is_empty());
}

#[tokio::test]
async fn saved_draft_omission_and_explicit_clear_match_save_semantics() {
    let server = CatalogServer::start(StatusCode::OK, json!({"data": []})).await;
    let db = Database::memory().unwrap();
    let save: SupplierSaveInput = serde_json::from_value(json!({
        "expectedGeneration": 0, "name": "Fixture", "baseUrl": server.base_url,
        "apiKey": "fixture_saved_shared", "endpoints": [{
            "protocol": "anthropic", "endpointUrl": format!("{}/override/v1/messages", server.base_url),
            "apiKeyOverride": "fixture_saved_override", "extraHeaders": {"x-fixture": "stored"}
        }]
    })).unwrap();
    let saved = save_supplier(&db, save).unwrap();
    let before = total_changes(&db);
    for (patch, path, key, has_header) in [
        (
            json!({}),
            "/override/v1/models",
            Some("fixture_saved_override"),
            true,
        ),
        (
            json!({"endpointUrl": "", "apiKeyOverride": "", "extraHeaders": {}}),
            "/v1/models",
            None,
            false,
        ),
        (
            json!({"useSharedApiKey": true}),
            "/override/v1/models",
            Some("fixture_saved_shared"),
            true,
        ),
        (
            json!({"useSharedApiKey": true, "apiKey": ""}),
            "/override/v1/models",
            None,
            true,
        ),
    ] {
        let mut input = json!({"supplierId": saved.supplier_id, "protocol": "anthropic"});
        input
            .as_object_mut()
            .unwrap()
            .extend(patch.as_object().unwrap().clone());
        let result = discover_supplier_models(&db, serde_json::from_value(input).unwrap())
            .await
            .unwrap();
        assert!(result.is_empty());
        let requests = server.requests.lock().unwrap();
        let (actual_path, headers) = requests.last().unwrap();
        assert_eq!(actual_path, path);
        assert_eq!(
            headers
                .get("x-api-key")
                .map(|value| value.to_str().unwrap()),
            key
        );
        assert_eq!(headers.contains_key("x-fixture"), has_header);
    }
    assert_eq!(total_changes(&db), before);
    assert_eq!(db.supplier_config_generation().unwrap(), saved.generation);
}

#[tokio::test]
async fn discovery_failures_are_redacted_and_do_not_change_saved_state() {
    let db = Database::memory().unwrap();
    let before = total_changes(&db);
    for (status, body, expected) in [
        (
            StatusCode::UNAUTHORIZED,
            json!({"error": "private-upstream-detail"}),
            "supplier_models_http_error_401",
        ),
        (
            StatusCode::OK,
            json!({"data": [{"id": null}]}),
            "supplier_models_decode_failed",
        ),
    ] {
        let server = CatalogServer::start(status, body).await;
        let error = discover_supplier_models(&db, draft("openai-chat", &server.base_url))
            .await
            .unwrap_err();
        assert_eq!(crate::services::suppliers::ipc_error(error), expected);
    }
    assert_eq!(total_changes(&db), before);
}

#[tokio::test]
async fn invalid_protocol_url_secret_and_self_loop_fail_before_network() {
    let server = CatalogServer::start(StatusCode::OK, json!({"data": []})).await;
    let db = Database::memory().unwrap();
    let mut invalid_secret = draft("anthropic", &server.base_url);
    invalid_secret.api_key = Some("private\r\nheader".into());
    for (input, expected) in [
        (
            draft("not-supported", &server.base_url),
            "supplier_protocol_unsupported",
        ),
        (draft("anthropic", ""), "supplier_endpoint_url_required"),
        (
            draft("anthropic", "http://localhost:8989/v1"),
            "supplier_endpoint_self_loop",
        ),
        (invalid_secret, "supplier_api_key_invalid"),
    ] {
        let error = discover_supplier_models(&db, input).await.unwrap_err();
        assert_eq!(crate::services::suppliers::ipc_error(error), expected);
    }
    assert!(server.requests.lock().unwrap().is_empty());
}
