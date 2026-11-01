//! Derive AAG rows from imported cc-switch data: upstream pool rows
//! (`ccs-<id>` proxy providers) and failover route chains. Pure policy —
//! every function maps parsed cc-switch inputs onto AAG row shapes; the
//! only side effects are the database writes inside `upsert_route_merge`
//! and `build_import_routes`, which the import orchestrator calls after
//! classification.

use super::cc_switch_db::{map_cc_switch_icon, map_cc_switch_icon_color};
use super::settings_extract::{
    codex_catalog_models, extract_extra_headers_from_settings, settings_env,
};
use super::{ImportedProvider, ImportedRoute};
use crate::database::dao::proxy_providers::{CircuitBreakerConfig, ProxyProvider};
use crate::database::dao::routes::RouteHop;
use crate::database::Database;
use std::collections::HashMap;

/// Breaker preset for imported upstreams. Imported providers see a mix of
/// upstream health (many return HTTP 4xx for specific model ids that
/// aren't in their plan). Tighten the breaker to 3 failures / 15 s
/// cooldown so a consistently-broken upstream opens quickly and is
/// skipped on subsequent requests — this bounds the streaming failover
/// walk cost to ~5 s once the bad hops are open.
fn imported_breaker() -> CircuitBreakerConfig {
    CircuitBreakerConfig {
        failure_threshold: 3,
        cooldown_ms: 15_000,
        half_open_retries: 1,
        success_threshold: 1,
    }
}

/// Resolve a provider's per-tier upstream model from its env block,
/// applying the any-tier-substitutes fallback. The `[1M]` context marker
/// is stripped — upstream Anthropic-compatible endpoints reject it as
/// not-a-real-model and either 404 or return an empty SSE body. The
/// 1M-context capability is preserved through a different mechanism
/// (the `anthropic-beta: context-1m-2025-08-07` header) injected by the
/// Anthropic provider, not the model id.
pub(crate) fn tier_models(env: &serde_json::Map<String, serde_json::Value>) -> [Option<String>; 4] {
    let pick = |keys: &[&str]| -> Option<String> {
        for k in keys {
            if let Some(s) = env.get(*k).and_then(|v| v.as_str()) {
                if !s.is_empty() {
                    return Some(crate::proxy::strip_one_m_suffix(s));
                }
            }
        }
        None
    };
    let default_model = pick(&["ANTHROPIC_MODEL", "ANTHROPIC_DEFAULT_MODEL"]);
    let haiku = pick(&[
        "ANTHROPIC_DEFAULT_HAIKU_MODEL",
        "ANTHROPIC_SMALL_FAST_MODEL",
    ]);
    let sonnet = pick(&["ANTHROPIC_DEFAULT_SONNET_MODEL"]);
    let opus = pick(&["ANTHROPIC_DEFAULT_OPUS_MODEL"]);
    let fable = pick(&["ANTHROPIC_DEFAULT_FABLE_MODEL"]);
    // Any available model can substitute for a missing tier.
    let fallback = fable
        .clone()
        .or_else(|| opus.clone())
        .or_else(|| sonnet.clone())
        .or_else(|| haiku.clone())
        .or_else(|| default_model.clone());
    [
        haiku.or_else(|| fallback.clone()),
        sonnet.or_else(|| fallback.clone()),
        opus.or_else(|| fallback.clone()),
        fable.or_else(|| fallback.clone()),
    ]
}

/// The pool-row filler: models + model_mapping derived from the row's
/// own tier env keys so the imported Claude upstream is usable out of
/// the box.
fn fill_claude_pool(
    env: &serde_json::Map<String, serde_json::Value>,
) -> (Vec<String>, HashMap<String, String>) {
    let tiers = tier_models(env);
    let mut models: Vec<String> = Vec::new();
    for t in tiers.iter().flatten() {
        if !models.iter().any(|m| m == t) {
            models.push(t.clone());
        }
    }
    let mut model_mapping: HashMap<String, String> = HashMap::new();
    for (alias, tier_idx) in [
        ("claude-haiku-4-5", 0usize),
        ("claude-sonnet-5", 1),
        ("claude-sonnet-4-6", 1),
        ("claude-opus-4-8", 2),
        ("claude-opus-4-7", 2),
        ("claude-opus-4-6", 2),
        ("claude-fable-5", 3),
    ] {
        if let Some(model) = &tiers[tier_idx] {
            model_mapping.insert(alias.to_string(), model.clone());
        }
    }
    (models, model_mapping)
}

