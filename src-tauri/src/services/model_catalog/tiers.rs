//! Capability tiers, public-evidence attribution, and selector classification.
//!
//! Pure logic, no I/O. Tier decisions follow one deterministic chain:
//!
//! 1. manual override (supplier `tierOverride`) — always wins,
//! 2. comparable Arena overall text ranking, relative to configured rated supply,
//! 3. comparable public coding evidence (EvalPlus pass@1, same leaderboard,
//!    per-suite scores kept separate; averaged only inside one condition),
//! 3. reliable same-family ordinal inheritance (never across families; date
//!    stamps are not versions; `mini`/`flash` variants never inherit an
//!    anchor's top tier unchecked),
//! 4. known-family role name hint (claude-fable/opus/sonnet/haiku and the
//!    documented gemini/gpt families; unknown families are never guessed),
//! 5. documented name/metadata inference (vendor flagship priors, flagship
//!    and cheap-variant name tokens, models.dev chat-capable presence) —
//!    the weakest accepted basis; queues order every evidence-assessed
//!    candidate above these,
//! 6. pending.
//!
//! Price and context window are metadata facts and are never used as
//! intelligence. Source data is factual evidence with provenance and
//! confidence; it never claims a supplier offers anything.

use std::collections::HashMap;

use chrono::NaiveDate;
use serde::Serialize;

use crate::error::AppError;

use super::arena::{self, ArenaAssessment, ArenaCatalog};
use super::identity::{
    canonical_model_id, evidence_key_tokens, family_of_id, introduces_variant, version_relation,
    VersionRelation,
};
use super::parse::{EvalPlusCatalog, EvalPlusEntry, ModelsDevCatalog, ModelsDevModel};

/// Tier ordinal contract shared with `SupplierModelInput::tier_override`
/// (0..=3) and the UI `Supplier_tier`.
pub(crate) const TIER_FAST: u8 = 0;
pub(crate) const TIER_BALANCED: u8 = 1;
pub(crate) const TIER_OPUS: u8 = 2;
pub(crate) const TIER_FABLE: u8 = 3;

/// Stable selector namespaces: `precise:<model id>` always means the literal
/// model id, `alias:<name>` always means a capability alias, and a bare name
/// is an alias only when it matches a known alias (so a real model that
/// shares an alias's spelling is addressed as `precise:<id>`).
pub(crate) const PRECISE_SELECTOR_PREFIX: &str = "precise:";
pub(crate) const ALIAS_SELECTOR_PREFIX: &str = "alias:";

pub(crate) const CONFIDENCE_MANUAL: f64 = 1.0;
pub(crate) const CONFIDENCE_CODING: f64 = 0.8;
pub(crate) const CONFIDENCE_CODING_AMBIGUOUS: f64 = 0.65;
const CONFIDENCE_ARENA: f64 = 0.8;
const CONFIDENCE_ARENA_AMBIGUOUS: f64 = 0.65;
pub(crate) const CONFIDENCE_FAMILY_ORDINAL: f64 = 0.5;
pub(crate) const CONFIDENCE_FAMILY_ROLE: f64 = 0.4;
pub(crate) const CONFIDENCE_NAME_INFERENCE: f64 = 0.3;

/// Flagship positioning vocabulary in a model id. These tokens mark a
/// vendor's top line, so an otherwise unknown id earns an opus prior.
const FLAGSHIP_TOKENS: &[&str] = &["max", "pro", "ultra", "opus", "fable"];

/// Vendor tier priors for flagship lines the public evidence catalogs have
/// not ranked yet. Keyed by the discovery family key (`rank::family_key`);
/// cheap/experimental variants of these families are already diverted to
/// [`TIER_FAST`] by the low-tier token rule before this table applies.
const VENDOR_TIER_PRIORS: &[(&str, u8)] = &[
    ("glm", TIER_OPUS),
    ("deepseek-chat", TIER_OPUS),
    ("deepseek-reasoner", TIER_OPUS),
    ("kimi", TIER_OPUS),
    ("minimax", TIER_OPUS),
    ("grok", TIER_OPUS),
];

/// Coding-score bands for the EvalPlus normalized average of
/// `humaneval+` and `mbpp+` (both 0..=100). These are documented policy
/// constants for *coding* capability only — not a unified intelligence claim.
const BAND_FABLE_AVG: f64 = 80.0;
const BAND_OPUS_AVG: f64 = 70.0;
const BAND_BALANCED_AVG: f64 = 55.0;

/// UI-compatible coarse origin (`Tier_resolution.origin`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum TierOrigin {
    Manual,
    Inferred,
    Pending,
}

/// Which step of the chain produced the tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum TierBasis {
    ManualOverride,
    ArenaEvidence,
    CodingEvidence,
    FamilyOrdinal,
    FamilyRole,
    NameInference,
    Pending,
}

impl TierBasis {
    /// Stable snake_case code for IPC views and logs.
    pub(crate) fn code(self) -> &'static str {
        match self {
            TierBasis::ManualOverride => "manual_override",
            TierBasis::ArenaEvidence => "arena_evidence",
            TierBasis::CodingEvidence => "coding_evidence",
            TierBasis::FamilyOrdinal => "family_ordinal",
            TierBasis::FamilyRole => "family_role",
            TierBasis::NameInference => "name_inference",
            TierBasis::Pending => "pending",
        }
    }

    /// Strength ordering of the chain step that produced this basis; lower is
    /// stronger. Alias queues order candidates inside one preference band by
    /// this rank first, so evidence-assessed supply always precedes
    /// name-inferred supply.
    pub(crate) fn prior_rank(self) -> u8 {
        match self {
            TierBasis::ManualOverride => 0,
            TierBasis::ArenaEvidence | TierBasis::CodingEvidence => 1,
            TierBasis::FamilyOrdinal => 2,
            TierBasis::FamilyRole => 3,
            TierBasis::NameInference => 4,
            TierBasis::Pending => 5,
        }
    }
}

/// One EvalPlus attribution: per-suite scores stay separate and the
/// evaluation condition stays visible; `average` is the in-leaderboard
/// normalization used for banding.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CodingEvidence {
    pub humaneval_plus: f64,
    pub mbpp_plus: f64,
    pub prompted: bool,
    pub average: f64,
    pub entry_keys: Vec<String>,
}

/// Provenance of one source that contributed to a decision.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AssessmentSource {
    pub source: String,
    pub fetched_at: Option<i64>,
    pub stale: bool,
    pub last_error: Option<String>,
}

/// Deterministic tier assessment for one model id.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModelAssessment {
    pub model_id: String,
    pub canonical_id: String,
    pub family: Option<String>,
    pub tier: Option<u8>,
    pub origin: TierOrigin,
    pub basis: TierBasis,
    pub confidence: Option<f64>,
    pub condition: Option<String>,
    /// Separate, weaker signal from the model's family role name; used by
    /// alias candidate matching, never promoted into `tier` on its own.
    pub role_tier_hint: Option<u8>,
    pub coding: Option<CodingEvidence>,
    pub arena: Option<ArenaAssessment>,
    pub sources: Vec<AssessmentSource>,
}

