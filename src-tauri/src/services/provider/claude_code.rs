//! Claude Code adapter.
//!
//! Claude Code stores its config at `~/.claude/settings.json` (legacy:
//! `~/.claude/claude.json`). The provider's `settings_config` JSON blob IS
//! the settings.json content (after stripping internal-only fields).
//! Mirrors cc-switch `services/provider/live.rs:1000` + `sanitize_claude_settings_for_live`.

use crate::config::{
    backup_file, get_backups_dir, get_claude_settings_path, read_json_file, write_json_file,
};
use crate::error::AppError;
use crate::provider::Provider;
use crate::services::provider::LiveWriteResult;
use serde_json::Value;

/// Is Claude Code installed? (config dir exists)
#[allow(dead_code)]
pub fn detect() -> bool {
    crate::config::get_claude_settings_path().exists()
}

/// Read the current live settings.json as a JSON value, or None if absent.
#[allow(dead_code)]
pub fn read_live() -> Result<Option<Value>, AppError> {
    let path = get_claude_settings_path();
    read_json_file(&path)
}

/// Strip internal-only fields that must never be written to Claude Code's
/// settings.json. Mirrors cc-switch `sanitize_claude_settings_for_live` (`live.rs:166`).
#[allow(dead_code)] // live-config write path retained for tests/history; not called in the app since Phase 15.9
fn sanitize(settings: &Value) -> Value {
    let mut v = settings.clone();
    if let Some(obj) = v.as_object_mut() {
        obj.remove("api_format");
        obj.remove("apiFormat");
        obj.remove("openrouter_compat_mode");
        obj.remove("openrouterCompatMode");
    }
    v
}

/// Write `provider.settings_config` to `~/.claude/settings.json`, backing up
/// the existing file first. Returns the written + backup paths.
#[allow(dead_code)] // live-config write path retained for tests/history; not called in the app since Phase 15.9
pub fn write_live(provider: &Provider) -> Result<LiveWriteResult, AppError> {
    let path = get_claude_settings_path();
    let settings = sanitize(&provider.settings_config);

    // Backup the existing file (if any) before overwriting.
    let backup = if path.exists() {
        let ts = chrono::Utc::now().timestamp();
        let backup_name = format!("settings.json.{ts}.bak");
        let backup_path = get_backups_dir()?.join(backup_name);
        backup_file(&path, &backup_path)?;
        Some(backup_path.to_string_lossy().into_owned())
    } else {
        None
    };

    write_json_file(&path, &settings)?;

    Ok(LiveWriteResult {
        app_type: "claude_code".into(),
        provider_id: provider.id.clone(),
        written: vec![path.to_string_lossy().into_owned()],
        backup,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sanitize_strips_internal_fields() {
        let input = json!({
            "apiKey": "sk-x",
            "baseURL": "https://example.com/v1",
            "api_format": "openai",
            "openrouter_compat_mode": true,
            "apiFormat": "openai",
            "openrouterCompatMode": true
        });
        let out = sanitize(&input);
        let obj = out.as_object().unwrap();
        assert!(obj.get("api_format").is_none());
        assert!(obj.get("apiFormat").is_none());
        assert!(obj.get("openrouter_compat_mode").is_none());
        assert!(obj.get("openrouterCompatMode").is_none());
        assert_eq!(obj.get("apiKey").unwrap(), "sk-x");
    }
}
