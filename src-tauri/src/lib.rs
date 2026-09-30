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
    let debug_deadline = diagnostics::restore_debug_deadline(&db);
    let diagnostics_runtime = diagnostics::Diagnostics_runtime::new(debug_deadline);

    let builder = tauri::Builder::default()
        .plugin(diagnostics::log_builder().build())
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
        .manage(services::dsh::Dsh_service::new(config::get_app_config_dir()))
        .manage(services::mcp::Mcp_service::new(config::get_home_dir()))
        .manage(diagnostics_runtime)
        .manage(commands::dsh_market::Market_state {
            root: config::get_app_config_dir().join("dsh_market"),
        })
        .manage(Mutex::new(None::<commands::proxy::RunningProxy>))
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
                commands::provider::provider_list,
                commands::provider::provider_current,
                commands::provider::provider_get,
                commands::provider::provider_save,
                commands::provider::provider_delete,
                commands::provider::provider_switch,
                commands::client_config::provider_config_status,
                commands::settings::settings_get,
                commands::settings::settings_set,
                commands::launcher::codex_cli_status,
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
                commands::launcher::vscode_open_with_extension,
                commands::launcher::vscode_extension_status,
                commands::proxy::proxy_status,
                commands::proxy::proxy_start,
                commands::proxy::proxy_stop,
                commands::proxy::proxy_config_get,
                commands::proxy::proxy_config_save,
                commands::proxy::proxy_get_pinned_provider,
                commands::proxy::proxy_set_pinned_provider,
                commands::proxy::proxy_get_responses_cot,
                commands::proxy::proxy_set_responses_cot,
                commands::proxy::proxy_test_provider,
                commands::proxy::proxy_fetch_models,
                commands::provider::provider_reorder,
                commands::provider::provider_set_failover_queue,
                commands::proxy::proxy_provider_list,
                commands::proxy::proxy_provider_save,
                commands::proxy::proxy_provider_delete,
                commands::proxy::route_list,
                commands::proxy::route_save,
                commands::proxy::route_delete,
                commands::proxy::route_rename,
                commands::proxy::import_cc_switch_providers,
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
        .setup(|app| {
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
            if arm_for_autostart {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    use tauri::Manager;
                    let db = handle.state::<Database>();
                    let state = handle.state::<Mutex<Option<commands::proxy::RunningProxy>>>();
                    match commands::proxy::start_proxy_impl(db.inner(), state.inner()).await {
                        Ok(_) => log::info!("proxy auto-start completed"),
                        Err(_) => log::warn!("proxy auto-start failed"),
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
            // Perform graceful cleanup before the event loop finishes. OS-driven
            // shutdowns (logoff / restart) may skip this event and go straight to
            // Exit; Windows will reclaim child processes in that case.
            let service = app_handle.state::<services::dsh::Dsh_service>();
            if service.shutdown().is_err() {
                log::warn!("dsh: runtime cleanup failed");
            }
            // Release the proxy server so it does not get aborted during runtime
            // teardown. We cannot await the async stop here, but sending the
            // shutdown signal gives the task a chance to finish gracefully.
            if let Ok(mut guard) = app_handle
                .state::<tokio::sync::Mutex<Option<commands::proxy::RunningProxy>>>()
                .try_lock()
            {
                if let Some(rp) = guard.take() {
                    let _ = rp.request_shutdown();
                }
            }
        }
        tauri::RunEvent::Exit => {
            log::info!("lifecycle: exit");
        }
        _ => {}
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
