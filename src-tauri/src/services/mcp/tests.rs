use super::*;
use serde_json::{json, Value};
use std::fs;

struct Fixture {
    home: PathBuf,
    db: Database,
    service: Mcp_service,
}
impl Fixture {
    fn new() -> Self {
        let mut nonce = [0u8; 12];
        getrandom::getrandom(&mut nonce).unwrap();
        let home = std::env::temp_dir().join(format!("aag_mcp_test_{}", hex::encode(nonce)));
        fs::create_dir(&home).unwrap();
        Self {
            service: Mcp_service::new(home.clone()),
            home,
            db: Database::memory().unwrap(),
        }
    }
    fn path(&self) -> PathBuf {
        self.home.join(".claude.json")
    }
    fn save(&self) {
        let mut request = input();
        if let Some(existing) = self.db.get_mcp_server(&request.id).unwrap() {
            request.expected_revision = Some(server_revision(&existing).unwrap());
        }
        self.service.save(&self.db, request).unwrap();
    }
    fn preview(&self) -> Mcp_sync_preview {
        self.service.preview(&self.db, Mcp_client::Claude).unwrap()
    }
    fn entries(&self) -> Vec<String> {
        let mut entries = fs::read_dir(&self.home)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        entries.sort();
        entries
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.home);
    }
}

fn input() -> Mcp_server_input {
    Mcp_server_input {
        id: "local_tools".into(),
        name: "Local tools".into(),
        description: Some("Owned test definition".into()),
        create_only: false,
        expected_revision: None,
        server_config: Some(
            json!({"command":"node", "args":["private_script.js", "private_arg_sentinel"],"env":{"TOKEN":"private_env_sentinel"}}),
        ),
        enabled_claude: true,
        enabled_codex: true,
    }
}

#[test]
fn registry_round_trip_masks_secrets_and_preserves_private_definition_on_edit() {
    let fixture = Fixture::new();
    fixture.save();
    let listed = serde_json::to_string(&fixture.service.list(&fixture.db).unwrap()).unwrap();
    assert!(!listed.contains("private_arg_sentinel"));
    assert!(!listed.contains("private_env_sentinel"));
    assert!(!listed.contains("private_script.js"));
    let mut edit = input();
    edit.name = "Renamed tools".into();
    edit.server_config = None;
    edit.expected_revision = Some(
        fixture.service.list(&fixture.db).unwrap()[0]
            .revision
            .clone(),
    );
    fixture.service.save(&fixture.db, edit).unwrap();
    let stored = fixture.db.get_mcp_server("local_tools").unwrap().unwrap();
    assert_eq!(stored.server_config["env"]["TOKEN"], "private_env_sentinel");
    assert_eq!(stored.name, "Renamed tools");
    assert!(fixture.entries().is_empty());
    fixture.service.delete(&fixture.db, "local_tools").unwrap();
    assert!(fixture.service.list(&fixture.db).unwrap().is_empty());
}

#[test]
fn input_validation_rejects_shells_strings_unknown_fields_and_redacted_values() {
    for config in [
        json!(null),
        json!({"command":"node --eval test"}),
        json!({"command":"cmd","args":["/c","echo test"]}),
        json!({"command":"pwsh"}),
        json!({"command":"node","args":"script.js"}),
        json!({"command":"node","args":[4]}),
        json!({"command":"node","args":["[REDACTED]"]}),
        json!({"command":"node","env":{"BAD KEY":"x"}}),
        json!({"command":"node","env":{"TOKEN":"\r\n"}}),
        json!({"command":"node","arbitrary":true}),
        json!({"command":"node","url":"https://example.test"}),
        json!({"command":"node","args":["x".repeat(32769)]}),
    ] {
        assert!(
            validate_config(&config).is_err(),
            "unsafe definition accepted"
        );
    }
    assert!(validate_config(&json!({"command":"node", "args":["server.js"]})).is_ok());
    for id in [
        "",
        "../escape",
        "has space",
        "legacy-name",
        "1start",
        "Upper",
    ] {
        assert!(validate_id(id).is_err());
    }
    assert!(validate_id("safe_1").is_ok());
}

