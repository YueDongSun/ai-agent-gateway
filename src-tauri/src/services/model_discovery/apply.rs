//! Planned and applied model upgrades for one pool provider.
//!
//! Precise request targets are locked: route hop models are **never**
//! rewritten by discovery, whatever successor the upstream catalog lists.
//! Discovery may only:
//!
//! * add newly discovered same-family successors to the provider's pool
//!   list (`added_models`),
//! * prune pool ids whose generation disappeared from a listing that still
//!   shows their family (`pruned_models`; hop targets and `model_mapping`
//!   values stay pinned), and
//! * update `model_mapping` values whose display key is an explicitly
//!   tracked capability alias (`fable`/`opus`/`sonnet`/`haiku`): to a
//!   strictly newer same-family id, or — when the mapped generation was
//!   delisted — to the newest still-listed same-family id.
//!
//! Anchors are the model ids already configured for a provider (pool
//! `models`, this provider's route hop models, `model_mapping` values);
//! pinned anchors are never removed and route aliases / mapping keys are
//! never renamed. `ProviderPlan::hop_rewrites` is retained on the wire for the
//! existing UI contract but is always empty and is ignored by `apply_plan`
//! even when present.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::database::dao::proxy_providers::{
    get_proxy_providers_on_conn, save_proxy_provider_on_conn, ProxyProvider,
};
use crate::database::dao::routes::Route;
use crate::database::{lock_conn, Database};
use crate::error::AppError;

use super::fetch::UpstreamModel;
use super::rank::{family_key, find_successor, newest_listed_family_member, normalize_id, Successor};

/// Reserved wire shape: precise hop models are locked, so discovery never
/// populates this list. Kept because the frontend still reads
/// `hopRewrites` from `ProviderPlan`.
///
/// `rename_all = "camelCase"` is required even though `ProviderPlan` is
/// `#[serde(flatten)]`ed into the camelCase `ProviderReport`: an outer
/// `rename_all` does NOT rename the fields of a flattened struct.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HopRewrite {
    pub alias: String,
    pub old_model: String,
    pub new_model: String,
}

/// One `model_mapping` value to upgrade (the display key is preserved).
/// Only capability-alias keys are eligible; display-name keys are precise
/// policy and never rewritten.
///
/// See `HopRewrite` for why `rename_all` is needed on the flattened struct.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MappingRewrite {
    pub display: String,
    pub old_model: String,
    pub new_model: String,
}

/// The full set of DB mutations planned for one provider.
///
/// `rename_all` must live here, not only on the flattening `ProviderReport`
/// (serde does not propagate an outer rename into `#[serde(flatten)]`ed
/// fields). The UI reads `addedModels`/`hopRewrites`/`mappingRewrites`/
/// `skippedAmbiguous`, so snake_case keys here break a real-backend refresh.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProviderPlan {
    pub added_models: Vec<String>,
    /// Pool ids removed because a listing that still shows their family no
    /// longer lists their generation. Pinned anchors (hop targets, mapping
    /// values) are never here.
    pub pruned_models: Vec<String>,
    /// Always empty: precise hop targets are locked (see module docs).
    pub hop_rewrites: Vec<HopRewrite>,
    pub mapping_rewrites: Vec<MappingRewrite>,
    /// Anchors whose same-family candidates existed but did not rank
    /// decisively (reported so the UI can explain the no-op).
    pub skipped_ambiguous: Vec<String>,
}

impl ProviderPlan {
    pub(crate) fn has_changes(&self) -> bool {
        !self.added_models.is_empty()
            || !self.pruned_models.is_empty()
            || !self.hop_rewrites.is_empty()
            || !self.mapping_rewrites.is_empty()
    }
}

/// Capability-alias mapping keys are the only auto-tracked policy: their
/// values follow the newest same-family release. Mirrors
/// `services::model_catalog::tiers::capability_tier_for_alias` semantics;
/// kept inline so discovery compiles before the catalog module is wired.
fn is_auto_tracked_alias_key(display: &str) -> bool {
    let name = display.to_lowercase();
    name.contains("fable")
        || name.contains("opus")
        || name.contains("sonnet")
        || name.contains("haiku")
}

