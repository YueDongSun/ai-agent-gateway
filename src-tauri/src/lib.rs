//! Tauri app library.
//!
//! Phase 1+2: Tauri shell with a SQLite-backed provider store + live config
//! switching for Claude Code and Codex, plus a local proxy with circuit
//! breaker + failover. See `../DESIGN.md` for the migration plan and
//! `../PROGRESS.md` for status.

// Public so external binaries (src/bin/*, examples/) can drive the DB
// directly without going through Tauri commands.
pub mod config;
pub mod database;
pub mod deeplink;
pub mod deeplink_parser;
pub mod diagnostics;
mod dsh_market_window;
mod dsh_window;
pub mod error;
pub mod provider;
pub mod proxy;
pub mod services;
pub mod startup;
pub mod tray;

#[cfg(test)]
mod canary;
#[cfg(test)]
mod e2e;

use database::Database;
use serde::Serialize;
use tauri::Manager;
use tokio::sync::Mutex;

#[derive(Serialize)]
struct AppInfo {
    name: &'static str,
    version: &'static str,
    phase: &'static str,
    platform: &'static str,
    architecture: &'static str,
}

#[tauri::command]
fn app_info() -> AppInfo {
    AppInfo {
        name: "AI Agent Gateway",
        version: env!("CARGO_PKG_VERSION"),
        phase: "2 - proxy + failover",
        platform: std::env::consts::OS,
        architecture: std::env::consts::ARCH,
    }
}

/// Check for an app update. Returns `Some(version)` if a newer version is
/// available, `None` if the app is up to date or no updater endpoint is
/// configured. Errors (network / parse / plugin init) are mapped to
/// `AppError::Message` and surfaced as a `String` to the frontend.
///
/// If `endpoint` is `Some(url)`, it overrides the endpoints configured in
/// `tauri.conf.json` for this single check (useful for testing against a
/// dev / staging server). The `pubkey` is also accepted but optional — if
/// omitted, the configured pubkey (or none) is used.
#[tauri::command]
async fn app_check_update(
    app: tauri::AppHandle,
    endpoint: Option<String>,
    pubkey: Option<String>,
) -> Result<Option<String>, String> {
    use tauri_plugin_updater::UpdaterExt;
    let mut builder = app.updater_builder();
    if let Some(url) = endpoint {
        let parsed =
            reqwest::Url::parse(&url).map_err(|_| "update_endpoint_invalid".to_string())?;
        builder = builder
            .endpoints(vec![parsed])
            .map_err(|_| "update_endpoint_rejected".to_string())?;
    }
    if let Some(k) = pubkey {
        builder = builder.pubkey(k);
    }
    let updater = builder
        .build()
        .map_err(|_| "updater_build_failed".to_string())?;
    let update = updater
        .check()
        .await
        .map_err(|_| "update_check_failed".to_string())?;
    Ok(update.map(|u| u.version))
}

// ----- Command modules (extracted in the file-structure refactor) -----
// All `#[tauri::command]` functions live in `commands/` now, grouped by
// responsibility. `mod commands;` makes them visible here; the
// `invoke_handler!` registration below references them through the module
// paths.

