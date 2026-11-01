//! Proxy lifecycle commands: start/stop/status/config, provider + route CRUD, cc-switch import.
//!
//! Extracted from `lib.rs` in the file-structure refactor - the command
//! bodies are moved verbatim; only module paths in `use` statements and
//! `pub(crate)` visibility changed.

use crate::database::Database;
use crate::services::proxy_runtime::lifecycle::{Proxy_shutdown, Proxy_task};
use crate::services::tailnet_publication::Publication_service;
use serde::Serialize;
use std::sync::Arc;
use tauri::Manager;
use tokio::sync::Mutex;

// ----- Proxy commands (Phase 2) -----

/// Raw model identifiers stay in bounded memory and the trusted manager IPC.
#[tauri::command]
pub(crate) async fn proxy_request_status() -> Vec<crate::proxy::request_status::Request_status> {
    crate::proxy::request_status::snapshot()
}

/// Handle to a running proxy server. Stored as Tauri managed state.
pub(crate) struct RunningProxy {
    task: Proxy_task,
    config: crate::database::dao::proxy_config::ProxyConfig,
    configured_host: String,
    state: crate::proxy::server::ProxyAppState,
    live: Option<Arc<crate::services::proxy_runtime::live::Live_runtime>>,
}

impl RunningProxy {
    /// Actual bound port of an accepting runtime, never a pending saved value.
    pub(crate) fn actual_port(&self) -> Option<u16> {
        self.accepts_requests().then_some(self.config.port)
    }

    pub(crate) fn publication_state(&self) -> Option<crate::proxy::server::ProxyAppState> {
        self.accepts_requests().then(|| self.state.clone())
    }
    pub(crate) fn credentialless_models(&self, surface: &str) -> Option<Vec<String>> {
        if !self.accepts_requests() {
            return None;
        }
        if let Some(live) = &self.live {
            let snapshot = live.current();
            Some(
                snapshot
                    .router
                    .credentialless_models(surface, snapshot.pinned_provider.as_deref()),
            )
        } else {
            let pin = self.state.pinned_provider.lock().ok()?;
            Some(
                self.state
                    .router
                    .credentialless_models(surface, pin.as_deref()),
            )
        }
    }
    /// Router and pin from one admitted snapshot, for read-only UI previews.
    pub(crate) fn preview_snapshot(&self) -> Option<(Arc<crate::proxy::router::Router>, Option<String>)> {
        if !self.accepts_requests() { return None; }
        if let Some(live) = &self.live {
            let snapshot = live.current();
            Some((Arc::clone(&snapshot.router), snapshot.pinned_provider.clone()))
        } else {
            Some((Arc::clone(&self.state.router), self.state.pinned_provider.lock().ok()?.clone()))
        }
    }

    /// Signal shutdown immediately; dropping the waiter cannot cancel the server owner.
    pub(crate) fn request_shutdown(mut self) -> Proxy_shutdown {
        self.task.request_shutdown()
    }

    fn accepts_requests(&self) -> bool {
        !self.task.is_stopping() && !self.task.has_finished()
    }

    /// The router snapshot this gateway is accepting requests for, excluding
    /// a draining or finished server. Shared with prompt enhancement so it
    /// inherits the active providers, routes and breakers.
    pub(crate) fn live_router(&self) -> Option<Arc<crate::proxy::router::Router>> {
        if !self.accepts_requests() {
            return None;
        }
        Some(
            self.live
                .as_ref()
                .map(|live| Arc::clone(&live.current().router))
                .unwrap_or_else(|| Arc::clone(&self.state.router)),
        )
    }
}

#[derive(Serialize)]
pub(crate) struct ProxyStatusDto {
    running: bool,
    /// The config the running gateway actually uses; while stopped, the
    /// saved config the next start will use. Display, copy, and health
    /// polling can therefore always read this field instead of re-reading
    /// the saved config and guessing which one is live.
    config: crate::database::dao::proxy_config::ProxyConfig,
    config_changed: bool,
    /// Snapshot generation and the last stable reload error code, so the UI
    /// can distinguish saved / pending-apply / applied / failed without a
    /// restart. Absent while no live runtime is active.
    hot_reload: Option<crate::services::proxy_runtime::live::Live_runtime_status>,
}

