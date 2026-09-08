//! Copilot CLI adapter.
//!
//! GitHub Copilot CLI stores user-editable settings at `~/.copilot/settings.json`
//! (JSONC — JSON with optional `//` comments). The CLI has **no persistent
//! provider configuration**: custom providers are injected only via
//! `COPILOT_PROVIDER_BASE_URL` / `COPILOT_PROVIDER_API_KEY` /
//! `COPILOT_PROVIDER_TYPE` / `COPILOT_MODEL` env vars set at launch.
//!
//! So AAG's Copilot strategy is:
//!   1. Persist the `model` choice (and optionally the env block for the
//!      provider) into `~/.copilot/settings.json`, merging with any existing
//!      user settings (never clobber unrelated keys).
//!   2. Provide a "open Copilot CLI terminal with COPILOT_PROVIDER_* injected"
//!      launcher (see `copilot_open_cli_terminal` in lib.rs) — the same
//!      pattern as the Codex CLI isolated-terminal feature.
//!
//! The `settings_config` shape is:
//! ```json
//! {
//!   "model": "claude-sonnet-4",
//!   "env": {
//!     "COPILOT_PROVIDER_BASE_URL": "http://127.0.0.1:8989/v1",
//!     "COPILOT_PROVIDER_API_KEY": "...",
//!     "COPILOT_PROVIDER_TYPE": "openai"
//!   }
//! }
//! ```

use crate::config::{
    backup_file, get_backups_dir, get_copilot_settings_path, write_json_file,
};
use crate::error::AppError;
use crate::provider::Provider;
use crate::services::provider::LiveWriteResult;
use serde_json::{json, Value};

/// Is Copilot CLI installed? (config dir exists)
#[allow(dead_code)]
pub fn detect() -> bool {
    let dir = crate::config::get_copilot_config_dir();
    dir.join("settings.json").exists() || dir.join("config.json").exists()
}

/// Read the current live settings.json as a JSON value, or None if absent.
/// Strips `//` JSONC comments before parsing so a hand-edited Copilot
/// settings.json (which GitHub documents as JSONC) doesn't break the merge.
#[allow(dead_code)]
pub fn read_live() -> Result<Option<Value>, AppError> {
    let path = get_copilot_settings_path();
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path).map_err(|e| AppError::io(&path, e))?;
    let stripped = strip_jsonc_comments(&raw);
    match serde_json::from_str(&stripped) {
        Ok(v) => Ok(Some(v)),
        Err(e) => Err(AppError::Json(e)),
    }
}

/// Naively strip `//` line comments and `/* */` block comments from a JSONC
/// string. Good enough for Copilot settings.json (comments appear on their own
/// lines or after a value, never inside a string literal in practice).
fn strip_jsonc_comments(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '/' if chars.peek() == Some(&'/') => {
                // line comment -> skip to end of line
                chars.next();
                for ch in chars.by_ref() {
                    if ch == '\n' {
                        break;
                    }
                }
                out.push('\n');
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                while let Some(ch) = chars.next() {
                    if ch == '*' && chars.peek() == Some(&'/') {
                        chars.next();
                        break;
                    }
                }
            }
            _ => out.push(c),
        }
    }
    out
}

/// Write `provider.settings_config` to `~/.copilot/settings.json`, merging with
/// any existing user settings and backing up the original first.
#[allow(dead_code)] // live-config write path retained for tests/history; not called in the app since Phase 15.9
pub fn write_live(provider: &Provider) -> Result<LiveWriteResult, AppError> {
    let path = get_copilot_settings_path();
    let obj = provider.settings_config.as_object().ok_or_else(|| {
        AppError::Config("Copilot provider settings_config must be a JSON object".into())
    })?;

    // Start from the existing settings (if any) so we don't clobber user keys.
    let mut merged = read_live()?.unwrap_or_else(|| json!({}));
    let merged_obj = merged.as_object_mut().ok_or_else(|| {
        AppError::Config("~/.copilot/settings.json must be a JSON object".into())
    })?;

    // model
    if let Some(model) = obj.get("model") {
        merged_obj.insert(String::from("model"), model.clone());
    }
    // env block (merge, don't replace)
    if let Some(env) = obj.get("env").and_then(|e| e.as_object()) {
        let merged_env = merged_obj
            .entry(String::from("env"))
            .or_insert_with(|| json!({}));
        if let Some(env_obj) = merged_env.as_object_mut() {
            for (k, v) in env {
                env_obj.insert(k.clone(), v.clone());
            }
        }
    }

    // Backup the existing file (if any) before overwriting.
    let backup = if path.exists() {
        let ts = chrono::Utc::now().timestamp();
        let backup_name = format!("copilot.settings.json.{ts}.bak");
        let backup_path = get_backups_dir()?.join(backup_name);
        backup_file(&path, &backup_path)?;
        Some(backup_path.to_string_lossy().into_owned())
    } else {
        None
    };

    write_json_file(&path, &merged)?;

    Ok(LiveWriteResult {
        app_type: "copilot".into(),
        provider_id: provider.id.clone(),
        written: vec![path.to_string_lossy().into_owned()],
        backup,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_jsonc_removes_line_and_block_comments() {
        let raw = r#"{
            // user setting
            "model": "claude-sonnet-4", /* inline */
            "theme": "auto"
        }"#;
        let stripped = strip_jsonc_comments(raw);
        let v: Value = serde_json::from_str(&stripped).expect("valid JSON after strip");
        assert_eq!(v["model"], "claude-sonnet-4");
        assert_eq!(v["theme"], "auto");
    }

    #[test]
    fn strip_jsonc_keeps_string_contents() {
        // A URL with // inside a string must not be stripped.
        let raw = r#"{ "url": "https://example.com/v1" }"#;
        let stripped = strip_jsonc_comments(raw);
        let v: Value = serde_json::from_str(&stripped).expect("valid JSON");
        assert_eq!(v["url"], "https://example.com/v1");
    }
}
