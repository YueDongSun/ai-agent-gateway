//! Local HTTP boundary regressions. All upstreams and credentials are fixtures.
use super::{anthropic::AnthropicProvider, openai::OpenAIProvider, Provider};
use crate::database::dao::{proxy_providers::CircuitBreakerConfig, routes::RouteHop};
use crate::proxy::{
    router::Router,
    server::{serve_detached, ProxyAppState},
};
use axum::{
    body::Body,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
    Json,
};
use futures_util::{stream, StreamExt};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Clone)]
#[allow(non_camel_case_types)]
struct Fixture_state {
    status: u16,
    response: Value,
    stream_body: String,
    requests: Arc<Mutex<Vec<Value>>>,
}

#[allow(non_camel_case_types)]
struct Test_server {
    base_url: String,
    abort: tokio::task::AbortHandle,
    _shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}
impl Drop for Test_server {
    fn drop(&mut self) {
        self.abort.abort();
    }
}

async fn fixture_handler(
    State(state): State<Fixture_state>,
    Json(request): Json<Value>,
) -> Response {
    let streaming = request
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    state.requests.lock().unwrap().push(request);
    if state.status >= 400 {
        return (
            StatusCode::from_u16(state.status).unwrap(),
            Json(state.response),
        )
            .into_response();
    }
    if !streaming {
        return Json(state.response).into_response();
    }
    let parts: Vec<_> = state
        .stream_body
        .as_bytes()
        .chunks(3)
        .map(|chunk| Ok::<_, std::io::Error>(bytes::Bytes::copy_from_slice(chunk)))
        .collect();
    Response::builder()
        .header("content-type", "text/event-stream")
        .body(Body::from_stream(stream::iter(parts)))
        .unwrap()
}

async fn fixture(
    status: u16,
    response: Value,
    stream_body: String,
) -> (Test_server, Fixture_state) {
    let state = Fixture_state {
        status,
        response,
        stream_body,
        requests: Default::default(),
    };
    let app = axum::Router::new()
        .route("/v1/messages", post(fixture_handler))
        .route("/v1/chat/completions", post(fixture_handler))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (
        Test_server {
            base_url: format!("http://{address}"),
            abort: handle.abort_handle(),
            _shutdown: None,
        },
        state,
    )
}

fn provider(id: &str, server: &Test_server, native: bool) -> Arc<dyn Provider> {
    if native {
        Arc::new(
            AnthropicProvider::new(
                id.into(),
                server.base_url.clone(),
                String::new(),
                vec!["model".into()],
                true,
                2_000,
                CircuitBreakerConfig::default(),
                Default::default(),
            )
            .unwrap(),
        )
    } else {
        Arc::new(
            OpenAIProvider::new(
                id.into(),
                format!("{}/v1", server.base_url),
                String::new(),
                vec!["model".into()],
                true,
                2_000,
                CircuitBreakerConfig::default(),
                Default::default(),
            )
            .unwrap(),
        )
    }
}

async fn proxy(providers: Vec<Arc<dyn Provider>>) -> Test_server {
    proxy_with_cot(providers, false).await
}

/// Like [`proxy`] but with the `/v1/responses` chain-of-thought prompt
/// injection pre-enabled, mirroring a live `proxy_set_responses_cot` toggle.
async fn proxy_with_cot(providers: Vec<Arc<dyn Provider>>, cot_enabled: bool) -> Test_server {
    let hops: Vec<_> = providers
        .iter()
        .map(|provider| RouteHop {
            provider: provider.id().into(),
            model: "model".into(),
        })
        .collect();
    let providers = providers
        .into_iter()
        .map(|provider| (provider.id().to_string(), provider))
        .collect();
    let router = Router::new(providers, [("alias".into(), hops)].into_iter().collect());
    let state = ProxyAppState {
        router: Arc::new(router),
        auth_token: String::new(),
        pinned_provider: Arc::new(Mutex::new(None)),
        responses_cot_enabled: Arc::new(std::sync::atomic::AtomicBool::new(cot_enabled)),
        passthrough_auth_allowed: false,
        db: None,
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    let (bound, handle) = serve_detached("127.0.0.1:0".parse().unwrap(), state, rx)
        .await
        .unwrap();
    Test_server {
        base_url: format!("http://{}", bound.addr),
        abort: handle.abort_handle(),
        _shutdown: Some(tx),
    }
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(4))
        .build()
        .unwrap()
}

fn sse(events: &[Value], done: bool) -> String {
    let mut wire = events
        .iter()
        .map(|event| format!("data: {event}\r\n\r\n"))
        .collect::<String>();
    if done {
        wire.push_str("data: [DONE]\r\n\r\n");
    }
    wire
}

