//! Hard-capability filtering and deterministic candidate planning.
//!
//! Pure logic, no I/O; the router (integration layer) feeds these functions
//! the offers actually available on supplier endpoints plus the public
//! evidence bundle. Contract:
//!
//! * precise selectors plan **only** the same canonical model — never an
//!   automatic model switch; unknown capabilities pass through (the request
//!   itself pins the model), proven-unsatisfying capabilities reject,
//! * capability aliases require assessed models; tier and reference-price
//!   fit influence preference without deleting cross-tier fallback supply,
//! * hard requirements (tools, images, context, output) are never silently
//!   dropped; for alias selection an unknown capability fails closed,
//! * utility orders every eligible model, then configured priority and stable
//!   identities break ties; fallback supply is never capped per supplier.
//!
//! Nothing here treats price or context size as intelligence, and the input
//! offers are the only things that may be selected — public catalog entries
//! are never turned into offers.

use super::identity::canonical_model_id;
use super::tiers::{evaluate_model, CatalogEvidence, Selector, TierBasis};
use crate::services::model_discovery::rank::{
    family_key, is_low_tier_token, normalize_id, remainder_exact, tokenize,
    version_cmp_normalized,
};
use std::cmp::Ordering;
use std::collections::HashMap;

/// Setting key overriding how many same-family generations an alias queue
/// keeps. Defaults to [`DEFAULT_GENERATION_KEEP_N`]; values are clamped to
/// 1..=4. Older generations of a family are dominated by a newer offered one.
pub(crate) const GENERATION_KEEP_N_SETTING: &str = "family_generation_keep_n";
pub(crate) const DEFAULT_GENERATION_KEEP_N: usize = 1;

/// Parse the stored setting; invalid or missing values fall back to the
/// default and the result is always clamped to a sane window.
pub(crate) fn parse_generation_keep_n(raw: Option<&str>) -> usize {
    raw.and_then(|raw| raw.parse::<usize>().ok())
        .unwrap_or(DEFAULT_GENERATION_KEEP_N)
        .clamp(1, 4)
}

/// One model actually offered by an enabled, configured supplier endpoint.
/// Backend-internal; no serde. The refs are the stable supplier/endpoint
/// ids from the runtime snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OfferedModel {
    pub model_id: String,
    pub endpoint_ref: String,
    pub supplier_ref: String,
    pub protocol: String,
    pub priority: i64,
    pub supports_tools: Option<bool>,
    pub supports_images: Option<bool>,
    pub context_window: Option<u64>,
    pub max_output_tokens: Option<u64>,
    pub tier_override: Option<u8>,
}

/// Hard request requirements derived from the incoming request semantics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct HardRequirements {
    pub needs_tools: bool,
    pub needs_images: bool,
    pub min_context_window: Option<u64>,
    pub min_max_output_tokens: Option<u64>,
}

/// Why one offer failed the hard requirements; stable codes only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HardReject {
    MissingTools,
    UnknownTools,
    MissingImages,
    UnknownImages,
    ContextTooSmall,
    ContextUnknown,
    OutputTooSmall,
    OutputUnknown,
}

impl HardReject {
    pub(crate) fn code(self) -> &'static str {
        match self {
            HardReject::MissingTools => "missing_tools",
            HardReject::UnknownTools => "unknown_tools",
            HardReject::MissingImages => "missing_images",
            HardReject::UnknownImages => "unknown_images",
            HardReject::ContextTooSmall => "context_too_small",
            HardReject::ContextUnknown => "context_unknown",
            HardReject::OutputTooSmall => "output_too_small",
            HardReject::OutputUnknown => "output_unknown",
        }
    }
}