/// Per-source cache status; TTL-expired, errored, or missing caches are
/// stale. Stale data is served with its flag, never as fresh.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SourceStatus {
    pub source: String,
    pub fetched_at: Option<i64>,
    pub last_error: Option<String>,
    pub stale: bool,
}

/// Lookup index over the parsed models.dev catalog.
#[derive(Debug, Clone, Default)]
pub(crate) struct ModelsDevIndex {
    by_id: HashMap<String, usize>,
    by_exact_id: HashMap<String, usize>,
    by_family: HashMap<String, Vec<usize>>,
}

impl ModelsDevIndex {
    pub(crate) fn build(catalog: &ModelsDevCatalog) -> Self {
        let mut by_id = HashMap::with_capacity(catalog.models.len());
        let mut by_exact_id = HashMap::with_capacity(catalog.models.len());
        let mut by_family: HashMap<String, Vec<usize>> = HashMap::new();
        for (index, model) in catalog.models.iter().enumerate() {
            by_id.insert(canonical_model_id(&model.id), index);
            by_exact_id.insert(model.id.clone(), index);
            if let Some(family) = model.family.as_deref() {
                by_family.entry(family.to_string()).or_default().push(index);
            }
        }
        ModelsDevIndex { by_id, by_exact_id, by_family }
    }

    pub(crate) fn by_id(&self, model_id: &str) -> Option<usize> {
        self.by_id.get(&canonical_model_id(model_id)).copied()
    }

    pub(crate) fn by_exact_id(&self, model_id: &str) -> Option<usize> {
        self.by_exact_id.get(model_id).copied()
    }

    pub(crate) fn by_family(&self, family: &str) -> &[usize] {
        self.by_family.get(family).map(Vec::as_slice).unwrap_or(&[])
    }
}

/// Parsed evidence plus per-source status. `models_index` is derived from
/// `models_dev` and must be rebuilt whenever the catalog changes.
pub(crate) struct CatalogEvidence {
    pub utility_policy: super::utility::Utility_policy,
    /// How many same-family generations an alias queue keeps; loaded from the
    /// `family_generation_keep_n` setting (see `select::GENERATION_KEEP_N_SETTING`).
    pub generation_keep_n: usize,
    pub models_dev: Option<ModelsDevCatalog>,
    pub models_dev_status: SourceStatus,
    pub evalplus: Option<EvalPlusCatalog>,
    pub evalplus_status: SourceStatus,
    pub models_index: ModelsDevIndex,
    pub arena: Option<ArenaCatalog>,
    pub arena_status: SourceStatus,
    arena_supply_tiers: HashMap<String, (u8, usize)>,
    /// These maps belong to one immutable evidence generation and contain
    /// only distinct configured/admitted offer IDs. Exact wire keys preserve
    /// condition/variant spellings that general canonicalization may erase.
    offered_model_ids: Vec<String>,
    arena_assessments: HashMap<String, Option<ArenaAssessment>>,
    baseline_assessments: HashMap<String, ModelAssessment>,
}

impl CatalogEvidence {
    pub(crate) fn new(
        models_dev: Option<ModelsDevCatalog>,
        models_dev_status: SourceStatus,
        evalplus: Option<EvalPlusCatalog>,
        evalplus_status: SourceStatus,
    ) -> Self {
        let models_index = models_dev
            .as_ref()
            .map(ModelsDevIndex::build)
            .unwrap_or_default();
        CatalogEvidence {
            utility_policy: super::utility::Utility_policy::default(),
            generation_keep_n: super::select::DEFAULT_GENERATION_KEEP_N,
            models_dev,
            models_dev_status,
            evalplus,
            evalplus_status,
            models_index,
            arena: None,
            arena_status: SourceStatus {
                source: arena::SOURCE_ID.into(),
                fetched_at: None,
                last_error: None,
                stale: true,
            },
            arena_supply_tiers: HashMap::new(),
            offered_model_ids: Vec::new(),
            arena_assessments: HashMap::new(),
            baseline_assessments: HashMap::new(),
        }
    }

    pub(crate) fn with_arena(
        mut self,
        catalog: Option<ArenaCatalog>,
        status: SourceStatus,
    ) -> Self {
        self.arena = catalog;
        self.arena_status = status;
        self.rebuild_offered_assessments();
        self
    }

    /// Routing roles compare the distinct rated models actually configured
    /// here, rather than calling every modern model strong against an archive
    /// of retired models. Raw Arena rank/score and the cohort stay visible.
    pub(crate) fn with_offered_models(mut self, models: Vec<String>) -> Self {
        self.offered_model_ids = models
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        self.rebuild_offered_assessments();
        self
    }

    fn rebuild_offered_assessments(&mut self) {
        self.arena_supply_tiers.clear();
        self.arena_assessments.clear();
        self.baseline_assessments.clear();
        for model in &self.offered_model_ids {
            let assessment = self
                .arena
                .as_ref()
                .and_then(|catalog| arena::assessment_for(model, catalog));
            // A known miss is a result too: repeated plans must not scan the
            // full public catalog again for an unrecognized offered ID.
            self.arena_assessments.insert(model.clone(), assessment);
        }
        let mut groups: std::collections::BTreeMap<String, (ArenaAssessment, Vec<String>)> =
            std::collections::BTreeMap::new();
        for model in &self.offered_model_ids {
            if let Some(assessment) = self.arena_assessments.get(model).and_then(Option::as_ref) {
                groups
                    .entry(assessment.model_name.to_lowercase())
                    .or_insert_with(|| (assessment.clone(), Vec::new()))
                    .1
                    .push(canonical_model_id(model));
            }
        }
        let mut groups: Vec<_> = groups.into_values().collect();
        groups.sort_by(|left, right| {
            left.0
                .rank
                .cmp(&right.0.rank)
                .then_with(|| left.0.model_name.cmp(&right.0.model_name))
        });
        let total = groups.len();
        for (index, (_, models)) in groups.into_iter().enumerate() {
            let tier = TIER_FABLE - ((index * 4) / total).min(3) as u8;
            for model in models {
                self.arena_supply_tiers.insert(model, (tier, total));
            }
        }
        // Relative tiers must be complete before any baseline is frozen.
        self.baseline_assessments = self
            .offered_model_ids
            .iter()
            .map(|model| (model.clone(), evaluate_model_uncached(model, None, self)))
            .collect();
    }

    fn arena_assessment_for(&self, model_id: &str) -> Option<ArenaAssessment> {
        match self.arena_assessments.get(model_id) {
            Some(assessment) => assessment.clone(),
            None => self
                .arena
                .as_ref()
                .and_then(|catalog| arena::assessment_for(model_id, catalog)),
        }
    }
}

/// Parsed selector: either a literal model id or a capability alias.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Selector {
    Precise { model_id: String },
    Alias { name: String, tier: u8 },
}

