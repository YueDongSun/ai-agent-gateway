use super::*;
use std::{collections::BTreeMap, fs};

fn fixture() -> crate::config::private_fs::PrivateTempDir {
    super::super::process::private_temp_dir().unwrap()
}

#[test]
fn windows_cursor_roaming_installation_uses_the_gui_executable_without_path() {
    let root = fixture();
    let home = make_directory(root.path(), "home");
    let roaming = make_directory(root.path(), "roaming app data");
    let bin = make_directory(&roaming, "Cursor/bin");
    fs::write(bin.join("cursor.cmd"), "fixture shim").unwrap();
    let binary = roaming.join("Cursor/Cursor.exe");
    fs::write(&binary, "fixture GUI executable").unwrap();

    let selected = editor_candidates(desktop_platform::windows, &home, None, None)
        .into_iter()
        .chain(roaming_cursor_candidate(
            desktop_platform::windows,
            Some(&roaming),
        ))
        .find(|(path, _)| executable(path, desktop_platform::windows));
    assert_eq!(selected, Some((binary, editor_kind::cursor)));
    assert!(roaming_cursor_candidate(desktop_platform::macos, Some(&roaming)).is_none());
    assert!(roaming_cursor_candidate(desktop_platform::windows, None).is_none());
    assert!(
        roaming_cursor_candidate(desktop_platform::windows, Some(Path::new("relative"))).is_none()
    );
}

fn make_directory(root: &Path, suffix: &str) -> PathBuf {
    let directory = root.join(suffix);
    fs::create_dir_all(&directory).unwrap();
    directory
}

#[test]
fn gui_discovery_covers_user_node_installations_without_shell_path() {
    let root = fixture();
    let home = root.path();
    let expected: Vec<_> = [
        ".nvm/versions/node/v22.10.0/bin",
        ".config/nvm/versions/node/v20.8.0/bin",
        "Library/Application Support/fnm/node-versions/v22.9.0/installation/bin",
        ".local/share/fnm/node-versions/v20.8.0/installation/bin",
        ".asdf/shims",
        ".asdf/installs/nodejs/22.8.0/bin",
        ".local/share/mise/shims",
        ".local/share/mise/installs/node/22.7.0/bin",
    ]
    .iter()
    .map(|suffix| make_directory(home, suffix))
    .collect();
    let directories = search_directories_for(desktop_platform::macos, home, |_| None);
    for expected in expected {
        assert!(
            directories.contains(&expected),
            "missing {}",
            expected.display()
        );
    }
}

#[test]
fn configured_manager_roots_and_homebrew_kegs_are_discovered() {
    let root = fixture();
    let home = make_directory(root.path(), "home");
    let overrides = BTreeMap::from([
        ("NVM_DIR", root.path().join("custom nvm")),
        ("FNM_DIR", root.path().join("custom fnm")),
        ("ASDF_DATA_DIR", root.path().join("custom asdf")),
        ("MISE_DATA_DIR", root.path().join("custom mise")),
        ("HOMEBREW_PREFIX", root.path().join("custom brew")),
    ]);
    let expected = [
        make_directory(&overrides["NVM_DIR"], "versions/node/v22.1.0/bin"),
        make_directory(
            &overrides["FNM_DIR"],
            "node-versions/v22.1.0/installation/bin",
        ),
        make_directory(&overrides["ASDF_DATA_DIR"], "installs/nodejs/22.1.0/bin"),
        make_directory(&overrides["MISE_DATA_DIR"], "installs/node/22.1.0/bin"),
        make_directory(&overrides["HOMEBREW_PREFIX"], "opt/node@22/bin"),
        make_directory(&overrides["HOMEBREW_PREFIX"], "opt/node/bin"),
    ];
    let directories = search_directories_for(desktop_platform::macos, &home, |name| {
        overrides.get(name).map(|path| path.as_os_str().to_owned())
    });
    for expected in expected {
        assert!(
            directories.contains(&expected),
            "missing {}",
            expected.display()
        );
    }
}

#[test]
fn xdg_and_mise_install_and_shim_overrides_are_honored() {
    let root = fixture();
    let home = make_directory(root.path(), "home");
    let overrides = BTreeMap::from([
        ("XDG_CONFIG_HOME", root.path().join("config")),
        ("XDG_DATA_HOME", root.path().join("data")),
        ("MISE_INSTALLS_DIR", root.path().join("installs")),
        ("MISE_SHIMS_DIR", root.path().join("shims")),
    ]);
    let expected = [
        make_directory(
            &overrides["XDG_CONFIG_HOME"],
            "nvm/versions/node/v22.1.0/bin",
        ),
        make_directory(
            &overrides["XDG_DATA_HOME"],
            "fnm/node-versions/v22.1.0/installation/bin",
        ),
        make_directory(&overrides["MISE_INSTALLS_DIR"], "node/22.1.0/bin"),
        make_directory(&overrides["MISE_SHIMS_DIR"], ""),
    ];
    let directories = search_directories_for(desktop_platform::macos, &home, |name| {
        overrides.get(name).map(|path| path.as_os_str().to_owned())
    });
    for expected in expected {
        assert!(directories.contains(&expected));
    }
}

