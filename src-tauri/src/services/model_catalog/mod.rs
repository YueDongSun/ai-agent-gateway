//! Public model catalog service: bounded fetch, TTL cache, provenance, and
//! deterministic tier/candidate planning on top of fixed public sources.
//!
//! Sources (credential-free, read-only):
//!
//! * `models_dev` — `https://models.dev/api.json`: factual metadata
//!   (family, tool support, modalities, limits, prices). Metadata is not
//!   intelligence and price/context are never used as tier evidence.
//! * `evalplus` — `https://evalplus.github.io/results.json`: public coding
//!   benchmark scores (`pass@1` HumanEval+/MBPP+) kept with their
//!   evaluation condition. Scores are evidence of coding performance only.
//! * `lmarena` — the official published overall text dataset. Original rank,
//!   score interval and condition stay visible. Available rated supply is
//!   divided into four routing bands; this is an operational policy, not a
//!   universal intelligence measurement.
//!
//! Contracts enforced here:
//!
//! * any fetch/parse failure keeps the previous cache and marks it stale —
//!   old documents are never cleared by a network error,
//! * TTL expiry, recorded errors and missing caches are always reported as
//!   stale (fail-closed freshness), never served as fresh,
//! * catalog entries never impersonate supplier offers: `offered_by` is
//!   built only from the supplier configuration and `catalog_only` says so
//!   explicitly,
//! * unknown families stay `pending`; tier precedence is
//!   manual > comparable Arena evidence > coding evidence > ordinal inheritance >
//!   known-family role hint > pending.
//!
//! Pure planning helpers for the router live in `select`; classification in
//! `tiers`; identity in `identity`; parsing in `parse`; bounded fetch in
//! `sources`.

pub(crate) mod arena;
pub(crate) mod arena_fetch;
pub(crate) mod refresh;
pub(crate) use refresh::Catalog_refresh_service;
pub(crate) mod identity;
pub(crate) mod parse;
pub(crate) mod select;
pub(crate) mod sources;
pub(crate) mod tiers;
pub(crate) mod utility;

use serde::{Deserialize, Serialize};

use crate::database::Database;
use crate::error::AppError;
use crate::services::suppliers::{
    get_public_catalog_cache, record_public_catalog_error, resolved_supplier_snapshot,
    save_public_catalog_cache, PublicCatalogCacheInput,
};

use parse::ModelsDevModel;
use select::OfferedModel;
use tiers::{evaluate_model, CatalogEvidence, ModelAssessment, Selector, SourceStatus};

/// Fixed source keys (also the cache row keys).
pub const SOURCE_MODELS_DEV: &str = "models_dev";
pub const SOURCE_EVALPLUS: &str = "evalplus";
pub const SOURCE_LMARENA: &str = arena::SOURCE_ID;

/// Fixed credential-free metadata and ranking sources.
const MODELS_DEV_URL: &str = "https://models.dev/api.json";
const EVALPLUS_URL: &str = "https://evalplus.github.io/results.json";

/// Freshness window: older caches are served but flagged stale.
pub const CATALOG_TTL_SECS: i64 = 86_400;

/// Alias preview depth returned to the UI.
const ALIAS_PREVIEW_LIMIT: usize = 20;

/// One source's cache state; mirrors the UI `Supplier_catalog_view` exactly.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogSourceView {
    pub source: String,
    pub fetched_at: Option<i64>,
    pub last_error: Option<String>,
    pub stale: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub incomplete_rows: Option<usize>,
}

/// Status of both sources; mirrors the UI `Supplier_catalog_status`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogStatusView {
    pub sources: Vec<CatalogSourceView>,
}

/// Refresh input; mirrors the UI `supplier_catalog_refresh` payload.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogRefreshInput {
    pub source: String,
}

/// Supplier endpoint reference for an actually offered model.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OfferedRefView {
    pub supplier_id: String,
    pub endpoint_id: String,
    pub protocol: String,
    pub model_id: String,
}

/// Hard-capability facts from models.dev; `None` means the source is silent.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityFactsView {
    pub supports_tools: Option<bool>,
    pub supports_images: Option<bool>,
    pub context_window: Option<u64>,
    pub max_output_tokens: Option<u64>,
    pub input_modalities: Vec<String>,
    pub output_modalities: Vec<String>,
}

/// Price facts; metadata only, never used as intelligence.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CostFactsView {
    pub input_per_million: Option<f64>,
    pub output_per_million: Option<f64>,
}

/// One alias-planning preview row for the UI's routing explanation.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionView {
    pub model_id: String,
    pub endpoint_ref: String,
    pub supplier_ref: String,
    pub tier: Option<u8>,
    pub basis: String,
    pub downgraded: bool,
    pub reason: String,
}

