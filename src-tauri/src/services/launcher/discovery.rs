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

/// Parse one to four dot-separated numeric components, zero-padded to four,
/// so 3-part Squirrel versions (`app-1.46388.4`) and 4-part store versions
/// (`3.6556.2.0`) compare against each other directly.
pub(super) fn padded_version(value: &str) -> Option<[u64; 4]> {
    let value = value.strip_prefix('v').unwrap_or(value);
    let mut result = [0u64; 4];
    let mut count = 0;
    for component in value.split('.') {
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

/// A store (Appx) installation only supersedes an existing local
/// (Squirrel-style) installation when its version is known and strictly
/// newer. A known store version supersedes a local install with no readable
/// version — the typical shape of a stale shim left behind after a store
/// migration — but an unknown store version never displaces a working local
/// install.
#[cfg(any(windows, test))]
pub(super) fn version_supersedes(store: Option<[u64; 4]>, local: Option<[u64; 4]>) -> bool {
    match (store, local) {
        (Some(store_version), Some(local_version)) => store_version > local_version,
        (Some(_), None) => true,
        (None, _) => false,
    }
}

/// One `Get-AppxPackage` result row: package identity plus install root.
/// `version` is `None` when the store reports an unparsable version string.
#[cfg(any(windows, test))]
pub(super) struct appx_package {
    pub(super) name: String,
    pub(super) version: Option<[u64; 4]>,
    pub(super) install_dir: PathBuf,
}

/// Parse `name|version|install_dir` lines emitted by `query_appx_packages`.
/// Malformed rows (wrong field count, empty fields, relative install paths)
/// are skipped so one broken companion package cannot hide the real app.
#[cfg(any(windows, test))]
pub(super) fn parse_appx_packages(output: &str) -> Vec<appx_package> {
    output.lines().filter_map(parse_appx_package_line).collect()
}

#[cfg(any(windows, test))]
fn parse_appx_package_line(line: &str) -> Option<appx_package> {
    let mut fields = line.trim().splitn(3, '|');
    let name = fields.next()?.trim();
    let version = fields.next()?.trim();
    let install_dir = fields.next()?.trim();
    if name.is_empty() || install_dir.is_empty() {
        return None;
    }
    let install_dir = PathBuf::from(install_dir);
    if !install_dir.is_absolute() {
        return None;
    }
    Some(appx_package {
        name: name.to_string(),
        version: padded_version(version),
        install_dir,
    })
}

/// Select only the requested desktop package, then its numerically newest
/// version (ties keep query order). Companion packages such as
/// `OpenAI.CodexPrimaryRuntime` match `*Codex*` without shipping the desktop
/// executable and must never stand in for a missing desktop package.
#[cfg(any(windows, test))]
pub(super) fn select_appx_package<'a>(
    packages: &'a [appx_package],
    exact_name: &str,
) -> Option<&'a appx_package> {
    let mut ranked: Vec<_> = packages
        .iter()
        .filter(|package| package.name == exact_name)
        .collect();
    ranked.sort_by(|left, right| right.version.cmp(&left.version));
    ranked.into_iter().next()
}

/// Check the real desktop executable inside the selected package. Relative
/// names are an internal allowlist bound to that application's known layout;
/// an activation stub is not a substitute for its Electron executable.
#[cfg(any(windows, test))]
pub(super) fn select_appx_desktop_executable(
    packages: &[appx_package],
    exact_name: &str,
    relative_paths: &[&str],
) -> Option<(PathBuf, Option<[u64; 4]>)> {
    let package = select_appx_package(packages, exact_name)?;
    relative_paths.iter().find_map(|relative| {
        // ShellExecute through cmd/start requires native separators even
        // though Windows file metadata accepts mixed slash spelling.
        let path: PathBuf = package.install_dir.join(relative).components().collect();
        (path.is_absolute() && executable(&path, desktop_platform::windows))
            .then_some((path, package.version))
    })
}

