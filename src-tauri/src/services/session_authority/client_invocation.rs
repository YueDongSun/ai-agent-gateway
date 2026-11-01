//! Desktop hook with a metadata-only native probe and an authenticated relay.

use crate::{
    config,
    services::owned_process::{encode_environment, run_capture, SpawnSpec},
};
use std::{
    ffi::OsString,
    path::{Component, PathBuf},
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

const CLIENT_SUBDIRECTORY: &str = "clients/desktop";
const MAX_STARTUP_INFO: usize = 32 * 1024;
const MAX_STARTUP_OUTPUT: usize = 256 * 1024;
const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);

enum Invocation {
    Relay,
    Startup(Vec<String>),
    Diagnostic(&'static str),
}

pub(super) fn run(arguments: Vec<OsString>) -> i32 {
    let action = match parse(&arguments) {
        Ok(value) => value,
        Err(code) => {
            eprintln!("{code}");
            return 64;
        }
    };
    if matches!(action, Invocation::Relay) {
        return super::client_bridge::run_stdio();
    }
    match metadata_probe(action) {
        Ok(output) => {
            use std::io::Write;
            if std::io::stdout().write_all(&output).is_err() {
                1
            } else {
                0
            }
        }
        Err(code) => {
            eprintln!("{code}");
            1
        }
    }
}

fn parse(arguments: &[OsString]) -> Result<Invocation, &'static str> {
    if arguments.len() == 2 {
        if arguments[1] == "--version" {
            return Ok(Invocation::Diagnostic("--version"));
        }
        if arguments[1] == "--help" || arguments[1] == "-h" {
            return Ok(Invocation::Diagnostic("--help"));
        }
    }
    let values = arguments
        .iter()
        .skip(1)
        .map(|value| value.to_str().ok_or("session_client_argument_invalid"))
        .collect::<Result<Vec<_>, _>>()?;
    let (mut index, mut app_server, mut startup, mut signout) = (0, false, false, false);
    let mut info = None;
    let mut config = Vec::new();
    while index < values.len() {
        match values[index] {
            "app-server" => {
                if app_server {
                    return Err("session_client_duplicate_app_server");
                }
                app_server = true;
            }
            "-c" | "--config" => {
                index += 1;
                let setting = *values.get(index).ok_or("session_client_config_missing")?;
                if ![
                    "features.code_mode_host=true",
                    "features.code_mode_host=false",
                ]
                .contains(&setting)
                {
                    return Err("session_client_config_override_forbidden");
                }
                // Recognize the Desktop bootstrap flag while keeping the native
                // metadata probe under its forced feature-disable policy.
            }
            "--analytics-default-enabled" => {}
            "--print-startup-config" => {
                if startup {
                    return Err("session_client_duplicate_startup_mode");
                }
                startup = true;
            }
            "--startup-sign-out" => signout = true,
            "--startup-client-info" => {
                index += 1;
                let value = *values
                    .get(index)
                    .ok_or("session_client_startup_info_missing")?;
                if info.is_some()
                    || value.len() > MAX_STARTUP_INFO
                    || !serde_json::from_str::<serde_json::Value>(value)
                        .is_ok_and(|value| value.is_object())
                {
                    return Err("session_client_startup_info_invalid");
                }
                info = Some(value.to_owned());
            }
            _ => return Err("session_client_native_command_forbidden"),
        }
        index += 1;
    }
    if !app_server || (!startup && (signout || info.is_some())) {
        return Err("session_client_startup_mode_invalid");
    }
    if !startup {
        return Ok(Invocation::Relay);
    }
    config.extend(["app-server".into(), "--print-startup-config".into()]);
    if let Some(value) = info {
        config.extend(["--startup-client-info".into(), value]);
    }
    if signout {
        config.push("--startup-sign-out".into());
    }
    Ok(Invocation::Startup(config))
}

fn metadata_home() -> Result<PathBuf, &'static str> {
    let expected = config::get_home_dir()
        .join(".codex-cli")
        .join(CLIENT_SUBDIRECTORY);
    let home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .ok_or("session_client_metadata_home_required")?;
    if !config::same_dir(&home, &expected)
        || home
            .components()
            .any(|part| matches!(part, Component::CurDir | Component::ParentDir))
    {
        return Err("session_client_metadata_home_invalid");
    }
    let marker = config::private_fs::read_mirror_file(&home.join("aag_client_metadata.json"))
        .map_err(|_| "session_client_metadata_marker_unavailable")?;
    let document: serde_json::Value = serde_json::from_slice(
        marker
            .contents
            .as_deref()
            .ok_or("session_client_metadata_marker_required")?,
    )
    .map_err(|_| "session_client_metadata_marker_invalid")?;
    if document
        .get("schema_version")
        .and_then(|value| value.as_u64())
        != Some(1)
        || document
            .get("metadata_only")
            .and_then(|value| value.as_bool())
            != Some(true)
    {
        return Err("session_client_metadata_marker_invalid");
    }
    no_payload(&home)?;
    config::private_fs::prepare_managed_private_dir(&home)
        .map_err(|_| "session_client_metadata_home_unavailable")?;
    Ok(home)
}

