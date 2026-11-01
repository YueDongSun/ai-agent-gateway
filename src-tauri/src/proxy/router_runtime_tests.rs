use super::*;
use crate::database::dao::proxy_providers::CircuitBreakerConfig;
use crate::proxy::canonical::{OAIChoice, OAIMessage, StreamDelta};
use crate::proxy::circuit_breaker::CircuitBreaker;
use crate::proxy::planner::{Model_planner, Planned_hop, Request_features, Resolution};
use crate::proxy::session_affinity::{next_transport_identity, Provider_binding};
use async_trait::async_trait;
use futures_core::Stream;
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::Mutex;

#[allow(non_camel_case_types)]
struct Fixture_provider {
    id: &'static str,
    models: Vec<String>,
    breaker: CircuitBreaker,
    calls: Mutex<Vec<String>>,
    fail: AtomicBool,
    native: bool,
    status: AtomicU16,
    enabled: bool,
    credential_required: bool,
    wire_protocol: &'static str,
    error_body: Option<&'static str>,
    stream_error_body: Option<Value>,
    stream_error_after_output: bool,
}

impl Fixture_provider {
    fn new(id: &'static str, native: bool) -> Arc<Self> {
        Arc::new(Self {
            id,
            models: vec![
                "m".into(),
                "literal".into(),
                "strong".into(),
                "backup".into(),
                "replacement".into(),
                "other".into(),
                "custom_model".into(),
            ],
            breaker: CircuitBreaker::new(CircuitBreakerConfig::default()),
            calls: Mutex::new(Vec::new()),
            fail: AtomicBool::new(false),
            native,
            status: AtomicU16::new(0),
            enabled: true,
            credential_required: false,
            wire_protocol: "unknown",
            error_body: None,
            stream_error_body: None,
            stream_error_after_output: false,
        })
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl Provider for Fixture_provider {
    fn id(&self) -> &str {
        self.id
    }
    fn enabled(&self) -> bool {
        self.enabled
    }
    fn requires_client_credential(&self) -> bool {
        self.credential_required
    }
    fn wire_protocol(&self) -> &'static str {
        self.wire_protocol
    }
    fn models(&self) -> &[String] {
        &self.models
    }
    fn breaker(&self) -> &CircuitBreaker {
        &self.breaker
    }

    async fn complete(
        &self,
        _req: &ChatCompletionRequest,
        model: &str,
    ) -> Result<OAIChatResponse, ProviderError> {
        let sequence = {
            let mut calls = self.calls.lock().unwrap();
            calls.push(model.to_string());
            calls.len()
        };
        let status = self.status.load(Ordering::SeqCst);
        if status != 0 {
            let body = self.error_body.unwrap_or("");
            return Err(canonical::make_provider_error_with_kind(
                "upstream_http_error",
                "response_status",
                Some(status),
                canonical::upstream_quota_retriable(status, body),
                None,
                canonical::classify_upstream_4xx(status, body),
            ));
        }
        if self.fail.load(Ordering::SeqCst) {
            return Err(canonical::make_error(
                "upstream_http_error",
                Some(503),
                Some(true),
            ));
        }
        Ok(OAIChatResponse {
            id: format!("resp_{}_{sequence}", self.id),
            object: "chat.completion".into(),
            created: 0,
            model: model.into(),
            choices: vec![OAIChoice {
                index: 0,
                message: OAIMessage {
                    role: "assistant".into(),
                    content: Some("fixture".into()),
                    tool_calls: None,
                    refusal: None,
                    extra: Default::default(),
                },
                finish_reason: Some("stop".into()),
                extra: Default::default(),
            }],
            usage: None,
            extra: Default::default(),
            source_anthropic_response: None,
            source_gemini_response: None,
            source_responses_response: self.native.then(|| {
                json!({
                    "id": format!("resp_{}_{sequence}", self.id),
                    "object": "response", "status": "completed", "error": null,
                    "output": [{"type": "reasoning", "id": format!("item_{}_{sequence}", self.id),
                        "encrypted_content": format!("opaque_{}_{sequence}", self.id)}],
                })
            }),
        })
    }

    async fn stream(
        &self,
        req: &ChatCompletionRequest,
        model: &str,
    ) -> Result<Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>>, ProviderError> {
        let response = self.complete(req, model).await?;
        let mut chunk = crate::proxy::converter::make_stream_chunk(
            &response.id,
            model,
            StreamDelta {
                role: Some("assistant".into()),
                content: Some("fixture".into()),
                tool_calls: None,
                reasoning: None,
                extra: Default::default(),
            },
            Some("stop".into()),
        );
        if let Some(source) = response.source_responses_response {
            chunk.source_responses_event = Some(crate::proxy::responses::ResponsesSseEvent {
                event: Some("response.completed".into()),
                id: None,
                retry: None,
                data: json!({"type": "response.completed", "response": source}).to_string(),
            });
        }
        if let Some(error) = &self.stream_error_body {
            let mut error_chunk = canonical::stream_error_chunk(model, 400, "api_error");
            error_chunk.extra.insert("error".into(), error.clone());
            if self.stream_error_after_output {
                chunk.choices[0].finish_reason = None;
            }
            return Ok(
                futures_util::stream::iter(if self.stream_error_after_output {
                    vec![chunk, error_chunk]
                } else {
                    vec![error_chunk]
                })
                .boxed(),
            );
        }
        Ok(futures_util::stream::iter(vec![chunk]).boxed())
    }

    async fn count_tokens(
        &self,
        body: Value,
        _credential: Option<&canonical::Client_credential>,
    ) -> Result<u32, ProviderError> {
        let model = body.get("model").and_then(Value::as_str).unwrap_or("m");
        self.complete(&request(model), model).await.map(|_| 4)
    }
}

#[allow(non_camel_case_types)]
struct Fixture_planner;

#[allow(non_camel_case_types)]
struct Tool_turn_provider {
    inner: Arc<Fixture_provider>,
    requests: Mutex<Vec<ChatCompletionRequest>>,
}

#[async_trait]
impl Provider for Tool_turn_provider {
    fn id(&self) -> &str {
        self.inner.id()
    }
    fn enabled(&self) -> bool {
        true
    }
    fn models(&self) -> &[String] {
        self.inner.models()
    }
    fn breaker(&self) -> &CircuitBreaker {
        self.inner.breaker()
    }

    async fn complete(
        &self,
        req: &ChatCompletionRequest,
        model: &str,
    ) -> Result<OAIChatResponse, ProviderError> {
        let first_turn = {
            let mut requests = self.requests.lock().unwrap();
            requests.push(req.clone());
            requests.len() == 1
        };
        let mut response = self.inner.complete(req, model).await?;
        if first_turn {
            response.choices[0].message.content = None;
            response.choices[0].message.tool_calls = Some(vec![json!({
                "id":"call_fixture", "type":"function",
                "function":{"name":"fixture_tool", "arguments":"{\"value\":1}"}
            })]);
            response.choices[0].finish_reason = Some("tool_calls".into());
        }
        Ok(response)
    }