#[test]
fn version_discovery_is_numeric_deterministic_and_stable_only() {
    let root = fixture();
    for version in [
        "v9.9.9",
        "v22.9.0",
        "v22.10.0",
        "v22.10.0-rc.1",
        "latest",
        "v999999999999999999999",
    ] {
        make_directory(root.path(), &format!("{version}/bin"));
    }
    assert_eq!(
        version_directories(root.path(), "", "bin"),
        ["v22.10.0/bin", "v22.9.0/bin", "v9.9.9/bin"].map(|suffix| root.path().join(suffix))
    );
    for version in [
        "node@9",
        "node@22",
        "node@23.1",
        "node@24-preview",
        "python@99",
    ] {
        make_directory(root.path(), &format!("{version}/bin"));
    }
    assert_eq!(
        version_directories(root.path(), "node@", "bin"),
        ["node@23.1/bin", "node@22/bin", "node@9/bin"].map(|suffix| root.path().join(suffix))
    );
}

#[test]
fn version_scans_have_explicit_directory_and_result_limits() {
    let root = fixture();
    for version in 0..(MAX_VERSIONS_PER_ROOT + 4) {
        make_directory(root.path(), &format!("v22.0.{version}/bin"));
    }
    let versions = version_directories(root.path(), "", "bin");
    assert_eq!(versions.len(), MAX_VERSIONS_PER_ROOT);
    assert_eq!(versions[0], root.path().join("v22.0.19/bin"));
    for version in (MAX_VERSIONS_PER_ROOT + 4)..=MAX_INSTALLATION_ENTRIES {
        make_directory(root.path(), &format!("v22.0.{version}/bin"));
    }
    assert!(version_directories(root.path(), "", "bin").is_empty());
}

#[test]
fn homebrew_node_kegs_survive_a_large_unrelated_formula_catalog() {
    let root = fixture();
    for formula in 0..(MAX_INSTALLATION_ENTRIES + 8) {
        make_directory(root.path(), &format!("unrelated_{formula}"));
    }
    let node = make_directory(root.path(), "node@22/bin");
    assert_eq!(version_directories(root.path(), "node@", "bin"), [node]);
}

#[test]
fn inherited_path_order_and_fallback_capacity_are_preserved() {
    let root = fixture();
    let home = make_directory(root.path(), "home");
    let inherited: Vec<_> = (0..MAX_INHERITED_PATH_DIRECTORIES)
        .map(|index| make_directory(root.path(), &format!("path_{index}")))
        .collect();
    let path = std::env::join_paths(&inherited).unwrap();
    let fallback = make_directory(&home, ".nvm/versions/node/v22.1.0/bin");
    let directories = search_directories_for(desktop_platform::macos, &home, |name| {
        (name == "PATH").then(|| path.clone())
    });
    assert_eq!(&directories[..inherited.len()], inherited);
    assert!(directories.contains(&fallback));
    assert!(directories.len() <= MAX_PATH_DIRECTORIES);
}

#[test]
fn relative_overrides_are_ignored_and_repeated_paths_are_deduplicated() {
    let root = fixture();
    let home = make_directory(root.path(), "home");
    let bin = make_directory(root.path(), "bin");
    let fallback = make_directory(&home, ".nvm/versions/node/v22.1.0/bin");
    let path = std::env::join_paths([Path::new("."), &bin, &bin]).unwrap();
    let directories = search_directories_for(desktop_platform::macos, &home, |name| {
        Some(if name == "PATH" {
            path.clone()
        } else {
            "relative".into()
        })
    });
    assert_eq!(directories.first(), Some(&bin));
    assert_eq!(directories.iter().filter(|value| *value == &bin).count(), 1);
    assert!(directories.iter().all(|path| path.is_absolute()));
    assert!(directories.contains(&fallback));
}

