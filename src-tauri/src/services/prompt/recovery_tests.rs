use super::*;
use serde_json::json;

#[test]
fn continuation_preserves_content_and_deduplicates_unicode_overlap() {
    assert_eq!(recovery::append_continuation("map", "ping"), "mapping");
    assert_eq!(recovery::append_continuation("foo", "orange"), "fooorange");
    assert_eq!(
        recovery::append_continuation("Build the parser. 验证输入", "验证输入 and test errors."),
        "Build the parser. 验证输入 and test errors."
    );
    assert_eq!(
        recovery::append_continuation("Build it.", "Build it."),
        "Build it."
    );
    assert_eq!(
        recovery::append_continuation("Build ", "the parser."),
        "Build the parser."
    );
    assert_eq!(
        recovery::append_continuation("First section.", "\n\nSecond section."),
        "First section.\n\nSecond section."
    );
}

fn budget_request() -> ChatCompletionRequest {
    serde_json::from_value(json!({"model":"route", "messages":[{"role":"user","content":"Implement the parser."}],"max_tokens":8192})).unwrap()
}

fn supplier_with_limits(db: &Database, id: &str, context: u64, output: u64) -> String {
    supplier_with_optional_limits(db, id, Some(context), Some(output))
}

fn supplier_with_optional_limits(
    db: &Database,
    id: &str,
    context: Option<u64>,
    output: Option<u64>,
) -> String {
    use crate::services::suppliers::{
        resolved_supplier_snapshot, save_supplier, SupplierSaveInput,
    };
    let input: SupplierSaveInput = serde_json::from_value(json!({
        "id":id,"expectedGeneration":crate::services::suppliers::supplier_config_generation(db).unwrap(),
        "name":id,"baseUrl":"https://fixture.example.invalid/v1","apiKey":"fixture-key",
        "endpoints":[{"protocol":"openai-chat","models":[{"modelId":"same-model","contextWindow":context,"maxOutputTokens":output}]}]
    })).unwrap();
    save_supplier(db, input).unwrap();
    resolved_supplier_snapshot(db)
        .unwrap()
        .endpoints
        .into_iter()
        .find(|endpoint| endpoint.supplier_id == id)
        .unwrap()
        .legacy_proxy_provider_id
        .unwrap()
}

#[test]
fn configured_limits_override_catalog_and_missing_fields_use_catalog_limits() {
    use crate::services::suppliers::{save_public_catalog_cache, PublicCatalogCacheInput};
    let db = Database::memory().unwrap();
    save_public_catalog_cache(&db, PublicCatalogCacheInput {
        source: "models_dev".into(),
        raw_json: json!({"fixture":{"models":{"same-model":{"id":"same-model","limit":{"context":8192,"output":1024}}}}}).to_string(),
        fetched_at: chrono::Utc::now().timestamp(),
    }).unwrap();
    let explicit = supplier_with_limits(&db, "explicit", 200_000, 8192);
    let unknown = supplier_with_optional_limits(&db, "unknown", None, None);
    let mixed = supplier_with_optional_limits(&db, "mixed", Some(200_000), None);
    let mut long_request = budget_request();
    long_request.messages[0].content = json!("x".repeat(30_000));
    let target = |provider_id| target::Prompt_target::Provider {
        provider_id,
        model: "same-model".into(),
    };
    assert_eq!(
        recovery::output_budget(&db, None, &target(explicit), &long_request, 8192).unwrap(),
        8192
    );
    assert_eq!(
        recovery::output_budget(&db, None, &target(mixed), &long_request, 8192).unwrap(),
        1024
    );
    assert_eq!(
        recovery::output_budget(&db, None, &target(unknown.clone()), &budget_request(), 8192)
            .unwrap(),
        1024
    );
    assert_eq!(
        recovery::output_budget(&db, None, &target(unknown), &long_request, 8192).unwrap_err(),
        "prompt_enhance_context_budget_exhausted"
    );
}

