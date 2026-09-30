use super::*;
use serde_json::json;
use std::{fs, path::PathBuf, sync::atomic::AtomicBool, time::Duration};

struct Test_directory(PathBuf);
impl Test_directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("aag_dsh_test_{}", config::operation_id()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn service(&self) -> Dsh_service {
        Dsh_service::new(self.0.clone())
    }
}
impl Drop for Test_directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn initialize(service: &Dsh_service) {
    let preview = service
        .preview(Dsh_operation_request {
            action: "initialize".into(),
            package_name: None,
            version: None,
        })
        .unwrap();
    assert_eq!(
        service.execute(preview.preview_id).unwrap().status,
        "succeeded"
    );
}

#[test]
fn snapshot_and_stop_never_initialize_the_instance() {
    let directory = Test_directory::new();
    let service = directory.service();
    let snapshot = service.snapshot().unwrap();
    assert!(!snapshot.initialized);
    assert_eq!(snapshot.runtime.phase, "stopped");
    service.stop().unwrap();
    assert!(!service.root.exists());
}

#[test]
fn initialization_requires_a_fresh_single_use_preview() {
    let directory = Test_directory::new();
    let service = directory.service();
    let request = Dsh_operation_request {
        action: "initialize".into(),
        package_name: None,
        version: None,
    };
    let preview = service.preview(request).unwrap();
    let id = preview.preview_id;
    assert!(!service.root.exists(), "preview is read-only");
    let receipt = service.execute(id.clone()).unwrap();
    assert_eq!(receipt.status, "succeeded");
    assert!(service
        .execute(id)
        .unwrap_err()
        .to_string()
        .contains("preview_not_found"));
    let snapshot = service.snapshot().unwrap();
    assert!(snapshot.initialized);
    assert_eq!(snapshot.receipts.len(), 1);
    assert!(snapshot
        .plugins
        .iter()
        .filter(|plugin| plugin.essential)
        .all(|plugin| plugin.enabled));
    assert!(snapshot
        .plugins
        .iter()
        .all(|plugin| plugin.runtime_phase == "unknown"));
    assert!(!directory.0.join(".dsh").exists());
    assert!(!directory.0.join(".claude").exists());
}

#[test]
fn settings_and_preview_detect_external_changes() {
    let directory = Test_directory::new();
    let service = directory.service();
    let old = service.snapshot().unwrap();
    let mut changed = old.settings.clone();
    changed.theme = "dark".into();
    service
        .save_settings(changed, old.settings_digest.clone())
        .unwrap();
    assert!(service
        .save_settings(old.settings, old.settings_digest)
        .unwrap_err()
        .to_string()
        .contains("settings_changed"));
    let preview = service
        .preview(Dsh_operation_request {
            action: "initialize".into(),
            package_name: None,
            version: None,
        })
        .unwrap();
    fs::write(service.root.join(config::SETTINGS_FILE), b"{}").unwrap();
    assert!(service
        .execute(preview.preview_id)
        .unwrap_err()
        .to_string()
        .contains("profile_changed"));
}

#[test]
fn registry_specs_and_core_bundle_mutations_are_rejected() {
    for version in [
        "latest",
        "^1.0.0",
        "file:../outside",
        "1.2.3;echo",
        "01.2.3",
        "1.2",
        "1.2.3\n",
    ] {
        assert!(!catalog::exact_version(version), "{version}");
    }
    for version in ["1.2.3", "0.1.0-rc.3", "1.2.3+build.1"] {
        assert!(catalog::exact_version(version));
    }
    let directory = Test_directory::new();
    let service = directory.service();
    initialize(&service);
    assert!(service
        .preview(Dsh_operation_request {
            action: "install".into(),
            package_name: Some("../../outside".into()),
            version: Some("1.0.0".into())
        })
        .unwrap_err()
        .to_string()
        .contains("package_not_allowlisted"));
    assert!(service
        .preview(Dsh_operation_request {
            action: "disable".into(),
            package_name: Some("@deepseek-ai/dsh-base".into()),
            version: None
        })
        .unwrap_err()
        .to_string()
        .contains("essential_bundle_protected"));
}