#[cfg(unix)]
fn write_executable(path: &Path, content: &str) {
    use std::os::unix::fs::PermissionsExt;
    fs::write(path, content).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

#[cfg(unix)]
#[test]
fn inherited_cli_wins_over_newer_fallback_installations() {
    let root = fixture();
    let home = make_directory(root.path(), "home");
    let inherited = make_directory(root.path(), "preferred bin");
    let fallback = make_directory(&home, ".nvm/versions/node/v22.1.0/bin");
    for directory in [&inherited, &fallback] {
        write_executable(&directory.join("codex"), "#!/bin/sh\nexit 0\n");
    }
    let directories = search_directories_for(desktop_platform::macos, &home, |name| {
        (name == "PATH").then(|| inherited.as_os_str().to_owned())
    });
    assert_eq!(
        find_in_directories(&["codex"], &directories, desktop_platform::macos),
        Some(inherited.join("codex"))
    );
}

#[cfg(unix)]
#[test]
fn gui_cli_uses_its_own_node_with_an_env_shebang() {
    let root = fixture();
    let home = make_directory(root.path(), "home with spaces");
    let older = make_directory(&home, ".nvm/versions/node/v20.8.0/bin");
    let newer = make_directory(&home, ".nvm/versions/node/v22.10.0/bin");
    let program = older.join("fixture_cli");
    write_executable(&program, "#!/usr/bin/env node\n// fixture only\n");
    write_executable(
        &older.join("node"),
        "#!/bin/sh\nprintf 'matched runtime\\n%s\\n' \"$1\"\n",
    );
    write_executable(
        &newer.join("node"),
        "#!/bin/sh\nprintf 'wrong runtime\\n'\nexit 3\n",
    );
    let directories = search_directories_for(desktop_platform::macos, &home, |name| {
        (name == "PATH").then(|| OsString::from("/usr/bin:/bin"))
    });
    assert!(
        directories.iter().position(|path| path == &newer).unwrap()
            < directories.iter().position(|path| path == &older).unwrap()
    );
    let selected =
        find_in_directories(&["fixture_cli"], &directories, desktop_platform::macos).unwrap();
    assert_eq!(selected, program);
    let mut command = std::process::Command::new(&selected);
    command.env_clear().env(
        "PATH",
        child_path_for(&selected, directories, desktop_platform::macos).unwrap(),
    );
    let output =
        super::super::process::capture_stdout(&mut command, std::time::Duration::from_secs(2))
            .unwrap();
    assert_eq!(
        String::from_utf8(output).unwrap(),
        format!("matched runtime\n{}\n", program.display())
    );
}

#[cfg(unix)]
#[test]
fn linked_npm_cli_keeps_the_runtime_of_its_resolved_installation() {
    let root = fixture();
    let older = make_directory(root.path(), "nvm/v20.8.0");
    let package = make_directory(&older, "lib/node_modules/fixture/bin");
    let older_bin = make_directory(&older, "bin");
    let newer_bin = make_directory(root.path(), "nvm/v22.10.0/bin");
    let link_bin = make_directory(root.path(), "local/bin");
    let target = package.join("fixture.js");
    write_executable(&target, "#!/usr/bin/env node\n// fixture only\n");
    write_executable(
        &older_bin.join("node"),
        "#!/bin/sh\nprintf 'linked runtime\\n'\n",
    );
    write_executable(&newer_bin.join("node"), "#!/bin/sh\nexit 3\n");
    let link = link_bin.join("fixture_cli");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let mut command = std::process::Command::new(&link);
    command.env_clear().env(
        "PATH",
        child_path_for(&link, vec![newer_bin, older_bin], desktop_platform::macos).unwrap(),
    );
    let output =
        super::super::process::capture_stdout(&mut command, std::time::Duration::from_secs(2))
            .unwrap();
    assert_eq!(output, b"linked runtime\n");
}

#[cfg(unix)]
#[test]
fn manager_shims_can_resolve_their_manager_with_a_gui_path() {
    let root = fixture();
    let home = make_directory(root.path(), "home");
    let asdf_bin = make_directory(&home, ".asdf/bin");
    let asdf_shims = make_directory(&home, ".asdf/shims");
    let mise_bin = make_directory(&home, ".local/bin");
    let mise_shims = make_directory(&home, ".local/share/mise/shims");
    write_executable(
        &asdf_bin.join("asdf"),
        "#!/bin/sh\nprintf 'asdf manager:%s:%s\\n' \"$1\" \"$2\"\n",
    );
    write_executable(
        &asdf_shims.join("fixture_asdf_cli"),
        "#!/bin/sh\nexec asdf exec fixture_asdf_cli\n",
    );
    write_executable(
        &mise_bin.join("mise"),
        "#!/bin/sh\nprintf 'mise manager\\n'\n",
    );
    std::os::unix::fs::symlink(mise_bin.join("mise"), mise_shims.join("fixture_mise_cli")).unwrap();
    let directories = search_directories_for(desktop_platform::macos, &home, |name| {
        (name == "PATH").then(|| OsString::from("/usr/bin:/bin"))
    });
    for (name, expected) in [
        ("fixture_asdf_cli", "asdf manager:exec:fixture_asdf_cli\n"),
        ("fixture_mise_cli", "mise manager\n"),
    ] {
        let selected = find_in_directories(&[name], &directories, desktop_platform::macos).unwrap();
        let mut command = std::process::Command::new(&selected);
        command.env_clear().env(
            "PATH",
            child_path_for(&selected, directories.clone(), desktop_platform::macos).unwrap(),
        );
        let output =
            super::super::process::capture_stdout(&mut command, std::time::Duration::from_secs(2))
                .unwrap();
        assert_eq!(String::from_utf8(output).unwrap(), expected);
    }
}

#[test]
fn newest_versioned_executable_prefers_the_numerically_newest_directory() {
    let root = fixture();
    for version in ["app-1.9.0", "app-1.10.0", "app-0.2.0"] {
        let directory = make_directory(root.path(), version);
        fs::write(directory.join("claude.exe"), "fixture executable").unwrap();
    }
    assert_eq!(
        newest_versioned_executable(
            root.path(),
            "app-",
            "claude.exe",
            desktop_platform::windows,
            128,
        ),
        Some(root.path().join("app-1.10.0").join("claude.exe"))
    );
}

#[test]
fn newest_versioned_executable_falls_back_to_lexicographic_only_for_unparsed_names() {
    let root = fixture();
    for name in ["build_abc", "build_xyz", "build_mno"] {
        let directory = make_directory(root.path(), name);
        fs::write(directory.join("codex.exe"), "fixture executable").unwrap();
    }
    assert_eq!(
        newest_versioned_executable(root.path(), "", "codex.exe", desktop_platform::windows, 128,),
        Some(root.path().join("build_xyz").join("codex.exe"))
    );
    // A parseable version always wins over unparsed names regardless of
    // lexicographic order.
    let versioned = make_directory(root.path(), "0.9.0");
    fs::write(versioned.join("codex.exe"), "fixture executable").unwrap();
    assert_eq!(
        newest_versioned_executable(root.path(), "", "codex.exe", desktop_platform::windows, 128,),
        Some(versioned.join("codex.exe"))
    );
}

#[test]
fn newest_versioned_executable_ignores_oversized_unreadable_and_relative_roots() {
    let root = fixture();
    let oversized = make_directory(root.path(), "oversized");
    for index in 0..4 {
        let directory = make_directory(&oversized, &format!("app-1.0.{index}"));
        fs::write(directory.join("claude.exe"), "fixture executable").unwrap();
    }
    assert_eq!(
        newest_versioned_executable(
            &oversized,
            "app-",
            "claude.exe",
            desktop_platform::windows,
            3,
        ),
        None
    );
    assert_eq!(
        newest_versioned_executable(
            &oversized.join("missing"),
            "app-",
            "claude.exe",
            desktop_platform::windows,
            3,
        ),
        None
    );
    assert_eq!(
        newest_versioned_executable(
            Path::new("relative/root"),
            "app-",
            "claude.exe",
            desktop_platform::windows,
            3,
        ),
        None
    );
}

fn appx_fixture(name: &str, version: &str, install_dir: &str) -> appx_package {
    appx_package {
        name: name.to_string(),
        version: padded_version(version),
        install_dir: PathBuf::from(install_dir),
    }
}

#[test]
fn parse_appx_packages_keeps_only_well_formed_absolute_rows() {
    let fixture = super::super::process::private_temp_dir().unwrap();
    let codex = fixture.path().join("OpenAI.Codex");
    let claude = fixture.path().join("Claude");
    let output = format!(
        "OpenAI.Codex|26.1016.8622.0|{}\n  Claude|3.6556.2.0|{}\r\nbroken-line-without-fields\n|1.0.0|{}\nNoName|1.0.0|relative/path\nEmptyLocation|1.0.0|",
        codex.display(), claude.display(), fixture.path().display()
    );
    let packages = parse_appx_packages(&output);
    assert_eq!(packages.len(), 2);
    assert_eq!(packages[0].name, "OpenAI.Codex");
    assert_eq!(packages[0].version, Some([26, 1016, 8622, 0]));
    assert_eq!(packages[1].name, "Claude");
    assert_eq!(packages[1].version, Some([3, 6556, 2, 0]));
    // An unparsable version keeps the row usable but ranks it lowest.
    let unparsable = parse_appx_packages(&format!("Odd|pre-release|{}", fixture.path().display()));
    assert_eq!(unparsable.len(), 1);
    assert_eq!(unparsable[0].version, None);
}

#[test]
fn select_appx_package_prefers_exact_name_over_newer_companions() {
    let runtime = appx_fixture(
        "OpenAI.CodexPrimaryRuntime",
        "27.0.0.0",
        "C:\\store\\runtime",
    );
    let app_old = appx_fixture("OpenAI.Codex", "26.1015.0.0", "C:\\store\\old");
    let app_new = appx_fixture("OpenAI.Codex", "26.1016.8622.0", "C:\\store\\new");
    let packages = [runtime, app_old, app_new];
    let selected = select_appx_package(&packages, "OpenAI.Codex").unwrap();
    assert_eq!(selected.name, "OpenAI.Codex");
    assert_eq!(selected.version, Some([26, 1016, 8622, 0]));
    assert_eq!(selected.install_dir, PathBuf::from("C:\\store\\new"));
    assert!(select_appx_package(&packages[..1], "OpenAI.Codex").is_none());
    assert!(select_appx_package(&packages, "Claude").is_none());
    assert!(select_appx_package(&[], "OpenAI.Codex").is_none());
}

#[test]
fn appx_desktop_requires_the_real_binary_in_the_exact_package() {
    let fixture = super::super::process::private_temp_dir().unwrap();
    let root = fixture.path();
    let companion_dir = root.join("runtime/app");
    let desktop_dir = root.join("desktop/app");
    std::fs::create_dir_all(&companion_dir).unwrap();
    std::fs::create_dir_all(&desktop_dir).unwrap();
    // Even a matching filename cannot turn a runtime companion into the app.
    std::fs::write(companion_dir.join("ChatGPT.exe"), "fixture companion").unwrap();
    std::fs::write(desktop_dir.join("Codex.exe"), "fixture activation stub").unwrap();
    let packages = [
        appx_package {
            name: "OpenAI.CodexPrimaryRuntime".into(),
            version: Some([27, 0, 0, 0]),
            install_dir: root.join("runtime"),
        },
        appx_package {
            name: "OpenAI.Codex".into(),
            version: Some([26, 1016, 8622, 0]),
            install_dir: root.join("desktop"),
        },
    ];
    assert!(
        select_appx_desktop_executable(&packages[..1], "OpenAI.Codex", &["app/ChatGPT.exe"])
            .is_none()
    );
    assert!(
        select_appx_desktop_executable(&packages, "OpenAI.Codex", &["app/ChatGPT.exe"]).is_none()
    );
    let binary = desktop_dir.join("ChatGPT.exe");
    std::fs::write(&binary, "fixture desktop").unwrap();
    assert_eq!(
        select_appx_desktop_executable(&packages, "OpenAI.Codex", &["app/ChatGPT.exe"]),
        Some((binary, Some([26, 1016, 8622, 0])))
    );
}

#[test]
fn claude_appx_executable_uses_the_registered_desktop_identity() {
    let fixture = super::super::process::private_temp_dir().unwrap();
    let root = fixture.path();
    for name in ["ClaudeRuntime", "Claude"] {
        let directory = root.join(name).join("app");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("Claude.exe"), "fixture desktop").unwrap();
    }
    let packages = [
        appx_package {
            name: "ClaudeRuntime".into(),
            version: Some([99, 0, 0, 0]),
            install_dir: root.join("ClaudeRuntime"),
        },
        appx_package {
            name: "Claude".into(),
            version: Some([3, 6556, 2, 0]),
            install_dir: root.join("Claude"),
        },
    ];
    assert_eq!(
        select_appx_desktop_executable(&packages, "Claude", &["app/Claude.exe", "Claude.exe"]),
        Some((root.join("Claude/app/Claude.exe"), Some([3, 6556, 2, 0])))
    );
}

#[test]
fn version_supersedes_requires_a_known_strictly_newer_store_version() {
    let store = Some([3, 6556, 2, 0]);
    assert!(version_supersedes(store, Some([1, 46388, 4, 0])));
    assert!(!version_supersedes(Some([1, 0, 0, 0]), Some([2, 0, 0, 0])));
    assert!(!version_supersedes(Some([1, 0, 0, 0]), Some([1, 0, 0, 0])));
    // A local install without a readable version is the stale-shim shape a
    // store migration leaves behind, so any known store version wins.
    assert!(version_supersedes(Some([0, 0, 1, 0]), None));
    assert!(!version_supersedes(None, Some([1, 0, 0, 0])));
    assert!(!version_supersedes(None, None));
}