fn no_payload(home: &std::path::Path) -> Result<(), &'static str> {
    for name in ["sessions", "archived_sessions"] {
        match std::fs::symlink_metadata(home.join(name)) {
            Ok(_) => return Err("session_client_payload_storage_forbidden"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("session_client_payload_inspection_failed"),
        }
    }
    Ok(())
}

fn metadata_probe(action: Invocation) -> Result<Vec<u8>, &'static str> {
    let home = metadata_home()?;
    let native = std::env::var_os("AAG_NATIVE_CODEX_EXE")
        .map(PathBuf::from)
        .ok_or("session_client_native_executable_required")?;
    let protected = ["ProgramW6432", "ProgramFiles"]
        .iter()
        .filter_map(std::env::var_os)
        .map(PathBuf::from)
        .any(|root| native.starts_with(root));
    if !protected
        || !native.is_absolute()
        || native
            .file_name()
            .is_none_or(|name| !name.to_string_lossy().eq_ignore_ascii_case("codex.exe"))
    {
        return Err("session_client_native_executable_unprotected");
    }
    config::private_fs::validate_external_file_path(&native)
        .map_err(|_| "session_client_native_executable_invalid")?;
    let args = match action {
        Invocation::Startup(args) => args,
        Invocation::Diagnostic(flag) => vec![flag.into()],
        Invocation::Relay => return Err("session_client_internal_mode_invalid"),
    };
    let index = home.join("startup_index");
    let temporary = home.join("temp");
    for directory in [&index, &temporary] {
        config::private_fs::prepare_managed_private_dir(directory)
            .map_err(|_| "session_client_metadata_directory_unavailable")?;
    }
    let encoded_index = serde_json::to_string(
        index
            .to_str()
            .ok_or("session_client_path_encoding_invalid")?,
    )
    .map_err(|_| "session_client_path_encoding_invalid")?;
    let mut forced = vec![
        "-c".into(),
        "history.persistence=\"none\"".into(),
        "-c".into(),
        format!("sqlite_home={encoded_index}"),
    ];
    for feature in [
        "plugins",
        "hooks",
        "remote_plugin",
        "code_mode_host",
        "skill_mcp_dependency_install",
    ] {
        forced.extend(["--disable".into(), feature.into()]);
    }
    forced.extend(args);
    let windows = std::env::var_os("SystemRoot").ok_or("session_client_system_root_unavailable")?;
    let environment = encode_environment(&[
        ("SystemRoot".into(), windows.clone()),
        ("WINDIR".into(), windows),
        ("CODEX_HOME".into(), home.as_os_str().to_owned()),
        ("TEMP".into(), temporary.as_os_str().to_owned()),
        ("TMP".into(), temporary.as_os_str().to_owned()),
    ])
    .map_err(|_| "session_client_metadata_environment_invalid")?;
    let mut spec = SpawnSpec::new(&native, &forced);
    spec.environment = Some(&environment);
    spec.cwd = Some(&home);
    let output = run_capture(
        &spec,
        &AtomicBool::new(false),
        Instant::now() + STARTUP_TIMEOUT,
        MAX_STARTUP_OUTPUT,
    )
    .map_err(|_| "session_client_native_startup_failed")?;
    if output.exit_code != 0 {
        return Err("session_client_native_startup_failed");
    }
    no_payload(&home)?;
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(values: &[&str]) -> Vec<OsString> {
        std::iter::once(OsString::from("shim"))
            .chain(values.iter().map(OsString::from))
            .collect()
    }
    #[test]
    fn desktop_bootstrap_has_no_arbitrary_native_execution_or_home_override() {
        assert!(matches!(
            parse(&args(&[
                "-c",
                "features.code_mode_host=true",
                "app-server",
                "--analytics-default-enabled"
            ])),
            Ok(Invocation::Relay)
        ));
        assert!(matches!(
            parse(&args(&[
                "app-server",
                "--print-startup-config",
                "--startup-client-info",
                "{\"name\":\"fixture\"}"
            ])),
            Ok(Invocation::Startup(_))
        ));
        for values in [
            vec!["exec", "fixture"],
            vec!["app-server", "--listen", "ws://0.0.0.0:1"],
            vec!["-c", "sqlite_home=\"C:\\foreign\"", "app-server"],
            vec!["app-server", "--startup-sign-out"],
        ] {
            assert!(parse(&args(&values)).is_err());
        }
    }
}
