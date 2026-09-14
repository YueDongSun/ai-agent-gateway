//! Application configuration: paths + file I/O helpers.
//!
//! AAG stores its SQLite database at `~/.aag/aag.db`. Live agent config files
//! are written atomically (temp file + rename), mirroring cc-switch's
//! `config.rs` pattern.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::AppError;
use serde::Serialize;
use serde_json::Value;

/// Return the application config directory (`~/.aag` on all platforms).
///
/// Override via `AAG_CONFIG_DIR` env var for testing / portable installs.
pub fn get_app_config_dir() -> PathBuf {
    if let Ok(custom) = std::env::var("AAG_CONFIG_DIR") {
        let trimmed = custom.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".aag")
}

/// Return the database file path (`~/.aag/aag.db`).
pub fn get_db_path() -> PathBuf {
    get_app_config_dir().join("aag.db")
}

/// Return the backups directory (`~/.aag/backups/`), creating it if absent.
#[allow(dead_code)] // live-config write path retained for tests/history; not called in the app since Phase 15.9
pub fn get_backups_dir() -> Result<PathBuf, AppError> {
    let dir = get_app_config_dir().join("backups");
    fs::create_dir_all(&dir).map_err(|e| AppError::io(&dir, e))?;
    Ok(dir)
}

/// Return the user's home directory for live agent config paths
/// (`~/.claude`, `~/.codex`, ...).
///
/// Override via `AAG_HOME_DIR` env var for testing / portable installs.
/// (The `dirs` crate's `home_dir()` reads USERPROFILE on Windows but can
/// fall back to SHGetKnownFolderPath which ignores USERPROFILE mutations
/// from inside a process — so we need our own override to make the
/// integration test reliable.)
pub fn get_home_dir() -> PathBuf {
    if let Ok(custom) = std::env::var("AAG_HOME_DIR") {
        let trimmed = custom.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

/// Return the Claude Code config directory (`~/.claude`).
pub fn get_claude_config_dir() -> PathBuf {
    get_home_dir().join(".claude")
}

/// Return the Claude Code settings file path. Prefers `settings.json`,
/// falls back to legacy `claude.json` if that exists, else `settings.json`
/// (for new installs). Mirrors cc-switch `config.rs:167`.
pub fn get_claude_settings_path() -> PathBuf {
    let dir = get_claude_config_dir();
    let settings = dir.join("settings.json");
    if settings.exists() {
        return settings;
    }
    let legacy = dir.join("claude.json");
    if legacy.exists() {
        return legacy;
    }
    settings
}

/// Return the Codex config directory (`~/.codex`).
///
/// When `codex_cli_home` is `Some`, the CLI config is isolated into that
/// directory instead (independent CODEX_HOME), leaving the Codex desktop
/// app's shared `~/.codex` untouched.
pub fn get_codex_config_dir_with_override(codex_cli_home: Option<&str>) -> PathBuf {
    match codex_cli_home.map(str::trim).filter(|s| !s.is_empty()) {
        Some(raw) => resolve_user_path(raw),
        None => get_home_dir().join(".codex"),
    }
}

/// The AAG mirror CODEX_HOME. Unlike `get_codex_config_dir_with_override`,
/// an unset override falls back to `~/.codex-cli` (NOT the native
/// `~/.codex`) so AAG never writes the Codex Desktop app's config.
/// Mirrors cc-switch's isolated-CLI pattern ("mirror install").
pub fn get_codex_mirror_dir(codex_cli_home: Option<&str>) -> PathBuf {
    match codex_cli_home.map(str::trim).filter(|s| !s.is_empty()) {
        Some(raw) => resolve_user_path(raw),
        None => get_home_dir().join(".codex-cli"),
    }
}

/// Return the Codex config directory (`~/.codex`).
pub fn get_codex_config_dir() -> PathBuf {
    get_codex_config_dir_with_override(None)
}

/// Expand `~` / `~/` / `~\` prefixes in a user-supplied path.
fn resolve_user_path(raw: &str) -> PathBuf {
    if raw == "~" {
        return get_home_dir();
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        return get_home_dir().join(rest);
    }
    if let Some(rest) = raw.strip_prefix("~\\") {
        return get_home_dir().join(rest);
    }
    PathBuf::from(raw)
}

/// Return the Codex `config.toml` path.
pub fn get_codex_config_path() -> PathBuf {
    get_codex_config_dir().join("config.toml")
}

/// Return the Codex `auth.json` path.
pub fn get_codex_auth_path() -> PathBuf {
    get_codex_config_dir().join("auth.json")
}

/// Return the Copilot CLI config directory (`~/.copilot`).
///
/// GitHub Copilot CLI stores its user-editable settings at
/// `~/.copilot/settings.json` (JSONC) and its managed state at
/// `~/.copilot/config.json`. Custom providers are injected at launch via
/// `COPILOT_PROVIDER_*` env vars (no persistent provider file exists), so
/// AAG persists the `model` choice + the env block in `settings.json`.
pub fn get_copilot_config_dir() -> PathBuf {
    get_home_dir().join(".copilot")
}

/// Return the Copilot CLI `settings.json` path.
pub fn get_copilot_settings_path() -> PathBuf {
    get_copilot_config_dir().join("settings.json")
}

/// Return the pi (Microsoft coding agent) config directory
/// (`~/.pi/agent`). Custom providers live in `models.json` and the default
/// model in `settings.json`.
pub fn get_pi_config_dir() -> PathBuf {
    get_home_dir().join(".pi").join("agent")
}

/// Return the pi `models.json` path (custom provider map).
#[allow(dead_code)] // live-config write path retained for tests/history; not called in the app since Phase 15.9
pub fn get_pi_models_path() -> PathBuf {
    get_pi_config_dir().join("models.json")
}

/// Return the pi `settings.json` path (default model).
#[allow(dead_code)] // live-config write path retained for tests/history; not called in the app since Phase 15.9
pub fn get_pi_settings_path() -> PathBuf {
    get_pi_config_dir().join("settings.json")
}

/// Return the Antigravity desktop app config directory.
///
/// Antigravity is a VS Code-derived Electron app. On Windows it stores its
/// profile under `%APPDATA%\Antigravity` (with `User/settings.json` once the
/// user opens the settings UI); macOS `~/Library/Application Support/Antigravity`;
/// Linux `~/.config/Antigravity`.
pub fn get_antigravity_config_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            return PathBuf::from(appdata).join("Antigravity");
        }
        get_home_dir().join("AppData").join("Roaming").join("Antigravity")
    }
    #[cfg(target_os = "macos")]
    {
        get_home_dir().join("Library").join("Application Support").join("Antigravity")
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        get_home_dir().join(".config").join("Antigravity")
    }
}

