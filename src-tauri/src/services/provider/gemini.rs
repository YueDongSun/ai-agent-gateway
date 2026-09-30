//! Gemini CLI adapter.
//!
//! The Gemini CLI reads its endpoint configuration from `~/.gemini/.env`
//! (`GOOGLE_GEMINI_BASE_URL` redirects the Generative Language API traffic)
//! and user settings from `~/.gemini/settings.json`. Two write modes:
//!
//! - [`write_live`] (direct): the provider's own `env` block lands in `.env`
//!   verbatim — the CLI talks straight to the provider's upstream.
//! - [`switch_gemini_mirror`] (unified upstream): `.env` points exclusively
//!   at AAG's proxy and the CLI authenticates with AAG's own proxy token;
//!   AAG holds the upstream keys and serves the Gemini-native surface with
//!   per-app failover. This is the cc-switch `takeover_live_configs` pattern
//!   and the direct analog of the Codex mirror contract
//!   (`services/provider/codex.rs::write_mirror_proxy_files`).
//!
//! The AAG `settings_config` shape (cc-switch-compatible):
//! ```json
//! {
//!   "env": {
//!     "GOOGLE_GEMINI_BASE_URL": "https://generativelanguage.googleapis.com",
//!     "GEMINI_API_KEY": "sk-...",
//!     "GEMINI_MODEL": "gemini-2.5-pro"
//!   },
//!   "api": "gemini" | "anthropic" | "openai",
//!   "config": { "merge-into": "settings.json" },
//!   "modelMapping": { "gemini-2.5-pro": "gemini-2.5-pro-preview-06-05" }
//! }
//! ```

use crate::error::AppError;
use crate::provider::Provider;
use crate::services::provider::LiveWriteResult;
use std::collections::BTreeMap;

/// Is the Gemini CLI installed? (config dir exists)
#[allow(dead_code)]
pub fn detect() -> bool {
    crate::config::get_gemini_config_dir().exists()
}

/// Keys AAG manages inside `~/.gemini/.env`. Everything else a user set
/// (GOOGLE_CLOUD_PROJECT, proxy vars, …) is preserved verbatim.
const MANAGED_ENV_KEYS: [&str; 3] = ["GOOGLE_GEMINI_BASE_URL", "GEMINI_API_KEY", "GEMINI_MODEL"];

/// Parse a `.env` file into ordered KEY=VALUE pairs (lax: comments and blank
/// lines are skipped; `export ` prefixes tolerated).
fn parse_env_file(raw: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for line in raw.lines() {
        let line = line.trim().strip_prefix("export ").unwrap_or(line.trim());
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            out.insert(
                key.trim().to_string(),
                value.trim().trim_matches('"').to_string(),
            );
        }
    }
    out
}

fn serialize_env_file(entries: &BTreeMap<String, String>) -> String {
    let mut out = String::new();
    for (key, value) in entries {
        out.push_str(&format!("{key}={value}\n"));
    }
    out
}

/// Read `~/.gemini/.env`, refusing symlink/reparse targets (the mirror
/// precedent: the directory is owned by the CLI, not AAG-managed storage).
fn read_env_file(path: &std::path::Path) -> Result<BTreeMap<String, String>, AppError> {
    crate::config::private_fs::validate_external_file_path(path)?;
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    if std::fs::symlink_metadata(path)
        .map(|meta| meta.file_type().is_symlink())
        .unwrap_or(false)
    {
        return Err(AppError::Config(format!(
            "refusing to write {}: target is a symlink or reparse point",
            path.file_name().unwrap_or_default().to_string_lossy()
        )));
    }
    let raw = std::fs::read_to_string(path).map_err(|e| AppError::io(path, e))?;
    Ok(parse_env_file(&raw))
}

/// Write the provider's own `env` block to `~/.gemini/.env` and merge
/// `settings_config.config` into `~/.gemini/settings.json` (direct mode —
/// no proxy in between).
#[allow(dead_code)] // live-config write path retained for tests/history; not called in the app since Phase 15.9
pub fn write_live(provider: &Provider) -> Result<LiveWriteResult, AppError> {
    let env = provider
        .settings_config
        .get("env")
        .and_then(|v| v.as_object())
        .ok_or_else(|| {
            AppError::Config("gemini provider settings_config.env must be an object".into())
        })?;
    let mut entries: BTreeMap<String, String> = BTreeMap::new();
    for (key, value) in env {
        if let Some(value) = value.as_str() {
            entries.insert(key.clone(), value.to_string());
        }
    }
    if entries.is_empty() {
        return Err(AppError::Config(
            "gemini provider requires env.GOOGLE_GEMINI_BASE_URL".into(),
        ));
    }
    let paths = super::config_paths::resolve("gemini")?;
    write_gemini_files(&paths.paths[0], &paths.paths[1], &entries, provider)
}

