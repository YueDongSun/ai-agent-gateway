//! Loopback provider-wire fixtures for metadata-only routing observations.

use super::canonical::{ChatCompletionRequest, OAIStreamChunk};
use super::providers::{
    anthropic::AnthropicProvider, gemini::GeminiProvider, openai::OpenAIProvider,
    responses::ResponsesProvider, Provider,
};
use super::routing_receipts::contract::{
    AssociationKind, BodyState, ModelSource, RoutingIdentity, RoutingReceipt, TerminalSignal,
    TransportState, ValueState, WireProtocol,
};
use super::routing_receipts::{
    observe_selector, scope, ProviderObservation, ReceiptRegistry, ReceiptSecret,
    RequestObservation,
};
use axum::{
    extract::State,
    http::{StatusCode, Uri},
    response::{IntoResponse, Response},
    Json, Router,
};
use futures_util::StreamExt;
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{sync::oneshot, task::JoinHandle};

const SELECTOR: &str = "requested-alias";
const ROUTED_MODEL: &str = "wire-model[1M]";
const WIRE_MODEL: &str = "wire-model";
const REPORTED_MODEL: &str = "raw-provider-model";
const ANSWER: &str = "synthetic protocol answer";
const DEADLINE: Duration = Duration::from_secs(3);

#[derive(Clone)]
struct Reply {
    status: StatusCode,
    content_type: &'static str,
    body: String,
}

impl Reply {
    fn json(value: Value) -> Self {
        Self {
            status: StatusCode::OK,
            content_type: "application/json",
            body: value.to_string(),
        }
    }

    fn sse(frames: &[Value]) -> Self {
        Self {
            status: StatusCode::OK,
            content_type: "text/event-stream",
            body: frames
                .iter()
                .map(|frame| format!("data: {frame}\n\n"))
                .collect(),
        }
    }
}

#[derive(Clone)]
struct CapturedRequest {
    uri: String,
    body: Value,
}

#[derive(Clone)]
struct Script {
    replies: Arc<Mutex<VecDeque<Reply>>>,
    requests: Arc<Mutex<Vec<CapturedRequest>>>,
}

struct UpstreamFixture {
    base_url: String,
    script: Script,
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<std::io::Result<()>>>,
}

impl UpstreamFixture {
    async fn start(replies: Vec<Reply>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let script = Script {
            replies: Arc::new(Mutex::new(replies.into())),
            requests: Default::default(),
        };
        let app = Router::new()
            .fallback(scripted_response)
            .with_state(script.clone());
        let (shutdown, stopped) = oneshot::channel();
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = stopped.await;
                })
                .await
        });
        Self {
            base_url,
            script,
            shutdown: Some(shutdown),
            task: Some(task),
        }
    }

    async fn finish(mut self) -> Vec<CapturedRequest> {
        let _ = self.shutdown.take().unwrap().send(());
        let mut task = self.task.take().unwrap();
        if let Ok(result) = tokio::time::timeout(DEADLINE, &mut task).await {
            result.unwrap().unwrap();
        } else {
            task.abort();
            let _ = task.await;
            panic!("protocol fixture shutdown exceeded its deadline");
        }
        assert!(
            self.script.replies.lock().unwrap().is_empty(),
            "unused scripted response"
        );
        let requests = self.script.requests.lock().unwrap().clone();
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

async fn scripted_response(
    State(script): State<Script>,
    uri: Uri,
    Json(body): Json<Value>,
) -> Response {
    script.requests.lock().unwrap().push(CapturedRequest {
        uri: uri.to_string(),
        body,
    });
    let reply = script.replies.lock().unwrap().pop_front();
    match reply {
        Some(reply) => (
            reply.status,
            [("content-type", reply.content_type)],
            reply.body,
        )
            .into_response(),
        None => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "unexpected fixture request",
        )
            .into_response(),
    }
}

fn provider(protocol: WireProtocol, base_url: &str) -> Box<dyn Provider> {
    macro_rules! build {
        ($provider:ty) => {
            Box::new(
                <$provider>::new(
                    "protocol_fixture".into(),
                    base_url.into(),
                    String::new(),
                    vec![WIRE_MODEL.into()],
                    true,
                    2_000,
                    Default::default(),
                    Default::default(),
                )
                .unwrap(),
            )
        };
    }
    match protocol {
        WireProtocol::OpenaiChat => build!(OpenAIProvider),
        WireProtocol::Anthropic => build!(AnthropicProvider),
        WireProtocol::OpenaiResponses => build!(ResponsesProvider),
        WireProtocol::Gemini => build!(GeminiProvider),
        WireProtocol::Unknown => panic!("fixture protocol must be explicit"),
    }
}

