//! Platform-aware launcher services; IPC scheduling lives in commands/launcher.rs.

mod claude;
#[cfg(windows)]
mod codex_catalog;
mod discovery;
#[cfg(any(windows, test))]
mod mirror_seed;
mod process;
mod terminal;
#[cfg(test)]
mod tests;
mod vscode;

use crate::database::Database;
pub(crate) use claude::{
    claude_mirror_set_mode, claude_mirror_status, claude_open_cli_terminal, claude_open_desktop,
    ClaudeMirrorStatusDto,
};
#[cfg(windows)]
use codex_catalog::write_codex_model_catalog;
#[cfg(windows)]
use discovery::find_codex_desktop_exe;
use discovery::{find_codex_cli_binary, find_copilot_cli_binary};
use serde::Serialize;
pub(crate) use vscode::{
    vscode_extension_status, vscode_open_with_extension, VsCodeExtensionStatusDto,
};

// ----- Codex CLI isolation -----

/// Codex CLI isolation status (for the frontend to render).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CodexCliStatusDto {
    /// Whether the effective CODEX_HOME is isolated, including the default mirror.
    isolation_enabled: bool,
    /// The Codex config directory in effect (custom mirror or default ~/.codex-cli).
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
    /// Mirror routing policy: exclusive or coexist.
    mirror_mode: String,
    /// Codex Desktop profile mirroring is currently a Windows-only launcher.
    desktop_supported: bool,
}

/// Read Codex CLI isolation-related status.
pub(crate) fn codex_cli_status(db: &Database) -> Result<CodexCliStatusDto, String> {
    let codex_cli_home = db
        .get_setting(crate::database::dao::settings::KEY_CODEX_CLI_HOME)
        .map_err(String::from)?;
    let outbound_proxy = db
        .get_setting(crate::database::dao::settings::KEY_OUTBOUND_PROXY)
        .map_err(String::from)?;
    let config_dir = checked_mirror_directory(codex_cli_home.as_deref())?;
    for name in ["config.toml", "auth.json"] {
        crate::config::private_fs::validate_external_file_path(&config_dir.join(name))
            .map_err(String::from)?;
    }
    let initialized =
        config_dir.join("config.toml").exists() || config_dir.join("auth.json").exists();

    // The currently selected Codex provider (the is_current row for app_type='codex' in the DB).
    let current_provider_id = db.get_current_provider("codex").map_err(String::from)?;
    let current_provider_name = current_provider_id
        .as_deref()
        .map(|id| db.get_provider_by_id(id, "codex"))
        .transpose()
        .map_err(String::from)?
        .flatten()
        .map(|p| p.name);

    Ok(CodexCliStatusDto {
        isolation_enabled: true,
        config_dir: config_dir.to_string_lossy().into_owned(),
        initialized,
        codex_cli_home,
        outbound_proxy,
        codex_binary: find_codex_cli_binary().map(|p| p.to_string_lossy().into_owned()),
        current_provider_id,
        current_provider_name,
        mirror_mode: crate::services::provider::codex::codex_mirror_mode_from_db(db)
            .as_str()
            .to_owned(),
        desktop_supported: cfg!(windows),
    })
}

pub(crate) fn codex_mirror_set_mode(
    db: &Database,
    mode: String,
) -> Result<serde_json::Value, String> {
    // Only the two known values are accepted — a typo'd mode is a client
    // error, not a silent flip to exclusive.
    let normalized = mode.trim();
    if !matches!(normalized, "exclusive" | "coexist") {
        return Err("codex_mirror_mode_invalid".to_string());
    }
    let codex_cli_home = db
        .get_setting(crate::database::dao::settings::KEY_CODEX_CLI_HOME)
        .map_err(String::from)?;
    checked_mirror_directory(codex_cli_home.as_deref())?;
    db.set_setting(
        crate::database::dao::settings::KEY_CODEX_MIRROR_MODE,
        normalized,
    )
    .map_err(String::from)?;
    let result =
        crate::services::provider::codex::switch_codex_mirror(db, codex_cli_home.as_deref(), false)
            .map_err(|e| e.to_string())?;
    Ok(serde_json::json!({
        "mode": normalized,
        "written": result.written,
    }))
}

