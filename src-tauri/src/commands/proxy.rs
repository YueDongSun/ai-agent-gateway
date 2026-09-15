//! Proxy lifecycle commands: start/stop/status/config, provider + route CRUD, cc-switch import.
//!
//! Extracted from `lib.rs` in the file-structure refactor - the command
//! bodies are moved verbatim; only module paths in `use` statements and
//! `pub(crate)` visibility changed.


use crate::commands::provider::queue_endpoint_of;
use crate::database::Database;
use crate::provider::Provider;
use crate::proxy;
use crate::services;
use serde::Serialize;
use tauri::Manager;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

// ----- Proxy commands (Phase 2) -----

/// Handle to a running proxy server. Stored as Tauri managed state.
pub(crate) struct RunningProxy {
    handle: tokio::task::JoinHandle<Result<(), String>>,
    shutdown: tokio::sync::oneshot::Sender<()>,
    config: crate::database::dao::proxy_config::ProxyConfig,
    state: crate::proxy::server::ProxyAppState,
}

#[derive(Serialize)]
pub(crate) struct ProxyStatusDto {
    running: bool,
    config: crate::database::dao::proxy_config::ProxyConfig,
}

#[tauri::command]
pub(crate) async fn proxy_status(state: tauri::State<'_, Mutex<Option<RunningProxy>>>) -> Result<ProxyStatusDto, String> {
    let guard = state.lock().await;
    let config = match &*guard {
        Some(rp) => rp.config.clone(),
        None => ProxyConfig::default(),
    };
    Ok(ProxyStatusDto { running: guard.is_some(), config })
}

/// Read the proxy listen config + auth token (for the endpoint card the
/// user copies into cc-switch).
#[tauri::command]
pub(crate) fn proxy_config_get(
    db: tauri::State<'_, Database>,
) -> Result<ProxyConfig, String> {
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
            let pinned = running.state.pinned_provider.lock().map_err(|e| format!("lock: {e}"))?;
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
        let mut pp = running.state.pinned_provider.lock().map_err(|e| format!("lock: {e}"))?;
        *pp = pinned;
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
        .ok_or_else(|| format!("proxy provider \"{provider_id}\" not found"))?;

    if !pp.enabled {
        return Ok(serde_json::json!({
            "success": false,
            "error": "provider is disabled",
            "latencyMs": 0,
        }));
    }

    // Build the test request. Use a minimal prompt to minimize token usage.
    let model = test_model.unwrap_or_else(|| {
        pp.models.first().cloned().unwrap_or_else(|| "gpt-3.5-turbo".into())
    });

    let provider_instance = crate::proxy::providers::build_provider(&pp)
        .map_err(|e| format!("build provider: {e}"))?;

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
    };

    let start = Instant::now();
    match provider_instance.complete(&req).await {
        Ok(resp) => {
            let latency = start.elapsed().as_millis() as u64;
            let reply = resp.choices.first()
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
        .ok_or_else(|| format!("proxy provider \"{provider_id}\" not found"))?;

    let base_url = pp.base_url.trim_end_matches('/');
    // Try common model list endpoints.
    let candidates = vec![
        format!("{}/v1/models", base_url),
        format!("{}/models", base_url),
    ];

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| format!("http client: {e}"))?;

    let mut last_err = String::new();
    for url in &candidates {
        let mut req = client.get(url);
        if !pp.api_key.is_empty() {
            req = req.header("authorization", format!("Bearer {}", pp.api_key));
        }
        match req.send().await {
            Ok(resp) if resp.status().is_success() => {
                let body: serde_json::Value = resp.json().await
                    .map_err(|e| format!("parse response: {e}"))?;
                let models = body.get("data")
                    .and_then(|d| d.as_array())
                    .cloned()
                    .unwrap_or_default();
                return Ok(models);
            }
            Ok(resp) => {
                last_err = format!("HTTP {}: {}", resp.status(), url);
            }
            Err(e) => {
                last_err = format!("request failed {}: {}", url, e);
            }
        }
    }
    Err(format!("could not fetch models from any endpoint: {last_err}"))
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

    let (mut providers, routes) = build_providers_and_routes(db).await?;
    let (queue_ids, queues) = build_failover_queues(db, &config, &mut providers)?;

    let router = Arc::new(
        crate::proxy::router::Router::new(providers, routes)
            .with_queue(queue_ids.clone())
            .with_queues(queues),
    );
    let pinned = db
        .get_setting(crate::database::dao::settings::KEY_PROXY_PINNED_PROVIDER)
        .ok()
        .flatten();
    let app_state = crate::proxy::server::ProxyAppState {
        router,
        auth_token: config.auth_token.clone(),
        pinned_provider: Arc::new(std::sync::Mutex::new(pinned)),
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
            log::warn!("proxy: could not persist fallback port {}: {}", updated.port, e);
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
        format!("listening on {}:{}", bound.addr.ip(), bound.addr.port())
    } else {
        format!(
            "listening on {}:{} (port {} was in use; saved as the new default)",
            bound.addr.ip(),
            bound.addr.port(),
            config.port
        )
    };
    if !queue_ids.is_empty() {
        msg.push_str(&format!(" · failover queue: {} provider(s)", queue_ids.len()));
    }
    Ok(msg)
}