#[test]
fn http_definition_requires_secure_transport_and_redacts_all_address_components() {
    let config = validate_config(&json!({"type":"streamable-http","url":"https://example.test/private_url_sentinel?key=private_query_sentinel","headers":{"Authorization":"private_header_sentinel"}})).unwrap();
    assert_eq!(config["type"], "http");
    let redacted = redact_config(&config).to_string();
    for secret in [
        "example.test",
        "private_url_sentinel",
        "private_query_sentinel",
        "private_header_sentinel",
    ] {
        assert!(!redacted.contains(secret));
    }
    for address in [
        "http://remote.test/mcp",
        "file:///private",
        "https://user:secret@example.test",
        "https://example.test/#token",
    ] {
        assert!(validate_config(&json!({"type":"http","url":address})).is_err());
    }
    assert!(validate_config(&json!({"url":"http://127.0.0.1:8765/mcp"})).is_ok());
}

#[test]
fn claude_preview_is_read_only_and_apply_preserves_unrelated_data_with_exact_backup() {
    let fixture = Fixture::new();
    fixture.save();
    let original = br#"{"oauthAccount":{"accessToken":"private_oauth_sentinel"},"projects":{"keep":{"hasTrustDialogAccepted":false}},"mcpServers":{"external":{"command":"keep"}}}"#;
    fs::write(fixture.path(), original).unwrap();
    let before = fixture.entries();
    let preview = fixture.preview();
    assert!(preview.can_apply);
    assert_eq!(before, fixture.entries());
    let public = serde_json::to_string(&preview).unwrap();
    for secret in [
        "private_oauth_sentinel",
        "private_env_sentinel",
        "private_arg_sentinel",
    ] {
        assert!(!public.contains(secret));
    }
    let token = preview.review_token.unwrap();
    let receipt = fixture.service.apply(&fixture.db, &token).unwrap();
    assert!(receipt.backup_created);
    assert_eq!(receipt.applied_server_count, 1);
    let content: Value = serde_json::from_slice(&fs::read(fixture.path()).unwrap()).unwrap();
    assert_eq!(
        content["oauthAccount"]["accessToken"],
        "private_oauth_sentinel"
    );
    assert_eq!(content["mcpServers"]["external"]["command"], "keep");
    assert_eq!(content["projects"]["keep"]["hasTrustDialogAccepted"], false);
    assert_eq!(
        content["mcpServers"]["aag_local_tools"]["env"]["TOKEN"],
        "private_env_sentinel"
    );
    assert_eq!(
        fs::read(
            fixture
                .home
                .join(format!(".claude.json.aag_backup_{token}"))
        )
        .unwrap(),
        original
    );
    assert!(!fixture
        .entries()
        .iter()
        .any(|name| name.contains("pending") || name.contains("lock")));
    assert!(fixture.service.apply(&fixture.db, &token).is_err());
    let no_change = fixture.preview();
    assert!(!no_change.can_apply);
    assert!(no_change.review_token.is_none());
}

#[test]
fn new_client_file_needs_no_backup_and_disabled_entry_is_removed_only_on_apply() {
    let fixture = Fixture::new();
    fixture.save();
    let token = fixture.preview().review_token.unwrap();
    let receipt = fixture.service.apply(&fixture.db, &token).unwrap();
    assert!(!receipt.backup_created);
    fixture
        .service
        .set_enabled(&fixture.db, "local_tools", Mcp_client::Claude, false)
        .unwrap();
    let before = fs::read(fixture.path()).unwrap();
    let preview = fixture.preview();
    assert_eq!(preview.changes[0].action, "remove");
    assert_eq!(before, fs::read(fixture.path()).unwrap());
    fixture
        .service
        .apply(&fixture.db, &preview.review_token.unwrap())
        .unwrap();
    let content: Value = serde_json::from_slice(&fs::read(fixture.path()).unwrap()).unwrap();
    assert!(content["mcpServers"].as_object().unwrap().is_empty());
}

#[test]
fn deleting_registry_never_modifies_live_file() {
    let fixture = Fixture::new();
    fixture.save();
    let preview = fixture.preview();
    fixture
        .service
        .apply(&fixture.db, &preview.review_token.unwrap())
        .unwrap();
    let original = fs::read(fixture.path()).unwrap();
    fixture.service.delete(&fixture.db, "local_tools").unwrap();
    assert_eq!(original, fs::read(fixture.path()).unwrap());
    assert!(!fixture.preview().can_apply);
}

