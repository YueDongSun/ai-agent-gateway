//! Native application-data roots, with portable/test home isolation first.
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

pub(super) fn desktop_config_root() -> PathBuf {
    resolve_root(
        std::env::consts::OS,
        &super::get_home_dir(),
        std::env::var("AAG_HOME_DIR").is_ok_and(|value| !value.trim().is_empty()),
        std::env::var_os("APPDATA").as_deref(),
        std::env::var_os("XDG_CONFIG_HOME").as_deref(),
    )
}

fn resolve_root(
    platform: &str,
    home: &Path,
    isolated: bool,
    app_data: Option<&OsStr>,
    xdg: Option<&OsStr>,
) -> PathBuf {
    let valid_override = |value: Option<&OsStr>| {
        value
            .map(PathBuf::from)
            .filter(|path| !isolated && path.is_absolute())
    };
    match platform {
        "windows" => valid_override(app_data).unwrap_or_else(|| home.join("AppData/Roaming")),
        "macos" => home.join("Library/Application Support"),
        _ => valid_override(xdg).unwrap_or_else(|| home.join(".config")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portable_home_overrides_inherited_native_config_directories() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("isolated home");
        let native = root.path().join("native config");
        for (platform, expected) in [
            ("windows", home.join("AppData/Roaming")),
            ("macos", home.join("Library/Application Support")),
            ("linux", home.join(".config")),
        ] {
            assert_eq!(
                resolve_root(
                    platform,
                    &home,
                    true,
                    Some(native.as_os_str()),
                    Some(native.as_os_str())
                ),
                expected
            );
        }
    }

    #[test]
    fn normal_windows_and_linux_roots_honor_only_absolute_overrides() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let native = root.path().join("config");
        assert_eq!(
            resolve_root("windows", &home, false, Some(native.as_os_str()), None),
            native
        );
        assert_eq!(
            resolve_root("linux", &home, false, None, Some(native.as_os_str())),
            native
        );
        assert_eq!(
            resolve_root("linux", &home, false, None, Some(OsStr::new("relative"))),
            home.join(".config")
        );
        assert_eq!(
            resolve_root("windows", &home, false, Some(OsStr::new("")), None),
            home.join("AppData/Roaming")
        );
    }
}
