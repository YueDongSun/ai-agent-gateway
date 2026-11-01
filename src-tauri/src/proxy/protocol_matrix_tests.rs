//! Unified-gateway protocol matrix tests.
//!
//! Each case runs the real Axum proxy server against four loopback fixture
//! upstreams built through the real provider factory. The fixtures capture
//! the exact outgoing wire body, so the assertions prove that the Anthropic,
//! OpenAI Chat, OpenAI Responses, and Gemini client surfaces all arrive at
//! Chat, Anthropic, Responses, and Gemini upstreams with the expected model,
//! system prompt, tool definitions, tool history, image, and safe wire fields.
//! Every upstream and credential here is synthetic; no live provider or real
//! network is touched.

use crate::database::dao::proxy_providers::{CircuitBreakerConfig, ProxyProvider};
use crate::database::dao::routes::RouteHop;
use crate::proxy::providers::{build_provider, Provider};
use crate::proxy::router::Router;
use crate::proxy::server::{serve_detached, ProxyAppState};
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router as AxumRouter};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const ALIAS: &str = "aag-matrix";
const MODEL: &str = "matrix-model";
const IMAGE_B64: &str = "aGVsbG8tZ2F0ZXdheQ==";
const TOOL_ONE_ARGS: &str = "{\"query\":\"one\"}";
const TOOL_TWO_ARGS: &str = "{\"query\":\"two\"}";
const FIXTURE_TEXT: &str = "matrix answer";

#[derive(Clone, Default)]
struct Capture {
    requests: Arc<Mutex<Vec<Captured_request>>>,
}

#[derive(Clone)]
struct Captured_request {
    method: String,
    path: String,
    query: Option<String>,
    content_type: String,
    authorization: Option<String>,
    x_api_key: Option<String>,
    google_key: Option<String>,
    body: Option<Value>,
    raw: String,
}

impl Capture {
    fn snapshot(&self) -> Vec<Captured_request> {
        self.requests
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
}

#[allow(non_camel_case_types)]
struct Fixture_server {
    protocol: &'static str,
    base_url: String,
    capture: Capture,
    abort: tokio::task::AbortHandle,
}

impl Drop for Fixture_server {
    fn drop(&mut self) {
        self.abort.abort();
    }
}

struct Protocol_body {
    protocol: &'static str,
    body: Value,
}

#[allow(non_camel_case_types)]
struct Gateway_server {
    base_url: String,
    abort: tokio::task::AbortHandle,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Drop for Gateway_server {
    fn drop(&mut self) {
        self.abort.abort();
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }
}

async fn start_fixture(protocol: &'static str) -> Fixture_server {
    let capture = Capture::default();
    let app = match protocol {
        "chat" => AxumRouter::new()
            .route("/v1/chat/completions", post(capture_json))
            .with_state(capture.clone()),
        "anthropic" => AxumRouter::new()
            .route("/v1/messages", post(capture_json))
            .with_state(capture.clone()),
        "responses" => AxumRouter::new()
            .route("/v1/responses", post(capture_json))
            .with_state(capture.clone()),
        "gemini" => AxumRouter::new()
            .route("/v1beta/models/*tail", post(capture_json))
            .with_state(capture.clone()),
        other => panic!("unknown fixture protocol {other}"),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Fixture_server {
        protocol,
        base_url: format!("http://{address}"),
        capture,
        abort: handle.abort_handle(),
    }
}

async fn capture_json(
    State(capture): State<Capture>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    };
    let raw = String::from_utf8_lossy(&body).into_owned();
    let parsed: Option<Value> = serde_json::from_slice(&body).ok();
    capture
        .requests
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .push(Captured_request {
            method: method.as_str().to_string(),
            path: uri.path().to_string(),
            query: uri.query().map(str::to_string),
            content_type: header("content-type").unwrap_or_default(),
            authorization: header("authorization"),
            x_api_key: header("x-api-key"),
            google_key: header("x-goog-api-key"),
            body: parsed.clone(),
            raw: raw.clone(),
        });

    let Some(body_json) = parsed else {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(json!({"error":"bad json"})),
        )
            .into_response();
    };
    let Protocol_body { protocol, body } = match protocol_response(&body_json) {
        Some(protocol) => protocol,
        None => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(json!({"error":"unknown upstream protocol"})),
            )
                .into_response();
        }
    };
    let stream = match protocol {
        "gemini" => uri.path().ends_with(":streamGenerateContent"),
        _ => body_json.get("stream").and_then(Value::as_bool) == Some(true),
    };
    if stream {
        return Response::builder()
            .header("content-type", "text/event-stream")
            .body(axum::body::Body::from(stream_wire(protocol)))
            .unwrap();
    }
    Json(body).into_response()
}

fn protocol_response(request: &Value) -> Option<Protocol_body> {
    let tool_parts = json!([
        {"type":"tool_use","id":"call_matrix_one","name":"lookup_one","input":{"query":"one"}},
        {"type":"tool_use","id":"call_matrix_two","name":"lookup_two","input":{"query":"two"}},
    ]);
    // Protocol identification is intentionally based on the request shape, not
    // the URL, so a converter accidentally posting to the wrong route still
    // makes the capture inspectable rather than silently succeeding.
    if request.get("contents").is_some()
        || request.get("systemInstruction").is_some()
        || request.get("generationConfig").is_some()
    {
        return Some(Protocol_body {
            protocol: "gemini",
            body: json!({
                "candidates": [{
                    "content": {"role":"model","parts": [
                        {"text": FIXTURE_TEXT},
                        {"functionCall": {"name":"lookup_one","args":{"query":"one"}}},
                        {"functionCall": {"name":"lookup_two","args":{"query":"two"}}},
                    ]},
                    "finishReason": "STOP",
                    "index": 0,
                }],
                "usageMetadata": {"promptTokenCount":7,"candidatesTokenCount":5,"totalTokenCount":12},
                "modelVersion": MODEL,
            }),
        });
    }
    if request.get("messages").is_some() {
        // Chat and Anthropic bodies both carry `messages`, so discriminate on
        // wire-structural markers, not optional fields (`max_tokens` appears
        // on Chat bodies too): Anthropic puts tool_use/tool_result/image-with-
        // source blocks in message content; Chat uses `role:"tool"`,
        // `tool_calls`, and `image_url` parts. The max_tokens fallback covers
        // text-only bodies, which these fixtures never send.
        let messages = request
            .get("messages")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let anthropic_blocks = messages.iter().any(|message| {
            message
                .get("content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .any(|block| match block.get("type").and_then(Value::as_str) {
                    Some("tool_use" | "tool_result") => true,
                    Some("image") => block.get("source").is_some(),
                    _ => false,
                })
        });
        let chat_markers = messages.iter().any(|message| {
            message.get("role").and_then(Value::as_str) == Some("tool")
                || message.get("tool_calls").is_some()
                || message
                    .get("content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .any(|block| block.get("type").and_then(Value::as_str) == Some("image_url"))
        });
        if anthropic_blocks
            || (!chat_markers
                && request.get("max_tokens").is_some()
                && request.get("choices").is_none())
        {
            return Some(Protocol_body {
                protocol: "anthropic",
                body: json!({
                    "id":"msg_matrix","type":"message","role":"assistant","model":MODEL,
                    "content": [
                        {"type":"text","text": FIXTURE_TEXT},
                        tool_parts[0].clone(),
                        tool_parts[1].clone(),
                    ],
                    "stop_reason":"tool_use",
                    "stop_sequence": Value::Null,
                    "usage":{"input_tokens":7,"output_tokens":5},
                }),
            });
        }
        return Some(Protocol_body {
            protocol: "chat",
            body: json!({
                "id":"chatcmpl-matrix","object":"chat.completion","created":1_700_000_000,"model":MODEL,
                "choices":[{"index":0,"message":{
                    "role":"assistant",
                    "content": FIXTURE_TEXT,
                    "tool_calls":[
                        {"id":"call_matrix_one","type":"function","function":{"name":"lookup_one","arguments":TOOL_ONE_ARGS}},
                        {"id":"call_matrix_two","type":"function","function":{"name":"lookup_two","arguments":TOOL_TWO_ARGS}},
                    ],
                },"finish_reason":"tool_calls"}],
                "usage":{"prompt_tokens":7,"completion_tokens":5,"total_tokens":12},
            }),
        });
    }
    if request.get("input").is_some() || request.get("instructions").is_some() {
        return Some(Protocol_body {
            protocol: "responses",
            body: json!({
                "id":"resp_matrix","object":"response","created_at":1_700_000_000,
                "status":"completed","error": Value::Null,"incomplete_details": Value::Null,
                "model":MODEL,
                "output":[
                    {"type":"message","id":"msg_matrix","role":"assistant","status":"completed",
                     "content":[{"type":"output_text","text": FIXTURE_TEXT}]},
                    {"type":"function_call","id":"fc_matrix_one","call_id":"call_matrix_one",
                     "name":"lookup_one","arguments":TOOL_ONE_ARGS,"status":"completed"},
                    {"type":"function_call","id":"fc_matrix_two","call_id":"call_matrix_two",
                     "name":"lookup_two","arguments":TOOL_TWO_ARGS,"status":"completed"},
                ],
                "usage":{"input_tokens":7,"output_tokens":5,"total_tokens":12},
            }),
        });
    }
    None
}

