use super::{discovery::*, process, terminal, vscode, *};
use std::{
    fs,
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

#[test]
fn native_status_dtos_match_frontend_camel_case_contracts() {
    let status = CodexCliStatusDto {
        isolation_enabled: true,
        config_dir: "fixture".into(),
        initialized: true,
        codex_cli_home: None,
        outbound_proxy: None,
        codex_binary: None,
        current_provider_id: None,
        current_provider_name: None,
        mirror_mode: "exclusive".into(),
        desktop_supported: cfg!(windows),
    };
    let json = serde_json::to_value(status).unwrap();
    for key in [
        "isolationEnabled",
        "configDir",
        "codexCliHome",
        "outboundProxy",
        "codexBinary",
        "currentProviderId",
        "currentProviderName",
        "mirrorMode",
        "desktopSupported",
    ] {
        assert!(json.get(key).is_some(), "missing frontend field {key}");
    }
    let status = VsCodeExtensionStatusDto {
        binary: None,
        extensions_dir: None,
        claude_code_installed: true,
        kilo_code_installed: false,
    };
    let json = serde_json::to_value(status).unwrap();
    assert_eq!(json["claudeCodeInstalled"], true);
    assert_eq!(json["kiloCodeInstalled"], false);
    assert!(json.get("extensionsDir").is_some());
    let json = serde_json::to_value(ClaudeMirrorStatusDto {
        mirror_dir: "fixture".into(),
        initialized: true,
        base_url_written: true,
        auth_token_written: false,
        mode: "token".into(),
        claude_binary: None,
        desktop_supported: cfg!(windows),
    })
    .unwrap();
    for key in [
        "mirrorDir",
        "baseUrlWritten",
        "authTokenWritten",
        "claudeBinary",
        "desktopSupported",
    ] {
        assert!(json.get(key).is_some(), "missing frontend field {key}");
    }
}

#[test]
fn mirror_modes_keep_upstream_behavior_with_isolated_configuration() {
    use crate::database::dao::settings::*;
    let directory = process::private_temp_dir().unwrap();
    let database = Database::memory().unwrap();
    let codex_dir = directory.path().join("codex");
    let claude_dir = directory.path().join("claude");
    database
        .set_setting(KEY_CODEX_CLI_HOME, codex_dir.to_str().unwrap())
        .unwrap();
    database
        .set_setting(KEY_CLAUDE_CLI_HOME, claude_dir.to_str().unwrap())
        .unwrap();
    assert!(codex_mirror_set_mode(&database, "unknown".into()).is_err());
    assert!(claude_mirror_set_mode(&database, "unknown".into()).is_err());
    assert!(!codex_dir.exists());
    assert!(!claude_dir.exists());
    let result = codex_mirror_set_mode(&database, " coexist ".into()).unwrap();
    assert_eq!(result["mode"], "coexist");
    assert_eq!(codex_cli_status(&database).unwrap().mirror_mode, "coexist");
    assert!(fs::read_to_string(codex_dir.join("config.toml"))
        .unwrap()
        .contains("[profiles.aag]"));
    let result = claude_mirror_set_mode(&database, "passthrough".into()).unwrap();
    assert_eq!(result["mode"], "passthrough");
    let status = claude_mirror_status(&database).unwrap();
    assert!(status.initialized);
    assert!(status.base_url_written);
    assert!(!status.auth_token_written);
    assert_eq!(status.mode, "passthrough");
    fs::write(claude_dir.join("settings.json"), "invalid json").unwrap();
    assert_eq!(
        claude_mirror_status(&database).err().unwrap(),
        "claude_mirror_settings_invalid"
    );
}

#[test]
fn mirror_seed_copies_allowlist_and_preserves_previous_mirror_bytes() {
    let directory = process::private_temp_dir().unwrap();
    let native = directory.path().join("native");
    let mirror = directory.path().join("mirror");
    fs::create_dir_all(native.join("skills/custom")).unwrap();
    fs::create_dir_all(native.join("agents")).unwrap();
    fs::write(native.join("AGENTS.md"), "# fixture instructions").unwrap();
    fs::write(native.join("skills/custom/skill.toml"), "name='fixture'").unwrap();
    fs::write(native.join("agents/helper.toml"), "fixture").unwrap();
    fs::write(native.join("auth.json"), "fixture secret").unwrap();
    mirror_seed::seed_mirror_from_native(&native, &mirror).unwrap();
    assert_eq!(
        fs::read_to_string(mirror.join("AGENTS.md")).unwrap(),
        "# fixture instructions"
    );
    assert!(mirror.join("skills/custom/skill.toml").is_file());
    assert!(mirror.join("agents/helper.toml").is_file());
    assert!(!mirror.join("auth.json").exists());
    fs::write(native.join("AGENTS.md"), "# changed").unwrap();
    mirror_seed::seed_mirror_from_native(&native, &mirror).unwrap();
    assert_eq!(
        fs::read_to_string(mirror.join("AGENTS.md.aag_backup")).unwrap(),
        "# fixture instructions"
    );
    crate::config::private_fs::verify_private_file(&mirror.join("AGENTS.md")).unwrap();
    crate::config::private_fs::verify_private_file(&mirror.join("AGENTS.md.aag_backup")).unwrap();
}

#[test]
fn mirror_sandbox_sync_requires_native_initialization_and_copies_only_setup_files() {
    let directory = process::private_temp_dir().unwrap();
    let native = directory.path().join("native");
    let mirror = directory.path().join("mirror");
    fs::create_dir(&native).unwrap();
    mirror_seed::sync_mirror_sandbox_credentials(&native, &mirror).unwrap();
    assert!(!mirror.exists());
    fs::create_dir(native.join(".sandbox")).unwrap();
    fs::create_dir(native.join(".sandbox-secrets")).unwrap();
    fs::write(native.join(".sandbox/setup_marker.json"), "{\"v\":5}").unwrap();
    fs::write(
        native.join(".sandbox-secrets/sandbox_users.json"),
        "{\"fixture\":true}",
    )
    .unwrap();
    fs::write(native.join(".sandbox/runtime.log"), "fixture runtime").unwrap();
    mirror_seed::sync_mirror_sandbox_credentials(&native, &mirror).unwrap();
    assert_eq!(
        fs::read_to_string(mirror.join(".sandbox/setup_marker.json")).unwrap(),
        "{\"v\":5}"
    );
    assert_eq!(
        fs::read_to_string(mirror.join(".sandbox-secrets/sandbox_users.json")).unwrap(),
        "{\"fixture\":true}"
    );
    assert!(!mirror.join(".sandbox/runtime.log").exists());
    crate::config::private_fs::verify_private_file(
        &mirror.join(".sandbox-secrets/sandbox_users.json"),
    )
    .unwrap();
}

#[cfg(unix)]
#[test]
fn mirror_seed_skips_source_links_and_refuses_destination_links() {
    use std::os::unix::fs::symlink;
    let directory = process::private_temp_dir().unwrap();
    let native = directory.path().join("native");
    let mirror = directory.path().join("mirror");
    let external = directory.path().join("external");
    fs::create_dir(&native).unwrap();
    fs::create_dir(&external).unwrap();
    fs::write(external.join("secret"), "untouched").unwrap();
    symlink(&external, native.join("skills")).unwrap();
    symlink(external.join("secret"), native.join("AGENTS.md")).unwrap();
    mirror_seed::seed_mirror_from_native(&native, &mirror).unwrap();
    assert!(!mirror.exists());
    fs::remove_file(native.join("AGENTS.md")).unwrap();
    fs::write(native.join("AGENTS.md"), "replacement").unwrap();
    crate::config::private_fs::prepare_private_dir(&mirror).unwrap();
    symlink(external.join("secret"), mirror.join("AGENTS.md")).unwrap();
    assert!(mirror_seed::seed_mirror_from_native(&native, &mirror).is_err());
    assert_eq!(
        fs::read_to_string(external.join("secret")).unwrap(),
        "untouched"
    );
}

#[cfg(unix)]
#[test]
fn mirror_seed_keeps_skill_helpers_executable_and_private() {
    use std::os::unix::fs::PermissionsExt;
    let directory = process::private_temp_dir().unwrap();
    let native = directory.path().join("native");
    let mirror = directory.path().join("mirror");
    fs::create_dir_all(native.join("skills/helper")).unwrap();
    write_executable(
        &native.join("skills/helper/run.sh"),
        "#!/bin/sh\nprintf fixture",
    );
    mirror_seed::seed_mirror_from_native(&native, &mirror).unwrap();
    let helper = mirror.join("skills/helper/run.sh");
    assert_eq!(
        fs::metadata(&helper).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        process::capture_stdout(&mut Command::new(helper), Duration::from_secs(2)).unwrap(),
        b"fixture"
    );
}

#[test]
fn macos_terminal_dispatch_uses_launch_services_with_literal_path() {
    let script = Path::new("/fixture/space and 'quote'/agent.command");
    let command = terminal::macos_command(script);
    assert_eq!(command.get_program(), "/usr/bin/open");
    assert_eq!(
        command.get_args().collect::<Vec<_>>(),
        ["-b", "com.apple.Terminal", script.to_str().unwrap()]
    );
}

#[cfg(unix)]
#[test]
fn executable_terminal_document_is_private_and_self_removes() {
    use std::os::unix::fs::PermissionsExt;
    let directory = process::private_temp_dir().unwrap();
    let contents = terminal::unix_script(Path::new("/usr/bin/true"), &[], None).unwrap();
    let script = terminal::write_script(directory.path(), &contents).unwrap();
    assert_eq!(
        fs::metadata(&script).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(script.extension().unwrap(), "command");
    process::capture_stdout(&mut Command::new(&script), Duration::from_secs(2)).unwrap();
    assert!(!directory.path().exists());
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "opens a native Terminal fixture window; requires an interactive desktop"]
fn macos_terminal_native_fixture_receives_environment_and_cleans_up() {
    let fixture = process::private_temp_dir().unwrap();
    let program = fixture.path().join("fixture_agent");
    let receipt = fixture.path().join("receipt");
    write_executable(
        &program,
        "#!/bin/sh\nprintf '%s' \"$AAG_FIXTURE_VALUE\" > \"$AAG_FIXTURE_RECEIPT\"\n",
    );
    let value = "native fixture '$HOME'; $(false)";
    terminal::launch_terminal(
        &program,
        &[
            ("AAG_FIXTURE_VALUE".into(), value.into()),
            (
                "AAG_FIXTURE_RECEIPT".into(),
                receipt.to_str().unwrap().into(),
            ),
        ],
        fixture.path().to_str(),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while !receipt.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        fs::read_to_string(receipt).expect("Terminal fixture did not execute"),
        value
    );
}

#[test]
fn default_cli_home_is_isolated_and_native_aliases_are_rejected() {
    let database = Database::memory().unwrap();
    let status = codex_cli_status(&database).unwrap();
    assert!(status.isolation_enabled);
    assert_eq!(
        Path::new(&status.config_dir),
        crate::config::get_home_dir().join(".codex-cli")
    );
    let native = crate::config::get_codex_config_dir();
    for alias in [native.clone(), native.join(".")] {
        assert_eq!(
            checked_mirror_directory(alias.to_str()).unwrap_err(),
            "codex_mirror_native_directory_forbidden"
        );
    }
}

#[test]
fn executable_discovery_rejects_directories_relative_paths_and_non_executable_files() {
    let directory = process::private_temp_dir().unwrap();
    fs::create_dir(directory.path().join("codex")).unwrap();
    let directories = vec![directory.path().to_owned()];
    assert!(find_in_directories(&["codex"], &directories, desktop_platform::current()).is_none());
    fs::write(directory.path().join("codex.cmd"), "fixture").unwrap();
    assert!(find_in_directories(&["codex.cmd"], &directories, desktop_platform::windows).is_some());
    assert!(find_in_directories(
        &["codex.cmd"],
        &[Path::new(".").to_owned()],
        desktop_platform::windows
    )
    .is_none());
    #[cfg(unix)]
    assert!(find_in_directories(&["codex.cmd"], &directories, desktop_platform::macos).is_none());
}

#[test]
fn installation_candidates_cover_three_platforms_and_match_editor_extensions() {
    let root = process::private_temp_dir().unwrap();
    let home = root.path().join("home");
    let local = root.path().join("local");
    let programs = root.path().join("program files");
    let windows = editor_candidates(
        desktop_platform::windows,
        &home,
        Some(&local),
        Some(&programs),
    );
    assert!(windows.contains(&(
        local.join("Programs/Microsoft VS Code/Code.exe"),
        editor_kind::stable
    )));
    assert!(windows.contains(&(
        programs.join("Microsoft VS Code Insiders/Code - Insiders.exe"),
        editor_kind::insiders
    )));
    let macos = editor_candidates(desktop_platform::macos, &home, None, None);
    assert!(macos.contains(&(
        home.join("Applications/Visual Studio Code.app/Contents/Resources/app/bin/code"),
        editor_kind::stable
    )));
    let linux = editor_candidates(desktop_platform::linux, &home, None, None);
    assert!(linux.contains(&(Path::new("/snap/bin/code").to_owned(), editor_kind::stable)));
    assert_eq!(
        editor_kind::cursor.extension_directory(&home),
        home.join(".cursor/extensions")
    );
    assert_eq!(
        editor_kind::insiders.extension_directory(&home),
        home.join(".vscode-insiders/extensions")
    );
    assert!(!vscode::extension_matches(
        "anthropic.claude-code-malicious",
        "anthropic.claude"
    ));
    assert!(vscode::extension_matches(
        "anthropic.claude-code-2.1.0",
        "anthropic.claude-code"
    ));
}

#[test]
fn linux_terminal_arguments_preserve_a_single_path_argument() {
    let script = Path::new("/fixture/space and 'quote'/agent.sh");
    for (terminal, expected) in [
        ("gnome-terminal", "--"),
        ("xfce4-terminal", "--execute"),
        ("konsole", "-e"),
        ("x-terminal-emulator", "-e"),
    ] {
        let arguments = terminal::linux_arguments(terminal, script);
        assert_eq!(
            arguments,
            [expected, "/bin/sh", script.to_str().unwrap()].map(std::ffi::OsString::from)
        );
    }
}

#[test]
fn invalid_environment_and_working_directory_are_rejected_before_launch() {
    for name in ["", "A=B", "A;touch", "2BAD"] {
        assert_eq!(
            process::validate_environment(&[(name.into(), "fixture".into())]).unwrap_err(),
            "launcher_environment_invalid"
        );
    }
    assert!(process::validate_environment(&[("VALID_2".into(), "a\0b".into())]).is_err());
    assert!(process::working_directory(Some("relative/path")).is_err());
    assert!(process::working_directory(Some(" ")).unwrap().is_none());
    let directory = process::private_temp_dir().unwrap();
    let missing = directory.path().join("missing");
    assert_eq!(
        terminal::launch_terminal(Path::new("unused"), &[], missing.to_str()).unwrap_err(),
        "launcher_working_directory_invalid"
    );
}

#[test]
fn editor_profiles_change_when_injected_environment_changes() {
    let editor = editor_installation {
        binary: Path::new("/fixture/code").to_owned(),
        kind: editor_kind::stable,
    };
    let first = vec![
        ("AUTH".into(), "first".into()),
        ("ENDPOINT".into(), "local".into()),
    ];
    let reordered = vec![first[1].clone(), first[0].clone()];
    let second = vec![("AUTH".into(), "second".into()), first[1].clone()];
    let profile = vscode::profile_directory(&editor, "claude_vscode", &first);
    assert_eq!(
        profile,
        vscode::profile_directory(&editor, "claude_vscode", &reordered)
    );
    assert_ne!(
        profile,
        vscode::profile_directory(&editor, "claude_vscode", &second)
    );
    assert_ne!(
        profile,
        vscode::profile_directory(&editor, "kilo_vscode", &first)
    );
    assert!(!profile.to_string_lossy().contains("first"));
    let redacted = serde_json::to_string(&redacted_environment(&first)).unwrap();
    assert!(!redacted.contains("first"));
}

#[cfg(not(windows))]
#[test]
fn unsupported_desktop_probe_does_not_report_false_success() {
    assert_eq!(
        mirror_chatgpt_process_count(Path::new("/fixture/mirror")).unwrap_err(),
        "codex_desktop_platform_unsupported"
    );
    assert_eq!(
        codex_open_desktop_window(&Database::memory().unwrap()).unwrap_err(),
        "codex_desktop_platform_unsupported"
    );
}

#[cfg(unix)]
fn write_executable(path: &Path, contents: &str) {
    use std::os::unix::fs::PermissionsExt;
    fs::write(path, contents).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

#[cfg(unix)]
#[test]
fn terminal_script_preserves_literal_values_cwd_and_removes_credential_artifact() {
    let fixture = process::private_temp_dir().unwrap();
    let program = fixture.path().join("agent ' with spaces.sh");
    let output = fixture.path().join("result");
    let cwd = fixture.path().join("work ' directory");
    fs::create_dir(&cwd).unwrap();
    write_executable(&program, "#!/bin/sh\nprintf '%s\\n' \"$AAG_TEST_VALUE\" > \"$AAG_TEST_OUTPUT\"\npwd >> \"$AAG_TEST_OUTPUT\"\n");
    let value = "literal '$HOME'; $(false) & | % !\nsecond line";
    let environment = vec![
        ("AAG_TEST_VALUE".into(), value.into()),
        ("AAG_TEST_OUTPUT".into(), output.to_str().unwrap().into()),
    ];
    let directory = process::private_temp_dir().unwrap();
    let script = directory.path().join("agent.sh");
    let contents = terminal::unix_script(&program, &environment, Some(&cwd)).unwrap();
    crate::config::private_fs::atomic_write(&script, contents.as_bytes()).unwrap();
    crate::config::private_fs::verify_private_file(&script).unwrap();
    let mut command = Command::new("/bin/sh");
    command.arg(&script);
    process::capture_stdout(&mut command, Duration::from_secs(2)).unwrap();
    assert_eq!(
        fs::read_to_string(output).unwrap(),
        format!("{value}\n{}\n", cwd.display())
    );
    assert!(!script.exists());
    assert!(!directory.path().exists());
}

#[cfg(unix)]
#[test]
fn invalid_cwd_at_execution_does_not_run_agent() {
    let fixture = process::private_temp_dir().unwrap();
    let marker = fixture.path().join("executed");
    let program = fixture.path().join("agent");
    write_executable(
        &program,
        &format!(
            "#!/bin/sh\ntouch {}\n",
            terminal::shell_quote(marker.to_str().unwrap())
        ),
    );
    let directory = process::private_temp_dir().unwrap();
    let script = directory.path().join("agent.sh");
    let contents =
        terminal::unix_script(&program, &[], Some(&fixture.path().join("missing"))).unwrap();
    fs::write(&script, contents).unwrap();
    let mut command = Command::new("/bin/sh");
    command.arg(script);
    assert_eq!(
        process::capture_stdout(&mut command, Duration::from_secs(2)).unwrap_err(),
        "launcher_probe_failed"
    );
    assert!(!marker.exists());
}

#[cfg(unix)]
#[test]
fn bounded_probe_times_out_and_reaps_child() {
    let fixture = process::private_temp_dir().unwrap();
    let pid_file = fixture.path().join("pid");
    let mut command = Command::new("/bin/sh");
    command
        .args(["-c", "echo $$ > \"$1\"; exec /bin/sleep 10", "fixture"])
        .arg(&pid_file);
    let started = Instant::now();
    assert_eq!(
        process::capture_stdout(&mut command, Duration::from_millis(150)).unwrap_err(),
        "launcher_probe_timeout"
    );
    assert!(started.elapsed() < Duration::from_secs(2));
    let pid = fs::read_to_string(pid_file).unwrap();
    let mut probe = Command::new("/bin/kill");
    probe.args(["-0", pid.trim()]);
    assert_eq!(
        process::capture_stdout(&mut probe, Duration::from_secs(2)).unwrap_err(),
        "launcher_probe_failed"
    );
}

#[cfg(unix)]
#[test]
fn probes_bound_output_and_do_not_surface_child_error_text() {
    let mut command = Command::new("/usr/bin/head");
    command.args(["-c", "1048577", "/dev/zero"]);
    assert_eq!(
        process::capture_stdout(&mut command, Duration::from_secs(2)).unwrap_err(),
        "launcher_probe_output_limit"
    );
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "echo fixture_secret; echo fixture_secret >&2; exit 7"]);
    assert_eq!(
        process::capture_stdout(&mut command, Duration::from_secs(2)).unwrap_err(),
        "launcher_probe_failed"
    );
}

#[cfg(unix)]
#[test]
fn editor_child_receives_profile_arguments_and_environment_without_shell_interpolation() {
    let fixture = process::private_temp_dir().unwrap();
    let binary = fixture.path().join("editor with spaces");
    write_executable(
        &binary,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" \"$ANTHROPIC_AUTH_TOKEN\"\n",
    );
    let editor = editor_installation {
        binary,
        kind: editor_kind::stable,
    };
    let profile = fixture.path().join("profile with ' quote");
    let secret = "fixture; $(false) ' \" % &";
    let mut command = vscode::editor_command(
        &editor,
        &profile,
        &[("ANTHROPIC_AUTH_TOKEN".into(), secret.into())],
    )
    .unwrap();
    let result = process::capture_stdout(&mut command, Duration::from_secs(2)).unwrap();
    let text = String::from_utf8(result).unwrap();
    assert!(text.starts_with(&format!(
        "--new-window\n--user-data-dir\n{}\n",
        profile.display()
    )));
    assert!(text.ends_with(&format!("{secret}\n")));
}
