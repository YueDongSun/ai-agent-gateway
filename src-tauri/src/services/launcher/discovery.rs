//! Passive executable discovery. Desktop launches need not inherit shell PATH.
#![allow(non_camel_case_types)]

use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
};

const MAX_PATH_DIRECTORIES: usize = 256;
const MAX_INHERITED_PATH_DIRECTORIES: usize = 128;
const MAX_INSTALLATION_ENTRIES: usize = 256;
const MAX_HOMEBREW_ENTRIES: usize = 4096;
const MAX_VERSIONS_PER_ROOT: usize = 16;

#[cfg(test)]
#[path = "discovery_tests.rs"]
mod tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum desktop_platform {
    windows,
    macos,
    linux,
    unsupported,
}

impl desktop_platform {
    pub(super) fn current() -> Self {
        match std::env::consts::OS {
            "windows" => Self::windows,
            "macos" => Self::macos,
            "linux" => Self::linux,
            _ => Self::unsupported,
        }
    }
}

pub(super) fn executable(path: &Path, platform: desktop_platform) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    if platform != desktop_platform::windows {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o111 == 0 {
                return false;
            }
        }
    }
    true
}

pub(super) fn find_in_directories(
    names: &[&str],
    directories: &[PathBuf],
    platform: desktop_platform,
) -> Option<PathBuf> {
    // Names come from a fixed internal allowlist, never from an IPC payload.
    names
        .iter()
        .flat_map(|name| {
            directories
                .iter()
                .take(MAX_PATH_DIRECTORIES)
                .map(move |directory| directory.join(name))
        })
        .find(|path| path.is_absolute() && executable(path, platform))
}

pub(super) fn search_directories() -> Vec<PathBuf> {
    search_directories_for(
        desktop_platform::current(),
        &crate::config::get_home_dir(),
        |name| std::env::var_os(name),
    )
}

fn search_directories_for(
    platform: desktop_platform,
    home: &Path,
    environment: impl Fn(&str) -> Option<OsString>,
) -> Vec<PathBuf> {
    let mut directories = Vec::new();
    let mut add = |path: PathBuf| {
        if path.is_absolute()
            && path.is_dir()
            && !directories.contains(&path)
            && directories.len() < MAX_PATH_DIRECTORIES
        {
            directories.push(path);
        }
    };
    if let Some(path) = environment("PATH") {
        for directory in std::env::split_paths(&path).take(MAX_INHERITED_PATH_DIRECTORIES) {
            add(directory);
        }
    }
    for name in [
        "PNPM_HOME",
        "NPM_CONFIG_PREFIX",
        "npm_config_prefix",
        "VOLTA_HOME",
    ] {
        if let Some(value) = environment(name).filter(|value| !value.is_empty()) {
            let root = PathBuf::from(value);
            add(root.join("bin"));
            add(root);
        }
    }
    for suffix in [
        ".local/bin",
        ".npm-global/bin",
        ".volta/bin",
        ".cargo/bin",
        ".local/share/pnpm",
        "Library/pnpm",
        ".bun/bin",
    ] {
        add(home.join(suffix));
    }
    if platform == desktop_platform::windows {
        for (key, suffix) in [
            ("APPDATA", "npm"),
            ("LOCALAPPDATA", "pnpm"),
            ("ProgramFiles", "nodejs"),
        ] {
            if let Some(value) = environment(key) {
                add(PathBuf::from(value).join(suffix));
            }
        }
    }
    if matches!(platform, desktop_platform::macos | desktop_platform::linux) {
        let absolute_environment = |name: &str| {
            environment(name)
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
        };
        let data_home =
            absolute_environment("XDG_DATA_HOME").unwrap_or_else(|| home.join(".local/share"));
        let config_home =
            absolute_environment("XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config"));

        let mut brew_roots = Vec::new();
        if let Some(prefix) = absolute_environment("HOMEBREW_PREFIX") {
            brew_roots.push(prefix);
        }
        brew_roots.extend(if platform == desktop_platform::macos {
            ["/opt/homebrew", "/usr/local"].map(PathBuf::from)
        } else {
            ["/home/linuxbrew/.linuxbrew", "/usr/local"].map(PathBuf::from)
        });
        for root in &brew_roots {
            add(root.join("bin"));
        }
        for root in ["/usr/bin", "/bin", "/usr/sbin", "/sbin"] {
            add(PathBuf::from(root));
        }
        for name in ["NVM_BIN", "MISE_SHIMS_DIR"] {
            if let Some(path) = absolute_environment(name) {
                add(path);
            }
        }
        if let Some(path) = absolute_environment("FNM_MULTISHELL_PATH") {
            add(path.join("bin"));
        }

        let asdf_root = absolute_environment("ASDF_DATA_DIR").unwrap_or_else(|| home.join(".asdf"));
        let mise_root =
            absolute_environment("MISE_DATA_DIR").unwrap_or_else(|| data_home.join("mise"));
        for root in [&asdf_root, &mise_root] {
            add(root.join("shims"));
        }
        // Older asdf shims invoke its manager through PATH.
        add(home.join(".asdf/bin"));
        if let Some(root) = absolute_environment("ASDF_DIR") {
            add(root.join("bin"));
        }

        let mut installations = Vec::new();
        if let Some(root) = absolute_environment("NVM_DIR") {
            installations.push((root.join("versions/node"), "", "bin"));
        }
        installations.push((home.join(".nvm/versions/node"), "", "bin"));
        installations.push((config_home.join("nvm/versions/node"), "", "bin"));
        if let Some(root) = absolute_environment("FNM_DIR") {
            installations.push((root.join("node-versions"), "", "installation/bin"));
        }
        for root in [
            data_home.join("fnm"),
            home.join("Library/Application Support/fnm"),
            home.join(".fnm"),
        ] {
            installations.push((root.join("node-versions"), "", "installation/bin"));
        }
        installations.push((asdf_root.join("installs/nodejs"), "", "bin"));
        installations.push((
            absolute_environment("MISE_INSTALLS_DIR")
                .unwrap_or_else(|| mise_root.join("installs"))
                .join("node"),
            "",
            "bin",
        ));
        for root in brew_roots {
            add(root.join("opt/node/bin"));
            installations.push((root.join("opt"), "node@", "bin"));
        }
        for (root, prefix, suffix) in installations {
            for directory in version_directories(&root, prefix, suffix) {
                add(directory);
            }
        }
    }
    directories
}

