//! Comparable LMArena overall text rankings, parsed without network or storage.
//!
//! The fetch owner supplies the complete `latest` / `text_style_control`
//! overall snapshot. A partial page, mixed category/date, or malformed row
//! rejects the document. Rank quartiles are an operational routing policy
//! relative to that published snapshot, not a scientific capability claim.
//! Scores, intervals, votes and evaluation conditions retain their provenance.

use super::identity::{evidence_key_tokens, is_date_like_token};
use super::parse::{ParseError, MAX_DOCUMENT_BYTES};
use crate::services::model_discovery::rank::tokenize;
use chrono::NaiveDate;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeSet;

pub(crate) const SOURCE_ID: &str = "lmarena";
pub(crate) const SOURCE_URL: &str =
    "https://huggingface.co/datasets/lmarena-ai/leaderboard-dataset";
pub(crate) const SOURCE_CONFIG: &str = "text_style_control";
pub(crate) const SOURCE_SPLIT: &str = "latest";
pub(crate) const CATEGORY: &str = "overall";
pub(crate) const TIER_POLICY: &str = "published_rank_quartiles_v1";
const MAX_ENTRIES: usize = 20_000;
const MAX_MODEL_NAME_CHARS: usize = 256;
const TIER_COUNT: u64 = 4;
const MAX_EXACT_FLOAT_INTEGER: f64 = 9_007_199_254_740_992.0;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ArenaEntry {
    pub model_name: String,
    pub rating: f64,
    pub rating_lower: f64,
    pub rating_upper: f64,
    pub vote_count: u64,
    pub rank: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ArenaCatalog {
    pub entries: Vec<ArenaEntry>,
    pub published_date: String,
    pub category: String,
    pub source_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ArenaAssessment {
    pub model_name: String,
    pub published_date: String,
    pub category: String,
    pub source_id: String,
    pub source_url: String,
    pub source_config: String,
    pub source_split: String,
    pub policy: String,
    pub rank: u32,
    pub total_models: usize,
    pub rating: f64,
    pub rating_lower: f64,
    pub rating_upper: f64,
    pub vote_count: u64,
    pub condition: Option<String>,
    pub condition_ambiguous: bool,
    pub tier: u8,
    pub compared_offered_models: Option<usize>,
}

/// Parse a complete Dataset Viewer response assembled from every filtered page.
/// Calendar validity is checked here; the fetch/cache owner supplies freshness
/// policy. `parse_arena_at` additionally checks an explicit observation date.
pub(crate) fn parse_arena(raw: &str) -> Result<ArenaCatalog, ParseError> {
    if raw.len() > MAX_DOCUMENT_BYTES {
        return Err(ParseError::TooLarge);
    }
    let root: Value = serde_json::from_str(raw).map_err(|_| ParseError::Decode)?;
    let rows = root
        .get("rows")
        .and_then(Value::as_array)
        .ok_or(ParseError::ShapeInvalid)?;
    let total = root
        .get("num_rows_total")
        .and_then(Value::as_u64)
        .ok_or(ParseError::ShapeInvalid)?;
    if rows.len() > MAX_ENTRIES || total > MAX_ENTRIES as u64 {
        return Err(ParseError::CountExceeded);
    }
    if rows.is_empty() || total != rows.len() as u64 {
        return Err(ParseError::ShapeInvalid);
    }
    let mut entries = Vec::with_capacity(rows.len());
    let mut published_date = None;
    let mut identities = BTreeSet::new();
    for row in rows {
        let row = row.get("row").ok_or(ParseError::RecordInvalid)?;
        let (entry, date) = parse_entry(row, rows.len())?;
        if published_date.as_ref().is_some_and(|prior| prior != &date) {
            return Err(ParseError::DateInvalid);
        }
        let label = model_label(&entry.model_name).ok_or(ParseError::RecordInvalid)?;
        if !identities.insert((label.tokens, label.condition)) {
            return Err(ParseError::RecordInvalid);
        }
        published_date = Some(date);
        entries.push(entry);
    }
    entries.sort_by(|left, right| {
        left.rank
            .cmp(&right.rank)
            .then_with(|| left.model_name.cmp(&right.model_name))
    });
    Ok(ArenaCatalog {
        entries,
        published_date: published_date.ok_or(ParseError::ShapeInvalid)?,
        category: CATEGORY.into(),
        source_id: SOURCE_ID.into(),
    })
}

/// Pure observation-time validation, with no implicit clock or freshness claim.
pub(crate) fn parse_arena_at(
    raw: &str,
    observed_date: NaiveDate,
) -> Result<ArenaCatalog, ParseError> {
    let catalog = parse_arena(raw)?;
    let published = parse_date(&catalog.published_date)?;
    if published > observed_date {
        return Err(ParseError::DateInvalid);
    }
    Ok(catalog)
}

fn parse_entry(row: &Value, total_models: usize) -> Result<(ArenaEntry, String), ParseError> {
    let model_name = string_field(row, "model_name")?;
    if model_name.is_empty()
        || model_name.chars().count() > MAX_MODEL_NAME_CHARS
        || model_name.chars().any(char::is_control)
    {
        return Err(ParseError::RecordInvalid);
    }
    if string_field(row, "category")? != CATEGORY {
        return Err(ParseError::RecordInvalid);
    }
    let date = string_field(row, "leaderboard_publish_date")?;
    parse_date(date)?;
    let rating = finite_number(row, "rating")?;
    let rating_lower = finite_number(row, "rating_lower")?;
    let rating_upper = finite_number(row, "rating_upper")?;
    if rating_lower > rating || rating > rating_upper {
        return Err(ParseError::NumericOutOfBounds);
    }
    let vote_count = vote_count(row.get("vote_count").ok_or(ParseError::RecordInvalid)?)?;
    let rank = row
        .get("rank")
        .and_then(Value::as_u64)
        .filter(|rank| *rank > 0 && *rank <= total_models as u64)
        .ok_or(ParseError::NumericOutOfBounds)?;
    Ok((
        ArenaEntry {
            model_name: model_name.into(),
            rating,
            rating_lower,
            rating_upper,
            vote_count,
            rank: rank as u32,
        },
        date.into(),
    ))
}

fn string_field<'a>(row: &'a Value, name: &str) -> Result<&'a str, ParseError> {
    row.get(name)
        .and_then(Value::as_str)
        .ok_or(ParseError::RecordInvalid)
}

