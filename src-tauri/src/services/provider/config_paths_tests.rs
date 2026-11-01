use super::*;
use std::collections::HashMap;

struct Fixture {
    _temporary: tempfile::TempDir,
    home: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        // macOS temporary roots may be spelled through /var; the application
        // rejects linked ancestors, so fixture ownership starts at the real root.
        let home = temporary
            .path()
            .canonicalize()
            .unwrap()
            .join("fixture home");
        fs::create_dir(&home).unwrap();
        crate::config::private_fs::prepare_managed_private_dir(&home).unwrap();
        Self {
            _temporary: temporary,
            home,
        }
    }

    fn file(&self, relative: &str) -> PathBuf {
        let path = self.home.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"fixture contents").unwrap();
        path
    }

    /// A path that still contains a literal `..` component. `Fixture::new`
    /// canonicalizes its root, which on Windows yields a verbatim
    /// (`\\?\C:\...`) path; joining or pushing `".."` onto a verbatim base
    /// eagerly normalizes the traversal away, so a traversal input must be
    /// built on a non-verbatim base. `std::env::temp_dir()` is non-verbatim,
    /// so build there — the value never needs to exist for the guard to
    /// reject it.
    fn traversal(tail: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push("aag-parent-traversal-fixture");
        path.push(Component::ParentDir.as_os_str());
        path.push(tail);
        assert!(
            path.components().any(|c| matches!(c, Component::ParentDir)),
            "traversal fixture must retain a ParentDir component: {path:?}"
        );
        path
    }

    fn resolve(
        &self,
        app_type: &str,
        isolated: bool,
        environment: &[(&str, PathBuf)],
    ) -> Result<Resolved_config_paths, AppError> {
        let variables: HashMap<_, _> = environment
            .iter()
            .map(|(name, path)| (*name, path.as_os_str().to_os_string()))
            .collect();
        resolve_with_environment(app_type, &self.home, isolated, |name| {
            variables.get(name).cloned()
        })
    }
}

#[test]
fn default_resolution_is_read_only_and_uses_cli_paths_on_every_host() {
    let fixture = Fixture::new();
    for (app_type, expected) in [
        ("copilot", vec![".copilot/settings.json"]),
        (
            "pi",
            vec![".pi/agent/models.json", ".pi/agent/settings.json"],
        ),
        ("gemini", vec![".gemini/.env", ".gemini/settings.json"]),
        ("opencode", vec![".config/opencode/opencode.json"]),
        ("kilo_cli", vec![".config/kilo/kilo.jsonc"]),
        ("kilo_vscode", vec![".config/kilo/kilo.jsonc"]),
        (
            "cline_cli",
            vec![
                ".cline_cli/data/settings/providers.json",
                ".cline_cli/data/settings/models.json",
            ],
        ),
    ] {
        let resolved = fixture.resolve(app_type, false, &[]).unwrap();
        assert_eq!(
            resolved.paths,
            expected
                .iter()
                .map(|path| fixture.home.join(path))
                .collect::<Vec<_>>()
        );
        assert_eq!(resolved.source, "default");
        assert!(!resolved.existing);
    }
    assert_eq!(fs::read_dir(&fixture.home).unwrap().count(), 0);
}

#[test]
fn explicit_directory_overrides_follow_each_clients_home_contract() {
    let fixture = Fixture::new();
    let custom = fixture.home.join("custom directory 空间");
    for (app_type, variable, expected) in [
        ("copilot", "COPILOT_HOME", vec!["settings.json"]),
        (
            "pi",
            "PI_CODING_AGENT_DIR",
            vec!["models.json", "settings.json"],
        ),
        (
            "gemini",
            "GEMINI_CLI_HOME",
            vec![".gemini/.env", ".gemini/settings.json"],
        ),
        ("opencode", "OPENCODE_CONFIG_DIR", vec!["opencode.json"]),
        ("kilo_cli", "KILO_CONFIG_DIR", vec!["kilo.jsonc"]),
    ] {
        let resolved = fixture
            .resolve(app_type, false, &[(variable, custom.clone())])
            .unwrap();
        assert_eq!(
            resolved.paths,
            expected
                .iter()
                .map(|path| custom.join(path))
                .collect::<Vec<_>>()
        );
        assert_eq!(resolved.source, "environment");
        assert!(!resolved.existing);
    }
    assert!(!custom.exists());
}

#[test]
fn explicit_file_overrides_do_not_fall_back_to_existing_global_files() {
    let fixture = Fixture::new();
    fixture.file(".config/opencode/opencode.json");
    fixture.file(".config/kilo/kilo.jsonc");
    for (app_type, variable) in [("opencode", "OPENCODE_CONFIG"), ("kilo_cli", "KILO_CONFIG")] {
        let custom = fixture.home.join(format!("custom/{app_type}.jsonc"));
        let resolved = fixture
            .resolve(app_type, false, &[(variable, custom.clone())])
            .unwrap();
        assert_eq!(resolved.paths, vec![custom]);
        assert_eq!(resolved.source, "environment");
        assert!(!resolved.existing);
    }
}

