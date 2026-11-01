//! Claude mirror (CLAUDE_CONFIG_DIR) writer.
//!
//! Claude Code / Claude Desktop read their runtime config from the
//! directory in `CLAUDE_CONFIG_DIR` (default `~/.claude`). AAG never writes
//! that native directory — it belongs to the live Claude install and to
//! cc-switch. Instead this module maintains a SEPARATE mirror home
//! (`~/.claude-cli` by default) whose `settings.json` points `env` at
//! AAG's proxy:
//!
//! - `ANTHROPIC_BASE_URL = http://<host>:<port>` — no `/v1` suffix; the
//!   Claude client appends the API path itself.
//! - `Token` mode (default): `ANTHROPIC_AUTH_TOKEN` carries AAG's proxy
//!   token, so the mirror authenticates to AAG like any other gateway
//!   client and AAG's static-key failover chain serves the requests.
//!   This is the historic flow and stays 100% independent of the
//!   passthrough feature.
//! - `Passthrough` mode: the token key is OMITTED, so a subscription
//!   login made inside the mirror sends its own OAuth credential to AAG.
//!   AAG's `/v1/messages` auth relaxation admits it and an
//!   `anthropic_passthrough` pool row relays it verbatim upstream.
//!
//! AAG writes the gateway fields and configured model selectors, with no upstream
//! credential. Foreign keys in an existing mirror `settings.json` (both
//! top-level and inside `env`) are preserved; the file is backed up to a
//! sibling `.bak` before each overwrite.

use crate::config;
use crate::error::AppError;
use crate::services::provider::LiveWriteResult;
use serde_json::Value;

/// How the Claude mirror authenticates to AAG (see the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(non_camel_case_types)]
pub enum Claude_mirror_mode {
    /// `env.ANTHROPIC_AUTH_TOKEN` = AAG proxy token (historic default;
    /// works with the static-key failover chain only).
    Token,
    /// The token key is omitted — a subscription login made inside the
    /// mirror is relayed verbatim by `anthropic_passthrough` pool rows.
    Passthrough,
}

impl Claude_mirror_mode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Claude_mirror_mode::Token => "token",
            Claude_mirror_mode::Passthrough => "passthrough",
        }
    }

    /// Parse the persisted setting. Unknown/absent values fail safe to the
    /// historic token behavior.
    pub fn from_setting(value: Option<&str>) -> Self {
        if value == Some("passthrough") {
            Claude_mirror_mode::Passthrough
        } else {
            Claude_mirror_mode::Token
        }
    }
}

/// Read the persisted mirror mode (`KEY_CLAUDE_MIRROR_MODE`; absent =
/// token).
pub(crate) fn claude_mirror_mode_from_db(db: &crate::database::Database) -> Claude_mirror_mode {
    Claude_mirror_mode::from_setting(
        db.get_setting(crate::database::dao::settings::KEY_CLAUDE_MIRROR_MODE)
            .ok()
            .flatten()
            .as_deref(),
    )
}

/// Write the mirror `settings.json` (see module docs). Returns the paths
/// written and the backup created, if any.
///
/// `proxy_base_url` must already be the bare `http://host:port` form —
/// no `/v1` (the Claude client appends the API path).
pub fn write_claude_mirror_files(
    claude_cli_home: Option<&str>,
    proxy_base_url: &str,
    auth_token: &str,
    mode: Claude_mirror_mode,
) -> Result<LiveWriteResult, AppError> {
    write_claude_mirror_with_model(
        claude_cli_home,
        proxy_base_url,
        auth_token,
        mode,
        None,
        false,
    )
}

