//! Explicit acceptance against the published, installed DSH runtime.
//! No external model calls or live user configuration are used. Run manually:
//! AAG_DSH_TEST_RUNTIME_DIR=<node_modules> AAG_DSH_TEST_NODE=<node executable>
//! cargo test --locked --offline --lib services::dsh::acceptance_tests::pinned_dsh_web_native_claude -- --ignored --exact --test-threads=1

use super::*;
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use serde_json::{json, Value};
use std::{
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

const FIXTURE_TOKEN: &str = "aag_acceptance_bearer_fixture";
const FIXTURE_MODEL: &str = "aag_acceptance_model";
const FIXTURE_REPLY: &str = "aag_acceptance_complete";

#[allow(non_camel_case_types)]
struct Acceptance_directory {
    path: PathBuf,
    service: Dsh_service,
}
impl Acceptance_directory {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("aag_dsh_acceptance_{}", config::operation_id()));
        fs::create_dir(&path).expect("create disposable acceptance directory");
        let service = Dsh_service::new(path.clone());
        Self { path, service }
    }
}
impl Drop for Acceptance_directory {
    fn drop(&mut self) {
        let _ = self.service.shutdown();
        // This exact directory was created by the test and never accepts user
        // paths; stop the owned runtime before removing its disposable data.
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[allow(non_camel_case_types)]
#[derive(Default)]
struct Fixture_observations {
    requests: usize,
    bearer_matches: bool,
    api_key_absent: bool,
    model_matches: bool,
    prompt_seen: bool,
}

async fn anthropic_fixture(
    State(observations): State<Arc<Mutex<Fixture_observations>>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    {
        let mut observed = observations.lock().unwrap();
        observed.requests += 1;
        observed.bearer_matches = headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            == Some(concat!("Bearer ", "aag_acceptance_bearer_fixture"));
        observed.api_key_absent = !headers.contains_key("x-api-key");
        observed.model_matches = body.get("model").and_then(Value::as_str) == Some(FIXTURE_MODEL);
        observed.prompt_seen |= body
            .get("messages")
            .is_some_and(|messages| messages.to_string().contains("aag_acceptance_prompt"));
    }
    let events = [
        (
            "message_start",
            json!({"type":"message_start","message":{"id":"msg_aag_fixture","type":"message","role":"assistant","model":FIXTURE_MODEL,"content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":9,"output_tokens":0}}}),
        ),
        (
            "content_block_start",
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        ),
        (
            "content_block_delta",
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":FIXTURE_REPLY}}),
        ),
        (
            "content_block_stop",
            json!({"type":"content_block_stop","index":0}),
        ),
        (
            "message_delta",
            json!({"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":7}}),
        ),
        ("message_stop", json!({"type":"message_stop"})),
    ];
    let stream = events
        .into_iter()
        .map(|(event, data)| format!("event: {event}\ndata: {data}\n\n"))
        .collect::<String>();
    (
        StatusCode::OK,
        [("content-type", "text/event-stream")],
        stream,
    )
}

