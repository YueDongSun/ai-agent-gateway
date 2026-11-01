//! Terminal adapters. Shell source and native arguments have separate escaping.

use super::{discovery, process};
use std::path::Path;

#[cfg(windows)]
pub(super) const WINDOWS_AGENT_SCRIPT: &str = "if ($env:AAG_LAUNCH_CWD) { Set-Location -LiteralPath $env:AAG_LAUNCH_CWD -ErrorAction Stop }; $launch_arguments = [string[]](ConvertFrom-Json -InputObject $env:AAG_LAUNCH_ARGUMENTS); & $env:AAG_LAUNCH_PROGRAM @launch_arguments";

#[cfg(unix)]
pub(super) fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

#[cfg(unix)]
pub(super) fn unix_script(
    program: &Path,
    arguments: &[String],
    environment: &[(String, String)],
    cwd: Option<&Path>,
) -> Result<String, String> {
    process::validate_environment(environment)?;
    let program = program
        .to_str()
        .ok_or_else(|| "launcher_path_encoding_invalid".to_string())?;
    let mut lines = vec![
        "#!/bin/sh".to_string(),
        // The interpreter already holds the file open. Remove credentials
        // immediately, including when the agent is long-lived or interrupted.
        "rm -f -- \"$0\"".to_string(),
        "rmdir -- \"$(dirname -- \"$0\")\" 2>/dev/null || :".to_string(),
    ];
    if let Some(cwd) = cwd {
        let cwd = cwd
            .to_str()
            .ok_or_else(|| "launcher_path_encoding_invalid".to_string())?;
        lines.push(format!("cd {} || exit 1", shell_quote(cwd)));
    }
    for (name, value) in environment {
        lines.push(format!("export {name}={}", shell_quote(value)));
    }
    let mut invocation = format!("exec {}", shell_quote(program));
    for argument in arguments {
        invocation.push(' ');
        invocation.push_str(&shell_quote(argument));
    }
    lines.push(invocation);
    Ok(lines.join("\n") + "\n")
}

#[cfg(any(target_os = "linux", test))]
pub(super) fn linux_arguments(terminal: &str, script: &Path) -> Vec<std::ffi::OsString> {
    let separator = match terminal {
        "gnome-terminal" => "--",
        "xfce4-terminal" => "--execute",
        _ => "-e", // x-terminal-emulator (Debian), Konsole, xterm.
    };
    vec![
        separator.into(),
        "/bin/sh".into(),
        script.as_os_str().to_owned(),
    ]
}

#[cfg(any(target_os = "macos", test))]
pub(super) fn macos_command(script: &Path) -> std::process::Command {
    let mut command = process::background_command("/usr/bin/open");
    // Launch Services opens the executable document; no AppleEvents
    // automation permission or user shell startup script is required.
    command.args(["-b", "com.apple.Terminal"]).arg(script);
    command
}

#[cfg(unix)]
pub(super) fn write_script(directory: &Path, contents: &str) -> Result<std::path::PathBuf, String> {
    use std::os::unix::fs::PermissionsExt;
    let script = directory.join("agent.command");
    crate::config::private_fs::atomic_write(&script, contents.as_bytes()).map_err(String::from)?;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700))
        .map_err(|_| "launcher_script_permissions_failed".to_string())?;
    Ok(script)
}

#[cfg(unix)]
fn retain_until_started(
    directory: crate::config::private_fs::PrivateTempDir,
) -> Result<(), String> {
    std::thread::Builder::new()
        .name("aag_terminal_cleanup".into())
        .spawn(move || {
            // Terminal services may accept a request then fail to execute it.
            // Expire that private credential artifact even in this failure case.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
            while directory.path().join("agent.command").exists()
                && std::time::Instant::now() < deadline
            {
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
            drop(directory);
        })
        .map_err(|_| "launcher_cleanup_worker_failed".to_string())?;
    Ok(())
}

pub(super) fn launch_terminal(
    program: &Path,
    arguments: &[String],
    environment: &[(String, String)],
    cwd: Option<&str>,
) -> Result<(), String> {
    let cwd = process::working_directory(cwd)?;
    process::validate_environment(environment)?;
    if arguments.iter().any(|argument| argument.contains('\0')) {
        return Err("launcher_argument_invalid".into());
    }
    if !discovery::executable(program, discovery::desktop_platform::current()) {
        return Err("launcher_executable_invalid".into());
    }
    let mut environment = environment.to_vec();
    environment.push((
        "PATH".into(),
        discovery::child_path(program)?
            .into_string()
            .map_err(|_| "launcher_path_encoding_invalid".to_string())?,
    ));

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // cmd/start gives the new console its own I/O even when the Tauri
        // process was launched with redirected handles. All variable values
        // travel through the environment, never interpolated source text.
        let system_root = std::env::var_os("SystemRoot")
            .ok_or_else(|| "launcher_system_root_unavailable".to_string())?;
        let mut command = process::background_command(
            std::path::PathBuf::from(system_root).join("System32/cmd.exe"),
        );
        command.args(["/D", "/V:OFF", "/S", "/C"]).raw_arg(format!(
            "start \"\" \"%AAG_LAUNCH_POWERSHELL%\" -NoLogo -NoProfile -NoExit -Command \"{WINDOWS_AGENT_SCRIPT}\""));
        command.envs(environment.iter().map(|(key, value)| (key, value)));
        command
            .env("AAG_LAUNCH_POWERSHELL", discovery::powershell()?)
            .env("AAG_LAUNCH_PROGRAM", program)
            .env(
                "AAG_LAUNCH_ARGUMENTS",
                serde_json::to_string(arguments)
                    .map_err(|_| "launcher_argument_invalid".to_string())?,
            )
            .env_remove("AAG_LAUNCH_CWD");
        if let Some(cwd) = &cwd {
            command.current_dir(cwd).env("AAG_LAUNCH_CWD", cwd);
        }
        process::capture_stdout(&mut command, process::PROBE_TIMEOUT)?;
        return Ok(());
    }

    #[cfg(unix)]
    {
        let directory = process::private_temp_dir()?;
        let content = unix_script(program, arguments, &environment, cwd.as_deref())?;
        let script = write_script(directory.path(), &content)?;
        #[cfg(target_os = "macos")]
        {
            let mut command = macos_command(&script);
            process::capture_stdout(&mut command, process::PROBE_TIMEOUT)?;
            return retain_until_started(directory);
        }
        #[cfg(target_os = "linux")]
        {
            let mut found_terminal = false;
            for terminal in [
                "x-terminal-emulator",
                "gnome-terminal",
                "konsole",
                "xfce4-terminal",
                "xterm",
            ] {
                let Some(binary) = discovery::find_tool(terminal, &[]) else {
                    continue;
                };
                found_terminal = true;
                let mut command = process::background_command(binary);
                command.args(linux_arguments(terminal, &script));
                if process::launch(&mut command).is_ok() {
                    return retain_until_started(directory);
                }
            }
            return Err(if found_terminal {
                "terminal_launch_failed"
            } else {
                "terminal_not_found"
            }
            .into());
        }
    }
    #[allow(unreachable_code)]
    Err("terminal_platform_unsupported".into())
}