/// Read only conventional installation roots, never shell profiles or managers.
/// An oversized root is skipped in full: taking arbitrary readdir entries would
/// otherwise make the chosen version depend on filesystem enumeration order.
fn version_directories(root: &Path, prefix: &str, suffix: &str) -> Vec<PathBuf> {
    if !root.is_absolute() {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    // Homebrew's opt directory also contains unrelated formulae, unlike a
    // manager's Node-only version directory.
    let entry_limit = if prefix == "node@" {
        MAX_HOMEBREW_ENTRIES
    } else {
        MAX_INSTALLATION_ENTRIES
    };
    let entries: Vec<_> = entries.take(entry_limit + 1).collect();
    if entries.len() > entry_limit || entries.iter().any(Result::is_err) {
        return Vec::new();
    }
    let mut versions: Vec<_> = entries
        .into_iter()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name();
            let version = numeric_version(name.to_str()?.strip_prefix(prefix)?)?;
            let path = entry.path().join(suffix);
            path.is_dir().then_some((version, path))
        })
        .collect();
    versions.sort_by(|(left_version, left_path), (right_version, right_path)| {
        right_version
            .cmp(left_version)
            .then_with(|| left_path.cmp(right_path))
    });
    versions
        .into_iter()
        .take(MAX_VERSIONS_PER_ROOT)
        .map(|(_, path)| path)
        .collect()
}

fn numeric_version(value: &str) -> Option<[u64; 3]> {
    let mut result = [0; 3];
    let mut count = 0;
    for component in value.strip_prefix('v').unwrap_or(value).split('.') {
        if count == result.len()
            || component.is_empty()
            || !component.bytes().all(|byte| byte.is_ascii_digit())
        {
            return None;
        }
        result[count] = component.parse().ok()?;
        count += 1;
    }
    (count > 0).then_some(result)
}

pub(super) fn child_path(program: &Path) -> Result<OsString, String> {
    child_path_for(program, search_directories(), desktop_platform::current())
}

