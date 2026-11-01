//! Fixture-only tests of receipt admission and retrieval through the real edge.

use super::providers::{anthropic::AnthropicProvider, openai::OpenAIProvider, Provider};
use super::routing_receipts::contract::{
    BodyState, CredentialMode, ModelSource, ReceiptPhase, RoutingReceipt, TerminalSignal,
    TransportState, WireProtocol,
};
use super::server::{build_router, ProxyAppState};
use crate::database::dao::routes::RouteHop;
use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, Request, Uri},
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
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{atomic::AtomicBool, Arc, Mutex},
    time::Duration,
};
use tokio::{
    sync::oneshot,
    task::JoinHandle,
    time::{sleep, Instant},
};
use tower::Service;

const GATEWAY_KEY: &str = "routing-receipt-fixture-gateway-key";
const STORED_KEY: &str = "routing-receipt-fixture-stored-key";
const NONCE: &str = "0123456789abcdef0123456789abcdef";
const CAPABILITY: &str = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";
const REQUESTED_SELECTOR: &str = "requested-alias";
const WIRE_MODEL: &str = "wire-model";
const PROVIDER_MODEL: &str = "provider-reported-model";
const STORED_PROVIDER: &str = "stored_fixture";
const RELAY_PROVIDER: &str = "relay_fixture";
const OPENAI_PROVIDER: &str = "openai_fixture";
const BODY_LIMIT: usize = 16 * 1024;
const FIXTURE_DEADLINE: Duration = Duration::from_secs(3);
const HUB_CONSUMER_DEADLINE: Duration = Duration::from_secs(15);
const HUB_CONSUMER_PASS: &str = "AAG_RD_CONSUMER_FIXTURE_PASS";
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Clone)]
struct UpstreamRequest {
    path: String,
    headers: HeaderMap,
    body: Value,
}

#[derive(Clone)]
struct UpstreamState {
    requests: Arc<Mutex<Vec<UpstreamRequest>>>,
}

struct UpstreamFixture {
    address: SocketAddr,
    requests: Arc<Mutex<Vec<UpstreamRequest>>>,
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<std::io::Result<()>>>,
}

impl UpstreamFixture {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind disposable receipt fixture");
        let address = listener.local_addr().expect("fixture address");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new()
            .route("/v1/chat/completions", post(upstream_response))
            .route("/v1/messages", post(upstream_response))
            .with_state(UpstreamState {
                requests: Arc::clone(&requests),
            });
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
            requests,
            shutdown: Some(shutdown),
            task: Some(task),
        }
    }

    async fn finish(mut self) -> Vec<UpstreamRequest> {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        let mut task = self.task.take().expect("fixture owns its server");
        match tokio::time::timeout(FIXTURE_DEADLINE, &mut task).await {
            Ok(result) => result
                .expect("fixture server task completed")
                .expect("fixture server stopped"),
            Err(_) => {
                task.abort();
                let _ = task.await;
                panic!("receipt fixture shutdown exceeded its deadline");
            }
        }
        let requests = self.requests.lock().expect("fixture requests").clone();
        requests
    }
}

impl Drop for UpstreamFixture {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

struct ConsumerEdgeFixture {
    address: SocketAddr,
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<std::io::Result<()>>>,
}

impl ConsumerEdgeFixture {
    async fn start(app: Router) -> std::io::Result<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let (shutdown, stopped) = oneshot::channel();
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = stopped.await;
                })
                .await
        });
        Ok(Self {
            address,
            shutdown: Some(shutdown),
            task: Some(task),
        })
    }

    async fn finish(mut self) -> Result<(), String> {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        let mut task = self
            .task
            .take()
            .ok_or("consumer edge server is not owned")?;
        match tokio::time::timeout(FIXTURE_DEADLINE, &mut task).await {
            Ok(result) => result
                .map_err(|error| format!("consumer edge task failed: {error}"))?
                .map_err(|error| format!("consumer edge server failed: {error}")),
            Err(_) => {
                task.abort();
                let _ = tokio::time::timeout(FIXTURE_DEADLINE, &mut task).await;
                Err("consumer edge shutdown exceeded its deadline".into())
            }
        }
    }
}

