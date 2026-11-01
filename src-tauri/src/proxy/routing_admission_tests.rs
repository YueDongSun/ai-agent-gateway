//! Source fixtures for admission-bound v2 receipts. No real gateway or provider.

use super::routing_receipts::contract::{RoutingReceipt, RoutingReceiptV2};
use super::server::{build_router, ProxyAppState};
use crate::database::dao::proxy_providers::ProxyProvider;
use crate::database::dao::routes::{Route, RouteHop};
use crate::database::Database;
use crate::services::proxy_runtime::{live::Live_runtime, snapshot::build_runtime_snapshot};
use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, HeaderValue, Request, Uri},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use serde_json::{json, Value};
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::{
    collections::HashMap,
    io::Read,
    net::SocketAddr,
    path::PathBuf,
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::{
    sync::{oneshot, Notify},
    task::JoinHandle,
};
use tower::Service;

const GATEWAY_KEY: &str = "admission-fixture-gateway-key";
const STORED_KEY: &str = "admission-fixture-stored-key";
const CAPABILITY: &str = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";
const NONCE: &str = "0123456789abcdef0123456789abcdef";
const SELECTOR: &str = "admission_alias";
const BODY_LIMIT: usize = 64 * 1024;
const FIXTURE_DEADLINE: Duration = Duration::from_secs(5);

#[derive(Clone)]
struct UpstreamRequest {
    headers: HeaderMap,
    body: Value,
}
struct Pause {
    armed: AtomicBool,
    started: Notify,
    release: Notify,
}
#[derive(Clone)]
struct UpstreamState {
    model: String,
    requests: Arc<Mutex<Vec<UpstreamRequest>>>,
    pause: Option<Arc<Pause>>,
}
struct UpstreamFixture {
    address: SocketAddr,
    state: UpstreamState,
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<std::io::Result<()>>>,
}
impl UpstreamFixture {
    async fn start(model: &str, pause: bool) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let state = UpstreamState {
            model: model.into(),
            requests: Arc::new(Mutex::new(Vec::new())),
            pause: pause.then(|| {
                Arc::new(Pause {
                    armed: AtomicBool::new(true),
                    started: Notify::new(),
                    release: Notify::new(),
                })
            }),
        };
        let app = Router::new()
            .route("/v1/chat/completions", post(upstream_response))
            .route("/v1/messages", post(upstream_response))
            .with_state(state.clone());
        let (shutdown, stopped) = oneshot::channel();
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = stopped.await;
                })
                .await
        });
        Self {
            address,
            state,
            shutdown: Some(shutdown),
            task: Some(task),
        }
    }
    fn requests(&self) -> Vec<UpstreamRequest> {
        self.state.requests.lock().unwrap().clone()
    }
    async fn finish(mut self) {
        if let Some(pause) = &self.state.pause {
            pause.release.notify_one();
        }
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        let mut task = self.task.take().unwrap();
        match tokio::time::timeout(FIXTURE_DEADLINE, &mut task).await {
            Ok(result) => result.unwrap().unwrap(),
            Err(_) => {
                task.abort();
                let _ = task.await;
                panic!("admission fixture shutdown timed out");
            }
        }
    }
}
impl Drop for UpstreamFixture {
    fn drop(&mut self) {
        if let Some(pause) = &self.state.pause {
            pause.release.notify_one();
        }
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}
async fn upstream_response(
    State(state): State<UpstreamState>,
    uri: Uri,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    state.requests.lock().unwrap().push(UpstreamRequest {
        headers,
        body: body.clone(),
    });
    if let Some(pause) = &state.pause {
        if pause.armed.swap(false, Ordering::SeqCst) {
            pause.started.notify_one();
            pause.release.notified().await;
        }
    }
    if uri.path() == "/v1/messages" {
        return Json(json!({
            "id":"msg_admission_fixture","type":"message","role":"assistant","model":state.model,
            "content":[{"type":"text","text":"aag_v2_fixture_ok"}],
            "stop_reason":"end_turn","stop_sequence":null,"usage":{"input_tokens":2,"output_tokens":1}
        })).into_response();
    }
    Json(json!({
        "id":"chatcmpl_admission_fixture","object":"chat.completion","created":1,"model":state.model,
        "choices":[{"index":0,"message":{"role":"assistant","content":"aag_v2_fixture_ok"},"finish_reason":"stop"}],
        "usage":{"prompt_tokens":2,"completion_tokens":1,"total_tokens":3}
    })).into_response()
}
fn empty_state() -> ProxyAppState {
    ProxyAppState {
        router: Arc::new(super::router::Router::new(HashMap::new(), HashMap::new())),
        auth_token: GATEWAY_KEY.into(),
        pinned_provider: Arc::new(Mutex::new(None)),
        responses_cot_enabled: Arc::new(AtomicBool::new(false)),
        passthrough_auth_allowed: false,
        db: None,
        live: None,
        allow_client_config_repair: false,
    }
}
fn save_provider(db: &Database, upstream: &UpstreamFixture, id: &str, protocol: &str) {
    let provider: ProxyProvider = serde_json::from_value(json!({
        "id":id,"name":id,"type":protocol,"apiKey":STORED_KEY,
        "baseURL":format!("http://{}/v1",upstream.address),"models":["wire_model"],
        "enabled":true,"timeoutMs":5000
    }))
    .unwrap();
    db.save_proxy_provider(&provider).unwrap();
}
fn save_route(db: &Database, provider: &str) {
    db.save_route(&Route {
        alias: SELECTOR.into(),
        description: String::new(),
        hops: vec![RouteHop {
            provider: provider.into(),
            model: "wire_model".into(),
        }],
    })
    .unwrap();
}
async fn live_state(
    upstream: &UpstreamFixture,
    protocol: &str,
) -> (ProxyAppState, Database, Arc<Live_runtime>) {
    let db = Database::memory().unwrap();
    save_provider(&db, upstream, "provider_a", protocol);
    save_route(&db, "provider_a");
    let config = db.get_proxy_config().unwrap();
    let snapshot = build_runtime_snapshot(&db, &config).await.unwrap();
    let mut state = empty_state();
    state.router = Arc::clone(&snapshot.router);
    let live = Live_runtime::new(db.clone(), config, snapshot);
    state.live = Some(Arc::clone(&live));
    (state, db, live)
}
async fn edge_call(app: &mut Router, request: Request<Body>) -> Response {
    tokio::time::timeout(FIXTURE_DEADLINE, app.call(request))
        .await
        .unwrap()
        .unwrap()
}
async fn json_body(response: Response) -> Value {
    let bytes = tokio::time::timeout(
        FIXTURE_DEADLINE,
        axum::body::to_bytes(response.into_body(), BODY_LIMIT),
    )
    .await
    .unwrap()
    .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}
fn get_request(path: &str) -> Request<Body> {
    Request::builder()
        .uri(path)
        .header("authorization", format!("Bearer {GATEWAY_KEY}"))
        .body(Body::empty())
        .unwrap()
}
async fn capabilities(app: &mut Router) -> Value {
    let response = edge_call(app, get_request("/_aag/v2/receipt_capabilities")).await;
    assert_eq!(response.status(), 200);
    json_body(response).await
}
fn precondition(capabilities: &Value) -> String {
    format!(
        "v2.{}.{}.{}",
        capabilities["server_instance_id"].as_str().unwrap(),
        capabilities["generation"].as_u64().unwrap(),
        u8::from(capabilities["pinned_provider_present"].as_bool().unwrap())
    )
}
fn inference(path: &str, version: u8, nonce: &str, precondition: Option<&str>) -> Request<Body> {
    let body = json!({"model":SELECTOR,"max_tokens":32,"stream":false,
        "messages":[{"role":"user","content":"synthetic admission fixture"}]});
    let mut request = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {GATEWAY_KEY}"))
        .header("x-aag-receipt", format!("v{version}.{nonce}.{CAPABILITY}"));
    if let Some(precondition) = precondition {
        request = request.header("x-aag-route-precondition", precondition);
    }
    request.body(Body::from(body.to_string())).unwrap()
}
async fn lookup(app: &mut Router, version: u8, nonce: &str) -> Response {
    let mut request = get_request(&format!("/_aag/v{version}/receipts/{nonce}"));
    request.headers_mut().insert(
        "x-aag-receipt-capability",
        HeaderValue::from_static(CAPABILITY),
    );
    edge_call(app, request).await
}
fn assert_private_headers_absent(request: &UpstreamRequest, route_precondition: &str) {
    for name in [
        "x-aag-receipt",
        "x-aag-receipt-capability",
        "x-aag-route-precondition",
    ] {
        assert!(!request.headers.contains_key(name));
    }
    assert!(!request.body.to_string().contains(CAPABILITY));
    assert!(!request.body.to_string().contains(route_precondition));
    assert!(!request.headers.values().any(|value| value
        .to_str()
        .is_ok_and(|value| value.contains(CAPABILITY)
            || value.contains(GATEWAY_KEY)
            || value.contains(route_precondition))));
}

#[tokio::test]
async fn routing_admission_v2_caps_require_authentication_and_one_live_snapshot() {
    let mut app = build_router(empty_state());
    let denied = edge_call(
        &mut app,
        Request::builder()
            .uri("/_aag/v2/receipt_capabilities")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(denied.status(), 401);
    let unavailable = edge_call(&mut app, get_request("/_aag/v2/receipt_capabilities")).await;
    assert_eq!(unavailable.status(), 503);
    assert_eq!(
        json_body(unavailable).await["error"]["code"],
        "receipt_route_snapshot_unavailable"
    );
    let upstream = UpstreamFixture::start("reported_model", false).await;
    let (state, _, live) = live_state(&upstream, "openai").await;
    let mut app = build_router(state);
    let caps = capabilities(&mut app).await;
    assert_eq!(caps.as_object().unwrap().len(), 12);
    assert_eq!(caps["schema_version"], 2);
    assert_eq!(caps["supported_versions"], json!([2]));
    assert_eq!(caps["generation"], live.current().generation);
    assert_eq!(caps["pinned_provider_present"], false);
    assert_eq!(caps["supported_surfaces"], json!(["chat", "messages"]));
    assert_eq!(caps["upstream_model_attestation"], "unavailable");
    assert_eq!(
        caps["messages_credential_scope"],
        "stored_key_only_snapshot"
    );
    assert!(upstream.requests().is_empty());
    upstream.finish().await;
}

#[tokio::test]
async fn routing_admission_v2_chat_and_messages_return_separate_matching_admission_proof() {
    for (protocol, path) in [
        ("openai", "/v1/chat/completions"),
        ("anthropic", "/v1/messages"),
    ] {
        let upstream = UpstreamFixture::start("reported_model", false).await;
        let (state, _, _) = live_state(&upstream, protocol).await;
        let mut app = build_router(state);
        let caps = capabilities(&mut app).await;
        let expected = precondition(&caps);
        let response = edge_call(&mut app, inference(path, 2, NONCE, Some(&expected))).await;
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["x-aag-receipt-version"], "2");
        json_body(response).await;
        let response = lookup(&mut app, 2, NONCE).await;
        assert_eq!(response.status(), 200);
        let value = json_body(response).await;
        let receipt: RoutingReceiptV2 = serde_json::from_value(value.clone()).unwrap();
        let admission = receipt.admission.unwrap();
        assert_eq!(receipt.schema_version, 2);
        assert_eq!(
            admission.server_instance_id,
            caps["server_instance_id"].as_str().unwrap()
        );
        assert_eq!(admission.generation, caps["generation"].as_u64().unwrap());
        assert!(!admission.pinned_provider_present);
        assert_eq!(receipt.routing_receipt.schema_version, 1);
        assert_eq!(
            receipt.routing_receipt.generation,
            Some(admission.generation)
        );
        assert_eq!(receipt.routing_receipt.selected_attempt, Some(1));
        assert_eq!(receipt.routing_receipt.attempts.len(), 1);
        assert!(receipt.routing_receipt.capture_complete);
        assert!(!value.to_string().contains(CAPABILITY));
        assert!(!value.to_string().contains(GATEWAY_KEY));
        assert!(!value.to_string().contains(STORED_KEY));
        assert!(!value.to_string().contains(&expected));
        let requests = upstream.requests();
        assert_eq!(requests.len(), 1);
        assert_private_headers_absent(&requests[0], &expected);
        assert_eq!(lookup(&mut app, 1, NONCE).await.status(), 404);
        // Shared nonce ownership remains enforced across receipt versions.
        let duplicate = edge_call(&mut app, inference(path, 1, NONCE, None)).await;
        assert_eq!(duplicate.status(), 409);
        assert_eq!(upstream.requests().len(), 1);
        upstream.finish().await;
    }
}

#[tokio::test]
async fn routing_admission_v1_receipts_remain_exact_and_cannot_be_read_through_v2() {
    let upstream = UpstreamFixture::start("reported_model", false).await;
    let (state, _, _) = live_state(&upstream, "openai").await;
    let mut app = build_router(state);
    let response = edge_call(&mut app, inference("/v1/chat/completions", 1, NONCE, None)).await;
    assert_eq!(response.status(), 200);
    json_body(response).await;
    let response = lookup(&mut app, 1, NONCE).await;
    assert_eq!(response.status(), 200);
    let value = json_body(response).await;
    assert_eq!(value["schema_version"], 1);
    assert!(value.get("admission").is_none());
    let receipt: RoutingReceipt = serde_json::from_value(value).unwrap();
    assert_eq!(receipt.attempts.len(), 1);
    assert_eq!(lookup(&mut app, 2, NONCE).await.status(), 404);
    upstream.finish().await;
}

#[tokio::test]
async fn routing_admission_stale_instance_generation_and_pin_refuse_without_upstream_effects() {
    let upstream = UpstreamFixture::start("reported_model", false).await;
    let (state, db, live) = live_state(&upstream, "openai").await;
    let mut app = build_router(state);
    let caps = capabilities(&mut app).await;
    for (index, expected) in [
        format!("v2.{}.{}.0", "f".repeat(32), caps["generation"]),
        format!(
            "v2.{}.{}.0",
            caps["server_instance_id"].as_str().unwrap(),
            caps["generation"].as_u64().unwrap() + 1
        ),
        format!(
            "v2.{}.{}.1",
            caps["server_instance_id"].as_str().unwrap(),
            caps["generation"]
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let nonce = format!("{:032x}", index + 1);
        let response = edge_call(
            &mut app,
            inference("/v1/chat/completions", 2, &nonce, Some(&expected)),
        )
        .await;
        assert_eq!(response.status(), 409);
        assert_eq!(
            json_body(response).await["error"]["code"],
            "receipt_route_precondition_failed"
        );
        let response = lookup(&mut app, 2, &nonce).await;
        assert_eq!(response.status(), 200);
        let receipt: RoutingReceiptV2 = serde_json::from_value(json_body(response).await).unwrap();
        assert!(receipt.admission.is_none());
        assert_eq!(receipt.routing_receipt.http_status, Some(409));
        assert!(receipt.routing_receipt.attempts.is_empty());
        assert!(!receipt.routing_receipt.capture_complete);
    }
    db.set_setting(
        crate::database::dao::settings::KEY_PROXY_PINNED_PROVIDER,
        "provider_a",
    )
    .unwrap();
    assert!(live.reload().await.unwrap());
    let response = edge_call(
        &mut app,
        inference("/v1/messages", 2, NONCE, Some(&precondition(&caps))),
    )
    .await;
    assert_eq!(response.status(), 409);
    json_body(response).await;
    assert!(upstream.requests().is_empty());
    upstream.finish().await;
}

#[tokio::test]
async fn routing_admission_header_pairing_is_unique_canonical_and_surface_bound() {
    let upstream = UpstreamFixture::start("reported_model", false).await;
    let (state, _, _) = live_state(&upstream, "openai").await;
    let mut app = build_router(state);
    let caps = capabilities(&mut app).await;
    let expected = precondition(&caps);
    let instance = caps["server_instance_id"].as_str().unwrap();
    for malformed in [
        format!("v2.{instance}.0.0"),
        format!("v2.{instance}.01.0"),
        format!("v2.{instance}.+1.0"),
        format!("v2.{instance}.18446744073709551616.0"),
        format!("v2.{instance}.1.true"),
        format!("v2.{}.1.0", "A".repeat(32)),
        format!("{expected}.extra"),
        format!(" {expected}"),
    ] {
        let response = edge_call(
            &mut app,
            inference("/v1/chat/completions", 2, NONCE, Some(&malformed)),
        )
        .await;
        assert_eq!(response.status(), 400);
        json_body(response).await;
    }
    for (path, version, pair) in [
        ("/v1/chat/completions", 2, None),
        ("/v1/chat/completions", 1, Some(expected.as_str())),
        ("/v1/responses", 2, Some(expected.as_str())),
        ("/v1/messages/count_tokens", 2, Some(expected.as_str())),
    ] {
        let response = edge_call(&mut app, inference(path, version, NONCE, pair)).await;
        assert_eq!(response.status(), 400);
        json_body(response).await;
    }
    let mut duplicate = inference("/v1/messages", 2, NONCE, Some(&expected));
    duplicate.headers_mut().append(
        "x-aag-route-precondition",
        HeaderValue::from_str(&expected).unwrap(),
    );
    assert_eq!(edge_call(&mut app, duplicate).await.status(), 400);
    let mut duplicate_opt_in = inference("/v1/chat/completions", 2, NONCE, Some(&expected));
    duplicate_opt_in.headers_mut().append(
        "x-aag-receipt",
        HeaderValue::from_str(&format!("v2.{NONCE}.{CAPABILITY}")).unwrap(),
    );
    assert_eq!(edge_call(&mut app, duplicate_opt_in).await.status(), 400);
    let mut unpaired = inference("/v1/chat/completions", 2, NONCE, Some(&expected));
    unpaired.headers_mut().remove("x-aag-receipt");
    assert_eq!(edge_call(&mut app, unpaired).await.status(), 400);
    assert!(upstream.requests().is_empty());
    upstream.finish().await;
}

#[tokio::test]
async fn routing_admission_no_live_snapshot_refuses_v2_inference_and_retains_rejected_receipt() {
    let mut app = build_router(empty_state());
    let expected = format!("v2.{}.1.0", "a".repeat(32));
    let response = edge_call(
        &mut app,
        inference("/v1/chat/completions", 2, NONCE, Some(&expected)),
    )
    .await;
    assert_eq!(response.status(), 503);
    json_body(response).await;
    let response = lookup(&mut app, 2, NONCE).await;
    let receipt: RoutingReceiptV2 = serde_json::from_value(json_body(response).await).unwrap();
    assert!(receipt.admission.is_none());
    assert!(receipt.routing_receipt.generation.is_none());
    assert!(receipt.routing_receipt.attempts.is_empty());
}

#[tokio::test]
async fn routing_admission_reload_after_capture_keeps_the_original_dispatch_and_proof() {
    let first = UpstreamFixture::start("original_model", true).await;
    let second = UpstreamFixture::start("replacement_model", false).await;
    let (state, db, live) = live_state(&first, "openai").await;
    let mut app = build_router(state);
    let caps = capabilities(&mut app).await;
    let expected = precondition(&caps);
    let mut in_flight_app = app.clone();
    let request = inference("/v1/chat/completions", 2, NONCE, Some(&expected));
    let operation = tokio::spawn(async move {
        let response = edge_call(&mut in_flight_app, request).await;
        assert_eq!(response.status(), 200);
        json_body(response).await
    });
    let pause = first.state.pause.as_ref().unwrap();
    tokio::time::timeout(FIXTURE_DEADLINE, pause.started.notified())
        .await
        .unwrap();
    save_provider(&db, &second, "provider_b", "openai");
    save_route(&db, "provider_b");
    assert!(live.reload().await.unwrap());
    let updated = capabilities(&mut app).await;
    assert!(updated["generation"].as_u64().unwrap() > caps["generation"].as_u64().unwrap());
    pause.release.notify_one();
    let output = tokio::time::timeout(FIXTURE_DEADLINE, operation)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(output["model"], "original_model");
    let response = lookup(&mut app, 2, NONCE).await;
    let receipt: RoutingReceiptV2 = serde_json::from_value(json_body(response).await).unwrap();
    assert_eq!(
        receipt.admission.unwrap().generation,
        caps["generation"].as_u64().unwrap()
    );
    assert_eq!(
        receipt.routing_receipt.attempts[0]
            .identity
            .provider_ref
            .as_deref(),
        Some("provider_a")
    );
    assert_eq!(
        receipt.routing_receipt.attempts[0]
            .provider_model
            .as_deref(),
        Some("original_model")
    );
    assert_eq!(first.requests().len(), 1);
    assert!(second.requests().is_empty());
    first.finish().await;
    second.finish().await;
}

#[tokio::test]
async fn routing_admission_v2_messages_preserve_the_mixed_credential_scope_refusal() {
    let upstream = UpstreamFixture::start("reported_model", false).await;
    let (state, db, live) = live_state(&upstream, "anthropic").await;
    let relay: ProxyProvider = serde_json::from_value(json!({
        "id":"relay_fixture","name":"relay_fixture","type":"anthropic_passthrough",
        "baseURL":format!("http://{}/v1",upstream.address),"models":["wire_model"],
        "enabled":true,"timeoutMs":5000
    }))
    .unwrap();
    db.save_proxy_provider(&relay).unwrap();
    assert!(live.reload().await.unwrap());
    assert!(live.current().passthrough_auth_allowed);
    let mut app = build_router(state);
    let caps = capabilities(&mut app).await;
    let response = edge_call(
        &mut app,
        inference("/v1/messages", 2, NONCE, Some(&precondition(&caps))),
    )
    .await;
    assert_eq!(response.status(), 400);
    assert_eq!(
        json_body(response).await["error"]["code"],
        "receipt_credential_scope_unsupported"
    );
    let response = lookup(&mut app, 2, NONCE).await;
    let receipt: RoutingReceiptV2 = serde_json::from_value(json_body(response).await).unwrap();
    assert!(receipt.admission.is_some());
    assert!(receipt.routing_receipt.attempts.is_empty());
    assert!(!receipt.routing_receipt.capture_complete);
    assert!(upstream.requests().is_empty());
    upstream.finish().await;
}

const PROBE_DEADLINE: Duration = Duration::from_secs(20);
const PROBE_POLL_INTERVAL: Duration = Duration::from_millis(10);
const PROBE_PASS: &str = "VIBEMUX_AAG_V2_OWNER_FIXTURE_PASS";

struct EdgeFixture {
    address: SocketAddr,
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<std::io::Result<()>>>,
}
impl EdgeFixture {
    async fn start(app: Router) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (shutdown, stopped) = oneshot::channel();
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = stopped.await;
                })
                .await
        });
        Self {
            address,
            shutdown: Some(shutdown),
            task: Some(task),
        }
    }
    async fn finish(mut self) -> Result<(), &'static str> {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        let mut task = self.task.take().ok_or("fixture_edge_not_owned")?;
        match tokio::time::timeout(FIXTURE_DEADLINE, &mut task).await {
            Ok(Ok(Ok(()))) => Ok(()),
            Ok(_) => Err("fixture_edge_failed"),
            Err(_) => {
                task.abort();
                let _ = tokio::time::timeout(FIXTURE_DEADLINE, task).await;
                Err("fixture_edge_shutdown_timeout")
            }
        }
    }
}
impl Drop for EdgeFixture {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}
struct ProbeChild {
    process: Option<Child>,
}
impl ProbeChild {
    async fn wait(&mut self, limit: Duration) -> Result<ExitStatus, &'static str> {
        let deadline = tokio::time::Instant::now() + limit;
        loop {
            let process = self.process.as_mut().ok_or("fixture_probe_not_owned")?;
            if let Some(status) = process
                .try_wait()
                .map_err(|_| "fixture_probe_wait_failed")?
            {
                self.process = None;
                return Ok(status);
            }
            if tokio::time::Instant::now() >= deadline {
                return Err("fixture_probe_deadline");
            }
            tokio::time::sleep(PROBE_POLL_INTERVAL).await;
        }
    }
    async fn terminate(&mut self) -> Result<(), &'static str> {
        if let Some(process) = self.process.as_mut() {
            let _ = process.kill();
        }
        if self.process.is_none() {
            return Ok(());
        }
        self.wait(FIXTURE_DEADLINE).await.map(|_| ())
    }
}
impl Drop for ProbeChild {
    fn drop(&mut self) {
        if let Some(mut process) = self.process.take() {
            let _ = process.kill();
            // A cancelled test retains ownership for bounded reaping after kill.
            let _ = std::thread::Builder::new()
                .name("aag_v2_probe_reaper".into())
                .spawn(move || {
                    let deadline = std::time::Instant::now() + FIXTURE_DEADLINE;
                    loop {
                        match process.try_wait() {
                            Ok(Some(_)) | Err(_) => break,
                            Ok(None) if std::time::Instant::now() < deadline => {
                                std::thread::sleep(PROBE_POLL_INTERVAL)
                            }
                            Ok(None) => break,
                        }
                    }
                });
        }
    }
}
fn probe_output(reader: impl Read + Send + 'static) -> JoinHandle<std::io::Result<Vec<u8>>> {
    tokio::task::spawn_blocking(move || {
        let mut bytes = Vec::new();
        reader
            .take((BODY_LIMIT + 1) as u64)
            .read_to_end(&mut bytes)?;
        Ok(bytes)
    })
}
async fn finish_probe_output(
    task: &mut JoinHandle<std::io::Result<Vec<u8>>>,
) -> Result<Vec<u8>, &'static str> {
    match tokio::time::timeout(FIXTURE_DEADLINE, &mut *task).await {
        Ok(Ok(Ok(bytes))) if bytes.len() <= BODY_LIMIT => Ok(bytes),
        Ok(_) => Err("fixture_probe_output_invalid"),
        Err(_) => {
            task.abort();
            Err("fixture_probe_output_cleanup_unconfirmed")
        }
    }
}
async fn run_vibemux_probe(executable: PathBuf, origin: &str) -> Result<(), &'static str> {
    let mut command = Command::new(&executable);
    command
        .args([
            "--ignored",
            "--exact",
            "consume_actual_aag_v2_owner",
            "--nocapture",
        ])
        .env_clear()
        .env("VIBEMUX_AAG_V2_FIXTURE_ORIGIN", origin)
        .env("VIBEMUX_AAG_V2_FIXTURE_TOKEN", GATEWAY_KEY)
        .current_dir(executable.parent().ok_or("fixture_probe_parent_invalid")?)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Preserve only OS bootstrap and the parent-owned temporary paths.
    for name in ["SystemRoot", "WINDIR", "SystemDrive", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    let process = command.spawn().map_err(|_| "fixture_probe_spawn_failed")?;
    let mut child = ProbeChild {
        process: Some(process),
    };
    let (stdout, stderr) = {
        let process = child.process.as_mut().ok_or("fixture_probe_not_owned")?;
        (process.stdout.take(), process.stderr.take())
    };
    let (Some(stdout), Some(stderr)) = (stdout, stderr) else {
        child.terminate().await?;
        return Err("fixture_probe_output_missing");
    };
    let mut stdout = probe_output(stdout);
    let mut stderr = probe_output(stderr);
    let status = child.wait(PROBE_DEADLINE).await;
    let cleanup = if status.is_err() {
        child.terminate().await
    } else {
        Ok(())
    };
    let (stdout, stderr) = tokio::join!(
        finish_probe_output(&mut stdout),
        finish_probe_output(&mut stderr)
    );
    cleanup?;
    let status = status?;
    let stdout = stdout?;
    let stderr = stderr?;
    if !status.success() {
        return Err("fixture_probe_failed");
    }
    let output = std::str::from_utf8(&stdout).map_err(|_| "fixture_probe_output_invalid")?;
    if output.matches(PROBE_PASS).count() != 1 {
        return Err("fixture_probe_pass_marker_missing");
    }
    for bytes in [&stdout, &stderr] {
        let text = String::from_utf8_lossy(bytes);
        if text.contains(GATEWAY_KEY) || text.contains(STORED_KEY) {
            return Err("fixture_probe_credential_output");
        }
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires an explicitly supplied prebuilt VibeMux v2 consumer fixture"]
async fn routing_admission_vibemux_consumer_reads_actual_v2_owner_over_http() {
    let executable = PathBuf::from(
        std::env::var_os("VIBEMUX_AAG_V2_PROBE_TEST")
            .expect("explicit VIBEMUX_AAG_V2_PROBE_TEST fixture executable is required"),
    );
    assert!(
        executable.is_absolute(),
        "fixture executable must be absolute"
    );
    let canonical = executable
        .canonicalize()
        .expect("resolve explicitly selected fixture executable");
    assert!(
        canonical.is_file(),
        "fixture executable must be a regular file"
    );
    let upstream = UpstreamFixture::start("reported_model", false).await;
    let (state, _, _) = live_state(&upstream, "openai").await;
    let edge = EdgeFixture::start(build_router(state)).await;
    let origin = format!("http://{}", edge.address);
    let result = run_vibemux_probe(executable, &origin).await;
    let edge_result = edge.finish().await;
    let requests = upstream.requests();
    upstream.finish().await;
    assert!(edge_result.is_ok(), "fixture edge cleanup failed");
    assert!(
        result.is_ok(),
        "VibeMux v2 consumer fixture failed: {result:?}"
    );
    assert_eq!(
        requests.len(),
        1,
        "enabled consumer submits once; default-disabled control submits nothing"
    );
    assert_eq!(requests[0].body["model"], "wire_model");
    assert_eq!(
        requests[0].headers["authorization"],
        format!("Bearer {STORED_KEY}")
    );
    for name in [
        "x-aag-receipt",
        "x-aag-receipt-capability",
        "x-aag-route-precondition",
    ] {
        assert!(!requests[0].headers.contains_key(name));
    }
    assert!(!requests[0].body.to_string().contains(GATEWAY_KEY));
}