/// Alias tier resolution, matching the UI `capability_tier_for_alias`
/// semantics: fable is the strongest tier and opus follows.
pub(crate) fn capability_tier_for_alias(alias: &str) -> Option<u8> {
    if let Some(tier) = crate::proxy::planner::client_role_tier(alias) {
        return Some(tier);
    }
    let name = alias.to_lowercase();
    if name.contains("fable") {
        Some(TIER_FABLE)
    } else if name.contains("opus") {
        Some(TIER_OPUS)
    } else if name.contains("sonnet") {
        Some(TIER_BALANCED)
    } else if name.contains("haiku") {
        Some(TIER_FAST)
    } else {
        None
    }
}

/// Known Anthropic role-family hint, mirroring the UI
/// `infer_anthropic_family_tier`. Unknown families (including `claude`
/// without a role word, e.g. `claude-mythos`) return `None` and are never
/// guessed.
pub(crate) fn role_tier_hint(model_id: &str) -> Option<u8> {
    let id = model_id.to_lowercase();
    if !id.contains("claude") && !id.contains("fable") {
        return None;
    }
    if id.contains("fable") {
        Some(TIER_FABLE)
    } else if id.contains("opus") {
        Some(TIER_OPUS)
    } else if id.contains("sonnet") {
        Some(TIER_BALANCED)
    } else if id.contains("haiku") {
        Some(TIER_FAST)
    } else {
        None
    }
}

/// Known models.dev family roles for the non-Claude vendors, matched
/// exactly (never by substring). Everything unlisted stays unclassified.
pub(crate) fn family_role_tier(family: &str) -> Option<u8> {
    const FAMILY_ROLES: &[(&str, u8)] = &[
        ("claude-fable", TIER_FABLE),
        ("claude-opus", TIER_OPUS),
        ("claude-sonnet", TIER_BALANCED),
        ("claude-haiku", TIER_FAST),
        ("gemini-pro", TIER_OPUS),
        ("gemini-flash", TIER_BALANCED),
        ("gemini-flash-lite", TIER_FAST),
        ("gpt-pro", TIER_OPUS),
        ("gpt-codex", TIER_OPUS),
        ("gpt-codex-mini", TIER_BALANCED),
        ("gpt-codex-spark", TIER_FAST),
        ("gpt-mini", TIER_FAST),
        ("o", TIER_OPUS),
    ];
    FAMILY_ROLES
        .iter()
        .find(|(name, _)| *name == family)
        .map(|(_, tier)| *tier)
}

/// Classify a raw client selector. Bare names resolve to a capability alias
/// only when they match a known alias; everything else is a precise id. The
/// `precise:` namespace keeps a real model named like an alias reachable.
pub(crate) fn classify_selector(raw: &str) -> Result<Selector, AppError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AppError::InvalidInput(
            "supplier_catalog_selector_invalid".into(),
        ));
    }
    if let Some(rest) = trimmed.strip_prefix(PRECISE_SELECTOR_PREFIX) {
        let model_id = rest.trim();
        if model_id.is_empty() {
            return Err(AppError::InvalidInput(
                "supplier_catalog_selector_invalid".into(),
            ));
        }
        return Ok(Selector::Precise {
            model_id: model_id.to_string(),
        });
    }
    if let Some(rest) = trimmed.strip_prefix(ALIAS_SELECTOR_PREFIX) {
        let name = rest.trim();
        let tier = capability_tier_for_alias(name)
            .ok_or_else(|| AppError::InvalidInput("supplier_catalog_alias_unknown".into()))?;
        return Ok(Selector::Alias {
            name: name.to_string(),
            tier,
        });
    }
    match capability_tier_for_alias(trimmed) {
        Some(tier) => Ok(Selector::Alias {
            name: trimmed.to_string(),
            tier,
        }),
        None => Ok(Selector::Precise {
            model_id: trimmed.to_string(),
        }),
    }
}

/// Band the EvalPlus normalized average into a capability tier.
pub(crate) fn coding_band(average: f64) -> u8 {
    if average >= BAND_FABLE_AVG {
        TIER_FABLE
    } else if average >= BAND_OPUS_AVG {
        TIER_OPUS
    } else if average >= BAND_BALANCED_AVG {
        TIER_BALANCED
    } else {
        TIER_FAST
    }
}

/// Parse the trailing `(Month Year)` condition of an EvalPlus key into a
/// first-of-month date used only as a deterministic tiebreak between
/// duplicate entries of one model.
fn entry_condition_date(key: &str) -> Option<NaiveDate> {
    let inner = key.rfind('(').and_then(|start| {
        key[start + 1..]
            .find(')')
            .map(|end| &key[start + 1..start + 1 + end])
    })?;
    let mut parts = inner.split_whitespace();
    let month = parts.next()?.to_lowercase();
    let year: i32 = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    let month = match month.as_str() {
        "jan" | "january" => 1,
        "feb" | "february" => 2,
        "mar" | "march" => 3,
        "apr" | "april" => 4,
        "may" => 5,
        "jun" | "june" => 6,
        "jul" | "july" => 7,
        "aug" | "august" => 8,
        "sep" | "sept" | "september" => 9,
        "oct" | "october" => 10,
        "nov" | "november" => 11,
        "dec" | "december" => 12,
        _ => return None,
    };
    NaiveDate::from_ymd_opt(year, month, 1)
}

fn sorted_multiset(tokens: &[String]) -> Vec<String> {
    let mut copy = tokens.to_vec();
    copy.sort();
    copy
}

/// All EvalPlus rows whose key describes exactly this model: the key's
/// tokens must equal the id's tokens, optionally after dropping one leading
/// org namespace (`anthropic/...`). Conservative by construction — partial
/// or fuzzy matches never attribute scores.
fn find_evalplus_matches<'a>(
    model_id: &str,
    catalog: &'a EvalPlusCatalog,
) -> Vec<&'a EvalPlusEntry> {
    let canonical = canonical_model_id(model_id);
    let id_tokens = sorted_multiset(&evidence_key_tokens(&canonical));
    let without_org = {
        let mut tokens = evidence_key_tokens(&canonical);
        if !tokens.is_empty() {
            tokens.remove(0);
        }
        sorted_multiset(&tokens)
    };
    if id_tokens.is_empty() {
        return Vec::new();
    }
    let mut matches: Vec<&EvalPlusEntry> = catalog
        .entries
        .iter()
        .filter(|entry| {
            let key_tokens = sorted_multiset(&evidence_key_tokens(&entry.key));
            !key_tokens.is_empty()
                && (key_tokens == id_tokens
                    || (!without_org.is_empty() && key_tokens == without_org))
        })
        .collect();
    // Deterministic winner: newest stated condition first, then the
    // lexicographically smallest key.
    matches.sort_by(|left, right| left.key.cmp(&right.key));
    matches.sort_by_key(|entry| {
        // Reverse-chronological fold below wants "greater is better"; the
        // stable key order above preserves smallest-key-first on ties.
        std::cmp::Reverse(entry_condition_date(&entry.key))
    });
    matches
}

