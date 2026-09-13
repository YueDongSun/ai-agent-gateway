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

use crate::config::{
    backup_file, get_backups_dir, get_pi_models_path, get_pi_settings_path, read_json_file,
    write_json_file,
};
use crate::error::AppError;
use crate::provider::Provider;
use crate::services::provider::LiveWriteResult;
use serde_json::{json, Value};

/// Is pi installed? (config dir exists)
#[allow(dead_code)]
pub fn detect() -> bool {
    let dir = crate::config::get_pi_config_dir();
    dir.join("models.json").exists() || dir.join("settings.json").exists()
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
        .map(|arr| {
            arr.iter()
                .map(|m| json!({ "id": m }))
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|| vec![json!({ "id": model })]);

    let ts = chrono::Utc::now().timestamp();
    let backup_dir = get_backups_dir()?;
    let mut written = Vec::new();
    let mut backups = Vec::new();

    // ── models.json: upsert the provider into the `providers` map ──
    let models_path = get_pi_models_path();
    let mut models_doc = read_json_file(&models_path)?.unwrap_or_else(|| json!({}));
    let doc_obj = models_doc
        .as_object_mut()
        .ok_or_else(|| AppError::Config("~/.pi/agent/models.json must be a JSON object".into()))?;
    let providers_map = doc_obj
        .entry(String::from("providers"))
        .or_insert_with(|| json!({}));
    let map_obj = providers_map.as_object_mut().ok_or_else(|| {
        AppError::Config("models.json providers must be a JSON object".into())
    })?;
    map_obj.insert(
        provider_id.to_string(),
        json!({
            "baseUrl": base_url,
            "api": api,
            "apiKey": api_key,
            "models": models,
        }),
    );
    if models_path.exists() {
        let bak = backup_dir.join(format!("pi.models.json.{ts}.bak"));
        backup_file(&models_path, &bak)?;
        backups.push(bak.to_string_lossy().into_owned());
    }
    write_json_file(&models_path, &models_doc)?;
    written.push(models_path.to_string_lossy().into_owned());

    // ── settings.json: merge the default model ──
    let settings_path = get_pi_settings_path();
    let mut settings_doc = read_json_file(&settings_path)?.unwrap_or_else(|| json!({}));
    if let Some(s_obj) = settings_doc.as_object_mut() {
        if !model.is_empty() {
            s_obj.insert("model".into(), Value::String(model.to_string()));
        }
    }
    if settings_path.exists() {
        let bak = backup_dir.join(format!("pi.settings.json.{ts}.bak"));
        backup_file(&settings_path, &bak)?;
        backups.push(bak.to_string_lossy().into_owned());
    }
    write_json_file(&settings_path, &settings_doc)?;
    written.push(settings_path.to_string_lossy().into_owned());

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
        // Use AAG_HOME_DIR override so we don't touch the real ~/.pi.
        let tmp = std::env::temp_dir().join(format!("aag-pi-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        std::env::set_var("AAG_HOME_DIR", &tmp);

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

        let _ = fs::remove_dir_all(&tmp);
    }
}