#[test]
fn custom_directory_layer_takes_precedence_over_custom_file_layer() {
    let fixture = Fixture::new();
    for (app_type, file_variable, directory_variable, existing) in [
        (
            "opencode",
            "OPENCODE_CONFIG",
            "OPENCODE_CONFIG_DIR",
            "opencode.jsonc",
        ),
        ("kilo_cli", "KILO_CONFIG", "KILO_CONFIG_DIR", "kilo.json"),
    ] {
        let custom_directory = fixture.home.join(app_type);
        let target = fixture.file(&format!("{app_type}/{existing}"));
        let custom_file = fixture.file(&format!("{app_type}/explicit.json"));
        let resolved = fixture
            .resolve(
                app_type,
                false,
                &[
                    (file_variable, custom_file),
                    (directory_variable, custom_directory),
                ],
            )
            .unwrap();
        assert_eq!(resolved.paths, vec![target]);
        assert_eq!(resolved.source, "environment");
        assert!(resolved.existing);
    }
}

#[test]
fn existing_candidates_are_reused_without_creating_another_config() {
    for (app_type, relative) in [
        ("copilot", ".copilot/settings.json"),
        ("pi", ".pi/agent/settings.json"),
        ("gemini", ".gemini/settings.json"),
        ("opencode", ".config/opencode/opencode.jsonc"),
        ("opencode", ".config/opencode/opencode.json"),
        ("kilo_cli", ".config/kilo/kilo.jsonc"),
        ("kilo_cli", ".config/kilo/kilo.json"),
        ("kilo_cli", ".config/kilo/opencode.jsonc"),
        ("kilo_cli", ".config/kilo/opencode.json"),
        ("kilo_cli", ".kilo/config.json"),
        ("kilo_vscode", ".kilo/config.json"),
    ] {
        let fixture = Fixture::new();
        let target = fixture.file(relative);
        let resolved = fixture.resolve(app_type, false, &[]).unwrap();
        assert!(resolved.paths.contains(&target));
        assert!(resolved.existing);
        assert_eq!(resolved.source, "existing");
        assert_eq!(fs::read(target).unwrap(), b"fixture contents");
    }
}

#[test]
fn xdg_root_is_used_instead_of_platform_application_data_or_stale_default() {
    let fixture = Fixture::new();
    fixture.file(".config/opencode/opencode.jsonc");
    fixture.file(".kilo/config.json");
    let xdg = fixture.home.join("custom xdg");
    for (app_type, expected) in [
        ("opencode", "opencode/opencode.json"),
        ("kilo_cli", "kilo/kilo.jsonc"),
    ] {
        let resolved = fixture
            .resolve(
                app_type,
                false,
                &[
                    ("XDG_CONFIG_HOME", xdg.clone()),
                    ("APPDATA", fixture.home.join("irrelevant application data")),
                ],
            )
            .unwrap();
        assert_eq!(resolved.paths, vec![xdg.join(expected)]);
        assert_eq!(resolved.source, "environment");
        assert!(!resolved.existing);
    }
}

#[test]
fn portable_home_ignores_external_overrides_and_accepts_contained_fixture_paths() {
    let fixture = Fixture::new();
    let outside = fixture.home.parent().unwrap().join("external home");
    let inside = fixture.home.join("client home");
    for (app_type, variable) in [
        ("copilot", "COPILOT_HOME"),
        ("pi", "PI_CODING_AGENT_DIR"),
        ("gemini", "GEMINI_CLI_HOME"),
        ("opencode", "OPENCODE_CONFIG_DIR"),
        ("kilo_cli", "KILO_CONFIG_DIR"),
        ("opencode", "OPENCODE_CONFIG"),
        ("kilo_cli", "KILO_CONFIG"),
        ("opencode", "XDG_CONFIG_HOME"),
        ("kilo_cli", "XDG_CONFIG_HOME"),
    ] {
        let resolved = fixture
            .resolve(app_type, true, &[(variable, outside.clone())])
            .unwrap();
        assert_eq!(resolved.source, "default");
        assert!(resolved
            .paths
            .iter()
            .all(|path| path.starts_with(&fixture.home)));

        let contained = fixture
            .resolve(app_type, true, &[(variable, inside.clone())])
            .unwrap();
        assert_eq!(contained.source, "environment");
        assert!(contained.paths.iter().all(|path| path.starts_with(&inside)));
    }
    assert!(!outside.exists());
    assert!(!inside.exists());
}

#[test]
fn path_failures_convert_to_stable_ipc_codes() {
    for (code, expected) in [
        ("not_absolute", "AAG_CLIENT_CONFIG_PATH_NOT_ABSOLUTE"),
        (
            "parent_traversal",
            "AAG_CLIENT_CONFIG_PATH_PARENT_TRAVERSAL",
        ),
        (
            "control_character",
            "AAG_CLIENT_CONFIG_PATH_CONTROL_CHARACTER",
        ),
        (
            "metadata_unavailable",
            "AAG_CLIENT_CONFIG_PATH_METADATA_UNAVAILABLE",
        ),
        ("unsupported_app", "AAG_CLIENT_CONFIG_PATH_UNSUPPORTED_APP"),
    ] {
        assert_eq!(failure(code).external_message(), expected, "{code}");
        assert_eq!(String::from(failure(code)), expected, "{code}");
    }

    let fixture = Fixture::new();
    assert_eq!(
        String::from(fixture.resolve("unsupported", false, &[]).unwrap_err()),
        "AAG_CLIENT_CONFIG_PATH_UNSUPPORTED_APP"
    );
}

