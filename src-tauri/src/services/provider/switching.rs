//! Apply a provider's client configuration before making it current.
//!
//! A failed filesystem write must remain visible to the caller and leave the
//! previous database selection intact. Mirror writers receive the selected row
//! explicitly so their model does not come from the previous current provider.

use crate::database::dao::settings::{KEY_CLAUDE_CLI_HOME, KEY_CODEX_CLI_HOME};
use crate::database::Database;
use crate::error::AppError;

use super::{AppType, LiveWriteResult};
use std::sync::{Arc, Mutex};

/// Shared by client-configuration commands across windows. The guard spans
/// mirror preparation and Desktop dispatch as well as provider selection.
#[derive(Clone, Default)]
#[allow(non_camel_case_types)]
pub struct Provider_switch_service {
    operation_lock: Arc<Mutex<()>>,
}

impl Provider_switch_service {
    /// Run one synchronous client mutation under the same admission guard as
    /// provider switching. Callers finish asynchronous runtime preparation
    /// first; keeping its original error type preserves operation context.
    pub(crate) fn with_client_write<T, E>(
        &self,
        operation: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, E>
    where
        E: From<AppError>,
    {
        let _guard = self
            .operation_lock
            .lock()
            .map_err(|_| AppError::Message("AAG_PROVIDER_SWITCH_LOCK_FAILED".into()))
            .map_err(E::from)?;
        operation()
    }

    pub fn switch_provider(
        &self,
        db: &Database,
        id: &str,
        app_type: &str,
    ) -> Result<LiveWriteResult, AppError> {
        self.with_client_write(|| switch_provider(db, id, app_type))
    }
}

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
    if app == AppType::Codex {
        super::codex::require_unmanaged_codex(db)?;
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
        AppType::ClineCli => super::cline::write_live(&provider)?,
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
    use crate::services::provider::test_support::isolated_home;
    use serde_json::{json, Value};
    use std::fs;
    use std::path::{Path, PathBuf};

    fn write(root: &Path, relative: &str, content: &str) -> PathBuf {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, content).unwrap();
        path
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
    fn managed_authority_blocks_legacy_codex_selection_without_file_changes() {
        let home = isolated_home("managed-legacy-switch");
        let db = providers(
            "codex",
            json!({"model":"old-model"}),
            json!({"model":"new-model"}),
        );
        db.set_setting("codex_session_authority_enabled", "1")
            .unwrap();
        let error = switch_provider(&db, "new", "codex").unwrap_err();
        assert_eq!(String::from(error), "session_client_authority_unavailable");
        assert_current(&db, "codex", "old");
        assert!(!home.root.join(".codex-cli").exists());
    }

    #[test]
    fn cloned_services_wait_for_the_same_switch_lock() {
        use std::sync::mpsc;
        use std::time::Duration;

        let service = Provider_switch_service::default();
        let worker_service = service.clone();
        let guard = service.operation_lock.lock().unwrap();
        let (ready_sender, ready_receiver) = mpsc::channel();
        let (result_sender, result_receiver) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let db = Database::memory().unwrap();
            ready_sender.send(()).unwrap();
            let result = worker_service.switch_provider(&db, "missing", "unknown");
            result_sender.send(result.map_err(String::from)).unwrap();
        });
        ready_receiver.recv_timeout(Duration::from_secs(5)).unwrap();
        let blocked_result = result_receiver.recv_timeout(Duration::from_millis(100));
        drop(guard);
        worker.join().unwrap();
        assert!(matches!(
            blocked_result,
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        let result = result_receiver
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        assert_eq!(result.unwrap_err(), "AAG_UNKNOWN_APP_TYPE");
    }

    #[test]
    fn competing_mirror_writes_wait_until_desktop_dispatch_releases_the_guard() {
        use std::sync::mpsc;
        use std::time::Duration;

        let home = isolated_home("serialized-mirror-writes");
        let mirror_config = write(&home.root, "mirror/config.toml", "original");
        let service = Provider_switch_service::default();
        let desktop_service = service.clone();
        let desktop_config = mirror_config.clone();
        let (prepared_sender, prepared_receiver) = mpsc::channel();
        let (dispatch_sender, dispatch_receiver) = mpsc::channel();
        let desktop = std::thread::spawn(move || {
            desktop_service.with_client_write(|| {
                fs::write(&desktop_config, "aag prepared")?;
                prepared_sender.send(()).unwrap();
                dispatch_receiver
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap();
                assert_eq!(fs::read_to_string(desktop_config)?, "aag prepared");
                Ok::<_, AppError>(())
            })
        });
        prepared_receiver
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        let mode_service = service.clone();
        let mode_config = mirror_config.clone();
        let (waiting_sender, waiting_receiver) = mpsc::channel();
        let (entered_sender, entered_receiver) = mpsc::channel();
        let mode_change = std::thread::spawn(move || {
            waiting_sender.send(()).unwrap();
            mode_service.with_client_write(|| {
                entered_sender.send(()).unwrap();
                fs::write(mode_config, "coexist applied")?;
                Ok::<_, AppError>(())
            })
        });
        waiting_receiver
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        let before_dispatch = entered_receiver.recv_timeout(Duration::from_millis(100));
        let prepared_config = fs::read_to_string(&mirror_config).unwrap();
        dispatch_sender.send(()).unwrap();
        desktop.join().unwrap().unwrap();
        mode_change.join().unwrap().unwrap();
        assert!(matches!(
            before_dispatch,
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        assert_eq!(prepared_config, "aag prepared");
        entered_receiver
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        assert_eq!(
            fs::read_to_string(mirror_config).unwrap(),
            "coexist applied"
        );
    }

    #[test]
    fn guarded_client_write_preserves_error_context_and_releases_the_guard() {
        let service = Provider_switch_service::default();
        let result = service.with_client_write(|| {
            Err::<(), _>(AppError::io(
                "fixture-config.toml",
                std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "fixture original context",
                ),
            ))
        });
        match result.unwrap_err() {
            AppError::Io { path, source } => {
                assert_eq!(path, "fixture-config.toml");
                assert_eq!(source.kind(), std::io::ErrorKind::PermissionDenied);
                assert_eq!(source.to_string(), "fixture original context");
            }
            error => panic!("original error was replaced: {error:?}"),
        }
        assert_eq!(
            service
                .with_client_write(|| Ok::<_, String>("next write"))
                .unwrap(),
            "next write"
        );
    }

    #[test]
    fn a_poisoned_switch_lock_fails_closed() {
        let service = Provider_switch_service::default();
        let worker_service = service.clone();
        let interrupted = std::thread::spawn(move || {
            let _guard = worker_service.operation_lock.lock().unwrap();
            panic!("fixture switch interruption");
        });
        assert!(interrupted.join().is_err());
        let db = providers("copilot", json!({}), json!({}));
        let result = service.switch_provider(&db, "new", "copilot");
        assert_eq!(
            String::from(result.unwrap_err()),
            "AAG_PROVIDER_SWITCH_LOCK_FAILED"
        );
        assert_current(&db, "copilot", "old");
    }

    #[test]
    fn failed_writes_preserve_current_provider_and_existing_files() {
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
            (
                "cline_cli",
                ".cline_cli/data/settings/providers.json",
                json!({"baseUrl":"http://127.0.0.1:8989/v1","apiKey":"fixture_key","model":"new-model"}),
            ),
        ] {
            let home = isolated_home("switch-failed-write");
            let blocked = write(&home.root, blocked_path, "invalid_client_config");
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
        let home = isolated_home("switch-codex-model");
        let native = write(&home.root, ".codex/config.toml", "native_configuration");
        let auth = write(
            &home.root,
            ".codex-cli/auth.json",
            r#"{"session":"fixture_session"}"#,
        );
        let db = providers(
            "codex",
            json!({"model": "old-model"}),
            json!({"model": "new-model"}),
        );
        // The catalog is authoritative: a selected model is written only
        // when an enabled upstream actually offers it.
        let upstream = serde_json::from_value(json!({
            "id": "switch_model_fixture",
            "name": "Switch model fixture",
            "type": "openai",
            "baseURL": "https://example.invalid/v1",
            "apiKey": "fixture_key",
            "models": ["old-model", "new-model"],
            "enabled": true,
            "autoTrackModels": false
        }))
        .unwrap();
        db.save_proxy_provider(&upstream).unwrap();
        let result = switch_provider(&db, "new", "codex").unwrap();
        assert_current(&db, "codex", "new");
        assert_eq!(result.provider_id, "new");
        assert_eq!(result.written.len(), 1);
        let config = fs::read_to_string(&result.written[0]).unwrap();
        let config = config.parse::<toml_edit::DocumentMut>().unwrap();
        assert_eq!(config["model"].as_str(), Some("new-model"));
        assert_eq!(
            config["profiles"]["aag"]["model"].as_str(),
            Some("new-model")
        );
        assert_eq!(fs::read_to_string(native).unwrap(), "native_configuration");
        assert_eq!(
            fs::read_to_string(auth).unwrap(),
            r#"{"session":"fixture_session"}"#
        );
    }

    #[test]
    fn discovered_custom_paths_are_the_exact_switch_targets() {
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
            let home = isolated_home("switch-custom-paths");
            let expected: Vec<PathBuf> = expected_paths
                .iter()
                .map(|path| write(&home.root, path, r#"{"theme":"fixture_theme"}"#))
                .collect();
            std::env::set_var(variable, home.root.join(override_path));
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
        let home = isolated_home("switch-pi-settings");
        let models = write(&home.root, ".pi/agent/models.json", r#"{"providers":{}}"#);
        write(&home.root, ".pi/agent/settings.json", "[]");
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
        let home = isolated_home("switch-gemini");
        let db = providers(
            "gemini",
            json!({"env": {"GEMINI_MODEL": "old-model"}}),
            json!({"env": {"GEMINI_MODEL": "new-model"}, "config": {"theme": "fixture_theme"}}),
        );
        let result = switch_provider(&db, "new", "gemini").unwrap();
        assert_current(&db, "gemini", "new");
        assert_eq!(result.provider_id, "new");
        let env = fs::read_to_string(home.root.join(".gemini/.env")).unwrap();
        assert!(env.contains("GEMINI_MODEL=new-model"));
        assert!(!env.contains("old-model"));
        let settings: Value = serde_json::from_str(
            &fs::read_to_string(home.root.join(".gemini/settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(settings["theme"], "fixture_theme");
    }

    #[test]
    fn claude_switch_updates_only_the_isolated_mirror() {
        for app_type in ["claude_code", "claude_desktop"] {
            let home = isolated_home("switch-claude-mirror");
            let native = write(&home.root, ".claude/settings.json", "native_configuration");
            let mirror = write(
                &home.root,
                ".claude-cli/settings.json",
                r#"{"theme":"fixture_theme"}"#,
            );
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