#[test]
fn stale_file_and_registry_and_expired_tokens_are_rejected_without_writes() {
    let fixture = Fixture::new();
    fixture.save();
    assert!(fixture.service.apply(&fixture.db, "unknown").is_err());
    let token = fixture.preview().review_token.unwrap();
    fs::write(fixture.path(), b"{}").unwrap();
    assert!(fixture
        .service
        .apply(&fixture.db, &token)
        .unwrap_err()
        .to_string()
        .contains("client_changed_since_preview"));
    assert_eq!(fixture.entries(), vec![".claude.json"]);
    let token = fixture.preview().review_token.unwrap();
    fixture
        .service
        .reviews
        .lock()
        .unwrap()
        .get_mut(&token)
        .unwrap()
        .expires = Instant::now() - Duration::from_secs(1);
    assert!(fixture
        .service
        .apply(&fixture.db, &token)
        .unwrap_err()
        .to_string()
        .contains("review_expired"));
    let token = fixture.preview().review_token.unwrap();
    fixture
        .db
        .toggle_mcp_app("local_tools", "claude", false)
        .unwrap();
    assert!(fixture
        .service
        .apply(&fixture.db, &token)
        .unwrap_err()
        .to_string()
        .contains("registry_changed_since_preview"));
    assert_eq!(fixture.entries(), vec![".claude.json"]);
}

#[test]
fn registry_mutations_invalidate_pending_review() {
    let fixture = Fixture::new();
    fixture.save();
    let token = fixture.preview().review_token.unwrap();
    fixture.save();
    assert!(fixture.service.apply(&fixture.db, &token).is_err());
    assert!(fixture.entries().is_empty());
}

#[test]
fn interrupted_or_other_writer_lock_blocks_apply_and_remains_untouched() {
    let fixture = Fixture::new();
    fixture.save();
    let token = fixture.preview().review_token.unwrap();
    let lock_path = fixture.home.join(".claude.json.aag_lock");
    fs::write(&lock_path, "other owner").unwrap();
    assert!(fixture
        .service
        .apply(&fixture.db, &token)
        .unwrap_err()
        .to_string()
        .contains("client_sync_busy_or_interrupted"));
    assert_eq!(fs::read_to_string(&lock_path).unwrap(), "other owner");
    assert!(!fixture.path().exists());
}

#[test]
fn malformed_and_oversized_client_configs_are_preserved_without_secret_diagnostics() {
    let fixture = Fixture::new();
    fixture.save();
    for bytes in [
        b"{private_error_sentinel".to_vec(),
        b"[]".to_vec(),
        br#"{"mcpServers":[]}"#.to_vec(),
        vec![b' '; 2 * 1024 * 1024 + 1],
    ] {
        fs::write(fixture.path(), &bytes).unwrap();
        let error = fixture
            .service
            .preview(&fixture.db, Mcp_client::Claude)
            .err()
            .unwrap()
            .to_string();
        assert!(!error.contains("private_error_sentinel"));
        assert_eq!(fs::read(fixture.path()).unwrap(), bytes);
        assert_eq!(fixture.entries(), vec![".claude.json"]);
    }
}

#[test]
fn codex_is_redacted_preview_only_and_never_reads_or_writes_config() {
    let fixture = Fixture::new();
    fixture.save();
    fs::create_dir(fixture.home.join(".codex")).unwrap();
    fs::write(
        fixture.home.join(".codex/config.toml"),
        "invalid TOML containing private_codex_sentinel",
    )
    .unwrap();
    let preview = fixture
        .service
        .preview(&fixture.db, Mcp_client::Codex)
        .unwrap();
    assert_eq!(preview.format, "toml");
    assert!(!preview.can_apply);
    assert!(preview.review_token.is_none());
    assert!(preview
        .redacted_config
        .contains("[mcp_servers.aag_local_tools]"));
    assert!(!preview.redacted_config.contains("private_"));
    assert_eq!(
        fs::read_to_string(fixture.home.join(".codex/config.toml")).unwrap(),
        "invalid TOML containing private_codex_sentinel"
    );
    assert!(!fixture.path().exists());
}