/// Return the Antigravity `User/settings.json` path (created by the app once
/// the settings UI is opened).
#[allow(dead_code)] // Planned for the Antigravity adapter's future write path
pub fn get_antigravity_settings_path() -> PathBuf {
    get_antigravity_config_dir().join("User").join("settings.json")
}

// ---- File I/O helpers (mirror cc-switch config.rs:274+) ----

/// Atomic write: write to `<path>.tmp.<n>` then rename. Prevents config
/// corruption if the process is interrupted mid-write.
#[allow(dead_code)] // live-config write path retained for tests/history; not called in the app since Phase 15.9
pub fn atomic_write(path: &Path, data: &[u8]) -> Result<(), AppError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| AppError::io(parent, e))?;
    }
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_extension(format!("tmp.{}.{}", std::process::id(), n));
    fs::write(&tmp, data).map_err(|e| AppError::io(&tmp, e))?;
    fs::rename(&tmp, path).map_err(|e| AppError::io(path, e))?;
    Ok(())
}

/// Serialize a value to pretty JSON and atomically write it.
#[allow(dead_code)] // live-config write path retained for tests/history; not called in the app since Phase 15.9
pub fn write_json_file<T: Serialize>(path: &Path, data: &T) -> Result<(), AppError> {
    let json = serde_json::to_string_pretty(data)?;
    atomic_write(path, json.as_bytes())
}

/// Read and parse a JSON file. Returns `Ok(None)` if the file doesn't exist.
#[allow(dead_code)]
pub fn read_json_file(path: &Path) -> Result<Option<Value>, AppError> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(path).map_err(|e| AppError::io(path, e))?;
    let value: Value = serde_json::from_str(&raw)?;
    Ok(Some(value))
}

/// Read a text file. Returns `Ok(None)` if the file doesn't exist.
#[allow(dead_code)]
pub fn read_text_file(path: &Path) -> Result<Option<String>, AppError> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(path).map_err(|e| AppError::io(path, e))?;
    Ok(Some(raw))
}

/// Atomically write a text file (for TOML / plain text).
#[allow(dead_code)] // live-config write path retained for tests/history; not called in the app since Phase 15.9
pub fn write_text_file(path: &Path, data: &str) -> Result<(), AppError> {
    atomic_write(path, data.as_bytes())
}

/// Copy a file to a backup path (best-effort, used before overwriting live config).
#[allow(dead_code)] // live-config write path retained for tests/history; not called in the app since Phase 15.9
pub fn backup_file(src: &Path, dst: &Path) -> Result<(), AppError> {
    if !src.exists() {
        return Ok(());
    }
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent).map_err(|e| AppError::io(parent, e))?;
    }
    fs::copy(src, dst).map_err(|e| AppError::io(src, e))?;
    Ok(())
}

#[cfg(test)]
static TEST_ENV_LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();

#[cfg(test)]
pub(crate) fn test_env_lock() -> &'static tokio::sync::Mutex<()> {
    TEST_ENV_LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}
