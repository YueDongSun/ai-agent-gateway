//! Claude mirror status and launch orchestration. Native configuration is read-only.

use super::discovery::{self, desktop_platform};
use super::process;
use crate::database::Database;
use serde::Serialize;
use std::path::{Path, PathBuf};

/// Upper bound for scanning one Claude Desktop Squirrel root. An oversized
/// root is skipped in full so the selection never depends on filesystem
/// enumeration order.
const CLAUDE_DESKTOP_SCAN_LIMIT: usize = 128;

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
        // Locating the desktop binary is best-effort metadata: a failed
        // directory scan must not fail the whole status. The lookup stays
        // local-only because this DTO is polled on a timer; the PowerShell
        // store probe runs only in the launch path.
        claude_binary: find_claude_desktop_exe_local()
            .map(|path| path.to_string_lossy().into_owned()),
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
    // Bootstrap the mirror's instruction roots on first launch when
    // continuous sync is opted out; sync itself owns them otherwise.
    if !crate::services::mirror_sync::instruction_sync_enabled(db).map_err(String::from)? {
        super::mirror_seed::seed_mirror_from_native(
            crate::services::mirror_sync::Client::Claude,
            &crate::config::get_claude_config_dir(),
            &mirror,
        )?;
    }
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
    super::terminal::launch_terminal(&binary, &[], &environment, cwd.as_deref())?;
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

/// A located Claude Desktop install plus its version when one can be read.
#[allow(non_camel_case_types)]
pub(super) struct claude_install {
    pub(super) exe: PathBuf,
    pub(super) version: Option<[u64; 4]>,
}

/// Locate the local (non-store) Claude Desktop install: the launcher shim in
/// a Squirrel root first, then the newest versioned `app-*` executable. The
/// version comes from the newest `app-*` directory so a store install can be
/// compared against it; a shim without versioned directories reports `None`.
pub(super) fn find_claude_desktop_local_install(
    platform: desktop_platform,
    home: &Path,
    local: Option<&Path>,
    program_files: Option<&Path>,
) -> Option<claude_install> {
    let candidates = desktop_candidates(platform, home, local, program_files);
    if let Some(path) = candidates
        .into_iter()
        .find(|path| discovery::executable(path, platform))
    {
        let version = (platform == desktop_platform::windows)
            .then(|| {
                path.parent()
                    .and_then(|root| claude_squirrel_version(root, platform))
            })
            .flatten();
        return Some(claude_install { exe: path, version });
    }
    if platform != desktop_platform::windows {
        return None;
    }
    let local = local?;
    for stem in ["AnthropicClaude", "Claude"] {
        // Squirrel keeps previous versions beside the newest one, so
        // the pick is numeric: app-1.10.0 must win over app-1.9.0.
        if let Some(path) = discovery::newest_versioned_executable(
            &local.join(stem),
            "app-",
            "claude.exe",
            platform,
            CLAUDE_DESKTOP_SCAN_LIMIT,
        ) {
            let version = path
                .parent()
                .and_then(|directory| directory.file_name())
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_prefix("app-"))
                .and_then(discovery::padded_version);
            return Some(claude_install { exe: path, version });
        }
    }
    None
}

/// Version of the newest `app-*` directory below a Squirrel install root.
fn claude_squirrel_version(root: &Path, platform: desktop_platform) -> Option<[u64; 4]> {
    let newest = discovery::newest_versioned_executable(
        root,
        "app-",
        "claude.exe",
        platform,
        CLAUDE_DESKTOP_SCAN_LIMIT,
    )?;
    newest
        .parent()
        .and_then(|directory| directory.file_name())
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix("app-"))
        .and_then(discovery::padded_version)
}

/// Locate a Microsoft Store (Appx) Claude Desktop install. When both a store
/// and a local install exist, the newer version wins, so a stale Squirrel
/// shim left behind by a store migration is no longer launched.
#[cfg(windows)]
fn find_claude_desktop_store_install(
    platform: desktop_platform,
) -> Result<Option<claude_install>, String> {
    debug_assert_eq!(platform, desktop_platform::windows);
    // Store identity and executable come from the registered app manifest;
    // wildcard companions must not hide or replace the desktop package.
    let packages = discovery::query_appx_packages("Claude")?;
    Ok(discovery::select_appx_desktop_executable(
        &packages,
        "Claude",
        &["app/Claude.exe", "Claude.exe"],
    )
    .map(|(exe, version)| claude_install { exe, version }))
}