fn write_claude_mirror_with_model(
    claude_cli_home: Option<&str>,
    proxy_base_url: &str,
    auth_token: &str,
    mode: Claude_mirror_mode,
    model: Option<&str>,
    mirror_roles: bool,
) -> Result<LiveWriteResult, AppError> {
    let mirror_dir = config::get_claude_mirror_dir(claude_cli_home);
    // The native `~/.claude` belongs to the live Claude install (and to
    // cc-switch when chained). Refuse a mirror override that resolves
    // there — the same guard the Codex mirror has for `~/.codex`.
    if config::same_dir(&mirror_dir, &config::get_claude_config_dir()) {
        return Err(AppError::Config(
            "claude_cli_home must not resolve to the native ~/.claude: refusing to write the live Claude install's shared config".into(),
        ));
    }
    // Harden the mirror dir to AAG owner-only (Windows: protected owner DACL;
    // Unix: 0700). Mirrors `codex::write_mirror_proxy_files` — this is the
    // AAG-managed directory, not the user's native ~/.claude, so the
    // tightening is appropriate and matches the existing codex-mirror
    // contract. Without this, profile dirs whose ACL carries untrusted
    // Modify grants (e.g. a sandbox group) make subsequent
    // `prepare_external_parent_dir` checks fail with
    // `external_directory_write_unsafe`.
    config::private_fs::prepare_managed_private_dir(&mirror_dir)?;
    let settings_path = mirror_dir.join("settings.json");
    config::private_fs::validate_external_file_path(&settings_path)?;

    // Shallow-merge an existing mirror settings.json: foreign top-level
    // keys AND foreign env keys survive; a malformed file blocks the write
    // and is retained (the contract is never to replace invalid user JSON
    // silently — the user can recover via the .bak from a previous write).
    let previous = config::private_fs::read_mirror_file(&settings_path)?;
    let mut settings: Value = match previous.contents.as_deref() {
        Some(raw) => serde_json::from_slice(raw).map_err(AppError::Json)?,
        None => serde_json::json!({}),
    };
    if !settings.is_object() {
        return Err(AppError::Config(
            "claude_mirror_settings_root_invalid".into(),
        ));
    }

    let env = settings
        .as_object_mut()
        .expect("settings is an object")
        .entry("env")
        .or_insert_with(|| serde_json::json!({}));
    if !env.is_object() {
        return Err(AppError::Config(
            "claude_mirror_settings_env_invalid".into(),
        ));
    }
    let env_obj = env.as_object_mut().expect("env is an object");
    if mirror_roles {
        for (key, model) in [
            (
                "ANTHROPIC_MODEL",
                crate::services::mirror_models::DEFAULT_CLAUDE_ROLE,
            ),
            ("ANTHROPIC_DEFAULT_FABLE_MODEL", "claude-fable-5"),
            ("ANTHROPIC_DEFAULT_OPUS_MODEL", "claude-opus-5"),
            ("ANTHROPIC_DEFAULT_SONNET_MODEL", "claude-sonnet-5"),
            ("ANTHROPIC_DEFAULT_HAIKU_MODEL", "claude-haiku-5-5"),
        ] {
            env_obj.insert(key.to_string(), Value::String(model.to_string()));
        }
    } else if let Some(model) = model {
        for key in [
            "ANTHROPIC_MODEL",
            "ANTHROPIC_DEFAULT_FABLE_MODEL",
            "ANTHROPIC_DEFAULT_OPUS_MODEL",
            "ANTHROPIC_DEFAULT_SONNET_MODEL",
            "ANTHROPIC_DEFAULT_HAIKU_MODEL",
        ] {
            env_obj.insert(key.to_string(), Value::String(model.to_string()));
        }
    }
    env_obj.insert(
        "ANTHROPIC_BASE_URL".to_string(),
        Value::String(proxy_base_url.to_string()),
    );
    match mode {
        Claude_mirror_mode::Token => {
            // Placeholder when the proxy token is empty so the Claude
            // client does not strip the key (gemini `.env` precedent).
            let token = if auth_token.is_empty() {
                "PROXY_MANAGED"
            } else {
                auth_token
            };
            env_obj.insert(
                "ANTHROPIC_AUTH_TOKEN".to_string(),
                Value::String(token.to_string()),
            );
        }
        Claude_mirror_mode::Passthrough => {
            // No token key: the mirror's own subscription login flows to
            // AAG verbatim (see the /v1/messages auth relaxation).
            env_obj.remove("ANTHROPIC_AUTH_TOKEN");
        }
    }
    if mirror_roles {
        settings["model"] =
            Value::String(crate::services::mirror_models::DEFAULT_CLAUDE_ROLE.into());
    }

    let serialized = serde_json::to_vec_pretty(&settings)?;
    let (written, backups) = super::config_writes::write_mirror_files(&[
        super::config_writes::MirrorConfigFileUpdate {
            path: &settings_path,
            contents: &serialized,
            app_type: "claude",
            previous: &previous,
        },
    ])?;

    Ok(LiveWriteResult {
        app_type: "claude_desktop".into(),
        provider_id: String::new(),
        written,
        backup: backups.into_iter().next(),
    })
}

