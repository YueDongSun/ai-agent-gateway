use super::*;
use crate::config::private_fs;
use std::fs;
use std::sync::atomic::Ordering;

struct Fixture {
    native: PathBuf,
    mirror: PathBuf,
    backups: PathBuf,
    trusted: Vec<PathBuf>,
    stopped: AtomicBool,
    directory: private_fs::PrivateTempDir,
}

impl Fixture {
    fn new() -> Self {
        let directory = private_fs::private_temp_dir(&std::env::temp_dir()).unwrap();
        let native = directory.path().join("native");
        let mirror = directory.path().join("mirror");
        let backups = directory.path().join("backups");
        let trusted = vec![directory.path().join("shared_skills")];
        private_fs::prepare_managed_private_dir(&native).unwrap();
        Self {
            native,
            mirror,
            backups,
            trusted,
            stopped: AtomicBool::new(false),
            directory,
        }
    }

    fn source(&self, relative: &str, contents: &str) {
        let path = self.native.join(relative);
        private_fs::prepare_managed_private_dir(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn destination(&self, relative: &str, contents: &str) {
        let path = self.mirror.join(relative);
        private_fs::prepare_managed_private_dir(path.parent().unwrap()).unwrap();
        private_fs::atomic_write(&path, contents.as_bytes()).unwrap();
    }

    fn sync(&self, client: Client) -> Client_report {
        reconcile_client(
            client,
            &self.native,
            &self.mirror,
            &self.trusted,
            &self.backups,
            &self.stopped,
        )
    }

    fn read(&self, relative: &str) -> String {
        fs::read_to_string(self.mirror.join(relative)).unwrap()
    }
}

fn assert_synced(report: &Client_report) {
    assert_eq!(report.status, "synced", "sync error: {:?}", report.error);
}

#[test]
fn managed_authority_blocks_codex_sync_but_keeps_claude_enabled() {
    let home = crate::services::provider::test_support::isolated_home("managed-legacy-sync");
    for (relative, contents) in [
        (".codex/AGENTS.md", "codex_source"),
        (".claude/CLAUDE.md", "claude_source"),
    ] {
        let path = home.root.join(relative);
        private_fs::prepare_private_dir(path.parent().unwrap()).unwrap();
        private_fs::atomic_write(&path, contents.as_bytes()).unwrap();
    }
    let db = Database::memory().unwrap();
    db.set_setting(KEY_ENABLED, "1").unwrap();
    db.set_setting("codex_session_authority_enabled", "1")
        .unwrap();
    let report = Mirror_sync_service::new().reconcile(&db);
    assert_eq!(report.codex.status, "blocked");
    assert_eq!(
        report.codex.error.as_deref(),
        Some("session_client_authority_unavailable")
    );
    assert!(!home.root.join(".codex-cli").exists());
    assert_synced(&report.claude);
    assert_eq!(
        fs::read(home.root.join(".claude-cli/CLAUDE.md")).unwrap(),
        b"claude_source"
    );
}

#[test]
fn managed_authority_blocks_direct_codex_sync_with_marker() {
    let home = crate::services::provider::test_support::isolated_home("managed-legacy-sync-direct");
    fs::create_dir_all(home.root.join(".codex-cli/.authority")).unwrap();
    let fixture = Fixture::new();
    fixture.source("AGENTS.md", "codex_source");
    let report = fixture.sync(Client::Codex);
    assert_eq!(report.status, "blocked");
    assert_eq!(
        report.error.as_deref(),
        Some("session_client_authority_unavailable")
    );
    assert!(!fixture.mirror.exists());
    assert!(!fixture.backups.exists());
}

#[test]
fn managed_authority_keeps_claude_sync_out_of_the_canonical_codex_namespace() {
    let home = crate::services::provider::test_support::isolated_home("managed-cross-client-sync");
    let source = home.root.join(".claude");
    private_fs::prepare_private_dir(&source).unwrap();
    private_fs::atomic_write(&source.join("CLAUDE.md"), b"claude_source").unwrap();
    let db = Database::memory().unwrap();
    db.set_setting(KEY_ENABLED, "1").unwrap();
    db.set_setting("codex_session_authority_enabled", "1")
        .unwrap();
    db.set_setting(
        crate::database::dao::settings::KEY_CODEX_CLI_HOME,
        home.root.join("legacy_codex_elsewhere").to_str().unwrap(),
    )
    .unwrap();
    for destination in [
        home.root.join(".codex-cli"),
        home.root.join(".codex-cli/clients/desktop"),
        home.root.clone(),
    ] {
        db.set_setting(
            crate::database::dao::settings::KEY_CLAUDE_CLI_HOME,
            destination.to_str().unwrap(),
        )
        .unwrap();
        let report = Mirror_sync_service::new().reconcile(&db);
        assert_eq!(report.codex.status, "blocked");
        assert_eq!(
            report.claude.error.as_deref(),
            Some("mirror_sync_directory_invalid")
        );
        assert!(!destination.join("CLAUDE.md").exists());
    }
    assert!(!home.root.join(".codex-cli").exists());
}

#[test]
fn direct_claude_sync_cannot_write_into_codex_metadata_homes() {
    let home = crate::services::provider::test_support::isolated_home("managed-direct-cross-sync");
    let fixture = Fixture::new();
    fixture.source("CLAUDE.md", "claude_source");
    let destination = home.root.join(".codex-cli/clients/desktop");
    let report = reconcile_client(
        Client::Claude,
        &fixture.native,
        &destination,
        &fixture.trusted,
        &fixture.backups,
        &fixture.stopped,
    );
    assert_eq!(
        report.error.as_deref(),
        Some("mirror_sync_directory_invalid")
    );
    assert!(!destination.exists());
}

#[test]
fn prompt_resources_follow_source_additions_changes_and_removals_for_both_clients() {
    for client in [Client::Codex, Client::Claude] {
        let fixture = Fixture::new();
        fixture.source("prompts/review.md", "original prompt");
        assert_synced(&fixture.sync(client));
        assert_eq!(fixture.read("prompts/review.md"), "original prompt");
        fixture.source("prompts/review.md", "revised prompt");
        fixture.destination("prompts/stale.md", "independent stale prompt");
        assert_synced(&fixture.sync(client));
        assert_eq!(fixture.read("prompts/review.md"), "revised prompt");
        assert!(!fixture.mirror.join("prompts/stale.md").exists());
        fs::remove_file(fixture.native.join("prompts/review.md")).unwrap();
        assert_synced(&fixture.sync(client));
        assert!(!fixture.mirror.join("prompts/review.md").exists());
    }
}

#[allow(non_camel_case_types)]
struct Stop_on_drop(Mirror_sync_service);

impl Drop for Stop_on_drop {
    fn drop(&mut self) {
        let _ = self.0.stop();
    }
}

#[test]
fn strict_source_authority_prunes_mirror_only_files_and_repairs_divergence() {
    let fixture = Fixture::new();
    fixture.source("AGENTS.md", "original instructions");
    fixture.source("skills/retired/SKILL.md", "retired skill");
    fixture.destination("skills/mirror_only/SKILL.md", "unowned content");
    fixture.destination("settings.json", "unmanaged runtime settings");
    let initial = fixture.sync(Client::Codex);
    assert_synced(&initial);
    assert_eq!(
        (initial.copied, initial.deleted, initial.unmanaged),
        (2, 1, 0)
    );
    assert!(!fixture.mirror.join("skills/mirror_only/SKILL.md").exists());

    fixture.source("AGENTS.md", "updated instructions");
    fixture.source("skills/new_skill/SKILL.md", "new skill");
    fs::remove_file(fixture.native.join("skills/retired/SKILL.md")).unwrap();
    fixture.destination("AGENTS.md", "independent mirror edit");
    fixture.destination("skills/independent/SKILL.md", "independent addition");
    let changed = fixture.sync(Client::Codex);
    assert_synced(&changed);
    assert_eq!(
        (changed.copied, changed.deleted, changed.unmanaged),
        (2, 2, 0)
    );
    assert_eq!(fixture.read("AGENTS.md"), "updated instructions");
    assert_eq!(fixture.read("skills/new_skill/SKILL.md"), "new skill");
    assert!(!fixture.mirror.join("skills/retired/SKILL.md").exists());
    assert!(!fixture.mirror.join("skills/independent/SKILL.md").exists());
    assert_eq!(fixture.read("settings.json"), "unmanaged runtime settings");
    assert!(fixture.backups.join("codex").is_dir());
    assert!(!fixture.mirror.join("AGENTS.md.aag_backup").exists());
}

#[test]
fn unchanged_content_does_not_rewrite_instructions_or_manifest() {
    let fixture = Fixture::new();
    fixture.source("CLAUDE.md", "instructions");
    assert_synced(&fixture.sync(Client::Claude));
    let instruction = fixture.mirror.join("CLAUDE.md");
    let manifest = fixture.mirror.join(".aag_instruction_sync.json");
    let before_instruction = fs::metadata(&instruction).unwrap().modified().unwrap();
    let before_manifest = fs::metadata(&manifest).unwrap().modified().unwrap();
    let repeated = fixture.sync(Client::Claude);
    assert_synced(&repeated);
    assert_eq!(
        (repeated.copied, repeated.deleted, repeated.unchanged),
        (0, 0, 1)
    );
    assert_eq!(
        fs::metadata(instruction).unwrap().modified().unwrap(),
        before_instruction
    );
    assert_eq!(
        fs::metadata(manifest).unwrap().modified().unwrap(),
        before_manifest
    );
}

#[test]
fn missing_source_preserves_owned_files_until_an_available_empty_source_removes_them() {
    let fixture = Fixture::new();
    fixture.source("AGENTS.md", "keep while offline");
    assert_synced(&fixture.sync(Client::Codex));
    let offline = fixture.directory.path().join("native_offline");
    fs::rename(&fixture.native, &offline).unwrap();
    let unavailable = fixture.sync(Client::Codex);
    assert_eq!(unavailable.status, "source_unavailable");
    assert_eq!(fixture.read("AGENTS.md"), "keep while offline");
    fs::rename(offline, &fixture.native).unwrap();
    fs::remove_file(fixture.native.join("AGENTS.md")).unwrap();
    let removed = fixture.sync(Client::Codex);
    assert_synced(&removed);
    assert_eq!(removed.deleted, 1);
    assert!(!fixture.mirror.join("AGENTS.md").exists());
}

#[test]
fn runtime_credentials_and_vendor_system_skills_are_outside_instruction_ownership() {
    let fixture = Fixture::new();
    fixture.source("AGENTS.md", "instructions");
    fixture.source("auth.json", "fixture credential");
    fixture.source("config.toml", "model = \"fixture_provider_model\"\n");
    fixture.source("sessions/session.json", "fixture session");
    fixture.source("skills/.system/bundled/SKILL.md", "native vendor version");
    fixture.destination("skills/.system/bundled/SKILL.md", "mirror vendor version");
    let report = fixture.sync(Client::Codex);
    assert_synced(&report);
    assert_eq!(report.copied, 1);
    assert!(!fixture.mirror.join("auth.json").exists());
    assert!(!fixture.mirror.join("config.toml").exists());
    assert!(!fixture.mirror.join("sessions").exists());
    assert_eq!(
        fixture.read("skills/.system/bundled/SKILL.md"),
        "mirror vendor version"
    );
}

#[test]
fn interrupted_additions_remain_owned_and_are_removed_after_source_deletion() {
    let fixture = Fixture::new();
    fixture.source("AGENTS.md", "instructions");
    assert_synced(&fixture.sync(Client::Codex));
    fixture.source(
        "skills/interrupted/SKILL.md",
        "addition before interruption",
    );
    let inventory = scan::source_inventory(
        Client::Codex,
        &fixture.native,
        &fixture.trusted,
        &fixture.stopped,
    )
    .unwrap();
    let result = manifest::apply_interrupted(
        Client::Codex,
        &fixture.native,
        &fixture.mirror,
        &fixture.backups,
        inventory,
        &fixture.stopped,
    );
    assert_eq!(result.err().as_deref(), Some("mirror_sync_stopped"));
    assert_eq!(
        fixture.read("skills/interrupted/SKILL.md"),
        "addition before interruption"
    );
    fs::remove_file(fixture.native.join("skills/interrupted/SKILL.md")).unwrap();
    fixture.stopped.store(false, Ordering::SeqCst);
    let resumed = fixture.sync(Client::Codex);
    assert_synced(&resumed);
    assert_eq!(resumed.deleted, 1);
    assert!(!fixture.mirror.join("skills/interrupted/SKILL.md").exists());
}

#[test]
fn source_and_destination_overlap_is_rejected_before_source_mutation() {
    let fixture = Fixture::new();
    fixture.source("AGENTS.md", "original");
    for destination in [
        &fixture.native,
        &fixture.native.join("child"),
        &fixture.directory.path().to_path_buf(),
    ] {
        let report = reconcile_client(
            Client::Codex,
            &fixture.native,
            destination,
            &fixture.trusted,
            &fixture.backups,
            &fixture.stopped,
        );
        assert_eq!(
            report.error.as_deref(),
            Some("mirror_sync_directory_invalid")
        );
    }
    assert_eq!(
        fs::read_to_string(fixture.native.join("AGENTS.md")).unwrap(),
        "original"
    );
    assert!(!fixture.native.join(".aag_instruction_sync.json").exists());
}

#[test]
fn recovery_storage_cannot_write_into_an_original_instruction_root() {
    let fixture = Fixture::new();
    fixture.source("AGENTS.md", "original");
    let report = reconcile_client(
        Client::Codex,
        &fixture.native,
        &fixture.mirror,
        &fixture.trusted,
        &fixture.native.join("recovery"),
        &fixture.stopped,
    );
    assert_eq!(
        report.error.as_deref(),
        Some("mirror_sync_directory_invalid")
    );
    assert!(!fixture.native.join("recovery").exists());
    assert!(!fixture.mirror.exists());
}

#[test]
fn invalid_manifest_cannot_claim_a_runtime_file_for_deletion() {
    let fixture = Fixture::new();
    fixture.destination("auth.json", "fixture credential");
    let contents = serde_json::json!({
        "schema_version":1,"client":"codex","source_root":fixture.native,
        "managed_files":["auth.json"]
    });
    fixture.destination(
        ".aag_instruction_sync.json",
        &serde_json::to_string(&contents).unwrap(),
    );
    let report = fixture.sync(Client::Codex);
    assert_eq!(
        report.error.as_deref(),
        Some("mirror_sync_manifest_invalid")
    );
    assert_eq!(fixture.read("auth.json"), "fixture credential");
}

#[test]
fn disabled_service_preserves_existing_mirror_content_and_starts_no_worker() {
    let home = crate::services::provider::test_support::isolated_home("instruction-sync-disabled");
    let mirror = home.root.join(".codex-cli");
    private_fs::prepare_managed_private_dir(&mirror).unwrap();
    private_fs::atomic_write(&mirror.join("AGENTS.md"), b"existing mirror").unwrap();
    let db = Database::memory().unwrap();
    // Sync defaults to on; only the explicit opt-out disables it.
    db.set_setting(KEY_ENABLED, "0").unwrap();
    let service = Mirror_sync_service::new();
    assert_eq!(
        service.start(db.clone()).err().as_deref(),
        Some("mirror_sync_disabled")
    );
    let report = service.reconcile(&db);
    assert_eq!(report.codex.error.as_deref(), Some("mirror_sync_disabled"));
    assert_eq!(
        fs::read_to_string(mirror.join("AGENTS.md")).unwrap(),
        "existing mirror"
    );
    assert!(!mirror.join(".aag_instruction_sync.json").exists());
    assert!(service.inner.worker.lock().unwrap().is_none());
}

#[cfg(any(unix, windows))]
fn directory_link(target: &Path, link: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(target, link).unwrap();
}

#[cfg(any(unix, windows))]
#[test]
fn trusted_top_level_skill_links_are_materialized_and_target_updates_are_observed() {
    let fixture = Fixture::new();
    private_fs::prepare_managed_private_dir(&fixture.native.join("skills")).unwrap();
    let target = fixture.trusted[0].join("linked_skill");
    private_fs::prepare_managed_private_dir(&target).unwrap();
    fs::write(target.join("SKILL.md"), "shared v1").unwrap();
    directory_link(&target, &fixture.native.join("skills/linked_skill"));
    assert_synced(&fixture.sync(Client::Claude));
    assert_eq!(fixture.read("skills/linked_skill/SKILL.md"), "shared v1");
    assert!(!scan::linked(
        &fs::symlink_metadata(fixture.mirror.join("skills/linked_skill")).unwrap()
    ));
    fs::write(target.join("SKILL.md"), "shared v2").unwrap();
    let report = fixture.sync(Client::Claude);
    assert_synced(&report);
    assert_eq!(report.copied, 1);
    assert_eq!(fixture.read("skills/linked_skill/SKILL.md"), "shared v2");
}

#[cfg(any(unix, windows))]
#[test]
fn escaped_or_nested_source_links_block_the_whole_inventory_before_deletions() {
    let fixture = Fixture::new();
    fixture.source("CLAUDE.md", "keep");
    assert_synced(&fixture.sync(Client::Claude));
    fs::remove_file(fixture.native.join("CLAUDE.md")).unwrap();
    private_fs::prepare_managed_private_dir(&fixture.native.join("skills")).unwrap();
    let outside = fixture.directory.path().join("untrusted");
    private_fs::prepare_managed_private_dir(&outside).unwrap();
    directory_link(&outside, &fixture.native.join("skills/escape"));
    let escaped = fixture.sync(Client::Claude);
    assert_eq!(
        escaped.error.as_deref(),
        Some("mirror_sync_source_link_forbidden")
    );
    assert_eq!(fixture.read("CLAUDE.md"), "keep");
    fs::remove_dir(fixture.native.join("skills/escape"))
        .or_else(|_| fs::remove_file(fixture.native.join("skills/escape")))
        .unwrap();
    let trusted = fixture.trusted[0].join("nested");
    private_fs::prepare_managed_private_dir(&trusted).unwrap();
    private_fs::prepare_managed_private_dir(&fixture.native.join("skills/ordinary")).unwrap();
    directory_link(
        &trusted,
        &fixture.native.join("skills/ordinary/nested_link"),
    );
    let nested = fixture.sync(Client::Claude);
    assert_eq!(
        nested.error.as_deref(),
        Some("mirror_sync_source_link_forbidden")
    );
    assert_eq!(fixture.read("CLAUDE.md"), "keep");
}

#[cfg(any(unix, windows))]
#[test]
fn destination_links_never_redirect_an_instruction_write() {
    let fixture = Fixture::new();
    fixture.source("skills/linked/SKILL.md", "source instructions");
    private_fs::prepare_managed_private_dir(&fixture.mirror.join("skills")).unwrap();
    let outside = fixture.directory.path().join("outside");
    private_fs::prepare_managed_private_dir(&outside).unwrap();
    fs::write(outside.join("SKILL.md"), "outside original").unwrap();
    directory_link(&outside, &fixture.mirror.join("skills/linked"));
    let report = fixture.sync(Client::Codex);
    assert_eq!(
        report.error.as_deref(),
        Some("mirror_sync_destination_link_forbidden")
    );
    assert_eq!(
        fs::read_to_string(outside.join("SKILL.md")).unwrap(),
        "outside original"
    );
}

#[cfg(any(unix, windows))]
#[test]
fn missing_mirror_below_an_existing_source_alias_is_rejected_before_creation() {
    let fixture = Fixture::new();
    fixture.source("AGENTS.md", "original instructions");
    let alias = fixture.directory.path().join("native_alias");
    directory_link(&fixture.native, &alias);
    let report = reconcile_client(
        Client::Codex,
        &fixture.native,
        &alias.join("missing_mirror"),
        &fixture.trusted,
        &fixture.backups,
        &fixture.stopped,
    );
    assert_eq!(
        report.error.as_deref(),
        Some("mirror_sync_directory_invalid")
    );
    assert!(!fixture.native.join("missing_mirror").exists());
    assert_eq!(
        fs::read_to_string(fixture.native.join("AGENTS.md")).unwrap(),
        "original instructions"
    );
}

#[cfg(windows)]
#[test]
fn case_only_source_rename_does_not_delete_the_matching_windows_file() {
    let fixture = Fixture::new();
    fixture.source("skills/lowercase/SKILL.md", "same file");
    assert_synced(&fixture.sync(Client::Codex));
    fs::rename(
        fixture.native.join("skills/lowercase"),
        fixture.native.join("skills/intermediate"),
    )
    .unwrap();
    fs::rename(
        fixture.native.join("skills/intermediate"),
        fixture.native.join("skills/LOWERCASE"),
    )
    .unwrap();
    let report = fixture.sync(Client::Codex);
    assert_synced(&report);
    assert_eq!(report.deleted, 0);
    assert_eq!(fixture.read("skills/LOWERCASE/SKILL.md"), "same file");
}

#[test]
fn app_worker_reconciles_periodically_and_start_is_idempotent() {
    let home = crate::services::provider::test_support::isolated_home("instruction-sync-worker");
    private_fs::prepare_managed_private_dir(&home.root.join(".codex")).unwrap();
    private_fs::prepare_managed_private_dir(&home.root.join(".claude")).unwrap();
    fs::write(home.root.join(".codex/AGENTS.md"), "worker v1").unwrap();
    let db = Database::memory().unwrap();
    db.set_setting(KEY_ENABLED, "1").unwrap();
    let service = Mirror_sync_service::new();
    let _stop = Stop_on_drop(service.clone());
    service.start(db.clone()).unwrap();
    let first_worker = service
        .inner
        .worker
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .thread()
        .id();
    service.start(db).unwrap();
    assert_eq!(
        service
            .inner
            .worker
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .thread()
            .id(),
        first_worker
    );
    let path = home.root.join(".codex-cli/AGENTS.md");
    let initial_deadline = Instant::now() + Duration::from_secs(2);
    while fs::read_to_string(&path).ok().as_deref() != Some("worker v1")
        && Instant::now() < initial_deadline
    {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(fs::read_to_string(&path).unwrap(), "worker v1");
    fs::write(home.root.join(".codex/AGENTS.md"), "worker v2").unwrap();
    let changed_deadline = Instant::now() + RECONCILE_INTERVAL + Duration::from_secs(2);
    while fs::read_to_string(&path).ok().as_deref() != Some("worker v2")
        && Instant::now() < changed_deadline
    {
        std::thread::sleep(Duration::from_millis(10));
    }
    let changed = fs::read_to_string(&path).unwrap();
    service.stop().unwrap();
    assert_eq!(changed, "worker v2");
    assert!(service.inner.worker.lock().unwrap().is_none());
}

fn plugin_skill(fixture: &Fixture, version: &str, directory: &str, name: &str, body: &str) {
    fixture.source(
        &format!("plugins/cache/personal/demo/{version}/.codex-plugin/plugin.json"),
        r#"{"name":"demo","version":"fixture","skills":"./skills/"}"#,
    );
    fixture.source(
        &format!("plugins/cache/personal/demo/{version}/skills/{directory}/SKILL.md"),
        &format!("---\nname: {name}\ndescription: Fixture instructions.\n---\n{body}\n"),
    );
}

fn projected(name: &str, file: &str) -> String {
    format!(
        "{}/personal/demo/{name}/{file}",
        plugin_instructions::PROJECTION_DIRECTORY
    )
}

#[test]
fn plugin_instruction_trees_and_two_agent_scalars_follow_source_without_runtime_activation() {
    let _home = crate::services::provider::test_support::isolated_home("plugin-instruction-sync");
    let fixture = Fixture::new();
    fixture.source("config.toml", "model = \"native_model\"\n[agents]\nenabled = true\nmax_concurrent_threads_per_session = 4\n[plugins.\"demo@personal\"]\nenabled = true\n");
    plugin_skill(
        &fixture,
        "installed",
        "entry",
        "plugin-skill",
        "Version one",
    );
    fixture.source(
        "plugins/cache/personal/demo/installed/skills/entry/references/guide.md",
        "reference one",
    );
    fixture.source(
        "plugins/cache/personal/demo/installed/skills/entry/scripts/helper.mjs",
        "export const fixture = 1;\n",
    );
    fixture.source(
        "plugins/cache/personal/demo/installed/.mcp.json",
        "fixture server definition",
    );
    fixture.destination("config.toml", "# retained\nmodel = \"aag_role\"\n[model_providers.aag]\nbase_url = \"http://127.0.0.1:8989/v1\"\nexperimental_bearer_token = \"fixture_token\"\n[agents]\nuser_field = 42\n");
    let report = fixture.sync(Client::Codex);
    assert_synced(&report);
    assert_eq!(report.copied, 3);
    assert!(report.config_updated);
    assert_eq!(
        fixture.read(&projected("plugin-skill", "references/guide.md")),
        "reference one"
    );
    let config: toml_edit::DocumentMut = fixture.read("config.toml").parse().unwrap();
    assert_eq!(config["agents"]["enabled"].as_bool(), Some(true));
    assert_eq!(
        config["agents"]["max_concurrent_threads_per_session"].as_integer(),
        Some(4)
    );
    assert_eq!(config["agents"]["user_field"].as_integer(), Some(42));
    assert_eq!(config["model"].as_str(), Some("aag_role"));
    assert_eq!(
        config["model_providers"]["aag"]["experimental_bearer_token"].as_str(),
        Some("fixture_token")
    );
    assert!(config.get("plugins").is_none());
    assert!(!fixture.mirror.join("plugins").exists());
    let config_path = fixture.mirror.join("config.toml");
    let before = fs::metadata(&config_path).unwrap().modified().unwrap();
    let repeated = fixture.sync(Client::Codex);
    assert_synced(&repeated);
    assert_eq!(repeated.copied, 0);
    assert!(!repeated.config_updated);
    assert_eq!(
        fs::metadata(&config_path).unwrap().modified().unwrap(),
        before
    );
    fixture.source(
        "plugins/cache/personal/demo/installed/skills/entry/references/guide.md",
        "reference two",
    );
    fixture.source(
        "plugins/cache/personal/demo/installed/skills/entry/references/new.md",
        "new reference",
    );
    fs::remove_file(
        fixture
            .native
            .join("plugins/cache/personal/demo/installed/skills/entry/scripts/helper.mjs"),
    )
    .unwrap();
    assert_synced(&fixture.sync(Client::Codex));
    assert_eq!(
        fixture.read(&projected("plugin-skill", "references/guide.md")),
        "reference two"
    );
    assert!(!fixture
        .mirror
        .join(projected("plugin-skill", "scripts/helper.mjs"))
        .exists());
    fixture.source(
        "config.toml",
        "[plugins.\"demo@personal\"]\nenabled = false\n",
    );
    let removed = fixture.sync(Client::Codex);
    assert_synced(&removed);
    assert_eq!(removed.deleted, 3);
    let config: toml_edit::DocumentMut = fixture.read("config.toml").parse().unwrap();
    assert!(config["agents"].get("enabled").is_none());
    assert!(config["agents"]
        .get("max_concurrent_threads_per_session")
        .is_none());
    assert_eq!(config["agents"]["user_field"].as_integer(), Some(42));
}

#[test]
fn plugin_projection_avoids_user_shared_and_already_installed_mirror_skill_names() {
    let _home =
        crate::services::provider::test_support::isolated_home("plugin-instruction-duplicates");
    let fixture = Fixture::new();
    fixture.source(
        "config.toml",
        "[plugins.\"demo@personal\"]\nenabled = true\n",
    );
    for name in [
        "user-skill",
        "shared-skill",
        "installed-skill",
        "missing-skill",
    ] {
        plugin_skill(&fixture, "one", name, name, "plugin instructions");
    }
    fixture.source(
        "skills/user/SKILL.md",
        "---\nname: user-skill\ndescription: User skill.\n---\nUser instructions\n",
    );
    let shared = fixture.directory.path().join(".agents/skills/shared");
    fs::create_dir_all(&shared).unwrap();
    fs::write(
        shared.join("SKILL.md"),
        "---\nname: shared-skill\ndescription: Shared skill.\n---\nShared instructions\n",
    )
    .unwrap();
    fixture.destination(
        "config.toml",
        "[plugins.\"installed@personal\"]\nenabled = true\n",
    );
    fixture.destination(
        "plugins/cache/personal/installed/one/.codex-plugin/plugin.json",
        r#"{"name":"installed","skills":"./skills/"}"#,
    );
    fixture.destination(
        "plugins/cache/personal/installed/one/skills/installed/SKILL.md",
        "---\nname: installed-skill\ndescription: Installed skill.\n---\nInstalled instructions\n",
    );
    assert_synced(&fixture.sync(Client::Codex));
    assert!(fixture
        .mirror
        .join(projected("missing-skill", "SKILL.md"))
        .exists());
    for name in ["user-skill", "shared-skill", "installed-skill"] {
        assert!(!fixture.mirror.join(projected(name, "SKILL.md")).exists());
    }
    assert_eq!(
        fixture.read("plugins/cache/personal/installed/one/skills/installed/SKILL.md"),
        "---\nname: installed-skill\ndescription: Installed skill.\n---\nInstalled instructions\n"
    );
}

#[test]
fn unavailable_invalid_or_ambiguous_plugin_sources_preserve_last_good_instructions_and_config() {
    let _home =
        crate::services::provider::test_support::isolated_home("plugin-instruction-source-state");
    let fixture = Fixture::new();
    let config = "[agents]\nenabled = true\n[plugins.\"demo@personal\"]\nenabled = true\n";
    fixture.source("config.toml", config);
    plugin_skill(&fixture, "one", "entry", "plugin-skill", "last good");
    assert_synced(&fixture.sync(Client::Codex));
    let instruction = fixture.read(&projected("plugin-skill", "SKILL.md"));
    let mirror_config = fixture.read("config.toml");
    fixture.source("config.toml", "[agents]\nenabled = \"invalid\"\n");
    assert_eq!(
        fixture.sync(Client::Codex).error.as_deref(),
        Some("mirror_sync_source_config_invalid")
    );
    assert_eq!(fixture.read("config.toml"), mirror_config);
    fixture.source("config.toml", config);
    plugin_skill(
        &fixture,
        "two",
        "entry",
        "plugin-skill",
        "ambiguous new version",
    );
    assert_eq!(
        fixture.sync(Client::Codex).error.as_deref(),
        Some("mirror_sync_plugin_version_ambiguous")
    );
    assert_eq!(
        fixture.read(&projected("plugin-skill", "SKILL.md")),
        instruction
    );
    fs::remove_file(fixture.native.join("config.toml")).unwrap();
    assert_eq!(fixture.sync(Client::Codex).status, "source_unavailable");
    assert_eq!(
        fixture.read(&projected("plugin-skill", "SKILL.md")),
        instruction
    );
    assert_eq!(fixture.read("config.toml"), mirror_config);
}

#[test]
fn plugin_instruction_projection_rejects_metadata_escapes_and_private_runtime_files() {
    let fixture = Fixture::new();
    fixture.source(
        "config.toml",
        "[plugins.\"demo@personal\"]\nenabled = true\n",
    );
    plugin_skill(&fixture, "one", "entry", "plugin-skill", "last good");
    assert_synced(&fixture.sync(Client::Codex));
    let prior = fixture.read(&projected("plugin-skill", "SKILL.md"));
    fixture.source(
        "plugins/cache/personal/demo/one/.codex-plugin/plugin.json",
        r#"{"name":"demo","skills":"../escape"}"#,
    );
    assert_eq!(
        fixture.sync(Client::Codex).error.as_deref(),
        Some("mirror_sync_plugin_metadata_invalid")
    );
    plugin_skill(&fixture, "one", "entry", "plugin-skill", "new content");
    fixture.source(
        "plugins/cache/personal/demo/one/skills/entry/auth.json",
        "fixture-only credential material",
    );
    assert_eq!(
        fixture.sync(Client::Codex).error.as_deref(),
        Some("mirror_sync_plugin_content_forbidden")
    );
    assert_eq!(fixture.read(&projected("plugin-skill", "SKILL.md")), prior);
    assert!(!fixture
        .mirror
        .join(projected("plugin-skill", "auth.json"))
        .exists());
}

#[test]
fn agent_scalar_merge_uses_caller_bound_snapshot_and_preserves_concurrent_client_edits() {
    let _home =
        crate::services::provider::test_support::isolated_home("agent-instruction-snapshot");
    let fixture = Fixture::new();
    fixture.source(
        "config.toml",
        "[agents]\nenabled = true\nmax_concurrent_threads_per_session = 4\n",
    );
    fixture.destination("config.toml", "model = \"initial_gateway_role\"\n");
    let mut budget = scan::Budget::default();
    let inputs = codex_preferences::prepare(&fixture.native, &fixture.mirror, &mut budget).unwrap();
    fixture.destination("config.toml", "model = \"concurrent_client_edit\"\n");
    assert_eq!(
        inputs.update.unwrap().commit().err().as_deref(),
        Some("mirror_sync_config_write_failed")
    );
    assert_eq!(
        fixture.read("config.toml"),
        "model = \"concurrent_client_edit\"\n"
    );
    fixture.source(
        "config.toml",
        "[agents]\nmax_concurrent_threads_per_session = 0\n",
    );
    assert_eq!(
        fixture.sync(Client::Codex).error.as_deref(),
        Some("mirror_sync_source_config_invalid")
    );
    assert_eq!(
        fixture.read("config.toml"),
        "model = \"concurrent_client_edit\"\n"
    );
}

#[cfg(any(unix, windows))]
struct PluginAlias {
    path: PathBuf,
}

#[cfg(any(unix, windows))]
impl Drop for PluginAlias {
    fn drop(&mut self) {
        if fs::symlink_metadata(&self.path).is_ok_and(|metadata| scan::linked(&metadata)) {
            // Remove only the fixture alias itself, before the fixture's
            // recursive cleanup can encounter it. Never traverse its target.
            #[cfg(windows)]
            let _ = fs::remove_dir(&self.path);
            #[cfg(unix)]
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(any(unix, windows))]
fn plugin_alias(fixture: &Fixture, target: &Path, link: &Path) -> PluginAlias {
    assert!(scan::plain_absolute_path(target) && scan::plain_absolute_path(link));
    assert!(scan::path_within(target, fixture.directory.path()));
    assert!(scan::path_within(link, fixture.directory.path()));
    let alias = PluginAlias {
        path: link.to_path_buf(),
    };
    #[cfg(unix)]
    directory_link(target, link);
    #[cfg(windows)]
    {
        use std::io::Read;
        use std::os::windows::{fs::MetadataExt, process::CommandExt};
        use std::process::{Command, Stdio};
        struct ChildGuard(std::process::Child);
        impl Drop for ChildGuard {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        for path in [target, link] {
            assert!(!path
                .to_string_lossy()
                .chars()
                .any(|ch| "\"\r\n%!&|^<>".contains(ch)));
        }
        // cmd builtins parse their own command line. Normalize mixed
        // separators at this boundary, then quote the two literal fixture
        // paths in one raw command rather than applying CRT argv escaping.
        let cmd: PathBuf = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
            .join("System32\\cmd.exe")
            .components()
            .collect();
        let link_native: PathBuf = link.components().collect();
        let target_native: PathBuf = target.components().collect();
        assert!(cmd.is_absolute() && cmd.is_file());
        let command_text = format!(
            "mklink /J \"{}\" \"{}\"",
            link_native.display(),
            target_native.display()
        );
        let mut command = Command::new(&cmd);
        command
            .args(["/D", "/V:OFF", "/S", "/C"])
            .raw_arg(&command_text)
            .creation_flags(0x0800_0000)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut child = ChildGuard(command.spawn().unwrap());
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                let mut stderr = Vec::new();
                if let Some(stderr_pipe) = child.0.stderr.take() {
                    stderr_pipe.take(4096).read_to_end(&mut stderr).unwrap();
                }
                assert!(status.success(), "fixture junction creation failed: executable={} args=/D /V:OFF /S /C {} exit={:?} stderr={}",
                    cmd.display(), command_text, status.code(), String::from_utf8_lossy(&stderr));
                break;
            }
            assert!(
                Instant::now() < deadline,
                "fixture junction creation timed out"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_ne!(
            fs::symlink_metadata(link).unwrap().file_attributes() & 0x400,
            0,
            "the Windows fixture must exercise an actual reparse-point junction"
        );
    }
    alias
}

#[cfg(any(unix, windows))]
#[test]
fn plugin_latest_alias_materializes_once_from_concrete_native_and_mirror_versions() {
    let fixture = Fixture::new();
    fixture.source(
        "config.toml",
        "[plugins.\"demo@personal\"]\nenabled = true\n",
    );
    plugin_skill(&fixture, "one", "entry", "plugin-skill", "version one");
    let cache = fixture.native.join("plugins/cache/personal/demo");
    let latest = plugin_alias(&fixture, &cache.join("one"), &cache.join("latest"));
    fixture.destination(
        "config.toml",
        "[plugins.\"installed@personal\"]\nenabled = true\n",
    );
    fixture.destination(
        "plugins/cache/personal/installed/one/.codex-plugin/plugin.json",
        r#"{"name":"installed","skills":"./skills/"}"#,
    );
    fixture.destination(
        "plugins/cache/personal/installed/one/skills/entry/SKILL.md",
        "---\nname: installed-skill\ndescription: Installed fixture.\n---\nExisting instructions\n",
    );
    let mirror_cache = fixture.mirror.join("plugins/cache/personal/installed");
    let mirror_latest = plugin_alias(
        &fixture,
        &mirror_cache.join("one"),
        &mirror_cache.join("latest"),
    );
    let versions =
        scan::plugin_version_directories(&cache, &mut scan::Budget::default(), &fixture.stopped)
            .unwrap();
    assert_eq!(versions, [fs::canonicalize(cache.join("one")).unwrap()]);
    assert_eq!(
        scan::sorted_directories(&cache, &mut scan::Budget::default(), &fixture.stopped)
            .err()
            .as_deref(),
        Some("mirror_sync_source_link_forbidden"),
        "general directory scans keep rejecting links"
    );
    let copied = fixture.sync(Client::Codex);
    assert_synced(&copied);
    assert_eq!(copied.copied, 1);
    plugin_skill(&fixture, "one", "entry", "plugin-skill", "version two");
    assert_synced(&fixture.sync(Client::Codex));
    assert!(fixture
        .read(&projected("plugin-skill", "SKILL.md"))
        .contains("version two"));
    drop(mirror_latest);
    drop(latest);
    assert!(cache.join("one/skills/entry/SKILL.md").is_file());
    assert!(mirror_cache.join("one/skills/entry/SKILL.md").is_file());
}

#[cfg(any(unix, windows))]
#[test]
fn plugin_alias_cannot_escape_use_nested_targets_dangle_or_use_another_name() {
    for case in [
        "outside",
        "nested",
        "nested_alias",
        "dangling",
        "other_name",
    ] {
        let fixture = Fixture::new();
        fixture.source(
            "config.toml",
            "[plugins.\"demo@personal\"]\nenabled = true\n",
        );
        plugin_skill(&fixture, "one", "entry", "plugin-skill", "last good");
        assert_synced(&fixture.sync(Client::Codex));
        let original = fixture.read(&projected("plugin-skill", "SKILL.md"));
        let cache = fixture.native.join("plugins/cache/personal/demo");
        let version = cache.join("one");
        let outside = fixture.directory.path().join("outside_version");
        let nested = version.join("nested");
        fs::create_dir_all(&outside).unwrap();
        fs::create_dir_all(&nested).unwrap();
        let nested_alias = (case == "nested_alias")
            .then(|| plugin_alias(&fixture, &version, &version.join("redirect")));
        let target = match case {
            "outside" => outside.clone(),
            "nested" => nested,
            "nested_alias" => version.join("redirect"),
            _ => version.clone(),
        };
        let link = cache.join(if case == "other_name" {
            "current"
        } else {
            "latest"
        });
        let alias = plugin_alias(&fixture, &target, &link);
        if case == "dangling" {
            fs::rename(&version, fixture.directory.path().join("offline_version")).unwrap();
        }
        assert_eq!(
            fixture.sync(Client::Codex).error.as_deref(),
            Some("mirror_sync_source_link_forbidden"),
            "case {case}"
        );
        assert_eq!(
            fixture.read(&projected("plugin-skill", "SKILL.md")),
            original
        );
        drop(alias);
        drop(nested_alias);
        assert!(outside.is_dir());
    }
}

#[cfg(any(unix, windows))]
#[test]
fn latest_alias_never_selects_between_multiple_actual_plugin_versions() {
    let fixture = Fixture::new();
    fixture.source(
        "config.toml",
        "[plugins.\"demo@personal\"]\nenabled = true\n",
    );
    plugin_skill(&fixture, "one", "entry", "plugin-skill", "last good");
    assert_synced(&fixture.sync(Client::Codex));
    let original = fixture.read(&projected("plugin-skill", "SKILL.md"));
    plugin_skill(
        &fixture,
        "two",
        "entry",
        "plugin-skill",
        "second actual version",
    );
    let cache = fixture.native.join("plugins/cache/personal/demo");
    let latest = plugin_alias(&fixture, &cache.join("one"), &cache.join("latest"));
    assert_eq!(
        scan::plugin_version_directories(&cache, &mut scan::Budget::default(), &fixture.stopped)
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        fixture.sync(Client::Codex).error.as_deref(),
        Some("mirror_sync_plugin_version_ambiguous")
    );
    assert_eq!(
        fixture.read(&projected("plugin-skill", "SKILL.md")),
        original
    );
    drop(latest);
}

#[cfg(windows)]
#[test]
fn projected_resource_updates_keep_unchanged_skills_across_mixed_windows_separators() {
    let fixture = Fixture::new();
    fixture.source(
        "config.toml",
        "[plugins.\"demo@personal\"]\nenabled = true\n",
    );
    plugin_skill(&fixture, "one", "entry", "plugin-skill", "instruction one");
    fixture.source(
        "plugins/cache/personal/demo/one/skills/entry/references/guide.md",
        "resource one",
    );
    let initial = fixture.sync(Client::Codex);
    assert_synced(&initial);
    assert_eq!((initial.copied, initial.deleted), (2, 0));
    let mixed_skill = projected("plugin-skill", "SKILL.md");
    assert!(
        mixed_skill.contains('/'),
        "projection inventory must exercise mixed separators"
    );
    let native_skill =
        PathBuf::from(r"skills\aag_plugin_instructions\personal\demo\plugin-skill\SKILL.md");
    let original_skill = fs::read(fixture.mirror.join(&native_skill)).unwrap();
    fixture.source(
        "plugins/cache/personal/demo/one/skills/entry/references/guide.md",
        "resource two",
    );
    let resource_update = fixture.sync(Client::Codex);
    assert_synced(&resource_update);
    assert_eq!(
        (
            resource_update.copied,
            resource_update.deleted,
            resource_update.unchanged
        ),
        (1, 0, 1)
    );
    assert_eq!(
        fs::read(fixture.mirror.join(&native_skill)).unwrap(),
        original_skill,
        "unchanged definition must not be adopted as an obsolete duplicate path"
    );
    assert_eq!(
        fixture.read(&projected("plugin-skill", "references/guide.md")),
        "resource two"
    );
    plugin_skill(&fixture, "one", "entry", "plugin-skill", "instruction two");
    let skill_update = fixture.sync(Client::Codex);
    assert_synced(&skill_update);
    assert_eq!(
        (
            skill_update.copied,
            skill_update.deleted,
            skill_update.unchanged
        ),
        (1, 0, 1)
    );
    assert!(fixture.read(&mixed_skill).contains("instruction two"));
    assert_eq!(
        fixture.read(&projected("plugin-skill", "references/guide.md")),
        "resource two"
    );
    let repeated = fixture.sync(Client::Codex);
    assert_synced(&repeated);
    assert_eq!(
        (repeated.copied, repeated.deleted, repeated.unchanged),
        (0, 0, 2)
    );
}

#[cfg(unix)]
#[test]
fn literal_unix_backslashes_remain_distinct_instruction_paths() {
    let fixture = Fixture::new();
    fixture.source(
        r"skills/literal\name/SKILL.md",
        "literal-backslash instructions",
    );
    fixture.source("skills/literal/name/SKILL.md", "directory instructions");
    assert_synced(&fixture.sync(Client::Codex));
    fixture.source(
        r"skills/literal\name/SKILL.md",
        "changed literal-backslash instructions",
    );
    let updated = fixture.sync(Client::Codex);
    assert_synced(&updated);
    assert_eq!(
        (updated.copied, updated.deleted, updated.unchanged),
        (1, 0, 1)
    );
    assert_eq!(
        fixture.read(r"skills/literal\name/SKILL.md"),
        "changed literal-backslash instructions"
    );
    assert_eq!(
        fixture.read("skills/literal/name/SKILL.md"),
        "directory instructions"
    );
}