fn frames(wire: &str) -> Vec<Value> {
    wire.lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter(|line| *line != "[DONE]")
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn oai(delta: Value, finish: Option<&str>) -> Value {
    json!({"id":"fixture","object":"chat.completion.chunk","created":1,"model":"model",
        "choices":[{"index":0,"delta":delta,"finish_reason":finish}]})
}

fn native_response() -> Value {
    json!({"id":"native","type":"message","role":"assistant","model":"model",
        "content":[{"type":"thinking","thinking":"推理😀","signature":"opaque-signature"},
            {"type":"redacted_thinking","data":"opaque-encrypted"},
            {"type":"text","text":"答案"},
            {"type":"tool_use","id":"call_1","name":"lookup","input":{"query":"中文"}}],
        "stop_reason":"tool_use","stop_sequence":null,
        "usage":{"input_tokens":7,"output_tokens":5,"cache_read_input_tokens":3,"cache_creation_input_tokens":2,"server_tool_use":{"web_search_requests":1}},
        "context_management":{"applied_edits":[]}})
}

#[tokio::test]
async fn native_http_roundtrip_preserves_signed_blocks_and_usage() {
    let response = native_response();
    let (upstream, state) = fixture(200, response.clone(), String::new()).await;
    let gateway = proxy(vec![provider("native", &upstream, true)]).await;
    let request = json!({"model":"alias","max_tokens":2048,"stream":false,
        "thinking":{"type":"enabled","budget_tokens":1024},
        "messages":[{"role":"assistant","content":response["content"]}],
        "system":[{"type":"text","text":"fixture","cache_control":{"type":"ephemeral"}}]});
    let result: Value = client()
        .post(format!("{}/v1/messages", gateway.base_url))
        .json(&request)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let mut expected = response;
    expected["model"] = json!("alias");
    assert_eq!(result, expected);
    let mut expected_request = request;
    expected_request["model"] = json!("model");
    assert_eq!(state.requests.lock().unwrap()[0], expected_request);
}

#[tokio::test]
async fn native_sse_preserves_thinking_signature_tool_order_and_cache_usage() {
    let events = vec![
        json!({"type":"message_start","message":{"id":"native","model":"model","role":"assistant","content":[],"usage":{"input_tokens":7,"output_tokens":0,"cache_read_input_tokens":3}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"推理😀"}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"opaque-signature"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"content_block_start","index":1,"content_block":{"type":"redacted_thinking","data":"opaque"}}),
        json!({"type":"content_block_stop","index":1}),
        json!({"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"call_1","name":"lookup","input":{}}}),
        json!({"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"query\":\"中文\"}"}}),
        json!({"type":"content_block_stop","index":2}),
        json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":5}}),
        json!({"type":"message_stop"}),
    ];
    let (upstream, _) = fixture(200, Value::Null, sse(&events, false)).await;
    let gateway = proxy(vec![provider("native", &upstream, true)]).await;
    let request = json!({"model":"alias","max_tokens":2048,"stream":true,"messages":[{"role":"user","content":"fixture"}]});
    let wire = client()
        .post(format!("{}/v1/messages", gateway.base_url))
        .json(&request)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .text()
        .await
        .unwrap();
    let mut expected = events;
    expected[0]["message"]["model"] = json!("alias");
    assert_eq!(frames(&wire), expected);
    assert!(!wire.contains("source_anthropic"));
}

#[tokio::test]
async fn openai_http_stream_preserves_reasoning_aliases_extensions_and_usage() {
    let events = vec![
        oai(
            json!({"role":"assistant","reasoning_content":"推理😀","reasoning":"alias","reasoning_details":[{"type":"summary","text":"fixture"}],"refusal":null}),
            None,
        ),
        oai(json!({"content":"答案"}), Some("stop")),
        json!({"id":"fixture","object":"chat.completion.chunk","created":1,"model":"model","choices":[],"usage":{"prompt_tokens":7,"completion_tokens":5,"total_tokens":12,"completion_tokens_details":{"reasoning_tokens":3}}}),
    ];
    let (upstream, _) = fixture(200, Value::Null, sse(&events, true)).await;
    let gateway = proxy(vec![provider("openai", &upstream, false)]).await;
    let request =
        json!({"model":"alias","stream":true,"messages":[{"role":"user","content":"fixture"}]});
    let wire = client()
        .post(format!("{}/v1/chat/completions", gateway.base_url))
        .json(&request)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .text()
        .await
        .unwrap();
    let actual = frames(&wire);
    assert_eq!(
        actual[0]["choices"][0]["delta"],
        events[0]["choices"][0]["delta"]
    );
    assert_eq!(actual[2]["usage"], events[2]["usage"]);
    assert!(wire.ends_with("data: [DONE]\n\n"));
    assert!(!wire.contains("source_anthropic"));
}

#[tokio::test]
async fn http_auth_validation_errors_never_fail_over_but_rate_limits_do() {
    for native in [false, true] {
        for status in [400, 401, 403, 422, 429, 503] {
            let (upstream, first) = fixture(
                status,
                json!({"error":{"message":"fixture"}}),
                String::new(),
            )
            .await;
            let (fallback, second) = fixture(
                status,
                json!({"error":{"message":"fixture"}}),
                String::new(),
            )
            .await;
            let gateway = proxy(vec![
                provider("first", &upstream, native),
                provider("second", &fallback, native),
            ])
            .await;
            for streaming in [false, true] {
                let request = json!({"model":"alias","stream":streaming,"messages":[{"role":"user","content":"fixture"}]});
                let result = client()
                    .post(format!("{}/v1/chat/completions", gateway.base_url))
                    .json(&request)
                    .send()
                    .await
                    .unwrap();
                assert_eq!(result.status().as_u16(), status);
            }
            assert_eq!(first.requests.lock().unwrap().len(), 2);
            assert_eq!(
                second.requests.lock().unwrap().len(),
                if status == 429 || status >= 500 { 2 } else { 0 }
            );
        }
    }
}

#[tokio::test]
async fn late_stream_error_has_no_failover_or_success_terminator() {
    for endpoint in ["chat/completions", "messages", "responses"] {
        let events = [
            oai(json!({"role":"assistant","content":"partial"}), None),
            json!({"error":{"type":"invalid_request_error","message":"fixture"}}),
        ];
        let (upstream, first) = fixture(200, Value::Null, sse(&events, false)).await;
        let (fallback, second) = fixture(503, Value::Null, String::new()).await;
        let gateway = proxy(vec![
            provider("first", &upstream, false),
            provider("second", &fallback, false),
        ])
        .await;
        let request = json!({"model":"alias","stream":true,"max_tokens":32,"messages":[{"role":"user","content":"fixture"}],"input":"fixture"});
        let wire = client()
            .post(format!("{}/v1/{endpoint}", gateway.base_url))
            .json(&request)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(wire.contains("error"));
        assert!(!wire.contains("[DONE]"));
        assert!(!wire.contains("message_stop"));
        assert!(!wire.contains("response.completed"));
        assert_eq!(first.requests.lock().unwrap().len(), 1);
        assert!(second.requests.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn responses_done_follows_completed_frame() {
    let events = [
        oai(json!({"role":"assistant","content":"fixture"}), None),
        oai(json!({}), Some("stop")),
    ];
    let (upstream, _) = fixture(200, Value::Null, sse(&events, true)).await;
    let gateway = proxy(vec![provider("openai", &upstream, false)]).await;
    let wire = client()
        .post(format!("{}/v1/responses", gateway.base_url))
        .json(&json!({"model":"alias","input":"fixture","stream":true}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(wire.find("response.completed").unwrap() < wire.find("[DONE]").unwrap());
    assert_eq!(wire.matches("[DONE]").count(), 1);
}

/// End-to-end codex-surface tool conversation through the real proxy:
/// flat Responses tools/tool_choice and `function_call` /
/// `function_call_output` input items must reach an OpenAI-shaped upstream
/// in the nested chat shapes, and streamed tool-call deltas must come back
/// as function_call items with argument deltas, usage, and correct
/// terminal ordering. This is the HTTP-level regression for the Codex CLI
/// relay fix.
#[tokio::test]
async fn responses_stream_relays_tool_conversations_end_to_end() {
    let tool_call_chunk = json!({"id":"fixture","object":"chat.completion.chunk","created":1,
        "model":"model",
        "choices":[{"index":0,"delta":{"role":"assistant","tool_calls":[
            {"index":0,"id":"call_1","type":"function","function":{"name":"shell","arguments":""}}
        ]},"finish_reason":null}]});
    let args_chunk = json!({"id":"fixture","object":"chat.completion.chunk","created":1,
        "model":"model",
        "choices":[{"index":0,"delta":{"tool_calls":[
            {"index":0,"function":{"arguments":"{\"cmd\":\"ls\"}"}}
        ]},"finish_reason":"tool_calls"}]});
    let usage_chunk = json!({"id":"fixture","object":"chat.completion.chunk","created":1,
        "model":"model","choices":[],
        "usage":{"prompt_tokens":7,"completion_tokens":3,"total_tokens":10}});
    let wire = sse(&[tool_call_chunk, args_chunk, usage_chunk], true);
    let (upstream, state) = fixture(200, Value::Null, wire).await;
    let gateway = proxy(vec![provider("openai", &upstream, false)]).await;

    let response = client()
        .post(format!("{}/v1/responses", gateway.base_url))
        .json(&json!({
            "model":"alias",
            "input":[
                {"type":"message","role":"user","content":"list files"},
                {"type":"function_call","call_id":"call_1","name":"shell","arguments":"{\"cmd\":\"ls\"}"},
                {"type":"function_call_output","call_id":"call_1","output":"a.txt"},
            ],
            "tools":[{"type":"function","name":"shell","description":"run",
                      "parameters":{"type":"object","properties":{}}}],
            "tool_choice":{"type":"function","name":"shell"},
            "stream":true,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    let body = response.text().await.unwrap();

    // Upstream saw the nested chat shapes, the hop model (not the alias),
    // and the converted tool history.
    let upstream_request = &state.requests.lock().unwrap()[0];
    assert_eq!(
        upstream_request["model"], "model",
        "hop model reaches upstream"
    );
    let tools = upstream_request["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 1, "non-function tools are dropped");
    assert_eq!(tools[0]["function"]["name"], "shell");
    assert_eq!(upstream_request["tool_choice"]["function"]["name"], "shell");
    let messages = upstream_request["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 3, "reasoning-free items all convert");
    assert_eq!(messages[1]["role"], "assistant");
    assert_eq!(messages[1]["tool_calls"][0]["id"], "call_1");
    assert_eq!(messages[1]["tool_calls"][0]["function"]["name"], "shell");
    assert_eq!(messages[2]["role"], "tool");
    assert_eq!(messages[2]["tool_call_id"], "call_1");
    assert_eq!(messages[2]["content"], "a.txt");

    // Proxy emitted the full function-call event sequence with usage and
    // terminal ordering: arguments.done < completed < [DONE].
    for expected in [
        "event: response.created",
        "event: response.output_item.added",
        "\"type\":\"function_call\"",
        "event: response.function_call_arguments.delta",
        "event: response.function_call_arguments.done",
        "event: response.completed",
    ] {
        assert!(body.contains(expected), "missing {expected} in:\n{body}");
    }
    let args_delta_pos = body.find("response.function_call_arguments.delta").unwrap();
    assert!(
        body[args_delta_pos..].contains("\\\"cmd\\\""),
        "argument fragment relayed, got:\n{body}"
    );
    let completed_pos = body.find("response.completed").unwrap();
    let done_pos = body.find("[DONE]").unwrap();
    assert!(
        body.find("response.function_call_arguments.done").unwrap() < completed_pos
            && completed_pos < done_pos,
        "terminal ordering broken:\n{body}"
    );
    assert_eq!(body.matches("[DONE]").count(), 1);
    let completed_frame = body[completed_pos..]
        .split("\n")
        .find(|line| line.starts_with("data: ") && line.contains("response.completed"))
        .expect("completed data frame");
    let completed: Value = serde_json::from_str(
        completed_frame
            .strip_prefix("data: ")
            .expect("data prefix")
            .trim(),
    )
    .unwrap();
    assert_eq!(completed["response"]["usage"]["input_tokens"], 7);
    let output = completed["response"]["output"].as_array().unwrap();
    assert!(
        output.iter().any(|item| item["type"] == "function_call"
            && item["arguments"] == "{\"cmd\":\"ls\"}"
            && item["call_id"] == "call_1"),
        "completed output carries the full call: {output:?}"
    );
}

#[tokio::test]
async fn responses_parallel_tool_follow_up_groups_anthropic_history() {
    let wire = sse(
        &[
            json!({"type":"message_start","message":{"usage":{"input_tokens":20,"output_tokens":0}}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"GROUPED"}}),
            json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}),
            json!({"type":"message_stop"}),
        ],
        false,
    );
    let (upstream, state) = fixture(200, Value::Null, wire).await;
    let gateway = proxy(vec![provider("native", &upstream, true)]).await;
    let mut input = vec![
        json!({"role":"user","content":"Read four public fixtures"}),
        json!({"role":"assistant","content":"Reading fixtures."}),
    ];
    for index in 0..4 {
        input.push(json!({"type":"function_call","call_id":format!("call_{index}"),
            "name":"read_fixture","arguments":json!({"fixture_index":index,"quantity":200.0}).to_string()}));
    }
    for index in 0..4 {
        input.push(
            json!({"type":"function_call_output","call_id":format!("call_{index}"),
            "output":format!("FIXTURE_{index}")}),
        );
    }
    let response = client()
        .post(format!("{}/v1/responses", gateway.base_url))
        .json(
            &json!({"model":"alias","input":input,"stream":true,"tool_choice":"auto",
            "tools":[{"type":"function","name":"read_fixture","parameters":{"type":"object",
                "properties":{"fixture_index":{"type":"integer"},"quantity":{"type":"number"}}}}]}),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    let body = response.text().await.unwrap();
    let request = state.requests.lock().unwrap()[0].clone();
    assert_eq!(request["model"], "model");
    assert_eq!(request["tool_choice"], json!({"type":"auto"}));
    assert_eq!(request["tools"][0]["name"], "read_fixture");
    let messages = request["messages"].as_array().unwrap();
    assert_eq!(
        messages.len(),
        3,
        "one initial user turn, one call turn, one result turn"
    );
    assert_eq!(messages[1]["content"].as_array().unwrap().len(), 5);
    assert_eq!(messages[2]["content"].as_array().unwrap().len(), 4);
    for index in 0..4 {
        let call = &messages[1]["content"][index + 1];
        let result = &messages[2]["content"][index];
        assert_eq!(call["type"], "tool_use");
        assert_eq!(call["input"]["fixture_index"], index);
        assert!(
            call["input"]["quantity"].is_f64(),
            "history grouping must not rewrite numeric arguments"
        );
        assert_eq!(result["type"], "tool_result");
        assert_eq!(result["tool_use_id"], call["id"]);
        assert_eq!(result["content"], format!("FIXTURE_{index}"));
    }
    let events = frames(&body);
    assert_eq!(events.last().unwrap()["type"], "response.completed");
    assert_eq!(
        events.last().unwrap()["response"]["output"][0]["content"][0]["text"],
        "GROUPED"
    );
    assert!(
        body.find("event: response.output_text.done").unwrap()
            < body.find("event: response.output_item.done").unwrap()
    );
    assert!(body.ends_with(crate::proxy::responses::STREAM_DONE));
}

#[tokio::test]
async fn precommit_sse_validation_error_is_terminal_and_empty_stream_can_fail_over() {
    for wire in [
        sse(
            &[json!({"error":{"type":"invalid_request_error","message":"fixture"}})],
            false,
        ),
        String::new(),
    ] {
        let is_empty = wire.is_empty();
        let (upstream, _) = fixture(200, Value::Null, wire).await;
        let (fallback, second) = fixture(503, Value::Null, String::new()).await;
        let gateway = proxy(vec![
            provider("first", &upstream, false),
            provider("second", &fallback, false),
        ])
        .await;
        let response = client().post(format!("{}/v1/chat/completions",gateway.base_url)).json(&json!({"model":"alias","stream":true,"messages":[{"role":"user","content":"fixture"}]})).send().await.unwrap();
        assert_eq!(response.status().as_u16(), if is_empty { 503 } else { 400 });
        assert_eq!(second.requests.lock().unwrap().len(), usize::from(is_empty));
    }
}

#[test]
fn auto_routes_use_surface_priority_then_stable_id_without_model_substitution() {
    use super::mock::MockProvider;
    let providers: HashMap<String, Arc<dyn Provider>> = ["a", "b", "c"]
        .into_iter()
        .map(|id| {
            (
                id.into(),
                Arc::new(MockProvider::new(
                    id.into(),
                    vec!["shared".into()],
                    true,
                    CircuitBreakerConfig::default(),
                )) as Arc<dyn Provider>,
            )
        })
        .collect();
    let router = Router::new(providers, HashMap::new()).with_queues(
        [("claude".into(), vec!["c".into(), "b".into()])]
            .into_iter()
            .collect(),
    );
    let resolved = router.resolve_route_for("claude", "shared").unwrap();
    assert_eq!(
        resolved
            .iter()
            .map(|hop| hop.provider.as_str())
            .collect::<Vec<_>>(),
        vec!["c", "b", "a"]
    );
    assert!(resolved.iter().all(|hop| hop.model == "shared"));
    assert_eq!(
        router
            .resolve_route_for("chat", "shared")
            .unwrap()
            .iter()
            .map(|hop| hop.provider.as_str())
            .collect::<Vec<_>>(),
        vec!["a", "b", "c"]
    );
}
#[tokio::test]
async fn openai_nonstream_extensions_and_cross_protocol_reasoning_are_preserved() {
    let response = json!({"id":"fixture","object":"chat.completion","created":1,"model":"model","system_fingerprint":"fixture",
        "choices":[{"index":0,"message":{"role":"assistant","reasoning_content":"推理😀","content":"答案","reasoning_details":[{"type":"summary","text":"fixture"}]},"finish_reason":"stop","logprobs":{"content":[]}}],
        "usage":{"prompt_tokens":7,"completion_tokens":5,"total_tokens":12,"completion_tokens_details":{"reasoning_tokens":3}}});
    let (upstream, _) = fixture(200, response.clone(), String::new()).await;
    let gateway = proxy(vec![provider("openai", &upstream, false)]).await;
    let request =
        json!({"model":"alias","messages":[{"role":"user","content":"fixture"}],"max_tokens":32});
    let result: Value = client()
        .post(format!("{}/v1/chat/completions", gateway.base_url))
        .json(&request)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(result, response);
    let result: Value = client()
        .post(format!("{}/v1/messages", gateway.base_url))
        .json(&request)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        result["content"][0],
        json!({"type":"thinking","thinking":"推理😀"})
    );
    assert_eq!(result["content"][1], json!({"type":"text","text":"答案"}));
    assert!(!result.to_string().contains("signature"));
}

#[tokio::test]
async fn cross_protocol_reasoning_text_and_tool_blocks_have_valid_indices() {
    let events = [
        oai(
            json!({"role":"assistant","reasoning":"推理😀","content":"答案"}),
            None,
        ),
        oai(
            json!({"tool_calls":[{"index":0,"id":"call_1","function":{"name":"lookup","arguments":"{\"query\":\"中文\"}"}}]}),
            None,
        ),
        oai(json!({"content":"continued"}), Some("tool_calls")),
    ];
    let (upstream, _) = fixture(200, Value::Null, sse(&events, true)).await;
    let gateway = proxy(vec![provider("openai", &upstream, false)]).await;
    let wire = client().post(format!("{}/v1/messages",gateway.base_url)).json(&json!({"model":"alias","stream":true,"max_tokens":32,"messages":[{"role":"user","content":"fixture"}]})).send().await.unwrap().error_for_status().unwrap().text().await.unwrap();
    let events = frames(&wire);
    let mut open = HashMap::new();
    let mut starts = Vec::new();
    for event in &events {
        let index = event["index"].as_u64().unwrap_or(0);
        match event["type"].as_str().unwrap() {
            "content_block_start" => {
                let kind = event["content_block"]["type"].as_str().unwrap();
                assert!(open.insert(index, kind).is_none());
                starts.push(kind);
                if kind == "tool_use" {
                    assert_eq!(event["content_block"]["input"], json!({}));
                }
            }
            "content_block_delta" => {
                let expected = match event["delta"]["type"].as_str().unwrap() {
                    "thinking_delta" => "thinking",
                    "text_delta" => "text",
                    "input_json_delta" => "tool_use",
                    other => panic!("unexpected delta {other}"),
                };
                assert_eq!(open.get(&index).copied(), Some(expected));
            }
            "content_block_stop" => {
                assert!(open.remove(&index).is_some());
            }
            _ => {}
        }
    }
    assert_eq!(starts, vec!["thinking", "text", "tool_use", "text"]);
    assert!(open.is_empty());
    assert!(!wire.contains("signature_delta"));
}

#[allow(non_camel_case_types)]
struct Cancellation_signal(Arc<tokio::sync::Notify>);
impl Drop for Cancellation_signal {
    fn drop(&mut self) {
        self.0.notify_one();
    }
}

async fn cancellable_handler(State(notify): State<Arc<tokio::sync::Notify>>) -> Response {
    let body = async_stream::stream! {
        let _signal = Cancellation_signal(notify);
        let first = format!("data: {}\n\n",oai(json!({"role":"assistant","content":"fixture"}),None));
        yield Ok::<_, std::io::Error>(bytes::Bytes::from(first));
        futures_util::future::pending::<()>().await;
    };
    Response::builder()
        .header("content-type", "text/event-stream")
        .body(Body::from_stream(body))
        .unwrap()
}

#[tokio::test]
async fn client_disconnect_drops_the_owned_upstream_body() {
    let notify = Arc::new(tokio::sync::Notify::new());
    let app = axum::Router::new()
        .route("/v1/chat/completions", post(cancellable_handler))
        .with_state(notify.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let upstream = Test_server {
        base_url: format!("http://{address}"),
        abort: handle.abort_handle(),
        _shutdown: None,
    };
    let gateway = proxy(vec![provider("openai", &upstream, false)]).await;
    let response = client().post(format!("{}/v1/chat/completions",gateway.base_url)).json(&json!({"model":"alias","stream":true,"messages":[{"role":"user","content":"fixture"}]})).send().await.unwrap();
    let mut bytes = response.bytes_stream();
    assert!(bytes.next().await.unwrap().is_ok());
    drop(bytes);
    tokio::time::timeout(Duration::from_secs(2), notify.notified())
        .await
        .expect("upstream body must be dropped when the client cancels");
}

fn request_for_router() -> crate::proxy::canonical::ChatCompletionRequest {
    serde_json::from_value(
        json!({"model":"alias","stream":true,"messages":[{"role":"user","content":"fixture"}]}),
    )
    .unwrap()
}

fn router_for_provider(provider: Arc<dyn Provider>) -> Router {
    Router::new(
        [(provider.id().to_string(), provider.clone())]
            .into_iter()
            .collect(),
        [(
            "alias".into(),
            vec![RouteHop {
                provider: provider.id().into(),
                model: "model".into(),
            }],
        )]
        .into_iter()
        .collect(),
    )
}

#[tokio::test]
async fn repeated_late_errors_open_breaker_without_recording_winner() {
    use crate::proxy::circuit_breaker::CircuitState;
    let events = [
        oai(json!({"role":"assistant","content":"partial"}), None),
        json!({"error":{"type":"api_error","message":"fixture"}}),
    ];
    let (upstream, state) = fixture(200, Value::Null, sse(&events, false)).await;
    let provider: Arc<dyn Provider> = Arc::new(
        OpenAIProvider::new(
            "first".into(),
            format!("{}/v1", upstream.base_url),
            String::new(),
            vec!["model".into()],
            true,
            2_000,
            CircuitBreakerConfig {
                failure_threshold: 2,
                cooldown_ms: 60_000,
                half_open_retries: 1,
                success_threshold: 1,
            },
            Default::default(),
        )
        .unwrap(),
    );
    let router = router_for_provider(provider.clone());
    for failures in [1, 2] {
        let stream = router
            .stream_for("chat", "copilot", &request_for_router())
            .await
            .unwrap();
        assert!(router.last_used.lock().unwrap().is_empty());
        let chunks: Vec<_> = stream.collect().await;
        assert!(chunks.last().unwrap().extra.contains_key("error"));
        assert_eq!(provider.breaker().snapshot().failures, failures);
        assert!(router.last_used.lock().unwrap().is_empty());
    }
    assert_eq!(provider.breaker().snapshot().state, CircuitState::Open);
    assert!(router
        .stream_for("chat", "copilot", &request_for_router())
        .await
        .is_err());
    assert_eq!(state.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn clean_stream_records_success_and_winner_only_after_drain() {
    let events = [
        oai(json!({"role":"assistant","content":"fixture"}), None),
        oai(json!({}), Some("stop")),
    ];
    let (upstream, _) = fixture(200, Value::Null, sse(&events, true)).await;
    let provider = provider("first", &upstream, false);
    provider.breaker().record_failure();
    let router = router_for_provider(provider.clone());
    let mut chunks = router
        .stream_for("chat", "copilot", &request_for_router())
        .await
        .unwrap();
    assert!(router.last_used.lock().unwrap().is_empty());
    assert_eq!(provider.breaker().snapshot().failures, 1);
    assert!(chunks.next().await.is_some());
    assert_eq!(provider.breaker().snapshot().failures, 1);
    let rest: Vec<_> = chunks.collect().await;
    assert!(!rest.iter().any(|chunk| chunk.extra.contains_key("error")));
    assert_eq!(provider.breaker().snapshot().failures, 0);
    assert_eq!(
        router
            .last_used
            .lock()
            .unwrap()
            .get("copilot")
            .map(String::as_str),
        Some("first")
    );
}

#[tokio::test]
async fn cancelled_half_open_stream_releases_its_probe_without_health_change() {
    use crate::proxy::circuit_breaker::CircuitState;
    let notify = Arc::new(tokio::sync::Notify::new());
    let app = axum::Router::new()
        .route("/v1/chat/completions", post(cancellable_handler))
        .with_state(notify);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let upstream = Test_server {
        base_url: format!("http://{address}"),
        abort: handle.abort_handle(),
        _shutdown: None,
    };
    let provider: Arc<dyn Provider> = Arc::new(
        OpenAIProvider::new(
            "probe".into(),
            format!("{}/v1", upstream.base_url),
            String::new(),
            vec!["model".into()],
            true,
            2_000,
            CircuitBreakerConfig {
                failure_threshold: 1,
                cooldown_ms: 0,
                half_open_retries: 1,
                success_threshold: 1,
            },
            Default::default(),
        )
        .unwrap(),
    );
    provider.breaker().record_failure();
    let router = router_for_provider(provider.clone());
    let mut first = router
        .stream_for("chat", "copilot", &request_for_router())
        .await
        .unwrap();
    assert_eq!(provider.breaker().snapshot().state, CircuitState::HalfOpen);
    assert!(router
        .stream_for("chat", "copilot", &request_for_router())
        .await
        .is_err());
    assert!(first.next().await.is_some());
    drop(first);
    let after_cancel = provider.breaker().snapshot();
    assert_eq!(after_cancel.state, CircuitState::HalfOpen);
    assert_eq!(after_cancel.failures, 1);
    assert_eq!(after_cancel.successes, 0);
    assert!(router.last_used.lock().unwrap().is_empty());
    let next = router
        .stream_for("chat", "copilot", &request_for_router())
        .await
        .unwrap();
    drop(next);
    assert!(provider.breaker().allow());
}

#[tokio::test]
async fn native_message_stop_drains_success_accounting() {
    let events = [
        json!({"type":"message_start","message":{"id":"native","model":"model","role":"assistant","content":[],"usage":{"input_tokens":1,"output_tokens":0}}}),
        json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}),
        json!({"type":"message_stop"}),
    ];
    let (upstream, _) = fixture(200, Value::Null, sse(&events, false)).await;
    let provider = provider("native", &upstream, true);
    provider.breaker().record_failure();
    let router = router_for_provider(provider.clone());
    let stream = router
        .stream_for("claude", "claude_code", &request_for_router())
        .await
        .unwrap();
    assert!(router.last_used.lock().unwrap().is_empty());
    let frames: Vec<_> =
        crate::proxy::converter::oai_chunks_to_anthropic_stream(stream, "alias".into())
            .collect()
            .await;
    assert_eq!(
        frames
            .iter()
            .filter(|frame| frame.contains("event: message_stop"))
            .count(),
        1
    );
    assert_eq!(provider.breaker().snapshot().failures, 0);
    assert_eq!(
        router
            .last_used
            .lock()
            .unwrap()
            .get("claude_code")
            .map(String::as_str),
        Some("native")
    );
}

/// `/v1/responses` chain-of-thought injection: when the toggle is on, the
/// built-in prompt must reach the upstream as the FIRST system message
/// (ahead of the client's `instructions`), and must NOT be echoed back to
/// the client in the response envelope.
#[tokio::test]
async fn responses_cot_injection_reaches_upstream_when_enabled() {
    let (upstream, state) = fixture(
        200,
        json!({"id":"fixture","object":"chat.completion","created":1,"model":"model",
               "choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],
               "usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}),
        String::new(),
    )
    .await;
    let gateway = proxy_with_cot(vec![provider("openai", &upstream, false)], true).await;
    let response = client()
        .post(format!("{}/v1/responses", gateway.base_url))
        .json(&json!({"model":"alias","instructions":"be brief","input":"hello","stream":false}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .text()
        .await
        .unwrap();
    let captured = state.requests.lock().unwrap()[0].clone();
    let messages = captured["messages"].as_array().unwrap();
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(
        messages[0]["content"],
        json!(crate::proxy::cot::RESPONSES_COT_SYSTEM_PROMPT.to_string())
    );
    assert_eq!(messages[1]["role"], "system");
    assert_eq!(messages[1]["content"], "be brief");
    assert_eq!(messages[2]["role"], "user");
    // No echo: the client-visible envelope never carries the prompt.
    assert!(!response.contains("step by step"));
}

/// With the toggle off, `/v1/responses` messages must be unchanged (guards
/// against accidental always-on or double injection).
#[tokio::test]
async fn responses_cot_disabled_leaves_messages_untouched() {
    let (upstream, state) = fixture(200, Value::Null, String::new()).await;
    let gateway = proxy(vec![provider("openai", &upstream, false)]).await;
    client()
        .post(format!("{}/v1/responses", gateway.base_url))
        .json(&json!({"model":"alias","instructions":"be brief","input":"hello","stream":true}))
        .send()
        .await
        .unwrap();
    let messages = state.requests.lock().unwrap()[0]["messages"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(messages[0]["content"], "be brief");
    assert_eq!(messages[1]["role"], "user");
}

/// The injected prompt must stay at position 0 across a Codex tool
/// round-trip (user message + function_call + function_call_output), with
/// the tool shapes after it untouched.
#[tokio::test]
async fn responses_cot_survives_tool_round_trip() {
    let tool_call_chunk = json!({"id":"fixture","object":"chat.completion.chunk","created":1,
        "model":"model","choices":[{"index":0,"delta":{"tool_calls":[
            {"index":0,"id":"call_1","type":"function","function":{"name":"shell","arguments":""}}
        ]},"finish_reason":"tool_calls"}]});
    let wire = sse(&[tool_call_chunk], true);
    let (upstream, state) = fixture(200, Value::Null, wire).await;
    let gateway = proxy_with_cot(vec![provider("openai", &upstream, false)], true).await;
    client()
        .post(format!("{}/v1/responses", gateway.base_url))
        .json(&json!({
            "model":"alias",
            "input":[
                {"type":"message","role":"user","content":"list files"},
                {"type":"function_call","call_id":"call_1","name":"shell","arguments":"{\"cmd\":\"ls\"}"},
                {"type":"function_call_output","call_id":"call_1","output":"a.txt"},
            ],
            "stream":true
        }))
        .send()
        .await
        .unwrap();
    let messages = state.requests.lock().unwrap()[0]["messages"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(
        messages[0]["content"],
        json!(crate::proxy::cot::RESPONSES_COT_SYSTEM_PROMPT.to_string())
    );
    assert_eq!(messages[1]["role"], "user");
    assert_eq!(messages[2]["role"], "assistant");
    assert!(messages[2]["tool_calls"].is_array());
    assert_eq!(messages[3]["role"], "tool");
    assert_eq!(messages[3]["tool_call_id"], "call_1");
}

/// On the native Anthropic hop the injected prompt and the client's
/// `instructions` must join into ONE `system` string, prompt first — the
/// Responses surface never sets `source_anthropic_request`, so the verbatim
/// passthrough cannot swallow the injection.
#[tokio::test]
async fn responses_cot_joins_anthropic_system_when_native() {
    let (upstream, state) = fixture(200, Value::Null, String::new()).await;
    let gateway = proxy_with_cot(vec![provider("native", &upstream, true)], true).await;
    client()
        .post(format!("{}/v1/responses", gateway.base_url))
        .json(&json!({"model":"alias","instructions":"be brief","input":"hello","stream":true}))
        .send()
        .await
        .unwrap();
    let captured = state.requests.lock().unwrap()[0].clone();
    let system = captured["system"].as_str().unwrap();
    assert!(system.starts_with(crate::proxy::cot::RESPONSES_COT_SYSTEM_PROMPT));
    assert!(system.ends_with("\n\nbe brief"));
}
