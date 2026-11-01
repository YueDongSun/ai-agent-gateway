//! Cline CLI adapter backed by its documented provider settings artifacts.
//!
//! AAG writes only to `~/.cline_cli/data/settings/`. Cline's native
//! `~/.cline/data/settings/` remains untouched. The installed Cline core
//! contract uses `providers.json` plus an adjacent `models.json`; both files
//! are validated and written as one recoverable batch.

use crate::{error::AppError, provider::Provider, services::provider::LiveWriteResult};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

const AAG_PROVIDER_ID: &str = "aag";
const CLINE_PROTOCOL: &str = "openai-chat";
const CLINE_CLIENT: &str = "openai-compatible";

/// The AAG-owned data directory passed to Cline as `CLINE_DATA_DIR`.
pub(crate) fn mirror_data_dir() -> PathBuf {
    crate::config::get_home_dir()
        .join(".cline_cli")
        .join("data")
}

/// Environment entries that keep Cline on the same isolated provider store
/// AAG updates. The selected provider and model are passed as CLI arguments by
/// the launcher; credentials never appear in arguments or launch receipts.
pub(crate) fn launch_environment() -> Result<Vec<(String, String)>, AppError> {
    let data_dir = mirror_data_dir();
    let provider_settings = data_dir.join("settings").join("providers.json");
    crate::config::private_fs::validate_external_file_path(&provider_settings)?;
    Ok(vec![
        (
            "CLINE_DATA_DIR".into(),
            data_dir.to_string_lossy().into_owned(),
        ),
        (
            "CLINE_PROVIDER_SETTINGS_PATH".into(),
            provider_settings.to_string_lossy().into_owned(),
        ),
    ])
}

#[allow(dead_code)]
pub fn detect() -> bool {
    super::config_paths::resolve("cline_cli")
        .map(|resolved| resolved.existing)
        .unwrap_or(false)
}

/// Persist the selected provider into the isolated Cline settings directory.
pub fn write_live(provider: &Provider) -> Result<LiveWriteResult, AppError> {
    let (base_url, api_key, model) = validate_provider(provider)?;
    let data_dir = mirror_data_dir();
    let settings_dir = data_dir.join("settings");
    crate::config::private_fs::prepare_managed_private_dir(&data_dir)?;
    crate::config::private_fs::prepare_managed_private_dir(&settings_dir)?;

    let paths = super::config_paths::resolve("cline_cli")?;
    if paths.paths.len() != 2 {
        return Err(AppError::Config("cline_config_paths_invalid".into()));
    }
    let provider_settings = update_provider_settings(
        read_json_object(&paths.paths[0])?,
        &base_url,
        &api_key,
        &model,
    )?;
    let model_settings =
        update_model_settings(read_json_object(&paths.paths[1])?, &base_url, &model)?;
    let provider_bytes = serde_json::to_vec_pretty(&provider_settings)?;
    let model_bytes = serde_json::to_vec_pretty(&model_settings)?;
    let updates = [
        super::config_writes::ConfigFileUpdate {
            path: &paths.paths[0],
            contents: &provider_bytes,
            app_type: "cline",
        },
        super::config_writes::ConfigFileUpdate {
            path: &paths.paths[1],
            contents: &model_bytes,
            app_type: "cline",
        },
    ];
    let (written, backups) = super::config_writes::write_files(&updates)?;
    Ok(LiveWriteResult {
        app_type: "cline_cli".into(),
        provider_id: provider.id.clone(),
        written,
        backup: backups.into_iter().next(),
    })
}

fn validate_provider(provider: &Provider) -> Result<(String, String, String), AppError> {
    let object = provider
        .settings_config
        .as_object()
        .ok_or_else(|| AppError::Config("cline_settings_config_must_be_object".into()))?;
    let string_value = |key: &str| {
        object
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty() && !value.chars().any(char::is_control))
            .map(str::to_string)
            .ok_or_else(|| AppError::Config(format!("cline_settings_config_{key}_invalid")))
    };
    let base_url = string_value("baseUrl")?;
    let parsed = url::Url::parse(&base_url)
        .map_err(|_| AppError::Config("cline_settings_config_baseUrl_invalid".into()))?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(AppError::Config(
            "cline_settings_config_baseUrl_invalid".into(),
        ));
    }
    Ok((base_url, string_value("apiKey")?, string_value("model")?))
}