/// Attribution of one EvalPlus row set to a model: newest condition wins,
/// ambiguity across multiple rows is recorded and lowers confidence.
fn coding_evidence_for(model_id: &str, catalog: &EvalPlusCatalog) -> Option<CodingEvidence> {
    let matches = find_evalplus_matches(model_id, catalog);
    let winner = matches.first()?;
    let average = (winner.humaneval_plus + winner.mbpp_plus) / 2.0;
    Some(CodingEvidence {
        humaneval_plus: winner.humaneval_plus,
        mbpp_plus: winner.mbpp_plus,
        prompted: winner.prompted,
        average,
        entry_keys: matches.iter().map(|entry| entry.key.clone()).collect(),
    })
}

/// Family key used for ordinal grouping: the catalog's own `family` when the
/// model is listed, otherwise the structural family of the id. Two models
/// group only when their keys agree exactly.
fn family_key_for(model_id: &str, evidence: &CatalogEvidence) -> String {
    if let Some(index) = evidence.models_index.by_id(model_id) {
        if let Some(family) = evidence
            .models_dev
            .as_ref()
            .and_then(|catalog| catalog.models.get(index))
            .and_then(|model| model.family.as_deref())
        {
            return family.to_string();
        }
    }
    family_of_id(model_id)
}

/// Same-family ordinal inheritance: a model without direct evidence inherits
/// only from *older* family members that have coding evidence, capped by
/// *newer* members' evidence. No floors means no claim (pending); date
/// stamps never create ordering; cheap variants cap at [`TIER_BALANCED`].
fn family_ordinal_tier(model_id: &str, evidence: &CatalogEvidence) -> Option<u8> {
    let catalog = evidence.models_dev.as_ref()?;
    let evalplus = evidence.evalplus.as_ref()?;
    let family = family_key_for(model_id, evidence);
    let members = evidence.models_index.by_family(&family);
    if members.is_empty() {
        return None;
    }
    let mut floor: Option<u8> = None;
    let mut ceiling: Option<u8> = None;
    for &index in members {
        let member: &ModelsDevModel = &catalog.models[index];
        if canonical_model_id(&member.id) == canonical_model_id(model_id) {
            continue;
        }
        let Some(coding) = coding_evidence_for(&member.id, evalplus) else {
            continue;
        };
        let anchor_tier = coding_band(coding.average);
        match version_relation(model_id, &member.id) {
            VersionRelation::Newer => {
                // The query may never inherit the anchor's top tier when it
                // introduces a cheap/experimental variant marker.
                let candidate = if introduces_variant(model_id, &member.id) {
                    anchor_tier.min(TIER_BALANCED)
                } else {
                    anchor_tier
                };
                floor = Some(floor.map_or(candidate, |current: u8| current.max(candidate)));
            }
            VersionRelation::Older => {
                ceiling = Some(ceiling.map_or(anchor_tier, |current: u8| current.min(anchor_tier)));
            }
            VersionRelation::Same | VersionRelation::Incomparable => {}
        }
    }
    let inherited = floor?;
    Some(match ceiling {
        Some(cap) => inherited.min(cap),
        None => inherited,
    })
}

/// Documented name/metadata inference for families the public evidence
/// catalogs have not ranked. Order inside: cheap-variant tokens (strongest,
/// most specific), flagship tokens, vendor flagship priors, then models.dev
/// chat-capable presence. Returns `(tier, human-readable prior)`.
fn name_inference_tier(model_id: &str, evidence: &CatalogEvidence) -> Option<(u8, String)> {
    use crate::services::model_discovery::rank::{
        family_key, is_low_tier_token, normalize_id, tokenize,
    };
    let norm = normalize_id(model_id);
    if norm.is_empty() {
        return None;
    }
    let tokens = tokenize(&norm);
    if tokens.iter().any(|token| is_low_tier_token(token)) {
        return Some((TIER_FAST, "cheap/experimental variant name token".into()));
    }
    if tokens
        .iter()
        .any(|token| FLAGSHIP_TOKENS.contains(&token.as_str()))
    {
        return Some((TIER_OPUS, "flagship name token".into()));
    }
    let family = family_key(&norm);
    if let Some((_, tier)) = VENDOR_TIER_PRIORS.iter().find(|(prior, _)| *prior == family) {
        return Some((*tier, format!("vendor flagship prior for {family}")));
    }
    // models.dev lists this exact id as a chat-capable model, but no ranking
    // catalog scored it: a standard-tier prior is defensible.
    if let Some(index) = evidence.models_index.by_id(model_id) {
        let chat_capable = evidence
            .models_dev
            .as_ref()
            .map(|catalog| &catalog.models[index])
            .is_some_and(|model| {
                model.tool_call.unwrap_or(false)
                    || model.output_modalities.iter().any(|m| m == "text")
            });
        if chat_capable {
            return Some((TIER_BALANCED, "models.dev chat-capable metadata".into()));
        }
    }
    None
}

/// The deterministic tier chain for one model id. See the module docs for
/// the exact precedence; unknown families end at `pending`.
pub(crate) fn evaluate_model(
    model_id: &str,
    manual_tier: Option<u8>,
    evidence: &CatalogEvidence,
) -> ModelAssessment {
    // A per-offer manual override always wins, even when another offer of
    // the same model uses the precomputed evidence-based baseline.
    if manual_tier.is_none_or(|tier| tier > TIER_FABLE) {
        if let Some(assessment) = evidence.baseline_assessments.get(model_id) {
            let mut assessment = assessment.clone();
            assessment.model_id = model_id.to_string();
            return assessment;
        }
    }
    evaluate_model_uncached(model_id, manual_tier, evidence)
}