mod commands;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    diagnostics::install_panic_hook();
    let db = match Database::init() {
        Ok(db) => db,
        Err(error) => {
            diagnostics::record_fatal_startup("database_init", &error.to_string());
            return;
        }
    };
    // Reconcile application-owned imports before any routing snapshot is built.
    // A failed recovery attempt leaves the database intact and suppresses auto-start;
    // the visible maintenance action can retry from the manager window.
    let startup_maintenance = services::import_cc_switch::maintain_imported_suppliers(&db);
    let (maintenance_ready, maintenance_service) = match startup_maintenance {
        Ok(result) => {
            let pending = db.get_setting(database::dao::suppliers::KEY_SUPPLIER_IMPORT_SETUP_PENDING)
                .map(|value| value.as_deref() == Some("1")).unwrap_or(true);
            let setup_error = if result.backup_created || pending {
                match services::supplier_setup::configure_mirrors(&db) {
                    Ok(()) => db.set_setting(database::dao::suppliers::KEY_SUPPLIER_IMPORT_SETUP_PENDING, "")
                        .err().map(|_| "supplier_setup_state_unavailable".to_string()),
                    Err(_) => Some("supplier_setup_pending".to_string()),
                }
            } else { None };
            (setup_error.is_none(), commands::supplier_import::Supplier_maintenance_service::with_initial_setup(result, setup_error))
        }
        Err(_) => (false, commands::supplier_import::Supplier_maintenance_service::default()),
    };
    let debug_deadline = diagnostics::restore_debug_deadline(&db);
    let diagnostics_runtime = diagnostics::Diagnostics_runtime::new(debug_deadline);

    let log_builder = match diagnostics::log_builder() {
        Ok(builder) => builder,
        Err(error) => {
            diagnostics::record_fatal_startup("diagnostics_init", &error);
            return;
        }
    };
    let builder = tauri::Builder::default()
        .plugin(log_builder.build())
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            // Second launch forwarded here. Forward any `aag://` URLs in argv
            // through the deep-link channel so Linux/Windows behave like macOS.
            log::info!("single-instance: forwarded argv_count={}", argv.len());
            deeplink::forward_argv(app, &argv);
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.unminimize();
                let _ = w.show();
                let _ = w.set_focus();
            }
        }))
        .plugin(tauri_plugin_deep_link::init())
        // Auto-updater (Phase 9 Step 3): best-effort. If the plugin fails to
        // init (e.g. missing pubkey / endpoints config), the app continues
        // without update support — `app_check_update` will return Ok(None).
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(db)
        .manage(services::provider_admission::review::Review_coordinator::default())
        .manage(services::provider::switching::Provider_switch_service::default())
        .manage(services::mirror_sync::Mirror_sync_service::new())
        .manage(services::model_catalog::Catalog_refresh_service::new())
        .manage(services::dsh::Dsh_service::new(config::get_app_config_dir()))
        .manage(services::mcp::Mcp_service::new(config::get_home_dir()))
        .manage(services::workflow_registry::Workflow_service::new(
            config::get_app_config_dir(),
        ))
        .manage(services::workflow_registry::ipc::Workflow_ipc::new())
        .manage(diagnostics_runtime)
        .manage(commands::dsh_market::Market_state::new(
            config::get_app_config_dir().join("dsh_market"),
        ))
        .manage(Mutex::new(None::<commands::proxy::RunningProxy>))
        .manage(services::tailnet_publication::Publication_service::new())
        .manage(services::tailnet_discovery::Tailnet_discovery_service::new())
        .manage(maintenance_service)
        .invoke_handler(|invoke| {
            if !dsh_market_window::allowed_command(
                invoke.message.webview_ref().label(),
                invoke.message.command(),
            ) {
                invoke.resolver.reject("dsh:untrusted_native_subject");
                return true;
            }
            let handler: fn(tauri::ipc::Invoke) -> bool = tauri::generate_handler![
                app_info,
                app_check_update,
                commands::diagnostics::diagnostics_get,
                commands::diagnostics::diagnostics_enable_debug,
                commands::diagnostics::diagnostics_disable_debug,
                commands::diagnostics::diagnostics_open_log_dir,
                commands::diagnostics::diagnostics_get_summary,
                commands::diagnostics::diagnostics_record_frontend_error,
                commands::diagnostics::diagnostics_request_timeline,
                commands::diagnostics::diagnostics_clear_request_timeline,
                commands::provider::provider_list,
                commands::provider::provider_current,
                commands::provider::provider_get,
                commands::provider::provider_save,
                commands::provider::provider_delete,
                commands::provider::provider_switch,
                commands::provider::provider_switch_all,
                commands::client_config::provider_config_status,
                commands::client_config::client_config_read,
                commands::client_config::client_config_save,
                commands::settings::settings_get,
                commands::settings::settings_set,
                commands::launcher::codex_cli_status,
                commands::launcher::codex_reasoning_models,
                commands::launcher::codex_open_cli_terminal,
                commands::launcher::codex_open_desktop_window,
                commands::launcher::codex_mirror_set_mode,
                commands::launcher::codex_mirror_session_audit,
                commands::launcher::codex_mirror_session_reset,
                commands::launcher::claude_mirror_status,
                commands::launcher::claude_mirror_set_mode,
                commands::launcher::claude_open_desktop,
                commands::launcher::claude_open_cli_terminal,
                commands::launcher::copilot_open_cli_terminal,
                commands::launcher::managed_cli_status,
                commands::launcher::managed_cli_open_terminal,
                commands::launcher::gemini_open_cli_terminal,
                commands::launcher::vscode_open_with_extension,
                commands::launcher::vscode_extension_status,
                commands::proxy::proxy_status,
                commands::proxy::proxy_request_status,
                commands::proxy::proxy_start,
                commands::proxy::proxy_stop,
                commands::proxy::proxy_config_get,
                commands::proxy::proxy_config_save,
                commands::tailnet_publication::tailnet_publication_config_get,
                commands::tailnet_publication::tailnet_publication_config_save,
                commands::tailnet_publication::tailnet_publication_start,
                commands::tailnet_publication::tailnet_publication_stop,
                commands::tailnet_publication::tailnet_publication_status,
                commands::proxy::proxy_get_pinned_provider,
                commands::proxy::proxy_set_pinned_provider,
                commands::proxy::proxy_get_routing_mode,
                commands::proxy::proxy_set_routing_mode,
                commands::proxy::proxy_get_responses_cot,
                commands::proxy::proxy_set_responses_cot,
                commands::proxy::proxy_test_provider,
                commands::proxy::proxy_fetch_models,
                commands::prompt::prompt_enhance_run,
                commands::prompt::prompt_enhance_defaults,
                commands::model_discovery::proxy_refresh_models,
                commands::model_discovery::model_discovery_status,
                commands::model_discovery::proxy_set_auto_track,
                commands::usage::usage_report,
                commands::usage::usage_prune,
                commands::suppliers::supplier_list,
                commands::suppliers::supplier_discover_models,
                commands::suppliers::supplier_get,
                commands::suppliers::supplier_save,
                commands::suppliers::supplier_delete,
                commands::suppliers::supplier_models_update,
                commands::suppliers::supplier_status,
                commands::supplier_import::supplier_import_maintain,
                commands::supplier_import::supplier_import_preview,
                commands::supplier_import::supplier_import_apply,
                commands::supplier_catalog::supplier_catalog_status,
                commands::supplier_catalog::supplier_catalog_refresh,
                commands::supplier_catalog::model_catalog_model,
                commands::tailnet_discovery::tailnet_discovery_scan,
                commands::tailnet_discovery::tailnet_discovery_status,
                commands::tailnet_discovery::tailnet_discovery_cancel,
                commands::prompt_workspace::prompt_workspace_list,
                commands::prompt_workspace::prompt_workspace_save,
                commands::prompt_workspace::prompt_workspace_select,
                commands::prompt_workspace::prompt_workspace_remove,
                commands::prompt_workspace::prompt_workspace_preview,
                commands::supplier_catalog::supplier_arena_snapshot,
                commands::provider::provider_reorder,
                commands::provider::provider_set_failover_queue,
                commands::proxy::proxy_provider_list,
                commands::proxy::proxy_provider_save,
                commands::proxy::proxy_provider_delete,
                commands::proxy::proxy_provider_supplier_mapped,
                commands::proxy::route_list,
                commands::proxy::route_save,
                commands::proxy::route_delete,
                commands::proxy::route_rename,
                commands::proxy::import_cc_switch_providers,
                commands::provider_screening::provider_screening_overview,
                commands::provider_screening::provider_screening_recheck,
                commands::dsh::dsh_get_snapshot,
                commands::dsh::dsh_save_settings,
                commands::dsh::dsh_preview_operation,
                commands::dsh::dsh_execute_operation,
                commands::dsh::dsh_cancel_operation,
                commands::dsh::dsh_list_upstreams,
                commands::dsh::dsh_start,
                commands::dsh::dsh_stop,
                commands::dsh::dsh_open_web_ui,
                commands::dsh_onboarding::dsh_discover_runtime,
                commands::dsh_onboarding::dsh_preview_setup,
                commands::dsh_onboarding::dsh_apply_setup,
                commands::dsh_market::dsh_market_get_catalog,
                commands::dsh_market::dsh_market_refresh,
                commands::dsh_market::dsh_market_open_source,
                commands::dsh_market::dsh_open_marketplace,
                commands::mcp::mcp_list_servers,
                commands::mcp::mcp_save_server,
                commands::mcp::mcp_delete_server,
                commands::mcp::mcp_set_enabled,
                commands::mcp::mcp_preview_sync,
                commands::mcp::mcp_apply_sync,
                commands::mcp::mcp_discard_review,
                commands::workflow_registry::workflow_list_capabilities,
                commands::workflow_registry::workflow_get_health,
                commands::workflow_registry::workflow_route_preview,
                commands::workflow_registry::workflow_probe_capability,
                commands::workflow_registry::workflow_evaluate_route,
            ];
            handler(invoke)
        });
    // The installed executable is launched with a private argument so a
    // login launch can stay tray-only while normal launches show the UI.
    #[cfg(desktop)]
    let builder = builder.plugin(tauri_plugin_autostart::init(
        tauri_plugin_autostart::MacosLauncher::LaunchAgent,
        Some(vec![startup::AUTOSTART_ARGUMENT]),
    ));

    let builder = builder
        .setup(move |app| {
            let window_config = app
                .config()
                .app
                .windows
                .first()
                .ok_or_else(|| std::io::Error::other("main_window_config_missing"))?;
            let mut window_builder =
                tauri::WebviewWindowBuilder::from_config(app.handle(), window_config)?;
            if let Some(directory) = diagnostics::configured_webview_data_directory()
                .map_err(std::io::Error::other)?
            {
                window_builder = window_builder.data_directory(directory);
            }
            window_builder.build()?;
            diagnostics::schedule_restored_expiry(app.handle().clone());
            log::info!(
                "startup: app ready mode={}",
                if startup::launched_from_autostart() {
                    "autostart"
                } else {
                    "normal"
                }
            );
            // The tray is the recovery path for a hidden main window, so a
            // missing tray is a startup error instead of a dead hidden app.
            tray::install(app.handle())
                .map_err(|_| std::io::Error::other("tray_install_failed"))?;
            #[cfg(all(desktop, not(debug_assertions)))]
            startup::enable_autostart(app.handle());
            #[cfg(desktop)]
            {
                startup::show_main_window_for_normal_launch(app.handle());
            }
            // Install the deep-link URL listener. Forwards aag:// URLs to
            // the frontend via the "aag://deeplink" Tauri event.
            deeplink::install(app.handle());

            app.state::<services::workflow_registry::ipc::Workflow_ipc>()
                .start(
                    app.handle().clone(),
                    config::get_app_config_dir().join("runtime"),
                );

            // Auto-start the proxy if `proxy_config.enabled` is true. This is
            // how `aag-setup` arms a persistent failover listener: the next
            // time the Tauri app boots, the proxy is already up on the saved
            // port (default 8989). The startup runs on the Tauri runtime so
            // `tauri::async_runtime::spawn` outlives the setup callback.
            let arm_for_autostart = match app.state::<Database>().get_proxy_config() {
                Ok(cfg) => cfg.enabled,
                Err(_) => {
                    log::warn!("setup: proxy config read failed code=database_error");
                    false
                }
            };
            if arm_for_autostart && maintenance_ready {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    use tauri::Manager;
                    let db = handle.state::<Database>();
                    let state = handle.state::<Mutex<Option<commands::proxy::RunningProxy>>>();
                    let publication = handle.state::<services::tailnet_publication::Publication_service>();
                    match commands::proxy::start_proxy_impl(db.inner(), state.inner(), publication.inner()).await {
                        Ok(_) => log::info!("proxy auto-start completed"),
                        Err(_) => log::warn!("proxy auto-start failed"),
                    }
                });
            }

            // Instruction mirroring runs by default (explicit "0" opts out)
            // and belongs to this app's lifetime, sharing one reconciliation
            // gate with launchers.
            {
                let db = app.state::<Database>();
                if services::mirror_sync::instruction_sync_enabled(db.inner())? {
                    let sync = app.state::<services::mirror_sync::Mirror_sync_service>();
                    if let Err(code) = sync.start(db.inner().clone()) {
                        log::warn!("mirror instruction sync: start failed code={code}");
                    }
                }
            }

            // Public evidence is separate from supplier model discovery. One
            // owner shares refresh admission with explicit catalog IPC calls.
            {
                let db = app.state::<Database>();
                if db.get_setting(services::mirror_models::KEY_ENABLED)?.as_deref() == Some("1") {
                    let refresh = app.state::<services::model_catalog::Catalog_refresh_service>();
                    match refresh.start(db.inner().clone()) {
                        Ok(mut reports) => {
                            let handle = app.handle().clone();
                            tauri::async_runtime::spawn(async move {
                                while let Some(report) = reports.recv().await {
                                    if report.changed > 0 {
                                        let state = handle.state::<Mutex<Option<commands::proxy::RunningProxy>>>();
                                        if let Err(code) = commands::proxy::runtime_reload(state.inner()).await {
                                            log::warn!("catalog refresh: runtime apply failed code={code}");
                                        }
                                    }
                                    log::info!("catalog refresh: attempted={} succeeded={} failed={} changed={}",
                                        report.attempted, report.succeeded, report.failed, report.changed);
                                }
                            });
                        }
                        Err(code) => log::warn!("catalog refresh: start failed code={code}"),
                    }
                }
            }

            // Model auto-discovery scheduler: one refresh at startup, then a
            // periodic check on the `model_discovery_interval_hours` setting
            // (default 24h, 0 disables). App-lifetime task, independent of
            // proxy state; run_refresh deduplicates concurrent runs itself.
            {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    use tauri::Manager;
                    let db = handle.state::<Database>();
                    let proxy_state =
                        handle.state::<Mutex<Option<commands::proxy::RunningProxy>>>();
                    let running = proxy_state.lock().await.is_some();
                    let result = services::model_discovery::run_refresh(
                        db.inner(),
                        services::model_discovery::Trigger::Startup,
                        running,
                        None,
                    )
                    .await;
                    match result {
                        Ok(report) => {
                            if report.has_changes
                                && commands::proxy::runtime_reload(proxy_state.inner())
                                    .await
                                    .is_ok()
                            {
                                services::model_discovery::clear_pending_restart(db.inner());
                            }
                            log::info!(
                                "model discovery: startup run changes={} dedup={}",
                                report.has_changes,
                                report.deduplicated
                            );
                        }
                        Err(error) => log::warn!(
                            "model discovery: startup run failed code={}",
                            error.external_message()
                        ),
                    }
                    let mut tick = tokio::time::interval(std::time::Duration::from_secs(3_600));
                    tick.tick().await; // the first tick fires immediately; skip it
                    loop {
                        tick.tick().await;
                        let interval_hours =
                            services::model_discovery::refresh_interval_hours(db.inner());
                        if interval_hours <= 0 {
                            continue;
                        }
                        let elapsed_seconds = chrono::Utc::now().timestamp()
                            - services::model_discovery::last_run_epoch(db.inner());
                        if elapsed_seconds < interval_hours * 3_600 {
                            continue;
                        }
                        let running = proxy_state.lock().await.is_some();
                        let result = services::model_discovery::run_refresh(
                            db.inner(),
                            services::model_discovery::Trigger::Periodic,
                            running,
                            None,
                        )
                        .await;
                        match result {
                            Ok(report) => {
                                if report.has_changes
                                    && commands::proxy::runtime_reload(proxy_state.inner())
                                        .await
                                        .is_ok()
                                {
                                    services::model_discovery::clear_pending_restart(db.inner());
                                }
                                log::info!(
                                    "model discovery: periodic run changes={} dedup={}",
                                    report.has_changes,
                                    report.deduplicated
                                );
                            }
                            Err(error) => log::warn!(
                                "model discovery: periodic run failed code={}",
                                error.external_message()
                            ),
                        }
                    }
                });
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if tray::should_hide_on_close(window.label()) {
                    log::info!(
                        "window: close requested action=hide label={}",
                        window.label()
                    );
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        });

    let app = match builder.build(tauri::generate_context!()) {
        Ok(app) => app,
        Err(error) => {
            diagnostics::record_fatal_startup("tauri_build", &error.to_string());
            return;
        }
    };
    app.run(|app_handle, event| match event {
        tauri::RunEvent::Ready => log::info!("lifecycle: ready"),
        tauri::RunEvent::ExitRequested { code, .. } => {
            log::info!("lifecycle: exit requested code={code:?}");
            // Graceful teardown on user-initiated exits (tray Quit, app.exit).
            // OS-driven shutdowns (Windows WM_ENDSESSION) skip this event and
            // deliver RunEvent::Exit directly — cleanup runs there too.
            run_exit_cleanup(app_handle);
        }
        tauri::RunEvent::Exit => {
            // The one event that fires on EVERY exit path, including the
            // Windows logoff/shutdown route (WM_ENDSESSION → tao loop_destroyed
            // → Exit) where ExitRequested never fires. Cleanup is idempotent
            // and guarded, so running it from both arms is safe.
            log::info!("lifecycle: exit");
            run_exit_cleanup(app_handle);
        }
        _ => {}
    });
}

