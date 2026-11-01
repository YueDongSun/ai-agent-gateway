//! Supplier/model-catalog planner behind the router's capability-alias hook.
//!
//! Built once per runtime snapshot from pure reads (supplier offers + public
//! catalog evidence) and installed on the router. Selector semantics follow
//! `proxy::planner`: `precise:` forces the literal model, `alias:` forces a
//! capability alias, a bare name that is canonically offered is an exact
//! call, other known alias/family names plan by tier with traceable
//! downgrades, and everything else stays on the legacy chain.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::database::Database;
use crate::error::AppError;
use crate::proxy::canonical::ProviderError;
use crate::proxy::planner::{Model_planner, Planned_hop, Request_features, Resolution};
use crate::services::model_catalog::select::{self, HardRequirements, OfferedModel, Selection};
use crate::services::model_catalog::tiers::{
    capability_tier_for_alias, classify_selector, CatalogEvidence, Selector,
};
use crate::services::model_catalog::{load_evidence, offered_models};

/// Planning inputs frozen into one runtime snapshot. Pure data: request-time
/// resolution performs no database or network access.
#[allow(non_camel_case_types)]
pub(crate) struct Supplier_model_planner {
    offers: Vec<OfferedModel>,
    evidence: CatalogEvidence,
    /// Supplier endpoint id -> projected proxy provider id. Offers without a
    /// routable projection are dropped at build time.
    provider_by_endpoint: HashMap<String, String>,
    priority_by_endpoint: HashMap<String, i64>,
    routing_identities: HashMap<String, crate::proxy::routing_receipts::contract::RoutingIdentity>,
}

pub(crate) fn build_model_planner(
    db: &Database,
    db_providers: &[crate::database::dao::proxy_providers::ProxyProvider],
) -> Result<Arc<Supplier_model_planner>, AppError> {
    build_model_planner_inner(db, db_providers, None)
}

/// Runtime admission defines the public mirror's relative Arena cohort.
/// Keep the complete offer inventory for exact and account-backed calls; only
/// the cohort used to assign relative routing tiers is credentialless supply.
pub(crate) fn build_model_planner_for_runtime(
    db: &Database,
    db_providers: &[crate::database::dao::proxy_providers::ProxyProvider],
    admitted_credentialless_provider_ids: &HashSet<String>,
) -> Result<Arc<Supplier_model_planner>, AppError> {
    build_model_planner_inner(db, db_providers, Some(admitted_credentialless_provider_ids))
}

fn build_model_planner_inner(
    db: &Database,
    db_providers: &[crate::database::dao::proxy_providers::ProxyProvider],
    cohort_provider_ids: Option<&HashSet<String>>,
) -> Result<Arc<Supplier_model_planner>, AppError> {
    let mut evidence = load_evidence(db);
    if let Some(raw) = db.get_setting(crate::services::model_catalog::utility::POLICY_SETTING)? {
        evidence.utility_policy = crate::services::model_catalog::utility::Utility_policy::parse(&raw)
            .map_err(|code| AppError::InvalidInput(code.into()))?;
    }
    let snapshot = crate::services::suppliers::resolved_supplier_snapshot(db)?;
    let mut provider_by_endpoint = HashMap::new();
    let mut routing_identities = HashMap::new();
    for endpoint in &snapshot.endpoints {
        if let Some(provider_id) = endpoint.legacy_proxy_provider_id.clone() {
            use crate::proxy::routing_receipts::contract::{safe_reference, AssociationKind, RoutingIdentity};
            let identity = RoutingIdentity {
                provider_ref: safe_reference(&provider_id), supplier_ref: safe_reference(&endpoint.supplier_id),
                endpoint_ref: safe_reference(&endpoint.endpoint_id), association: AssociationKind::SupplierEndpoint,
            };
            let identity = if identity.provider_ref.is_some() && identity.supplier_ref.is_some() && identity.endpoint_ref.is_some() {
                identity
            } else { RoutingIdentity { provider_ref:safe_reference(&provider_id), supplier_ref:None, endpoint_ref:None, association:AssociationKind::Unmapped } };
            match routing_identities.entry(provider_id.clone()) {
                std::collections::hash_map::Entry::Vacant(entry) => { entry.insert(identity); },
                std::collections::hash_map::Entry::Occupied(mut entry) => {
                    entry.insert(RoutingIdentity { provider_ref:safe_reference(&provider_id), supplier_ref:None, endpoint_ref:None, association:AssociationKind::Ambiguous });
                },
            }
            provider_by_endpoint.insert(endpoint.endpoint_id.clone(), provider_id);
        }
    }
    let mut offers: Vec<OfferedModel> = offered_models(db)?
        .into_iter()
        .filter(|offer| provider_by_endpoint.contains_key(&offer.endpoint_ref))
        .collect();
    // Manual proxy providers without a supplier projection keep working for
    // exact calls: synthesize unknown-capability offers so `precise:` and
    // bare exact names can still plan them. Priority mirrors the legacy
    // weight axis (larger weight first) onto the supplier axis (smaller
    // first).
    let projected_providers: std::collections::HashSet<String> =
        provider_by_endpoint.values().cloned().collect();
    for provider in db_providers {
        if !provider.enabled || projected_providers.contains(provider.id.as_str()) {
            continue;
        }
        let admitted: Option<HashSet<_>> = crate::services::provider_admission::enforcement_enabled(db)
            .then(|| crate::services::provider_admission::admitted_models(db, provider).into_iter().collect());
        routing_identities.insert(provider.id.clone(), crate::proxy::routing_receipts::contract::RoutingIdentity {
            provider_ref: crate::proxy::routing_receipts::contract::safe_reference(&provider.id), supplier_ref:None, endpoint_ref:None,
            association: crate::proxy::routing_receipts::contract::AssociationKind::ManualProvider,
        });
        for model in &provider.models {
            if admitted.as_ref().is_some_and(|models| !models.contains(model)) { continue; }
            let endpoint_ref = format!("provider:{}", provider.id);
            provider_by_endpoint
                .entry(endpoint_ref.clone())
                .or_insert_with(|| provider.id.clone());
            offers.push(OfferedModel {
                model_id: model.clone(),
                endpoint_ref,
                supplier_ref: provider.id.clone(),
                protocol: provider.provider_type.clone(),
                priority: 2_000_001 - i64::from(provider.weight.min(2_000_001)),
                supports_tools: None,
                supports_images: None,
                context_window: None,
                max_output_tokens: None,
                tier_override: None,
            });
        }
    }
    let cohort_models = offers
        .iter()
        .filter(|offer| {
            cohort_provider_ids.is_none_or(|admitted| {
                provider_by_endpoint
                    .get(&offer.endpoint_ref)
                    .is_some_and(|provider| admitted.contains(provider))
            })
        })
        .map(|offer| offer.model_id.clone())
        .collect();
    let evidence = evidence.with_offered_models(cohort_models);
    let priority_by_endpoint = offers
        .iter()
        .map(|offer| (offer.endpoint_ref.clone(), offer.priority))
        .collect();
    Ok(Arc::new(Supplier_model_planner {
        offers: enrich_offers(offers, &evidence),
        evidence,
        provider_by_endpoint,
        priority_by_endpoint,
        routing_identities,
    }))
}

