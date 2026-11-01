//! Read-only client configuration target discovery for the active host.
use crate::{database::Database, error::AppError};
use serde::Serialize;
use std::path::PathBuf;

use super::{config_paths, AppType};

#[allow(non_camel_case_types)]
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Provider_config_status {
    app_type: String,
    platform: String,
    supported: bool,
    mode: &'static str,
    source: &'static str,
    config_paths: Vec<String>,
    existing: bool,
    reason: Option<&'static str>,
}

fn platform_name() -> &'static str {
    match std::env::consts::OS {
        "macos" => "macos",
        "windows" => "windows",
        "linux" => "linux",
        _ => "unsupported",
    }
}

fn mirror_status(
    app_type: &str,
    directory: PathBuf,
    source: &'static str,
    filename: &str,
) -> Result<Provider_config_status, AppError> {
    let path = directory.join(filename);
    crate::config::private_fs::validate_external_file_path(&path)?;
    let existing = std::fs::symlink_metadata(&path).is_ok();
    Ok(Provider_config_status {
        app_type: app_type.into(),
        platform: platform_name().into(),
        supported: platform_name() != "unsupported",
        mode: "mirror",
        source: if existing { "existing" } else { source },
        config_paths: vec![path.to_string_lossy().into_owned()],
        existing,
        reason: (platform_name() == "unsupported").then_some("client_config_platform_unsupported"),
    })
}

pub fn provider_config_status(
    db: &Database,
    app_type: &str,
) -> Result<Provider_config_status, AppError> {
    let app = AppType::from_db_str(app_type)?;
    match app {
        AppType::ClaudeCode | AppType::ClaudeDesktop => {
            let setting = db.get_setting(crate::database::dao::settings::KEY_CLAUDE_CLI_HOME)?;
            let directory = crate::config::get_claude_mirror_dir(setting.as_deref());
            mirror_status(
                app_type,
                directory,
                if setting
                    .as_deref()
                    .is_some_and(|value| !value.trim().is_empty())
                {
                    "setting"
                } else {
                    "default"
                },
                "settings.json",
            )
        }
        AppType::Codex => {
            let setting = db.get_setting(crate::database::dao::settings::KEY_CODEX_CLI_HOME)?;
            let directory = crate::config::get_codex_mirror_dir(setting.as_deref());
            mirror_status(
                app_type,
                directory,
                if setting
                    .as_deref()
                    .is_some_and(|value| !value.trim().is_empty())
                {
                    "setting"
                } else {
                    "default"
                },
                "config.toml",
            )
        }
        AppType::Copilot
        | AppType::Pi
        | AppType::Gemini
        | AppType::OpenCode
        | AppType::KiloCli
        | AppType::ClineCli
        | AppType::KiloVsCode => {
            let resolved = config_paths::resolve(app_type)?;
            Ok(Provider_config_status {
                app_type: app_type.into(),
                platform: platform_name().into(),
                supported: platform_name() != "unsupported",
                mode: if app == AppType::ClineCli {
                    "mirror"
                } else {
                    "config"
                },
                source: resolved.source,
                config_paths: resolved
                    .paths
                    .iter()
                    .map(|path| path.to_string_lossy().into_owned())
                    .collect(),
                existing: resolved.existing,
                reason: (platform_name() == "unsupported")
                    .then_some("client_config_platform_unsupported"),
            })
        }
        AppType::ClaudeVsCode => Ok(Provider_config_status {
            app_type: app_type.into(),
            platform: platform_name().into(),
            supported: platform_name() != "unsupported",
            mode: "environment",
            source: "default",
            config_paths: Vec::new(),
            existing: false,
            reason: (platform_name() == "unsupported")
                .then_some("client_config_platform_unsupported"),
        }),
        AppType::Antigravity | AppType::OpenClaw | AppType::Hermes | AppType::GrokBuild => {
            Ok(Provider_config_status {
                app_type: app_type.into(),
                platform: platform_name().into(),
                supported: false,
                mode: "unsupported",
                source: "unsupported",
                config_paths: Vec::new(),
                existing: false,
                reason: Some("client_config_write_unsupported"),
            })
        }
    }
}
