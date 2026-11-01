//! Explicit, opt-in local acceptance operations. Never run in the ordinary suite.
//! These exercise the same source-owned import, discovery and setup services as IPC.

use crate::{
    config,
    database::Database,
    services::{import_cc_switch, suppliers},
};
use serde_json::json;

fn live_database(action: &str) -> Database {
    assert_eq!(
        std::env::var("AAG_MIRROR_LOADING_ACCEPTANCE").as_deref(),
        Ok(action)
    );
    assert!(std::env::var_os("AAG_CONFIG_DIR").is_none());
    assert!(std::env::var_os("AAG_HOME_DIR").is_none());
    Database::init().expect("open current AAG configuration")
}

#[tokio::test]
#[ignore = "reads current configured model inventory through its owning database service"]
async fn live_mirror_model_inventory() {
    let db = live_database("INSPECT");
    let (generation, suppliers) = db.get_supplier_snapshot().unwrap();
    let routes = db.get_all_routes().unwrap();
    let snapshot = crate::services::proxy_runtime::snapshot::build_runtime_snapshot(
        &db,
        &db.get_proxy_config().unwrap(),
    )
    .await
    .unwrap();
    let role_targets = |surface: &str, selectors: &[&str]| {
        selectors.iter().map(|selector| {
            match snapshot.router.resolve_model(surface, selector, Default::default()) {
                Ok(hops) => json!({
                    "selector":selector,
                    "preferred_upstream_model":hops.first().map(|hop|hop.model.as_str()),
                    "upstream_models":hops.iter().map(|hop|hop.model.as_str()).collect::<Vec<_>>(),
                    "selector_equals_every_upstream_model":!hops.is_empty() && hops.iter().all(|hop|hop.model == *selector)
                }),
                Err(error) => json!({"selector":selector,"resolution_error":error.code}),
            }
        }).collect::<Vec<_>>()
    };
    println!(
        "{}",
        json!({
            "generation":generation,
        "source_and_mirror_paths":{
            "codex_source":config::get_codex_config_dir(),
            "codex_mirror":config::get_codex_mirror_dir(db.get_setting(crate::database::dao::settings::KEY_CODEX_CLI_HOME).unwrap().as_deref()),
            "claude_source":config::get_claude_config_dir(),
            "claude_mirror":config::get_claude_mirror_dir(db.get_setting(crate::database::dao::settings::KEY_CLAUDE_CLI_HOME).unwrap().as_deref())
        },
        "mirror_role_queues_enabled":snapshot.router.mirror_roles_enabled(),
        "instruction_sync_enabled":crate::services::mirror_sync::instruction_sync_enabled(&db).unwrap_or(false),
        "claude_role_targets":role_targets(crate::proxy::router::SURFACE_CLAUDE, &crate::services::mirror_models::CLAUDE_ROLE_MODELS),
        "codex_role_targets":role_targets(crate::proxy::router::SURFACE_CODEX, &crate::services::mirror_models::CODEX_ROLE_MODELS),
            "default_model":crate::services::supplier_setup::selected_model(&db).unwrap(),
            "suppliers":suppliers.iter().map(|supplier| json!({
                "enabled":supplier.enabled,
                "endpoints":supplier.endpoints.iter().map(|endpoint| json!({
                    "protocol":endpoint.protocol,"enabled":endpoint.enabled,
                    "models":endpoint.models.iter().map(|model| json!({"id":model.model_id,"enabled":model.enabled})).collect::<Vec<_>>()
                })).collect::<Vec<_>>()
            })).collect::<Vec<_>>(),
            "routes":routes.iter().map(|route|json!({"alias":route.alias,"models":route.hops.iter().map(|hop|&hop.model).collect::<Vec<_>>()})).collect::<Vec<_>>(),
            "codex_models":snapshot.router.credentialless_models(crate::proxy::router::SURFACE_CODEX, snapshot.pinned_provider.as_deref()),
            "claude_models":snapshot.router.credentialless_models(crate::proxy::router::SURFACE_CLAUDE, snapshot.pinned_provider.as_deref())
        })
    );
}