fn child_path_for(
    program: &Path,
    mut directories: Vec<PathBuf>,
    platform: desktop_platform,
) -> Result<OsString, String> {
    // npm CLIs often use /usr/bin/env node. A CLI discovered in an older
    // installation must retain that installation's Node instead of accidentally
    // picking the first/newest version added to the fallback PATH.
    let runtime = if platform == desktop_platform::windows {
        OsStr::new("node.exe")
    } else {
        OsStr::new("node")
    };
    let npm_runtime = std::fs::canonicalize(program).ok().and_then(|resolved| {
        resolved
            .ancestors()
            .take(16)
            .find(|ancestor| {
                ancestor.file_name() == Some(OsStr::new("node_modules"))
                    && ancestor.parent().and_then(Path::file_name) == Some(OsStr::new("lib"))
            })
            .and_then(Path::parent)
            .and_then(Path::parent)
            .map(|prefix| prefix.join("bin"))
            .filter(|directory| executable(&directory.join(runtime), platform))
    });
    let runtime_directory = npm_runtime.or_else(|| {
        program
            .parent()
            .filter(|directory| {
                directory.is_absolute() && executable(&directory.join(runtime), platform)
            })
            .map(Path::to_path_buf)
    });
    if let Some(directory) = runtime_directory {
        directories.retain(|path| path != &directory);
        directories.insert(0, directory);
    }
    std::env::join_paths(directories).map_err(|_| "launcher_path_invalid".to_string())
}

pub(super) fn find_tool(unix_name: &str, windows_names: &[&str]) -> Option<PathBuf> {
    let platform = desktop_platform::current();
    let names = if platform == desktop_platform::windows {
        windows_names
    } else {
        &[unix_name]
    };
    find_in_directories(names, &search_directories(), platform)
}

pub(super) fn find_codex_cli_binary() -> Option<PathBuf> {
    if let Some(path) = find_tool("codex", &["codex.exe", "codex.cmd"]) {
        return Some(path);
    }
    #[cfg(windows)]
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let root = PathBuf::from(local).join("OpenAI/Codex/bin");
        if executable(&root.join("codex.exe"), desktop_platform::windows) {
            return Some(root.join("codex.exe"));
        }
        let mut candidates: Vec<_> = std::fs::read_dir(root)
            .ok()?
            .take(64)
            .filter_map(Result::ok)
            .map(|entry| entry.path().join("codex.exe"))
            .filter(|path| executable(path, desktop_platform::windows))
            .collect();
        candidates.sort();
        return candidates.pop();
    }
    None
}

pub(super) fn find_copilot_cli_binary() -> Option<PathBuf> {
    find_tool("copilot", &["copilot.exe", "copilot.cmd"])
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum editor_kind {
    stable,
    insiders,
    cursor,
}

impl editor_kind {
    pub(super) fn extension_directory(self, home: &Path) -> PathBuf {
        home.join(match self {
            Self::stable => ".vscode",
            Self::insiders => ".vscode-insiders",
            Self::cursor => ".cursor",
        })
        .join("extensions")
    }

    pub(super) fn name(self) -> &'static str {
        match self {
            Self::stable => "code",
            Self::insiders => "code_insiders",
            Self::cursor => "cursor",
        }
    }
}

pub(super) struct editor_installation {
    pub binary: PathBuf,
    pub kind: editor_kind,
}

pub(super) fn editor_candidates(
    platform: desktop_platform,
    home: &Path,
    local: Option<&Path>,
    program_files: Option<&Path>,
) -> Vec<(PathBuf, editor_kind)> {
    let mut candidates = Vec::new();
    for (kind, bundle, windows_folder, exe) in [
        (
            editor_kind::insiders,
            "Visual Studio Code - Insiders.app",
            "Microsoft VS Code Insiders",
            "Code - Insiders.exe",
        ),
        (
            editor_kind::stable,
            "Visual Studio Code.app",
            "Microsoft VS Code",
            "Code.exe",
        ),
        (editor_kind::cursor, "Cursor.app", "Cursor", "Cursor.exe"),
    ] {
        match platform {
            desktop_platform::macos => {
                let cli = if kind == editor_kind::insiders {
                    "code-insiders"
                } else if kind == editor_kind::cursor {
                    "cursor"
                } else {
                    "code"
                };
                for root in [PathBuf::from("/Applications"), home.join("Applications")] {
                    candidates.push((
                        root.join(bundle)
                            .join("Contents/Resources/app/bin")
                            .join(cli),
                        kind,
                    ));
                }
            }
            desktop_platform::windows => {
                if let Some(local) = local {
                    candidates.push((local.join("Programs").join(windows_folder).join(exe), kind));
                }
                if let Some(root) = program_files {
                    candidates.push((root.join(windows_folder).join(exe), kind));
                }
            }
            desktop_platform::linux => {
                for root in ["/usr/bin", "/usr/local/bin", "/snap/bin"] {
                    let name = match kind {
                        editor_kind::stable => "code",
                        editor_kind::insiders => "code-insiders",
                        editor_kind::cursor => "cursor",
                    };
                    candidates.push((PathBuf::from(root).join(name), kind));
                }
            }
            desktop_platform::unsupported => {}
        }
    }
    candidates
}

