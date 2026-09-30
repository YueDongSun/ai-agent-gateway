use crate::database::Database;

/// Generate `cc-switch-model-catalog.json` from the active Codex provider's
/// `modelCatalog` (user-configured display→upstream mappings), plus fallback
/// sources (proxy_providers, routes, OFFICIAL_MODELS).
///
/// The active Codex provider's `settings_config.modelCatalog.models` has the
/// shape `[{ "display": "GPT-5", "model": "deepseek-v4-flash", "provider": "deepseek" }]`:
/// - `display` = what Codex Desktop shows in the dropdown AND sends in the
///   API request (becomes the catalog entry's `id` and `display_name`).
/// - `model` = the actual upstream model (used by the proxy route hop).
/// - `provider` = the proxy_provider id that serves this model.
///
/// Entries from the provider's modelCatalog take priority. Fallback sources
/// (proxy_providers.models, routes.alias, OFFICIAL_MODELS) fill in any gaps.
pub(super) fn write_codex_model_catalog(
    db: &Database,
    config_dir: &std::path::Path,
) -> Result<(), String> {
    use crate::proxy::models::OFFICIAL_MODELS;
    use std::collections::{BTreeMap, BTreeSet};

    // --- Collect models from the active Codex provider's modelCatalog ---
    // Key: display name (what Codex sends), Value: (upstream model, context, desc).
    let mut catalog: BTreeMap<String, (String, u64, String)> = BTreeMap::new();

    if let Ok(Some(cur_id)) = db.get_current_provider("codex") {
        if let Ok(Some(prov)) = db.get_provider_by_id(&cur_id, "codex") {
            if let Some(mc) = prov.settings_config.get("modelCatalog") {
                if let Some(models) = mc.get("models").and_then(|v| v.as_array()) {
                    for entry in models {
                        let display = entry.get("display").and_then(|v| v.as_str()).unwrap_or("");
                        let model = entry
                            .get("model")
                            .and_then(|v| v.as_str())
                            .unwrap_or(display);
                        if display.is_empty() {
                            continue;
                        }
                        let (ctx, _max, desc) = model_meta(model);
                        catalog.insert(
                            display.to_string(),
                            (model.to_string(), ctx, desc.to_string()),
                        );
                    }
                }
            }
        }
    }

    // --- Fallback: proxy_providers, routes, OFFICIAL_MODELS ---
    let mut fallback_ids: BTreeSet<String> = BTreeSet::new();
    if let Ok(provs) = db.get_all_proxy_providers() {
        for p in &provs {
            for m in &p.models {
                fallback_ids.insert(m.clone());
            }
        }
    }
    if let Ok(routes) = db.get_all_routes() {
        for r in &routes {
            fallback_ids.insert(r.alias.clone());
        }
    }
    for m in OFFICIAL_MODELS {
        fallback_ids.insert(m.id.to_string());
    }

    for model_id in &fallback_ids {
        catalog.entry(model_id.clone()).or_insert_with(|| {
            let (ctx, _max, desc) = model_meta(model_id);
            (model_id.clone(), ctx, desc.to_string())
        });
    }

    if catalog.is_empty() {
        return Ok(());
    }

    let base_instructions = read_mirror_base_instructions(config_dir)
        .unwrap_or_else(|| DEFAULT_CODEX_INSTRUCTIONS.to_string());

    let mut entries: Vec<serde_json::Value> = Vec::new();
    for (display, (upstream_model, _ctx_hint, description)) in &catalog {
        let (ctx, max_out, _) = model_meta(upstream_model);
        entries.push(serde_json::json!({
            "apply_patch_tool_type": "freeform",
            "auto_compact_token_limit": null,
            "auto_review_model_override": null,
            "availability_nux": null,
            "base_instructions": base_instructions,
            "comp_hash": "3000",
            "context_window": ctx,
            "default_reasoning_level": "high",
            "default_reasoning_summary": "none",
            "default_service_tier": null,
            "default_verbosity": "low",
            "description": description,
            "display_name": display,
            "effective_context_window_percent": 95,
            "experimental_supported_tools": [],
            "include_skills_usage_instructions": false,
            "input_modalities": ["text"],
            "max_context_window": ctx,
            "minimal_client_version": "0.144.0",
            "model_messages": { "approvals": null, "instructions_template": null, "instructions_variables": null },
            "multi_agent_version": "v2",
            "prefer_websockets": false,
            "priority": 1,
            "reasoning_summary_format": "experimental",
            "shell_type": "shell_command",
            "slug": display.to_lowercase().replace('_', "-"),
            "support_verbosity": true,
            "supported_in_api": true,
            "supported_reasoning_levels": [
                { "description": "Fast responses with lighter reasoning", "effort": "low" },
                { "description": "Balanced reasoning depth", "effort": "medium" },
                { "description": "Maximum reasoning depth for the hardest problems", "effort": "max" }
            ],
            "supports_image_detail_original": false,
            "supports_parallel_tool_calls": true,
            "supports_reasoning_summaries": true,
            "supports_search_tool": true,
            "tool_mode": null,
            "truncation_policy": { "mode": "rolling", "limit": max_out },
            "upgrade": null,
            "use_responses_lite": false,
            "visibility": "list",
            "web_search_tool_type": "text",
        }));
    }

    let catalog_json = serde_json::json!({ "models": entries });
    let catalog_path = config_dir.join("cc-switch-model-catalog.json");
    let json = serde_json::to_string_pretty(&catalog_json)
        .map_err(|_| "codex_model_catalog_serialize_failed".to_string())?;
    crate::config::private_fs::atomic_write(&catalog_path, json.as_bytes())
        .map_err(String::from)?;
    Ok(())
}

