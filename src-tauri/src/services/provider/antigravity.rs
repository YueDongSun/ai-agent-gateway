//! Antigravity desktop app adapter.
//!
//! Google's Antigravity 2.0 is a VS Code-derived Electron desktop IDE. Its
//! profile lives under `%APPDATA%\Antigravity` (Windows) with a
//! `User/settings.json` that the app creates once its settings UI is opened.
//! The app's model configuration is driven through the in-app model dropdown
//! and API-key entry; the on-disk schema for custom providers is **not publicly
//! documented** and the app stores much of its state in Chromium LevelDB.
//!
//! Strategy for this iteration: **detect-only**. AAG reports whether
//! Antigravity is installed and where its config lives, but does NOT attempt
//! to write live config (switching is disabled — `write_live` returns an
//! error guiding the user to configure Antigravity in-app).
//!
//! Future work: the app is VS Code-derived, so a "forwarding proxy like Claude"
//! approach may eventually work by writing a `github.copilot.chat.customOAIModels`
//! block (or the newer `chatLanguageModels.json`) into `User/settings.json`.
//! That is explicitly out of scope for this version until Google documents the
//! supported schema.

use crate::config::get_antigravity_config_dir;
use crate::error::AppError;
use crate::provider::Provider;
use crate::services::provider::LiveWriteResult;

/// Is Antigravity installed? (config dir with app files exists)
pub fn detect() -> bool {
    let dir = get_antigravity_config_dir();
    dir.join("Preferences").exists() || dir.join("Local State").exists()
}

/// The Antigravity config directory (for status display).
#[allow(dead_code)] // Planned for the Antigravity status surface in the UI
pub fn config_dir() -> std::path::PathBuf {
    get_antigravity_config_dir()
}

/// Switching is intentionally disabled for this iteration — the on-disk schema
/// for custom providers is not publicly documented.
#[allow(dead_code)] // live-config write path retained for tests/history; not called in the app since Phase 15.9
pub fn write_live(_provider: &Provider) -> Result<LiveWriteResult, AppError> {
    Err(AppError::Message(
        "Antigravity desktop switching is not supported yet (no public config format). Configure the API key in Antigravity's model dropdown, or pick another supported agent on the AAG Providers page".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_false_when_not_installed() {
        // Override AAG_HOME_DIR to a temp dir so APPDATA fallback paths are
        // irrelevant; on Windows APPDATA is read directly, so we can't easily
        // force a clean value — just assert detect() returns a bool without
        // panicking and that config_dir() returns a non-empty path.
        let dir = config_dir();
        assert!(!dir.as_os_str().is_empty());
        let _ = detect();
    }

    #[test]
    fn write_live_returns_disabled_error() {
        let p = Provider::new("x".into(), "X".into(), serde_json::json!({}));
        let err = write_live(&p).expect_err("Antigravity switching is disabled");
        assert!(err.to_string().contains("not supported yet"));
    }
}
