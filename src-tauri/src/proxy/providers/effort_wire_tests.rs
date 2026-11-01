//! Capture real loopback HTTP requests at the provider boundary. No real
//! account, live configuration, or external endpoint is used by these tests.

use super::{openai::OpenAIProvider, responses::ResponsesProvider, Provider};
use crate::database::dao::proxy_providers::CircuitBreakerConfig;
use crate::proxy::{canonical::ChatCompletionRequest, converter, responses::ResponsesRequest};
use axum::{routing::post, Json, Router};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Duration;

async fn capture_request(request: &ChatCompletionRequest, responses: bool) -> Value {
    let (capture_sender, capture_receiver) = tokio::sync::oneshot::channel();
    let sender = Arc::new(Mutex::new(Some(capture_sender)));
    let response = if responses {
        json!({"id":"resp_fixture","object":"response","created_at":0,"status":"completed","model":"gpt-5.6-sol","output":[{"type":"message","id":"msg_fixture","status":"completed","role":"assistant","content":[{"type":"output_text","text":"ok","annotations":[]}]}],"usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}})
    } else {
        json!({"id":"chat_fixture","object":"chat.completion","created":0,"model":"gpt-5.6-sol","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}})
    };
    let app = Router::new().route(
        if responses {
            "/v1/responses"
        } else {
            "/v1/chat/completions"
        },
        post(move |Json(body): Json<Value>| {
            let sender = sender.clone();
            let response = response.clone();
            async move {
                if let Some(sender) = sender.lock().unwrap().take() {
                    let _ = sender.send(body);
                }
                Json(response)
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (shutdown_sender, shutdown_receiver) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = shutdown_receiver.await;
            })
            .await
            .unwrap();
    });
    let base = format!("http://{address}/v1");
    let provider: Box<dyn Provider> = if responses {
        Box::new(
            ResponsesProvider::new(
                "fixture".into(),
                base,
                String::new(),
                vec!["gpt-5.6-sol".into()],
                true,
                2000,
                CircuitBreakerConfig::default(),
                Default::default(),
            )
            .unwrap(),
        )
    } else {
        Box::new(
            OpenAIProvider::new(
                "fixture".into(),
                base,
                String::new(),
                vec!["gpt-5.6-sol".into()],
                true,
                2000,
                CircuitBreakerConfig::default(),
                Default::default(),
            )
            .unwrap(),
        )
    };
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        provider.complete(request, "gpt-5.6-sol"),
    )
    .await;
    let _ = shutdown_sender.send(());
    server.await.unwrap();
    result
        .expect("bounded fixture request")
        .expect("fixture completion");
    capture_receiver.await.expect("upstream received request")
}

#[tokio::test]
async fn codex_selected_effort_reaches_native_responses_and_translated_chat_wire() {
    for effort in ["low", "medium", "high", "xhigh", "max"] {
        let source: ResponsesRequest = serde_json::from_value(
            json!({"model":"5.6sol","input":"fixture","reasoning":{"effort":effort}}),
        )
        .unwrap();
        let native = converter::responses_to_chat(&source);
        let native_wire = capture_request(&native, true).await;
        assert_eq!(native_wire["model"], "gpt-5.6-sol");
        assert_eq!(native_wire["reasoning"]["effort"], effort);
        assert!(native_wire.get("reasoning_effort").is_none());

        let translated = converter::try_responses_to_chat(&source).unwrap();
        let chat_wire = capture_request(&translated, false).await;
        assert_eq!(chat_wire["model"], "gpt-5.6-sol");
        assert_eq!(chat_wire["reasoning_effort"], effort);
        assert!(chat_wire.get("reasoning").is_none());
        assert_eq!(
            translated.source_responses_request.as_ref().unwrap().extra["reasoning"]["effort"],
            effort
        );
    }
}

#[tokio::test]
async fn existing_openai_chat_effort_reaches_both_wire_formats_unchanged() {
    let request: ChatCompletionRequest = serde_json::from_value(json!({"model":"gpt-5.6-sol","messages":[{"role":"user","content":"fixture"}],"reasoning_effort":"high"})).unwrap();
    let chat_wire = capture_request(&request, false).await;
    assert_eq!(chat_wire["reasoning_effort"], "high");
    let responses_wire = capture_request(&request, true).await;
    assert_eq!(responses_wire["reasoning"]["effort"], "high");
    assert!(responses_wire.get("reasoning_effort").is_none());
}

#[test]
fn codex_effort_is_never_silently_dropped_by_a_gemini_hop() {
    let source: ResponsesRequest = serde_json::from_value(json!({
        "model":"fixture_alias", "input":"fixture", "reasoning":{"effort":"high"}
    }))
    .unwrap();
    let request = converter::try_responses_to_chat(&source).unwrap();
    let error = crate::proxy::gemini::request_body(&request).unwrap_err();
    assert_eq!(error.code, "responses_feature_unsupported");
    // Other client surfaces retain the pre-existing Gemini conversion behavior.
    let chat: ChatCompletionRequest = serde_json::from_value(json!({
        "model":"fixture", "messages":[{"role":"user","content":"fixture"}], "reasoning_effort":"high"
    })).unwrap();
    assert!(crate::proxy::gemini::request_body(&chat).is_ok());
}

#[test]
fn responses_translation_rejects_unrepresentable_reasoning_instead_of_dropping_it() {
    for reasoning in [
        json!({"effort":12}),
        json!({"effort":"high","summary":"auto"}),
        json!({"effort":"high","future_option":true}),
    ] {
        let source: ResponsesRequest = serde_json::from_value(
            json!({"model":"5.6sol","input":"fixture","reasoning":reasoning}),
        )
        .unwrap();
        assert!(converter::try_responses_to_chat(&source).is_err());
        let native = converter::responses_to_chat(&source);
        assert_eq!(
            native.source_responses_request.unwrap().extra["reasoning"],
            reasoning
        );
    }
}

#[test]
fn absent_or_null_responses_reasoning_retains_upstream_defaults() {
    for reasoning in [Value::Null, json!({}), json!({"effort":null})] {
        let source: ResponsesRequest = serde_json::from_value(
            json!({"model":"5.6sol","input":"fixture","reasoning":reasoning}),
        )
        .unwrap();
        let translated = converter::try_responses_to_chat(&source).unwrap();
        assert!(translated
            .extra
            .get("reasoning_effort")
            .is_none_or(Value::is_null));
        assert!(!translated.extra.contains_key("reasoning"));
    }
}