/// Heuristic model metadata based on the model id prefix.
fn model_meta(id: &str) -> (u64, u64, &'static str) {
    let lower = id.to_lowercase();
    if lower.starts_with("claude-") {
        if lower.contains("opus") || lower.contains("fable") {
            (
                200_000,
                32_000,
                "High-capacity Anthropic model for complex agentic tasks.",
            )
        } else if lower.contains("haiku") {
            (200_000, 8_192, "Fast, lightweight Anthropic model.")
        } else {
            (
                200_000,
                16_000,
                "Balanced Anthropic model for coding and reasoning.",
            )
        }
    } else if lower.starts_with("gpt-") || lower.starts_with("o3") || lower.starts_with("o4") {
        (
            128_000,
            16_384,
            "OpenAI model for general-purpose coding and reasoning.",
        )
    } else if lower.starts_with("deepseek") {
        (
            1_000_000,
            16_384,
            "DeepSeek model with large context window.",
        )
    } else {
        (128_000, 8_192, "Model available through AAG gateway.")
    }
}

/// Try to read the `base_instructions` from the first model of the mirror's
/// OWN `cc-switch-model-catalog.json` (written by an earlier run of
/// `write_codex_model_catalog`). Mirror-scoped only — the native `~/.codex`
/// is never consulted. Returns None if the file doesn't exist or is
/// malformed (first mirror launch falls back to `DEFAULT_CODEX_INSTRUCTIONS`).
fn read_mirror_base_instructions(config_dir: &std::path::Path) -> Option<String> {
    let catalog = config_dir.join("cc-switch-model-catalog.json");
    if !catalog.exists() {
        return None;
    }
    let content = std::fs::read_to_string(&catalog).ok()?;
    let v: serde_json::Value = serde_json::from_str(&content).ok()?;
    v.get("models")?
        .as_array()?
        .first()?
        .get("base_instructions")?
        .as_str()
        .map(String::from)
}

/// Fallback instructions when the mirror catalog is unavailable.
const DEFAULT_CODEX_INSTRUCTIONS: &str = "You are a helpful coding assistant. You and the user share one workspace. Collaborate with them until their goal is genuinely handled. Be concise and direct. Use the tools available to you to read, write, and execute code. When providing code, use the apply_patch tool with the freeform patch format.";
