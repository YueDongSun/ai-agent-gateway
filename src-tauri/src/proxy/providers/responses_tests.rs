//! Focused local-fixture tests for the native Responses upstream provider.
//! All upstreams and credentials are in-process fixtures — no real network.
use super::responses::ResponsesProvider;
use super::Provider;
use crate::database::dao::proxy_providers::CircuitBreakerConfig;
use crate::proxy::canonical::{ChatCompletionRequest, ChatMessage, OAIStreamChunk, ProviderError};
use crate::proxy::converter::{
    canonical_to_responses_request_value, responses_response_to_chat, responses_stream_to_sse,
};
use crate::proxy::responses::ResponsesRequest;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

fn breaker() -> CircuitBreakerConfig {
    CircuitBreakerConfig::default()
}

fn chat_request(messages: Vec<ChatMessage>, stream: bool) -> ChatCompletionRequest {
    ChatCompletionRequest {
        model: "alias".into(),
        messages,
        stream,
        temperature: Some(0.2),
        max_tokens: Some(64),
        tools: Some(vec![json!({
            "type": "function",
            "function": {"name": "shell", "parameters": {"type": "object", "properties": {}}},
        })]),
        tool_choice: Some(json!("auto")),
        extra: Default::default(),
        source_anthropic_request: None,
        source_gemini_request: None,
        source_responses_request: None,
        request_id: Some("req_test".into()),
        client_credential: None,
    }
}

fn user_message(text: &str) -> ChatMessage {
    ChatMessage {
        role: "user".into(),
        content: json!(text),
        tool_calls: None,
        tool_call_id: None,
        extra: Default::default(),
    }
}

fn native_responses_request() -> ChatCompletionRequest {
    let source = ResponsesRequest {
        model: "alias".into(),
        input: json!("hello"),
        instructions: Some("be brief".into()),
        stream: false,
        temperature: None,
        max_output_tokens: None,
        tools: None,
        metadata: None,
        previous_response_id: Some("resp_previous".into()),
        extra: [("tool_choice".to_string(), json!("auto"))]
            .into_iter()
            .collect(),
    };
    // Use a chat request whose messages match the sidecar's own chat
    // projection exactly (instructions -> system + input -> user). The
    // provider's sidecar path then treats the sidecar as the source of truth
    // and overlays only the routed model and stream flag.
    let mut request = chat_request(
        vec![
            ChatMessage {
                role: "system".into(),
                content: json!("be brief"),
                tool_calls: None,
                tool_call_id: None,
                extra: Default::default(),
            },
            user_message("hello"),
        ],
        false,
    );
    request.source_responses_request = Some(source);
    request
}

#[derive(Clone, Default)]
struct Capture {
    bodies: Arc<Mutex<Vec<Value>>>,
}

async fn capture_handler(State(state): State<Capture>, Json(body): Json<Value>) -> Response {
    state.bodies.lock().unwrap().push(body);
    Json(json!({
        "id": "resp_fixture",
        "object": "response",
        "created_at": 1_700_000_000,
        "status": "completed",
        "error": null,
        "incomplete_details": null,
        "model": "gpt-fixture",
        "output": [{
            "type": "message",
            "id": "msg_1",
            "role": "assistant",
            "status": "completed",
            "content": [{"type": "output_text", "text": "hi"}],
        }],
        "usage": {"input_tokens": 3, "output_tokens": 2, "total_tokens": 5},
    }))
    .into_response()
}

async fn capture_server() -> (String, Capture, tokio::task::AbortHandle) {
    let state = Capture::default();
    let app = Router::new()
        .route("/v1/responses", post(capture_handler))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{address}"), state, handle.abort_handle())
}

fn responses_provider(base_url: &str) -> ResponsesProvider {
    ResponsesProvider::new(
        "responses-test".into(),
        base_url.into(),
        "sk-fixture".into(),
        vec!["gpt-fixture".into()],
        true,
        2_000,
        breaker(),
        Default::default(),
    )
    .unwrap()
}