/// Check one offer against the hard requirements. `strict_unknown` (alias
/// selection) rejects unknown capabilities; precise selection only rejects
/// proven-unsatisfying ones because the request itself pinned the model.
pub(crate) fn check_hard_capabilities(
    offered: &OfferedModel,
    requirements: &HardRequirements,
    strict_unknown: bool,
) -> Result<(), HardReject> {
    if requirements.needs_tools {
        match offered.supports_tools {
            Some(false) => return Err(HardReject::MissingTools),
            None if strict_unknown => return Err(HardReject::UnknownTools),
            _ => {}
        }
    }
    if requirements.needs_images {
        match offered.supports_images {
            Some(false) => return Err(HardReject::MissingImages),
            None if strict_unknown => return Err(HardReject::UnknownImages),
            _ => {}
        }
    }
    if let Some(minimum) = requirements.min_context_window {
        match offered.context_window {
            Some(window) if window < minimum => return Err(HardReject::ContextTooSmall),
            None if strict_unknown => return Err(HardReject::ContextUnknown),
            _ => {}
        }
    }
    if let Some(minimum) = requirements.min_max_output_tokens {
        match offered.max_output_tokens {
            Some(limit) if limit < minimum => return Err(HardReject::OutputTooSmall),
            None if strict_unknown => return Err(HardReject::OutputUnknown),
            _ => {}
        }
    }
    Ok(())
}

/// One planned selection, ordered for the router's failover chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Selection {
    pub model_id: String,
    pub endpoint_ref: String,
    pub supplier_ref: String,
    pub tier: Option<u8>,
    pub basis: TierBasis,
    /// True when a tiered alias resolved below its target tier; the router
    /// must record this as a traceable downgrade.
    pub downgraded: bool,
    pub reason: &'static str,
}

fn offer_tier(offered: &OfferedModel, evidence: &CatalogEvidence) -> (Option<u8>, TierBasis) {
    let assessment = evaluate_model(&offered.model_id, offered.tier_override, evidence);
    (assessment.tier, assessment.basis)
}

/// Plan the failover chain for a precise model request: only offers of the
/// same canonical model qualify, so a precise call can never be silently
/// switched to a different model.
pub(crate) fn plan_precise(
    model_id: &str,
    offered: &[OfferedModel],
    requirements: &HardRequirements,
    evidence: &CatalogEvidence,
) -> Vec<Selection> {
    let wanted = canonical_model_id(model_id);
    let mut eligible: Vec<&OfferedModel> = offered
        .iter()
        .filter(|offer| canonical_model_id(&offer.model_id) == wanted)
        .filter(|offer| check_hard_capabilities(offer, requirements, false).is_ok())
        .collect();
    sort_by_priority(&mut eligible);
    eligible
        .into_iter()
        .map(|offer| {
            let (tier, basis) = offer_tier(offer, evidence);
            Selection {
                model_id: offer.model_id.clone(),
                endpoint_ref: offer.endpoint_ref.clone(),
                supplier_ref: offer.supplier_ref.clone(),
                tier,
                basis,
                downgraded: false,
                reason: "precise_same_model",
            }
        })
        .collect()
}

fn sort_by_priority(eligible: &mut [&OfferedModel]) {
    eligible.sort_by(|left, right| {
        left.priority
            .cmp(&right.priority)
            .then_with(|| left.supplier_ref.cmp(&right.supplier_ref))
            .then_with(|| left.endpoint_ref.cmp(&right.endpoint_ref))
            .then_with(|| left.model_id.cmp(&right.model_id))
    });
}