#[test]
fn cross_process_lock_is_exclusive_and_stale_files_are_reusable() {
    let directory = Test_directory::new();
    let service = directory.service();
    let lock = config::Instance_lock::acquire(&service.root).unwrap();
    assert!(config::Instance_lock::acquire(&service.root)
        .err()
        .unwrap()
        .to_string()
        .contains("instance_locked"));
    drop(lock);
    assert!(directory.0.join("dsh_instance.lock").exists());
    fs::write(
        directory.0.join("dsh_instance.lock"),
        b"previous_crashed_process",
    )
    .unwrap();
    let recovered = config::Instance_lock::acquire(&service.root).unwrap();
    drop(recovered);
    assert_ne!(
        fs::read(directory.0.join("dsh_instance.lock")).unwrap(),
        b"previous_crashed_process"
    );
}
#[test]
fn failed_install_preserves_active_manifest_and_writes_sanitized_receipt() {
    let directory = Test_directory::new();
    let service = directory.service();
    initialize(&service);
    let manifest_path = service
        .root
        .join(config::MANAGED_PROFILE)
        .join("package.json");
    let before = fs::read(&manifest_path).unwrap();
    let preview = service
        .preview(Dsh_operation_request {
            action: "install".into(),
            package_name: Some("@steven-wu/dsh-cost-meter".into()),
            version: Some("0.1.0-rc.3".into()),
        })
        .unwrap();
    let receipt = service.execute(preview.preview_id).unwrap();
    assert_eq!(receipt.status, "failed");
    assert_eq!(receipt.error_code.as_deref(), Some("node_path_required"));
    assert_eq!(receipt.rollback_status, "not_needed");
    assert_eq!(fs::read(manifest_path).unwrap(), before);
    let wire = serde_json::to_string(&receipt).unwrap();
    assert!(!wire.contains(directory.0.to_str().unwrap()));
    assert!(!wire.contains("stdout") && !wire.contains("stderr"));
    assert!(service.snapshot().unwrap().job.is_none());
}

fn test_node() -> Option<PathBuf> {
    let override_path = std::env::var_os("AAG_DSH_TEST_NODE").map(PathBuf::from);
    if let Some(path) = override_path.filter(|path| path.is_file()) {
        return Some(path);
    }
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|path| path.join(if cfg!(windows) { "node.exe" } else { "node" }))
        .find(|path| path.is_file())
}

fn configure_fake_pnpm(
    directory: &Test_directory,
    service: &Dsh_service,
    source: &str,
) -> Option<Dsh_settings> {
    let node = test_node()?;
    let tool_dir = directory.0.join("fake_pnpm");
    fs::create_dir(&tool_dir).unwrap();
    fs::write(
        tool_dir.join("package.json"),
        serde_json::to_vec(&json!({"name":"pnpm","version":runtime::SUPPORTED_PNPM_VERSION,"bin":{"pnpm":"pnpm.cjs"}}))
            .unwrap(),
    )
    .unwrap();
    fs::write(tool_dir.join("pnpm.cjs"), source).unwrap();
    let old = service.snapshot().unwrap();
    let mut settings = old.settings;
    settings.node_path = node.to_string_lossy().into_owned();
    settings.pnpm_entry_path = tool_dir.join("pnpm.cjs").to_string_lossy().into_owned();
    service
        .save_settings(settings.clone(), old.settings_digest)
        .unwrap();
    Some(settings)
}