fn roaming_cursor_candidate(
    platform: desktop_platform,
    roaming: Option<&Path>,
) -> Option<(PathBuf, editor_kind)> {
    if platform != desktop_platform::windows {
        return None;
    }
    // Preserve the older APPDATA/Cursor/bin/cursor.cmd installation layout,
    // using its GUI executable directly just like a discovered cmd shim.
    let roaming = roaming.filter(|path| path.is_absolute())?;
    Some((roaming.join("Cursor/Cursor.exe"), editor_kind::cursor))
}

pub(super) fn find_editor() -> Option<editor_installation> {
    let platform = desktop_platform::current();
    for (name, kind, exe) in [
        (
            "code-insiders",
            editor_kind::insiders,
            "Code - Insiders.exe",
        ),
        ("code", editor_kind::stable, "Code.exe"),
        ("cursor", editor_kind::cursor, "Cursor.exe"),
    ] {
        if let Some(mut binary) = find_tool(name, &[&format!("{name}.cmd"), exe]) {
            // Run the actual Windows GUI binary, avoiding cmd shim reparsing.
            if platform == desktop_platform::windows
                && binary
                    .extension()
                    .is_some_and(|value| value.eq_ignore_ascii_case("cmd"))
            {
                let actual = binary.parent()?.parent()?.join(exe);
                if !executable(&actual, platform) {
                    continue;
                }
                binary = actual;
            }
            return Some(editor_installation { binary, kind });
        }
    }
    let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    let program_files = std::env::var_os("ProgramFiles").map(PathBuf::from);
    let roaming = std::env::var_os("APPDATA").map(PathBuf::from);
    editor_candidates(
        platform,
        &crate::config::get_home_dir(),
        local.as_deref(),
        program_files.as_deref(),
    )
    .into_iter()
    .chain(roaming_cursor_candidate(platform, roaming.as_deref()))
    .find(|(path, _)| executable(path, platform))
    .map(|(binary, kind)| editor_installation { binary, kind })
}

#[cfg(windows)]
pub(super) fn powershell() -> Result<PathBuf, String> {
    let root = std::env::var_os("SystemRoot")
        .ok_or_else(|| "launcher_system_root_unavailable".to_string())?;
    let path = PathBuf::from(root).join("System32/WindowsPowerShell/v1.0/powershell.exe");
    executable(&path, desktop_platform::windows)
        .then_some(path)
        .ok_or_else(|| "launcher_powershell_unavailable".into())
}

#[cfg(windows)]
pub(super) fn find_codex_desktop_exe() -> Result<PathBuf, String> {
    for key in ["LOCALAPPDATA", "ProgramFiles"] {
        if let Some(root) = std::env::var_os(key) {
            let path = PathBuf::from(root).join("OpenAI/Codex/ChatGPT.exe");
            if executable(&path, desktop_platform::windows) {
                return Ok(path);
            }
        }
    }
    let mut command = super::process::background_command(powershell()?);
    command.args(["-NoProfile", "-NonInteractive", "-Command", "[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false); Get-AppxPackage *Codex* -ErrorAction Stop | Select-Object -First 1 -ExpandProperty InstallLocation"]);
    let bytes = super::process::capture_stdout(&mut command, super::process::PROBE_TIMEOUT)?;
    let location =
        String::from_utf8(bytes).map_err(|_| "codex_desktop_probe_invalid".to_string())?;
    let path = PathBuf::from(location.trim()).join("app/ChatGPT.exe");
    if path.is_absolute() && executable(&path, desktop_platform::windows) {
        Ok(path)
    } else {
        Err("codex_desktop_not_found".into())
    }
}