fn evaluate_model_uncached(
    model_id: &str,
    manual_tier: Option<u8>,
    evidence: &CatalogEvidence,
) -> ModelAssessment {
    let canonical_id = canonical_model_id(model_id);
    let family = Some(family_key_for(model_id, evidence)).filter(|value| !value.is_empty());
    let role = role_tier_hint(model_id).or_else(|| family.as_deref().and_then(family_role_tier));

    let mut sources = Vec::new();
    let push_evalplus = |sources: &mut Vec<AssessmentSource>| {
        sources.push(AssessmentSource {
            source: evidence.evalplus_status.source.clone(),
            fetched_at: evidence.evalplus_status.fetched_at,
            stale: evidence.evalplus_status.stale,
            last_error: evidence.evalplus_status.last_error.clone(),
        });
    };
    let push_models_dev = |sources: &mut Vec<AssessmentSource>| {
        sources.push(AssessmentSource {
            source: evidence.models_dev_status.source.clone(),
            fetched_at: evidence.models_dev_status.fetched_at,
            stale: evidence.models_dev_status.stale,
            last_error: evidence.models_dev_status.last_error.clone(),
        });
    };

    // 1. Manual override always wins.
    if let Some(tier) = manual_tier.filter(|tier| *tier <= TIER_FABLE) {
        return ModelAssessment {
            model_id: model_id.to_string(),
            canonical_id,
            family,
            tier: Some(tier),
            origin: TierOrigin::Manual,
            basis: TierBasis::ManualOverride,
            confidence: Some(CONFIDENCE_MANUAL),
            condition: Some("manual tier override".into()),
            role_tier_hint: role,
            coding: None,
            arena: None,
            sources,
        };
    }

    // Arena scores are a declared routing proxy, not a universal intelligence
    // measurement. Only an exact, version-preserving identity match qualifies.
    if let Some(mut ranked) = evidence.arena_assessment_for(model_id) {
        if let Some((tier, total)) = evidence.arena_supply_tiers.get(&canonical_id) {
            ranked.tier = *tier;
            ranked.compared_offered_models = Some(*total);
            ranked.policy = "available_model_rank_quartiles_v1".into();
        }
        sources.push(AssessmentSource {
            source: evidence.arena_status.source.clone(),
            fetched_at: evidence.arena_status.fetched_at,
            stale: evidence.arena_status.stale,
            last_error: evidence.arena_status.last_error.clone(),
        });
        return ModelAssessment {
            model_id: model_id.to_string(),
            canonical_id,
            family,
            tier: Some(ranked.tier),
            origin: TierOrigin::Inferred,
            basis: TierBasis::ArenaEvidence,
            confidence: Some(if ranked.condition_ambiguous {
                CONFIDENCE_ARENA_AMBIGUOUS
            } else {
                CONFIDENCE_ARENA
            }),
            condition: Some(format!(
                "Arena {} overall {} rank {}/{}; {}",
                arena::SOURCE_CONFIG,
                ranked.published_date,
                ranked.rank,
                ranked.total_models,
                ranked.policy
            )),
            role_tier_hint: role,
            coding: None,
            arena: Some(ranked),
            sources,
        };
    }

    // Comparable public coding evidence remains a separate fallback.
    if let Some(evalplus) = evidence.evalplus.as_ref() {
        if let Some(coding) = coding_evidence_for(model_id, evalplus) {
            let ambiguous = coding.entry_keys.len() > 1;
            let tier = coding_band(coding.average);
            push_evalplus(&mut sources);
            return ModelAssessment {
                model_id: model_id.to_string(),
                canonical_id,
                family,
                tier: Some(tier),
                origin: TierOrigin::Inferred,
                basis: TierBasis::CodingEvidence,
                confidence: Some(if ambiguous {
                    CONFIDENCE_CODING_AMBIGUOUS
                } else {
                    CONFIDENCE_CODING
                }),
                condition: Some(format!(
                    "evalplus pass@1 humaneval+/mbpp+ prompted={}",
                    coding.prompted
                )),
                role_tier_hint: role,
                coding: Some(coding),
                arena: None,
                sources,
            };
        }
    }

    // 3. Same-family ordinal inheritance.
    if let Some(tier) = family_ordinal_tier(model_id, evidence) {
        push_evalplus(&mut sources);
        push_models_dev(&mut sources);
        let family_name = family.as_deref().unwrap_or("unknown").to_string();
        return ModelAssessment {
            model_id: model_id.to_string(),
            canonical_id,
            family,
            tier: Some(tier),
            origin: TierOrigin::Inferred,
            basis: TierBasis::FamilyOrdinal,
            confidence: Some(CONFIDENCE_FAMILY_ORDINAL),
            condition: Some(format!(
                "same-family ordinal inheritance within {family_name}"
            )),
            role_tier_hint: role,
            coding: None,
            arena: None,
            sources,
        };
    }

    // 4. Known-family role name hint only.
    if let Some(tier) = role {
        if family.is_some() {
            push_models_dev(&mut sources);
        }
        return ModelAssessment {
            model_id: model_id.to_string(),
            canonical_id,
            family,
            tier: Some(tier),
            origin: TierOrigin::Inferred,
            basis: TierBasis::FamilyRole,
            confidence: Some(CONFIDENCE_FAMILY_ROLE),
            condition: Some("known family role name".into()),
            role_tier_hint: role,
            coding: None,
            arena: None,
            sources,
        };
    }

    // 5. Documented name/metadata inference — the weakest accepted basis.
    if let Some((tier, prior)) = name_inference_tier(model_id, evidence) {
        if evidence.models_index.by_id(model_id).is_some() {
            push_models_dev(&mut sources);
        }
        return ModelAssessment {
            model_id: model_id.to_string(),
            canonical_id,
            family,
            tier: Some(tier),
            origin: TierOrigin::Inferred,
            basis: TierBasis::NameInference,
            confidence: Some(CONFIDENCE_NAME_INFERENCE),
            condition: Some(prior),
            role_tier_hint: role,
            coding: None,
            arena: None,
            sources,
        };
    }

    // 6. Pending: nothing reliable is known.
    ModelAssessment {
        model_id: model_id.to_string(),
        canonical_id,
        family,
        tier: None,
        origin: TierOrigin::Pending,
        basis: TierBasis::Pending,
        confidence: None,
        condition: None,
        role_tier_hint: role,
        coding: None,
        arena: None,
        sources,
    }
}

#[cfg(test)]
mod tests {
    use super::super::parse;
    use super::*;

    fn arena_cache_fixture(names: &[&str], date: &str) -> ArenaCatalog {
        let rows: Vec<_> = names.iter().enumerate().map(|(index, name)| serde_json::json!({"row": {
            "model_name":name,"rating":1500-index,"rating_lower":1490-index,"rating_upper":1510-index,
            "vote_count":1000,"rank":index+1,"category":"overall","leaderboard_publish_date":date
        }})).collect();
        arena::parse_arena(
            &serde_json::json!({"rows":rows,"num_rows_total":names.len()}).to_string(),
        )
        .unwrap()
    }

    fn arena_cache_status(stale: bool, fetched_at: i64) -> SourceStatus {
        SourceStatus {
            source: arena::SOURCE_ID.into(),
            fetched_at: Some(fetched_at),
            last_error: stale.then(|| "fetch_timeout".into()),
            stale,
        }
    }

