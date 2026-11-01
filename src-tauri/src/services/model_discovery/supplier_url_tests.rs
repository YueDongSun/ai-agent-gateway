use super::*;
use crate::database::dao::suppliers::SupplierDiscoveryHealth;
use crate::services::suppliers::{get_supplier, save_supplier};
use axum::{extract::State, http::Uri, routing::get, Json, Router};
use serde_json::json;
use std::sync::{Arc, Mutex};

#[tokio::test]
async fn discovers_only_validated_new_url_types_and_repeated_import_is_idempotent() {
    use axum::http::{header::ALLOW, StatusCode};
    let app = Router::new()
        .route(
            "/openapi.json",
            get(|| async {
                Json(json!({
                    "openapi":"3.0.3", "paths":{
                        "/v1/chat/completions":{"post":{}},
                        "/v1/responses":{"post":{}},
                        "/broken/v1/messages":{"post":{}}
                    }
                }))
            }),
        )
        .route(
            "/v1/models",
            get(|| async { Json(json!({"data":[{"id":"shared-model"},{"id":"shared-model"}]})) }),
        )
        .route(
            "/v1/responses",
            get(|| async { (StatusCode::METHOD_NOT_ALLOWED, [(ALLOW, "POST")]) }),
        )
        .route(
            "/broken/v1/messages",
            get(|| async { StatusCode::SERVICE_UNAVAILABLE }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let db = Database::memory().unwrap();
    let saved = save_supplier(&db, serde_json::from_value(json!({
        "expectedGeneration":0,"name":"available fixture","baseUrl":base_url,"apiKey":"fixture_secret",
        "endpoints":[{"protocol":"openai-chat","models":[{"modelId":"shared-model"},{"modelId":"manual-model","enabled":false}]}]
    })).unwrap()).unwrap();
    use crate::database::dao::provider_admission::{
        Admission_evidence, Admission_probe, Admission_state,
    };
    let source = db.get_all_proxy_providers().unwrap().remove(0);
    let evidence = Admission_evidence {
        revision: "fixture_source".into(),
        state: Admission_state::NotRequired,
        valid_until: None,
    };
    let version = db
        .provider_admission_begin(&source, &evidence, "fixture_policy", 0)
        .unwrap();
    db.provider_admission_finish(
        &source,
        &evidence,
        "fixture_policy",
        version,
        Admission_probe {
            probe_model: "shared-model".into(),
            checked_at: chrono::Utc::now().timestamp(),
            eligible: true,
            reason: "accepted".into(),
            latency_ms: Some(1),
            response: Some("ok".into()),
            response_truncated: false,
            response_redacted: false,
            upstream_status: None,
            upstream_code: None,
        },
        vec!["shared-model".into()],
    )
    .unwrap();
    let first = run_refresh(&db, Trigger::Manual, false, None)
        .await
        .unwrap();
    let supplier = get_supplier(&db, &saved.supplier_id)
        .unwrap()
        .supplier
        .unwrap();
    assert_eq!(
        supplier.endpoints.len(),
        2,
        "only the validated Responses endpoint should be appended"
    );
    let responses = supplier
        .endpoints
        .iter()
        .find(|entry| entry.protocol == "openai-responses")
        .unwrap();
    assert_eq!(
        responses.endpoint_url.as_deref(),
        Some(format!("{base_url}/v1/responses").as_str())
    );
    assert_eq!(responses.models.len(), 1);
    assert_eq!(responses.models[0].model_id, "shared-model");
    let chat = supplier
        .endpoints
        .iter()
        .find(|entry| entry.protocol == "openai-chat")
        .unwrap();
    assert!(chat
        .models
        .iter()
        .any(|model| model.model_id == "manual-model" && !model.enabled));
    assert!(first.providers.iter().any(|entry| !entry.ok
        && entry
            .error
            .as_deref()
            .is_some_and(|reason| reason.contains("503"))));
    let generation = db.supplier_config_generation().unwrap();
    let second = run_refresh(&db, Trigger::Manual, false, None)
        .await
        .unwrap();
    assert!(!second.has_changes);
    assert_eq!(db.supplier_config_generation().unwrap(), generation);
    assert_eq!(db.get_supplier_snapshot().unwrap().1.len(), 1);
    assert_eq!(db.get_all_proxy_providers().unwrap().len(), 2);
    let new_provider = db
        .get_all_proxy_providers()
        .unwrap()
        .into_iter()
        .find(|provider| provider.provider_type == "openai-responses")
        .unwrap();
    assert!(db.provider_admission_record(&source.id).unwrap().is_some());
    assert!(
        db.provider_admission_record(&new_provider.id)
            .unwrap()
            .is_none(),
        "a URL must not inherit another connection's receipt"
    );
    assert!(db
        .provider_subscription_value(&new_provider.id)
        .unwrap()
        .is_none());
    db.set_provider_admission_enforced(true).unwrap();
    assert!(crate::services::provider_admission::admitted_models(&db, &new_provider).is_empty());
    assert_eq!(
        get_supplier(&db, &saved.supplier_id)
            .unwrap()
            .supplier
            .unwrap()
            .endpoints
            .len(),
        2
    );
    server.abort();
}

#[tokio::test]
async fn configured_unavailable_suppliers_are_reported_before_any_model_request() {
    let requests = Arc::new(Mutex::new(Vec::<String>::new()));
    let app = Router::new()
        .fallback(get(
            |State(requests): State<Arc<Mutex<Vec<String>>>>, uri: Uri| async move {
                requests.lock().unwrap().push(uri.path().into());
                Json(json!({"data":[{"id":"shared-model"}]}))
            },
        ))
        .with_state(requests.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let db = Database::memory().unwrap();
    let mut suppliers = Vec::new();
    for (name, enabled) in [
        ("disabled", false),
        ("unavailable", true),
        ("expired", true),
        ("obsolete", true),
    ] {
        let saved = save_supplier(
            &db,
            serde_json::from_value(json!({
                "expectedGeneration":db.supplier_config_generation().unwrap(),
                "name":name,"enabled":enabled,"baseUrl":base_url,
                "endpoints":[{"protocol":"openai-chat","models":[{"modelId":"retained-model"}]}]
            }))
            .unwrap(),
        )
        .unwrap();
        suppliers.push(saved.supplier_id);
    }
    let providers = db.get_all_proxy_providers().unwrap();
    for name in ["expired", "obsolete"] {
        use crate::database::dao::provider_admission::{Admission_evidence, Admission_state};
        let mut provider = providers
            .iter()
            .find(|provider| provider.name.starts_with(name))
            .unwrap()
            .clone();
        if name == "obsolete" {
            provider.base_url.push_str("/old");
        }
        db.provider_admission_begin(
            &provider,
            &Admission_evidence {
                revision: "fixture_revision".into(),
                state: Admission_state::Active,
                valid_until: Some(
                    chrono::Utc::now().timestamp() + if name == "expired" { -1 } else { 300 },
                ),
            },
            "fixture_policy",
            0,
        )
        .unwrap();
    }
    let provider = providers
        .iter()
        .find(|provider| provider.name.starts_with("unavailable"))
        .unwrap();
    db.record_supplier_discovery_failure(
        &provider,
        SupplierDiscoveryHealth {
            status: "temporarily_unavailable".into(),
            checked_at: chrono::Utc::now().timestamp(),
            last_success_at: None,
            latency_ms: 0,
            consecutive_failures: 1,
            error: Some("proxy_models_http_error_503".into()),
            listed_model_ids: Vec::new(),
            inference_status: "unverified".into(),
        },
    )
    .unwrap();
    let report = run_refresh(&db, Trigger::Manual, false, None)
        .await
        .unwrap();
    server.abort();
    assert!(
        requests.lock().unwrap().is_empty(),
        "excluded suppliers must not reach catalog or URL discovery"
    );
    assert_eq!(report.providers.len(), 4);
    assert!(report.providers.iter().all(|entry| !entry.ok
        && entry
            .error
            .as_deref()
            .is_some_and(|reason| reason.starts_with("supplier_import_excluded:"))));
    assert!(report.providers.iter().any(|entry| entry
        .error
        .as_deref()
        .unwrap()
        .contains("disabled")));
    assert!(report
        .providers
        .iter()
        .any(|entry| entry.error.as_deref().unwrap().contains("503")));
    assert!(report.providers.iter().any(|entry| entry
        .error
        .as_deref()
        .unwrap()
        .contains("subscription_expired")));
    assert!(report.providers.iter().any(|entry| entry
        .error
        .as_deref()
        .unwrap()
        .contains("configuration_obsolete")));
    for supplier_id in suppliers {
        let supplier = get_supplier(&db, &supplier_id).unwrap().supplier.unwrap();
        assert_eq!(supplier.endpoints[0].models[0].model_id, "retained-model");
    }
}

#[tokio::test]
async fn suppliers_without_endpoints_and_opted_out_entries_have_explicit_results() {
    let db = Database::memory().unwrap();
    for (name, endpoints) in [
        ("empty", json!([])),
        ("opted out", json!([{"protocol":"openai-chat"}])),
    ] {
        save_supplier(
            &db,
            serde_json::from_value(json!({
                "expectedGeneration":db.supplier_config_generation().unwrap(), "name":name,
                "baseUrl":"http://127.0.0.1:1", "endpoints":endpoints
            }))
            .unwrap(),
        )
        .unwrap();
    }
    let mut provider = db.get_all_proxy_providers().unwrap().remove(0);
    provider.auto_track_models = false;
    db.save_proxy_provider(&provider).unwrap();
    let generation = db.supplier_config_generation().unwrap();
    let report = run_refresh(&db, Trigger::Manual, false, None)
        .await
        .unwrap();
    assert_eq!(report.providers.len(), 2);
    assert!(report.providers.iter().any(|entry| entry.ok
        && entry.error.as_deref() == Some("supplier_import_skipped:auto_tracking_disabled")));
    assert!(report.providers.iter().any(|entry| !entry.ok
        && entry.error.as_deref() == Some("supplier_import_excluded:no_configured_endpoint")));
    assert_eq!(db.supplier_config_generation().unwrap(), generation);
}

#[tokio::test]
async fn concurrent_supplier_edit_is_preserved_and_invalidates_url_append() {
    use axum::http::{header::ALLOW, StatusCode};
    let db = Database::memory().unwrap();
    let supplier_id = Arc::new(Mutex::new(String::new()));
    let app =
        Router::new()
            .route(
                "/openapi.json",
                get(|| async {
                    Json(json!({"openapi":"3.0.3","paths":{"/v1/responses":{"post":{}}}}))
                }),
            )
            .route(
                "/v1/models",
                get(|| async { Json(json!({"data":[{"id":"listed"}]})) }),
            )
            .route(
                "/v1/responses",
                get(
                    |State((db, id)): State<(Database, Arc<Mutex<String>>)>| async move {
                        let id = id.lock().unwrap().clone();
                        save_supplier(&db, serde_json::from_value(json!({
                "id":id,"expectedGeneration":db.supplier_config_generation().unwrap(),
                "name":"user edit during validation","enabled":false
            })).unwrap()).unwrap();
                        (StatusCode::METHOD_NOT_ALLOWED, [(ALLOW, "POST")])
                    },
                ),
            )
            .with_state((db.clone(), supplier_id.clone()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let saved = save_supplier(
        &db,
        serde_json::from_value(json!({
            "expectedGeneration":0,"name":"available fixture","baseUrl":base_url,
            "endpoints":[{"protocol":"openai-chat","models":[]}]
        }))
        .unwrap(),
    )
    .unwrap();
    *supplier_id.lock().unwrap() = saved.supplier_id.clone();
    let report = run_refresh(&db, Trigger::Manual, false, None)
        .await
        .unwrap();
    let supplier = get_supplier(&db, &saved.supplier_id)
        .unwrap()
        .supplier
        .unwrap();
    assert!(!supplier.enabled);
    assert_eq!(supplier.name, "user edit during validation");
    assert_eq!(supplier.endpoints.len(), 1);
    assert!(report
        .providers
        .iter()
        .any(|entry| !entry.ok && entry.error.is_some()));
    server.abort();
}

#[tokio::test]
async fn malformed_catalogs_remain_retryable_and_old_unavailable_health_can_recover() {
    let app = Router::new().fallback(get(|| async { Json(json!({"data":[{"id":"recovered"}]})) }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    for (status, age, reason) in [
        ("catalog_invalid", 0, "proxy_models_pagination_invalid"),
        (
            "temporarily_unavailable",
            301,
            "proxy_models_http_error_503",
        ),
    ] {
        let db = Database::memory().unwrap();
        let saved = save_supplier(&db,serde_json::from_value(json!({"expectedGeneration":0,"name":"retry fixture","baseUrl":base_url,"endpoints":[{"protocol":"openai-chat","models":[]}]})).unwrap()).unwrap();
        let provider = db.get_all_proxy_providers().unwrap().remove(0);
        db.record_supplier_discovery_failure(
            &provider,
            SupplierDiscoveryHealth {
                status: status.into(),
                checked_at: chrono::Utc::now().timestamp() - age,
                last_success_at: None,
                latency_ms: 0,
                consecutive_failures: 1,
                error: Some(reason.into()),
                listed_model_ids: Vec::new(),
                inference_status: "unverified".into(),
            },
        )
        .unwrap();
        let report = run_refresh(&db, Trigger::Manual, false, None)
            .await
            .unwrap();
        assert!(report
            .providers
            .iter()
            .any(|entry| entry.ok && entry.plan.added_models == ["recovered"]));
        assert_eq!(
            get_supplier(&db, &saved.supplier_id)
                .unwrap()
                .supplier
                .unwrap()
                .endpoints[0]
                .models[0]
                .model_id,
            "recovered"
        );
    }
    server.abort();
}

struct CapturedLogs(Mutex<Vec<String>>);
static IMPORT_LOGS: CapturedLogs = CapturedLogs(Mutex::new(Vec::new()));
static LOG_INIT: std::sync::Once = std::sync::Once::new();
impl log::Log for CapturedLogs {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }
    fn log(&self, record: &log::Record) {
        let line = record.args().to_string();
        if line.starts_with("supplier_import ") {
            self.0.lock().unwrap().push(line);
        }
    }
    fn flush(&self) {}
}

#[tokio::test]
async fn corrupt_and_failed_supplier_entries_do_not_block_later_imports_and_logs_are_redacted() {
    LOG_INIT.call_once(|| log::set_logger(&IMPORT_LOGS).unwrap());
    let prior_level = log::max_level();
    log::set_max_level(log::LevelFilter::Info);
    IMPORT_LOGS.0.lock().unwrap().clear();
    let requests = Arc::new(Mutex::new(Vec::<String>::new()));
    let app = Router::new()
        .fallback(get(
            |State(requests): State<Arc<Mutex<Vec<String>>>>, uri: Uri| async move {
                requests.lock().unwrap().push(uri.path().into());
                if uri.path().starts_with("/denied/") {
                    (
                        axum::http::StatusCode::UNAUTHORIZED,
                        Json(json!({"error":"secret upstream body"})),
                    )
                } else {
                    (
                        axum::http::StatusCode::OK,
                        Json(json!({"data":[{"id":"shared-model"}]})),
                    )
                }
            },
        ))
        .with_state(requests.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let db = Database::memory().unwrap();
    let mut ids = Vec::new();
    for (index, name) in [
        "corrupt",
        "corrupt_projection",
        "denied",
        "refused_import",
        "available",
    ]
    .iter()
    .enumerate()
    {
        let saved = save_supplier(&db, serde_json::from_value(json!({
            "expectedGeneration":db.supplier_config_generation().unwrap(),"name":name,"priority":index,
            "baseUrl":format!("{base_url}/{name}/v1"),"apiKey":"private_fixture_key",
            "endpoints":[{"protocol":"openai-chat","models":[]}]
        })).unwrap()).unwrap();
        ids.push(saved.supplier_id);
    }
    db.conn
        .lock()
        .unwrap()
        .execute(
            "UPDATE suppliers SET provenance_json='{' WHERE id=?1",
            [&ids[0]],
        )
        .unwrap();
    db.conn.lock().unwrap().execute("UPDATE proxy_providers SET extra_headers='{' WHERE id=(SELECT p.proxy_provider_id FROM supplier_runtime_projections p JOIN supplier_endpoints e ON e.id=p.endpoint_id WHERE e.supplier_id=?1)", [&ids[1]]).unwrap();
    let refused_endpoint = crate::database::dao::suppliers::endpoint_id_for(&ids[3], "openai-chat");
    // The target is generated by the repository's deterministic ID function.
    db.conn.lock().unwrap().execute_batch(&format!("CREATE TRIGGER refuse_catalog BEFORE INSERT ON supplier_models WHEN NEW.endpoint_id='{refused_endpoint}' BEGIN SELECT RAISE(ABORT,'fixture_catalog_write_refused'); END;")).unwrap();
    let report = run_refresh(&db, Trigger::Manual, false, None)
        .await
        .unwrap();
    server.abort();
    assert!(report
        .providers
        .iter()
        .any(|entry| entry.ok && entry.plan.added_models == ["shared-model"]));
    assert!(report
        .providers
        .iter()
        .any(|entry| !entry.ok && entry.error.as_deref() == Some("proxy_models_http_error_401")));
    assert!(report.providers.iter().filter(|entry| !entry.ok).count() >= 3);
    assert!(requests
        .lock()
        .unwrap()
        .iter()
        .all(|path| !path.starts_with("/corrupt/")));
    assert_eq!(
        get_supplier(&db, &ids[4])
            .unwrap()
            .supplier
            .unwrap()
            .endpoints[0]
            .models[0]
            .model_id,
        "shared-model"
    );
    let logs = IMPORT_LOGS.0.lock().unwrap().join("\n");
    assert!(logs.contains(&format!(
        "supplier_ref={}",
        crate::diagnostics::safe_reference(&ids[2])
    )));
    assert!(logs.contains(
        "url_type=openai-chat stage=catalog outcome=failed reason=proxy_models_http_error_401"
    ));
    assert!(logs.contains("stage=import outcome=failed reason=database_error"));
    let report_json = serde_json::to_string(&stored_report(&db).unwrap()).unwrap();
    for secret in [&base_url, "private_fixture_key", "secret upstream body"] {
        assert!(!logs.contains(secret));
        assert!(!report_json.contains(secret));
    }
    log::set_max_level(prior_level);
}

#[tokio::test]
async fn shared_catalog_and_prefix_method_handler_cannot_validate_a_new_protocol() {
    use axum::http::{header::ALLOW, StatusCode};
    use axum::response::IntoResponse;
    let app = Router::new()
        .route(
            "/openapi.json",
            get(|| async {
                Json(json!({"openapi":"3.0.3","paths":{"/v1/responses":{"post":{}}}}))
            }),
        )
        .route(
            "/v1/models",
            get(|| async { Json(json!({"data":[{"id":"listed"}]})) }),
        )
        .fallback(get(|uri: Uri| async move {
            if uri.path().starts_with("/v1/") {
                (StatusCode::METHOD_NOT_ALLOWED, [(ALLOW, "POST")]).into_response()
            } else {
                StatusCode::NOT_FOUND.into_response()
            }
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let db = Database::memory().unwrap();
    let saved = save_supplier(
        &db,
        serde_json::from_value(json!({
            "expectedGeneration":0,"name":"wildcard fixture","baseUrl":base_url,
            "endpoints":[{"protocol":"openai-chat","models":[]}]
        }))
        .unwrap(),
    )
    .unwrap();
    let report = run_refresh(&db, Trigger::Manual, false, None)
        .await
        .unwrap();
    assert_eq!(
        get_supplier(&db, &saved.supplier_id)
            .unwrap()
            .supplier
            .unwrap()
            .endpoints
            .len(),
        1
    );
    assert!(report.providers.iter().any(|entry| entry
        .error
        .as_deref()
        .is_some_and(|error| error.contains("url_validation_route_unproven"))));
    server.abort();
}

#[test]
fn openapi_discovery_respects_server_precedence_origin_and_protocol_identity() {
    let source = reqwest::Url::parse("https://configured.example/openapi.json").unwrap();
    let document = json!({"openapi":"3.1.0","servers":[{"url":"/api"}],"paths":{
        "/responses":{"post":{}},
        "/messages":{"servers":[{"url":"https://unconfigured.example"}],"post":{}},
        "/chat/completions":{"servers":[{"url":"/ignored"}],"post":{"servers":[{"url":"/v2"}]}},
        "/models/{model}:generateContent":{"post":{"servers":[{"url":"/v1beta"}]}},
        "/unknown":{"post":{}},"/admin/{tenant}/messages":{"post":{}}
    }});
    let candidates = super::url_candidates::parse_candidates(&document, &source).unwrap();
    let actual: Vec<_> = candidates
        .iter()
        .map(|entry| (entry.protocol, entry.url.as_str()))
        .collect();
    assert_eq!(
        actual,
        [
            (
                "openai-responses",
                "https://configured.example/api/responses"
            ),
            (
                "openai-chat",
                "https://configured.example/v2/chat/completions"
            ),
            ("gemini", "https://configured.example/v1beta"),
        ]
    );
}

#[tokio::test]
async fn gemini_catalog_model_slots_cannot_escape_to_another_protocol_route() {
    use axum::http::{header::ALLOW, StatusCode};
    use axum::response::IntoResponse;
    let requests = Arc::new(Mutex::new(Vec::<String>::new()));
    let app=Router::new().fallback(get(|State(requests):State<Arc<Mutex<Vec<String>>>>,uri:Uri| async move {
        requests.lock().unwrap().push(uri.path().into());
        match uri.path() {
            "/openapi.json" => Json(json!({"openapi":"3.0.3","paths":{"/v1/models/{model}:generateContent":{"post":{}}}})).into_response(),
            "/v1/models" => Json(json!({"data":[{"id":"../responses?ignored="}]})).into_response(),
            "/v1/responses" => (StatusCode::METHOD_NOT_ALLOWED,[(ALLOW,"POST")]).into_response(),
            _ => StatusCode::NOT_FOUND.into_response(),
        }
    })).with_state(requests.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let db = Database::memory().unwrap();
    let saved=save_supplier(&db,serde_json::from_value(json!({"expectedGeneration":0,"name":"model slot fixture","baseUrl":base_url,"endpoints":[{"protocol":"openai-chat","models":[]}]})).unwrap()).unwrap();
    let report = run_refresh(&db, Trigger::Manual, false, None)
        .await
        .unwrap();
    assert!(
        requests
            .lock()
            .unwrap()
            .iter()
            .all(|path| path != "/v1/responses"),
        "a catalog ID must not select a different route"
    );
    assert_eq!(
        get_supplier(&db, &saved.supplier_id)
            .unwrap()
            .supplier
            .unwrap()
            .endpoints
            .len(),
        1
    );
    assert!(report.providers.iter().any(|entry| entry
        .error
        .as_deref()
        .is_some_and(|error| error.contains("url_validation_model_path_invalid"))));
    server.abort();
}

#[tokio::test]
async fn supplier_revalidation_isolates_corrupt_selected_and_unrelated_configuration() {
    let app = Router::new().fallback(get(|| async { Json(json!({"data":[{"id":"listed"}]})) }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let db = Database::memory().unwrap();
    let mut ids = Vec::new();
    for name in ["corrupt", "available"] {
        ids.push(save_supplier(&db,serde_json::from_value(json!({"expectedGeneration":db.supplier_config_generation().unwrap(),"name":name,"baseUrl":base_url,"endpoints":[{"protocol":"openai-chat","models":[]}]})).unwrap()).unwrap().supplier_id);
    }
    db.conn
        .lock()
        .unwrap()
        .execute(
            "UPDATE suppliers SET provenance_json='{' WHERE id=?1",
            [&ids[0]],
        )
        .unwrap();
    assert!(crate::services::suppliers::revalidate_suppliers(&db, &ids)
        .await
        .is_err());
    assert_eq!(
        get_supplier(&db, &ids[1])
            .unwrap()
            .supplier
            .unwrap()
            .endpoints[0]
            .models[0]
            .model_id,
        "listed"
    );
    crate::services::suppliers::revalidate_suppliers(&db, &ids[1..])
        .await
        .unwrap();
    server.abort();
}

#[tokio::test]
async fn supplier_revalidation_timeout_does_not_cancel_later_suppliers() {
    let app = Router::new().fallback(get(|uri: Uri| async move {
        if uri.path().starts_with("/slow/") {
            tokio::time::sleep(std::time::Duration::from_secs(16)).await;
        }
        Json(json!({"data":[{"id":"listed"}]}))
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let db = Database::memory().unwrap();
    let mut ids = Vec::new();
    for (index, path) in ["slow", "slow", "available"].iter().enumerate() {
        ids.push(save_supplier(&db,serde_json::from_value(json!({"expectedGeneration":db.supplier_config_generation().unwrap(),"name":format!("fixture {index}"),"baseUrl":format!("{base_url}/{path}/v1"),"endpoints":[{"protocol":"openai-chat","models":[]}]})).unwrap()).unwrap().supplier_id);
    }
    let error = crate::services::suppliers::revalidate_suppliers(&db, &ids)
        .await
        .unwrap_err();
    assert!(error.contains("timeout"));
    assert_eq!(
        get_supplier(&db, &ids[2])
            .unwrap()
            .supplier
            .unwrap()
            .endpoints[0]
            .models[0]
            .model_id,
        "listed"
    );
    server.abort();
}

#[tokio::test]
async fn all_configured_sources_and_both_metadata_documents_are_searched() {
    use axum::http::{header::ALLOW, StatusCode};
    let first_app = Router::new().route(
        "/v1/models",
        get(|| async { Json(json!({"data":[{"id":"listed"}]})) }),
    );
    let first_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let first_url = format!("http://{}", first_listener.local_addr().unwrap());
    let first_server =
        tokio::spawn(async move { axum::serve(first_listener, first_app).await.unwrap() });
    let second_app = Router::new()
        .route(
            "/openapi.json",
            get(|| async { Json(json!({"openapi":"3.0.3","paths":{"/admin/status":{"get":{}}}})) }),
        )
        .route(
            "/tenant/v1/openapi.json",
            get(|| async {
                Json(json!({"openapi":"3.0.3","paths":{"/tenant/v1/responses":{"post":{}}}}))
            }),
        )
        .route(
            "/tenant/v1/models",
            get(|| async { Json(json!({"data":[{"id":"listed"}]})) }),
        )
        .route(
            "/tenant/v1/responses",
            get(|| async { (StatusCode::METHOD_NOT_ALLOWED, [(ALLOW, "POST")]) }),
        );
    let second_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let second_url = format!("http://{}/tenant/v1", second_listener.local_addr().unwrap());
    let second_server =
        tokio::spawn(async move { axum::serve(second_listener, second_app).await.unwrap() });
    for additional_source in [false, true] {
        let db = Database::memory().unwrap();
        let mut endpoints = vec![
            json!({"protocol":"openai-chat","endpointUrl":second_url,"priority":1,"models":[]}),
        ];
        if additional_source {
            endpoints.push(
                json!({"protocol":"anthropic","endpointUrl":first_url,"priority":0,"models":[]}),
            );
        }
        let saved = save_supplier(&db,serde_json::from_value(json!({"expectedGeneration":0,"name":"multi source","baseUrl":first_url,"endpoints":endpoints})).unwrap()).unwrap();
        if additional_source {
            assert_eq!(
                db.get_all_proxy_providers().unwrap()[0].provider_type,
                "anthropic"
            );
        }
        run_refresh(&db, Trigger::Manual, false, None)
            .await
            .unwrap();
        let supplier = get_supplier(&db, &saved.supplier_id)
            .unwrap()
            .supplier
            .unwrap();
        assert!(
            supplier
                .endpoints
                .iter()
                .any(|endpoint| endpoint.protocol == "openai-responses"),
            "the useful configured API description must not be hidden"
        );
    }
    first_server.abort();
    second_server.abort();
}

#[tokio::test]
async fn gemini_uses_the_exact_declared_route_and_can_share_an_existing_api_root() {
    use axum::http::{header::ALLOW, StatusCode};
    let requests = Arc::new(Mutex::new(Vec::<String>::new()));
    let app = Router::new()
        .fallback(get(
            |State(requests): State<Arc<Mutex<Vec<String>>>>, uri: Uri| async move {
                use axum::response::IntoResponse;
                requests.lock().unwrap().push(uri.path().into());
                match uri.path() {
                    "/openapi.json" => Json(json!({"openapi":"3.0.3","paths":{
                        "/custom/models/{model}:generateContent":{"post":{}},
                        "/v1/models/{model}:generateContent":{"post":{}}
                    }}))
                    .into_response(),
                    "/v1/models" | "/custom/v1beta/models" => {
                        Json(json!({"data":[{"id":"listed"}]})).into_response()
                    }
                    "/v1/models/listed:generateContent"
                    | "/custom/v1beta/models/listed:generateContent" => {
                        (StatusCode::METHOD_NOT_ALLOWED, [(ALLOW, "POST")]).into_response()
                    }
                    _ => StatusCode::NOT_FOUND.into_response(),
                }
            },
        ))
        .with_state(requests.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let db = Database::memory().unwrap();
    let saved=save_supplier(&db,serde_json::from_value(json!({"expectedGeneration":0,"name":"shared root","baseUrl":base_url,"endpoints":[{"protocol":"openai-chat","models":[]}]})).unwrap()).unwrap();
    let report = run_refresh(&db, Trigger::Manual, false, None)
        .await
        .unwrap();
    let supplier = get_supplier(&db, &saved.supplier_id)
        .unwrap()
        .supplier
        .unwrap();
    let gemini = supplier
        .endpoints
        .iter()
        .find(|endpoint| endpoint.protocol == "gemini")
        .expect("validated Gemini protocol at the same root");
    assert!(
        gemini.endpoint_url.is_none(),
        "reuse the supplier base instead of duplicating its URL"
    );
    assert!(
        requests
            .lock()
            .unwrap()
            .iter()
            .all(|path| !path.starts_with("/custom/")),
        "never validate a rewritten undeclared route"
    );
    assert!(report.providers.iter().any(|entry| entry
        .error
        .as_deref()
        .is_some_and(|error| error.contains("url_validation_runtime_path_mismatch"))));
    server.abort();
}