/// Query `Get-AppxPackage` for one wildcard `pattern` and return every
/// matching package. Output is `name|version|install_dir` per line so the
/// caller can select deterministically in-process instead of trusting
/// PowerShell's package ordering.
#[cfg(windows)]
pub(super) fn query_appx_packages(pattern: &str) -> Result<Vec<appx_package>, String> {
    let script = format!(
        "[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false); Get-AppxPackage {pattern} -ErrorAction Stop | ForEach-Object {{ \"{{0}}|{{1}}|{{2}}\" -f $_.Name, $_.Version, $_.InstallLocation }}"
    );
    let mut command = super::process::background_command(powershell()?);
    command.args(["-NoProfile", "-NonInteractive", "-Command", &script]);
    let bytes = super::process::capture_stdout(&mut command, super::process::PROBE_TIMEOUT)?;
    let output = String::from_utf8(bytes).map_err(|_| "appx_probe_invalid".to_string())?;
    Ok(parse_appx_packages(&output))
}

/// Pick the executable from the numerically newest versioned sibling
/// directory (`app-1.2.3`, `0.42.0`) below `root`. Numeric comparison keeps
/// `1.10.0` newer than `1.9.0`, which a lexicographic sort would misorder;
/// directories whose names do not parse as a version are used only when no
/// versioned candidate exists (last resort, lexicographically newest).
/// An unreadable or oversized root yields `None` instead of a
/// filesystem-order-dependent pick, mirroring `version_directories`.
pub(super) fn newest_versioned_executable(
    root: &Path,
    prefix: &str,
    binary_name: &str,
    platform: desktop_platform,
    entry_limit: usize,
) -> Option<PathBuf> {
    if !root.is_absolute() {
        return None;
    }
    let entries = std::fs::read_dir(root).ok()?;
    let mut candidates = entries.take(entry_limit + 1);
    let mut versioned: Vec<([u64; 3], PathBuf)> = Vec::new();
    let mut unparsed: Vec<PathBuf> = Vec::new();
    for entry in (&mut candidates).take(entry_limit) {
        let entry = entry.ok()?;
        let name = entry.file_name();
        let name = name.to_str()?;
        let path = entry.path().join(binary_name);
        if !executable(&path, platform) {
            continue;
        }
        match name.strip_prefix(prefix).and_then(numeric_version) {
            Some(version) => versioned.push((version, path)),
            None => unparsed.push(path),
        }
    }
    // The limit+1 probe must not leave an unconsumed iterator behind before
    // we can tell whether the root was oversized.
    if candidates.next().is_some() {
        return None;
    }
    versioned.sort_by(|(left_version, left_path), (right_version, right_path)| {
        right_version
            .cmp(left_version)
            .then_with(|| left_path.cmp(right_path))
    });
    versioned
        .into_iter()
        .next()
        .map(|(_, path)| path)
        .or_else(|| {
            unparsed.sort();
            unparsed.pop()
        })
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
        // Versioned sibling directories: newest numeric version wins, so a
        // `0.44.0` installation beats an older `0.9.0` one.
        return newest_versioned_executable(
            &root,
            "",
            "codex.exe",
            desktop_platform::windows,
            MAX_INSTALLATION_ENTRIES,
        );
    }
    None
}

pub(super) fn find_copilot_cli_binary() -> Option<PathBuf> {
    find_tool("copilot", &["copilot.exe", "copilot.cmd"])
}

/// The Gemini CLI ships as an npm package whose shim is `gemini`
/// (`gemini.cmd` on Windows), same shape as the Copilot CLI shim.
pub(super) fn find_gemini_cli_binary() -> Option<PathBuf> {
    find_tool("gemini", &["gemini.exe", "gemini.cmd"])
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
    // Store installs register runtime companion packages that also match
    // `*Codex*` (for example `OpenAI.CodexPrimaryRuntime`) but never ship the
    // desktop executable; only the app package does, so selection is by
    // exact package name, then newest version. The current package manifest
    // names app/ChatGPT.exe; Codex.exe can be an activation stub and does not
    // establish a launch that inherits the isolated mirror environment.
    let packages =
        query_appx_packages("*Codex*").map_err(|_| "codex_desktop_probe_invalid".to_string())?;
    select_appx_desktop_executable(&packages, "OpenAI.Codex", &["app/ChatGPT.exe"])
        .map(|(path, _)| path)
        .ok_or_else(|| "codex_desktop_not_found".into())
}
