//! Bounded, passive discovery of installed runtimes. This module never executes
//! a PATH entry, parses a command shim, reads native agent settings, or downloads
//! packages. Static metadata identifies candidates, not trusted executable code.

use super::{config, runtime, types::Dsh_settings, Dsh_service};
use crate::error::AppError;
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::BTreeSet,
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_PATH_DIRECTORIES: usize = 48;
const MAX_LOCATIONS: usize = 1024;
const MAX_MANIFEST_BYTES: u64 = 256 * 1024;
const MAX_CANDIDATES: usize = 48;
const MAX_NPX_CACHE_ROOTS: usize = 64;

#[derive(Clone, Debug, Serialize)]
pub struct Dsh_runtime_candidate {
    pub candidate_id: String,
    pub kind: String,
    pub path: String,
    pub source: String,
    pub version: Option<String>,
    pub compatible: bool,
    pub validation: String,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Dsh_runtime_paths {
    pub node_path: String,
    pub dsh_entry_path: String,
    pub pnpm_entry_path: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Dsh_discovery {
    pub candidates: Vec<Dsh_runtime_candidate>,
    pub recommended: Dsh_runtime_paths,
    pub issues: Vec<String>,
    pub scanned_locations: usize,
    pub scan_limited: bool,
}

/// Inputs are captured once for deterministic, environment-independent tests.
/// Only known installation directories are derived from a home directory.
#[derive(Default)]
pub(super) struct Discovery_environment {
    pub deployment_root: Option<PathBuf>,
    pub path_directories: Vec<PathBuf>,
    pub standard_roots: Vec<(PathBuf, &'static str)>,
    pub npx_cache_roots: Vec<PathBuf>,
    pub npx_cache_limited: bool,
}

impl Discovery_environment {
    fn capture() -> Self {
        let (npx_cache_roots, npx_cache_limited) = capture_npx_cache_roots();
        let mut environment = Self {
            deployment_root: std::env::var_os("AAG_DSH_RUNTIME_DIR").map(PathBuf::from),
            path_directories: std::env::var_os("PATH")
                .map(|value| std::env::split_paths(&value).collect())
                .unwrap_or_default(),
            standard_roots: Vec::new(),
            npx_cache_roots,
            npx_cache_limited,
        };
        for key in ["PNPM_HOME", "NPM_CONFIG_PREFIX", "npm_config_prefix"] {
            if let Some(value) = std::env::var_os(key) {
                environment
                    .standard_roots
                    .push((PathBuf::from(value), "environment_prefix"));
            }
        }
        #[cfg(windows)]
        {
            for (key, suffix, source) in [
                ("APPDATA", "npm", "npm_global"),
                ("LOCALAPPDATA", "pnpm", "pnpm_global"),
                ("ProgramFiles", "nodejs", "system_install"),
            ] {
                if let Some(value) = std::env::var_os(key) {
                    environment
                        .standard_roots
                        .push((PathBuf::from(value).join(suffix), source));
                }
            }
        }
        if let Some(home) = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }) {
            let home = PathBuf::from(home);
            for suffix in [".npm-global", ".local", ".local/share/pnpm", "Library/pnpm"] {
                environment
                    .standard_roots
                    .push((home.join(suffix), "user_install"));
            }
        }
        #[cfg(unix)]
        for root in ["/usr", "/usr/local", "/opt/homebrew"] {
            environment
                .standard_roots
                .push((PathBuf::from(root), "system_install"));
        }
        environment
    }
}

/// Capture only bounded, known npm npx cache entry roots. The cache is a
/// package source, not an executable search path; each package is still
/// validated by Scanner before it can become a recommendation.
fn capture_npx_cache_roots() -> (Vec<PathBuf>, bool) {
    let mut cache_directories = Vec::new();
    for key in ["NPM_CONFIG_CACHE", "npm_config_cache"] {
        if let Some(value) = std::env::var_os(key) {
            cache_directories.push(PathBuf::from(value));
        }
    }
    #[cfg(windows)]
    if let Some(value) = std::env::var_os("LOCALAPPDATA") {
        cache_directories.push(PathBuf::from(value).join("npm-cache"));
    }
    #[cfg(not(windows))]
    if let Some(value) = std::env::var_os("HOME") {
        cache_directories.push(PathBuf::from(value).join(".npm"));
    }

    let mut seen_cache_directories = BTreeSet::new();
    let mut entries: Vec<(SystemTime, PathBuf)> = Vec::new();
    for cache_directory in cache_directories {
        let key = cache_directory.to_string_lossy().into_owned();
        let key = if cfg!(windows) {
            key.to_lowercase()
        } else {
            key
        };
        if !seen_cache_directories.insert(key) {
            continue;
        }
        let npx_directory = cache_directory.join("_npx");
        let Ok(directory_entries) = fs::read_dir(npx_directory) else {
            continue;
        };
        for entry in directory_entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_dir() {
                continue;
            }
            let modified = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .unwrap_or(UNIX_EPOCH);
            entries.push((modified, entry.path()));
        }
    }
    entries.sort_by(|(left_time, left_path), (right_time, right_path)| {
        right_time
            .cmp(left_time)
            .then_with(|| left_path.cmp(right_path))
    });
    let limited = entries.len() > MAX_NPX_CACHE_ROOTS;
    (
        entries
            .into_iter()
            .take(MAX_NPX_CACHE_ROOTS)
            .map(|(_, path)| path)
            .collect(),
        limited,
    )
}