fn finite_number(row: &Value, name: &str) -> Result<f64, ParseError> {
    row.get(name)
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
        .ok_or(ParseError::NumericOutOfBounds)
}

fn vote_count(value: &Value) -> Result<u64, ParseError> {
    if let Some(votes) = value.as_u64() {
        return Ok(votes);
    }
    // Dataset Viewer exposes this source column as float64. Admit only exact
    // whole counts; a fractional or imprecisely large value is never rounded.
    value
        .as_f64()
        .filter(|votes| {
            votes.is_finite()
                && *votes >= 0.0
                && votes.fract() == 0.0
                && *votes <= MAX_EXACT_FLOAT_INTEGER
        })
        .map(|votes| votes as u64)
        .ok_or(ParseError::NumericOutOfBounds)
}

fn parse_date(raw: &str) -> Result<NaiveDate, ParseError> {
    if raw.len() != 10 {
        return Err(ParseError::DateInvalid);
    }
    let date = NaiveDate::parse_from_str(raw, "%Y-%m-%d").map_err(|_| ParseError::DateInvalid)?;
    if date.format("%Y-%m-%d").to_string() != raw {
        return Err(ParseError::DateInvalid);
    }
    Ok(date)
}

/// Published ordinal rank determines the routing band. With N divisible by
/// four, ranks 1..N/4 receive tier 3, then tiers 2, 1 and 0. Small snapshots
/// round each boundary upward. An invalid rank never becomes a default tier.
pub(crate) fn tier_for_entry(entry: &ArenaEntry, total_models: usize) -> Option<u8> {
    if total_models == 0 || entry.rank == 0 || entry.rank as usize > total_models {
        return None;
    }
    let quartile = (u64::from(entry.rank) - 1) * TIER_COUNT / total_models as u64;
    Some((TIER_COUNT - 1 - quartile) as u8)
}