#[tokio::test]
#[ignore = "refreshes credential-free public ranking/capability caches in the live AAG store"]
async fn live_refresh_ranked_gateway_evidence() {
    let db = live_database("RANKING_REFRESH");
    let service = crate::services::model_catalog::Catalog_refresh_service::new();
    for source in [
        crate::services::model_catalog::SOURCE_MODELS_DEV,
        crate::services::model_catalog::SOURCE_EVALPLUS,
        crate::services::model_catalog::SOURCE_LMARENA,
    ] {
        if let Ok(selected) = std::env::var("AAG_ACCEPTANCE_PUBLIC_SOURCE") {
            if selected != source {
                continue;
            }
        }
        let view = service
            .refresh(
                &db,
                &crate::services::model_catalog::CatalogRefreshInput {
                    source: source.into(),
                },
            )
            .await
            .unwrap();
        println!(
            "{}",
            json!({"source":source,"fetched":view.fetched_at.is_some(),"stale":view.stale,"error":view.last_error})
        );
    }
    let evidence = crate::services::model_catalog::load_evidence(&db);
    assert!(
        evidence.arena.is_some(),
        "Arena snapshot unavailable; no capability-routing success is claimed"
    );
    let models = crate::services::model_catalog::offered_models(&db).unwrap();
    let evidence =
        evidence.with_offered_models(models.iter().map(|model| model.model_id.clone()).collect());
    let assessments:Vec<_>=models.iter().map(|model| {
        let assessment=crate::services::model_catalog::tiers::evaluate_model(&model.model_id,model.tier_override,&evidence);
        json!({"model":model.model_id,"tier":assessment.tier,"basis":assessment.basis.code(),"tools_configured":model.supports_tools})
    }).collect();
    println!(
        "{}",
        json!({"arena_models":evidence.arena.as_ref().map(|arena|arena.entries.len()),"assessments":assessments})
    );
}

#[test]
#[ignore = "adds documented native Responses endpoints to existing official MiniMax suppliers"]
fn live_configure_minimax_dual_protocols() {
    let db = live_database("MINIMAX_PROTOCOLS");
    let (mut generation, records) = db.get_supplier_snapshot().unwrap();
    let mut plans = Vec::new();
    for supplier in records.iter().filter(|supplier| supplier.enabled) {
        if supplier
            .endpoints
            .iter()
            .any(|endpoint| endpoint.protocol == "openai-responses")
        {
            continue;
        }
        let Some(anthropic) = supplier
            .endpoints
            .iter()
            .find(|endpoint| endpoint.enabled && endpoint.protocol == "anthropic")
        else {
            continue;
        };
        let Ok(mut url) = reqwest::Url::parse(
            anthropic
                .endpoint_url
                .as_deref()
                .unwrap_or(&supplier.base_url),
        ) else {
            continue;
        };
        if url.scheme() != "https"
            || !matches!(url.host_str(), Some("api.minimax.io" | "api.minimaxi.com"))
        {
            continue;
        }
        let models: Vec<_> = anthropic
            .models
            .iter()
            .filter(|model| {
                model.enabled
                    && matches!(model.model_id.as_str(), "MiniMax-M3.1-Flash" | "MiniMax-M3")
            })
            .map(|model| suppliers::SupplierModelInput {
                model_id: model.model_id.clone(),
                display_name: model.display_name.clone(),
                enabled: true,
                supports_tools: model.supports_tools,
                supports_images: model.supports_images,
                context_window: model.context_window,
                max_output_tokens: model.max_output_tokens,
                tier_override: model.tier_override,
                source: Some("provider_documentation".into()),
                provenance: None,
            })
            .collect();
        if models.is_empty() {
            continue;
        }
        url.set_path("/v1");
        url.set_query(None);
        url.set_fragment(None);
        let mut endpoints: Vec<_> = supplier
            .endpoints
            .iter()
            .map(|endpoint| suppliers::SupplierEndpointInput {
                protocol: endpoint.protocol.clone(),
                endpoint_url: None,
                api_key_override: None,
                use_shared_api_key: false,
                enabled: None,
                priority: None,
                extra_headers: None,
                source: None,
                provenance: None,
                models: None,
            })
            .collect();
        endpoints.push(suppliers::SupplierEndpointInput {
            protocol: "openai-responses".into(),
            endpoint_url: Some(url.to_string()),
            api_key_override: anthropic.api_key_override.clone(),
            use_shared_api_key: anthropic.api_key_override.is_none(),
            enabled: Some(true),
            priority: Some(anthropic.priority),
            extra_headers: Some(Default::default()),
            source: Some("provider_documentation".into()),
            provenance: None,
            models: Some(models),
        });
        plans.push(suppliers::SupplierSaveInput {
            id: Some(supplier.id.clone()),
            expected_generation: 0,
            name: supplier.name.clone(),
            base_url: None,
            api_key: None,
            enabled: supplier.enabled,
            priority: supplier.priority,
            source: None,
            provenance: None,
            icon: None,
            icon_color: None,
            endpoints: Some(endpoints),
        });
    }
    let count = plans.len();
    if count > 0 {
        let backup = config::get_app_config_dir()
            .join("provider_reset_backups")
            .join(format!(
                "before_dual_protocol_setup_{}.db",
                chrono::Utc::now().format("%Y%m%dT%H%M%S%fZ")
            ));
        config::private_fs::ensure_private_file(&backup).unwrap();
        db.conn
            .lock()
            .unwrap()
            .execute("VACUUM INTO ?1", [backup.to_string_lossy().as_ref()])
            .unwrap();
        config::private_fs::secure_existing_file(&backup).unwrap();
        for mut plan in plans {
            plan.expected_generation = generation;
            generation = suppliers::save_supplier(&db, plan).unwrap().generation;
        }
    }
    println!(
        "{}",
        json!({"updated_suppliers":count,"generation":generation,"protocols":["anthropic","openai-responses"],"backup_created":count>0})
    );
}

