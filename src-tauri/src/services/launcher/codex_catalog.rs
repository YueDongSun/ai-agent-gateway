//! Codex app-server model metadata derived from configured gateway supply.

use crate::{database::Database, error::AppError};
use std::{collections::BTreeMap, path::Path};

pub(crate) const CATALOG_FILENAME: &str = "cc-switch-model-catalog.json";
const TOOL_OUTPUT_TOKEN_LIMIT: u64 = 10_000;
const DEFAULT_CODEX_INSTRUCTIONS: &str = "You are a helpful coding assistant. You and the user share one workspace. Collaborate until the requested work is complete. Use the available tools and verify your changes.";

#[derive(Clone, Default)]
struct CatalogModel {
    context_window: Option<u64>,
    supports_images: bool,
}

fn configured_models(
    db: &Database,
    selected_provider: Option<&crate::provider::Provider>,
) -> Result<BTreeMap<String, CatalogModel>, AppError> {
    let mut models = BTreeMap::<String, CatalogModel>::new();
    let (_, suppliers) = db.get_supplier_snapshot()?;
    if !suppliers.is_empty() {
        for offer in crate::services::model_catalog::offered_models(db)? {
            if offer.model_id.trim().is_empty() {
                continue;
            }
            let candidate = CatalogModel {
                context_window: offer.context_window,
                supports_images: offer.supports_images == Some(true),
            };
            models
                .entry(offer.model_id)
                .and_modify(|existing| {
                    existing.supports_images &= candidate.supports_images;
                    existing.context_window =
                        match (existing.context_window, candidate.context_window) {
                            (Some(left), Some(right)) => Some(left.min(right)),
                            _ => None,
                        };
                })
                .or_insert(candidate);
        }
        return Ok(models);
    }

    // A legacy-only installation remains supported. Disabled supplier rows
    // above are authoritative and never fall through to a stale legacy pool.
    let proxy = db.get_proxy_config()?;
    let providers: BTreeMap<_, _> = db
        .get_all_proxy_providers()?
        .into_iter()
        .filter(|provider| {
            provider.enabled
                && provider.provider_type != "anthropic_passthrough"
                && !crate::proxy::self_loop::base_url_targets_self(
                    &provider.base_url,
                    &proxy.host,
                    proxy.port,
                )
        })
        .map(|provider| (provider.id.clone(), provider))
        .collect();
    for provider in providers.values() {
        for model in &provider.models {
            if !model.trim().is_empty() {
                models.insert(model.clone(), CatalogModel::default());
            }
        }
    }
    let valid_hop = |provider_id: &str, model: &str| {
        // Legacy mappings are explicit model configuration even when that
        // provider has no separately fetched model inventory.
        !model.trim().is_empty() && providers.contains_key(provider_id)
    };
    let routes = db.get_all_routes()?;
    for route in &routes {
        if !route.alias.trim().is_empty()
            && route
                .hops
                .iter()
                .any(|hop| valid_hop(&hop.provider, &hop.model))
        {
            models.insert(route.alias.clone(), CatalogModel::default());
        } else {
            // Explicit invalid routes shadow an otherwise valid raw model.
            models.remove(&route.alias);
        }
    }
    let explicit_aliases: std::collections::BTreeSet<_> =
        routes.iter().map(|route| route.alias.as_str()).collect();
    let current_provider = if selected_provider.is_none() {
        db.get_current_provider("codex")?
            .map(|id| db.get_provider_by_id(&id, "codex"))
            .transpose()?
            .flatten()
    } else {
        None
    };
    if let Some(provider) = selected_provider.or(current_provider.as_ref()) {
        if let Some(entries) = provider
            .settings_config
            .pointer("/modelCatalog/models")
            .and_then(serde_json::Value::as_array)
        {
            for entry in entries {
                let display = entry
                    .get("display")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                let model = entry
                    .get("model")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(display);
                let upstream = entry
                    .get("provider")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                if !display.trim().is_empty()
                    && !explicit_aliases.contains(display)
                    && valid_hop(upstream, model)
                {
                    models.insert(display.to_string(), CatalogModel::default());
                }
            }
        }
    }
    for provider in providers.values() {
        for (display, model) in &provider.model_mapping {
            if !display.trim().is_empty()
                && !explicit_aliases.contains(display.as_str())
                && valid_hop(&provider.id, model)
            {
                models.entry(display.clone()).or_default();
            }
        }
    }
    Ok(models)
}