/// Match exact identity tokens, preserving versions, variants and release dates.
/// A known organization prefix and punctuation/case differences are tolerated.
/// An unqualified model with multiple published thinking conditions uses the
/// worst rank (then lower interval/rating), retaining its condition ambiguity.
pub(crate) fn assessment_for(model_id: &str, catalog: &ArenaCatalog) -> Option<ArenaAssessment> {
    let query = model_label(model_id)?;
    let mut candidates: Vec<_> = catalog
        .entries
        .iter()
        .filter_map(|entry| model_label(&entry.model_name).map(|label| (entry, label)))
        .filter(|(_, label)| label.tokens == query.tokens)
        .collect();
    let condition = if query.condition.is_none() && candidates.is_empty() {
        let qualified = suffix_condition(&query)?;
        candidates = catalog
            .entries
            .iter()
            .filter_map(|entry| model_label(&entry.model_name).map(|label| (entry, label)))
            .filter(|(_, label)| label.tokens == qualified.tokens)
            .collect();
        qualified.condition
    } else {
        query.condition
    };
    if let Some(condition) = &condition {
        candidates.retain(|(_, label)| label.condition.as_ref() == Some(condition));
    }
    let condition_ambiguous = condition.is_none()
        && candidates
            .iter()
            .any(|(_, label)| label.condition.is_some());
    candidates.sort_by(|(left, _), (right, _)| {
        right
            .rank
            .cmp(&left.rank)
            .then_with(|| left.rating_lower.total_cmp(&right.rating_lower))
            .then_with(|| left.rating.total_cmp(&right.rating))
            .then_with(|| left.model_name.cmp(&right.model_name))
    });
    let (entry, label) = candidates.first()?;
    Some(ArenaAssessment {
        model_name: entry.model_name.clone(),
        published_date: catalog.published_date.clone(),
        category: catalog.category.clone(),
        source_id: catalog.source_id.clone(),
        source_url: SOURCE_URL.into(),
        source_config: SOURCE_CONFIG.into(),
        source_split: SOURCE_SPLIT.into(),
        policy: TIER_POLICY.into(),
        rank: entry.rank,
        total_models: catalog.entries.len(),
        rating: entry.rating,
        rating_lower: entry.rating_lower,
        rating_upper: entry.rating_upper,
        vote_count: entry.vote_count,
        condition: label.condition.clone(),
        condition_ambiguous,
        tier: tier_for_entry(entry, catalog.entries.len())?,
        compared_offered_models: None,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ModelLabel {
    tokens: Vec<String>,
    condition: Option<String>,
}

fn model_label(raw: &str) -> Option<ModelLabel> {
    let raw = raw.trim();
    if raw.is_empty()
        || raw.chars().count() > MAX_MODEL_NAME_CHARS
        || raw.chars().any(char::is_control)
    {
        return None;
    }
    let raw = match raw.rsplit_once('/') {
        Some((prefix, name))
            if !prefix.is_empty()
                && prefix
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || "-_.@/".contains(ch)) =>
        {
            name
        }
        _ => raw,
    };
    let mut identity = String::new();
    let mut conditions = BTreeSet::new();
    let mut chars = raw.chars();
    while let Some(ch) = chars.next() {
        if ch == '(' || ch == '[' {
            let closing = if ch == '(' { ')' } else { ']' };
            let mut group = String::new();
            let mut found = false;
            for next in chars.by_ref() {
                if next == closing {
                    found = true;
                    break;
                }
                if matches!(next, '(' | ')' | '[' | ']') {
                    return None;
                }
                group.push(next);
            }
            if !found {
                return None;
            }
            if ch == '(' {
                if let Some(condition) = condition_name(&group) {
                    conditions.insert(condition);
                    continue;
                }
            } else if is_context_marker(&group) {
                continue;
            }
            // Unknown parentheses and brackets may carry a model revision or
            // variant. Keep their contents instead of dropping that identity.
            identity.push(' ');
            identity.push_str(&group);
            identity.push(' ');
        } else if ch == ')' || ch == ']' {
            return None;
        } else {
            identity.push(ch);
        }
    }
    let identity = identity.to_lowercase();
    let all_tokens = tokenize(&identity);
    let tokens = if all_tokens.iter().any(|token| is_date_like_token(token)) {
        // The shared evidence helper intentionally strips dates for older
        // benchmarks. Arena matching keeps every published release token.
        all_tokens
    } else {
        evidence_key_tokens(&identity)
    };
    if tokens.is_empty() {
        return None;
    }
    Some(ModelLabel {
        tokens,
        condition: (!conditions.is_empty())
            .then(|| conditions.into_iter().collect::<Vec<_>>().join("+")),
    })
}

fn is_context_marker(raw: &str) -> bool {
    let raw = raw.trim().to_ascii_lowercase();
    raw.strip_suffix('k')
        .or_else(|| raw.strip_suffix('m'))
        .is_some_and(|amount| !amount.is_empty() && amount.bytes().all(|ch| ch.is_ascii_digit()))
}

fn condition_name(raw: &str) -> Option<String> {
    let words = tokenize(&raw.trim().to_ascii_lowercase());
    let normalized = words.join("-");
    match normalized.as_str() {
        "high" | "medium" | "low" | "max" | "minimal" | "none" | "off" | "thinking"
        | "reasoning" => Some(normalized),
        "non-thinking" | "no-thinking" => Some("non-thinking".into()),
        "non-reasoning" | "no-reasoning" => Some("non-reasoning".into()),
        "thinking-high" | "thinking-medium" | "thinking-low" | "thinking-max"
        | "reasoning-high" | "reasoning-medium" | "reasoning-low" | "reasoning-max" => {
            Some(normalized)
        }
        _ => None,
    }
}

fn suffix_condition(label: &ModelLabel) -> Option<ModelLabel> {
    for length in [2, 1] {
        if label.tokens.len() <= length {
            continue;
        }
        let split = label.tokens.len() - length;
        if let Some(condition) = condition_name(&label.tokens[split..].join("-")) {
            return Some(ModelLabel {
                tokens: label.tokens[..split].to_vec(),
                condition: Some(condition),
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn row(name: &str, rank: u32) -> Value {
        json!({"row":{
            "model_name":name,"rating":1500.0 - f64::from(rank),
            "rating_lower":1490.0 - f64::from(rank),"rating_upper":1510.0 - f64::from(rank),
            "vote_count":1000,"rank":rank,"category":"overall","leaderboard_publish_date":"2026-10-20"
        }})
    }

    fn document(rows: Vec<Value>) -> String {
        json!({"num_rows_total":rows.len(),"rows":rows}).to_string()
    }

    #[test]
    fn complete_snapshot_retains_ranking_and_provenance() {
        let catalog = parse_arena(&document(vec![row("Model B", 2), row("Model A", 1)])).unwrap();
        assert_eq!(catalog.published_date, "2026-10-20");
        assert_eq!(catalog.category, CATEGORY);
        assert_eq!(catalog.source_id, SOURCE_ID);
        assert_eq!(catalog.entries[0].model_name, "Model A");
        let assessment = assessment_for("model-a", &catalog).unwrap();
        assert_eq!(assessment.vote_count, 1000);
        assert_eq!(assessment.total_models, 2);
        assert_eq!(assessment.rating_lower, 1489.0);
        let serialized = serde_json::to_value(assessment).unwrap();
        assert_eq!(serialized["publishedDate"], "2026-10-20");
        assert_eq!(serialized["sourceConfig"], "text_style_control");
        assert_eq!(serialized["policy"], TIER_POLICY);
    }

    #[test]
    fn partial_pages_empty_snapshots_and_duplicate_identity_are_rejected() {
        let mut partial: Value = serde_json::from_str(&document(vec![row("Model A", 1)])).unwrap();
        partial["num_rows_total"] = json!(2);
        assert_eq!(
            parse_arena(&partial.to_string()).unwrap_err(),
            ParseError::ShapeInvalid
        );
        assert_eq!(
            parse_arena(&document(vec![])).unwrap_err(),
            ParseError::ShapeInvalid
        );
        assert_eq!(
            parse_arena(&document(vec![row("Model A", 1), row("model-a", 1)])).unwrap_err(),
            ParseError::RecordInvalid
        );
    }

    #[test]
    fn malformed_row_rejects_the_entire_otherwise_valid_snapshot() {
        for (field, invalid) in [
            ("rating", json!("NaN")),
            ("rating_lower", json!(2000)),
            ("rating_upper", json!(-2000)),
            ("vote_count", json!(-1)),
            ("vote_count", json!(1.5)),
            ("rank", json!(0)),
            ("rank", json!(3)),
        ] {
            let mut invalid_row = row("Model B", 2);
            invalid_row["row"][field] = invalid;
            assert_eq!(
                parse_arena(&document(vec![row("Model A", 1), invalid_row])).unwrap_err(),
                ParseError::NumericOutOfBounds,
                "field={field}"
            );
        }
        assert!(
            parse_arena("{\"rows\":[{\"row\":{\"rating\":1e400}}],\"num_rows_total\":1}").is_err()
        );
    }

    #[test]
    fn mixed_categories_and_publication_dates_do_not_form_one_comparable_board() {
        let mut category = row("Model B", 2);
        category["row"]["category"] = json!("coding");
        assert_eq!(
            parse_arena(&document(vec![row("Model A", 1), category])).unwrap_err(),
            ParseError::RecordInvalid
        );
        let mut date = row("Model B", 2);
        date["row"]["leaderboard_publish_date"] = json!("2026-10-19");
        assert_eq!(
            parse_arena(&document(vec![row("Model A", 1), date])).unwrap_err(),
            ParseError::DateInvalid
        );
    }

    #[test]
    fn invalid_and_future_dates_are_rejected_with_explicit_observation_time() {
        let mut invalid = row("Model A", 1);
        invalid["row"]["leaderboard_publish_date"] = json!("2026-02-30");
        assert_eq!(
            parse_arena(&document(vec![invalid])).unwrap_err(),
            ParseError::DateInvalid
        );
        let raw = document(vec![row("Model A", 1)]);
        assert_eq!(
            parse_arena_at(&raw, NaiveDate::from_ymd_opt(2026, 10, 19).unwrap()).unwrap_err(),
            ParseError::DateInvalid
        );
        assert!(parse_arena_at(&raw, NaiveDate::from_ymd_opt(2026, 10, 20).unwrap()).is_ok());
    }

    #[test]
    fn case_punctuation_context_and_optional_org_prefix_match_exact_tokens() {
        let catalog = parse_arena(&document(vec![row("Claude Opus 5.1", 1)])).unwrap();
        for model in [
            "claude-opus-5-1",
            "CLAUDE.OPUS.5.1",
            "anthropic/claude-opus-5.1[1M]",
            "@cf/anthropic/claude-opus-5.1",
        ] {
            assert!(assessment_for(model, &catalog).is_some(), "{model}");
        }
    }

    #[test]
    fn different_versions_variants_dates_and_routing_suffixes_never_fuzzily_match() {
        let catalog = parse_arena(&document(vec![row("GLM 5.3", 1)])).unwrap();
        for model in [
            "glm-5.2",
            "glm-5.3-flash",
            "glm-5.3-flashx",
            "glm-5.3-20261020",
            "glm-5.3:free",
            "unknown-model",
        ] {
            assert!(assessment_for(model, &catalog).is_none(), "{model}");
        }
        let dated = parse_arena(&document(vec![row("GLM 5.3 (20261020)", 1)])).unwrap();
        assert!(assessment_for("glm-5.3", &dated).is_none());
        assert!(assessment_for("glm-5.3-20261020", &dated).is_some());
    }

    #[test]
    fn unqualified_conditions_use_the_conservative_rank_and_disclose_ambiguity() {
        let catalog = parse_arena(&document(vec![
            row("GPT 5.2 (High)", 1),
            row("GPT 5.2 (Low)", 4),
            row("Other A", 2),
            row("Other B", 3),
        ]))
        .unwrap();
        let unqualified = assessment_for("gpt-5.2", &catalog).unwrap();
        assert_eq!((unqualified.rank, unqualified.tier), (4, 0));
        assert_eq!(unqualified.condition.as_deref(), Some("low"));
        assert!(unqualified.condition_ambiguous);
        let high = assessment_for("gpt-5.2 (High)", &catalog).unwrap();
        assert_eq!((high.rank, high.tier), (1, 3));
        assert!(!high.condition_ambiguous);
        assert_eq!(assessment_for("gpt-5.2-high", &catalog).unwrap().rank, 1);
        assert!(assessment_for("gpt-5.2 (Max)", &catalog).is_none());
    }

    #[test]
    fn actual_name_wins_over_an_interpreted_condition_suffix() {
        let catalog = parse_arena(&document(vec![
            row("Model High", 2),
            row("Model (High)", 1),
        ]))
        .unwrap();
        let assessment = assessment_for("model-high", &catalog).unwrap();
        assert_eq!(assessment.rank, 2);
        assert_eq!(assessment.condition, None);
    }

    #[test]
    fn quartiles_keep_all_four_tiers_and_never_default_invalid_ranks() {
        let catalog = parse_arena(&document(
            (1..=8)
                .map(|rank| row(&format!("Model {rank}"), rank))
                .collect(),
        ))
        .unwrap();
        let tiers: Vec<_> = catalog
            .entries
            .iter()
            .map(|entry| tier_for_entry(entry, catalog.entries.len()).unwrap())
            .collect();
        assert_eq!(tiers, vec![3, 3, 2, 2, 1, 1, 0, 0]);
        assert_eq!(tier_for_entry(&catalog.entries[0], 0), None);
        assert_eq!(tier_for_entry(&catalog.entries[7], 7), None);
    }

    #[test]
    fn zero_votes_remain_visible_without_becoming_missing_or_fabricated_evidence() {
        let mut item = row("Model A", 1);
        item["row"]["vote_count"] = json!(0);
        let catalog = parse_arena(&document(vec![item])).unwrap();
        assert_eq!(assessment_for("model-a", &catalog).unwrap().vote_count, 0);
    }

    #[test]
    fn official_whole_float_vote_counts_are_accepted_without_rounding() {
        let mut item = row("Model A", 1);
        item["row"]["vote_count"] = json!(10083.0);
        let catalog = parse_arena(&document(vec![item])).unwrap();
        assert_eq!(catalog.entries[0].vote_count, 10083);
        for invalid in [json!(10083.5), json!(-1.0), json!(1e30)] {
            let mut item = row("Model A", 1);
            item["row"]["vote_count"] = invalid;
            assert_eq!(
                parse_arena(&document(vec![item])).unwrap_err(),
                ParseError::NumericOutOfBounds
            );
        }
    }

    #[test]
    fn document_and_entry_bounds_are_enforced_before_use() {
        assert_eq!(
            parse_arena(&" ".repeat(MAX_DOCUMENT_BYTES + 1)).unwrap_err(),
            ParseError::TooLarge
        );
        let raw = json!({"rows":[],"num_rows_total":MAX_ENTRIES + 1}).to_string();
        assert_eq!(parse_arena(&raw).unwrap_err(), ParseError::CountExceeded);
    }
}
