//! Kilo CLI provider adapter.
//!
//! Kilo reads a JSONC config file in the OpenCode-compatible projection
//! (`{ provider: { <id>: { npm, options, models } }, model }`). The Node
//! gateway already supports Kilo at `src/agents/adapters.ts:48-60, 250-297`
//! (the `KiloAdapter` class). This module is the Tauri-side mirror so the
//! desktop app can write Kilo's live config when a user switches provider.
//!
//! Five candidate paths (mirroring the Node adapter, in priority order):
//!   1. `~/.config/kilo/kilo.jsonc`
//!   2. `~/.config/kilo/kilo.json`
//!   3. `~/.config/kilo/opencode.jsonc`
//!   4. `~/.config/kilo/opencode.json`
//!   5. `~/.kilo/config.json`
//!
//! The first existing file is the write target. If none exist, the
//! adapter creates the highest-priority path (`kilo.jsonc`).
//!
//! The merge contract matches the Node adapter:
//!   - root must be a JSON object (else fail closed — never overwrite);
//!   - `provider` must be an object when present;
//!   - shallow-per-provider-id merge of the `provider` map;
//!   - top-level `model` overwritten when the patch carries a non-empty
//!     `model`;
//!   - JSONC `//` and `/* */` comments stripped before parsing (Kilo
//!     settings may be JSONC).
//!
//! Atomic writes with a timestamped backup live in `~/.aag/backups/` (per
//! `crate::config::get_backups_dir`).

use crate::error::AppError;
use crate::provider::Provider;
use crate::services::provider::LiveWriteResult;
use serde_json::{json, Value};
use std::path::PathBuf;

const DEFAULT_NPM: &str = "@ai-sdk/openai-compatible";

/// The five candidate config paths in priority order. Mirrors
/// `src/agents/adapters.ts:51-57`.
#[cfg(test)]
fn kilo_config_paths() -> [PathBuf; 5] {
    let home = crate::config::get_home_dir();
    [
        home.join(".config").join("kilo").join("kilo.jsonc"),
        home.join(".config").join("kilo").join("kilo.json"),
        home.join(".config").join("kilo").join("opencode.jsonc"),
        home.join(".config").join("kilo").join("opencode.json"),
        home.join(".kilo").join("config.json"),
    ]
}

/// Is any Kilo config file present (under `~/.config/kilo/*` or `~/.kilo/*`)?
#[allow(dead_code)]
pub fn detect() -> bool {
    super::config_paths::resolve("kilo_cli")
        .map(|resolved| resolved.existing)
        .unwrap_or(false)
}

/// Resolve the write target: first existing path, else the highest-priority
/// path (`kilo.jsonc`) so the first write creates a usable config.
#[cfg(test)]
fn resolve_config_path() -> PathBuf {
    super::config_paths::resolve("kilo_cli")
        .unwrap()
        .paths
        .remove(0)
}

/// Strip JSONC `//` line and `/* */` block comments (mirrors the Node
/// `KiloAdapter.read()` regex at `src/agents/adapters.ts:257-261`).
fn strip_jsonc_comments(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '/' if chars.peek() == Some(&'/') => {
                chars.next();
                for ch in chars.by_ref() {
                    if ch == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                while let Some(ch) = chars.next() {
                    if ch == '*' && chars.peek() == Some(&'/') {
                        chars.next();
                        break;
                    }
                }
            }
            _ => out.push(c),
        }
    }
    out
}

fn read_existing(path: &PathBuf) -> Result<Option<Value>, AppError> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(path).map_err(|e| AppError::io(path, e))?;
    let stripped = strip_jsonc_comments(&raw);
    let value: Value = serde_json::from_str(&stripped).map_err(AppError::Json)?;
    Ok(Some(value))
}

/// Build the patch the merge step writes into the Kilo config.
fn build_patch(provider: &Provider) -> Result<Value, AppError> {
    let entry = provider.settings_config.as_object().ok_or_else(|| {
        AppError::Config("Kilo provider settings_config must be a JSON object".into())
    })?;
    let options = entry
        .get("options")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            AppError::Config("Kilo provider requires an object 'options' field".into())
        })?;
    let base_url = options
        .get("baseURL")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::Config("Kilo provider requires options.baseURL".into()))?;
    let api_key = options
        .get("apiKey")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let npm = entry
        .get("npm")
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_NPM);
    let name = entry
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or(&provider.name);

    let models = entry
        .get("models")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    // Resolve model_id before consuming `models` (move into JSON below).
    let first_model = models.keys().next().cloned();
    let model_id = entry
        .get("model")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .or(first_model);

    let mut provider_obj = serde_json::Map::new();
    provider_obj.insert("npm".into(), Value::String(npm.to_string()));
    provider_obj.insert("name".into(), Value::String(name.to_string()));
    let mut opts_obj = serde_json::Map::new();
    opts_obj.insert("baseURL".into(), Value::String(base_url.to_string()));
    if !api_key.is_empty() {
        opts_obj.insert("apiKey".into(), Value::String(api_key));
    }
    provider_obj.insert("options".into(), Value::Object(opts_obj));
    provider_obj.insert("models".into(), Value::Object(models));

    let mut patch = serde_json::Map::new();
    let mut providers = serde_json::Map::new();
    providers.insert(provider.id.clone(), Value::Object(provider_obj));
    patch.insert("provider".into(), Value::Object(providers));
    if let Some(model_id) = model_id {
        patch.insert(
            "model".into(),
            Value::String(format!("{}/{}", provider.id, model_id)),
        );
    }
    Ok(Value::Object(patch))
}

