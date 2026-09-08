//! Codex adapter.
//!
//! Codex stores config at `~/.codex/config.toml` (TOML) and `~/.codex/auth.json`
//! (JSON). The provider's `settings_config` is a JSON object with shape:
//! `{ "auth": <auth.json content>, "config": "<TOML string>" }`.
//! Mirrors cc-switch `services/provider/live.rs:1012`.
//!
//! When the `codex_cli_home` setting is configured (independent CODEX_HOME),
//! writes go to that directory instead of the default `~/.codex`, isolating
//! the Codex CLI from the Codex desktop app's shared config.

use crate::config::{
    backup_file, get_backups_dir, get_codex_auth_path, get_codex_config_path,
    read_json_file, read_text_file, write_json_file, write_text_file,
};
use crate::error::AppError;
use crate::provider::Provider;
use crate::services::provider::LiveWriteResult;
use serde_json::Value;

/// Is Codex installed? (config dir exists)
#[allow(dead_code)]
pub fn detect() -> bool {
    get_codex_config_path().exists() || get_codex_auth_path().exists()
}

/// Read the current live config as `{config: <toml string>, auth: <auth.json value>}`,
/// or None if neither file exists.
#[allow(dead_code)]
pub fn read_live() -> Result<Option<Value>, AppError> {
    let config_path = get_codex_config_path();
    let auth_path = get_codex_auth_path();
    let config = read_text_file(&config_path)?;
    let auth = read_json_file(&auth_path)?;
    if config.is_none() && auth.is_none() {
        return Ok(None);
    }
    Ok(Some(serde_json::json!({
        "config": config.unwrap_or_default(),
        "auth": auth.unwrap_or(Value::Null),
    })))
}

/// Write `provider.settings_config` to Codex's `config.toml` + `auth.json`,
/// backing up existing files first. `settings_config` must be an object with
/// `auth` (any JSON) and `config` (TOML string) fields.
///
/// `codex_cli_home` optionally overrides the Codex config directory (isolated
/// CODEX_HOME for the CLI); `None` uses the default `~/.codex`.
///
/// Re-enabled in Phase 16: called from `provider_switch` when
/// `app_type == "codex"`. The mirror-install contract requires the user's
/// native `~/.codex` (Codex Desktop) is never touched - callers must pass a
/// `codex_cli_home` that resolves outside `~/.codex` (the UI defaults to
/// `~/.codex-cli`).
pub fn write_live(
    provider: &Provider,
    codex_cli_home: Option<&str>,
) -> Result<LiveWriteResult, AppError> {
    let obj = provider.settings_config.as_object().ok_or_else(|| {
        AppError::Config("Codex provider settings_config must be a JSON object".into())
    })?;
    let auth = obj
        .get("auth")
        .ok_or_else(|| AppError::Config("Codex settings_config missing 'auth' field".into()))?;
    let config_str = obj
        .get("config")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::Config("Codex settings_config missing 'config' string field".into()))?;

    let config_path = crate::config::get_codex_mirror_dir(codex_cli_home).join("config.toml");
    let auth_path = crate::config::get_codex_mirror_dir(codex_cli_home).join("auth.json");
    let ts = chrono::Utc::now().timestamp();
    let backup_dir = get_backups_dir()?;

    let mut written = Vec::new();
    let mut backups = Vec::new();

    // Backup + write config.toml
    if config_path.exists() {
        let bak = backup_dir.join(format!("config.toml.{ts}.bak"));
        backup_file(&config_path, &bak)?;
        backups.push(bak.to_string_lossy().into_owned());
    }
    write_text_file(&config_path, config_str)?;
    written.push(config_path.to_string_lossy().into_owned());

    // Backup + write auth.json
    if auth_path.exists() {
        let bak = backup_dir.join(format!("auth.json.{ts}.bak"));
        backup_file(&auth_path, &bak)?;
        backups.push(bak.to_string_lossy().into_owned());
    }
    write_json_file(&auth_path, auth)?;
    written.push(auth_path.to_string_lossy().into_owned());

    Ok(LiveWriteResult {
        app_type: "codex".into(),
        provider_id: provider.id.clone(),
        written,
        backup: backups.into_iter().next(),
    })
}