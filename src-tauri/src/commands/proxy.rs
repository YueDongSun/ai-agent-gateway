//! Proxy lifecycle commands: start/stop/status/config, provider + route CRUD, cc-switch import.
//!
//! Extracted from `lib.rs` in the file-structure refactor - the command
//! bodies are moved verbatim; only module paths in `use` statements and
//! `pub(crate)` visibility changed.

use crate::commands::provider::queue_endpoint_of;
use crate::database::Database;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use tauri::Manager;
use tokio::sync::Mutex;

// ----- Proxy commands (Phase 2) -----

/// Handle to a running proxy server. Stored as Tauri managed state.
pub(crate) struct RunningProxy {
    handle: tokio::task::JoinHandle<Result<(), String>>,
    shutdown: tokio::sync::oneshot::Sender<()>,
    config: crate::database::dao::proxy_config::ProxyConfig,
    state: crate::proxy::server::ProxyAppState,
}

impl RunningProxy {
    /// Send the shutdown signal and return the join handle so the caller can
    /// await graceful termination. Consumes the handle because the oneshot
    /// sender is single-use.
    pub(crate) fn request_shutdown(self) -> tokio::task::JoinHandle<Result<(), String>> {
        let _ = self.shutdown.send(());
        self.handle
    }
}

#[derive(Serialize)]
pub(crate) struct ProxyStatusDto {
    running: bool,
    config: crate::database::dao::proxy_config::ProxyConfig,
}

#[tauri::command]
pub(crate) async fn proxy_status(
    state: tauri::State<'_, Mutex<Option<RunningProxy>>>,
) -> Result<ProxyStatusDto, String> {
    let guard = state.lock().await;
    let config = match &*guard {
        Some(rp) => rp.config.clone(),
        None => ProxyConfig::default(),
    };
    Ok(ProxyStatusDto {
        running: guard.is_some(),
        config,
    })
}

/// Read the proxy listen config + auth token (for the endpoint card the
/// user copies into cc-switch).
#[tauri::command]
pub(crate) fn proxy_config_get(db: tauri::State<'_, Database>) -> Result<ProxyConfig, String> {
    db.get_proxy_config().map_err(String::from)
}

/// Update the proxy listen config (host / port / auth token). Takes
/// effect on next proxy start (or immediately via stop + start).
#[tauri::command]
pub(crate) fn proxy_config_save(
    db: tauri::State<'_, Database>,
    config: ProxyConfig,
) -> Result<(), String> {
    db.save_proxy_config(&config).map_err(String::from)
}

#[tauri::command]
pub(crate) async fn proxy_get_pinned_provider(
    state: tauri::State<'_, Mutex<Option<RunningProxy>>>,
) -> Result<Option<String>, String> {
    let guard = state.lock().await;
    match guard.as_ref() {
        Some(running) => {
            let pinned = running
                .state
                .pinned_provider
                .lock()
                .map_err(|_| "proxy_state_lock_failed".to_string())?;
            Ok(pinned.clone())
        }
        None => Ok(None),
    }
}

/// Set (or clear) the pinned proxy provider. When set, ALL requests are
/// forced through that provider, bypassing the failover chain. Pass empty
/// string to clear (restore normal failover).
#[tauri::command]
pub(crate) async fn proxy_set_pinned_provider(
    db: tauri::State<'_, Database>,
    state: tauri::State<'_, Mutex<Option<RunningProxy>>>,
    provider_id: String,
) -> Result<(), String> {
    let pinned = if provider_id.trim().is_empty() {
        None
    } else {
        Some(provider_id.trim().to_string())
    };
    // Persist to settings so it survives restarts.
    db.set_setting(
        crate::database::dao::settings::KEY_PROXY_PINNED_PROVIDER,
        pinned.as_deref().unwrap_or(""),
    )
    .map_err(String::from)?;
    // Update the live proxy state (if running).
    let guard = state.lock().await;
    if let Some(running) = guard.as_ref() {
        let mut pp = running
            .state
            .pinned_provider
            .lock()
            .map_err(|_| "proxy_state_lock_failed".to_string())?;
        *pp = pinned;
    }
    Ok(())
}

/// Read the `/v1/responses` chain-of-thought prompt toggle. Prefers the
/// live flag of a running proxy; falls back to the persisted setting when
/// stopped so the UI toggle reflects the stored value even before the
/// proxy starts (unlike the pinned read, which returns None when stopped).
#[tauri::command]
pub(crate) async fn proxy_get_responses_cot(
    db: tauri::State<'_, Database>,
    state: tauri::State<'_, Mutex<Option<RunningProxy>>>,
) -> Result<bool, String> {
    let guard = state.lock().await;
    if let Some(running) = guard.as_ref() {
        return Ok(running
            .state
            .responses_cot_enabled
            .load(std::sync::atomic::Ordering::Relaxed));
    }
    Ok(db
        .get_setting(crate::database::dao::settings::KEY_PROXY_RESPONSES_COT_ENABLED)
        .map_err(String::from)?
        .as_deref()
        == Some("1"))
}

/// Toggle the built-in chain-of-thought system prompt injected into
/// `/v1/responses` requests (Codex surface only). Persists across restarts
/// and takes effect on the RUNNING proxy immediately — no restart needed.
#[tauri::command]
pub(crate) async fn proxy_set_responses_cot(
    db: tauri::State<'_, Database>,
    state: tauri::State<'_, Mutex<Option<RunningProxy>>>,
    enabled: bool,
) -> Result<(), String> {
    // Persist first so a proxy starting concurrently reads the new value.
    // "" deletes the key — absent means off (settings.rs convention).
    db.set_setting(
        crate::database::dao::settings::KEY_PROXY_RESPONSES_COT_ENABLED,
        if enabled { "1" } else { "" },
    )
    .map_err(String::from)?;
    // Update the live proxy state (if running).
    let guard = state.lock().await;
    if let Some(running) = guard.as_ref() {
        running
            .state
            .responses_cot_enabled
            .store(enabled, std::sync::atomic::Ordering::Relaxed);
    }
    Ok(())
}