impl Drop for ConsumerEdgeFixture {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

fn required_consumer_file(variable: &str) -> PathBuf {
    let supplied = std::env::var_os(variable)
        .unwrap_or_else(|| panic!("explicitly selected Hub consumer fixture requires {variable}"));
    let path = PathBuf::from(supplied);
    assert!(
        path.is_absolute(),
        "{variable} must be an absolute file path"
    );
    let canonical = path
        .canonicalize()
        .unwrap_or_else(|error| panic!("cannot resolve explicitly supplied {variable}: {error}"));
    assert!(
        canonical.is_file(),
        "{variable} must identify an existing file"
    );
    // Node's entrypoint realpath handling does not accept every Windows
    // verbatim path produced by Rust canonicalize. Validate the resolved file,
    // then pass the caller's already-absolute native spelling to the process.
    path
}

struct ConsumerChild {
    process: Option<Child>,
}

impl ConsumerChild {
    async fn wait(&mut self, limit: Duration) -> Result<ExitStatus, String> {
        let deadline = Instant::now() + limit;
        loop {
            let process = self
                .process
                .as_mut()
                .ok_or("Node fixture was already reaped")?;
            if let Some(status) = process
                .try_wait()
                .map_err(|error| format!("Node fixture wait failed: {error}"))?
            {
                self.process = None;
                return Ok(status);
            }
            if Instant::now() >= deadline {
                return Err("Node fixture wait exceeded its deadline".into());
            }
            sleep(PROCESS_POLL_INTERVAL).await;
        }
    }