#[test]
fn malformed_persisted_json_is_not_silently_replaced() {
    let fixture = Fixture::new();
    fixture.save();
    fixture
        .db
        .conn
        .lock()
        .unwrap()
        .execute(
            "UPDATE mcp_servers SET server_config = ?1",
            ["{private_corrupt_sentinel"],
        )
        .unwrap();
    let error = fixture.service.list(&fixture.db).err().unwrap().to_string();
    assert!(error.contains("invalid persisted MCP JSON"));
    assert!(!error.contains("private_corrupt_sentinel"));
    assert!(fixture
        .service
        .preview(&fixture.db, Mcp_client::Claude)
        .is_err());
}

#[test]
fn restricted_native_subjects_cannot_invoke_mcp_commands() {
    for command in [
        "mcp_list_servers",
        "mcp_save_server",
        "mcp_delete_server",
        "mcp_set_enabled",
        "mcp_preview_sync",
        "mcp_apply_sync",
        "mcp_discard_review",
    ] {
        assert!(crate::dsh_market_window::allowed_command("main", command));
        assert!(!crate::dsh_market_window::allowed_command(
            "dsh_market",
            command
        ));
        assert!(!crate::dsh_market_window::allowed_command(
            "dsh_web_owned",
            command
        ));
    }
}

#[test]
fn too_many_reviews_are_bounded_and_empty_registry_has_no_write_authority() {
    let fixture = Fixture::new();
    assert!(!fixture.preview().can_apply);
    assert!(fixture.entries().is_empty());
    fixture.save();
    for _ in 0..MAX_PENDING_REVIEWS {
        assert!(fixture.preview().can_apply);
    }
    assert!(fixture
        .service
        .preview(&fixture.db, Mcp_client::Claude)
        .is_err());
    assert!(fixture.entries().is_empty());
}

#[test]
fn imported_identifiers_remain_compatible_without_relaxing_new_record_names() {
    let fixture = Fixture::new();
    fixture.save();
    let mut imported = fixture.db.get_mcp_server("local_tools").unwrap().unwrap();
    imported.id = "External-MCP-123".into();
    fixture.db.save_mcp_server(&imported).unwrap();
    let mut edit = input();
    edit.id = imported.id.clone();
    edit.server_config = None;
    edit.expected_revision = Some(server_revision(&imported).unwrap());
    let result = fixture.service.save(&fixture.db, edit).unwrap();
    assert!(result.validation_error.is_none());
    let preview = fixture.preview();
    assert!(preview.redacted_config.contains("aag_External-MCP-123"));
    fixture
        .service
        .apply(&fixture.db, &preview.review_token.unwrap())
        .unwrap();
    let content: Value = serde_json::from_slice(&fs::read(fixture.path()).unwrap()).unwrap();
    assert!(content["mcpServers"]["aag_External-MCP-123"].is_object());
    let mut new_input = input();
    new_input.id = "New-Hyphenated".into();
    assert!(fixture.service.save(&fixture.db, new_input).is_err());
}

#[test]
fn simultaneous_duplicate_apply_has_exactly_one_winner() {
    let fixture = Fixture::new();
    fixture.save();
    let token = fixture.preview().review_token.unwrap();
    let outcomes = std::thread::scope(|scope| {
        let one = scope.spawn(|| fixture.service.apply(&fixture.db, &token));
        let two = scope.spawn(|| fixture.service.apply(&fixture.db, &token));
        vec![one.join().unwrap(), two.join().unwrap()]
    });
    assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
    assert_eq!(fixture.entries(), vec![".claude.json"]);
}

#[test]
fn backup_collision_preserves_original_and_cleans_owned_temporary_files() {
    let fixture = Fixture::new();
    fixture.save();
    fs::write(fixture.path(), b"{}").unwrap();
    let token = fixture.preview().review_token.unwrap();
    let backup = fixture
        .home
        .join(format!(".claude.json.aag_backup_{token}"));
    fs::write(&backup, "existing backup").unwrap();
    assert!(fixture.service.apply(&fixture.db, &token).is_err());
    assert_eq!(fs::read(fixture.path()).unwrap(), b"{}");
    assert_eq!(fs::read_to_string(backup).unwrap(), "existing backup");
    assert!(!fixture
        .entries()
        .iter()
        .any(|name| name.contains("pending") || name.contains("lock")));
}