#[test]
fn staged_install_enable_disable_remove_roundtrip_uses_real_node_process() {
    let directory = Test_directory::new();
    let service = directory.service();
    initialize(&service);
    let Some(_) = configure_fake_pnpm(
        &directory,
        &service,
        r#"
const fs = require('node:fs'); const path = require('node:path');
for (const name of ['HOME', 'USERPROFILE', 'APPDATA', 'LOCALAPPDATA', 'XDG_CONFIG_HOME', 'XDG_CACHE_HOME']) {
 if (!process.env[name] || !fs.statSync(process.env[name]).isDirectory()) process.exit(13);
}
if (process.platform === 'win32' && (!/^[A-Za-z]:$/.test(process.env.SystemDrive || '') || !fs.existsSync(path.join(process.env.USERPROFILE, 'Desktop')))) process.exit(14);
if (!process.argv.includes('--ignore-scripts') || !process.argv.includes('--config.ignore-pnpmfile=true')) process.exit(9);
const manifest = JSON.parse(fs.readFileSync('package.json','utf8'));
fs.mkdirSync('node_modules',{recursive:true});
for(const [name,version] of Object.entries(manifest.dependencies)) {
 const dir=path.join('node_modules',name);fs.mkdirSync(dir,{recursive:true});
 fs.writeFileSync(path.join(dir,'package.json'),JSON.stringify({name,version,dsh:{bundle:{patch:'./cordis.patch.yml'}}}));
 fs.writeFileSync(path.join(dir,'cordis.patch.yml'),'[]');
}
fs.writeFileSync('pnpm-lock.yaml','lockfileVersion: 9\n');
"#,
    ) else {
        panic!("Node is required for DSH process tests; set AAG_DSH_TEST_NODE");
    };
    for action in ["install", "disable", "enable", "remove"] {
        let preview = service
            .preview(Dsh_operation_request {
                action: action.into(),
                package_name: Some("@steven-wu/dsh-cost-meter".into()),
                version: (action == "install").then(|| "0.1.0-rc.3".into()),
            })
            .unwrap();
        let receipt = service.execute(preview.preview_id).unwrap();
        assert_eq!(
            receipt.status, "succeeded",
            "{action}: {:?}",
            receipt.error_code
        );
        let plugin = service
            .snapshot()
            .unwrap()
            .plugins
            .into_iter()
            .find(|plugin| plugin.plugin_id == "cost_meter")
            .unwrap();
        assert_eq!(plugin.enabled, action == "install" || action == "enable");
        assert_eq!(plugin.installed_version.is_some(), action != "remove");
        assert_eq!(plugin.runtime_phase, "unknown");
    }
}