fn stream_wire(protocol: &str) -> String {
    match protocol {
        "chat" => chat_stream_wire(),
        "anthropic" => anthropic_stream_wire(),
        "responses" => responses_stream_wire(),
        "gemini" => gemini_stream_wire(),
        _ => unreachable!(),
    }
}

fn sse_data(value: &Value) -> String {
    format!("data: {}\n\n", serde_json::to_string(value).unwrap())
}

fn sse_event(event: &str, value: &Value) -> String {
    format!(
        "event: {event}\ndata: {}\n\n",
        serde_json::to_string(value).unwrap()
    )
}

fn chat_stream_wire() -> String {
    let mut wire = String::new();
    for (delta, finish) in [
        (json!({"role":"assistant","content":"matrix "}), None),
        (json!({"content":"answer"}), None),
        (
            json!({"tool_calls":[{"index":0,"id":"call_matrix_one","type":"function","function":{"name":"lookup_one","arguments":""}}]}),
            None,
        ),
        (
            json!({"tool_calls":[{"index":0,"function":{"arguments":"{\"query\":"}}]}),
            None,
        ),
        // A second tool starts before tool one's arguments are complete. This
        // intentionally exercises interleaved parallel deltas rather than one
        // simple sequential pair.
        (
            json!({"tool_calls":[{"index":1,"id":"call_matrix_two","type":"function","function":{"name":"lookup_two","arguments":""}}]}),
            None,
        ),
        (
            json!({"tool_calls":[{"index":0,"function":{"arguments":"\"one\"}"}}]}),
            None,
        ),
        (
            json!({"tool_calls":[{"index":1,"function":{"arguments":"{\"query\":\"two\"}"}}]}),
            None,
        ),
        (json!({}), Some("tool_calls")),
    ] {
        wire.push_str(&sse_data(&json!({
            "id":"chatcmpl-matrix","object":"chat.completion.chunk","created":1,"model":MODEL,
            "choices":[{"index":0,"delta":delta,"finish_reason":finish}],
        })));
    }
    wire.push_str(&sse_data(&json!({
        "id":"chatcmpl-matrix","object":"chat.completion.chunk","created":1,"model":MODEL,
        "choices":[],
        "usage":{"prompt_tokens":7,"completion_tokens":5,"total_tokens":12},
    })));
    wire.push_str("data: [DONE]\n\n");
    wire
}

fn anthropic_stream_wire() -> String {
    let mut wire = String::new();
    let events = [
        (
            "message_start",
            json!({"type":"message_start","message":{
                "id":"msg_matrix","type":"message","role":"assistant","model":MODEL,
                "content":[],"stop_reason": Value::Null,"stop_sequence": Value::Null,
                "usage":{"input_tokens":7,"output_tokens":0},
            }}),
        ),
        (
            "content_block_start",
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        ),
        (
            "content_block_delta",
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"matrix "}}),
        ),
        (
            "content_block_delta",
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"answer"}}),
        ),
        (
            "content_block_stop",
            json!({"type":"content_block_stop","index":0}),
        ),
        (
            "content_block_start",
            json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"call_matrix_one","name":"lookup_one","input":{}}}),
        ),
        (
            "content_block_delta",
            json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":TOOL_ONE_ARGS}}),
        ),
        (
            "content_block_stop",
            json!({"type":"content_block_stop","index":1}),
        ),
        (
            "content_block_start",
            json!({"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"call_matrix_two","name":"lookup_two","input":{}}}),
        ),
        (
            "content_block_delta",
            json!({"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":TOOL_TWO_ARGS}}),
        ),
        (
            "content_block_stop",
            json!({"type":"content_block_stop","index":2}),
        ),
        (
            "message_delta",
            json!({"type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence": Value::Null},"usage":{"output_tokens":5}}),
        ),
        ("message_stop", json!({"type":"message_stop"})),
    ];
    for (event, value) in events {
        wire.push_str(&sse_event(event, &value));
    }
    wire
}