/// Test a proxy provider's connectivity and model availability.
/// Sends a minimal chat completion request to the provider and returns:
/// - success/fail status
/// - latency in milliseconds
/// - the model name that was actually used (after any mapping)
/// - the first few tokens of the response (to verify the model works)
/// - error message if failed
///
/// This is similar to cc-switch's `fetch_models_for_config` but tests
/// actual request forwarding, not just the /v1/models endpoint.
#[tauri::command]
pub(crate) async fn proxy_test_provider(
    db: tauri::State<'_, Database>,
    provider_id: String,
    test_model: Option<String>,
) -> Result<serde_json::Value, String> {
    use std::time::Instant;

    let pp = db
        .get_all_proxy_providers()
        .map_err(String::from)?
        .into_iter()
        .find(|p| p.id == provider_id)
        .ok_or_else(|| "proxy_provider_not_found".to_string())?;

    if !pp.enabled {
        return Ok(serde_json::json!({
            "success": false,
            "error": "provider is disabled",
            "latencyMs": 0,
        }));
    }

    // Build the test request. Use a minimal prompt to minimize token usage.
    let model = test_model.unwrap_or_else(|| {
        pp.models
            .first()
            .cloned()
            .unwrap_or_else(|| "gpt-3.5-turbo".into())
    });

    let provider_instance = crate::proxy::providers::build_provider(&pp)
        .map_err(|_| "proxy_provider_build_failed".to_string())?;

    let req = crate::proxy::canonical::ChatCompletionRequest {
        model: model.clone(),
        messages: vec![crate::proxy::canonical::ChatMessage {
            role: "user".into(),
            content: serde_json::Value::String("Reply with exactly: AAG_TEST_OK".into()),
            tool_calls: None,
            tool_call_id: None,
            extra: Default::default(),
        }],
        stream: false,
        temperature: Some(0.0),
        max_tokens: Some(16),
        tools: None,
        tool_choice: None,
        extra: Default::default(),
        source_anthropic_request: None,
        request_id: None,
        client_credential: None,
    };

    let start = Instant::now();
    match provider_instance.complete(&req, &req.model).await {
        Ok(resp) => {
            let latency = start.elapsed().as_millis() as u64;
            let reply = resp
                .choices
                .first()
                .and_then(|c| c.message.content.clone())
                .unwrap_or_default();
            Ok(serde_json::json!({
                "success": true,
                "latencyMs": latency,
                "model": resp.model,
                "reply": reply,
                "providerId": pp.id,
            }))
        }
        Err(e) => {
            let latency = start.elapsed().as_millis() as u64;
            Ok(serde_json::json!({
                "success": false,
                "error": e.message,
                "errorCode": e.code,
                "latencyMs": latency,
                "providerId": pp.id,
            }))
        }
    }
}

/// Fetch available models from a provider's /v1/models endpoint.
/// Similar to cc-switch's `fetch_models_for_config` — tests the provider's
/// model catalog endpoint to verify connectivity and see what models are
/// available.
#[tauri::command]
pub(crate) async fn proxy_fetch_models(
    provider_id: String,
    db: tauri::State<'_, Database>,
) -> Result<Vec<serde_json::Value>, String> {
    let pp = db
        .get_all_proxy_providers()
        .map_err(String::from)?
        .into_iter()
        .find(|p| p.id == provider_id)
        .ok_or_else(|| "proxy_provider_not_found".to_string())?;

    let base_url = pp.base_url.trim_end_matches('/');
    // Try common model list endpoints. Full-endpoint bases (e.g. a stored
    // `.../v1/messages`) are reduced to their API root first so the probe
    // hits `.../v1/models`, not a doubled path.
    let protocol = match pp.provider_type.as_str() {
        "anthropic" => crate::proxy::endpoint::Protocol::Anthropic,
        "gemini" => crate::proxy::endpoint::Protocol::Gemini,
        _ => crate::proxy::endpoint::Protocol::Openai,
    };
    let candidates = crate::proxy::endpoint::models_urls(base_url, protocol);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|_| "proxy_http_client_build_failed".to_string())?;

    let mut last_status: Option<u16> = None;
    let mut transport_failed = false;
    for url in &candidates {
        let mut req = client.get(url);
        if !pp.api_key.is_empty() {
            if pp.provider_type == "gemini" {
                // Gemini's model list authenticates with the API-key header.
                req = req.header("x-goog-api-key", &pp.api_key);
            } else {
                req = req.header("authorization", format!("Bearer {}", pp.api_key));
            }
        }
        match req.send().await {
            Ok(resp) if resp.status().is_success() => {
                let body: serde_json::Value = resp
                    .json()
                    .await
                    .map_err(|_| "proxy_models_decode_failed".to_string())?;
                // OpenAI shape (`data[].id`) first; Gemini lists come back as
                // `models[].name` ("models/gemini-2.5-pro") and are projected
                // onto the OpenAI shape the UI expects.
                if let Some(models) = body.get("data").and_then(|d| d.as_array()) {
                    return Ok(models.clone());
                }
                if let Some(models) = body.get("models").and_then(|m| m.as_array()) {
                    let projected: Vec<serde_json::Value> = models
                        .iter()
                        .filter_map(|entry| {
                            let name = entry.get("name").and_then(|n| n.as_str())?;
                            let id = name.strip_prefix("models/").unwrap_or(name);
                            Some(serde_json::json!({
                                "id": id,
                                "object": "model",
                                "owned_by": "google",
                                "display_name": entry
                                    .get("displayName")
                                    .and_then(|d| d.as_str())
                                    .unwrap_or(id),
                            }))
                        })
                        .collect();
                    return Ok(projected);
                }
                return Ok(Vec::new());
            }
            Ok(resp) => {
                last_status = Some(resp.status().as_u16());
            }
            Err(_) => {
                transport_failed = true;
            }
        }
    }
    if let Some(status) = last_status {
        Err(format!("proxy_models_http_error_{status}"))
    } else if transport_failed {
        Err("proxy_models_transport_failed".into())
    } else {
        Err("proxy_models_unavailable".into())
    }
}

#[tauri::command]
pub(crate) async fn proxy_start(
    app: tauri::AppHandle,
    state: tauri::State<'_, Mutex<Option<RunningProxy>>>,
) -> Result<String, String> {
    // Refuse if already running
    {
        let guard = state.lock().await;
        if guard.is_some() {
            return Err("proxy already running".into());
        }
    }
    let db = app.state::<Database>();
    start_proxy_impl(&db, state.inner()).await
}

