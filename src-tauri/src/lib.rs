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
    // Codex CLI isolated CODEX_HOME: if an isolated directory is set, write
    // live config there instead of the default ~/.codex; desktop app config
    // is left untouched.
    let codex_cli_home = db
        .get_setting(database::dao::settings::KEY_CODEX_CLI_HOME)
        .map_err(String::from)?;
    services::provider::write_live_snapshot(app, &provider, codex_cli_home.as_deref())
        .map_err(String::from)
}

// ----- App settings (Codex CLI isolation etc.) -----

/// Read a device-level setting (app_meta table).
#[tauri::command]
fn settings_get(
    db: tauri::State<'_, Database>,
    key: String,
) -> Result<Option<String>, String> {
    db.get_setting(&key).map_err(String::from)
}

/// Write a device-level setting. An empty value deletes the key.
#[tauri::command]
fn settings_set(
    db: tauri::State<'_, Database>,
    key: String,
    value: String,
) -> Result<(), String> {
    db.set_setting(&key, &value).map_err(String::from)
}

// ----- Codex CLI isolation -----

/// Codex CLI isolation status (for the frontend to render).
#[derive(Serialize)]
struct CodexCliStatusDto {
    /// Whether an isolated CODEX_HOME is set (isolation enabled).
    isolation_enabled: bool,
    /// The Codex config directory in effect (isolated dir or default ~/.codex).
    config_dir: String,
    /// Whether that directory is initialized (config.toml or auth.json exists).
    initialized: bool,
    /// The isolated dir setting value (raw string, None = not set).
    codex_cli_home: Option<String>,
    /// The global outbound proxy setting value.
    outbound_proxy: Option<String>,
    /// Path to the found codex CLI binary.
    codex_binary: Option<String>,
    /// The currently selected Codex provider (from the DB, may be None).
    current_provider_id: Option<String>,
    /// Display name of the currently selected provider.
    current_provider_name: Option<String>,
}

/// Read Codex CLI isolation-related status.
#[tauri::command]
fn codex_cli_status(db: tauri::State<'_, Database>) -> Result<CodexCliStatusDto, String> {
    let codex_cli_home = db
        .get_setting(database::dao::settings::KEY_CODEX_CLI_HOME)
        .map_err(String::from)?;
    let outbound_proxy = db
        .get_setting(database::dao::settings::KEY_OUTBOUND_PROXY)
        .map_err(String::from)?;
    let config_dir = crate::config::get_codex_config_dir_with_override(
        codex_cli_home.as_deref(),
    );
    let initialized = config_dir.join("config.toml").exists()
        || config_dir.join("auth.json").exists();

    // The currently selected Codex provider (the is_current row for app_type='codex' in the DB).
    let current_provider_id = db.get_current_provider("codex").map_err(String::from)?;
    let current_provider_name = current_provider_id
        .as_deref()
        .and_then(|id| db.get_provider_by_id(id, "codex").ok().flatten())
        .map(|p| p.name);

    Ok(CodexCliStatusDto {
        isolation_enabled: codex_cli_home.is_some(),
        config_dir: config_dir.to_string_lossy().into_owned(),
        initialized,
        codex_cli_home,
        outbound_proxy,
        codex_binary: find_codex_cli_binary().map(|p| p.to_string_lossy().into_owned()),
        current_provider_id,
        current_provider_name,
    })
}