fn responses_stream_wire() -> String {
    let mut wire = String::new();
    let envelope = |status: &str, output: Value| {
        json!({
            "id":"resp_matrix","object":"response","created_at":1_700_000_000,"status":status,
            "error": Value::Null,"incomplete_details": Value::Null,"model":MODEL,
            "output":output,"usage":{"input_tokens":7,"output_tokens":5,"total_tokens":12},
        })
    };
    let frames = [
        (
            "response.created",
            json!({"type":"response.created","response":envelope("in_progress", json!([]))}),
        ),
        (
            "response.output_item.added",
            json!({"type":"response.output_item.added","output_index":0,"item":{
                "type":"message","id":"msg_matrix","role":"assistant","status":"in_progress","content":[]}}),
        ),
        (
            "response.output_text.delta",
            json!({"type":"response.output_text.delta","item_id":"msg_matrix","output_index":0,"content_index":0,"delta":"matrix "}),
        ),
        (
            "response.output_text.delta",
            json!({"type":"response.output_text.delta","item_id":"msg_matrix","output_index":0,"content_index":0,"delta":"answer"}),
        ),
        (
            "response.output_text.done",
            json!({"type":"response.output_text.done","item_id":"msg_matrix","output_index":0,"content_index":0,"text": FIXTURE_TEXT}),
        ),
        (
            "response.output_item.done",
            json!({"type":"response.output_item.done","output_index":0,"item":{
                "type":"message","id":"msg_matrix","role":"assistant","status":"completed",
                "content":[{"type":"output_text","text": FIXTURE_TEXT}]}}),
        ),
        (
            "response.output_item.added",
            json!({"type":"response.output_item.added","output_index":1,"item":{
                "type":"function_call","id":"fc_matrix_one","call_id":"call_matrix_one",
                "name":"lookup_one","arguments":"","status":"in_progress"}}),
        ),
        (
            "response.function_call_arguments.delta",
            json!({"type":"response.function_call_arguments.delta","item_id":"fc_matrix_one","output_index":1,"delta":"{\"query\":"}),
        ),
        (
            "response.output_item.added",
            json!({"type":"response.output_item.added","output_index":2,"item":{
                "type":"function_call","id":"fc_matrix_two","call_id":"call_matrix_two",
                "name":"lookup_two","arguments":"","status":"in_progress"}}),
        ),
        (
            "response.function_call_arguments.delta",
            json!({"type":"response.function_call_arguments.delta","item_id":"fc_matrix_one","output_index":1,"delta":"\"one\"}"}),
        ),
        (
            "response.function_call_arguments.delta",
            json!({"type":"response.function_call_arguments.delta","item_id":"fc_matrix_two","output_index":2,"delta":TOOL_TWO_ARGS}),
        ),
        (
            "response.function_call_arguments.done",
            json!({"type":"response.function_call_arguments.done","item_id":"fc_matrix_one","output_index":1,"arguments":TOOL_ONE_ARGS}),
        ),
        (
            "response.function_call_arguments.done",
            json!({"type":"response.function_call_arguments.done","item_id":"fc_matrix_two","output_index":2,"arguments":TOOL_TWO_ARGS}),
        ),
        (
            "response.output_item.done",
            json!({"type":"response.output_item.done","output_index":1,"item":{
                "type":"function_call","id":"fc_matrix_one","call_id":"call_matrix_one",
                "name":"lookup_one","arguments":TOOL_ONE_ARGS,"status":"completed"}}),
        ),
        (
            "response.output_item.done",
            json!({"type":"response.output_item.done","output_index":2,"item":{
                "type":"function_call","id":"fc_matrix_two","call_id":"call_matrix_two",
                "name":"lookup_two","arguments":TOOL_TWO_ARGS,"status":"completed"}}),
        ),
        (
            "response.completed",
            json!({"type":"response.completed","response":envelope("completed", json!([
                {"type":"message","id":"msg_matrix","role":"assistant","status":"completed",
                 "content":[{"type":"output_text","text": FIXTURE_TEXT}]},
                {"type":"function_call","id":"fc_matrix_one","call_id":"call_matrix_one",
                 "name":"lookup_one","arguments":TOOL_ONE_ARGS,"status":"completed"},
                {"type":"function_call","id":"fc_matrix_two","call_id":"call_matrix_two",
                 "name":"lookup_two","arguments":TOOL_TWO_ARGS,"status":"completed"},
            ]))}),
        ),
    ];
    for (event, value) in frames {
        wire.push_str(&sse_event(event, &value));
    }
    wire.push_str("data: [DONE]\n\n");
    wire
}

fn gemini_stream_wire() -> String {
    let frames = [
        json!({"candidates":[{"content":{"role":"model","parts":[{"text":"matrix "}]},"index":0}],"modelVersion":MODEL}),
        json!({"candidates":[{"content":{"role":"model","parts":[{"text":"answer"}]},"index":0}],"modelVersion":MODEL}),
        json!({"candidates":[{"content":{"role":"model","parts":[
            {"functionCall":{"name":"lookup_one","args":{"query":"one"}}},
            {"functionCall":{"name":"lookup_two","args":{"query":"two"}}},
        ]},"finishReason":"STOP","index":0}],"usageMetadata":{"promptTokenCount":7,"candidatesTokenCount":5,"totalTokenCount":12},"modelVersion":MODEL}),
    ];
    frames.iter().map(sse_data).collect()
}

async fn start_gateway(provider: Arc<dyn Provider>, native_responses: bool) -> Gateway_server {
    let provider_id = provider.id().to_string();
    let providers = [(provider.id().to_string(), provider)]
        .into_iter()
        .collect::<HashMap<_, _>>();
    let routes = [(
        ALIAS.to_string(),
        vec![RouteHop {
            provider: provider_id.clone(),
            model: MODEL.to_string(),
        }],
    )]
    .into_iter()
    .collect();
    // Mirror the production runtime wiring (services/proxy_runtime/
    // transports.rs marks openai-responses bindings native and installs them
    // via with_provider_bindings). Without it the router treats a native
    // Responses hop as foreign and re-projects the source envelope.
    let bindings = [(
        provider_id,
        crate::proxy::session_affinity::Provider_binding {
            identity: crate::proxy::session_affinity::next_transport_identity(),
            native_responses,
        },
    )]
    .into_iter()
    .collect();
    let router = Router::new(providers, routes).with_provider_bindings(bindings);
    let state = ProxyAppState {
        router: Arc::new(router),
        auth_token: String::new(),
        pinned_provider: Arc::new(Mutex::new(None)),
        responses_cot_enabled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        passthrough_auth_allowed: false,
        db: None,
        live: None,
        allow_client_config_repair: false,
    };
    let (shutdown, rx) = tokio::sync::oneshot::channel();
    let (bound, handle) = serve_detached("127.0.0.1:0".parse().unwrap(), state, rx)
        .await
        .expect("bind gateway fixture");
    Gateway_server {
        base_url: format!("http://{}", bound.addr),
        abort: handle.abort_handle(),
        shutdown: Some(shutdown),
    }
}

fn provider_row(server: &Fixture_server) -> ProxyProvider {
    ProxyProvider {
        id: format!("fixture-{}", server.protocol),
        name: format!("fixture-{}", server.protocol),
        provider_type: match server.protocol {
            // build_provider's chat-family type is "openai"; Responses has its
            // own factory entry. Everything else matches the fixture label.
            "chat" => "openai".to_string(),
            "responses" => "openai-responses".to_string(),
            other => other.to_string(),
        },
        api_key: format!("fixture-{}-key", server.protocol),
        base_url: server.base_url.clone(),
        models: vec![MODEL.to_string()],
        enabled: true,
        timeout_ms: 2_000,
        weight: 1,
        circuit_breaker: CircuitBreakerConfig::default(),
        model_mapping: HashMap::new(),
        extra_headers: HashMap::new(),
        auto_track_models: false,
        icon: None,
        icon_color: None,
    }
}

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(4))
        .build()
        .unwrap()
}

fn tool_definitions(style: &str) -> Value {
    let names = ["lookup_one", "lookup_two"];
    match style {
        "chat" => Value::Array(
            names
                .iter()
                .map(|name| {
                    json!({"type":"function","function":{"name":name,"description":"fixture lookup","parameters":{"type":"object","properties":{"query":{"type":"string"}},"required":["query"]}}})
                })
                .collect(),
        ),
        "anthropic" => Value::Array(
            names
                .iter()
                .map(|name| {
                    json!({"name":name,"description":"fixture lookup","input_schema":{"type":"object","properties":{"query":{"type":"string"}},"required":["query"]}})
                })
                .collect(),
        ),
        "responses" => Value::Array(
            names
                .iter()
                .map(|name| {
                    json!({"type":"function","name":name,"description":"fixture lookup","parameters":{"type":"object","properties":{"query":{"type":"string"}},"required":["query"]},"strict": false})
                })
                .collect(),
        ),
        "gemini" => json!([{
            "functionDeclarations": names.iter().map(|name| {
                json!({"name":name,"description":"fixture lookup","parameters":{"type":"object","properties":{"query":{"type":"string"}},"required":["query"]}})
            }).collect::<Vec<_>>()
        }]),
        other => panic!("unknown tool style {other}"),
    }
}