#[test]
fn capability_budget_is_scoped_to_provider_and_tiny_fallback_does_not_block_primary() {
    use crate::database::dao::routes::RouteHop;
    use std::collections::HashMap;
    let db = Database::memory().unwrap();
    let large = supplier_with_limits(&db, "large", 128_000, 16_000);
    let small = supplier_with_limits(&db, "small", 512, 128);
    let direct = target::Prompt_target::Provider {
        provider_id: large.clone(),
        model: "same-model".into(),
    };
    assert_eq!(
        recovery::output_budget(&db, None, &direct, &budget_request(), 8192).unwrap(),
        8192
    );
    let direct_small = target::Prompt_target::Provider {
        provider_id: small.clone(),
        model: "same-model".into(),
    };
    assert_eq!(
        recovery::output_budget(&db, None, &direct_small, &budget_request(), 8192).unwrap_err(),
        "prompt_enhance_context_budget_exhausted"
    );
    let router = Arc::new(Router::new(
        HashMap::new(),
        HashMap::from([(
            "route".into(),
            vec![
                RouteHop {
                    provider: large,
                    model: "same-model".into(),
                },
                RouteHop {
                    provider: small,
                    model: "same-model".into(),
                },
            ],
        )]),
    ));
    assert_eq!(
        recovery::output_budget(
            &db,
            Some(&router),
            &target::Prompt_target::Gateway {
                model: "route".into()
            },
            &budget_request(),
            8192
        )
        .unwrap(),
        8192
    );
}

#[test]
fn output_budget_reserves_the_same_context_margin_as_router_admission() {
    let db = Database::memory().unwrap();
    let provider = supplier_with_limits(&db, "bounded", 4096, 4096);
    let target = target::Prompt_target::Provider {
        provider_id: provider,
        model: "same-model".into(),
    };
    let mut request = budget_request();
    request.max_tokens = Some(recovery::output_budget(&db, None, &target, &request, 8192).unwrap());
    assert!(
        crate::proxy::planner::request_features(&request)
            .min_context_window
            .unwrap()
            <= 4096
    );
    assert!(request.max_tokens.unwrap() >= MIN_TOKENS);
}

#[tokio::test]
async fn exhausted_generation_resumes_only_missing_text_and_reports_total_usage() {
    use axum::{extract::Json, routing::post, Router as HttpRouter};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured = Arc::clone(&observed);
    let upstream = tokio::spawn(async move {
        axum::serve(listener, HttpRouter::new().route("/v1/chat/completions", post(move |Json(body): Json<Value>| {
            let captured = Arc::clone(&captured);
            async move {
                let mut requests = captured.lock().unwrap(); requests.push(body);
                let first = requests.len() == 1;
                Json(json!({"id":"fixture","object":"chat.completion","created":0,"model":"fixture-model",
                    "choices":[{"index":0,"finish_reason":if first {"length"} else {"stop"},"message":{"role":"assistant","content":if first {"Implement the parser. Validate inputs"} else {"Validate inputs and test errors."}}}],
                    "usage":{"prompt_tokens":100,"completion_tokens":50,"total_tokens":150}}))
            }
        }))).await.unwrap();
    });
    let db = Database::memory().unwrap();
    let mut provider = tests::mock_provider_row();
    provider.provider_type = "openai".into();
    provider.base_url = base_url;
    db.save_proxy_provider(&provider).unwrap();
    let mut request = tests::gateway_request(
        "en_optimize",
        "Implement a parser with input validation and tests.",
    );
    request.target = target::Prompt_target::Provider {
        provider_id: provider.id,
        model: "mock-model".into(),
    };
    let output = run_stage(&db, None, request).await.unwrap();
    assert!(output.complete);
    assert_eq!(output.attempts, 2);
    assert_eq!(
        output.output,
        "Implement the parser. Validate inputs and test errors."
    );
    assert_eq!(output.usage.unwrap().total_tokens, 300);
    let requests = observed.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1]["messages"][2]["role"], "assistant");
    assert!(requests[1]["messages"][3]["content"]
        .as_str()
        .unwrap()
        .contains("only the missing continuation"));
    drop(requests);
    upstream.abort();
    let _ = upstream.await;
}

