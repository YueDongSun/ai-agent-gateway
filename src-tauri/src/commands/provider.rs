//! Provider CRUD commands + the failover-queue endpoint extractor.
//!
//! Extracted from `lib.rs` in the file-structure refactor - the command
//! bodies are moved verbatim; only module paths in `use` statements and
//! `pub(crate)` visibility changed.


use crate::database::Database;
use crate::provider::Provider;

// ----- Provider commands (Phase 1) -----

#[tauri::command]
pub(crate) fn provider_list(db: tauri::State<'_, Database>, app_type: String) -> Result<Vec<Provider>, String> {
    db.get_all_providers(&app_type).map(|m| m.into_iter().map(|(_, v)| v).collect()).map_err(String::from)
}
#[tauri::command]
pub(crate) fn provider_current(db: tauri::State<'_, Database>, app_type: String) -> Result<Option<String>, String> {
    db.get_current_provider(&app_type).map_err(String::from)
}
#[tauri::command]
pub(crate) fn provider_get(db: tauri::State<'_, Database>, id: String, app_type: String) -> Result<Option<Provider>, String> {
    db.get_provider_by_id(&id, &app_type).map_err(String::from)
}
#[tauri::command]
pub(crate) fn provider_save(db: tauri::State<'_, Database>, app_type: String, provider: Provider) -> Result<(), String> {
    db.save_provider(&app_type, &provider).map_err(String::from)
}
#[tauri::command]
pub(crate) fn provider_delete(db: tauri::State<'_, Database>, id: String, app_type: String) -> Result<(), String> {
    db.delete_provider(&id, &app_type).map_err(String::from)
}
#[tauri::command]
/// Mark a provider as current in the DB and (per app) write the live config
/// files.
///
/// Live-config split (Phase 17):
/// - `codex` -> write to the mirror CODEX_HOME (`<codex_cli_home>/config.toml`
///   + `auth.json`). The user's native `~/.codex` is **never** touched.
/// - `copilot` -> merge `model` + `env` into `~/.copilot/settings.json`.
/// - `pi` -> upsert the provider into `~/.pi/agent/models.json` + default
///   model into `~/.pi/agent/settings.json`.
/// - `claude_code` -> no live write. cc-switch owns `~/.claude/settings.json`;
///   AAG is just an upstream proxy at `http://127.0.0.1:8989` that cc-switch
///   chains through.
pub(crate) fn provider_switch(
    db: tauri::State<'_, Database>,
    id: String,
    app_type: String,
) -> Result<crate::services::provider::LiveWriteResult, String> {
    let _app = crate::services::provider::AppType::from_db_str(&app_type).map_err(String::from)?;
    let provider = db.get_provider_by_id(&id, &app_type)
        .map_err(String::from)?
        .ok_or_else(|| format!("provider \"{id}\" not found for app \"{app_type}\""))?;

    db.set_current_provider(&id, &app_type).map_err(String::from)?;

    let live = match app_type.as_str() {
        "codex" => {
            let codex_cli_home = db
                .get_setting(crate::database::dao::settings::KEY_CODEX_CLI_HOME)
                .map_err(String::from)?;
            crate::services::provider::codex::write_live(&provider, codex_cli_home.as_deref()).ok()
        }
        "copilot" => crate::services::provider::copilot::write_live(&provider).ok(),
        "pi" => crate::services::provider::pi::write_live(&provider).ok(),
        _ => None,
    };
    if live.is_none() {
        if app_type != "claude_code" {
            log::warn!("provider_switch: {app_type} live write failed or unsupported");
        }
    }

    Ok(live.unwrap_or(crate::services::provider::LiveWriteResult {
        app_type,
        provider_id: provider.id,
        written: vec![],
        backup: None,
    }))
}

pub(crate) fn queue_endpoint_of(
    app_type: &str,
    cfg: &serde_json::Value,
) -> Option<(String, String, String)> {
    let env = cfg.get("env").cloned().unwrap_or(serde_json::json!({}));
    match app_type {
        "claude_code" => {
            let base = env.get("ANTHROPIC_BASE_URL")?.as_str()?.to_string();
            if base.is_empty() {
                return None;
            }
            let key = env
                .get("ANTHROPIC_AUTH_TOKEN")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Some((base, key, "anthropic".into()))
        }
        "codex" => {
            // Flat fields first (the smart form writes these)...
            if let Some(base) = cfg.get("baseUrl").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
                let key = cfg.get("apiKey").and_then(|v| v.as_str()).unwrap_or("").to_string();
                return Some((base.to_string(), key, "openai".into()));
            }
            // ...then the cc-switch TOML `config` string (model_providers.aag.base_url).
            let toml_str = cfg.get("config").and_then(|v| v.as_str())?;
            let base = toml_str
                .lines()
                .find_map(|l| {
                    let l = l.trim();
                    let (k, v) = l.split_once('=')?;
                    if k.trim() == "base_url" {
                        Some(v.trim().trim_matches('"').to_string())
                    } else {
                        None
                    }
                })?
                .to_string();
            if base.is_empty() {
                return None;
            }
            let key = cfg
                .get("auth")
                .and_then(|a| a.get("tokens"))
                .and_then(|t| t.get("value"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Some((base, key, "openai".into()))
        }
        "copilot" => {
            let base = env.get("COPILOT_PROVIDER_BASE_URL")?.as_str()?.to_string();
            if base.is_empty() {
                return None;
            }
            let key = env
                .get("COPILOT_PROVIDER_API_KEY")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let wire = env
                .get("COPILOT_PROVIDER_TYPE")
                .and_then(|v| v.as_str())
                .unwrap_or("openai");
            let provider_type = if wire == "anthropic" { "anthropic" } else { "openai" };
            Some((base, key, provider_type.into()))
        }
        "pi" => {
            let base = cfg.get("baseUrl")?.as_str()?.to_string();
            if base.is_empty() {
                return None;
            }
            let key = cfg.get("apiKey").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let provider_type = match cfg.get("api").and_then(|v| v.as_str()).unwrap_or("") {
                "anthropic-messages" => "anthropic",
                _ => "openai",
            };
            Some((base, key, provider_type.into()))
        }
        _ => None,
    }
}

#[tauri::command]
pub(crate) fn provider_reorder(
    db: tauri::State<'_, Database>,
    app_type: String,
    ordered_ids: Vec<String>,
) -> Result<(), String> {
    db.reorder_providers(&app_type, &ordered_ids).map_err(String::from)
}

/// Toggle a provider's membership in its app's failover chain (the per-card
/// "join the chain" switch). Rows with the bit off are excluded when the
/// proxy builds the per-app queue at `proxy_start`.
#[tauri::command]
pub(crate) fn provider_set_failover_queue(
    db: tauri::State<'_, Database>,
    id: String,
    app_type: String,
    in_queue: bool,
) -> Result<(), String> {
    db.set_in_failover_queue(&id, &app_type, in_queue)
        .map_err(String::from)
}