fn chat_client_request(stream: bool) -> Value {
    json!({
        "model": ALIAS,
        "stream": stream,
        "messages": [
            {"role":"system","content":"You are a matrix fixture."},
            {"role":"user","content":"first question"},
            {"role":"assistant","content": Value::Null,"tool_calls":[
                {"id":"hist_one","type":"function","function":{"name":"lookup_one","arguments":"{\"query\":\"history-one\"}"}},
                {"id":"hist_two","type":"function","function":{"name":"lookup_two","arguments":"{\"query\":\"history-two\"}"}},
            ]},
            {"role":"tool","tool_call_id":"hist_one","content":"{\"ok\":true,\"id\":1}"},
            {"role":"tool","tool_call_id":"hist_two","content":"{\"ok\":true,\"id\":2}"},
            {"role":"user","content":[
                {"type":"text","text":"describe this image and call both tools"},
                {"type":"image_url","image_url":{"url":format!("data:image/png;base64,{IMAGE_B64}")}},
            ]},
        ],
        "tools": tool_definitions("chat"),
        "tool_choice": "auto",
        "temperature": 0.2,
        "max_tokens": 64,
    })
}

fn anthropic_client_request(stream: bool) -> Value {
    json!({
        "model": ALIAS,
        "max_tokens": 64,
        "stream": stream,
        "temperature": 0.2,
        "system": [{"type":"text","text":"You are a matrix fixture."}],
        "messages": [
            {"role":"user","content":"first question"},
            {"role":"assistant","content":[
                {"type":"tool_use","id":"hist_one","name":"lookup_one","input":{"query":"history-one"}},
                {"type":"tool_use","id":"hist_two","name":"lookup_two","input":{"query":"history-two"}},
            ]},
            {"role":"user","content":[
                {"type":"tool_result","tool_use_id":"hist_one","content":"{\"ok\":true,\"id\":1}"},
                {"type":"tool_result","tool_use_id":"hist_two","content":"{\"ok\":true,\"id\":2}"},
            ]},
            {"role":"user","content":[
                {"type":"text","text":"describe this image and call both tools"},
                {"type":"image","source":{"type":"base64","media_type":"image/png","data":IMAGE_B64}},
            ]},
        ],
        "tools": tool_definitions("anthropic"),
    })
}

fn responses_client_request(stream: bool) -> Value {
    // Stateless continuation shape per the ingress contract: `call_id`
    // correlates history, while an input item `id` is gateway-issued
    // continuation state. Fabricated ids are unknown continuation ids and
    // are rejected with 409 before dispatch
    // (Session_affinity::source_target), so this fixture deliberately omits
    // them and keeps the real correlation keys only.
    json!({
        "model": ALIAS,
        "stream": stream,
        "instructions": "You are a matrix fixture.",
        "temperature": 0.2,
        "max_output_tokens": 64,
        "input": [
            {"type":"message","role":"user","content":[{"type":"input_text","text":"first question"}]},
            {"type":"function_call","call_id":"hist_one","name":"lookup_one","arguments":"{\"query\":\"history-one\"}","status":"completed"},
            {"type":"function_call","call_id":"hist_two","name":"lookup_two","arguments":"{\"query\":\"history-two\"}","status":"completed"},
            {"type":"function_call_output","call_id":"hist_one","output":"{\"ok\":true,\"id\":1}"},
            {"type":"function_call_output","call_id":"hist_two","output":"{\"ok\":true,\"id\":2}"},
            {"type":"message","role":"user","content":[
                {"type":"input_text","text":"describe this image and call both tools"},
                {"type":"input_image","image_url":format!("data:image/png;base64,{IMAGE_B64}")},
            ]},
        ],
        "tools": tool_definitions("responses"),
        "tool_choice": "auto",
    })
}

fn gemini_client_request() -> Value {
    json!({
        "systemInstruction": {"parts":[{"text":"You are a matrix fixture."}]},
        "contents": [
            {"role":"user","parts":[{"text":"first question"}]},
            {"role":"model","parts":[
                {"functionCall":{"name":"lookup_one","args":{"query":"history-one"}}},
                {"functionCall":{"name":"lookup_two","args":{"query":"history-two"}}},
            ]},
            {"role":"user","parts":[
                {"functionResponse":{"name":"lookup_one","response":{"ok":true,"id":1}}},
                {"functionResponse":{"name":"lookup_two","response":{"ok":true,"id":2}}},
            ]},
            {"role":"user","parts":[
                {"text":"describe this image and call both tools"},
                {"inlineData":{"mimeType":"image/png","data":IMAGE_B64}},
            ]},
        ],
        "tools": tool_definitions("gemini"),
        "toolConfig": {"functionCallingConfig":{"mode":"AUTO"}},
        "generationConfig": {"temperature":0.2,"maxOutputTokens":64},
    })
}

fn tool_names(body: &Value) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let Some(tools) = body.get("tools").and_then(Value::as_array) else {
        return names;
    };
    for tool in tools {
        let name = tool
            .pointer("/function/name")
            .or_else(|| tool.get("name"))
            .and_then(Value::as_str);
        if let Some(name) = name {
            names.insert(name.to_string());
        }
    }
    names
}

fn contains_string(value: &Value, needle: &str) -> bool {
    match value {
        Value::String(text) => text.contains(needle),
        Value::Array(items) => items.iter().any(|item| contains_string(item, needle)),
        Value::Object(map) => map.values().any(|item| contains_string(item, needle)),
        _ => false,
    }
}

fn gemini_function_parts(body: &Value, name: &str) -> Vec<Value> {
    body.get("contents")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|content| {
            content
                .get("parts")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        })
        .filter(|part| {
            part.pointer("/functionCall/name").and_then(Value::as_str) == Some(name)
                || part
                    .pointer("/functionResponse/name")
                    .and_then(Value::as_str)
                    == Some(name)
        })
        .collect()
}

fn assert_no_internal_fields(body: &Value, raw: &str) {
    let text = serde_json::to_string(body).unwrap();
    for forbidden in [
        "source_anthropic_request",
        "source_anthropic_response",
        "source_responses_request",
        "source_responses_response",
        "client_credential",
    ] {
        assert!(
            !text.contains(forbidden) && !raw.contains(forbidden),
            "internal field leaked upstream: {forbidden}"
        );
    }
}

