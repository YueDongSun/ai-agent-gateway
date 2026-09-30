//! Apply a provider's client configuration before making it current.
//!
//! A failed filesystem write must remain visible to the caller and leave the
//! previous database selection intact. Mirror writers receive the selected row
//! explicitly so their model does not come from the previous current provider.

use crate::database::dao::settings::{KEY_CLAUDE_CLI_HOME, KEY_CODEX_CLI_HOME};
use crate::database::Database;
use crate::error::AppError;

use super::{AppType, LiveWriteResult};

pub fn switch_provider(
    db: &Database,
    id: &str,
    app_type: &str,
) -> Result<LiveWriteResult, AppError> {
    let app = AppType::from_db_str(app_type)
        .map_err(|_| AppError::InvalidInput("AAG_UNKNOWN_APP_TYPE".into()))?;
    if matches!(
        app,
        AppType::Antigravity | AppType::OpenClaw | AppType::Hermes | AppType::GrokBuild
    ) {
        return Err(AppError::InvalidInput(
            "AAG_PROVIDER_SWITCH_UNSUPPORTED".into(),
        ));
    }
    let provider = db
        .get_provider_by_id(id, app_type)?
        .ok_or_else(|| AppError::InvalidInput("AAG_PROVIDER_NOT_FOUND".into()))?;

    let mut result = match app {
        AppType::ClaudeCode | AppType::ClaudeDesktop => {
            let mirror_home = db.get_setting(KEY_CLAUDE_CLI_HOME)?;
            super::claude_mirror::switch_claude_mirror(db, mirror_home.as_deref())?
        }
        AppType::Codex => {
            let mirror_home = db.get_setting(KEY_CODEX_CLI_HOME)?;
            super::codex::switch_codex_mirror_for_provider(
                db,
                mirror_home.as_deref(),
                false,
                &provider,
            )?
        }
        AppType::Gemini => super::gemini::switch_gemini_mirror_for_provider(db, &provider)?,
        AppType::Copilot
        | AppType::Pi
        | AppType::OpenCode
        | AppType::KiloCli
        | AppType::KiloVsCode => {
            let resolved = super::config_paths::resolve(app_type)?;
            match app {
                AppType::Copilot => super::copilot::write_live_at(&provider, &resolved.paths)?,
                AppType::Pi => super::pi::write_live_at(&provider, &resolved.paths)?,
                AppType::OpenCode => super::opencode::write_live_at(&provider, &resolved.paths)?,
                AppType::KiloCli | AppType::KiloVsCode => {
                    super::kilo::write_live_at(&provider, &resolved.paths)?
                }
                _ => {
                    unreachable!("only clients with standard configuration paths enter this branch")
                }
            }
        }
        AppType::ClaudeVsCode => super::vscode_extension::write_live(
            super::vscode_extension::VsCodeApp::ClaudeCode,
            &provider,
        )?,
        AppType::Antigravity | AppType::OpenClaw | AppType::Hermes | AppType::GrokBuild => {
            return Err(AppError::InvalidInput(
                "AAG_PROVIDER_SWITCH_UNSUPPORTED".into(),
            ));
        }
    };
    db.set_current_provider(id, app_type)?;
    result.app_type = app_type.to_string();
    result.provider_id = provider.id;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::Provider;
    use serde_json::{json, Value};
    use std::ffi::OsString;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

    struct Fixture {
        root: PathBuf,
        environment: Vec<(&'static str, Option<OsString>)>,
    }

    impl Fixture {
        fn new() -> Self {
            let root = fs::canonicalize(std::env::temp_dir())
                .unwrap()
                .join(format!(
                    "aag_provider_switch_{}_{}",
                    std::process::id(),
                    NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
                ));
            fs::create_dir(&root).unwrap();
            crate::config::private_fs::prepare_managed_private_dir(&root).unwrap();
            let fixture = Self {
                root,
                environment: [
                    "AAG_HOME_DIR",
                    "AAG_CONFIG_DIR",
                    "COPILOT_HOME",
                    "PI_CODING_AGENT_DIR",
                    "GEMINI_CLI_HOME",
                    "OPENCODE_CONFIG",
                    "OPENCODE_CONFIG_DIR",
                    "KILO_CONFIG",
                    "KILO_CONFIG_DIR",
                    "XDG_CONFIG_HOME",
                ]
                .into_iter()
                .map(|key| (key, std::env::var_os(key)))
                .collect(),
            };
            for (key, _) in &fixture.environment {
                std::env::remove_var(key);
            }
            std::env::set_var("AAG_HOME_DIR", &fixture.root);
            std::env::set_var("AAG_CONFIG_DIR", fixture.root.join("aag_state"));
            fixture
        }

        fn write(&self, relative: &str, content: &str) -> PathBuf {
            let path = self.root.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, content).unwrap();
            path
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            for (key, value) in &self.environment {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn providers(app_type: &str, old: Value, new: Value) -> Database {
        let db = Database::memory().unwrap();
        db.save_provider(app_type, &Provider::new("old".into(), "Old".into(), old))
            .unwrap();
        db.save_provider(app_type, &Provider::new("new".into(), "New".into(), new))
            .unwrap();
        db.set_current_provider("old", app_type).unwrap();
        db
    }

    fn assert_current(db: &Database, app_type: &str, expected: &str) {
        assert_eq!(
            db.get_current_provider(app_type).unwrap().as_deref(),
            Some(expected)
        );
    }

    #[test]
    fn failed_writes_preserve_current_provider_and_existing_files() {
        let _lock = crate::config::test_env_lock().blocking_lock();
        for (app_type, blocked_path, config) in [
            ("codex", ".codex-cli", json!({"model": "new-model"})),
            (
                "gemini",
                ".gemini",
                json!({"env": {"GEMINI_MODEL": "new-model"}}),
            ),
            (
                "copilot",
                ".copilot/settings.json",
                json!({"model": "new-model"}),
            ),
            (
                "pi",
                ".pi/agent/models.json",
                json!({"baseUrl": "https://example.invalid/v1", "model": "new-model"}),
            ),
        ] {
            let fixture = Fixture::new();
            let blocked = fixture.write(blocked_path, "invalid_client_config");
            let db = providers(app_type, json!({}), config);
            assert!(switch_provider(&db, "new", app_type).is_err(), "{app_type}");
            assert_current(&db, app_type, "old");
            assert_eq!(
                fs::read_to_string(blocked).unwrap(),
                "invalid_client_config"
            );
        }
    }

    #[test]
    fn selected_codex_model_is_written_before_selection_changes() {
        let _lock = crate::config::test_env_lock().blocking_lock();
        let fixture = Fixture::new();
        let native = fixture.write(".codex/config.toml", "native_configuration");
        let auth = fixture.write(".codex-cli/auth.json", r#"{"session":"fixture_session"}"#);
        let db = providers(
            "codex",
            json!({"model": "old-model"}),
            json!({"model": "new-model"}),
        );
        let result = switch_provider(&db, "new", "codex").unwrap();
        assert_current(&db, "codex", "new");
        assert_eq!(result.provider_id, "new");
        assert_eq!(result.written.len(), 1);
        let config = fs::read_to_string(&result.written[0]).unwrap();
        assert!(config.contains("model = \"new-model\""));
        assert!(!config.contains("old-model"));
        assert_eq!(fs::read_to_string(native).unwrap(), "native_configuration");
        assert_eq!(
            fs::read_to_string(auth).unwrap(),
            r#"{"session":"fixture_session"}"#
        );
    }

    #[test]
    fn discovered_custom_paths_are_the_exact_switch_targets() {
        let _lock = crate::config::test_env_lock().blocking_lock();
        for (app_type, variable, override_path, expected_paths, config) in [
            (
                "copilot",
                "COPILOT_HOME",
                "custom_copilot",
                vec!["custom_copilot/settings.json"],
                json!({"model": "new-model"}),
            ),
            (
                "pi",
                "PI_CODING_AGENT_DIR",
                "custom_pi",
                vec!["custom_pi/models.json", "custom_pi/settings.json"],
                json!({"baseUrl": "https://example.invalid/v1", "model": "new-model"}),
            ),
            (
                "opencode",
                "OPENCODE_CONFIG",
                "custom_opencode/user.jsonc",
                vec!["custom_opencode/user.jsonc"],
                json!({"options": {"baseURL": "https://example.invalid/v1"}, "model": "new-model"}),
            ),
            (
                "kilo_cli",
                "KILO_CONFIG_DIR",
                "custom_kilo",
                vec!["custom_kilo/opencode.json"],
                json!({"options": {"baseURL": "https://example.invalid/v1"}, "model": "new-model"}),
            ),
            (
                "kilo_vscode",
                "KILO_CONFIG",
                "custom_extension/user.jsonc",
                vec!["custom_extension/user.jsonc"],
                json!({"options": {"baseURL": "https://example.invalid/v1"}, "model": "new-model"}),
            ),
        ] {
            let fixture = Fixture::new();
            let expected: Vec<PathBuf> = expected_paths
                .iter()
                .map(|path| fixture.write(path, r#"{"theme":"fixture_theme"}"#))
                .collect();
            std::env::set_var(variable, fixture.root.join(override_path));
            let resolved = super::super::config_paths::resolve(app_type).unwrap();
            assert_eq!(resolved.paths, expected, "{app_type}");
            let db = providers(app_type, json!({}), config);
            let result = switch_provider(&db, "new", app_type).unwrap();
            assert_current(&db, app_type, "new");
            assert_eq!(result.app_type, app_type);
            assert_eq!(result.provider_id, "new");
            assert_eq!(
                result.written.iter().map(PathBuf::from).collect::<Vec<_>>(),
                expected,
                "{app_type} must write exactly what discovery reported"
            );
            for path in expected {
                let document: Value =
                    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
                assert_eq!(document["theme"], "fixture_theme");
            }
            assert!(
                result.backup.is_some(),
                "{app_type} must preserve existing content"
            );
        }
    }

    #[test]
    fn pi_invalid_settings_does_not_modify_models_or_selection() {
        let _lock = crate::config::test_env_lock().blocking_lock();
        let fixture = Fixture::new();
        let models = fixture.write(".pi/agent/models.json", r#"{"providers":{}}"#);
        fixture.write(".pi/agent/settings.json", "[]");
        let db = providers(
            "pi",
            json!({}),
            json!({"baseUrl": "https://example.invalid/v1", "model": "new-model"}),
        );
        assert!(switch_provider(&db, "new", "pi").is_err());
        assert_current(&db, "pi", "old");
        assert_eq!(fs::read_to_string(models).unwrap(), r#"{"providers":{}}"#);
    }

    #[test]
    fn selected_gemini_model_and_settings_are_written_before_selection_changes() {
        let _lock = crate::config::test_env_lock().blocking_lock();
        let fixture = Fixture::new();
        let db = providers(
            "gemini",
            json!({"env": {"GEMINI_MODEL": "old-model"}}),
            json!({"env": {"GEMINI_MODEL": "new-model"}, "config": {"theme": "fixture_theme"}}),
        );
        let result = switch_provider(&db, "new", "gemini").unwrap();
        assert_current(&db, "gemini", "new");
        assert_eq!(result.provider_id, "new");
        let env = fs::read_to_string(fixture.root.join(".gemini/.env")).unwrap();
        assert!(env.contains("GEMINI_MODEL=new-model"));
        assert!(!env.contains("old-model"));
        let settings: Value = serde_json::from_str(
            &fs::read_to_string(fixture.root.join(".gemini/settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(settings["theme"], "fixture_theme");
    }

    #[test]
    fn claude_switch_updates_only_the_isolated_mirror() {
        let _lock = crate::config::test_env_lock().blocking_lock();
        for app_type in ["claude_code", "claude_desktop"] {
            let fixture = Fixture::new();
            let native = fixture.write(".claude/settings.json", "native_configuration");
            let mirror = fixture.write(".claude-cli/settings.json", r#"{"theme":"fixture_theme"}"#);
            let db = providers(app_type, json!({}), json!({}));
            let result = switch_provider(&db, "new", app_type).unwrap();
            assert_current(&db, app_type, "new");
            assert_eq!(result.app_type, app_type);
            assert_eq!(result.provider_id, "new");
            assert!(result.written.iter().any(|path| Path::new(path) == mirror));
            assert_eq!(fs::read_to_string(native).unwrap(), "native_configuration");
            let applied: Value =
                serde_json::from_str(&fs::read_to_string(mirror).unwrap()).unwrap();
            assert_eq!(applied["theme"], "fixture_theme");
            assert_eq!(
                applied["env"]["ANTHROPIC_BASE_URL"],
                "http://127.0.0.1:8989"
            );
            let backup = result.backup.expect("existing mirror is backed up");
            assert_eq!(
                fs::read_to_string(backup).unwrap(),
                r#"{"theme":"fixture_theme"}"#
            );
        }
    }

    #[test]
    fn invalid_or_unsupported_switch_does_not_change_current_provider() {
        let db = providers("copilot", json!({}), json!({}));
        assert_eq!(
            String::from(switch_provider(&db, "missing", "copilot").unwrap_err()),
            "AAG_PROVIDER_NOT_FOUND"
        );
        assert_eq!(
            String::from(switch_provider(&db, "new", "unknown").unwrap_err()),
            "AAG_UNKNOWN_APP_TYPE"
        );
        for app_type in ["antigravity", "openclaw", "hermes", "grok_build"] {
            let unsupported_db = providers(app_type, json!({}), json!({}));
            assert_eq!(
                String::from(switch_provider(&unsupported_db, "new", app_type).unwrap_err()),
                "AAG_PROVIDER_SWITCH_UNSUPPORTED"
            );
            assert_current(&unsupported_db, app_type, "old");
        }
        assert_current(&db, "copilot", "old");
    }
}