/// Evenly distribute a Codex provider's upstream catalog models across
/// the default Codex client models: client[i] → upstream[i*N/4]. Shared
/// by the route builder and the pool-row model_mapping filler so both
/// always agree on the assignment. An empty catalog yields the plain
/// client alias list with no models.
pub(crate) fn codex_catalog_assignments(upstream_models: &[String]) -> Vec<(&'static str, String)> {
    let client_ids: Vec<&'static str> = crate::proxy::models::CODEX_MODELS
        .iter()
        .map(|m| m.id)
        .collect();
    if upstream_models.is_empty() {
        return client_ids
            .into_iter()
            .map(|alias| (alias, String::new()))
            .collect();
    }
    let n = upstream_models.len();
    let client_count = client_ids.len();
    client_ids
        .into_iter()
        .enumerate()
        .map(|(i, alias)| (alias, upstream_models[(i * n) / client_count].clone()))
        .collect()
}

/// Build the `ccs-<id>` upstream pool row for one imported provider.
/// Returns None when the row has no usable upstream coordinates (the
/// caller then simply creates no pool row). `is_subscription` selects
/// the `anthropic_passthrough` shape (no key — the client credential
/// rides the request; models = the official aliases it can serve
/// verbatim).
pub(crate) fn pool_row_for_import(
    pp_id: &str,
    row: &super::cc_switch_db::CcSwitchRow,
    settings_value: &serde_json::Value,
    upstream: Option<&(String, String, String)>,
    is_subscription: bool,
) -> Option<ProxyProvider> {
    if is_subscription {
        let pp = ProxyProvider {
            id: pp_id.to_string(),
            name: row.name.clone(),
            provider_type: "anthropic_passthrough".into(),
            api_key: String::new(),
            base_url: upstream
                .map(|(base, _, _)| base.clone())
                .unwrap_or_else(|| "https://api.anthropic.com".into()),
            models: crate::proxy::models::OFFICIAL_MODELS
                .iter()
                .filter(|m| m.id.starts_with("claude-"))
                .map(|m| m.id.to_string())
                .collect(),
            enabled: true,
            timeout_ms: 60_000,
            weight: 1,
            circuit_breaker: imported_breaker(),
            model_mapping: Default::default(),
            extra_headers: Default::default(),
            auto_track_models: true,
            icon: map_cc_switch_icon(row.icon.as_deref()),
            icon_color: map_cc_switch_icon_color(row.icon_color.as_deref()),
        };
        return Some(pp);
    }
    let (base_url, api_key, provider_type) = upstream?;
    // Only `models` and `model_mapping` differ per app_type; everything
    // else about an imported pool row is fixed.
    let (models, model_mapping) = match row.app_type.as_str() {
        "gemini" => {
            // The pool row is created even without a GEMINI_MODEL (empty
            // models list) — the user can fill the model in the pool UI;
            // only the route seeding needs a concrete model.
            let gemini_model = settings_env(settings_value)
                .get("GEMINI_MODEL")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            if gemini_model.is_empty() {
                (Vec::new(), HashMap::new())
            } else {
                (vec![gemini_model.to_string()], HashMap::new())
            }
        }
        "claude_code" => fill_claude_pool(settings_env(settings_value)),
        "codex" => {
            let catalog_models = codex_catalog_models(settings_value);
            if catalog_models.is_empty() {
                (Vec::new(), HashMap::new())
            } else {
                let model_mapping = codex_catalog_assignments(&catalog_models)
                    .into_iter()
                    .map(|(alias, model)| (alias.to_string(), model))
                    .collect();
                (catalog_models, model_mapping)
            }
        }
        // Other app_types import coordinates only; models and mappings
        // can be filled in the pool UI.
        _ => (Vec::new(), HashMap::new()),
    };
    let extra_headers =
        extract_extra_headers_from_settings(&row.app_type, settings_value, base_url);
    Some(ProxyProvider {
        id: pp_id.to_string(),
        name: row.name.clone(),
        provider_type: provider_type.clone(),
        api_key: api_key.clone(),
        base_url: base_url.clone(),
        models,
        enabled: true,
        timeout_ms: 60_000,
        weight: 1,
        circuit_breaker: imported_breaker(),
        model_mapping,
        extra_headers,
        auto_track_models: true,
        icon: map_cc_switch_icon(row.icon.as_deref()),
        icon_color: map_cc_switch_icon_color(row.icon_color.as_deref()),
    })
}

