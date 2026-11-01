//! Usage accounting for proxied requests.
//!
//! `database/dao/usage.rs` owns the `usage_records` table and its aggregates,
//! but nothing ever wrote a row: the DAO existed, the table stayed empty, and
//! no IPC command read it. This module supplies the missing write path —
//! extraction of token counts from a canonical response and a bounded,
//! failure-tolerant insert — plus the cost model that turns tokens into USD.
//!
//! Recording is best-effort by construction. Usage accounting must never
//! fail a request the upstream already answered, so every error here is
//! logged and dropped rather than propagated.

use crate::database::Database;
use crate::database::dao::usage::UsageRecordInput;
use crate::proxy::canonical::{OAIChatResponse, Usage};

/// USD per million tokens, as (prompt, completion). `None` means the model is
/// unpriced and its requests record zero cost rather than a fabricated number.
type Rate = (f64, f64);

/// Per-million-token prices for the models AAG is configured against.
///
/// Deliberately a small, explicit table rather than a fetched catalog: a
/// wrong live price silently corrupts cost reporting, while a missing entry
/// reports zero and is visibly wrong instead of subtly wrong. Prices are as
/// of 2026-10.
const PRICES: &[(&str, Rate)] = &[
    ("claude-opus", (15.0, 75.0)),
    ("claude-sonnet", (3.0, 15.0)),
    ("claude-haiku", (0.8, 4.0)),
    ("claude-fable", (3.0, 15.0)),
    ("gpt-5", (1.25, 10.0)),
    ("gpt-4o-mini", (0.15, 0.6)),
    ("gpt-4o", (2.5, 10.0)),
    ("gpt-4.1", (2.0, 8.0)),
    ("o3", (2.0, 8.0)),
    ("gemini-3", (1.25, 10.0)),
    ("gemini-2.5-pro", (1.25, 10.0)),
    ("gemini-2.5-flash", (0.3, 2.5)),
    ("deepseek-chat", (0.27, 1.1)),
    ("deepseek-reasoner", (0.55, 2.19)),
    ("glm-4", (0.6, 2.2)),
    ("glm-5", (0.6, 2.2)),
    ("kimi-k2", (0.6, 2.5)),
    ("kimi-k3", (0.6, 2.5)),
    ("minimax", (0.3, 1.2)),
    ("mimo", (0.3, 1.2)),
];

/// Per-million-token price of `model`, or `None` when unpriced.
///
/// Longest *word-boundary* prefix wins. A successor whose version suffix
/// rotates (`deepseek-reasoner-250905`) matches its family; a cheaper
/// variant that shares the prefix with a dearer family must not inherit its
/// price — `gpt-4o-mini` matches `gpt-4o-mini`, not `gpt-4o`, because the
/// boundary check requires the character after the family name to be a
/// separator. Without that, any `gpt-4o*` id would price at the flagship
/// rate and overstate the bill.
fn rate_for(model: &str) -> Option<Rate> {
    let id = model.to_ascii_lowercase();
    PRICES
        .iter()
        .filter(|(prefix, _)| {
            id.starts_with(prefix)
                && id[prefix.len()..]
                    .chars()
                    .next()
                    .map(|c| c == '-' || c == '.' || c == '/')
                    .unwrap_or(true)
        })
        .max_by_key(|(prefix, _)| prefix.len())
        .map(|(_, rate)| *rate)
}

/// USD cost of a request, or 0.0 when the model has no entry.
pub fn cost_usd(model: &str, prompt_tokens: i64, completion_tokens: i64) -> f64 {
    match rate_for(model) {
        Some((prompt_rate, completion_rate)) => {
            (prompt_tokens as f64 * prompt_rate + completion_tokens as f64 * completion_rate)
                / 1_000_000.0
        }
        None => 0.0,
    }
}

/// Extract token counts from a canonical response.
///
/// Returns `(0, 0)` when the upstream sent no usage block — a relay that
/// strips `usage` still produces a valid row with zero tokens, which keeps
/// request counts and latency meaningful even without token accounting.
pub fn tokens_of(response: &OAIChatResponse) -> (i64, i64) {
    response
        .usage
        .as_ref()
        .map(|Usage {
                 prompt_tokens,
                 completion_tokens,
                 ..
             }| (*prompt_tokens as i64, *completion_tokens as i64))
        .unwrap_or((0, 0))
}

