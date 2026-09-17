//! Codex CLI / Codex Desktop / Copilot CLI launcher commands.
//!
//! Extracted from `lib.rs` in the file-structure refactor - the command
//! bodies are moved verbatim; only module paths in `use` statements and
//! `pub(crate)` visibility changed.


use crate::database::Database;
use serde::Serialize;

// ----- Codex CLI isolation -----

/// Codex CLI isolation status (for the frontend to render).
#[derive(Serialize)]
pub(crate) struct CodexCliStatusDto {
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
pub(crate) fn codex_cli_status(db: tauri::State<'_, Database>) -> Result<CodexCliStatusDto, String> {
    let codex_cli_home = db
        .get_setting(crate::database::dao::settings::KEY_CODEX_CLI_HOME)
        .map_err(String::from)?;
    let outbound_proxy = db
        .get_setting(crate::database::dao::settings::KEY_OUTBOUND_PROXY)
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
pub(crate) fn codex_open_cli_terminal(
    db: tauri::State<'_, Database>,
    cwd: Option<String>,
) -> Result<serde_json::Value, String> {
    let codex_bin = find_codex_cli_binary()
        .ok_or_else(|| "codex CLI not found; make sure it is installed and on PATH".to_string())?;
    let codex_bin = codex_bin.to_string_lossy().to_string();

    let codex_cli_home = db
        .get_setting(crate::database::dao::settings::KEY_CODEX_CLI_HOME)
        .map_err(String::from)?;
    let outbound_proxy = db
        .get_setting(crate::database::dao::settings::KEY_OUTBOUND_PROXY)
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
#[cfg(target_os = "windows")]
pub(crate) fn codex_open_desktop_window(
    db: tauri::State<'_, Database>,
) -> Result<serde_json::Value, String> {
    let exe = find_codex_desktop_exe()
        .ok_or_else(|| "Codex Desktop not found; install it or use \"Open Codex CLI in terminal\"".to_string())?;

    let codex_cli_home = db
        .get_setting(crate::database::dao::settings::KEY_CODEX_CLI_HOME)
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

#[tauri::command]
#[cfg(not(target_os = "windows"))]
pub(crate) fn codex_open_desktop_window(
    _db: tauri::State<'_, Database>,
) -> Result<serde_json::Value, String> {
    Err("Opening an isolated Codex Desktop mirror is currently supported only on Windows".to_string())
}

/// Launch the Copilot CLI in an isolated terminal (injecting
/// COPILOT_PROVIDER_* + outbound proxy environment variables). The Copilot
/// CLI's custom provider can only be injected via environment variables at
/// launch (there is no persisted provider config), so AAG reads
/// baseUrl/apiKey/model from the currently selected Copilot provider's
/// settings_config and opens a terminal with `COPILOT_PROVIDER_BASE_URL`
/// and friends set.
#[tauri::command]
pub(crate) fn copilot_open_cli_terminal(
    db: tauri::State<'_, Database>,
    cwd: Option<String>,
) -> Result<serde_json::Value, String> {
    let outbound_proxy = db
        .get_setting(crate::database::dao::settings::KEY_OUTBOUND_PROXY)
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