/// Model metadata lookup result: selector interpretation, tier assessment
/// with source/condition/confidence, factual capabilities and costs, and the
/// strict split between catalog knowledge and supplier offers.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCatalogEntry {
    pub selector: String,
    pub selector_kind: String,
    pub alias_name: Option<String>,
    pub alias_tier: Option<u8>,
    pub model_id: Option<String>,
    pub canonical_id: Option<String>,
    pub family: Option<String>,
    pub release_date: Option<String>,
    pub assessment: Option<ModelAssessment>,
    pub capabilities: Option<CapabilityFactsView>,
    pub cost: Option<CostFactsView>,
    /// True when no enabled supplier endpoint offers this model: the entry
    /// is catalog knowledge only and never a claim of availability.
    pub catalog_only: bool,
    pub offered_by: Vec<OfferedRefView>,
    pub sources: Vec<CatalogSourceView>,
    pub alias_preview: Vec<SelectionView>,
}

fn source_url(source: &str) -> Result<&'static str, AppError> {
    match source {
        SOURCE_MODELS_DEV => Ok(MODELS_DEV_URL),
        SOURCE_EVALPLUS => Ok(EVALPLUS_URL),
        SOURCE_LMARENA => Ok(arena::SOURCE_URL),
        // Same code the UI demo mock rejects unknown sources with.
        _ => Err(AppError::InvalidInput(
            "supplier_catalog_shape_invalid".into(),
        )),
    }
}

/// Status of both catalog sources in fixed order.
pub fn catalog_status(db: &Database) -> Result<CatalogStatusView, AppError> {
    Ok(CatalogStatusView {
        sources: vec![
            catalog_source_view(db, SOURCE_MODELS_DEV)?,
            catalog_source_view(db, SOURCE_EVALPLUS)?,
            catalog_source_view(db, SOURCE_LMARENA)?,
        ],
    })
}

/// One source's cache state. TTL expiry, a recorded error, or a missing
/// cache always reports `stale: true`.
pub fn catalog_source_view(db: &Database, source: &str) -> Result<CatalogSourceView, AppError> {
    source_url(source)?;
    let record = get_public_catalog_cache(db, source)?;
    let now = chrono::Utc::now().timestamp();
    Ok(match record {
        Some(cache) => {
            let stale = cache.last_error.is_some()
                || cache.fetched_at <= 0
                || now.saturating_sub(cache.fetched_at) > CATALOG_TTL_SECS;
            CatalogSourceView {
                incomplete_rows: (source == SOURCE_EVALPLUS)
                    .then(|| {
                        parse::parse_evalplus(&cache.raw_json)
                            .ok()
                            .map(|parsed| parsed.incomplete_rows)
                    })
                    .flatten(),
                source: cache.source,
                fetched_at: (cache.fetched_at > 0).then_some(cache.fetched_at),
                last_error: cache.last_error,
                stale,
            }
        }
        None => CatalogSourceView {
            source: source.to_string(),
            fetched_at: None,
            last_error: None,
            stale: true,
            incomplete_rows: None,
        },
    })
}

/// Refresh one source with a bounded fetch. A successful fetch replaces the
/// cache and clears the error; any failure records a stable error code and
/// keeps the previous document, returned as a stale view. Unknown sources
/// are rejected before any network activity.
pub async fn refresh_source(
    db: &Database,
    input: &CatalogRefreshInput,
) -> Result<CatalogSourceView, AppError> {
    let url = source_url(&input.source)?;
    let fetched = if input.source == SOURCE_LMARENA {
        arena_fetch::fetch_arena_document().await
    } else {
        sources::fetch_document(url, parse::MAX_DOCUMENT_BYTES).await
    };
    apply_fetch_result(db, &input.source, fetched)?;
    catalog_source_view(db, &input.source)
}

/// Accept one fetch outcome into the cache layer. Split from
/// [`refresh_source`] so tests exercise the full persistence semantics
/// offline. A failure records a stable code and keeps the previous document;
/// a success that fails schema validation does the same.
pub(crate) fn apply_fetch_result(
    db: &Database,
    source: &str,
    fetched: Result<String, sources::FetchFailure>,
) -> Result<(), AppError> {
    source_url(source)?;
    let raw = match fetched {
        Ok(raw) => raw,
        Err(failure) => return record_public_catalog_error(db, source, failure.code()),
    };
    let valid = match source {
        SOURCE_MODELS_DEV => parse::parse_models_dev(&raw).map(|_| ()),
        SOURCE_EVALPLUS => parse::parse_evalplus(&raw).map(|_| ()),
        SOURCE_LMARENA => arena::parse_arena_at(&raw, chrono::Utc::now().date_naive()).map(|_| ()),
        _ => unreachable!("source validated before fetch"),
    };
    match valid {
        Ok(()) => {
            let saved = save_public_catalog_cache(
                db,
                PublicCatalogCacheInput {
                    source: source.to_string(),
                    raw_json: raw,
                    fetched_at: chrono::Utc::now().timestamp(),
                },
            );
            if saved.is_err() {
                // Keep the previous cache when the new document is refused.
                record_public_catalog_error(db, source, "fetch_schema_invalid")?;
            }
            Ok(())
        }
        Err(error) => record_public_catalog_error(db, source, error.code()),
    }
}