#[tokio::test]
async fn chat_originating_request_maps_fields_and_headers() {
    let (base_url, state, abort) = capture_server().await;
    let provider = responses_provider(&base_url);
    let request = chat_request(vec![user_message("hello")], false);
    let response = provider
        .complete(&request, "gpt-fixture[1M]")
        .await
        .expect("fixture upstream answers");
    abort.abort();

    let bodies = state.bodies.lock().unwrap();
    let upstream = &bodies[0];
    assert_eq!(upstream["model"], "gpt-fixture", "[1M] marker stripped");
    assert_eq!(upstream["stream"], false);
    assert_eq!(upstream["temperature"], 0.2);
    assert_eq!(upstream["max_output_tokens"], 64);
    assert_eq!(
        upstream["tool_choice"], "auto",
        "chat tool_choice string passes through"
    );
    let input = upstream["input"].as_array().unwrap();
    assert_eq!(input.len(), 1);
    assert_eq!(input[0]["type"], "message");
    assert_eq!(input[0]["role"], "user");
    // A single user message's plain-text content becomes a bare string on
    // the wire (the documented Responses `input` shorthand for one message).
    assert_eq!(input[0]["content"], "hello");
    let tools = upstream["tools"].as_array().unwrap();
    assert_eq!(tools[0]["type"], "function");
    assert_eq!(tools[0]["name"], "shell");

    assert_eq!(response.model, "gpt-fixture");
    assert_eq!(response.choices[0].message.content.as_deref(), Some("hi"));
    let usage = response.usage.as_ref().expect("usage present");
    assert_eq!(usage.prompt_tokens, 3);
    assert_eq!(usage.completion_tokens, 2);
    let source = response
        .source_responses_response
        .as_ref()
        .expect("sidecar kept");
    assert_eq!(source["id"], "resp_fixture");
    assert_eq!(source["status"], "completed");
    assert_eq!(source.get("error"), Some(&Value::Null));
    // The source sidecar is never serialized on a Chat wire.
    assert!(serde_json::to_value(&response)
        .unwrap()
        .get("source_responses_response")
        .is_none());
}

#[tokio::test]
async fn native_request_sidecar_relays_previous_response_id_and_opaque_fields() {
    let (base_url, state, abort) = capture_server().await;
    let provider = responses_provider(&base_url);
    let request = native_responses_request();
    provider
        .complete(&request, "gpt-fixture")
        .await
        .expect("fixture upstream answers");
    abort.abort();

    let bodies = state.bodies.lock().unwrap();
    let upstream = &bodies[0];
    assert_eq!(upstream["model"], "gpt-fixture");
    assert_eq!(upstream["stream"], false);
    assert_eq!(upstream["instructions"], "be brief");
    assert_eq!(
        upstream["previous_response_id"], "resp_previous",
        "native state must not be dropped on a native hop"
    );
    assert_eq!(upstream["input"], json!("hello"));
}

#[test]
fn lossy_direction_refuses_unsupported_fields_before_any_request() {
    let mut request = chat_request(vec![user_message("hello")], false);
    request.extra.insert("logprobs".into(), json!(true));
    let error = canonical_to_responses_request_value(&request, "gpt-fixture", false)
        .expect_err("chat-only fields must fail closed");
    assert_eq!(error.code, "responses_feature_unsupported");
    assert_eq!(error.status, Some(400));
    assert!(!error.retriable);
    assert_eq!(error.stage, "request_conversion");
}