#[test]
fn duplicate_add_rejects_existing_record_without_overwriting_private_data() {
    let fixture = Fixture::new();
    fixture.save();
    let original = fixture.db.get_mcp_server("local_tools").unwrap().unwrap();
    let mut duplicate = input();
    duplicate.create_only = true;
    duplicate.server_config = Some(json!({"command":"different"}));
    duplicate.enabled_claude = false;
    let error = fixture
        .service
        .save(&fixture.db, duplicate)
        .err()
        .unwrap()
        .to_string();
    assert_eq!(error, "mcp:server_exists");
    let retained = fixture.db.get_mcp_server("local_tools").unwrap().unwrap();
    assert_eq!(
        server_revision(&original).unwrap(),
        server_revision(&retained).unwrap()
    );
}

#[test]
fn stale_or_missing_revision_cannot_overwrite_newer_settings() {
    let fixture = Fixture::new();
    fixture.save();
    let mut edit = input();
    edit.server_config = None;
    assert!(fixture
        .service
        .save(&fixture.db, edit.clone())
        .err()
        .unwrap()
        .to_string()
        .contains("revision_mismatch"));
    edit.expected_revision = Some(
        fixture.service.list(&fixture.db).unwrap()[0]
            .revision
            .clone(),
    );
    fixture
        .service
        .set_enabled(&fixture.db, "local_tools", Mcp_client::Claude, false)
        .unwrap();
    assert!(fixture
        .service
        .save(&fixture.db, edit.clone())
        .err()
        .unwrap()
        .to_string()
        .contains("revision_mismatch"));
    assert!(
        !fixture
            .db
            .get_mcp_server("local_tools")
            .unwrap()
            .unwrap()
            .enabled_claude
    );
    edit.expected_revision = Some(
        fixture.service.list(&fixture.db).unwrap()[0]
            .revision
            .clone(),
    );
    let updated = fixture.service.save(&fixture.db, edit).unwrap();
    assert!(updated.enabled_claude);
}

#[test]
fn discarding_review_is_idempotent_and_releases_capacity_without_writes() {
    let fixture = Fixture::new();
    fixture.save();
    let mut tokens = Vec::new();
    for _ in 0..MAX_PENDING_REVIEWS {
        tokens.push(fixture.preview().review_token.unwrap());
    }
    assert!(fixture
        .service
        .preview(&fixture.db, Mcp_client::Claude)
        .is_err());
    fixture.service.discard_review(&tokens[0]).unwrap();
    fixture.service.discard_review(&tokens[0]).unwrap();
    assert!(fixture.service.apply(&fixture.db, &tokens[0]).is_err());
    assert!(fixture.preview().can_apply);
    for _ in 0..32 {
        fixture.service.discard_review(&tokens[1]).unwrap();
        let token = fixture.preview().review_token.unwrap();
        fixture.service.discard_review(&token).unwrap();
    }
    assert!(fixture.entries().is_empty());
}

