//! Strict parsers for the two fixed public evidence sources.
//!
//! Pure logic, no I/O. Numeric fields are range-checked so a corrupt or
//! hostile document can never manufacture capabilities or inflate scores,
//! and a document with a broken shape or an out-of-range number is refused
//! as a whole (stable codes below).
//!
//! A record that is merely *incomplete* is not a broken document: it
//! contributes no evidence and is skipped. That covers the shapes the real
//! public sources actually use — a family name with a dot (`qwen3.6`), a
//! month-precision release date (`2026-01`), an unmetered token limit
//! (`0` = not published = unknown), and a benchmark row without one of the
//! two comparable suites. Nothing is ever filled in to make an incomplete
//! record look complete.
//!
//! These documents are factual metadata and benchmark scores — they are
//! never treated as unified intelligence, and a parsed entry never claims
//! that any supplier offers the model.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use serde_json::Value;

/// Mirror of `database::dao::suppliers::MAX_CATALOG_JSON_BYTES` (the cache
/// layer enforces the same cap on write).
pub(crate) const MAX_DOCUMENT_BYTES: usize = 8 * 1024 * 1024;

const MAX_MODELS_DEV_ENTRIES: usize = 50_000;
const MAX_EVALPLUS_ENTRIES: usize = 20_000;
const MAX_ID_CHARS: usize = 256;
const MAX_FAMILY_CHARS: usize = 64;
const MAX_MODALITY_CHARS: usize = 32;
const MAX_MODALITY_COUNT: usize = 16;
const MAX_PROVIDER_REFS: usize = 64;
const MAX_TOKEN_LIMIT: u64 = 100_000_000;
const MAX_COST_PER_MTOK: f64 = 1_000_000.0;

/// Stable failure codes; also recorded as the cache `last_error` so the UI
/// never sees raw parser or network text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ParseError {
    TooLarge,
    Decode,
    ShapeInvalid,
    RecordInvalid,
    NumericOutOfBounds,
    DateInvalid,
    CountExceeded,
}

impl ParseError {
    pub(crate) fn code(self) -> &'static str {
        match self {
            ParseError::TooLarge => "fetch_too_large",
            ParseError::Decode => "fetch_decode_failed",
            ParseError::ShapeInvalid | ParseError::RecordInvalid => "fetch_schema_invalid",
            ParseError::NumericOutOfBounds => "fetch_numeric_out_of_bounds",
            ParseError::DateInvalid => "fetch_date_invalid",
            ParseError::CountExceeded => "fetch_entry_limit_exceeded",
        }
    }
}

/// One models.dev model entry. Optional fields stay `None` when the source
/// is silent — unknown is never converted into a claim.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ModelsDevModel {
    pub id: String,
    pub family: Option<String>,
    pub tool_call: Option<bool>,
    pub input_modalities: Vec<String>,
    pub output_modalities: Vec<String>,
    pub context_window: Option<u64>,
    pub max_output_tokens: Option<u64>,
    /// USD per million input tokens; price is metadata, never intelligence.
    pub cost_input_per_mtok: Option<f64>,
    pub cost_output_per_mtok: Option<f64>,
    /// Validated date with the source's day or month precision.
    pub release_date: Option<String>,
    /// Provider keys listing this model id, sorted; directory presence only.
    pub providers: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct ModelsDevCatalog {
    pub models: Vec<ModelsDevModel>,
}

/// One EvalPlus leaderboard row: only the comparable pass@1 pair plus the
/// evaluation condition. Raw scores stay per-suite; nothing is zero-filled
/// and nothing is mixed across leaderboards.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct EvalPlusEntry {
    pub key: String,
    pub humaneval_plus: f64,
    pub mbpp_plus: f64,
    pub prompted: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct EvalPlusCatalog {
    pub entries: Vec<EvalPlusEntry>,
    pub incomplete_rows: usize,
}