#[test]
fn responses_response_failure_is_an_explicit_error() {
    let failed = json!({
        "id": "resp_x",
        "object": "response",
        "created_at": 1,
        "status": "failed",
        "error": {"code": 429, "message": "fixture"},
    });
    let error: ProviderError = responses_response_to_chat(&failed, "m", true).unwrap_err();
    assert_eq!(error.code, "upstream_http_error");
    assert_eq!(error.status, Some(429));
    assert!(error.retriable, "429-classified failure is retriable");
    let failed_without_details =
        responses_response_to_chat(&json!({"status":"failed", "error":null}), "m", true)
            .expect_err("null error details cannot make failed status succeed");
    assert_eq!(failed_without_details.status, Some(502));
    let cancelled =
        responses_response_to_chat(&json!({"status":"cancelled", "error":null}), "m", true)
            .expect_err("cancelled status must fail explicitly");
    assert_eq!(cancelled.status, Some(499));
    assert!(!cancelled.retriable);

    let incomplete = json!({
        "id": "resp_y",
        "object": "response",
        "created_at": 1,
        "status": "incomplete",
        "error": null,
        "incomplete_details": {"reason": "max_output_tokens"},
        "output": [],
        "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2},
    });
    let parsed = responses_response_to_chat(&incomplete, "m", true).expect("incomplete parses");
    assert_eq!(parsed.choices[0].finish_reason.as_deref(), Some("length"));

    let encrypted = json!({
        "id": "resp_z",
        "object": "response",
        "created_at": 1,
        "status": "completed",
        "output": [
            {"type": "reasoning", "id": "rs_1", "encrypted_content": "opaque"},
            {"type": "message", "role": "assistant",
             "content": [{"type": "output_text", "text": "visible"}]},
        ],
    });
    let blocked = responses_response_to_chat(&encrypted, "m", false)
        .expect_err("encrypted reasoning must not cross to non-native surfaces silently");
    assert_eq!(blocked.code, "upstream_decode_error");
    assert!(!blocked.retriable);
    let allowed = responses_response_to_chat(&encrypted, "m", true)
        .expect("native opaque surfaces keep the envelope");
    assert!(allowed.source_responses_response.is_some());
}

fn frame(event: &str, payload: Value) -> String {
    format!(
        "event: {event}\ndata: {}\n\n",
        serde_json::to_string(&payload).unwrap()
    )
}