/// Compute the upgrades for `pp` given its routes and the fetched catalog.
/// Precise hop models are anchors for pool discovery but are never
/// rewritten themselves.
pub(crate) fn plan_provider(
    pp: &ProxyProvider,
    routes: &[Route],
    fetched: &[UpstreamModel],
) -> ProviderPlan {
    let mut anchors: Vec<String> = Vec::new();
    let mut anchor_norms: BTreeSet<String> = BTreeSet::new();
    let mut push_anchor = |raw: &str| {
        if raw.trim().is_empty() {
            return;
        }
        if anchor_norms.insert(normalize_id(raw)) {
            anchors.push(raw.to_string());
        }
    };
    for m in &pp.models {
        push_anchor(m);
    }
    for r in routes {
        for h in &r.hops {
            if h.provider == pp.id {
                push_anchor(&h.model);
            }
        }
    }
    for v in pp.model_mapping.values() {
        push_anchor(v);
    }

    let mut plan = ProviderPlan::default();
    let mut winners: BTreeMap<String, String> = BTreeMap::new();
    for anchor in &anchors {
        match find_successor(anchor, fetched) {
            Successor::Found(w) => {
                winners.insert(normalize_id(anchor), w.id);
            }
            Successor::Ambiguous => plan.skipped_ambiguous.push(anchor.clone()),
            Successor::None => {}
        }
    }
    let existing: BTreeSet<String> = pp.models.iter().map(|m| normalize_id(m)).collect();
    // Two anchors can resolve to the same winner; report and apply it once.
    let mut added_seen: BTreeSet<String> = BTreeSet::new();
    for winner in winners.values() {
        let norm = normalize_id(winner);
        if !existing.contains(&norm) && added_seen.insert(norm) {
            plan.added_models.push(winner.clone());
        }
    }
    plan.pruned_models = pruned_pool_models(pp, routes, fetched);
    let listed = listed_norms(fetched);
    // Hop models are precise targets and stay locked even when a winner
    // exists; only explicit capability-alias mapping keys auto-track.
    for (display, upstream) in &pp.model_mapping {
        if !is_auto_tracked_alias_key(display) {
            continue;
        }
        let upstream_norm = normalize_id(upstream);
        if listed.contains(&upstream_norm) {
            if let Some(winner) = winners.get(&upstream_norm) {
                if winner != upstream {
                    plan.mapping_rewrites.push(MappingRewrite {
                        display: display.clone(),
                        old_model: upstream.clone(),
                        new_model: winner.clone(),
                    });
                }
            }
            continue;
        }
        // Retirement re-point: the mapped generation is delisted, so the
        // never-downgrade rule must not keep the alias aimed at a dead id.
        // The newest still-listed same-family member takes over, even when
        // it ranks below the retired anchor.
        if let Some(fresh) = newest_listed_family_member(upstream, fetched) {
            if fresh != *upstream {
                plan.mapping_rewrites.push(MappingRewrite {
                    display: display.clone(),
                    old_model: upstream.clone(),
                    new_model: fresh,
                });
            }
        }
    }
    plan
}

/// Listed catalog ids, normalized.
fn listed_norms(fetched: &[UpstreamModel]) -> BTreeSet<String> {
    fetched.iter().map(|m| normalize_id(&m.id)).collect()
}

