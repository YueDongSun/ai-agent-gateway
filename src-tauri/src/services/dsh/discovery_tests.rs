use super::*;
use serde_json::json;

struct Test_directory(PathBuf);
impl Test_directory {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("aag_discovery_test_{}", config::operation_id()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Test_directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn package(root: &Path, kind: &str, version: &str) -> PathBuf {
    let (name, _) = package_identity(kind);
    let directory = root.join("node_modules").join(name);
    fs::create_dir_all(directory.join("bin")).unwrap();
    fs::write(
        directory.join("package.json"),
        serde_json::to_vec(
            &json!({"name":name,"version":version,"bin":{kind:format!("bin/{kind}.mjs")}}),
        )
        .unwrap(),
    )
    .unwrap();
    let entry = directory.join("bin").join(format!("{kind}.mjs"));
    fs::write(&entry, b"throw new Error('this file must never execute');").unwrap();
    entry
}

fn node(root: &Path) -> PathBuf {
    fs::create_dir_all(root).unwrap();
    let path = root.join(if cfg!(windows) { "node.exe" } else { "node" });
    fs::write(&path, if cfg!(windows) { b"MZ\0\0" } else { b"\x7fELF" }).unwrap();
    path
}

fn environment(root: &Path) -> Discovery_environment {
    Discovery_environment {
        deployment_root: Some(root.to_owned()),
        ..Default::default()
    }
}

#[test]
fn detects_supported_deployment_without_execution_or_writes() {
    let directory = Test_directory::new();
    let node_path = node(&directory.0);
    let dsh_entry = package(&directory.0, "dsh", runtime::SUPPORTED_DSH_VERSION);
    let pnpm_entry = package(&directory.0, "pnpm", runtime::SUPPORTED_PNPM_VERSION);
    let before = fs::read(&dsh_entry).unwrap();
    let result = discover(&Dsh_settings::default(), &environment(&directory.0));
    assert_eq!(Path::new(&result.recommended.node_path), node_path);
    assert_eq!(Path::new(&result.recommended.dsh_entry_path), dsh_entry);
    assert_eq!(Path::new(&result.recommended.pnpm_entry_path), pnpm_entry);
    assert_eq!(result.candidates.len(), 3);
    assert_eq!(fs::read(dsh_entry).unwrap(), before);
    assert!(!directory.0.join("aag_settings.json").exists());
    assert!(result
        .issues
        .iter()
        .any(|issue| issue == "node_version_unverified"));
    assert!(result
        .candidates
        .iter()
        .find(|candidate| candidate.kind == "node")
        .unwrap()
        .version
        .is_none());
}

#[test]
fn detects_supported_dsh_from_an_npx_cache_root() {
    let directory = Test_directory::new();
    let node_root = directory.0.join("nodejs");
    let node_path = node(&node_root);
    let dsh_entry = package(&directory.0, "dsh", runtime::SUPPORTED_DSH_VERSION);
    let result = discover(
        &Dsh_settings::default(),
        &Discovery_environment {
            path_directories: vec![node_root],
            npx_cache_roots: vec![directory.0.clone()],
            ..Default::default()
        },
    );
    assert_eq!(Path::new(&result.recommended.node_path), node_path);
    assert_eq!(Path::new(&result.recommended.dsh_entry_path), dsh_entry);
    assert_eq!(
        result
            .candidates
            .iter()
            .find(|candidate| candidate.kind == "dsh")
            .unwrap()
            .source,
        "npx_cache"
    );
}

#[test]
fn configured_runtime_wins_without_replacing_unsupported_explicit_path() {
    let directory = Test_directory::new();
    let selected = directory.0.join("selected");
    let other = directory.0.join("other");
    let selected_entry = package(&selected, "dsh", runtime::SUPPORTED_DSH_VERSION);
    package(&other, "dsh", runtime::SUPPORTED_DSH_VERSION);
    let settings = Dsh_settings {
        dsh_entry_path: selected_entry.to_string_lossy().into_owned(),
        ..Default::default()
    };
    let result = discover(&settings, &environment(&other));
    assert_eq!(result.recommended.dsh_entry_path, settings.dsh_entry_path);
    assert_eq!(result.candidates[0].source, "configured");
    package(&selected, "dsh", "99.0.0");
    let result = discover(&settings, &environment(&other));
    assert_eq!(
        result.candidates[0].reason.as_deref(),
        Some("unsupported_runtime_version")
    );
    assert_eq!(settings.dsh_entry_path, selected_entry.to_string_lossy());
}

#[test]
fn npm_and_pnpm_shims_are_never_executed_or_adopted_as_javascript() {
    let directory = Test_directory::new();
    for filename in ["dsh.cmd", "dsh.ps1", "dsh", "pnpm.cmd"] {
        fs::write(directory.0.join(filename), b"exit 99 # must never execute").unwrap();
    }
    let npm_entry = package(&directory.0, "dsh", runtime::SUPPORTED_DSH_VERSION);
    let pnpm_root = directory.0.join("global/5");
    let pnpm_entry = package(&pnpm_root, "pnpm", runtime::SUPPORTED_PNPM_VERSION);
    let env = Discovery_environment {
        path_directories: vec![directory.0.clone()],
        ..Default::default()
    };
    let result = discover(&Dsh_settings::default(), &env);
    assert_eq!(Path::new(&result.recommended.dsh_entry_path), npm_entry);
    assert_eq!(Path::new(&result.recommended.pnpm_entry_path), pnpm_entry);
    assert!(result
        .candidates
        .iter()
        .all(|candidate| candidate.source == "path_sibling"));
    let settings = Dsh_settings {
        dsh_entry_path: directory.0.join("dsh.cmd").to_string_lossy().into_owned(),
        ..Default::default()
    };
    assert_eq!(
        discover(&settings, &Discovery_environment::default()).candidates[0]
            .reason
            .as_deref(),
        Some("entry_must_be_javascript")
    );
}

#[test]
fn scripts_named_node_are_not_native_binary_candidates() {
    let directory = Test_directory::new();
    let node_path = node(&directory.0);
    fs::write(&node_path, b"#!/bin/sh\nexit 0\n").unwrap();
    let result = discover(&Dsh_settings::default(), &environment(&directory.0));
    assert!(result.recommended.node_path.is_empty());
    assert_eq!(
        result.candidates[0].reason.as_deref(),
        Some("node_binary_format_invalid")
    );
}

#[test]
fn manifest_version_identity_and_declared_bin_are_required() {
    let directory = Test_directory::new();
    let entry = package(&directory.0, "dsh", runtime::SUPPORTED_DSH_VERSION);
    let manifest = entry
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("package.json");
    fs::write(&manifest, b"not json").unwrap();
    let result = discover(&Dsh_settings::default(), &environment(&directory.0));
    assert_eq!(
        result.candidates[0].reason.as_deref(),
        Some("runtime_manifest_invalid")
    );
    fs::write(&manifest, serde_json::to_vec(&json!({"name":"@deepseek-ai/dsh","version":runtime::SUPPORTED_DSH_VERSION,"bin":{"dsh":"../../outside.js"}})).unwrap()).unwrap();
    let result = discover(&Dsh_settings::default(), &environment(&directory.0));
    assert_eq!(
        result.candidates[0].reason.as_deref(),
        Some("runtime_bin_invalid")
    );
    package(&directory.0, "dsh", runtime::SUPPORTED_DSH_VERSION);
    let other_entry = entry.with_file_name("other.mjs");
    fs::write(&other_entry, b"// unrelated file within a known package").unwrap();
    let settings = Dsh_settings {
        dsh_entry_path: other_entry.to_string_lossy().into_owned(),
        ..Default::default()
    };
    assert_eq!(
        discover(&settings, &Discovery_environment::default()).candidates[0]
            .reason
            .as_deref(),
        Some("runtime_entry_not_declared")
    );
}

#[test]
fn excessive_path_and_manifest_inputs_are_bounded() {
    let directory = Test_directory::new();
    let entry = package(&directory.0, "dsh", runtime::SUPPORTED_DSH_VERSION);
    fs::write(
        entry
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("package.json"),
        vec![b' '; MAX_MANIFEST_BYTES as usize + 1],
    )
    .unwrap();
    let mut env = environment(&directory.0);
    env.path_directories = (0..1000)
        .map(|index| directory.0.join(format!("path_{index}")))
        .collect();
    let result = discover(&Dsh_settings::default(), &env);
    assert!(result.scan_limited);
    assert!(result.scanned_locations <= MAX_LOCATIONS);
    assert_eq!(
        result.candidates[0].reason.as_deref(),
        Some("runtime_manifest_too_large")
    );
    assert!(!directory.0.join("path_0").exists());
}

#[test]
fn nested_projects_and_acceptance_fixtures_are_not_discovered() {
    let directory = Test_directory::new();
    package(
        &directory.0.join("unrelated/project"),
        "dsh",
        runtime::SUPPORTED_DSH_VERSION,
    );
    let result = discover(&Dsh_settings::default(), &environment(&directory.0));
    assert!(result.recommended.dsh_entry_path.is_empty());
    let fixture = directory.0.join("tmp/dsh_acceptance/runtime");
    package(&fixture, "dsh", runtime::SUPPORTED_DSH_VERSION);
    let result = discover(
        &Dsh_settings::default(),
        &Discovery_environment {
            path_directories: vec![fixture.clone()],
            ..Default::default()
        },
    );
    assert!(result.recommended.dsh_entry_path.is_empty());
    assert_eq!(
        result.candidates[0].reason.as_deref(),
        Some("acceptance_fixture_excluded")
    );
    let explicit = discover(&Dsh_settings::default(), &environment(&fixture));
    assert!(!explicit.recommended.dsh_entry_path.is_empty());
    assert!(explicit
        .issues
        .iter()
        .any(|issue| issue == "acceptance_runtime_explicit"));
}

#[test]
fn explicit_missing_paths_remain_diagnosable_and_no_managed_state_is_created() {
    let directory = Test_directory::new();
    let settings = Dsh_settings {
        node_path: directory
            .0
            .join("missing/node.exe")
            .to_string_lossy()
            .into_owned(),
        ..Default::default()
    };
    let result = discover(&settings, &Discovery_environment::default());
    assert_eq!(
        result.candidates[0].reason.as_deref(),
        Some("runtime_file_missing")
    );
    assert_eq!(result.scanned_locations, 1);
    let service = Dsh_service::new(directory.0.join("managed"));
    service.discover_runtime().unwrap();
    assert!(!service.root.exists());
}

#[cfg(unix)]
#[test]
fn package_bin_symlink_cannot_escape_declared_package() {
    let directory = Test_directory::new();
    let entry = package(&directory.0, "dsh", runtime::SUPPORTED_DSH_VERSION);
    let outside = directory.0.join("outside.mjs");
    fs::write(&outside, b"// outside").unwrap();
    fs::remove_file(&entry).unwrap();
    std::os::unix::fs::symlink(outside, entry).unwrap();
    let result = discover(&Dsh_settings::default(), &environment(&directory.0));
    assert_eq!(
        result.candidates[0].reason.as_deref(),
        Some("runtime_entry_escape")
    );
}