/// Fill an offer's unknown capability facts from the public catalog.
///
/// A supplier that states a capability keeps its own statement: manual facts
/// always win. Only the fields the supplier left unknown are taken from
/// models.dev, and only for a model the catalog actually knows. A model the
/// catalog does not know stays unknown, so alias planning keeps failing
/// closed for it instead of guessing.
fn enrich_offers(mut offers: Vec<OfferedModel>, evidence: &CatalogEvidence) -> Vec<OfferedModel> {
    crate::services::model_catalog::enrich_capabilities(&mut offers, evidence);
    offers
}

impl Supplier_model_planner {
    fn to_hops(&self, selections: Vec<Selection>) -> Vec<Planned_hop> {
        let mut seen = HashSet::new();
        selections
            .into_iter()
            .filter_map(|selection| {
                let provider = self.provider_by_endpoint.get(&selection.endpoint_ref)?;
                // Endpoint aliases can map to the same executable provider.
                // Deduplicate only exact wire pairs after resolving that mapping.
                if !seen.insert((provider.clone(), selection.model_id.clone())) {
                    return None;
                }
                Some(Planned_hop {
                    provider: provider.clone(),
                    model: selection.model_id,
                    tier: selection.tier,
                    priority: self
                        .priority_by_endpoint
                        .get(&selection.endpoint_ref)
                        .copied()
                        .unwrap_or(i64::MAX),
                    basis: selection.basis.code().to_string(),
                    downgraded: selection.downgraded,
                    reason: selection.reason.to_string(),
                })
            })
            .collect()
    }

    /// Keep the utility winner first, then prefer a prefix with distinct
    /// executable providers and model IDs. All remaining pairs retain their
    /// ranked order: diversity changes preference, never fallback membership.
    fn to_alias_hops(&self, selections: Vec<Selection>) -> Vec<Planned_hop> {
        crate::proxy::planner::diverse_fallback_order(self.to_hops(selections),
            |hop| (hop.provider.as_str(), hop.model.as_str()))
    }

    fn hard_requirements(features: Request_features) -> HardRequirements {
        HardRequirements {
            needs_tools: features.needs_tools,
            needs_images: features.needs_images,
            min_context_window: features.min_context_window,
            min_max_output_tokens: features.min_max_output_tokens,
        }
    }

    fn canonically_offered(&self, model: &str) -> bool {
        let wanted = crate::services::model_catalog::identity::canonical_model_id(model);
        self.offers.iter().any(|offer| {
            crate::services::model_catalog::identity::canonical_model_id(&offer.model_id) == wanted
        })
    }
}