    async fn stream(
        &self,
        req: &ChatCompletionRequest,
        model: &str,
    ) -> Result<Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>>, ProviderError> {
        let response = self.complete(req, model).await?;
        let choice = response.choices.into_iter().next().unwrap();
        let tool_calls = choice.message.tool_calls.map(|calls| {
            calls
                .into_iter()
                .enumerate()
                .map(|(index, mut call)| {
                    call["index"] = json!(index);
                    call
                })
                .collect()
        });
        let chunk = crate::proxy::converter::make_stream_chunk(
            &response.id,
            model,
            StreamDelta {
                role: Some("assistant".into()),
                content: choice.message.content,
                tool_calls,
                reasoning: None,
                extra: Default::default(),
            },
            choice.finish_reason,
        );
        Ok(futures_util::stream::iter(vec![chunk]).boxed())
    }
}

fn planned(provider: &str, model: &str, tier: u8, downgraded: bool) -> Planned_hop {
    Planned_hop {
        provider: provider.into(),
        model: model.into(),
        tier: Some(tier),
        priority: 0,
        downgraded,
        basis: if downgraded {
            "public_metadata"
        } else {
            "manual_override"
        }
        .into(),
        reason: if downgraded {
            "tier_downgrade"
        } else {
            "tier_match"
        }
        .into(),
    }
}

impl Model_planner for Fixture_planner {
    fn resolve(&self, model: &str, _: Request_features) -> Result<Resolution, ProviderError> {
        Ok(match model {
            "precise:literal" | "literal" => Resolution::Precise {
                model: "literal".into(),
                hops: vec![
                    planned("p1", "literal", 3, false),
                    planned("p2", "literal", 3, false),
                ],
            },
            "alias:fable" | "fable" => Resolution::Alias {
                alias: "fable".into(),
                tier: 3,
                hops: vec![
                    planned("p1", "strong", 3, false),
                    planned("p2", "backup", 2, true),
                ],
            },
            other => Resolution::Exact(other.into()),
        })
    }
}

fn hop(provider: &str, model: &str) -> RouteHop {
    RouteHop {
        provider: provider.into(),
        model: model.into(),
    }
}

fn router(providers: &[Arc<Fixture_provider>], routes: HashMap<String, Vec<RouteHop>>) -> Router {
    let bindings = providers
        .iter()
        .map(|provider| {
            (
                provider.id.into(),
                Provider_binding {
                    identity: next_transport_identity(),
                    native_responses: provider.native,
                },
            )
        })
        .collect();
    Router::new(
        providers
            .iter()
            .map(|provider| {
                (
                    provider.id.into(),
                    Arc::clone(provider) as Arc<dyn Provider>,
                )
            })
            .collect(),
        routes,
    )
    .with_provider_bindings(bindings)
    .with_planner(Arc::new(Fixture_planner))
}

fn request(model: &str) -> ChatCompletionRequest {
    serde_json::from_value(
        json!({"model": model, "messages": [{"role": "user", "content": "fixture"}]}),
    )
    .unwrap()
}

fn responses_request(body: Value) -> ChatCompletionRequest {
    crate::proxy::converter::responses_to_chat(&serde_json::from_value(body).unwrap())
}

fn native_request(previous: Option<&str>) -> ChatCompletionRequest {
    let mut body = json!({"model": "m", "input": "fixture"});
    if let Some(previous) = previous {
        body["previous_response_id"] = previous.into();
    }
    responses_request(body)
}

#[test]
fn manual_routes_only_restrict_plans_and_cannot_substitute_exact_models() {
    for model in ["precise:literal", "literal"] {
        let p1 = Fixture_provider::new("p1", false);
        let router = router(
            &[p1],
            HashMap::from([(model.into(), vec![hop("p1", "replacement")])]),
        );
        let error = router
            .resolve_request(SURFACE_CHAT, &request(model))
            .unwrap_err();
        assert_eq!(error.code, "model_precise_unavailable");
    }
    let p1 = Fixture_provider::new("p1", false);
    let p2 = Fixture_provider::new("p2", false);
    let router = router(
        &[p1, p2],
        HashMap::from([(
            "alias:fable".into(),
            vec![
                hop("p2", "backup"),
                hop("p1", "invented"),
                hop("p1", "strong"),
            ],
        )]),
    );
    let hops = router
        .resolve_request(SURFACE_CHAT, &request("alias:fable"))
        .unwrap();
    assert_eq!(
        hops.iter()
            .map(|hop| hop.model.as_str())
            .collect::<Vec<_>>(),
        vec!["backup", "strong"]
    );
}

#[tokio::test]
async fn pins_resolve_real_models_and_reject_ineligible_providers_without_dispatch() {
    let p1 = Fixture_provider::new("p1", false);
    let p2 = Fixture_provider::new("p2", false);
    let p3 = Fixture_provider::new("p3", false);
    let router = router(&[p1, p2.clone(), p3.clone()], HashMap::new());
    let precise = router
        .complete_with_provider(SURFACE_CHAT, "chat", "p2", &request("precise:literal"))
        .await
        .unwrap();
    assert_eq!(precise.model, "literal");
    let alias = router
        .complete_with_provider(SURFACE_CHAT, "chat", "p2", &request("alias:fable"))
        .await
        .unwrap();
    assert_eq!(alias.model, "backup");
    let chunks = router
        .stream_with_provider(SURFACE_CHAT, "chat", "p2", &request("alias:fable"))
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;
    assert_eq!(chunks[0].model, "backup");
    let error = router
        .complete_with_provider(SURFACE_CHAT, "chat", "p3", &request("alias:fable"))
        .await
        .unwrap_err();
    assert_eq!(error.code, "pinned_provider_ineligible");
    assert!(router
        .stream_with_provider(SURFACE_CHAT, "chat", "p3", &request("precise:literal"))
        .await
        .is_err());
    assert!(p3.calls().is_empty());
    assert_eq!(p2.calls(), ["literal", "backup", "backup"]);
}

#[tokio::test]
async fn legacy_entrypoints_still_obey_precise_and_alias_namespaces() {
    let p1 = Fixture_provider::new("p1", false);
    let router = router(&[p1.clone()], HashMap::new());
    assert_eq!(
        router
            .complete(&request("precise:literal"))
            .await
            .unwrap()
            .model,
        "literal"
    );
    let chunks = router
        .stream(&request("alias:fable"))
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;
    assert_eq!(chunks[0].model, "strong");
    assert_eq!(p1.calls(), ["literal", "strong"]);
}

#[tokio::test]
async fn arbitrary_manual_alias_remains_usable_without_a_planned_claim() {
    let p1 = Fixture_provider::new("p1", false);
    let router = router(
        &[p1],
        HashMap::from([("custom_route".into(), vec![hop("p1", "custom_model")])]),
    );
    assert_eq!(
        router
            .complete(&request("custom_route"))
            .await
            .unwrap()
            .model,
        "custom_model"
    );
}

#[tokio::test]
async fn precise_namespace_is_safe_without_a_planner_or_matching_manual_route() {
    let p1 = Fixture_provider::new("p1", false);
    let router = router(
        &[p1.clone()],
        HashMap::from([("precise:literal".into(), vec![hop("p1", "other")])]),
    );
    let mut unplanned = router;
    unplanned.planner = None;
    assert_eq!(
        unplanned
            .complete(&request("precise:literal"))
            .await
            .unwrap()
            .model,
        "literal"
    );
    assert!(unplanned.complete(&request("alias:fable")).await.is_err());
    assert_eq!(p1.calls(), ["literal"]);
}

#[tokio::test]
async fn native_continuation_stays_on_failover_source_and_never_replays_to_recovered_head() {
    let p1 = Fixture_provider::new("p1", true);
    let p2 = Fixture_provider::new("p2", true);
    let router = router(
        &[p1.clone(), p2.clone()],
        HashMap::from([("m".into(), vec![hop("p1", "m"), hop("p2", "m")])]),
    );
    p1.fail.store(true, Ordering::SeqCst);
    let first = router.complete(&native_request(None)).await.unwrap();
    assert_eq!(first.id, "resp_p2_1");
    p1.fail.store(false, Ordering::SeqCst);
    let continuation = native_request(Some(&first.id));
    assert_eq!(
        router.complete(&continuation).await.unwrap().id,
        "resp_p2_2"
    );
    p2.fail.store(true, Ordering::SeqCst);
    assert_eq!(
        router.complete(&continuation).await.unwrap_err().status,
        Some(503)
    );
    assert_eq!(
        p1.calls().len(),
        1,
        "bound state cannot be replayed to recovered p1"
    );
    assert_eq!(p2.calls().len(), 3);
}

#[tokio::test]
async fn unknown_native_state_is_rejected_before_any_provider_call() {
    let p1 = Fixture_provider::new("p1", true);
    let router = router(&[p1.clone()], HashMap::new());
    for body in [
        json!({"model": "m", "input": "fixture", "previous_response_id": "unknown"}),
        json!({"model": "m", "input": [{"type": "item_reference", "id": "unknown"}]}),
        json!({"model": "m", "input": [{"type": "reasoning", "encrypted_content": "unknown"}]}),
        json!({"model": "m", "input": "fixture", "conversation": "unknown"}),
    ] {
        let error = router.complete(&responses_request(body)).await.unwrap_err();
        assert_eq!(error.code, "responses_source_unknown");
        assert!(!error.retriable);
    }
    assert!(p1.calls().is_empty());
}

#[tokio::test]
async fn materialized_codex_message_ids_do_not_require_a_native_source_binding() {
    let provider = Fixture_provider::new("p1", false);
    let router = router(&[provider.clone()], HashMap::new());
    let req = responses_request(json!({
        "model":"m",
        "include":["reasoning.encrypted_content"],
        "input":[
            {"id":"client_developer_1", "type":"message", "role":"developer", "content":[{"type":"input_text", "text":"fixture instruction"}]},
            {"id":"client_developer_2", "type":"message", "role":"developer", "content":[{"type":"input_text", "text":"fixture context"}]},
            {"id":"client_user_1", "type":"message", "role":"user", "content":[{"type":"input_text", "text":"fixture question"}]},
            {"id":"client_user_2", "type":"message", "role":"user", "content":[{"type":"input_text", "text":"fixture request"}]}
        ]
    }));
    assert!(router.complete(&req).await.is_ok());
    let chunks = router.stream(&req).await.unwrap().collect::<Vec<_>>().await;
    assert!(!chunks.is_empty());
    assert_eq!(provider.calls(), ["m", "m"]);
}

#[tokio::test]
async fn materialized_message_ids_do_not_weaken_opaque_or_explicit_source_guards() {
    let provider = Fixture_provider::new("p1", true);
    let router = router(&[provider.clone()], HashMap::new());
    let complete_message = json!({"type":"message", "id":"client_item", "role":"user", "content":[{"type":"input_text", "text":"fixture"}]});
    for body in [
        json!({"model":"m", "previous_response_id":"unknown", "input":[complete_message.clone()]}),
        json!({"model":"m", "input":[complete_message.clone(), {"type":"item_reference", "id":"unknown"}]}),
        json!({"model":"m", "input":[complete_message.clone(), {"type":"reasoning", "id":"opaque_item", "encrypted_content":"unknown"}]}),
        json!({"model":"m", "input":[{"type":"message", "id":"opaque_item", "role":"user", "content":"fixture", "encrypted_content":"unknown"}]}),
        json!({"model":"m", "input":[{"type":"message", "id":"unmaterialized", "role":"assistant"}]}),
        json!({"model":"m", "input":[{"type":"message", "id":"unfinished", "role":"assistant", "content":[], "status":"in_progress"}]}),
    ] {
        assert_eq!(
            router
                .complete(&responses_request(body))
                .await
                .unwrap_err()
                .code,
            "responses_source_unknown"
        );
    }
    assert!(provider.calls().is_empty());
}

#[tokio::test]
async fn fallback_responses_tool_turns_replay_materialized_calls_without_native_bindings() {
    for streaming in [false, true] {
        let provider = Arc::new(Tool_turn_provider {
            inner: Fixture_provider::new("tool-provider", false),
            requests: Mutex::new(Vec::new()),
        });
        let provider_trait: Arc<dyn Provider> = provider.clone();
        let router = Router::new(
            [("tool-provider".into(), provider_trait)]
                .into_iter()
                .collect(),
            HashMap::new(),
        );
        let initial_message = json!({"type":"message", "id":"client_user", "role":"user", "content":"fixture tool request"});
        let initial = responses_request(
            json!({"model":"m", "input":[initial_message.clone()], "stream":streaming}),
        );
        let first_response = if streaming {
            let stream = router.stream(&initial).await.unwrap();
            let frames = crate::proxy::converter::chat_chunks_to_responses_stream(stream, "m")
                .collect::<Vec<_>>()
                .await;
            frames
                .iter()
                .flat_map(|frame| frame.lines())
                .filter_map(|line| line.strip_prefix("data: "))
                .filter_map(|data| serde_json::from_str::<Value>(data).ok())
                .find(|event| event["type"] == "response.completed")
                .expect("generated stream completes")["response"]
                .clone()
        } else {
            crate::proxy::converter::chat_to_responses(
                &router.complete(&initial).await.unwrap(),
                "m",
            )
        };
        let call = first_response["output"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["type"] == "function_call")
            .expect("generated tool call")
            .clone();
        assert!(call["id"].as_str().is_some_and(|id| !id.is_empty()));
        assert_eq!(call["arguments"], "{\"value\":1}");
        let followup = responses_request(json!({
            "model":"m", "stream":streaming,
            "input":[initial_message, call.clone(), {
                "type":"function_call_output", "id":"client_output", "call_id":call["call_id"], "output":"fixture tool result"
            }]
        }));
        if streaming {
            let chunks = router
                .stream(&followup)
                .await
                .unwrap()
                .collect::<Vec<_>>()
                .await;
            assert!(chunks
                .iter()
                .any(|chunk| chunk
                    .choices
                    .iter()
                    .any(|choice| choice.delta.content.as_deref() == Some("fixture"))));
        } else {
            assert_eq!(
                router.complete(&followup).await.unwrap().choices[0]
                    .message
                    .content
                    .as_deref(),
                Some("fixture")
            );
        }
        let requests = provider.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        let assistant = requests[1]
            .messages
            .iter()
            .find(|message| message.tool_calls.is_some())
            .unwrap();
        let canonical_call = &assistant.tool_calls.as_ref().unwrap()[0];
        assert_eq!(canonical_call["id"], "call_fixture");
        assert_eq!(canonical_call["function"]["arguments"], "{\"value\":1}");
        let result = requests[1]
            .messages
            .iter()
            .find(|message| message.role == "tool")
            .unwrap();
        assert_eq!(result.tool_call_id.as_deref(), Some("call_fixture"));
        assert_eq!(result.content, "fixture tool result");
        drop(requests);
        // Generated ids carry no native state ownership and cannot be used as
        // a bare server reference in place of the materialized payload.
        let reference = responses_request(
            json!({"model":"m", "input":[{"type":"item_reference", "id":call["id"]}]}),
        );
        assert_eq!(
            router.complete(&reference).await.unwrap_err().code,
            "responses_source_unknown"
        );
        assert_eq!(provider.requests.lock().unwrap().len(), 2);
    }
}

#[tokio::test]
async fn incomplete_or_opaque_function_items_still_require_source_ownership() {
    let provider = Fixture_provider::new("p1", true);
    let router = router(&[provider.clone()], HashMap::new());
    for item in [
        json!({"type":"function_call", "id":"unknown", "call_id":"call", "name":"tool", "arguments":"{"}),
        json!({"type":"function_call", "id":"unknown", "call_id":"call", "name":"tool", "arguments":"{}", "status":"in_progress"}),
        json!({"type":"function_call", "id":"unknown", "call_id":"call", "name":"tool", "arguments":"{}", "encrypted_content":"opaque"}),
        json!({"type":"function_call_output", "id":"unknown", "call_id":"call"}),
        json!({"type":"function_call_output", "id":"unknown", "output":"fixture"}),
        json!({"type":"function_call_output", "id":"unknown", "call_id":"call", "output":[{"type":"input_file", "file_id":"server_file"}]}),
    ] {
        let req = responses_request(json!({"model":"m", "input":[item]}));
        assert_eq!(
            router.complete(&req).await.unwrap_err().code,
            "responses_source_unknown"
        );
    }
    assert!(provider.calls().is_empty());
}

#[tokio::test]
async fn known_continuation_with_new_materialized_message_ids_keeps_its_source() {
    let first_provider = Fixture_provider::new("p1", true);
    let second_provider = Fixture_provider::new("p2", true);
    let router = router(
        &[first_provider.clone(), second_provider.clone()],
        HashMap::new(),
    );
    let first = router
        .complete_with_provider(SURFACE_CODEX, "codex", "p2", &native_request(None))
        .await
        .unwrap();
    let req = responses_request(json!({
        "model":"m", "previous_response_id":first.id,
        "input":[{"type":"message", "id":"new_client_message", "role":"user", "content":"fixture follow-up"}]
    }));
    assert_eq!(router.complete(&req).await.unwrap().id, "resp_p2_2");
    assert!(first_provider.calls().is_empty());
    assert_eq!(second_provider.calls(), ["m", "m"]);
}

#[tokio::test]
async fn source_binding_is_caller_scoped_and_rejects_mixed_provider_state() {
    let p1 = Fixture_provider::new("p1", true);
    let p2 = Fixture_provider::new("p2", true);
    let router = router(
        &[p1.clone(), p2.clone()],
        HashMap::from([("m".into(), vec![hop("p1", "m"), hop("p2", "m")])]),
    );
    let first = router
        .complete_with_provider(SURFACE_CODEX, "codex", "p1", &native_request(None))
        .await
        .unwrap();
    let second = router
        .complete_with_provider(SURFACE_CODEX, "codex", "p2", &native_request(None))
        .await
        .unwrap();
    let mixed = responses_request(json!({"model": "m", "previous_response_id": first.id,
        "input": [{"type": "item_reference", "id": second.source_responses_response.unwrap()["output"][0]["id"]}]}));
    assert_eq!(
        router.complete(&mixed).await.unwrap_err().code,
        "responses_source_mismatch"
    );
    let mut foreign = native_request(Some("resp_p1_1"));
    foreign.client_credential = Some(canonical::Client_credential {
        authorization: Some("synthetic_other_client".into()),
        ..Default::default()
    });
    assert_eq!(
        router.complete(&foreign).await.unwrap_err().code,
        "responses_source_unknown"
    );
    assert_eq!(p1.calls().len(), 1);
    assert_eq!(p2.calls().len(), 1);
}

#[tokio::test]
async fn changed_transport_identity_invalidates_native_state_after_reload() {
    let p1 = Fixture_provider::new("p1", true);
    let mut old = router(&[p1.clone()], HashMap::new());
    old.complete(&native_request(None)).await.unwrap();
    let mut new = router(&[p1.clone()], HashMap::new());
    new.affinity = Arc::clone(&old.affinity);
    assert_eq!(
        new.complete(&native_request(Some("resp_p1_1")))
            .await
            .unwrap_err()
            .code,
        "responses_source_unavailable"
    );
    new.provider_bindings = std::mem::take(&mut old.provider_bindings);
    assert_eq!(
        new.complete(&native_request(Some("resp_p1_1")))
            .await
            .unwrap()
            .id,
        "resp_p1_2"
    );
    assert_eq!(p1.calls().len(), 2);
}

#[tokio::test]
async fn session_preference_keeps_the_exact_winning_model_across_reload() {
    let p1 = Fixture_provider::new("p1", false);
    let p2 = Fixture_provider::new("p2", false);
    let p3 = Fixture_provider::new("p3", false);
    let routes = HashMap::from([(
        "m".into(),
        vec![hop("p1", "m"), hop("p2", "m"), hop("p3", "m")],
    )]);
    let old = router(&[p1.clone(), p2.clone(), p3.clone()], routes.clone());
    let mut request = request("m");
    request
        .extra
        .insert("user".into(), "conversation_fixture".into());
    old.complete_with_provider(SURFACE_CHAT, "chat", "p2", &request)
        .await
        .unwrap();
    assert_eq!(p2.calls().len(), 1);
    let mut new = router(
        &[p1.clone(), p2.clone(), p3.clone()],
        HashMap::from([(
            "m".into(),
            vec![hop("p1", "m"), hop("p2", "replacement"), hop("p3", "m")],
        )]),
    );
    new.affinity = Arc::clone(&old.affinity);
    new.complete(&request).await.unwrap();
    assert_eq!(
        p1.calls(),
        ["m"],
        "same provider with a changed model does not inherit the session"
    );
    assert_eq!(
        p2.calls().len(),
        1,
        "the old binding must not promote the changed model"
    );
    assert!(p3.calls().is_empty());
}

#[tokio::test]
async fn successful_native_stream_records_continuation_source_but_unread_stream_does_not() {
    let p1 = Fixture_provider::new("p1", true);
    let router = router(&[p1.clone()], HashMap::new());
    let unread = router.stream(&native_request(None)).await.unwrap();
    drop(unread);
    assert_eq!(
        router
            .complete(&native_request(Some("resp_p1_1")))
            .await
            .unwrap_err()
            .code,
        "responses_source_unknown"
    );
    let chunks = router
        .stream(&native_request(None))
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;
    assert!(chunks[0].source_responses_event.is_some());
    assert_eq!(
        router
            .complete(&native_request(Some("resp_p1_2")))
            .await
            .unwrap()
            .id,
        "resp_p1_3"
    );
}

#[tokio::test]
async fn nonnative_responses_hop_validates_before_dispatch_and_keeps_simple_input() {
    let p1 = Fixture_provider::new("p1", false);
    let router = router(&[p1.clone()], HashMap::new());
    for body in [
        json!({"model": "m", "input": "fixture", "tools": [{"type": "web_search_preview"}]}),
        json!({"model": "m", "input": [{"type":"reasoning", "encrypted_content":"opaque_fixture"}]}),
        json!({"model": "m", "input": "fixture", "background": true}),
    ] {
        let req = responses_request(body);
        assert!(router.complete(&req).await.is_err());
        assert!(router.stream(&req).await.is_err());
    }
    assert!(p1.calls().is_empty());
    let mut simple = responses_request(json!({
        "model":"m", "include":["reasoning.encrypted_content"],
        "input":[{"id":"client_message", "type":"message", "role":"user", "content":[{"type":"input_text", "text":"fixture"}]}]
    }));
    simple.messages.insert(
        0,
        serde_json::from_value(json!({"role": "system", "content": "prefix"})).unwrap(),
    );
    assert!(router.complete(&simple).await.is_ok());
    assert_eq!(p1.calls(), ["m"]);
}

async fn timeline_result(fail_head: bool, stream: bool) -> crate::diagnostics::Request_event {
    use axum::{body::Body, extract::Request, response::Response, routing::post};
    use tower::Service;
    crate::diagnostics::clear_request_timeline();
    let p1 = Fixture_provider::new("p1", false);
    p1.fail.store(fail_head, Ordering::SeqCst);
    let p2 = Fixture_provider::new("p2", false);
    let router = Arc::new(router(&[p1, p2], HashMap::new()));
    let mut app = axum::Router::new()
        .route(
            "/v1/chat/completions",
            post(move || {
                let router = Arc::clone(&router);
                async move {
                    let req = request("alias:fable");
                    if stream {
                        let chunks = router.stream(&req).await.unwrap().map(|_| {
                            Ok::<_, std::io::Error>(bytes::Bytes::from_static(b"data: fixture\n\n"))
                        });
                        Response::builder()
                            .header("content-type", "text/event-stream")
                            .body(Body::from_stream(chunks))
                            .unwrap()
                    } else {
                        router.complete(&req).await.unwrap();
                        Response::new(Body::empty())
                    }
                }
            }),
        )
        .layer(axum::middleware::from_fn(
            crate::proxy::request_timeline::observe_request,
        ));
    let response = app
        .call(
            Request::builder()
                .uri("/v1/chat/completions")
                .method("POST")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    if stream {
        assert!(crate::diagnostics::request_timeline().is_empty());
    }
    axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let events = crate::diagnostics::request_timeline();
    assert_eq!(events.len(), 1);
    events[0].clone()
}

#[tokio::test]
async fn actual_winner_trace_does_not_mark_successful_head_as_downgraded() {
    let event = timeline_result(false, false).await;
    assert_eq!(event.plan_kind.as_deref(), Some("alias"));
    assert_eq!(event.plan_tier, Some(3));
    assert_eq!(event.plan_downgraded, Some(false));
    assert_eq!(event.plan_basis.as_deref(), Some("manual_override"));
    assert_eq!(event.plan_reason.as_deref(), Some("tier_match"));
}

#[tokio::test]
async fn live_status_reports_actual_alias_target_and_fallback() {
    for (fail_head, expected_model, fallback_count) in [(false, "strong", 0), (true, "backup", 1)] {
        let event = timeline_result(fail_head, false).await;
        let status = crate::proxy::request_status::snapshot().into_iter()
            .find(|row| row.request_id == event.request_id)
            .expect("dispatched request must have live status");
        assert_eq!(status.requested_model, "alias:fable");
        assert_eq!(status.resolved_model.as_deref(), Some(expected_model));
        assert_eq!(status.fallback_count, fallback_count);
        assert_eq!(status.phase, "completed");
        assert_eq!(status.status, Some(200));
        assert_eq!(status.error_code.as_deref(), fail_head.then_some("upstream_http_error"));
    }
}

async fn live_mapping_result(target: &str, generation: u64, stream: bool) -> crate::proxy::request_status::Request_status {
    use axum::{body::Body, extract::Request, response::Response, routing::post};
    use tower::Service;
    let provider = Fixture_provider::new("p1", false);
    let router = Arc::new(router(&[provider], HashMap::from([
        ("5.6sol".into(), vec![hop("unavailable", "unused"), hop("p1", target)]),
    ])));
    let mut app = axum::Router::new().route("/v1/responses", post(move || {
        let router = Arc::clone(&router);
        async move {
            crate::proxy::request_timeline::observe_generation(generation);
            crate::proxy::request_timeline::observe_model("5.6sol");
            let request_id = crate::proxy::request_timeline::request_id();
            let initial = crate::proxy::request_status::snapshot().into_iter()
                .find(|row| row.request_id == request_id).unwrap();
            assert_eq!(initial.phase, "requesting");
            assert_eq!(initial.resolved_model, None);
            let body = if stream {
                let source = router.stream(&request("5.6sol")).await.unwrap()
                    .map(|_| Ok::<_, std::io::Error>(bytes::Bytes::from_static(b"data: fixture\n\n")));
                Body::from_stream(source)
            } else {
                router.complete(&request("5.6sol")).await.unwrap();
                Body::empty()
            };
            Response::builder().header("x-fixture-request-id", request_id)
                .header("content-type", if stream { "text/event-stream" } else { "application/json" })
                .body(body).unwrap()
        }
    })).layer(axum::middleware::from_fn(crate::proxy::request_timeline::observe_request));
    let response = app.call(Request::builder().uri("/v1/responses")
        .method("POST").body(Body::empty()).unwrap()).await.unwrap();
    let request_id = response.headers()["x-fixture-request-id"].to_str().unwrap().to_string();
    if stream {
        let active = crate::proxy::request_status::snapshot().into_iter()
            .find(|row| row.request_id == request_id).unwrap();
        assert_eq!(active.phase, "streaming");
        assert_eq!(active.resolved_model.as_deref(), Some(target));
        assert_eq!(active.config_generation, Some(generation));
    }
    axum::body::to_bytes(response.into_body(), 4096).await.unwrap();
    crate::proxy::request_status::snapshot().into_iter()
        .find(|row| row.request_id == request_id).unwrap()
}

#[tokio::test]
async fn live_status_keeps_actual_mapping_and_generation_after_configuration_changes() {
    let original = live_mapping_result("strong", 7, true).await;
    let replacement = live_mapping_result("replacement", 8, false).await;
    assert_eq!(original.requested_model, "5.6sol");
    assert_eq!(original.resolved_model.as_deref(), Some("strong"));
    assert_eq!(original.phase, "completed");
    assert_eq!(original.config_generation, Some(7));
    assert_eq!(original.fallback_count, 0, "skipped providers are not outgoing attempts");
    assert_eq!(replacement.resolved_model.as_deref(), Some("replacement"));
    assert_eq!(replacement.config_generation, Some(8));
    assert_ne!(original.request_id, replacement.request_id);
    let retained = crate::proxy::request_status::snapshot().into_iter()
        .find(|row| row.request_id == original.request_id).unwrap();
    assert_eq!(retained.resolved_model, original.resolved_model);
    assert_eq!(retained.config_generation, Some(7));
}

#[tokio::test]
async fn live_status_concurrent_requests_keep_their_own_targets() {
    let (first, second) = tokio::join!(
        live_mapping_result("strong", 71, true),
        live_mapping_result("replacement", 72, true),
    );
    assert_ne!(first.request_id, second.request_id);
    assert_eq!(first.resolved_model.as_deref(), Some("strong"));
    assert_eq!(second.resolved_model.as_deref(), Some("replacement"));
    assert_eq!(first.phase, "completed");
    assert_eq!(second.phase, "completed");
}

#[tokio::test]
async fn actual_fallback_trace_records_selected_tier_and_basis() {
    let event = timeline_result(true, false).await;
    assert_eq!(event.plan_tier, Some(2));
    assert_eq!(event.plan_downgraded, Some(true));
    assert_eq!(event.plan_basis.as_deref(), Some("public_metadata"));
    assert_eq!(event.plan_reason.as_deref(), Some("tier_downgrade"));
}

#[tokio::test]
async fn stream_trace_uses_captured_request_metadata_after_handler_task_scope() {
    let event = timeline_result(true, true).await;
    assert_eq!(event.plan_tier, Some(2));
    assert_eq!(event.plan_downgraded, Some(true));
    assert_eq!(event.plan_basis.as_deref(), Some("public_metadata"));
    assert_eq!(event.plan_reason.as_deref(), Some("tier_downgrade"));
    assert!(event.streamed);
}

fn mirror_fixture(status: u16) -> (Router, Arc<Fixture_provider>, Arc<Fixture_provider>) {
    let mut first = Fixture_provider::new("mini", false);
    Arc::get_mut(&mut first).unwrap().models = vec!["MiniMax-M3.1-Flash".into()];
    first.status.store(status, Ordering::SeqCst);
    let mut second = Fixture_provider::new("backup", false);
    Arc::get_mut(&mut second).unwrap().models = vec!["configured-backup".into()];
    let roles = [SURFACE_CLAUDE, SURFACE_CODEX]
        .into_iter()
        .map(|surface| {
            (
                surface.into(),
                HashMap::from([(
                    if surface == SURFACE_CLAUDE {
                        "claude-fable-5"
                    } else {
                        "gpt-5.6-sol"
                    }
                    .into(),
                    vec![
                        hop("mini", "MiniMax-M3.1-Flash"),
                        hop("backup", "configured-backup"),
                    ],
                )]),
            )
        })
        .collect();
    (
        router(&[first.clone(), second.clone()], HashMap::new())
            .with_mirror_role_routes(true, roles),
        first,
        second,
    )
}

#[tokio::test]
async fn mirror_roles_forward_actual_models_and_fall_through_quota_errors() {
    for (surface, selector, stream) in [
        (SURFACE_CLAUDE, "claude-fable-5", false),
        (SURFACE_CODEX, "gpt-5.6-sol", true),
    ] {
        let (router, first, second) = mirror_fixture(429);
        if stream {
            let chunks = router
                .stream_for(surface, "fixture", &request(selector))
                .await
                .unwrap()
                .collect::<Vec<_>>()
                .await;
            assert_eq!(chunks[0].model, "configured-backup");
        } else {
            let response = router
                .complete_for(surface, "fixture", &request(selector))
                .await
                .unwrap();
            assert_eq!(response.model, "configured-backup");
        }
        assert_eq!(first.calls(), ["MiniMax-M3.1-Flash"]);
        assert_eq!(second.calls(), ["configured-backup"]);
        assert_eq!(
            router.mirror_role_models(surface, Some("backup")),
            [selector]
        );
        assert!(router
            .mirror_role_models(surface, Some("missing"))
            .is_empty());
        assert!(router
            .credentialless_models(surface, None)
            .contains(&selector.into()));
    }
}

#[tokio::test]
async fn mirror_roles_do_not_replay_authentication_failures_or_bypass_pins() {
    let (router, first, second) = mirror_fixture(401);
    assert_eq!(
        router
            .complete_for(SURFACE_CLAUDE, "fixture", &request("claude-fable-5"))
            .await
            .unwrap_err()
            .status,
        Some(401)
    );
    assert_eq!(first.calls(), ["MiniMax-M3.1-Flash"]);
    assert!(second.calls().is_empty());
    let response = router
        .complete_with_provider(SURFACE_CODEX, "fixture", "backup", &request("gpt-5.6-sol"))
        .await
        .unwrap();
    assert_eq!(response.model, "configured-backup");
    assert_eq!(first.calls().len(), 1);
    assert_eq!(
        router
            .complete_with_provider(SURFACE_CODEX, "fixture", "missing", &request("gpt-5.6-sol"))
            .await
            .unwrap_err()
            .code,
        "pinned_provider_ineligible"
    );
}

#[tokio::test]
async fn mirror_roles_never_substitute_an_actually_offered_selector_or_precise_call() {
    let (mut router, first, second) = mirror_fixture(0);
    let mut literal = Fixture_provider::new("literal-role", false);
    Arc::get_mut(&mut literal).unwrap().models = vec!["gpt-5.6-sol".into()];
    router
        .providers
        .insert("literal-role".into(), literal.clone());
    for selector in ["gpt-5.6-sol", "precise:gpt-5.6-sol"] {
        assert_eq!(
            router
                .complete_for(SURFACE_CODEX, "fixture", &request(selector))
                .await
                .unwrap()
                .model,
            "gpt-5.6-sol"
        );
    }
    assert_eq!(literal.calls(), ["gpt-5.6-sol", "gpt-5.6-sol"]);
    assert!(first.calls().is_empty());
    assert!(second.calls().is_empty());
}

#[test]
fn mirror_role_admission_drops_disabled_unoffered_and_client_credential_supply() {
    let mut disabled = Fixture_provider::new("disabled", false);
    Arc::get_mut(&mut disabled).unwrap().enabled = false;
    let mut oauth = Fixture_provider::new("oauth", false);
    Arc::get_mut(&mut oauth).unwrap().credential_required = true;
    let configured = Fixture_provider::new("configured", false);
    let router = router(&[disabled, oauth, configured], HashMap::new()).with_mirror_role_routes(
        true,
        HashMap::from([(
            SURFACE_CODEX.into(),
            HashMap::from([(
                "gpt-5.6-sol".into(),
                vec![
                    hop("disabled", "m"),
                    hop("oauth", "m"),
                    hop("configured", "invented"),
                ],
            )]),
        )]),
    );
    assert!(router.mirror_roles_enabled());
    assert!(router.mirror_role_models(SURFACE_CODEX, None).is_empty());
    assert_eq!(
        router
            .resolve_request(SURFACE_CODEX, &request("gpt-5.6-sol"))
            .unwrap_err()
            .code,
        "mirror_role_supply_unavailable"
    );
}

#[tokio::test]
async fn mirror_role_without_supply_degrades_to_next_lower_tier_role() {
    // fable has no admitted supply; sonnet still does. The request for the
    // top role must be served by the sonnet supply and flagged degraded.
    let mut sonnet_provider = Fixture_provider::new("sonnet_supply", false);
    Arc::get_mut(&mut sonnet_provider).unwrap().models = vec!["balanced-model".into()];
    let roles = HashMap::from([(
        SURFACE_CLAUDE.into(),
        HashMap::from([
            ("claude-fable-5".to_string(), Vec::new()),
            (
                "claude-sonnet-5".to_string(),
                vec![hop("sonnet_supply", "balanced-model")],
            ),
        ]),
    )]);
    let degraded_router = router(&[sonnet_provider.clone()], HashMap::new())
        .with_mirror_role_routes(true, roles);
    let resolved = degraded_router
        .resolve_request(SURFACE_CLAUDE, &request("claude-fable-5"))
        .expect("degraded resolution succeeds");
    assert_eq!(resolved.first().unwrap().model, "balanced-model");
    let completed = degraded_router
        .complete_for(SURFACE_CLAUDE, "fixture", &request("claude-fable-5"))
        .await
        .unwrap();
    assert_eq!(completed.model, "balanced-model");
    assert_eq!(sonnet_provider.calls(), ["balanced-model"]);
    // The whole family empty still fails closed.
    let empty = HashMap::from([(
        SURFACE_CLAUDE.into(),
        HashMap::from([
            ("claude-fable-5".to_string(), Vec::new()),
            ("claude-sonnet-5".to_string(), Vec::new()),
        ]),
    )]);
    let empty_router = router(&[], HashMap::new()).with_mirror_role_routes(true, empty);
    assert_eq!(
        empty_router
            .resolve_request(SURFACE_CLAUDE, &request("claude-fable-5"))
            .unwrap_err()
            .code,
        "mirror_role_supply_unavailable"
    );
}

fn assessed_gateway(models: Vec<Value>) -> Router {
    use crate::proxy::providers::mock::MockProvider;
    use crate::services::suppliers::{save_supplier, SupplierSaveInput};
    let db = crate::database::Database::memory().unwrap();
    let input: SupplierSaveInput = serde_json::from_value(json!({
        "expectedGeneration":0,"name":"assessed fixture","baseUrl":"https://example.invalid","apiKey":"fixture",
        "endpoints":[{"protocol":"openai-chat","models":models}]
    })).unwrap();
    save_supplier(&db, input).unwrap();
    db.set_setting(crate::services::mirror_models::KEY_ENABLED, "1")
        .unwrap();
    let rows = db.get_all_proxy_providers().unwrap();
    let providers: HashMap<String, Arc<dyn Provider>> = rows
        .iter()
        .map(|row| {
            (
                row.id.clone(),
                Arc::new(MockProvider::new(
                    row.id.clone(),
                    row.models.clone(),
                    row.enabled,
                    CircuitBreakerConfig::default(),
                )) as Arc<dyn Provider>,
            )
        })
        .collect();
    let planner = crate::services::proxy_runtime::planner::build_model_planner(&db, &rows).unwrap();
    let (enabled, roles) =
        crate::services::mirror_models::build_mirror_role_routes(&db, &providers, planner.as_ref())
            .unwrap();
    Router::new(providers, HashMap::new())
        .with_planner(planner)
        .with_mirror_role_routes(enabled, roles)
}

fn assessed_model(id: &str, tier: Option<u8>) -> Value {
    json!({"modelId":id,"tierOverride":tier,"supportsTools":true,"supportsImages":true,
        "contextWindow":200_000,"maxOutputTokens":64_000})
}

#[tokio::test]
async fn all_canonical_roles_route_by_assessed_tier_on_every_public_surface() {
    let router = assessed_gateway(vec![
        assessed_model("highest", Some(3)),
        assessed_model("high", Some(2)),
        assessed_model("balanced", Some(1)),
        assessed_model("fast", Some(0)),
    ]);
    for surface in [SURFACE_CLAUDE, SURFACE_CODEX, SURFACE_CHAT, SURFACE_GEMINI] {
        for (selector, model) in [
            ("claude-fable-5", "highest"),
            ("claude-opus-5", "high"),
            ("claude-sonnet-5", "balanced"),
            ("claude-haiku-5-5", "fast"),
            ("gpt-5.6-sol", "highest"),
            ("gpt-5.6-terra", "balanced"),
            ("gpt-5.6-luna", "fast"),
        ] {
            assert_eq!(
                router
                    .complete_for(surface, "fixture", &request(selector))
                    .await
                    .unwrap()
                    .model,
                model
            );
            assert!(router
                .catalog_models(surface, None)
                .contains(&selector.to_string()));
        }
    }
    let request = request("claude-fable-5");
    let hops = router.resolve_request(SURFACE_CLAUDE, &request).unwrap();
    let head = router.serving_plan(&request, &hops[0]);
    let fallback = router.serving_plan(&request, hops.last().unwrap());
    assert_eq!(head.tier, Some(3));
    assert_eq!(head.basis.as_deref(), Some("manual_override"));
    assert_eq!(head.downgraded, Some(false));
    // Preferred role ordering does not remove the other eligible fallbacks.
    assert_eq!(hops.len(), 4);
    assert_eq!(fallback.tier, Some(0));
    assert_eq!(fallback.downgraded, Some(true));
    assert_eq!(router.mirror_role_models(SURFACE_CLAUDE, None).len(), 4);
    assert_eq!(router.mirror_role_models(SURFACE_CODEX, None).len(), 3);
}

#[test]
fn ranked_roles_fail_closed_for_unranked_or_incapable_supply_and_discovery_matches() {
    let unranked = assessed_gateway(vec![assessed_model("unranked", None)]);
    for surface in [SURFACE_CLAUDE, SURFACE_CODEX, SURFACE_CHAT, SURFACE_GEMINI] {
        assert_eq!(
            unranked
                .resolve_request(surface, &request("gpt-5.6-sol"))
                .unwrap_err()
                .code,
            "model_alias_no_candidates"
        );
        assert_eq!(unranked.catalog_models(surface, None), ["unranked"]);
    }
    let mut limited = assessed_model("strongest", Some(3));
    limited["supportsImages"] = false.into();
    let capable = assessed_model("balanced", Some(1));
    let router = assessed_gateway(vec![limited, capable]);
    let mut image_request = request("claude-fable-5");
    image_request.messages[0].content =
        json!([{"type":"image_url","image_url":{"url":"data:image/png;base64,fixture"}}]);
    let hops = router
        .resolve_request(SURFACE_GEMINI, &image_request)
        .unwrap();
    assert_eq!(
        hops.iter()
            .map(|hop| hop.model.as_str())
            .collect::<Vec<_>>(),
        ["balanced"]
    );
    assert!(router
        .catalog_models(SURFACE_GEMINI, Some("unrelated-pin"))
        .is_empty());
    let offered = Router::new(router.providers.clone(), HashMap::new());
    assert!(
        offered
            .catalog_models(SURFACE_CHAT, None)
            .contains(&"balanced".into()),
        "raw offers are discoverable without a route row"
    );
    assert!(
        !offered
            .catalog_models(SURFACE_CHAT, None)
            .contains(&"fable".into()),
        "no planner means no invented capability alias"
    );
}

struct ProtocolPriorityPlanner {
    hops: Vec<Planned_hop>,
}

impl Model_planner for ProtocolPriorityPlanner {
    fn resolve(&self, raw: &str, _features: Request_features) -> Result<Resolution, ProviderError> {
        Ok(Resolution::Alias {
            alias: raw.into(),
            tier: 3,
            hops: self.hops.clone(),
        })
    }
}

#[test]
fn native_protocol_preference_only_reorders_same_tier_priority_ties() {
    let mut responses = Fixture_provider::new("responses", false);
    Arc::get_mut(&mut responses).unwrap().wire_protocol = "openai-responses";
    let mut gemini = Fixture_provider::new("gemini", false);
    Arc::get_mut(&mut gemini).unwrap().wire_protocol = "gemini";
    let mut anthropic = Fixture_provider::new("anthropic", false);
    Arc::get_mut(&mut anthropic).unwrap().wire_protocol = "anthropic";
    let hops = vec![
        planned("responses", "m", 3, false),
        planned("gemini", "m", 3, false),
        planned("anthropic", "m", 3, false),
    ];
    let providers = [responses, gemini, anthropic];
    let make_router = |hops, routes| {
        router(&providers, routes).with_planner(Arc::new(ProtocolPriorityPlanner { hops }))
    };
    let ranked = make_router(hops.clone(), HashMap::new());
    for (surface, expected) in [
        (SURFACE_CLAUDE, "anthropic"),
        (SURFACE_CODEX, "responses"),
        (SURFACE_GEMINI, "gemini"),
    ] {
        assert_eq!(
            ranked
                .resolve_request(surface, &request("claude-fable-5"))
                .unwrap()[0]
                .provider,
            expected
        );
    }
    let mut prioritized = hops.clone();
    prioritized[0].priority = -1;
    let priority_router = make_router(prioritized, HashMap::new());
    assert_eq!(
        priority_router
            .resolve_request(SURFACE_CLAUDE, &request("claude-fable-5"))
            .unwrap()[0]
            .provider,
        "responses"
    );
    let mut tiered = hops.clone();
    tiered[1].tier = Some(2);
    tiered[2].tier = Some(2);
    let tier_router = make_router(tiered, HashMap::new());
    assert_eq!(
        tier_router
            .resolve_request(SURFACE_CLAUDE, &request("claude-fable-5"))
            .unwrap()[0]
            .provider,
        "responses"
    );
    let manual = make_router(
        hops,
        HashMap::from([(
            "claude-fable-5".into(),
            vec![hop("gemini", "m"), hop("anthropic", "m")],
        )]),
    );
    assert_eq!(
        manual
            .resolve_request(SURFACE_CLAUDE, &request("claude-fable-5"))
            .unwrap()[0]
            .provider,
        "gemini"
    );
}

fn quota_fixture(status: u16, body: Option<&'static str>) -> (Router, [Arc<Fixture_provider>; 3]) {
    let mut anthropic = Fixture_provider::new("quota-anthropic", false);
    Arc::get_mut(&mut anthropic).unwrap().error_body = body;
    Arc::get_mut(&mut anthropic).unwrap().wire_protocol = "anthropic";
    anthropic.status.store(status, Ordering::SeqCst);
    let mut responses = Fixture_provider::new("quota-responses", false);
    Arc::get_mut(&mut responses).unwrap().wire_protocol = "openai-responses";
    let independent = Fixture_provider::new("independent", false);
    let providers = [anthropic, responses, independent];
    let routes = HashMap::from([(
        "quota-role".into(),
        vec![
            hop("quota-anthropic", "m"),
            hop("quota-responses", "m"),
            hop("quota-anthropic", "other"),
            hop("independent", "m"),
        ],
    )]);
    let router = router(&providers, routes).with_quota_scopes(HashMap::from([
        ("quota-anthropic".into(), 0),
        ("quota-responses".into(), 0),
        ("independent".into(), 1),
    ]));
    (router, providers)
}

#[tokio::test]
async fn quota_failure_skips_other_protocols_and_models_on_the_same_resource() {
    for (status, body) in [
        (429, Some(r#"{"error":{"code":"usage_limit_reached"}}"#)),
        (400, Some(r#"{"base_resp":{"status_code":2056}}"#)),
        (403, Some(r#"{"base_resp":{"status_code":2067}}"#)),
    ] {
        for operation in ["complete", "stream", "count"] {
            let (router, providers) = quota_fixture(status, body);
            match operation {
                "complete" => {
                    router
                        .complete_for(SURFACE_CLAUDE, "fixture", &request("quota-role"))
                        .await
                        .unwrap();
                }
                "stream" => {
                    router
                        .stream_for(SURFACE_CODEX, "fixture", &request("quota-role"))
                        .await
                        .unwrap()
                        .collect::<Vec<_>>()
                        .await;
                }
                _ => {
                    assert_eq!(
                        router
                            .count_tokens_for(
                                SURFACE_CLAUDE,
                                "quota-role",
                                &json!({"model":"quota-role"}),
                                None
                            )
                            .await
                            .unwrap(),
                        4
                    );
                }
            }
            assert_eq!(
                providers[0].calls(),
                ["m"],
                "one failed account attempt per request"
            );
            assert!(
                providers[1].calls().is_empty(),
                "another protocol does not provide new allowance"
            );
            assert_eq!(providers[2].calls(), ["m"]);
        }
    }
}

#[tokio::test]
async fn quota_resource_state_is_request_local_and_network_failures_keep_wire_fallback() {
    let (quota, providers) = quota_fixture(429, Some(r#"{"base_resp":{"status_code":2056}}"#));
    for _ in 0..2 {
        quota
            .complete_for(SURFACE_CLAUDE, "fixture", &request("quota-role"))
            .await
            .unwrap();
    }
    assert_eq!(
        providers[0].calls(),
        ["m", "m"],
        "no invented reset timer or persistent quota lock"
    );
    assert!(providers[1].calls().is_empty());
    assert_eq!(providers[2].calls().len(), 2);
    let (network, providers) = quota_fixture(503, None);
    network
        .complete_for(SURFACE_CLAUDE, "fixture", &request("quota-role"))
        .await
        .unwrap();
    assert_eq!(providers[0].calls(), ["m"]);
    assert_eq!(
        providers[1].calls(),
        ["m"],
        "transport failure may use another protocol"
    );
    assert!(providers[2].calls().is_empty());
    let (tool_failure, providers) = quota_fixture(
        400,
        Some(r#"{"error":{"message":"unsupported tool schema"}}"#),
    );
    let mut tool_request = request("quota-role");
    tool_request.tools = Some(vec![
        json!({"type":"function","function":{"name":"fixture","parameters":{"type":"object"}}}),
    ]);
    tool_failure
        .complete_for(SURFACE_CLAUDE, "fixture", &tool_request)
        .await
        .unwrap();
    assert_eq!(providers[0].calls(), ["m"]);
    assert_eq!(
        providers[1].calls(),
        ["m"],
        "a tool mismatch does not exhaust account quota"
    );
    assert!(providers[2].calls().is_empty());
}

#[tokio::test]
async fn model_rate_limits_keep_another_model_on_the_same_account_eligible() {
    for body in [
        None,
        Some(
            r#"{"error":{"type":"rate_limit_error","code":"rate_limit_exceeded","metric":"tokens_per_minute","model":"m"}}"#,
        ),
        Some(r#"{"error":{"code":"quota_exceeded","model":"m"}}"#),
        Some(r#"{"base_resp":{"status_code":2062}}"#),
    ] {
        for operation in ["complete", "stream", "count"] {
            let (mut router, providers) = quota_fixture(429, body);
            router.routes.insert(
                "quota-role".into(),
                Arc::new(vec![
                    hop("quota-anthropic", "m"),
                    hop("quota-responses", "other"),
                    hop("independent", "m"),
                ]),
            );
            match operation {
                "complete" => {
                    router
                        .complete_for(SURFACE_CLAUDE, "fixture", &request("quota-role"))
                        .await
                        .unwrap();
                }
                "stream" => {
                    router
                        .stream_for(SURFACE_CODEX, "fixture", &request("quota-role"))
                        .await
                        .unwrap()
                        .collect::<Vec<_>>()
                        .await;
                }
                _ => {
                    assert_eq!(
                        router
                            .count_tokens_for(
                                SURFACE_CLAUDE,
                                "quota-role",
                                &json!({"model":"quota-role"}),
                                None
                            )
                            .await
                            .unwrap(),
                        4
                    );
                }
            }
            assert_eq!(providers[0].calls(), ["m"]);
            assert_eq!(
                providers[1].calls(),
                ["other"],
                "a model rate limit does not establish an exhausted account"
            );
            assert!(providers[2].calls().is_empty());
        }
    }
}

#[tokio::test]
async fn streamed_model_rate_limits_preserve_same_account_fallback_only_before_output() {
    for after_output in [false, true] {
        let (mut router, mut providers) = quota_fixture(0, None);
        let mut first = Fixture_provider::new("quota-anthropic", false);
        Arc::get_mut(&mut first).unwrap().stream_error_body =
            Some(json!({"type":"rate_limit_error", "code":"rate_limit_exceeded"}));
        Arc::get_mut(&mut first).unwrap().stream_error_after_output = after_output;
        providers[0] = first.clone();
        router.providers.insert("quota-anthropic".into(), first);
        router.routes.insert(
            "quota-role".into(),
            Arc::new(vec![
                hop("quota-anthropic", "m"),
                hop("quota-responses", "other"),
                hop("independent", "m"),
            ]),
        );
        let chunks = router
            .stream_for(SURFACE_GEMINI, "fixture", &request("quota-role"))
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;
        assert_eq!(providers[0].calls(), ["m"]);
        if after_output {
            assert!(providers[1].calls().is_empty());
            assert_eq!(
                canonical::stream_chunk_error(chunks.last().unwrap())
                    .unwrap()
                    .kind,
                Some(canonical::UpstreamErrorKind::RateLimited)
            );
        } else {
            assert_eq!(providers[1].calls(), ["other"]);
            assert!(chunks
                .iter()
                .all(|chunk| canonical::stream_chunk_error(chunk).is_none()));
        }
        assert!(providers[2].calls().is_empty());
    }
}

#[tokio::test]
async fn quota_scope_does_not_make_authentication_or_invalid_requests_retriable() {
    for (status, body) in [
        (401, r#"{"base_resp":{"status_code":2056}}"#),
        (
            403,
            r#"{"error":{"type":"authentication_error","code":"usage_limit_reached"}}"#,
        ),
        (400, r#"{"base_resp":{"status_code":2013}}"#),
    ] {
        let (router, providers) = quota_fixture(status, Some(body));
        assert_eq!(
            router
                .complete_for(SURFACE_CLAUDE, "fixture", &request("quota-role"))
                .await
                .unwrap_err()
                .status,
            Some(status)
        );
        assert_eq!(providers[0].calls(), ["m"]);
        assert!(providers[1].calls().is_empty());
        assert!(providers[2].calls().is_empty());
    }
}

#[tokio::test]
async fn streamed_quota_falls_back_only_before_output_is_committed() {
    for after_output in [false, true] {
        let (mut router, mut providers) = quota_fixture(0, None);
        // Rebuild with the fixture's queued stream error before sharing it.
        let mut first = Fixture_provider::new("quota-anthropic", false);
        Arc::get_mut(&mut first).unwrap().stream_error_body =
            Some(json!({"base_resp":{"status_code":2056}}));
        Arc::get_mut(&mut first).unwrap().stream_error_after_output = after_output;
        providers[0] = first.clone();
        router.providers.insert("quota-anthropic".into(), first);
        let chunks = router
            .stream_for(SURFACE_CLAUDE, "fixture", &request("quota-role"))
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;
        assert_eq!(providers[0].calls(), ["m"]);
        assert!(providers[1].calls().is_empty());
        if after_output {
            assert_eq!(chunks.len(), 2);
            assert_eq!(
                canonical::stream_chunk_error(chunks.last().unwrap())
                    .unwrap()
                    .kind,
                Some(canonical::UpstreamErrorKind::QuotaExceeded)
            );
            assert!(
                providers[2].calls().is_empty(),
                "already emitted output must never be replayed"
            );
        } else {
            assert_eq!(providers[2].calls(), ["m"]);
            assert!(chunks
                .iter()
                .all(|chunk| canonical::stream_chunk_error(chunk).is_none()));
        }
    }
}
