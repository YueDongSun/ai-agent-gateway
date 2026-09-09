//! Tauri app library.
//!
//! Phase 1+2: Tauri shell with a SQLite-backed provider store + live config
//! switching for Claude Code and Codex, plus a local proxy with circuit
//! breaker + failover. See `../DESIGN.md` for the migration plan and
//! `../PROGRESS.md` for status.

mod config;
mod database;
mod deeplink;
mod deeplink_parser;
mod error;
mod provider;
mod proxy;
mod services;
mod tray;

#[cfg(test)]
mod e2e;

use database::Database;
use provider::Provider;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use tauri::Manager;
use tokio::sync::Mutex;

#[derive(Serialize)]
struct AppInfo {
    name: &'static str,
    version: &'static str,
    phase: &'static str,
}

#[tauri::command]
fn app_info() -> AppInfo {
    AppInfo {
        name: "AI Agent Gateway",
        version: env!("CARGO_PKG_VERSION"),
        phase: "2 - proxy + failover",
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

/// Serializable view of a `DeeplinkAction` for the frontend.
#[derive(serde::Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum DeeplinkActionDto {
    SwitchProvider {
        provider_id: String,
        app_type: String,
    },
    FocusProvider {
        provider_id: String,
    },
    FocusMcp {
        server_id: String,
    },
    FocusSkill {
        skill_id: String,
    },
    ShowDeeplinks,
    Unknown {
        raw: String,
        reason: String,
    },
}

impl From<deeplink_parser::DeeplinkAction> for DeeplinkActionDto {
    fn from(a: deeplink_parser::DeeplinkAction) -> Self {
        match a {
            deeplink_parser::DeeplinkAction::SwitchProvider {
                provider_id,
                app_type,
            } => Self::SwitchProvider {
                provider_id,
                app_type,
            },
            deeplink_parser::DeeplinkAction::FocusProvider { provider_id } => {
                Self::FocusProvider { provider_id }
            }
            deeplink_parser::DeeplinkAction::FocusMcp { server_id } => {
                Self::FocusMcp { server_id }
            }
            deeplink_parser::DeeplinkAction::FocusSkill { skill_id } => {
                Self::FocusSkill { skill_id }
            }
            deeplink_parser::DeeplinkAction::ShowDeeplinks => Self::ShowDeeplinks,
            deeplink_parser::DeeplinkAction::Unknown { raw, reason } => {
                Self::Unknown { raw, reason }
            }
        }
    }
}

/// Parse a deep-link URL into a structured action.
#[tauri::command]
fn deeplink_parse(url: String) -> DeeplinkActionDto {
    deeplink_parser::parse(&url).into()
}

// ----- Provider commands (Phase 1) -----

#[tauri::command]
fn provider_list(db: tauri::State<'_, Database>, app_type: String) -> Result<Vec<Provider>, String> {
    db.get_all_providers(&app_type).map(|m| m.into_iter().map(|(_, v)| v).collect()).map_err(String::from)
}
#[tauri::command]
fn provider_current(db: tauri::State<'_, Database>, app_type: String) -> Result<Option<String>, String> {
    db.get_current_provider(&app_type).map_err(String::from)
}
#[tauri::command]
fn provider_get(db: tauri::State<'_, Database>, id: String, app_type: String) -> Result<Option<Provider>, String> {
    db.get_provider_by_id(&id, &app_type).map_err(String::from)
}
#[tauri::command]
fn provider_save(db: tauri::State<'_, Database>, app_type: String, provider: Provider) -> Result<(), String> {
    db.save_provider(&app_type, &provider).map_err(String::from)
}
#[tauri::command]
fn provider_delete(db: tauri::State<'_, Database>, id: String, app_type: String) -> Result<(), String> {
    db.delete_provider(&id, &app_type).map_err(String::from)
}
#[tauri::command]
fn provider_switch(
    db: tauri::State<'_, Database>,
    id: String,
    app_type: String,
) -> Result<services::provider::LiveWriteResult, String> {
    let app = services::provider::AppType::from_db_str(&app_type).map_err(String::from)?;
    let provider = db.get_provider_by_id(&id, &app_type)
        .map_err(String::from)?
        .ok_or_else(|| format!("provider \"{id}\" not found for app \"{app_type}\""))?;
    db.set_current_provider(&id, &app_type).map_err(String::from)?;
    services::provider::write_live_snapshot(app, &provider).map_err(String::from)
}

// ----- Proxy commands (Phase 2) -----

/// Handle to a running proxy server. Stored as Tauri managed state.
struct RunningProxy {
    handle: tauri::async_runtime::JoinHandle<()>,
    shutdown: tokio::sync::oneshot::Sender<()>,
    config: database::dao::proxy_config::ProxyConfig,
}

#[derive(Serialize)]
struct ProxyStatusDto {
    running: bool,
    config: database::dao::proxy_config::ProxyConfig,
}

#[tauri::command]
async fn proxy_status(state: tauri::State<'_, Mutex<Option<RunningProxy>>>) -> Result<ProxyStatusDto, String> {
    let guard = state.lock().await;
    let config = match &*guard {
        Some(rp) => rp.config.clone(),
        None => ProxyConfig::default(),
    };
    Ok(ProxyStatusDto { running: guard.is_some(), config })
}

#[tauri::command]
async fn proxy_start(
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
    let config = db.get_proxy_config().map_err(String::from)?;
    if !config.enabled {
        // Auto-enable on first start (can be turned off later via save_proxy_config)
        let mut updated = config.clone();
        updated.enabled = true;
        db.save_proxy_config(&updated).map_err(String::from)?;
    }

    // Build provider instances from DB
    let db_providers = db.get_all_proxy_providers().map_err(String::from)?;
    let mut providers: HashMap<String, Arc<dyn proxy::providers::Provider>> = HashMap::new();
    for pp in &db_providers {
        match proxy::providers::build_provider(pp) {
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
    let mut routes: HashMap<String, Vec<database::dao::routes::RouteHop>> = HashMap::new();
    for r in db_routes {
        routes.insert(r.alias, r.hops);
    }

    let router = Arc::new(proxy::router::Router::new(providers, routes));
    let app_state = proxy::server::ProxyAppState {
        router,
        auth_token: config.auth_token.clone(),
    };
    let bind_addr = proxy::server::socket_addr(&config.host, config.port);
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();

    let handle = tauri::async_runtime::spawn(async move {
        if let Err(e) = proxy::server::serve(bind_addr, app_state, shutdown_rx).await {
            log::error!("proxy: server error: {e}");
        }
    });

    {
        let mut guard = state.lock().await;
        *guard = Some(RunningProxy {
            handle,
            shutdown: shutdown_tx,
            config: config.clone(),
        });
    }
    Ok(format!("listening on {}:{}", config.host, config.port))
}

#[tauri::command]
async fn proxy_stop(state: tauri::State<'_, Mutex<Option<RunningProxy>>>) -> Result<(), String> {
    let mut guard = state.lock().await;
    if let Some(rp) = guard.take() {
        rp.shutdown.send(()).map_err(|_| "shutdown signal failed".to_string())?;
        // Wait for the server task to finish gracefully.
        rp.handle.await.map_err(|e| e.to_string())?;
    }
    Ok(())
}

use database::dao::proxy_config::ProxyConfig;

// ----- Backup commands (Phase 4) -----

#[tauri::command]
fn backup_create(db: tauri::State<'_, Database>, label: Option<String>) -> Result<database::dao::backup::BackupEntry, String> {
    let entry = db.create_backup(label).map_err(String::from)?;
    // Auto-rotate: keep only the 10 most recent backups (mirrors cc-switch default).
    if let Ok(deleted) = db.cleanup_rotated(10) {
        if !deleted.is_empty() {
            log::info!("backup: rotated {} old backups", deleted.len());
        }
    }
    Ok(entry)
}

#[tauri::command]
fn backup_list(db: tauri::State<'_, Database>) -> Result<Vec<database::dao::backup::BackupEntry>, String> {
    db.list_backups().map_err(String::from)
}

#[tauri::command]
fn backup_delete(db: tauri::State<'_, Database>, id: String) -> Result<(), String> {
    db.delete_backup(&id).map_err(String::from)
}

// ----- MCP commands (Phase 5) -----

#[tauri::command]
fn mcp_list(db: tauri::State<'_, Database>) -> Result<Vec<database::dao::mcp::McpServer>, String> {
    db.get_all_mcp_servers().map_err(String::from)
}

#[tauri::command]
fn mcp_save(
    db: tauri::State<'_, Database>,
    server: database::dao::mcp::McpServer,
) -> Result<(), String> {
    db.save_mcp_server(&server).map_err(String::from)
}

#[tauri::command]
fn mcp_delete(db: tauri::State<'_, Database>, id: String) -> Result<(), String> {
    db.delete_mcp_server(&id).map_err(String::from)
}

#[tauri::command]
fn mcp_toggle_app(
    db: tauri::State<'_, Database>,
    id: String,
    app: String,
    enabled: bool,
) -> Result<(), String> {
    db.toggle_mcp_app(&id, &app, enabled).map_err(String::from)
}

// ----- Skills commands (Phase 5) -----

#[tauri::command]
fn skill_list(db: tauri::State<'_, Database>) -> Result<Vec<database::dao::skills::InstalledSkill>, String> {
    db.get_all_skills().map_err(String::from)
}

#[tauri::command]
fn skill_save(
    db: tauri::State<'_, Database>,
    skill: database::dao::skills::InstalledSkill,
) -> Result<(), String> {
    db.save_skill(&skill).map_err(String::from)
}

#[tauri::command]
fn skill_delete(db: tauri::State<'_, Database>, id: String) -> Result<(), String> {
    db.delete_skill(&id).map_err(String::from)
}

// ----- Session commands (Phase 6) -----

#[tauri::command]
fn session_list(
    db: tauri::State<'_, Database>,
    app_id: Option<String>,
) -> Result<Vec<database::dao::sessions::SessionMeta>, String> {
    match app_id {
        Some(app) => db.get_sessions_by_app(&app).map_err(String::from),
        None => db.get_all_sessions().map_err(String::from),
    }
}

#[tauri::command]
fn session_get(
    db: tauri::State<'_, Database>,
    id: String,
) -> Result<Option<database::dao::sessions::SessionMeta>, String> {
    db.get_session(&id).map_err(String::from)
}

#[tauri::command]
fn session_delete(db: tauri::State<'_, Database>, id: String) -> Result<(), String> {
    db.delete_session(&id).map_err(String::from)
}

// ----- Usage commands (Phase 7) -----

#[tauri::command]
fn usage_record(
    db: tauri::State<'_, Database>,
    record: database::dao::usage::UsageRecordInput,
) -> Result<i64, String> {
    db.record_usage(&record).map_err(String::from)
}

#[tauri::command]
fn usage_summary(
    db: tauri::State<'_, Database>,
    since: Option<i64>,
    until: Option<i64>,
    app_id: Option<String>,
) -> Result<database::dao::usage::UsageSummary, String> {
    db.get_usage_summary(since, until, app_id.as_deref())
        .map_err(String::from)
}

#[tauri::command]
fn usage_by_model(
    db: tauri::State<'_, Database>,
    since: Option<i64>,
    until: Option<i64>,
    app_id: Option<String>,
) -> Result<Vec<database::dao::usage::ModelUsage>, String> {
    db.get_usage_by_model(since, until, app_id.as_deref())
        .map_err(String::from)
}

#[tauri::command]
fn usage_by_provider(
    db: tauri::State<'_, Database>,
    since: Option<i64>,
    until: Option<i64>,
    app_id: Option<String>,
) -> Result<Vec<database::dao::usage::ProviderUsage>, String> {
    db.get_usage_by_provider(since, until, app_id.as_deref())
        .map_err(String::from)
}

// ----- Sync commands (Phase 8) -----

#[tauri::command]
fn sync_list(db: tauri::State<'_, Database>) -> Result<Vec<database::dao::sync_config::SyncConfig>, String> {
    db.get_all_sync_configs().map_err(String::from)
}

#[tauri::command]
fn sync_save(
    db: tauri::State<'_, Database>,
    config: database::dao::sync_config::SyncConfig,
) -> Result<(), String> {
    db.save_sync_config(&config).map_err(String::from)
}

#[tauri::command]
fn sync_delete(db: tauri::State<'_, Database>, provider: String) -> Result<(), String> {
    let target = database::dao::sync_config::SyncTarget::from_str(&provider).map_err(String::from)?;
    db.delete_sync_config(target).map_err(String::from)
}

#[tauri::command]
fn sync_record_result(
    db: tauri::State<'_, Database>,
    provider: String,
    error: Option<String>,
) -> Result<(), String> {
    let target = database::dao::sync_config::SyncTarget::from_str(&provider).map_err(String::from)?;
    db.record_sync_result(target, error.as_deref()).map_err(String::from)
}

#[tauri::command]
async fn sync_check(
    db: tauri::State<'_, Database>,
    config: database::dao::sync_config::SyncConfig,
) -> Result<(), String> {
    let target = database::dao::sync_config::SyncTarget::from_str(&config.provider)
        .map_err(String::from)?;
    match target {
        database::dao::sync_config::SyncTarget::Webdav => {
            services::sync::webdav::check_and_stamp(
                &db,
                &config.endpoint,
                &config.access_key,
                &config.secret_key,
                config.remote_path.as_deref(),
            )
            .await
        }
        database::dao::sync_config::SyncTarget::S3 => {
            let creds = services::sync::s3::S3Credentials {
                access_key_id: config.access_key,
                secret_access_key: config.secret_key,
                region: config.region.unwrap_or_default(),
                bucket: config.bucket.unwrap_or_default(),
                endpoint: config.endpoint,
            };
            services::sync::s3::check_and_stamp(&db, &creds).await
        }
    }
    .map_err(String::from)
}

#[tauri::command]
async fn sync_upload(
    db: tauri::State<'_, Database>,
    config: database::dao::sync_config::SyncConfig,
) -> Result<serde_json::Value, String> {
    let target = database::dao::sync_config::SyncTarget::from_str(&config.provider)
        .map_err(String::from)?;
    let value = match target {
        database::dao::sync_config::SyncTarget::Webdav => {
            let r = services::sync::webdav::upload_and_stamp(
                &db,
                &config.endpoint,
                &config.access_key,
                &config.secret_key,
                config.remote_path.as_deref(),
            )
            .await?;
            serde_json::to_value(r).map_err(|e| e.to_string())?
        }
        database::dao::sync_config::SyncTarget::S3 => {
            let creds = services::sync::s3::S3Credentials {
                access_key_id: config.access_key,
                secret_access_key: config.secret_key,
                region: config.region.unwrap_or_default(),
                bucket: config.bucket.unwrap_or_default(),
                endpoint: config.endpoint,
            };
            let r = services::sync::s3::upload_and_stamp(&db, &creds).await?;
            serde_json::to_value(r).map_err(|e| e.to_string())?
        }
    };
    Ok(value)
}

#[tauri::command]
async fn sync_download(
    db: tauri::State<'_, Database>,
    config: database::dao::sync_config::SyncConfig,
) -> Result<serde_json::Value, String> {
    let target = database::dao::sync_config::SyncTarget::from_str(&config.provider)
        .map_err(String::from)?;
    let value = match target {
        database::dao::sync_config::SyncTarget::Webdav => {
            let r = services::sync::webdav::download_and_stamp(
                &db,
                &config.endpoint,
                &config.access_key,
                &config.secret_key,
                config.remote_path.as_deref(),
            )
            .await?;
            serde_json::to_value(r).map_err(|e| e.to_string())?
        }
        database::dao::sync_config::SyncTarget::S3 => {
            let creds = services::sync::s3::S3Credentials {
                access_key_id: config.access_key,
                secret_access_key: config.secret_key,
                region: config.region.unwrap_or_default(),
                bucket: config.bucket.unwrap_or_default(),
                endpoint: config.endpoint,
            };
            let r = services::sync::s3::download_and_stamp(&db, &creds).await?;
            serde_json::to_value(r).map_err(|e| e.to_string())?
        }
    };
    Ok(value)
}

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
        .manage(Mutex::new(None::<RunningProxy>))
        .invoke_handler(tauri::generate_handler![
            app_info, app_check_update,
            deeplink_parse,
            provider_list, provider_current, provider_get, provider_save, provider_delete, provider_switch,
            proxy_status, proxy_start, proxy_stop,
            backup_create, backup_list, backup_delete,
            mcp_list, mcp_save, mcp_delete, mcp_toggle_app,
            skill_list, skill_save, skill_delete,
            session_list, session_get, session_delete,
            usage_record, usage_summary, usage_by_model, usage_by_provider,
            sync_list, sync_save, sync_delete, sync_record_result,
            sync_check, sync_upload, sync_download,
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
            Ok(())
        });

    builder
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}