#[tokio::test]
#[ignore = "sends two small native MiniMax functional requests using one existing configured subscription"]
async fn live_minimax_native_protocol_smoke() {
    let db = live_database("MINIMAX_NATIVE_SMOKE");
    let snapshot = suppliers::resolved_supplier_snapshot(&db).unwrap();
    let native = snapshot
        .endpoints
        .iter()
        .find(|endpoint| {
            endpoint.protocol == "openai-responses"
                && reqwest::Url::parse(&endpoint.base_url)
                    .ok()
                    .is_some_and(|url| {
                        matches!(url.host_str(), Some("api.minimaxi.com" | "api.minimax.io"))
                    })
                && endpoint
                    .models
                    .iter()
                    .any(|model| model.enabled && model.model_id == "MiniMax-M3.1-Flash")
        })
        .expect("configured official MiniMax Responses endpoint");
    for protocol in ["anthropic", "openai-responses"] {
        let endpoint = snapshot
            .endpoints
            .iter()
            .find(|endpoint| {
                endpoint.supplier_id == native.supplier_id && endpoint.protocol == protocol
            })
            .unwrap();
        let provider = db
            .get_proxy_provider(endpoint.legacy_proxy_provider_id.as_deref().unwrap())
            .unwrap()
            .unwrap();
        let provider = crate::proxy::providers::build_provider(&provider).unwrap();
        let model = "MiniMax-M3.1-Flash";
        let mut request = if protocol == "anthropic" {
            let source=serde_json::from_value(json!({"model":model,"max_tokens":1024,"output_config":{"effort":"low"},"messages":[{"role":"user","content":"Reply exactly AAG_OK."}]})).unwrap();
            crate::proxy::converter::anthropic_req_to_oai_req(&source)
        } else {
            let source=serde_json::from_value(json!({"model":model,"max_output_tokens":1024,"reasoning":{"effort":"low"},"store":false,"input":"Reply exactly AAG_OK."})).unwrap();
            crate::proxy::converter::responses_to_chat(&source)
        };
        request.request_id = Some("aag_minimax_native_smoke".into());
        let started = std::time::Instant::now();
        match provider.complete(&request, model).await {
            Ok(response) => {
                let text = response
                    .choices
                    .iter()
                    .filter_map(|choice| choice.message.content.as_deref())
                    .collect::<String>();
                let matched = text.trim() == "AAG_OK";
                println!(
                    "{}",
                    json!({"protocol":protocol,"model":model,"matches_expected":matched,"text_characters":text.len(),"elapsed_ms":started.elapsed().as_millis(),"native_responses":response.source_responses_response.is_some()})
                );
                assert!(
                    matched,
                    "native provider did not produce the expected diagnostic text"
                );
            }
            Err(error) => {
                println!(
                    "{}",
                    json!({"protocol":protocol,"model":model,"status":error.status,"code":error.code,"stage":error.stage,"elapsed_ms":started.elapsed().as_millis()})
                );
                panic!("native protocol smoke failed; no endpoint success is claimed");
            }
        }
    }
}