/// Load both sources from cache into the evidence bundle. A stored document
/// that no longer parses is treated as absent for that source and flagged
/// stale with a stable code — it is never partially trusted.
pub fn load_evidence(db: &Database) -> CatalogEvidence {
    let (models_dev_status, models_dev) =
        load_source(db, SOURCE_MODELS_DEV, parse::parse_models_dev);
    let (evalplus_status, evalplus) = load_source(db, SOURCE_EVALPLUS, parse::parse_evalplus);
    let (arena_status, arena) = load_source(db, SOURCE_LMARENA, |raw| {
        arena::parse_arena_at(raw, chrono::Utc::now().date_naive())
    });
    let mut evidence = CatalogEvidence::new(models_dev, models_dev_status, evalplus, evalplus_status)
        .with_arena(arena, arena_status);
    if let Some(policy) = db.get_setting(utility::POLICY_SETTING).ok().flatten()
        .and_then(|raw| utility::Utility_policy::parse(&raw).ok()) {
        evidence.utility_policy = policy;
    }
    evidence.generation_keep_n = select::parse_generation_keep_n(
        db.get_setting(select::GENERATION_KEEP_N_SETTING).ok().flatten().as_deref(),
    );
    evidence
}

fn load_source<T>(
    db: &Database,
    source: &str,
    parse: impl Fn(&str) -> Result<T, parse::ParseError>,
) -> (SourceStatus, Option<T>) {
    let mut status = SourceStatus {
        source: source.to_string(),
        fetched_at: None,
        last_error: None,
        stale: true,
    };
    let Ok(Some(cache)) = get_public_catalog_cache(db, source) else {
        return (status, None);
    };
    let now = chrono::Utc::now().timestamp();
    status.fetched_at = (cache.fetched_at > 0).then_some(cache.fetched_at);
    status.stale = cache.last_error.is_some()
        || cache.fetched_at <= 0
        || now.saturating_sub(cache.fetched_at) > CATALOG_TTL_SECS;
    if cache.raw_json.trim() == "null" {
        // The error-row placeholder: no document, keep the error code.
        status.last_error = cache.last_error;
        status.stale = true;
        return (status, None);
    }
    match parse(&cache.raw_json) {
        Ok(parsed) => {
            status.last_error = cache.last_error;
            (status, Some(parsed))
        }
        Err(error) => {
            // Fail closed: a corrupt stored document contributes nothing.
            status.last_error = Some(error.code().to_string());
            status.stale = true;
            (status, None)
        }
    }
}

/// Models actually offered by enabled supplier endpoints. This is the only
/// source of routing offers; the public catalog never contributes rows here.
pub fn offered_models(db: &Database) -> Result<Vec<OfferedModel>, AppError> {
    let snapshot = resolved_supplier_snapshot(db)?;
    let admitted: Option<std::collections::HashMap<_, std::collections::HashSet<_>>> =
        if crate::services::provider_admission::enforcement_enabled(db) {
            Some(db.get_all_proxy_providers()?.iter().map(|provider| (provider.id.clone(),
                crate::services::provider_admission::admitted_models(db, provider).into_iter().collect())).collect())
        } else { None };
    let mut offered = Vec::new();
    for endpoint in snapshot.endpoints {
        for model in endpoint.models {
            if !model.enabled {
                continue;
            }
            if admitted.as_ref().is_some_and(|admitted| endpoint.legacy_proxy_provider_id.as_ref()
                .and_then(|provider| admitted.get(provider)).is_none_or(|models| !models.contains(&model.model_id))) {
                continue;
            }
            offered.push(OfferedModel {
                model_id: model.model_id,
                endpoint_ref: endpoint.endpoint_id.clone(),
                supplier_ref: endpoint.supplier_id.clone(),
                protocol: endpoint.protocol.clone(),
                priority: endpoint.priority,
                supports_tools: model.supports_tools,
                supports_images: model.supports_images,
                context_window: model.context_window,
                max_output_tokens: model.max_output_tokens,
                tier_override: model.tier_override,
            });
        }
    }
    Ok(offered)
}

