//! Passive CLI availability and explicit launches for the managed Kilo/Cline pages.

use super::{discovery, process, terminal};
use crate::{database::Database, provider::Provider};
use serde::Serialize;

#[allow(non_camel_case_types)]
struct Managed_cli_config {
    app_type: &'static str,
    executable_name: &'static str,
    windows_names: &'static [&'static str],
    install_url: &'static str,
}

const MANAGED_CLIS: &[Managed_cli_config] = &[
    Managed_cli_config {
        app_type: "kilo_cli",
        executable_name: "kilo",
        windows_names: &["kilo.exe", "kilo.cmd"],
        install_url: "https://kilo.ai/docs/code-with-ai/platforms/cli",
    },
    Managed_cli_config {
        app_type: "cline_cli",
        executable_name: "cline",
        windows_names: &["cline.exe", "cline.cmd"],
        install_url: "https://github.com/cline/cline/tree/main/apps/cli",
    },
];

#[allow(non_camel_case_types)]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Managed_cli_status {
    app_type: String,
    available: bool,
    binary: Option<String>,
    install_url: &'static str,
}

fn cli_config(app_type: &str) -> Result<&'static Managed_cli_config, String> {
    MANAGED_CLIS
        .iter()
        .find(|config| config.app_type == app_type)
        .ok_or_else(|| "managed_cli_app_unsupported".into())
}

pub(crate) fn managed_cli_status(app_type: String) -> Result<Managed_cli_status, String> {
    let config = cli_config(&app_type)?;
    let binary = discovery::find_tool(config.executable_name, config.windows_names);
    Ok(Managed_cli_status {
        app_type,
        available: binary.is_some(),
        binary: binary.map(|path| path.to_string_lossy().into_owned()),
        install_url: config.install_url,
    })
}

fn selected_provider(db: &Database, app_type: &str) -> Result<Provider, String> {
    let provider_id = db
        .get_current_provider(app_type)
        .map_err(String::from)?
        .ok_or_else(|| "managed_cli_provider_required".to_string())?;
    db.get_provider_by_id(&provider_id, app_type)
        .map_err(String::from)?
        .ok_or_else(|| "managed_cli_provider_required".into())
}

pub(crate) fn managed_cli_open_terminal(
    db: &Database,
    app_type: String,
    cwd: Option<String>,
) -> Result<serde_json::Value, String> {
    let config = cli_config(&app_type)?;
    process::working_directory(cwd.as_deref())?;
    let binary = discovery::find_tool(config.executable_name, config.windows_names)
        .ok_or_else(|| "managed_cli_not_found".to_string())?;
    let provider = selected_provider(db, &app_type)?;
    crate::services::provider::switching::switch_provider(db, &provider.id, &app_type)
        .map_err(String::from)?;

    let (mut environment, arguments) = match app_type.as_str() {
        "kilo_cli" => {
            let paths = crate::services::provider::config_paths::resolve("kilo_cli")
                .map_err(String::from)?;
            let path = paths
                .paths
                .first()
                .ok_or_else(|| "managed_cli_config_invalid".to_string())?;
            (
                vec![("KILO_CONFIG".into(), path.to_string_lossy().into_owned())],
                Vec::new(),
            )
        }
        "cline_cli" => (
            crate::services::provider::cline::launch_environment().map_err(String::from)?,
            vec![
                "-i".into(),
                "--auto-approve".into(),
                "false".into(),
                "--config".into(),
                crate::services::provider::cline::mirror_data_dir()
                    .parent()
                    .ok_or_else(|| "managed_cli_config_invalid".to_string())?
                    .to_string_lossy()
                    .into_owned(),
                "--provider".into(),
                "aag".into(),
                "--model".into(),
                provider
                    .settings_config
                    .get("model")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| "managed_cli_config_invalid".to_string())?
                    .trim()
                    .to_string(),
            ],
        ),
        _ => unreachable!("cli_config admits only managed CLI types"),
    };
    if let Some(proxy) = db
        .get_setting(crate::database::dao::settings::KEY_OUTBOUND_PROXY)
        .map_err(String::from)?
        .filter(|proxy| !proxy.is_empty())
    {
        for name in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY"] {
            environment.push((name.into(), proxy.clone()));
        }
    }
    environment.push(("NO_PROXY".into(), "localhost,127.0.0.1,::1".into()));
    terminal::launch_terminal(&binary, &arguments, &environment, cwd.as_deref())?;
    Ok(serde_json::json!({
        "appType": app_type,
        "binary": binary.to_string_lossy(),
        "providerId": provider.id,
        "env": super::redacted_environment(&environment),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passive_status_rejects_arbitrary_executable_names() {
        assert!(managed_cli_status("shell".into()).is_err());
        assert!(managed_cli_status("../kilo".into()).is_err());
        assert!(managed_cli_status("antigravity".into()).is_err());
    }

    #[test]
    fn status_contract_has_only_passive_availability_fields() {
        let json = serde_json::to_value(Managed_cli_status {
            app_type: "cline_cli".into(),
            available: false,
            binary: None,
            install_url: cli_config("cline_cli").unwrap().install_url,
        })
        .unwrap();
        assert_eq!(json["appType"], "cline_cli");
        assert_eq!(json["available"], false);
        assert_eq!(json["binary"], serde_json::Value::Null);
        assert!(json["installUrl"].as_str().unwrap().starts_with("https://"));
        assert_eq!(json.as_object().unwrap().len(), 4);
    }

    #[test]
    fn missing_provider_does_not_create_a_selection() {
        let db = Database::memory().unwrap();
        assert_eq!(
            selected_provider(&db, "cline_cli").err().unwrap(),
            "managed_cli_provider_required"
        );
        assert!(db.get_current_provider("cline_cli").unwrap().is_none());
    }
}