/// Point the Gemini CLI exclusively at AAG's proxy (unified-upstream mode).
/// Mirrors [`crate::services::provider::codex::switch_codex_mirror`]: the
/// CLI sees only AAG's endpoint + AAG's own proxy token; the provider's
/// upstream credentials stay in AAG's database and drive the failover chain.
pub fn switch_gemini_mirror(db: &crate::database::Database) -> Result<LiveWriteResult, AppError> {
    let provider = db
        .get_current_provider("gemini")?
        .map(|id| db.get_provider_by_id(&id, "gemini"))
        .transpose()?
        .flatten()
        .unwrap_or_else(|| Provider::new(String::new(), String::new(), serde_json::json!({})));
    switch_gemini_mirror_for_provider(db, &provider)
}

/// Write the selected provider's model and settings before updating DB current.
pub fn switch_gemini_mirror_for_provider(
    db: &crate::database::Database,
    provider: &Provider,
) -> Result<LiveWriteResult, AppError> {
    let proxy_cfg = db.get_proxy_config().map_err(AppError::from)?;
    // 0.0.0.0 means "listen everywhere" but is not a valid client target.
    let host = if proxy_cfg.host == "0.0.0.0" {
        "127.0.0.1".to_string()
    } else if proxy_cfg.host.is_empty() {
        "127.0.0.1".to_string()
    } else {
        proxy_cfg.host.clone()
    };
    // No trailing /v1 — the CLI appends /v1beta/models/{model}:method itself.
    let base_url = format!("http://{host}:{}", proxy_cfg.port);
    // When auth is disabled any value passes the check; the placeholder keeps
    // the CLI from warning about a missing key (cc-switch parity).
    let api_key = if proxy_cfg.auth_token.is_empty() {
        "PROXY_MANAGED".to_string()
    } else {
        proxy_cfg.auth_token.clone()
    };

    let mut entries = BTreeMap::new();
    entries.insert("GOOGLE_GEMINI_BASE_URL".to_string(), base_url);
    entries.insert("GEMINI_API_KEY".to_string(), api_key);
    // Carry the provider's default model so the CLI requests the mapped
    // alias out of the box.
    if let Some(model) = provider
        .settings_config
        .pointer("/env/GEMINI_MODEL")
        .and_then(|value| value.as_str())
        .filter(|model| !model.is_empty())
    {
        entries.insert("GEMINI_MODEL".to_string(), model.to_string());
    }

    let paths = super::config_paths::resolve("gemini")?;
    write_gemini_files(&paths.paths[0], &paths.paths[1], &entries, provider)
}