/// Body of `proxy_start` minus the "already running" check, so the
/// Tauri setup hook can auto-start the proxy when `proxy_config.enabled`
/// is true (set by `aag-setup` or by the user via the Proxy page).
pub(crate) async fn start_proxy_impl(
    db: &Database,
    state: &Mutex<Option<RunningProxy>>,
) -> Result<String, String> {
    let config = db.get_proxy_config().map_err(String::from)?;
    if !config.enabled {
        // Auto-enable on first start (can be turned off later via save_proxy_config)
        let mut updated = config.clone();
        updated.enabled = true;
        db.save_proxy_config(&updated).map_err(String::from)?;
    }

    let (mut providers, routes) = build_providers_and_routes(db, &config).await?;
    let (queue_ids, queues, queue_routes) = build_failover_queues(db, &config, &mut providers)?;
    // Per-app-provider model mappings (settings_config.modelMapping) become
    // routes pointing at the synthesized queue providers. Explicit routes
    // (pool model_mapping, modelCatalog, manual) always win.
    let mut routes = routes;
    for (alias, hops) in queue_routes {
        routes.entry(alias).or_insert(hops);
    }

    let router = Arc::new(
        crate::proxy::router::Router::new(providers, routes)
            .with_queue(queue_ids.clone())
            .with_queues(queues),
    );
    let pinned = db
        .get_setting(crate::database::dao::settings::KEY_PROXY_PINNED_PROVIDER)
        .ok()
        .flatten();
    let responses_cot_on = db
        .get_setting(crate::database::dao::settings::KEY_PROXY_RESPONSES_COT_ENABLED)
        .ok()
        .flatten()
        .as_deref()
        == Some("1");
    // Passthrough auth relaxation (see `proxy::server`): only when an
    // enabled, non-self-loop `anthropic_passthrough` row is in the pool AND
    // the bind host is loopback. Fail-closed otherwise.
    let passthrough_auth_allowed = crate::proxy::server::host_is_loopback(&config.host)
        && db
            .get_all_proxy_providers()
            .map(|rows| {
                rows.iter().any(|pp| {
                    pp.enabled
                        && pp.provider_type == "anthropic_passthrough"
                        && !is_self_loop(&pp.base_url, &config)
                })
            })
            .unwrap_or(false);
    let app_state = crate::proxy::server::ProxyAppState {
        router,
        auth_token: config.auth_token.clone(),
        pinned_provider: Arc::new(std::sync::Mutex::new(pinned)),
        responses_cot_enabled: Arc::new(std::sync::atomic::AtomicBool::new(responses_cot_on)),
        passthrough_auth_allowed,
        // Review fix: dropped redundant `Arc<Database>` wrap. `Database`
        // already cheap-clones via its internal `Arc<Mutex<Connection>>`.
        db: Some(db.clone()),
    };
    let (bound, handle, shutdown_tx) = bind_with_fallback(&app_state, &config).await?;

    // Persist the actual port if it differs from the configured one, so
    // the next start prefers it.
    if bound.addr.port() != config.port {
        let mut updated = config.clone();
        updated.port = bound.addr.port();
        if let Err(e) = db.save_proxy_config(&updated) {
            let _ = e;
            log::warn!(
                "proxy: fallback port persistence failed port={} code=database_error",
                updated.port
            );
        }
    }

    {
        let mut guard = state.lock().await;
        *guard = Some(RunningProxy {
            handle,
            shutdown: shutdown_tx,
            config: ProxyConfig {
                host: bound.addr.ip().to_string(),
                port: bound.addr.port(),
                ..config.clone()
            },
            state: app_state.clone(),
        });
    }
    let mut msg = if bound.addr.port() == config.port {
        format!("proxy_started port={}", bound.addr.port())
    } else {
        format!(
            "proxy_started port={} (port {} was in use; fallback enabled)",
            bound.addr.port(),
            config.port
        )
    };
    if !queue_ids.is_empty() {
        msg.push_str(&format!(
            " · failover queue: {} provider(s)",
            queue_ids.len()
        ));
    }
    Ok(msg)
}

/// Build the provider-instance map and the routes map from the DB, applying
/// the Phase 22.1/22.2 hot-fixes ([1M] strip on route hops, breaker tighten
/// on imported `ccs-*` providers) plus the two auto-route sources (active
/// Codex provider's modelCatalog, each proxy_provider's model_mapping).
async fn build_providers_and_routes(
    db: &Database,
    config: &crate::database::dao::proxy_config::ProxyConfig,
) -> Result<
    (
        HashMap<String, Arc<dyn crate::proxy::providers::Provider>>,
        HashMap<String, Vec<crate::database::dao::routes::RouteHop>>,
    ),
    String,
