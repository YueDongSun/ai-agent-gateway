//! Canonical model identity, date/variant recognition, and version relations.
//!
//! Pure logic, no I/O. This module is the single home for "what does this
//! model id mean" answers shared by tier evaluation and candidate planning:
//!
//! * canonical normalization (context markers, routing variants, case),
//! * date-token recognition so `claude-sonnet-4-5-20250929` is the same
//!   release as `claude-sonnet-4-5`, never a newer one,
//! * cheap-variant recognition (`mini`/`flash`/`lite`/...) so ordinal
//!   inheritance never promotes a fast variant to a top tier,
//! * decisive same-family version ordering (never across families).
//!
//! Version comparison semantics live in `model_discovery::rank` and are
//! reused here through thin wrappers so both subsystems order ids the same
//! way.

use std::cmp::Ordering;

use crate::services::model_discovery::rank::{
    family_key, is_low_tier_token, remainder_exact, tokenize, version_cmp_normalized,
};

/// Canonical form of a model id: trimmed, lowercased, `[...]` context
/// markers and OpenRouter-style `:variant` suffixes removed. This is the
/// identity key used everywhere in the catalog.
pub(crate) fn canonical_model_id(raw: &str) -> String {
    crate::services::model_discovery::rank::normalize_id(raw)
}

/// True for calendar tokens that describe a release date rather than a
/// version: four-digit years (`2024`) and `YYYYMMDD` stamps (`20250929`).
/// Three-digit revision markers such as the `002` in
/// `gemini-1.5-pro-002` are part of the identity and are never stripped.
pub(crate) fn is_date_like_token(token: &str) -> bool {
    if !token.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    match token.len() {
        4 => matches!(token.parse::<u32>(), Ok(year) if (1900..=2100).contains(&year)),
        8 => chrono::NaiveDate::parse_from_str(token, "%Y%m%d").is_ok(),
        _ => false,
    }
}

/// Remove date-like tokens from a normalized id, keeping every other
/// identity token in order. `claude-sonnet-4-5-20250929` and
/// `claude-sonnet-4-5` therefore share one identity after stripping.
pub(crate) fn strip_date_tokens(normalized: &str) -> String {
    tokenize(normalized)
        .into_iter()
        .filter(|token| !is_date_like_token(token))
        .collect::<Vec<_>>()
        .join("-")
}

/// Tokens used to match public benchmark keys against model ids: drop
/// `(...)` and `[...]` parentheticals (dates and context markers), case-fold,
/// split, and remove date-like tokens. `Claude Sonnet 3.5 (June 2024)` and
/// `claude-sonnet-3-5` produce the same multiset of tokens.
pub(crate) fn evidence_key_tokens(raw: &str) -> Vec<String> {
    let mut out = String::with_capacity(raw.len());
    let mut depth = 0_usize;
    for ch in raw.chars() {
        match ch {
            '(' | '[' => depth = depth.saturating_add(1),
            ')' | ']' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }
    tokenize(&out.to_lowercase())
        .into_iter()
        .filter(|token| !is_date_like_token(token))
        .collect()
}

/// Decisive version relation of `a` relative to `b`, date-stripped and
/// family-scoped by construction (the comparator only looks at the version
/// remainder after the family prefix, and callers keep comparisons inside
/// one family). [`VersionRelation::Incomparable`] covers both "same
/// version remainder" and "the heuristic refuses to order these".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VersionRelation {
    Older,
    Newer,
    Same,
    Incomparable,
}

pub(crate) fn version_relation(a: &str, b: &str) -> VersionRelation {
    let a_stripped = strip_date_tokens(&canonical_model_id(a));
    let b_stripped = strip_date_tokens(&canonical_model_id(b));
    if a_stripped == b_stripped || remainder_exact(&a_stripped, &b_stripped) {
        return VersionRelation::Same;
    }
    match version_cmp_normalized(&a_stripped, &b_stripped) {
        Ordering::Less => VersionRelation::Older,
        Ordering::Greater => VersionRelation::Newer,
        Ordering::Equal => VersionRelation::Incomparable,
    }
}

/// True when `query` introduces a cheap/experimental variant marker that
/// `anchor` lacks (`glm-5.3` -> `glm-5.3-flash`), including compound
/// spellings at most two characters past the marker (`flashx`). Such models
/// never inherit their anchor's tier unchecked.
pub(crate) fn introduces_variant(query: &str, anchor: &str) -> bool {
    let anchor_tokens = tokenize(&strip_date_tokens(&canonical_model_id(anchor)));
    tokenize(&strip_date_tokens(&canonical_model_id(query)))
        .into_iter()
        .any(|token| is_low_tier_token(&token) && !anchor_tokens.contains(&token))
}

