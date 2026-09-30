//! Claude mirror status and launch orchestration. Native configuration is read-only.

use super::discovery::{self, desktop_platform};
use super::process;
use crate::database::Database;
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ClaudeMirrorStatusDto {
    pub mirror_dir: String,
    pub initialized: bool,
    pub base_url_written: bool,
    pub auth_token_written: bool,
    pub mode: String,
    pub claude_binary: Option<String>,
    pub desktop_supported: bool,
}

fn checked_mirror_directory(setting: Option<&str>) -> Result<PathBuf, String> {
    let directory = crate::config::get_claude_mirror_dir(setting);
    if crate::config::same_dir(&directory, &crate::config::get_claude_config_dir()) {
        return Err("claude_mirror_native_directory_forbidden".into());
    }
    if !directory.is_absolute()
        || directory
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
        || directory.to_string_lossy().chars().any(char::is_control)
    {
        return Err("claude_mirror_directory_invalid".into());
    }
    Ok(directory)
}

pub(crate) fn claude_mirror_status(db: &Database) -> Result<ClaudeMirrorStatusDto, String> {
    let setting = db
        .get_setting(crate::database::dao::settings::KEY_CLAUDE_CLI_HOME)
        .map_err(String::from)?;
    let mirror_dir = checked_mirror_directory(setting.as_deref())?;
    let path = mirror_dir.join("settings.json");
    crate::config::private_fs::validate_external_file_path(&path).map_err(String::from)?;
    let settings = match std::fs::read(&path) {
        Ok(raw) => Some(
            serde_json::from_slice::<serde_json::Value>(&raw)
                .map_err(|_| "claude_mirror_settings_invalid".to_string())?,
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => return Err("claude_mirror_settings_read_failed".into()),
    };
    let environment = settings.as_ref().and_then(|value| value.get("env"));
    Ok(ClaudeMirrorStatusDto {
        mirror_dir: mirror_dir.to_string_lossy().into_owned(),
        initialized: settings.is_some(),
        base_url_written: environment
            .and_then(|value| value.get("ANTHROPIC_BASE_URL"))
            .and_then(|value| value.as_str())
            .is_some(),
        auth_token_written: environment
            .and_then(|value| value.get("ANTHROPIC_AUTH_TOKEN"))
            .is_some(),
        mode: crate::services::provider::claude_mirror::claude_mirror_mode_from_db(db)
            .as_str()
            .to_owned(),
        claude_binary: find_claude_desktop_exe()?.map(|path| path.to_string_lossy().into_owned()),
        desktop_supported: cfg!(windows),
    })
}

pub(crate) fn claude_mirror_set_mode(
    db: &Database,
    mode: String,
) -> Result<serde_json::Value, String> {
    let normalized = mode.trim();
    if !matches!(normalized, "token" | "passthrough") {
        return Err("claude_mirror_mode_invalid".into());
    }
    let setting = db
        .get_setting(crate::database::dao::settings::KEY_CLAUDE_CLI_HOME)
        .map_err(String::from)?;
    checked_mirror_directory(setting.as_deref())?;
    db.set_setting(
        crate::database::dao::settings::KEY_CLAUDE_MIRROR_MODE,
        normalized,
    )
    .map_err(String::from)?;
    let result =
        crate::services::provider::claude_mirror::switch_claude_mirror(db, setting.as_deref())
            .map_err(String::from)?;
    Ok(serde_json::json!({ "mode": normalized, "written": result.written }))
}

/// Launch Claude Code with the isolated AAG mirror on every desktop OS.
/// The native `~/.claude` remains untouched.
pub(crate) fn claude_open_cli_terminal(
    db: &Database,
    cwd: Option<String>,
) -> Result<serde_json::Value, String> {
    process::working_directory(cwd.as_deref())?;
    let setting = db
        .get_setting(crate::database::dao::settings::KEY_CLAUDE_CLI_HOME)
        .map_err(String::from)?;
    let mirror = checked_mirror_directory(setting.as_deref())?;
    let binary = discovery::find_tool("claude", &["claude.exe", "claude.cmd"])
        .ok_or_else(|| "claude_cli_not_found".to_string())?;

    // Re-apply the current proxy endpoint and authentication contract just
    // before launch. Existing mirror session files are preserved.
    crate::services::provider::claude_mirror::switch_claude_mirror(db, setting.as_deref())
        .map_err(String::from)?;
    let mut environment = vec![(
        "CLAUDE_CONFIG_DIR".to_string(),
        mirror.to_string_lossy().into_owned(),
    )];
    if let Some(proxy) = db
        .get_setting(crate::database::dao::settings::KEY_OUTBOUND_PROXY)
        .map_err(String::from)?
    {
        for name in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY"] {
            environment.push((name.to_string(), proxy.clone()));
        }
    }
    environment.push((
        "NO_PROXY".to_string(),
        "localhost,127.0.0.1,::1".to_string(),
    ));
    super::terminal::launch_terminal(&binary, &environment, cwd.as_deref())?;
    Ok(serde_json::json!({
        "claudeHome": mirror,
        "claudeBinary": binary.to_string_lossy(),
        "env": super::redacted_environment(&environment),
    }))
}