fn request(stream: bool) -> ChatCompletionRequest {
    let mut request: ChatCompletionRequest = serde_json::from_value(json!({
        "model": SELECTOR, "stream": stream, "max_tokens": 64,
        "messages": [{"role":"user","content":"synthetic protocol request"}]
    }))
    .unwrap();
    request.request_id = Some("req_protocol_fixture".into());
    request
}

async fn observation(provider: &dyn Provider) -> (RequestObservation, ProviderObservation) {
    let request = ReceiptRegistry::new()
        .unwrap()
        .reserve(
            "0123456789abcdef0123456789abcdef",
            &ReceiptSecret::fixture(7),
            "req_protocol_fixture",
            "chat",
        )
        .unwrap();
    scope(request.clone(), async {
        observe_selector(SELECTOR);
    })
    .await;
    let hop = request.hop(
        2,
        RoutingIdentity {
            provider_ref: Some("protocol_fixture".into()),
            supplier_ref: None,
            endpoint_ref: None,
            association: AssociationKind::ManualProvider,
        },
        provider.wire_protocol(),
    );
    (request, hop)
}

async fn chunks(
    provider: &dyn Provider,
    request: &ChatCompletionRequest,
    hop: ProviderObservation,
) -> Vec<OAIStreamChunk> {
    let stream = provider
        .stream_observed(request, ROUTED_MODEL, hop)
        .await
        .unwrap();
    tokio::time::timeout(DEADLINE, stream.collect())
        .await
        .expect("bounded fixture stream")
}

fn output(value: &impl Serialize) -> Value {
    let mut value = serde_json::to_value(value).unwrap();
    // Canonical converters generate these two values independently of observation.
    value.as_object_mut().unwrap().remove("id");
    value.as_object_mut().unwrap().remove("created");
    value
}

fn chunk_output(chunks: &[OAIStreamChunk]) -> Vec<Value> {
    chunks.iter().map(output).collect()
}

fn settle(
    request: &RequestObservation,
    hop: &ProviderObservation,
    streaming: bool,
) -> RoutingReceipt {
    hop.selected(streaming);
    request.finish(Some(200), BodyState::Exhausted);
    request.snapshot()
}

fn assert_model(receipt: &RoutingReceipt, reported: bool, source: ModelSource) {
    assert_eq!(receipt.requested_selector.as_deref(), Some(SELECTOR));
    assert_eq!(receipt.attempts.len(), 1);
    let attempt = &receipt.attempts[0];
    assert_eq!(attempt.wire_model.as_deref(), Some(WIRE_MODEL));
    assert_eq!(
        attempt.provider_model.as_deref(),
        reported.then_some(REPORTED_MODEL)
    );
    assert_eq!(
        attempt.provider_model_state,
        if reported {
            ValueState::Reported
        } else {
            ValueState::Missing
        }
    );
    assert_eq!(attempt.provider_model_source, reported.then_some(source));
    assert!(attempt.send_started);
    assert_eq!(attempt.http_status, Some(200));
}

fn assert_wire(requests: &[CapturedRequest], protocol: WireProtocol, stream: bool) {
    for request in requests {
        let path = match protocol {
            WireProtocol::OpenaiChat => "/v1/chat/completions",
            WireProtocol::Anthropic => "/v1/messages",
            WireProtocol::OpenaiResponses => "/v1/responses",
            WireProtocol::Gemini if stream => {
                "/v1beta/models/wire-model:streamGenerateContent?alt=sse"
            }
            WireProtocol::Gemini => "/v1beta/models/wire-model:generateContent",
            WireProtocol::Unknown => unreachable!(),
        };
        assert_eq!(request.uri, path);
        if protocol == WireProtocol::Gemini {
            assert!(request.body.get("model").is_none());
        } else {
            assert_eq!(request.body["model"], WIRE_MODEL);
            assert_eq!(request.body["stream"], stream);
        }
    }
}

fn response(protocol: WireProtocol, reported: bool) -> Value {
    let mut value = match protocol {
        WireProtocol::OpenaiResponses => {
            json!({"id":"resp_fixture","object":"response","created_at":1,
            "status":"completed","error":null,"output":[{"type":"message","id":"msg_fixture",
            "role":"assistant","status":"completed","content":[{"type":"output_text","text":ANSWER}]}],
            "usage":{"input_tokens":3,"output_tokens":1,"total_tokens":4}})
        }
        WireProtocol::Gemini => json!({"responseId":"gemini_fixture","candidates":[{"index":0,
            "content":{"role":"model","parts":[{"text":ANSWER}]},"finishReason":"STOP"}],
            "usageMetadata":{"promptTokenCount":3,"candidatesTokenCount":1,"totalTokenCount":4}}),
        WireProtocol::Anthropic => json!({"id":"msg_fixture","type":"message","role":"assistant",
            "content":[{"type":"text","text":ANSWER}],"stop_reason":"end_turn","stop_sequence":null,
            "usage":{"input_tokens":3,"output_tokens":1}}),
        _ => unreachable!(),
    };
    if reported {
        value[if protocol == WireProtocol::Gemini {
            "modelVersion"
        } else {
            "model"
        }] = json!(REPORTED_MODEL);
    }
    value
}