/// Metadata lookup for one selector: precise model id or capability alias,
/// with tier assessment (source/condition/confidence), factual capabilities
/// and costs, and the offered/catalog-only split.
pub fn model_metadata(db: &Database, raw_selector: &str) -> Result<ModelCatalogEntry, AppError> {
    let mut offered = offered_models(db)?;
    let evidence = load_evidence(db)
        .with_offered_models(offered.iter().map(|offer| offer.model_id.clone()).collect());
    enrich_capabilities(&mut offered, &evidence);
    // Exact-name precedence mirrors the router: a bare name that a configured
    // endpoint canonically offers is an exact call even when it contains a
    // family word (`claude-opus-4-8`). Without this, the classifier would
    // return alias previews for a model requests route to exactly. The
    // original alias classification is kept as a dual view so the panel can
    // also show the forwarding queue of the tier this name belongs to.
    let raw_trimmed = raw_selector.trim();
    let bare_name_offered = !raw_trimmed.starts_with(tiers::PRECISE_SELECTOR_PREFIX)
        && !raw_trimmed.starts_with(tiers::ALIAS_SELECTOR_PREFIX)
        && offered.iter().any(|offer| {
            identity::canonical_model_id(&offer.model_id)
                == identity::canonical_model_id(raw_trimmed)
        });
    let classified = tiers::classify_selector(raw_selector)?;
    let selector = match &classified {
        tiers::Selector::Alias { .. } if bare_name_offered => tiers::Selector::Precise {
            model_id: raw_trimmed.to_string(),
        },
        other => other.clone(),
    };
    let dual_alias_tier = match (&classified, &selector) {
        (tiers::Selector::Alias { tier, .. }, tiers::Selector::Precise { .. }) => Some(*tier),
        _ => None,
    };
    let mut sources = vec![
        CatalogSourceView::from(&evidence.models_dev_status),
        CatalogSourceView::from(&evidence.evalplus_status),
        CatalogSourceView::from(&evidence.arena_status),
    ];
    sources[1].incomplete_rows = evidence
        .evalplus
        .as_ref()
        .map(|catalog| catalog.incomplete_rows);
    Ok(match &selector {
        Selector::Precise { model_id } => {
            let wanted = identity::canonical_model_id(model_id);
            let matches =
                |offer: &&OfferedModel| identity::canonical_model_id(&offer.model_id) == wanted;
            // First configured override wins; per-endpoint differences are
            // resolved at selection time.
            let manual_tier = offered
                .iter()
                .filter(|offer| matches(offer))
                .find_map(|offer| offer.tier_override);
            let assessment = evaluate_model(model_id, manual_tier, &evidence);
            let facts = catalog_facts(model_id, &evidence);
            let offered_by = offered
                .iter()
                .filter(|offer| matches(offer))
                .map(|offer| OfferedRefView {
                    supplier_id: offer.supplier_ref.clone(),
                    endpoint_id: offer.endpoint_ref.clone(),
                    protocol: offer.protocol.clone(),
                    model_id: offer.model_id.clone(),
                })
                .collect::<Vec<_>>();
            ModelCatalogEntry {
                selector: raw_selector.to_string(),
                selector_kind: "precise".into(),
                alias_name: dual_alias_tier.map(|_| raw_trimmed.to_string()),
                alias_tier: dual_alias_tier,
                model_id: Some(model_id.clone()),
                canonical_id: Some(assessment.canonical_id.clone()),
                family: assessment.family.clone(),
                release_date: facts.and_then(|model| model.release_date.clone()),
                assessment: Some(assessment),
                capabilities: facts.map(capability_facts),
                cost: facts.map(cost_facts),
                catalog_only: offered_by.is_empty(),
                offered_by,
                sources,
                alias_preview: dual_alias_tier
                    .map(|tier| alias_preview_rows(tier, &offered, &evidence))
                    .unwrap_or_default(),
            }
        }
        Selector::Alias { name, tier } => {
            let alias_preview = alias_preview_rows(*tier, &offered, &evidence);
            ModelCatalogEntry {
                selector: raw_selector.to_string(),
                selector_kind: "alias".into(),
                alias_name: Some(name.clone()),
                alias_tier: Some(*tier),
                model_id: None,
                canonical_id: None,
                family: None,
                release_date: None,
                assessment: None,
                capabilities: None,
                cost: None,
                catalog_only: alias_preview.is_empty(),
                offered_by: Vec::new(),
                sources,
                alias_preview,
            }
        }
    })
}

/// Build the forwarding-queue preview for one alias tier: the same planner
/// the router dispatches through, capped for the metadata view.
fn alias_preview_rows(
    tier: u8,
    offered: &[OfferedModel],
    evidence: &CatalogEvidence,
) -> Vec<SelectionView> {
    select::plan_alias(tier, offered, &select::HardRequirements::default(), evidence)
        .iter()
        .take(ALIAS_PREVIEW_LIMIT)
        .map(|selection| SelectionView {
            model_id: selection.model_id.clone(),
            endpoint_ref: selection.endpoint_ref.clone(),
            supplier_ref: selection.supplier_ref.clone(),
            tier: selection.tier,
            basis: selection.basis.code().to_string(),
            downgraded: selection.downgraded,
            reason: selection.reason.to_string(),
        })
        .collect()
}