/// Plan an alias from all assessed, hard-capable offers. Tier fit and known
/// affordability set preferred bands, then utility ranks within each band.
/// Unknown prices never imply affordability or unavailability. Unassessed
/// models still fail closed until the admission owner supplies their evidence.
pub(crate) fn plan_alias(
    alias_tier: u8,
    offered: &[OfferedModel],
    requirements: &HardRequirements,
    evidence: &CatalogEvidence,
) -> Vec<Selection> {
    struct Ranked_offer<'a> {
        offer: &'a OfferedModel,
        tier: u8,
        basis: TierBasis,
        preference_band: u8,
        utility: f64,
        affordable_reuse: bool,
    }
    let mut eligible: Vec<Ranked_offer<'_>> = offered
        .iter()
        .filter(|offer| check_hard_capabilities(offer, requirements, true).is_ok())
        .filter_map(|offer| {
            let (tier, basis) = offer_tier(offer, evidence);
            let tier = tier?;
            let utility =
                super::utility::score(offer, alias_tier, evidence, requirements.needs_tools);
            let affordable_reuse = tier > alias_tier && utility.reference_cost.is_some_and(|cost| {
                cost <= evidence.utility_policy.fast_reference_cost_ceiling * f64::from(alias_tier + 1)
            });
            let preference_band = if tier == alias_tier { 0 }
                else if affordable_reuse { 1 }
                else if tier < alias_tier { 2 }
                else { 3 };
            Some(Ranked_offer { offer, tier, basis, preference_band, utility: utility.total, affordable_reuse })
        })
        .collect();
    eligible.sort_by(|left, right| {
        left.preference_band.cmp(&right.preference_band)
            // Inside one band, evidence-assessed supply precedes name-
            // inferred supply; manual overrides rank strongest of all.
            .then_with(|| left.basis.prior_rank().cmp(&right.basis.prior_rank()))
            .then_with(|| right.utility.total_cmp(&left.utility))
            .then_with(|| left.offer.priority.cmp(&right.offer.priority))
            .then_with(|| left.offer.supplier_ref.cmp(&right.offer.supplier_ref))
            .then_with(|| left.offer.endpoint_ref.cmp(&right.offer.endpoint_ref))
            .then_with(|| left.offer.model_id.cmp(&right.offer.model_id))
    });
    let mut seen = std::collections::HashSet::new();
    let planned = eligible
        .into_iter()
        .filter(|ranked| seen.insert((ranked.offer.endpoint_ref.clone(), ranked.offer.model_id.clone())))
        .map(|Ranked_offer { tier, basis, offer, affordable_reuse, .. }| Selection {
            model_id: offer.model_id.clone(),
            endpoint_ref: offer.endpoint_ref.clone(),
            supplier_ref: offer.supplier_ref.clone(),
            tier: Some(tier),
            basis,
            downgraded: tier < alias_tier,
            reason: if affordable_reuse {
                "alias_affordable_quality_reuse"
            } else if tier > alias_tier {
                "cross_tier_fallback"
            } else if tier == alias_tier {
                "alias_tier_exact"
            } else {
                "alias_tier_downgrade"
            },
        })
        .collect::<Vec<_>>();
    keep_newest_generations(planned, evidence.generation_keep_n)
}

/// Alias queues serve each family from its newest generations only. Distinct
/// suppliers offering the same generation all stay (parallel failover
/// supply); strictly older generations of the same family lane are dominated
/// by a newer one and trimmed once `keep_n` newest generations survive.
/// Cheap/experimental variants (`:free`-style tokens such as `flash`/`lite`)
/// form their own lane, so a base id never evicts the family's fast variant,
/// and incomparable siblings are never trimmed — ranking must prove a
/// generation older before it can drop.
fn keep_newest_generations(selections: Vec<Selection>, keep_n: usize) -> Vec<Selection> {
    if keep_n == 0 || selections.len() <= 1 {
        return selections;
    }
    // family -> lane -> distinct generation representatives, first-seen order.
    let mut families: HashMap<(String, bool), Vec<String>> = HashMap::new();
    for selection in &selections {
        let norm = normalize_id(&selection.model_id);
        let key = (family_key(&norm), generation_lane(&norm));
        let representatives = families.entry(key).or_default();
        if !representatives
            .iter()
            .any(|representative| remainder_exact(representative, &norm))
        {
            representatives.push(norm);
        }
    }
    let mut kept: HashMap<(String, bool), Vec<String>> = HashMap::with_capacity(families.len());
    for (key, representatives) in families {
        let survivors: Vec<String> = representatives
            .iter()
            .filter(|candidate| {
                representatives
                    .iter()
                    .filter(|other| {
                        other != candidate && ranks_below_lanes_equal(candidate, other)
                    })
                    .count() < keep_n
            })
            .cloned()
            .collect();
        kept.insert(key, survivors);
    }
    selections
        .into_iter()
        .filter(|selection| {
            let norm = normalize_id(&selection.model_id);
            kept.get(&(family_key(&norm), generation_lane(&norm)))
                .is_some_and(|survivors| {
                    survivors.iter().any(|representative| {
                        remainder_exact(representative, &norm)
                    })
                })
        })
        .collect()
}

/// Cheap/experimental variant lane flag: ids carrying a low-tier token the
/// plain generation ranking must not cross.
fn generation_lane(normalized: &str) -> bool {
    tokenize(normalized).iter().any(|t| is_low_tier_token(t))
}

/// Decisive "candidate is older than other" within one lane; incomparable
/// pairs (Equal) and cross-lane pairs never count.
fn ranks_below_lanes_equal(candidate: &str, other: &str) -> bool {
    generation_lane(candidate) == generation_lane(other)
        && version_cmp_normalized(candidate, other) == Ordering::Less
}

