//! Application configuration: paths + file I/O helpers.
//!
//! AAG stores its SQLite database at `~/.aag/aag.db`. Live agent config files
//! are written atomically (temp file + rename), mirroring cc-switch's
//! `config.rs` pattern.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::AppError;
use serde::Serialize;
use serde_json::Value;

mod platform_paths;
pub(crate) mod private_fs;

/// Return the application config directory (`~/.aag` on all platforms).
///
/// Override via an absolute, dedicated `AAG_CONFIG_DIR` for testing or
/// portable installs. The private filesystem boundary rejects relative,
/// current-directory, parent-traversal, and filesystem-root targets before
/// changing permissions. An empty override uses the default.
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

pub(crate) fn app_config_dir_is_managed_default() -> bool {
    std::env::var("AAG_CONFIG_DIR")
        .ok()
        .map(|value| value.trim().is_empty())
        .unwrap_or(true)
}

/// Return the database file path (`~/.aag/aag.db`).
pub fn get_db_path() -> PathBuf {
    get_app_config_dir().join("aag.db")
}

/// Return the backups directory (`~/.aag/backups/`), creating it if absent.
#[allow(dead_code)] // live-config write path retained for tests/history; not called in the app since Phase 15.9
pub fn get_backups_dir() -> Result<PathBuf, AppError> {
    let app_dir = get_app_config_dir();
    if app_config_dir_is_managed_default() {
        private_fs::prepare_managed_private_dir(&app_dir)?;
    } else {
        private_fs::prepare_private_dir(&app_dir)?;
    }
    let dir = app_dir.join("backups");
    private_fs::prepare_managed_private_dir(&dir)?;
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

/// The AAG Claude mirror directory (CLAUDE_CONFIG_DIR). The native
/// `~/.claude` belongs to the live Claude Code install (and cc-switch),
/// so an unset override falls back to `~/.claude-cli` — the mirror is a
/// separate, AAG-created home. Same pattern as [`get_codex_mirror_dir`].
pub fn get_claude_mirror_dir(claude_cli_home: Option<&str>) -> PathBuf {
    match claude_cli_home.map(str::trim).filter(|s| !s.is_empty()) {
        Some(raw) => resolve_user_path(raw),
        None => get_home_dir().join(".claude-cli"),
    }
}

/// Return the Claude mirror `settings.json` path.
pub fn get_claude_mirror_settings_path(claude_cli_home: Option<&str>) -> PathBuf {
    get_claude_mirror_dir(claude_cli_home).join("settings.json")
}

/// Compare two directory paths as the same directory, tolerating spelling
/// differences that plain `PathBuf ==` would miss (trailing separators,
/// Windows case, `\\?\` verbatim prefixes from canonicalize). Used by the
/// native-home guards: a mirror override that resolves to a native agent
/// home (e.g. `~/.codex`, `~/.claude`) must be refused before any write.
pub(crate) fn same_dir(a: &Path, b: &Path) -> bool {
    fn normalize(path: &Path) -> String {
        let normalized: PathBuf = path.components().collect();
        let text = normalized
            .to_string_lossy()
            .trim_end_matches(['/', '\\'])
            .to_string();
        #[cfg(windows)]
        {
            text.strip_prefix("\\\\?\\").unwrap_or(&text).to_lowercase()
        }
        #[cfg(not(windows))]
        {
            text
        }
    }
    if normalize(a) == normalize(b) {
        return true;
    }
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(canonical_a), Ok(canonical_b)) => normalize(&canonical_a) == normalize(&canonical_b),
        _ => false,
    }
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

