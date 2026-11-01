//! Local-only protocol acceptance: no live provider configuration or calls.
use super::*;
use axum::{
    extract::{Json, OriginalUri},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
    Router as HttpRouter,
};
use serde_json::json;

#[tokio::test]
async fn anthropic_prompt_paths_preserve_auth_caps_and_normal_termination() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured = observed.clone();
    let upstream = tokio::spawn(async move {
        let handler = move |OriginalUri(uri): OriginalUri,
                            headers: HeaderMap,
                            Json(body): Json<Value>| {
            let captured = captured.clone();
            async move {
                if headers
                    .get("x-api-key")
                    .and_then(|value| value.to_str().ok())
                    != Some("fixture-anthropic-key")
                {
                    return (StatusCode::UNAUTHORIZED, Json(json!({"type":"error","error":{"type":"authentication_error","message":"fixture credential missing"}}))).into_response();
                }
                assert_eq!(headers.get("anthropic-version").unwrap(), "2023-06-01");
                assert_eq!(body["model"], "fixture-anthropic-model");
                assert_eq!(body["stream"], false);
                assert!(body["system"].is_string());
                assert_eq!(body["messages"][0]["role"], "user");
                assert_eq!(body["messages"][0]["content"][0]["type"], "text");
                assert!(body["messages"][0]["content"][0]["text"]
                    .as_str()
                    .unwrap()
                    .contains("<requirement>"));
                assert!(body["max_tokens"].as_u64().unwrap() <= 1024);
                assert!(body.get("source_anthropic_request").is_none());
                assert!(body.get("max_completion_tokens").is_none());
                captured.lock().unwrap().push(uri.to_string());
                Json(json!({"id":"fixture","type":"message","role":"assistant","model":"fixture-anthropic-model", "content":[{"type":"text","text":"Implement the requested change and verify it."}], "stop_reason":"end_turn","stop_sequence":null,"usage":{"input_tokens":20,"output_tokens":10}})).into_response()
            }
        };
        axum::serve(
            listener,
            HttpRouter::new()
                .route("/v1/messages", post(handler.clone()))
                .route("/relay/anthropic/v1/messages", post(handler)),
        )
        .await
        .unwrap();
    });
    for base in [
        &origin,
        &format!("{origin}/v1"),
        &format!("{origin}/v1/messages"),
        &format!("{origin}/relay/anthropic"),
        &format!("{origin}/relay/anthropic/v1/messages?api-version=fixture"),
    ] {
        let db = Database::memory().unwrap();
        let mut provider = tests::mock_provider_row();
        provider.provider_type = "anthropic".into();
        provider.base_url = base.to_string();
        provider.api_key = "fixture-anthropic-key".into();
        provider.models = vec!["fixture-anthropic-model".into()];
        db.save_proxy_provider(&provider).unwrap();
        for stage in ["zh_expand", "en_optimize"] {
            let mut request =
                tests::gateway_request(stage, "Implement a parser and preserve original records.");
            request.max_tokens = Some(1024);
            request.target = target::Prompt_target::Provider {
                provider_id: provider.id.clone(),
                model: "fixture-anthropic-model".into(),
            };
            let output = run_stage(&db, None, request).await.unwrap();
            assert!(output.complete);
            assert_eq!(output.attempts, 1);
            assert_eq!(
                output.output,
                "Implement the requested change and verify it."
            );
        }
    }
    assert_eq!(observed.lock().unwrap().len(), 10);
    assert!(observed
        .lock()
        .unwrap()
        .iter()
        .any(|uri| uri.ends_with("?api-version=fixture")));
    upstream.abort();
    let _ = upstream.await;
}

#[tokio::test]
async fn known_reasoning_model_gets_prompt_budget_controls_on_actual_hop_for_both_stages() {
    use crate::database::dao::routes::RouteHop;
    use std::collections::HashMap;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured = observed.clone();
    let upstream = tokio::spawn(async move {
        axum::serve(listener, HttpRouter::new().route("/v1/messages", post(move |Json(body): Json<Value>| {
            let captured = captured.clone();
            async move {
                let known = body["model"] == "MiniMax-M3.1-Flash";
                let exhausted = known && body["output_config"]["effort"] != "low";
                if !known { assert!(body.get("output_config").is_none()); }
                captured.lock().unwrap().push(body.clone());
                Json(json!({"id":"fixture","type":"message","role":"assistant","model":body["model"],
                    "content":if exhausted { json!([{"type":"thinking","thinking":"fixture private reasoning","signature":"fixture"}]) } else { json!([{"type":"text","text":"Implement the requested parser and verify the result."}]) },
                    "stop_reason":if exhausted {"max_tokens"} else {"end_turn"},"usage":{"input_tokens":20,"output_tokens":100}}))
            }
        }))).await.unwrap();
    });
    let db = Database::memory().unwrap();
    let mut provider = tests::mock_provider_row();
    provider.provider_type = "anthropic".into();
    provider.base_url = origin;
    provider.models = vec!["MiniMax-M3.1-Flash".into(), "other-model".into()];
    db.save_proxy_provider(&provider).unwrap();
    let runtime: Arc<dyn crate::proxy::providers::Provider> =
        Arc::from(crate::proxy::providers::build_provider(&provider).unwrap());
    let router = Arc::new(Router::new(
        HashMap::from([(provider.id.clone(), runtime)]),
        HashMap::from([
            (
                "prompt-route".into(),
                vec![RouteHop {
                    provider: provider.id.clone(),
                    model: "MiniMax-M3.1-Flash".into(),
                }],
            ),
            (
                "other-route".into(),
                vec![RouteHop {
                    provider: provider.id.clone(),
                    model: "other-model".into(),
                }],
            ),
        ]),
    ));
    for stage in ["zh_expand", "en_optimize"] {
        for target in [
            target::Prompt_target::Provider {
                provider_id: provider.id.clone(),
                model: "MiniMax-M3.1-Flash".into(),
            },
            target::Prompt_target::Gateway {
                model: "prompt-route".into(),
            },
            target::Prompt_target::Gateway {
                model: "other-route".into(),
            },
        ] {
            let mut request = tests::gateway_request(stage, "Implement a parser.");
            request.target = target;
            let output = run_stage(&db, Some(router.clone()), request).await.unwrap();
            assert!(output.complete);
            assert_eq!(output.attempts, 1);
            assert!(!output.output.contains("fixture private reasoning"));
        }
    }
    assert_eq!(observed.lock().unwrap().len(), 6);
    upstream.abort();
    let _ = upstream.await;
}