/// Dispatch on a classified selector; the router records an empty
/// alias chain as an explicit error instead of falling through to a raw
/// blind send.
pub(crate) fn plan_selector(
    selector: &Selector,
    offered: &[OfferedModel],
    requirements: &HardRequirements,
    evidence: &CatalogEvidence,
) -> Vec<Selection> {
    match selector {
        Selector::Precise { model_id } => plan_precise(model_id, offered, requirements, evidence),
        Selector::Alias { tier, .. } => plan_alias(*tier, offered, requirements, evidence),
    }
}

#[cfg(test)]
mod tests {
    use super::super::tiers::{SourceStatus, TIER_BALANCED, TIER_FABLE, TIER_FAST, TIER_OPUS};
    use super::*;

    #[test]
    fn competitive_new_model_wins_and_affordable_quality_can_serve_fast_role() {
        let mut evidence = empty_evidence();
        let catalog = super::super::parse::parse_models_dev(
            r#"{"fixture":{"models":{
          "old":{"id":"old","cost":{"input":2,"output":8},"release_date":"2024-01-01"},
          "new":{"id":"new","cost":{"input":0.1,"output":0.2},"release_date":"2026-10-01"}
        }}}"#,
        )
        .unwrap();
        evidence.models_index = super::super::tiers::ModelsDevIndex::build(&catalog);
        evidence.models_dev = Some(catalog);
        let mut old = offer("old", "old_endpoint", 1);
        let mut new = offer("new", "new_endpoint", 1);
        old.tier_override = Some(TIER_OPUS);
        new.tier_override = Some(TIER_OPUS);
        let offers = vec![old, new];
        let premium = plan_alias(TIER_FABLE, &offers, &HardRequirements::default(), &evidence);
        assert_eq!(premium[0].model_id, "new");
        let fast = plan_alias(TIER_FAST, &offers, &HardRequirements::default(), &evidence);
        assert_eq!(
            fast.iter()
                .map(|hop| hop.model_id.as_str())
                .collect::<Vec<_>>(),
            vec!["new", "old"]
        );
        assert_eq!(fast[0].reason, "alias_affordable_quality_reuse");
        assert_eq!(fast[1].reason, "cross_tier_fallback");
    }

    #[test]
    fn allocation_retains_overflow_models_and_deduplicates_endpoint_model_pairs() {
        let mut evidence = empty_evidence();
        evidence.utility_policy.models_per_supplier = 1;
        let mut offers = Vec::new();
        for id in ["a", "b", "c"] {
            let mut row = offer(id, "shared", 1);
            row.tier_override = Some(TIER_OPUS);
            offers.push(row.clone());
            offers.push(row);
        }
        let planned = plan_alias(TIER_OPUS, &offers, &HardRequirements::default(), &evidence);
        assert_eq!(planned.len(), 3, "allocation preferences must never truncate fallback supply");
        assert_eq!(planned[0].model_id, "a");
        assert_eq!(planned[1].model_id, "b");
        assert_eq!(planned[2].model_id, "c");
    }

    #[test]
    fn unpriced_variant_remains_fallback_without_borrowing_a_base_price() {
        let mut evidence = empty_evidence();
        evidence.models_dev = Some(
            super::super::parse::parse_models_dev(
                r#"{"p":{"models":{"base":{"cost":{"input":0.1,"output":0.1}}}}}"#,
            )
            .unwrap(),
        );
        evidence.models_index = super::super::tiers::ModelsDevIndex::build(evidence.models_dev.as_ref().unwrap());
        let mut variant = offer("base:extended", "e", 1);
        variant.tier_override = Some(TIER_OPUS);
        let utility = super::super::utility::score(&variant, TIER_FAST, &evidence, false);
        assert!(utility.reference_cost.is_none());
        let planned = plan_alias(
            TIER_FAST,
            &[variant],
            &HardRequirements::default(),
            &evidence
        );
        assert_eq!(planned.len(), 1);
        assert_eq!(planned[0].model_id, "base:extended");
        assert_eq!(planned[0].reason, "cross_tier_fallback");
    }

    #[test]
    fn requested_tier_stays_preferred_while_higher_tier_fallbacks_keep_hard_limits() {
        let evidence = empty_evidence();
        let mut preferred = offer("fast", "preferred", 100);
        preferred.tier_override = Some(TIER_FAST);
        let mut capable = offer("premium", "fallback", 1);
        capable.tier_override = Some(TIER_FABLE);
        let mut too_small = offer("small-premium", "small", 0);
        too_small.tier_override = Some(TIER_FABLE);
        too_small.context_window = Some(4096);
        let mut missing_tools = offer("no-tools-premium", "no-tools", 0);
        missing_tools.tier_override = Some(TIER_FABLE);
        missing_tools.supports_tools = Some(false);
        let planned = plan_alias(TIER_FAST, &[capable, too_small, missing_tools, preferred], &HardRequirements {
            needs_tools: true, min_context_window: Some(50_000), ..Default::default()
        }, &evidence);
        assert_eq!(planned.iter().map(|candidate| (candidate.model_id.as_str(), candidate.reason)).collect::<Vec<_>>(), vec![("fast", "alias_tier_exact"), ("premium", "cross_tier_fallback")]);
    }

    fn offer(id: &str, endpoint: &str, priority: i64) -> OfferedModel {
        OfferedModel {
            model_id: id.into(),
            endpoint_ref: endpoint.into(),
            supplier_ref: format!("supplier-{endpoint}"),
            protocol: "openai-chat".into(),
            priority,
            supports_tools: Some(true),
            supports_images: Some(false),
            context_window: Some(200_000),
            max_output_tokens: Some(8_192),
            tier_override: None,
        }
    }

    fn empty_evidence() -> CatalogEvidence {
        CatalogEvidence::new(
            None,
            SourceStatus {
                source: "models_dev".into(),
                fetched_at: None,
                last_error: None,
                stale: true,
            },
            None,
            SourceStatus {
                source: "evalplus".into(),
                fetched_at: None,
                last_error: None,
                stale: true,
            },
        )
    }

    #[test]
    fn precise_planning_never_switches_models() {
        let evidence = empty_evidence();
        let offered = vec![
            offer("minimax-m3", "e1", 10),
            offer("minimax-m3.1", "e1", 1),
            offer("MiniMax-M3", "e2", 5),
        ];
        let planned = plan_precise(
            "minimax-m3",
            &offered,
            &HardRequirements::default(),
            &evidence,
        );
        assert_eq!(planned.len(), 2);
        assert!(planned.iter().all(|s| s.reason == "precise_same_model"));
        // Canonical equality groups spellings; the newer model is never
        // substituted even with a better priority.
        assert!(planned
            .iter()
            .all(|s| canonical_model_id(&s.model_id) == "minimax-m3"));
        // Priority decides failover order.
        assert_eq!(planned[0].endpoint_ref, "e2");
        assert_eq!(planned[1].endpoint_ref, "e1");
    }

    #[test]
    fn alias_planning_prefers_tier_fit_and_retains_higher_tier_fallbacks() {
        let evidence = empty_evidence();
        let mut fast = offer("claude-haiku-4-5", "e1", 1);
        let mut balanced = offer("claude-sonnet-4-5", "e2", 2);
        let mut opus = offer("claude-opus-4-8", "e3", 3);
        let mut fable = offer("claude-fable-5", "e4", 4);
        for entry in [&mut fast, &mut balanced, &mut opus, &mut fable] {
            entry.tier_override = None;
        }
        let offered = vec![fast.clone(), balanced.clone(), opus.clone(), fable.clone()];
        let planned = plan_alias(TIER_OPUS, &offered, &HardRequirements::default(), &evidence);
        assert_eq!(planned.len(), 4);
        assert_eq!(planned[0].model_id, "claude-opus-4-8");
        assert!(!planned[0].downgraded);
        assert_eq!(planned[0].reason, "alias_tier_exact");
        assert_eq!(planned[1].model_id, "claude-sonnet-4-5");
        assert!(planned[1].downgraded);
        assert_eq!(planned[1].reason, "alias_tier_downgrade");
        assert_eq!(planned[2].model_id, "claude-haiku-4-5");
        assert!(planned[2].downgraded);
        assert_eq!(planned[3].model_id, "claude-fable-5");
        assert_eq!(planned[3].reason, "cross_tier_fallback");
        assert!(!planned[3].downgraded);
    }

    #[test]
    fn alias_planning_fails_closed_on_unknown_models_instead_of_blind_sending() {
        let evidence = empty_evidence();
        // Neither id carries evidence or a documented prior: the alias chain
        // is explicitly empty so the router can raise a clear error.
        let offered = vec![offer("mystery-x9", "e1", 1), offer("claude-mythos-7", "e2", 2)];
        assert!(plan_alias(
            TIER_FABLE,
            &offered,
            &HardRequirements::default(),
            &evidence
        )
        .is_empty());
        // The same unknown models remain reachable through precise calls.
        assert_eq!(
            plan_precise(
                "mystery-x9",
                &offered,
                &HardRequirements::default(),
                &evidence
            )
            .len(),
            1
        );
    }

    #[test]
    fn inferred_tiers_join_alias_queues_after_evidence_assessed_supply() {
        let evidence = empty_evidence();
        // glm-5.3 carries a vendor flagship prior (inferred opus); the
        // tier-override offer is evidence-class and must lead the same band.
        let mut inferred = offer("glm-5.3", "e_inferred", 1);
        let mut assessed = offer("big-evidence", "e_assessed", 2);
        inferred.tier_override = None;
        assessed.tier_override = Some(TIER_OPUS);
        let planned = plan_alias(
            TIER_OPUS,
            &[inferred, assessed],
            &HardRequirements::default(),
            &evidence,
        );
        assert_eq!(
            planned
                .iter()
                .map(|hop| hop.model_id.as_str())
                .collect::<Vec<_>>(),
            vec!["big-evidence", "glm-5.3"]
        );
    }

    #[test]
    fn hard_requirements_are_never_silently_dropped() {
        let evidence = empty_evidence();
        let mut no_tools = offer("claude-opus-4-8", "e1", 1);
        no_tools.supports_tools = Some(false);
        let mut unknown_tools = offer("claude-opus-4-8", "e2", 2);
        unknown_tools.supports_tools = None;

        let needs_tools = HardRequirements {
            needs_tools: true,
            ..HardRequirements::default()
        };
        // Alias selection fails closed on unknown tool support and rejects
        // proven-unsatisfying offers: nothing qualifies.
        let alias_offers = vec![no_tools.clone(), unknown_tools.clone()];
        assert!(plan_alias(TIER_OPUS, &alias_offers, &needs_tools, &evidence).is_empty());

        // Precise selection still rejects proven-unsatisfying offers but
        // may pass an unknown capability through for the pinned model.
        let planned = plan_precise("claude-opus-4-8", &alias_offers, &needs_tools, &evidence);
        assert_eq!(planned.len(), 1);
        assert_eq!(planned[0].endpoint_ref, "e2");

        let mut tiny = offer("claude-opus-4-8", "e3", 3);
        tiny.context_window = Some(8_000);
        let mut wide = offer("claude-opus-4-8", "e4", 4);
        wide.context_window = Some(200_000);
        let context_offers = vec![tiny, wide];
        let needs_context = HardRequirements {
            min_context_window: Some(100_000),
            ..HardRequirements::default()
        };
        // A proven-too-small context window is rejected in both modes.
        for planned in [
            plan_precise(
                "claude-opus-4-8",
                &context_offers,
                &needs_context,
                &evidence,
            ),
            plan_alias(TIER_OPUS, &context_offers, &needs_context, &evidence),
        ] {
            assert_eq!(planned.len(), 1);
            assert_eq!(planned[0].endpoint_ref, "e4");
        }
    }

    #[test]
    fn manual_overrides_steer_alias_selection() {
        let evidence = empty_evidence();
        let mut overridden = offer("custom-encoder-1", "e1", 10);
        overridden.tier_override = Some(TIER_FABLE);
        let offered = vec![overridden, offer("claude-sonnet-4-5", "e2", 1)];
        let planned = plan_alias(
            TIER_FABLE,
            &offered,
            &HardRequirements::default(),
            &evidence,
        );
        // The manual override outranks the lower-tier role family even with
        // a worse priority; ordering still places exact-tier first.
        assert_eq!(planned[0].model_id, "custom-encoder-1");
        assert_eq!(planned[0].basis, TierBasis::ManualOverride);
        assert!(!planned[0].downgraded);
        assert_eq!(planned[1].model_id, "claude-sonnet-4-5");
        assert!(planned[1].downgraded);
    }

    #[test]
    fn ordering_is_deterministic_under_ties() {
        let evidence = empty_evidence();
        let offered = vec![
            offer("claude-sonnet-4-5", "e-b", 5),
            offer("claude-sonnet-4-5", "e-a", 5),
            offer("claude-haiku-4-5", "e-c", 5),
            offer("claude-haiku-4-5", "e-a", 5),
        ];
        let planned = plan_alias(
            TIER_BALANCED,
            &offered,
            &HardRequirements::default(),
            &evidence,
        );
        let order: Vec<&str> = planned.iter().map(|s| s.endpoint_ref.as_str()).collect();
        assert_eq!(order, vec!["e-a", "e-b", "e-a", "e-c"]);
        let mut reversed = offered.clone();
        reversed.reverse();
        let replanned = plan_alias(
            TIER_BALANCED,
            &reversed,
            &HardRequirements::default(),
            &evidence,
        );
        assert_eq!(planned, replanned);
    }

    #[test]
    fn tiny_models_can_still_serve_alias_requests_that_ask_for_nothing() {
        // Sanity: default requirements never exclude capable models.
        let evidence = empty_evidence();
        let offered = vec![offer("claude-haiku-4-5", "e1", 1)];
        assert_eq!(
            plan_alias(TIER_FAST, &offered, &HardRequirements::default(), &evidence).len(),
            1
        );
    }

    #[test]
    fn alias_queues_keep_only_newest_family_generation() {
        let evidence = empty_evidence();
        let mut old = offer("glm-4-6", "endpoint_old", 1);
        let mut new = offer("glm-5-3", "endpoint_new", 1);
        old.tier_override = Some(TIER_OPUS);
        new.tier_override = Some(TIER_OPUS);
        let planned =
            plan_alias(TIER_OPUS, &[old, new], &HardRequirements::default(), &evidence);
        assert_eq!(
            planned
                .iter()
                .map(|hop| hop.model_id.as_str())
                .collect::<Vec<_>>(),
            vec!["glm-5-3"]
        );
    }

    #[test]
    fn same_generation_keeps_parallel_supplier_supply() {
        let evidence = empty_evidence();
        let mut first = offer("glm-5-3", "endpoint_a", 1);
        let mut second = offer("glm-5-3", "endpoint_b", 2);
        first.tier_override = Some(TIER_OPUS);
        second.tier_override = Some(TIER_OPUS);
        let planned = plan_alias(
            TIER_OPUS,
            &[first, second],
            &HardRequirements::default(),
            &evidence,
        );
        assert_eq!(planned.len(), 2);
    }

    #[test]
    fn cheap_variant_lane_never_evicts_family_fast_supply() {
        // The base generation must not evict the family's cheap variant, but
        // an older variant generation still loses to a newer variant.
        let evidence = empty_evidence();
        let mut base = offer("glm-5-3", "endpoint_base", 1);
        let mut old_flash = offer("glm-4-6-flash", "endpoint_flash_old", 1);
        let mut new_flash = offer("glm-5-3-flash", "endpoint_flash_new", 1);
        base.tier_override = Some(TIER_OPUS);
        old_flash.tier_override = Some(TIER_FAST);
        new_flash.tier_override = Some(TIER_FAST);
        let planned = plan_alias(
            TIER_FAST,
            &[base, old_flash, new_flash],
            &HardRequirements::default(),
            &evidence,
        );
        let ids: Vec<&str> = planned.iter().map(|hop| hop.model_id.as_str()).collect();
        assert!(ids.contains(&"glm-5-3-flash"));
        assert!(ids.contains(&"glm-5-3"));
        assert!(!ids.contains(&"glm-4-6-flash"));
    }

    #[test]
    fn incomparable_family_siblings_are_never_trimmed() {
        // Same family ("glm"), same lane, but the alpha suffixes are
        // decisively unordered: neither can be proven older, so both stay.
        let evidence = empty_evidence();
        let mut pro = offer("glm-5-3-pro", "endpoint_pro", 1);
        let mut max = offer("glm-5-3-max", "endpoint_max", 1);
        pro.tier_override = Some(TIER_BALANCED);
        max.tier_override = Some(TIER_BALANCED);
        let planned = plan_alias(
            TIER_BALANCED,
            &[pro, max],
            &HardRequirements::default(),
            &evidence,
        );
        assert_eq!(planned.len(), 2);
    }
}