async fn fixture_stream(wire: String) -> Vec<OAIStreamChunk> {
    let app = Router::new().route(
        "/v1/responses",
        post(move || {
            let wire = wire.clone();
            async move {
                Response::builder()
                    .header("content-type", "text/event-stream")
                    .body(axum::body::Body::from(wire))
                    .unwrap()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let provider = responses_provider(&format!("http://{address}"));
    let stream = provider
        .stream(&chat_request(vec![user_message("hi")], true), "gpt-fixture")
        .await
        .expect("fixture stream opens");
    let chunks = tokio::time::timeout(std::time::Duration::from_secs(5), stream.collect())
        .await
        .expect("fixture stream terminates");
    server.abort();
    chunks
}

fn streamed_content(chunks: &[OAIStreamChunk]) -> String {
    chunks
        .iter()
        .flat_map(|chunk| &chunk.choices)
        .filter_map(|choice| choice.delta.content.as_deref())
        .collect()
}

async fn native_wire(chunks: Vec<OAIStreamChunk>) -> String {
    responses_stream_to_sse(Box::pin(futures_util::stream::iter(chunks)), "gpt-fixture")
        .collect::<Vec<_>>()
        .await
        .concat()
}

#[tokio::test]
async fn stream_text_done_snapshots_do_not_repeat_deltas() {
    let item = json!({
        "type":"message", "id":"msg_1", "role":"assistant", "status":"completed",
        "content":[{"type":"output_text", "text":"Hello 世界😀"}, {"type":"output_text", "text":"!"}],
    });
    let wire = [
        frame("response.created", json!({"type":"response.created", "response":{"id":"resp_text", "status":"in_progress", "error":null, "usage":null}})),
        frame("response.output_item.added", json!({"type":"response.output_item.added", "output_index":0, "item":{"type":"message", "id":"msg_1", "role":"assistant", "content":[]}})),
        frame("response.output_text.delta", json!({"type":"response.output_text.delta", "item_id":"msg_1", "output_index":0, "content_index":0, "delta":"Hello "})),
        frame("response.output_text.delta", json!({"type":"response.output_text.delta", "item_id":"msg_1", "output_index":0, "content_index":0, "delta":"世界😀"})),
        frame("response.output_text.done", json!({"type":"response.output_text.done", "item_id":"msg_1", "output_index":0, "content_index":0, "text":"Hello 世界😀"})),
        frame("response.content_part.done", json!({"type":"response.content_part.done", "item_id":"msg_1", "output_index":0, "content_index":0, "part":{"type":"output_text", "text":"Hello 世界😀"}})),
        frame("response.output_item.done", json!({"type":"response.output_item.done", "output_index":0, "item":item.clone()})),
        frame("response.completed", json!({"type":"response.completed", "response":{"id":"resp_text", "status":"completed", "error":null, "output":[item], "usage":{"input_tokens":5, "output_tokens":3, "total_tokens":8, "input_tokens_details":{"cached_tokens":2}}}})),
    ].concat();
    let chunks = fixture_stream(wire.clone()).await;
    assert_eq!(streamed_content(&chunks), "Hello 世界😀!");
    assert!(chunks
        .iter()
        .all(|chunk| !chunk.extra.contains_key("error")));
    assert!(chunks
        .iter()
        .all(|chunk| chunk.id == "resp_text" && chunk.object == "chat.completion.chunk"));
    assert!(chunks[..chunks.len() - 1]
        .iter()
        .all(|chunk| chunk.choices[0].finish_reason.is_none()));
    let final_chunk = chunks.last().unwrap();
    assert_eq!(
        final_chunk.choices[0].finish_reason.as_deref(),
        Some("stop")
    );
    assert_eq!(final_chunk.extra["usage"]["prompt_tokens"], 5);
    assert_eq!(final_chunk.extra["usage"]["completion_tokens"], 3);
    assert_eq!(
        final_chunk.extra["usage"]["prompt_tokens_details"]["cached_tokens"],
        2
    );
    assert_eq!(
        native_wire(chunks).await,
        format!("{wire}{}", crate::proxy::responses::STREAM_DONE),
        "native frames stay lossless and the surface terminator is appended"
    );
}

#[tokio::test]
async fn stream_parallel_tools_keep_indices_and_incremental_arguments() {
    let first = json!({"type":"function_call", "id":"fc_a", "call_id":"call_a", "name":"first", "arguments":"{\"a\":1}"});
    let second = json!({"type":"function_call", "id":"fc_b", "call_id":"call_b", "name":"second", "arguments":"{\"b\":2}"});
    let reasoning = json!({"type":"reasoning", "id":"rs_1", "summary":[{"type":"summary_text", "text":"Plan"}]});
    let wire = [
        frame("response.reasoning_summary_text.delta", json!({"type":"response.reasoning_summary_text.delta", "item_id":"rs_1", "output_index":0, "summary_index":0, "delta":"Plan"})),
        frame("response.output_item.added", json!({"type":"response.output_item.added", "output_index":1, "item":{"type":"function_call", "id":"fc_a", "call_id":"call_a", "name":"first", "arguments":""}})),
        frame("response.output_item.added", json!({"type":"response.output_item.added", "output_index":2, "item":{"type":"function_call", "id":"fc_b", "call_id":"call_b", "name":"second", "arguments":""}})),
        frame("response.function_call_arguments.delta", json!({"type":"response.function_call_arguments.delta", "item_id":"fc_a", "output_index":1, "delta":"{\"a\":"})),
        frame("response.function_call_arguments.delta", json!({"type":"response.function_call_arguments.delta", "item_id":"fc_b", "output_index":2, "delta":"{\"b\":"})),
        frame("response.function_call_arguments.delta", json!({"type":"response.function_call_arguments.delta", "item_id":"fc_b", "output_index":2, "delta":"2}"})),
        frame("response.function_call_arguments.delta", json!({"type":"response.function_call_arguments.delta", "item_id":"fc_a", "output_index":1, "delta":"1}"})),
        frame("response.function_call_arguments.done", json!({"type":"response.function_call_arguments.done", "item_id":"fc_a", "output_index":1, "arguments":"{\"a\":1}"})),
        frame("response.function_call_arguments.done", json!({"type":"response.function_call_arguments.done", "item_id":"fc_b", "output_index":2, "arguments":"{\"b\":2}"})),
        frame("response.output_item.done", json!({"type":"response.output_item.done", "output_index":1, "item":first.clone()})),
        frame("response.output_item.done", json!({"type":"response.output_item.done", "output_index":2, "item":second.clone()})),
        frame("response.completed", json!({"type":"response.completed", "response":{"id":"resp_tools", "status":"completed", "error":null, "output":[reasoning, first, second]}})),
    ].concat();
    let chunks = fixture_stream(wire.clone()).await;
    assert!(chunks
        .iter()
        .all(|chunk| !chunk.extra.contains_key("error")));
    assert_eq!(
        streamed_content(&chunks),
        "",
        "reasoning is not visible answer text"
    );
    let reasoning: String = chunks
        .iter()
        .flat_map(|chunk| &chunk.choices)
        .filter_map(|choice| choice.delta.reasoning.as_deref())
        .collect();
    assert_eq!(reasoning, "Plan");
    let mut arguments = [String::new(), String::new()];
    let mut ids = [Vec::new(), Vec::new()];
    let mut names = [Vec::new(), Vec::new()];
    for chunk in &chunks {
        for call in chunk
            .choices
            .iter()
            .flat_map(|choice| choice.delta.tool_calls.iter().flatten())
        {
            let index = call["index"].as_u64().unwrap() as usize;
            assert!(index < 2);
            if let Some(id) = call["id"].as_str() {
                ids[index].push(id.to_string());
            }
            if let Some(name) = call["function"]["name"].as_str() {
                names[index].push(name.to_string());
            }
            if let Some(fragment) = call["function"]["arguments"].as_str() {
                assert_eq!(
                    chunk
                        .source_responses_event
                        .as_ref()
                        .unwrap()
                        .event
                        .as_deref(),
                    Some("response.function_call_arguments.delta")
                );
                arguments[index].push_str(fragment);
            }
        }
    }
    assert_eq!(ids, [vec!["call_a"], vec!["call_b"]]);
    assert_eq!(names, [vec!["first"], vec!["second"]]);
    assert_eq!(arguments, ["{\"a\":1}", "{\"b\":2}"]);
    assert_eq!(
        chunks.last().unwrap().choices[0].finish_reason.as_deref(),
        Some("tool_calls")
    );
    assert_eq!(
        native_wire(chunks).await,
        format!("{wire}{}", crate::proxy::responses::STREAM_DONE)
    );
}

#[tokio::test]
async fn stream_argument_deltas_without_tool_identity_fail_closed() {
    let wire = [
        frame("response.function_call_arguments.delta", json!({"type":"response.function_call_arguments.delta", "item_id":"fc_missing", "output_index":0, "delta":"{}"})),
        frame("response.completed", json!({"type":"response.completed", "response":{"status":"completed", "error":null}})),
    ].concat();
    let chunks = fixture_stream(wire).await;
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].extra["error"]["status"], 502);
    assert!(chunks[0].choices.is_empty());
}

#[tokio::test]
async fn stream_done_only_items_and_terminal_snapshot_project_once() {
    let message = json!({"type":"message", "id":"msg", "role":"assistant", "content":[{"type":"output_text", "text":"Answer"}]});
    let first = json!({"type":"function_call", "id":"fc_1", "call_id":"call_1", "name":"first", "arguments":"{}"});
    let second = json!({"type":"function_call", "id":"fc_2", "call_id":"call_2", "name":"second", "arguments":"[]"});
    let wire = [
        frame("response.output_item.done", json!({"type":"response.output_item.done", "output_index":0, "item":message.clone()})),
        frame("response.output_item.done", json!({"type":"response.output_item.done", "output_index":1, "item":first.clone()})),
        frame("response.completed", json!({"type":"response.completed", "response":{"id":"resp_done", "status":"completed", "error":null, "output":[message, first, second]}})),
    ].concat();
    let chunks = fixture_stream(wire).await;
    assert!(chunks
        .iter()
        .all(|chunk| !chunk.extra.contains_key("error")));
    assert_eq!(streamed_content(&chunks), "Answer");
    let calls: Vec<_> = chunks
        .iter()
        .flat_map(|chunk| &chunk.choices)
        .flat_map(|choice| choice.delta.tool_calls.iter().flatten())
        .collect();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0]["index"], 0);
    assert_eq!(calls[0]["function"]["arguments"], "{}");
    assert_eq!(calls[1]["index"], 1);
    assert_eq!(calls[1]["id"], "call_2");
    assert_eq!(calls[1]["function"]["arguments"], "[]");
}