/// Find the codex CLI binary (PATH + common install locations, including the
/// CLI bundled with the OpenAI desktop app).
fn find_codex_cli_binary() -> Option<std::path::PathBuf> {
    #[cfg(target_os = "windows")]
    {
        if let Some(found) = find_on_windows_path("codex.exe") {
            return Some(found);
        }
        if let Some(found) = find_on_windows_path("codex.cmd") {
            return Some(found);
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        if let Some(found) = find_on_unix_path("codex") {
            return Some(found);
        }
    }
    for c in [
        "/opt/homebrew/bin/codex",
        "/usr/local/bin/codex",
        "/home/linuxbrew/.linuxbrew/bin/codex",
    ] {
        let p = std::path::PathBuf::from(c);
        if p.exists() {
            return Some(p);
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
            let codex_bin =
                std::path::PathBuf::from(local_app_data).join("OpenAI").join("Codex").join("bin");
            let direct = codex_bin.join("codex.exe");
            if direct.exists() {
                return Some(direct);
            }
            if let Ok(entries) = std::fs::read_dir(&codex_bin) {
                let mut found: Vec<_> = entries
                    .filter_map(Result::ok)
                    .map(|e| e.path().join("codex.exe"))
                    .filter(|p| p.exists())
                    .collect();
                found.sort();
                if let Some(latest) = found.last() {
                    return Some(latest.clone());
                }
            }
        }
    }
    None
}

#[cfg(target_os = "windows")]
fn find_on_windows_path(exe_name: &str) -> Option<std::path::PathBuf> {
    let output = std::process::Command::new("where")
        .arg(exe_name)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(std::path::PathBuf::from)
        .find(|p| p.exists())
}

#[cfg(not(target_os = "windows"))]
fn find_on_unix_path(exe_name: &str) -> Option<std::path::PathBuf> {
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {exe_name}"))
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let trimmed = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if trimmed.is_empty() {
        return None;
    }
    let p = std::path::PathBuf::from(trimmed);
    p.exists().then_some(p)
}

/// Launch the Codex CLI in an isolated terminal (injecting CODEX_HOME +
/// outbound proxy environment variables).
#[tauri::command]
fn codex_open_cli_terminal(
    db: tauri::State<'_, Database>,
    cwd: Option<String>,
) -> Result<serde_json::Value, String> {
    let codex_bin = find_codex_cli_binary()
        .ok_or_else(|| "codex CLI not found; make sure it is installed and on PATH".to_string())?;
    let codex_bin = codex_bin.to_string_lossy().to_string();

    let codex_cli_home = db
        .get_setting(database::dao::settings::KEY_CODEX_CLI_HOME)
        .map_err(String::from)?;
    let outbound_proxy = db
        .get_setting(database::dao::settings::KEY_OUTBOUND_PROXY)
        .map_err(String::from)?;

    let config_dir = crate::config::get_codex_config_dir_with_override(codex_cli_home.as_deref());
    std::fs::create_dir_all(&config_dir).map_err(|e| format!("failed to create CODEX_HOME: {e}"))?;
    let codex_home = config_dir.to_string_lossy().to_string();

    let mut env: Vec<(String, String)> = vec![("CODEX_HOME".to_string(), codex_home.clone())];
    if let Some(proxy) = &outbound_proxy {
        env.push(("HTTP_PROXY".to_string(), proxy.clone()));
        env.push(("HTTPS_PROXY".to_string(), proxy.clone()));
        env.push(("ALL_PROXY".to_string(), proxy.clone()));
    }
    env.push((
        "NO_PROXY".to_string(),
        "localhost,127.0.0.1,::1".to_string(),
    ));

    #[cfg(target_os = "windows")]
    let command_line = {
        let mut lines = Vec::new();
        if let Some(dir) = cwd.as_deref().filter(|s| !s.trim().is_empty()) {
            lines.push(format!("cd /d \"{}\" || exit /b 1", escape_batch(dir)));
        }
        for (k, v) in &env {
            lines.push(format!("set \"{k}={}\"", escape_batch(v)));
        }
        lines.push(format!("\"{}\"", escape_batch(&codex_bin)));
        lines.join("\r\n")
    };

    #[cfg(not(target_os = "windows"))]
    let command_line = {
        let mut lines = Vec::new();
        if let Some(dir) = cwd.as_deref().filter(|s| !s.trim().is_empty()) {
            lines.push(format!("cd {}", shell_quote(dir)));
        }
        for (k, v) in &env {
            lines.push(format!("export {k}={}", shell_quote(v)));
        }
        lines.push(shell_quote(&codex_bin));
        lines.join("\n")
    };

    launch_agent_terminal(&command_line, "codex_cli")?;

    Ok(serde_json::json!({
        "codexHome": codex_home,
        "codexBinary": codex_bin,
        "env": env,
    }))
}

/// Launch the Copilot CLI in an isolated terminal (injecting
/// COPILOT_PROVIDER_* + outbound proxy environment variables). The Copilot
/// CLI's custom provider can only be injected via environment variables at
/// launch (there is no persisted provider config), so AAG reads
/// baseUrl/apiKey/model from the currently selected Copilot provider's
/// settings_config and opens a terminal with `COPILOT_PROVIDER_BASE_URL`
/// and friends set.
#[tauri::command]
fn copilot_open_cli_terminal(
    db: tauri::State<'_, Database>,
    cwd: Option<String>,
) -> Result<serde_json::Value, String> {
    let outbound_proxy = db
        .get_setting(database::dao::settings::KEY_OUTBOUND_PROXY)
        .map_err(String::from)?;

    // The currently selected Copilot provider (the is_current row for app_type='copilot' in the DB).
    let provider_id = db
        .get_current_provider("copilot")
        .map_err(String::from)?;
    let provider = provider_id
        .as_deref()
        .and_then(|id| db.get_provider_by_id(id, "copilot").ok().flatten())
        .ok_or_else(|| {
            "no Copilot provider configured: add and switch to a Copilot provider on the Providers page (with baseUrl/apiKey/model)".to_string()
        })?;
    let cfg_obj = provider
        .settings_config
        .as_object()
        .ok_or_else(|| "Copilot provider settings_config must be a JSON object".to_string())?;
    let base_url = cfg_obj
        .get("env")
        .and_then(|e| e.get("COPILOT_PROVIDER_BASE_URL"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| "settings_config.env.COPILOT_PROVIDER_BASE_URL is missing".to_string())?;
    let api_key = cfg_obj
        .get("env")
        .and_then(|e| e.get("COPILOT_PROVIDER_API_KEY"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let provider_type = cfg_obj
        .get("env")
        .and_then(|e| e.get("COPILOT_PROVIDER_TYPE"))
        .and_then(|v| v.as_str())
        .unwrap_or("openai");
    let model = cfg_obj
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    // Find the copilot CLI binary (prefer PATH; fall back to common install locations).
    let copilot_bin = find_copilot_cli_binary()
        .ok_or_else(|| "copilot CLI not found; make sure it is installed and on PATH".to_string())?;
    let copilot_bin = copilot_bin.to_string_lossy().to_string();

    let mut env: Vec<(String, String)> = vec![
        ("COPILOT_PROVIDER_BASE_URL".to_string(), base_url.to_string()),
        ("COPILOT_PROVIDER_TYPE".to_string(), provider_type.to_string()),
    ];
    if !api_key.is_empty() {
        env.push(("COPILOT_PROVIDER_API_KEY".to_string(), api_key.to_string()));
    }
    if !model.is_empty() {
        env.push(("COPILOT_MODEL".to_string(), model.to_string()));
    }
    if let Some(proxy) = &outbound_proxy {
        env.push(("HTTP_PROXY".to_string(), proxy.clone()));
        env.push(("HTTPS_PROXY".to_string(), proxy.clone()));
        env.push(("ALL_PROXY".to_string(), proxy.clone()));
    }
    env.push((
        "NO_PROXY".to_string(),
        "localhost,127.0.0.1,::1".to_string(),
    ));

    #[cfg(target_os = "windows")]
    let command_line = {
        let mut lines = Vec::new();
        if let Some(dir) = cwd.as_deref().filter(|s| !s.trim().is_empty()) {
            lines.push(format!("cd /d \"{}\" || exit /b 1", escape_batch(dir)));
        }
        for (k, v) in &env {
            lines.push(format!("set \"{k}={}\"", escape_batch(v)));
        }
        lines.push(format!("\"{}\"", escape_batch(&copilot_bin)));
        lines.join("\r\n")
    };

    #[cfg(not(target_os = "windows"))]
    let command_line = {
        let mut lines = Vec::new();
        if let Some(dir) = cwd.as_deref().filter(|s| !s.trim().is_empty()) {
            lines.push(format!("cd {}", shell_quote(dir)));
        }
        for (k, v) in &env {
            lines.push(format!("export {k}={}", shell_quote(v)));
        }
        lines.push(shell_quote(&copilot_bin));
        lines.join("\n")
    };

    launch_agent_terminal(&command_line, "copilot_cli")?;

    Ok(serde_json::json!({
        "copilotBinary": copilot_bin,
        "providerId": provider.id,
        "env": env,
    }))
}

/// Find the copilot CLI binary (PATH + common npm/standalone locations).
fn find_copilot_cli_binary() -> Option<std::path::PathBuf> {
    #[cfg(target_os = "windows")]
    {
        if let Some(found) = find_on_windows_path("copilot.exe") {
            return Some(found);
        }
        if let Some(found) = find_on_windows_path("copilot.cmd") {
            return Some(found);
        }
        // npm global install: %APPDATA%\npm\copilot.cmd
        if let Some(appdata) = std::env::var_os("APPDATA") {
            let p = std::path::PathBuf::from(appdata).join("npm").join("copilot.cmd");
            if p.exists() {
                return Some(p);
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        if let Some(found) = find_on_unix_path("copilot") {
            return Some(found);
        }
    }
    None
}

#[cfg(target_os = "windows")]
fn escape_batch(value: &str) -> String {
    value
        .replace('^', "^^")
        .replace('%', "%%")
        .replace('&', "^&")
        .replace('|', "^|")
        .replace('<', "^<")
        .replace('>', "^>")
        .replace('(', "^(")
        .replace(')', "^)")
}

#[cfg(not(target_os = "windows"))]
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

/// Run a command script in the user's default/preferred terminal (new window).
/// `script_stem` is used for the temp script/batch filename (e.g. "codex_cli" / "copilot_cli").
fn launch_agent_terminal(command_line: &str, script_stem: &str) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        let bat = std::env::temp_dir().join(format!(
            "aag_{script_stem}_{}.bat",
            std::process::id()
        ));
        let content = format!(
            "@echo off\r\n{command_line}\r\necho.\r\necho [AAG] Press any key to close.\r\npause >nul\r\ndel \"%~f0\" >nul 2>&1\r\n"
        );
        std::fs::write(&bat, content).map_err(|e| format!("failed to write batch file: {e}"))?;
        let bat_path = bat.to_string_lossy().to_string();
        let output = std::process::Command::new("cmd")
            .args(["/C", "start", "cmd", "/K", &bat_path])
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .output()
            .map_err(|e| format!("failed to launch terminal: {e}"))?;
        if !output.status.success() {
            return Err("failed to launch terminal".to_string());
        }
        return Ok(());
    }

    #[cfg(target_os = "macos")]
    {
        let script = std::env::temp_dir().join(format!(
            "aag_{script_stem}_{}.sh",
            std::process::id()
        ));
        let content = format!(
            "#!/bin/sh\n{command_line}\necho \"[AAG] Press Enter to close.\"\nread -r _\nrm -f \"{script_path}\"\n",
            script_path = script.display()
        );
        std::fs::write(&script, content).map_err(|e| format!("failed to write script: {e}"))?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("failed to set permissions: {e}"))?;
        let applescript = format!(
            "tell application \"Terminal\"\n  activate\n  do script \"sh {}\"\nend tell",
            script.display()
        );
        let output = std::process::Command::new("osascript")
            .arg("-e")
            .arg(&applescript)
            .output()
            .map_err(|e| format!("failed to launch terminal: {e}"))?;
        if !output.status.success() {
            return Err("failed to launch terminal".to_string());
        }
        return Ok(());
    }

    #[cfg(target_os = "linux")]
    {
        let script = std::env::temp_dir().join(format!(
            "aag_{script_stem}_{}.sh",
            std::process::id()
        ));
        let content = format!(
            "#!/bin/sh\n{command_line}\necho \"[AAG] Press Enter to close.\"\nread -r _\nrm -f \"{script_path}\"\n",
            script_path = script.display()
        );
        std::fs::write(&script, content).map_err(|e| format!("failed to write script: {e}"))?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("failed to set permissions: {e}"))?;
        for terminal in ["x-terminal-emulator", "gnome-terminal", "konsole", "xfce4-terminal"] {
            if std::process::Command::new(terminal)
                .arg("-e")
                .arg(&script)
                .spawn()
                .is_ok()
            {
                return Ok(());
            }
        }
        return Err("no usable terminal emulator found".to_string());
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        let _ = command_line;
        let _ = script_stem;
        Err("unsupported operating system".to_string())
    }
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

    // Port-fallback: try requested_port, then +1..+5 in order. For each
    // candidate, bind a server, probe /health to confirm we own the port
    // (avoids landing on another app that grabbed the same port), then
    // persist the actual port so the next start prefers it.
    const MAX_PORT_OFFSET: u16 = 5;
    let mut last_err: Option<String> = None;
    let mut chosen: Option<(proxy::server::BoundInfo, tauri::async_runtime::JoinHandle<()>, tokio::sync::oneshot::Sender<()>)> = None;
    let original_port = config.port;
    for offset in 0..=MAX_PORT_OFFSET {
        let try_port = original_port.wrapping_add(offset);
        let try_addr = proxy::server::socket_addr(&config.host, try_port);
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<Option<proxy::server::BoundInfo>>();
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let state_for_try = app_state.clone();
        let handle = tauri::async_runtime::spawn(async move {
            // serve() owns shutdown_rx until its axum loop ends. If bind
            // fails it returns immediately and shutdown_rx is dropped; if
            // bind succeeds the inner axum loop awaits shutdown_rx. Either
            // way, send the bind outcome through ready_tx so the caller can
            // decide whether to keep this server or drop it.
            let outcome = proxy::server::serve(try_addr, state_for_try, shutdown_rx).await;
            let _ = ready_tx.send(outcome.ok());
            // If bind failed, outcome was Err and serve() has already
            // returned — this task is done. If bind succeeded, serve() is
            // still running axum on try_addr; we just hold the task open by
            // awaiting shutdown_rx again... but it's been moved. So if bind
            // succeeded, axum is now serving and will only stop when the
            // *outer* shutdown_rx (held by the caller, not us) is signalled
            // and the original inner axum loop wakes. From this task's POV,
            // we have nothing more to do; signal the outcome and return.
            // (Cleanup on probe failure is handled by dropping handle below —
            // the spawn task terminates when this closure returns, which is
            // safe because serve() has already moved shutdown_rx.)
        });
        // Wait for the bind result.
        let attempt = tokio::time::timeout(std::time::Duration::from_millis(300), ready_rx).await;
        let bound = match attempt {
            Ok(Ok(Some(b))) => b,
            Ok(Ok(None)) => {
                // Bind failed; record and try next.
                last_err = Some(format!("port {} bind failed", try_port));
                // The task has already returned; drop the handle.
                drop(handle);
                let _ = shutdown_tx.send(()); // no-op, but consistent
                continue;
            }
            Ok(Err(_)) => {
                last_err = Some(format!("port {} bind channel closed", try_port));
                drop(handle);
                let _ = shutdown_tx.send(());
                continue;
            }
            Err(_) => {
                // Timeout — bind hung. Treat as failure.
                last_err = Some(format!("port {} bind timed out", try_port));
                drop(handle);
                let _ = shutdown_tx.send(());
                continue;
            }
        };
        // Confirm we actually own the port via /health (rules out a port we
        // successfully bound to but another process already had reserved,
        // which shouldn't happen on Linux/macOS but CAN on Windows when the
        // port is in TIME_WAIT from a prior run).
        let probe = match reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(500))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                last_err = Some(format!("reqwest build: {e}"));
                drop(handle);
                let _ = shutdown_tx.send(());
                continue;
            }
        };
        let probe_url = format!("http://{}/health", bound.addr);
        let probe_ok = probe.get(&probe_url).send().await.map(|r| r.status().is_success()).unwrap_or(false);
        if !probe_ok {
            // Not our server — somebody else owns this port. Tear down and
            // try the next candidate.
            log::warn!("proxy: {} answered /health with non-2xx (or refused), assuming another process owns it", bound.addr);
            last_err = Some(format!("port {} owned by another process", try_port));
            drop(handle);
            let _ = shutdown_tx.send(());
            continue;
        }
        chosen = Some((bound, handle, shutdown_tx));
        break;
    }
    let (bound, handle, shutdown_tx) = chosen.ok_or_else(|| {
        format!(
            "no free port near {} (tried {}+0..+{}): {}",
            original_port, original_port, MAX_PORT_OFFSET, last_err.unwrap_or_else(|| "unknown".into())
        )
    })?;

    // Persist the actual port if it differs from the configured one, so
    // the next start prefers it.
    if bound.addr.port() != original_port {
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
        });
    }
    let msg = if bound.addr.port() == original_port {
        format!("listening on {}:{}", bound.addr.ip(), bound.addr.port())
    } else {
        format!(
            "listening on {}:{} (port {} was in use; saved as the new default)",
            bound.addr.ip(),
            bound.addr.port(),
            original_port
        )
    };
    Ok(msg)
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