fn terminal_frame(protocol: WireProtocol, reported: bool) -> Value {
    match protocol {
        WireProtocol::OpenaiChat => {
            let mut frame = json!({"id":"chat_fixture","object":"chat.completion.chunk","created":1,
                "choices":[{"index":0,"delta":{"content":ANSWER},"finish_reason":"stop"}]});
            if reported {
                frame["model"] = json!(REPORTED_MODEL);
            }
            frame
        }
        WireProtocol::OpenaiResponses => {
            json!({"type":"response.completed","response":response(protocol, reported)})
        }
        WireProtocol::Gemini => response(protocol, reported),
        WireProtocol::Anthropic => json!({"type":"message_stop"}),
        WireProtocol::Unknown => unreachable!(),
    }
}

#[tokio::test]
async fn openai_sse_observation_preserves_output_and_raw_or_missing_model_claims() {
    for reported in [true, false] {
        let mut reply = Reply::sse(&[terminal_frame(WireProtocol::OpenaiChat, reported)]);
        reply.body.push_str("data: [DONE]\n\n");
        let fixture = UpstreamFixture::start(vec![reply.clone(), reply]).await;
        let provider = provider(WireProtocol::OpenaiChat, &fixture.base_url);
        let (request_observation, hop) = observation(provider.as_ref()).await;
        let observed = chunks(provider.as_ref(), &request(true), hop.clone()).await;
        let ordinary = chunks(
            provider.as_ref(),
            &request(true),
            ProviderObservation::default(),
        )
        .await;
        assert_eq!(chunk_output(&observed), chunk_output(&ordinary));
        let receipt = settle(&request_observation, &hop, true);
        assert_model(&receipt, reported, ModelSource::Model);
        let attempt = &receipt.attempts[0];
        if reported {
            assert_eq!(observed[0].model, REPORTED_MODEL);
            assert_eq!(
                attempt.terminal_signals,
                vec![TerminalSignal::OpenaiFinish, TerminalSignal::OpenaiDone]
            );
            assert_eq!(attempt.transport_state, TransportState::StreamEnded);
        } else {
            assert!(observed[0].extra.contains_key("error"));
            assert_eq!(attempt.transport_state, TransportState::Failed);
            assert_eq!(attempt.error_code.as_deref(), Some("upstream_stream_error"));
        }
        let requests = fixture.finish().await;
        assert_eq!(requests.len(), 2);
        assert_wire(&requests, WireProtocol::OpenaiChat, true);
    }
}

#[tokio::test]
async fn responses_and_gemini_observe_native_models_before_canonical_fallbacks() {
    for protocol in [WireProtocol::OpenaiResponses, WireProtocol::Gemini] {
        for stream in [false, true] {
            for reported in [false, true] {
                let reply = if stream {
                    Reply::sse(&[terminal_frame(protocol, reported)])
                } else {
                    Reply::json(response(protocol, reported))
                };
                let fixture = UpstreamFixture::start(vec![reply.clone(), reply]).await;
                let provider = provider(protocol, &fixture.base_url);
                let (request_observation, hop) = observation(provider.as_ref()).await;
                if stream {
                    let observed = chunks(provider.as_ref(), &request(true), hop.clone()).await;
                    let ordinary = chunks(
                        provider.as_ref(),
                        &request(true),
                        ProviderObservation::default(),
                    )
                    .await;
                    assert_eq!(chunk_output(&observed), chunk_output(&ordinary));
                    assert_eq!(observed[0].model, WIRE_MODEL);
                    assert!(observed
                        .iter()
                        .all(|chunk| !chunk.extra.contains_key("error")));
                } else {
                    let observed = provider
                        .complete_observed(&request(false), ROUTED_MODEL, hop.clone())
                        .await
                        .unwrap();
                    let ordinary = provider
                        .complete(&request(false), ROUTED_MODEL)
                        .await
                        .unwrap();
                    assert_eq!(output(&observed), output(&ordinary));
                    assert_eq!(
                        observed.model,
                        if reported {
                            REPORTED_MODEL
                        } else {
                            ROUTED_MODEL
                        }
                    );
                }
                let receipt = settle(&request_observation, &hop, stream);
                assert_model(
                    &receipt,
                    reported,
                    if protocol == WireProtocol::Gemini {
                        ModelSource::ModelVersion
                    } else {
                        ModelSource::ResponseModel
                    },
                );
                assert!(receipt.capture_complete);
                assert_eq!(
                    receipt.attempts[0].transport_state,
                    if stream {
                        TransportState::StreamEnded
                    } else {
                        TransportState::ResponseParsed
                    }
                );
                if stream {
                    assert_eq!(
                        receipt.attempts[0].terminal_signals,
                        vec![if protocol == WireProtocol::Gemini {
                            TerminalSignal::GeminiFinish
                        } else {
                            TerminalSignal::ResponsesCompleted
                        }]
                    );
                }
                let requests = fixture.finish().await;
                assert_eq!(requests.len(), 2);
                assert_wire(&requests, protocol, stream);
            }
        }
    }
}

