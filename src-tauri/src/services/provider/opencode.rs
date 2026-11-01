//! OpenCode provider adapter.
//!
//! OpenCode and Kilo use the same provider projection: a top-level `provider`
//! map whose entries contain an AI SDK package, options, and model map. The
//! AAG provider row stores one entry; this adapter merges it into the global
//! OpenCode config and updates the selected model without replacing unrelated
//! user settings. Existing JSONC documents are patched in place so user
//! comments, key order and formatting survive a switch.

#[cfg(test)]
use crate::config::get_opencode_config_path;
use crate::error::AppError;
use crate::provider::Provider;
use crate::services::provider::LiveWriteResult;
use serde_json::{json, Value};
use std::path::PathBuf;

const DEFAULT_NPM: &str = "@ai-sdk/openai-compatible";

/// Is a global OpenCode config present?
#[allow(dead_code)]
pub fn detect() -> bool {
    super::config_paths::resolve("opencode")
        .map(|resolved| resolved.existing)
        .unwrap_or(false)
}

/// Strip JSONC comments before parsing (delegates to the shared stripper in
/// [`super::config_editor`]).
fn strip_jsonc_comments(raw: &str) -> String {
    super::config_editor::strip_jsonc_comments(raw)
}

/// Read the current config value together with its raw text (when the file
/// exists), so the JSONC patcher can preserve the user's comments and
/// formatting on rewrite.
fn read_config(path: &std::path::Path) -> Result<(Value, Option<String>), AppError> {
    if !path.exists() {
        return Ok((json!({}), None));
    }
    let raw = std::fs::read_to_string(path).map_err(|e| AppError::io(path, e))?;
    let value: Value = serde_json::from_str(&strip_jsonc_comments(&raw)).map_err(AppError::Json)?;
    if !value.is_object() {
        return Err(AppError::Config(format!(
            "OpenCode config {} must have an object root",
            path.display()
        )));
    }
    Ok((value, Some(raw)))
}

fn model_from_provider(id: &str, entry: &serde_json::Map<String, Value>) -> Option<String> {
    entry
        .get("model")
        .and_then(Value::as_str)
        .filter(|model| !model.is_empty())
        .or_else(|| {
            entry
                .get("models")
                .and_then(Value::as_object)
                .and_then(|models| models.keys().next().map(String::as_str))
        })
        .map(|model| format!("{id}/{model}"))
}

/// Apply the provider-entry and `model` edits to the raw document, then
/// verify the patched text against `config`.
fn patch_jsonc(
    raw: &str,
    provider_id: &str,
    entry: &Value,
    model: Option<&str>,
    config: &Value,
) -> Result<String, AppError> {
    let mut editor = super::jsonc_edit::JsoncEditor::new(raw)?;
    editor.set_provider_entry(provider_id, entry)?;
    if let Some(model) = model {
        editor.set_top_level("model", &Value::String(model.to_string()))?;
    }
    editor.finish(config)
}

/// Write one AAG provider into the global OpenCode config.
pub fn write_live(provider: &Provider) -> Result<LiveWriteResult, AppError> {
    let resolved = super::config_paths::resolve("opencode")?;
    write_live_at(provider, &resolved.paths)
}

