//! Public rating geometry and a read-only routing strategy preview. No model calls.
use super::{arena::{self, ArenaEntry, ArenaAssessment}, load_evidence, tiers::SourceStatus};
use crate::{database::{Database, dao::arena_visualization::Observed_router_model}, error::AppError,
    proxy::planner::{Model_planner, Request_features, Resolution}};
use serde::Serialize;
use std::collections::HashMap;

const MAX_SELECTOR_CHARS: usize = 256;

#[derive(Clone, Debug, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Arena_model_health {
    pub provider_state: Option<String>,
    pub samples: u64,
    pub success_rate: Option<f64>,
    pub latency_ms: Option<f64>,
    pub unavailable: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Arena_model_cost {
    pub input_per_million: Option<f64>,
    pub output_per_million: Option<f64>,
    pub source: String,
    pub stale: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Arena_flow_model {
    pub model: String,
    pub provider: String,
    pub protocol: Option<String>,
    pub preferred: bool,
    pub pinned: bool,
    pub health: Arena_model_health,
    pub cost: Option<Arena_model_cost>,
    pub tier: Option<u8>,
    pub downgraded: bool,
    pub basis: String,
    pub reason: String,
    pub utility: Option<super::utility::Utility_score>,
    pub arena: Option<ArenaAssessment>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Arena_snapshot {
    pub source: SourceStatus,
    pub source_url: String,
    pub published_date: Option<String>,
    pub category: String,
    pub entries: Vec<ArenaEntry>,
    pub selected_selector: Option<String>,
    pub selected: Option<ArenaAssessment>,
    pub observed: Option<Observed_router_model>,
    pub candidates: Vec<Arena_flow_model>,
    pub fable_candidates: Vec<Arena_flow_model>,
    /// Read-only failover-queue health projection from the admitted runtime
    /// (configured queue ids, explicit source association, resolved flag,
    /// derived failure view). `None` until a runtime preview is applied: no
    /// runtime means unmeasured health, never measured zeros.
    pub queue_health: Option<crate::proxy::router::Queue_health_projection>,
    pub plan_error: Option<String>,
    pub configuration_generation: u64,
    pub plan_source: String,
    // Internal metadata joins for explicit runtime routes that were absent
    // from saved alias planning. Never serialize private provider records.
    #[serde(skip)]
    model_costs: HashMap<String, Arena_model_cost>,
    #[serde(skip)]
    provider_protocols: HashMap<String, String>,
}

pub(crate) fn snapshot(db: &Database, selector: Option<String>, needs_tools: bool, needs_images: bool) -> Result<Arena_snapshot, AppError> {
    if selector.as_ref().is_some_and(|value| value.trim().is_empty() || value.chars().count() > MAX_SELECTOR_CHARS || value.chars().any(char::is_control)) {
        return Err(AppError::InvalidInput("arena_selector_invalid".into()));
    }
    let mut offers = super::offered_models(db)?;
    let evidence = load_evidence(db).with_offered_models(offers.iter().map(|offer| offer.model_id.clone()).collect());
    super::enrich_capabilities(&mut offers, &evidence);
    let observed = db.last_observed_router_model()?;
    let selected_selector = selector.or(db.get_setting(crate::services::supplier_setup::KEY_UNIFIED_DEFAULT_MODEL)?)
        .or_else(|| observed.as_ref().map(|item| item.model.clone()));
    let rows = db.get_all_proxy_providers()?;
    let provider_protocols: HashMap<_, _> = rows.iter().map(|row| (row.id.clone(), row.provider_type.clone())).collect();
    // Exact model identifiers only: a nearby model/version is not pricing evidence.
    let model_costs: HashMap<_, _> = evidence.models_dev.as_ref().into_iter().flat_map(|catalog| &catalog.models).map(|model| (
        model.id.clone(), Arena_model_cost {
            input_per_million: model.cost_input_per_mtok,
            output_per_million: model.cost_output_per_mtok,
            source: super::SOURCE_MODELS_DEV.into(), stale: evidence.models_dev_status.stale,
        }
    )).collect();
    let planner = crate::services::proxy_runtime::planner::build_model_planner(db, &rows)?;
    let features = Request_features { needs_tools, needs_images, ..Default::default() };
    let resolve = |selector: &str| -> Result<Vec<Arena_flow_model>, String> {
        let resolution = planner.resolve(selector, features).map_err(|_| "arena_plan_unavailable".to_string())?;
        let (hops, alias_tier) = match resolution {
            Resolution::Alias { hops, tier, .. } => (hops, Some(tier)),
            Resolution::Precise { hops, .. } => (hops, None),
            Resolution::Exact(_) => (Vec::new(), None),
        };
        Ok(hops.into_iter().enumerate().map(|(index, hop)| Arena_flow_model {
            arena: evidence.arena.as_ref().and_then(|catalog| arena::assessment_for(&hop.model, catalog)),
            provider: crate::proxy::routing_receipts::contract::safe_reference(&hop.provider).unwrap_or_else(|| "unreported".into()),
            protocol: provider_protocols.get(&hop.provider).cloned(),
            preferred: index == 0, pinned: false, health: Arena_model_health::default(),
            cost: model_costs.get(&hop.model).cloned(),
            utility: alias_tier.and_then(|tier| {
                let identity = planner.routing_identity(&hop.provider)?;
                let offer = offers.iter().find(|offer| Some(offer.endpoint_ref.as_str()) == identity.endpoint_ref.as_deref() && offer.model_id == hop.model)?;
                Some(super::utility::score(offer, tier, &evidence, needs_tools))
            }),
            reason: hop.reason,
            model: hop.model, tier: hop.tier, downgraded: hop.downgraded, basis: hop.basis,
        }).collect())
    };
    let mut plan_error = None;
    let candidates = match selected_selector.as_deref().map(resolve) {
        Some(Ok(models)) => models,
        Some(Err(code)) => { plan_error = Some(code); Vec::new() },
        None => Vec::new(),
    };
    let selected_model = candidates.first().map(|item| item.model.as_str()).or(selected_selector.as_deref());
    let selected = selected_model.and_then(|model| evidence.arena.as_ref().and_then(|catalog| arena::assessment_for(model.trim_start_matches("precise:"), catalog)));
    let fable_candidates = resolve("alias:fable").unwrap_or_default();
    Ok(Arena_snapshot {
        source: evidence.arena_status,
        source_url: arena::SOURCE_URL.into(),
        published_date: evidence.arena.as_ref().map(|catalog| catalog.published_date.clone()),
        category: arena::CATEGORY.into(),
        entries: evidence.arena.map(|catalog| catalog.entries).unwrap_or_default(),
        selected_selector, selected, observed, candidates, fable_candidates, plan_error,
        queue_health: None,
        configuration_generation: db.supplier_config_generation()?,
        plan_source: "saved_configuration".into(),
        model_costs, provider_protocols,
    })
}

pub(crate) fn apply_runtime_preview(view: &mut Arena_snapshot, router: &crate::proxy::router::Router,
    pin: Option<&str>, surface: &str, features: Request_features) {
    let catalog = arena::ArenaCatalog { entries: view.entries.clone(), published_date: view.published_date.clone().unwrap_or_default(), category: view.category.clone(), source_id: view.source.source.clone() };
    let prior: Vec<_> = view.candidates.iter().chain(view.fable_candidates.iter()).cloned().collect();
    let provider_health = router.health();
    let resolve = |selector: &str| router.preview_model(surface, selector, features, pin).map(|hops| {
        hops.iter().enumerate().map(|(index, hop)| {
            let assessment = arena::assessment_for(&hop.model, &catalog);
            let model_health = router.model_health(&hop.provider, &hop.model);
            let provider = crate::proxy::routing_receipts::contract::safe_reference(&hop.provider).unwrap_or_else(|| "unreported".into());
            let previous = prior.iter().find(|item| item.model == hop.model && item.provider == provider);
            Arena_flow_model { model: hop.model.clone(), provider,
                protocol: view.provider_protocols.get(&hop.provider).cloned(), preferred: index == 0,
                pinned: pin == Some(hop.provider.as_str()),
                health: Arena_model_health {
                    provider_state: provider_health.iter().find(|item| item.id == hop.provider).map(|item| item.breaker.state.to_string()),
                    samples: model_health.samples, success_rate: model_health.success_rate,
                    latency_ms: model_health.latency_ms, unavailable: model_health.unavailable,
                },
                cost: view.model_costs.get(&hop.model).cloned(),
                utility: previous.and_then(|item| item.utility.clone()),
                reason: previous.map(|item| item.reason.clone()).unwrap_or_else(|| "explicit_runtime_route".into()),
                tier: previous.and_then(|item| item.tier),
                downgraded: previous.is_some_and(|item| item.downgraded), basis: "admitted_runtime".into(), arena: assessment }
        }).collect::<Vec<_>>()
    });
    if let Some(selector) = view.selected_selector.as_deref() {
        match resolve(selector) {
            Ok(candidates) => { view.candidates = candidates; view.plan_error = view.candidates.is_empty().then(|| "arena_plan_unavailable".into()); },
            Err(_) => { view.candidates.clear(); view.plan_error = Some("arena_plan_unavailable".into()); }
        }
    }
    view.fable_candidates = resolve("alias:fable").unwrap_or_default();
    view.queue_health = Some(router.queue_health_projection());
    // Reuse the evidence matcher (including version/condition rules), not a
    // fuzzy rating guess, when the runtime primary differs from the saved one.
    view.selected = view.candidates.first().and_then(|primary| primary.arena.clone());
    view.plan_source = "admitted_runtime".into();
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn empty_cache_is_visible_and_read_does_not_change_configuration() {
        let db = Database::memory().unwrap();
        let generation = db.supplier_config_generation().unwrap();
        let view = snapshot(&db, None, false, false).unwrap();
        assert!(view.entries.is_empty());
        assert!(view.source.stale);
        assert!(view.selected.is_none());
        assert_eq!(db.supplier_config_generation().unwrap(), generation);
    }
    #[test]
    fn invalid_selector_is_rejected_without_reading_source_data() {
        let db = Database::memory().unwrap();
        assert!(snapshot(&db, Some("\n".into()), false, false).is_err());
    }

    #[test]
    fn runtime_graph_keeps_every_provider_model_hop_in_dispatch_order() {
        use crate::database::dao::{routes::RouteHop, proxy_providers::CircuitBreakerConfig};
        use crate::proxy::{providers::{Provider, mock::MockProvider}, router::Router};
        use std::{collections::HashMap, sync::Arc};
        let db = Database::memory().unwrap();
        let hops: Vec<_> = (0..40).map(|index| RouteHop {
            provider: format!("provider_{index}"),
            model: if index < 2 { "shared_model".into() } else { format!("unrated_model_{index}") },
        }).collect();
        let providers: HashMap<String, Arc<dyn Provider>> = hops.iter().map(|hop| (
            hop.provider.clone(), Arc::new(MockProvider::new(hop.provider.clone(), vec![hop.model.clone()], true, CircuitBreakerConfig::default())) as Arc<dyn Provider>
        )).collect();
        let router = Router::new(providers, HashMap::from([("fixture_route".into(), hops.clone())]));
        let mut view = snapshot(&db, Some("fixture_route".into()), false, false).unwrap();
        apply_runtime_preview(&mut view, &router, None, "chat", Request_features::default());
        assert_eq!(view.candidates.len(), 40, "neither model deduplication nor a display cap may alter the runtime queue");
        assert_eq!(view.candidates.iter().map(|candidate| candidate.model.as_str()).collect::<Vec<_>>(), hops.iter().map(|hop| hop.model.as_str()).collect::<Vec<_>>());
        assert_eq!(view.candidates.iter().map(|candidate| candidate.provider.as_str()).collect::<Vec<_>>(), hops.iter().map(|hop| hop.provider.as_str()).collect::<Vec<_>>());
        assert!(view.candidates.iter().all(|candidate| candidate.arena.is_none()));
        assert!(view.candidates[0].preferred);
        assert!(view.candidates.iter().skip(1).all(|candidate| !candidate.preferred));
        assert_eq!(view.candidates[0].health.samples, 0);
        assert_eq!(view.candidates[0].health.success_rate, None);
        assert!(view.candidates[0].cost.is_none());
        apply_runtime_preview(&mut view, &router, Some("provider_1"), "chat", Request_features::default());
        assert_eq!(view.candidates.len(), 1);
        assert_eq!(view.candidates[0].provider, "provider_1");
        assert!(view.candidates[0].preferred && view.candidates[0].pinned);
        let serialized = serde_json::to_value(view).unwrap();
        assert!(serialized.get("modelCosts").is_none());
        assert!(serialized.get("providerProtocols").is_none());
    }

    /// Without a runtime the queue health projection is absent (unmeasured),
    /// never measured zeros; with one it is a read-only pure projection.
    #[test]
    fn queue_health_projection_is_absent_without_runtime_and_read_only_with_it() {
        use crate::database::dao::routes::RouteHop;
        use crate::proxy::providers::{mock::MockProvider, Provider};
        use crate::proxy::router::Router;
        use std::{collections::HashMap, sync::Arc};
        let db = Database::memory().unwrap();
        let providers: HashMap<String, Arc<dyn Provider>> = HashMap::from([(
            "provider_0".into(),
            Arc::new(MockProvider::new(
                "provider_0".into(),
                vec!["fixture_route".into()],
                true,
                crate::database::dao::proxy_providers::CircuitBreakerConfig::default(),
            )) as Arc<dyn Provider>,
        )]);
        let router = Router::new(
            providers,
            HashMap::from([(
                "fixture_route".into(),
                vec![RouteHop {
                    provider: "provider_0".into(),
                    model: "fixture_route".into(),
                }],
            )]),
        )
        .with_queue(vec!["provider_0".to_string()]);
        let mut view = snapshot(&db, Some("fixture_route".into()), false, false).unwrap();
        assert!(
            view.queue_health.is_none(),
            "no runtime means unmeasured health, never measured zeros"
        );
        assert!(serde_json::to_value(&view).unwrap()["queueHealth"].is_null());
        apply_runtime_preview(&mut view, &router, None, "chat", Request_features::default());
        let projection = view.queue_health.as_ref().expect("runtime projection applied");
        assert_eq!(projection.totals.total, 1);
        assert_eq!(projection.totals.unobserved, 1);
        assert!(projection.candidates[0].resolved);
        // The projection is a pure read: applying the preview again changes
        // nothing.
        let before = serde_json::to_value(&view.queue_health).unwrap();
        apply_runtime_preview(&mut view, &router, None, "chat", Request_features::default());
        assert_eq!(serde_json::to_value(&view.queue_health).unwrap(), before);
    }
}
