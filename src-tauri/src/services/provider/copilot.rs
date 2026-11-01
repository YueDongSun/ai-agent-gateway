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

use crate::error::AppError;
use crate::provider::Provider;
use crate::services::provider::LiveWriteResult;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Is Copilot CLI installed? (config dir exists)
#[allow(dead_code)]
pub fn detect() -> bool {
    super::config_paths::resolve("copilot")
        .map(|resolved| {
            resolved.existing
                || resolved
                    .paths
                    .first()
                    .and_then(|path| path.parent())
                    .is_some_and(|directory| directory.join("config.json").exists())
        })
        .unwrap_or(false)
}

/// Read the current live settings.json as a JSON value, or None if absent.
/// Strips `//` JSONC comments before parsing so a hand-edited Copilot
/// settings.json (which GitHub documents as JSONC) doesn't break the merge.
#[allow(dead_code)]
pub fn read_live() -> Result<Option<Value>, AppError> {
    let resolved = super::config_paths::resolve("copilot")?;
    let path = resolved
        .paths
        .first()
        .ok_or_else(|| AppError::Config("AAG_CLIENT_CONFIG_PATHS_INVALID".into()))?;
    read_live_at(path).map(|existing| existing.map(|(_, value)| value))
}

/// Read the current settings value together with the raw text (when the
/// file exists), so the JSONC patcher can preserve the user's comments and
/// formatting on rewrite.
fn read_live_at(path: &Path) -> Result<Option<(String, Value)>, AppError> {
    crate::config::private_fs::validate_external_file_path(path)?;
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(path).map_err(|e| AppError::io(path, e))?;
    let stripped = strip_jsonc_comments(&raw);
    match serde_json::from_str(&stripped) {
        Ok(v) => Ok(Some((raw, v))),
        Err(e) => Err(AppError::Json(e)),
    }
}

/// Strip JSONC comments before parsing (delegates to the shared stripper in
/// [`super::config_editor`], which also handles escaped quotes inside string
/// literals).
fn strip_jsonc_comments(raw: &str) -> String {
    super::config_editor::strip_jsonc_comments(raw)
}

/// Apply the `model` and `env` edits to the raw document, then verify the
/// patched text against `merged`.
fn patch_jsonc(
    raw: &str,
    settings: &serde_json::Map<String, Value>,
    merged: &Value,
) -> Result<String, AppError> {
    let mut editor = super::jsonc_edit::JsoncEditor::new(raw)?;
    if let Some(model) = settings.get("model") {
        editor.set_top_level("model", model)?;
    }
    if let Some(env) = settings.get("env") {
        let env = env
            .as_object()
            .ok_or_else(|| AppError::Config("Copilot provider env must be a JSON object".into()))?;
        editor.merge_object("env", env)?;
    }
    editor.finish(merged)
}

/// Write `provider.settings_config` to `~/.copilot/settings.json`, merging with
/// any existing user settings and backing up the original first.
#[allow(dead_code)] // live-config write path retained for tests/history; not called in the app since Phase 15.9
pub fn write_live(provider: &Provider) -> Result<LiveWriteResult, AppError> {
    let resolved = super::config_paths::resolve("copilot")?;
    write_live_at(provider, &resolved.paths)
}