async fn rpc(
    client: &reqwest::Client,
    base_url: &str,
    cookie: &str,
    method: &str,
    payload: Value,
) -> Value {
    let response = client.post(format!("{base_url}/api/{method}"))
        .header(reqwest::header::COOKIE, cookie)
        .header(reqwest::header::ORIGIN, base_url)
        .json(&json!({"type":"client-request","rpcId":"aag_acceptance","method":method,"payload":payload}))
        .send().await.expect("local DSH RPC transport");
    assert!(
        response.status().is_success(),
        "RPC {method} transport status {}",
        response.status()
    );
    let response: Value = response.json().await.expect("DSH RPC JSON");
    assert_eq!(response["rpcId"], "aag_acceptance", "RPC correlation");
    assert_eq!(
        response["result"]["ok"],
        true,
        "RPC {method} business failure code {}",
        response
            .pointer("/result/error/code")
            .unwrap_or(&Value::Null)
    );
    response["result"]["value"].clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires explicit pinned DSH installation paths; starts only disposable loopback services"]
async fn pinned_dsh_web_native_claude() {
    run_pinned_dsh(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires explicit pinned DSH installation paths; tests passive discovery and managed auto setup"]
async fn pinned_dsh_auto_setup_native_claude() {
    run_pinned_dsh(true).await;
}

struct Acceptance_environment(Vec<(&'static str, Option<std::ffi::OsString>)>);
impl Acceptance_environment {
    fn discovery(runtime_dir: &std::path::Path, node_path: &std::path::Path) -> Self {
        let previous = vec![
            (
                "AAG_DSH_RUNTIME_DIR",
                std::env::var_os("AAG_DSH_RUNTIME_DIR"),
            ),
            ("PATH", std::env::var_os("PATH")),
        ];
        std::env::set_var(
            "AAG_DSH_RUNTIME_DIR",
            runtime_dir.parent().expect("runtime parent"),
        );
        let mut paths = vec![node_path.parent().expect("Node parent").to_path_buf()];
        if let Some(value) = std::env::var_os("PATH") {
            paths.extend(std::env::split_paths(&value));
        }
        std::env::set_var("PATH", std::env::join_paths(paths).expect("test PATH"));
        Self(previous)
    }
}
impl Drop for Acceptance_environment {
    fn drop(&mut self) {
        for (key, value) in self.0.drain(..).rev() {
            if let Some(value) = value {
                std::env::set_var(key, value);
            } else {
                std::env::remove_var(key);
            }
        }
    }
}

async fn run_pinned_dsh(auto_setup: bool) {
    let runtime_dir = PathBuf::from(
        std::env::var_os("AAG_DSH_TEST_RUNTIME_DIR")
            .expect("set AAG_DSH_TEST_RUNTIME_DIR to pinned node_modules"),
    );
    let node_path = PathBuf::from(
        std::env::var_os("AAG_DSH_TEST_NODE").expect("set AAG_DSH_TEST_NODE explicitly"),
    );
    let dsh_entry = runtime_dir.join("@deepseek-ai/dsh/lib/bin.js");
    let pnpm_entry = runtime_dir.join("pnpm/bin/pnpm.cjs");
    assert!(
        node_path.is_file() && dsh_entry.is_file() && pnpm_entry.is_file(),
        "pinned acceptance runtime missing"
    );
    let directory = Acceptance_directory::new();
    let service = &directory.service;
    if !auto_setup {
        let preview = service
            .preview(Dsh_operation_request {
                action: "initialize".into(),
                package_name: None,
                version: None,
            })
            .unwrap();
        assert_eq!(
            service.execute(preview.preview_id).unwrap().status,
            "succeeded"
        );
    }
    let fixture_listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let fixture_url = format!(
        "http://{}/native/v1",
        fixture_listener.local_addr().unwrap()
    );
    let observations = Arc::new(Mutex::new(Fixture_observations::default()));
    let fixture = Router::new()
        .route("/native/v1/messages", post(anthropic_fixture))
        .with_state(observations.clone());
    let fixture_task = tokio::spawn(async move {
        axum::serve(fixture_listener, fixture).await.unwrap();
    });
    let db = crate::database::Database::memory().unwrap();
    db.save_provider("claude_code", &crate::provider::Provider::new("fixture_native".into(), "Acceptance native source".into(), json!({"env":{
        "ANTHROPIC_BASE_URL":fixture_url,"ANTHROPIC_AUTH_TOKEN":FIXTURE_TOKEN,"ANTHROPIC_MODEL":FIXTURE_MODEL
    }}))).unwrap();
    let _environment =
        auto_setup.then(|| Acceptance_environment::discovery(&runtime_dir, &node_path));
    let current = if auto_setup {
        db.set_current_provider("fixture_native", "claude_code")
            .unwrap();
        let before = service.snapshot().unwrap();
        assert!(!before.initialized);
        let discovered = service.discover_runtime().unwrap();
        assert_eq!(
            std::fs::canonicalize(&discovered.recommended.node_path).unwrap(),
            std::fs::canonicalize(&node_path).unwrap(),
            "auto setup must use the explicit test Node"
        );
        assert_eq!(
            std::fs::canonicalize(&discovered.recommended.dsh_entry_path).unwrap(),
            std::fs::canonicalize(&dsh_entry).unwrap()
        );
        assert_eq!(
            std::fs::canonicalize(&discovered.recommended.pnpm_entry_path).unwrap(),
            std::fs::canonicalize(&pnpm_entry).unwrap()
        );
        assert!(
            !service.root.exists(),
            "passive discovery must not create managed state"
        );
        let options = upstreams::list_upstreams(&db, None).unwrap();
        let (preferred_source, preferred_model) =
            setup_preferences::preferred_native(&db, &options).unwrap();
        let plan = service
            .preview_setup(
                setup::Dsh_setup_request {
                    expected_digest: before.settings_digest,
                    upstream_id: None,
                    model_id: None,
                },
                &options,
                preferred_source.as_deref(),
                preferred_model.as_deref(),
            )
            .unwrap();
        assert!(plan.can_apply, "auto setup blocked: {:?}", plan.blockers);
        assert_eq!(plan.source_reason, "current_claude");
        assert_eq!(
            plan.settings.upstream_id.as_deref(),
            Some("claude:fixture_native")
        );
        assert_eq!(plan.settings.model_id.as_deref(), Some(FIXTURE_MODEL));
        assert!(
            !service.root.exists(),
            "preview must not create managed state"
        );
        let applied = service.apply_setup(plan.plan_id.clone(), &options).unwrap();
        assert!(applied.initialized);
        assert_eq!(
            applied.runtime.phase, "stopped",
            "setup must not implicitly launch"
        );
        assert!(
            service.apply_setup(plan.plan_id, &options).is_err(),
            "setup preview must be single use"
        );
        applied
    } else {
        service.snapshot().unwrap()
    };
    let mut settings = current.settings;
    if !auto_setup {
        settings.node_path = node_path.to_string_lossy().into_owned();
        settings.dsh_entry_path = dsh_entry.to_string_lossy().into_owned();
        settings.pnpm_entry_path = pnpm_entry.to_string_lossy().into_owned();
    }
    settings.port = std::net::TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    settings.startup_timeout_secs = 45;
    settings.theme = "dark".into();
    if !auto_setup {
        settings.upstream_id = Some("claude:fixture_native".into());
        settings.model_id = Some(FIXTURE_MODEL.into());
    }
    let saved = service
        .save_settings(settings, current.settings_digest)
        .unwrap();
    let resolved =
        upstreams::resolve_upstream(&db, "claude:fixture_native", FIXTURE_MODEL, None).unwrap();
    let started = service
        .start(
            resolved.llm_settings,
            resolved.secret_env.into_iter().collect(),
            saved.settings_digest,
        )
        .await
        .expect("actual pinned DSH must boot through Dsh_service");
    assert_eq!(started.runtime.phase, "web_ready");
    #[cfg(windows)]
    assert!(
        service.root.join("user_home/Desktop").is_dir(),
        "the native folder picker needs an existing isolated Desktop"
    );
    for relative in ["AppData/Roaming", "AppData/Local", ".config", ".cache"] {
        assert!(service.root.join("user_home").join(relative).is_dir());
    }
    tokio::time::sleep(Duration::from_millis(1000)).await;
    let stable_runtime = service.snapshot().unwrap().runtime;
    assert_eq!(
        stable_runtime.phase, "web_ready",
        "DSH exited after initial readiness: {:?}",
        stable_runtime.error_code
    );
    let base_url = started.runtime.web_url.as_ref().unwrap();
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap();
    assert_eq!(
        client
            .get(format!("{base_url}/api/pluginInventory/list"))
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::FORBIDDEN
    );
    let bootstrap = client
        .get(service.validated_web_url().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(bootstrap.status(), reqwest::StatusCode::SEE_OTHER);
    let cookie = bootstrap
        .headers()
        .get(reqwest::header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let html = client
        .get(base_url)
        .header(reqwest::header::COOKIE, &cookie)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        html.contains("__DSH_BOOT__"),
        "actual DSH shell marker missing"
    );
    assert!(
        html.contains("data-ds-dark-theme"),
        "upstream theme bootstrap missing"
    );
    let inventory = rpc(
        &client,
        base_url,
        &cookie,
        "pluginInventory/list",
        json!({"args":{}}),
    )
    .await;
    let entries = inventory["entries"]
        .as_array()
        .expect("real plugin inventory entries");
    assert!(!entries.is_empty());
    for expected in [
        "@deepseek-ai/dsh-llm-pi-ai",
        "@deepseek-ai/dsh-host-plugin-inventory",
    ] {
        assert!(
            entries
                .iter()
                .any(|entry| entry["moduleName"] == expected && entry["fiberPhase"] == "active"),
            "required plugin not active: {expected}"
        );
    }
    let session = rpc(
        &client,
        base_url,
        &cookie,
        "session.create",
        json!({"agentPreset":"standard"}),
    )
    .await;
    let session_id = session["sessionId"]
        .as_str()
        .expect("created actual DSH session");
    let models = rpc(
        &client,
        base_url,
        &cookie,
        "session.models",
        json!({"sessionId":session_id}),
    )
    .await;
    assert_eq!(models["current"]["provider"], "anthropic");
    assert_eq!(models["current"]["model"], FIXTURE_MODEL);
    let prompt = rpc(&client, base_url, &cookie, "session.prompt", json!({"sessionId":session_id,"mode":"queue","content":[{"type":"text","text":"aag_acceptance_prompt"}]})).await;
    assert_eq!(prompt["accepted"], true);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let history = rpc(
            &client,
            base_url,
            &cookie,
            "session.history",
            json!({"sessionId":session_id}),
        )
        .await;
        if history.to_string().contains(FIXTURE_REPLY) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "actual DSH did not persist fixture reply"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let observed = observations.lock().unwrap();
    assert!(
        observed.requests > 0
            && observed.bearer_matches
            && observed.api_key_absent
            && observed.model_matches
            && observed.prompt_seen,
        "native Anthropic wire contract failed"
    );
    drop(observed);
    assert!(!serde_json::to_string(&service.snapshot().unwrap())
        .unwrap()
        .contains(FIXTURE_TOKEN));
    assert!(!fs::read_to_string(service.root.join("settings.json"))
        .unwrap()
        .contains(FIXTURE_TOKEN));
    let changed_theme = rpc(
        &client,
        base_url,
        &cookie,
        "settings.update",
        json!({"ns":"ui-theme","patch":{"preference":"light"}}),
    )
    .await;
    assert_eq!(changed_theme["value"]["preference"], "light");
    let persisted: Value =
        serde_json::from_slice(&fs::read(service.root.join("settings.json")).unwrap())
            .expect("DSH UI must preserve JSON settings format");
    assert_eq!(persisted["ui-theme"]["preference"], "light");
    service.stop().unwrap();
    assert!(
        client.get(base_url).send().await.is_err(),
        "owned DSH listener must terminate"
    );
    let current = service.snapshot().unwrap();
    let resolved =
        upstreams::resolve_upstream(&db, "claude:fixture_native", FIXTURE_MODEL, None).unwrap();
    let restarted = service
        .start(
            resolved.llm_settings,
            resolved.secret_env.into_iter().collect(),
            current.settings_digest,
        )
        .await
        .expect("restart after actual DSH settings mutation");
    assert_eq!(restarted.runtime.phase, "web_ready");
    service.stop().unwrap();
    fixture_task.abort();
    let _ = fixture_task.await;
}