fn assert_common_capture(
    capture: &Captured_request,
    protocol: &'static str,
    surface: &'static str,
    stream: bool,
) -> Value {
    assert_eq!(capture.method, "POST");
    assert!(
        capture.content_type.contains("application/json"),
        "content-type {}",
        capture.content_type
    );
    match protocol {
        "chat" => {
            assert_eq!(capture.path, "/v1/chat/completions");
            assert_eq!(
                capture.authorization.as_deref(),
                Some("Bearer fixture-chat-key")
            );
        }
        "anthropic" => {
            assert_eq!(capture.path, "/v1/messages");
            assert_eq!(capture.x_api_key.as_deref(), Some("fixture-anthropic-key"));
        }
        "responses" => {
            assert_eq!(capture.path, "/v1/responses");
            assert_eq!(
                capture.authorization.as_deref(),
                Some("Bearer fixture-responses-key")
            );
        }
        "gemini" => {
            let suffix = if stream {
                ":streamGenerateContent"
            } else {
                ":generateContent"
            };
            assert!(
                capture.path.ends_with(&format!("/{MODEL}{suffix}")),
                "path {}",
                capture.path
            );
            assert_eq!(capture.google_key.as_deref(), Some("fixture-gemini-key"));
            if stream {
                assert_eq!(capture.query.as_deref(), Some("alt=sse"));
            }
        }
        other => panic!("unknown upstream protocol {other}"),
    }
    let body = capture.body.clone().expect("fixture captured JSON body");
    assert_no_internal_fields(&body, &capture.raw);
    if protocol != "gemini" {
        assert_eq!(body.get("model").and_then(Value::as_str), Some(MODEL));
    } else {
        assert!(
            body.get("model").is_none(),
            "Gemini model belongs in the URL only"
        );
    }
    assert!(
        contains_string(&body, "matrix fixture"),
        "system prompt missing from {}",
        serde_json::to_string(&body).unwrap()
    );
    assert!(
        contains_string(&body, "history-one") && contains_string(&body, "history-two"),
        "both historical tool calls missing"
    );
    assert_tool_results(protocol, surface, &body);
    assert!(
        contains_string(&body, IMAGE_B64),
        "{surface}->{protocol} base64 image did not reach the upstream"
    );
    assert!(
        contains_string(&body, "lookup_one")
            && contains_string(&body, "lookup_two")
            && contains_string(&body, "history-one")
            && contains_string(&body, "history-two"),
        "{surface}->{protocol} assistant history lost or merged its two tool calls"
    );
    assert!(
        contains_string(&body, "describe this image"),
        "final user turn missing"
    );
    let declared_tools = match protocol {
        "gemini" => {
            // Gemini declares tools under functionDeclarations, while history
            // appears as functionCall/functionResponse parts.
            let names = body
                .get("tools")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .flat_map(|tool| {
                    tool.get("functionDeclarations")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default()
                })
                .filter_map(|declaration| {
                    declaration
                        .get("name")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .collect::<BTreeSet<_>>();
            assert_eq!(
                names,
                ["lookup_one", "lookup_two"]
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
                "both Gemini tools must be declared upstream"
            );
            for name in ["lookup_one", "lookup_two"] {
                assert!(
                    !gemini_function_parts(&body, name).is_empty(),
                    "Gemini history lost {name}"
                );
            }
            names
        }
        _ => tool_names(&body),
    };
    assert_eq!(
        declared_tools,
        ["lookup_one", "lookup_two"]
            .into_iter()
            .map(str::to_string)
            .collect(),
        "both tools must be declared upstream"
    );
    if let Some(tools) = body.get("tools").and_then(Value::as_array) {
        let names = match protocol {
            "gemini" => tools
                .iter()
                .flat_map(|tool| {
                    tool.get("functionDeclarations")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                })
                .filter_map(|declaration| declaration.get("name").and_then(Value::as_str))
                .collect::<Vec<_>>(),
            _ => tools
                .iter()
                .filter_map(|tool| {
                    tool.pointer("/function/name")
                        .or_else(|| tool.get("name"))
                        .and_then(Value::as_str)
                })
                .collect::<Vec<_>>(),
        };
        assert_eq!(
            names,
            ["lookup_one", "lookup_two"],
            "{surface}->{protocol} tool declarations lost or merged"
        );
    }
    assert_protocol_history(protocol, surface, &body);
    body
}

/// The two historical tool results must reach the upstream intact and
/// unmerged. Gemini carries them as structured `functionResponse.response`
/// objects (oai_req_to_gemini_req parses the JSON payload), so equality is
/// checked on the parsed objects; the Chat/Anthropic/Responses wires carry
/// the raw JSON text, so the serialized payload is checked there instead.
fn assert_tool_results(protocol: &'static str, surface: &str, body: &Value) {
    match protocol {
        "gemini" => {
            for (name, expected) in [
                ("lookup_one", json!({"ok":true,"id":1})),
                ("lookup_two", json!({"ok":true,"id":2})),
            ] {
                let mut responses = gemini_function_parts(body, name)
                    .into_iter()
                    .filter_map(|part| part.pointer("/functionResponse/response").cloned())
                    .collect::<Vec<_>>();
                assert_eq!(
                    responses.len(),
                    1,
                    "{surface}->gemini must carry exactly one {name} tool result: {responses:?}"
                );
                let response = responses.pop().unwrap();
                assert_eq!(
                    response, expected,
                    "{surface}->gemini corrupted or merged the {name} tool result"
                );
            }
        }
        _ => {
            assert!(
                contains_string(&body, "ok") && contains_string(&body, "id"),
                "tool results missing"
            );
            assert!(
                contains_string(&body, "\"id\":1") && contains_string(&body, "\"id\":2"),
                "the two historical tool results were merged or dropped"
            );
        }
    }
}

/// History correlation is part of the protocol contract, not just payload
/// presence. Native Anthropic/Responses and OpenAI Chat hops must keep the
/// original call ids; the Gemini hop intentionally uses function names
/// because Gemini has no call-id field.
fn assert_protocol_history(protocol: &'static str, surface: &'static str, body: &Value) {
    if surface == "gemini" && protocol != "gemini" {
        // Gemini-origin history carries no client call ids — that wire only
        // names functions — so the gateway generates deterministic per-name
        // correlation ids (gemini_req_to_oai_req). The contract to hold is
        // two distinct calls with every result still paired to its own call;
        // demanding the foreign-surface literals here would be wrong.
        let (calls, results) = match protocol {
            "chat" => {
                let mut calls = Vec::new();
                let mut results = Vec::new();
                for message in body
                    .get("messages")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if message.get("role").and_then(Value::as_str) == Some("tool") {
                        results.push(
                            message
                                .get("tool_call_id")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                        );
                    }
                    for call in message_tool_calls(message) {
                        calls.push(
                            call.get("id")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                        );
                    }
                }
                (calls, results)
            }
            "anthropic" => {
                let mut calls = Vec::new();
                let mut results = Vec::new();
                for message in body
                    .get("messages")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    let Some(blocks) = message.get("content").and_then(Value::as_array) else {
                        continue;
                    };
                    for block in blocks {
                        match block.get("type").and_then(Value::as_str) {
                            Some("tool_use") => calls.push(
                                block
                                    .get("id")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .to_string(),
                            ),
                            Some("tool_result") => results.push(
                                block
                                    .get("tool_use_id")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .to_string(),
                            ),
                            _ => {}
                        }
                    }
                }
                (calls, results)
            }
            "responses" => {
                let mut calls = Vec::new();
                let mut results = Vec::new();
                for item in body
                    .get("input")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    match item.get("type").and_then(Value::as_str) {
                        Some("function_call") => calls.push(
                            item.get("call_id")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                        ),
                        Some("function_call_output") => results.push(
                            item.get("call_id")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                        ),
                        _ => {}
                    }
                }
                (calls, results)
            }
            other => panic!("unknown upstream protocol {other}"),
        };
        assert_eq!(
            calls.len(),
            2,
            "{surface}->{protocol} lost or merged its two tool calls: {calls:?}"
        );
        assert_eq!(
            results.len(),
            2,
            "{surface}->{protocol} lost or merged its two tool results: {results:?}"
        );
        let call_set = calls.iter().collect::<HashSet<_>>();
        let result_set = results.iter().collect::<HashSet<_>>();
        assert_eq!(
            call_set.len(),
            2,
            "{surface}->{protocol} generated colliding tool ids: {calls:?}"
        );
        assert_eq!(
            call_set, result_set,
            "{surface}->{protocol} broke call/result pairing: calls={calls:?} results={results:?}"
        );
        return;
    }
    let contains = |needle: &str| contains_string(body, needle);
    match protocol {
        "chat" | "anthropic" | "responses" => {
            for id in ["hist_one", "hist_two"] {
                assert!(
                    contains(id),
                    "{surface}->{protocol} dropped historical tool id {id}"
                );
            }
        }
        "gemini" => {
            for name in ["lookup_one", "lookup_two"] {
                let parts = gemini_function_parts(body, name);
                assert!(
                    parts.iter().any(|part| part.get("functionCall").is_some()),
                    "{surface}->gemini lost call {name}"
                );
                assert!(
                    parts
                        .iter()
                        .any(|part| part.get("functionResponse").is_some()),
                    "{surface}->gemini lost result {name}"
                );
            }
        }
        other => panic!("unknown upstream protocol {other}"),
    }
}