/// The canonical id without an org prefix (`anthropic/claude-sonnet-4-5` ->
/// `claude-sonnet-4-5`). Only the last `/` separates the org, so nested
/// provider paths keep everything before it.
pub(crate) fn strip_org_prefix(raw: &str) -> String {
    let canonical = canonical_model_id(raw);
    match canonical.rfind('/') {
        Some(index) => canonical[index + 1..].to_string(),
        None => canonical,
    }
}

/// Family key of a raw id: an optional org prefix (`anthropic/...`,
/// `google/...`) is dropped before the family split so
/// `anthropic/claude-sonnet-4-5` groups with `claude-sonnet-4-5` under
/// `claude-sonnet`. Org prefixes are only stripped at the last `/`.
pub(crate) fn family_of_id(raw: &str) -> String {
    family_key(&strip_date_tokens(&strip_org_prefix(raw)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_identity_normalization_is_shared_with_discovery() {
        assert_eq!(canonical_model_id("  MiniMax-M3.1 "), "minimax-m3.1");
        assert_eq!(
            canonical_model_id("claude-sonnet-4-5[1M]"),
            "claude-sonnet-4-5"
        );
        assert_eq!(canonical_model_id("a/b-5.5:batch"), "a/b-5.5");
    }

    #[test]
    fn date_tokens_are_recognized_and_stripped_without_touching_revisions() {
        assert!(is_date_like_token("2024"));
        assert!(is_date_like_token("20250929"));
        assert!(!is_date_like_token("002"));
        assert!(!is_date_like_token("4"));
        assert!(!is_date_like_token("20251301"));
        assert_eq!(
            strip_date_tokens("claude-sonnet-4-5-20250929"),
            "claude-sonnet-4-5"
        );
        assert_eq!(
            strip_date_tokens("gemini-1-5-pro-002"),
            "gemini-1-5-pro-002"
        );
    }

    #[test]
    fn dated_snapshots_are_the_same_release_not_a_newer_one() {
        assert_eq!(
            version_relation("claude-sonnet-4-5-20250929", "claude-sonnet-4-5"),
            VersionRelation::Same
        );
        assert_eq!(
            version_relation("claude-sonnet-3-5", "claude-sonnet-4-5"),
            VersionRelation::Older
        );
        assert_eq!(
            version_relation("claude-sonnet-4-5", "claude-sonnet-3-5"),
            VersionRelation::Newer
        );
        // Incomparable shapes (gpt-4.1 vs gpt-4o) must not be guessed.
        assert_eq!(
            version_relation("gpt-4-1", "gpt-4o"),
            VersionRelation::Incomparable
        );
    }

    #[test]
    fn evidence_key_tokens_strip_parentheticals_and_dates() {
        assert_eq!(
            evidence_key_tokens("Claude Sonnet 3.5 (June 2024)"),
            vec!["claude", "sonnet", "3", "5"]
        );
        assert_eq!(
            evidence_key_tokens("GPT-4-Turbo (Nov 2023)"),
            vec!["gpt", "4", "turbo"]
        );
        assert_eq!(
            evidence_key_tokens("Gemini 1.5 Pro 002"),
            vec!["gemini", "1", "5", "pro", "002"]
        );
    }

    #[test]
    fn cheap_variant_markers_never_inherit_unchecked() {
        assert!(introduces_variant("glm-5-3-flash", "glm-5-3"));
        assert!(introduces_variant("glm-5.3-flashx", "glm-5.3"));
        assert!(introduces_variant(
            "deepseek-v4-flash-vision-exp",
            "deepseek-v4-flash"
        ));
        // `minimax` is not the `mini` marker.
        assert!(!introduces_variant("MiniMax-M3", "MiniMax-M2"));
        assert!(!introduces_variant("claude-haiku-4-5", "claude-haiku-3"));
    }

    #[test]
    fn family_keys_drop_org_prefixes_but_keep_role_families() {
        assert_eq!(family_of_id("anthropic/claude-sonnet-4-5"), "claude-sonnet");
        assert_eq!(family_of_id("claude-sonnet-4-5-20250929"), "claude-sonnet");
        assert_eq!(family_of_id("gemini-3-7-flash"), "gemini");
        assert_eq!(family_of_id("google/gemma-4-26b-a4b-it"), "gemma");
    }

    #[test]
    fn org_prefixes_are_stripped_at_the_last_slash_only() {
        assert_eq!(
            strip_org_prefix("anthropic/claude-fable-5.1"),
            "claude-fable-5.1"
        );
        assert_eq!(strip_org_prefix("@cf/moonshotai/kimi-k2.6"), "kimi-k2.6");
        assert_eq!(strip_org_prefix("claude-fable-5.1"), "claude-fable-5.1");
        // The canonical form still applies: case and context markers.
        assert_eq!(
            strip_org_prefix("Anthropic/Claude-Fable-5.1[1M]"),
            "claude-fable-5.1"
        );
    }
}