> {
    // Build provider instances from DB
    let mut db_providers = db.get_all_proxy_providers().map_err(String::from)?;

    // Hot-fix (Phase 22.2): tighten the circuit breaker on imported `ccs-*`
    // providers from the 5/30s defaults to 3/15s. With the first-byte
    // timeout (Phase 22.2) capping per-hop pre-stream cost at 1.5 s, a
    // tighter breaker opens after ~3 dead requests and skips the bad hop
    // on the next request — turning a ~5-hop, ~7.5 s pre-stream walk into
    // a one-hop landing on the first working provider.
    let tightened_cb = crate::database::dao::proxy_providers::CircuitBreakerConfig {
        failure_threshold: 3,
        cooldown_ms: 15_000,
        half_open_retries: 1,
        success_threshold: 1,
    };
    for pp in db_providers.iter_mut() {
        // Review fix: only clobber the breaker if the user hasn't manually
        // tuned it tighter. A row with failure_threshold <= 3 OR
        // cooldown_ms <= 15000 was tuned deliberately by the user and
        // must be preserved (the hot-fix used to overwrite any threshold
        // > 3, even if the user had bumped the rest of the breaker).
        //
        // The `pp` here is a fresh read from `get_all_proxy_providers()`, so
        // the condition is checked against the *persisted* state, not the
        // in-memory mutation. After a successful write the next proxy_start
        // sees tightened values and the condition is false, so we write each
        // ccs-* row at most once across the lifetime of the database —
        // there's no "write storm" on repeated restarts.
        if pp.id.starts_with("ccs-")
            && (pp.circuit_breaker.failure_threshold > 3 || pp.circuit_breaker.cooldown_ms > 15_000)
        {
            log::info!(
                "proxy: hot-fix provider_ref={} old_failure_threshold={} old_cooldown_ms={} new_failure_threshold=3 new_cooldown_ms=15000",
                crate::diagnostics::safe_reference(&pp.id),
                pp.circuit_breaker.failure_threshold,
                pp.circuit_breaker.cooldown_ms
            );
            pp.circuit_breaker = tightened_cb.clone();
            if let Err(e) = db.save_proxy_provider(pp) {
                let _ = e;
                log::warn!(
                    "proxy: hot-fix provider_ref={} failed code=database_error",
                    crate::diagnostics::safe_reference(&pp.id)
                );
            }
        }
    }

    let mut providers: HashMap<String, Arc<dyn crate::proxy::providers::Provider>> = HashMap::new();
    for pp in &db_providers {
        // Self-loop guard: never register a pool provider whose upstream is
        // AAG's own listener. Unlike the failover-queue path, explicit routes
        // and pinned dispatch reach these providers directly, so the guard
        // must live here too (it now also catches the full-endpoint form).
        if is_self_loop(&pp.base_url, config) {
            log::warn!(
                "proxy: skip provider_ref={} reason=self_loop",
                crate::diagnostics::safe_reference(&pp.id)
            );
            continue;
        }
        match crate::proxy::providers::build_provider(pp) {
            Ok(p) => {
                providers.insert(pp.id.clone(), Arc::from(p));
            }
            Err(e) => {
                let _ = e;
                log::warn!(
                    "proxy: skip provider_ref={} code=provider_build_failed",
                    crate::diagnostics::safe_reference(&pp.id)
                );
            }
        }
    }

    // Build routes from DB
    let db_routes = db.get_all_routes().map_err(String::from)?;
    let mut routes: HashMap<String, Vec<crate::database::dao::routes::RouteHop>> = HashMap::new();
    for r in db_routes {
        // Hot-fix (Phase 22.1): strip the local `[1M]` context marker from
        // every hop model — earlier imports preserved it verbatim, but
        // upstream Anthropic-compatible endpoints reject it as not-a-real
        // model (404 or empty SSE body). Persist the cleaned hops back so
        // the fix survives a restart. The defensive `strip_one_m_suffix`
        // in the providers is a safety net for client-supplied model ids.
        let mut cleaned: Vec<crate::database::dao::routes::RouteHop> = r
            .hops
            .iter()
            .map(|h| crate::database::dao::routes::RouteHop {
                provider: h.provider.clone(),
                model: crate::proxy::strip_one_m_suffix(&h.model),
            })
            .collect();
        if cleaned
            .iter()
            .zip(r.hops.iter())
            .any(|(c, h)| c.model != h.model)
        {
            let mut fixed = r.clone();
            fixed.hops = cleaned.clone();
            if let Err(e) = db.save_route(&fixed) {
                let _ = e;
                log::warn!(
                    "proxy: hot-fix route_ref={} failed code=database_error",
                    crate::diagnostics::safe_reference(&r.alias)
                );
            } else {
                log::info!(
                    "proxy: hot-fix route_ref={} stripped_suffix_count={}",
                    crate::diagnostics::safe_reference(&r.alias),
                    r.hops
                        .iter()
                        .zip(cleaned.iter())
                        .filter(|(o, n)| o.model != n.model)
                        .count()
                );
            }
        }
        routes.insert(r.alias, std::mem::take(&mut cleaned));
    }

    // Auto-create routes from the active Codex provider's modelCatalog.
    // Each entry { display, model, provider } maps a "display" model name
    // (what Codex Desktop sends) to an upstream model on a specific
    // proxy_provider. This is the model-mapping feature: the user configures
    // "GPT-5 -> deepseek-v4-flash on deepseek" in the provider edit modal,
    // and the proxy automatically routes requests accordingly.
    // Only creates routes that don't already exist (won't overwrite manual routes).
    if let Ok(Some(cur_id)) = db.get_current_provider("codex") {
        if let Ok(Some(prov)) = db.get_provider_by_id(&cur_id, "codex") {
            if let Some(mc) = prov.settings_config.get("modelCatalog") {
                if let Some(models) = mc.get("models").and_then(|v| v.as_array()) {
                    for entry in models {
                        let display = match entry.get("display").and_then(|v| v.as_str()) {
                            Some(d) if !d.is_empty() => d.to_string(),
                            _ => continue,
                        };
                        let upstream_model = entry
                            .get("model")
                            .and_then(|v| v.as_str())
                            .unwrap_or(&display)
                            .to_string();
                        let provider_id = entry
                            .get("provider")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        if provider_id.is_empty() {
                            continue;
                        }
                        // Skip if the route already exists (manual route takes priority).
                        if routes.contains_key(&display) {
                            continue;
                        }
                        // Skip if the referenced proxy_provider doesn't exist.
                        let prov_exists = providers.contains_key(&provider_id);
                        if !prov_exists {
                            log::warn!(
                                "proxy: model catalog skip route_ref={} model_ref={} provider_ref={} reason=provider_not_found",
                                crate::diagnostics::safe_reference(&display),
                                crate::diagnostics::safe_reference(&upstream_model),
                                crate::diagnostics::safe_reference(&provider_id)
                            );
                            continue;
                        }
                        log::info!(
                            "proxy: model catalog auto-route route_ref={} provider_ref={} model_ref={}",
                            crate::diagnostics::safe_reference(&display),
                            crate::diagnostics::safe_reference(&provider_id),
                            crate::diagnostics::safe_reference(&upstream_model)
                        );
                        routes.insert(
                            display,
                            vec![crate::database::dao::routes::RouteHop {
                                provider: provider_id,
                                model: upstream_model,
                            }],
                        );
                    }
                }
            }
        }
    }

    // Auto-create routes from proxy_provider model_mapping.
    // Each proxy_provider can have a `model_mapping` field (JSON object:
    // `{ "claude-sonnet-5": "deepseek-v4-pro", ... }`). For each entry,
    // create a route: alias = display model, hop = { this provider, upstream model }.
    // Only creates routes that don't already exist (manual routes + modelCatalog
    // routes take priority).
    for pp in &db_providers {
        if pp.model_mapping.is_empty() {
            continue;
        }
        for (display, upstream_model) in &pp.model_mapping {
            if routes.contains_key(display) {
                continue;
            }
            log::info!(
                "proxy: model mapping auto-route route_ref={} provider_ref={} model_ref={}",
                crate::diagnostics::safe_reference(display),
                crate::diagnostics::safe_reference(&pp.id),
                crate::diagnostics::safe_reference(upstream_model)
            );
            routes.insert(
                display.clone(),
                vec![crate::database::dao::routes::RouteHop {
                    provider: pp.id.clone(),
                    model: upstream_model.clone(),
                }],
            );
        }
    }
    Ok((providers, routes))
}