#[tokio::test]
async fn stream_failed_cancelled_and_error_events_never_become_success() {
    for (event_type, response, expected_status) in [
        (
            "response.failed",
            json!({"status":"failed", "error":{"code":429, "message":"fixture"}}),
            429,
        ),
        (
            "response.failed",
            json!({"status":"failed", "error":{"code":"server_error", "message":"fixture"}}),
            502,
        ),
        (
            "response.failed",
            json!({"status":"failed", "error":null}),
            502,
        ),
        (
            "response.cancelled",
            json!({"status":"cancelled", "error":null}),
            499,
        ),
        ("error", json!({"code":401, "message":"fixture"}), 401),
    ] {
        let failed = if event_type == "error" {
            json!({"type":"error", "error":response})
        } else {
            json!({"type":event_type, "response":response})
        };
        let prefix = frame(
            "response.output_text.delta",
            json!({"type":"response.output_text.delta", "output_index":0, "item_id":"msg", "content_index":0, "delta":"partial"}),
        );
        let terminal = frame(event_type, failed);
        let wire = [
            prefix.clone(), terminal.clone(),
            frame("response.output_text.delta", json!({"type":"response.output_text.delta", "output_index":0, "item_id":"msg", "content_index":0, "delta":"must not stitch"})),
            frame("response.completed", json!({"type":"response.completed", "response":{"status":"completed", "error":null}})),
        ].concat();
        let chunks = fixture_stream(wire).await;
        assert_eq!(chunks.len(), 2, "{event_type}: stop reading after failure");
        assert_eq!(streamed_content(&chunks), "partial");
        assert_eq!(chunks[1].extra["error"]["status"], expected_status);
        assert!(chunks
            .iter()
            .flat_map(|chunk| &chunk.choices)
            .all(|choice| choice.finish_reason.is_none()));
        assert_eq!(native_wire(chunks).await, format!("{prefix}{terminal}"));
    }
}