/// Parse the models.dev `provider -> { models: { id -> entry } }` document.
pub(crate) fn parse_models_dev(raw: &str) -> Result<ModelsDevCatalog, ParseError> {
    if raw.len() > MAX_DOCUMENT_BYTES {
        return Err(ParseError::TooLarge);
    }
    let root: Value = serde_json::from_str(raw).map_err(|_| ParseError::Decode)?;
    let providers = root.as_object().ok_or(ParseError::ShapeInvalid)?;
    // Sorted provider keys keep the merged output deterministic.
    let mut merged: BTreeMap<String, ModelsDevModel> = BTreeMap::new();
    for (provider_key, provider) in providers {
        let provider = provider.as_object().ok_or(ParseError::ShapeInvalid)?;
        let Some(models) = provider.get("models") else {
            continue;
        };
        let models = models.as_object().ok_or(ParseError::ShapeInvalid)?;
        for (map_key, entry) in models {
            let model = parse_models_dev_entry(map_key, entry, provider_key)?;
            if merged.len() >= MAX_MODELS_DEV_ENTRIES && !merged.contains_key(&model.id) {
                return Err(ParseError::CountExceeded);
            }
            match merged.get_mut(&model.id) {
                Some(existing) => {
                    // Different hosting prices are not a trustworthy quote for
                    // an unrelated supplier. Keep conflicting prices unknown.
                    if existing.cost_input_per_mtok != model.cost_input_per_mtok { existing.cost_input_per_mtok = None; }
                    if existing.cost_output_per_mtok != model.cost_output_per_mtok { existing.cost_output_per_mtok = None; }
                    if !existing.providers.contains(provider_key) {
                        existing.providers.push(provider_key.clone());
                        if existing.providers.len() > MAX_PROVIDER_REFS {
                            existing.providers.truncate(MAX_PROVIDER_REFS);
                        }
                    }
                }
                None => {
                    merged.insert(model.id.clone(), model);
                }
            }
        }
    }
    let mut models: Vec<ModelsDevModel> = merged.into_values().collect();
    for model in &mut models {
        model.providers.sort();
    }
    Ok(ModelsDevCatalog { models })
}

fn parse_models_dev_entry(
    map_key: &str,
    entry: &Value,
    provider_key: &str,
) -> Result<ModelsDevModel, ParseError> {
    let entry = entry.as_object().ok_or(ParseError::RecordInvalid)?;
    let id = match entry.get("id") {
        Some(value) => value.as_str().ok_or(ParseError::RecordInvalid)?,
        None => map_key,
    };
    validate_id(id)?;
    let family = match entry.get("family") {
        None | Some(Value::Null) => None,
        Some(value) => {
            let family = value.as_str().ok_or(ParseError::RecordInvalid)?;
            validate_family(family)?;
            Some(family.to_lowercase())
        }
    };
    let tool_call = match entry.get("tool_call") {
        None | Some(Value::Null) => None,
        Some(value) => Some(value.as_bool().ok_or(ParseError::RecordInvalid)?),
    };
    let (input_modalities, output_modalities) = match entry.get("modalities") {
        None | Some(Value::Null) => (Vec::new(), Vec::new()),
        Some(value) => parse_modalities(value)?,
    };
    let (context_window, max_output_tokens) = match entry.get("limit") {
        None | Some(Value::Null) => (None, None),
        Some(value) => parse_limits(value)?,
    };
    let (cost_input_per_mtok, cost_output_per_mtok) = match entry.get("cost") {
        None | Some(Value::Null) => (None, None),
        Some(value) => parse_costs(value)?,
    };
    let release_date = match entry.get("release_date") {
        None | Some(Value::Null) => None,
        Some(value) => {
            let date = value.as_str().ok_or(ParseError::RecordInvalid)?;
            Some(parse_release_date(date)?)
        }
    };
    Ok(ModelsDevModel {
        id: id.to_string(),
        family,
        tool_call,
        input_modalities,
        output_modalities,
        context_window,
        max_output_tokens,
        cost_input_per_mtok,
        cost_output_per_mtok,
        release_date,
        providers: vec![provider_key.to_string()],
    })
}

fn parse_modalities(value: &Value) -> Result<(Vec<String>, Vec<String>), ParseError> {
    let object = value.as_object().ok_or(ParseError::RecordInvalid)?;
    let parse_list = |key: &str| -> Result<Vec<String>, ParseError> {
        let mut list = Vec::new();
        if let Some(values) = object.get(key) {
            let values = values.as_array().ok_or(ParseError::RecordInvalid)?;
            if values.len() > MAX_MODALITY_COUNT {
                return Err(ParseError::RecordInvalid);
            }
            for item in values {
                let name = item.as_str().ok_or(ParseError::RecordInvalid)?;
                if name.is_empty()
                    || name.chars().count() > MAX_MODALITY_CHARS
                    || name.chars().any(char::is_control)
                {
                    return Err(ParseError::RecordInvalid);
                }
                list.push(name.to_lowercase());
            }
        }
        Ok(list)
    };
    Ok((parse_list("input")?, parse_list("output")?))
}