/// Build the per-surface failover queues (Phase 17) from the `providers`
/// Self-loop guard: true when a provider's upstream URL targets AAG's own
/// listen address. Without this, a failover chain or route can loop incoming
/// requests back into this same proxy — every client call would stall in a
/// circle until the per-hop timeout fires, surfacing as a client "port" hang.
///
/// Compared by parsed host:port (not substring) so loopback aliases the user
/// typed — `http://127.0.0.1:8989/v1`, `http://localhost:8989`,
/// `http://127.0.0.1:8989/v1/messages` — all still trip the guard.
fn is_self_loop(base_url: &str, config: &crate::database::dao::proxy_config::ProxyConfig) -> bool {
    if config.host.is_empty() {
        return false;
    }
    let Ok(url) = url::Url::parse(base_url) else {
        return false;
    };
    let host_matches = match url.host() {
        Some(url::Host::Domain(name)) => {
            name.eq_ignore_ascii_case(&config.host)
                || (name.eq_ignore_ascii_case("localhost") && config.host == "127.0.0.1")
                || (name.eq_ignore_ascii_case("127.0.0.1") && config.host == "localhost")
        }
        Some(url::Host::Ipv4(address)) => {
            address.to_string() == config.host
                || (address.is_loopback() && config.host == "localhost")
        }
        Some(url::Host::Ipv6(address)) => {
            address.to_string() == config.host
                || (address.is_loopback()
                    && (config.host == "localhost"
                        || config.host == "127.0.0.1"
                        || config.host == "::1"))
        }
        None => false,
    };
    if !host_matches {
        return false;
    }
    // Compare effective ports (URL default for the scheme when unspecified).
    let config_port = config.port;
    let url_port = url.port_or_known_default().unwrap_or(0);
    url_port == config_port
}

/// Failover queue construction. From the proxy_provider_failover queue
/// table: each app's in-failover-queue rows, in configured (sort_index)
/// order, synthesize `queue-<id>` proxy_provider entries. Includes the
/// self-loop guard and the optional cc-switch auto-sync.
fn build_failover_queues(
    db: &Database,
    config: &crate::database::dao::proxy_config::ProxyConfig,
    providers: &mut HashMap<String, Arc<dyn crate::proxy::providers::Provider>>,
) -> Result<
    (
        Vec<String>,
        HashMap<String, Vec<String>>,
        HashMap<String, Vec<crate::database::dao::routes::RouteHop>>,
    ),
    String,
> {
    // Per-app failover chains (Phase 17, cc-switch style): each app's
    // provider list (configured sort_index order, only rows with
    // in_failover_queue = 1) becomes a failover queue for its entry
    // surface. Every request always starts from the first configured
    // position — the `is_current` marker is UI/live-config state only and
    // never reorders the chain:
    //   claude_code -> SURFACE_CLAUDE  (/v1/messages)
    //   codex       -> SURFACE_CODEX  (/v1/responses)
    //   copilot|pi  -> SURFACE_CHAT   (/v1/chat/completions)
    // When no explicit route / auto-route matches a model, the request tries
    // each upstream in chain order, model passed through.
    //
    // If the user enabled sync_with_cc_switch_enabled, pull every new
    // claude_code provider out of cc-switch's DB into AAG first. The
    // sync is additive — locked rows are skipped, existing rows are
    // untouched. Failures degrade to warnings; the proxy still starts.
    //
    // OFF by default: the key must be the literal string "1" (not
    // absent, not "0", not "true"). `aag-setup` clears any pre-existing
    // "1" after its explicit one-shot import so users don't get silent
    // background imports unless they re-enable the toggle in Settings.
    let sync_on = db
        .get_setting(crate::database::dao::settings::KEY_SYNC_WITH_CCSWITCH)
        .map_err(String::from)?
        .as_deref()
        == Some("1");
    if sync_on {
        let cc_path = crate::services::import_cc_switch::cc_switch_db_path();
        match crate::services::import_cc_switch::sync_claude_code_from_cc_switch(db, &cc_path) {
            Ok(r) if r.imported.len() > 0 => log::info!(
                "proxy: cc-switch auto-sync imported {} new provider(s) (skipped {})",
                r.imported.len(),
                r.skipped_existing.len()
            ),
            Ok(_) => {}
            Err(_) => log::warn!("proxy: cc-switch auto-sync failed code=import_error"),
        }
    }

    // Self-loop guard (see `is_self_loop`): skip providers whose upstream URL
    // targets AAG's own listen address, so a failover chain can't loop incoming
    // requests back into this same proxy.
    let mut queue_ids: Vec<String> = Vec::new();
    let mut queues: HashMap<String, Vec<String>> = HashMap::new();
    // Per-app-provider model mappings (`settings_config.modelMapping`, e.g.
    // `{"gemini-2.5-pro": "gemini-2.5-pro-preview-06-05"}`) become routes
    // alias -> single hop on the synthesized queue provider. This is the
    // provider-config-driven model mapping for agents whose client model
    // differs from the upstream's (Gemini CLI preview-version pinning,
    // cross-wire rerouting like gemini-2.5-pro -> claude-sonnet-5).
    let mut queue_routes: HashMap<String, Vec<crate::database::dao::routes::RouteHop>> =
        HashMap::new();
    for (app_type, surface) in [
        ("claude_code", crate::proxy::router::SURFACE_CLAUDE),
        ("claude_vscode", crate::proxy::router::SURFACE_CLAUDE),
        ("codex", crate::proxy::router::SURFACE_CODEX),
        ("gemini", crate::proxy::router::SURFACE_GEMINI),
        ("copilot", crate::proxy::router::SURFACE_CHAT),
        ("pi", crate::proxy::router::SURFACE_CHAT),
        ("opencode", crate::proxy::router::SURFACE_CHAT),
        ("kilo_cli", crate::proxy::router::SURFACE_CHAT),
        ("kilo_vscode", crate::proxy::router::SURFACE_CHAT),
    ] {
        for (_, prov) in db.get_all_providers(app_type).map_err(String::from)? {
            if !prov.in_failover_queue {
                continue;
            }
            let (base_url, api_key, provider_type) = match queue_endpoint_of(
                app_type,
                &prov.settings_config,
            ) {
                Some(v) => v,
                None => {
                    log::warn!(
                        "proxy: failover chain skip provider_ref={} app_type={} reason=upstream_unavailable",
                        crate::diagnostics::safe_reference(&prov.id),
                        app_type
                    );
                    continue;
                }
            };
            // Match host:port, not the full URL — loopback aliases and
            // full-endpoint bases (e.g. `http://127.0.0.1:8989/v1/messages`)
            // all resolve to the same listener and must trip the guard.
            if is_self_loop(&base_url, config) {
                log::warn!(
                    "proxy: failover chain skip provider_ref={} app_type={} reason=self_loop",
                    crate::diagnostics::safe_reference(&prov.id),
                    app_type
                );
                continue;
            }
            let pid = format!("queue-{}", prov.id);
            let pp = crate::database::dao::proxy_providers::ProxyProvider {
                id: pid.clone(),
                name: format!("{} (failover queue)", prov.name),
                provider_type,
                api_key,
                base_url,
                models: vec![],
                enabled: true,
                timeout_ms: 60_000,
                weight: 1,
                circuit_breaker: serde_json::from_str("{}").unwrap_or_default(),
                model_mapping: Default::default(),
                extra_headers: Default::default(),
            };
            match crate::proxy::providers::build_provider(&pp) {
                Ok(p) => {
                    providers.insert(pid.clone(), Arc::from(p));
                    // Register the provider's model mappings as routes onto
                    // this queue hop (first registered wins across apps, so
                    // an alias maps to the head-most provider that declares it).
                    if let Some(mapping) = prov.settings_config.get("modelMapping").and_then(|m| m.as_object()) {
                        for (display, upstream) in mapping {
                            let Some(upstream) = upstream.as_str() else { continue };
                            if display.is_empty() || upstream.is_empty() {
                                continue;
                            }
                            queue_routes
                                .entry(display.clone())
                                .or_insert_with(|| {
                                    vec![crate::database::dao::routes::RouteHop {
                                        provider: pid.clone(),
                                        model: upstream.to_string(),
                                    }]
                                });
                        }
                    }
                    // Legacy global queue keeps the claude_code chain (back-comat
                    // with tests that assert on the /health failover queue).
                    if app_type == "claude_code" {
                        queue_ids.push(pid.clone());
                    }
                    // Append in configured (sort_index) order — the head is
                    // always the first enabled queue row, never the
                    // `is_current` marker, so requests always start from the
                    // first position even after a failover.
                    queues.entry(surface.to_string()).or_default().push(pid);
                }
                Err(_) => log::warn!(
                    "proxy: failover chain skip provider_ref={} app_type={} reason=provider_build_failed",
                    crate::diagnostics::safe_reference(&prov.id),
                    app_type
                ),
            }
        }
    }
    Ok((queue_ids, queues, queue_routes))
}

