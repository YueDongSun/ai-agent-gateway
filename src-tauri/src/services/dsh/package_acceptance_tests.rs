//! Explicit, public-registry package acceptance. This installs package files
//! with lifecycle scripts disabled; it never starts DSH or executes a plugin.
//! Set AAG_DSH_TEST_RUNTIME_DIR and AAG_DSH_TEST_NODE before running ignored tests.
use super::*;
use std::{fs, path::PathBuf};

struct Package_directory {
    path: PathBuf,
    service: Dsh_service,
}

impl Package_directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "aag_dsh_package_acceptance_{}",
            config::operation_id()
        ));
        fs::create_dir(&path).expect("create disposable package acceptance directory");
        let service = Dsh_service::new(path.clone());
        Self { path, service }
    }
}

impl Drop for Package_directory {
    fn drop(&mut self) {
        let _ = self.service.shutdown();
        // Keep failed acceptance state for a separate, explicit diagnosis.
        if !std::thread::panicking() {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

#[test]
#[ignore = "downloads a public allowlisted package with pinned pnpm; requires explicit runtime paths"]
fn pinned_pnpm_plugin_lifecycle() {
    let runtime_dir = PathBuf::from(
        std::env::var_os("AAG_DSH_TEST_RUNTIME_DIR")
            .expect("set AAG_DSH_TEST_RUNTIME_DIR to pinned node_modules"),
    );
    let node_path = PathBuf::from(
        std::env::var_os("AAG_DSH_TEST_NODE").expect("set AAG_DSH_TEST_NODE explicitly"),
    );
    let pnpm_entry = runtime_dir.join("pnpm/bin/pnpm.cjs");
    assert!(
        node_path.is_file() && pnpm_entry.is_file(),
        "pinned package-manager prerequisites missing"
    );
    let directory = Package_directory::new();
    let service = &directory.service;
    assert!(!service.snapshot().unwrap().initialized);
    let initial = service.snapshot().unwrap();
    let mut settings = initial.settings;
    settings.node_path = node_path.to_string_lossy().into_owned();
    settings.pnpm_entry_path = pnpm_entry.to_string_lossy().into_owned();
    settings.operation_timeout_secs = 600;
    service
        .save_settings(settings, initial.settings_digest)
        .unwrap();
    let initialize = service
        .preview(Dsh_operation_request {
            action: "initialize".into(),
            package_name: None,
            version: None,
        })
        .unwrap();
    assert_eq!(
        service.execute(initialize.preview_id).unwrap().status,
        "succeeded"
    );
    let package_name = "@steven-wu/dsh-cost-meter";
    let version = "0.1.0-rc.3";
    let mut installed_lockfile = Vec::new();
    for action in ["install", "disable", "enable", "remove"] {
        let preview = service
            .preview(Dsh_operation_request {
                action: action.into(),
                package_name: Some(package_name.into()),
                version: (action == "install").then(|| version.into()),
            })
            .unwrap();
        let receipt = service.execute(preview.preview_id).unwrap();
        assert_eq!(
            receipt.status, "succeeded",
            "actual pinned pnpm {action} failed with {:?}; isolated state retained",
            receipt.error_code
        );
        let snapshot = service.snapshot().unwrap();
        let plugin = snapshot
            .plugins
            .iter()
            .find(|plugin| plugin.package_name == package_name)
            .unwrap();
        assert_eq!(
            plugin.enabled,
            action == "install" || action == "enable",
            "configured bundle state after {action}"
        );
        assert_eq!(
            plugin.installed_version.as_deref(),
            (action != "remove").then_some(version),
            "actual package identity after {action}"
        );
        assert_eq!(
            plugin.runtime_phase, "unknown",
            "package installation must not claim runtime loading"
        );
        assert_eq!(
            snapshot.runtime.phase, "stopped",
            "acceptance must never execute plugin code"
        );
        let profile = service.root.join(config::MANAGED_PROFILE);
        if action == "install" {
            installed_lockfile =
                fs::read(profile.join("pnpm-lock.yaml")).expect("real pnpm generated a lockfile");
            assert!(String::from_utf8_lossy(&installed_lockfile).contains(package_name));
            assert!(
                profile
                    .join("node_modules")
                    .join(package_name)
                    .join("lib/index.js")
                    .is_file(),
                "published plugin executable files must be materialized without running them"
            );
        }
        if action == "remove" {
            let removed_lockfile = fs::read(profile.join("pnpm-lock.yaml"))
                .expect("real pnpm regenerated the lockfile");
            assert_ne!(
                removed_lockfile, installed_lockfile,
                "reviewed dependency removal must update the previous CI lockfile"
            );
            assert!(!profile.join("node_modules").join(package_name).exists());
        }
    }
    let finished = service.snapshot().unwrap();
    assert_eq!(finished.receipts.len(), 5);
    assert!(finished
        .receipts
        .iter()
        .all(|receipt| receipt.status == "succeeded"));
    assert!(!directory.path.join(".dsh").exists());
    assert!(!directory.path.join(".claude").exists());
}
