//! Automatic AAG mirror configuration after the unified provider inventory changes.
//! Upstream credentials stay in the supplier store; clients receive the local gateway token.

use crate::{database::Database, error::AppError};

pub(crate) const KEY_UNIFIED_DEFAULT_MODEL: &str = "unified_default_model";
const MANAGED_CLIENT_ID: &str = "aag_unified_gateway";
const MANAGED_CLIENT_CATEGORY: &str = "aag_unified_client";
const MANAGED_CLIENTS: &[&str] = &[
    "claude_code",
    "claude_desktop",
    "codex",
    "copilot",
    "gemini",
    "opencode",
    "kilo_cli",
    "cline_cli",
    "claude_vscode",
    "kilo_vscode",
];

/// Prepare every AAG launcher from the same local endpoint. Existing launcher
/// adapters perform their backed-up file writes when that client is opened.
fn configure_client_records(db: &Database, model: Option<&str>) -> Result<(), AppError> {
    let proxy = db.get_proxy_config()?;
    let base = format!("http://{}:{}", proxy.client_endpoint_host(), proxy.port);
    let offered = super::model_catalog::offered_models(db)?;
    let mirror_roles = db
        .get_setting(super::mirror_models::KEY_ENABLED)?
        .as_deref()
        == Some("1");
    let mut models = serde_json::Map::new();
    for item in &offered {
        models.insert(
            item.model_id.clone(),
            serde_json::json!({"name":item.model_id}),
        );
    }
    for app in MANAGED_CLIENTS {
        let existing = db.get_provider_by_id(MANAGED_CLIENT_ID, app)?;
        if existing
            .as_ref()
            .is_some_and(|provider| provider.category.as_deref() != Some(MANAGED_CLIENT_CATEGORY))
        {
            return Err(AppError::Config("AAG_UNIFIED_CLIENT_ID_CONFLICT".into()));
        }
        let Some(model) = model else {
            if existing.is_some() {
                db.delete_provider(MANAGED_CLIENT_ID, app)?;
            }
            continue;
        };
        let model = if mirror_roles {
            match *app {
                "claude_code" | "claude_desktop" | "claude_vscode" => {
                    super::mirror_models::DEFAULT_CLAUDE_ROLE
                }
                "codex" => super::mirror_models::DEFAULT_CODEX_ROLE,
                _ => model,
            }
        } else {
            model
        };
        let settings = match *app {
            "claude_code" | "claude_desktop" | "claude_vscode" => {
                serde_json::json!({"env":{"ANTHROPIC_BASE_URL":base,"ANTHROPIC_AUTH_TOKEN":proxy.auth_token,"ANTHROPIC_MODEL":model}})
            }
            "gemini" => {
                serde_json::json!({"env":{"GOOGLE_GEMINI_BASE_URL":base,"GEMINI_API_KEY":proxy.auth_token,"GEMINI_MODEL":model}})
            }
            "opencode" | "kilo_cli" | "kilo_vscode" => {
                serde_json::json!({"npm":"@ai-sdk/openai-compatible","options":{"baseURL":format!("{base}/v1"),"apiKey":proxy.auth_token},"model":model,"models":models})
            }
            _ => {
                serde_json::json!({"baseUrl":format!("{base}/v1"),"apiKey":proxy.auth_token,"model":model})
            }
        };
        let mut provider = crate::provider::Provider::new(
            MANAGED_CLIENT_ID.into(),
            "AAG unified gateway".into(),
            settings,
        );
        provider.category = Some(MANAGED_CLIENT_CATEGORY.into());
        provider.in_failover_queue = false;
        provider.sync_locked = true;
        db.save_provider(app, &provider)?;
        let current = db.get_current_provider(app)?;
        // An explicit later direct-provider selection remains the user's choice.
        if current.is_none() || current.as_deref() == Some(MANAGED_CLIENT_ID) {
            db.set_current_provider(MANAGED_CLIENT_ID, app)?;
        }
    }
    Ok(())
}

pub(crate) fn selected_model(db: &Database) -> Result<Option<String>, AppError> {
    let snapshot = super::model_catalog::offered_models(db)?;
    let models: Vec<_> = snapshot.iter()
        .map(|model| model.model_id.as_str())
        .collect();
    let current = db.get_setting(KEY_UNIFIED_DEFAULT_MODEL)?;
    Ok(current
        .filter(|model| models.contains(&model.as_str()))
        .or_else(|| models.first().map(|model| (*model).to_string())))
}

pub(crate) fn configure_mirrors(db: &Database) -> Result<(), AppError> {
    configure_mirrors_for_runtime(db, None)
}