#[test]
fn invalid_explicit_paths_fail_without_silent_fallback() {
    let fixture = Fixture::new();
    for (app_type, variable) in [
        ("copilot", "COPILOT_HOME"),
        ("pi", "PI_CODING_AGENT_DIR"),
        ("gemini", "GEMINI_CLI_HOME"),
        ("opencode", "OPENCODE_CONFIG"),
        ("opencode", "OPENCODE_CONFIG_DIR"),
        ("kilo_cli", "KILO_CONFIG"),
        ("kilo_cli", "KILO_CONFIG_DIR"),
        ("opencode", "XDG_CONFIG_HOME"),
    ] {
        for (invalid, expected) in [
            (
                PathBuf::from("relative"),
                "AAG_CLIENT_CONFIG_PATH_NOT_ABSOLUTE",
            ),
            (
                Fixture::traversal("escape"),
                "AAG_CLIENT_CONFIG_PATH_PARENT_TRAVERSAL",
            ),
            (
                fixture.home.join("newline\npath"),
                "AAG_CLIENT_CONFIG_PATH_CONTROL_CHARACTER",
            ),
        ] {
            let outcome = fixture.resolve(app_type, false, &[(variable, invalid.clone())]);
            let error = outcome.unwrap_err();
            assert_eq!(
                String::from(error),
                expected,
                "{app_type}/{variable} invalid={invalid:?}"
            );
        }
    }
    assert_eq!(fs::read_dir(&fixture.home).unwrap().count(), 0);
}

#[test]
fn existing_non_file_target_and_non_directory_parent_are_rejected() {
    let fixture = Fixture::new();
    let directory = fixture.home.join(".config/opencode/opencode.jsonc");
    fs::create_dir_all(&directory).unwrap();
    assert!(fixture.resolve("opencode", false, &[]).is_err());
    let file = fixture.file("not a directory");
    assert!(fixture
        .resolve("copilot", false, &[("COPILOT_HOME", file)])
        .is_err());
}

#[test]
fn read_only_file_validation_rejects_invalid_spelling_and_directories() {
    let fixture = Fixture::new();
    for invalid in [
        PathBuf::from("relative.json"),
        Fixture::traversal("target.json"),
        fixture.home.join("control\r.json"),
        fixture.home.clone(),
    ] {
        let outcome = private_fs::validate_external_file_path(&invalid);
        assert!(
            outcome.is_err(),
            "expected {invalid:?} to fail, got {outcome:?}"
        );
    }
    let absent = fixture.home.join("absent/config.json");
    private_fs::validate_external_file_path(&absent).unwrap();
    assert!(!absent.parent().unwrap().exists());
}

#[cfg(unix)]
#[test]
fn linked_parent_is_rejected_before_read_or_permission_changes() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let fixture = Fixture::new();
    let target = fixture.file("actual/settings.json");
    fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
    let linked = fixture.home.join("linked");
    symlink(target.parent().unwrap(), &linked).unwrap();
    assert!(fixture
        .resolve("copilot", false, &[("COPILOT_HOME", linked.clone())])
        .is_err());
    assert!(private_fs::atomic_write(&linked.join("settings.json"), b"changed").is_err());
    assert_eq!(fs::read(&target).unwrap(), b"fixture contents");
    assert_eq!(
        fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o644
    );
}

#[cfg(unix)]
#[test]
fn linked_and_dangling_file_candidates_fail_closed() {
    use std::os::unix::fs::symlink;
    for dangling in [false, true] {
        let fixture = Fixture::new();
        let target = if dangling {
            fixture.home.join("missing.json")
        } else {
            fixture.file("actual.json")
        };
        let linked = fixture.home.join(".config/opencode/opencode.jsonc");
        fs::create_dir_all(linked.parent().unwrap()).unwrap();
        symlink(target, &linked).unwrap();
        assert!(fixture.resolve("opencode", false, &[]).is_err());
    }
}

#[cfg(windows)]
#[test]
fn isolated_containment_folds_path_case_on_windows() {
    let fixture = Fixture::new();
    for (label, inside) in [
        (
            "uppercase home",
            PathBuf::from(fixture.home.to_string_lossy().to_uppercase()),
        ),
        (
            "lowercase home",
            PathBuf::from(fixture.home.to_string_lossy().to_lowercase()),
        ),
    ] {
        let inside = inside.join("client home");
        let resolved = fixture
            .resolve("copilot", true, &[("COPILOT_HOME", inside.clone())])
            .expect("case-spelled contained override must be accepted");
        assert_eq!(resolved.source, "environment", "{label}");
        assert!(
            resolved.paths.iter().all(|path| path.starts_with(&inside)),
            "{label}"
        );
    }
}
