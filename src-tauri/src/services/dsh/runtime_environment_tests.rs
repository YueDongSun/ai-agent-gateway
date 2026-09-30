use super::*;
use std::collections::BTreeMap;

struct Test_directory(PathBuf);

impl Test_directory {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("aag_dsh_environment_{}", config::operation_id()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Test_directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn command_construction_is_read_only_and_excludes_parent_secrets() {
    let directory = Test_directory::new();
    let root = directory.0.join("managed");
    let environment = Runtime_environment::new(&root);
    let parent: BTreeMap<&str, OsString> = [
        ("PATH", "fixture_path"),
        ("SystemRoot", r"C:\Windows"),
        ("SystemDrive", "C:"),
        ("HOME", "foreign_home"),
        ("USERPROFILE", "foreign_home"),
        ("ANTHROPIC_AUTH_TOKEN", "fixture_secret"),
        ("OPENAI_API_KEY", "fixture_secret"),
        ("NODE_OPTIONS", "--require=untrusted_hook"),
        ("HTTP_PROXY", "fixture_proxy"),
        ("NPM_CONFIG_USERCONFIG", "foreign_npmrc"),
    ]
    .into_iter()
    .map(|(key, value)| (key, value.into()))
    .collect();
    let mut command = Command::new("unused_node");
    command.env("PREVIOUS_COMMAND_SECRET", "fixture_secret");
    environment
        .configure_command(&mut command, |key| parent.get(key).cloned())
        .unwrap();
    assert!(
        !root.exists(),
        "building a command must not create a profile"
    );
    let actual: BTreeMap<OsString, OsString> = command
        .get_envs()
        .filter_map(|(key, value)| value.map(|value| (key.to_owned(), value.to_owned())))
        .collect();
    assert_eq!(
        actual.get(&OsString::from("PATH")),
        Some(&OsString::from("fixture_path"))
    );
    assert_eq!(
        actual.get(&OsString::from("HOME")),
        Some(&root.join("user_home").into_os_string())
    );
    assert_eq!(
        actual.get(&OsString::from("DSH_HOME")),
        Some(&root.clone().into_os_string())
    );
    #[cfg(windows)]
    assert_eq!(
        actual.get(&OsString::from("SystemDrive")),
        Some(&OsString::from("C:"))
    );
    for name in [
        "ANTHROPIC_AUTH_TOKEN",
        "OPENAI_API_KEY",
        "NODE_OPTIONS",
        "HTTP_PROXY",
        "NPM_CONFIG_USERCONFIG",
        "PREVIOUS_COMMAND_SECRET",
    ] {
        assert!(
            !actual.contains_key(&OsString::from(name)),
            "inherited {name}"
        );
    }
    // Every advertised profile/cache path is materialized by explicit prepare.
    environment.prepare().unwrap();
    for name in [
        "HOME",
        "USERPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
        "XDG_CONFIG_HOME",
        "XDG_CACHE_HOME",
    ] {
        let path = Path::new(actual.get(&OsString::from(name)).unwrap());
        assert!(path.is_dir(), "missing {name} directory");
        assert!(path.starts_with(root.join("user_home")));
    }
    assert!(environment.workspace.is_dir());
    #[cfg(windows)]
    assert!(
        root.join("user_home/Desktop").is_dir(),
        "native folder picker requires Desktop"
    );
}

#[test]
fn repeated_preparation_preserves_existing_user_content() {
    let directory = Test_directory::new();
    let environment = Runtime_environment::new(&directory.0);
    environment.prepare().unwrap();
    let existing = directory.0.join("user_home/.config/user_settings.json");
    fs::write(&existing, b"preserve_existing_bytes").unwrap();
    environment.prepare().unwrap();
    assert_eq!(fs::read(existing).unwrap(), b"preserve_existing_bytes");
}

#[test]
fn conflicting_profile_file_is_skipped_without_replacing_user_content() {
    let directory = Test_directory::new();
    let root = directory.0.join("managed");
    fs::create_dir_all(root.join("user_home")).unwrap();
    let obstruction = root.join("user_home/.cache");
    fs::write(&obstruction, b"do_not_replace").unwrap();
    // A pre-existing regular file at a managed sub-path is tolerated: the
    // directory is skipped, user content is preserved, and other directories
    // are still created.
    Runtime_environment::new(&root).prepare().unwrap();
    assert_eq!(fs::read(obstruction).unwrap(), b"do_not_replace");
    assert!(root.join("workspace").is_dir());
    assert!(root.join("user_home/AppData/Roaming").is_dir());
}

#[test]
fn workspace_file_is_rejected_with_a_specific_creation_error() {
    let directory = Test_directory::new();
    let root = directory.0.join("managed");
    fs::create_dir_all(&root).unwrap();
    let workspace = root.join("workspace");
    fs::write(&workspace, b"preserve_workspace_file").unwrap();
    let error = Runtime_environment::new(&root).prepare().unwrap_err();
    assert_eq!(error.to_string(), "dsh:workspace_create_failed");
    assert_eq!(fs::read(workspace).unwrap(), b"preserve_workspace_file");
}

#[test]
fn managed_home_parent_file_is_skipped_without_a_create_dir_failure() {
    let directory = Test_directory::new();
    let root = directory.0.join("managed");
    let user_home = root.join("user_home");
    fs::create_dir_all(&user_home).unwrap();
    let app_data = user_home.join("AppData");
    fs::write(&app_data, b"preserve_appdata_file").unwrap();
    Runtime_environment::new(&root).prepare().unwrap();
    assert_eq!(fs::read(app_data).unwrap(), b"preserve_appdata_file");
    assert!(root.join("workspace").is_dir());
    assert!(root.join("user_home/.config").is_dir());
}

#[cfg(windows)]
#[test]
fn system_drive_is_preserved_or_derived_from_an_absolute_system_root() {
    let absolute_system_root = ['C', ':', '\\', 'W', 'i', 'n', 'd', 'o', 'w', 's']
        .into_iter()
        .collect::<String>();
    let network_system_root = [
        '\\', '\\', 's', 'e', 'r', 'v', 'e', 'r', '\\', 'W', 'i', 'n', 'd', 'o', 'w', 's',
    ]
    .into_iter()
    .collect::<String>();
    assert_eq!(
        system_drive(&|key| (key == "SystemDrive").then(|| "D:".into())).unwrap(),
        OsString::from("D:")
    );
    assert_eq!(
        system_drive(&|key| (key == "SystemRoot").then(|| absolute_system_root.clone().into()))
            .unwrap(),
        OsString::from("C:")
    );
    for invalid in [
        "",
        "C",
        "C:relative",
        absolute_system_root.as_str(),
        network_system_root.as_str(),
    ] {
        let error =
            system_drive(&|key| (key == "SystemDrive").then(|| invalid.into())).unwrap_err();
        assert_eq!(error.to_string(), "dsh:invalid_system_drive");
    }
    for invalid_root in ["", "C:Windows", network_system_root.as_str()] {
        let error =
            system_drive(&|key| (key == "SystemRoot").then(|| invalid_root.into())).unwrap_err();
        assert_eq!(error.to_string(), "dsh:system_drive_unavailable");
    }
}