/// Write `.env` (managed keys updated, user keys preserved) and merge the
/// provider's `config` object into `settings.json` preserving unknown keys
/// (mcpServers, themes, ...). Backs up both files before overwrite.
fn write_gemini_files(
    env_path: &std::path::Path,
    settings_path: &std::path::Path,
    managed: &BTreeMap<String, String>,
    provider: &Provider,
) -> Result<LiveWriteResult, AppError> {
    // Harden the parent directory (typically `~/.gemini`) to AAG owner-only.
    // Mirrors `claude_mirror::write_claude_mirror_files` and the existing
    // codex-mirror pattern — without this, profile dirs whose ACL carries
    // untrusted Modify grants (e.g. a sandbox group) cause
    // `prepare_external_parent_dir` to fail with
    // `external_directory_write_unsafe` on Windows.
    if let Some(parent) = env_path.parent() {
        crate::config::private_fs::prepare_managed_private_dir(parent)?;
    }
    if let Some(parent) = settings_path.parent() {
        crate::config::private_fs::prepare_managed_private_dir(parent)?;
    }
    crate::config::private_fs::validate_external_file_path(env_path)?;
    crate::config::private_fs::validate_external_file_path(settings_path)?;
    // ── .env: managed keys replaced, everything else preserved ──
    let mut entries = read_env_file(env_path)?;
    for key in MANAGED_ENV_KEYS {
        entries.remove(key);
    }
    for (key, value) in managed {
        entries.insert(key.clone(), value.clone());
    }
    let env_contents = serialize_env_file(&entries);

    // ── settings.json: shallow merge of settings_config.config ──
    let settings_contents = if let Some(config) = provider
        .settings_config
        .get("config")
        .filter(|c| c.is_object())
    {
        let mut settings: serde_json::Value = if settings_path.exists() {
            let raw = std::fs::read_to_string(settings_path)
                .map_err(|e| AppError::io(&settings_path, e))?;
            serde_json::from_str(&raw).map_err(AppError::Json)?
        } else {
            serde_json::json!({})
        };
        if !settings.is_object() {
            return Err(AppError::Config(
                "Gemini settings.json root must be an object".into(),
            ));
        }
        if let (Some(target), Some(source)) = (settings.as_object_mut(), config.as_object()) {
            for (key, value) in source {
                target.insert(key.clone(), value.clone());
            }
        }
        Some(serde_json::to_vec_pretty(&settings)?)
    } else {
        None
    };
    let mut updates = vec![super::config_writes::ConfigFileUpdate {
        path: env_path,
        contents: env_contents.as_bytes(),
        app_type: "gemini",
    }];
    if let Some(contents) = &settings_contents {
        updates.push(super::config_writes::ConfigFileUpdate {
            path: settings_path,
            contents,
            app_type: "gemini",
        });
    }
    let (written, backups) = super::config_writes::write_files(&updates)?;

    Ok(LiveWriteResult {
        app_type: "gemini".into(),
        provider_id: provider.id.clone(),
        written,
        backup: backups.into_iter().next(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::provider::test_support::isolated_home;

    #[test]
    fn env_file_round_trip_preserves_unmanaged_keys() {
        let raw = "# comment\nGOOGLE_GEMINI_BASE_URL=https://old.example.com\nexport GEMINI_API_KEY=\"old-key\"\nGOOGLE_CLOUD_PROJECT=my-project\n";
        let entries = parse_env_file(raw);
        assert_eq!(
            entries.get("GOOGLE_GEMINI_BASE_URL").map(String::as_str),
            Some("https://old.example.com")
        );
        assert_eq!(
            entries.get("GEMINI_API_KEY").map(String::as_str),
            Some("old-key")
        );
        assert_eq!(
            entries.get("GOOGLE_CLOUD_PROJECT").map(String::as_str),
            Some("my-project")
        );
        let serialized = serialize_env_file(&entries);
        let reparsed = parse_env_file(&serialized);
        assert_eq!(reparsed, entries);
    }

    #[test]
    fn write_live_writes_env_and_merges_settings() {
        let home = isolated_home("gemini");

        let provider = Provider::new(
            "prov-1".into(),
            "Google Gemini".into(),
            serde_json::json!({
                "env": {
                    "GOOGLE_GEMINI_BASE_URL": "https://generativelanguage.googleapis.com",
                    "GEMINI_API_KEY": "sk-test",
                    "GEMINI_MODEL": "gemini-2.5-pro"
                },
                "config": { "theme": "dark" }
            }),
        );
        let result = write_live(&provider).expect("write_live");
        assert!(result.written.iter().any(|p| p.ends_with(".env")));
        assert!(result.written.iter().any(|p| p.ends_with("settings.json")));

        let env_raw = std::fs::read_to_string(home.root.join(".gemini").join(".env")).unwrap();
        assert!(
            env_raw.contains("GOOGLE_GEMINI_BASE_URL=https://generativelanguage.googleapis.com")
        );
        assert!(env_raw.contains("GEMINI_API_KEY=sk-test"));
        let settings: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(home.root.join(".gemini").join("settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(settings["theme"], "dark");

        // A second write with different managed keys preserves unmanaged ones.
        let provider2 = Provider::new(
            "prov-2".into(),
            "Relay".into(),
            serde_json::json!({
                "env": { "GOOGLE_GEMINI_BASE_URL": "https://relay.example.com", "GEMINI_API_KEY": "k2" }
            }),
        );
        // Seed an unmanaged key first.
        std::fs::write(
            home.root.join(".gemini").join(".env"),
            "GOOGLE_CLOUD_PROJECT=keep-me\n",
        )
        .unwrap();
        write_live(&provider2).expect("second write_live");
        let env_raw = std::fs::read_to_string(home.root.join(".gemini").join(".env")).unwrap();
        assert!(env_raw.contains("GOOGLE_CLOUD_PROJECT=keep-me"));
        assert!(env_raw.contains("GOOGLE_GEMINI_BASE_URL=https://relay.example.com"));
        // GEMINI_MODEL from the first provider is a managed key and was replaced.
        assert!(!env_raw.contains("GEMINI_MODEL"));
    }
}