#[test]
fn cancellation_kills_a_real_package_process_and_preserves_manifest() {
    let directory = Test_directory::new();
    let service = directory.service();
    initialize(&service);
    let Some(_) = configure_fake_pnpm(&directory, &service, "setInterval(()=>{},1000);") else {
        panic!("Node is required for DSH cancellation tests; set AAG_DSH_TEST_NODE");
    };
    let before = fs::read(
        service
            .root
            .join(config::MANAGED_PROFILE)
            .join("package.json"),
    )
    .unwrap();
    let preview = service
        .preview(Dsh_operation_request {
            action: "install".into(),
            package_name: Some("@steven-wu/dsh-cost-meter".into()),
            version: Some("0.1.0-rc.3".into()),
        })
        .unwrap();
    let executing = service.clone();
    let worker = std::thread::spawn(move || executing.execute(preview.preview_id).unwrap());
    for _ in 0..100 {
        if service.snapshot().unwrap().job.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(service.cancel());
    assert_eq!(worker.join().unwrap().status, "cancelled");
    assert_eq!(
        fs::read(
            service
                .root
                .join(config::MANAGED_PROFILE)
                .join("package.json")
        )
        .unwrap(),
        before
    );
}

#[test]
fn process_timeout_is_bounded_and_returns_no_child_output() {
    let Some(node) = test_node() else {
        panic!("Node is required for DSH timeout tests; set AAG_DSH_TEST_NODE");
    };
    let mut command = std::process::Command::new(node);
    command.args([
        "-e",
        "console.error('fake_secret_do_not_surface');setInterval(()=>{},1000)",
    ]);
    let error = owned_process::run_bounded(command, 0, &AtomicBool::new(false)).unwrap_err();
    assert_eq!(error.to_string(), "dsh:timed_out");
}

fn configure_fake_dsh(
    directory: &Test_directory,
    service: &Dsh_service,
    delay_ms: u64,
) -> Dsh_snapshot {
    let node = test_node().expect("Node is required for DSH runtime tests; set AAG_DSH_TEST_NODE");
    let tool_dir = directory.0.join("fake_dsh");
    fs::create_dir_all(&tool_dir).unwrap();
    fs::write(
        tool_dir.join("package.json"),
        serde_json::to_vec(
            &json!({"name":"@deepseek-ai/dsh","version":runtime::SUPPORTED_DSH_VERSION,"bin":{"dsh":"index.cjs"}}),
        )
        .unwrap(),
    )
    .unwrap();
    let source = format!(
        r#"
const http = require('node:http');
const fs = require('node:fs');
const path = require('node:path');
if (process.platform === 'win32' && (!/^[A-Za-z]:$/.test(process.env.SystemDrive || '') || !fs.existsSync(path.join(process.env.USERPROFILE, 'Desktop')))) process.exit(14);
const patch_index = process.argv.indexOf('--patch');
const host_index = process.argv.indexOf('--host');
if (patch_index < 0 || patch_index > host_index) process.exit(11);
const overlay = JSON.parse(fs.readFileSync(process.argv[patch_index + 1], 'utf8'));
if (overlay[0].id !== 'settings' || !overlay[0].config.path.endsWith('settings.json')) process.exit(12);
const changed_settings = JSON.parse(fs.readFileSync(overlay[0].config.path, 'utf8'));
changed_settings.fixture_user_preferences = {{preserved:true}};
fs.writeFileSync(overlay[0].config.path, JSON.stringify(changed_settings));
const port = Number(process.argv[process.argv.indexOf('--port') + 1]);
setTimeout(() => http.createServer((request,response) => {{
 response.writeHead(200,{{'content-type':'text/html'}});
 response.end('<html><script>window.__DSH_BOOT__={{}};</script></html>');
}}).listen(port,'127.0.0.1'), {delay_ms});
"#
    );
    fs::write(tool_dir.join("index.cjs"), source).unwrap();
    let old = service.snapshot().unwrap();
    let mut settings = old.settings;
    settings.node_path = node.to_string_lossy().into_owned();
    settings.dsh_entry_path = tool_dir.join("index.cjs").to_string_lossy().into_owned();
    settings.port = std::net::TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    settings.startup_timeout_secs = 5;
    settings.theme = "dark".into();
    service
        .save_settings(settings, old.settings_digest)
        .unwrap()
}

#[tokio::test]
async fn authenticated_runtime_rejects_foreign_requests_and_clears_deselected_model() {
    let directory = Test_directory::new();
    let service = directory.service();
    initialize(&service);
    let settings = configure_fake_dsh(&directory, &service, 0);
    let secret = "fixture_provider_secret";
    let snapshot = service.start(json!({"llm-pi-ai":{"providers":{"aag":{"apiKeyEnv":"AAG_DSH_API_KEY"}}},"agent-default-model":{"provider":"aag","model":"fixture"}}), BTreeMap::from([("AAG_DSH_API_KEY".into(),secret.into())]), settings.settings_digest.clone()).await.unwrap();
    assert_eq!(snapshot.runtime.phase, "web_ready");
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    assert_eq!(
        client
            .get(snapshot.runtime.web_url.as_ref().unwrap())
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::FORBIDDEN
    );
    let bootstrap = service.validated_web_url().unwrap();
    assert!(bootstrap.contains("/__aag_bootstrap?token="));
    let wire = serde_json::to_string(&snapshot).unwrap();
    assert!(!wire.contains(secret) && !wire.contains("?token="));
    let settings_bytes = fs::read(service.root.join("settings.json")).unwrap();
    assert!(!String::from_utf8_lossy(&settings_bytes).contains(secret));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&settings_bytes).unwrap()["ui-theme"]
            ["preference"],
        "dark"
    );
    service.stop().unwrap();
    service
        .start(json!({}), BTreeMap::new(), settings.settings_digest)
        .await
        .unwrap();
    let final_settings = config::read_json(&service.root.join("settings.json"))
        .unwrap()
        .unwrap();
    assert!(final_settings.get("llm-pi-ai").is_none());
    assert!(final_settings.get("agent-default-model").is_none());
    assert_eq!(
        final_settings["fixture_user_preferences"]["preserved"],
        true
    );
    assert!(!service.root.join("settings.yaml").exists());
    service.stop().unwrap();
}