    #[test]
    fn frozen_offered_assessments_preserve_public_evidence_wire_identity_and_known_misses() {
        let catalog = arena_cache_fixture(
            &[
                "rated-model-1",
                "rated-model-2",
                "rated-model-3",
                "rated-model-4",
            ],
            "2026-10-20",
        );
        let public_first = arena::assessment_for("rated-model-1", &catalog).unwrap();
        let evidence = evidence(None, None, false)
            .with_arena(Some(catalog), arena_cache_status(true, 1000))
            .with_offered_models(
                [
                    "rated-model-1",
                    "vendor/rated-model-1[1M]",
                    "rated-model-2",
                    "rated-model-3",
                    "rated-model-4",
                    "unknown-model",
                    "rated-model-1[unpublished-variant]",
                    "rated-model-1",
                ]
                .into_iter()
                .map(str::to_string)
                .collect(),
            );
        assert_eq!(evidence.offered_model_ids.len(), 7);
        assert_eq!(evidence.arena_assessments.len(), 7);
        assert_eq!(evidence.baseline_assessments.len(), 7);
        assert!(evidence.arena_assessments["unknown-model"].is_none());
        assert!(
            evidence.arena_assessments["rated-model-1[unpublished-variant]"].is_none(),
            "unknown brackets must not borrow a canonical collision's evidence"
        );
        assert_eq!(
            serde_json::to_value(
                evidence.arena_assessments["rated-model-1"]
                    .as_ref()
                    .unwrap()
            )
            .unwrap(),
            serde_json::to_value(public_first).unwrap()
        );
        for model in &evidence.offered_model_ids {
            let cached = evaluate_model(model, None, &evidence);
            let direct = evaluate_model_uncached(model, None, &evidence);
            assert_eq!(
                serde_json::to_value(&cached).unwrap(),
                serde_json::to_value(direct).unwrap()
            );
            assert_eq!(&cached.model_id, model);
        }
        let selected = evaluate_model("vendor/rated-model-1[1M]", None, &evidence);
        assert_eq!(selected.model_id, "vendor/rated-model-1[1M]");
        assert!(selected.sources[0].stale);
        assert_eq!(
            selected.sources[0].last_error.as_deref(),
            Some("fetch_timeout")
        );
        assert_eq!(selected.arena.unwrap().compared_offered_models, Some(4));
        assert_eq!(
            evaluate_model("rated-model-1[unpublished-variant]", None, &evidence).basis,
            TierBasis::Pending
        );
        let before = evidence.baseline_assessments.len();
        assert_eq!(
            evaluate_model("unconfigured-query", None, &evidence).basis,
            TierBasis::Pending
        );
        assert_eq!(
            evidence.baseline_assessments.len(),
            before,
            "UI queries cannot grow the immutable offer cache"
        );
    }

    #[test]
    fn cohort_and_arena_changes_rebuild_frozen_tiers_sources_and_misses() {
        let names = [
            "rated-model-1",
            "rated-model-2",
            "rated-model-3",
            "rated-model-4",
        ];
        let make_evidence = || {
            evidence(None, None, false)
                .with_arena(
                    Some(arena_cache_fixture(&names, "2026-10-20")),
                    arena_cache_status(true, 1000),
                )
                .with_offered_models(names.into_iter().map(str::to_string).collect())
        };
        let original = make_evidence();
        assert_eq!(
            evaluate_model("rated-model-4", None, &original).tier,
            Some(0)
        );
        let narrowed = original.with_offered_models(vec!["rated-model-4".into()]);
        let narrow = evaluate_model("rated-model-4", None, &narrowed);
        assert_eq!(narrow.tier, Some(3));
        assert_eq!(narrow.arena.unwrap().compared_offered_models, Some(1));
        assert_eq!(narrowed.baseline_assessments.len(), 1);
        assert_eq!(
            evaluate_model("rated-model-1", None, &narrowed)
                .arena
                .unwrap()
                .compared_offered_models,
            None,
            "removed members cannot retain stale cohort tiers"
        );
        let replaced = make_evidence().with_arena(
            Some(arena_cache_fixture(
                &[
                    "rated-model-4",
                    "rated-model-3",
                    "rated-model-2",
                    "rated-model-1",
                ],
                "2026-10-21",
            )),
            arena_cache_status(false, 2000),
        );
        let refreshed = evaluate_model("rated-model-4", None, &replaced);
        assert_eq!(refreshed.tier, Some(3));
        let ranked = refreshed.arena.unwrap();
        assert_eq!(ranked.rank, 1);
        assert_eq!(ranked.published_date, "2026-10-21");
        assert_eq!(ranked.compared_offered_models, Some(4));
        assert!(!refreshed.sources[0].stale);
        assert_eq!(refreshed.sources[0].fetched_at, Some(2000));
        let removed = replaced.with_arena(None, arena_cache_status(true, 2000));
        assert_eq!(
            evaluate_model("rated-model-4", None, &removed).basis,
            TierBasis::Pending
        );
        assert!(removed.arena_assessments.values().all(Option::is_none));
        assert_eq!(removed.baseline_assessments.len(), 4);
    }

    #[test]
    fn manual_overrides_win_over_frozen_baselines_and_cache_evalplus_misses() {
        let raw = evalplus_doc("scored-model", 90.0, 88.0);
        let evidence = evidence(None, Some(&raw), false)
            .with_offered_models(vec!["scored-model".into(), "unknown-model".into()]);
        let baseline = evaluate_model("scored-model", None, &evidence);
        assert_eq!(baseline.basis, TierBasis::CodingEvidence);
        assert_eq!(baseline.tier, Some(3));
        let manual = evaluate_model("scored-model", Some(0), &evidence);
        assert_eq!(manual.basis, TierBasis::ManualOverride);
        assert_eq!(manual.tier, Some(0));
        assert!(manual.coding.is_none());
        assert!(manual.arena.is_none());
        assert_eq!(
            serde_json::to_value(evaluate_model("scored-model", None, &evidence)).unwrap(),
            serde_json::to_value(&baseline).unwrap()
        );
        assert_eq!(
            serde_json::to_value(evaluate_model("scored-model", Some(4), &evidence)).unwrap(),
            serde_json::to_value(baseline).unwrap(),
            "invalid overrides retain the established evidence fallback"
        );
        assert_eq!(
            evidence.baseline_assessments["unknown-model"].basis,
            TierBasis::Pending
        );
        assert_eq!(evidence.arena_assessments.len(), 2);
    }

    #[test]
    fn arena_relative_supply_ranks_differentiate_roles_and_preserve_evidence() {
        let rows: Vec<_> = (1..=8)
            .map(|rank| {
                serde_json::json!({"row":{
                    "model_name":format!("rated-model-{rank}"),"rating":1500-rank,
                    "rating_lower":1490-rank,"rating_upper":1510-rank,"vote_count":1000,
                    "rank":rank,"category":"overall","leaderboard_publish_date":"2026-10-20"
                }})
            })
            .collect();
        let raw = serde_json::json!({"rows":rows,"num_rows_total":8}).to_string();
        let catalog = arena::parse_arena(&raw).unwrap();
        let evidence = evidence(None, None, false)
            .with_arena(
                Some(catalog),
                SourceStatus {
                    source: arena::SOURCE_ID.into(),
                    fetched_at: Some(1_000),
                    last_error: None,
                    stale: true,
                },
            )
            .with_offered_models((1..=4).map(|rank| format!("rated-model-{rank}")).collect());
        for (rank, tier) in [(1, 3), (2, 2), (3, 1), (4, 0)] {
            let assessment = evaluate_model(&format!("rated-model-{rank}"), None, &evidence);
            assert_eq!(assessment.tier, Some(tier));
            assert_eq!(assessment.basis, TierBasis::ArenaEvidence);
            let arena = assessment.arena.unwrap();
            assert_eq!(arena.rank, rank);
            assert_eq!(arena.total_models, 8);
            assert_eq!(arena.compared_offered_models, Some(4));
            assert_eq!(arena.policy, "available_model_rank_quartiles_v1");
            assert!(assessment.sources[0].stale);
        }
        assert_eq!(
            evaluate_model("rated-model-1", Some(0), &evidence).basis,
            TierBasis::ManualOverride
        );
        assert_eq!(evaluate_model("unknown-model", None, &evidence).tier, None);
    }