/// One selection rule for the persisted model and the desktop picker. The
/// selected row wins, then unified preference, then current legacy preference;
/// every candidate must survive the same runtime-filtered catalogue.
pub(crate) fn default_model(
    db: &Database,
    model_ids: &[String],
    selected_provider: Option<&crate::provider::Provider>,
) -> Result<String, AppError> {
    let selected = selected_provider
        .and_then(|provider| provider.settings_config.get("model"))
        .and_then(serde_json::Value::as_str);
    if let Some(model) = selected.filter(|model| model_ids.iter().any(|id| id.as_str() == *model)) {
        return Ok(model.to_string());
    }
    if db
        .get_setting(crate::services::mirror_models::KEY_ENABLED)?
        .as_deref()
        == Some("1")
    {
        let preferred = crate::services::mirror_models::DEFAULT_CODEX_ROLE;
        return Ok(model_ids
            .iter()
            .find(|id| id.as_str() == preferred)
            .or_else(|| model_ids.first())
            .cloned()
            .unwrap_or_default());
    }
    if let Some(model) = crate::services::supplier_setup::selected_model(db)? {
        if model_ids.contains(&model) {
            return Ok(model);
        }
    }
    if let Some(id) = db.get_current_provider("codex")? {
        if let Some(provider) = db.get_provider_by_id(&id, "codex")? {
            if let Some(model) = provider
                .settings_config
                .get("model")
                .and_then(serde_json::Value::as_str)
                .filter(|model| model_ids.iter().any(|id| id.as_str() == *model))
            {
                return Ok(model.to_string());
            }
        }
    }
    Ok(model_ids.first().cloned().unwrap_or_default())
}

/// Atomically replace the catalogue, including an explicit empty catalogue
/// after supply is removed. Return its exact wire ids for default selection.
/// Desktop's app-server reads this through model_catalog_json at startup;
/// a successful write alone does not establish a running client's model list.
pub(crate) fn write_codex_model_catalog(
    db: &Database,
    config_dir: &Path,
    selected_provider: Option<&crate::provider::Provider>,
    allowed_ids: Option<&[String]>,
) -> Result<Vec<String>, AppError> {
    let (ids, bytes) = build_codex_model_catalog(db, selected_provider, allowed_ids)?;
    write_prepared_codex_model_catalog(db, config_dir, &bytes)?;
    Ok(ids)
}

/// Persist an already validated catalogue, retaining the same authority and
/// backup boundary used by the normal catalogue writer.
pub(crate) fn write_prepared_codex_model_catalog(
    db: &Database,
    config_dir: &Path,
    bytes: &[u8],
) -> Result<(), AppError> {
    if crate::services::session_authority::client_config::managed_launch_required(db)
        .map_err(AppError::Config)?
    {
        return Err(AppError::SessionClientAuthorityUnavailable);
    }
    if crate::config::same_dir(config_dir, &crate::config::get_codex_config_dir()) {
        return Err(AppError::Config(
            "codex_mirror_native_directory_forbidden".into(),
        ));
    }
    crate::config::private_fs::prepare_managed_private_dir(config_dir)?;
    let path = config_dir.join(CATALOG_FILENAME);
    crate::services::provider::config_writes::write_files(&[
        crate::services::provider::config_writes::ConfigFileUpdate {
            path: &path,
            contents: bytes,
            app_type: "codex",
        },
    ])?;
    Ok(())
}