fn catalog_facts<'a>(model_id: &str, evidence: &'a CatalogEvidence) -> Option<&'a ModelsDevModel> {
    let catalog = evidence.models_dev.as_ref()?;
    let index = evidence.models_index.by_id(model_id).or_else(|| {
        evidence
            .models_index
            .by_id(&identity::strip_org_prefix(model_id))
    })?;
    catalog.models.get(index)
}

/// Public capability facts for one model id, used to qualify a supplier
/// offer whose own capability facts are unknown.
///
/// The lookup falls back to the id without its org prefix, because supplier
/// catalogues spell a model with a vendor prefix (`anthropic/claude-fable-5.1`)
/// while the public catalog keys it bare (`claude-fable-5.1`). Facts are
/// model-level: the catalog never says which supplier serves a model, and a
/// supplier's own facts always win over these.
pub(crate) fn catalog_capability_facts(
    model_id: &str,
    evidence: &CatalogEvidence,
) -> Option<CapabilityFactsView> {
    catalog_facts(model_id, evidence).map(capability_facts)
}

pub(crate) fn enrich_capabilities(offers: &mut [OfferedModel], evidence: &CatalogEvidence) {
    for offer in offers {
        let Some(facts) = catalog_capability_facts(&offer.model_id, evidence) else {
            continue;
        };
        offer.supports_tools = offer.supports_tools.or(facts.supports_tools);
        offer.supports_images = offer.supports_images.or(facts.supports_images);
        offer.context_window = offer.context_window.or(facts.context_window);
        offer.max_output_tokens = offer.max_output_tokens.or(facts.max_output_tokens);
    }
}

fn capability_facts(model: &ModelsDevModel) -> CapabilityFactsView {
    let supports_images = if model.input_modalities.is_empty() {
        None
    } else {
        Some(model.input_modalities.iter().any(|name| name == "image"))
    };
    CapabilityFactsView {
        supports_tools: model.tool_call,
        supports_images,
        context_window: model.context_window,
        max_output_tokens: model.max_output_tokens,
        input_modalities: model.input_modalities.clone(),
        output_modalities: model.output_modalities.clone(),
    }
}

fn cost_facts(model: &ModelsDevModel) -> CostFactsView {
    CostFactsView {
        input_per_million: model.cost_input_per_mtok,
        output_per_million: model.cost_output_per_mtok,
    }
}