/// Port-fallback bind loop: try the configured port, then +1..+5. Each
/// candidate is bound via `serve_detached` and confirmed with a `/health`
/// probe so we never land on a port owned by another process.
fn health_probe_request(
    client: &reqwest::Client,
    url: &str,
    auth_token: &str,
) -> reqwest::RequestBuilder {
    let request = client.get(url);
    if auth_token.is_empty() {
        request
    } else {
        request.bearer_auth(auth_token)
    }
}

async fn bind_with_fallback(
    app_state: &crate::proxy::server::ProxyAppState,
    config: &crate::database::dao::proxy_config::ProxyConfig,
) -> Result<
    (
        crate::proxy::server::BoundInfo,
        tokio::task::JoinHandle<Result<(), String>>,
        tokio::sync::oneshot::Sender<()>,
    ),
    String,
> {
    // Port-fallback: try requested_port, then +1..+5 in order. For each
    // candidate, bind via serve_detached (bind outcome known immediately —
    // no readiness guessing), probe /health to confirm we own the port
    // (avoids landing on another app), then persist the actual port.
    const MAX_PORT_OFFSET: u16 = 5;
    let mut failed_candidates = 0_u16;
    let mut chosen: Option<(
        crate::proxy::server::BoundInfo,
        tokio::task::JoinHandle<Result<(), String>>,
        tokio::sync::oneshot::Sender<()>,
    )> = None;
    let original_port = config.port;
    for offset in 0..=MAX_PORT_OFFSET {
        let try_port = original_port.wrapping_add(offset);
        let try_addr = crate::proxy::server::socket_addr(&config.host, try_port);
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let (bound, handle) =
            match crate::proxy::server::serve_detached(try_addr, app_state.clone(), shutdown_rx)
                .await
            {
                Ok(v) => v,
                Err(_) => {
                    failed_candidates = failed_candidates.saturating_add(1);
                    continue;
                }
            };
        // Confirm we actually own the port via /health (rules out the
        // Windows TIME_WAIT trap where the port answers but isn't ours).
        let probe = match reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(500))
            .build()
        {
            Ok(c) => c,
            Err(_) => {
                failed_candidates = failed_candidates.saturating_add(1);
                let _ = shutdown_tx.send(());
                let _ = handle.await;
                continue;
            }
        };
        let probe_url = format!("http://{}/health", bound.addr);
        let probe_ok = health_probe_request(&probe, &probe_url, &config.auth_token)
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false);
        if !probe_ok {
            log::warn!(
                "proxy: health probe failed port={} reason=non_success_or_refused",
                bound.addr.port()
            );
            failed_candidates = failed_candidates.saturating_add(1);
            let _ = shutdown_tx.send(());
            let _ = handle.await;
            continue;
        }
        chosen = Some((bound, handle, shutdown_tx));
        break;
    }
    chosen.ok_or_else(|| {
        let _ = failed_candidates;
        "proxy_bind_exhausted".to_string()
    })
}

#[tauri::command]
pub(crate) async fn proxy_stop(
    state: tauri::State<'_, Mutex<Option<RunningProxy>>>,
) -> Result<(), String> {
    let mut guard = state.lock().await;
    if let Some(rp) = guard.take() {
        rp.shutdown
            .send(())
            .map_err(|_| "proxy_shutdown_signal_failed".to_string())?;
        // Wait for the server task to finish gracefully.
        let result = rp
            .handle
            .await
            .map_err(|_| "proxy_server_task_join_failed".to_string())?;
        result?;
    }
    Ok(())
}

use crate::database::dao::proxy_config::ProxyConfig;

// ----- Proxy providers + Routes CRUD (failover config) -----
// The Router is rebuilt from the DB every time `proxy_start` runs, so saving
// a provider or route then restarting the proxy applies the new failover chain.

