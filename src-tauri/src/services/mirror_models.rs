//! Desktop role catalogues backed by the frozen capability planner.
//! Client role names request a tier; evidence ranks the actual upstreams.

use crate::database::dao::proxy_providers::ProxyProvider;
use crate::database::dao::routes::RouteHop;
use crate::database::Database;
use crate::error::AppError;
use crate::proxy::planner::{Model_planner, Resolution};
use crate::proxy::providers::Provider;
use crate::proxy::router::{
    Mirror_role_routes, SURFACE_CHAT, SURFACE_CLAUDE, SURFACE_CODEX, SURFACE_GEMINI,
};
use std::collections::HashMap;
use std::sync::Arc;

pub(crate) const KEY_ENABLED: &str = "mirror_model_roles_enabled";
pub(crate) const CLAUDE_ROLE_MODELS: [&str; 4] = [
    "claude-fable-5",
    "claude-opus-5",
    "claude-sonnet-5",
    "claude-haiku-5-5",
];
pub(crate) const CODEX_ROLE_MODELS: [&str; 3] = ["gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna"];
pub(crate) const DEFAULT_CODEX_ROLE: &str = CODEX_ROLE_MODELS[0];
pub(crate) const DEFAULT_CLAUDE_ROLE: &str = CLAUDE_ROLE_MODELS[0];

fn planned_role_hops(planner: &dyn Model_planner, selector: &str) -> Vec<RouteHop> {
    match planner.resolve(selector, Default::default()) {
        Ok(Resolution::Alias { hops, .. }) | Ok(Resolution::Precise { hops, .. }) => hops
            .into_iter()
            .map(|hop| RouteHop {
                provider: hop.provider,
                model: hop.model,
            })
            .collect(),
        // Unknown/unranked supply must never turn into a selected-model queue.
        _ => Vec::new(),
    }
}

/// Catalog projections are frozen with the same planner as request dispatch.
/// Request-time resolution still repeats hard capability filtering using the
/// actual request; these default-feature chains are for discovery only.
pub(crate) fn build_mirror_role_routes(
    db: &Database,
    providers: &HashMap<String, Arc<dyn Provider>>,
    planner: &dyn Model_planner,
) -> Result<(bool, Mirror_role_routes), AppError> {
    let enabled = db.get_setting(KEY_ENABLED)?.as_deref() == Some("1");
    if !enabled {
        return Ok((false, HashMap::new()));
    }
    let roles: HashMap<String, Vec<RouteHop>> = crate::proxy::planner::CLIENT_ROLE_SELECTORS
        .iter()
        .map(|(selector, _)| {
            let hops = planned_role_hops(planner, selector)
                .into_iter()
                .filter(|hop| {
                    providers.get(&hop.provider).is_some_and(|provider| {
                        provider.enabled()
                            && !provider.requires_client_credential()
                            && provider.models().contains(&hop.model)
                    })
                })
                .collect();
            ((*selector).into(), hops)
        })
        .collect();
    Ok((
        true,
        [SURFACE_CLAUDE, SURFACE_CODEX, SURFACE_CHAT, SURFACE_GEMINI]
            .into_iter()
            .map(|surface| (surface.into(), roles.clone()))
            .collect(),
    ))
}

fn static_row(row: &ProxyProvider, host: &str, port: u16) -> bool {
    row.enabled
        && row.provider_type != "anthropic_passthrough"
        && !crate::proxy::self_loop::base_url_targets_self(&row.base_url, host, port)
}

/// Configuration-only projection used before a runtime exists. Its ranking
/// and hard capability rules come from the same planner; no factory, network,
/// implicit tier assignment, or selected-model fallback runs here.
pub(crate) fn configured_role_models(
    db: &Database,
    surface: &str,
) -> Result<Vec<String>, AppError> {
    if db.get_setting(KEY_ENABLED)?.as_deref() != Some("1") {
        return Ok(Vec::new());
    }
    let rows = db.get_all_proxy_providers()?;
    let planner = super::proxy_runtime::planner::build_model_planner(db, &rows)?;
    let config = db.get_proxy_config()?;
    let pin = db.get_setting(crate::database::dao::settings::KEY_PROXY_PINNED_PROVIDER)?;
    let pin = pin.as_deref().filter(|pin| !pin.is_empty());
    let rows_by_id: HashMap<_, _> = rows.iter().map(|row| (row.id.as_str(), row)).collect();
    let routes = db.get_all_routes()?;
    let selectors: &[&str] = match surface {
        SURFACE_CLAUDE => &CLAUDE_ROLE_MODELS,
        SURFACE_CODEX => &CODEX_ROLE_MODELS,
        _ => return Ok(Vec::new()),
    };
    Ok(selectors
        .iter()
        .filter(|selector| {
            let planned = planned_role_hops(planner.as_ref(), selector);
            let manual = routes.iter().find(|route| route.alias == **selector);
            planned.iter().any(|hop| {
                pin.is_none_or(|pin| pin == hop.provider)
                    && manual.is_none_or(|route| {
                        route.hops.iter().any(|candidate| {
                            candidate.provider == hop.provider && candidate.model == hop.model
                        })
                    })
                    && rows_by_id.get(hop.provider.as_str()).is_some_and(|row| {
                        static_row(row, &config.host, config.port)
                            && row.models.contains(&hop.model)
                    })
            })
        })
        .map(|selector| (*selector).to_string())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::suppliers::{save_supplier, SupplierSaveInput};
    use serde_json::json;

    #[test]
    fn configured_roles_follow_assessed_tiers_and_pins_without_selected_model_fallback() {
        let db = Database::memory().unwrap();
        let input: SupplierSaveInput = serde_json::from_value(json!({
            "expectedGeneration":0,"name":"ranked fixture","baseUrl":"https://example.invalid","apiKey":"fixture",
            "endpoints":[{"protocol":"openai-chat","models":[
                {"modelId":"unknown-model"},
                {"modelId":"strong-model","tierOverride":3},
                {"modelId":"balanced-model","tierOverride":1},
                {"modelId":"fast-model","tierOverride":0}
            ]}]
        })).unwrap();
        save_supplier(&db, input).unwrap();
        db.set_setting(
            super::super::supplier_setup::KEY_UNIFIED_DEFAULT_MODEL,
            "unknown-model",
        )
        .unwrap();
        assert!(configured_role_models(&db, SURFACE_CODEX)
            .unwrap()
            .is_empty());
        db.set_setting(KEY_ENABLED, "1").unwrap();
        assert_eq!(
            configured_role_models(&db, SURFACE_CODEX).unwrap(),
            CODEX_ROLE_MODELS
        );
        db.set_setting(
            crate::database::dao::settings::KEY_PROXY_PINNED_PROVIDER,
            "missing",
        )
        .unwrap();
        assert!(configured_role_models(&db, SURFACE_CODEX)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn unranked_configured_supply_does_not_advertise_any_role() {
        let db = Database::memory().unwrap();
        let input: SupplierSaveInput = serde_json::from_value(json!({
            "expectedGeneration":0,"name":"unknown fixture","baseUrl":"https://example.invalid","apiKey":"fixture",
            "endpoints":[{"protocol":"openai-chat","models":[{"modelId":"mystery-model"}]}]
        })).unwrap();
        save_supplier(&db, input).unwrap();
        db.set_setting(KEY_ENABLED, "1").unwrap();
        assert!(configured_role_models(&db, SURFACE_CODEX)
            .unwrap()
            .is_empty());
        assert!(configured_role_models(&db, SURFACE_CLAUDE)
            .unwrap()
            .is_empty());
    }
}