#[tokio::test]
async fn recovery_exhaustion_returns_partial_content_without_claiming_completion() {
    use axum::{extract::Json, routing::post, Router as HttpRouter};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        axum::serve(listener, HttpRouter::new().route("/v1/chat/completions", post(|Json(_): Json<Value>| async {
            Json(json!({"id":"fixture","object":"chat.completion","created":0,"model":"fixture-model",
                "choices":[{"index":0,"finish_reason":"length","message":{"role":"assistant","content":"Preserved partial section."}}]}))
        }))).await.unwrap();
    });
    let db = Database::memory().unwrap();
    let mut provider = tests::mock_provider_row();
    provider.provider_type = "openai".into();
    provider.base_url = base_url;
    db.save_proxy_provider(&provider).unwrap();
    let mut request = tests::gateway_request("en_optimize", "Implement a parser.");
    request.target = target::Prompt_target::Provider {
        provider_id: provider.id,
        model: "mock-model".into(),
    };
    let output = run_stage(&db, None, request).await.unwrap();
    assert!(!output.complete);
    assert!(output.attempts <= 3);
    assert_eq!(output.output, "Preserved partial section.");
    assert_eq!(
        output.failure_code.as_deref(),
        Some("prompt_enhance_continuation_stalled")
    );
    upstream.abort();
    let _ = upstream.await;
}