impl Dsh_service {
    /// Read-only: no directory creation, persistent caching, or process launch.
    pub fn discover_runtime(&self) -> Result<Dsh_discovery, AppError> {
        let (settings, _) = config::read_settings(&self.root)?;
        Ok(discover(&settings, &Discovery_environment::capture()))
    }
}

struct Scanner {
    candidates: Vec<Dsh_runtime_candidate>,
    paths_seen: BTreeSet<String>,
    locations_seen: BTreeSet<String>,
    scanned_locations: usize,
    scan_limited: bool,
}

fn valid_local_path(path: &Path) -> bool {
    if !path.is_absolute() || path.to_string_lossy().chars().any(char::is_control) {
        return false;
    }
    #[cfg(windows)]
    if matches!(path.components().next(), Some(Component::Prefix(prefix)) if !matches!(prefix.kind(), std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_)))
    {
        return false;
    }
    true
}

fn path_key(path: &Path) -> String {
    let normalized: PathBuf = path.components().collect();
    let value = normalized.to_string_lossy().into_owned();
    if cfg!(windows) {
        value.to_lowercase()
    } else {
        value
    }
}

fn acceptance_fixture(path: &Path) -> bool {
    // Test fixtures must never become implicit production installation advice.
    path.components().any(|part| {
        part.as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case("dsh_acceptance")
    })
}

impl Scanner {
    fn location(&mut self, path: &Path) -> bool {
        if !valid_local_path(path) || !self.locations_seen.insert(path_key(path)) {
            return false;
        }
        if self.scanned_locations >= MAX_LOCATIONS || self.candidates.len() >= MAX_CANDIDATES {
            self.scan_limited = true;
            return false;
        }
        self.scanned_locations += 1;
        true
    }

    fn candidate(
        &mut self,
        kind: &str,
        path: &Path,
        source: &str,
        version: Option<String>,
        validation: &str,
        reason: Option<&str>,
    ) {
        let key = format!("{kind}:{}", path_key(path));
        if !self.paths_seen.insert(key.clone()) {
            return;
        }
        if self.candidates.len() >= MAX_CANDIDATES {
            self.scan_limited = true;
            return;
        }
        self.candidates.push(Dsh_runtime_candidate {
            candidate_id: config::digest(key.as_bytes())[..16].to_owned(),
            kind: kind.into(),
            path: path.to_string_lossy().into_owned(),
            source: source.into(),
            version,
            compatible: reason.is_none(),
            validation: validation.into(),
            reason: reason.map(str::to_owned),
        });
    }

    fn node(&mut self, path: &Path, source: &str, explicit: bool) {
        if !valid_local_path(path) {
            if explicit {
                self.candidate(
                    "node",
                    path,
                    source,
                    None,
                    "rejected",
                    Some("invalid_runtime_path"),
                );
            }
            return;
        }
        if !self.location(path) {
            return;
        }
        if !path.is_file() {
            if explicit {
                self.candidate(
                    "node",
                    path,
                    source,
                    None,
                    "missing",
                    Some("runtime_file_missing"),
                );
            }
            return;
        }
        let reason = if !matches!(
            path.file_name().and_then(|name| name.to_str()),
            Some("node" | "node.exe")
        ) {
            Some("node_binary_required")
        } else if acceptance_fixture(path) && !matches!(source, "configured" | "deployment_root") {
            Some("acceptance_fixture_excluded")
        } else if !native_binary_header(path) {
            Some("node_binary_format_invalid")
        } else {
            None
        };
        self.candidate(
            "node",
            path,
            source,
            None,
            if reason.is_none() {
                "binary_format_only"
            } else {
                "rejected"
            },
            reason,
        );
    }