/// Build the provider-instance map and the routes map from the DB, applying
/// the Phase 22.1/22.2 hot-fixes ([1M] strip on route hops, breaker tighten
/// on imported `ccs-*` providers) plus the two auto-route sources (active
/// Codex provider's modelCatalog, each proxy_provider's model_mapping).
async fn build_providers_and_routes(
    db: &Database,
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
            && (pp.circuit_breaker.failure_threshold > 3
                || pp.circuit_breaker.cooldown_ms > 15_000)
        {
            log::info!(
                "proxy: hot-fix provider \"{}\" — tighten breaker {} failures/{}ms → 3/15000ms",
                pp.id, pp.circuit_breaker.failure_threshold, pp.circuit_breaker.cooldown_ms
            );
            pp.circuit_breaker = tightened_cb.clone();
            if let Err(e) = db.save_proxy_provider(pp) {
                log::warn!("proxy: hot-fix provider \"{}\" failed: {e}", pp.id);
            }
        }
    }

    let mut providers: HashMap<String, Arc<dyn crate::proxy::providers::Provider>> = HashMap::new();
    for pp in &db_providers {
        match crate::proxy::providers::build_provider(pp) {
            Ok(p) => {
                providers.insert(pp.id.clone(), Arc::from(p));
            }
            Err(e) => {
                log::warn!("proxy: skip provider {}: {}", pp.id, e);
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
        if cleaned.iter().zip(r.hops.iter()).any(|(c, h)| c.model != h.model) {
            let mut fixed = r.clone();
            fixed.hops = cleaned.clone();
            if let Err(e) = db.save_route(&fixed) {
                log::warn!("proxy: hot-fix route \"{}\" failed: {e}", r.alias);
            } else {
                log::info!(
                    "proxy: hot-fix route \"{}\" — stripped [1M] from {} hop model(s)",
                    r.alias,
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
                        if provider_id.is_empty() { continue; }
                        // Skip if the route already exists (manual route takes priority).
                        if routes.contains_key(&display) { continue; }
                        // Skip if the referenced proxy_provider doesn't exist.
                        let prov_exists = providers.contains_key(&provider_id);
                        if !prov_exists {
                            log::warn!(
                                "proxy: modelCatalog skip \"{}\" -> \"{}\": proxy_provider \"{}\" not found",
                                display, upstream_model, provider_id
                            );
                            continue;
                        }
                        log::info!(
                            "proxy: modelCatalog auto-route \"{}\" -> {}/{}",
                            display, provider_id, upstream_model
                        );
                        routes.insert(display, vec![crate::database::dao::routes::RouteHop {
                            provider: provider_id,
                            model: upstream_model,
                        }]);
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
        if pp.model_mapping.is_empty() { continue; }
        for (display, upstream_model) in &pp.model_mapping {
            if routes.contains_key(display) { continue; }
            log::info!(
                "proxy: model_mapping auto-route \"{}\" -> {}/{}",
                display, pp.id, upstream_model
            );
            routes.insert(display.clone(), vec![crate::database::dao::routes::RouteHop {
                provider: pp.id.clone(),
                model: upstream_model.clone(),
            }]);
        }
    }
    Ok((providers, routes))
}

/// Build the per-surface failover queues (Phase 17) from the `providers`
/// table: each app's in-failover-queue rows (current provider first)
/// synthesize `queue-<id>` proxy_provider entries. Includes the self-loop
/// guard and the optional cc-switch auto-sync.
fn build_failover_queues(
    db: &Database,
    config: &crate::database::dao::proxy_config::ProxyConfig,
    providers: &mut HashMap<String, Arc<dyn crate::proxy::providers::Provider>>,
) -> Result<(Vec<String>, HashMap<String, Vec<String>>), String> {
    // Per-app failover chains (Phase 17, cc-switch style): each app's
    // provider list (current provider first, then homepage order, only rows
    // with in_failover_queue = 1) becomes a failover queue for its entry
    // surface:
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
            Err(e) => log::warn!("proxy: cc-switch auto-sync failed: {e}"),
        }
    }

    // Self-loop guard: skip providers whose upstream URL targets AAG's own
    // listen address (host:port). Without this, a failover chain can include
    // an entry that loops incoming requests back into this same proxy —
    // every client call would stall in a circle until the per-hop timeout
    // fires, surfacing as a client "port" hang.
    let aag_listen = format!("{}:{}", config.host, config.port);
    let mut queue_ids: Vec<String> = Vec::new();
    let mut queues: HashMap<String, Vec<String>> = HashMap::new();
    let current_by_app: HashMap<&str, Option<String>> = [
        ("claude_code", db.get_current_provider("claude_code").ok().flatten()),
        ("codex", db.get_current_provider("codex").ok().flatten()),
        ("copilot", db.get_current_provider("copilot").ok().flatten()),
        ("pi", db.get_current_provider("pi").ok().flatten()),
    ]
    .into_iter()
    .collect();
    for (app_type, surface) in [
        ("claude_code", crate::proxy::router::SURFACE_CLAUDE),
        ("codex", crate::proxy::router::SURFACE_CODEX),
        ("copilot", crate::proxy::router::SURFACE_CHAT),
        ("pi", crate::proxy::router::SURFACE_CHAT),
    ] {
        let current_id = current_by_app.get(app_type).cloned().flatten();
        for (_, prov) in db.get_all_providers(app_type).map_err(String::from)? {
            if !prov.in_failover_queue {
                continue;
            }
            let (base_url, api_key, provider_type) = match queue_endpoint_of(app_type, &prov.settings_config) {
                Some(v) => v,
                None => {
                    log::warn!(
                        "proxy: failover chain skip \"{}\" ({}): no upstream URL in settings_config",
                        prov.id,
                        app_type
                    );
                    continue;
                }
            };
            // Match host:port, not the full URL — `http://127.0.0.1:8989/v1`
            // and `http://localhost:8989` both contain `127.0.0.1:8989` /
            // `localhost:8989`. Compare against both AAG's host:port and
            // the loopback equivalents so any alias the user typed still
            // trips the guard.
            let base_trim = base_url
                .split("//")
                .nth(1)
                .unwrap_or(&base_url)
                .split('/')
                .next()
                .unwrap_or("")
                .to_string();
            let is_self_loop = !aag_listen.is_empty()
                && (base_trim == aag_listen
                    || (config.host == "127.0.0.1" && base_trim == format!("localhost:{}", config.port))
                    || (config.host == "localhost" && base_trim == format!("127.0.0.1:{}", config.port)));
            if is_self_loop {
                log::warn!(
                    "proxy: failover chain skip \"{}\": upstream ({}) targets AAG itself at {} — would loop",
                    prov.id, base_url, aag_listen
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
            };
            match crate::proxy::providers::build_provider(&pp) {
                Ok(p) => {
                    providers.insert(pid.clone(), Arc::from(p));
                    // Legacy global queue keeps the claude_code chain (back-comat
                    // with tests that assert on the /health failover queue).
                    if app_type == "claude_code" {
                        queue_ids.push(pid.clone());
                    }
                    let chain = queues.entry(surface.to_string()).or_default();
                    // Chain head = the app's current provider; everyone else
                    // appends in homepage (sort) order.
                    if current_id.as_deref() == Some(prov.id.as_str()) {
                        chain.insert(0, pid);
                    } else {
                        chain.push(pid);
                    }
                }
                Err(e) => log::warn!("proxy: failover chain skip \"{}\": {e}", prov.id),
            }
        }
    }
    Ok((queue_ids, queues))
}

/// Port-fallback bind loop: try the configured port, then +1..+5. Each
/// candidate is bound via `serve_detached` and confirmed with a `/health`
/// probe so we never land on a port owned by another process.
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
    let mut errors: Vec<String> = Vec::new();
    let mut chosen: Option<(crate::proxy::server::BoundInfo, tokio::task::JoinHandle<Result<(), String>>, tokio::sync::oneshot::Sender<()>)> = None;
    let original_port = config.port;
    for offset in 0..=MAX_PORT_OFFSET {
        let try_port = original_port.wrapping_add(offset);
        let try_addr = crate::proxy::server::socket_addr(&config.host, try_port);
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let (bound, handle) = match crate::proxy::server::serve_detached(try_addr, app_state.clone(), shutdown_rx).await {
            Ok(v) => v,
            Err(e) => {
                errors.push(format!("port {try_port}: {e}"));
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
            Err(e) => {
                errors.push(format!("port {try_port}: reqwest build: {e}"));
                let _ = shutdown_tx.send(());
                let _ = handle.await;
                continue;
            }
        };
        let probe_url = format!("http://{}/health", bound.addr);
        let probe_ok = probe.get(&probe_url).send().await.map(|r| r.status().is_success()).unwrap_or(false);
        if !probe_ok {
            log::warn!("proxy: {} answered /health with non-2xx (or refused), assuming another process owns it", bound.addr);
            errors.push(format!("port {try_port}: owned by another process"));
            let _ = shutdown_tx.send(());
            let _ = handle.await;
            continue;
        }
        chosen = Some((bound, handle, shutdown_tx));
        break;
    }
    chosen.ok_or_else(|| {
        format!(
            "no free port near {} (tried {}+0..+{}): {}",
            original_port,
            original_port,
            MAX_PORT_OFFSET,
            if errors.is_empty() { "unknown".into() } else { errors.join("; ") }
        )
    })
}


#[tauri::command]
pub(crate) async fn proxy_stop(state: tauri::State<'_, Mutex<Option<RunningProxy>>>) -> Result<(), String> {
    let mut guard = state.lock().await;
    if let Some(rp) = guard.take() {
        rp.shutdown.send(()).map_err(|_| "shutdown signal failed".to_string())?;
        // Wait for the server task to finish gracefully.
        let result = rp.handle.await.map_err(|e| e.to_string())?;
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
pub(crate) fn proxy_provider_delete(db: tauri::State<'_, Database>, id: String) -> Result<(), String> {
    db.delete_proxy_provider(&id).map_err(String::from)
}

#[tauri::command]
pub(crate) fn route_list(db: tauri::State<'_, Database>) -> Result<Vec<crate::database::dao::routes::Route>, String> {
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