#[derive(Debug, Default, Clone)]
struct ParsedStream {
    text_fragments: Vec<String>,
    tool_names: HashMap<usize, String>,
    tool_args: HashMap<usize, String>,
    tool_ids: HashMap<usize, String>,
    gemini_calls: HashMap<String, Value>,
    finish_reasons: Vec<String>,
    usages: Vec<Value>,
    events: Vec<String>,
    raw: String,
}

fn sse_blocks(raw: &str) -> Vec<(Option<String>, String)> {
    raw.split("\n\n")
        .filter_map(|block| {
            let mut event = None;
            let mut data = Vec::new();
            for line in block.lines() {
                if let Some(value) = line.strip_prefix("event:") {
                    event = Some(value.trim().to_string());
                } else if let Some(value) = line.strip_prefix("data:") {
                    data.push(value.trim_start().to_string());
                }
            }
            (!data.is_empty()).then(|| (event, data.join("\n")))
        })
        .collect()
}

fn remember_tool(parsed: &mut ParsedStream, index: usize, call: &Value) {
    if let Some(id) = call
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
    {
        parsed.tool_ids.insert(index, id.to_string());
    }
    if let Some(name) = call
        .pointer("/function/name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
    {
        parsed.tool_names.insert(index, name.to_string());
    }
    if let Some(arguments) = call.pointer("/function/arguments").and_then(Value::as_str) {
        parsed
            .tool_args
            .entry(index)
            .or_default()
            .push_str(arguments);
    }
}

fn parse_client_stream(raw: &str, surface: &str) -> ParsedStream {
    let mut parsed = ParsedStream {
        raw: raw.to_string(),
        ..ParsedStream::default()
    };
    for (event, data) in sse_blocks(raw) {
        if data == "[DONE]" {
            parsed.events.push("[DONE]".to_string());
            continue;
        }
        let value: Value = serde_json::from_str(&data)
            .unwrap_or_else(|error| panic!("invalid SSE JSON for {surface}: {error}: {data}"));
        if let Some(ref event) = event {
            parsed.events.push(event.clone());
        }
        match surface {
            "chat" => {
                if let Some(usage) = value.get("usage").filter(|usage| !usage.is_null()) {
                    parsed.usages.push(usage.clone());
                }
                if let Some(choice) = value
                    .get("choices")
                    .and_then(Value::as_array)
                    .and_then(|choices| choices.first())
                {
                    if let Some(delta) = choice.get("delta") {
                        if let Some(text) = delta.get("content").and_then(Value::as_str) {
                            parsed.text_fragments.push(text.to_string());
                        }
                        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
                            for call in calls {
                                let index =
                                    call.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                                remember_tool(&mut parsed, index, call);
                            }
                        }
                    }
                    if let Some(finish) = choice.get("finish_reason").and_then(Value::as_str) {
                        parsed.finish_reasons.push(finish.to_string());
                    }
                }
            }
            "anthropic" => {
                let event_type = event.as_deref().unwrap_or_default();
                match event_type {
                    "content_block_start" => {
                        let index =
                            value.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                        let block = value.get("content_block").cloned().unwrap_or(Value::Null);
                        if block.get("type").and_then(Value::as_str) == Some("tool_use") {
                            parsed.tool_names.insert(
                                index,
                                block
                                    .get("name")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .to_string(),
                            );
                            parsed.tool_ids.insert(
                                index,
                                block
                                    .get("id")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .to_string(),
                            );
                        }
                    }
                    "content_block_delta" => {
                        let index =
                            value.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                        match value.pointer("/delta/type").and_then(Value::as_str) {
                            Some("text_delta") => parsed.text_fragments.push(
                                value
                                    .pointer("/delta/text")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .to_string(),
                            ),
                            Some("input_json_delta") => {
                                parsed.tool_args.entry(index).or_default().push_str(
                                    value
                                        .pointer("/delta/partial_json")
                                        .and_then(Value::as_str)
                                        .unwrap_or_default(),
                                )
                            }
                            _ => {}
                        }
                    }
                    "message_delta" => {
                        if let Some(stop) =
                            value.pointer("/delta/stop_reason").and_then(Value::as_str)
                        {
                            parsed.finish_reasons.push(stop.to_string());
                        }
                        if let Some(usage) = value.get("usage") {
                            parsed.usages.push(usage.clone());
                        }
                    }
                    _ => {}
                }
            }
            "responses" => {
                let event_type = event.as_deref().unwrap_or_default();
                match event_type {
                    "response.output_text.delta" => parsed.text_fragments.push(
                        value
                            .get("delta")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                    ),
                    "response.output_item.added" => {
                        let item = value.get("item").cloned().unwrap_or(Value::Null);
                        if item.get("type").and_then(Value::as_str) == Some("function_call") {
                            let index = value
                                .get("output_index")
                                .and_then(Value::as_u64)
                                .unwrap_or(0) as usize;
                            parsed.tool_names.insert(
                                index,
                                item.get("name")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .to_string(),
                            );
                            parsed.tool_ids.insert(
                                index,
                                item.get("call_id")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .to_string(),
                            );
                        }
                    }
                    "response.function_call_arguments.delta" => {
                        let index = value
                            .get("output_index")
                            .and_then(Value::as_u64)
                            .unwrap_or(0) as usize;
                        parsed.tool_args.entry(index).or_default().push_str(
                            value
                                .get("delta")
                                .and_then(Value::as_str)
                                .unwrap_or_default(),
                        );
                    }
                    "response.completed" => {
                        if let Some(usage) = value.pointer("/response/usage") {
                            parsed.usages.push(usage.clone());
                        }
                        // The Responses wire has no finish_reason field; item
                        // status on the terminal envelope is the protocol's
                        // completion signal, so record it the way the Chat and
                        // Gemini arms record their terminal finish reasons.
                        if let Some(output) =
                            value.pointer("/response/output").and_then(Value::as_array)
                        {
                            for item in output {
                                if item.get("status").and_then(Value::as_str) != Some("completed") {
                                    continue;
                                }
                                match item.get("type").and_then(Value::as_str) {
                                    Some("function_call") => {
                                        parsed.finish_reasons.push("tool_calls".to_string())
                                    }
                                    Some("message") => {
                                        parsed.finish_reasons.push("message_completed".to_string())
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            "gemini" => {
                if let Some(usage) = value.get("usageMetadata") {
                    parsed.usages.push(usage.clone());
                }
                if let Some(candidate) = value
                    .get("candidates")
                    .and_then(Value::as_array)
                    .and_then(|candidates| candidates.first())
                {
                    if let Some(finish) = candidate.get("finishReason").and_then(Value::as_str) {
                        parsed.finish_reasons.push(finish.to_string());
                    }
                    if let Some(parts) = candidate
                        .pointer("/content/parts")
                        .and_then(Value::as_array)
                    {
                        for part in parts {
                            if let Some(text) = part.get("text").and_then(Value::as_str) {
                                parsed.text_fragments.push(text.to_string());
                            }
                            if let Some(call) = part.get("functionCall") {
                                let name = call
                                    .get("name")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .to_string();
                                parsed
                                    .gemini_calls
                                    .insert(name, call.get("args").cloned().unwrap_or(Value::Null));
                            }
                        }
                    }
                }
            }
            other => panic!("unknown stream surface {other}"),
        }
    }
    parsed
}

fn assert_stream_semantics(surface: &str, raw: &str) {
    assert!(
        !raw.contains("source_") && !raw.contains("client_credential"),
        "internal stream fields leaked on {surface}"
    );
    let parsed = parse_client_stream(raw, surface);
    assert_eq!(
        parsed.text_fragments.concat(),
        FIXTURE_TEXT,
        "{surface} text stream changed or duplicated"
    );
    assert!(
        !parsed.usages.is_empty(),
        "{surface} stream omitted usage evidence"
    );
    match surface {
        "gemini" => {
            assert_eq!(
                parsed.gemini_calls.get("lookup_one"),
                Some(&json!({"query":"one"})),
                "Gemini lookup_one args broken"
            );
            assert_eq!(
                parsed.gemini_calls.get("lookup_two"),
                Some(&json!({"query":"two"})),
                "Gemini lookup_two args broken"
            );
            assert!(
                parsed.finish_reasons.iter().any(|finish| finish == "STOP"),
                "Gemini terminal finish missing"
            );
        }
        _ => {
            let mut pairs = parsed
                .tool_names
                .iter()
                .map(|(index, name)| {
                    (
                        *index,
                        name.clone(),
                        parsed.tool_args.get(index).cloned().unwrap_or_default(),
                    )
                })
                .collect::<Vec<_>>();
            pairs.sort_by_key(|(index, _, _)| *index);
            assert_eq!(
                pairs.len(),
                2,
                "{surface} must expose exactly two tools: {pairs:?}"
            );
            let by_name = pairs
                .iter()
                .map(|(_, name, args)| (name.clone(), args.clone()))
                .collect::<HashMap<_, _>>();
            assert_eq!(
                by_name.get("lookup_one").map(String::as_str),
                Some(TOOL_ONE_ARGS)
            );
            assert_eq!(
                by_name.get("lookup_two").map(String::as_str),
                Some(TOOL_TWO_ARGS)
            );
            let ids = parsed.tool_ids.values().collect::<HashSet<_>>();
            assert_eq!(
                ids.len(),
                2,
                "{surface} tool ids merged: {:?}",
                parsed.tool_ids
            );
            assert!(
                parsed
                    .finish_reasons
                    .iter()
                    .any(|finish| matches!(finish.as_str(), "tool_calls" | "tool_use")),
                "{surface} terminal finish missing"
            );
        }
    }
    if surface == "responses" {
        assert!(parsed
            .events
            .iter()
            .any(|event| event == "response.created"));
        assert!(parsed
            .events
            .iter()
            .any(|event| event == "response.completed"));
        assert_eq!(
            parsed
                .events
                .iter()
                .filter(|event| event.as_str() == "response.output_text.done")
                .count(),
            1
        );
        let completed_index = parsed
            .events
            .iter()
            .position(|event| event == "response.completed")
            .expect("completed frame");
        let done_index = parsed
            .events
            .iter()
            .position(|event| event == "[DONE]")
            .expect("Responses [DONE] sentinel");
        assert!(completed_index < done_index);
    }
    if surface == "anthropic" {
        assert!(parsed.events.iter().any(|event| event == "message_start"));
        assert!(parsed.events.iter().any(|event| event == "message_delta"));
        assert!(parsed.events.iter().any(|event| event == "message_stop"));
    }
}

fn message_tool_calls(message: &Value) -> Vec<Value> {
    message
        .get("tool_calls")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn tool_call_pair(calls: &[Value]) -> HashMap<String, (String, Value)> {
    calls
        .iter()
        .map(|call| {
            let name = call
                .pointer("/function/name")
                .or_else(|| call.get("name"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let id = call
                .get("id")
                .or_else(|| call.get("call_id"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let args = call
                .pointer("/function/arguments")
                .or_else(|| call.get("arguments"))
                .and_then(Value::as_str)
                .map(|text| serde_json::from_str::<Value>(text).unwrap_or(Value::Null))
                .or_else(|| call.get("input").cloned())
                .unwrap_or(Value::Null);
            (name, (id, args))
        })
        .collect()
}

fn assert_tool_pair(pair: &HashMap<String, (String, Value)>, surface: &str) {
    assert_eq!(
        pair.len(),
        2,
        "{surface} did not preserve two distinct tools"
    );
    assert_eq!(pair["lookup_one"].1, json!({"query":"one"}));
    assert_eq!(pair["lookup_two"].1, json!({"query":"two"}));
    let ids = pair
        .values()
        .map(|(id, _)| id.clone())
        .collect::<HashSet<_>>();
    assert_eq!(ids.len(), 2, "{surface} merged tool call identities");
}

async fn read_json(response: reqwest::Response, surface: &str) -> Value {
    let status = response.status();
    let bytes = response.bytes().await.expect("read body");
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        status.is_success(),
        "{surface} nonstream failed status={status} body={text}"
    );
    serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("{surface} invalid JSON: {error}: {text}"))
}

async fn read_stream(response: reqwest::Response, surface: &str) -> String {
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let mut raw = String::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        raw.push_str(&String::from_utf8_lossy(&chunk.expect("stream chunk")));
    }
    assert!(
        status.is_success(),
        "{surface} stream failed status={status} body={raw}"
    );
    assert!(
        content_type.contains("text/event-stream"),
        "{surface} stream content-type {content_type}"
    );
    raw
}

fn assert_nonstream_response(surface: &str, body: &Value, upstream: &'static str) {
    let raw = serde_json::to_string(body).unwrap();
    assert!(
        !raw.contains("source_") && !raw.contains("client_credential"),
        "internal response field leaked on {surface}"
    );
    match surface {
        "chat" => {
            let message = body.pointer("/choices/0/message").expect("chat message");
            assert_eq!(
                message.get("content").and_then(Value::as_str),
                Some(FIXTURE_TEXT)
            );
            assert_tool_pair(&tool_call_pair(&message_tool_calls(message)), surface);
            assert_eq!(
                body.pointer("/choices/0/finish_reason")
                    .and_then(Value::as_str),
                Some("tool_calls")
            );
            assert_eq!(
                body.pointer("/usage/total_tokens").and_then(Value::as_u64),
                Some(12)
            );
        }
        "anthropic" => {
            assert_eq!(body.get("type").and_then(Value::as_str), Some("message"));
            assert_eq!(body.get("role").and_then(Value::as_str), Some("assistant"));
            let content = body
                .get("content")
                .and_then(Value::as_array)
                .expect("blocks");
            let text = content
                .iter()
                .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .collect::<String>();
            assert_eq!(text, FIXTURE_TEXT);
            let calls = content
                .iter()
                .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_use"))
                .cloned()
                .collect::<Vec<_>>();
            let pair = calls
                .iter()
                .map(|call| {
                    (
                        call.get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        (
                            call.get("id")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                            call.get("input").cloned().unwrap_or(Value::Null),
                        ),
                    )
                })
                .collect::<HashMap<_, _>>();
            assert_tool_pair(&pair, surface);
            assert_eq!(
                body.get("stop_reason").and_then(Value::as_str),
                Some("tool_use")
            );
            assert_eq!(
                body.pointer("/usage/output_tokens").and_then(Value::as_u64),
                Some(5)
            );
        }
        "responses" => {
            assert_eq!(body.get("object").and_then(Value::as_str), Some("response"));
            assert_eq!(
                body.get("status").and_then(Value::as_str),
                Some("completed")
            );
            if upstream == "responses" {
                assert!(
                    body.get("error") == Some(&Value::Null),
                    "native Responses envelope must preserve an explicit null error"
                );
            }
            let output = body
                .get("output")
                .and_then(Value::as_array)
                .expect("output");
            let text = output
                .iter()
                .filter(|item| item.get("type").and_then(Value::as_str) == Some("message"))
                .flat_map(|item| {
                    item.get("content")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default()
                })
                .filter_map(|part| part.get("text").and_then(Value::as_str).map(str::to_owned))
                .collect::<String>();
            assert_eq!(text, FIXTURE_TEXT);
            let calls = output
                .iter()
                .filter(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
                .cloned()
                .collect::<Vec<_>>();
            assert_tool_pair(&tool_call_pair(&calls), surface);
            assert_eq!(
                body.pointer("/usage/total_tokens").and_then(Value::as_u64),
                Some(12)
            );
        }
        "gemini" => {
            let candidate = body
                .get("candidates")
                .and_then(Value::as_array)
                .and_then(|candidates| candidates.first())
                .expect("Gemini candidate");
            let parts = candidate
                .pointer("/content/parts")
                .and_then(Value::as_array)
                .expect("Gemini parts");
            let text = parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str).map(str::to_owned))
                .collect::<String>();
            assert_eq!(text, FIXTURE_TEXT);
            let calls = parts
                .iter()
                .filter_map(|part| part.get("functionCall"))
                .map(|call| {
                    (
                        call.get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        (
                            String::new(),
                            call.get("args").cloned().unwrap_or(Value::Null),
                        ),
                    )
                })
                .collect::<HashMap<_, _>>();
            assert_eq!(calls.len(), 2, "Gemini did not preserve two tools");
            assert_eq!(calls["lookup_one"].1, json!({"query":"one"}));
            assert_eq!(calls["lookup_two"].1, json!({"query":"two"}));
            assert_eq!(
                candidate.get("finishReason").and_then(Value::as_str),
                Some("STOP")
            );
            assert_eq!(
                body.pointer("/usageMetadata/totalTokenCount")
                    .and_then(Value::as_u64),
                Some(12)
            );
        }
        other => panic!("unknown client surface {other}"),
    }
}

async fn post_json(url: String, request: Value, surface: &str) -> reqwest::Response {
    http_client()
        .post(url)
        .json(&request)
        .send()
        .await
        .unwrap_or_else(|error| panic!("{surface} request failed: {error}"))
}

async fn run_client_case(
    surface: &'static str,
    upstream: &'static str,
    gateway: &Gateway_server,
    stream: bool,
) {
    let url = match surface {
        "chat" => format!("{}/v1/chat/completions", gateway.base_url),
        "anthropic" => format!("{}/v1/messages", gateway.base_url),
        "responses" => format!("{}/v1/responses", gateway.base_url),
        "gemini" => format!(
            "{}/v1beta/models/{ALIAS}:{}",
            gateway.base_url,
            if stream {
                "streamGenerateContent"
            } else {
                "generateContent"
            }
        ),
        other => panic!("unknown client surface {other}"),
    };
    let request = match surface {
        "chat" => chat_client_request(stream),
        "anthropic" => anthropic_client_request(stream),
        "responses" => responses_client_request(stream),
        "gemini" => gemini_client_request(),
        other => panic!("unknown client surface {other}"),
    };
    let response = post_json(url, request, surface).await;
    if stream {
        let raw = read_stream(response, surface).await;
        assert_stream_semantics(surface, &raw);
    } else {
        let body = read_json(response, surface).await;
        assert_nonstream_response(surface, &body, upstream);
    }
}

async fn run_pair(surface: &'static str, upstream: &'static str) {
    let fixture = start_fixture(upstream).await;
    let provider = build_provider(&provider_row(&fixture))
        .unwrap_or_else(|error| panic!("build {upstream} provider: {error}"));
    let gateway = start_gateway(Arc::from(provider), upstream == "responses").await;
    for stream in [false, true] {
        let before = fixture.capture.snapshot().len();
        run_client_case(surface, upstream, &gateway, stream).await;
        let captures = fixture.capture.snapshot();
        assert_eq!(
            captures.len(),
            before + 1,
            "{surface}->{upstream} stream={stream} should send exactly one upstream request"
        );
        let capture = captures.last().unwrap();
        assert_eq!(
            capture
                .body
                .as_ref()
                .and_then(|body| body.get("stream"))
                .and_then(Value::as_bool),
            (upstream != "gemini").then_some(stream),
            "{surface}->{upstream} stream flag mismatch"
        );
        assert_ne!(
            capture
                .body
                .as_ref()
                .and_then(|body| body.get("model"))
                .and_then(Value::as_str),
            Some(ALIAS),
            "{surface}->{upstream} leaked the local route alias instead of the resolved model"
        );
        let body = assert_common_capture(capture, upstream, surface, stream);
        assert!(
            body.get("max_tokens").is_some()
                || body.get("max_output_tokens").is_some()
                || body.pointer("/generationConfig/maxOutputTokens").is_some(),
            "output budget missing upstream"
        );
    }
}

macro_rules! protocol_matrix_case {
    ($name:ident, $surface:literal, $upstream:literal) => {
        #[tokio::test]
        async fn $name() {
            run_pair($surface, $upstream).await;
        }
    };
}

protocol_matrix_case!(chat_to_chat, "chat", "chat");
protocol_matrix_case!(chat_to_anthropic, "chat", "anthropic");
protocol_matrix_case!(chat_to_responses, "chat", "responses");
protocol_matrix_case!(chat_to_gemini, "chat", "gemini");
protocol_matrix_case!(anthropic_to_chat, "anthropic", "chat");
protocol_matrix_case!(anthropic_to_anthropic, "anthropic", "anthropic");
protocol_matrix_case!(anthropic_to_responses, "anthropic", "responses");
protocol_matrix_case!(anthropic_to_gemini, "anthropic", "gemini");
protocol_matrix_case!(responses_to_chat, "responses", "chat");
protocol_matrix_case!(responses_to_anthropic, "responses", "anthropic");
protocol_matrix_case!(responses_to_responses, "responses", "responses");
protocol_matrix_case!(responses_to_gemini, "responses", "gemini");
protocol_matrix_case!(gemini_to_chat, "gemini", "chat");
protocol_matrix_case!(gemini_to_anthropic, "gemini", "anthropic");
protocol_matrix_case!(gemini_to_responses, "gemini", "responses");
protocol_matrix_case!(gemini_to_gemini, "gemini", "gemini");

#[tokio::test]
async fn unsupported_gemini_foreign_features_fail_before_the_upstream_is_called() {
    let fixture = start_fixture("anthropic").await;
    let provider: Arc<dyn Provider> = Arc::from(build_provider(&provider_row(&fixture)).unwrap());
    let gateway = start_gateway(provider, false).await;
    for patch in [
        json!({"tools":[{"googleSearch":{}}]}),
        json!({"generationConfig":{"responseMimeType":"application/json"}}),
        json!({"contents":[{"role":"user","parts":[{"fileData":{"fileUri":"fixture://file","mimeType":"application/pdf"}}]}]}),
        json!({"toolConfig":{"functionCallingConfig":{"mode":"ANY","allowedFunctionNames":["lookup_one","lookup_two"]}}}),
    ] {
        let mut body = gemini_client_request();
        body.as_object_mut()
            .unwrap()
            .extend(patch.as_object().unwrap().clone());
        let response = post_json(
            format!("{}/v1beta/models/{ALIAS}:generateContent", gateway.base_url),
            body,
            "gemini",
        )
        .await;
        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
        assert!(
            fixture.capture.snapshot().is_empty(),
            "unsupported Gemini data must never reach a foreign supplier"
        );
    }
}