#[cfg(windows)]
#[test]
fn restrictive_windows_owner_group_and_dacl_survive_replacement_and_backup() {
    use super::windows_permissions::{restrict_to_owner, Windows_permissions};
    let fixture = Fixture::new();
    fixture.save();
    fs::write(fixture.path(), br#"{"private":"old_secret"}"#).unwrap();
    restrict_to_owner(&fixture.path()).unwrap();
    let permissions = Windows_permissions::read(&fixture.path()).unwrap();
    let empty_pending = fixture.home.join("empty_pending");
    fs::write(&empty_pending, b"").unwrap();
    permissions.apply(&empty_pending).unwrap();
    assert_eq!(fs::metadata(&empty_pending).unwrap().len(), 0);
    assert!(permissions.matches(&Windows_permissions::read(&empty_pending).unwrap()));
    fs::remove_file(empty_pending).unwrap();
    let token = fixture.preview().review_token.unwrap();
    fixture.service.apply(&fixture.db, &token).unwrap();
    assert!(permissions.matches(&Windows_permissions::read(&fixture.path()).unwrap()));
    let backup = fixture
        .home
        .join(format!(".claude.json.aag_backup_{token}"));
    assert!(permissions.matches(&Windows_permissions::read(&backup).unwrap()));
    assert_eq!(fs::read(backup).unwrap(), br#"{"private":"old_secret"}"#);
}

#[test]
fn readonly_client_fails_before_creating_backup_lock_or_pending_file() {
    let fixture = Fixture::new();
    fixture.save();
    fs::write(fixture.path(), b"{}").unwrap();
    let original_permissions = fs::metadata(fixture.path()).unwrap().permissions();
    let mut readonly = original_permissions.clone();
    readonly.set_readonly(true);
    fs::set_permissions(fixture.path(), readonly).unwrap();
    let token = fixture.preview().review_token.unwrap();
    let outcome = fixture.service.apply(&fixture.db, &token);
    fs::set_permissions(fixture.path(), original_permissions).unwrap();
    assert!(outcome.unwrap_err().to_string().contains("client_readonly"));
    assert_eq!(fixture.entries(), vec![".claude.json"]);
    assert_eq!(fs::read(fixture.path()).unwrap(), b"{}");
}

#[cfg(windows)]
#[test]
fn windows_transaction_creation_blocks_reader_handles_before_acl_and_through_secret_write() {
    use super::windows_permissions::{restrict_to_owner, Windows_permissions};
    use std::io::Write;
    let fixture = Fixture::new();
    fs::write(fixture.path(), b"{}").unwrap();
    restrict_to_owner(&fixture.path()).unwrap();
    let permissions = Windows_permissions::read(&fixture.path()).unwrap();
    for filename in ["test_pending", "test_backup"] {
        let path = fixture.home.join(filename);
        let mut writer = client_config::create_private(&path).unwrap();
        // ERROR_SHARING_VIOLATION even for this same owner proves an account
        // allowed by the inherited DACL cannot capture a data-read handle.
        assert_eq!(fs::File::open(&path).unwrap_err().raw_os_error(), Some(32));
        permissions.apply(&path).unwrap();
        assert!(permissions.matches(&Windows_permissions::read(&path).unwrap()));
        assert_eq!(fs::File::open(&path).unwrap_err().raw_os_error(), Some(32));
        writer.write_all(b"private_transaction_sentinel").unwrap();
        writer.sync_all().unwrap();
        assert_eq!(fs::File::open(&path).unwrap_err().raw_os_error(), Some(32));
        drop(writer);
        assert_eq!(fs::read(&path).unwrap(), b"private_transaction_sentinel");
    }
}

#[cfg(windows)]
#[test]
fn first_windows_client_config_has_verified_owner_only_protected_acl() {
    use super::windows_permissions::{restrict_to_owner, Windows_permissions};
    let fixture = Fixture::new();
    fixture.save();
    let expected_path = fixture.home.join("owner_only_reference");
    fs::write(&expected_path, b"").unwrap();
    restrict_to_owner(&expected_path).unwrap();
    let owner_only = Windows_permissions::read(&expected_path).unwrap();
    let token = fixture.preview().review_token.unwrap();
    fixture.service.apply(&fixture.db, &token).unwrap();
    assert!(owner_only.matches(&Windows_permissions::read(&fixture.path()).unwrap()));
    assert!(!fixture
        .entries()
        .iter()
        .any(|name| name.contains("pending") || name.contains("lock")));
}

#[test]
fn deleted_record_cannot_be_recreated_by_stale_complete_editor_payload() {
    let fixture = Fixture::new();
    fixture.save();
    let revision = fixture.service.list(&fixture.db).unwrap()[0]
        .revision
        .clone();
    let mut stale_edit = input();
    stale_edit.expected_revision = Some(revision);
    stale_edit.server_config = Some(json!({"command": "replacement_server"}));
    fixture.service.delete(&fixture.db, &stale_edit.id).unwrap();
    let error = fixture.service.save(&fixture.db, stale_edit).err().unwrap();
    assert_eq!(error.to_string(), "mcp:revision_mismatch");
    assert!(fixture.service.list(&fixture.db).unwrap().is_empty());
    assert!(fixture.entries().is_empty());
    let mut fresh_create = input();
    fresh_create.create_only = true;
    assert!(fixture.service.save(&fixture.db, fresh_create).is_ok());
}