/// Public model metadata only. The caller owns its admitted destination and
/// write boundary; building the catalogue has no filesystem side effects.
pub(crate) fn build_codex_model_catalog(
    db: &Database,
    selected_provider: Option<&crate::provider::Provider>,
    allowed_ids: Option<&[String]>,
) -> Result<(Vec<String>, Vec<u8>), AppError> {
    let mirror_roles = db
        .get_setting(crate::services::mirror_models::KEY_ENABLED)?
        .as_deref()
        == Some("1");
    let mut models = if mirror_roles {
        let available = if let Some(allowed) = allowed_ids {
            allowed.to_vec()
        } else {
            crate::services::mirror_models::configured_role_models(
                db,
                crate::proxy::router::SURFACE_CODEX,
            )?
        };
        crate::services::mirror_models::CODEX_ROLE_MODELS
            .iter()
            .filter(|id| available.iter().any(|available| available == **id))
            .map(|id| ((*id).to_string(), CatalogModel::default()))
            .collect()
    } else {
        configured_models(db, selected_provider)?
    };
    if let Some(allowed_ids) = allowed_ids {
        // The async runtime owner supplies viability from its captured Codex
        // routing snapshot, including explicit routes, authentication and pin.
        // Empty is authoritative; it must not restore the unfiltered inventory.
        let allowed: std::collections::BTreeSet<_> =
            allowed_ids.iter().map(String::as_str).collect();
        models.retain(|id, _| allowed.contains(id.as_str()));
    }
    let ids: Vec<_> = models.keys().cloned().collect();
    let selected_default = default_model(db, &ids, selected_provider)?;
    let reasoning_catalog =
        super::codex_reasoning::Reasoning_catalog::from_db(db, selected_provider)?;
    let entries: Vec<_> = models
        .iter()
        .map(|(model_id, metadata)| {
            let reasoning = reasoning_catalog.for_model(model_id);
            let mut modalities = vec!["text"];
            if metadata.supports_images {
                modalities.push("image");
            }
            serde_json::json!({
                "slug": model_id,
                "display_name": if mirror_roles { format!("{model_id} (AAG routing)") } else { model_id.clone() },
                "description": if mirror_roles { "AAG routing role. The configured queue selects the serving upstream model and advances on retriable failures." } else { "Configured model available through AAG gateway." },
                "base_instructions": DEFAULT_CODEX_INSTRUCTIONS,
                "default_reasoning_level": reasoning.default,
                "supported_reasoning_levels": reasoning.wire_levels(),
                "shell_type": "shell_command",
                "visibility": "list",
                "supported_in_api": true,
                "priority": if model_id == &selected_default { 0 } else { 1 },
                "availability_nux": null,
                "upgrade": null,
                "model_messages": null,
                "include_skills_usage_instructions": false,
                "default_reasoning_summary": "none",
                "support_verbosity": false,
                "default_verbosity": null,
                // The translated gateway baseline accepts function tools.
                // Shell tools remain available for coding; this client has no
                // function-form apply_patch or flat collaboration substitute.
                "apply_patch_tool_type": null,
                "multi_agent_version": "disabled",
                "web_search_tool_type": "text",
                // This limits tool-output truncation, not the model's output limit.
                "truncation_policy": {"mode":"tokens", "limit":TOOL_OUTPUT_TOKEN_LIMIT},
                "context_window": metadata.context_window,
                "max_context_window": metadata.context_window,
                "effective_context_window_percent": 95,
                "experimental_supported_tools": [],
                "input_modalities": modalities,
                "supports_parallel_tool_calls": false,
                "supports_reasoning_summaries": false,
                "supports_reasoning_summary_parameter": false,
                "supports_search_tool": false,
                "prefer_websockets": false,
                "use_responses_lite": false
            })
        })
        .collect();
    let bytes = serde_json::to_vec_pretty(&serde_json::json!({"models":entries}))?;
    Ok((ids, bytes))
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::provider::test_support::isolated_home;
    use crate::services::suppliers::{save_supplier, SupplierSaveInput};
    use serde_json::{json, Value};

    #[test]
    fn explicit_role_selection_wins_while_unselected_roles_keep_the_existing_default() {
        let db = Database::memory().unwrap();
        db.set_setting(crate::services::mirror_models::KEY_ENABLED, "1")
            .unwrap();
        let ids = crate::services::mirror_models::CODEX_ROLE_MODELS
            .iter()
            .map(|id| (*id).to_string())
            .collect::<Vec<_>>();
        for model in ["gpt-5.6-terra", "gpt-5.6-luna"] {
            let provider = crate::provider::Provider::new(
                "codex".into(),
                "Codex".into(),
                json!({"model":model}),
            );
            assert_eq!(default_model(&db, &ids, Some(&provider)).unwrap(), model);
        }
        assert_eq!(default_model(&db, &ids, None).unwrap(), "gpt-5.6-sol");
    }

    fn read_catalog(directory: &std::path::Path) -> Value {
        serde_json::from_slice(
            &std::fs::read(directory.join("cc-switch-model-catalog.json")).unwrap(),
        )
        .unwrap()
    }

    fn save_fixture_supplier(db: &Database, enabled: bool) {
        let input: SupplierSaveInput = serde_json::from_value(json!({
            "expectedGeneration":db.supplier_config_generation().unwrap(),
            "name":"Catalog fixture", "baseUrl":"https://example.invalid",
            "apiKey":"fixture_upstream_secret", "enabled":enabled,
            "endpoints":[{"protocol":"openai-responses", "models":[
                {"modelId":"Vendor_Model.V1", "supportsImages":true,
                 "contextWindow":32768, "maxOutputTokens":4096},
                {"modelId":"disabled-model", "enabled":false}
            ]}]
        }))
        .unwrap();
        save_supplier(db, input).unwrap();
    }

    fn save_legacy_provider(db: &Database, enabled: bool) {
        let provider = serde_json::from_value(json!({
            "id":"legacy", "name":"Legacy", "type":"openai",
            "baseURL":"https://legacy.invalid/v1", "apiKey":"fixture_legacy_secret",
            "models":["legacy_model"], "enabled":enabled
        }))
        .unwrap();
        db.save_proxy_provider(&provider).unwrap();
    }

    #[test]
    fn empty_inventory_clears_stale_catalog_without_fabricated_models() {
        let home = isolated_home("codex-empty-catalog");
        let db = Database::memory().unwrap();
        let path = home.root.join("cc-switch-model-catalog.json");
        crate::config::private_fs::atomic_write(&path, br#"{"models":[{"slug":"stale-model"}]}"#)
            .unwrap();
        write_codex_model_catalog(&db, &home.root, None, None).unwrap();
        assert_eq!(read_catalog(&home.root), json!({"models":[]}));
    }

    #[test]
    fn unified_catalog_preserves_wire_ids_and_uses_supported_client_schema() {
        let home = isolated_home("codex-unified-catalog");
        let db = Database::memory().unwrap();
        save_fixture_supplier(&db, true);
        write_codex_model_catalog(&db, &home.root, None, None).unwrap();
        let catalog = read_catalog(&home.root);
        let models = catalog["models"].as_array().unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0]["slug"], "Vendor_Model.V1");
        assert_eq!(models[0]["truncation_policy"]["mode"], "tokens");
        assert_eq!(models[0]["context_window"], 32768);
        assert_eq!(models[0]["input_modalities"], json!(["text", "image"]));
        assert!(models[0]["apply_patch_tool_type"].is_null());
        assert_eq!(models[0]["multi_agent_version"], "disabled");
        // An unknown vendor model has no fabricated reasoning capability.
        assert_eq!(models[0]["supported_reasoning_levels"], json!([]));
        assert!(models[0]["default_reasoning_level"].is_null());
        assert!(!catalog.to_string().contains("fixture_upstream_secret"));
    }

    #[test]
    fn disabled_supplier_inventory_does_not_revive_legacy_supply() {
        let home = isolated_home("codex-disabled-catalog");
        let db = Database::memory().unwrap();
        save_fixture_supplier(&db, false);
        save_legacy_provider(&db, true);
        write_codex_model_catalog(&db, &home.root, None, None).unwrap();
        assert_eq!(read_catalog(&home.root)["models"], json!([]));
    }

    #[test]
    fn legacy_catalog_retains_only_enabled_real_models_and_valid_mapping_aliases() {
        let home = isolated_home("codex-legacy-catalog");
        let db = Database::memory().unwrap();
        save_legacy_provider(&db, true);
        let provider = crate::provider::Provider::new(
            "codex-fixture".into(),
            "Codex".into(),
            json!({
                "modelCatalog":{"models":[
                    {"display":"Display_Model", "model":"legacy_model", "provider":"legacy"},
                    {"display":"Missing_Model", "model":"absent", "provider":"missing"}
                ]}
            }),
        );
        db.save_provider("codex", &provider).unwrap();
        db.set_current_provider(&provider.id, "codex").unwrap();
        write_codex_model_catalog(&db, &home.root, None, None).unwrap();
        let catalog = read_catalog(&home.root);
        let ids: Vec<_> = catalog["models"]
            .as_array()
            .unwrap()
            .iter()
            .map(|model| model["slug"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["Display_Model", "legacy_model"]);
        save_legacy_provider(&db, false);
        write_codex_model_catalog(&db, &home.root, None, None).unwrap();
        assert_eq!(read_catalog(&home.root)["models"], json!([]));
    }

    #[test]
    fn legacy_mapping_only_provider_keeps_its_explicit_model_alias() {
        let home = isolated_home("codex-mapping-only-catalog");
        let db = Database::memory().unwrap();
        let provider = serde_json::from_value(json!({
            "id":"legacy", "name":"Legacy", "type":"openai",
            "baseURL":"https://legacy.invalid/v1", "apiKey":"fixture",
            "modelMapping":{"Legacy_Alias":"upstream_model"}
        }))
        .unwrap();
        db.save_proxy_provider(&provider).unwrap();
        let ids = write_codex_model_catalog(&db, &home.root, None, None).unwrap();
        assert_eq!(ids, ["Legacy_Alias"]);
    }

    #[test]
    fn picker_priority_matches_configured_default_after_runtime_filtering() {
        let home = isolated_home("codex-picker-default");
        let db = Database::memory().unwrap();
        let input: SupplierSaveInput = serde_json::from_value(json!({
            "expectedGeneration":0, "name":"Picker fixture", "baseUrl":"https://example.invalid",
            "apiKey":"fixture", "endpoints":[{"protocol":"openai-responses", "models":[
                {"modelId":"A_Alphabetic"}, {"modelId":"Z_Configured"}
            ]}]
        }))
        .unwrap();
        save_supplier(&db, input).unwrap();
        db.set_setting(
            crate::services::supplier_setup::KEY_UNIFIED_DEFAULT_MODEL,
            "Z_Configured",
        )
        .unwrap();
        let ids = write_codex_model_catalog(&db, &home.root, None, None).unwrap();
        let catalog = read_catalog(&home.root);
        let models = catalog["models"].as_array().unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0]["slug"], "A_Alphabetic");
        assert_eq!(models[0]["priority"], 1);
        assert_eq!(models[1]["priority"], 0);
        assert_eq!(default_model(&db, &ids, None).unwrap(), "Z_Configured");

        let allowed = vec!["A_Alphabetic".to_string()];
        let ids = write_codex_model_catalog(&db, &home.root, None, Some(&allowed)).unwrap();
        let catalog = read_catalog(&home.root);
        assert_eq!(catalog["models"].as_array().unwrap().len(), 1);
        assert_eq!(catalog["models"][0]["priority"], 0);
        assert_eq!(default_model(&db, &ids, None).unwrap(), "A_Alphabetic");
    }

    #[test]
    fn mirror_role_catalog_exposes_only_admitted_roles_and_marks_routing() {
        let home = isolated_home("codex-role-catalog");
        let db = Database::memory().unwrap();
        save_fixture_supplier(&db, true);
        db.set_setting(crate::services::mirror_models::KEY_ENABLED, "1")
            .unwrap();
        let allowed = vec![
            "gpt-5.6-sol".to_string(),
            "gpt-5.6-luna".to_string(),
            "Vendor_Model.V1".to_string(),
        ];
        let ids = write_codex_model_catalog(&db, &home.root, None, Some(&allowed)).unwrap();
        assert_eq!(ids, ["gpt-5.6-luna", "gpt-5.6-sol"]);
        let catalog = read_catalog(&home.root);
        assert!(catalog["models"].as_array().unwrap().iter().all(|entry| {
            entry["display_name"]
                .as_str()
                .unwrap()
                .contains("AAG routing")
        }));
        assert_eq!(default_model(&db, &ids, None).unwrap(), "gpt-5.6-sol");
        write_codex_model_catalog(&db, &home.root, None, Some(&[])).unwrap();
        assert_eq!(read_catalog(&home.root)["models"], json!([]));
    }
}