#[tokio::test]
async fn eof_and_malformed_finish_reasons_never_invent_protocol_terminals() {
    let cases = [
        (
            WireProtocol::OpenaiChat,
            Reply::sse(&[]),
            TransportState::StreamEnded,
        ),
        (
            WireProtocol::Gemini,
            Reply::sse(&[]),
            TransportState::StreamEnded,
        ),
        (
            WireProtocol::OpenaiResponses,
            Reply::sse(&[]),
            TransportState::Failed,
        ),
        (
            WireProtocol::OpenaiChat,
            {
                let mut frame = terminal_frame(WireProtocol::OpenaiChat, true);
                frame["choices"][0]["finish_reason"] = json!("made_up_finish");
                Reply::sse(&[frame])
            },
            TransportState::StreamEnded,
        ),
        (
            WireProtocol::OpenaiChat,
            {
                let mut frame = terminal_frame(WireProtocol::OpenaiChat, true);
                frame["choices"][0]["finish_reason"] = json!({"stop":true});
                Reply::sse(&[frame])
            },
            TransportState::Failed,
        ),
        (
            WireProtocol::Gemini,
            {
                let mut frame = terminal_frame(WireProtocol::Gemini, true);
                frame["candidates"][0]["finishReason"] = json!("MADE_UP_FINISH");
                Reply::sse(&[frame])
            },
            TransportState::StreamEnded,
        ),
        (
            WireProtocol::Gemini,
            {
                let mut frame = terminal_frame(WireProtocol::Gemini, true);
                frame["candidates"][0]["finishReason"] = json!({"STOP":true});
                Reply::sse(&[frame])
            },
            TransportState::StreamEnded,
        ),
    ];
    for (protocol, reply, expected_state) in cases {
        let fixture = UpstreamFixture::start(vec![reply.clone(), reply]).await;
        let provider = provider(protocol, &fixture.base_url);
        let (request_observation, hop) = observation(provider.as_ref()).await;
        let observed = chunks(provider.as_ref(), &request(true), hop.clone()).await;
        let ordinary = chunks(
            provider.as_ref(),
            &request(true),
            ProviderObservation::default(),
        )
        .await;
        assert_eq!(chunk_output(&observed), chunk_output(&ordinary));
        let receipt = settle(&request_observation, &hop, true);
        assert_eq!(receipt.attempts[0].transport_state, expected_state);
        assert!(receipt.attempts[0].terminal_signals.is_empty());
        if expected_state == TransportState::Failed {
            assert_eq!(
                receipt.attempts[0].error_code.as_deref(),
                Some("upstream_stream_error")
            );
            assert!(observed
                .iter()
                .any(|chunk| chunk.extra.contains_key("error")));
        } else {
            assert!(!receipt.capture_complete);
        }
        assert_eq!(fixture.finish().await.len(), 2);
    }
}