/// Apply to the same JSON/JSONC path displayed by configuration discovery.
pub fn write_live_at(provider: &Provider, paths: &[PathBuf]) -> Result<LiveWriteResult, AppError> {
    let [path] = paths else {
        return Err(AppError::Config("AAG_CLIENT_CONFIG_PATHS_INVALID".into()));
    };
    crate::config::private_fs::validate_external_file_path(path)?;
    let entry = provider.settings_config.as_object().ok_or_else(|| {
        AppError::Config("OpenCode provider settings_config must be a JSON object".into())
    })?;
    let options = entry
        .get("options")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            AppError::Config("OpenCode provider requires an object 'options' field".into())
        })?;
    let _base_url = options
        .get("baseURL")
        .and_then(Value::as_str)
        .filter(|url| !url.trim().is_empty())
        .ok_or_else(|| AppError::Config("OpenCode provider requires options.baseURL".into()))?;

    let (mut config, existing_text) = read_config(path)?;
    let root = config
        .as_object_mut()
        .expect("read_config validates object root");
    let providers = match root.get_mut("provider") {
        None => {
            root.insert("provider".into(), json!({}));
            root.get_mut("provider")
                .and_then(Value::as_object_mut)
                .unwrap()
        }
        Some(value) => value.as_object_mut().ok_or_else(|| {
            AppError::Config("OpenCode config 'provider' must be an object".into())
        })?,
    };

    let mut entry_value = Value::Object(entry.clone());
    let entry_obj = entry_value.as_object_mut().expect("entry is an object");
    entry_obj
        .entry("npm")
        .or_insert_with(|| Value::String(DEFAULT_NPM.into()));
    entry_obj
        .entry("name")
        .or_insert_with(|| Value::String(provider.name.clone()));
    let selected_model = model_from_provider(&provider.id, entry);
    providers.insert(provider.id.clone(), entry_value.clone());

    if let Some(selected_model) = &selected_model {
        root.insert("model".into(), Value::String(selected_model.clone()));
    }

    let serialized = match existing_text.as_deref() {
        Some(raw) => match patch_jsonc(
            raw,
            &provider.id,
            &entry_value,
            selected_model.as_deref(),
            &config,
        ) {
            Ok(text) => text.into_bytes(),
            // The patcher refuses documents it cannot splice safely; fall
            // back to the full rewrite exactly as before.
            Err(_) => serde_json::to_vec_pretty(&config)?,
        },
        None => serde_json::to_vec_pretty(&config)?,
    };
    let (written, backups) =
        super::config_writes::write_files(&[super::config_writes::ConfigFileUpdate {
            path,
            contents: &serialized,
            app_type: "opencode",
        }])?;

    Ok(LiveWriteResult {
        app_type: "opencode".into(),
        provider_id: provider.id.clone(),
        written,
        backup: backups.into_iter().next(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::Provider;
    use crate::services::provider::test_support::isolated_home;
    use std::fs;

    fn provider() -> Provider {
        Provider::new(
            "zai".into(),
            "Z.AI".into(),
            json!({
                "npm": DEFAULT_NPM,
                "options": {"baseURL": "https://example.invalid/v1", "apiKey": "sk-secret"},
                "models": {"glm-5": {"name": "GLM-5"}}
            }),
        )
    }

    #[test]
    fn write_live_merges_provider_and_preserves_unrelated_config() {
        let _home = isolated_home("opencode");
        let path = get_opencode_config_path();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "{ // JSONC\n \"small_model\":\"zai/glm-mini\", \"theme\":\"dark\", \"provider\":{\"old\":{\"npm\":\"x\"}} }").unwrap();

        let result = write_live(&provider()).expect("OpenCode write");
        let written = fs::read_to_string(&path).unwrap();
        assert!(
            written.contains("// JSONC"),
            "the JSONC comment must survive the switch"
        );
        let value: Value = serde_json::from_str(&strip_jsonc_comments(&written)).unwrap();
        assert_eq!(value["theme"], "dark");
        assert_eq!(value["small_model"], "zai/glm-mini");
        assert_eq!(value["provider"]["old"]["npm"], "x");
        assert_eq!(
            value["provider"]["zai"]["options"]["baseURL"],
            "https://example.invalid/v1"
        );
        assert_eq!(value["model"], "zai/glm-5");
        assert!(result.backup.is_some());
    }

    #[test]
    fn write_live_preserves_jsonc_comments_and_key_order() {
        let _home = isolated_home("opencode-jsonc");
        let path = get_opencode_config_path();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            concat!(
                "{\n",
                "  // user theme\n",
                "  \"theme\": \"dark\",\n",
                "  \"provider\": {\n",
                "    \"old\": { \"npm\": \"x\" } // keep sibling\n",
                "  }\n",
                "}\n",
            ),
        )
        .unwrap();

        write_live(&provider()).expect("OpenCode write");
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            concat!(
                "{\n",
                "  // user theme\n",
                "  \"theme\": \"dark\",\n",
                "  \"provider\": {\n",
                "    \"zai\": {\n",
                "      \"npm\": \"@ai-sdk/openai-compatible\",\n",
                "      \"options\": {\n",
                "        \"baseURL\": \"https://example.invalid/v1\",\n",
                "        \"apiKey\": \"sk-secret\"\n",
                "      },\n",
                "      \"models\": {\n",
                "        \"glm-5\": {\n",
                "          \"name\": \"GLM-5\"\n",
                "        }\n",
                "      },\n",
                "      \"name\": \"Z.AI\"\n",
                "    },\n",
                "    \"old\": { \"npm\": \"x\" } // keep sibling\n",
                "  },\n",
                "  \"model\": \"zai/glm-5\"\n",
                "}\n",
            )
        );
    }

    #[test]
    fn write_live_rejects_malformed_root_without_overwriting() {
        let _home = isolated_home("opencode");
        let path = get_opencode_config_path();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "[]").unwrap();
        let err = write_live(&provider()).expect_err("array root must fail closed");
        assert!(err.to_string().contains("object root"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "[]");
    }

    #[test]
    fn repeated_write_is_idempotent_and_does_not_create_a_redundant_backup() {
        let _home = isolated_home("opencode");
        let result = write_live(&provider()).expect("initial write");
        let second = write_live(&provider()).expect("repeated write");
        assert!(result.backup.is_none());
        assert!(second.backup.is_none());
        assert!(second.written.is_empty());
        let value: Value =
            serde_json::from_str(&fs::read_to_string(get_opencode_config_path()).unwrap()).unwrap();
        assert_eq!(value["provider"]["zai"]["models"]["glm-5"]["name"], "GLM-5");
    }

    #[test]
    fn write_live_rejects_provider_without_endpoint_or_key_leak_in_error() {
        let _home = isolated_home("opencode");
        let bad = Provider::new(
            "bad".into(),
            "Bad".into(),
            json!({"options": {"apiKey": "sk-secret"}}),
        );
        let err = write_live(&bad).expect_err("missing endpoint must fail");
        assert!(err.to_string().contains("options.baseURL"));
        assert!(!err.to_string().contains("sk-secret"));
    }
}