/// Return the Gemini CLI config directory (`~/.gemini`).
///
/// The CLI reads `settings.json` (JSON) and `.env` (KEY=VALUE lines) from
/// here; `GOOGLE_GEMINI_BASE_URL` in `.env` redirects its Generative
/// Language API traffic to a custom endpoint (AAG's proxy).
pub fn get_gemini_config_dir() -> PathBuf {
    get_home_dir().join(".gemini")
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

/// Return the OpenCode global configuration directory (`~/.config/opencode`).
pub fn get_opencode_config_dir() -> PathBuf {
    get_home_dir().join(".config").join("opencode")
}

/// Return the OpenCode global configuration file. Prefer an existing JSONC or
/// JSON file so switching does not silently create a second config source.
pub fn get_opencode_config_path() -> PathBuf {
    let dir = get_opencode_config_dir();
    let jsonc = dir.join("opencode.jsonc");
    if jsonc.exists() {
        return jsonc;
    }
    dir.join("opencode.json")
}

/// Return the Kilo CLI global configuration directory (highest-priority
/// candidate). The Kilo adapter in [`crate::services::provider::kilo`]
/// resolves all five candidate paths itself (mirroring the Node
/// `KiloAdapter` at `src/agents/adapters.ts:48-60`), so this getter exists
/// for callers that want a single reference path (e.g. the
/// `agent.detect_home` migration helper).
#[allow(dead_code)]
pub fn get_kilo_config_dir() -> PathBuf {
    get_home_dir().join(".config").join("kilo")
}

/// Return the highest-priority Kilo CLI configuration path. The Kilo
/// adapter resolves all candidates dynamically; this is the
/// "create-if-absent" target.
#[allow(dead_code)]
pub fn get_kilo_config_path() -> PathBuf {
    get_kilo_config_dir().join("kilo.jsonc")
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
/// Linux `$XDG_CONFIG_HOME/Antigravity` (falling back to `~/.config/Antigravity`).
pub fn get_antigravity_config_dir() -> PathBuf {
    platform_paths::desktop_config_root().join("Antigravity")
}

/// Return the Antigravity `User/settings.json` path (created by the app once
/// the settings UI is opened).
#[allow(dead_code)] // Planned for the Antigravity adapter's future write path
pub fn get_antigravity_settings_path() -> PathBuf {
    get_antigravity_config_dir()
        .join("User")
        .join("settings.json")
}

/// Return the VS Code global `User/settings.json` path. This is the file
/// that VS Code itself and its installed extensions (Kilo Code, Claude Code,
/// etc.) read for user-level settings. AAG merges a single provider entry
/// into the extension-namespaced section of this file without disturbing
/// unrelated keys (themes, keybindings, …).
///
/// - Windows: `%APPDATA%\Code\User\settings.json` (falls back to
///   `%USERPROFILE%\AppData\Roaming\Code\User\settings.json`).
/// - macOS: `~/Library/Application Support/Code/User/settings.json`.
/// - Linux/other: `$XDG_CONFIG_HOME/Code/User/settings.json`, falling back
///   to `~/.config/Code/User/settings.json`.
///
/// `AAG_HOME_DIR` overrides the home dir for testing/portable installs
/// and takes priority over native APPDATA/XDG_CONFIG_HOME overrides.
#[allow(dead_code)]
pub fn get_vscode_user_settings_path() -> PathBuf {
    platform_paths::desktop_config_root().join("Code/User/settings.json")
}

// ---- File I/O helpers (mirror cc-switch config.rs:274+) ----

/// Atomically write credential-bearing state through an exclusive private
/// temporary file in the same directory. Existing tighter owner permissions
/// are retained; linked/reparse paths and unsafe ownership fail closed.
#[allow(dead_code)] // live-config write path retained for tests/history; not called in the app since Phase 15.9
pub fn atomic_write(path: &Path, data: &[u8]) -> Result<(), AppError> {
    private_fs::atomic_write(path, data)
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

/// Copy an exact file to a private atomic backup path before overwrite.
///
/// Missing sources remain a no-op for compatibility. Existing linked/reparse
/// sources fail closed and unsafe source grants never propagate to the backup.
#[allow(dead_code)] // live-config write path retained for tests/history; not called in the app since Phase 15.9
pub fn backup_file(src: &Path, dst: &Path) -> Result<(), AppError> {
    match fs::symlink_metadata(src) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(AppError::io(src, error)),
        Ok(_) => {}
    }
    private_fs::backup_file(src, dst)
}

#[cfg(test)]
static TEST_ENV_LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();

#[cfg(test)]
pub(crate) fn test_env_lock() -> &'static tokio::sync::Mutex<()> {
    TEST_ENV_LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}
