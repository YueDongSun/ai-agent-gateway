//! Provider CRUD commands + the failover-queue endpoint extractor.
//!
//! Live configuration switching is owned by the provider service so that
//! filesystem failures cannot mark an unapplied provider as current.

use crate::database::Database;
use crate::provider::Provider;

// ----- Provider commands (Phase 1) -----

#[tauri::command]
pub(crate) fn provider_list(
    db: tauri::State<'_, Database>,
    app_type: String,
) -> Result<Vec<Provider>, String> {
    db.get_all_providers(&app_type)
        .map(|m| m.into_iter().map(|(_, v)| v).collect())
        .map_err(String::from)
}
#[tauri::command]
pub(crate) fn provider_current(
    db: tauri::State<'_, Database>,
    app_type: String,
) -> Result<Option<String>, String> {
    db.get_current_provider(&app_type).map_err(String::from)
}
#[tauri::command]
pub(crate) fn provider_get(
    db: tauri::State<'_, Database>,
    id: String,
    app_type: String,
) -> Result<Option<Provider>, String> {
    db.get_provider_by_id(&id, &app_type).map_err(String::from)
}
#[tauri::command]
pub(crate) fn provider_save(
    db: tauri::State<'_, Database>,
    app_type: String,
    provider: Provider,
) -> Result<(), String> {
    // upsert (not save_provider): UI edits to an existing row must persist;
    // save_provider stays insert-only for importer idempotency.
    db.upsert_provider(&app_type, &provider)
        .map_err(String::from)
}
#[tauri::command]
pub(crate) fn provider_delete(
    db: tauri::State<'_, Database>,
    id: String,
    app_type: String,
) -> Result<(), String> {
    db.delete_provider(&id, &app_type).map_err(String::from)
}
#[tauri::command]
/// Apply the app's live configuration, then mark the provider as current.
/// Claude and Codex use their isolated AAG mirror homes; other supported
/// clients resolve their platform-specific configuration paths in the service.
pub(crate) fn provider_switch(
    db: tauri::State<'_, Database>,
    proxy: tauri::State<'_, tokio::sync::Mutex<Option<crate::commands::proxy::RunningProxy>>>,
    id: String,
    app_type: String,
) -> Result<crate::services::provider::LiveWriteResult, String> {
    let live = crate::services::provider::switching::switch_provider(&db, &id, &app_type)
        .map_err(String::from)?;

    // The running proxy routes from an immutable proxy_start snapshot: a
    // switch never rewrites its failover queue (queue order is the
    // configured order), but the active provider's model-catalog auto-routes
    // only rebuild on the next proxy_start. Surface that boundary so a
    // switch while running isn't silently half-applied.
    if let Ok(running) = proxy.try_lock() {
        if running.is_some() {
            log::info!(
                "provider_switch: proxy running; model-catalog routes rebuild on next proxy_start app_type={} provider_ref={}",
                app_type,
                crate::diagnostics::safe_reference(&id)
            );
        }
    }

    Ok(live)
}

pub(crate) fn queue_endpoint_of(
    app_type: &str,
    cfg: &serde_json::Value,
) -> Option<(String, String, String)> {
    let env = cfg.get("env").cloned().unwrap_or(serde_json::json!({}));
    match app_type {
        // claude_vscode rows carry the same env shape the launcher injects
        // (ANTHROPIC_BASE_URL / ANTHROPIC_AUTH_TOKEN).
        "claude_code" | "claude_vscode" => {
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
            if let Some(base) = cfg
                .get("baseUrl")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                let key = cfg
                    .get("apiKey")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
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
        "gemini" => {
            // Gemini providers carry the cc-switch-compatible env block:
            // GOOGLE_GEMINI_BASE_URL (upstream base) + GEMINI_API_KEY. The
            // optional `api` field selects the upstream wire — "gemini"
            // (native Generative Language API, default), "anthropic", or
            // "openai" — so a Gemini CLI chain can fail over across wires.
            let env = cfg.get("env").cloned().unwrap_or(serde_json::json!({}));
            let base = env.get("GOOGLE_GEMINI_BASE_URL")?.as_str()?.to_string();
            if base.is_empty() {
                return None;
            }
            let key = env
                .get("GEMINI_API_KEY")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let provider_type = match cfg.get("api").and_then(|v| v.as_str()).unwrap_or("") {
                "anthropic" => "anthropic",
                "openai" => "openai",
                _ => "gemini",
            };
            Some((base, key, provider_type.into()))
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
            let provider_type = if wire == "anthropic" {
                "anthropic"
            } else {
                "openai"
            };
            Some((base, key, provider_type.into()))
        }
        "pi" => {
            let base = cfg.get("baseUrl")?.as_str()?.to_string();
            if base.is_empty() {
                return None;
            }
            let key = cfg
                .get("apiKey")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let provider_type = match cfg.get("api").and_then(|v| v.as_str()).unwrap_or("") {
                "anthropic-messages" => "anthropic",
                _ => "openai",
            };
            Some((base, key, provider_type.into()))
        }
        "opencode" | "kilo_cli" | "kilo_vscode" => {
            // Shared OpenCode-compatible projection: { options: { baseURL,
            // apiKey }, models: { ... } }. Anthropic providers are rare in
            // this surface; default to openai unless the npm package hints
            // otherwise.
            let options = cfg.get("options").and_then(|v| v.as_object())?;
            let base = options.get("baseURL")?.as_str()?.to_string();
            if base.is_empty() {
                return None;
            }
            let key = options
                .get("apiKey")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let provider_type = match cfg.get("npm").and_then(|v| v.as_str()).unwrap_or("") {
                npm if npm.contains("anthropic") => "anthropic",
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
    db.reorder_providers(&app_type, &ordered_ids)
        .map_err(String::from)
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