    fn package(&mut self, directory: &Path, kind: &str, source: &str, configured: Option<&Path>) {
        let manifest_path = directory.join("package.json");
        if !self.location(&manifest_path) {
            return;
        }
        if !manifest_path.is_file() {
            return;
        }
        let manifest = match read_manifest(&manifest_path) {
            Ok(value) => value,
            Err(reason) => {
                self.candidate(
                    kind,
                    configured.unwrap_or(&manifest_path),
                    source,
                    None,
                    "rejected",
                    Some(reason),
                );
                return;
            }
        };
        let (package_name, supported_version) = package_identity(kind);
        if manifest.get("name").and_then(Value::as_str) != Some(package_name) {
            if let Some(configured_path) = configured {
                self.candidate(
                    kind,
                    configured_path,
                    source,
                    None,
                    "rejected",
                    Some("runtime_package_identity_missing"),
                );
            }
            return;
        }
        let version = manifest
            .get("version")
            .and_then(Value::as_str)
            .filter(|value| value.len() <= 100 && !value.chars().any(char::is_control))
            .map(str::to_owned);
        let bin = manifest.get("bin").and_then(|bin| {
            bin.as_str()
                .or_else(|| bin.get(kind).and_then(Value::as_str))
        });
        let entry = bin
            .filter(|value| safe_relative_entry(value))
            .map(|value| directory.join(value));
        let candidate_path = configured.or(entry.as_deref()).unwrap_or(&manifest_path);
        let reason = if acceptance_fixture(candidate_path)
            && !matches!(source, "configured" | "deployment_root")
        {
            Some("acceptance_fixture_excluded")
        } else if version.as_deref() != Some(supported_version) {
            Some("unsupported_runtime_version")
        } else if entry.is_none() {
            Some("runtime_bin_invalid")
        } else if !entry.as_ref().is_some_and(|path| path.is_file()) {
            Some("runtime_file_missing")
        } else if !entry
            .as_ref()
            .and_then(|path| fs::canonicalize(path).ok())
            .zip(fs::canonicalize(directory).ok())
            .is_some_and(|(entry, directory)| entry.starts_with(directory))
        {
            Some("runtime_entry_escape")
        } else if configured.is_some_and(|path| {
            fs::canonicalize(path).ok() != fs::canonicalize(entry.as_ref().unwrap()).ok()
        }) {
            Some("runtime_entry_not_declared")
        } else {
            None
        };
        self.candidate(
            kind,
            candidate_path,
            source,
            version,
            if reason.is_none() {
                "manifest_verified"
            } else {
                "rejected"
            },
            reason,
        );
    }

    fn configured_package(&mut self, value: &str, kind: &str) {
        if value.is_empty() {
            return;
        }
        let path = Path::new(value);
        if !valid_local_path(path) {
            self.candidate(
                kind,
                path,
                "configured",
                None,
                "rejected",
                Some("invalid_runtime_path"),
            );
            return;
        }
        if !path.is_file() {
            self.candidate(
                kind,
                path,
                "configured",
                None,
                "missing",
                Some("runtime_file_missing"),
            );
            return;
        }
        if !javascript_entry(path) {
            self.candidate(
                kind,
                path,
                "configured",
                None,
                "rejected",
                Some("entry_must_be_javascript"),
            );
            return;
        }
        let resolved = fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
        for directory in resolved.ancestors().skip(1).take(5) {
            if directory.join("package.json").is_file() {
                self.package(directory, kind, "configured", Some(path));
                return;
            }
        }
        self.candidate(
            kind,
            path,
            "configured",
            None,
            "rejected",
            Some("runtime_package_identity_missing"),
        );
    }

    fn root(&mut self, root: &Path, source: &str) {
        if !valid_local_path(root) {
            return;
        }
        let node_name = if cfg!(windows) { "node.exe" } else { "node" };
        self.node(&root.join(node_name), source, false);
        self.node(&root.join("bin").join(node_name), source, false);
        for base in [
            root.join("node_modules"),
            root.join("lib/node_modules"),
            root.join("global/5/node_modules"),
        ] {
            self.package(&base.join("@deepseek-ai/dsh"), "dsh", source, None);
            self.package(&base.join("pnpm"), "pnpm", source, None);
        }
        self.package(
            &root
                .join(".tools/pnpm")
                .join(runtime::SUPPORTED_PNPM_VERSION),
            "pnpm",
            source,
            None,
        );
    }
}

