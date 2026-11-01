use super::*;
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::get,
    Json, Router,
};
use serde_json::json;
use std::sync::{Arc, Mutex};

struct SourceFixture {
    _root: crate::config::private_fs::PrivateTempDir,
    path: PathBuf,
    provider: ProxyProvider,
}
impl SourceFixture {
    fn new(usage: Value) -> Self {
        let root = crate::config::private_fs::private_temp_dir(
            &std::env::temp_dir().canonicalize().unwrap(),
        )
        .unwrap();
        let path = root.path().join("cc_switch.db");
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE providers(id TEXT,app_type TEXT,name TEXT,settings_config TEXT,website_url TEXT,category TEXT,created_at INTEGER,sort_index INTEGER,notes TEXT,icon TEXT,icon_color TEXT,meta TEXT,is_current INTEGER,in_failover_queue INTEGER);").unwrap();
        let settings = json!({"env":{"ANTHROPIC_BASE_URL":"https://api.minimax.io/anthropic","ANTHROPIC_AUTH_TOKEN":"fixture_key"}});
        conn.execute("INSERT INTO providers(id,app_type,name,settings_config,meta,is_current,in_failover_queue) VALUES('source','claude','Fixture',?1,?2,0,0)",rusqlite::params![settings.to_string(),json!({"usage_script":usage}).to_string()]).unwrap();
        let provider=serde_json::from_value(json!({"id":"provider_fixture","name":"Fixture","type":"anthropic","baseURL":"https://api.minimax.io/anthropic","apiKey":"fixture_key","models":["model"]})).unwrap();
        Self {
            _root: root,
            path,
            provider,
        }
    }
    fn plan(&self) -> SourcePlan {
        prepare_cc_switch_source(&self.path, "claude_code:source", &self.provider).unwrap()
    }
    fn set_usage(&self, usage: Value) {
        rusqlite::Connection::open(&self.path)
            .unwrap()
            .execute(
                "UPDATE providers SET meta=?1",
                [json!({"usage_script":usage}).to_string()],
            )
            .unwrap();
    }
}
fn usage() -> Value {
    json!({"enabled":true,"language":"javascript","templateType":"token_plan","codingPlanProvider":"minimax","code":"throw new Error('must never execute stored JavaScript')"})
}

#[test]
fn source_auth_scheme_preserves_selected_credential_field_and_explicit_mode() {
    use crate::proxy::providers::AnthropicAuthScheme::{ApiKey, Bearer};

    for (env, api_key_field, auth_mode, expected) in [
        (
            json!({"ANTHROPIC_AUTH_TOKEN":"fixture_key"}),
            None,
            None,
            Bearer,
        ),
        (
            json!({"ANTHROPIC_API_KEY":"fixture_key"}),
            None,
            None,
            ApiKey,
        ),
        (
            json!({"ANTHROPIC_AUTH_TOKEN":"fixture_key","ANTHROPIC_API_KEY":"fixture_key"}),
            None,
            None,
            Bearer,
        ),
        (
            json!({"ANTHROPIC_AUTH_TOKEN":"fixture_key","ANTHROPIC_API_KEY":"fixture_key"}),
            Some("ANTHROPIC_API_KEY"),
            None,
            ApiKey,
        ),
        (
            json!({"ANTHROPIC_AUTH_TOKEN":"fixture_key","ANTHROPIC_API_KEY":"fixture_key"}),
            Some("ANTHROPIC_AUTH_TOKEN"),
            None,
            Bearer,
        ),
        (
            json!({"ANTHROPIC_AUTH_TOKEN":"fixture_key","CUSTOM_KEY":"fixture_key"}),
            Some("CUSTOM_KEY"),
            None,
            ApiKey,
        ),
        (
            json!({"ANTHROPIC_AUTH_TOKEN":"fixture_key"}),
            Some("MISSING_KEY"),
            None,
            Bearer,
        ),
        (
            json!({"ANTHROPIC_AUTH_TOKEN":"fixture_key","ANTHROPIC_API_KEY":42}),
            Some("ANTHROPIC_API_KEY"),
            None,
            Bearer,
        ),
        (
            json!({"ANTHROPIC_API_KEY":"fixture_key","AUTH_MODE":"bearer_only"}),
            None,
            None,
            Bearer,
        ),
        (
            json!({"ANTHROPIC_API_KEY":"fixture_key"}),
            None,
            Some("bearer_only"),
            Bearer,
        ),
        (
            json!({"ANTHROPIC_API_KEY":"fixture_key"}),
            None,
            Some("unrecognized"),
            ApiKey,
        ),
    ] {
        let fixture = SourceFixture::new(usage());
        let mut settings = json!({"env":env});
        settings["env"]["ANTHROPIC_BASE_URL"] = json!(fixture.provider.base_url);
        if let Some(mode) = auth_mode {
            settings["auth_mode"] = json!(mode);
        }
        let mut meta = json!({"usage_script":usage()});
        if let Some(field) = api_key_field {
            meta["apiKeyField"] = json!(field);
        }
        rusqlite::Connection::open(&fixture.path)
            .unwrap()
            .execute(
                "UPDATE providers SET settings_config=?1,meta=?2",
                rusqlite::params![settings.to_string(), meta.to_string()],
            )
            .unwrap();
        let plan = fixture.plan();
        assert_eq!(plan.view().reason, SourceReason::Ready);
        assert_eq!(plan.auth_scheme(), expected);
    }
}