/// Launch the Codex CLI in an isolated terminal (injecting CODEX_HOME +
/// outbound proxy environment variables).
pub(crate) fn codex_open_cli_terminal(
    db: &Database,
    cwd: Option<String>,
) -> Result<serde_json::Value, String> {
    process::working_directory(cwd.as_deref())?;
    let codex_bin = find_codex_cli_binary().ok_or_else(|| "codex_cli_not_found".to_string())?;
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

    let config_dir = checked_mirror_directory(codex_cli_home.as_deref())?;
    crate::config::private_fs::prepare_private_dir(&config_dir).map_err(String::from)?;
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

    terminal::launch_terminal(std::path::Path::new(&codex_bin), &env, cwd.as_deref())?;

    Ok(serde_json::json!({
        "codexHome": codex_home,
        "codexBinary": codex_bin,
        "env": redacted_environment(&env),
    }))
}

/// Open the supported Windows desktop mirror using ShellExecute via cmd/start.
/// Values are inherited as environment variables, with no credential-bearing
/// launcher file. Native Codex configuration is never used as the mirror.
#[cfg(target_os = "windows")]
pub(crate) fn codex_open_desktop_window(db: &Database) -> Result<serde_json::Value, String> {
    let exe = find_codex_desktop_exe()?;

    let codex_cli_home = db
        .get_setting(crate::database::dao::settings::KEY_CODEX_CLI_HOME)
        .map_err(String::from)?;
    let config_dir = checked_mirror_directory(codex_cli_home.as_deref())?;
    crate::config::private_fs::prepare_private_dir(&config_dir).map_err(String::from)?;
    let codex_home = config_dir.to_string_lossy().to_string();
    let electron_dir = config_dir.join("electron");
    // Preserve the existing mirror profile/login; destructive reset is explicit.
    crate::config::private_fs::prepare_private_dir(&electron_dir).map_err(String::from)?;

    // Auto-write the mirror contract (config.toml → AAG proxy) so the
    // parasite routes through AAG's proxy. The mirror's OWN login (a spare
    // account signed in inside the mirror) is PRESERVED across launches —
    // `purge_auth = false`; AAG still never writes any credential. A
    // stale/broken login is reset explicitly via `codex_mirror_session_reset`
    // (the audit command detects that case and says so).
    crate::services::provider::codex::switch_codex_mirror(&db, codex_cli_home.as_deref(), false)
        .map_err(String::from)?;

    let native_dir = crate::config::get_codex_config_dir();
    mirror_seed::seed_mirror_from_native(&native_dir, &config_dir)?;
    mirror_seed::sync_mirror_sandbox_credentials(&native_dir, &config_dir)?;

    // Generate cc-switch-model-catalog.json from AAG's proxy providers +
    // routes + official models. This tells Codex Desktop which models are
    // actually available through AAG's gateway (instead of showing only the
    // GPT defaults from the inherited models_cache.json).
    write_codex_model_catalog(&db, &config_dir)?;

    let exe_str = exe.to_string_lossy().to_string();
    let system_root = std::env::var_os("SystemRoot")
        .ok_or_else(|| "launcher_system_root_unavailable".to_string())?;
    let mut command =
        process::background_command(std::path::PathBuf::from(system_root).join("System32/cmd.exe"));
    use std::os::windows::process::CommandExt;
    command
        .args(["/D", "/V:OFF", "/S", "/C"])
        .raw_arg("start \"\" \"%AAG_LAUNCH_PROGRAM%\" --user-data-dir=\"%AAG_LAUNCH_PROFILE%\"");
    command
        .env("CODEX_HOME", &config_dir)
        .env("AAG_LAUNCH_PROGRAM", &exe)
        .env("AAG_LAUNCH_PROFILE", &electron_dir);
    process::capture_stdout(&mut command, process::PROBE_TIMEOUT)?;

    Ok(serde_json::json!({
        "codexHome": codex_home,
        "codexBinary": exe_str,
        "mirror": true,
    }))
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn codex_open_desktop_window(_db: &Database) -> Result<serde_json::Value, String> {
    Err("codex_desktop_platform_unsupported".to_string())
}

/// Launch the Copilot CLI in an isolated terminal (injecting
/// COPILOT_PROVIDER_* + outbound proxy environment variables). The Copilot
/// CLI's custom provider can only be injected via environment variables at
/// launch (there is no persisted provider config), so AAG reads
/// baseUrl/apiKey/model from the currently selected Copilot provider's
/// settings_config and opens a terminal with `COPILOT_PROVIDER_BASE_URL`
/// and friends set.
pub(crate) fn copilot_open_cli_terminal(
    db: &Database,
    cwd: Option<String>,
) -> Result<serde_json::Value, String> {
    process::working_directory(cwd.as_deref())?;
    let outbound_proxy = db
        .get_setting(crate::database::dao::settings::KEY_OUTBOUND_PROXY)
        .map_err(String::from)?;

    // The currently selected Copilot provider (the is_current row for app_type='copilot' in the DB).
    let provider_id = db.get_current_provider("copilot").map_err(String::from)?;
    let provider = provider_id
        .as_deref()
        .map(|id| db.get_provider_by_id(id, "copilot"))
        .transpose().map_err(String::from)?.flatten()
        .ok_or_else(|| {
            "no Copilot provider configured: add and switch to a Copilot provider on the Providers page (with baseUrl/apiKey/model)".to_string()
        })?;
    crate::services::provider::switching::switch_provider(db, &provider.id, "copilot")
        .map_err(String::from)?;
    let cfg_obj = provider
        .settings_config
        .as_object()
        .ok_or_else(|| "copilot_provider_config_invalid".to_string())?;
    let base_url = cfg_obj
        .get("env")
        .and_then(|e| e.get("COPILOT_PROVIDER_BASE_URL"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| "copilot_provider_endpoint_missing".to_string())?;

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
    let model = cfg_obj.get("model").and_then(|v| v.as_str()).unwrap_or("");

    // Find the copilot CLI binary (prefer PATH; fall back to common install locations).
    let copilot_bin =
        find_copilot_cli_binary().ok_or_else(|| "copilot_cli_not_found".to_string())?;
    let copilot_bin = copilot_bin.to_string_lossy().to_string();

    let mut env: Vec<(String, String)> = vec![
        (
            "COPILOT_PROVIDER_BASE_URL".to_string(),
            base_url.to_string(),
        ),
        (
            "COPILOT_PROVIDER_TYPE".to_string(),
            provider_type.to_string(),
        ),
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

    terminal::launch_terminal(std::path::Path::new(&copilot_bin), &env, cwd.as_deref())?;

    Ok(serde_json::json!({
        "copilotBinary": copilot_bin,
        "providerId": provider.id,
        "env": redacted_environment(&env),
    }))
}

/// Rule-based hints for the mirror session audit. Pure so it is testable;
/// strings are stable `code: text` pairs the UI can key on.
fn mirror_audit_hints(
    auth_state: &str,
    mirror_running: bool,
    session_cache_present: bool,
    native_auth_age_hours: Option<f64>,
) -> Vec<String> {
    let mut hints = Vec::new();
    match auth_state {
        "credentials_present" => hints.push(
            "mirror_has_session: mirror auth.json holds a stored session (the spare account signed in inside the mirror). If it errors with a refresh failure, run codex_mirror_session_reset and sign in again — do NOT sign in with the native account there, same-account sessions kick each other.".into(),
        ),
        "empty" if mirror_running => hints.push(
            "mirror_logged_out_by_design: the mirror is a standalone logged-out instance. Sign in INSIDE the mirror window; if native and mirror share one OpenAI account they will kick each other's session.".into(),
        ),
        "empty" => hints.push(
            "mirror_logged_out: open the Codex Desktop mirror via AAG and sign in inside that window.".into(),
        ),
        _ => {}
    }
    if session_cache_present {
        hints.push(
            "session_cache_present: cache/ or .codex-global-state.json from a previous logged-in session exists. Run codex_mirror_session_reset to clear it if the mirror is stuck on a dead login.".into(),
        );
    }
    if let Some(age) = native_auth_age_hours {
        if age > 20.0 {
            hints.push(format!(
                "native_session_stale: native ~/.codex/auth.json has not refreshed for {age:.0}h. If the error appears in the NATIVE Codex window, its server-side session is revoked — sign in again in the NATIVE window. AAG never touches native files."
            ));
        }
    }
    hints
}

/// Explicitly reset the Codex mirror's login/session state (auth.json,
/// global-state cache, cache/sessions carriers). On-demand only — neither
/// switching nor opening the desktop resets a login the user created
/// inside the mirror. The native `~/.codex` is never touched.
pub(crate) fn codex_mirror_session_reset(db: &Database) -> Result<(), String> {
    let codex_cli_home = db
        .get_setting(crate::database::dao::settings::KEY_CODEX_CLI_HOME)
        .map_err(String::from)?;
    crate::services::provider::codex::reset_mirror_session_state_guarded(codex_cli_home.as_deref())
        .map_err(String::from)
}

/// Read-only session-state audit for the Codex mirror — codifies the
/// stale-session diagnosis ("which window is broken and why") so it can be
/// queried from the UI instead of manual file hunting. Credential CONTENTS
/// are never read: auth.json is classified by shape only, and the native
/// `~/.codex` is touched by metadata (mtime) alone.
#[derive(serde::Serialize)]
#[allow(non_camel_case_types)]
pub(crate) struct Codex_mirror_session_audit {
    pub mirror_dir: String,
    pub mirror_running: bool,
    pub mirror_process_count: u32,
    pub auth_json: String,
    pub global_state_present: bool,
    pub session_cache_present: bool,
    pub electron_profile_present: bool,
    pub native_auth_age_hours: Option<f64>,
    pub hints: Vec<String>,
}

fn mirror_chatgpt_process_count(mirror_dir: &std::path::Path) -> Result<u32, String> {
    #[cfg(windows)]
    {
        let mut command = process::background_command(discovery::powershell()?);
        command.args(["-NoProfile", "-NonInteractive", "-Command",
            "(Get-CimInstance Win32_Process -Filter \"Name='ChatGPT.exe'\" -ErrorAction Stop | Where-Object { $_.CommandLine -and $_.CommandLine.Contains($env:AAG_MIRROR_PROFILE) } | Measure-Object).Count"]);
        command.env("AAG_MIRROR_PROFILE", mirror_dir.join("electron"));
        let output = process::capture_stdout(&mut command, process::PROBE_TIMEOUT)?;
        return String::from_utf8(output)
            .map_err(|_| "codex_mirror_probe_invalid".to_string())?
            .trim()
            .parse::<u32>()
            .map_err(|_| "codex_mirror_probe_invalid".to_string());
    }
    #[cfg(not(windows))]
    {
        let _ = mirror_dir;
        Err("codex_desktop_platform_unsupported".into())
    }
}

pub(crate) fn codex_mirror_session_audit(
    db: &Database,
) -> Result<Codex_mirror_session_audit, String> {
    let codex_cli_home = db
        .get_setting(crate::database::dao::settings::KEY_CODEX_CLI_HOME)
        .map_err(String::from)?;
    let mirror_dir = crate::config::get_codex_mirror_dir(codex_cli_home.as_deref());
    let mirror_process_count = mirror_chatgpt_process_count(&mirror_dir)?;
    let auth_state = crate::services::provider::codex::classify_mirror_auth_json(&mirror_dir);
    let global_state_present = mirror_dir.join(".codex-global-state.json").exists();
    let session_cache_present =
        mirror_dir.join("cache").exists() || mirror_dir.join("sessions").exists();
    let electron_profile_present = mirror_dir.join("electron").exists();
    let native_auth_age_hours = std::fs::metadata(crate::config::get_codex_auth_path())
        .ok()
        .and_then(|meta| meta.modified().ok())
        .and_then(|modified| modified.elapsed().ok())
        .map(|elapsed| elapsed.as_secs_f64() / 3600.0);
    let hints = mirror_audit_hints(
        auth_state,
        mirror_process_count > 0,
        session_cache_present,
        native_auth_age_hours,
    );
    Ok(Codex_mirror_session_audit {
        mirror_dir: mirror_dir.to_string_lossy().into_owned(),
        mirror_running: mirror_process_count > 0,
        mirror_process_count,
        auth_json: auth_state.to_string(),
        global_state_present,
        session_cache_present,
        electron_profile_present,
        native_auth_age_hours,
        hints,
    })
}

fn redacted_environment(environment: &[(String, String)]) -> Vec<(String, String)> {
    environment
        .iter()
        .map(|(name, _)| (name.clone(), "[redacted]".into()))
        .collect()
}

fn checked_mirror_directory(setting: Option<&str>) -> Result<std::path::PathBuf, String> {
    let directory = crate::config::get_codex_mirror_dir(setting);
    if crate::services::provider::codex::same_codex_dir(
        &directory,
        &crate::config::get_codex_config_dir(),
    ) {
        return Err("codex_mirror_native_directory_forbidden".into());
    }
    if !directory.is_absolute()
        || directory
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
        || directory.to_string_lossy().chars().any(char::is_control)
    {
        return Err("codex_mirror_directory_invalid".into());
    }
    Ok(directory)
}