#[tauri::command]
pub(crate) fn proxy_provider_list(
    db: tauri::State<'_, Database>,
) -> Result<Vec<crate::database::dao::proxy_providers::ProxyProvider>, String> {
    db.get_all_proxy_providers().map_err(String::from)
}

#[tauri::command]
pub(crate) fn proxy_provider_save(
    db: tauri::State<'_, Database>,
    provider: crate::database::dao::proxy_providers::ProxyProvider,
) -> Result<(), String> {
    db.save_proxy_provider(&provider).map_err(String::from)
}

#[tauri::command]
pub(crate) fn proxy_provider_delete(
    db: tauri::State<'_, Database>,
    id: String,
) -> Result<(), String> {
    db.delete_proxy_provider(&id).map_err(String::from)
}

#[tauri::command]
pub(crate) fn route_list(
    db: tauri::State<'_, Database>,
) -> Result<Vec<crate::database::dao::routes::Route>, String> {
    db.get_all_routes().map_err(String::from)
}

#[tauri::command]
pub(crate) fn route_save(
    db: tauri::State<'_, Database>,
    route: crate::database::dao::routes::Route,
) -> Result<(), String> {
    db.save_route(&route).map_err(String::from)
}

#[tauri::command]
pub(crate) fn route_delete(db: tauri::State<'_, Database>, alias: String) -> Result<(), String> {
    db.delete_route(&alias).map_err(String::from)
}

/// Rename a route's alias in place (hops/description ride along).
/// Rejects a target alias that already exists (PK conflict) or an
/// unknown source.
#[tauri::command]
pub(crate) fn route_rename(
    db: tauri::State<'_, Database>,
    from: String,
    to: String,
) -> Result<(), String> {
    let to = to.trim();
    if to.is_empty() {
        return Err("new alias must not be empty".into());
    }
    if to == from {
        return Ok(());
    }
    db.rename_route(&from, to).map_err(String::from)
}

/// One-shot import of all providers from the cc-switch SQLite DB at the
/// default path (`~/.cc-switch/cc-switch.db` or `CC_SWITCH_DB_PATH`).
/// Idempotent: rows whose `(id, app_type)` already exists in AAG are
/// skipped, not overwritten. Returns a structured report so the UI can
/// toast the number imported + skipped.
#[tauri::command]
pub(crate) fn import_cc_switch_providers(
    db: tauri::State<'_, Database>,
) -> Result<crate::services::import_cc_switch::ImportReport, String> {
    let path = crate::services::import_cc_switch::cc_switch_db_path();
    crate::services::import_cc_switch::import_from_db(&db, &path).map_err(String::from)
}

/// DSH consumers use the actual active listener and model snapshot.
pub(crate) async fn dsh_gateway_snapshot(
    state: &Mutex<Option<RunningProxy>>,
) -> Result<Option<crate::services::dsh::upstreams::Gateway_snapshot>, String> {
    use crate::services::dsh::upstreams::{Gateway_route, Gateway_snapshot};
    let guard = state.lock().await;
    let Some(running) = guard
        .as_ref()
        .filter(|running| !running.handle.is_finished())
    else {
        return Ok(None);
    };
    if running
        .state
        .pinned_provider
        .lock()
        .map_err(|_| "dsh:proxy_state_lock_failed")?
        .is_some()
    {
        return Ok(None);
    }
    let host = match running.config.host.as_str() {
        "127.0.0.1" | "localhost" | "0.0.0.0" => "127.0.0.1",
        "::" | "::1" => "[::1]",
        _ => return Ok(None),
    };
    Ok(Some(Gateway_snapshot {
        base_url: format!("http://{host}:{}", running.config.port),
        auth_key: running.config.auth_token.clone(),
        routes: running
            .state
            .router
            .list_models()
            .into_iter()
            .map(|alias| Gateway_route {
                alias,
                protocol: "anthropic-messages".into(),
            })
            .collect(),
    }))
}

#[cfg(test)]
mod tests {
    use super::{bind_with_fallback, health_probe_request};
    use crate::database::dao::proxy_config::ProxyConfig;
    use crate::database::dao::proxy_providers::CircuitBreakerConfig;
    use crate::proxy::providers::mock::MockProvider;
    use crate::proxy::router::Router;
    use crate::proxy::server::ProxyAppState;
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::oneshot;

    #[test]
    fn health_probe_reuses_configured_gateway_auth_without_auth_when_disabled() {
        let client = reqwest::Client::new();
        let authenticated = health_probe_request(
            &client,
            "http://127.0.0.1:8989/health",
            "fixture-gateway-token",
        )
        .build()
        .unwrap();
        assert_eq!(
            authenticated
                .headers()
                .get(reqwest::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok()),
            Some("Bearer fixture-gateway-token")
        );

        let unauthenticated = health_probe_request(&client, "http://127.0.0.1:8989/health", "")
            .build()
            .unwrap();
        assert!(unauthenticated
            .headers()
            .get(reqwest::header::AUTHORIZATION)
            .is_none());
    }

    #[tokio::test]
    async fn authenticated_health_probe_accepts_the_bound_proxy() {
        let mut providers: HashMap<String, Arc<dyn crate::proxy::providers::Provider>> =
            HashMap::new();
        providers.insert(
            "mock".into(),
            Arc::new(MockProvider::new(
                "mock".into(),
                vec!["m".into()],
                true,
                CircuitBreakerConfig::default(),
            )),
        );
        let state = ProxyAppState {
            router: Arc::new(Router::new(providers, HashMap::new())),
            auth_token: "fixture-gateway-token".into(),
            pinned_provider: Arc::new(std::sync::Mutex::new(None)),
            responses_cot_enabled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            passthrough_auth_allowed: false,
            db: None,
        };
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let (bound, handle) = crate::proxy::server::serve_detached(
            "127.0.0.1:0".parse().unwrap(),
            state,
            shutdown_rx,
        )
        .await
        .unwrap();
        let client = reqwest::Client::new();
        let response = health_probe_request(
            &client,
            &format!("http://{}/health", bound.addr),
            "fixture-gateway-token",
        )
        .send()
        .await
        .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        shutdown_tx.send(()).unwrap();
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn bind_with_fallback_accepts_an_authenticated_health_probe() {
        let mut providers: HashMap<String, Arc<dyn crate::proxy::providers::Provider>> =
            HashMap::new();
        providers.insert(
            "mock".into(),
            Arc::new(MockProvider::new(
                "mock".into(),
                vec!["m".into()],
                true,
                CircuitBreakerConfig::default(),
            )),
        );
        let state = ProxyAppState {
            router: Arc::new(Router::new(providers, HashMap::new())),
            auth_token: "fixture-gateway-token".into(),
            pinned_provider: Arc::new(std::sync::Mutex::new(None)),
            responses_cot_enabled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            passthrough_auth_allowed: false,
            db: None,
        };
        let config = ProxyConfig {
            host: "127.0.0.1".into(),
            port: 0,
            auth_token: "fixture-gateway-token".into(),
            enabled: true,
        };
        let (bound, handle, shutdown_tx) = bind_with_fallback(&state, &config).await.unwrap();
        assert!(bound.addr.port() > 0);
        shutdown_tx.send(()).unwrap();
        handle.await.unwrap().unwrap();
    }
}

#[cfg(test)]
mod failover_queue_tests {
    use super::*;
    use crate::database::dao::proxy_config::ProxyConfig;
    use crate::proxy::providers::Provider as Provider_instance;

