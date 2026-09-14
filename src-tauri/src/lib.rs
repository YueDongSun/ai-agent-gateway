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
/// Mark a provider as current in the DB and (per app) write the live config
/// files.
///
/// Live-config split (Phase 17):
/// - `codex` -> write to the mirror CODEX_HOME (`<codex_cli_home>/config.toml`
///   + `auth.json`). The user's native `~/.codex` is **never** touched.
/// - `copilot` -> merge `model` + `env` into `~/.copilot/settings.json`.
/// - `pi` -> upsert the provider into `~/.pi/agent/models.json` + default
///   model into `~/.pi/agent/settings.json`.
/// - `claude_code` -> no live write. cc-switch owns `~/.claude/settings.json`;
///   AAG is just an upstream proxy at `http://127.0.0.1:8989` that cc-switch
///   chains through.
fn provider_switch(
    db: tauri::State<'_, Database>,
    id: String,
    app_type: String,
) -> Result<services::provider::LiveWriteResult, String> {
    let _app = services::provider::AppType::from_db_str(&app_type).map_err(String::from)?;
    let provider = db.get_provider_by_id(&id, &app_type)
        .map_err(String::from)?
        .ok_or_else(|| format!("provider \"{id}\" not found for app \"{app_type}\""))?;

    db.set_current_provider(&id, &app_type).map_err(String::from)?;

    let live = match app_type.as_str() {
        "codex" => {
            let codex_cli_home = db
                .get_setting(database::dao::settings::KEY_CODEX_CLI_HOME)
                .map_err(String::from)?;
            services::provider::codex::write_live(&provider, codex_cli_home.as_deref()).ok()
        }
        "copilot" => services::provider::copilot::write_live(&provider).ok(),
        "pi" => services::provider::pi::write_live(&provider).ok(),
        _ => None,
    };
    if live.is_none() {
        if app_type != "claude_code" {
            log::warn!("provider_switch: {app_type} live write failed or unsupported");
        }
    }

    Ok(live.unwrap_or(services::provider::LiveWriteResult {
        app_type,
        provider_id: provider.id,
        written: vec![],
        backup: None,
    }))
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
    let config_dir = crate::config::get_codex_mirror_dir(
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
/// Extract the upstream (base_url, api_key, provider_type) a provider row
/// contributes to its app's failover chain. Each app's `settings_config`
/// shape differs (see the per-app adapters in `services/provider/`):
/// - claude_code: `env.ANTHROPIC_BASE_URL` + `env.ANTHROPIC_AUTH_TOKEN` (anthropic)
/// - codex: flat `baseUrl`/`apiKey`, or parsed from the TOML `config` string
/// - copilot: `env.COPILOT_PROVIDER_BASE_URL`/`_API_KEY` (+ `_TYPE` for the wire)
/// - pi: flat `baseUrl`/`apiKey` + `api` wire selector
fn queue_endpoint_of(
    app_type: &str,
    cfg: &serde_json::Value,
) -> Option<(String, String, String)> {
    let env = cfg.get("env").cloned().unwrap_or(serde_json::json!({}));
    match app_type {
        "claude_code" => {
            let base = env.get("ANTHROPIC_BASE_URL")?.as_str()?.to_string();
            if base.is_empty() {
                return None;
            }
            let key = env
                .get("ANTHROPIC_AUTH_TOKEN")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Some((base, key, "anthropic".into()))
        }
        "codex" => {
            // Flat fields first (the smart form writes these)...
            if let Some(base) = cfg.get("baseUrl").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
                let key = cfg.get("apiKey").and_then(|v| v.as_str()).unwrap_or("").to_string();
                return Some((base.to_string(), key, "openai".into()));
            }
            // ...then the cc-switch TOML `config` string (model_providers.aag.base_url).
            let toml_str = cfg.get("config").and_then(|v| v.as_str())?;
            let base = toml_str
                .lines()
                .find_map(|l| {
                    let l = l.trim();
                    let (k, v) = l.split_once('=')?;
                    if k.trim() == "base_url" {
                        Some(v.trim().trim_matches('"').to_string())
                    } else {
                        None
                    }
                })?
                .to_string();
            if base.is_empty() {
                return None;
            }
            let key = cfg
                .get("auth")
                .and_then(|a| a.get("tokens"))
                .and_then(|t| t.get("value"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Some((base, key, "openai".into()))
        }
        "copilot" => {
            let base = env.get("COPILOT_PROVIDER_BASE_URL")?.as_str()?.to_string();
            if base.is_empty() {
                return None;
            }
            let key = env
                .get("COPILOT_PROVIDER_API_KEY")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let wire = env
                .get("COPILOT_PROVIDER_TYPE")
                .and_then(|v| v.as_str())
                .unwrap_or("openai");
            let provider_type = if wire == "anthropic" { "anthropic" } else { "openai" };
            Some((base, key, provider_type.into()))
        }
        "pi" => {
            let base = cfg.get("baseUrl")?.as_str()?.to_string();
            if base.is_empty() {
                return None;
            }
            let key = cfg.get("apiKey").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let provider_type = match cfg.get("api").and_then(|v| v.as_str()).unwrap_or("") {
                "anthropic-messages" => "anthropic",
                _ => "openai",
            };
            Some((base, key, provider_type.into()))
        }
        _ => None,
    }
}

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

    // cc-switch coexistence guard: disabled in the mirror-install model
    // (Phase 16). The user runs Codex Desktop + Codex CLI separately; AAG
    // only writes its own mirror CODEX_HOME and the guard is moot.

    let config_dir = crate::config::get_codex_mirror_dir(codex_cli_home.as_deref());
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

/// Locate the installed Codex Desktop executable (the Electron main binary).
///
/// Scan order:
///   1. `%LOCALAPPDATA%\OpenAI\Codex\ChatGPT.exe` (per-user MSI install)
///   2. `%PROGRAMFILES%\OpenAI\Codex\ChatGPT.exe` (machine-wide MSI)
///   3. Windows Store: `Get-AppxPackage *Codex*` → InstallLocation\app\ChatGPT.exe
///      (Get-AppxPackage reads the deployment API, bypassing the WindowsApps
///      folder ACL that blocks raw read_dir)
///   4. macOS: `/Applications/Codex.app/Contents/MacOS/Codex`
///   5. Linux: `/usr/bin/codex-desktop` / `/usr/local/bin/codex-desktop`
fn find_codex_desktop_exe() -> Option<std::path::PathBuf> {
    #[cfg(target_os = "windows")]
    {
        // 1-2. MSI / NSIS installs — the Electron main binary is ChatGPT.exe.
        let bases: [Option<std::ffi::OsString>; 2] = [
            std::env::var_os("LOCALAPPDATA"),
            std::env::var("PROGRAMFILES").ok().map(std::ffi::OsString::from),
        ];
        for base in bases {
            if let Some(b) = base {
                let p = std::path::PathBuf::from(&b)
                    .join("OpenAI").join("Codex").join("ChatGPT.exe");
                if p.exists() {
                    return Some(p);
                }
            }
        }
        // 3. Windows Store install. The package binary lives at
        //    <InstallLocation>\app\ChatGPT.exe. Resolve InstallLocation via
        //    PowerShell Get-AppxPackage (the WindowsApps folder ACL denies
        //    raw read_dir enumeration for normal users).
        if let Some(loc) = codex_desktop_install_location() {
            let p = loc.join("app").join("ChatGPT.exe");
            if p.exists() {
                return Some(p);
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        let p = std::path::PathBuf::from("/Applications/Codex.app/Contents/MacOS/Codex");
        if p.exists() {
            return Some(p);
        }
    }
    #[cfg(target_os = "linux")]
    {
        for c in [
            "/usr/bin/codex-desktop",
            "/usr/local/bin/codex-desktop",
        ] {
            let p = std::path::PathBuf::from(c);
            if p.exists() {
                return Some(p);
            }
        }
    }
    None
}

/// Windows Store package lookup: returns the Codex Desktop InstallLocation via
/// PowerShell Get-AppxPackage (bypasses the WindowsApps folder ACL).
#[cfg(target_os = "windows")]
fn codex_desktop_install_location() -> Option<std::path::PathBuf> {
    let output = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Get-AppxPackage *Codex* | Select-Object -First 1 -ExpandProperty InstallLocation",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let loc = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if loc.is_empty() {
        return None;
    }
    Some(std::path::PathBuf::from(loc))
}

/// The native Codex Desktop's Chromium profile (Electron userData), used to
/// seed the mirror's electron dir on first launch so the parasite inherits
/// the login session (cookies + localStorage) instead of showing the login
/// page. For Store installs this lives under
/// `%LOCALAPPDATA%\Packages\<PackageFamilyName>\LocalCache\Roaming\Codex\`.
#[cfg(target_os = "windows")]
fn codex_desktop_native_chromium_profile() -> Option<std::path::PathBuf> {
    // Get the PackageFamilyName (e.g. "OpenAI.Codex_2p2nqsd0c76g0").
    let output = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Get-AppxPackage *Codex* | Select-Object -First 1 -ExpandProperty PackageFamilyName",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let fam = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if fam.is_empty() || !fam.contains('.') {
        return None;
    }
    let local = std::env::var_os("LOCALAPPDATA")?;
    let p = std::path::PathBuf::from(local)
        .join("Packages")
        .join(&fam)
        .join("LocalCache")
        .join("Roaming")
        .join("Codex");
    if p.exists() {
        Some(p)
    } else {
        None
    }
}

/// Generate `cc-switch-model-catalog.json` from the active Codex provider's
/// `modelCatalog` (user-configured display→upstream mappings), plus fallback
/// sources (proxy_providers, routes, OFFICIAL_MODELS).
///
/// The active Codex provider's `settings_config.modelCatalog.models` has the
/// shape `[{ "display": "GPT-5", "model": "deepseek-v4-flash", "provider": "deepseek" }]`:
/// - `display` = what Codex Desktop shows in the dropdown AND sends in the
///   API request (becomes the catalog entry's `id` and `display_name`).
/// - `model` = the actual upstream model (used by the proxy route hop).
/// - `provider` = the proxy_provider id that serves this model.
///
/// Entries from the provider's modelCatalog take priority. Fallback sources
/// (proxy_providers.models, routes.alias, OFFICIAL_MODELS) fill in any gaps.
fn write_codex_model_catalog(
    db: &Database,
    config_dir: &std::path::Path,
) -> Result<(), String> {
    use crate::proxy::models::OFFICIAL_MODELS;
    use std::collections::{BTreeMap, BTreeSet};

    // --- Collect models from the active Codex provider's modelCatalog ---
    // Key: display name (what Codex sends), Value: (upstream model, context, desc).
    let mut catalog: BTreeMap<String, (String, u64, String)> = BTreeMap::new();

    if let Ok(Some(cur_id)) = db.get_current_provider("codex") {
        if let Ok(Some(prov)) = db.get_provider_by_id(&cur_id, "codex") {
            if let Some(mc) = prov.settings_config.get("modelCatalog") {
                if let Some(models) = mc.get("models").and_then(|v| v.as_array()) {
                    for entry in models {
                        let display = entry.get("display").and_then(|v| v.as_str()).unwrap_or("");
                        let model = entry.get("model").and_then(|v| v.as_str()).unwrap_or(display);
                        if display.is_empty() { continue; }
                        let (ctx, _max, desc) = model_meta(model);
                        catalog.insert(display.to_string(), (model.to_string(), ctx, desc.to_string()));
                    }
                }
            }
        }
    }

    // --- Fallback: proxy_providers, routes, OFFICIAL_MODELS ---
    let mut fallback_ids: BTreeSet<String> = BTreeSet::new();
    if let Ok(provs) = db.get_all_proxy_providers() {
        for p in &provs { for m in &p.models { fallback_ids.insert(m.clone()); } }
    }
    if let Ok(routes) = db.get_all_routes() {
        for r in &routes { fallback_ids.insert(r.alias.clone()); }
    }
    for m in OFFICIAL_MODELS { fallback_ids.insert(m.id.to_string()); }

    for model_id in &fallback_ids {
        catalog.entry(model_id.clone()).or_insert_with(|| {
            let (ctx, _max, desc) = model_meta(model_id);
            (model_id.clone(), ctx, desc.to_string())
        });
    }

    if catalog.is_empty() { return Ok(()); }

    let base_instructions = read_native_base_instructions()
        .unwrap_or_else(|| DEFAULT_CODEX_INSTRUCTIONS.to_string());

    let mut entries: Vec<serde_json::Value> = Vec::new();
    for (display, (upstream_model, _ctx_hint, description)) in &catalog {
        let (ctx, max_out, _) = model_meta(upstream_model);
        entries.push(serde_json::json!({
            "apply_patch_tool_type": "freeform",
            "auto_compact_token_limit": null,
            "auto_review_model_override": null,
            "availability_nux": null,
            "base_instructions": base_instructions,
            "comp_hash": "3000",
            "context_window": ctx,
            "default_reasoning_level": "high",
            "default_reasoning_summary": "none",
            "default_service_tier": null,
            "default_verbosity": "low",
            "description": description,
            "display_name": display,
            "effective_context_window_percent": 95,
            "experimental_supported_tools": [],
            "include_skills_usage_instructions": false,
            "input_modalities": ["text"],
            "max_context_window": ctx,
            "minimal_client_version": "0.144.0",
            "model_messages": { "approvals": null, "instructions_template": null, "instructions_variables": null },
            "multi_agent_version": "v2",
            "prefer_websockets": false,
            "priority": 1,
            "reasoning_summary_format": "experimental",
            "shell_type": "shell_command",
            "slug": display.to_lowercase().replace('_', "-"),
            "support_verbosity": true,
            "supported_in_api": true,
            "supported_reasoning_levels": [
                { "description": "Fast responses with lighter reasoning", "effort": "low" },
                { "description": "Balanced reasoning depth", "effort": "medium" },
                { "description": "Maximum reasoning depth for the hardest problems", "effort": "max" }
            ],
            "supports_image_detail_original": false,
            "supports_parallel_tool_calls": true,
            "supports_reasoning_summaries": true,
            "supports_search_tool": true,
            "tool_mode": null,
            "truncation_policy": { "mode": "rolling", "limit": max_out },
            "upgrade": null,
            "use_responses_lite": false,
            "visibility": "list",
            "web_search_tool_type": "text",
        }));
    }

    let catalog_json = serde_json::json!({ "models": entries });
    let catalog_path = config_dir.join("cc-switch-model-catalog.json");
    let json = serde_json::to_string_pretty(&catalog_json)
        .map_err(|e| format!("serialize model catalog: {e}"))?;
    std::fs::write(&catalog_path, json)
        .map_err(|e| format!("write model catalog: {e}"))?;
    Ok(())
}

/// Heuristic model metadata based on the model id prefix.
fn model_meta(id: &str) -> (u64, u64, &'static str) {
    let lower = id.to_lowercase();
    if lower.starts_with("claude-") {
        if lower.contains("opus") || lower.contains("fable") {
            (200_000, 32_000, "High-capacity Anthropic model for complex agentic tasks.")
        } else if lower.contains("haiku") {
            (200_000, 8_192, "Fast, lightweight Anthropic model.")
        } else {
            (200_000, 16_000, "Balanced Anthropic model for coding and reasoning.")
        }
    } else if lower.starts_with("gpt-") || lower.starts_with("o3") || lower.starts_with("o4") {
        (128_000, 16_384, "OpenAI model for general-purpose coding and reasoning.")
    } else if lower.starts_with("deepseek") {
        (1_000_000, 16_384, "DeepSeek model with large context window.")
    } else {
        (128_000, 8_192, "Model available through AAG gateway.")
    }
}

/// Try to read the `base_instructions` from the first model in the native
/// `~/.codex/cc-switch-model-catalog.json`. Returns None if the file doesn't
/// exist or is malformed.
fn read_native_base_instructions() -> Option<String> {
    let native_catalog = crate::config::get_codex_config_dir().join("cc-switch-model-catalog.json");
    if !native_catalog.exists() {
        return None;
    }
    let content = std::fs::read_to_string(&native_catalog).ok()?;
    let v: serde_json::Value = serde_json::from_str(&content).ok()?;
    v.get("models")?
        .as_array()?
        .first()?
        .get("base_instructions")?
        .as_str()
        .map(String::from)
}

/// Fallback instructions when the native catalog is unavailable.
const DEFAULT_CODEX_INSTRUCTIONS: &str = "You are a helpful coding assistant. You and the user share one workspace. Collaborate with them until their goal is genuinely handled. Be concise and direct. Use the tools available to you to read, write, and execute code. When providing code, use the apply_patch tool with the freeform patch format.";

/// Open a SECOND Codex Desktop instance — a "parasite" running against AAG's
/// mirror CODEX_HOME, alongside the user's native instance (which is never
/// touched).
///
/// Mechanism (verified end-to-end on a Store install):
///   1. Auto-write mirror `config.toml` + `auth.json` (pointing at AAG's
///      proxy, so the Desktop routes through AAG; the proxy handles failover).
///   2. Write a temporary `.bat` that sets `CODEX_HOME=<mirror>` then runs
///      `start "" "<exe>" --user-data-dir=<mirror>/electron`.
///   3. Launch the bat via `cmd /c` (CreateProcess on `cmd.exe`, which is
///      always allowed). `cmd`'s internal `start` uses ShellExecute, which
///      correctly handles WindowsApps paths (unlike raw CreateProcess, which
///      is blocked by package-identity protection — os error 5).
///   4. ChatGPT.exe inherits `CODEX_HOME` from the cmd process → reads the
///      mirror config → routes through AAG's gateway.
///   5. `--user-data-dir` separates the Chromium profile → bypasses Electron's
///      single-instance lock → parasite coexists with the native instance.
///
/// No elevation, no UAC, no user-env dance, no killing the native instance.
/// The native `~/.codex` is never written.
#[tauri::command]
fn codex_open_desktop_window(
    db: tauri::State<'_, Database>,
) -> Result<serde_json::Value, String> {
    let exe = find_codex_desktop_exe()
        .ok_or_else(|| "Codex Desktop not found; install it or use \"Open Codex CLI in terminal\"".to_string())?;

    let codex_cli_home = db
        .get_setting(database::dao::settings::KEY_CODEX_CLI_HOME)
        .map_err(String::from)?;
    let config_dir = crate::config::get_codex_mirror_dir(codex_cli_home.as_deref());
    std::fs::create_dir_all(&config_dir).map_err(|e| format!("failed to create CODEX_HOME: {e}"))?;
    let codex_home = config_dir.to_string_lossy().to_string();
    let electron_dir = config_dir.join("electron");
    std::fs::create_dir_all(&electron_dir).map_err(|e| format!("failed to create electron dir: {e}"))?;
    let electron_dir_str = electron_dir.to_string_lossy().to_string();

    // Auto-write mirror config.toml + auth.json (always points at AAG proxy).
    let proxy_cfg = db.get_proxy_config().map_err(String::from)?;
    let gateway_url = format!("http://{}:{}/v1", proxy_cfg.host, proxy_cfg.port);
    let default_model = db.get_current_provider("codex")
        .ok()
        .flatten()
        .and_then(|id| db.get_provider_by_id(&id, "codex").ok().flatten())
        .and_then(|p| p.settings_config.get("model").and_then(|v| v.as_str()).map(String::from))
        .unwrap_or_else(|| "gpt-5".into());
    let mirror_config = format!(
        "model = \"{model}\"\nmodel_provider = \"aag\"\n\n\
         [model_providers.aag]\n\
         name = \"AAG Gateway\"\n\
         base_url = \"{url}\"\n\
         wire_api = \"responses\"\n\
         requires_openai_auth = true\n",
        model = default_model,
        url = gateway_url,
    );
    let config_toml_path = config_dir.join("config.toml");
    if config_toml_path.exists() {
        let ts = chrono::Utc::now().timestamp();
        let bak = config_dir.join(format!("config.toml.{ts}.bak"));
        let _ = std::fs::copy(&config_toml_path, &bak);
    }
    std::fs::write(&config_toml_path, &mirror_config)
        .map_err(|e| format!("failed to write mirror config.toml: {e}"))?;
    let auth_path = config_dir.join("auth.json");
    // Copy native auth.json into the mirror so the parasite sees valid
    // credentials (OAuth tokens / API key) and skips the login screen.
    // Read-only copy — native ~/.codex/auth.json is never modified.
    let native_auth = crate::config::get_codex_auth_path();
    if native_auth.exists() {
        std::fs::copy(&native_auth, &auth_path)
            .map_err(|e| format!("failed to copy native auth.json to mirror: {e}"))?;
    } else if !auth_path.exists() {
        std::fs::write(&auth_path, "{}")
            .map_err(|e| format!("failed to write mirror auth.json: {e}"))?;
    }

    // Generate cc-switch-model-catalog.json from AAG's proxy providers +
    // routes + official models. This tells Codex Desktop which models are
    // actually available through AAG's gateway (instead of showing only the
    // GPT defaults from the inherited models_cache.json).
    write_codex_model_catalog(&db, &config_dir)?;

    // Write a temp .bat that sets CODEX_HOME + launches via cmd's `start`
    // (ShellExecute handles WindowsApps paths; raw CreateProcess does not).
    // On first launch, seed the mirror's Chromium profile from the native
    // one (robocopy /E, excluding large cache dirs) so the parasite
    // inherits the login session and skips the ChatGPT login page.
    let bat_path = std::env::temp_dir().join(format!("aag_codex_desktop_{}.bat", std::process::id()));
    let exe_str = exe.to_string_lossy().to_string();
    // Native Chromium profile (Electron userData) — only needed for the
    // first-launch robocopy seed.
    let native_profile = codex_desktop_native_chromium_profile()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    let electron_win = electron_dir_str.replace('/', "\\");
    let native_win = native_profile.replace('/', "\\");
    let bat_content = if native_profile.is_empty() {
        // No native profile found (MSI install or non-Store) — just launch.
        format!(
            "@echo off\r\nset \"CODEX_HOME={home}\"\r\nstart \"\" \"{exe}\" --user-data-dir=\"{electron}\"\r\n",
            home = codex_home, exe = exe_str, electron = electron_dir_str,
        )
    } else {
        // Store install: seed the mirror Chromium profile on first launch
        // (when "Local Storage" dir doesn't exist yet in the mirror).
        // robocopy /E copies dirs recursively; /XD excludes large cache
        // dirs; /R:1 /W:0 = one retry, no wait (skip locked files).
        format!(
            "@echo off\r\n\
set \"CODEX_HOME={home}\"\r\n\
if not exist \"{electron}\\Local Storage\" (\r\n\
  echo [AAG] Seeding Chromium profile from native...\r\n\
  robocopy \"{native}\" \"{electron}\" /E /R:1 /W:0 /NFL /NDL /NJH /NJS /nc /ns /np\
 /XD Cache \"Code Cache\" GPUCache \"DawnGraphiteCache\" \"DawnWebGPUCache\"\
 Crashpad DIPS DIPS-wal blob_storage >nul 2>&1\r\n\
)\r\n\
start \"\" \"{exe}\" --user-data-dir=\"{electron}\"\r\n",
            home = codex_home, exe = exe_str, electron = electron_win, native = native_win,
        )
    };
    std::fs::write(&bat_path, &bat_content)
        .map_err(|e| format!("failed to write launcher bat: {e}"))?;
    let bat_path_str = bat_path.to_string_lossy().to_string();

    let mut cmd = std::process::Command::new("cmd");
    cmd.args(["/C", &bat_path_str]);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.spawn().map_err(|e| format!("failed to launch Codex Desktop: {e}"))?;

    Ok(serde_json::json!({
        "codexHome": codex_home,
        "codexBinary": exe_str,
        "mirror": true,
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

    // cc-switch coexistence guard: refuse to launch a terminal that would
    // inject COPILOT_PROVIDER_BASE_URL pointing at AAG's proxy while
    // cc-switch is also running.
    // cc-switch guard removed (mirror-install model, Phase 16).

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
    handle: tokio::task::JoinHandle<Result<(), String>>,
    shutdown: tokio::sync::oneshot::Sender<()>,
    config: database::dao::proxy_config::ProxyConfig,
    state: proxy::server::ProxyAppState,
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

/// Read the proxy listen config + auth token (for the endpoint card the
/// user copies into cc-switch).
#[tauri::command]
fn proxy_config_get(
    db: tauri::State<'_, Database>,
) -> Result<ProxyConfig, String> {
    db.get_proxy_config().map_err(String::from)
}

/// Update the proxy listen config (host / port / auth token). Takes
/// effect on next proxy start (or immediately via stop + start).
#[tauri::command]
fn proxy_config_save(
    db: tauri::State<'_, Database>,
    config: ProxyConfig,
) -> Result<(), String> {
    db.save_proxy_config(&config).map_err(String::from)
}

#[tauri::command]
fn provider_reorder(
    db: tauri::State<'_, Database>,
    app_type: String,
    ordered_ids: Vec<String>,
) -> Result<(), String> {
    db.reorder_providers(&app_type, &ordered_ids).map_err(String::from)
}

/// Toggle a provider's membership in its app's failover chain (the per-card
/// "join the chain" switch). Rows with the bit off are excluded when the
/// proxy builds the per-app queue at `proxy_start`.
#[tauri::command]
fn provider_set_failover_queue(
    db: tauri::State<'_, Database>,
    id: String,
    app_type: String,
    in_queue: bool,
) -> Result<(), String> {
    db.set_in_failover_queue(&id, &app_type, in_queue)
        .map_err(String::from)
}

/// Get the currently pinned proxy provider (force-pin mode). Returns null if none.
#[tauri::command]
async fn proxy_get_pinned_provider(
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
async fn proxy_set_pinned_provider(
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
        database::dao::settings::KEY_PROXY_PINNED_PROVIDER,
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
async fn proxy_test_provider(
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

    let provider_instance = proxy::providers::build_provider(&pp)
        .map_err(|e| format!("build provider: {e}"))?;

    let req = proxy::canonical::ChatCompletionRequest {
        model: model.clone(),
        messages: vec![proxy::canonical::ChatMessage {
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
async fn proxy_fetch_models(
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
    start_proxy_impl(&db, state.inner()).await
}

/// Body of `proxy_start` minus the "already running" check, so the
/// Tauri setup hook can auto-start the proxy when `proxy_config.enabled`
/// is true (set by `aag-setup` or by the user via the Proxy page).
async fn start_proxy_impl(
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
                        routes.insert(display, vec![database::dao::routes::RouteHop {
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
            routes.insert(display.clone(), vec![database::dao::routes::RouteHop {
                provider: pp.id.clone(),
                model: upstream_model.clone(),
            }]);
        }
    }

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
        .get_setting(database::dao::settings::KEY_SYNC_WITH_CCSWITCH)
        .map_err(String::from)?
        .as_deref()
        == Some("1");
    if sync_on {
        let cc_path = services::import_cc_switch::cc_switch_db_path();
        match services::import_cc_switch::sync_claude_code_from_cc_switch(db, &cc_path) {
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
        ("claude_code", proxy::router::SURFACE_CLAUDE),
        ("codex", proxy::router::SURFACE_CODEX),
        ("copilot", proxy::router::SURFACE_CHAT),
        ("pi", proxy::router::SURFACE_CHAT),
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
            let pp = database::dao::proxy_providers::ProxyProvider {
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
            match proxy::providers::build_provider(&pp) {
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

    let router = Arc::new(
        proxy::router::Router::new(providers, routes)
            .with_queue(queue_ids.clone())
            .with_queues(queues),
    );
    let pinned = db
        .get_setting(database::dao::settings::KEY_PROXY_PINNED_PROVIDER)
        .ok()
        .flatten();
    let app_state = proxy::server::ProxyAppState {
        router,
        auth_token: config.auth_token.clone(),
        pinned_provider: Arc::new(std::sync::Mutex::new(pinned)),
    };

    // Port-fallback: try requested_port, then +1..+5 in order. For each
    // candidate, bind via serve_detached (bind outcome known immediately —
    // no readiness guessing), probe /health to confirm we own the port
    // (avoids landing on another app), then persist the actual port.
    const MAX_PORT_OFFSET: u16 = 5;
    let mut errors: Vec<String> = Vec::new();
    let mut chosen: Option<(proxy::server::BoundInfo, tokio::task::JoinHandle<Result<(), String>>, tokio::sync::oneshot::Sender<()>)> = None;
    let original_port = config.port;
    for offset in 0..=MAX_PORT_OFFSET {
        let try_port = original_port.wrapping_add(offset);
        let try_addr = proxy::server::socket_addr(&config.host, try_port);
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let (bound, handle) = match proxy::server::serve_detached(try_addr, app_state.clone(), shutdown_rx).await {
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
    let (bound, handle, shutdown_tx) = chosen.ok_or_else(|| {
        format!(
            "no free port near {} (tried {}+0..+{}): {}",
            original_port,
            original_port,
            MAX_PORT_OFFSET,
            if errors.is_empty() { "unknown".into() } else { errors.join("; ") }
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
            state: app_state.clone(),
        });
    }
    let mut msg = if bound.addr.port() == original_port {
        format!("listening on {}:{}", bound.addr.ip(), bound.addr.port())
    } else {
        format!(
            "listening on {}:{} (port {} was in use; saved as the new default)",
            bound.addr.ip(),
            bound.addr.port(),
            original_port
        )
    };
    if !queue_ids.is_empty() {
        msg.push_str(&format!(" · failover queue: {} provider(s)", queue_ids.len()));
    }
    Ok(msg)
}

#[tauri::command]
async fn proxy_stop(state: tauri::State<'_, Mutex<Option<RunningProxy>>>) -> Result<(), String> {
    let mut guard = state.lock().await;
    if let Some(rp) = guard.take() {
        rp.shutdown.send(()).map_err(|_| "shutdown signal failed".to_string())?;
        // Wait for the server task to finish gracefully.
        let result = rp.handle.await.map_err(|e| e.to_string())?;
        result?;
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

/// Reorder a route's hops (drag-and-drop in the Claude page's failover
/// chain editor). Persists the new order via the same upsert as `route_save`.
#[tauri::command]
fn route_reorder_hops(
    db: tauri::State<'_, Database>,
    alias: String,
    ordered_providers: Vec<String>,
) -> Result<(), String> {
    let mut route = db
        .get_all_routes()
        .map_err(String::from)?
        .into_iter()
        .find(|r| r.alias == alias)
        .ok_or_else(|| format!("route \"{alias}\" not found"))?;
    let mut new_hops = Vec::with_capacity(ordered_providers.len());
    for pid in &ordered_providers {
        let hop = route
            .hops
            .iter()
            .find(|h| &h.provider == pid)
            .cloned()
            .ok_or_else(|| format!("hop provider \"{pid}\" not in route \"{alias}\""))?;
        new_hops.push(hop);
    }
    route.hops = new_hops;
    db.save_route(&route).map_err(String::from)
}

/// One-shot import of all providers from the cc-switch SQLite DB at the
/// default path (`~/.cc-switch/cc-switch.db` or `CC_SWITCH_DB_PATH`).
/// Idempotent: rows whose `(id, app_type)` already exists in AAG are
/// skipped, not overwritten. Returns a structured report so the UI can
/// toast the number imported + skipped.
#[tauri::command]
fn import_cc_switch_providers(
    db: tauri::State<'_, Database>,
) -> Result<services::import_cc_switch::ImportReport, String> {
    let path = services::import_cc_switch::cc_switch_db_path();
    services::import_cc_switch::import_from_db(&db, &path).map_err(String::from)
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
            provider_list, provider_current, provider_get, provider_save, provider_delete, provider_switch,
            settings_get, settings_set,
            codex_cli_status, codex_open_cli_terminal, codex_open_desktop_window,
            copilot_open_cli_terminal,
            proxy_status, proxy_start, proxy_stop, proxy_config_get, proxy_config_save,
            proxy_get_pinned_provider, proxy_set_pinned_provider,
            proxy_test_provider, proxy_fetch_models,
            provider_reorder, provider_set_failover_queue,
            proxy_provider_list, proxy_provider_save, proxy_provider_delete,
            route_list, route_save, route_delete, route_reorder_hops,
            import_cc_switch_providers,
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
                    let state = handle.state::<Mutex<Option<RunningProxy>>>();
                    match start_proxy_impl(db.inner(), state.inner()).await {
                        Ok(msg) => log::info!("proxy auto-start: {msg}"),
                        Err(e) => log::warn!("proxy auto-start failed: {e}"),
                    }
                });
            }

            Ok(())
        });

    builder
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}