pub(super) fn desktop_candidates(
    platform: desktop_platform,
    home: &Path,
    local: Option<&Path>,
    program_files: Option<&Path>,
) -> Vec<PathBuf> {
    match platform {
        desktop_platform::macos => [home.join("Applications"), PathBuf::from("/Applications")]
            .into_iter()
            .map(|root| root.join("Claude.app/Contents/MacOS/Claude"))
            .collect(),
        desktop_platform::linux => [
            "/usr/bin/claude-desktop",
            "/usr/local/bin/claude-desktop",
            "/opt/Claude/claude",
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect(),
        desktop_platform::windows => {
            let mut candidates = Vec::new();
            for root in [local, program_files].into_iter().flatten() {
                for stem in ["AnthropicClaude", "Claude"] {
                    candidates.push(root.join(stem).join("claude.exe"));
                }
            }
            candidates
        }
        desktop_platform::unsupported => Vec::new(),
    }
}

fn find_claude_desktop_exe() -> Result<Option<PathBuf>, String> {
    let platform = desktop_platform::current();
    let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    let program_files = std::env::var_os("ProgramFiles").map(PathBuf::from);
    let candidates = desktop_candidates(
        platform,
        &crate::config::get_home_dir(),
        local.as_deref(),
        program_files.as_deref(),
    );
    if let Some(path) = candidates
        .into_iter()
        .find(|path| discovery::executable(path, platform))
    {
        return Ok(Some(path));
    }
    #[cfg(windows)]
    {
        if let Some(local) = &local {
            for stem in ["AnthropicClaude", "Claude"] {
                let entries = match std::fs::read_dir(local.join(stem)) {
                    Ok(entries) => entries,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(_) => return Err("claude_desktop_scan_failed".into()),
                };
                let mut candidates = Vec::new();
                for (index, entry) in entries.enumerate() {
                    if index >= 128 {
                        return Err("claude_desktop_scan_limit".into());
                    }
                    let entry = entry.map_err(|_| "claude_desktop_scan_failed".to_string())?;
                    if entry.file_name().to_string_lossy().starts_with("app-") {
                        let path = entry.path().join("claude.exe");
                        if discovery::executable(&path, platform) {
                            candidates.push(path);
                        }
                    }
                }
                candidates.sort();
                if let Some(path) = candidates.pop() {
                    return Ok(Some(path));
                }
            }
        }
        for pattern in ["Anthropic*", "*Claude*"] {
            let mut command = process::background_command(discovery::powershell()?);
            command.args(["-NoProfile", "-NonInteractive", "-Command", &format!(
                "[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false); Get-AppxPackage {pattern} -ErrorAction Stop | Select-Object -First 1 -ExpandProperty InstallLocation"
            )]);
            let bytes = process::capture_stdout(&mut command, process::PROBE_TIMEOUT)?;
            let root =
                String::from_utf8(bytes).map_err(|_| "claude_desktop_probe_invalid".to_string())?;
            if root.trim().is_empty() {
                continue;
            }
            let root = PathBuf::from(root.trim());
            if !root.is_absolute() {
                return Err("claude_desktop_probe_invalid".into());
            }
            for path in [root.join("claude.exe"), root.join("app/claude.exe")] {
                if discovery::executable(&path, platform) {
                    return Ok(Some(path));
                }
            }
        }
    }
    Ok(None)
}

#[cfg(windows)]
pub(crate) fn claude_open_desktop(db: &Database) -> Result<serde_json::Value, String> {
    let setting = db
        .get_setting(crate::database::dao::settings::KEY_CLAUDE_CLI_HOME)
        .map_err(String::from)?;
    let directory = checked_mirror_directory(setting.as_deref())?;
    let binary =
        find_claude_desktop_exe()?.ok_or_else(|| "claude_desktop_not_found".to_string())?;
    crate::services::provider::claude_mirror::switch_claude_mirror(db, setting.as_deref())
        .map_err(String::from)?;
    let profile = directory.join("electron");
    crate::config::private_fs::prepare_private_dir(&profile).map_err(String::from)?;
    let system_root = std::env::var_os("SystemRoot")
        .ok_or_else(|| "launcher_system_root_unavailable".to_string())?;
    let mut command =
        process::background_command(PathBuf::from(system_root).join("System32/cmd.exe"));
    use std::os::windows::process::CommandExt;
    command
        .args(["/D", "/V:OFF", "/S", "/C"])
        .raw_arg("start \"\" \"%AAG_LAUNCH_PROGRAM%\" --user-data-dir=\"%AAG_LAUNCH_PROFILE%\"")
        .env("CLAUDE_CONFIG_DIR", &directory)
        .env("AAG_LAUNCH_PROGRAM", &binary)
        .env("AAG_LAUNCH_PROFILE", &profile);
    process::capture_stdout(&mut command, process::PROBE_TIMEOUT)?;
    Ok(serde_json::json!({ "claudeHome": directory, "claudeBinary": binary, "mirror": true }))
}

#[cfg(not(windows))]
pub(crate) fn claude_open_desktop(_db: &Database) -> Result<serde_json::Value, String> {
    Err("claude_desktop_platform_unsupported".into())
}
