use super::{discovery::*, process, terminal, vscode, *};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[test]
fn managed_codex_launch_requires_authority_before_cwd_or_mirror_mutation() {
    let fixture = crate::services::provider::test_support::isolated_home("managed-launch-guard");
    let database = Database::memory().unwrap();
    database
        .set_setting("codex_session_authority_enabled", "1")
        .unwrap();
    database
        .set_setting(
            crate::database::dao::settings::KEY_CODEX_CLI_HOME,
            crate::config::get_codex_config_dir().to_str().unwrap(),
        )
        .unwrap();
    assert_eq!(
        codex_open_cli_terminal(
            &database,
            Some(
                fixture
                    .root
                    .join("missing-cwd")
                    .to_string_lossy()
                    .into_owned()
            ),
            None,
            None
        )
        .unwrap_err(),
        "session_client_authority_unavailable"
    );
    assert_eq!(
        codex_open_desktop_window(&database, None, None).unwrap_err(),
        "session_client_authority_unavailable"
    );
    assert!(!fixture.root.join(".codex").exists());
    assert!(!fixture.root.join(".codex-cli").exists());
}

#[test]
fn mirror_session_reset_hint_preserves_history_and_cache_contract() {
    let hints = mirror_audit_hints("empty", false, true, None);
    let hint = hints
        .iter()
        .find(|hint| hint.starts_with("session_cache_present:"))
        .unwrap();
    assert!(hint.contains("clears auth and known global login-state files"));
    assert!(hint.contains("preserving cache, conversation history, and attachments"));
    assert!(mirror_audit_hints("empty", false, false, None)
        .iter()
        .all(|hint| !hint.starts_with("session_cache_present:")));
}

#[test]
fn managed_broker_key_is_redacted_in_launcher_receipts() {
    let environment = vec![
        ("AAG_SESSION_API_KEY".into(), "fixture_active_secret".into()),
        (
            "AAG_SESSION_BROKER_URL".into(),
            "ws://127.0.0.1:43219/aag/codex".into(),
        ),
    ];
    let receipt = serde_json::to_string(&redacted_environment(&environment)).unwrap();
    assert!(!receipt.contains("fixture_active_secret"));
    assert!(receipt.contains("AAG_SESSION_API_KEY"));
    assert!(receipt.contains("[redacted]"));
}