/// Apply to the same settings path displayed by client configuration discovery.
pub fn write_live_at(provider: &Provider, paths: &[PathBuf]) -> Result<LiveWriteResult, AppError> {
    let [path] = paths else {
        return Err(AppError::Config("AAG_CLIENT_CONFIG_PATHS_INVALID".into()));
    };
    crate::config::private_fs::validate_external_file_path(path)?;
    let obj = provider.settings_config.as_object().ok_or_else(|| {
        AppError::Config("Copilot provider settings_config must be a JSON object".into())
    })?;

    // Start from the existing settings (if any) so we don't clobber user keys.
    let existing = read_live_at(path)?;
    let mut merged = existing
        .as_ref()
        .map(|(_, value)| value.clone())
        .unwrap_or_else(|| json!({}));
    let merged_obj = merged
        .as_object_mut()
        .ok_or_else(|| AppError::Config("~/.copilot/settings.json must be a JSON object".into()))?;

    // model
    if let Some(model) = obj.get("model") {
        merged_obj.insert(String::from("model"), model.clone());
    }
    // env block (merge, don't replace)
    if let Some(env) = obj.get("env") {
        let env = env
            .as_object()
            .ok_or_else(|| AppError::Config("Copilot provider env must be a JSON object".into()))?;
        let merged_env = merged_obj
            .entry(String::from("env"))
            .or_insert_with(|| json!({}));
        let env_obj = merged_env.as_object_mut().ok_or_else(|| {
            AppError::Config("Copilot settings.json env must be a JSON object".into())
        })?;
        for (k, v) in env {
            env_obj.insert(k.clone(), v.clone());
        }
    }

    let serialized = match existing.as_ref() {
        Some((raw, _)) => match patch_jsonc(raw, obj, &merged) {
            Ok(text) => text.into_bytes(),
            // The patcher refuses documents it cannot splice safely; fall
            // back to the full rewrite exactly as before.
            Err(_) => serde_json::to_vec_pretty(&merged)?,
        },
        None => serde_json::to_vec_pretty(&merged)?,
    };
    let (written, backups) =
        super::config_writes::write_files(&[super::config_writes::ConfigFileUpdate {
            path,
            contents: &serialized,
            app_type: "copilot",
        }])?;

    Ok(LiveWriteResult {
        app_type: "copilot".into(),
        provider_id: provider.id.clone(),
        written,
        backup: backups.into_iter().next(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::provider::test_support::isolated_home;

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

    #[test]
    fn strip_jsonc_treats_escaped_quotes_as_string_contents() {
        // An escaped quote must not end the string: after `"say \"` the
        // scanner is still inside the string, so the `//` in the URL value
        // and the following property survive.
        let raw = r#"{
            // user setting
            "note": "say \" hi",
            "url": "https://example.com//relay"
        }"#;
        let stripped = strip_jsonc_comments(raw);
        let v: Value = serde_json::from_str(&stripped).expect("valid JSON after strip");
        assert_eq!(v["note"], "say \" hi");
        assert_eq!(v["url"], "https://example.com//relay");
    }

    #[test]
    fn strip_jsonc_survives_backslash_run_before_quote() {
        // A literal backslash before the closing quote is itself escaped.
        let raw = r#"{ "path": "C:\\dir\\", "url": "https://x//y" }"#;
        let stripped = strip_jsonc_comments(raw);
        let v: Value = serde_json::from_str(&stripped).expect("valid JSON after strip");
        assert_eq!(v["path"], "C:\\dir\\");
        assert_eq!(v["url"], "https://x//y");
    }

    #[test]
    fn write_live_preserves_jsonc_comments_and_merges_env() {
        let home = isolated_home("copilot-jsonc");
        let path = home.root.join(".copilot").join("settings.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            concat!(
                "{\n",
                "  // keep this comment\n",
                "  \"theme\": \"auto\", /* inline */\n",
                "  \"env\": {\n",
                "    \"COPILOT_PROVIDER_BASE_URL\": \"https://old.invalid\" // old endpoint\n",
                "  }\n",
                "}\n",
            ),
        )
        .unwrap();

        let provider = Provider::new(
            "copilot-aag".into(),
            "Copilot".into(),
            json!({
                "model": "claude-sonnet-4",
                "env": {
                    "COPILOT_PROVIDER_BASE_URL": "https://example.invalid/v1",
                    "COPILOT_PROVIDER_API_KEY": "sk-secret"
                }
            }),
        );
        write_live(&provider).expect("Copilot write");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            concat!(
                "{\n",
                "  // keep this comment\n",
                "  \"theme\": \"auto\", /* inline */\n",
                "  \"env\": {\n",
                "    \"COPILOT_PROVIDER_API_KEY\": \"sk-secret\",\n",
                "    \"COPILOT_PROVIDER_BASE_URL\": \"https://example.invalid/v1\" // old endpoint\n",
                "  },\n",
                "  \"model\": \"claude-sonnet-4\"\n",
                "}\n",
            )
        );
        drop(home);
    }
}
