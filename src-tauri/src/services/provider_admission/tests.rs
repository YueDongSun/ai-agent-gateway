//! Owner-level integration with disposable CC Switch state and loopback only.
use super::*;
use crate::services::provider_screening::{ScreeningReason, SCREENING_CONFIG_KEY};
use axum::{extract::State, routing::post, Json, Router};
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

const SOURCE_REF: &str = "claude_code:source";
const MODEL: &str = "fixture-model";

struct Local_fixture {
    _root: crate::config::private_fs::PrivateTempDir,
    path: PathBuf,
    base_url: String,
    reply: Arc<Mutex<(String, u64)>>,
    inference_calls: Arc<AtomicUsize>,
    status_calls: Arc<AtomicUsize>,
    pause: Arc<Mutex<Option<Arc<tokio::sync::Notify>>>>,
    entered: Arc<tokio::sync::Notify>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Local_fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Local_fixture {
    async fn new() -> Self {
        let reply = Arc::new(Mutex::new(("ok".to_string(), 0u64)));
        let inference_calls = Arc::new(AtomicUsize::new(0));
        let status_calls = Arc::new(AtomicUsize::new(0));
        let pause = Arc::new(Mutex::new(None::<Arc<tokio::sync::Notify>>));
        let entered = Arc::new(tokio::sync::Notify::new());
        let state = (reply.clone(), inference_calls.clone(), pause.clone(), entered.clone());
        let other_calls = status_calls.clone();
        let app = Router::new().route("/v1/messages", post(
            |State((reply, calls, pause, entered)): State<(Arc<Mutex<(String, u64)>>, Arc<AtomicUsize>, Arc<Mutex<Option<Arc<tokio::sync::Notify>>>>, Arc<tokio::sync::Notify>)>, Json(body): Json<Value>| async move {
                calls.fetch_add(1, Ordering::SeqCst);
                let gate = pause.lock().unwrap().clone();
                entered.notify_one();
                if let Some(gate) = gate { gate.notified().await; }
                assert_eq!(body["messages"], json!([{"role":"user","content":[{"type":"text","text":"Reply with exactly: ok"}]}]));
                assert!(body.get("system").is_none(), "screening must never attach business context");
                let (text, delay_ms) = reply.lock().unwrap().clone();
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                Json(json!({"id":"fixture","type":"message","role":"assistant","model":body["model"],
                    "content":[{"type":"text","text":text}],"stop_reason":"end_turn",
                    "usage":{"input_tokens":1,"output_tokens":1}}))
            })).fallback(move || { let calls = other_calls.clone(); async move {
                calls.fetch_add(1, Ordering::SeqCst); axum::http::StatusCode::NOT_FOUND
            }}).with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let root = crate::config::private_fs::private_temp_dir(
            &std::env::temp_dir().canonicalize().unwrap(),
        )
        .unwrap();
        let path = root.path().join("cc_switch.db");
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE providers(id TEXT,app_type TEXT,name TEXT,settings_config TEXT,website_url TEXT,category TEXT,created_at INTEGER,sort_index INTEGER,notes TEXT,icon TEXT,icon_color TEXT,meta TEXT,is_current INTEGER,in_failover_queue INTEGER);").unwrap();
        let settings =
            json!({"env":{"ANTHROPIC_BASE_URL":base_url,"ANTHROPIC_AUTH_TOKEN":"fixture_key"}});
        conn.execute("INSERT INTO providers(id,app_type,name,settings_config,meta,is_current,in_failover_queue) VALUES('source','claude','Fixture',?1,'{}',0,0)", [settings.to_string()]).unwrap();
        Self {
            _root: root,
            path,
            base_url,
            reply,
            inference_calls,
            status_calls,
            pause,
            entered,
            task,
        }
    }

    fn provider(&self, id: &str) -> ProxyProvider {
        serde_json::from_value(
            json!({"id":id,"name":"Fixture","type":"anthropic","baseURL":self.base_url,
            "apiKey":"fixture_key","models":[MODEL,"fixture-second",MODEL],"enabled":true}),
        )
        .unwrap()
    }

    async fn screen(
        &self,
        db: &Database,
        provider: &ProxyProvider,
        explicit: bool,
    ) -> crate::services::provider_screening::ScreeningReport {
        screen_bound_provider(db, provider, MODEL, &self.path, SOURCE_REF, true, explicit)
            .await
            .unwrap()
    }

    fn change_metadata(&self) {
        rusqlite::Connection::open(&self.path)
            .unwrap()
            .execute(
                "UPDATE providers SET meta=?1",
                [json!({"usage_script":{"enabled":false}}).to_string()],
            )
            .unwrap();
    }
}

fn database() -> Database {
    let db = Database::memory().unwrap();
    db.set_provider_admission_enforced(true).unwrap();
    db
}

#[tokio::test]
async fn unconfigured_probe_model_is_rejected_before_any_socket() {
    let fixture = Local_fixture::new().await;
    let db = database();
    let provider = fixture.provider("fixture-model-boundary");
    db.save_proxy_provider(&provider).unwrap();
    let result = screen_bound_provider(
        &db,
        &provider,
        "not-an-offered-model",
        &fixture.path,
        SOURCE_REF,
        true,
        true,
    )
    .await;
    assert!(result.is_err());
    assert_eq!(fixture.inference_calls.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.status_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn accepted_probe_preserves_distinct_provider_model_pairs_without_reprobing_offers() {
    let fixture = Local_fixture::new().await;
    let db = database();
    let mut pairs = BTreeSet::new();
    for id in ["fixture-first", "fixture-second"] {
        let provider = fixture.provider(id);
        db.save_proxy_provider(&provider).unwrap();
        let report = fixture.screen(&db, &provider, true).await;
        assert!(report.eligible);
        assert_eq!(report.response.as_deref(), Some("ok"));
        let models = admitted_models(&db, &provider);
        assert_eq!(models.len(), 2);
        for model in models {
            ensure_provider_admitted(&db, &provider, &model)
                .await
                .unwrap();
            pairs.insert((id.to_string(), model));
        }
        let receipt = db.provider_admission_record(id).unwrap().unwrap();
        assert_eq!(receipt.probe.unwrap().probe_model, MODEL);
    }
    assert_eq!(pairs.len(), 4);
    assert_eq!(fixture.inference_calls.load(Ordering::SeqCst), 2);
    assert_eq!(fixture.status_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn registered_recheck_of_a_missing_target_does_not_import_or_enable_screening() {
    let fixture = Local_fixture::new().await;
    let db = Database::memory().unwrap();
    let other = fixture.provider("unselected-fixture");
    db.save_proxy_provider(&other).unwrap();
    let before = db.get_all_proxy_providers().unwrap();
    assert_eq!(super::review::recheck_registered(&db, "removed-target").await.unwrap_err(), "provider_admission_missing");
    assert_eq!(db.get_all_proxy_providers().unwrap(), before);
    assert!(!enforcement_enabled(&db));
    assert_eq!(fixture.inference_calls.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.status_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn registered_recheck_preflight_preserves_policy_and_unselected_availability() {
    let fixture = Local_fixture::new().await;
    for enforced in [false, true] {
        for missing_model in [false, true] {
            let db = Database::memory().unwrap();
            db.set_provider_admission_enforced(enforced).unwrap();
            let mut selected = fixture.provider("selected_preflight");
            if missing_model {
                selected.models.clear();
            }
            let other = fixture.provider("unselected_preflight");
            db.save_proxy_provider(&selected).unwrap();
            db.save_proxy_provider(&other).unwrap();
            let providers_before = db.get_all_proxy_providers().unwrap();
            let availability_before = admitted_models(&db, &other);

            assert!(review::recheck_registered(&db, &selected.id).await.is_err());

            assert_eq!(enforcement_enabled(&db), enforced);
            assert_eq!(db.get_all_proxy_providers().unwrap(), providers_before);
            assert_eq!(admitted_models(&db, &other), availability_before);
            assert!(db.provider_admission_record(&selected.id).unwrap().is_none());
            assert!(db.provider_subscription_value(&selected.id).unwrap().is_none());
        }
    }
    assert_eq!(fixture.inference_calls.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.status_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn provisional_recheck_checks_only_selected_source_without_registration() {
    for reply in ["ok", "ineligible fixture reply"] {
        let fixture = Local_fixture::new().await;
        let db = Database::memory().unwrap();
        let source_settings = json!({"env": {
            "ANTHROPIC_BASE_URL": fixture.base_url,
            "ANTHROPIC_AUTH_TOKEN": "fixture_key", "ANTHROPIC_MODEL": MODEL
        }});
        let other_settings = json!({"env": {
            "ANTHROPIC_BASE_URL": fixture.base_url,
            "ANTHROPIC_AUTH_TOKEN": "fixture_other_key", "ANTHROPIC_MODEL": MODEL
        }});
        let source = rusqlite::Connection::open(&fixture.path).unwrap();
        source.execute("UPDATE providers SET settings_config=?1", [source_settings.to_string()]).unwrap();
        source.execute("INSERT INTO providers(id,app_type,name,settings_config,meta,is_current,in_failover_queue) VALUES('other','claude','Other fixture',?1,'{}',0,0)", [other_settings.to_string()]).unwrap();
        drop(source);
        let preview = crate::services::import_cc_switch::preview_import(&db, &fixture.path).unwrap();
        let screened = crate::services::import_cc_switch::screen_import(
            &db, &fixture.path, &preview.plan_id,
            &[SOURCE_REF.into(), "claude_code:other".into()],
        ).await.unwrap();
        assert_eq!(screened.len(), 2);
        assert!(screened.iter().all(|result| result.eligible));
        let selected = screened.iter().find(|result| result.source_record_ref == SOURCE_REF).unwrap();
        let other = screened.iter().find(|result| result.source_record_ref != SOURCE_REF).unwrap();
        let other_before = serde_json::to_value(db.provider_admission_record(&other.provider_id).unwrap()).unwrap();
        let other_source_before = db.provider_subscription_value(&other.provider_id).unwrap();
        let suppliers_before = db.get_supplier_snapshot().unwrap().0;
        let calls_before = fixture.inference_calls.load(Ordering::SeqCst);
        *fixture.reply.lock().unwrap() = (reply.into(), 0);

        review::recheck(&db, &selected.provider_id).await.unwrap();

        assert!(db.get_all_proxy_providers().unwrap().is_empty());
        assert_eq!(db.get_supplier_snapshot().unwrap().0, suppliers_before);
        assert_eq!(serde_json::to_value(db.provider_admission_record(&other.provider_id).unwrap()).unwrap(), other_before);
        assert_eq!(db.provider_subscription_value(&other.provider_id).unwrap(), other_source_before);
        assert_eq!(fixture.inference_calls.load(Ordering::SeqCst), calls_before + 1);
        assert_eq!(fixture.status_calls.load(Ordering::SeqCst), 0);
        let selected_record = db.provider_admission_record(&selected.provider_id).unwrap().unwrap();
        assert_eq!(selected_record.probe.unwrap().eligible, reply == "ok");
    }
}

#[tokio::test]
async fn recheck_receipt_preserves_preflight_and_reports_concurrent_policy_separately() {
    let fixture = Local_fixture::new().await;
    let db = Database::memory().unwrap();
    let provider = fixture.provider("receipt_preflight");
    db.save_proxy_provider(&provider).unwrap();
    let outcome = review::recheck_outcome(&db, &provider.id).await;
    assert!(!outcome.check_started);
    assert_eq!(outcome.check_error.as_deref(), Some("provider_subscription_source_missing"));
    assert!(!outcome.changes.source_committed);
    assert!(!outcome.changes.admission_committed);
    // A separate operator changes policy after this operation's preflight.
    db.set_provider_admission_enforced(true).unwrap();
    let calls = AtomicUsize::new(0);
    let receipt = crate::commands::provider_screening::complete_recheck(&db, outcome, async {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }).await;
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(!receipt.outcome.changes.policy_changed);
    assert!(!receipt.outcome.changes.registration_changed);
    assert_eq!(receipt.runtime_reconciled, None);
    assert!(receipt.overview.unwrap().enforced);
    assert_eq!(fixture.inference_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn recheck_receipt_retains_commits_and_overview_after_finish_and_runtime_failure() {
    let fixture = Local_fixture::new().await;
    let db = database();
    let provider = fixture.provider("receipt_partial");
    db.save_proxy_provider(&provider).unwrap();
    assert!(fixture.screen(&db, &provider, true).await.eligible);
    {
        let connection = db.conn.lock().unwrap();
        connection.execute_batch(r#"CREATE TRIGGER fail_recheck_finish BEFORE UPDATE ON app_meta
            WHEN NEW.key GLOB 'provider_admission:record:*' AND NEW.value LIKE '%"probe":{%'
            BEGIN SELECT RAISE(FAIL, 'private fixture finish error'); END;"#).unwrap();
    }
    let outcome = review::recheck_outcome(&db, &provider.id).await;
    assert!(outcome.check_started);
    assert!(outcome.changes.admission_committed);
    assert!(outcome.changes.source_committed);
    assert!(outcome.admission_version.is_some());
    assert_eq!(outcome.check_error.as_deref(), Some("provider_admission_conflict"));
    let reconciliations = AtomicUsize::new(0);
    let receipt = crate::commands::provider_screening::complete_recheck(&db, outcome, async {
        reconciliations.fetch_add(1, Ordering::SeqCst);
        Err("private fixture runtime error".into())
    }).await;
    assert_eq!(reconciliations.load(Ordering::SeqCst), 1);
    assert_eq!(receipt.runtime_reconciled, Some(false));
    assert_eq!(receipt.runtime_error.as_deref(), Some("provider_screening_runtime_reconcile_failed"));
    assert!(receipt.overview_error.is_none());
    let overview = receipt.overview.as_ref().unwrap();
    assert!(overview.enforced);
    assert!(overview.entries.iter().find(|entry| entry.provider_id == provider.id).unwrap().admitted_models.is_empty());
    let serialized = serde_json::to_string(&receipt).unwrap();
    assert!(!serialized.contains("private fixture"));
    assert!(!serialized.contains("fixture_key"));
}

#[tokio::test]
async fn recheck_selects_nonempty_offer_and_unknown_subscription_never_sends_inference() {
    let fixture = Local_fixture::new().await;
    let db = Database::memory().unwrap();
    let mut provider = fixture.provider("receipt_model");
    provider.models.insert(0, String::new());
    db.save_proxy_provider(&provider).unwrap();
    source_cache::bind_source(&db, &provider, &fixture.path, SOURCE_REF, false).unwrap();
    let outcome = review::recheck_outcome(&db, &provider.id).await;
    assert!(outcome.check_started);
    assert_eq!(outcome.eligible, Some(false));
    assert!(outcome.check_error.is_none());
    assert!(!enforcement_enabled(&db));
    assert_eq!(fixture.inference_calls.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.status_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn manual_provider_test_requires_source_without_inference_or_policy_mutation() {
    let fixture = Local_fixture::new().await;
    let db = Database::memory().unwrap();
    let provider = fixture.provider("manual_test_prerequisite");
    db.save_proxy_provider(&provider).unwrap();
    assert_eq!(test_provider(&db, &provider, MODEL).await.unwrap_err(), "provider_subscription_source_missing");
    assert!(!enforcement_enabled(&db));
    assert!(db.provider_admission_record(&provider.id).unwrap().is_none());
    assert!(db.provider_subscription_value(&provider.id).unwrap().is_none());
    assert_eq!(fixture.inference_calls.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.status_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn recheck_coordinator_blocks_duplicate_probe_but_allows_independent_provider_and_later_retry() {
    let fixture = Local_fixture::new().await;
    let db = Database::memory().unwrap();
    let coordinator = Arc::new(review::Review_coordinator::default());
    let selected = fixture.provider("single_flight_selected");
    let independent = fixture.provider("single_flight_independent");
    for provider in [&selected, &independent] {
        db.save_proxy_provider(provider).unwrap();
        source_cache::bind_source(&db, provider, &fixture.path, SOURCE_REF, true).unwrap();
    }
    let gate = Arc::new(tokio::sync::Notify::new());
    *fixture.pause.lock().unwrap() = Some(gate.clone());
    let first_db = db.clone();
    let first_coordinator = coordinator.clone();
    let selected_id = selected.id.clone();
    let first = tokio::spawn(async move {
        crate::commands::provider_screening::run_recheck(&first_db, &first_coordinator, &selected_id, async { Ok(()) }).await
    });
    tokio::time::timeout(Duration::from_secs(5), fixture.entered.notified()).await.unwrap();
    *fixture.pause.lock().unwrap() = None;
    let duplicate = crate::commands::provider_screening::run_recheck(&db, &coordinator, &selected.id, async { Ok(()) }).await;
    assert!(!duplicate.outcome.check_started);
    assert_eq!(duplicate.outcome.check_error.as_deref(), Some("provider_admission_busy"));
    assert!(!duplicate.outcome.changes.admission_committed);
    assert_eq!(fixture.inference_calls.load(Ordering::SeqCst), 1);
    let independent_result = crate::commands::provider_screening::run_recheck(&db, &coordinator, &independent.id, async { Ok(()) }).await;
    assert_eq!(independent_result.outcome.eligible, Some(true));
    assert_eq!(fixture.inference_calls.load(Ordering::SeqCst), 2);
    gate.notify_one();
    let first_result = tokio::time::timeout(Duration::from_secs(5), first).await.unwrap().unwrap();
    assert_eq!(first_result.outcome.eligible, Some(true));
    let later = crate::commands::provider_screening::run_recheck(&db, &coordinator, &selected.id, async { Ok(()) }).await;
    assert_eq!(later.outcome.eligible, Some(true));
    assert_eq!(fixture.inference_calls.load(Ordering::SeqCst), 3);
}

#[test]
fn recheck_permit_releases_on_drop_and_preserves_other_provider_ownership() {
    let coordinator = review::Review_coordinator::default();
    let first = coordinator.try_begin("first").unwrap();
    let second = coordinator.try_begin("second").unwrap();
    assert!(coordinator.try_begin("first").is_err());
    drop(first);
    assert!(coordinator.try_begin("first").is_ok());
    assert!(coordinator.try_begin("second").is_err());
    drop(second);
    assert!(coordinator.try_begin("second").is_ok());
}

#[tokio::test]
async fn recheck_receipt_reports_unavailable_overview_without_inventing_success() {
    let db = Database::memory().unwrap();
    let outcome = review::rejected_recheck("fixture_missing", "provider_admission_missing");
    db.set_setting(SCREENING_CONFIG_KEY, "invalid fixture policy").unwrap();
    let receipt = crate::commands::provider_screening::complete_recheck(&db, outcome, async {
        panic!("preflight rejection must not reconcile runtime");
        #[allow(unreachable_code)]
        Ok(())
    }).await;
    assert!(receipt.overview.is_none());
    assert_eq!(receipt.overview_error.as_deref(), Some("provider_screening_policy_invalid"));
    assert_eq!(receipt.runtime_reconciled, None);
    assert!(!serde_json::to_string(&receipt).unwrap().contains("invalid fixture policy"));
}

#[tokio::test]
async fn prior_bare_ok_receipt_is_not_admission_for_the_instruction_contract() {
    let fixture = Local_fixture::new().await;
    let db = database();
    let provider = fixture.provider("fixture-contract");
    db.save_proxy_provider(&provider).unwrap();
    assert!(fixture.screen(&db, &provider, true).await.eligible);
    let record = db.provider_admission_record(&provider.id).unwrap().unwrap();
    let legacy_policy = "v1:5000:15000:64:1024";
    let version = db.provider_admission_begin(&provider, &record.evidence, legacy_policy, record.version).unwrap();
    let mut probe = record.probe.unwrap();
    probe.checked_at = chrono::Utc::now().timestamp();
    db.provider_admission_finish(&provider, &record.evidence, legacy_policy, version, probe, record.offered_models).unwrap();
    assert!(admitted_models(&db, &provider).is_empty());
    assert!(ensure_provider_admitted(&db, &provider, MODEL).await.is_err());
    assert_eq!(fixture.inference_calls.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.status_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn non_ok_slow_and_timeout_probes_remain_excluded_with_matching_receipts() {
    let fixture = Local_fixture::new().await;
    for (reply, delay, latency, deadline, expected) in [
        (
            "actual non-ok fixture reply",
            0,
            5000,
            15000,
            ScreeningReason::AbnormalResponse,
        ),
        ("ok", 150, 20, 2000, ScreeningReason::LatencyExceeded),
        ("ok", 250, 10, 20, ScreeningReason::Timeout),
    ] {
        let db = database();
        let provider = fixture.provider("fixture-rejected");
        db.save_proxy_provider(&provider).unwrap();
        db.set_setting(SCREENING_CONFIG_KEY, &json!({"max_latency_ms":latency,"probe_deadline_ms":deadline,"max_output_tokens":64,"max_response_chars":1024}).to_string()).unwrap();
        *fixture.reply.lock().unwrap() = (reply.into(), delay);
        let report = fixture.screen(&db, &provider, true).await;
        assert!(!report.eligible);
        assert_eq!(report.reason, expected);
        assert!(admitted_models(&db, &provider).is_empty());
        let record = db.provider_admission_record(&provider.id).unwrap().unwrap();
        assert!(record.offered_models.is_empty());
        let probe = record.probe.unwrap();
        assert!(!probe.eligible);
        assert_eq!(probe.response, report.response);
        if expected == ScreeningReason::AbnormalResponse {
            assert_eq!(probe.response.as_deref(), Some(reply));
        }
    }
    assert_eq!(fixture.inference_calls.load(Ordering::SeqCst), 3);
    assert_eq!(fixture.status_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn source_changes_and_policy_tightening_revoke_admission_without_inference() {
    let fixture = Local_fixture::new().await;
    let db = database();
    let provider = fixture.provider("fixture-revoked");
    db.save_proxy_provider(&provider).unwrap();
    assert!(fixture.screen(&db, &provider, true).await.eligible);
    db.set_setting(SCREENING_CONFIG_KEY, &json!({"max_latency_ms":1,"probe_deadline_ms":15000,"max_output_tokens":64,"max_response_chars":1024}).to_string()).unwrap();
    assert!(admitted_models(&db, &provider).is_empty());
    assert!(ensure_provider_admitted(&db, &provider, MODEL)
        .await
        .is_err());
    // Explicit recheck of the same locally asserted source can produce a new receipt.
    db.set_setting(
        SCREENING_CONFIG_KEY,
        &serde_json::to_string(&ScreeningConfig::default()).unwrap(),
    )
    .unwrap();
    assert!(fixture.screen(&db, &provider, true).await.eligible);
    fixture.change_metadata();
    assert!(admitted_models(&db, &provider).is_empty());
    assert_eq!(
        ensure_provider_admitted(&db, &provider, MODEL)
            .await
            .unwrap_err(),
        "provider_subscription_source_changed"
    );
    assert_eq!(fixture.inference_calls.load(Ordering::SeqCst), 2);
    assert_eq!(fixture.status_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn cached_inactive_sources_never_probe_and_only_explicit_local_recheck_can_renew() {
    let fixture = Local_fixture::new().await;
    let db = database();
    let provider = fixture.provider("fixture-inactive");
    db.save_proxy_provider(&provider).unwrap();
    assert!(fixture.screen(&db, &provider, true).await.eligible);
    // Simulate a previously stored authoritative inactive observation through
    // the private source-owner CAS seam, keeping its exact source binding.
    let old = db
        .provider_subscription_value(&provider.id)
        .unwrap()
        .unwrap();
    let mut revoked: Value = serde_json::from_str(&old).unwrap();
    revoked["observation"]["evidence"]["state"] = json!("inactive");
    revoked["observation"]["probeAllowed"] = json!(false);
    revoked["observation"]["reason"] = json!("no_subscription");
    assert!(db
        .compare_exchange_provider_subscription(&provider.id, Some(&old), &revoked.to_string())
        .unwrap());
    assert!(ensure_provider_admitted(&db, &provider, MODEL)
        .await
        .is_err());
    assert!(ensure_catalog_admitted(&db, &provider).await.is_err());
    let blocked = fixture.screen(&db, &provider, false).await;
    assert_eq!(blocked.reason, ScreeningReason::SubscriptionInactive);
    assert!(!blocked.eligible);
    assert!(admitted_models(&db, &provider).is_empty());
    assert_eq!(fixture.inference_calls.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.status_calls.load(Ordering::SeqCst), 0);
    assert!(fixture.screen(&db, &provider, true).await.eligible);
    assert_eq!(admitted_models(&db, &provider).len(), 2);
    assert_eq!(fixture.inference_calls.load(Ordering::SeqCst), 2);
    assert_eq!(fixture.status_calls.load(Ordering::SeqCst), 0);
}
