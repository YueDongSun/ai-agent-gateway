use super::{discovery, process};
use crate::database::Database;
use serde::Serialize;

/// Launch the selected editor with a profile bound to its AAG environment.
/// VS Code reuses the environment of an existing instance unless user-data-dir
/// is distinct; a changed proxy/token therefore selects a different profile.
pub(crate) fn vscode_open_with_extension(
    db: &Database,
    app_type: String,
) -> Result<serde_json::Value, String> {
    let ext = match app_type.as_str() {
        "claude_vscode" => VsCodeExt::ClaudeCode,
        "kilo_vscode" => VsCodeExt::KiloCode,
        _ => return Err("vscode_app_type_invalid".into()),
    };

    // Refresh the exact resolved config target before launch. This keeps the
    // extension's file and the path shown by configuration discovery aligned.
    if let Some(current) = db.get_current_provider(&app_type).map_err(String::from)? {
        crate::services::provider::switching::switch_provider(db, &current, &app_type)
            .map_err(String::from)?;
    }

    let proxy_cfg = db.get_proxy_config().map_err(String::from)?;
    let outbound_proxy = db
        .get_setting(crate::database::dao::settings::KEY_OUTBOUND_PROXY)
        .map_err(String::from)?;

    // Host loopback normalization — `ProxyConfig::client_endpoint_host`.
    if !proxy_cfg.is_loopback_bind() {
        return Err("vscode_proxy_host_not_loopback".into());
    }
    let base_url = format!(
        "http://{}:{}",
        proxy_cfg.client_endpoint_host(),
        proxy_cfg.port
    );
    let base_url_v1 = format!("{base_url}/v1");
    let auth_token = proxy_cfg.auth_token.trim();

    // Preserve the provider environment contract from provider/vscode_extension.rs.
    let mut env: Vec<(String, String)> = Vec::new();
    match ext {
        VsCodeExt::ClaudeCode => {
            env.push(("ANTHROPIC_BASE_URL".into(), base_url.clone()));
            if !auth_token.is_empty() {
                env.push(("ANTHROPIC_AUTH_TOKEN".into(), auth_token.to_string()));
            }
        }
        VsCodeExt::KiloCode => {
            // Prefer the managed config file (the write_live target). If
            // it doesn't exist yet, inject inline content instead — the
            // extension treats KILO_CONFIG_CONTENT as a virtual source.
            let kilo_paths = crate::services::provider::config_paths::resolve("kilo_vscode")
                .map_err(String::from)?
                .paths;
            let kilo_cfg = kilo_paths
                .into_iter()
                .next()
                .ok_or_else(|| "AAG_CLIENT_CONFIG_PATHS_INVALID".to_string())?;
            if kilo_cfg.exists() {
                env.push((
                    "KILO_CONFIG".into(),
                    kilo_cfg.to_string_lossy().into_owned(),
                ));
            } else {
                let token = if auth_token.is_empty() {
                    "aag"
                } else {
                    auth_token
                };
                let inline = serde_json::json!({
                    "provider": {
                        "aag": {
                            "npm": "@ai-sdk/openai-compatible",
                            "options": { "baseURL": base_url_v1, "apiKey": token },
                            "models": { "aag-default": { "name": "aag-default" } }
                        }
                    },
                    "model": "aag/aag-default"
                });
                env.push(("KILO_CONFIG_CONTENT".into(), inline.to_string()));
            }
        }
    }
    if let Some(proxy) = &outbound_proxy {
        env.push(("HTTP_PROXY".into(), proxy.clone()));
        env.push(("HTTPS_PROXY".into(), proxy.clone()));
        env.push(("ALL_PROXY".into(), proxy.clone()));
    }
    env.push(("NO_PROXY".into(), "localhost,127.0.0.1,::1".into()));

    process::validate_environment(&env)?;
    let installation = discovery::find_editor().ok_or_else(|| "vscode_not_found".to_string())?;
    let profile = profile_directory(&installation, &app_type, &env);
    crate::config::private_fs::prepare_managed_private_dir(&profile).map_err(String::from)?;
    let mut command = editor_command(&installation, &profile, &env)?;
    process::launch(&mut command)?;
    let bin_str = installation.binary.to_string_lossy().into_owned();
    log::info!(
        "vscode_open_with_extension: launched app_type={} auth_token_set={}",
        app_type,
        !auth_token.is_empty()
    );

    Ok(serde_json::json!({
        "binary": bin_str,
        "appType": app_type,
        "baseUrl": base_url,
        "authTokenSet": !auth_token.is_empty(),
        "env": super::redacted_environment(&env),
    }))
}