/// Pool entries a successful listing confirms gone: unlisted themselves,
/// not pinned by any hop target or mapping value, and with their family
/// still visible in the listing. A family that vanished entirely is kept —
/// the upstream may simply have changed listing style, not retired supply.
fn pruned_pool_models(pp: &ProxyProvider, routes: &[Route], fetched: &[UpstreamModel]) -> Vec<String> {
    if fetched.is_empty() {
        return Vec::new();
    }
    let listed = listed_norms(fetched);
    let families_listed: BTreeSet<String> = fetched
        .iter()
        .map(|m| family_key(&normalize_id(&m.id)))
        .collect();
    let pinned: BTreeSet<String> = routes
        .iter()
        .flat_map(|r| &r.hops)
        .filter(|h| h.provider == pp.id)
        .map(|h| normalize_id(&h.model))
        .chain(pp.model_mapping.values().map(|v| normalize_id(v)))
        .collect();
    pp.models
        .iter()
        .filter(|model| {
            let norm = normalize_id(model);
            !listed.contains(&norm)
                && !pinned.contains(&norm)
                && families_listed.contains(&family_key(&norm))
        })
        .cloned()
        .collect()
}

/// A fixed conflict code: the fetched catalog no longer belongs to current settings.
pub(crate) const PROVIDER_CONFLICT: &str = "AAG_MODEL_DISCOVERY_PROVIDER_CONFLICT";