impl Model_planner for Supplier_model_planner {
    fn resolve_excluding(&self, raw_model: &str, features: Request_features,
        excluded: &HashSet<(String, String)>) -> Result<Resolution, ProviderError> {
        let resolution = self.resolve(raw_model, features)?;
        if excluded.is_empty() { return Ok(resolution); }
        match resolution {
            Resolution::Alias { alias, tier, .. } => {
                let offers: Vec<_> = self.offers.iter().filter(|offer|
                    self.provider_by_endpoint.get(&offer.endpoint_ref).is_some_and(|provider|
                        !excluded.contains(&(provider.clone(), offer.model_id.clone())))).cloned().collect();
                let hops = self.to_alias_hops(select::plan_alias(tier, &offers, &Self::hard_requirements(features), &self.evidence));
                Ok(Resolution::Alias { alias, tier, hops })
            },
            other => Ok(other),
        }
    }
    fn routing_identity(&self, provider_id: &str) -> Option<crate::proxy::routing_receipts::contract::RoutingIdentity> {
        self.routing_identities.get(provider_id).cloned()
    }

    fn resolve(
        &self,
        raw_model: &str,
        features: Request_features,
    ) -> Result<Resolution, ProviderError> {
        let trimmed = raw_model.trim();
        if trimmed.is_empty() {
            return Ok(Resolution::Exact(trimmed.to_string()));
        }
        let hard = Self::hard_requirements(features);
        if let Some(model) = trimmed.strip_prefix("precise:") {
            let model = model.trim();
            if model.is_empty() {
                return Err(crate::proxy::planner::selector_invalid_error());
            }
            let hops = self.to_hops(select::plan_precise(
                model,
                &self.offers,
                &hard,
                &self.evidence,
            ));
            return Ok(Resolution::Precise {
                model: model.to_string(),
                hops,
            });
        }
        if let Some(name) = trimmed.strip_prefix("alias:") {
            let name = name.trim();
            let Some(tier) = crate::proxy::planner::client_role_tier(name)
                .or_else(|| capability_tier_for_alias(name))
            else {
                return Err(crate::proxy::planner::alias_unknown_error(name));
            };
            let hops = self.to_alias_hops(select::plan_alias(
                tier,
                &self.offers,
                &hard,
                &self.evidence,
            ));
            return Ok(Resolution::Alias {
                alias: name.to_string(),
                tier,
                hops,
            });
        }
        // Bare names that are canonically offered are exact calls: the named
        // model is never substituted, and `precise:` is not required.
        if self.canonically_offered(trimmed) {
            let hops = self.to_hops(select::plan_precise(
                trimmed,
                &self.offers,
                &hard,
                &self.evidence,
            ));
            return Ok(Resolution::Precise {
                model: trimmed.to_string(),
                hops,
            });
        }
        // Canonical desktop role spellings are gateway-wide capability
        // selectors. Their surface never changes the requested tier, and
        // exact offered names above retain precedence over this policy.
        if let Some(tier) = crate::proxy::planner::client_role_tier(trimmed) {
            let hops = self.to_alias_hops(select::plan_alias(
                tier,
                &self.offers,
                &hard,
                &self.evidence,
            ));
            return Ok(Resolution::Alias {
                alias: trimmed.into(),
                tier,
                hops,
            });
        }
        // Other bare names: known capability aliases/family-role names plan
        // by tier; everything else stays on the legacy chain untouched.
        match classify_selector(trimmed) {
            Ok(Selector::Alias { name, tier }) => {
                let hops = self.to_alias_hops(select::plan_alias(
                    tier,
                    &self.offers,
                    &hard,
                    &self.evidence,
                ));
                Ok(Resolution::Alias {
                    alias: name,
                    tier,
                    hops,
                })
            }
            _ => Ok(Resolution::Exact(trimmed.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::model_catalog::tiers::SourceStatus;

    #[test]
    fn runtime_cohort_omits_self_loop_passthrough_and_missing_supply_but_retains_exact_offers() {
        let db = Database::memory().unwrap();
        let rows: Vec<_> = (1..=8)
            .map(|rank| {
                serde_json::json!({"row":{
                    "model_name":format!("rated-model-{rank}"), "rank":rank,
                    "rating":1500-rank, "rating_lower":1490-rank, "rating_upper":1510-rank,
                    "vote_count":1000, "category":"overall",
                    "leaderboard_publish_date":chrono::Utc::now().date_naive().to_string()
                }})
            })
            .collect();
        crate::services::suppliers::save_public_catalog_cache(
            &db,
            crate::services::suppliers::PublicCatalogCacheInput {
                source: crate::services::model_catalog::SOURCE_LMARENA.into(),
                raw_json: serde_json::json!({"rows":rows,"num_rows_total":8}).to_string(),
                fetched_at: chrono::Utc::now().timestamp(),
            },
        )
        .unwrap();
        let row = |id: &str, model: &str, protocol: &str, base_url: &str| {
            serde_json::from_value::<crate::database::dao::proxy_providers::ProxyProvider>(
                serde_json::json!({
                    "id":id, "name":id, "type":protocol, "baseURL":base_url,
                    "apiKey":"fixture", "models":[model], "enabled":true
                }),
            )
            .unwrap()
        };
        let config = db.get_proxy_config().unwrap();
        let self_url = format!("http://{}:{}", config.client_endpoint_host(), config.port);
        let providers = vec![
            row("self_loop", "rated-model-1", "openai", &self_url),
            row(
                "passthrough",
                "rated-model-2",
                "anthropic_passthrough",
                "https://example.invalid",
            ),
            row(
                "runtime_missing",
                "rated-model-3",
                "openai",
                "https://example.invalid",
            ),
            row(
                "admitted",
                "rated-model-4",
                "openai",
                "https://example.invalid",
            ),
        ];
        let configured = build_model_planner(&db, &providers).unwrap();
        let admitted = HashSet::from(["admitted".to_string()]);
        let runtime = build_model_planner_for_runtime(&db, &providers, &admitted).unwrap();
        let assess = |planner: &Supplier_model_planner| {
            crate::services::model_catalog::tiers::evaluate_model(
                "rated-model-4",
                None,
                &planner.evidence,
            )
        };
        let configured_assessment = assess(configured.as_ref());
        assert_eq!(configured_assessment.tier, Some(0));
        assert_eq!(
            configured_assessment.arena.unwrap().compared_offered_models,
            Some(4)
        );
        let runtime_assessment = assess(runtime.as_ref());
        assert_eq!(runtime_assessment.tier, Some(3));
        let arena = runtime_assessment.arena.unwrap();
        assert_eq!(arena.compared_offered_models, Some(1));
        assert_eq!((arena.rank, arena.total_models), (4, 8));
        assert_eq!(runtime.offers.len(), configured.offers.len());
        match runtime
            .resolve("precise:rated-model-2", Default::default())
            .unwrap()
        {
            Resolution::Precise { hops, .. } => {
                assert_eq!(hops.len(), 1);
                assert_eq!(hops[0].provider, "passthrough");
                assert_eq!(hops[0].model, "rated-model-2");
            }
            other => panic!("exact account-backed supply must remain explicit: {other:?}"),
        }
    }

    fn offer(model: &str, endpoint: &str, priority: i64, tier: Option<u8>) -> OfferedModel {
        OfferedModel {
            model_id: model.into(),
            endpoint_ref: endpoint.into(),
            supplier_ref: "supplier".into(),
            protocol: "anthropic".into(),
            priority,
            supports_tools: Some(true),
            supports_images: Some(true),
            context_window: Some(200_000),
            max_output_tokens: Some(64_000),
            tier_override: tier,
        }
    }

    fn empty_status(source: &str) -> SourceStatus {
        SourceStatus {
            source: source.into(),
            fetched_at: None,
            last_error: None,
            stale: true,
        }
    }

    fn planner(offers: Vec<OfferedModel>) -> Supplier_model_planner {
        let mut provider_by_endpoint = HashMap::new();
        for offer in &offers {
            provider_by_endpoint
                .entry(offer.endpoint_ref.clone())
                .or_insert_with(|| format!("prov-{}", offer.endpoint_ref));
        }
        let priority_by_endpoint = offers
            .iter()
            .map(|offer| (offer.endpoint_ref.clone(), offer.priority))
            .collect();
        Supplier_model_planner {
            offers,
            evidence: CatalogEvidence::new(
                None,
                empty_status("models_dev"),
                None,
                empty_status("evalplus"),
            ),
            provider_by_endpoint,
            priority_by_endpoint,
            routing_identities: HashMap::new(),
        }
    }

    #[test]
    fn healthy_overflow_model_refills_slots_after_preferred_models_fail() {
        let planner = planner(vec![offer("a", "same", 1, Some(2)), offer("b", "same", 1, Some(2)), offer("c", "same", 1, Some(2))]);
        let excluded = HashSet::from([("prov-same".into(), "a".into()), ("prov-same".into(), "b".into())]);
        let Resolution::Alias { hops, .. } = planner.resolve_excluding("alias:opus", Default::default(), &excluded).unwrap() else { panic!("expected alias"); };
        assert_eq!(hops.iter().map(|hop| hop.model.as_str()).collect::<Vec<_>>(), vec!["c"]);
        let Resolution::Precise { hops, .. } = planner.resolve_excluding("precise:a", Default::default(), &excluded).unwrap() else { panic!("expected exact model"); };
        assert_eq!(hops[0].model, "a");
    }

    #[test]
    fn alias_queue_prefers_distinct_provider_and_model_then_retains_every_pair() {
        let planner = planner(vec![
            offer("a", "first", 1, Some(2)),
            offer("b", "first", 1, Some(2)),
            offer("a", "second", 2, Some(2)),
            offer("b", "second", 2, Some(2)),
            offer("c", "third", 3, Some(2)),
            offer("c", "third", 3, Some(2)),
        ]);
        let Resolution::Alias { hops, .. } = planner.resolve("alias:opus", no_hard()).unwrap() else { panic!("expected alias"); };
        let pairs: Vec<_> = hops.iter().map(|hop| (hop.provider.as_str(), hop.model.as_str())).collect();
        assert_eq!(pairs, vec![("prov-first", "a"), ("prov-second", "b"), ("prov-third", "c"), ("prov-first", "b"), ("prov-second", "a")]);
        let mut reversed = planner.offers.clone();
        reversed.reverse();
        let reversed_planner = self::planner(reversed);
        let Resolution::Alias { hops: reversed_hops, .. } = reversed_planner.resolve("alias:opus", no_hard()).unwrap() else { panic!("expected alias"); };
        assert_eq!(reversed_hops.iter().map(|hop| (hop.provider.as_str(), hop.model.as_str())).collect::<Vec<_>>(), pairs);
        let Resolution::Precise { hops, .. } = planner.resolve("precise:a", no_hard()).unwrap() else { panic!("expected precise"); };
        assert_eq!(hops.iter().map(|hop| (hop.provider.as_str(), hop.model.as_str())).collect::<Vec<_>>(), vec![("prov-first", "a"), ("prov-second", "a")]);
    }

    #[test]
    fn same_model_supply_retains_all_distinct_providers_without_forced_substitution() {
        let planner = planner(vec![offer("shared", "first", 1, Some(2)), offer("shared", "second", 2, Some(2)), offer("shared", "third", 3, Some(2))]);
        for selector in ["alias:opus", "precise:shared"] {
            let (Resolution::Alias { hops, .. } | Resolution::Precise { hops, .. }) = planner.resolve(selector, no_hard()).unwrap() else { panic!("expected planned queue"); };
            assert_eq!(hops.iter().map(|hop| (hop.provider.as_str(), hop.model.as_str())).collect::<Vec<_>>(), vec![("prov-first", "shared"), ("prov-second", "shared"), ("prov-third", "shared")]);
        }
    }

    #[test]
    fn route_projection_deduplicates_actual_provider_pairs_not_supplier_names() {
        let mut planner = planner(vec![
            offer("a", "first", 1, Some(2)),
            offer("b", "first", 1, Some(2)),
            offer("a", "alias_endpoint", 2, Some(2)),
            offer("b", "alias_endpoint", 2, Some(2)),
            offer("a", "separate", 3, Some(2)),
            offer("b", "separate", 3, Some(2)),
        ]);
        planner.provider_by_endpoint.insert("alias_endpoint".into(), "prov-first".into());
        let Resolution::Alias { hops, .. } = planner.resolve("alias:opus", no_hard()).unwrap() else { panic!("expected alias"); };
        assert_eq!(hops.iter().map(|hop| (hop.provider.as_str(), hop.model.as_str())).collect::<Vec<_>>(), vec![("prov-first", "a"), ("prov-separate", "b"), ("prov-first", "b"), ("prov-separate", "a")]);
        let Resolution::Precise { hops, .. } = planner.resolve("precise:a", no_hard()).unwrap() else { panic!("expected precise"); };
        assert_eq!(hops.iter().map(|hop| hop.provider.as_str()).collect::<Vec<_>>(), vec!["prov-first", "prov-separate"]);
    }

    #[test]
    fn diverse_fallbacks_still_obey_hard_capabilities_and_health_exclusion() {
        let mut rejected = offer("b", "other", 0, Some(2));
        rejected.supports_tools = Some(false);
        let planner = planner(vec![offer("a", "first", 1, Some(2)), rejected, offer("c", "third", 2, Some(2)), offer("d", "fourth", 3, Some(2))]);
        let excluded = HashSet::from([("prov-third".into(), "c".into())]);
        let features = Request_features { needs_tools: true, ..Default::default() };
        let Resolution::Alias { hops, .. } = planner.resolve_excluding("alias:opus", features, &excluded).unwrap() else { panic!("expected alias"); };
        assert_eq!(hops.iter().map(|hop| (hop.provider.as_str(), hop.model.as_str())).collect::<Vec<_>>(), vec![("prov-first", "a"), ("prov-fourth", "d")]);
    }

    /// Public catalog fixture: one known model with full capability facts.
    fn catalog_evidence() -> CatalogEvidence {
        let raw = r#"{"anthropic": {"models": {
            "claude-fable-5.1": {
                "id": "claude-fable-5.1",
                "family": "claude-fable",
                "tool_call": true,
                "modalities": {"input": ["text", "image"], "output": ["text"]},
                "limit": {"context": 1000000, "output": 128000}
            },
            "text-only-model": {
                "id": "text-only-model",
                "family": "other",
                "tool_call": false,
                "modalities": {"input": ["text"], "output": ["text"]},
                "limit": {"context": 8000, "output": 4096}
            }
        }}}"#;
        let models_dev = crate::services::model_catalog::parse::parse_models_dev(raw)
            .expect("catalog fixture parses");
        CatalogEvidence::new(
            Some(models_dev),
            empty_status("models_dev"),
            None,
            empty_status("evalplus"),
        )
    }

    fn unknown_offer(model: &str, endpoint: &str) -> OfferedModel {
        let mut offer = offer(model, endpoint, 1, None);
        offer.supports_tools = None;
        offer.supports_images = None;
        offer.context_window = None;
        offer.max_output_tokens = None;
        offer
    }

    #[test]
    fn canonical_client_roles_request_distinct_assessed_tiers() {
        let planner = planner(vec![
            offer("strongest", "ep3", 0, Some(3)),
            offer("strong", "ep2", 0, Some(2)),
            offer("balanced", "ep1", 0, Some(1)),
            offer("fast", "ep0", 0, Some(0)),
        ]);
        for (selector, wanted_tier, first_model) in [
            ("claude-fable-5", 3, "strongest"),
            ("claude-opus-5", 2, "strong"),
            ("claude-sonnet-5", 1, "balanced"),
            ("claude-haiku-5-5", 0, "fast"),
            ("gpt-5.6-sol", 3, "strongest"),
            ("gpt-5.6-terra", 1, "balanced"),
            ("gpt-5.6-luna", 0, "fast"),
        ] {
            match planner.resolve(selector, Default::default()).unwrap() {
                Resolution::Alias { tier, hops, .. } => {
                    assert_eq!(tier, wanted_tier);
                    assert_eq!(hops[0].model, first_model);
                    assert_eq!(hops[0].tier, Some(wanted_tier));
                    assert!(!hops[0].downgraded);
                    assert!(hops.iter().skip(1).all(|hop| hop.downgraded || hop.reason == "cross_tier_fallback"));
                    assert!(hops.iter().all(|hop| hop.basis == "manual_override"));
                }
                other => panic!("expected ranked alias, got {other:?}"),
            }
        }
    }

    #[test]
    fn canonical_role_plans_retain_request_capability_filters_and_exact_precedence() {
        let mut image_missing = offer("strongest", "ep3", 0, Some(3));
        image_missing.supports_images = Some(false);
        let mut tools_unknown = offer("strong", "ep2", 0, Some(2));
        tools_unknown.supports_tools = None;
        let planner = planner(vec![
            image_missing,
            tools_unknown,
            offer("balanced", "ep1", 0, Some(1)),
            offer("gpt-5.6-sol", "literal", 0, None),
        ]);
        let features = Request_features {
            needs_tools: true,
            needs_images: true,
            ..Default::default()
        };
        match planner.resolve("claude-fable-5", features).unwrap() {
            Resolution::Alias { hops, .. } => {
                assert_eq!(hops.len(), 1);
                assert_eq!(hops[0].model, "balanced");
                assert!(hops[0].downgraded);
            }
            other => panic!("expected ranked alias, got {other:?}"),
        }
        for selector in ["gpt-5.6-sol", "precise:gpt-5.6-sol"] {
            match planner.resolve(selector, Default::default()).unwrap() {
                Resolution::Precise { hops, .. } => assert_eq!(
                    hops.iter()
                        .map(|hop| hop.model.as_str())
                        .collect::<Vec<_>>(),
                    ["gpt-5.6-sol"]
                ),
                other => panic!("an actual offered role spelling stays precise: {other:?}"),
            }
        }
    }

    fn no_hard() -> Request_features {
        Request_features::default()
    }

    #[test]
    fn precise_namespace_selects_same_model_offers_only() {
        let planner = planner(vec![
            offer("model-a", "ep1", 1, Some(2)),
            offer("model-a", "ep2", 2, Some(2)),
            offer("model-b", "ep3", 0, Some(3)),
        ]);
        match planner
            .resolve("precise:model-a", no_hard())
            .expect("resolves")
        {
            Resolution::Precise { model, hops } => {
                assert_eq!(model, "model-a");
                let chain: Vec<(String, String)> = hops
                    .iter()
                    .map(|hop| (hop.provider.clone(), hop.model.clone()))
                    .collect();
                assert_eq!(
                    chain,
                    vec![
                        ("prov-ep1".into(), "model-a".into()),
                        ("prov-ep2".into(), "model-a".into()),
                    ]
                );
            }
            other => panic!("expected Precise, got {:?}", other),
        }
    }

    #[test]
    fn precise_namespace_without_supply_returns_an_empty_chain() {
        let planner = planner(vec![offer("model-a", "ep1", 1, Some(2))]);
        match planner
            .resolve("precise:model-z", no_hard())
            .expect("resolves")
        {
            Resolution::Precise { model, hops } => {
                assert_eq!(model, "model-z");
                assert!(hops.is_empty(), "no offer of the literal model");
            }
            other => panic!("expected Precise, got {:?}", other),
        }
    }

    #[test]
    fn empty_precise_namespace_is_invalid() {
        let planner = planner(vec![]);
        let err = planner.resolve("precise:", no_hard()).expect_err("invalid");
        assert_eq!(err.code, "model_selector_invalid");
    }

    #[test]
    fn alias_namespace_plans_by_tier_with_traceable_downgrades() {
        let planner = planner(vec![
            offer("strong-model", "ep1", 5, Some(3)),
            offer("mid-model", "ep2", 1, Some(2)),
            offer("strong-two", "ep3", 9, Some(3)),
        ]);
        match planner.resolve("alias:fable", no_hard()).expect("resolves") {
            Resolution::Alias { alias, tier, hops } => {
                assert_eq!(alias, "fable");
                assert_eq!(tier, 3);
                let chain: Vec<(String, bool, &str)> = hops
                    .iter()
                    .map(|hop| (hop.model.clone(), hop.downgraded, hop.reason.as_str()))
                    .collect();
                assert_eq!(
                    chain,
                    vec![
                        ("strong-model".into(), false, "alias_tier_exact"),
                        ("strong-two".into(), false, "alias_tier_exact"),
                        ("mid-model".into(), true, "alias_tier_downgrade"),
                    ]
                );
            }
            other => panic!("expected Alias, got {:?}", other),
        }
    }

    #[test]
    fn alias_namespace_retains_above_tier_supply_as_labeled_fallback() {
        let planner = planner(vec![offer("only-opus", "ep1", 1, Some(2))]);
        match planner
            .resolve("alias:sonnet", no_hard())
            .expect("resolves")
        {
            Resolution::Alias { tier, hops, .. } => {
                assert_eq!(tier, 1);
                assert_eq!(hops.len(), 1);
                assert_eq!(hops[0].model, "only-opus");
                assert_eq!(hops[0].reason, "cross_tier_fallback");
                assert_eq!(hops[0].tier, Some(2));
            }
            other => panic!("expected Alias, got {:?}", other),
        }
    }

    #[test]
    fn alias_namespace_unknown_name_is_rejected() {
        let planner = planner(vec![]);
        let err = planner
            .resolve("alias:mythos", no_hard())
            .expect_err("unknown alias");
        assert_eq!(err.code, "model_alias_unknown");
    }

    #[test]
    fn bare_offered_name_is_an_exact_call_never_an_alias() {
        // `claude-opus-4` substring-matches the opus alias, but because a
        // supply of that literal model exists it must be served as an exact
        // call: the named model is never substituted.
        let planner = planner(vec![
            offer("claude-opus-4", "ep1", 1, Some(2)),
            offer("stronger-elsewhere", "ep2", 0, Some(3)),
        ]);
        match planner
            .resolve("claude-opus-4", no_hard())
            .expect("resolves")
        {
            Resolution::Precise { model, hops } => {
                assert_eq!(model, "claude-opus-4");
                assert_eq!(hops.len(), 1);
                assert_eq!(hops[0].model, "claude-opus-4");
            }
            other => panic!("expected Precise, got {:?}", other),
        }
    }

    #[test]
    fn bare_offered_name_matches_canonical_identity() {
        // Case and context-marker differences are the same identity.
        let planner = planner(vec![offer("Claude-Opus-4[1M]", "ep1", 1, Some(2))]);
        match planner
            .resolve("claude-opus-4", no_hard())
            .expect("resolves")
        {
            Resolution::Precise { hops, .. } => {
                assert_eq!(hops.len(), 1);
                assert_eq!(hops[0].model, "Claude-Opus-4[1M]");
            }
            other => panic!("expected Precise, got {:?}", other),
        }
    }

    #[test]
    fn bare_alias_name_not_offered_plans_by_tier() {
        let planner = planner(vec![
            offer("tier-two-model", "ep1", 1, Some(2)),
            offer("tier-zero-model", "ep2", 0, Some(0)),
        ]);
        match planner.resolve("opus", no_hard()).expect("resolves") {
            Resolution::Alias { alias, tier, hops } => {
                assert_eq!(alias, "opus");
                assert_eq!(tier, 2);
                assert_eq!(hops.len(), 2, "tier 2 exact + tier 0 downgrade");
                assert!(!hops[0].downgraded);
                assert!(hops[1].downgraded);
            }
            other => panic!("expected Alias, got {:?}", other),
        }
    }

    #[test]
    fn bare_unknown_name_stays_on_the_exact_path() {
        let planner = planner(vec![]);
        match planner
            .resolve("gpt-unknown-9000", no_hard())
            .expect("resolves")
        {
            Resolution::Exact(name) => assert_eq!(name, "gpt-unknown-9000"),
            other => panic!("expected Exact, got {:?}", other),
        }
    }

    #[test]
    fn alias_hard_requirements_reject_unknown_capabilities() {
        let mut uncertain = offer("uncertain-tools", "ep1", 1, Some(3));
        uncertain.supports_tools = None;
        let planner = planner(vec![uncertain]);
        let features = Request_features {
            needs_tools: true,
            ..Request_features::default()
        };
        match planner.resolve("alias:fable", features).expect("resolves") {
            Resolution::Alias { hops, .. } => {
                assert!(
                    hops.is_empty(),
                    "unknown tool support must not satisfy a tool-requiring alias"
                );
            }
            other => panic!("expected Alias, got {:?}", other),
        }
    }

    #[test]
    fn precise_hard_requirements_only_reject_proven_unfit_offers() {
        // A precise call pinned the model: an unknown capability is not a
        // reason to refuse the only supply of that literal model.
        let mut uncertain = offer("model-a", "ep1", 1, Some(2));
        uncertain.supports_tools = None;
        let planner = planner(vec![uncertain]);
        let features = Request_features {
            needs_tools: true,
            ..Request_features::default()
        };
        match planner
            .resolve("precise:model-a", features)
            .expect("resolves")
        {
            Resolution::Precise { hops, .. } => assert_eq!(hops.len(), 1),
            other => panic!("expected Precise, got {:?}", other),
        }
    }

    /// The advertised selectors on the model lists must agree with the tier
    /// mapping this planner actually applies — a listed alias that planned
    /// at a different rank would make the wire lie about routing.
    #[test]
    fn advertised_alias_selectors_match_the_services_tier_mapping() {
        for (name, rank) in crate::proxy::planner::CAPABILITY_ALIAS_SELECTORS {
            assert_eq!(
                capability_tier_for_alias(name),
                Some(*rank),
                "advertised selector {name} must map to its advertised rank"
            );
            match classify_selector(name) {
                Ok(Selector::Alias { tier, .. }) => assert_eq!(tier, *rank),
                other => panic!(
                    "advertised selector {name} must classify as an alias: {:?}",
                    other
                ),
            }
        }
    }

    /// A supplier that states nothing about a model's capabilities is still
    /// usable for an alias, because the public catalog states them.
    #[test]
    fn catalog_facts_qualify_an_offer_whose_supplier_stated_nothing() {
        let evidence = catalog_evidence();
        let offers = enrich_offers(
            vec![unknown_offer("anthropic/claude-fable-5.1", "ep1")],
            &evidence,
        );
        let offer = &offers[0];
        assert_eq!(offer.supports_tools, Some(true));
        assert_eq!(offer.supports_images, Some(true));
        assert_eq!(offer.context_window, Some(1_000_000));
        assert_eq!(offer.max_output_tokens, Some(128_000));

        // The vendor-prefixed id resolves to the same model the catalog
        // knows, and the alias now has a candidate.
        let mut planner = planner(offers);
        planner.evidence = evidence;
        match planner.resolve("alias:fable", no_hard()).expect("resolves") {
            Resolution::Alias { hops, .. } => {
                assert_eq!(hops.len(), 1);
                assert_eq!(hops[0].model, "anthropic/claude-fable-5.1");
                assert_eq!(hops[0].tier, Some(3));
                assert!(!hops[0].downgraded);
            }
            other => panic!("expected Alias, got {:?}", other),
        }
    }

    /// A supplier's own capability statement always wins over the catalog.
    #[test]
    fn supplier_facts_win_over_catalog_facts() {
        let evidence = catalog_evidence();
        let mut manual = unknown_offer("anthropic/claude-fable-5.1", "ep1");
        manual.supports_tools = Some(false);
        manual.context_window = Some(4_000);
        let offers = enrich_offers(vec![manual], &evidence);
        assert_eq!(offers[0].supports_tools, Some(false));
        assert_eq!(offers[0].context_window, Some(4_000));
        // The catalog still fills what the supplier left unknown.
        assert_eq!(offers[0].max_output_tokens, Some(128_000));
    }

    /// A model the catalog does not know stays unknown: the alias keeps
    /// failing closed instead of routing to an unassessed model.
    #[test]
    fn unknown_models_stay_unknown_and_never_enter_an_alias_plan() {
        let evidence = catalog_evidence();
        let offers = enrich_offers(vec![unknown_offer("mystery-x9", "ep1")], &evidence);
        assert_eq!(offers[0].supports_tools, None);
        assert_eq!(offers[0].context_window, None);
        let mut planner = planner(offers);
        planner.evidence = evidence;
        match planner.resolve("alias:fable", no_hard()).expect("resolves") {
            Resolution::Alias { hops, .. } => assert!(hops.is_empty()),
            other => panic!("expected Alias, got {:?}", other),
        }
    }

    /// A catalog model that provably cannot take tools is rejected for a
    /// tool-requiring alias instead of being tried and failing upstream.
    #[test]
    fn catalog_facts_reject_a_proven_unsuitable_model() {
        let evidence = catalog_evidence();
        let offers = enrich_offers(vec![unknown_offer("text-only-model", "ep1")], &evidence);
        assert_eq!(offers[0].supports_tools, Some(false));
        let mut planner = planner(offers);
        planner.evidence = evidence;
        let features = Request_features {
            needs_tools: true,
            ..Request_features::default()
        };
        match planner.resolve("alias:fable", features).expect("resolves") {
            Resolution::Alias { hops, .. } => assert!(hops.is_empty()),
            other => panic!("expected Alias, got {:?}", other),
        }
    }
}
