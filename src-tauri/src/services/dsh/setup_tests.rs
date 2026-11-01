use super::*;
use crate::services::dsh::{discovery::Dsh_runtime_candidate, runtime};

struct Fixture {
    directory: PathBuf,
    service: Dsh_service,
    discovery: Dsh_discovery,
}

impl Fixture {
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("aag_setup_test_{}", config::operation_id()));
        let runtime_root = directory.join("runtime");
        fs::create_dir_all(&runtime_root).unwrap();
        let node = runtime_root.join(if cfg!(windows) { "node.exe" } else { "node" });
        fs::write(node, if cfg!(windows) { b"MZ\0\0" } else { b"\x7fELF" }).unwrap();
        for (name, kind, version) in [
            ("@deepseek-ai/dsh", "dsh", runtime::SUPPORTED_DSH_VERSION),
            ("pnpm", "pnpm", runtime::SUPPORTED_PNPM_VERSION),
        ] {
            let package = runtime_root.join("node_modules").join(name);
            fs::create_dir_all(package.join("bin")).unwrap();
            fs::write(
                package.join("package.json"),
                json!({"name":name,"version":version,"bin":{kind:format!("bin/{kind}.mjs")}})
                    .to_string(),
            )
            .unwrap();
            fs::write(
                package.join(format!("bin/{kind}.mjs")),
                b"throw new Error('do not execute');",
            )
            .unwrap();
        }
        let discovery = discovery::discover(
            &Dsh_settings::default(),
            &Discovery_environment {
                deployment_root: Some(runtime_root),
                ..Default::default()
            },
        );
        Self {
            service: Dsh_service::new(directory.join("managed")),
            directory,
            discovery,
        }
    }

    fn request(&self) -> Dsh_setup_request {
        Dsh_setup_request {
            expected_digest: self.service.snapshot().unwrap().settings_digest,
            upstream_id: None,
            model_id: None,
        }
    }

    fn preview(
        &self,
        sources: &[Upstream_option],
        preferred: Option<&str>,
        model: Option<&str>,
    ) -> Dsh_setup_plan {
        self.service
            .preview_setup_with_discovery(
                self.request(),
                sources,
                preferred,
                model,
                self.discovery.clone(),
            )
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn upstream(id: &str) -> Upstream_option {
    Upstream_option {
        id: id.into(),
        name: "Saved source".into(),
        protocol: "anthropic-messages".into(),
        models: vec!["existing-model".into()],
        source_kind: "claude_native".into(),
        available: true,
        reason: None,
    }
}

#[test]
fn first_setup_is_readonly_until_reviewed_then_initializes_without_process_or_native_writes() {
    let fixture = Fixture::new();
    let sources = vec![upstream("claude:saved")];
    let plan = fixture.preview(&sources, None, None);
    assert!(plan.can_apply);
    assert_eq!(plan.source_reason, "unique_available");
    assert!(plan
        .changes
        .iter()
        .any(|change| change == "initialize_profile"));
    assert!(!fixture.service.root.exists());
    let snapshot = fixture
        .service
        .apply_setup(plan.plan_id.clone(), &sources)
        .unwrap();
    assert!(snapshot.initialized);
    assert_eq!(
        snapshot.settings.upstream_id.as_deref(),
        Some("claude:saved")
    );
    assert_eq!(
        snapshot.settings.model_id.as_deref(),
        Some("existing-model")
    );
    assert_eq!(snapshot.runtime.phase, "stopped");
    assert!(!fixture.directory.join(".claude").exists());
    assert!(!fixture.directory.join(".dsh").exists());
    assert!(!fixture.service.root.join("workspace").exists());
    assert!(fixture
        .service
        .apply_setup(plan.plan_id, &sources)
        .unwrap_err()
        .to_string()
        .contains("setup_plan_not_found"));
}

#[test]
fn preferred_native_source_and_its_configured_model_win_without_changing_family() {
    let fixture = Fixture::new();
    let mut current = upstream("claude:current");
    current.models.push("current-model".into());
    let sources = vec![upstream("claude:other"), current];
    let plan = fixture.preview(&sources, Some("claude:current"), Some("current-model"));
    assert!(plan.can_apply);
    assert_eq!(plan.source_reason, "current_claude");
    assert_eq!(plan.settings.upstream_id.as_deref(), Some("claude:current"));
    assert_eq!(plan.settings.model_id.as_deref(), Some("current-model"));
}

#[test]
fn unavailable_current_source_blocks_instead_of_substituting_another_source() {
    let fixture = Fixture::new();
    let mut current = upstream("claude:current");
    current.available = false;
    current.reason = Some("DSH_CREDENTIAL_UNAVAILABLE".into());
    let plan = fixture.preview(
        &[current, upstream("claude:other")],
        Some("claude:current"),
        None,
    );
    assert!(!plan.can_apply);
    assert_eq!(plan.settings.upstream_id.as_deref(), Some("claude:current"));
    assert!(plan
        .blockers
        .iter()
        .any(|code| code == "upstream_unavailable"));
}

#[test]
fn multiple_sources_or_models_require_selection_without_first_entry_fallback() {
    let fixture = Fixture::new();
    let plan = fixture.preview(&[upstream("claude:a"), upstream("claude:b")], None, None);
    assert!(!plan.can_apply);
    assert!(plan.settings.upstream_id.is_none());
    assert!(plan
        .blockers
        .iter()
        .any(|code| code == "upstream_selection_required"));
    let mut source = upstream("claude:a");
    source.models.push("second-model".into());
    let plan = fixture.preview(&[source], None, None);
    assert!(!plan.can_apply);
    assert!(plan.settings.model_id.is_none());
    assert!(plan
        .blockers
        .iter()
        .any(|code| code == "model_selection_required"));
}

#[test]
fn existing_settings_are_preserved_and_conflicting_request_is_blocked() {
    let fixture = Fixture::new();
    let sources = vec![upstream("claude:saved"), upstream("claude:current")];
    let mut settings = Dsh_settings {
        node_path: fixture.discovery.recommended.node_path.clone(),
        dsh_entry_path: fixture.discovery.recommended.dsh_entry_path.clone(),
        pnpm_entry_path: fixture.discovery.recommended.pnpm_entry_path.clone(),
        upstream_id: Some("claude:saved".into()),
        model_id: Some("existing-model".into()),
        theme: "dark".into(),
        port: 4999,
        startup_timeout_secs: 77,
        ..Default::default()
    };
    fixture
        .service
        .save_settings(settings.clone(), fixture.request().expected_digest)
        .unwrap();
    let plan = fixture.preview(&sources, Some("claude:current"), None);
    assert_eq!(plan.settings, settings);
    assert_eq!(plan.source_reason, "preserved");
    let mut request = fixture.request();
    request.upstream_id = Some("claude:current".into());
    let plan = fixture
        .service
        .preview_setup_with_discovery(request, &sources, None, None, fixture.discovery.clone())
        .unwrap();
    assert!(!plan.can_apply);
    assert!(plan
        .blockers
        .iter()
        .any(|code| code == "explicit_selection_conflict"));
    assert_eq!(plan.settings, settings);
    settings.node_path = fixture
        .directory
        .join("missing/node.exe")
        .to_string_lossy()
        .into_owned();
    let missing_plan = build_plan(
        settings.clone(),
        fixture.request(),
        fixture.discovery.clone(),
        &sources,
        None,
        None,
        false,
    );
    assert_eq!(missing_plan.settings.node_path, settings.node_path);
    assert!(missing_plan
        .blockers
        .iter()
        .any(|code| code == "runtime_node_invalid"));
}

#[test]
fn windows_path_case_differences_do_not_invalidate_a_discovered_runtime() {
    let fixture = Fixture::new();
    let sources = vec![upstream("claude:current")];
    let mut settings = fixture.preview(&sources, None, None).settings;
    if !cfg!(windows) {
        // Case-sensitive filesystems: a different case IS a different file.
        return;
    }
    settings.node_path = settings.node_path.to_uppercase();
    let plan = build_plan(
        settings.clone(),
        fixture.request(),
        fixture.discovery.clone(),
        &sources,
        None,
        None,
        false,
    );
    assert_eq!(plan.settings.node_path, settings.node_path);
    assert!(!plan
        .blockers
        .iter()
        .any(|code| code == "runtime_node_invalid"));
}

#[test]
fn expired_stale_and_changed_upstream_plans_fail_without_settings_writes() {
    let fixture = Fixture::new();
    let sources = vec![upstream("claude:saved")];
    let plan = fixture.preview(&sources, None, None);
    fixture
        .service
        .state
        .lock()
        .unwrap()
        .setup_plans
        .get_mut(&plan.plan_id)
        .unwrap()
        .plan
        .expires_at_ms = 0;
    assert!(fixture
        .service
        .apply_setup(plan.plan_id, &sources)
        .unwrap_err()
        .to_string()
        .contains("setup_plan_expired"));
    assert!(!fixture.service.root.exists());
    let plan = fixture.preview(&sources, None, None);
    fs::create_dir_all(&*fixture.service.root).unwrap();
    fs::write(
        fixture.service.root.join(SETTINGS_FILE),
        serde_json::to_vec(&Dsh_settings {
            theme: "dark".into(),
            ..Default::default()
        })
        .unwrap(),
    )
    .unwrap();
    assert!(fixture
        .service
        .apply_setup(plan.plan_id, &sources)
        .unwrap_err()
        .to_string()
        .contains("settings_changed"));
    let plan = fixture.preview(&sources, None, None);
    let mut changed_sources = sources.clone();
    changed_sources[0].models = vec!["changed-model".into()];
    assert!(fixture
        .service
        .apply_setup(plan.plan_id, &changed_sources)
        .unwrap_err()
        .to_string()
        .contains("setup_upstream_changed"));
    assert!(!fixture.service.root.join(MANAGED_PROFILE).exists());
}

#[test]
fn changed_runtime_metadata_after_review_is_not_applied() {
    let fixture = Fixture::new();
    let sources = vec![upstream("claude:saved")];
    let plan = fixture.preview(&sources, None, None);
    fs::write(
        Path::new(&plan.settings.dsh_entry_path)
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("package.json"),
        b"{}",
    )
    .unwrap();
    assert!(fixture
        .service
        .apply_setup(plan.plan_id, &sources)
        .unwrap_err()
        .to_string()
        .contains("setup_runtime_changed"));
    assert!(!fixture.service.root.exists());
}

#[test]
fn setup_failure_after_settings_promotion_restores_original_bytes_and_keeps_journal() {
    let fixture = Fixture::new();
    let settings = Dsh_settings {
        theme: "dark".into(),
        ..Default::default()
    };
    fixture
        .service
        .save_settings(settings, fixture.request().expected_digest)
        .unwrap();
    let original = fs::read(fixture.service.root.join(SETTINGS_FILE)).unwrap();
    let plan = fixture.preview(&[upstream("claude:saved")], None, None);
    let mut state = fixture.service.state.lock().unwrap();
    let pending = state.setup_plans.remove(&plan.plan_id).unwrap();
    let _instance_lock = Instance_lock::acquire(&fixture.service.root).unwrap();
    assert!(commit_setup(&fixture.service.root, &pending, true)
        .unwrap_err()
        .to_string()
        .contains("setup_injected_failure"));
    assert_eq!(
        fs::read(fixture.service.root.join(SETTINGS_FILE)).unwrap(),
        original
    );
    assert!(!fixture
        .service
        .root
        .join(MANAGED_PROFILE)
        .join("package.json")
        .exists());
    assert_eq!(
        config::read_json(
            &fixture
                .service
                .root
                .join("operations")
                .join(plan.plan_id)
                .join("commit_finished.json")
        )
        .unwrap()
        .unwrap()["status"],
        "restored"
    );
    plugins::ensure_recovered(&fixture.service.root).unwrap();
}

#[test]
fn setup_missing_pnpm_warns_but_no_fake_node_or_missing_dsh_is_accepted() {
    let fixture = Fixture::new();
    let sources = vec![upstream("claude:saved")];
    let mut discovery = fixture.discovery.clone();
    discovery.recommended.pnpm_entry_path.clear();
    discovery
        .candidates
        .retain(|candidate| candidate.kind != "pnpm");
    let plan = build_plan(
        Dsh_settings::default(),
        fixture.request(),
        discovery.clone(),
        &sources,
        None,
        None,
        true,
    );
    assert!(plan.can_apply);
    assert!(plan.warnings.iter().any(|code| code == "pnpm_not_found"));
    discovery.recommended.node_path.clear();
    discovery
        .candidates
        .retain(|candidate| candidate.kind != "node");
    discovery.candidates.push(Dsh_runtime_candidate {
        candidate_id: "fake".into(),
        kind: "node".into(),
        path: "/fake/node".into(),
        source: "path_sibling".into(),
        version: None,
        compatible: false,
        validation: "rejected".into(),
        reason: Some("node_binary_format_invalid".into()),
    });
    let plan = build_plan(
        Dsh_settings::default(),
        fixture.request(),
        discovery,
        &sources,
        None,
        None,
        true,
    );
    assert!(!plan.can_apply);
    assert!(plan
        .blockers
        .iter()
        .any(|code| code == "runtime_node_required"));
}

#[test]
fn existing_profile_is_not_rewritten_or_reset_during_setup() {
    let fixture = Fixture::new();
    let profile = fixture.service.root.join(MANAGED_PROFILE);
    fs::create_dir_all(&profile).unwrap();
    let manifest = br#"{"name":"custom_profile","dsh":{"profile":{"bundles":["custom_plugin"]}},"dependencies":{"custom_plugin":"1.2.3"}}"#;
    fs::write(profile.join("package.json"), manifest).unwrap();
    let sources = vec![upstream("claude:saved")];
    let plan = fixture.preview(&sources, None, None);
    assert!(!plan
        .changes
        .iter()
        .any(|change| change == "initialize_profile"));
    fixture.service.apply_setup(plan.plan_id, &sources).unwrap();
    assert_eq!(fs::read(profile.join("package.json")).unwrap(), manifest);
}