#[tokio::test]
async fn stream_incomplete_preserves_length_and_content_filter_classification() {
    for (reason, expected) in [
        ("max_output_tokens", "length"),
        ("content_filter", "content_filter"),
    ] {
        let wire = [
            frame("response.output_text.delta", json!({"type":"response.output_text.delta", "output_index":0, "item_id":"msg", "content_index":0, "delta":"partial"})),
            frame("response.incomplete", json!({"type":"response.incomplete", "response":{"id":"resp_incomplete", "status":"incomplete", "error":null, "incomplete_details":{"reason":reason}, "output":[{"id":"msg", "type":"message", "content":[{"type":"output_text", "text":"partial"}]}], "usage":{"input_tokens":2, "output_tokens":3, "total_tokens":5}}})),
        ].concat();
        let chunks = fixture_stream(wire.clone()).await;
        assert_eq!(streamed_content(&chunks), "partial");
        assert!(chunks
            .iter()
            .all(|chunk| !chunk.extra.contains_key("error")));
        assert_eq!(
            chunks.last().unwrap().choices[0].finish_reason.as_deref(),
            Some(expected)
        );
        assert_eq!(chunks.last().unwrap().extra["usage"]["total_tokens"], 5);
        // `response.incomplete` is a success-class terminal: the surface
        // terminates the stream with `[DONE]` after it.
        assert_eq!(
            native_wire(chunks).await,
            format!("{wire}{}", crate::proxy::responses::STREAM_DONE)
        );
    }
}

