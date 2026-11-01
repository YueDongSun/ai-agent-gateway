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
    // Match the fill path's resolution: `GEMINI_CLI_HOME` replaces the home
    // root (config_paths), so an overridden install must report detected
    // too. Fall back to the plain config dir when resolution fails.
    let resolved_dir = crate::services::provider::config_paths::resolve("gemini")
        .ok()
        .and_then(|paths| {
            paths
                .paths
                .first()
                .and_then(|path| path.parent())
                .map(std::path::Path::to_path_buf)
        });
    match resolved_dir {
        Some(dir) => dir.exists(),
        None => crate::config::get_gemini_config_dir().exists(),
    }
}

/// Keys AAG manages inside `~/.gemini/.env`. On every write each of them is
/// either replaced with the caller's value or removed entirely — a previous
/// provider's value must never leak into the next one. Everything else a
/// user set (GOOGLE_CLOUD_PROJECT, proxy vars, comments, unparsable lines)
/// is preserved verbatim.
const MANAGED_ENV_KEYS: [&str; 3] = ["GOOGLE_GEMINI_BASE_URL", "GEMINI_API_KEY", "GEMINI_MODEL"];

/// One parsed line of a `.env` file.
#[allow(non_camel_case_types)]
#[derive(Debug, PartialEq, Eq)]
enum Env_line {
    /// `KEY=VALUE` (an optional `export ` prefix is tolerated). `raw` keeps
    /// the original spelling so unmanaged assignments are preserved exactly.
    Assignment { key: String, raw: String },
    /// Comment, blank line, or text without `=` — preserved verbatim.
    Verbatim(String),
}

/// Parse a `.env` file into ordered lines plus the effective KEY=VALUE map
/// (lax: comments and unparsable lines are kept verbatim; `export ` prefixes
/// tolerated; the last assignment for a key wins in the map).
fn parse_env_file(raw: &str) -> (Vec<Env_line>, BTreeMap<String, String>) {
    let mut lines = Vec::new();
    let mut entries = BTreeMap::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        let without_export = trimmed.strip_prefix("export ").unwrap_or(trimmed);
        if let Some((key, value)) = without_export.split_once('=') {
            let key = key.trim().to_string();
            if !key.is_empty() {
                entries.insert(key.clone(), value.trim().trim_matches('"').to_string());
                lines.push(Env_line::Assignment {
                    key,
                    raw: line.to_string(),
                });
                continue;
            }
        }
        lines.push(Env_line::Verbatim(line.to_string()));
    }
    (lines, entries)
}

/// Rebuild the file: managed assignments are replaced (first occurrence wins,
/// later duplicates are dropped), `cleared` managed keys are dropped, every
/// other line is preserved verbatim in its original order, and managed keys
/// absent from the file are appended.
fn serialize_env_file(
    lines: &[Env_line],
    managed: &BTreeMap<String, String>,
    cleared: &[&str],
) -> String {
    let mut out = String::new();
    let mut emitted = std::collections::BTreeSet::new();
    for line in lines {
        match line {
            Env_line::Assignment { key, .. } if managed.contains_key(key) => {
                if emitted.insert(key.as_str()) {
                    out.push_str(&format!("{key}={}\n", managed[key.as_str()]));
                }
            }
            Env_line::Assignment { key, .. } if cleared.contains(&key.as_str()) => {}
            Env_line::Assignment { raw, .. } | Env_line::Verbatim(raw) => {
                out.push_str(raw);
                out.push('\n');
            }
        }
    }
    for (key, value) in managed {
        if !emitted.contains(key.as_str()) {
            out.push_str(&format!("{key}={value}\n"));
        }
    }
    out
}