#[tokio::test]
async fn stalled_gateway_continuation_uses_configured_hop_despite_upstream_model_labels() {
    use crate::database::dao::routes::RouteHop;
    use axum::{extract::Json, routing::post, Router as HttpRouter};
    use std::collections::HashMap;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured = Arc::clone(&observed);
    let upstream = tokio::spawn(async move {
        axum::serve(listener, HttpRouter::new().route("/v1/chat/completions", post(move |Json(body): Json<Value>| {
            let captured = Arc::clone(&captured);
            async move {
                let model = body["model"].as_str().unwrap(); captured.lock().unwrap().push(body.clone());
                let completes = model == "alternate";
                Json(json!({"id":"fixture","object":"chat.completion","created":0,"model":format!("{model}-2026-10-28"),
                    "choices":[{"index":0,"finish_reason":if completes {"stop"} else {"length"},"message":{"role":"assistant","content":if completes {"Preserve input and report errors."} else {"Implement parser. Preserve input"}}}]}))
            }
        }))).await.unwrap();
    });
    let db = Database::memory().unwrap();
    let mut provider = tests::mock_provider_row();
    provider.provider_type = "openai".into();
    provider.base_url = base_url;
    provider.models = vec!["preferred".into(), "alternate".into()];
    db.save_proxy_provider(&provider).unwrap();
    let runtime: Arc<dyn crate::proxy::providers::Provider> =
        Arc::from(crate::proxy::providers::build_provider(&provider).unwrap());
    let router = Arc::new(Router::new(
        HashMap::from([(provider.id.clone(), runtime)]),
        HashMap::from([(
            "prompt-route".into(),
            vec![
                RouteHop {
                    provider: provider.id.clone(),
                    model: "preferred".into(),
                },
                RouteHop {
                    provider: provider.id.clone(),
                    model: "alternate".into(),
                },
            ],
        )]),
    ));
    let output = run_stage(
        &db,
        Some(router.clone()),
        tests::gateway_request("zh_expand", "Build a parser."),
    )
    .await
    .unwrap();
    assert!(output.complete);
    assert_eq!(output.attempts, 3);
    assert_eq!(
        output.output,
        "Implement parser. Preserve input and report errors."
    );
    assert_eq!(output.model, "alternate-2026-10-28");
    {
        let calls = observed.lock().unwrap();
        assert_eq!(
            calls
                .iter()
                .map(|body| body["model"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["preferred", "preferred", "alternate"]
        );
        assert_eq!(
            calls[2]["messages"][2]["content"],
            "Implement parser. Preserve input"
        );
    }
    for target in [
        target::Prompt_target::Gateway {
            model: "precise:preferred".into(),
        },
        target::Prompt_target::Provider {
            provider_id: provider.id,
            model: "preferred".into(),
        },
    ] {
        let mut request = tests::gateway_request("zh_expand", "Build a parser.");
        request.target = target;
        let output = run_stage(&db, Some(router.clone()), request).await.unwrap();
        assert!(!output.complete);
        assert_eq!(output.attempts, 2);
        assert_eq!(
            output.failure_code.as_deref(),
            Some("prompt_enhance_continuation_stalled")
        );
    }
    assert_eq!(observed.lock().unwrap().len(), 7);
    assert_eq!(
        router
            .preview_model(
                crate::proxy::router::SURFACE_CHAT,
                "prompt-route",
                Default::default(),
                None
            )
            .unwrap()[0]
            .model,
        "preferred"
    );
    upstream.abort();
    let _ = upstream.await;
}

#[tokio::test]
async fn concurrent_prompt_dispatches_keep_request_local_serving_hops() {
    use crate::database::dao::routes::RouteHop;
    use axum::{extract::Json, routing::post, Router as HttpRouter};
    use std::collections::HashMap;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let upstream = tokio::spawn(async move {
        let app = HttpRouter::new().route("/v1/chat/completions",post(move |Json(body):Json<Value>| {
            let barrier = barrier.clone();
            async move {
                barrier.wait().await;
                Json(json!({"id":"fixture","object":"chat.completion","created":0,
                    "model":format!("{}-dated-upstream-label",body["model"].as_str().unwrap()),
                    "choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":"Complete prompt."}}]}))
            }
        }));
        axum::serve(listener, app).await.unwrap();
    });
    let db = Database::memory().unwrap();
    let mut runtimes = HashMap::new();
    let mut routes = HashMap::new();
    for (provider_id, model_id, alias) in [
        ("provider_a", "model_a", "route_a"),
        ("provider_b", "model_b", "route_b"),
    ] {
        let mut provider = tests::mock_provider_row();
        provider.id = provider_id.into();
        provider.provider_type = "openai".into();
        provider.base_url = base_url.clone();
        provider.models = vec![model_id.into()];
        db.save_proxy_provider(&provider).unwrap();
        let runtime: Arc<dyn crate::proxy::providers::Provider> =
            Arc::from(crate::proxy::providers::build_provider(&provider).unwrap());
        runtimes.insert(provider_id.to_string(), runtime);
        routes.insert(
            alias.to_string(),
            vec![RouteHop {
                provider: provider_id.into(),
                model: model_id.into(),
            }],
        );
    }
    let router = Arc::new(Router::new(runtimes, routes));
    let mut request_a = budget_request();
    request_a.model = "route_a".into();
    let mut request_b = budget_request();
    request_b.model = "route_b".into();
    let target_a = target::Prompt_target::Gateway {
        model: "route_a".into(),
    };
    let target_b = target::Prompt_target::Gateway {
        model: "route_b".into(),
    };
    let outcomes = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(
            target::dispatch(&db, Some(router.clone()), &target_a, &request_a),
            target::dispatch(&db, Some(router.clone()), &target_b, &request_b)
        )
    })
    .await;
    upstream.abort();
    let _ = upstream.await;
    let (result_a, result_b) = outcomes.expect("both concurrent requests completed");
    for (result, provider_id, model_id) in [
        (result_a, "provider_a", "model_a"),
        (result_b, "provider_b", "model_b"),
    ] {
        let result = result.unwrap();
        assert_eq!(result.provider.as_deref(), Some(provider_id));
        assert_eq!(result.serving_hop.provider, provider_id);
        assert_eq!(result.serving_hop.model, model_id);
        assert_eq!(
            result.response.model,
            format!("{model_id}-dated-upstream-label")
        );
    }
    // The shared bookkeeping still receives normal router updates, but only
    // one final winner can live there; neither receipt depends on that map.
    assert!(router
        .last_used
        .lock()
        .unwrap()
        .contains_key(target::PROMPT_APP_TYPE));
}