#[cfg(windows)]
#[test]
fn powershell_cli_arguments_arrive_as_literal_values() {
    // Use the documented Node prerequisite as a harmless argv receiver;
    // PowerShell script execution policy must not affect CLI argument tests.
    let program = discovery::find_tool("node", &["node.exe"])
        .expect("Node.js is a documented project prerequisite");
    let arguments = vec![
        "--auto-approve".to_string(),
        "false".into(),
        "literal '$HOME'; $(throw 'must not execute') & | % !".into(),
        "model with spaces".into(),
    ];
    let mut command = process::background_command(discovery::powershell().unwrap());
    let mut invocation_arguments = vec![
        "-e".to_string(),
        "process.stdout.write(JSON.stringify(process.argv.slice(1)))".into(),
        "--".into(),
    ];
    invocation_arguments.extend(arguments.iter().cloned());
    command.args([
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        terminal::WINDOWS_AGENT_SCRIPT,
    ]);
    command
        .env("AAG_LAUNCH_PROGRAM", &program)
        .env(
            "AAG_LAUNCH_ARGUMENTS",
            serde_json::to_string(&invocation_arguments).unwrap(),
        )
        .env_remove("AAG_LAUNCH_CWD");
    let output = process::capture_stdout(&mut command, process::PROBE_TIMEOUT).unwrap();
    let received: Vec<String> = serde_json::from_slice(&output).unwrap();
    assert_eq!(received, arguments);
}
// `Command`, `Duration`, and `Instant` are consumed only by the unix/macos
// process-spawning probes below; importing them unconditionally would warn on
// Windows where those tests are compiled out.
#[cfg(unix)]
use std::{
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
fn codex_mirror_set_mode_never_leaks_internal_error_details_over_ipc() {
    use crate::database::dao::settings::*;
    let directory = process::private_temp_dir().unwrap();
    let database = Database::memory().unwrap();
    // Point the mirror home under a regular file so the mirror write fails
    // with an io error carrying the absolute path and OS error text.
    let blocker = directory.path().join("blocker");
    fs::write(&blocker, b"not a directory").unwrap();
    database
        .set_setting(KEY_CODEX_CLI_HOME, blocker.join("sub").to_str().unwrap())
        .unwrap();
    let error = codex_mirror_set_mode(&database, "coexist".into()).unwrap_err();
    // Every launcher sibling redacts through String::from(AppError) ==
    // external_message(): the stable rule-2 code crosses verbatim; a Display
    // stringification would mangle it into "config error: <code>" (and leak
    // paths for the Io variant).
    assert_eq!(error, "private_storage:non_directory_component", "{error}");
    assert!(!error.contains(directory.path().to_str().unwrap()));
}

#[test]
fn mirror_seed_copies_allowlist_and_preserves_previous_mirror_bytes() {
    let home = crate::services::provider::test_support::isolated_home("instruction-initial-copy");
    let native = home.root.join("native");
    let mirror = home.root.join("mirror");
    fs::create_dir_all(native.join("skills/custom")).unwrap();
    fs::create_dir_all(native.join("agents")).unwrap();
    fs::write(native.join("AGENTS.md"), "# fixture instructions").unwrap();
    fs::write(native.join("skills/custom/skill.toml"), "name='fixture'").unwrap();
    fs::write(native.join("agents/helper.toml"), "fixture").unwrap();
    fs::write(native.join("auth.json"), "fixture secret").unwrap();
    mirror_seed::seed_mirror_from_native(crate::services::mirror_sync::Client::Codex, &native, &mirror).unwrap();
    assert_eq!(
        fs::read_to_string(mirror.join("AGENTS.md")).unwrap(),
        "# fixture instructions"
    );
    assert!(mirror.join("skills/custom/skill.toml").is_file());
    assert!(mirror.join("agents/helper.toml").is_file());
    assert!(!mirror.join("auth.json").exists());
    fs::write(native.join("AGENTS.md"), "# changed").unwrap();
    mirror_seed::seed_mirror_from_native(crate::services::mirror_sync::Client::Codex, &native, &mirror).unwrap();
    assert_eq!(
        fs::read_to_string(mirror.join("AGENTS.md")).unwrap(),
        "# fixture instructions"
    );
    assert!(!mirror.join("AGENTS.md.aag_backup").exists());
    crate::config::private_fs::verify_private_file(&mirror.join("AGENTS.md")).unwrap();
}

#[test]
fn claude_mirror_seed_copies_claude_instruction_roots() {
    let home = crate::services::provider::test_support::isolated_home("claude-instruction-initial-copy");
    let native = home.root.join("native");
    let mirror = home.root.join("mirror");
    fs::create_dir_all(native.join("skills/custom")).unwrap();
    fs::create_dir_all(native.join("commands")).unwrap();
    fs::create_dir_all(native.join("rules")).unwrap();
    fs::write(native.join("CLAUDE.md"), "# claude fixture").unwrap();
    fs::write(native.join("skills/custom/SKILL.md"), "fixture skill").unwrap();
    fs::write(native.join("commands/build.md"), "fixture command").unwrap();
    fs::write(native.join("rules/keep.md"), "fixture rule").unwrap();
    fs::write(native.join("settings.json"), "{\"fixture\":true}").unwrap();
    mirror_seed::seed_mirror_from_native(crate::services::mirror_sync::Client::Claude, &native, &mirror).unwrap();
    assert_eq!(
        fs::read_to_string(mirror.join("CLAUDE.md")).unwrap(),
        "# claude fixture"
    );
    assert!(mirror.join("skills/custom/SKILL.md").is_file());
    assert!(mirror.join("commands/build.md").is_file());
    // `rules` is sync-managed but not part of the bootstrap seed set.
    assert!(!mirror.join("rules").exists());
    assert!(!mirror.join("settings.json").exists());
}

fn seed_fixture_permissions(path: &Path) -> Vec<u8> {
    #[cfg(windows)]
    {
        let mut command = process::background_command(discovery::powershell().unwrap());
        command.args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "(Get-Acl -LiteralPath $env:AAG_TEST_SEED_PATH -ErrorAction Stop).Sddl",
        ]);
        // Windows PowerShell's filesystem provider does not accept Rust's
        // canonical verbatim prefix; remove it only at this test probe boundary.
        let native_path = path.to_string_lossy();
        let shell_path = native_path
            .strip_prefix(r"\\?\UNC\")
            .map(|suffix| format!(r"\\{suffix}"))
            .unwrap_or_else(|| {
                native_path
                    .strip_prefix(r"\\?\")
                    .unwrap_or(&native_path)
                    .to_string()
            });
        command.env("AAG_TEST_SEED_PATH", shell_path);
        // PowerShell 7's module path may select incompatible modules in the
        // Windows PowerShell child used only for this ACL snapshot.
        command.env_remove("PSModulePath");
        let permissions = process::capture_stdout(&mut command, process::PROBE_TIMEOUT).unwrap();
        assert!(!permissions.is_empty());
        permissions
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = fs::metadata(path).unwrap();
        format!("{}:{}:{}", metadata.mode(), metadata.uid(), metadata.gid()).into_bytes()
    }
}

#[test]
fn mirror_seed_preserves_initialized_instruction_roots_without_acl_or_content_changes() {
    let directory = process::private_temp_dir().unwrap();
    let native = directory.path().join("native");
    let mirror = directory.path().join("mirror");
    fs::create_dir_all(native.join("skills/new_skill")).unwrap();
    fs::create_dir_all(native.join("agents")).unwrap();
    fs::write(native.join("AGENTS.md"), "native instructions").unwrap();
    fs::write(native.join("skills/new_skill/SKILL.md"), "new native skill").unwrap();
    fs::write(native.join("agents/new.toml"), "new native agent").unwrap();
    crate::config::private_fs::prepare_managed_private_dir(&mirror).unwrap();
    // Ordinary runtime-created children inherit the private parent's ACL.
    // They are still rejected by AAG's stricter protected-DACL copy boundary.
    fs::create_dir(mirror.join("skills")).unwrap();
    fs::create_dir(mirror.join("agents")).unwrap();
    fs::write(mirror.join("AGENTS.md"), "mirror instructions").unwrap();
    fs::write(mirror.join("skills/keep.md"), "mirror skill").unwrap();
    fs::write(mirror.join("agents/keep.toml"), "mirror agent").unwrap();
    let paths = [
        mirror.join("AGENTS.md"),
        mirror.join("skills"),
        mirror.join("agents"),
    ];
    let permissions: Vec<_> = paths
        .iter()
        .map(|path| seed_fixture_permissions(path))
        .collect();
    mirror_seed::seed_mirror_from_native(crate::services::mirror_sync::Client::Codex, &native, &mirror).unwrap();
    assert_eq!(
        fs::read_to_string(mirror.join("AGENTS.md")).unwrap(),
        "mirror instructions"
    );
    assert_eq!(
        fs::read_to_string(mirror.join("skills/keep.md")).unwrap(),
        "mirror skill"
    );
    assert_eq!(
        fs::read_to_string(mirror.join("agents/keep.toml")).unwrap(),
        "mirror agent"
    );
    assert!(!mirror.join("skills/new_skill").exists());
    assert!(!mirror.join("agents/new.toml").exists());
    assert!(!mirror.join("AGENTS.md.aag_backup").exists());
    for (path, expected) in paths.iter().zip(permissions) {
        assert_eq!(seed_fixture_permissions(path), expected);
    }
}

#[test]
fn mirror_sandbox_sync_preserves_initialized_setup_without_acl_or_content_changes() {
    let directory = process::private_temp_dir().unwrap();
    let native = directory.path().join("native");
    let mirror = directory.path().join("mirror");
    for home in [&native, &mirror] {
        crate::config::private_fs::prepare_managed_private_dir(home).unwrap();
        fs::create_dir(home.join(".sandbox")).unwrap();
        fs::create_dir(home.join(".sandbox-secrets")).unwrap();
    }
    for (name, native_bytes, mirror_bytes) in [
        (
            ".sandbox/setup_marker.json",
            "native marker",
            "mirror marker",
        ),
        (
            ".sandbox-secrets/sandbox_users.json",
            "native users",
            "mirror users",
        ),
    ] {
        fs::write(native.join(name), native_bytes).unwrap();
        fs::write(mirror.join(name), mirror_bytes).unwrap();
    }
    let paths = [
        mirror.join(".sandbox"),
        mirror.join(".sandbox-secrets"),
        mirror.join(".sandbox/setup_marker.json"),
        mirror.join(".sandbox-secrets/sandbox_users.json"),
    ];
    let permissions: Vec<_> = paths
        .iter()
        .map(|path| seed_fixture_permissions(path))
        .collect();
    mirror_seed::sync_mirror_sandbox_credentials(&native, &mirror).unwrap();
    assert_eq!(
        fs::read_to_string(mirror.join(".sandbox/setup_marker.json")).unwrap(),
        "mirror marker"
    );
    assert_eq!(
        fs::read_to_string(mirror.join(".sandbox-secrets/sandbox_users.json")).unwrap(),
        "mirror users"
    );
    assert!(!mirror
        .join(".sandbox/setup_marker.json.aag_backup")
        .exists());
    assert!(!mirror
        .join(".sandbox-secrets/sandbox_users.json.aag_backup")
        .exists());
    for (path, expected) in paths.iter().zip(permissions) {
        assert_eq!(seed_fixture_permissions(path), expected);
    }
}

#[test]
fn mirror_seed_rejects_existing_destination_shapes_before_any_copy() {
    let directory = process::private_temp_dir().unwrap();
    let native = directory.path().join("native");
    fs::create_dir_all(native.join("skills")).unwrap();
    fs::write(native.join("AGENTS.md"), "native instructions").unwrap();
    for name in ["skills", "agents"] {
        let mirror = directory.path().join(format!("invalid-{name}"));
        crate::config::private_fs::prepare_managed_private_dir(&mirror).unwrap();
        fs::write(mirror.join(name), "preserve malformed destination").unwrap();
        assert!(mirror_seed::seed_mirror_from_native(crate::services::mirror_sync::Client::Codex, &native, &mirror).is_err());
        assert_eq!(
            fs::read_to_string(mirror.join(name)).unwrap(),
            "preserve malformed destination"
        );
        assert!(!mirror.join("AGENTS.md").exists());
    }
    let mirror = directory.path().join("invalid-sandbox");
    fs::create_dir_all(mirror.join(".sandbox/setup_marker.json")).unwrap();
    assert!(mirror_seed::sync_mirror_sandbox_credentials(&native, &mirror).is_err());
}

#[test]
fn mirror_seed_preserves_the_clients_system_skill_runtime() {
    let directory = process::private_temp_dir().unwrap();
    let native = directory.path().join("native");
    let mirror = directory.path().join("mirror");
    fs::create_dir_all(native.join("skills/.system")).unwrap();
    fs::create_dir_all(native.join("skills/custom")).unwrap();
    fs::write(native.join("skills/.system/vendor.txt"), "native runtime").unwrap();
    fs::write(native.join("skills/custom/SKILL.md"), "custom instructions").unwrap();
    crate::config::private_fs::prepare_private_dir(&mirror.join("skills")).unwrap();
    fs::create_dir(mirror.join("skills/.system")).unwrap();
    fs::write(mirror.join("skills/.system/vendor.txt"), "mirror runtime").unwrap();
    mirror_seed::seed_mirror_from_native(crate::services::mirror_sync::Client::Codex, &native, &mirror).unwrap();
    assert_eq!(
        fs::read_to_string(mirror.join("skills/.system/vendor.txt")).unwrap(),
        "mirror runtime"
    );
    assert!(!mirror.join("skills/custom/SKILL.md").exists());
}

#[test]
fn mirror_sandbox_sync_requires_native_initialization_and_copies_only_setup_files() {
    let home = crate::services::provider::test_support::isolated_home("sandbox-initial-copy");
    let native = home.root.join("native");
    let mirror = home.root.join("mirror");
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

#[test]
fn mirror_sandbox_sync_refuses_partial_setup_without_overwriting_existing_state() {
    let directory = process::private_temp_dir().unwrap();
    let native = directory.path().join("native");
    fs::create_dir_all(native.join(".sandbox")).unwrap();
    fs::create_dir_all(native.join(".sandbox-secrets")).unwrap();
    fs::write(native.join(".sandbox/setup_marker.json"), "native marker").unwrap();
    fs::write(
        native.join(".sandbox-secrets/sandbox_users.json"),
        "native users",
    )
    .unwrap();
    for (index, name) in [
        ".sandbox/setup_marker.json",
        ".sandbox-secrets/sandbox_users.json",
    ]
    .iter()
    .enumerate()
    {
        let mirror = directory.path().join(format!("partial-{index}"));
        let existing = mirror.join(name);
        crate::config::private_fs::prepare_managed_private_dir(existing.parent().unwrap()).unwrap();
        fs::write(&existing, "preserved partial state").unwrap();
        let permissions = seed_fixture_permissions(existing.parent().unwrap());
        assert_eq!(
            mirror_seed::sync_mirror_sandbox_credentials(&native, &mirror).unwrap_err(),
            "codex_mirror_sandbox_setup_partial"
        );
        assert_eq!(
            fs::read_to_string(&existing).unwrap(),
            "preserved partial state"
        );
        assert_eq!(
            seed_fixture_permissions(existing.parent().unwrap()),
            permissions
        );
        assert!(!mirror.join(format!("{name}.aag_backup")).exists());
        let missing = if index == 0 {
            ".sandbox-secrets/sandbox_users.json"
        } else {
            ".sandbox/setup_marker.json"
        };
        assert!(!mirror.join(missing).exists());
    }
}

#[test]
fn mirror_sandbox_sync_preflights_both_source_files_before_creating_destinations() {
    let directory = process::private_temp_dir().unwrap();
    let native = directory.path().join("native");
    let mirror = directory.path().join("mirror");
    fs::create_dir_all(native.join(".sandbox")).unwrap();
    fs::create_dir_all(native.join(".sandbox-secrets")).unwrap();
    fs::write(native.join(".sandbox/setup_marker.json"), "native marker").unwrap();
    assert_eq!(
        mirror_seed::sync_mirror_sandbox_credentials(&native, &mirror).unwrap_err(),
        "codex_mirror_sandbox_setup_incomplete"
    );
    assert!(!mirror.exists());
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
    mirror_seed::seed_mirror_from_native(crate::services::mirror_sync::Client::Codex, &native, &mirror).unwrap();
    assert!(!mirror.exists());
    fs::remove_file(native.join("AGENTS.md")).unwrap();
    fs::write(native.join("AGENTS.md"), "replacement").unwrap();
    crate::config::private_fs::prepare_private_dir(&mirror).unwrap();
    symlink(external.join("secret"), mirror.join("AGENTS.md")).unwrap();
    assert!(mirror_seed::seed_mirror_from_native(crate::services::mirror_sync::Client::Codex, &native, &mirror).is_err());
    assert_eq!(
        fs::read_to_string(external.join("secret")).unwrap(),
        "untouched"
    );
    fs::remove_file(mirror.join("AGENTS.md")).unwrap();
    for name in ["skills", "agents"] {
        symlink(&external, mirror.join(name)).unwrap();
        assert!(mirror_seed::seed_mirror_from_native(crate::services::mirror_sync::Client::Codex, &native, &mirror).is_err());
        assert!(!mirror.join("AGENTS.md").exists());
        fs::remove_file(mirror.join(name)).unwrap();
    }
}

#[cfg(unix)]
#[test]
fn mirror_seed_keeps_skill_helpers_executable_and_private() {
    use std::os::unix::fs::PermissionsExt;
    let home =
        crate::services::provider::test_support::isolated_home("instruction-executable-copy");
    let native = home.root.join("native");
    let mirror = home.root.join("mirror");
    fs::create_dir_all(native.join("skills/helper")).unwrap();
    write_executable(
        &native.join("skills/helper/run.sh"),
        "#!/bin/sh\nprintf fixture",
    );
    mirror_seed::seed_mirror_from_native(crate::services::mirror_sync::Client::Codex, &native, &mirror).unwrap();
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
    let contents = terminal::unix_script(Path::new("/usr/bin/true"), &[], &[], None).unwrap();
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
        &[],
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
        terminal::launch_terminal(Path::new("unused"), &[], &[], missing.to_str()).unwrap_err(),
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
        codex_open_desktop_window(&Database::memory().unwrap(), None, None).unwrap_err(),
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
    write_executable(&program, "#!/bin/sh\nprintf '%s\\n' \"$AAG_TEST_VALUE\" > \"$AAG_TEST_OUTPUT\"\npwd >> \"$AAG_TEST_OUTPUT\"\nprintf '%s\\n' \"$@\" >> \"$AAG_TEST_OUTPUT\"\n");
    let value = "literal '$HOME'; $(false) & | % !\nsecond line";
    let environment = vec![
        ("AAG_TEST_VALUE".into(), value.into()),
        ("AAG_TEST_OUTPUT".into(), output.to_str().unwrap().into()),
    ];
    let directory = process::private_temp_dir().unwrap();
    let script = directory.path().join("agent.sh");
    let arguments = vec![
        "--auto-approve".into(),
        "false".into(),
        "literal ' $(false); & |".into(),
    ];
    let contents = terminal::unix_script(&program, &arguments, &environment, Some(&cwd)).unwrap();
    crate::config::private_fs::atomic_write(&script, contents.as_bytes()).unwrap();
    crate::config::private_fs::verify_private_file(&script).unwrap();
    let mut command = Command::new("/bin/sh");
    command.arg(&script);
    process::capture_stdout(&mut command, Duration::from_secs(2)).unwrap();
    assert_eq!(
        fs::read_to_string(output).unwrap(),
        format!("{value}\n{}\n{}\n", cwd.display(), arguments.join("\n"))
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
        terminal::unix_script(&program, &[], &[], Some(&fixture.path().join("missing"))).unwrap();
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

#[test]
fn claude_local_install_prefers_the_shim_and_reports_the_newest_squirrel_version() {
    let root = process::private_temp_dir().unwrap();
    let home = root.path().join("home");
    let local = root.path().join("local");
    let anthropic = local.join("AnthropicClaude");
    fs::create_dir_all(&anthropic).unwrap();
    fs::write(anthropic.join("claude.exe"), "fixture shim").unwrap();
    for version in ["app-1.46388.2", "app-1.46388.4"] {
        let directory = anthropic.join(version);
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("claude.exe"), "fixture executable").unwrap();
    }
    let install = super::claude::find_claude_desktop_local_install(
        desktop_platform::windows,
        &home,
        Some(&local),
        None,
    )
    .unwrap();
    assert_eq!(install.exe, anthropic.join("claude.exe"));
    assert_eq!(install.version, Some([1, 46388, 4, 0]));
}

#[test]
fn claude_local_install_falls_back_to_versioned_exe_and_reports_unknown_version() {
    let root = process::private_temp_dir().unwrap();
    let home = root.path().join("home");
    let local = root.path().join("local");
    // No shim at the root: the newest app-* executable is the install.
    let versioned = local.join("Claude/app-1.10.0");
    fs::create_dir_all(&versioned).unwrap();
    fs::write(versioned.join("claude.exe"), "fixture executable").unwrap();
    let install = super::claude::find_claude_desktop_local_install(
        desktop_platform::windows,
        &home,
        Some(&local),
        None,
    )
    .unwrap();
    assert_eq!(install.exe, versioned.join("claude.exe"));
    assert_eq!(install.version, Some([1, 10, 0, 0]));
    // A shim without versioned app directories is the stale leftover a store
    // migration leaves behind: still locatable, but with no version, so any
    // known store install supersedes it.
    let stale = local.join("AnthropicClaude");
    fs::create_dir_all(&stale).unwrap();
    fs::write(stale.join("claude.exe"), "fixture shim").unwrap();
    let install = super::claude::find_claude_desktop_local_install(
        desktop_platform::windows,
        &home,
        Some(&local),
        None,
    )
    .unwrap();
    assert_eq!(install.exe, stale.join("claude.exe"));
    assert_eq!(install.version, None);
    assert!(super::claude::find_claude_desktop_local_install(
        desktop_platform::windows,
        &home,
        None,
        None
    )
    .is_none());
}

#[test]
fn claude_selection_prefers_a_newer_store_but_keeps_newer_or_unknown_local() {
    use super::claude::{claude_install, select_claude_desktop_install};
    let install = |name: &str, version| claude_install {
        exe: PathBuf::from(name),
        version,
    };
    for (local_version, store_version, expected) in [
        (Some([1, 46388, 4, 0]), Some([3, 6556, 2, 0]), "store"),
        (Some([4, 0, 0, 0]), Some([3, 6556, 2, 0]), "local"),
        (Some([3, 6556, 2, 0]), Some([3, 6556, 2, 0]), "local"),
        (None, Some([3, 6556, 2, 0]), "store"),
        (Some([3, 6556, 2, 0]), None, "local"),
        (None, None, "local"),
    ] {
        assert_eq!(
            select_claude_desktop_install(
                Some(install("local", local_version)),
                Ok(Some(install("store", store_version))),
            )
            .unwrap(),
            Some(PathBuf::from(expected)),
        );
    }
    assert_eq!(
        select_claude_desktop_install(None, Ok(Some(install("store", None)))).unwrap(),
        Some(PathBuf::from("store")),
    );
}

#[test]
fn claude_store_query_failure_retains_verified_local_or_preserves_the_error() {
    use super::claude::{claude_install, select_claude_desktop_install};
    let error = "launcher_probe_timeout".to_string();
    assert_eq!(
        select_claude_desktop_install(
            Some(claude_install {
                exe: PathBuf::from("local"),
                version: None
            }),
            Err(error.clone()),
        )
        .unwrap(),
        Some(PathBuf::from("local")),
    );
    assert_eq!(
        select_claude_desktop_install(None, Err(error.clone())),
        Err(error)
    );
    assert_eq!(select_claude_desktop_install(None, Ok(None)).unwrap(), None);
}
#[cfg(windows)]
#[test]
fn windows_cmd_mixed_separator_path_executes_literal_command() {
    let root = std::env::var_os("SystemRoot").unwrap();
    let mut command = process::background_command(PathBuf::from(root).join("System32/cmd.exe"));
    command.args(["/D", "/V:OFF", "/S", "/C", "echo native_path_ok"]);
    let output = process::capture_stdout(&mut command, std::time::Duration::from_secs(3)).unwrap();
    assert_eq!(String::from_utf8(output).unwrap().trim(), "native_path_ok");
}