/// Read `~/.gemini/.env`, refusing symlink/reparse targets (the mirror
/// precedent: the directory is owned by the CLI, not AAG-managed storage).
fn read_env_file(
    path: &std::path::Path,
) -> Result<(Vec<Env_line>, BTreeMap<String, String>), AppError> {
    crate::config::private_fs::validate_external_file_path(path)?;
    if !path.exists() {
        return Ok((Vec::new(), BTreeMap::new()));
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
    let proxy_cfg = db.get_proxy_config()?;
    // A wildcard or unset bind is not a client target — `ProxyConfig`
    // translates it to the loopback the CLI can dial; a routable host the
    // user configured is kept as-is.
    let host = proxy_cfg.client_endpoint_host();
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
    // ── .env: managed keys replaced or cleared, everything else preserved verbatim ──
    let (lines, _existing) = read_env_file(env_path)?;
    let cleared: Vec<&str> = MANAGED_ENV_KEYS
        .iter()
        .copied()
        .filter(|key| !managed.contains_key(*key))
        .collect();
    let env_contents = serialize_env_file(&lines, managed, &cleared);

    // ── settings.json: shallow merge of settings_config.config ──
    let settings_contents = if let Some(config) = provider
        .settings_config
        .get("config")
        .filter(|c| c.is_object())
    {
        let mut settings: serde_json::Value = if settings_path.exists() {
            // The earlier validation ran before the .env read; a link swapped
            // in between must not be followed (read_env_file parity).
            if std::fs::symlink_metadata(settings_path)
                .map(|meta| meta.file_type().is_symlink())
                .unwrap_or(false)
            {
                return Err(AppError::Config(format!(
                    "refusing to write {}: target is a symlink or reparse point",
                    settings_path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                )));
            }
            let raw = std::fs::read_to_string(settings_path)
                .map_err(|e| AppError::io(settings_path, e))?;
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
    fn env_file_round_trip_preserves_unmanaged_keys_and_verbatim_lines() {
        let raw = "# comment\nGOOGLE_GEMINI_BASE_URL=https://old.example.com\nexport GEMINI_API_KEY=\"old-key\"\nGOOGLE_CLOUD_PROJECT=my-project\nMY_FLAG\n\n";
        let (lines, entries) = parse_env_file(raw);
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
        // Comments, unparsable lines, and blanks are kept verbatim in order.
        let serialized = serialize_env_file(&lines, &BTreeMap::new(), &[]);
        assert_eq!(serialized, raw);
        let (reparsed_lines, reparsed_entries) = parse_env_file(&serialized);
        assert_eq!(reparsed_entries, entries);
        assert_eq!(reparsed_lines, lines);
    }

    #[test]
    fn env_file_managed_keys_are_replaced_without_touching_user_lines() {
        let raw = "# provider block\nexport GEMINI_API_KEY=\"old\"\nGOOGLE_CLOUD_PROJECT=keep\nGEMINI_API_KEY=dup\nSTALE_MODEL=x\n";
        let (lines, _entries) = parse_env_file(raw);
        let mut managed = BTreeMap::new();
        managed.insert("GEMINI_API_KEY".to_string(), "new".to_string());
        managed.insert("GEMINI_MODEL".to_string(), "gemini-2.5-pro".to_string());
        let serialized = serialize_env_file(&lines, &managed, &["STALE_MODEL"]);
        assert_eq!(
            serialized,
            "# provider block\nGEMINI_API_KEY=new\nGOOGLE_CLOUD_PROJECT=keep\nGEMINI_MODEL=gemini-2.5-pro\n"
        );
    }

    #[test]
    fn write_live_writes_env_and_merges_settings() {
        // The WIP fixture built its own temp root and hardened it to
        // owner-only; `isolated_home` does both (temp root + owner-only DACL
        // via `prepare_managed_private_dir`) and also captures/restores the
        // AAG_* env overrides on Drop.
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

        // A second write with different managed keys preserves unmanaged
        // assignments, comments, and unparsable lines verbatim.
        let provider2 = Provider::new(
            "prov-2".into(),
            "Relay".into(),
            serde_json::json!({
                "env": { "GOOGLE_GEMINI_BASE_URL": "https://relay.example.com", "GEMINI_API_KEY": "k2" }
            }),
        );
        std::fs::write(
            home.root.join(".gemini").join(".env"),
            "# user header\nGOOGLE_CLOUD_PROJECT=keep-me\nMY_FLAG\nexport GOOGLE_GEMINI_BASE_URL=https://old\nGEMINI_MODEL=old-model\n",
        )
        .unwrap();
        write_live(&provider2).expect("second write_live");
        let env_raw = std::fs::read_to_string(home.root.join(".gemini").join(".env")).unwrap();
        assert!(env_raw.contains("# user header"));
        assert!(env_raw.contains("GOOGLE_CLOUD_PROJECT=keep-me"));
        assert!(
            env_raw.contains("\nMY_FLAG\n"),
            "unparsable line preserved: {env_raw}"
        );
        assert!(env_raw.contains("GOOGLE_GEMINI_BASE_URL=https://relay.example.com"));
        // Managed keys are replaced; later duplicates of a managed key are
        // dropped instead of shadowing the new value.
        assert_eq!(
            env_raw.matches("GOOGLE_GEMINI_BASE_URL=").count(),
            1,
            "managed key duplicates must be dropped: {env_raw}"
        );
        // GEMINI_MODEL from the first provider is a managed key and was replaced.
        assert!(!env_raw.contains("GEMINI_MODEL"));
    }
}