// ----- Proxy providers + Routes CRUD (failover config) -----
// The Router is rebuilt from the DB every time `proxy_start` runs, so saving
// a provider or route then restarting the proxy applies the new failover chain.

#[tauri::command]
fn proxy_provider_list(
    db: tauri::State<'_, Database>,
) -> Result<Vec<database::dao::proxy_providers::ProxyProvider>, String> {
    db.get_all_proxy_providers().map_err(String::from)
}

#[tauri::command]
fn proxy_provider_save(
    db: tauri::State<'_, Database>,
    provider: database::dao::proxy_providers::ProxyProvider,
) -> Result<(), String> {
    db.save_proxy_provider(&provider).map_err(String::from)
}

#[tauri::command]
fn proxy_provider_delete(db: tauri::State<'_, Database>, id: String) -> Result<(), String> {
    db.delete_proxy_provider(&id).map_err(String::from)
}

#[tauri::command]
fn route_list(db: tauri::State<'_, Database>) -> Result<Vec<database::dao::routes::Route>, String> {
    db.get_all_routes().map_err(String::from)
}

#[tauri::command]
fn route_save(
    db: tauri::State<'_, Database>,
    route: database::dao::routes::Route,
) -> Result<(), String> {
    db.save_route(&route).map_err(String::from)
}