#[tokio::test]
async fn unread_and_terminal_then_dropped_provider_streams_remain_abandoned() {
    for protocol in [
        WireProtocol::OpenaiChat,
        WireProtocol::Anthropic,
        WireProtocol::OpenaiResponses,
        WireProtocol::Gemini,
    ] {
        for read_terminal in [false, true] {
            let fixture =
                UpstreamFixture::start(vec![Reply::sse(&[terminal_frame(protocol, true)])]).await;
            let provider = provider(protocol, &fixture.base_url);
            let (request_observation, hop) = observation(provider.as_ref()).await;
            let mut stream = provider
                .stream_observed(&request(true), ROUTED_MODEL, hop.clone())
                .await
                .unwrap();
            if read_terminal {
                let chunk = tokio::time::timeout(DEADLINE, stream.next())
                    .await
                    .unwrap()
                    .expect("terminal fixture chunk");
                assert!(!chunk.extra.contains_key("error"));
                assert!(!request_observation.snapshot().attempts[0]
                    .terminal_signals
                    .is_empty());
            }
            drop(stream);
            hop.selected(true);
            request_observation.finish(Some(200), BodyState::Dropped);
            let receipt = request_observation.snapshot();
            assert_eq!(
                receipt.attempts[0].transport_state,
                TransportState::Abandoned
            );
            assert_eq!(
                receipt.attempts[0].terminal_signals.is_empty(),
                !read_terminal
            );
            assert!(!receipt.capture_complete);
            assert_eq!(fixture.finish().await.len(), 1);
        }
    }
}

#[tokio::test]
async fn anthropic_compatibility_retry_records_both_exact_wire_sends() {
    let source: super::converter::AnthropicRequest = serde_json::from_value(json!({
        "model":SELECTOR,"max_tokens":2048,"thinking":{"type":"enabled","budget_tokens":1024},
        "messages":[{"role":"assistant","content":[
            {"type":"thinking","thinking":"synthetic summary","signature":"fixture-signature"},
            {"type":"text","text":"continue"}]}]
    }))
    .unwrap();
    let request = super::converter::anthropic_req_to_oai_req(&source);
    for stream in [false, true] {
        let mut rejected = Reply::json(json!({"error":{"message":"invalid signature"}}));
        rejected.status = StatusCode::BAD_REQUEST;
        let success = if stream {
            Reply::sse(&[
                json!({"type":"message_start","message":response(WireProtocol::Anthropic, true)}),
                json!({"type":"message_stop"}),
            ])
        } else {
            Reply::json(response(WireProtocol::Anthropic, true))
        };
        let fixture =
            UpstreamFixture::start(vec![rejected.clone(), success.clone(), rejected, success])
                .await;
        let provider = provider(WireProtocol::Anthropic, &fixture.base_url);
        let (request_observation, hop) = observation(provider.as_ref()).await;
        if stream {
            let observed = chunks(provider.as_ref(), &request, hop.clone()).await;
            let ordinary =
                chunks(provider.as_ref(), &request, ProviderObservation::default()).await;
            assert_eq!(chunk_output(&observed), chunk_output(&ordinary));
        } else {
            let observed = provider
                .complete_observed(&request, ROUTED_MODEL, hop.clone())
                .await
                .unwrap();
            let ordinary = provider.complete(&request, ROUTED_MODEL).await.unwrap();
            assert_eq!(output(&observed), output(&ordinary));
        }
        let receipt = settle(&request_observation, &hop, stream);
        assert_eq!(receipt.attempts.len(), 2);
        assert_eq!(receipt.selected_attempt, Some(2));
        assert!(receipt.capture_complete);
        for (index, attempt) in receipt.attempts.iter().enumerate() {
            assert_eq!(
                (attempt.index, attempt.hop_index, attempt.subattempt),
                (index as u32 + 1, 2, index as u32)
            );
            assert_eq!(attempt.protocol, WireProtocol::Anthropic);
            assert_eq!(attempt.wire_model.as_deref(), Some(WIRE_MODEL));
            assert!(attempt.send_started);
        }
        assert_eq!(receipt.attempts[0].http_status, Some(400));
        assert_eq!(receipt.attempts[0].transport_state, TransportState::Failed);
        assert_eq!(
            receipt.attempts[0].error_code.as_deref(),
            Some("upstream_http_error")
        );
        assert_eq!(
            receipt.attempts[0].provider_model_state,
            ValueState::Missing
        );
        assert_eq!(receipt.attempts[1].http_status, Some(200));
        assert_eq!(
            receipt.attempts[1].provider_model.as_deref(),
            Some(REPORTED_MODEL)
        );
        assert_eq!(
            receipt.attempts[1].provider_model_source,
            Some(ModelSource::MessageModel)
        );
        let requests = fixture.finish().await;
        assert_eq!(requests.len(), 4);
        assert_wire(&requests, WireProtocol::Anthropic, stream);
        assert_eq!(requests[0].body, requests[2].body);
        assert_eq!(requests[1].body, requests[3].body);
        assert_eq!(
            requests[0].body["messages"][0]["content"][0]["type"],
            "thinking"
        );
        assert_eq!(
            requests[1].body["messages"][0]["content"][0]["type"],
            "text"
        );
        assert_eq!(requests[1].body["thinking"]["budget_tokens"], 1024);
    }
}