#[tokio::test]
#[ignore = "measures local snapshot/planning with the current live configuration; no upstream calls"]
async fn live_ranked_routing_local_timing() {
    let db = live_database("ROUTING_TIMING");
    let started = std::time::Instant::now();
    let snapshot = crate::services::proxy_runtime::snapshot::build_runtime_snapshot(
        &db,
        &db.get_proxy_config().unwrap(),
    )
    .await
    .unwrap();
    let snapshot_ms = started.elapsed().as_millis();
    let mut samples = Vec::new();
    for selector in [
        "claude-fable-5",
        "claude-opus-5",
        "claude-haiku-5-5",
        "gpt-5.6-sol",
        "gpt-5.6-terra",
        "gpt-5.6-luna",
    ] {
        let surface = if selector.starts_with("claude") {
            crate::proxy::router::SURFACE_CLAUDE
        } else {
            crate::proxy::router::SURFACE_CODEX
        };
        let started = std::time::Instant::now();
        let hops = snapshot
            .router
            .resolve_model(surface, selector, Default::default())
            .unwrap();
        samples.push(json!({"selector":selector,"planning_ms":started.elapsed().as_millis(),"hops":hops.len()}));
    }
    println!("{}", json!({"snapshot_ms":snapshot_ms,"samples":samples}));
}