#[test]
fn source_auth_scheme_changes_invalidate_existing_binding_without_changing_key() {
    use crate::proxy::providers::AnthropicAuthScheme::{ApiKey, Bearer};

    let fixture = SourceFixture::new(usage());
    let original = fixture.plan();
    assert_eq!(original.auth_scheme(), Bearer);
    let settings = json!({"env":{"ANTHROPIC_BASE_URL":fixture.provider.base_url,
        "ANTHROPIC_API_KEY":"fixture_key"}});
    rusqlite::Connection::open(&fixture.path)
        .unwrap()
        .execute(
            "UPDATE providers SET settings_config=?1",
            [settings.to_string()],
        )
        .unwrap();
    let changed = fixture.plan();
    assert_eq!(changed.auth_scheme(), ApiKey);
    assert_ne!(original.binding, changed.binding);
    assert!(!validate_current_binding(&original, &fixture.provider).unwrap());
}
fn minimax_body(remaining: f64) -> Value {
    json!({"base_resp":{"status_code":0},"model_remains":[{"model_name":"general","current_interval_remaining_percent":remaining,"current_interval_status":1,"current_weekly_status":3}]})
}

struct StatusServer {
    url: String,
    calls: Arc<Mutex<Vec<HeaderMap>>>,
    reply: Arc<Mutex<(StatusCode, Value)>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for StatusServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl StatusServer {
    async fn new(status: StatusCode, body: Value) -> Self {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let reply = Arc::new(Mutex::new((status, body)));
        let app = Router::new()
            .route(
                "/status",
                get(
                    |State((calls, reply)): State<(
                        Arc<Mutex<Vec<HeaderMap>>>,
                        Arc<Mutex<(StatusCode, Value)>>,
                    )>,
                     headers: HeaderMap| async move {
                        calls.lock().unwrap().push(headers);
                        let (status, body) = reply.lock().unwrap().clone();
                        (status, Json(body))
                    },
                ),
            )
            .with_state((calls.clone(), reply.clone()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/status", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            url,
            calls,
            reply,
            task,
        }
    }
}

#[test]
fn configured_native_template_never_executes_script_and_bindings_detect_source_and_provider_changes(
) {
    let fixture = SourceFixture::new(usage());
    let plan = fixture.plan();
    assert_eq!(plan.view().reason, SourceReason::Ready);
    assert_eq!(
        plan.view().status_url.as_deref(),
        Some("https://api.minimax.io/v1/api/openplatform/coding_plan/remains")
    );
    assert!(validate_current_binding(&plan, &fixture.provider).unwrap());
    let mut changed = fixture.provider.clone();
    changed.api_key = "other_fixture_key".into();
    assert!(!validate_current_binding(&plan, &changed).unwrap());
    fixture.set_usage(
        json!({"enabled":false,"templateType":"token_plan","codingPlanProvider":"minimax"}),
    );
    assert!(!validate_current_binding(&plan, &fixture.provider).unwrap());
    assert_eq!(
        fixture.plan().view().reason,
        SourceReason::UsageQueryDisabled
    );
}

#[test]
fn host_validation_rejects_lookalikes_and_selects_only_explicit_supported_templates() {
    for base in [
        "https://api.minimax.io.evil.invalid/",
        "https://evil.invalid/api.minimax.io",
        "http://api.minimax.io/",
        "https://secret@api.minimax.io/",
        "https://api.minimax.io:8443/",
    ] {
        assert!(native::plan_request("token_plan", Some("minimax"), base, "fixture").is_err());
    }
    assert!(matches!(
        native::plan_request(
            "token_plan",
            Some("volcengine"),
            "https://ark.cn-beijing.volces.com/api/coding",
            "fixture"
        ),
        Err(SourceReason::UnsupportedProvider)
    ));
    assert!(matches!(
        native::plan_request("custom", None, "https://api.minimax.io/", "fixture"),
        Err(SourceReason::UnsupportedTemplate)
    ));
    let zhipu = native::plan_request(
        "token_plan",
        Some("zhipu"),
        "https://open.bigmodel.cn/api/anthropic",
        "fixture",
    )
    .unwrap();
    assert_eq!(zhipu.authorization, "fixture");
    let kimi = native::plan_request(
        "token_plan",
        Some("kimi"),
        "https://api.kimi.com/coding/v1",
        "fixture",
    )
    .unwrap();
    assert_eq!(kimi.url, "https://api.kimi.com/coding/v1/usages");
    assert_eq!(
        native::plan_request(
            "token_plan",
            Some("kimi"),
            "https://api.kimi.com/coding",
            "fixture"
        )
        .unwrap()
        .url,
        kimi.url
    );
    assert!(native::plan_request(
        "token_plan",
        Some("kimi"),
        "https://api.kimi.com/coding-extra",
        "fixture"
    )
    .is_err());
}

#[test]
fn status_parsing_distinguishes_active_exhausted_invalid_and_missing_facts() {
    assert_eq!(
        native::interpret(NativeAdapter::MiniMax, 200, &minimax_body(80.0)),
        (
            SubscriptionState::Active,
            true,
            SourceReason::VerifiedAllowance
        )
    );
    assert_eq!(
        native::interpret(NativeAdapter::MiniMax, 200, &minimax_body(0.0)),
        (
            SubscriptionState::Active,
            false,
            SourceReason::QuotaExhausted
        )
    );
    for body in [
        json!({}),
        json!({"base_resp":{"status_code":0},"model_remains":[]}),
        minimax_body(101.0),
    ] {
        assert_eq!(
            native::interpret(NativeAdapter::MiniMax, 200, &body).0,
            SubscriptionState::Unknown
        );
    }
    assert_eq!(
        native::interpret(
            NativeAdapter::Zhipu,
            200,
            &json!({"success":true,"data":{"limits":[{"type":"TOKENS_LIMIT","percentage":25}]}})
        )
        .1,
        true
    );
    assert_eq!(
        native::interpret(
            NativeAdapter::Zhipu,
            200,
            &json!({"success":false,"msg":"当前用户不存在coding plan"})
        )
        .0,
        SubscriptionState::Inactive
    );
    assert_eq!(native::interpret(NativeAdapter::Kimi,200,&json!({"usage":{"limit":100,"remaining":30},"limits":[{"detail":{"limit":10,"remaining":0}}]})).2,SourceReason::QuotaExhausted);
    assert_eq!(
        native::interpret(
            NativeAdapter::Kimi,
            200,
            &json!({"usage":{"limit":100,"remaining":101}})
        )
        .0,
        SubscriptionState::Unknown
    );
    assert_eq!(
        native::interpret(
            NativeAdapter::OpenRouter,
            200,
            &json!({"data":{"total_credits":0,"total_usage":0}})
        ),
        (
            SubscriptionState::Active,
            false,
            SourceReason::BalanceUnavailable
        )
    );
    assert_eq!(
        native::interpret(
            NativeAdapter::DeepSeek,
            200,
            &json!({"is_available":true,"balance_infos":[{"total_balance":"4.5"}]})
        )
        .1,
        true
    );
    assert_eq!(
        native::interpret(NativeAdapter::StepFun, 200, &json!({"balance":3})).1,
        true
    );
    assert_eq!(
        native::interpret(
            NativeAdapter::SiliconFlow,
            200,
            &json!({"code":20000,"data":{"totalBalance":"3"}})
        )
        .1,
        true
    );
    assert_eq!(
        native::interpret(
            NativeAdapter::Novita,
            200,
            &json!({"availableBalance":1000})
        )
        .1,
        true
    );
    assert_eq!(
        native::interpret(NativeAdapter::MiniMax, 401, &Value::Null).0,
        SubscriptionState::Inactive
    );
    assert_eq!(
        native::interpret(NativeAdapter::MiniMax, 429, &Value::Null).0,
        SubscriptionState::Unknown
    );
}

#[tokio::test]
async fn disabled_unsupported_and_changed_source_never_issue_status_requests() {
    let server = StatusServer::new(StatusCode::OK, minimax_body(80.0)).await;
    for meta in [
        json!({"enabled":false,"templateType":"token_plan","codingPlanProvider":"minimax"}),
        json!({"enabled":true,"templateType":"custom","code":"fetch('forbidden')"}),
        Value::Null,
    ] {
        let fixture = SourceFixture::new(meta);
        let plan = fixture.plan();
        let result = query_subscription(&plan).await;
        assert_eq!(result.evidence.state, SubscriptionState::Unknown);
        assert!(!result.probe_allowed);
    }
    let fixture = SourceFixture::new(usage());
    let mut plan = fixture.plan();
    plan.request.as_mut().unwrap().url = server.url.clone();
    fixture.set_usage(json!({"enabled":false}));
    assert_eq!(
        query_subscription(&plan).await.reason,
        SourceReason::SourceChanged
    );
    assert!(server.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn explicit_native_status_recheck_can_observe_renewal_with_stable_evidence_identity() {
    let fixture = SourceFixture::new(usage());
    let server = StatusServer::new(StatusCode::UNAUTHORIZED, Value::Null).await;
    let mut plan = fixture.plan();
    plan.request.as_mut().unwrap().url = server.url.clone();
    let inactive = query_subscription(&plan).await;
    assert_eq!(inactive.evidence.state, SubscriptionState::Inactive);
    assert!(!inactive.probe_allowed);
    *server.reply.lock().unwrap() = (StatusCode::OK, minimax_body(80.0));
    // This is a separate explicit status refresh; no normal-routing loop exists.
    let active = query_subscription(&plan).await;
    assert_eq!(active.evidence.state, SubscriptionState::Active);
    assert!(active.probe_allowed);
    assert_eq!(inactive.evidence.evidence_id, active.evidence.evidence_id);
    let serialized = serde_json::to_string(&active).unwrap();
    assert!(!serialized.contains("fixture_key"));
    assert!(!serialized.contains("http://"));
    let requests = server.calls.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["authorization"], "Bearer fixture_key");
}

#[tokio::test]
async fn status_transport_is_bounded_and_does_not_follow_redirects() {
    let server = StatusServer::new(
        StatusCode::OK,
        json!({"padding":"x".repeat(MAX_STATUS_BYTES+1)}),
    )
    .await;
    let request = native::NativeRequest {
        adapter: NativeAdapter::MiniMax,
        url: server.url.clone(),
        authorization: "Bearer fixture".into(),
    };
    assert!(matches!(
        query_native(&request).await,
        Err(SourceReason::StatusTooLarge)
    ));
    let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = hits.clone();
    let app = Router::new()
        .route(
            "/status",
            get(|| async { (StatusCode::FOUND, [("location", "/forbidden")], "") }),
        )
        .route(
            "/forbidden",
            get(move || {
                let observed = observed.clone();
                async move {
                    observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Json(minimax_body(80.0))
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let request = native::NativeRequest {
        adapter: NativeAdapter::MiniMax,
        url: format!("http://{}/status", listener.local_addr().unwrap()),
        authorization: "Bearer fixture".into(),
    };
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let result = query_native(&request).await;
    task.abort();
    let _ = task.await;
    assert_eq!(result.unwrap().0, 302);
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[test]
fn source_binding_includes_exact_source_file_and_empty_metadata_cannot_assert_local_exemption() {
    let first = SourceFixture::new(usage());
    let second = SourceFixture::new(usage());
    assert_ne!(first.plan().binding, second.plan().binding);
    let local = SourceFixture::new(json!({}));
    let plan = local.plan();
    assert!(plan.request.is_none());
    assert_ne!(
        observation(&plan, SubscriptionState::Unknown, false, plan.reason, None)
            .evidence
            .state,
        SubscriptionState::NotRequired
    );
    assert_eq!(
        native::interpret(
            NativeAdapter::StepFun,
            200,
            &json!({"balance":10,"error":{"code":"denied"}})
        )
        .0,
        SubscriptionState::Unknown
    );
}