#[tokio::test]
async fn stopped_startup_cannot_replace_or_kill_a_new_runtime_generation() {
    let directory = Test_directory::new();
    let service = directory.service();
    initialize(&service);
    let snapshot = configure_fake_dsh(&directory, &service, 300);
    let first_service = service.clone();
    let first_digest = snapshot.settings_digest.clone();
    let first = tokio::spawn(async move {
        first_service
            .start(json!({}), BTreeMap::new(), first_digest)
            .await
    });
    for _ in 0..100 {
        if service.snapshot().unwrap().runtime.phase == "starting" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(service.snapshot().unwrap().runtime.phase, "starting");
    service.stop().unwrap();
    let replacement = service
        .start(json!({}), BTreeMap::new(), snapshot.settings_digest)
        .await
        .unwrap();
    assert_eq!(replacement.runtime.phase, "web_ready");
    assert!(first
        .await
        .unwrap()
        .unwrap_err()
        .to_string()
        .contains("runtime_generation_changed"));
    tokio::time::sleep(Duration::from_millis(350)).await;
    let current = service.snapshot().unwrap();
    assert_eq!(current.runtime.pid, replacement.runtime.pid);
    assert_eq!(current.runtime.phase, "web_ready");
    service.stop().unwrap();
}

#[tokio::test]
async fn startup_rejects_changed_settings_without_spawning() {
    let directory = Test_directory::new();
    let service = directory.service();
    initialize(&service);
    let old = configure_fake_dsh(&directory, &service, 0);
    let mut changed = old.settings;
    changed.theme = "light".into();
    service
        .save_settings(changed, old.settings_digest.clone())
        .unwrap();
    assert!(service
        .start(json!({}), BTreeMap::new(), old.settings_digest)
        .await
        .unwrap_err()
        .to_string()
        .contains("settings_changed"));
    assert_eq!(service.snapshot().unwrap().runtime.phase, "stopped");
    assert!(!service.root.join("settings.json").exists());
}

#[test]
fn interrupted_profile_promotion_restores_original_or_reports_rollback_failure() {
    for break_rollback in [false, true] {
        let directory = Test_directory::new();
        let service = directory.service();
        initialize(&service);
        configure_fake_pnpm(&directory, &service, r#"
const fs=require('node:fs'),path=require('node:path');
const manifest=JSON.parse(fs.readFileSync('package.json','utf8'));
fs.mkdirSync('node_modules',{recursive:true});
for(const [name,version] of Object.entries(manifest.dependencies)) {
 const dir=path.join('node_modules',name);fs.mkdirSync(dir,{recursive:true});
 fs.writeFileSync(path.join(dir,'package.json'),JSON.stringify({name,version,dsh:{bundle:{patch:'./cordis.patch.yml'}}}));
 fs.writeFileSync(path.join(dir,'cordis.patch.yml'),'[]');
}
fs.writeFileSync('pnpm-lock.yaml','lockfileVersion: 9\n');
"#).expect("Node is required for rollback tests; set AAG_DSH_TEST_NODE");
        let install = service
            .preview(Dsh_operation_request {
                action: "install".into(),
                package_name: Some("@steven-wu/dsh-cost-meter".into()),
                version: Some("0.1.0-rc.3".into()),
            })
            .unwrap();
        assert_eq!(
            service.execute(install.preview_id).unwrap().status,
            "succeeded"
        );
        let profile = service.root.join(config::MANAGED_PROFILE);
        let before = fs::read(profile.join("package.json")).unwrap();
        let remove = service
            .preview(Dsh_operation_request {
                action: "remove".into(),
                package_name: Some("@steven-wu/dsh-cost-meter".into()),
                version: None,
            })
            .unwrap();
        // A deterministic real-filesystem failure after the manifest and
        // lockfile have already been promoted, without weakening production IO.
        let journal = service.root.join("operations").join(&remove.preview_id);
        fs::create_dir_all(&journal).unwrap();
        fs::create_dir(journal.join("backup_node_modules")).unwrap();
        fs::write(
            journal.join("backup_node_modules/obstruction"),
            b"keep_destination_nonempty",
        )
        .unwrap();
        if break_rollback {
            fs::create_dir(journal.join("failed_package.json")).unwrap();
            fs::write(
                journal.join("failed_package.json/obstruction"),
                b"keep_destination_nonempty",
            )
            .unwrap();
        }
        let receipt = service.execute(remove.preview_id).unwrap();
        assert_eq!(receipt.error_code.as_deref(), Some("backup_failed"));
        if break_rollback {
            assert_eq!(receipt.status, "rollback_failed");
            assert_eq!(receipt.rollback_status, "failed");
            assert_eq!(
                fs::read(journal.join("backup_package.json")).unwrap(),
                before
            );
        } else {
            assert_eq!(receipt.status, "failed");
            assert_eq!(receipt.rollback_status, "restored");
            assert_eq!(fs::read(profile.join("package.json")).unwrap(), before);
            let plugin = service
                .snapshot()
                .unwrap()
                .plugins
                .into_iter()
                .find(|plugin| plugin.plugin_id == "cost_meter")
                .unwrap();
            assert_eq!(plugin.installed_version.as_deref(), Some("0.1.0-rc.3"));
            assert!(plugin.enabled);
        }
    }
}

#[test]
fn crash_markers_block_mutations_only_after_live_promotion_begins() {
    let directory = Test_directory::new();
    let service = directory.service();
    initialize(&service);
    let snapshot = service.snapshot().unwrap();
    let journal = service.root.join("operations/crash_fixture");
    fs::create_dir_all(journal.join("stage")).unwrap();
    fs::write(journal.join("request.json"), b"{}").unwrap();
    plugins::ensure_recovered(&service.root).unwrap();
    fs::write(journal.join("commit_started.json"), b"{}").unwrap();
    assert!(plugins::ensure_recovered(&service.root)
        .unwrap_err()
        .to_string()
        .contains("interrupted_transaction"));
    assert!(
        service.snapshot().unwrap().initialized,
        "inspection remains available"
    );
    assert!(service
        .save_settings(snapshot.settings, snapshot.settings_digest)
        .unwrap_err()
        .to_string()
        .contains("interrupted_transaction"));
    assert!(service
        .preview(Dsh_operation_request {
            action: "install".into(),
            package_name: Some("@steven-wu/dsh-cost-meter".into()),
            version: Some("0.1.0-rc.3".into())
        })
        .unwrap_err()
        .to_string()
        .contains("interrupted_transaction"));
    fs::write(
        journal.join("commit_finished.json"),
        br#"{"status":"restored"}"#,
    )
    .unwrap();
    plugins::ensure_recovered(&service.root).unwrap();
}

#[test]
fn runtime_execution_rejects_an_undeclared_package_script() {
    let directory = Test_directory::new();
    let service = directory.service();
    let mut settings = configure_fake_pnpm(&directory, &service, "process.exit(0)")
        .expect("Node is required for runtime entry validation");
    runtime::validate_executables(&settings, false).unwrap();
    let undeclared = directory.0.join("fake_pnpm/other.cjs");
    fs::write(&undeclared, "throw new Error('must_not_run')").unwrap();
    settings.pnpm_entry_path = undeclared.to_string_lossy().into_owned();
    assert_eq!(
        runtime::validate_executables(&settings, false)
            .unwrap_err()
            .to_string(),
        "dsh:runtime_entry_not_declared"
    );
}

#[test]
fn enabling_an_existing_bundle_rejects_a_patch_outside_its_package() {
    let directory = Test_directory::new();
    let service = directory.service();
    initialize(&service);
    let profile = service.root.join(config::MANAGED_PROFILE);
    let package_name = "@steven-wu/dsh-cost-meter";
    let package = profile.join("node_modules").join(package_name);
    fs::create_dir_all(&package).unwrap();
    fs::write(profile.join("node_modules/outside.patch.yml"), b"[]").unwrap();
    fs::write(
        package.join("package.json"),
        serde_json::to_vec(&json!({
            "name": package_name, "version": "0.1.0-rc.3",
            "dsh": {"bundle": {"patch": "../../outside.patch.yml"}}
        }))
        .unwrap(),
    )
    .unwrap();
    let before = fs::read(profile.join("package.json")).unwrap();
    assert_eq!(
        service
            .preview(Dsh_operation_request {
                action: "enable".into(),
                package_name: Some(package_name.into()),
                version: None,
            })
            .unwrap_err()
            .to_string(),
        "dsh:bundle_patch_escape"
    );
    assert_eq!(fs::read(profile.join("package.json")).unwrap(), before);
}

#[tokio::test]
async fn aborted_startup_reaps_owned_child_and_releases_instance_lock() {
    let directory = Test_directory::new();
    let service = directory.service();
    initialize(&service);
    let settings = configure_fake_dsh(&directory, &service, 5000);
    let starting_service = service.clone();
    let startup = tokio::spawn(async move {
        starting_service
            .start(json!({}), BTreeMap::new(), settings.settings_digest)
            .await
    });
    for _ in 0..100 {
        if service.snapshot().unwrap().runtime.phase == "starting" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(service.snapshot().unwrap().runtime.phase, "starting");
    let child_pid = service.snapshot().unwrap().runtime.pid.unwrap();
    startup.abort();
    assert!(startup.await.unwrap_err().is_cancelled());
    let snapshot = service.snapshot().unwrap();
    assert_eq!(snapshot.runtime.phase, "failed");
    assert_eq!(
        snapshot.runtime.error_code.as_deref(),
        Some("startup_cancelled")
    );
    assert!(snapshot.runtime.pid.is_none());
    assert!(service.state.lock().unwrap().child.is_none());
    drop(config::Instance_lock::acquire(&service.root).unwrap());
    let probe = std::process::Command::new(test_node().unwrap())
        .args(["-e", "try{process.kill(Number(process.argv[1]),0);process.exit(1)}catch(error){process.exit(error.code==='ESRCH'?0:2)}"])
        .arg(child_pid.to_string()).output().unwrap();
    assert!(
        probe.status.success(),
        "the cancelled startup child must be reaped"
    );
}

#[test]
fn bundle_enable_preview_is_invalidated_by_patch_content_changes() {
    let directory = Test_directory::new();
    let service = directory.service();
    initialize(&service);
    let profile = service.root.join(config::MANAGED_PROFILE);
    let package_name = "@steven-wu/dsh-cost-meter";
    let package = profile.join("node_modules").join(package_name);
    fs::create_dir_all(&package).unwrap();
    fs::write(
        package.join("package.json"),
        serde_json::to_vec(&json!({
            "name": package_name, "version": "0.1.0-rc.3",
            "dsh": {"bundle": {"patch": "./bundle.patch.yml"}}
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(package.join("bundle.patch.yml"), b"[]").unwrap();
    let before = fs::read(profile.join("package.json")).unwrap();
    let preview = service
        .preview(Dsh_operation_request {
            action: "enable".into(),
            package_name: Some(package_name.into()),
            version: None,
        })
        .unwrap();
    fs::write(
        package.join("bundle.patch.yml"),
        b"[{\"id\":\"unreviewed\"}]",
    )
    .unwrap();
    assert_eq!(
        service
            .execute(preview.preview_id.clone())
            .unwrap_err()
            .to_string(),
        "dsh:profile_changed"
    );
    assert_eq!(
        service.execute(preview.preview_id).unwrap_err().to_string(),
        "dsh:preview_not_found"
    );
    assert_eq!(fs::read(profile.join("package.json")).unwrap(), before);
}

#[test]
fn installed_package_identity_mismatch_is_rejected() {
    let directory = Test_directory::new();
    let service = directory.service();
    initialize(&service);
    let profile = service.root.join(config::MANAGED_PROFILE);
    let package_name = "@steven-wu/dsh-cost-meter";
    let package = profile.join("node_modules").join(package_name);
    fs::create_dir_all(&package).unwrap();
    fs::write(
        package.join("package.json"),
        br#"{"name":"different_package","version":"1.0.0"}"#,
    )
    .unwrap();
    assert_eq!(
        catalog::installed_manifest(&profile, package_name)
            .unwrap_err()
            .to_string(),
        "dsh:installed_identity_mismatch"
    );
}

#[test]
fn managed_module_root_rejects_directory_links() {
    let directory = Test_directory::new();
    let service = directory.service();
    initialize(&service);
    let profile = service.root.join(config::MANAGED_PROFILE);
    let outside = directory.0.join("outside_modules");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("sentinel"), b"must_remain_unchanged").unwrap();
    let link = profile.join("node_modules");
    let linked = std::process::Command::new(test_node().expect("Node is required for linked-path validation"))
        .args(["-e", "require('node:fs').symlinkSync(process.argv[1],process.argv[2],process.platform==='win32'?'junction':'dir')"])
        .arg(&outside).arg(&link).output().unwrap();
    assert!(
        linked.status.success(),
        "could not create the owned directory-link fixture"
    );
    let result = catalog::installed_manifest(&profile, "@steven-wu/dsh-cost-meter");
    #[cfg(windows)]
    fs::remove_dir(&link).unwrap();
    #[cfg(unix)]
    fs::remove_file(&link).unwrap();
    assert_eq!(result.unwrap_err().to_string(), "dsh:linked_managed_path");
    assert_eq!(
        fs::read(outside.join("sentinel")).unwrap(),
        b"must_remain_unchanged"
    );
}

#[test]
fn already_cancelled_package_work_does_not_attempt_to_spawn() {
    let directory = Test_directory::new();
    let command = std::process::Command::new(directory.0.join("missing_executable"));
    assert_eq!(
        owned_process::run_bounded(command, 5, &AtomicBool::new(true))
            .unwrap_err()
            .to_string(),
        "dsh:cancelled"
    );
}