fn native_binary_header(path: &Path) -> bool {
    let mut header = [0u8; 4];
    if fs::File::open(path)
        .and_then(|mut file| file.read_exact(&mut header))
        .is_err()
    {
        return false;
    }
    #[cfg(windows)]
    {
        header.starts_with(b"MZ")
    }
    #[cfg(not(windows))]
    {
        header == *b"\x7fELF"
            || matches!(
                header,
                [0xfe, 0xed, 0xfa, 0xce]
                    | [0xfe, 0xed, 0xfa, 0xcf]
                    | [0xce, 0xfa, 0xed, 0xfe]
                    | [0xcf, 0xfa, 0xed, 0xfe]
                    | [0xca, 0xfe, 0xba, 0xbe]
            )
    }
}

fn package_identity(kind: &str) -> (&'static str, &'static str) {
    if kind == "dsh" {
        ("@deepseek-ai/dsh", runtime::SUPPORTED_DSH_VERSION)
    } else {
        ("pnpm", runtime::SUPPORTED_PNPM_VERSION)
    }
}

fn javascript_entry(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|value| value.to_str()),
        Some("js" | "mjs" | "cjs")
    )
}

pub(super) fn safe_relative_entry(value: &str) -> bool {
    let path = Path::new(value);
    !value.is_empty()
        && value.len() <= 4096
        && !value.chars().any(char::is_control)
        && !value.contains('\\')
        && !path.is_absolute()
        && javascript_entry(path)
        && path
            .components()
            .all(|part| matches!(part, Component::Normal(_) | Component::CurDir))
}

fn read_manifest(path: &Path) -> Result<Value, &'static str> {
    let file = fs::File::open(path).map_err(|_| "runtime_manifest_unreadable")?;
    let mut data = Vec::new();
    file.take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut data)
        .map_err(|_| "runtime_manifest_unreadable")?;
    if data.len() as u64 > MAX_MANIFEST_BYTES {
        return Err("runtime_manifest_too_large");
    }
    serde_json::from_slice(&data).map_err(|_| "runtime_manifest_invalid")
}

pub(super) fn discover(
    settings: &Dsh_settings,
    environment: &Discovery_environment,
) -> Dsh_discovery {
    let mut scanner = Scanner {
        candidates: Vec::new(),
        paths_seen: BTreeSet::new(),
        locations_seen: BTreeSet::new(),
        scanned_locations: 0,
        scan_limited: false,
    };
    if !settings.node_path.is_empty() {
        scanner.node(Path::new(&settings.node_path), "configured", true);
    }
    scanner.configured_package(&settings.dsh_entry_path, "dsh");
    scanner.configured_package(&settings.pnpm_entry_path, "pnpm");
    if let Some(root) = &environment.deployment_root {
        scanner.root(root, "deployment_root");
        // A deployment override may name the DSH package itself.
        scanner.package(root, "dsh", "deployment_root", None);
    }
    for directory in environment
        .path_directories
        .iter()
        .take(MAX_PATH_DIRECTORIES)
    {
        scanner.root(directory, "path_sibling");
        if directory.file_name().is_some_and(|name| name == "bin") {
            if let Some(prefix) = directory.parent() {
                scanner.root(prefix, "path_sibling");
            }
        }
    }
    for root in &environment.npx_cache_roots {
        scanner.root(root, "npx_cache");
    }
    for (root, source) in &environment.standard_roots {
        scanner.root(root, source);
    }
    scanner.scan_limited |= environment.path_directories.len() > MAX_PATH_DIRECTORIES;
    scanner.scan_limited |= environment.npx_cache_limited;
    let recommended_path = |kind: &str| {
        scanner
            .candidates
            .iter()
            .find(|candidate| candidate.kind == kind && candidate.compatible)
            .map(|candidate| candidate.path.clone())
            .unwrap_or_default()
    };
    let recommended = Dsh_runtime_paths {
        node_path: recommended_path("node"),
        dsh_entry_path: recommended_path("dsh"),
        pnpm_entry_path: recommended_path("pnpm"),
    };
    let mut issues = Vec::new();
    if recommended.node_path.is_empty() {
        issues.push("node_not_found".into());
    } else {
        issues.push("node_version_unverified".into());
    }
    if recommended.dsh_entry_path.is_empty() {
        issues.push("dsh_not_found".into());
    }
    if recommended.pnpm_entry_path.is_empty() {
        issues.push("pnpm_not_found".into());
    }
    if scanner
        .candidates
        .iter()
        .any(|candidate| candidate.compatible && acceptance_fixture(Path::new(&candidate.path)))
    {
        issues.push("acceptance_runtime_explicit".into());
    }
    if scanner.scan_limited {
        issues.push("discovery_scan_limited".into());
    }
    Dsh_discovery {
        candidates: scanner.candidates,
        recommended,
        issues,
        scanned_locations: scanner.scanned_locations,
        scan_limited: scanner.scan_limited,
    }
}

#[cfg(test)]
#[path = "discovery_tests.rs"]
mod tests;