#[tokio::test]
async fn empty_stream_is_a_failure() {
    let chunks = fixture_stream(String::new()).await;
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].extra["error"]["status"], 502);
    assert!(chunks[0].choices.is_empty());
}

#[tokio::test]
async fn stream_replays_named_events_and_requires_terminal_frame() {
    let wire = [
        frame("response.created", json!({"type":"response.created","response":{"id":"resp_1","object":"response"}})),
        frame("response.output_text.delta", json!({"type":"response.output_text.delta","item_id":"msg_1","output_index":0,"content_index":0,"delta":"推理😀"})),
        frame("response.completed", json!({"type":"response.completed","response":{"id":"resp_1","status":"completed","usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}})),
        // Relay artifacts must not be mistaken for protocol frames.
        "data: [DONE]\n\n".to_string(),
    ]
    .concat();

    let app = Router::new()
        .route(
            "/v1/responses",
            post(move |Json(_): Json<Value>| {
                let body = wire.clone();
                async move {
                    Response::builder()
                        .header("content-type", "text/event-stream")
                        .body(axum::body::Body::from(body))
                        .unwrap()
                }
            }),
        )
        .with_state(());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let provider = responses_provider(&format!("http://{address}"));
    let stream = provider
        .stream(&chat_request(vec![user_message("hi")], true), "gpt-fixture")
        .await
        .expect("stream opens");
    let chunks: Vec<OAIStreamChunk> = stream.collect().await;
    handle.abort();

    assert_eq!(chunks.len(), 3, "no error chunk, no [DONE] frame");
    assert_eq!(
        chunks[0]
            .source_responses_event
            .as_ref()
            .unwrap()
            .event
            .as_deref(),
        Some("response.created")
    );
    let delta = chunks[1].source_responses_event.as_ref().unwrap();
    assert_eq!(delta.event.as_deref(), Some("response.output_text.delta"));
    assert!(
        delta.data.contains("推理😀"),
        "UTF-8 payload survives the SSE decode"
    );
    assert!(
        chunks[1].choices[0].delta.content.as_deref() == Some("推理😀"),
        "cross-protocol text delta present: {:?}",
        chunks[1].choices[0].delta.content
    );
    let completed = chunks[2].source_responses_event.as_ref().unwrap();
    assert_eq!(completed.event.as_deref(), Some("response.completed"));
    assert!(completed.is_terminal());
    assert_eq!(chunks[2].choices[0].finish_reason.as_deref(), Some("stop"));
}

#[tokio::test]
async fn stream_without_terminal_frame_is_a_failure() {
    let app = Router::new()
        .route("/v1/responses", post(|| async {
            Response::builder()
                .header("content-type", "text/event-stream")
                .body(axum::body::Body::from(frame(
                    "response.output_text.delta",
                    json!({"type":"response.output_text.delta","item_id":"m","output_index":0,"content_index":0,"delta":"partial"}),
                )))
                .unwrap()
        }))
        .with_state(());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let provider = responses_provider(&format!("http://{address}"));
    let stream = provider
        .stream(&chat_request(vec![user_message("hi")], true), "gpt-fixture")
        .await
        .expect("stream opens");
    let chunks: Vec<OAIStreamChunk> = stream.collect().await;
    handle.abort();

    assert!(
        chunks.last().unwrap().extra.contains_key("error"),
        "transport EOF before a terminal frame must surface an error, got {chunks:?}"
    );
}

#[tokio::test]
async fn upstream_rejection_fails_before_streaming_with_status() {
    let app = Router::new()
        .route(
            "/v1/responses",
            post(|| async {
                (
                    axum::http::StatusCode::TOO_MANY_REQUESTS,
                    Json(json!({"error":{"message":"fixture"}})),
                )
                    .into_response()
            }),
        )
        .with_state(());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let provider = responses_provider(&format!("http://{address}"));
    let error = match provider
        .stream(&chat_request(vec![user_message("hi")], true), "gpt-fixture")
        .await
    {
        Err(error) => error,
        Ok(_) => panic!("429 must surface eagerly"),
    };
    handle.abort();
    assert_eq!(error.status, Some(429));
    assert!(error.retriable);
}

#[test]
fn sse_frame_terminal_detection_covers_failure_variants() {
    use crate::proxy::responses::ResponsesSseEvent;
    let terminal = |event: &str| {
        ResponsesSseEvent {
            event: Some(event.into()),
            id: None,
            retry: None,
            data: "{}".into(),
        }
        .is_terminal()
    };
    for event in [
        "response.completed",
        "response.incomplete",
        "response.failed",
        "response.cancelled",
    ] {
        assert!(terminal(event), "{event} must be terminal");
    }
    assert!(!terminal("response.output_text.delta"));
    assert!(!terminal("response.created"));
}

#[test]
fn multiline_data_reconstructs_the_joined_payload() {
    use crate::proxy::responses::ResponsesSseEvent;
    let frame = ResponsesSseEvent {
        event: Some("response.output_text.delta".into()),
        id: Some("opaque-id".into()),
        retry: None,
        data: "{\"a\":1,\n\"b\":\"推理😀\"}".into(),
    };
    let wire = frame.to_sse();
    assert_eq!(
        wire,
        "event: response.output_text.delta\nid: opaque-id\ndata: {\"a\":1,\ndata: \"b\":\"推理😀\"}\n\n"
    );
    assert!(frame.is_terminal() == false);
}

#[tokio::test]
async fn sse_event_stream_error_classification_and_limits() {
    use super::sse::SseEventStream;
    use bytes::Bytes;
    use futures_util::stream;

    // Invalid UTF-8 is a hard decode error, never a silent replacement.
    let invalid: Vec<Result<Bytes, std::io::Error>> =
        vec![Ok(Bytes::from_static(b"data: \xff\n\n"))];
    let events: Vec<_> =
        futures_util::StreamExt::collect::<Vec<_>>(SseEventStream::new(stream::iter(invalid)))
            .await;
    assert_eq!(
        events[0].as_ref().unwrap_err().kind(),
        std::io::ErrorKind::InvalidData
    );

    // A comment keepalive never dispatches an event.
    let keepalive: Vec<Result<Bytes, std::io::Error>> =
        vec![Ok(Bytes::from_static(b": ping\n\r\ndata: ok\n\r\n"))];
    let events: Vec<_> =
        futures_util::StreamExt::collect::<Vec<_>>(SseEventStream::new(stream::iter(keepalive)))
            .await
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].data, "ok");
}

#[allow(dead_code)]
fn assert_provider_object_safe(_: &dyn Provider) {}

#[test]
fn provider_is_object_safe() {
    let provider = responses_provider("http://127.0.0.1:1/v1");
    assert_provider_object_safe(&provider);
    assert_eq!(provider.models(), &["gpt-fixture".to_string()]);
    assert!(provider.enabled());
}