    fn evidence(
        models_json: Option<&str>,
        evalplus_json: Option<&str>,
        models_stale: bool,
    ) -> CatalogEvidence {
        let models_dev =
            models_json.map(|raw| parse::parse_models_dev(raw).expect("models fixture"));
        let evalplus =
            evalplus_json.map(|raw| parse::parse_evalplus(raw).expect("evalplus fixture"));
        CatalogEvidence::new(
            models_dev,
            SourceStatus {
                source: "models_dev".into(),
                fetched_at: Some(1_000),
                last_error: None,
                stale: models_stale,
            },
            evalplus,
            SourceStatus {
                source: "evalplus".into(),
                fetched_at: Some(1_000),
                last_error: None,
                stale: false,
            },
        )
    }

    fn evalplus_doc(key: &str, humaneval_plus: f64, mbpp_plus: f64) -> String {
        format!(
            r#"{{"{key}": {{"pass@1": {{"humaneval+": {humaneval_plus}, "mbpp+": {mbpp_plus}}}, "prompted": true}}}}"#
        )
    }

    #[test]
    fn manual_override_wins_over_everything() {
        let evidence = evidence(None, Some(&evalplus_doc("mystery-x", 99.0, 99.0)), false);
        let assessment = evaluate_model("mystery-x", Some(TIER_FAST), &evidence);
        assert_eq!(assessment.tier, Some(TIER_FAST));
        assert_eq!(assessment.origin, TierOrigin::Manual);
        assert_eq!(assessment.basis, TierBasis::ManualOverride);
        assert_eq!(assessment.confidence, Some(CONFIDENCE_MANUAL));
    }