    /// The failover chain must follow the configured (sort_index) order and
    /// the `is_current` marker must never jump to the chain head — every
    /// request starts from the first configured position, and a deliberate
    /// user switch (or a persisted failover winner) must not reorder it.
    #[test]
    fn queue_follows_configured_order_and_current_marker_never_jumps_the_head() {
        let db = Database::memory().expect("in-memory database");
        // Configured order: c (0), a (1), b (2).
        for (id, sort_index, port) in [("a", 1_usize, 9001_u16), ("b", 2, 9002), ("c", 0, 9003)] {
            let mut row = crate::provider::Provider::new(
                id.into(),
                format!("row {id}"),
                serde_json::json!({
                    "env": {
                        "ANTHROPIC_BASE_URL": format!("http://127.0.0.1:{port}"),
                        "ANTHROPIC_AUTH_TOKEN": "fixture",
                    }
                }),
            );
            row.sort_index = Some(sort_index);
            db.save_provider("claude_code", &row)
                .expect("save provider row");
        }
        // Deliberate user switch to "b": the marker is UI/live-config state
        // and must not put "b" at the head of the routing chain.
        db.set_current_provider("b", "claude_code")
            .expect("set current");

        let config = ProxyConfig {
            host: "127.0.0.1".into(),
            port: 8989,
            auth_token: String::new(),
            enabled: true,
        };
        let mut providers: HashMap<String, Arc<dyn Provider_instance>> = HashMap::new();
        let (queue_ids, queues, _) =
            build_failover_queues(&db, &config, &mut providers).expect("build queues");

        let chain = queues
            .get(crate::proxy::router::SURFACE_CLAUDE)
            .expect("claude surface chain");
        assert_eq!(
            chain,
            &[
                "queue-c".to_string(),
                "queue-a".to_string(),
                "queue-b".to_string(),
            ],
            "chain must follow configured sort_index order, not the current marker"
        );
        assert_eq!(
            &queue_ids, chain,
            "legacy global queue mirrors the claude_code chain"
        );
        assert_eq!(
            providers.len(),
            3,
            "every queue row builds a provider instance"
        );
    }

    /// Self-loop guard: a provider whose upstream URL targets AAG's own
    /// listener must be excluded from the chain (it would loop every client
    /// call back into the proxy until the per-hop timeout fires), and rows
    /// with `in_failover_queue = 0` stay out as well.
    #[test]
    fn queue_excludes_self_loop_and_opted_out_rows() {
        let db = Database::memory().expect("in-memory database");
        for (id, base_url, in_queue) in [
            ("ok", "http://127.0.0.1:9001", true),
            // Same host:port as the proxy config below — self-loop.
            ("loopback", "http://127.0.0.1:8989", true),
            // localhost alias of the same listener — also a self-loop.
            ("alias", "http://localhost:8989", true),
            // Fine upstream, but the user opted it out of the chain.
            ("opted_out", "http://127.0.0.1:9002", false),
        ] {
            let mut row = crate::provider::Provider::new(
                id.into(),
                format!("row {id}"),
                serde_json::json!({
                    "env": {
                        "ANTHROPIC_BASE_URL": base_url,
                        "ANTHROPIC_AUTH_TOKEN": "fixture",
                    }
                }),
            );
            row.in_failover_queue = in_queue;
            db.save_provider("claude_code", &row)
                .expect("save provider row");
        }

        let config = ProxyConfig {
            host: "127.0.0.1".into(),
            port: 8989,
            auth_token: String::new(),
            enabled: true,
        };
        let mut providers: HashMap<String, Arc<dyn Provider_instance>> = HashMap::new();
        let (_, queues, _) =
            build_failover_queues(&db, &config, &mut providers).expect("build queues");

        let chain = queues
            .get(crate::proxy::router::SURFACE_CLAUDE)
            .expect("claude surface chain");
        assert_eq!(
            chain,
            &["queue-ok".to_string()],
            "only the legitimate upstream survives the guards"
        );
        assert_eq!(
            providers.len(),
            1,
            "excluded rows build no provider instance"
        );
    }

    #[test]
    fn self_loop_detects_loopback_aliases_and_full_endpoints() {
        let config = ProxyConfig {
            host: "127.0.0.1".into(),
            port: 8989,
            auth_token: String::new(),
            enabled: true,
        };
        // Loopback host, exact port.
        assert!(is_self_loop("http://127.0.0.1:8989", &config));
        // localhost alias of the same listener.
        assert!(is_self_loop("http://localhost:8989", &config));
        // Full-endpoint base (the form this fix now accepts) targeting the
        // same listener.
        assert!(is_self_loop("http://127.0.0.1:8989/v1/messages", &config));
        // A path only, still the same host:port.
        assert!(is_self_loop("http://127.0.0.1:8989/v1", &config));
        // Different port is a different service.
        assert!(!is_self_loop("http://127.0.0.1:9001", &config));
        // A real upstream.
        assert!(!is_self_loop("https://api.minimaxi.com", &config));
        // Unparseable / empty input never loops.
        assert!(!is_self_loop("not a url", &config));
        assert!(!is_self_loop("", &config));
        // Empty config host disables the guard.
        let empty = ProxyConfig {
            host: String::new(),
            port: 8989,
            auth_token: String::new(),
            enabled: true,
        };
        assert!(!is_self_loop("http://127.0.0.1:8989", &empty));
    }
}