    async fn terminate(&mut self) -> Result<(), String> {
        let Some(process) = self.process.as_mut() else {
            return Ok(());
        };
        let _ = process.kill();
        self.wait(FIXTURE_DEADLINE)
            .await
            .map(|_| ())
            .map_err(|error| format!("Node fixture cleanup is unconfirmed: {error}"))
    }
}

impl Drop for ConsumerChild {
    fn drop(&mut self) {
        let Some(mut process) = self.process.take() else {
            return;
        };
        let _ = process.kill();
        // Cancellation cannot await cleanup. Retain ownership for bounded reaping
        // after requesting termination, without blocking the runtime or claiming success.
        let _ = std::thread::Builder::new()
            .name("aag_fixture_reaper".into())
            .spawn(move || {
                let deadline = std::time::Instant::now() + FIXTURE_DEADLINE;
                loop {
                    match process.try_wait() {
                        Ok(Some(_)) | Err(_) => break,
                        Ok(None) if std::time::Instant::now() < deadline => {
                            std::thread::sleep(PROCESS_POLL_INTERVAL);
                        }
                        Ok(None) => break,
                    }
                }
            });
    }
}

fn consumer_output(reader: impl Read + Send + 'static) -> JoinHandle<std::io::Result<Vec<u8>>> {
    tokio::task::spawn_blocking(move || {
        let mut bytes = Vec::new();
        reader
            .take((BODY_LIMIT + 1) as u64)
            .read_to_end(&mut bytes)?;
        Ok(bytes)
    })
}

async fn finish_consumer_output(
    task: &mut JoinHandle<std::io::Result<Vec<u8>>>,
) -> Result<Vec<u8>, String> {
    match tokio::time::timeout(FIXTURE_DEADLINE, &mut *task).await {
        Ok(Ok(Ok(bytes))) if bytes.len() <= BODY_LIMIT => Ok(bytes),
        Ok(Ok(Ok(_))) => Err("Node fixture exceeded the output limit".into()),
        Ok(_) => Err("Node fixture output capture failed".into()),
        Err(_) => {
            task.abort();
            Err("Node fixture output cleanup is unconfirmed".into())
        }
    }
}

async fn run_hub_consumer(node: &Path, script: &Path, base_url: &str) -> Result<(), String> {
    let mut command = Command::new(node);
    command
        .arg(script)
        .args([
            base_url,
            GATEWAY_KEY,
            REQUESTED_SELECTOR,
            WIRE_MODEL,
            PROVIDER_MODEL,
        ])
        .env_clear()
        .current_dir(
            script
                .parent()
                .ok_or("consumer script has no parent directory")?,
        )
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Keep only OS bootstrap and parent-owned temporary-directory references.
    // Provider, proxy, Node-injection and real-home configuration are not inherited.
    for name in ["SystemRoot", "WINDIR", "SystemDrive", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    let process = command
        .spawn()
        .map_err(|error| format!("cannot start Node fixture: {error}"))?;
    let mut child = ConsumerChild {
        process: Some(process),
    };
    let (stdout, stderr) = {
        let process = child.process.as_mut().expect("fixture owns its child");
        (process.stdout.take(), process.stderr.take())
    };
    let (Some(stdout), Some(stderr)) = (stdout, stderr) else {
        child.terminate().await?;
        return Err("Node fixture output pipes are unavailable".into());
    };
    let mut stdout = consumer_output(stdout);
    let mut stderr = consumer_output(stderr);
    let status = child.wait(HUB_CONSUMER_DEADLINE).await;
    let cleanup = if status.is_err() {
        child.terminate().await
    } else {
        Ok(())
    };
    // The trusted script has no children. Reaping its sole writer closes the
    // pipes; blocking reads stay off the runtime and their joins are bounded.
    let (stdout, stderr) = tokio::join!(
        finish_consumer_output(&mut stdout),
        finish_consumer_output(&mut stderr),
    );
    cleanup?;
    let status = status?;
    let stdout = stdout?;
    let stderr = stderr?;
    if !status.success() {
        return Err(format!(
            "Node fixture failed ({status}); stdout={:?}; stderr={:?}",
            String::from_utf8_lossy(&stdout),
            String::from_utf8_lossy(&stderr),
        ));
    }
    if !stderr.is_empty()
        || std::str::from_utf8(&stdout).ok().map(str::trim) != Some(HUB_CONSUMER_PASS)
    {
        return Err(format!(
            "Node fixture did not emit only its pass marker; stdout={:?}; stderr={:?}",
            String::from_utf8_lossy(&stdout),
            String::from_utf8_lossy(&stderr),
        ));
    }
    Ok(())
}

async fn upstream_response(
    State(state): State<UpstreamState>,
    uri: Uri,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    state
        .requests
        .lock()
        .expect("fixture requests")
        .push(UpstreamRequest {
            path: uri.path().into(),
            headers,
            body: body.clone(),
        });
    if uri.path() == "/v1/messages" {
        if body["stream"].as_bool() == Some(true) {
            let events = [
                json!({"type":"message_start","message":{
                    "id":"msg_receipt_fixture","type":"message","role":"assistant",
                    "model":PROVIDER_MODEL,"content":[],"stop_reason":null,
                    "stop_sequence":null,"usage":{"input_tokens":3,"output_tokens":0}
                }}),
                json!({"type":"content_block_start","index":0,
                    "content_block":{"type":"text","text":""}}),
                json!({"type":"content_block_delta","index":0,
                    "delta":{"type":"text_delta","text":"fixture answer"}}),
                json!({"type":"content_block_stop","index":0}),
                json!({"type":"message_delta","delta":{
                    "stop_reason":"end_turn","stop_sequence":null
                },"usage":{"output_tokens":1}}),
                json!({"type":"message_stop"}),
            ];
            let stream = events
                .iter()
                .map(|event| {
                    format!(
                        "event: {}\ndata: {event}\n\n",
                        event["type"].as_str().unwrap()
                    )
                })
                .collect::<String>();
            return ([("content-type", "text/event-stream")], stream).into_response();
        }
        return Json(json!({
            "id":"msg_receipt_fixture","type":"message","role":"assistant",
            "model":PROVIDER_MODEL,"content":[{"type":"text","text":"fixture answer"}],
            "stop_reason":"end_turn","stop_sequence":null,
            "usage":{"input_tokens":3,"output_tokens":1}
        }))
        .into_response();
    }
    Json(json!({
        "id":"chatcmpl_receipt_fixture","object":"chat.completion","created":1,
        "model":PROVIDER_MODEL,"choices":[{"index":0,
            "message":{"role":"assistant","content":"fixture answer"},"finish_reason":"stop"}],
        "usage":{"prompt_tokens":3,"completion_tokens":1,"total_tokens":4}
    }))
    .into_response()
}

fn receipt_opt_in() -> String {
    format!("v1.{NONCE}.{CAPABILITY}")
}

fn inference_request(path: &str, body: Value, opted_in: bool) -> Request<Body> {
    let mut request = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {GATEWAY_KEY}"));
    if opted_in {
        request = request.header("x-aag-receipt", receipt_opt_in());
    }
    request
        .body(Body::from(body.to_string()))
        .expect("fixture request")
}

fn messages_body(stream: bool) -> Value {
    json!({
        "model":REQUESTED_SELECTOR,"max_tokens":32,"stream":stream,
        "messages":[{"role":"user","content":"synthetic receipt fixture"}]
    })
}

async fn response_bytes(response: Response) -> Vec<u8> {
    tokio::time::timeout(
        FIXTURE_DEADLINE,
        axum::body::to_bytes(response.into_body(), BODY_LIMIT),
    )
    .await
    .expect("fixture response completed")
    .expect("bounded fixture response")
    .to_vec()
}

async fn fetch_receipt(app: &mut Router) -> RoutingReceipt {
    let response = edge_call(
        app,
        Request::builder()
            .uri(format!("/_aag/v1/receipts/{NONCE}"))
            .header("authorization", format!("Bearer {GATEWAY_KEY}"))
            .header("x-aag-receipt-capability", CAPABILITY)
            .body(Body::empty())
            .expect("receipt retrieval request"),
    )
    .await;
    assert_eq!(response.status(), 200);
    let bytes = response_bytes(response).await;
    let encoded = std::str::from_utf8(&bytes).expect("receipt JSON is UTF-8");
    assert!(!encoded.contains(GATEWAY_KEY));
    assert!(!encoded.contains(STORED_KEY));
    assert!(!encoded.contains(CAPABILITY));
    serde_json::from_slice(&bytes).expect("closed receipt response")
}

fn assert_capability_not_forwarded(request: &UpstreamRequest) {
    assert!(!request.headers.contains_key("x-aag-receipt"));
    assert!(!request.headers.contains_key("x-aag-receipt-capability"));
    assert!(!request.body.to_string().contains(CAPABILITY));
    assert!(!request
        .headers
        .values()
        .any(|value| { value.to_str().is_ok_and(|value| value.contains(CAPABILITY)) }));
}

async fn edge_call(app: &mut Router, request: Request<Body>) -> Response {
    tokio::time::timeout(FIXTURE_DEADLINE, app.call(request))
        .await
        .expect("fixture edge call completed")
        .expect("fixture edge response")
}

fn state() -> ProxyAppState {
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

fn messages_state(upstream: &UpstreamFixture, mixed: bool) -> ProxyAppState {
    let base_url = format!("http://{}", upstream.address);
    let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
    providers.insert(
        STORED_PROVIDER.into(),
        Arc::new(
            AnthropicProvider::new(
                STORED_PROVIDER.into(),
                base_url.clone(),
                STORED_KEY.into(),
                vec![WIRE_MODEL.into()],
                true,
                2_000,
                Default::default(),
                HashMap::new(),
            )
            .expect("stored-key fixture provider"),
        ),
    );
    let mut hops = Vec::new();
    if mixed {
        providers.insert(
            RELAY_PROVIDER.into(),
            Arc::new(
                AnthropicProvider::new_passthrough(
                    RELAY_PROVIDER.into(),
                    base_url,
                    vec![WIRE_MODEL.into()],
                    true,
                    2_000,
                    Default::default(),
                    HashMap::new(),
                )
                .expect("passthrough fixture provider"),
            ),
        );
        hops.push(RouteHop {
            provider: RELAY_PROVIDER.into(),
            model: WIRE_MODEL.into(),
        });
    }
    hops.push(RouteHop {
        provider: STORED_PROVIDER.into(),
        model: WIRE_MODEL.into(),
    });
    let mut configured = state();
    configured.router = Arc::new(super::router::Router::new(
        providers,
        HashMap::from([(REQUESTED_SELECTOR.into(), hops)]),
    ));
    configured.passthrough_auth_allowed = mixed;
    configured
}

fn openai_state(upstream: &UpstreamFixture) -> ProxyAppState {
    let provider: Arc<dyn Provider> = Arc::new(
        OpenAIProvider::new(
            OPENAI_PROVIDER.into(),
            format!("http://{}/v1", upstream.address),
            STORED_KEY.into(),
            vec![WIRE_MODEL.into()],
            true,
            2_000,
            Default::default(),
            HashMap::new(),
        )
        .expect("OpenAI fixture provider"),
    );
    let mut configured = state();
    configured.router = Arc::new(super::router::Router::new(
        HashMap::from([(OPENAI_PROVIDER.into(), provider)]),
        HashMap::from([(
            REQUESTED_SELECTOR.into(),
            vec![RouteHop {
                provider: OPENAI_PROVIDER.into(),
                // The receipt must observe the final builder value, not this marker.
                model: format!("{WIRE_MODEL}[1M]"),
            }],
        )]),
    ));
    configured
}

#[tokio::test]
async fn capability_negotiation_is_authenticated_and_never_requests_inference() {
    let mut app = build_router(state());
    let denied = app
        .call(
            Request::builder()
                .uri("/_aag/v1/receipt_capabilities")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(denied.status(), 401);
    let response = app
        .call(
            Request::builder()
                .uri("/_aag/v1/receipt_capabilities")
                .header("authorization", format!("Bearer {GATEWAY_KEY}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body = axum::body::to_bytes(response.into_body(), 16 * 1024)
        .await
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["issuer"], "aag");
    assert_eq!(value["supported_versions"], serde_json::json!([1]));
    assert_eq!(value["upstream_model_attestation"], "unavailable");
    assert!(!String::from_utf8(body.to_vec())
        .unwrap()
        .contains(GATEWAY_KEY));
}

#[tokio::test]
async fn mixed_passthrough_messages_receipts_reject_before_all_provider_effects() {
    for stream in [false, true] {
        for pinned in [None, Some(RELAY_PROVIDER), Some(STORED_PROVIDER)] {
            let upstream = UpstreamFixture::start().await;
            let configured = messages_state(&upstream, true);
            *configured.pinned_provider.lock().expect("fixture pin") = pinned.map(str::to_owned);
            let mut app = build_router(configured);
            let response = edge_call(
                &mut app,
                inference_request("/v1/messages", messages_body(stream), true),
            )
            .await;
            assert_eq!(response.status(), 400, "stream={stream}, pin={pinned:?}");
            assert_eq!(response.headers()["x-aag-receipt-nonce"], NONCE);
            let error: Value = serde_json::from_slice(&response_bytes(response).await)
                .expect("Messages rejection JSON");
            assert_eq!(
                error["error"]["code"],
                "receipt_credential_scope_unsupported"
            );
            assert_eq!(error["error"]["type"], "invalid_request_error");

            let receipt = fetch_receipt(&mut app).await;
            assert_eq!(receipt.request_nonce, NONCE);
            assert_eq!(receipt.surface, "messages");
            assert_eq!(receipt.phase, ReceiptPhase::Settled);
            assert_eq!(receipt.body_state, BodyState::Exhausted);
            assert_eq!(receipt.http_status, Some(400));
            assert!(receipt.attempts.is_empty());
            assert_eq!(receipt.selected_attempt, None);
            assert!(!receipt.capture_complete);
            assert!(
                upstream.finish().await.is_empty(),
                "rejection reached an upstream"
            );
        }
    }
}

#[tokio::test]
async fn header_absent_mixed_messages_preserve_legacy_passthrough_behavior() {
    for stream in [false, true] {
        let upstream = UpstreamFixture::start().await;
        let mut app = build_router(messages_state(&upstream, true));
        let response = edge_call(
            &mut app,
            inference_request("/v1/messages", messages_body(stream), false),
        )
        .await;
        assert_eq!(response.status(), 200);
        assert!(!response.headers().contains_key("x-aag-receipt-version"));
        let bytes = response_bytes(response).await;
        if stream {
            assert!(std::str::from_utf8(&bytes)
                .unwrap()
                .contains("message_stop"));
        } else {
            let answer: Value = serde_json::from_slice(&bytes).expect("Messages fixture answer");
            assert_eq!(answer["content"][0]["text"], "fixture answer");
        }

        let requests = upstream.finish().await;
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert_eq!(request.path, "/v1/messages");
        // Characterize the legacy behavior without using a real gateway secret.
        let legacy_authorization = format!("Bearer {GATEWAY_KEY}");
        assert_eq!(
            request
                .headers
                .get("authorization")
                .and_then(|value| value.to_str().ok()),
            Some(legacy_authorization.as_str()),
        );
        assert!(!request.headers.contains_key("x-api-key"));
        assert_eq!(request.body["model"], WIRE_MODEL);
        assert_eq!(request.body["stream"], stream);
        assert_capability_not_forwarded(request);
    }
}

#[tokio::test]
async fn stored_key_messages_receipts_never_relay_the_gateway_credential() {
    for stream in [false, true] {
        let upstream = UpstreamFixture::start().await;
        let mut app = build_router(messages_state(&upstream, false));
        let response = edge_call(
            &mut app,
            inference_request("/v1/messages", messages_body(stream), true),
        )
        .await;
        assert_eq!(response.status(), 200);
        response_bytes(response).await;

        let receipt = fetch_receipt(&mut app).await;
        assert_eq!(
            receipt.requested_selector.as_deref(),
            Some(REQUESTED_SELECTOR)
        );
        assert_eq!(receipt.phase, ReceiptPhase::Settled);
        assert_eq!(receipt.body_state, BodyState::Exhausted);
        assert_eq!(receipt.http_status, Some(200));
        assert!(receipt.capture_complete);
        assert_eq!(receipt.attempts.len(), 1);
        let attempt = &receipt.attempts[0];
        assert_eq!(
            attempt.identity.provider_ref.as_deref(),
            Some(STORED_PROVIDER)
        );
        assert_eq!(attempt.protocol, WireProtocol::Anthropic);
        assert_eq!(attempt.credential_mode, CredentialMode::StoredKey);
        assert_eq!(attempt.wire_model.as_deref(), Some(WIRE_MODEL));
        assert_eq!(attempt.provider_model.as_deref(), Some(PROVIDER_MODEL));
        assert_eq!(attempt.http_status, Some(200));
        assert!(attempt.send_started);
        if stream {
            assert_eq!(attempt.transport_state, TransportState::StreamEnded);
            assert!(attempt
                .terminal_signals
                .contains(&TerminalSignal::AnthropicMessageStop));
        } else {
            assert_eq!(attempt.transport_state, TransportState::ResponseParsed);
        }

        let requests = upstream.finish().await;
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert_eq!(request.path, "/v1/messages");
        assert_eq!(
            request
                .headers
                .get("x-api-key")
                .and_then(|value| value.to_str().ok()),
            Some(STORED_KEY),
        );
        assert!(!request.headers.contains_key("authorization"));
        assert_eq!(request.body["model"], WIRE_MODEL);
        assert_capability_not_forwarded(request);
    }
}

#[tokio::test]
async fn openai_receipt_keeps_selector_wire_and_reported_models_distinct_without_nonce_replay() {
    let upstream = UpstreamFixture::start().await;
    let mut app = build_router(openai_state(&upstream));
    let body = json!({
        "model":REQUESTED_SELECTOR,"max_tokens":32,"stream":false,
        "messages":[{"role":"user","content":"synthetic receipt fixture"}]
    });
    let response = edge_call(
        &mut app,
        inference_request("/v1/chat/completions", body.clone(), true),
    )
    .await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["x-aag-receipt-version"], "1");
    assert_eq!(response.headers()["x-aag-receipt-nonce"], NONCE);
    assert!(!response.headers().contains_key("x-aag-receipt-capability"));
    let response_request_id = response.headers()["x-aag-request-id"]
        .to_str()
        .expect("AAG request id")
        .to_owned();
    let answer: Value =
        serde_json::from_slice(&response_bytes(response).await).expect("OpenAI fixture answer");
    assert_eq!(answer["model"], PROVIDER_MODEL);

    let receipt = fetch_receipt(&mut app).await;
    assert_eq!(receipt.request_id, response_request_id);
    assert_eq!(
        receipt.requested_selector.as_deref(),
        Some(REQUESTED_SELECTOR)
    );
    assert_eq!(receipt.phase, ReceiptPhase::Settled);
    assert_eq!(receipt.body_state, BodyState::Exhausted);
    assert!(receipt.capture_complete);
    assert_eq!(receipt.selected_attempt, Some(1));
    assert_eq!(receipt.attempts.len(), 1);
    let attempt = &receipt.attempts[0];
    assert_eq!(
        attempt.identity.provider_ref.as_deref(),
        Some(OPENAI_PROVIDER)
    );
    assert_eq!(attempt.protocol, WireProtocol::OpenaiChat);
    assert_eq!(attempt.credential_mode, CredentialMode::StoredKey);
    assert_eq!(attempt.wire_model.as_deref(), Some(WIRE_MODEL));
    assert_eq!(attempt.provider_model.as_deref(), Some(PROVIDER_MODEL));
    assert_eq!(attempt.provider_model_source, Some(ModelSource::Model));
    assert_eq!(attempt.transport_state, TransportState::ResponseParsed);
    assert_eq!(attempt.http_status, Some(200));
    assert!(attempt.send_started);

    let duplicate = edge_call(
        &mut app,
        inference_request("/v1/chat/completions", body, true),
    )
    .await;
    assert_eq!(duplicate.status(), 409);
    let error: Value =
        serde_json::from_slice(&response_bytes(duplicate).await).expect("duplicate nonce response");
    assert_eq!(error["error"]["code"], "receipt_nonce_reused");
    let retained = fetch_receipt(&mut app).await;
    assert_eq!(retained.request_id, response_request_id);
    assert_eq!(retained.attempts.len(), 1);

    let requests = upstream.finish().await;
    assert_eq!(
        requests.len(),
        1,
        "reused nonce caused another provider send"
    );
    let request = &requests[0];
    assert_eq!(request.path, "/v1/chat/completions");
    assert_eq!(request.body["model"], WIRE_MODEL);
    let upstream_authorization = format!("Bearer {STORED_KEY}");
    assert_eq!(
        request
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok()),
        Some(upstream_authorization.as_str()),
    );
    assert_capability_not_forwarded(request);
}

#[tokio::test]
#[ignore = "requires explicit Hub consumer fixture"]
async fn hub_consumer_fixture_reads_aag_routing_receipt_over_actual_http() {
    // Explicit selection is an acceptance request, not permission to silently skip.
    // The caller supplies a previously verified Node executable and the owned Hub script.
    let node = required_consumer_file("AAG_RD_NODE_EXE");
    let script = required_consumer_file("AAG_RD_CONSUMER_FIXTURE_SCRIPT");
    assert_eq!(
        script.file_name().and_then(|name| name.to_str()),
        Some("rd_aag_contract_probe.mjs"),
        "the opt-in fixture must name the parent-owned Hub contract script",
    );

    let upstream = UpstreamFixture::start().await;
    let edge = ConsumerEdgeFixture::start(build_router(openai_state(&upstream)))
        .await
        .expect("bind disposable AAG consumer edge");
    let base_url = format!("http://{}", edge.address);
    let consumer_result = run_hub_consumer(&node, &script, &base_url).await;
    // Finish both owners before asserting child success, including failure and timeout.
    let edge_result = edge.finish().await;
    let requests = upstream.finish().await;
    assert!(edge_result.is_ok(), "{edge_result:?}");
    assert!(consumer_result.is_ok(), "{consumer_result:?}");

    assert_eq!(
        requests.len(),
        1,
        "the Hub consumer must issue one generated POST"
    );
    let request = &requests[0];
    assert_eq!(request.path, "/v1/chat/completions");
    assert_eq!(request.body["model"], WIRE_MODEL);
    assert!(request.body["messages"]
        .as_array()
        .is_some_and(|messages| !messages.is_empty()));
    let upstream_authorization = format!("Bearer {STORED_KEY}");
    assert_eq!(
        request
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok()),
        Some(upstream_authorization.as_str()),
    );
    assert!(!request
        .headers
        .keys()
        .any(|name| name.as_str().starts_with("x-aag-receipt")));
    assert!(!request.headers.values().any(|value| {
        value
            .to_str()
            .is_ok_and(|value| value.contains(GATEWAY_KEY))
    }));
    assert!(!request.body.to_string().contains(GATEWAY_KEY));
    assert_capability_not_forwarded(request);
}