    #[test]
    fn evalplus_scores_drive_coding_evidence_tiers_with_visible_conditions() {
        // avg 90.0 -> fable band, avg 75.0 -> opus band.
        let evalplus = r#"{
            "strong-model": {"pass@1": {"humaneval+": 92.0, "mbpp+": 88.0}, "prompted": true},
            "mid-model": {"pass@1": {"humaneval+": 80.0, "mbpp+": 70.0}, "prompted": true}
        }"#;
        let evidence = evidence(None, Some(evalplus), false);
        let strong = evaluate_model("strong-model", None, &evidence);
        assert_eq!(strong.tier, Some(TIER_FABLE));
        assert_eq!(strong.basis, TierBasis::CodingEvidence);
        assert_eq!(strong.confidence, Some(CONFIDENCE_CODING));
        assert_eq!(
            strong.condition.as_deref(),
            Some("evalplus pass@1 humaneval+/mbpp+ prompted=true")
        );
        assert_eq!(strong.coding.as_ref().unwrap().average, 90.0);
        let mid = evaluate_model("mid-model", None, &evidence);
        assert_eq!(mid.tier, Some(TIER_OPUS));
    }

    #[test]
    fn benchmark_keys_match_model_ids_across_spellings_and_org_prefixes() {
        let evidence = evidence(
            None,
            Some(&evalplus_doc("Claude Sonnet 3.5 (June 2024)", 81.7, 74.3)),
            false,
        );
        for id in [
            "claude-sonnet-3-5",
            "claude-sonnet-3.5",
            "Claude-Sonnet-3.5",
            "anthropic/claude-sonnet-3-5",
        ] {
            let assessment = evaluate_model(id, None, &evidence);
            assert_eq!(
                assessment.basis,
                TierBasis::CodingEvidence,
                "expected evidence match for {id}"
            );
            assert_eq!(assessment.coding.as_ref().unwrap().humaneval_plus, 81.7);
        }
        // A different version never borrows the 3.5 scores.
        let other = evaluate_model("claude-sonnet-4-5", None, &evidence);
        assert_ne!(other.basis, TierBasis::CodingEvidence);
    }

    #[test]
    fn duplicate_benchmark_rows_pick_the_newest_condition_and_note_ambiguity() {
        let evalplus = r#"{
            "GPT-4-Turbo (April 2024)": {"pass@1": {"humaneval+": 86.0, "mbpp+": 73.0}, "prompted": true},
            "GPT-4-Turbo (Nov 2023)": {"pass@1": {"humaneval+": 81.7, "mbpp+": 73.3}, "prompted": true}
        }"#;
        let evidence = evidence(None, Some(evalplus), false);
        let assessment = evaluate_model("gpt-4-turbo", None, &evidence);
        let coding = assessment.coding.as_ref().expect("coding evidence");
        assert_eq!(coding.humaneval_plus, 86.0);
        assert_eq!(coding.entry_keys.len(), 2);
        assert_eq!(assessment.confidence, Some(CONFIDENCE_CODING_AMBIGUOUS));
    }

    #[test]
    fn family_ordinal_inheritance_stays_inside_the_family_and_respects_direction() {
        let models = r#"{
            "p": {"models": {
                "claude-sonnet-3-5": {"id": "claude-sonnet-3-5", "family": "claude-sonnet"},
                "claude-sonnet-5-5": {"id": "claude-sonnet-5-5", "family": "claude-sonnet"},
                "claude-sonnet-2-1": {"id": "claude-sonnet-2-1", "family": "claude-sonnet"},
                "claude-haiku-4-5": {"id": "claude-haiku-4-5", "family": "claude-haiku"},
                "claude-mythos-7": {"id": "claude-mythos-7", "family": "claude-mythos"}
            }}
        }"#;
        let evalplus = evalplus_doc("Claude Sonnet 3.5 (June 2024)", 81.7, 74.3);
        let evidence = evidence(Some(models), Some(&evalplus), false);

        // Newer same-family member inherits a floor from the evidence.
        let newer = evaluate_model("claude-sonnet-5-5", None, &evidence);
        assert_eq!(newer.basis, TierBasis::FamilyOrdinal);
        assert_eq!(newer.tier, Some(TIER_OPUS));
        assert_eq!(newer.confidence, Some(CONFIDENCE_FAMILY_ORDINAL));

        // Older member cannot inherit upward from a newer anchor.
        let older = evaluate_model("claude-sonnet-2-1", None, &evidence);
        assert_eq!(older.basis, TierBasis::FamilyRole);
        assert_eq!(older.tier, Some(TIER_BALANCED));

        // Different families never share ordinal evidence.
        let haiku = evaluate_model("claude-haiku-4-5", None, &evidence);
        assert_eq!(haiku.basis, TierBasis::FamilyRole);
        assert_eq!(haiku.tier, Some(TIER_FAST));

        // Unknown role family is never guessed.
        let mythos = evaluate_model("claude-mythos-7", None, &evidence);
        assert_eq!(mythos.tier, None);
        assert_eq!(mythos.basis, TierBasis::Pending);
    }

    #[test]
    fn mini_and_flash_variants_never_inherit_the_anchor_unchecked() {
        let models = r#"{
            "p": {"models": {
                "gemini-pro-3-5": {"id": "gemini-pro-3-5", "family": "gemini-pro"},
                "gemini-pro-3-5-flash": {"id": "gemini-pro-3-5-flash", "family": "gemini-pro"}
            }}
        }"#;
        let evalplus = evalplus_doc("Gemini Pro 3.5", 90.0, 88.0);
        let evidence = evidence(Some(models), Some(&evalplus), false);
        let flash = evaluate_model("gemini-pro-3-5-flash", None, &evidence);
        assert_eq!(flash.basis, TierBasis::FamilyOrdinal);
        assert_eq!(flash.tier, Some(TIER_BALANCED));
    }

    #[test]
    fn dated_snapshots_evaluate_as_the_same_release() {
        let evidence = evidence(
            None,
            Some(&evalplus_doc("Claude Sonnet 3.5 (June 2024)", 81.7, 74.3)),
            false,
        );
        let dated = evaluate_model("claude-sonnet-3-5-20250601", None, &evidence);
        assert_eq!(dated.basis, TierBasis::CodingEvidence);
        assert_eq!(dated.tier, Some(TIER_OPUS));
    }

    #[test]
    fn known_family_role_hints_carry_low_confidence_and_unknown_families_stay_pending() {
        let evidence = evidence(None, None, false);
        let opus = evaluate_model("claude-opus-4-8", None, &evidence);
        assert_eq!(opus.basis, TierBasis::FamilyRole);
        assert_eq!(opus.tier, Some(TIER_OPUS));
        assert_eq!(opus.confidence, Some(CONFIDENCE_FAMILY_ROLE));

        // New alias-role versions are recognized…
        let fable = evaluate_model("claude-fable-5-20260607", None, &evidence);
        assert_eq!(fable.tier, Some(TIER_FABLE));
        // …but unknown families without a documented prior stay pending.
        for id in ["mystery-x9", "claude-mythos-7"] {
            let assessment = evaluate_model(id, None, &evidence);
            assert_eq!(assessment.tier, None, "must stay pending: {id}");
            assert_eq!(assessment.basis, TierBasis::Pending);
        }
    }

    #[test]
    fn name_inference_assigns_documented_priors_below_evidence_confidence() {
        let evidence = evidence(None, None, false);
        // Vendor flagship prior.
        let glm = evaluate_model("glm-5.3", None, &evidence);
        assert_eq!(glm.tier, Some(TIER_OPUS));
        assert_eq!(glm.basis, TierBasis::NameInference);
        assert_eq!(glm.confidence, Some(CONFIDENCE_NAME_INFERENCE));
        assert!(glm.condition.as_deref().is_some_and(|c| c.contains("glm")));
        // Flagship token prior.
        let flagship = evaluate_model("vendor-x9-max", None, &evidence);
        assert_eq!(flagship.tier, Some(TIER_OPUS));
        assert_eq!(flagship.basis, TierBasis::NameInference);
        // Cheap variants of a prior family divert to fast.
        let flash = evaluate_model("glm-5.3-flashx", None, &evidence);
        assert_eq!(flash.tier, Some(TIER_FAST));
        assert_eq!(flash.basis, TierBasis::NameInference);
        // No prior, no tokens, no metadata: pending stays fail-closed.
        assert_eq!(evaluate_model("mystery-x9", None, &evidence).basis, TierBasis::Pending);
    }

    #[test]
    fn models_dev_chat_presence_infers_balanced_without_borrowing_rankings() {
        let models = r#"{"fixture": {"models": {
            "listed-chat": {"id": "listed-chat", "family": "chatline", "tool_call": true},
            "listed-embed": {"id": "listed-embed", "family": "embedline"}
        }}}"#;
        let evidence = evidence(Some(models), None, false);
        let chat = evaluate_model("listed-chat", None, &evidence);
        assert_eq!(chat.tier, Some(TIER_BALANCED));
        assert_eq!(chat.basis, TierBasis::NameInference);
        // Non-chat metadata presence never earns a tier.
        let embed = evaluate_model("listed-embed", None, &evidence);
        assert_eq!(embed.tier, None);
        assert_eq!(embed.basis, TierBasis::Pending);
    }

    #[test]
    fn selector_namespaces_disambiguate_aliases_from_real_models() {
        assert_eq!(
            classify_selector("fable").unwrap(),
            Selector::Alias {
                name: "fable".into(),
                tier: TIER_FABLE
            }
        );
        assert_eq!(
            classify_selector("claude-opus-4-8").unwrap(),
            Selector::Alias {
                name: "claude-opus-4-8".into(),
                tier: TIER_OPUS
            }
        );
        // New alias-role spellings resolve too.
        assert_eq!(
            classify_selector("Claude-Fable-5-20260607").unwrap(),
            Selector::Alias {
                name: "Claude-Fable-5-20260607".into(),
                tier: TIER_FABLE
            }
        );
        // A real model that shares an alias's spelling stays reachable and
        // precise through the stable namespace.
        assert_eq!(
            classify_selector("precise:fable").unwrap(),
            Selector::Precise {
                model_id: "fable".into()
            }
        );
        assert_eq!(
            classify_selector("precise:claude-opus-4-8").unwrap(),
            Selector::Precise {
                model_id: "claude-opus-4-8".into()
            }
        );
        // Non-conflicting model names remain precise without ceremony.
        assert_eq!(
            classify_selector("minimax-m3").unwrap(),
            Selector::Precise {
                model_id: "minimax-m3".into()
            }
        );
        assert_eq!(
            classify_selector("alias:sonnet").unwrap(),
            Selector::Alias {
                name: "sonnet".into(),
                tier: TIER_BALANCED
            }
        );
        assert!(classify_selector("alias:minimax-m3").is_err());
        assert!(classify_selector("precise:").is_err());
        assert!(classify_selector("").is_err());
    }

    #[test]
    fn coding_bands_are_monotonic_and_bounded() {
        assert_eq!(coding_band(100.0), TIER_FABLE);
        assert_eq!(coding_band(80.0), TIER_FABLE);
        assert_eq!(coding_band(79.9), TIER_OPUS);
        assert_eq!(coding_band(70.0), TIER_OPUS);
        assert_eq!(coding_band(69.9), TIER_BALANCED);
        assert_eq!(coding_band(55.0), TIER_BALANCED);
        assert_eq!(coding_band(54.9), TIER_FAST);
        assert_eq!(coding_band(0.0), TIER_FAST);
    }
}
