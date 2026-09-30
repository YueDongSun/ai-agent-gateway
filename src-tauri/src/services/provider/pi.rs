//! pi (Microsoft coding agent) adapter.
//!
//! pi stores custom model providers in `~/.pi/agent/models.json` and the
//! default model in `~/.pi/agent/settings.json`.
//!
//! `models.json` shape (from the pi docs `packages/coding-agent/docs/models.md`):
//! ```json
//! {
//!   "providers": {
//!     "<id>": {
//!       "baseUrl": "https://api.example.com/v1",
//!       "api": "openai-completions" | "anthropic-messages",
//!       "apiKey": "sk-...",
//!       "models": [ { "id": "model-name" } ]
//!     }
//!   }
//! }
//! ```
//! `settings.json` shape: `{ "model": "<default-model-id>" }` (merged with any
//! other user settings).
//!
//! The AAG `settings_config` shape:
//! ```json
//! {
//!   "providerId": "my-provider",
//!   "baseUrl": "http://127.0.0.1:8989/v1",
//!   "apiKey": "...",
//!   "api": "openai-completions",
//!   "model": "claude-sonnet-4",
//!   "models": ["claude-sonnet-4", "gpt-4o"]
//! }
//! ```

use crate::config::read_json_file;
use crate::error::AppError;
use crate::provider::Provider;
use crate::services::provider::LiveWriteResult;
use serde_json::{json, Value};
use std::path::PathBuf;

/// Is pi installed? (config dir exists)
#[allow(dead_code)]
pub fn detect() -> bool {
    super::config_paths::resolve("pi")
        .map(|resolved| resolved.existing)
        .unwrap_or(false)
}

fn provider_api(settings: &Value) -> String {
    settings
        .get("api")
        .and_then(|v| v.as_str())
        .unwrap_or("openai-completions")
        .to_string()
}

/// Write `provider.settings_config` to pi's `models.json` + `settings.json`,
/// backing up existing files first.
#[allow(dead_code)] // live-config write path retained for tests/history; not called in the app since Phase 15.9
pub fn write_live(provider: &Provider) -> Result<LiveWriteResult, AppError> {
    let resolved = super::config_paths::resolve("pi")?;
    write_live_at(provider, &resolved.paths)
}

/// Apply the models/settings pair selected by client configuration discovery.
pub fn write_live_at(provider: &Provider, paths: &[PathBuf]) -> Result<LiveWriteResult, AppError> {
    let [models_path, settings_path] = paths else {
        return Err(AppError::Config("AAG_CLIENT_CONFIG_PATHS_INVALID".into()));
    };
    for path in paths {
        crate::config::private_fs::validate_external_file_path(path)?;
    }
    let obj = provider.settings_config.as_object().ok_or_else(|| {
        AppError::Config("pi provider settings_config must be a JSON object".into())
    })?;

    let provider_id = obj
        .get("providerId")
        .and_then(|v| v.as_str())
        .unwrap_or(&provider.id);
    let base_url = obj
        .get("baseUrl")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::Config("pi provider requires 'baseUrl'".into()))?;
    let api_key = obj.get("apiKey").and_then(|v| v.as_str()).unwrap_or("");
    let api = provider_api(&provider.settings_config);
    let model = obj.get("model").and_then(|v| v.as_str()).unwrap_or("");
    let models: Vec<serde_json::Value> = obj
        .get("models")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().map(|m| json!({ "id": m })).collect::<Vec<_>>())
        .unwrap_or_else(|| vec![json!({ "id": model })]);

    // ── models.json: upsert the provider into the `providers` map ──
    let mut models_doc = read_json_file(&models_path)?.unwrap_or_else(|| json!({}));
    let doc_obj = models_doc
        .as_object_mut()
        .ok_or_else(|| AppError::Config("~/.pi/agent/models.json must be a JSON object".into()))?;
    let providers_map = doc_obj
        .entry(String::from("providers"))
        .or_insert_with(|| json!({}));
    let map_obj = providers_map
        .as_object_mut()
        .ok_or_else(|| AppError::Config("models.json providers must be a JSON object".into()))?;
    map_obj.insert(
        provider_id.to_string(),
        json!({
            "baseUrl": base_url,
            "api": api,
            "apiKey": api_key,
            "models": models,
        }),
    );

    // Validate both destination documents before changing either file.
    let mut settings_doc = read_json_file(&settings_path)?.unwrap_or_else(|| json!({}));
    let settings_obj = settings_doc
        .as_object_mut()
        .ok_or_else(|| AppError::Config("pi settings.json must be a JSON object".into()))?;
    if !model.is_empty() {
        settings_obj.insert("model".into(), Value::String(model.to_string()));
    }
    let models_contents = serde_json::to_vec_pretty(&models_doc)?;
    let settings_contents = serde_json::to_vec_pretty(&settings_doc)?;
    let (written, backups) = super::config_writes::write_files(&[
        super::config_writes::ConfigFileUpdate {
            path: models_path,
            contents: &models_contents,
            app_type: "pi",
        },
        super::config_writes::ConfigFileUpdate {
            path: settings_path,
            contents: &settings_contents,
            app_type: "pi",
        },
    ])?;

    Ok(LiveWriteResult {
        app_type: "pi".into(),
        provider_id: provider.id.clone(),
        written,
        backup: backups.into_iter().next(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::get_pi_config_dir;
    use crate::provider::Provider;
    use crate::services::provider::test_support::isolated_home;
    use std::fs;

    #[test]
    fn provider_api_defaults_to_openai_completions() {
        assert_eq!(provider_api(&json!({})), "openai-completions");
        assert_eq!(
            provider_api(&json!({ "api": "anthropic-messages" })),
            "anthropic-messages"
        );
    }

    #[test]
    fn write_live_writes_models_and_settings() {
        let _home = isolated_home("pi");

        let p = Provider::new(
            "my-pi-prov".into(),
            "My Pi Provider".into(),
            json!({
                "providerId": "my-pi-prov",
                "baseUrl": "http://127.0.0.1:8989/v1",
                "apiKey": "sk-test",
                "api": "openai-completions",
                "model": "claude-sonnet-4",
                "models": ["claude-sonnet-4", "gpt-4o"]
            }),
        );
        let res = write_live(&p).expect("write_live");
        assert_eq!(res.written.len(), 2, "models.json + settings.json written");

        // Verify models.json shape.
        let models: Value = serde_json::from_str(
            &fs::read_to_string(get_pi_config_dir().join("models.json")).unwrap(),
        )
        .unwrap();
        let prov = &models["providers"]["my-pi-prov"];
        assert_eq!(prov["baseUrl"], "http://127.0.0.1:8989/v1");
        assert_eq!(prov["apiKey"], "sk-test");
        assert_eq!(prov["api"], "openai-completions");
        assert_eq!(prov["models"][0]["id"], "claude-sonnet-4");
        assert_eq!(prov["models"][1]["id"], "gpt-4o");

        // Verify settings.json default model.
        let settings: Value = serde_json::from_str(
            &fs::read_to_string(get_pi_config_dir().join("settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(settings["model"], "claude-sonnet-4");
    }
}