fn parse_limits(value: &Value) -> Result<(Option<u64>, Option<u64>), ParseError> {
    let object = value.as_object().ok_or(ParseError::RecordInvalid)?;
    let context = object
        .get("context")
        .map(bound_token_limit)
        .transpose()?
        .flatten();
    let output = object
        .get("output")
        .map(bound_token_limit)
        .transpose()?
        .flatten();
    Ok((context, output))
}

/// A token limit is a positive integer. The public catalog states `0` for
/// models it does not meter (image models), which is "no limit published",
/// not a broken record: it becomes unknown, and unknown limits never
/// satisfy a hard requirement.
fn bound_token_limit(value: &Value) -> Result<Option<u64>, ParseError> {
    let raw = value.as_f64().ok_or(ParseError::NumericOutOfBounds)?;
    if !raw.is_finite() || raw < 0.0 || raw > MAX_TOKEN_LIMIT as f64 {
        return Err(ParseError::NumericOutOfBounds);
    }
    // Integrality: `262100.0` is fine, `262100.5` is not.
    if raw.fract() != 0.0 {
        return Err(ParseError::NumericOutOfBounds);
    }
    if raw == 0.0 {
        return Ok(None);
    }
    Ok(Some(raw as u64))
}

/// Release dates arrive as `YYYY-MM-DD` or as the coarser `YYYY-MM`. Both
/// are calendar dates; anything else is a malformed record.
fn parse_release_date(value: &str) -> Result<String, ParseError> {
    if value.len() == 10 && value.as_bytes()[4] == b'-' && value.as_bytes()[7] == b'-' {
        return NaiveDate::parse_from_str(value, "%Y-%m-%d")
            .map(|date| date.to_string())
            .map_err(|_| ParseError::DateInvalid);
    }
    if value.len() == 7
        && value.as_bytes()[4] == b'-'
        && NaiveDate::parse_from_str(&format!("{value}-01"), "%Y-%m-%d").is_ok()
    {
        return Ok(value.to_string());
    }
    Err(ParseError::DateInvalid)
}

fn parse_costs(value: &Value) -> Result<(Option<f64>, Option<f64>), ParseError> {
    let object = value.as_object().ok_or(ParseError::RecordInvalid)?;
    let bound = |key: &str| -> Result<Option<f64>, ParseError> {
        match object.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(raw) => {
                let amount = raw.as_f64().ok_or(ParseError::NumericOutOfBounds)?;
                if !amount.is_finite() || !(0.0..=MAX_COST_PER_MTOK).contains(&amount) {
                    return Err(ParseError::NumericOutOfBounds);
                }
                Ok(Some(amount))
            }
        }
    };
    Ok((bound("input")?, bound("output")?))
}

/// Parse the EvalPlus `key -> { "pass@1": {...}, prompted, ... }` document.
///
/// Both `humaneval+` and `mbpp+` must be present and in `0..=100` for a row
/// to count as evidence: a missing score is never filled with zero, and a
/// row that lacks one of the two comparable suites contributes no evidence
/// at all instead of being scored on a single suite. Such rows are skipped
/// rather than poisoning the whole document; a malformed document (wrong
/// shape, out-of-range numbers) is still refused as a whole.
pub(crate) fn parse_evalplus(raw: &str) -> Result<EvalPlusCatalog, ParseError> {
    if raw.len() > MAX_DOCUMENT_BYTES {
        return Err(ParseError::TooLarge);
    }
    let root: Value = serde_json::from_str(raw).map_err(|_| ParseError::Decode)?;
    let rows = root.as_object().ok_or(ParseError::ShapeInvalid)?;
    if rows.len() > MAX_EVALPLUS_ENTRIES {
        return Err(ParseError::CountExceeded);
    }
    let mut entries = Vec::with_capacity(rows.len());
    let mut incomplete_rows = 0;
    for (key, row) in rows {
        validate_id(key)?;
        let row = row.as_object().ok_or(ParseError::RecordInvalid)?;
        let pass = row
            .get("pass@1")
            .and_then(Value::as_object)
            .ok_or(ParseError::RecordInvalid)?;
        let score = |key: &str| -> Result<Option<f64>, ParseError> {
            match pass.get(key) {
                None | Some(Value::Null) => Ok(None),
                Some(value) => value
                    .as_f64()
                    .ok_or(ParseError::RecordInvalid)
                    .and_then(bound_score)
                    .map(Some),
            }
        };
        let humaneval_plus = score("humaneval+")?;
        let mbpp_plus = score("mbpp+")?;
        let prompted = row
            .get("prompted")
            .and_then(Value::as_bool)
            .ok_or(ParseError::RecordInvalid)?;
        let (Some(humaneval_plus), Some(mbpp_plus)) = (humaneval_plus, mbpp_plus) else {
            incomplete_rows += 1;
            continue;
        };
        entries.push(EvalPlusEntry {
            key: key.clone(),
            humaneval_plus: bound_score(humaneval_plus)?,
            mbpp_plus: bound_score(mbpp_plus)?,
            prompted,
        });
    }
    Ok(EvalPlusCatalog {
        entries,
        incomplete_rows,
    })
}