#[tokio::test]
#[ignore = "starts a temporary loopback gateway and sends five small functional requests through existing supply"]
async fn live_ranked_gateway_surfaces() {
    use std::sync::{atomic::AtomicBool, Arc, Mutex};
    use std::time::Duration;
    let db = live_database("GATEWAY_SURFACES");
    let snapshot = crate::services::proxy_runtime::snapshot::build_runtime_snapshot(
        &db,
        &db.get_proxy_config().unwrap(),
    )
    .await
    .unwrap();
    let auth = "aag_transient_surface_diagnostic";
    let state = crate::proxy::server::ProxyAppState {
        router: snapshot.router,
        auth_token: auth.into(),
        pinned_provider: Arc::new(Mutex::new(snapshot.pinned_provider)),
        responses_cot_enabled: Arc::new(AtomicBool::new(false)),
        passthrough_auth_allowed: false,
        db: None,
        live: None,
        allow_client_config_repair: false,
    };
    let (shutdown, receiver) = tokio::sync::oneshot::channel();
    let (bound, server) =
        crate::proxy::server::serve_detached("127.0.0.1:0".parse().unwrap(), state, receiver)
            .await
            .unwrap();
    struct Server_guard(tokio::task::AbortHandle);
    impl Drop for Server_guard {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let guard = Server_guard(server.abort_handle());
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(60))
        .build()
        .unwrap();
    let prompt = "Reply exactly AAG_OK.";
    let gemini_body = json!({"contents":[{"role":"user","parts":[{"text":prompt}]}],
        "generationConfig":{"maxOutputTokens":4096}});
    let cases = [
        (
            "anthropic",
            "/claude-desktop/v1/messages",
            json!({"model":"claude-fable-5","max_tokens":4096,
            "messages":[{"role":"user","content":prompt}]}),
        ),
        (
            "responses",
            "/v1/responses",
            json!({"model":"gpt-5.6-sol","max_output_tokens":4096,
            "input":prompt,"store":false}),
        ),
        (
            "chat",
            "/v1/chat/completions",
            json!({"model":"MiniMax-M3.1-Flash","max_tokens":4096,
            "messages":[{"role":"user","content":prompt}]}),
        ),
        (
            "gemini",
            "/v1beta/models/MiniMax-M3.1-Flash:generateContent",
            gemini_body.clone(),
        ),
        (
            "gemini_stream",
            "/v1beta/models/MiniMax-M3.1-Flash:streamGenerateContent?alt=sse",
            gemini_body,
        ),
    ];
    for (surface, path, body) in cases {
        if std::env::var("AAG_ACCEPTANCE_SURFACE_FILTER")
            .ok()
            .is_some_and(|selected| selected != surface)
        {
            continue;
        }
        let started = std::time::Instant::now();
        let response = client
            .post(format!("http://{}{path}", bound.addr))
            .bearer_auth(auth)
            .header("x-goog-api-key", auth)
            .header("anthropic-version", "2023-06-01")
            .json(&body)
            .send()
            .await
            .unwrap();
        let status = response.status();
        let raw = response.bytes().await.unwrap();
        assert!(raw.len() < 4 * 1024 * 1024);
        if !status.is_success() {
            let error = serde_json::from_slice::<serde_json::Value>(&raw).unwrap_or_default();
            println!(
                "{}",
                json!({"surface":surface,"status":status.as_u16(),
                "error_code":error.pointer("/error/code").or_else(||error.pointer("/error/type")),
                "elapsed_ms":started.elapsed().as_millis()})
            );
            panic!("functional gateway surface rejected the diagnostic; no success claimed");
        }
        let text = if surface == "gemini_stream" {
            std::str::from_utf8(&raw)
                .unwrap()
                .lines()
                .filter_map(|line| line.strip_prefix("data:"))
                .filter_map(|data| serde_json::from_str::<serde_json::Value>(data.trim()).ok())
                .filter_map(|data| {
                    data.pointer("/candidates/0/content/parts")
                        .and_then(|parts| parts.as_array())
                        .cloned()
                })
                .flatten()
                .filter(|part| part.get("thought").and_then(|value| value.as_bool()) != Some(true))
                .filter_map(|part| {
                    part.get("text")
                        .and_then(|value| value.as_str())
                        .map(str::to_string)
                })
                .collect::<String>()
        } else {
            let data: serde_json::Value = serde_json::from_slice(&raw).unwrap();
            match surface {
                "anthropic" => data["content"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(|part| part.get("text").and_then(|value| value.as_str()))
                    .collect(),
                "responses" => data["output"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|item| {
                        item.get("type").and_then(|value| value.as_str()) == Some("message")
                    })
                    .filter_map(|item| item.get("content").and_then(|value| value.as_array()))
                    .flatten()
                    .filter(|part| {
                        part.get("type").and_then(|value| value.as_str()) == Some("output_text")
                    })
                    .filter_map(|part| part.get("text").and_then(|value| value.as_str()))
                    .collect(),
                "chat" => data["choices"][0]["message"]["content"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                _ => data["candidates"][0]["content"]["parts"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|part| {
                        part.get("thought").and_then(|value| value.as_bool()) != Some(true)
                    })
                    .filter_map(|part| part.get("text").and_then(|value| value.as_str()))
                    .collect(),
            }
        };
        let matched = text.trim() == "AAG_OK";
        println!(
            "{}",
            json!({"surface":surface,"status":status.as_u16(),"matches_expected":matched,
                "text_characters":text.len(),"elapsed_ms":started.elapsed().as_millis()})
        );
        assert!(
            matched,
            "functional surface did not produce expected diagnostic text"
        );
    }
    let _ = shutdown.send(());
    tokio::time::timeout(Duration::from_secs(3), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    drop(guard);
}

#[test]
#[ignore = "reconciles live instruction mirrors and enables app-owned continuous synchronization"]
fn live_enable_instruction_sync() {
    let db = live_database("SYNC");
    let previous = db
        .get_setting(crate::services::mirror_sync::KEY_ENABLED)
        .unwrap();
    db.set_setting(crate::services::mirror_sync::KEY_ENABLED, "1")
        .unwrap();
    let service = crate::services::mirror_sync::Mirror_sync_service::new();
    let report = service.reconcile(&db);
    println!("{}", serde_json::to_string(&report).unwrap());
    if report.for_client("codex").is_err() || report.for_client("claude").is_err() {
        db.set_setting(
            crate::services::mirror_sync::KEY_ENABLED,
            previous.as_deref().unwrap_or("0"),
        )
        .unwrap();
    }
    report.for_client("codex").unwrap();
    report.for_client("claude").unwrap();
    let repeated = service.reconcile(&db);
    println!("{}", serde_json::to_string(&repeated).unwrap());
    repeated.for_client("codex").unwrap();
    repeated.for_client("claude").unwrap();
}

#[tokio::test]
#[ignore = "enables live mirror role queues and rewrites only managed mirror configuration"]
async fn live_enable_mirror_role_configuration() {
    let db = live_database("MODELS");
    db.set_setting(crate::services::mirror_models::KEY_ENABLED, "1")
        .unwrap();
    let config = db.get_proxy_config().unwrap();
    let snapshot = crate::services::proxy_runtime::snapshot::build_runtime_snapshot(&db, &config)
        .await
        .unwrap();
    let allowed = snapshot.router.credentialless_models(
        crate::proxy::router::SURFACE_CODEX,
        snapshot.pinned_provider.as_deref(),
    );
    assert_eq!(
        snapshot
            .router
            .mirror_role_models(
                crate::proxy::router::SURFACE_CODEX,
                snapshot.pinned_provider.as_deref()
            )
            .len(),
        3
    );
    assert_eq!(
        snapshot
            .router
            .mirror_role_models(
                crate::proxy::router::SURFACE_CLAUDE,
                snapshot.pinned_provider.as_deref()
            )
            .len(),
        4
    );
    crate::services::supplier_setup::configure_mirrors_for_runtime(&db, Some(&allowed)).unwrap();
    for (surface, aliases) in [
        (
            crate::proxy::router::SURFACE_CLAUDE,
            crate::services::mirror_models::CLAUDE_ROLE_MODELS.as_slice(),
        ),
        (
            crate::proxy::router::SURFACE_CODEX,
            crate::services::mirror_models::CODEX_ROLE_MODELS.as_slice(),
        ),
    ] {
        for alias in aliases {
            let queue = snapshot
                .router
                .resolve_model(surface, alias, Default::default())
                .unwrap();
            assert!(!queue.is_empty());
            println!(
                "{}",
                json!({"surface":surface,"selector":alias,"upstream_queue":queue.iter().map(|hop|hop.model.as_str()).collect::<Vec<_>>()})
            );
        }
    }
}

#[test]
#[ignore = "requires explicit read-only cc-switch preview authorization"]
fn live_cc_switch_import_preview() {
    let db = live_database("PREVIEW");
    let preview =
        import_cc_switch::preview_import(&db, &import_cc_switch::cc_switch_db_path()).unwrap();
    println!(
        "{}",
        json!({"source_rows":preview.total_rows,"importable":preview.plans.len(),"pending":preview.pending_unsupported.len(),"problems":preview.problems.len(),"providers":preview.plans.iter().map(|entry|json!({"name":entry.name,"protocol":entry.protocol,"models":entry.model_count,"key_configured":entry.api_key_configured})).collect::<Vec<_>>()})
    );
}

#[test]
#[ignore = "writes live AAG configuration; explicit import authorization required"]
fn live_cc_switch_import_apply() {
    let db = live_database("IMPORT");
    let source = import_cc_switch::cc_switch_db_path();
    let preview = import_cc_switch::preview_import(&db, &source).unwrap();
    let backup = config::get_db_path()
        .parent()
        .unwrap()
        .join("provider_reset_backups")
        .join(format!(
            "before_cc_switch_import_{}.db",
            chrono::Utc::now().format("%Y%m%dT%H%M%S%fZ")
        ));
    config::private_fs::ensure_private_file(&backup).unwrap();
    db.conn
        .lock()
        .unwrap()
        .execute("VACUUM INTO ?1", [backup.to_string_lossy().as_ref()])
        .unwrap();
    config::private_fs::secure_existing_file(&backup).unwrap();
    let result = import_cc_switch::apply_import(&db, &source, &preview.plan_id).unwrap();
    println!(
        "{}",
        json!({"created":result.created_count,"refreshed":result.refreshed_count,"pending":result.pending_unsupported_count,"problems":result.problem_count,"already_applied":result.already_applied,"generation":result.applied_generation,"backup_created":true})
    );
}

#[tokio::test]
#[ignore = "calls configured catalog APIs and updates their imported model inventories"]
async fn live_imported_catalog_discovery() {
    let db = live_database("DISCOVER");
    let (mut generation, records) = db.get_supplier_snapshot().unwrap();
    let mut discovered = Vec::new();
    for supplier in records
        .iter()
        .filter(|supplier| supplier.enabled && supplier.source == "cc_switch")
    {
        for endpoint in supplier
            .endpoints
            .iter()
            .filter(|endpoint| endpoint.enabled)
        {
            let input = serde_json::from_value(
                json!({"supplierId":supplier.id,"protocol":endpoint.protocol}),
            )
            .unwrap();
            match suppliers::discover_supplier_models(&db, input).await {
                Ok(models) if !models.is_empty() => {
                    let model_inputs = models
                        .iter()
                        .map(|model| {
                            let old = endpoint.models.iter().find(|old| old.model_id == *model);
                            suppliers::SupplierModelInput {
                                model_id: model.clone(),
                                display_name: old.and_then(|old| old.display_name.clone()),
                                enabled: old.is_none_or(|old| old.enabled),
                                supports_tools: old.and_then(|old| old.supports_tools),
                                supports_images: old.and_then(|old| old.supports_images),
                                context_window: old.and_then(|old| old.context_window),
                                max_output_tokens: old.and_then(|old| old.max_output_tokens),
                                tier_override: old.and_then(|old| old.tier_override),
                                source: Some("api_discovery".into()),
                                provenance: None,
                            }
                        })
                        .collect();
                    let result = suppliers::update_supplier_models(
                        &db,
                        suppliers::SupplierModelsUpdateInput {
                            supplier_id: supplier.id.clone(),
                            protocol: endpoint.protocol.clone(),
                            expected_generation: generation,
                            models: model_inputs,
                        },
                    )
                    .unwrap();
                    generation = result.generation;
                    discovered.push((
                        supplier.id.clone(),
                        endpoint.protocol.clone(),
                        models.clone(),
                    ));
                    println!(
                        "{}",
                        json!({"name":supplier.name,"protocol":endpoint.protocol,"discovered":models.len(),"status":"updated"})
                    );
                }
                Ok(_) => println!(
                    "{}",
                    json!({"name":supplier.name,"protocol":endpoint.protocol,"status":"empty_catalog_preserved"})
                ),
                Err(error) => println!(
                    "{}",
                    json!({"name":supplier.name,"protocol":endpoint.protocol,"status":"catalog_failed_preserved","code":suppliers::ipc_error(error)})
                ),
            }
        }
    }
    println!(
        "{}",
        json!({"successful_endpoints":discovered.len(),"generation":generation})
    );
    assert!(
        !discovered.is_empty(),
        "no catalog succeeded; existing model data was preserved"
    );
}

#[tokio::test]
#[ignore = "writes live mirror configuration and starts a temporary local gateway"]
async fn live_reconcile_mirror_configuration() {
    let db = live_database("SETUP");
    if let Ok(model) = std::env::var("AAG_ACCEPTANCE_MODEL") {
        assert!(suppliers::resolved_supplier_snapshot(&db)
            .unwrap()
            .endpoints
            .iter()
            .any(|endpoint| endpoint
                .models
                .iter()
                .any(|offered| offered.enabled && offered.model_id == model)));
        db.set_setting(
            crate::services::supplier_setup::KEY_UNIFIED_DEFAULT_MODEL,
            &model,
        )
        .unwrap();
    }
    let state = tokio::sync::Mutex::new(None);
    super::suppliers::reconcile_supplier_setup(
        &db,
        &state,
        &crate::services::tailnet_publication::Publication_service::new(),
        &crate::services::provider::switching::Provider_switch_service::default(),
    )
    .await
    .unwrap();
    let running = state.lock().await.take().unwrap();
    let mut shutdown = running.request_shutdown();
    shutdown.wait().await.unwrap();
    println!("mirror_configuration_reconciled=true");
}