/// Upsert a route: create it with `new_hops` if absent; otherwise prepend
/// any providers from `new_hops` not already in the chain (preserving
/// user-tuned hop order). Failures propagate — a route write the import
/// cannot perform must be reported, not swallowed.
pub(crate) fn upsert_route_merge(
    db: &Database,
    alias: &str,
    description: String,
    new_hops: Vec<RouteHop>,
) -> Result<(), crate::error::AppError> {
    match db.get_route(alias)? {
        Some(existing) => {
            let existing_ids: std::collections::HashSet<String> =
                existing.hops.iter().map(|h| h.provider.clone()).collect();
            let mut hops: Vec<RouteHop> = Vec::new();
            for h in &new_hops {
                if existing_ids.contains(&h.provider) {
                    continue;
                }
                hops.push(h.clone());
            }
            hops.extend(existing.hops);
            db.save_route(&crate::database::dao::routes::Route {
                alias: alias.to_string(),
                // Preserve any user-tuned description on merge.
                description: existing.description.clone(),
                hops,
            })?;
        }
        None => {
            db.save_route(&crate::database::dao::routes::Route {
                alias: alias.to_string(),
                description,
                hops: new_hops,
            })?;
        }
    }
    Ok(())
}

/// Build every failover route implied by the import. Inputs are the
/// collected per-app_type provider summaries (in cc-switch queue order —
/// current first, then sort_index — so the chain head matches what
/// cc-switch had active); the returned route feedback feeds the import
/// report. Route write failures propagate with the alias they failed on.
pub(crate) fn build_import_routes(
    db: &Database,
    imported_claude: &[(String, [Option<String>; 4], bool)],
    imported_codex: &[(String, Vec<String>)],
    imported_gemini: &[(String, String, HashMap<String, String>)],
    passthrough_providers: &[ImportedProvider],
) -> Result<Vec<ImportedRoute>, crate::error::AppError> {
    let mut imported_routes: Vec<ImportedRoute> = Vec::new();

    // Auto-create failover routes for official Claude models using each
    // newly imported provider's `ANTHROPIC_DEFAULT_*_MODEL` env keys as
    // the per-hop model. cc-switch stores per-tier mapping in the JSON
    // `env` block of each provider's `settings_config`; the importer
    // round-trips that into AAG's route format:
    //
    //   claude-fable-5      -> env.ANTHROPIC_DEFAULT_FABLE_MODEL
    //   claude-opus-4-8/7/6 -> env.ANTHROPIC_DEFAULT_OPUS_MODEL
    //   claude-sonnet-5/-4-6-> env.ANTHROPIC_DEFAULT_SONNET_MODEL
    //   claude-haiku-4-5    -> env.ANTHROPIC_DEFAULT_HAIKU_MODEL
    //
    // Fallback rule (per user direction): if a tier has no explicit
    // mapping, ANY other tier's model substitutes for it — e.g. a vendor
    // that only maps `mimo-v2.5-pro[1M]` → fable-5 gets `mimo-v2.5-pro[1M]`
    // for haiku too. The `[1M]` context suffix is stripped at import
    // time; the capability is preserved via the provider's beta header.
    //
    // Per-imported-provider tier mapping: [haiku, sonnet, opus, fable],
    // precomputed by the importer. Only providers flagged
    // `in_failover_queue` in cc-switch contribute hops — the queue is
    // what cc-switch itself fails over through.
    let mut provider_tiers: Vec<(String, [Option<String>; 4])> = Vec::new();
    for (ccs_id, tiers, in_queue) in imported_claude {
        if !in_queue {
            log::info!(
                "import_cc_switch: {} in_failover_queue=0 — imported but not in failover chain",
                ccs_id
            );
            continue;
        }
        // Skip providers with no usable model at all.
        if tiers.iter().all(|m| m.is_none()) {
            continue;
        }
        provider_tiers.push((ccs_id.clone(), tiers.clone()));
    }

    if !provider_tiers.is_empty() {
        for m in crate::proxy::models::OFFICIAL_MODELS {
            let tier_idx = match m.id {
                "claude-haiku-4-5" => 0usize,
                "claude-sonnet-5" | "claude-sonnet-4-6" => 1,
                _ if m.id.starts_with("claude-opus-") => 2,
                _ => 3, // claude-fable-5
            };
            let new_hops: Vec<RouteHop> = provider_tiers
                .iter()
                .filter_map(|(ccs_id, tiers)| {
                    tiers[tier_idx].as_ref().map(|model| RouteHop {
                        provider: format!("ccs-{}", ccs_id),
                        model: model.clone(),
                    })
                })
                .collect();
            if new_hops.is_empty() {
                continue;
            }
            upsert_route_merge(
                db,
                m.id,
                format!("{} — imported from cc-switch", m.display_name),
                new_hops.clone(),
            )?;
            for h in &new_hops {
                imported_routes.push(ImportedRoute {
                    alias: m.id.to_string(),
                    provider: h.provider.clone(),
                    model: h.model.clone(),
                });
            }
        }
    }

    // Codex route builder: each imported codex provider's upstream models
    // are evenly distributed across the default Codex client models
    // (codex-5-6-sol / codex-5-6-luna / codex-5-6-terra / codex-5-5) via
    // `codex_catalog_assignments` (shared with the pool-row filler), so:
    //   N=1 → all four client models use the single upstream model
    //   N=2 → first two use m0, last two use m1 (two per group)
    //   N≥3 → spread across as many upstream models as available
    // Build hops per client model — ALL providers at once (in cc-switch
    // queue order) so the chain head matches the queue. Processing one
    // provider at a time would reverse the order (each merge prepends to
    // the head).
    let codex_assignments: Vec<(&String, Vec<(&'static str, String)>)> = imported_codex
        .iter()
        .map(|(ccs_id, models)| (ccs_id, codex_catalog_assignments(models)))
        .collect();
    let codex_client_aliases: Vec<&'static str> = crate::proxy::models::CODEX_MODELS
        .iter()
        .map(|m| m.id)
        .collect();
    for cm in codex_client_aliases {
        let new_hops: Vec<RouteHop> = codex_assignments
            .iter()
            .filter_map(|(ccs_id, assignments)| {
                assignments
                    .iter()
                    .find(|(alias, _)| *alias == cm)
                    .map(|(_, model)| RouteHop {
                        provider: format!("ccs-{}", ccs_id),
                        model: model.clone(),
                    })
            })
            .collect();
        if new_hops.is_empty() {
            continue;
        }
        upsert_route_merge(
            db,
            cm,
            format!("{} — imported from cc-switch", cm),
            new_hops.clone(),
        )?;
        for h in &new_hops {
            imported_routes.push(ImportedRoute {
                alias: cm.to_string(),
                provider: h.provider.clone(),
                model: h.model.clone(),
            });
        }
    }

    // Gemini route builder: each imported gemini provider with a
    // GEMINI_MODEL contributes its modelMapping entries as routes
    // (alias → upstream hop); a bare alias route is created from the
    // GEMINI_MODEL itself when no mapping exists, so the gemini chain
    // always has a default route.
    for (ccs_id, gemini_model, model_mapping) in imported_gemini {
        if model_mapping.is_empty() {
            upsert_route_merge(
                db,
                gemini_model,
                format!("{} — imported from cc-switch", gemini_model),
                vec![RouteHop {
                    provider: format!("ccs-{}", ccs_id),
                    model: gemini_model.clone(),
                }],
            )?;
            imported_routes.push(ImportedRoute {
                alias: gemini_model.clone(),
                provider: format!("ccs-{}", ccs_id),
                model: gemini_model.clone(),
            });
            continue;
        }
        for (alias, upstream) in model_mapping {
            upsert_route_merge(
                db,
                alias,
                format!("{} — imported from cc-switch", alias),
                vec![RouteHop {
                    provider: format!("ccs-{}", ccs_id),
                    model: upstream.clone(),
                }],
            )?;
            imported_routes.push(ImportedRoute {
                alias: alias.clone(),
                provider: format!("ccs-{}", ccs_id),
                model: upstream.clone(),
            });
        }
    }

    // Subscription passthrough rows ride the official claude-* routes at
    // the head: a request for the official alias relays the client
    // credential verbatim (anthropic_passthrough), with the static-key
    // pool rows behind it as fallback.
    if !passthrough_providers.is_empty() {
        for entry in passthrough_providers {
            let pp_id = format!("ccs-{}", entry.id);
            for m in crate::proxy::models::OFFICIAL_MODELS
                .iter()
                .filter(|m| m.id.starts_with("claude-"))
            {
                upsert_route_merge(
                    db,
                    m.id,
                    format!("{} — Anthropic subscription (passthrough)", m.display_name),
                    vec![RouteHop {
                        provider: pp_id.clone(),
                        model: m.id.to_string(),
                    }],
                )?;
            }
        }
    }

    Ok(imported_routes)
}