/// Persist only matching pool/alias-mapping upgrades, atomically with the
/// current provider snapshot. Route hops are precise targets and are never
/// written here; any `hop_rewrites` present in the plan are ignored.
pub(crate) fn apply_plan(
    db: &Database,
    pp: &ProxyProvider,
    plan: &ProviderPlan,
) -> Result<ProviderPlan, AppError> {
    let mut conn = lock_conn!(db.conn);
    let transaction = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let current = get_proxy_providers_on_conn(&transaction, Some(&pp.id))?
        .into_iter()
        .next();
    if current.as_ref() != Some(pp) || !super::eligible_for_refresh(pp) {
        return Err(AppError::Message(PROVIDER_CONFLICT.into()));
    }

    let mut applied = ProviderPlan {
        added_models: plan.added_models.clone(),
        pruned_models: Vec::new(),
        hop_rewrites: Vec::new(),
        mapping_rewrites: Vec::new(),
        skipped_ambiguous: plan.skipped_ambiguous.clone(),
    };
    if !plan.added_models.is_empty()
        || !plan.pruned_models.is_empty()
        || !plan.mapping_rewrites.is_empty()
    {
        let mut updated = pp.clone();
        for model in &plan.added_models {
            if !updated
                .models
                .iter()
                .any(|existing| normalize_id(existing) == normalize_id(model))
            {
                updated.models.push(model.clone());
            }
        }
        for model in &plan.pruned_models {
            let norm = normalize_id(model);
            let before = updated.models.len();
            updated.models.retain(|existing| normalize_id(existing) != norm);
            if updated.models.len() != before {
                applied.pruned_models.push(model.clone());
            }
        }
        for rewrite in &plan.mapping_rewrites {
            if !is_auto_tracked_alias_key(&rewrite.display) {
                continue;
            }
            if let Some(value) = updated.model_mapping.get_mut(&rewrite.display) {
                if *value == rewrite.old_model {
                    *value = rewrite.new_model.clone();
                    applied.mapping_rewrites.push(rewrite.clone());
                }
            }
        }
        save_proxy_provider_on_conn(&transaction, &updated)?;
    }
    transaction.commit()?;
    Ok(applied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::dao::routes::{Route, RouteHop};
    use crate::database::Database;

    fn provider(id: &str, models: &[&str], mapping: &[(&str, &str)]) -> ProxyProvider {
        ProxyProvider {
            id: id.to_string(),
            name: id.to_string(),
            provider_type: "anthropic".into(),
            api_key: "k".into(),
            base_url: "https://upstream.example/v1".into(),
            models: models.iter().map(|m| m.to_string()).collect(),
            enabled: true,
            timeout_ms: 60_000,
            weight: 1,
            circuit_breaker: Default::default(),
            model_mapping: mapping
                .iter()
                .map(|(d, u)| (d.to_string(), u.to_string()))
                .collect(),
            extra_headers: Default::default(),
            auto_track_models: true,
            icon: None,
            icon_color: None,
        }
    }

    fn route(alias: &str, provider_id: &str, model: &str) -> Route {
        Route {
            alias: alias.to_string(),
            description: String::new(),
            hops: vec![RouteHop {
                provider: provider_id.to_string(),
                model: model.to_string(),
            }],
        }
    }

    fn catalog(ids: &[&str]) -> Vec<UpstreamModel> {
        ids.iter()
            .enumerate()
            .map(|(i, id)| UpstreamModel {
                id: id.to_string(),
                created: Some(1_000 + i as i64),
            })
            .collect()
    }

    #[test]
    fn delisted_pool_generation_is_pruned_while_pinned_anchors_stay() {
        // m1 is unpinned and delisted -> pruned; m2 is delisted but pinned by
        // both a route hop and a mapping value -> kept; m3 is still listed.
        let pp = provider(
            "p1",
            &["minimax-m1", "minimax-m2", "minimax-m3"],
            &[("opus", "minimax-m2"), ("sonnet", "minimax-m3")],
        );
        let routes = vec![route("locked", "p1", "minimax-m2")];
        let fetched = catalog(&["minimax-m3"]);

        let plan = plan_provider(&pp, &routes, &fetched);
        assert_eq!(plan.pruned_models, vec!["minimax-m1"]);

        let db = Database::memory().unwrap();
        db.save_proxy_provider(&pp).unwrap();
        db.save_route(&routes[0]).unwrap();
        let applied = apply_plan(&db, &pp, &plan).unwrap();
        assert_eq!(applied.pruned_models, vec!["minimax-m1"]);
        let saved = db
            .get_all_proxy_providers()
            .unwrap()
            .into_iter()
            .find(|p| p.id == "p1")
            .unwrap();
        assert!(!saved.models.contains(&"minimax-m1".to_string()));
        assert!(saved.models.contains(&"minimax-m2".to_string()));
        assert!(saved.models.contains(&"minimax-m3".to_string()));
        // The delisted mapping value re-points to the newest live member.
        assert_eq!(saved.model_mapping.get("opus").unwrap(), "minimax-m3");
    }

    #[test]
    fn vanished_family_never_prunes_its_pool_entries() {
        // The whole family left the listing: that can be a listing-style
        // change, not a retirement, so every pool entry stays.
        let pp = provider("p1", &["minimax-m2"], &[]);
        let fetched = catalog(&["brandnew-x9"]);
        let plan = plan_provider(&pp, &[], &fetched);
        assert!(plan.pruned_models.is_empty());
    }

    #[test]
    fn empty_or_missing_catalog_never_prunes() {
        let pp = provider("p1", &["minimax-m2"], &[]);
        let plan = plan_provider(&pp, &[], &[]);
        assert!(plan.pruned_models.is_empty());
    }

    #[test]
    fn delisted_alias_mapping_repoints_to_newest_live_family_member() {
        // The mapped generation (m3.1) is gone; only an older live
        // generation (m3) remains. Never-downgrade must not keep the alias
        // aimed at the dead id.
        let pp = provider("p1", &["minimax-m3"], &[("opus", "minimax-m3.1")]);
        let fetched = catalog(&["minimax-m3"]);
        let plan = plan_provider(&pp, &[], &fetched);
        assert_eq!(
            plan.mapping_rewrites,
            vec![MappingRewrite {
                display: "opus".into(),
                old_model: "minimax-m3.1".into(),
                new_model: "minimax-m3".into()
            }]
        );

        let db = Database::memory().unwrap();
        db.save_proxy_provider(&pp).unwrap();
        apply_plan(&db, &pp, &plan).unwrap();
        let saved = db
            .get_all_proxy_providers()
            .unwrap()
            .into_iter()
            .find(|p| p.id == "p1")
            .unwrap();
        assert_eq!(saved.model_mapping.get("opus").unwrap(), "minimax-m3");
    }

    #[test]
    fn listed_alias_mapping_keeps_successor_only_upgrade() {
        // The mapped generation is still listed: the classic never-downgrade
        // successor rule applies and no re-point to an older id happens.
        let pp = provider("p1", &["minimax-m3"], &[("opus", "minimax-m3")]);
        let fetched = catalog(&["minimax-m2", "minimax-m3"]);
        let plan = plan_provider(&pp, &[], &fetched);
        assert!(plan.mapping_rewrites.is_empty());
    }

    #[test]
    fn repoint_never_lands_on_a_cheap_variant() {
        let pp = provider("p1", &[], &[("opus", "minimax-m3.1")]);
        let fetched = catalog(&["minimax-m3.1-flash", "minimax-m3"]);
        let plan = plan_provider(&pp, &[], &fetched);
        assert_eq!(
            plan.mapping_rewrites,
            vec![MappingRewrite {
                display: "opus".into(),
                old_model: "minimax-m3.1".into(),
                new_model: "minimax-m3".into()
            }]
        );
    }

    #[test]
    fn adds_discovered_models_but_never_rewrites_precise_hops() {
        let pp = provider(
            "p1",
            &["MiniMax-M3"],
            &[("MiniMax M3", "MiniMax-M3"), ("opus", "MiniMax-M3")],
        );
        let routes = vec![route("max-tier", "p1", "MiniMax-M3")];
        let fetched = catalog(&["MiniMax-M2", "MiniMax-M3", "MiniMax-M3.1"]);

        let plan = plan_provider(&pp, &routes, &fetched);
        assert_eq!(plan.added_models, vec!["MiniMax-M3.1"]);
        // Precise hop targets are locked: discovery may never rewrite them.
        assert!(plan.hop_rewrites.is_empty());
        // Only the capability-alias mapping key auto-tracks; the display
        // name stays a precise mapping value.
        assert_eq!(
            plan.mapping_rewrites,
            vec![MappingRewrite {
                display: "opus".into(),
                old_model: "MiniMax-M3".into(),
                new_model: "MiniMax-M3.1".into()
            }]
        );
        assert!(plan.skipped_ambiguous.is_empty());

        let db = Database::memory().unwrap();
        db.save_proxy_provider(&pp).unwrap();
        db.save_route(&routes[0]).unwrap();
        let applied = apply_plan(&db, &pp, &plan).unwrap();
        assert!(applied.hop_rewrites.is_empty());

        let saved_route = db.get_route("max-tier").unwrap().unwrap();
        assert_eq!(saved_route.alias, "max-tier");
        assert_eq!(saved_route.description, "");
        assert_eq!(
            saved_route.hops[0].model, "MiniMax-M3",
            "precise hop locked"
        );

        let saved_pp = db
            .get_all_proxy_providers()
            .unwrap()
            .into_iter()
            .find(|p| p.id == "p1")
            .unwrap();
        assert!(saved_pp.models.contains(&"MiniMax-M3".to_string()));
        assert!(saved_pp.models.contains(&"MiniMax-M3.1".to_string()));
        assert_eq!(
            saved_pp.model_mapping.get("MiniMax M3").unwrap(),
            "MiniMax-M3",
            "display-name mappings are precise"
        );
        assert_eq!(
            saved_pp.model_mapping.get("opus").unwrap(),
            "MiniMax-M3.1",
            "alias mappings auto-track"
        );
    }

    #[test]
    fn hop_models_still_anchor_pool_discovery() {
        // The pool list is empty but a route hop pins the anchor: the
        // successor joins the pool while the hop itself stays untouched.
        let pp = provider("p1", &[], &[]);
        let routes = vec![route("alias-a", "p1", "minimax-m3")];
        let fetched = catalog(&["minimax-m3", "minimax-m3.1"]);
        let plan = plan_provider(&pp, &routes, &fetched);
        assert_eq!(plan.added_models, vec!["minimax-m3.1"]);
        assert!(plan.hop_rewrites.is_empty());
    }

    #[test]
    fn apply_ignores_hop_rewrites_even_from_legacy_plans() {
        // Defense in depth: a deserialized legacy plan (or a hand-built one)
        // carrying hop rewrites must never reach the routes table. The
        // failure trigger on `blocked_route` would abort the apply if any
        // route write happened at all.
        let db = Database::memory().unwrap();
        let snapshot = provider("p1", &["minimax-m3"], &[]);
        db.save_proxy_provider(&snapshot).unwrap();
        db.save_route(&route("blocked_route", "p1", "minimax-m3"))
            .unwrap();
        crate::database::dao::routes::install_discovery_write_failure(&db);

        let hostile = ProviderPlan {
            added_models: vec!["minimax-m4".into()],
            pruned_models: Vec::new(),
            hop_rewrites: vec![HopRewrite {
                alias: "blocked_route".into(),
                old_model: "minimax-m3".into(),
                new_model: "minimax-m4".into(),
            }],
            mapping_rewrites: Vec::new(),
            skipped_ambiguous: Vec::new(),
        };
        let applied = apply_plan(&db, &snapshot, &hostile).unwrap();
        assert!(applied.hop_rewrites.is_empty());
        assert!(applied.added_models.contains(&"minimax-m4".to_string()));
        let saved = db.get_route("blocked_route").unwrap().unwrap();
        assert_eq!(saved.hops[0].model, "minimax-m3", "precise hop locked");
    }

    #[test]
    fn never_downgrades_when_upstream_has_no_successor() {
        let pp = provider("p1", &["minimax-m3.1"], &[]);
        let routes = vec![route("a", "p1", "minimax-m3.1")];
        let fetched = catalog(&["minimax-m3", "minimax-m3.1"]);
        let plan = plan_provider(&pp, &routes, &fetched);
        assert!(!plan.has_changes());
        assert!(plan.skipped_ambiguous.is_empty());
    }

    #[test]
    fn unrelated_new_models_do_not_trigger_changes() {
        let pp = provider("p1", &["deepseek-chat"], &[]);
        let fetched = catalog(&["deepseek-chat", "deepseek-reasoner", "brandnew-x9"]);
        let plan = plan_provider(&pp, &[], &fetched);
        assert!(!plan.has_changes());
        // reasoner is a different family (excluded), x9 as well: nothing
        // was ambiguous, there was simply no same-family successor.
        assert!(plan.skipped_ambiguous.is_empty());
    }

    #[test]
    fn other_providers_hops_are_never_touched() {
        let pp = provider("p1", &["minimax-m3"], &[]);
        let routes = vec![
            route("mine", "p1", "minimax-m3"),
            route("theirs", "p2", "minimax-m3"),
        ];
        let fetched = catalog(&["minimax-m3", "minimax-m3.1"]);
        let plan = plan_provider(&pp, &routes, &fetched);
        assert!(plan.hop_rewrites.is_empty());
    }

    #[test]
    fn apply_is_idempotent() {
        let pp = provider("p1", &["minimax-m3"], &[("opus", "minimax-m3")]);
        let routes = vec![route("a", "p1", "minimax-m3")];
        let fetched = catalog(&["minimax-m3", "minimax-m3.1"]);
        let plan = plan_provider(&pp, &routes, &fetched);
        assert_eq!(plan.added_models, vec!["minimax-m3.1"]);
        assert_eq!(plan.mapping_rewrites.len(), 1);

        let db = Database::memory().unwrap();
        db.save_proxy_provider(&pp).unwrap();
        db.save_route(&routes[0]).unwrap();
        apply_plan(&db, &pp, &plan).unwrap();

        // Second run: the anchor is now the winner itself, so nothing changes.
        let saved_pp = db
            .get_all_proxy_providers()
            .unwrap()
            .into_iter()
            .find(|p| p.id == "p1")
            .unwrap();
        let saved_route = db.get_route("a").unwrap().unwrap();
        let plan2 = plan_provider(&saved_pp, &[saved_route], &fetched);
        assert!(!plan2.has_changes());
    }

    #[test]
    fn stale_provider_snapshots_preserve_every_user_field() {
        let mutations: &[fn(&mut ProxyProvider)] = &[
            |current| current.name = "edited".into(),
            |current| current.provider_type = "openai".into(),
            |current| current.api_key = "changed_key".into(),
            |current| current.base_url = "https://changed.example/v1".into(),
            |current| current.models.push("user_model".into()),
            |current| current.enabled = false,
            |current| current.timeout_ms += 1,
            |current| current.weight += 1,
            |current| current.circuit_breaker.cooldown_ms += 1,
            |current| {
                current
                    .model_mapping
                    .insert("user_alias".into(), "user_model".into());
            },
            |current| {
                current
                    .extra_headers
                    .insert("x-user".into(), "edited".into());
            },
            |current| current.auto_track_models = false,
        ];
        for mutate in mutations {
            let db = Database::memory().unwrap();
            let snapshot = provider("p1", &["minimax-m3"], &[]);
            let original_route = route("alias", "p1", "minimax-m3");
            let plan = plan_provider(
                &snapshot,
                &[original_route.clone()],
                &catalog(&["minimax-m4"]),
            );
            assert!(!plan.added_models.is_empty());
            db.save_proxy_provider(&snapshot).unwrap();
            db.save_route(&original_route).unwrap();
            let mut current = snapshot.clone();
            mutate(&mut current);
            db.save_proxy_provider(&current).unwrap();
            let error = apply_plan(&db, &snapshot, &plan).unwrap_err();
            assert_eq!(error.external_message(), PROVIDER_CONFLICT);
            assert_eq!(db.get_all_proxy_providers().unwrap(), vec![current]);
            assert_eq!(
                db.get_route("alias").unwrap().unwrap().hops[0].model,
                "minimax-m3"
            );
        }
    }

    #[test]
    fn deleted_provider_is_not_recreated() {
        let db = Database::memory().unwrap();
        let snapshot = provider("p1", &["minimax-m3"], &[]);
        db.save_proxy_provider(&snapshot).unwrap();
        let plan = plan_provider(&snapshot, &[], &catalog(&["minimax-m4"]));
        db.delete_proxy_provider("p1").unwrap();
        assert_eq!(
            apply_plan(&db, &snapshot, &plan)
                .unwrap_err()
                .external_message(),
            PROVIDER_CONFLICT
        );
        assert!(db.get_all_proxy_providers().unwrap().is_empty());
    }

    #[test]
    fn provider_write_failure_rolls_back_the_whole_apply() {
        let db = Database::memory().unwrap();
        let snapshot = provider("p1", &["minimax-m3"], &[("opus", "minimax-m3")]);
        let routes = [route("first_route", "p1", "minimax-m3")];
        db.save_proxy_provider(&snapshot).unwrap();
        for original_route in &routes {
            db.save_route(original_route).unwrap();
        }
        let plan = plan_provider(&snapshot, &routes, &catalog(&["minimax-m4"]));
        assert!(!plan.added_models.is_empty());
        assert_eq!(plan.mapping_rewrites.len(), 1);
        {
            let conn = db.conn.lock().unwrap();
            conn.execute_batch(
                "CREATE TRIGGER discovery_provider_write_failure BEFORE UPDATE ON proxy_providers
                 BEGIN SELECT RAISE(FAIL, 'injected_provider_write_failure'); END;
                 CREATE TRIGGER discovery_provider_insert_failure BEFORE INSERT ON proxy_providers
                 BEGIN SELECT RAISE(FAIL, 'injected_provider_write_failure'); END;",
            )
            .unwrap();
        }
        assert!(apply_plan(&db, &snapshot, &plan).is_err());
        // The pool addition and the alias mapping rewrite roll back together.
        assert_eq!(db.get_all_proxy_providers().unwrap(), vec![snapshot]);
        for original_route in &routes {
            assert_eq!(
                db.get_route(&original_route.alias).unwrap().unwrap().hops[0].model,
                "minimax-m3"
            );
        }
    }
}
