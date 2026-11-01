//! Normalized allocation policy. Catalog prices are reference prices, not supplier quotes.
use super::{
    select::OfferedModel,
    tiers::{evaluate_model, CatalogEvidence},
};
use serde::{Deserialize, Serialize};

pub(crate) const POLICY_SETTING: &str = "model_utility_policy";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Utility_policy {
    pub quality_weight: f64,
    pub cost_weight: f64,
    pub freshness_weight: f64,
    pub suitability_weight: f64,
    pub freshness_half_life_days: f64,
    pub output_token_ratio: f64,
    pub fast_reference_cost_ceiling: f64,
    /// Deprecated compatibility field. Accepted persisted policies keep loading,
    /// but this former allocation cap never removes eligible fallback models.
    pub models_per_supplier: usize,
}

impl Default for Utility_policy {
    fn default() -> Self {
        Self {
            quality_weight: 0.50,
            cost_weight: 0.20,
            freshness_weight: 0.15,
            suitability_weight: 0.15,
            freshness_half_life_days: 180.0,
            output_token_ratio: 3.0,
            fast_reference_cost_ceiling: 3.0,
            models_per_supplier: 2,
        }
    }
}

impl Utility_policy {
    pub(crate) fn parse(raw: &str) -> Result<Self, &'static str> {
        let policy: Self = serde_json::from_str(raw).map_err(|_| "model_utility_policy_invalid")?;
        let weights = [
            policy.quality_weight,
            policy.cost_weight,
            policy.freshness_weight,
            policy.suitability_weight,
        ];
        if weights
            .iter()
            .any(|value| !value.is_finite() || !(0.0..=1.0).contains(value))
            || weights.iter().sum::<f64>() <= 0.0
            || !policy.freshness_half_life_days.is_finite()
            || !(1.0..=3650.0).contains(&policy.freshness_half_life_days)
            || !policy.output_token_ratio.is_finite()
            || !(0.0..=100.0).contains(&policy.output_token_ratio)
            || !policy.fast_reference_cost_ceiling.is_finite()
            || !(0.0..=1000.0).contains(&policy.fast_reference_cost_ceiling)
            || !(1..=2).contains(&policy.models_per_supplier)
        {
            return Err("model_utility_policy_invalid");
        }
        Ok(policy)
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Utility_score {
    pub total: f64,
    pub quality: f64,
    pub cost_efficiency: Option<f64>,
    pub freshness: Option<f64>,
    pub suitability: f64,
    pub reference_cost: Option<f64>,
    pub metadata_stale: bool,
}

pub(crate) fn score(
    offer: &OfferedModel,
    alias_tier: u8,
    evidence: &CatalogEvidence,
    needs_tools: bool,
) -> Utility_score {
    let policy = &evidence.utility_policy;
    let assessment = evaluate_model(&offer.model_id, offer.tier_override, evidence);
    // Prices and release versions require exact identity. Capability aliases
    // may normalize a variant, but that does not establish its price.
    let metadata = evidence.models_dev.as_ref().and_then(|catalog|
        evidence.models_index.by_exact_id(&offer.model_id).and_then(|index| catalog.models.get(index)));
    let reference_cost = metadata.and_then(|model| {
        Some(model.cost_input_per_mtok? + policy.output_token_ratio * model.cost_output_per_mtok?)
    });
    let cost_efficiency = reference_cost.map(|cost| 1.0 / (1.0 + cost));
    let freshness = metadata
        .and_then(|model| model.release_date.as_deref())
        .and_then(|date| {
            // Month-only dates stay unknown rather than inventing a release day.
            let date = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
            let age = (chrono::Utc::now().date_naive() - date).num_days();
            (age >= 0).then(|| 2f64.powf(-(age as f64) / policy.freshness_half_life_days))
        });
    let quality = assessment
        .arena
        .as_ref()
        .map(|arena| 1.0 - (arena.rank.saturating_sub(1) as f64 / arena.total_models.max(1) as f64))
        .or_else(|| {
            assessment
                .coding
                .as_ref()
                .map(|coding| coding.average / 100.0)
        })
        .unwrap_or(f64::from(assessment.tier.unwrap_or(0) + 1) / 4.0)
        .clamp(0.0, 1.0);
    let suitability = if needs_tools {
        assessment
            .coding
            .as_ref()
            .map(|coding| coding.average / 100.0)
            .unwrap_or(if offer.supports_tools == Some(true) {
                0.75
            } else {
                0.0
            })
    } else {
        quality
    };
    // Fast/review tasks weight cost more; premium roles retain quality as primary evidence.
    let cost_weight = policy.cost_weight * if alias_tier == 0 { 3.0 } else { 1.0 };
    let total_weight =
        policy.quality_weight + cost_weight + policy.freshness_weight + policy.suitability_weight;
    let total = (quality * policy.quality_weight
        + cost_efficiency.unwrap_or(0.0) * cost_weight
        + freshness.unwrap_or(0.0) * policy.freshness_weight
        + suitability * policy.suitability_weight)
        / total_weight;
    Utility_score {
        total,
        quality,
        cost_efficiency,
        freshness,
        suitability,
        reference_cost,
        metadata_stale: evidence.models_dev_status.stale,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn policy_rejects_unknown_fields_and_unbounded_weights() {
        assert!(Utility_policy::parse(r#"{"quality_weight":-1}"#).is_err());
        assert!(Utility_policy::parse(r#"{"models_per_supplier":100}"#).is_err());
        assert!(Utility_policy::parse(r#"{"invented":1}"#).is_err());
        assert!(Utility_policy::parse("{}").is_ok());
    }
}