pub(crate) fn configure_mirrors_for_runtime(
    db: &Database,
    codex_models: Option<&[String]>,
) -> Result<(), AppError> {
    let codex_managed = super::session_authority::client_config::managed_launch_required(db)
        .map_err(AppError::Config)?;
    let Some(model) = selected_model(db)? else {
        db.set_setting(KEY_UNIFIED_DEFAULT_MODEL, "")?;
        configure_client_records(db, None)?;
        return Ok(());
    };
    db.set_setting(KEY_UNIFIED_DEFAULT_MODEL, &model)?;
    configure_client_records(db, Some(&model))?;
    let codex_home = db.get_setting(crate::database::dao::settings::KEY_CODEX_CLI_HOME)?;
    let claude_home = db.get_setting(crate::database::dao::settings::KEY_CLAUDE_CLI_HOME)?;
    if !codex_managed {
        if let Some(models) = codex_models {
            super::provider::codex::apply_codex_runtime_models(db, codex_home.as_deref(), models)?;
        } else {
            super::provider::codex::switch_codex_mirror(db, codex_home.as_deref(), false)?;
        }
    }
    super::provider::claude_mirror::switch_claude_mirror(db, claude_home.as_deref())?;
    let claude_directory = crate::config::get_claude_mirror_dir(claude_home.as_deref());
    super::provider::claude_desktop::write_gateway_profile(db, &claude_directory)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::provider::test_support::isolated_home;
    use crate::services::suppliers::{save_supplier, SupplierSaveInput};
    use serde_json::json;

    #[test]
    fn managed_authority_skips_codex_files_and_preserves_claude_supplier_setup() {
        let home = isolated_home("managed-legacy-supplier");
        let db = Database::memory().unwrap();
        db.set_setting("codex_session_authority_enabled", "1")
            .unwrap();
        let mut proxy = db.get_proxy_config().unwrap();
        proxy.auth_token = "fixture_gateway_token".into();
        db.save_proxy_config(&proxy).unwrap();
        let input: SupplierSaveInput = serde_json::from_value(json!({
            "expectedGeneration":0, "name":"Fixture", "baseUrl":"https://example.invalid",
            "apiKey":"fixture_upstream_secret", "endpoints":[{"protocol":"openai-chat","models":[{"modelId":"fixture_model"}]}]
        })).unwrap();
        save_supplier(&db, input).unwrap();
        configure_mirrors(&db).unwrap();
        assert!(!home.root.join(".codex-cli").exists());
        assert!(home.root.join(".claude-cli/settings.json").is_file());
        assert!(home
            .root
            .join(".claude-cli/desktop_local/Claude-3p/claude_desktop_config.json")
            .is_file());
    }

    #[test]
    fn one_supplier_configures_both_mirrors_without_native_account_changes() {
        let home = isolated_home("supplier-setup");
        let native_codex = home.root.join(".codex");
        let native_claude = home.root.join(".claude");
        std::fs::create_dir_all(&native_codex).unwrap();
        std::fs::create_dir_all(&native_claude).unwrap();
        std::fs::write(
            native_codex.join("config.toml"),
            "model = \"native-model\"\n",
        )
        .unwrap();
        std::fs::write(native_claude.join("settings.json"), "{\"theme\":\"dark\"}").unwrap();
        let db = Database::memory().unwrap();
        let mut config = db.get_proxy_config().unwrap();
        config.auth_token = "fixture_gateway_token".into();
        db.save_proxy_config(&config).unwrap();
        let input: SupplierSaveInput = serde_json::from_value(json!({
            "expectedGeneration": 0, "name": "One provider", "baseUrl": "https://example.invalid",
            "apiKey": "fixture_upstream_secret", "endpoints": [{"protocol":"openai-chat", "models":[{"modelId":"test-model"}]}]
        })).unwrap();
        save_supplier(&db, input).unwrap();
        configure_mirrors(&db).unwrap();
        let codex = std::fs::read_to_string(home.root.join(".codex-cli/config.toml")).unwrap();
        let claude = std::fs::read_to_string(home.root.join(".claude-cli/settings.json")).unwrap();
        assert!(codex.contains("test-model"));
        assert!(codex.contains("fixture_gateway_token"));
        assert!(claude.contains("test-model"));
        assert!(claude.contains("fixture_gateway_token"));
        assert!(!codex.contains("fixture_upstream_secret"));
        assert!(!claude.contains("fixture_upstream_secret"));
        for app in MANAGED_CLIENTS {
            assert_eq!(
                db.get_current_provider(app).unwrap().as_deref(),
                Some(MANAGED_CLIENT_ID)
            );
            let provider = db
                .get_provider_by_id(MANAGED_CLIENT_ID, app)
                .unwrap()
                .unwrap();
            assert!(
                !provider.in_failover_queue,
                "gateway clients must never become upstream loops"
            );
            assert!(!provider
                .settings_config
                .to_string()
                .contains("fixture_upstream_secret"));
        }
        assert_eq!(
            std::fs::read_to_string(native_codex.join("config.toml")).unwrap(),
            "model = \"native-model\"\n"
        );
        assert_eq!(
            std::fs::read_to_string(native_claude.join("settings.json")).unwrap(),
            "{\"theme\":\"dark\"}"
        );
        assert!(
            db.get_all_routes().unwrap().is_empty(),
            "supplier planner needs no manual routes"
        );
    }
}