fn bound_score(raw: f64) -> Result<f64, ParseError> {
    if !raw.is_finite() || !(0.0..=100.0).contains(&raw) {
        return Err(ParseError::NumericOutOfBounds);
    }
    Ok(raw)
}

fn validate_id(value: &str) -> Result<(), ParseError> {
    if value.is_empty()
        || value.chars().count() > MAX_ID_CHARS
        || value.chars().any(char::is_control)
    {
        return Err(ParseError::RecordInvalid);
    }
    Ok(())
}

fn validate_family(value: &str) -> Result<(), ParseError> {
    if value.is_empty()
        || value.chars().count() > MAX_FAMILY_CHARS
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_' || byte == b'.'
        })
    {
        return Err(ParseError::RecordInvalid);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn models_dev_fixture_parses_family_tool_modalities_limit_cost() {
        let raw = r#"{
            "anthropic": {
                "id": "anthropic",
                "models": {
                    "claude-sonnet-4-5": {
                        "id": "claude-sonnet-4-5",
                        "family": "claude-sonnet",
                        "tool_call": true,
                        "modalities": {"input": ["text", "image"], "output": ["text"]},
                        "limit": {"context": 200000, "output": 64000},
                        "cost": {"input": 3, "output": 15, "cache_read": 0.3},
                        "release_date": "2025-09-29",
                        "unknown_future_field": {"x": 1}
                    }
                }
            },
            "empty-provider": {"models": {}},
            "model-less": {"name": "no models key"}
        }"#;
        let catalog = parse_models_dev(raw).expect("fixture parses");
        assert_eq!(catalog.models.len(), 1);
        let model = &catalog.models[0];
        assert_eq!(model.id, "claude-sonnet-4-5");
        assert_eq!(model.family.as_deref(), Some("claude-sonnet"));
        assert_eq!(model.tool_call, Some(true));
        assert_eq!(model.input_modalities, vec!["text", "image"]);
        assert_eq!(model.output_modalities, vec!["text"]);
        assert_eq!(model.context_window, Some(200_000));
        assert_eq!(model.max_output_tokens, Some(64_000));
        assert_eq!(model.cost_input_per_mtok, Some(3.0));
        assert_eq!(model.release_date.as_deref(), Some("2025-09-29"));
        assert_eq!(model.providers, vec!["anthropic"]);
    }

    #[test]
    fn same_model_across_providers_merges_provider_refs_deterministically() {
        let raw = r#"{
            "b-provider": {"models": {"m1": {"id": "m1", "tool_call": true}}},
            "a-provider": {"models": {"m1": {"id": "m1"}}}
        }"#;
        let catalog = parse_models_dev(raw).expect("fixture parses");
        assert_eq!(catalog.models.len(), 1);
        assert_eq!(
            catalog.models[0].providers,
            vec!["a-provider", "b-provider"]
        );
    }

    #[test]
    fn conflicting_host_prices_are_not_used_as_supplier_prices() {
        let catalog = parse_models_dev(r#"{"a":{"models":{"m":{"cost":{"input":1,"output":4}}}},"b":{"models":{"m":{"cost":{"input":2,"output":8}}}}}"#).unwrap();
        assert_eq!(catalog.models[0].cost_input_per_mtok, None);
        assert_eq!(catalog.models[0].cost_output_per_mtok, None);
    }

    #[test]
    fn numeric_bounds_are_enforced_and_never_guessed() {
        // Out-of-range context and negative cost both fail closed.
        for raw in [
            r#"{"p": {"models": {"m": {"limit": {"context": 999999999}}}}}"#,
            r#"{"p": {"models": {"m": {"limit": {"context": 1.5}}}}}"#,
            r#"{"p": {"models": {"m": {"limit": {"context": -4}}}}}"#,
            r#"{"p": {"models": {"m": {"cost": {"input": -1}}}}}"#,
            r#"{"p": {"models": {"m": {"cost": {"output": 1e99}}}}}"#,
            r#"{"p": {"models": {"m": {"cost": {"input": "3"}}}}}"#,
            r#"{"p": {"models": {"m": {"tool_call": "yes"}}}}"#,
        ] {
            assert!(parse_models_dev(raw).is_err(), "should reject: {raw}");
        }
        // Missing optional fields stay unknown, not defaulted.
        let catalog =
            parse_models_dev(r#"{"p": {"models": {"m": {"id": "m"}}}}"#).expect("minimal entry");
        assert_eq!(catalog.models[0].tool_call, None);
        assert_eq!(catalog.models[0].context_window, None);
        assert_eq!(catalog.models[0].cost_input_per_mtok, None);
    }

    /// The shapes the live public catalog really uses must parse: a family
    /// name with a dot, a month-precision release date, and an unmetered
    /// token limit (`0`), which means "not published" and stays unknown.
    #[test]
    fn live_catalog_shapes_parse_without_inventing_facts() {
        let raw = r#"{"p": {"models": {
            "qwen3.6-plus": {
                "id": "qwen3.6-plus",
                "family": "qwen3.6",
                "tool_call": true,
                "modalities": {"input": ["text"], "output": ["text"]},
                "limit": {"context": 0, "output": 0},
                "release_date": "2026-01"
            }
        }}}"#;
        let catalog = parse_models_dev(raw).expect("live shapes parse");
        let model = &catalog.models[0];
        assert_eq!(model.family.as_deref(), Some("qwen3.6"));
        // A month-precision date is a real date, kept at month precision.
        assert_eq!(model.release_date.as_deref(), Some("2026-01"));
        // An unmetered limit is unknown, never a claim of zero capacity and
        // never a fabricated number.
        assert_eq!(model.context_window, None);
        assert_eq!(model.max_output_tokens, None);
        assert_eq!(model.tool_call, Some(true));
    }

    /// A benchmark row without one of the two comparable suites carries no
    /// evidence and is skipped — never scored on a single suite and never
    /// filled with a zero.
    #[test]
    fn evalplus_rows_without_a_comparable_pair_are_skipped() {
        let raw = r#"{
            "Has Both": {
                "pass@1": {"humaneval+": 81.7, "mbpp+": 74.3},
                "prompted": true
            },
            "Missing Mbpp": {
                "pass@1": {"humaneval+": 87.2},
                "prompted": true
            },
            "Missing Humaneval": {
                "pass@1": {"mbpp+": 72.2},
                "prompted": true
            }
        }"#;
        let catalog = parse_evalplus(raw).expect("fixture parses");
        assert_eq!(catalog.entries.len(), 1);
        assert_eq!(catalog.entries[0].key, "Has Both");
        assert_eq!(catalog.entries[0].humaneval_plus, 81.7);
        assert_eq!(catalog.entries[0].mbpp_plus, 74.3);
    }

    /// A row that does carry the pair still has to state its condition, and
    /// an out-of-range score still fails the document.
    #[test]
    fn evalplus_keeps_enforcing_scores_and_conditions() {
        assert_eq!(
            parse_evalplus(
                r#"{"m": {"pass@1": {"humaneval+": 101.0, "mbpp+": 50.0}, "prompted": true}}"#
            )
            .unwrap_err(),
            ParseError::NumericOutOfBounds
        );
        assert_eq!(
            parse_evalplus(
                r#"{"m": {"pass@1": {"humaneval+": 80.0, "mbpp+": 50.0}, "prompted": "yes"}}"#
            )
            .unwrap_err(),
            ParseError::RecordInvalid
        );
        assert!(parse_evalplus(r#"{"m": []}"#).is_err());
        assert!(parse_evalplus(r#"not json"#).is_err());
    }

    #[test]
    fn malformed_records_fail_the_whole_document() {
        for raw in [
            r#"{"p": {"models": {"m": {"id": ""}}}}"#,
            r#"{"p": {"models": {"m": {"id": "m", "family": "bad family!"}}}}"#,
            r#"{"p": {"models": {"m": {"id": "m", "modalities": {"input": 3}}}}}"#,
            r#"{"p": {"models": []}}"#,
            r#"{"p": []}"#,
            r#"[1, 2]"#,
            r#"not json"#,
        ] {
            assert!(parse_models_dev(raw).is_err(), "should reject: {raw}");
        }
    }

    #[test]
    fn release_dates_must_be_real_calendar_dates() {
        assert_eq!(
            parse_models_dev(
                r#"{"p": {"models": {"m": {"id": "m", "release_date": "2026-02-29"}}}}"#
            )
            .unwrap_err(),
            ParseError::DateInvalid
        );
        assert_eq!(
            parse_models_dev(
                r#"{"p": {"models": {"m": {"id": "m", "release_date": "20260228"}}}}"#
            )
            .unwrap_err(),
            ParseError::DateInvalid
        );
    }

    #[test]
    fn evalplus_fixture_parses_scores_condition_and_size() {
        let raw = r#"{
            "Claude Sonnet 3.5 (June 2024)": {
                "link": "https://example.invalid",
                "open-data": "NONE",
                "pass@1": {"humaneval": 87.2, "humaneval+": 81.7, "mbpp": 89.4, "mbpp+": 74.3},
                "prompted": true,
                "size": null
            },
            "GPT 4o (Aug 2024)": {
                "pass@1": {"humaneval+": 87.2, "mbpp+": 72.2},
                "prompted": true,
                "size": 8.0
            }
        }"#;
        let catalog = parse_evalplus(raw).expect("fixture parses");
        assert_eq!(catalog.entries.len(), 2);
        let sonnet = catalog
            .entries
            .iter()
            .find(|entry| entry.key.starts_with("Claude"))
            .expect("sonnet row");
        assert_eq!(sonnet.humaneval_plus, 81.7);
        assert_eq!(sonnet.mbpp_plus, 74.3);
        assert!(sonnet.prompted);
    }

    #[test]
    fn evalplus_missing_or_out_of_range_scores_fail_closed() {
        // A missing `mbpp+` is a hole, not a zero: the row carries no
        // evidence at all and is skipped.
        let partial =
            parse_evalplus(r#"{"m": {"pass@1": {"humaneval+": 50.0}, "prompted": true}}"#)
                .expect("a partial row is not a broken document");
        assert!(
            partial.entries.is_empty(),
            "no suite pair means no evidence"
        );
        for raw in [
            r#"{"m": {"pass@1": {"humaneval+": 50.0, "mbpp+": 50.0}, "prompted": "yes"}}"#,
            r#"{"m": {"pass@1": {"humaneval+": -1, "mbpp+": 50.0}, "prompted": true}}"#,
            r#"{"m": {"pass@1": {"humaneval+": 100.5, "mbpp+": 50.0}, "prompted": true}}"#,
            r#"{"m": {"pass@1": {"humaneval+": 50.0, "mbpp+": 50.0}}}"#,
            r#"{"m": {}}"#,
        ] {
            assert!(parse_evalplus(raw).is_err(), "should reject: {raw}");
        }
        assert_eq!(
            parse_evalplus(
                r#"{"m": {"pass@1": {"humaneval+": 50.0, "mbpp+": 50.0}, "prompted": true}}"#
            )
            .unwrap()
            .entries
            .len(),
            1
        );
    }

    #[test]
    fn partial_scores_still_validate_types_ranges_and_conditions() {
        for raw in [
            r#"{"m":{"pass@1":{"humaneval+":101},"prompted":true}}"#,
            r#"{"m":{"pass@1":{"humaneval+":"80"},"prompted":true}}"#,
            r#"{"m":{"pass@1":{"humaneval+":80},"prompted":"yes"}}"#,
        ] {
            assert!(parse_evalplus(raw).is_err());
        }
        for invalid in [
            "2026-13",
            "2026-00",
            "2026-2",
            "2026-01-99",
            "2026-01-01-extra",
        ] {
            assert_eq!(parse_release_date(invalid), Err(ParseError::DateInvalid));
        }
    }

    #[test]
    fn oversized_documents_and_entry_counts_fail_closed() {
        let big = "x".repeat(MAX_DOCUMENT_BYTES + 1);
        assert_eq!(parse_models_dev(&big).unwrap_err(), ParseError::TooLarge);
        assert_eq!(parse_evalplus(&big).unwrap_err(), ParseError::TooLarge);
    }
}