impl From<&SourceStatus> for CatalogSourceView {
    fn from(status: &SourceStatus) -> Self {
        CatalogSourceView {
            source: status.source.clone(),
            fetched_at: status.fetched_at,
            last_error: status.last_error.clone(),
            stale: status.stale,
            incomplete_rows: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::tiers::TIER_OPUS;
    use super::*;

    fn db() -> Database {
        Database::memory().expect("catalog test database")
    }

    fn store(db: &Database, source: &str, raw: &str, fetched_at: i64) {
        save_public_catalog_cache(
            db,
            PublicCatalogCacheInput {
                source: source.into(),
                raw_json: raw.into(),
                fetched_at,
            },
        )
        .expect("store fixture");
    }

    #[test]
    fn status_reports_missing_sources_as_stale_without_fabricating_documents() {
        let db = db();
        let status = catalog_status(&db).expect("status");
        assert_eq!(status.sources.len(), 3);
        assert_eq!(status.sources[0].source, SOURCE_MODELS_DEV);
        assert_eq!(status.sources[1].source, SOURCE_EVALPLUS);
        assert_eq!(status.sources[2].source, SOURCE_LMARENA);
        for source in &status.sources {
            assert!(source.stale);
            assert!(source.fetched_at.is_none());
            assert!(source.last_error.is_none());
        }
        let evidence = load_evidence(&db);
        assert!(evidence.models_dev.is_none());
        assert!(evidence.evalplus.is_none());
        // Fail closed: an unknown family with no evidence stays pending.
        let assessment = evaluate_model("mystery-x9", None, &evidence);
        assert_eq!(assessment.tier, None);
        assert_eq!(assessment.basis.code(), "pending");
        // Known-family role hints are name-based (not source data), so they
        // still resolve while both sources are missing — marked family_role.
        let hinted = evaluate_model("claude-opus-4-8", None, &evidence);
        assert_eq!(hinted.tier, Some(TIER_OPUS));
        assert_eq!(hinted.basis.code(), "family_role");
    }

    #[test]
    fn ttl_expiry_is_fail_closed_even_with_a_valid_document() {
        let db = db();
        let now = chrono::Utc::now().timestamp();
        let raw =
            r#"{"p": {"models": {"m": {"id": "m", "family": "claude-opus", "tool_call": true}}}}"#;
        store(&db, SOURCE_MODELS_DEV, raw, now - CATALOG_TTL_SECS - 5);
        let view = catalog_source_view(&db, SOURCE_MODELS_DEV).expect("view");
        assert!(view.stale, "expired TTL must not read as fresh");
        assert!(view.last_error.is_none());
        assert_eq!(view.fetched_at, Some(now - CATALOG_TTL_SECS - 5));
        let evidence = load_evidence(&db);
        assert!(evidence.models_dev_status.stale);
        assert!(evidence.models_dev.is_some());
        // The data is still usable, but every derived assessment carries the
        // stale provenance instead of pretending to be current.
        let assessment = evaluate_model("claude-opus-4-8", None, &evidence);
        assert_eq!(assessment.basis.code(), "family_role");
        assert_eq!(assessment.sources.len(), 1);
        assert_eq!(assessment.sources[0].source, SOURCE_MODELS_DEV);
        assert!(assessment.sources[0].stale);
    }

    #[test]
    fn fetch_failures_keep_the_previous_cache_and_mark_it_stale() {
        let db = db();
        let now = chrono::Utc::now().timestamp();
        let raw = r#"{"p": {"models": {"m": {"id": "m", "family": "claude-opus"}}}}"#;
        store(&db, SOURCE_MODELS_DEV, raw, now);

        apply_fetch_result(&db, SOURCE_MODELS_DEV, Err(sources::FetchFailure::Timeout))
            .expect("record failure");
        let view = catalog_source_view(&db, SOURCE_MODELS_DEV).expect("view");
        assert_eq!(view.last_error.as_deref(), Some("fetch_timeout"));
        assert!(view.stale);
        assert_eq!(view.fetched_at, Some(now), "old timestamp preserved");
        let evidence = load_evidence(&db);
        assert!(evidence.models_dev.is_some(), "old document must survive");
        assert_eq!(
            evidence.models_dev.as_ref().unwrap().models[0].id,
            "m",
            "old document contents must survive"
        );

        // A successful refresh clears the error and the stale flag.
        apply_fetch_result(&db, SOURCE_MODELS_DEV, Ok(raw.to_string())).expect("accept");
        let view = catalog_source_view(&db, SOURCE_MODELS_DEV).expect("view");
        assert!(view.last_error.is_none());
        assert!(!view.stale);
        assert!(view.fetched_at.unwrap() > now - 5);
    }

    #[test]
    fn invalid_fetched_documents_are_refused_and_keep_the_old_cache() {
        let db = db();
        let now = chrono::Utc::now().timestamp();
        let raw = r#"{"p": {"models": {"m": {"id": "m"}}}}"#;
        store(&db, SOURCE_MODELS_DEV, raw, now);

        // Decode failure, schema failure and out-of-bounds numbers all keep
        // the previous good document and record a stable code.
        for (fetched, code) in [
            (Err(sources::FetchFailure::Decode), "fetch_decode_failed"),
            (Ok("not-json".to_string()), "fetch_decode_failed"),
            (
                Ok(
                    r#"{"p": {"models": {"m": {"id": "m", "limit": {"context": -4}}}}}"#
                        .to_string(),
                ),
                "fetch_numeric_out_of_bounds",
            ),
            (
                Ok(r#"{"p": {"models": {"m": {"release_date": "yesterday"}}}}"#.to_string()),
                "fetch_date_invalid",
            ),
        ] {
            apply_fetch_result(&db, SOURCE_MODELS_DEV, fetched).expect("record");
            let view = catalog_source_view(&db, SOURCE_MODELS_DEV).expect("view");
            assert_eq!(view.last_error.as_deref(), Some(code));
            assert!(view.stale);
            let evidence = load_evidence(&db);
            assert_eq!(
                evidence
                    .models_dev
                    .as_ref()
                    .expect("old doc survives")
                    .models[0]
                    .id,
                "m"
            );
        }
    }

    #[test]
    fn corrupt_stored_documents_fail_closed_with_stable_codes() {
        let db = db();
        // Valid JSON through every cache validator, but rejected by the
        // catalog parsers (array root, then a bad release date): the source
        // must contribute nothing and carry a stable code.
        store(&db, SOURCE_EVALPLUS, "[]", chrono::Utc::now().timestamp());
        let evidence = load_evidence(&db);
        assert!(evidence.evalplus.is_none());
        assert!(evidence.evalplus_status.stale);
        assert_eq!(
            evidence.evalplus_status.last_error.as_deref(),
            Some("fetch_schema_invalid")
        );

        store(
            &db,
            SOURCE_MODELS_DEV,
            r#"{"p": {"models": {"m": {"release_date": "yesterday"}}}}"#,
            chrono::Utc::now().timestamp(),
        );
        let evidence = load_evidence(&db);
        assert!(evidence.models_dev.is_none());
        assert!(evidence.models_dev_status.stale);
        assert_eq!(
            evidence.models_dev_status.last_error.as_deref(),
            Some("fetch_date_invalid")
        );

        // The error-marker row ('null' placeholder) keeps its recorded code
        // and is never mistaken for a document. It is the first write here so
        // the placeholder row is what load sees.
        let marker_db = Database::memory().expect("marker test database");
        record_public_catalog_error(&marker_db, SOURCE_EVALPLUS, "fetch_timeout").expect("record");
        let evidence = load_evidence(&marker_db);
        assert!(evidence.evalplus.is_none());
        assert!(evidence.evalplus_status.stale);
        assert_eq!(
            evidence.evalplus_status.last_error.as_deref(),
            Some("fetch_timeout")
        );
    }

    #[test]
    fn catalog_models_never_impersonate_supplier_offers() {
        use crate::services::suppliers::{
            save_supplier, SupplierEndpointInput, SupplierModelInput, SupplierSaveInput,
        };

        let db = db();
        let now = chrono::Utc::now().timestamp();
        let raw = r#"{
            "p": {"models": {
                "claude-opus-4-8": {"id": "claude-opus-4-8", "family": "claude-opus", "tool_call": true},
                "catalog-only-model": {"id": "catalog-only-model", "family": "other"}
            }}
        }"#;
        store(&db, SOURCE_MODELS_DEV, raw, now);

        save_supplier(
            &db,
            SupplierSaveInput {
                id: Some("supplier_demo".into()),
                expected_generation: 0,
                name: "Demo Supplier".into(),
                base_url: Some("https://api.example.invalid/v1".into()),
                api_key: Some("sk-catalog-test".into()),
                enabled: true,
                priority: 100,
                source: None,
                provenance: None,
                icon: None,
                icon_color: None,
                endpoints: Some(vec![SupplierEndpointInput {
                    protocol: "openai-chat".into(),
                    endpoint_url: None,
                    api_key_override: None,
                    use_shared_api_key: false,
                    enabled: Some(true),
                    priority: Some(100),
                    extra_headers: None,
                    source: None,
                    provenance: None,
                    models: Some(vec![SupplierModelInput {
                        model_id: "claude-opus-4-8".into(),
                        display_name: None,
                        enabled: true,
                        supports_tools: Some(true),
                        supports_images: None,
                        context_window: None,
                        max_output_tokens: None,
                        tier_override: None,
                        source: None,
                        provenance: None,
                    }]),
                }]),
            },
        )
        .expect("save supplier");

        let offered_entry = model_metadata(&db, "precise:claude-opus-4-8").expect("metadata");
        assert!(!offered_entry.catalog_only);
        assert_eq!(offered_entry.offered_by.len(), 1);
        assert_eq!(offered_entry.offered_by[0].model_id, "claude-opus-4-8");
        assert_eq!(offered_entry.offered_by[0].protocol, "openai-chat");
        assert_eq!(
            offered_entry.capabilities.as_ref().unwrap().supports_tools,
            Some(true)
        );

        // A bare offered name is an exact call in metadata too, mirroring the
        // router, not an alias preview that suggests substitution.
        let bare_entry = model_metadata(&db, "claude-opus-4-8").expect("bare metadata");
        assert_eq!(bare_entry.selector_kind, "precise");
        assert!(!bare_entry.catalog_only);
        assert_eq!(bare_entry.offered_by.len(), 1);

        let catalog_entry = model_metadata(&db, "precise:catalog-only-model").expect("metadata");
        assert!(catalog_entry.catalog_only);
        assert!(catalog_entry.offered_by.is_empty());
        // Catalog presence alone never creates an offer.
        let offered = offered_models(&db).expect("offers");
        assert!(offered
            .iter()
            .all(|offer| offer.model_id != "catalog-only-model"));
    }

    #[test]
    fn incomplete_benchmark_rows_are_reported_in_source_status() {
        let db = db();
        let raw = r#"{
            "partial":{"pass@1":{"humaneval+":70},"prompted":true},
            "complete":{"pass@1":{"humaneval+":70,"mbpp+":60},"prompted":true}
        }"#;
        apply_fetch_result(&db, SOURCE_EVALPLUS, Ok(raw.into())).unwrap();
        let view = catalog_source_view(&db, SOURCE_EVALPLUS).unwrap();
        assert_eq!(view.incomplete_rows, Some(1));
        assert!(!view.stale);
        assert_eq!(load_evidence(&db).evalplus.unwrap().entries.len(), 1);
    }

    #[test]
    fn unknown_sources_are_rejected_with_the_demo_contract_code() {
        let db = db();
        let error = catalog_source_view(&db, "bogus_source").unwrap_err();
        assert!(error.to_string().contains("supplier_catalog_shape_invalid"));
        assert!(apply_fetch_result(&db, "bogus_source", Ok("{}".into())).is_err());
    }

    /// The live public documents must parse with the shipped rules. A
    /// stricter rule set refuses the real catalog, which leaves capability
    /// planning with no evidence at all and every alias empty — the failure
    /// is silent, so it is guarded explicitly here.
    ///
    /// Metadata fetch only: no generation, no credentials, no budget.
    #[tokio::test]
    #[ignore = "needs network access to the public catalog sources"]
    async fn live_public_sources_parse_with_the_shipped_rules() {
        for source in [SOURCE_MODELS_DEV, SOURCE_EVALPLUS] {
            let url = source_url(source).expect("fixed source url");
            let raw = sources::fetch_document(url, parse::MAX_DOCUMENT_BYTES)
                .await
                .unwrap_or_else(|failure| panic!("{source} fetch failed: {}", failure.code()));
            match source {
                SOURCE_MODELS_DEV => {
                    let catalog = parse::parse_models_dev(&raw).expect("models.dev parses");
                    assert!(!catalog.models.is_empty(), "models.dev has no models");
                }
                _ => {
                    let catalog = parse::parse_evalplus(&raw).expect("evalplus parses");
                    assert!(!catalog.entries.is_empty(), "evalplus has no evidence");
                }
            }
        }
    }

    #[test]
    fn alias_metadata_previews_only_real_offers() {
        use crate::services::suppliers::{
            save_supplier, SupplierEndpointInput, SupplierModelInput, SupplierSaveInput,
        };

        let db = db();
        save_supplier(
            &db,
            SupplierSaveInput {
                id: Some("supplier_alias".into()),
                expected_generation: 0,
                name: "Alias Supplier".into(),
                base_url: Some("https://api.example.invalid/v1".into()),
                api_key: Some("sk-alias-test".into()),
                enabled: true,
                priority: 100,
                source: None,
                provenance: None,
                icon: None,
                icon_color: None,
                endpoints: Some(vec![SupplierEndpointInput {
                    protocol: "anthropic".into(),
                    endpoint_url: None,
                    api_key_override: None,
                    use_shared_api_key: false,
                    enabled: Some(true),
                    priority: Some(100),
                    extra_headers: None,
                    source: None,
                    provenance: None,
                    models: Some(vec![
                        SupplierModelInput {
                            model_id: "claude-opus-4-8".into(),
                            display_name: None,
                            enabled: true,
                            supports_tools: Some(true),
                            supports_images: Some(true),
                            context_window: Some(200_000),
                            max_output_tokens: Some(32_000),
                            tier_override: None,
                            source: None,
                            provenance: None,
                        },
                        SupplierModelInput {
                            model_id: "claude-sonnet-5-5".into(),
                            display_name: None,
                            enabled: true,
                            supports_tools: Some(true),
                            supports_images: Some(true),
                            context_window: Some(200_000),
                            max_output_tokens: Some(64_000),
                            tier_override: None,
                            source: None,
                            provenance: None,
                        },
                    ]),
                }]),
            },
        )
        .expect("save supplier");

        let entry = model_metadata(&db, "fable").expect("alias metadata");
        assert_eq!(entry.selector_kind, "alias");
        assert_eq!(entry.alias_tier, Some(3));
        assert!(!entry.catalog_only);
        // The opus-class model is a traceable downgrade; nothing above the
        // alias tier is previewed and nothing outside the offers appears.
        assert!(!entry.alias_preview.is_empty());
        assert!(entry
            .alias_preview
            .iter()
            .all(|row| row.model_id == "claude-opus-4-8" || row.model_id == "claude-sonnet-5-5"));
        assert!(entry.alias_preview[0].downgraded);
        assert_eq!(entry.alias_preview[0].model_id, "claude-opus-4-8");
    }

    #[test]
    fn offered_bare_name_keeps_precise_identity_and_its_tier_queue() {
        let db = Database::memory().unwrap();
        crate::services::suppliers::save_supplier(
            &db,
            serde_json::from_value(serde_json::json!({
                "expectedGeneration":0,"name":"fixture","baseUrl":"https://example.invalid","apiKey":"k",
                "endpoints":[{"protocol":"anthropic","models":[
                    {"modelId":"claude-opus-4-8","supportsTools":true,"supportsImages":true,"contextWindow":200000,"maxOutputTokens":32000}
                ]}]
            }))
            .unwrap(),
        )
        .expect("save supplier");

        let entry = model_metadata(&db, "claude-opus-4-8").expect("precise metadata");
        assert_eq!(entry.selector_kind, "precise");
        assert_eq!(entry.model_id.as_deref(), Some("claude-opus-4-8"));
        assert!(!entry.offered_by.is_empty());
        // Dual view: the opus-tier forwarding queue is visible even though
        // the selector stays precise for routing.
        assert_eq!(entry.alias_tier, Some(2));
        assert!(!entry.alias_preview.is_empty());
        assert!(entry
            .alias_preview
            .iter()
            .all(|row| row.model_id == "claude-opus-4-8"));
    }
}

pub(crate) mod arena_visualization;
