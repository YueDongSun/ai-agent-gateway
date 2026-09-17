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
pub mod error;
pub mod provider;
pub mod proxy;
pub mod services;
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
        let parsed = reqwest::Url::parse(&url)
            .map_err(|e| format!("invalid endpoint URL: {e}"))?;
        builder = builder
            .endpoints(vec![parsed])
            .map_err(|e| format!("failed to set endpoint: {e}"))?;
    }
    if let Some(k) = pubkey {
        builder = builder.pubkey(k);
    }
    let updater = builder
        .build()
        .map_err(|e| format!("failed to build updater: {e}"))?;
    let update = updater
        .check()
        .await
        .map_err(|e| format!("update check failed: {e}"))?;
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
    let db = Database::init().expect("failed to open database");

    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_log::Builder::new().build())
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            // Second launch forwarded here. Forward any `aag://` URLs in argv
            // through the deep-link channel so Linux/Windows behave like macOS.
            log::info!("single-instance: forwarded argv = {argv:?}");
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
        .manage(Mutex::new(None::<commands::proxy::RunningProxy>))
        .manage(tray::Tray_availability::default())
        .invoke_handler(tauri::generate_handler![
            app_info, app_check_update,
            commands::provider::provider_list, commands::provider::provider_current,
            commands::provider::provider_get, commands::provider::provider_save,
            commands::provider::provider_delete, commands::provider::provider_switch,
            commands::settings::settings_get, commands::settings::settings_set,
            commands::launcher::codex_cli_status, commands::launcher::codex_open_cli_terminal,
            commands::launcher::codex_open_desktop_window,
            commands::launcher::copilot_open_cli_terminal,
            commands::proxy::proxy_status, commands::proxy::proxy_start, commands::proxy::proxy_stop,
            commands::proxy::proxy_config_get, commands::proxy::proxy_config_save,
            commands::proxy::proxy_get_pinned_provider, commands::proxy::proxy_set_pinned_provider,
            commands::proxy::proxy_test_provider, commands::proxy::proxy_fetch_models,
            commands::provider::provider_reorder, commands::provider::provider_set_failover_queue,
            commands::proxy::proxy_provider_list, commands::proxy::proxy_provider_save,
            commands::proxy::proxy_provider_delete,
            commands::proxy::route_list, commands::proxy::route_save, commands::proxy::route_delete,
            commands::proxy::route_rename,
            commands::proxy::import_cc_switch_providers,
        ])
        .setup(|app| {
            // Install the system tray icon (Show / Hide / Quit menu).
            // Best-effort: a missing icon shouldn't crash the app.
            if let Err(e) = tray::install(app.handle()) {
                log::warn!("tray: failed to install tray icon: {e}");
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
                Err(e) => {
                    log::warn!("setup: could not read proxy_config: {e}");
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
                        Ok(msg) => log::info!("proxy auto-start: {msg}"),
                        Err(e) => log::warn!("proxy auto-start failed: {e}"),
                    }
                });
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let tray_available = window
                    .app_handle()
                    .state::<tray::Tray_availability>()
                    .is_available();
                if tray::should_hide_on_close(window.label(), tray_available) {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        });

    builder
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
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