fn read_json_object(path: &Path) -> Result<Value, AppError> {
    crate::config::private_fs::validate_external_file_path(path)?;
    match std::fs::read(path) {
        Ok(raw) => {
            let value: Value = serde_json::from_slice(&raw).map_err(AppError::Json)?;
            if !value.is_object() {
                return Err(AppError::Config("cline_settings_root_invalid".into()));
            }
            Ok(value)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(error) => Err(AppError::io(path, error)),
    }
}

fn object_entry<'a>(
    object: &'a mut Map<String, Value>,
    key: &str,
) -> Result<&'a mut Map<String, Value>, AppError> {
    let value = object.entry(key).or_insert_with(|| json!({}));
    value
        .as_object_mut()
        .ok_or_else(|| AppError::Config("cline_settings_shape_invalid".into()))
}

fn update_provider_settings(
    mut document: Value,
    base_url: &str,
    api_key: &str,
    model: &str,
) -> Result<Value, AppError> {
    let root = document
        .as_object_mut()
        .expect("read_json_object validates root");
    if root.get("version").is_some_and(|version| version != 1) {
        return Err(AppError::Config(
            "cline_provider_settings_version_unsupported".into(),
        ));
    }
    root.insert("version".into(), json!(1));
    root.insert("lastUsedProvider".into(), json!(AAG_PROVIDER_ID));
    let providers = object_entry(root, "providers")?;
    let settings = json!({
        "provider": AAG_PROVIDER_ID,
        "apiKey": api_key,
        "model": model,
        "protocol": CLINE_PROTOCOL,
        "client": CLINE_CLIENT,
        "baseUrl": base_url
    });
    let updated_at = providers
        .get(AAG_PROVIDER_ID)
        .filter(|entry| entry.get("settings") == Some(&settings))
        .and_then(|entry| entry.get("updatedAt"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
    providers.insert(
        AAG_PROVIDER_ID.into(),
        json!({
            "settings": settings,
            "updatedAt": updated_at,
            "tokenSource": "manual"
        }),
    );
    Ok(document)
}

fn update_model_settings(
    mut document: Value,
    base_url: &str,
    model: &str,
) -> Result<Value, AppError> {
    let root = document
        .as_object_mut()
        .expect("read_json_object validates root");
    if root.get("version").is_some_and(|version| version != 1) {
        return Err(AppError::Config(
            "cline_model_settings_version_unsupported".into(),
        ));
    }
    root.insert("version".into(), json!(1));
    let providers = object_entry(root, "providers")?;
    let aag = object_entry(providers, AAG_PROVIDER_ID)?;
    aag.insert(
        "provider".into(),
        json!({
            "name": "AAG",
            "baseUrl": base_url,
            "defaultModelId": model,
            "protocol": CLINE_PROTOCOL,
            "client": CLINE_CLIENT
        }),
    );
    let models = object_entry(aag, "models")?;
    models.insert(model.into(), json!({"id": model, "name": model}));
    Ok(document)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::provider::test_support::isolated_home;

    fn provider(settings: Value) -> Provider {
        Provider::new("provider-1".into(), "Provider One".into(), settings)
    }

    #[test]
    fn writes_source_contract_into_private_mirror_and_preserves_other_providers() {
        let home = isolated_home("cline");
        let native = home.root.join(".cline/data/settings/providers.json");
        std::fs::create_dir_all(native.parent().unwrap()).unwrap();
        std::fs::write(&native, br#"{"native":"unchanged"}"#).unwrap();
        let mirror_settings = mirror_data_dir().join("settings/providers.json");
        std::fs::create_dir_all(mirror_settings.parent().unwrap()).unwrap();
        std::fs::write(
            &mirror_settings,
            br#"{"version":1,"providers":{"cline":{"settings":{"provider":"cline","auth":{"accessToken":"fixture_token"}},"updatedAt":"2026-01-01T00:00:00.000Z","tokenSource":"oauth"}}}"#,
        )
        .unwrap();
        let models = mirror_data_dir().join("settings/models.json");
        std::fs::write(
            &models,
            br#"{"version":1,"providers":{"other":{"models":{"x":{"id":"x"}}}}}"#,
        )
        .unwrap();

        let result = write_live(&provider(json!({
            "baseUrl":"http://127.0.0.1:8989/v1",
            "apiKey":"fixture_key",
            "model":"fixture-model"
        })))
        .unwrap();
        assert_eq!(result.app_type, "cline_cli");
        assert_eq!(result.provider_id, "provider-1");
        assert_eq!(result.written.len(), 2);
        let saved: Value =
            serde_json::from_slice(&std::fs::read(mirror_settings).unwrap()).unwrap();
        assert_eq!(saved["lastUsedProvider"], "aag");
        assert_eq!(saved["providers"]["aag"]["settings"]["provider"], "aag");
        assert_eq!(
            saved["providers"]["aag"]["settings"]["protocol"],
            "openai-chat"
        );
        assert_eq!(
            saved["providers"]["aag"]["settings"]["client"],
            "openai-compatible"
        );
        assert_eq!(
            saved["providers"]["aag"]["settings"]["apiKey"],
            "fixture_key"
        );
        assert_eq!(
            saved["providers"]["cline"]["settings"]["auth"]["accessToken"],
            "fixture_token"
        );
        let saved_models: Value = serde_json::from_slice(&std::fs::read(models).unwrap()).unwrap();
        assert_eq!(
            saved_models["providers"]["aag"]["models"]["fixture-model"]["id"],
            "fixture-model"
        );
        assert_eq!(saved_models["providers"]["other"]["models"]["x"]["id"], "x");
        assert_eq!(std::fs::read(native).unwrap(), br#"{"native":"unchanged"}"#);
        let repeated = write_live(&provider(json!({
            "baseUrl":"http://127.0.0.1:8989/v1",
            "apiKey":"fixture_key",
            "model":"fixture-model"
        })))
        .unwrap();
        assert!(
            repeated.written.is_empty(),
            "unchanged settings must not be rewritten"
        );
        assert!(
            repeated.backup.is_none(),
            "unchanged settings must not create backups"
        );
    }

    #[test]
    fn rejects_invalid_provider_fields_and_endpoint_schemes() {
        for settings in [
            json!({"apiKey":"key","model":"m"}),
            json!({"baseUrl":"file:///tmp","apiKey":"key","model":"m"}),
            json!({"baseUrl":"https://user:pass@example.invalid","apiKey":"key","model":"m"}),
            json!({"baseUrl":"https://example.invalid","apiKey":" ","model":"m"}),
            json!({"baseUrl":"https://example.invalid","apiKey":"key","model":"m\nbad"}),
        ] {
            assert!(validate_provider(&provider(settings)).is_err());
        }
    }

    #[test]
    fn provider_documents_match_installed_settings_envelopes_and_preserve_unrelated_data() {
        let existing_providers = json!({
            "version": 1,
            "providers": {
                "cline": {"settings": {"provider": "cline", "auth": {"accessToken": "native_auth"}}}
            }
        });
        let configured = update_provider_settings(
            existing_providers,
            "https://aag.example/v1",
            "fixture_key",
            "fixture-model",
        )
        .unwrap();
        assert_eq!(configured["lastUsedProvider"], "aag");
        assert_eq!(
            configured["providers"]["aag"]["settings"]["protocol"],
            "openai-chat"
        );
        assert_eq!(
            configured["providers"]["aag"]["settings"]["model"],
            "fixture-model"
        );
        assert_eq!(
            configured["providers"]["cline"]["settings"]["auth"]["accessToken"],
            "native_auth"
        );

        let existing_models =
            json!({"version": 1, "providers": {"other": {"models": {"x": {"id":"x"}}}}});
        let models =
            update_model_settings(existing_models, "https://aag.example/v1", "fixture-model")
                .unwrap();
        assert_eq!(
            models["providers"]["aag"]["provider"]["protocol"],
            "openai-chat"
        );
        assert_eq!(
            models["providers"]["aag"]["models"]["fixture-model"]["id"],
            "fixture-model"
        );
        assert_eq!(models["providers"]["other"]["models"]["x"]["id"], "x");
    }

    #[test]
    fn launch_environment_pins_supported_settings_paths_to_mirror() {
        let home = isolated_home("cline_launch");
        let environment = launch_environment().unwrap();
        assert_eq!(environment[0].0, "CLINE_DATA_DIR");
        assert_eq!(PathBuf::from(&environment[0].1), mirror_data_dir());
        assert_eq!(environment[1].0, "CLINE_PROVIDER_SETTINGS_PATH");
        assert_eq!(
            PathBuf::from(&environment[1].1),
            mirror_data_dir().join("settings/providers.json")
        );
        drop(home);
    }
}