#[tauri::command]
pub(crate) async fn proxy_status(
    state: tauri::State<'_, Mutex<Option<RunningProxy>>>,
    db: tauri::State<'_, Database>,
) -> Result<ProxyStatusDto, String> {
    read_proxy_status(state.inner(), db.inner()).await
}

async fn read_proxy_status(
    state: &Mutex<Option<RunningProxy>>,
    db: &Database,
) -> Result<ProxyStatusDto, String> {
    let guard = state.lock().await;
    let running = guard
        .as_ref()
        .filter(|running| !running.task.has_finished());
    let config = match running {
        Some(running) => running.config.clone(),
        None => db.get_proxy_config().map_err(String::from)?,
    };
    let config_changed = if let Some(running) = running {
        let saved_config = db.get_proxy_config().map_err(String::from)?;
        let active_revision = running
            .live
            .as_ref()
            .map(|live| live.routing_revision())
            .unwrap_or_else(|| db.routing_revision().unwrap_or(0));
        active_revision != db.routing_revision().map_err(String::from)?
            || saved_config.host != running.configured_host
            || saved_config.port != running.config.port
            || saved_config.auth_token != running.config.auth_token
    } else {
        false
    };
    Ok(ProxyStatusDto {
        running: running.is_some(),
        config,
        config_changed,
        hot_reload: running.and_then(|running| running.live.as_ref().map(|live| live.status())),
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
    db: tauri::State<'_, Database>,
) -> Result<Option<String>, String> {
    get_pinned_provider_impl(state.inner(), db.inner()).await
}

async fn get_pinned_provider_impl(
    state: &Mutex<Option<RunningProxy>>,
    db: &Database,
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
        // The pin is persisted as a routing input and takes effect on the
        // next start; report it while stopped so the badge and toggle stay
        // truthful instead of suggesting no pin exists.
        None => Ok(db
            .get_setting(crate::database::dao::settings::KEY_PROXY_PINNED_PROVIDER)
            .map_err(String::from)?
            .filter(|id| !id.trim().is_empty())),
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
    set_pinned_provider_impl(&db, state.inner(), provider_id).await
}

async fn set_pinned_provider_impl(
    db: &Database,
    state: &Mutex<Option<RunningProxy>>,
    provider_id: String,
) -> Result<(), String> {
    let pinned = if provider_id.trim().is_empty() {
        None
    } else {
        Some(provider_id.trim().to_string())
    };
    // Persist to settings so it survives restarts. The pin is part of the
    // routing fingerprint, so the write alone marks the runtime stale.
    db.set_setting(
        crate::database::dao::settings::KEY_PROXY_PINNED_PROVIDER,
        pinned.as_deref().unwrap_or(""),
    )
    .map_err(String::from)?;
    // Keep the direct-toggle view in sync for status reads while the hot
    // reload publishes the new snapshot generation.
    {
        let guard = state.lock().await;
        if let Some(running) = guard.as_ref() {
            let mut pp = running
                .state
                .pinned_provider
                .lock()
                .map_err(|_| "proxy_state_lock_failed".to_string())?;
            *pp = pinned;
        }
    }
    runtime_reload(state).await
}

/// Read the failover-queue spread mode. The settings row is the source of
/// truth (it feeds the routing fingerprint and the snapshot build), so this
/// reads it directly and reflects the stored value whether or not the proxy
/// is running. Unknown or absent values decode as `"order"`.
#[tauri::command]
pub(crate) fn proxy_get_routing_mode(db: tauri::State<'_, Database>) -> Result<String, String> {
    Ok(crate::proxy::router::Routing_mode::parse(
        db.get_setting(crate::database::dao::settings::KEY_ROUTING_MODE)
            .map_err(String::from)?
            .as_deref(),
    )
    .as_str()
    .to_string())
}

/// Set the failover-queue spread mode: `"order"` (configured order),
/// `"rotate"` (round-robin), `"weight"` (smooth weighted round-robin), or
/// `"usage"` (least recorded token traffic over the last hour first).
/// Only queue-built chains are spread; explicit routes and planner-assessed
/// alias candidates keep their order. The mode is part of the routing
/// fingerprint, so the write alone marks the runtime stale and the reload
/// publishes it to new requests.
#[tauri::command]
pub(crate) async fn proxy_set_routing_mode(
    db: tauri::State<'_, Database>,
    state: tauri::State<'_, Mutex<Option<RunningProxy>>>,
    mode: String,
) -> Result<(), String> {
    let parsed = match mode.trim() {
        "order" => crate::proxy::router::Routing_mode::Order,
        "rotate" => crate::proxy::router::Routing_mode::Rotate,
        "weight" => crate::proxy::router::Routing_mode::Weight,
        "usage" => crate::proxy::router::Routing_mode::Usage,
        _ => return Err("routing_mode_invalid".to_string()),
    };
    db.set_setting(
        crate::database::dao::settings::KEY_ROUTING_MODE,
        parsed.as_str(),
    )
    .map_err(String::from)?;
    runtime_reload(state.inner()).await
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
    let pp = db
        .get_proxy_provider(&provider_id)
        .map_err(String::from)?
        .ok_or_else(|| "proxy_provider_not_found".to_string())?;
    let model = provider_test_model(&pp, test_model)?;
    match crate::services::provider_admission::test_provider(&db, &pp, &model).await {
        Ok(report) => {
            let reason = serde_json::to_value(report.reason).map_err(|_| "provider_test_report_invalid")?;
            let reason = reason.as_str().ok_or("provider_test_report_invalid")?;
            let error_code = (!report.eligible).then(|| format!("provider_screening_{reason}"));
            Ok(serde_json::json!({
                "success": report.eligible,
                "latencyMs": report.latency_ms.unwrap_or(0),
                "model": model,
                "reply": report.response.unwrap_or_default(),
                "providerId": pp.id,
                "error": error_code,
                "errorCode": error_code,
                "responseTruncated": report.response_truncated,
                "responseRedacted": report.response_redacted,
                "upstreamStatus": report.upstream_status,
            }))
        }
        Err(code) => Ok(serde_json::json!({
            "success": false, "latencyMs": 0, "model": model,
            "providerId": pp.id, "error": code, "errorCode": code,
        })),
    }
}

fn provider_test_model(provider: &crate::database::dao::proxy_providers::ProxyProvider, requested: Option<String>) -> Result<String, String> {
    let model = requested.or_else(|| provider.models.first().cloned())
        .ok_or_else(|| "provider_test_model_required".to_string())?;
    if model.trim().is_empty() || !provider.models.contains(&model) {
        return Err("provider_test_model_unavailable".into());
    }
    Ok(model)
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
        .get_proxy_provider(&provider_id)
        .map_err(String::from)?
        .ok_or_else(|| "proxy_provider_not_found".to_string())?;

    if crate::services::provider_admission::enforcement_enabled(&db) {
        crate::services::provider_admission::ensure_catalog_admitted(&db, &pp).await?;
    }

    // Shared probe with the model-discovery service; the stable error
    // strings (`proxy_models_http_error_<status>` etc.) live on
    // `fetch::FetchError` and reach the UI verbatim.
    let auth_scheme = crate::services::provider_admission::auth_scheme(&db, &pp)?;
    let fetched = crate::services::model_discovery::fetch::fetch_models(&pp, auth_scheme)
        .await
        .map_err(|e| e.to_string())?;
    Ok(fetched.raw)
}

#[tauri::command]
pub(crate) async fn proxy_start(
    app: tauri::AppHandle,
    state: tauri::State<'_, Mutex<Option<RunningProxy>>>,
    publication: tauri::State<'_, Publication_service>,
) -> Result<String, String> {
    let db = app.state::<Database>();
    let msg = start_proxy_impl(&db, state.inner(), publication.inner()).await?;
    // Fire-and-forget discovery pass: the just-built snapshot is hot, so any
    // pool/alias-policy change it finds is applied to new requests without a
    // listener restart.
    let db_handle = db.inner().clone();
    let proxy_handle = app.clone();
    tauri::async_runtime::spawn(async move {
        let report = crate::services::model_discovery::run_refresh(
            &db_handle,
            crate::services::model_discovery::Trigger::Startup,
            true,
            None,
        )
        .await;
        if let Ok(report) = report {
            if report.has_changes {
                let proxy_state = proxy_handle.state::<Mutex<Option<RunningProxy>>>();
                if runtime_reload(proxy_state.inner()).await.is_ok() {
                    crate::services::model_discovery::clear_pending_restart(&db_handle);
                }
            }
        }
    });
    Ok(msg)
}

/// Body of `proxy_start` minus the "already running" check, so the
/// Tauri setup hook can auto-start the proxy when `proxy_config.enabled`
/// is true (set by `aag-setup` or by the user via the Proxy page).
pub(crate) async fn start_proxy_impl(
    db: &Database,
    state: &Mutex<Option<RunningProxy>>,
    publication: &Publication_service,
) -> Result<String, String> {
    let mut guard = state.lock().await;
    if guard
        .as_ref()
        .is_some_and(|running| !running.task.has_finished())
    {
        return Err("proxy already running".into());
    }
    // Replacing a completed local listener cannot leave its publication alive.
    publication.stop().await?;
    let mut config = db.get_proxy_config().map_err(String::from)?;
    if !config.enabled {
        // Auto-enable on first start (can be turned off later via save_proxy_config)
        config.enabled = true;
        db.save_proxy_config(&config).map_err(String::from)?;
    }

    crate::services::proxy_runtime::maybe_auto_sync_cc_switch(db)?;
    let snapshot =
        crate::services::proxy_runtime::snapshot::build_runtime_snapshot(db, &config).await?;
    let live = crate::services::proxy_runtime::live::Live_runtime::new(
        db.clone(),
        config.clone(),
        snapshot,
    );
    let responses_cot_on = db
        .get_setting(crate::database::dao::settings::KEY_PROXY_RESPONSES_COT_ENABLED)
        .map_err(String::from)?
        .as_deref()
        == Some("1");
    let initial = live.current();
    let app_state = crate::proxy::server::ProxyAppState {
        router: Arc::clone(&initial.router),
        auth_token: config.auth_token.clone(),
        pinned_provider: Arc::new(std::sync::Mutex::new(initial.pinned_provider.clone())),
        responses_cot_enabled: Arc::new(std::sync::atomic::AtomicBool::new(responses_cot_on)),
        passthrough_auth_allowed: initial.passthrough_auth_allowed,
        // Review fix: dropped redundant `Arc<Database>` wrap. `Database`
        // already cheap-clones via its internal `Arc<Mutex<Connection>>`.
        db: Some(db.clone()),
        live: Some(Arc::clone(&live)),
        allow_client_config_repair: false,
    };
    let queue_count = initial.queue_count;
    drop(initial);
    let (bound, handle, shutdown_tx) =
        crate::services::proxy_runtime::bind::bind_with_fallback(&app_state, &config).await?;

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

    *guard = Some(RunningProxy {
        task: Proxy_task::new(handle, shutdown_tx),
        config: ProxyConfig {
            host: bound.addr.ip().to_string(),
            port: bound.addr.port(),
            ..config.clone()
        },
        configured_host: config.host.clone(),
        state: app_state.clone(),
        live: Some(live),
    });
    publication
        .bind_gateway(guard.as_ref().unwrap().task.subscribe_completion())
        .await;
    let mut msg = if bound.addr.port() == config.port {
        format!("proxy_started port={}", bound.addr.port())
    } else {
        format!(
            "proxy_started port={} (port {} was in use; fallback enabled)",
            bound.addr.port(),
            config.port
        )
    };
    if queue_count != 0 {
        msg.push_str(&format!(" · failover queue: {} provider(s)", queue_count));
    }
    // The fresh snapshot already reflects any model-discovery changes that
    // were applied while the proxy was stopped.
    crate::services::model_discovery::clear_pending_restart(db);
    // A publication failure is reported independently; the local API stays up.
    if let Err(code) = publication.start_saved(db, app_state).await {
        log::warn!("tailnet publication: auto-start failed code={code}");
        msg.push_str(&format!(" · Tailnet publication: {code}"));
    }
    Ok(msg)
}

#[tauri::command]
pub(crate) async fn proxy_stop(
    state: tauri::State<'_, Mutex<Option<RunningProxy>>>,
    publication: tauri::State<'_, Publication_service>,
) -> Result<(), String> {
    stop_proxy_impl(state.inner(), publication.inner()).await
}

async fn stop_proxy_impl(
    state: &Mutex<Option<RunningProxy>>,
    publication: &Publication_service,
) -> Result<(), String> {
    publication.cancel();
    let shutdown = {
        let mut guard = state.lock().await;
        publication.stop().await?;
        guard
            .as_mut()
            .map(|running| running.task.request_shutdown())
    };
    let Some(mut shutdown) = shutdown else {
        return Ok(());
    };
    let outcome = shutdown.wait().await;
    let mut guard = state.lock().await;
    if guard
        .as_ref()
        .is_some_and(|running| running.task.matches_shutdown(&shutdown))
    {
        guard.take();
    }
    if !shutdown.signal_sent {
        return Err("proxy_shutdown_signal_failed".into());
    }
    outcome
}

use crate::database::dao::proxy_config::ProxyConfig;

// ----- Proxy providers + Routes CRUD (failover config) -----
// Saves apply to a running gateway through the live snapshot hot reload; a
// restart is only needed for listener/auth config changes.

#[tauri::command]
pub(crate) fn proxy_provider_list(
    db: tauri::State<'_, Database>,
) -> Result<Vec<crate::database::dao::proxy_providers::ProxyProvider>, String> {
    db.get_all_proxy_providers().map_err(String::from)
}

/// Proxy-provider ids referenced by a supplier endpoint projection. The
/// providers page hides direct edit/delete for them: `supplier_save` owns
/// their contents, and direct deletion would make the next supplier save
/// fail. Deletion is also refused backend-side.
#[tauri::command]
pub(crate) fn proxy_provider_supplier_mapped(
    db: tauri::State<'_, Database>,
) -> Result<Vec<String>, String> {
    let mut ids = db.list_supplier_projection_ids().map_err(String::from)?;
    ids.extend(db.supplier_variant_projection_ids().map_err(String::from)?);
    ids.sort(); ids.dedup();
    Ok(ids)
}

#[tauri::command]
pub(crate) async fn proxy_provider_save(
    db: tauri::State<'_, Database>,
    state: tauri::State<'_, Mutex<Option<RunningProxy>>>,
    provider: crate::database::dao::proxy_providers::ProxyProvider,
) -> Result<(), String> {
    save_proxy_provider_impl(&db, state.inner(), provider).await
}

async fn save_proxy_provider_impl(
    db: &Database,
    state: &Mutex<Option<RunningProxy>>,
    provider: crate::database::dao::proxy_providers::ProxyProvider,
) -> Result<(), String> {
    db.save_proxy_provider(&provider).map_err(String::from)?;
    runtime_reload(state).await
}

#[tauri::command]
pub(crate) async fn proxy_provider_delete(
    db: tauri::State<'_, Database>,
    state: tauri::State<'_, Mutex<Option<RunningProxy>>>,
    id: String,
) -> Result<(), String> {
    delete_proxy_provider_impl(&db, state.inner(), id).await
}

async fn delete_proxy_provider_impl(
    db: &Database,
    state: &Mutex<Option<RunningProxy>>,
    id: String,
) -> Result<(), String> {
    db.delete_proxy_provider(&id).map_err(String::from)?;
    runtime_reload(state).await
}

#[tauri::command]
pub(crate) fn route_list(
    db: tauri::State<'_, Database>,
) -> Result<Vec<crate::database::dao::routes::Route>, String> {
    db.get_all_routes().map_err(String::from)
}

#[tauri::command]
pub(crate) async fn route_save(
    db: tauri::State<'_, Database>,
    state: tauri::State<'_, Mutex<Option<RunningProxy>>>,
    route: crate::database::dao::routes::Route,
) -> Result<(), String> {
    save_route_impl(&db, state.inner(), route).await
}

async fn save_route_impl(
    db: &Database,
    state: &Mutex<Option<RunningProxy>>,
    route: crate::database::dao::routes::Route,
) -> Result<(), String> {
    db.save_route(&route).map_err(String::from)?;
    runtime_reload(state).await
}

#[tauri::command]
pub(crate) async fn route_delete(
    db: tauri::State<'_, Database>,
    state: tauri::State<'_, Mutex<Option<RunningProxy>>>,
    alias: String,
) -> Result<(), String> {
    delete_route_impl(&db, state.inner(), alias).await
}

async fn delete_route_impl(
    db: &Database,
    state: &Mutex<Option<RunningProxy>>,
    alias: String,
) -> Result<(), String> {
    db.delete_route(&alias).map_err(String::from)?;
    runtime_reload(state).await
}

/// Rename a route's alias in place (hops/description ride along).
/// Rejects a target alias that already exists (PK conflict) or an
/// unknown source.
#[tauri::command]
pub(crate) async fn route_rename(
    db: tauri::State<'_, Database>,
    state: tauri::State<'_, Mutex<Option<RunningProxy>>>,
    from: String,
    to: String,
) -> Result<(), String> {
    rename_route_impl(&db, state.inner(), from, to).await
}

async fn rename_route_impl(
    db: &Database,
    state: &Mutex<Option<RunningProxy>>,
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
    db.rename_route(&from, to).map_err(String::from)?;
    runtime_reload(state).await
}

/// Apply a successful routing write to a running gateway. The persisted write
/// is already the source of truth; a failed hot reload keeps the old snapshot
/// serving and is reported through status rather than making the save fail.
pub(crate) async fn runtime_reload(state: &Mutex<Option<RunningProxy>>) -> Result<(), String> {
    let live = {
        let guard = state.lock().await;
        guard
            .as_ref()
            .filter(|running| running.accepts_requests())
            .and_then(|running| running.live.as_ref().map(Arc::clone))
    };
    let Some(live) = live else {
        return Ok(());
    };
    live.reload().await.map(|_| ())
}

/// Apply the selected check's persisted result without querying or rewriting
/// subscription evidence for any unselected connection.
pub(crate) async fn runtime_reload_after_recheck(
    state: &Mutex<Option<RunningProxy>>,
) -> Result<(), String> {
    let live = {
        let guard = state.lock().await;
        guard
            .as_ref()
            .filter(|running| running.accepts_requests())
            .and_then(|running| running.live.as_ref().map(Arc::clone))
    };
    let Some(live) = live else {
        return Ok(());
    };
    live.reload_persisted_admission().await.map(|_| ())
}

/// Complete automatic setup before reporting that a newly added supplier is ready.
pub(crate) async fn activate_supplier_runtime(
    db: &Database,
    state: &Mutex<Option<RunningProxy>>,
    publication: &Publication_service,
) -> Result<(), String> {
    let mut config = db.get_proxy_config().map_err(String::from)?;
    if config.auth_token.is_empty() {
        let mut bytes = [0u8; 32];
        getrandom::getrandom(&mut bytes)
            .map_err(|_| "gateway_auth_generation_failed".to_string())?;
        config.auth_token = hex::encode(bytes);
        db.save_proxy_config(&config).map_err(String::from)?;
    }
    let auth_changed = {
        let guard = state.lock().await;
        guard.as_ref().is_some_and(|running| {
            running.accepts_requests() && running.state.auth_token != config.auth_token
        })
    };
    if auth_changed {
        stop_proxy_impl(state, publication).await?;
    }
    let live = {
        let guard = state.lock().await;
        match guard.as_ref() {
            Some(running) if running.accepts_requests() => running.live.as_ref().map(Arc::clone),
            Some(running) if !running.task.has_finished() => return Err("proxy_stopping".into()),
            _ => None,
        }
    };
    if let Some(live) = live {
        live.reload().await?;
    } else {
        start_proxy_impl(db, state, publication).await?;
    }
    Ok(())
}

/// One-shot import of all providers from the cc-switch SQLite DB at the
/// default path (`~/.cc-switch/cc-switch.db` or `CC_SWITCH_DB_PATH`).
/// Idempotent: rows whose `(id, app_type)` already exists in AAG are
/// skipped, not overwritten. Returns a structured report so the UI can
/// toast the number imported + skipped.
#[tauri::command]
pub(crate) async fn import_cc_switch_providers(
    db: tauri::State<'_, Database>,
    proxy: tauri::State<'_, tokio::sync::Mutex<Option<RunningProxy>>>,
) -> Result<crate::services::import_cc_switch::ImportReport, String> {
    use crate::services::import_cc_switch::{self, ImportedProvider, ImportReport};
    let path = import_cc_switch::cc_switch_db_path();
    let preview = import_cc_switch::preview_import(&db, &path).map_err(|_| "supplier_import_preview_failed")?;
    let outcomes = import_cc_switch::screen_import(&db, &path, &preview.plan_id, &[]).await?;
    import_cc_switch::apply_import(&db, &path, &preview.plan_id).map_err(|_| "supplier_import_apply_failed")?;
    let imported = preview.plans.iter().filter(|entry| outcomes.iter().any(|outcome| outcome.eligible
        && outcome.source_record_ref == import_cc_switch::subscription_source_ref(&entry.source_app_type, &entry.source_id)))
        .map(|entry| ImportedProvider { id: entry.supplier_id.clone(), app_type: entry.source_app_type.clone(), name: entry.name.clone() }).collect();
    let report = ImportReport { source_db: "cc_switch".into(), imported, skipped_existing: Vec::new(),
        skipped_no_key: Vec::new(), total_in_source: preview.total_rows, imported_routes: Vec::new(),
        queued_providers: Vec::new(), passthrough_providers: Vec::new() };
    crate::commands::proxy::runtime_reload(proxy.inner()).await?;
    Ok(report)
}

/// DSH consumers use the actual active listener and model snapshot.
pub(crate) async fn dsh_gateway_snapshot(
    state: &Mutex<Option<RunningProxy>>,
) -> Result<Option<crate::services::dsh::upstreams::Gateway_snapshot>, String> {
    use crate::services::dsh::upstreams::{Gateway_route, Gateway_snapshot};
    let guard = state.lock().await;
    let Some(running) = guard.as_ref().filter(|running| running.accepts_requests()) else {
        return Ok(None);
    };
    if running
        .live
        .as_ref()
        .map(|live| live.pinned_provider())
        .unwrap_or_else(|| {
            running
                .state
                .pinned_provider
                .lock()
                .ok()
                .and_then(|guard| guard.clone())
        })
        .is_some()
    {
        return Ok(None);
    }
    if !running.config.is_loopback_bind() {
        return Ok(None);
    }
    let host = running.config.client_endpoint_host();
    Ok(Some(Gateway_snapshot {
        base_url: format!("http://{host}:{}", running.config.port),
        auth_key: running.config.auth_token.clone(),
        routes: running
            .live_router()
            .unwrap_or_else(|| Arc::clone(&running.state.router))
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
#[path = "proxy_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "proxy_lifecycle_tests.rs"]
mod lifecycle_tests;

#[cfg(test)]
mod screening_selection_tests {
    use super::provider_test_model;
    use crate::database::dao::proxy_providers::ProxyProvider;

    #[test]
    fn provider_probe_requires_a_real_configured_model() {
        let mut provider = ProxyProvider {
            id: "fixture".into(), name: "fixture".into(), provider_type: "mock".into(),
            api_key: String::new(), base_url: String::new(), models: vec!["exact-offer".into()],
            enabled: true, timeout_ms: 1000, weight: 1, circuit_breaker: Default::default(),
            model_mapping: Default::default(), extra_headers: Default::default(), auto_track_models: false,
            icon: None, icon_color: None,
        };
        assert_eq!(provider_test_model(&provider, None).unwrap(), "exact-offer");
        assert_eq!(provider_test_model(&provider, Some("invented-model".into())).unwrap_err(), "provider_test_model_unavailable");
        provider.models.clear();
        assert_eq!(provider_test_model(&provider, None).unwrap_err(), "provider_test_model_required");
    }
}