#[tauri::command]
fn route_delete(db: tauri::State<'_, Database>, alias: String) -> Result<(), String> {
    db.delete_route(&alias).map_err(String::from)
}

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

// ----- Vendor commands (unified upstream supplier management) -----
// One vendor may serve multiple protocols (anthropic / openai-chat /
// openai-responses) sharing a single API key. The smart provider form
// resolves endpoint = endpoints[protocol] ?? base_url.

#[tauri::command]
fn vendor_list(db: tauri::State<'_, Database>) -> Result<Vec<database::dao::vendors::Vendor>, String> {
    db.get_all_vendors().map_err(String::from)
}

#[tauri::command]
fn vendor_save(
    db: tauri::State<'_, Database>,
    vendor: database::dao::vendors::Vendor,
) -> Result<(), String> {
    db.save_vendor(&vendor).map_err(String::from)
}

#[tauri::command]
fn vendor_delete(db: tauri::State<'_, Database>, id: String) -> Result<(), String> {
    db.delete_vendor(&id).map_err(String::from)
}

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
            settings_get, settings_set,
            codex_cli_status, codex_open_cli_terminal, copilot_open_cli_terminal,
            proxy_status, proxy_start, proxy_stop,
            proxy_provider_list, proxy_provider_save, proxy_provider_delete,
            route_list, route_save, route_delete,
            backup_create, backup_list, backup_delete,
            mcp_list, mcp_save, mcp_delete, mcp_toggle_app,
            skill_list, skill_save, skill_delete,
            session_list, session_get, session_delete,
            usage_record, usage_summary, usage_by_model, usage_by_provider,
            sync_list, sync_save, sync_delete, sync_record_result,
            sync_check, sync_upload, sync_download,
            vendor_list, vendor_save, vendor_delete,
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