/// Gather the mirror inputs from the database and write the mirror
/// settings.json. The single entry point for `claude_mirror_status` /
/// `claude_open_desktop`.
pub fn switch_claude_mirror(
    db: &crate::database::Database,
    claude_cli_home: Option<&str>,
) -> Result<LiveWriteResult, AppError> {
    let proxy_cfg = db.get_proxy_config()?;
    // A wildcard or unset bind is not a client target — the Claude client
    // dials this address, so translate it to the loopback (same rule as the
    // Codex and Gemini mirrors).
    let base_url = format!(
        "http://{}:{}",
        proxy_cfg.client_endpoint_host(),
        proxy_cfg.port
    );
    let default_model =
        db.get_setting(crate::services::supplier_setup::KEY_UNIFIED_DEFAULT_MODEL)?;
    let mirror_roles = db
        .get_setting(crate::services::mirror_models::KEY_ENABLED)?
        .as_deref()
        == Some("1");
    let mut result = write_claude_mirror_with_model(
        claude_cli_home,
        &base_url,
        &proxy_cfg.auth_token,
        claude_mirror_mode_from_db(db),
        default_model.as_deref(),
        mirror_roles,
    )?;
    result.provider_id = db
        .get_current_provider("claude_code")
        .ok()
        .flatten()
        .unwrap_or_default();
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::provider::test_support::{isolated_home, IsolatedHome};
    use std::path::PathBuf;

    #[test]
    fn managed_role_configuration_keeps_roles_separate_from_actual_supply() {
        let (home, mirror) = fresh_home("role-config");
        let db = crate::database::Database::memory().unwrap();
        db.set_setting(crate::services::mirror_models::KEY_ENABLED, "1")
            .unwrap();
        db.set_setting(
            crate::services::supplier_setup::KEY_UNIFIED_DEFAULT_MODEL,
            "MiniMax-M3.1-Flash",
        )
        .unwrap();
        config::private_fs::prepare_managed_private_dir(&mirror).unwrap();
        config::private_fs::atomic_write(
            &mirror.join("settings.json"),
            br#"{"theme":"dark","env":{"UNRELATED":"kept"}}"#,
        )
        .unwrap();
        switch_claude_mirror(&db, mirror.to_str()).unwrap();
        let settings = read_mirror_settings(&mirror);
        assert_eq!(settings["model"], "claude-fable-5");
        assert_eq!(settings["env"]["ANTHROPIC_MODEL"], "claude-fable-5");
        assert_eq!(
            settings["env"]["ANTHROPIC_DEFAULT_FABLE_MODEL"],
            "claude-fable-5"
        );
        assert_eq!(
            settings["env"]["ANTHROPIC_DEFAULT_OPUS_MODEL"],
            "claude-opus-5"
        );
        assert_eq!(
            settings["env"]["ANTHROPIC_DEFAULT_SONNET_MODEL"],
            "claude-sonnet-5"
        );
        assert_eq!(
            settings["env"]["ANTHROPIC_DEFAULT_HAIKU_MODEL"],
            "claude-haiku-5-5"
        );
        assert_eq!(settings["env"]["UNRELATED"], "kept");
        assert_eq!(settings["theme"], "dark");
        assert!(!settings.to_string().contains("MiniMax"));
        assert!(!home.root.join(".claude/settings.json").exists());
        assert!(switch_claude_mirror(&db, mirror.to_str())
            .unwrap()
            .written
            .is_empty());
    }

    /// Build a fresh owner-only home for a single test. The returned
    /// `IsolatedHome` keeps AAG_HOME_DIR/AAG_CONFIG_DIR captured and will
    /// restore them on Drop; it already secures the temp root to owner-only
    /// (Windows protected owner DACL / Unix 0o700) so the write path's
    /// external-parent ACL checks pass on hosts whose shared temp directory
    /// carries untrusted writer ACEs (the WIP fixture hardened its root the
    /// same way before pre-writing settings.json).
    fn fresh_home(tag: &str) -> (IsolatedHome, PathBuf) {
        let home = isolated_home(&format!("claude-mirror-{tag}"));
        let mirror = home.root.clone();
        (home, mirror)
    }

    fn read_mirror_settings(home: &std::path::Path) -> Value {
        let path = config::get_claude_mirror_settings_path(Some(home.to_str().unwrap()));
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    /// Token mode writes base URL + auth token; a pre-existing foreign
    /// top-level key AND a foreign env key survive the merge; a `.bak`
    /// captures the previous file.
    #[test]
    fn token_mode_merges_env_and_preserves_foreign_keys() {
        let (_home, mirror) = fresh_home("token");

        std::fs::create_dir_all(&mirror).unwrap();
        std::fs::write(
            mirror.join("settings.json"),
            r#"{"permissions":{"allow":["Bash"]},"env":{"ANTHROPIC_MODEL":"claude-opus-4","ANTHROPIC_BASE_URL":"https://old.example.com"}}"#,
        )
        .unwrap();

        let result = write_claude_mirror_files(
            Some(mirror.to_str().unwrap()),
            "http://127.0.0.1:8989",
            "aag-proxy-token",
            Claude_mirror_mode::Token,
        )
        .expect("write claude mirror");

        assert_eq!(result.written.len(), 1);
        assert!(result.backup.is_some(), "existing file must be backed up");

        let settings = read_mirror_settings(&mirror);
        assert_eq!(
            settings["permissions"]["allow"][0], "Bash",
            "foreign top-level key must survive"
        );
        assert_eq!(
            settings["env"]["ANTHROPIC_MODEL"], "claude-opus-4",
            "foreign env key must survive"
        );
        assert_eq!(
            settings["env"]["ANTHROPIC_BASE_URL"],
            "http://127.0.0.1:8989"
        );
        assert_eq!(settings["env"]["ANTHROPIC_AUTH_TOKEN"], "aag-proxy-token");
        assert!(
            !settings["env"]["ANTHROPIC_BASE_URL"]
                .as_str()
                .unwrap()
                .contains("/v1"),
            "base URL must not carry a /v1 suffix"
        );
    }

    /// Passthrough mode REMOVES the token key (even one written by an
    /// earlier token-mode write) so the mirror's own subscription login
    /// reaches AAG verbatim.
    #[test]
    fn passthrough_mode_omits_and_strips_the_token_key() {
        let (_home, mirror) = fresh_home("passthrough");

        std::fs::create_dir_all(&mirror).unwrap();
        std::fs::write(
            mirror.join("settings.json"),
            r#"{"env":{"ANTHROPIC_AUTH_TOKEN":"stale-token"}}"#,
        )
        .unwrap();

        write_claude_mirror_files(
            Some(mirror.to_str().unwrap()),
            "http://127.0.0.1:8989",
            "aag-proxy-token",
            Claude_mirror_mode::Passthrough,
        )
        .expect("write claude mirror");

        let settings = read_mirror_settings(&mirror);
        assert_eq!(
            settings["env"]["ANTHROPIC_BASE_URL"],
            "http://127.0.0.1:8989"
        );
        assert!(
            settings["env"].get("ANTHROPIC_AUTH_TOKEN").is_none(),
            "passthrough must not leave a token key behind: {}",
            settings["env"]
        );
    }

    /// An empty proxy token writes the PROXY_MANAGED placeholder (token
    /// mode) instead of an empty string.
    #[test]
    fn empty_token_writes_placeholder() {
        let (_home, home) = fresh_home("placeholder");

        write_claude_mirror_files(
            Some(home.to_str().unwrap()),
            "http://127.0.0.1:8989",
            "",
            Claude_mirror_mode::Token,
        )
        .expect("write claude mirror");

        let settings = read_mirror_settings(&home);
        assert_eq!(settings["env"]["ANTHROPIC_AUTH_TOKEN"], "PROXY_MANAGED");
    }

    /// A malformed mirror settings.json BLOCKS the write and is retained:
    /// invalid user JSON is never replaced silently (the validation doc's
    /// contract). The stale comment above the merge used to claim the
    /// opposite; this test pins the actual fail-closed behavior.
    #[test]
    fn malformed_mirror_settings_blocks_the_write_and_is_retained() {
        let (_home, home) = fresh_home("malformed");

        let mirror = config::get_claude_mirror_dir(Some(home.to_str().unwrap()));
        std::fs::create_dir_all(&mirror).unwrap();
        let broken = r#"{"env": { "ANTHROPIC_BASE_URL": "#;
        std::fs::write(mirror.join("settings.json"), broken).unwrap();

        let error = write_claude_mirror_files(
            Some(home.to_str().unwrap()),
            "http://127.0.0.1:8989",
            "aag-proxy-token",
            Claude_mirror_mode::Token,
        )
        .expect_err("malformed settings must block the write");
        assert!(matches!(error, AppError::Json(_)), "got: {error}");
        assert_eq!(
            std::fs::read_to_string(mirror.join("settings.json")).unwrap(),
            broken,
            "the invalid file must be retained untouched"
        );
    }

    /// A mirror override resolving to the native `~/.claude` is refused
    /// before any filesystem write.
    #[test]
    fn native_claude_dir_is_refused() {
        let (_home, _mirror) = fresh_home("native-guard");

        let native = config::get_claude_config_dir()
            .to_string_lossy()
            .into_owned();
        let err = write_claude_mirror_files(
            Some(native.as_str()),
            "http://127.0.0.1:8989",
            "t",
            Claude_mirror_mode::Token,
        )
        .expect_err("native ~/.claude must be refused");
        assert!(err.to_string().contains("native ~/.claude"), "{err}");
        assert!(
            !std::path::Path::new(&native).join("settings.json").exists(),
            "no file may be created in the native dir"
        );
    }

    /// A wildcard proxy bind must not be written into the client's
    /// `ANTHROPIC_BASE_URL`: the CLI needs a dialable endpoint, so the
    /// mirror translates `0.0.0.0` to the loopback.
    #[test]
    fn wildcard_bind_is_translated_to_a_dialable_client_endpoint() {
        let (_home, mirror) = fresh_home("wildcard-bind");
        let db = crate::database::Database::memory().unwrap();
        db.save_proxy_config(&crate::database::dao::proxy_config::ProxyConfig {
            host: "0.0.0.0".into(),
            ..crate::database::dao::proxy_config::ProxyConfig::default()
        })
        .unwrap();
        switch_claude_mirror(&db, Some(mirror.to_str().unwrap())).expect("mirror write");
        let settings = read_mirror_settings(&mirror);
        assert_eq!(
            settings["env"]["ANTHROPIC_BASE_URL"], "http://127.0.0.1:8989",
            "wildcard bind must reach the client as the loopback"
        );
    }

    /// Mode parsing fails safe to token for unknown/absent values.
    #[test]
    fn mode_parsing_fails_safe_to_token() {
        assert_eq!(
            Claude_mirror_mode::from_setting(Some("passthrough")),
            Claude_mirror_mode::Passthrough
        );
        assert_eq!(
            Claude_mirror_mode::from_setting(Some("token")),
            Claude_mirror_mode::Token
        );
        assert_eq!(
            Claude_mirror_mode::from_setting(Some("nonsense")),
            Claude_mirror_mode::Token
        );
        assert_eq!(
            Claude_mirror_mode::from_setting(None),
            Claude_mirror_mode::Token
        );
    }
}