/// Store discovery is best effort when a verified local desktop is available.
/// Keep a failed probe distinguishable from an absent install if no fallback
/// exists. Unknown Store versions can still launch on Store-only machines.
#[cfg(any(windows, test))]
pub(super) fn select_claude_desktop_install(
    local: Option<claude_install>,
    store: Result<Option<claude_install>, String>,
) -> Result<Option<PathBuf>, String> {
    match (local, store) {
        (Some(local), Ok(Some(store))) => {
            if discovery::version_supersedes(store.version, local.version) {
                Ok(Some(store.exe))
            } else {
                Ok(Some(local.exe))
            }
        }
        (Some(local), _) => Ok(Some(local.exe)),
        (None, store) => store.map(|install| install.map(|install| install.exe)),
    }
}

fn find_claude_desktop_exe() -> Result<Option<PathBuf>, String> {
    let platform = desktop_platform::current();
    let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    let program_files = std::env::var_os("ProgramFiles").map(PathBuf::from);
    let local_install = find_claude_desktop_local_install(
        platform,
        &crate::config::get_home_dir(),
        local.as_deref(),
        program_files.as_deref(),
    );
    #[cfg(windows)]
    {
        select_claude_desktop_install(local_install, find_claude_desktop_store_install(platform))
    }
    #[cfg(not(windows))]
    {
        Ok(local_install.map(|install| install.exe))
    }
}

/// Local-install-only lookup for the status DTO: the UI refreshes mirror
/// status on a timer, so the PowerShell store probe stays reserved for the
/// explicit launch action. Store-only machines report no binary here,
/// matching the Codex mirror status behavior.
fn find_claude_desktop_exe_local() -> Option<PathBuf> {
    let platform = desktop_platform::current();
    let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    let program_files = std::env::var_os("ProgramFiles").map(PathBuf::from);
    find_claude_desktop_local_install(
        platform,
        &crate::config::get_home_dir(),
        local.as_deref(),
        program_files.as_deref(),
    )
    .map(|install| install.exe)
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
    let desktop = crate::services::provider::claude_desktop::write_gateway_profile(db, &directory)
        .map_err(String::from)?;
    let profile = desktop.user_data;
    super::desktop_reload::prepare_reload(
        &binary,
        &profile,
        &[
            profile.join("claude_desktop_config.json"),
            profile.join("configLibrary/_meta.json"),
            profile.join("configLibrary/00000000-0000-4000-8000-000000008989.json"),
        ],
    )?;
    let system_root = std::env::var_os("SystemRoot")
        .ok_or_else(|| "launcher_system_root_unavailable".to_string())?;
    let mut command =
        process::background_command(PathBuf::from(system_root).join("System32/cmd.exe"));
    use std::os::windows::process::CommandExt;
    command
        .args(["/D", "/V:OFF", "/S", "/C"])
        .raw_arg("start \"\" \"%AAG_LAUNCH_PROGRAM%\" --user-data-dir=\"%AAG_LAUNCH_PROFILE%\"")
        .env("CLAUDE_CONFIG_DIR", &directory)
        .env("LOCALAPPDATA", &desktop.local_app_data)
        .env("AAG_LAUNCH_PROGRAM", &binary)
        .env("AAG_LAUNCH_PROFILE", &profile);
    // GUI activation needs no output capture; the launcher reaps its shell.
    process::launch(&mut command)?;
    Ok(
        serde_json::json!({ "claudeHome": directory, "claudeBinary": binary, "desktopProfile": profile, "gatewayMode": true, "mirror": true }),
    )
}

#[cfg(not(windows))]
pub(crate) fn claude_open_desktop(_db: &Database) -> Result<serde_json::Value, String> {
    Err("claude_desktop_platform_unsupported".into())
}