/// Write one AAG provider into the Kilo config. Mirrors the Node
/// `KiloAdapter.write()` at `src/agents/adapters.ts:263-282` with one
/// addition: atomic write + timestamped backup of any pre-existing file.
pub fn write_live(provider: &Provider) -> Result<LiveWriteResult, AppError> {
    let resolved = super::config_paths::resolve("kilo_cli")?;
    write_live_at(provider, &resolved.paths)
}

/// Apply to the exact CLI or extension path chosen by configuration discovery.
pub fn write_live_at(provider: &Provider, paths: &[PathBuf]) -> Result<LiveWriteResult, AppError> {
    let [path] = paths else {
        return Err(AppError::Config("AAG_CLIENT_CONFIG_PATHS_INVALID".into()));
    };
    // Harden the parent directory (typically `~/.config/kilo` or
    // `~/.kilo`) to AAG owner-only. Mirrors
    // `claude_mirror::write_claude_mirror_files` and the existing
    // codex-mirror pattern — see the comment there for the rationale
    // (Windows profile ACL with untrusted Modify grants).
    if let Some(parent) = path.parent() {
        crate::config::private_fs::prepare_managed_private_dir(parent)?;
    }
    crate::config::private_fs::validate_external_file_path(path)?;
    let patch = build_patch(provider)?;

    let mut merged: Value = json!({});
    if let Some(existing) = read_existing(&path)? {
        let obj = existing.as_object().ok_or_else(|| {
            AppError::Config(format!(
                "Kilo config {} must be a JSON object",
                path.display()
            ))
        })?;
        if let Some(p) = obj.get("provider") {
            if !p.is_object() {
                return Err(AppError::Config(format!(
                    "Kilo config {} 'provider' must be an object",
                    path.display()
                )));
            }
        }
        merged = existing;
    }

    let patch_provider = patch
        .get("provider")
        .and_then(Value::as_object)
        .ok_or_else(|| AppError::Config("Kilo patch must contain an object provider".into()))?;
    let merged_obj = merged
        .as_object_mut()
        .expect("merged is initialized as object");
    let merged_provider = merged_obj
        .entry(String::from("provider"))
        .or_insert_with(|| json!({}));
    let merged_provider_obj = merged_provider.as_object_mut().ok_or_else(|| {
        AppError::Config(format!(
            "Kilo config {} 'provider' must be an object",
            path.display()
        ))
    })?;
    for (k, v) in patch_provider {
        merged_provider_obj.insert(k.clone(), v.clone());
    }
    if let Some(model) = patch.get("model").and_then(Value::as_str) {
        if !model.is_empty() {
            merged_obj.insert(String::from("model"), Value::String(model.to_string()));
        }
    }

    let serialized = serde_json::to_vec_pretty(&merged)?;
    let (written, backups) =
        super::config_writes::write_files(&[super::config_writes::ConfigFileUpdate {
            path,
            contents: &serialized,
            app_type: "kilo_cli",
        }])?;

    Ok(LiveWriteResult {
        app_type: String::from("kilo_cli"),
        provider_id: provider.id.clone(),
        written,
        backup: backups.into_iter().next(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::provider::test_support::isolated_home;
    use serde_json::json;
    use std::fs;

    fn provider() -> Provider {
        Provider::new(
            "aag".into(),
            "AAG".into(),
            json!({
                "options": {
                    "baseURL": "https://example.invalid/v1",
                    "apiKey": "sk-secret"
                },
                "models": { "glm-5": { "name": "GLM-5" } }
            }),
        )
    }

    #[test]
    fn resolve_config_path_creates_kilo_jsonc_when_none_exist() {
        let _home = isolated_home("kilo");
        let p = resolve_config_path();
        assert!(p.ends_with(".config/kilo/kilo.jsonc"));
    }

    #[test]
    fn resolve_config_path_picks_first_existing_path() {
        let home = isolated_home("kilo");
        let kilo_dir = home.root.join(".config").join("kilo");
        fs::create_dir_all(&kilo_dir).unwrap();
        // Create the 3rd-priority path (`opencode.jsonc`). It must be picked
        // ahead of `kilo.jsonc`/`kilo.json`.
        let target = kilo_dir.join("opencode.jsonc");
        fs::write(&target, "{}").unwrap();
        let p = resolve_config_path();
        assert_eq!(p, target, "first existing path wins");
    }

    #[test]
    fn write_live_creates_kilo_jsonc_with_correct_projection() {
        let home = isolated_home("kilo");
        let result = write_live(&provider()).expect("Kilo write");
        assert_eq!(result.app_type, "kilo_cli");
        assert!(result.backup.is_none(), "no prior file → no backup");

        let path = kilo_config_paths()[0].clone();
        assert!(path.exists());
        let value: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value["provider"]["aag"]["npm"], "@ai-sdk/openai-compatible");
        assert_eq!(
            value["provider"]["aag"]["options"]["baseURL"],
            "https://example.invalid/v1"
        );
        assert_eq!(value["provider"]["aag"]["options"]["apiKey"], "sk-secret");
        assert_eq!(value["provider"]["aag"]["models"]["glm-5"]["name"], "GLM-5");
        assert_eq!(value["model"], "aag/glm-5");
        drop(home);
    }

    #[test]
    fn write_live_preserves_other_providers_and_unrelated_keys() {
        let home = isolated_home("kilo");
        let path = kilo_config_paths()[0].clone();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(
            &path,
            r#"{
                // comment line
                "theme": "dark",
                "small_model": "aag/glm-mini",
                "provider": {
                    "openai": { "npm": "@ai-sdk/openai-compatible", "options": { "baseURL": "https://openai.invalid" } },
                    "aag":    { "npm": "@ai-sdk/openai-compatible", "options": { "baseURL": "https://old.invalid" }, "models": {} }
                }
            }"#,
        )
        .unwrap();

        write_live(&provider()).expect("Kilo write");
        let value: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value["theme"], "dark");
        assert_eq!(value["small_model"], "aag/glm-mini");
        assert_eq!(
            value["provider"]["openai"]["options"]["baseURL"],
            "https://openai.invalid"
        );
        assert_eq!(
            value["provider"]["aag"]["options"]["baseURL"], "https://example.invalid/v1",
            "aag entry replaced"
        );
        drop(home);
    }

    #[test]
    fn write_live_fails_closed_on_array_root() {
        let home = isolated_home("kilo");
        let path = kilo_config_paths()[0].clone();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, "[]").unwrap();
        let err = write_live(&provider()).expect_err("array root must fail");
        assert!(err.to_string().contains("JSON object"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "[]", "file unchanged");
        drop(home);
    }

    #[test]
    fn write_live_fails_closed_on_non_object_provider() {
        let home = isolated_home("kilo");
        let path = kilo_config_paths()[0].clone();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, r#"{ "provider": [] }"#).unwrap();
        let err = write_live(&provider()).expect_err("non-object provider must fail");
        assert!(err.to_string().contains("provider"));
        drop(home);
    }

    #[test]
    fn write_live_rejects_missing_base_url() {
        let _home = isolated_home("kilo");
        let bad = Provider::new(
            "bad".into(),
            "Bad".into(),
            json!({ "options": { "apiKey": "sk" }, "models": {} }),
        );
        let err = write_live(&bad).expect_err("missing endpoint must fail");
        assert!(err.to_string().contains("baseURL"));
    }

    #[test]
    fn write_live_strips_jsonc_comments_before_parsing() {
        let home = isolated_home("kilo");
        let path = kilo_config_paths()[0].clone();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(
            &path,
            r#"/* header */
{
    // existing user comment
    "theme": "dark" /* trailing */,
    "provider": {}
}"#,
        )
        .unwrap();
        write_live(&provider()).expect("Kilo write tolerates JSONC");
        let value: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value["theme"], "dark");
        assert_eq!(
            value["provider"]["aag"]["options"]["baseURL"],
            "https://example.invalid/v1"
        );
        drop(home);
    }

    #[test]
    fn write_live_writes_into_existing_kilo_dir_path() {
        let home = isolated_home("kilo");
        // Pre-create `~/.kilo/config.json` (lowest priority). The adapter
        // must pick it (existing file wins over creating a higher-priority
        // file).
        let kilo_dir = home.root.join(".kilo");
        fs::create_dir_all(&kilo_dir).unwrap();
        let existing = kilo_dir.join("config.json");
        fs::write(&existing, r#"{ "theme": "light" }"#).unwrap();

        let result = write_live(&provider()).expect("Kilo write");
        // The resolved candidate is built from the literal `".kilo/config.json"`
        // (forward slash) while `existing` uses `PathBuf::join` (backslash on
        // Windows). Compare as paths so the separator does not matter.
        assert_eq!(
            std::path::PathBuf::from(result.written[0].as_str()),
            existing,
            "write target must be the pre-existing lowest-priority file"
        );
        let value: Value = serde_json::from_str(&fs::read_to_string(&existing).unwrap()).unwrap();
        assert_eq!(value["theme"], "light", "unrelated keys preserved");
        assert_eq!(
            value["provider"]["aag"]["options"]["baseURL"],
            "https://example.invalid/v1"
        );
    }
}