/// Teardown shared by ExitRequested and Exit. Runs inside the event-loop
/// frame, so every step is panic-isolated: a failure in one service must not
/// prevent the remaining cleanup or abort the process. Each step carries its
/// own catch_unwind — one big closure would let a panic in the first step
/// silently skip the rest. The once-flag exists because both events can fire
/// in sequence on a normal exit.
fn run_exit_cleanup<R: tauri::Runtime>(app_handle: &tauri::AppHandle<R>) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static CLEANUP_DONE: AtomicBool = AtomicBool::new(false);
    if CLEANUP_DONE.swap(true, Ordering::SeqCst) {
        return;
    }
    let step = |label: &'static str, body: &dyn Fn()| {
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)).is_err() {
            log::error!("lifecycle: exit cleanup step panicked label={label}");
        }
    };
    // Close admission immediately, even when a startup IPC owns proxy state.
    // The native owner retains a bounded cleanup task if this exit waiter times out.
    step("tailnet_discovery", &|| {
        if let Some(service) = app_handle.try_state::<services::tailnet_discovery::Tailnet_discovery_service>() { service.cancel_all(); }
    });
    step("tailnet_publication", &|| {
        if let Some(service) =
            app_handle.try_state::<services::tailnet_publication::Publication_service>()
        {
            service.request_exit();
            let result = tauri::async_runtime::block_on(async {
                tokio::time::timeout(std::time::Duration::from_secs(20), async {
                    let state = app_handle.state::<Mutex<Option<commands::proxy::RunningProxy>>>();
                    let _guard = state.lock().await;
                    service.stop().await
                })
                .await
            });
            match result {
                Ok(Ok(_)) => {},
                Ok(Err(code)) => log::warn!("tailnet publication: exit cleanup failed code={code}"),
                Err(_) => log::warn!("tailnet publication: exit cleanup failed code=tailnet_publication_stop_timeout"),
            }
        }
    });
    // DSH: signal the worker, bounded-wait for the job, stop the runtime.
    // shutdown() early-returns when nothing was ever started, so the
    // common exit costs nothing.
    step("dsh", &|| {
        if let Some(service) = app_handle.try_state::<services::dsh::Dsh_service>() {
            if let Err(error) = service.shutdown() {
                log::warn!("dsh: runtime cleanup failed code={error}");
            }
        }
    });
    step("workflow_ipc", &|| {
        if let Some(ipc) = app_handle.try_state::<services::workflow_registry::ipc::Workflow_ipc>()
        {
            ipc.stop();
        }
    });
    step("mirror_sync", &|| {
        if let Some(service) = app_handle.try_state::<services::mirror_sync::Mirror_sync_service>()
        {
            if let Err(code) = service.stop() {
                log::warn!("mirror instruction sync: stop failed code={code}");
            }
        }
    });
    step("catalog_refresh", &|| {
        if let Some(service) =
            app_handle.try_state::<services::model_catalog::Catalog_refresh_service>()
        {
            if let Err(code) = tauri::async_runtime::block_on(service.stop()) {
                log::warn!("catalog refresh: stop failed code={code}");
            }
        }
    });
    // Release the proxy listener so the axum task can finish before the
    // process dies. We cannot await the async stop here; the shutdown
    // signal gives the task a chance to close sockets gracefully.
    step("proxy", &|| {
        if let Some(state) =
            app_handle.try_state::<tokio::sync::Mutex<Option<commands::proxy::RunningProxy>>>()
        {
            if let Ok(mut guard) = state.try_lock() {
                if let Some(rp) = guard.take() {
                    // The completion observer retains the server task even
                    // when this exit-time waiter is dropped without awaiting.
                    let _ = rp.request_shutdown();
                }
            }
        }
    });
}

#[cfg(test)]
mod app_info_tests {
    use super::app_info;

    #[test]
    fn app_info_reports_build_platform_and_architecture() {
        let info = app_info();
        assert_eq!(info.platform, std::env::consts::OS);
        assert_eq!(info.architecture, std::env::consts::ARCH);
    }
}