#[tokio::test]
async fn long_brief_uses_economical_review_then_one_escalation() {
    use crate::database::dao::routes::RouteHop;
    use axum::{extract::Json, routing::post, Router as HttpRouter};
    use std::collections::HashMap;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured = Arc::clone(&observed);
    let upstream = tokio::spawn(async move {
        axum::serve(listener, HttpRouter::new().route("/v1/chat/completions", post(move |Json(body): Json<Value>| {
            let captured = Arc::clone(&captured);
            async move {
                let model = body["model"].as_str().unwrap(); captured.lock().unwrap().push(model.to_string());
                let content = match model {
                    "review-cheap" => "This is not a usable structured verdict.",
                    "review-strong" => r#"{"verdict":"ok","issues":[]}"#,
                    _ => "Implement the parser. Preserve every input and report invalid records.",
                };
                Json(json!({"id":"fixture","object":"chat.completion","created":0,"model":model,
                    "choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":content}}]}))
            }
        }))).await.unwrap();
    });
    let db = Database::memory().unwrap();
    let mut provider = tests::mock_provider_row();
    provider.provider_type = "openai".into();
    provider.base_url = base_url;
    provider.models = vec![
        "generate".into(),
        "review-cheap".into(),
        "review-strong".into(),
    ];
    db.save_proxy_provider(&provider).unwrap();
    let runtime: Arc<dyn crate::proxy::providers::Provider> =
        Arc::from(crate::proxy::providers::build_provider(&provider).unwrap());
    // Explicit alias namespaces require an admitted capability plan, even
    // when a route of that name exists. Keep that production boundary intact.
    struct Review_fixture_planner {
        provider_id: String,
    }
    impl crate::proxy::planner::Model_planner for Review_fixture_planner {
        fn resolve(
            &self,
            selector: &str,
            features: crate::proxy::planner::Request_features,
        ) -> Result<crate::proxy::planner::Resolution, crate::proxy::canonical::ProviderError>
        {
            use crate::proxy::planner::{Planned_hop, Resolution};
            let (model, tier) = match selector {
                "alias:haiku" => ("review-cheap", 0),
                "alias:opus" => ("review-strong", 2),
                _ => return Ok(Resolution::Exact(selector.into())),
            };
            let eligible = !features.needs_tools
                && !features.needs_images
                && features
                    .min_context_window
                    .is_none_or(|tokens| tokens <= 128_000)
                && features
                    .min_max_output_tokens
                    .is_none_or(|tokens| tokens <= 8_192);
            Ok(Resolution::Alias {
                alias: selector.into(),
                tier,
                hops: if eligible {
                    vec![Planned_hop {
                        provider: self.provider_id.clone(),
                        model: model.into(),
                        tier: Some(tier),
                        priority: 100,
                        basis: "fixture_capabilities".into(),
                        downgraded: false,
                        reason: "fixture_review_offer".into(),
                    }]
                } else {
                    Vec::new()
                },
            })
        }
    }
    let planner = Arc::new(Review_fixture_planner {
        provider_id: provider.id.clone(),
    });
    let router = Arc::new(
        Router::new(
            HashMap::from([(provider.id.clone(), runtime)]),
            HashMap::from([
                (
                    "alias:haiku".into(),
                    vec![RouteHop {
                        provider: provider.id.clone(),
                        model: "review-cheap".into(),
                    }],
                ),
                (
                    "alias:opus".into(),
                    vec![RouteHop {
                        provider: provider.id.clone(),
                        model: "review-strong".into(),
                    }],
                ),
            ]),
        )
        .with_planner(planner),
    );
    assert!(router
        .preview_model(
            crate::proxy::router::SURFACE_CHAT,
            "alias:haiku",
            crate::proxy::planner::Request_features {
                min_max_output_tokens: Some(8193),
                ..Default::default()
            },
            None
        )
        .is_err());
    let mut request = tests::gateway_request(
        "en_optimize",
        &"Preserve records and test validation. ".repeat(150),
    );
    request.target = target::Prompt_target::Provider {
        provider_id: provider.id,
        model: "generate".into(),
    };
    let output = run_stage(&db, Some(router), request).await.unwrap();
    assert!(output.complete);
    assert_eq!(output.review_status, "passed");
    assert_eq!(output.attempts, 3);
    assert_eq!(
        *observed.lock().unwrap(),
        vec!["generate", "review-cheap", "review-strong"]
    );
    upstream.abort();
    let _ = upstream.await;
}