/// Diagnostic: which VS Code binary the launcher would use + whether the
/// Claude Code / Kilo Code extension is installed.
pub(crate) fn vscode_extension_status() -> Result<VsCodeExtensionStatusDto, String> {
    let installation = discovery::find_editor();
    let extensions_dir = installation
        .as_ref()
        .map(|editor| {
            editor
                .kind
                .extension_directory(&crate::config::get_home_dir())
        })
        .filter(|directory| directory.is_dir());
    let mut claude_present = false;
    let mut kilo_present = false;
    if let Some(ref directory) = extensions_dir {
        let entries =
            std::fs::read_dir(directory).map_err(|_| "vscode_extension_scan_failed".to_string())?;
        for (index, entry) in entries.enumerate() {
            if index >= 1024 {
                return Err("vscode_extension_scan_limit".into());
            }
            let entry = entry.map_err(|_| "vscode_extension_scan_failed".to_string())?;
            if !entry
                .file_type()
                .map_err(|_| "vscode_extension_scan_failed".to_string())?
                .is_dir()
            {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            claude_present |= extension_matches(&name, "anthropic.claude-code");
            kilo_present |= extension_matches(&name, "kilocode.kilo-code");
        }
    }
    Ok(VsCodeExtensionStatusDto {
        binary: installation.map(|editor| editor.binary.to_string_lossy().into_owned()),
        extensions_dir: extensions_dir.map(|p| p.to_string_lossy().into_owned()),
        claude_code_installed: claude_present,
        kilo_code_installed: kilo_present,
    })
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct VsCodeExtensionStatusDto {
    pub binary: Option<String>,
    pub extensions_dir: Option<String>,
    pub claude_code_installed: bool,
    pub kilo_code_installed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VsCodeExt {
    ClaudeCode,
    KiloCode,
}

pub(super) fn extension_matches(directory: &str, extension: &str) -> bool {
    directory.strip_prefix(extension).is_some_and(|suffix| {
        suffix.is_empty()
            || suffix
                .strip_prefix('-')
                .and_then(|version| version.chars().next())
                .is_some_and(|first| first.is_ascii_digit())
    })
}

pub(super) fn profile_directory(
    installation: &discovery::editor_installation,
    app_type: &str,
    environment: &[(String, String)],
) -> std::path::PathBuf {
    use sha2::{Digest, Sha256};
    let mut environment = environment.to_vec();
    environment.sort();
    let mut digest = Sha256::new();
    // Length-prefixed serialization avoids ambiguity between keys/values.
    let material = serde_json::to_vec(&(
        installation.binary.as_os_str().as_encoded_bytes(),
        environment,
    ))
    .expect("launcher environment is serializable");
    digest.update(material);
    crate::config::get_app_config_dir()
        .join("editor_profiles")
        .join(installation.kind.name())
        .join(app_type)
        .join(hex::encode(&digest.finalize()[..16]))
}

pub(super) fn editor_command(
    installation: &discovery::editor_installation,
    profile: &std::path::Path,
    environment: &[(String, String)],
) -> Result<std::process::Command, String> {
    process::validate_environment(environment)?;
    let mut command = process::background_command(&installation.binary);
    command
        .arg("--new-window")
        .arg("--user-data-dir")
        .arg(profile);
    // Retain installed extensions from the editor actually selected, without
    // copying credentials or the native editor's user settings/profile.
    let extensions = installation
        .kind
        .extension_directory(&crate::config::get_home_dir());
    if extensions.is_dir() {
        command.arg("--extensions-dir").arg(extensions);
    }
    for name in [
        "VSCODE_IPC_HOOK_CLI",
        "VSCODE_CWD",
        "ELECTRON_RUN_AS_NODE",
        "ANTHROPIC_BASE_URL",
        "ANTHROPIC_AUTH_TOKEN",
        "ANTHROPIC_API_KEY",
        "KILO_CONFIG",
        "KILO_CONFIG_CONTENT",
    ] {
        command.env_remove(name);
    }
    command.envs(environment.iter().map(|(key, value)| (key, value)));
    command.env("PATH", discovery::child_path(&installation.binary)?);
    Ok(command)
}