/// Build the row for one answered request.
///
/// `model` is the requested alias and `upstream_model` the hop's concrete
/// model; pricing follows the concrete model, while the alias is what the
/// user configured and what route-level reporting should group by. Both are
/// kept so a later reporting pass can choose.
pub fn build_record(
    app_type: &str,
    provider_id: &str,
    upstream_model: &str,
    model: &str,
    response: &OAIChatResponse,
    duration_ms: i64,
) -> UsageRecordInput {
    let (prompt_tokens, completion_tokens) = tokens_of(response);
    UsageRecordInput {
        ts: chrono::Utc::now().timestamp(),
        app_id: app_type.to_string(),
        provider_id: provider_id.to_string(),
        // Record the concrete upstream model: cost and token comparisons
        // across a failover chain are only meaningful on the model actually
        // served, and the alias is recoverable from the route table.
        model: if upstream_model.is_empty() {
            model.to_string()
        } else {
            upstream_model.to_string()
        },
        prompt_tokens,
        completion_tokens,
        cost_usd: cost_usd(upstream_model, prompt_tokens, completion_tokens),
        duration_ms,
    }
}

/// Persist one usage row, swallowing failures.
///
/// The router calls this from the request path: a locked or full database
/// must not turn an answered request into a 500.
pub fn record(db: Option<&Database>, record: UsageRecordInput) {
    let Some(db) = db else { return };
    if let Err(e) = db.record_usage(&record) {
        log::warn!(
            "usage: record failed app_ref={} provider_ref={} error={}",
            crate::diagnostics::safe_reference(&record.app_id),
            crate::diagnostics::safe_reference(&record.provider_id),
            e
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response_with(prompt: u32, completion: u32) -> OAIChatResponse {
        OAIChatResponse {
            id: "r".into(),
            object: "chat.completion".into(),
            created: 0,
            model: "m".into(),
            choices: vec![],
            usage: Some(Usage {
                prompt_tokens: prompt,
                completion_tokens: completion,
                total_tokens: prompt + completion,
                extra: Default::default(),
            }),
            extra: Default::default(),
            source_anthropic_response: None,
            source_gemini_response: None,
            source_responses_response: None,
        }
    }

    #[test]
    fn cost_uses_concrete_model_family() {
        assert!(cost_usd("claude-sonnet-5", 1_000_000, 0) > 2.9);
        assert!(cost_usd("claude-sonnet-5", 1_000_000, 0) < 3.1);
    }

    #[test]
    fn unpriced_model_records_zero_not_a_guess() {
        assert_eq!(cost_usd("some-unknown-model-v9", 1_000, 1_000), 0.0);
    }

    /// A successor must inherit its family's price even when its version suffix
/// rotates, and a cheaper variant sharing the prefix must match its own
/// family, not the flagship's.
#[test]
    fn prefix_match_is_stable_across_version_suffixes() {
        // Version-suffixed ids keep the family price.
        assert_eq!(
            rate_for("deepseek-reasoner-250905"),
            Some((0.55, 2.19)),
            "a dated successor must price as its family, not as unknown"
        );
        // A cheaper variant shares the `gpt-4o` prefix but is its own family:
        // the longest boundary-matching prefix wins, so it prices at 0.15/1M,
        // not the flagship's 2.5/1M.
        assert_eq!(
            rate_for("gpt-4o-mini"),
            Some((0.15, 0.6)),
            "gpt-4o-mini must match its own family, not gpt-4o"
        );
        assert_eq!(
            cost_usd("gpt-4o-mini", 1_000_000, 0),
            0.15,
            "gpt-4o-mini at the flagship rate would overstate the bill ~17x"
        );
    }

    #[test]
    fn missing_usage_block_yields_zero_tokens() {
        let mut resp = response_with(10, 20);
        resp.usage = None;
        assert_eq!(tokens_of(&resp), (0, 0));
    }

    #[test]
    fn tokens_are_read_from_usage() {
        assert_eq!(tokens_of(&response_with(11, 22)), (11, 22));
    }

    #[test]
    fn record_prefers_the_concrete_upstream_model() {
        let rec = build_record("claude", "p1", "claude-sonnet-5", "my-alias", &response_with(10, 20), 42);
        assert_eq!(rec.model, "claude-sonnet-5");
        assert_eq!(rec.app_id, "claude");
        assert_eq!(rec.provider_id, "p1");
        assert_eq!(rec.duration_ms, 42);
        assert!(rec.ts > 1_700_000_000);
    }

    #[test]
    fn record_falls_back_to_the_alias_when_no_hop_model() {
        let rec = build_record("claude", "p1", "", "my-alias", &response_with(10, 20), 1);
        assert_eq!(rec.model, "my-alias");
        assert_eq!(rec.cost_usd, 0.0);
    }

    #[test]
    fn missing_db_is_a_no_op() {
        // Must not panic: the router runs without a DB handle in tests.
        record(None, build_record("a", "p", "m", "m", &response_with(1, 1), 1));
    }
}