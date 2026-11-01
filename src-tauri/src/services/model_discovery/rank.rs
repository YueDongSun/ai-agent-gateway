//! Model id version ranking for auto-discovery.
//!
//! Pure logic, no I/O. The discovery service anchors on every model id the
//! user already configured for a provider (pool `models`, route hop models,
//! `model_mapping` values) and looks for a strictly newer id of the same
//! family in the upstream `/v1/models` list. Never-downgrade is structural:
//! only candidates that rank strictly above the anchor qualify, and an
//! ambiguous ranking resolves to "no change".

use std::cmp::Ordering;
use std::collections::HashMap;

use super::fetch::UpstreamModel;

/// Outcome of ranking one anchor against the upstream list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Successor {
    /// No same-family candidate ranks above the anchor.
    None,
    /// Same-family candidates exist but no single one ranks decisively
    /// above the others (e.g. `deepseek-chat` vs `deepseek-reasoner`).
    Ambiguous,
    Found(UpstreamModel),
}

/// One version token, parsed as `[alpha head][digits][alpha tail]`:
/// `"6astsa"` = ("", 6, "astsa"), `"m3"` = ("m", 3, ""), `"4o"` =
/// ("", 4, "o"), `"chat"` = ("chat", None, ""). The alpha head lets
/// `m3`/`m4`-style ids compare numerically while staying incomparable to
/// differently-headed tokens (`m3` vs `text`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Token {
    head: String,
    num: Option<u64>,
    tail: String,
}

fn parse_token(s: &str) -> Token {
    let head_len = s.find(|c: char| c.is_ascii_digit()).unwrap_or(s.len());
    let after_head = &s[head_len..];
    let digits = after_head
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .count();
    let num = if digits > 0 {
        after_head[..digits].parse::<u64>().ok()
    } else {
        None
    };
    Token {
        head: s[..head_len].to_string(),
        num,
        tail: after_head[digits..].to_string(),
    }
}

/// Lowercase, trim, drop `[...]` context markers such as the local `[1M]`
/// long-context suffix (mirrors `proxy::strip_one_m_suffix`, which only
/// knows that one marker), and drop OpenRouter-style routing-variant
/// suffixes (`:batch`, `:free`, `:thinking`) — those are pricing/queue
/// markers, not versions, and a base model must never "upgrade" into its
/// own variant.
pub(crate) fn normalize_id(id: &str) -> String {
    let trimmed = id.trim().to_lowercase();
    let mut out = String::with_capacity(trimmed.len());
    let mut depth = 0usize;
    for ch in trimmed.chars() {
        match ch {
            '[' => depth += 1,
            ']' => depth = depth.saturating_sub(1),
            c if depth == 0 => out.push(c),
            _ => {}
        }
    }
    let out = out.trim();
    match out.find(':') {
        Some(i) => out[..i].trim_end().to_string(),
        None => out.to_string(),
    }
}

/// Tier tokens that mark cheaper or experimental variants. A successor may
/// never introduce one the anchor lacks: `glm-5.3` must not "upgrade" to
/// `glm-5-3-flash-…`, and `deepseek-v4-flash` not to `…-vision-exp`.
/// Compound spellings count too when the extra part is at most two
/// characters (`glm-5.3-flashx`), so unrelated tokens like `minimax`
/// (beyond `mini` by three) are never mistaken for a tier.
const LOW_TIERS: &[&str] = &[
    "flash", "lite", "mini", "nano", "air", "exp", "free", "batch",
];

pub(crate) fn is_low_tier_token(token: &str) -> bool {
    LOW_TIERS.iter().any(|w| {
        token == *w || (token.len() > w.len() && token.starts_with(w) && token.len() - w.len() <= 2)
    })
}

/// Split a normalized id into lowercase tokens the ranking compares.
pub(crate) fn tokenize(normalized: &str) -> Vec<String> {
    normalized
        .split(['-', '_', '.', '/', ' '])
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

/// Decisive version ordering between two raw ids: `Ordering::Less` when `a`
/// is strictly older than `b`, `Greater` when strictly newer, and `Equal`
/// for both "same version" and "no decisive ordering". Callers that must
/// distinguish those two use [`remainder_exact`]. Used by the model catalog
/// family-ordinal tier inheritance, which strips date tokens first.
pub(crate) fn version_cmp_normalized(a: &str, b: &str) -> Ordering {
    version_cmp(
        &remainder_tokens(&normalize_id(a)),
        &remainder_tokens(&normalize_id(b)),
    )
}

/// True when two ids share an identical version remainder after family
/// normalization (`claude-sonnet-4-5` and `claude-sonnet-4.5` compare equal).
pub(crate) fn remainder_exact(a: &str, b: &str) -> bool {
    remainder_tokens(&normalize_id(a)) == remainder_tokens(&normalize_id(b))
}

fn introduces_new_low_tier(anchor_tokens: &[String], candidate_norm: &str) -> bool {
    tokenize(candidate_norm)
        .into_iter()
        .any(|t| is_low_tier_token(&t) && !anchor_tokens.contains(&t))
}

/// Split a normalized id into (family token count, family key). The family
/// is the longest leading run of digit-free tokens, capped at two tokens;
/// when even the first token carries a digit the first token alone is the
/// family. `minimax-m3.1` -> "minimax", `claude-sonnet-4-6` ->
/// "claude-sonnet", `gpt-6astsa` -> "gpt", `deepseek-chat` ->
/// "deepseek-chat", `qwen3-max` -> "qwen3".
fn family_split(normalized: &str) -> (usize, String) {
    let tokens = tokenize(normalized);
    if tokens.is_empty() {
        return (0, String::new());
    }
    let mut count = 0usize;
    for t in &tokens {
        if count >= 2 || t.chars().any(|c| c.is_ascii_digit()) {
            break;
        }
        count += 1;
    }
    if count == 0 {
        return (1, tokens[0].clone());
    }
    (count, tokens[..count].join("-"))
}

pub(crate) fn family_key(normalized: &str) -> String {
    family_split(normalized).1
}

fn remainder_tokens(normalized: &str) -> Vec<Token> {
    let tokens = tokenize(normalized);
    let (count, _) = family_split(normalized);
    tokens[count..].iter().map(|t| parse_token(t)).collect()
}

/// Compare two version remainders (the tokens after the family prefix).
///
/// Pairwise token compare: matching alpha heads order numerically (`m3` <
/// `m4`), numeric bases dominate (`5` > `4.x` at any depth); equal numeric
/// bases order by tail, where a bare number loses to a suffixed one (`6` <
/// `6astsa`) only when both remainders end at that slot — `gpt-4.1` vs
/// `gpt-4o` (extra trailing token vs suffix at the same slot) must not
/// guess. Differing alpha heads (`m3` vs `text`, `chat` vs `reasoner`) and
/// numeric-vs-alpha slots are incomparable. A strictly longer remainder
/// wins when the common prefix ties (`m3` < `m3.1`). `Ordering::Equal`
/// means "no decisive ordering" and callers must treat it as ambiguous.
fn version_cmp(a: &[Token], b: &[Token]) -> Ordering {
    let n = a.len().min(b.len());
    for i in 0..n {
        if a[i] == b[i] {
            continue;
        }
        let both_numeric = a[i].head.is_empty() && b[i].head.is_empty();
        let same_head = !a[i].head.is_empty() && a[i].head == b[i].head;
        if !both_numeric && !same_head {
            return Ordering::Equal;
        }
        let (x, y) = match (a[i].num, b[i].num) {
            (Some(x), Some(y)) => (x, y),
            // Same head but only one side carries a number: incomparable.
            _ => return Ordering::Equal,
        };
        let ord = x.cmp(&y);
        if !ord.is_eq() {
            return ord;
        }
        // Equal numeric cores: order by tail only when both remainders end
        // here; otherwise the shapes differ and the slot is incomparable.
        return match (a[i].tail.is_empty(), b[i].tail.is_empty()) {
            (true, true) => Ordering::Equal,
            (true, false) if i + 1 == a.len() && i + 1 == b.len() => Ordering::Less,
            (false, true) if i + 1 == a.len() && i + 1 == b.len() => Ordering::Greater,
            _ => Ordering::Equal,
        };
    }
    a.len().cmp(&b.len())
}

/// Bundled ranking assist for families whose version encoding the generic
/// heuristic cannot decisively order. Each entry lists its family's ids in
/// ascending (oldest first) order; discovery picks the highest-ranked
/// candidate above the anchor. Updated per app release; no network access.
pub(crate) static FAMILY_HINTS: &[(&str, &[&str])] = &[
    (
        "minimax",
        &["minimax-m1", "minimax-m2", "minimax-m3", "minimax-m3.1"],
    ),
    // Kimi vendors mix generational ids (`kimi-k3`) with dated snapshots
    // (`kimi-k2-250905`) and plan suffixes (`kimi-k2.7-code`); the generic
    // heuristic reads the snapshot date as a huge version. The table fixes
    // the order, and the contradiction veto below blocks heuristic winners
    // the table says are older than the anchor.
    ("kimi", &["kimi-k2-250905", "kimi-k2.7-code", "kimi-k3"]),
];

fn hint_rank(hints: &[(&str, &[&str])], family: &str, normalized_id: &str) -> Option<usize> {
    hints
        .iter()
        .find(|(fam, _)| *fam == family)
        .and_then(|(_, ids)| ids.iter().position(|id| *id == normalized_id))
}

/// Rank one anchor id against the upstream candidate list using the bundled
/// [`FAMILY_HINTS`] assist table.
pub(crate) fn find_successor(anchor: &str, candidates: &[UpstreamModel]) -> Successor {
    find_successor_with(anchor, candidates, FAMILY_HINTS)
}

/// Newest listed same-family member for a retired anchor. Unlike
/// [`find_successor`] the result may rank below the anchor: the anchor's own
/// generation is gone from the listing, so the newest live generation of the
/// family is the best remaining target. Cheap/experimental variants the
/// anchor did not already carry stay excluded, and an ambiguous tie keeps
/// the first (stable) candidate instead of guessing.
pub(crate) fn newest_listed_family_member(
    anchor: &str,
    candidates: &[UpstreamModel],
) -> Option<String> {
    let anchor_norm = normalize_id(anchor);
    if anchor_norm.is_empty() {
        return None;
    }
    let family = family_key(&anchor_norm);
    let anchor_tokens = tokenize(&anchor_norm);
    let mut best: Option<(String, String)> = None;
    for candidate in candidates {
        let norm = normalize_id(&candidate.id);
        if norm.is_empty() || norm == anchor_norm || family_key(&norm) != family {
            continue;
        }
        if introduces_new_low_tier(&anchor_tokens, &norm) {
            continue;
        }
        let replaces = best.as_ref().is_none_or(|(best_norm, _)| {
            version_cmp_normalized(&norm, best_norm) == Ordering::Greater
        });
        if replaces {
            best = Some((norm, candidate.id.clone()));
        }
    }
    best.map(|(_, id)| id)
}

/// [`find_successor`] with an injected hint table (test seam; also keeps the
/// shipped table's contents out of the ranking logic itself).
fn find_successor_with(
    anchor: &str,
    candidates: &[UpstreamModel],
    hints: &[(&str, &[&str])],
) -> Successor {
    let anchor_norm = normalize_id(anchor);
    if anchor_norm.is_empty() {
        return Successor::None;
    }
    let family = family_key(&anchor_norm);
    // The low-tier veto compares the anchor's tokens against every
    // candidate's; tokenize the anchor once.
    let anchor_tokens = tokenize(&anchor_norm);

    // Deduplicate candidates by normalized id (upstreams may list the same
    // id in several spellings); prefer the variant-free spelling (`:batch`
    // and friends are queue markers, never hop targets), else the newest
    // `created`. Same-family only; other families can never be successors.
    // Each surviving candidate memoizes its normalized id and remainder
    // tokens so the dedup probes and the winner scans below never
    // re-normalize or re-tokenize.
    struct Candidate {
        model: UpstreamModel,
        norm: String,
        rest: Vec<Token>,
    }
    let mut uniq: Vec<Candidate> = Vec::new();
    // Normalized id -> index into `uniq` (insertion order stays in the vec).
    let mut uniq_index: HashMap<String, usize> = HashMap::new();
    for c in candidates {
        let norm = normalize_id(&c.id);
        if norm.is_empty() || norm == anchor_norm || family_key(&norm) != family {
            continue;
        }
        // A candidate may not introduce a cheaper/experimental tier the
        // anchor does not already carry.
        if introduces_new_low_tier(&anchor_tokens, &norm) {
            continue;
        }
        match uniq_index.get(&norm).copied() {
            Some(index) => {
                let existing = &mut uniq[index];
                if existing.model.id.contains(':') && !c.id.contains(':') {
                    existing.model.id = c.id.clone();
                }
                if c.created.unwrap_or(i64::MIN) > existing.model.created.unwrap_or(i64::MIN) {
                    existing.model.created = c.created;
                }
            }
            None => {
                uniq_index.insert(norm.clone(), uniq.len());
                let rest = remainder_tokens(&norm);
                uniq.push(Candidate {
                    model: c.clone(),
                    norm,
                    rest,
                });
            }
        }
    }
    if uniq.is_empty() {
        return Successor::None;
    }

    // Hints resolve known ambiguous releases, but may predate the newest
    // catalog ids. Keep the best hinted release as the comparison baseline.
    let anchor_rank = hint_rank(hints, &family, &anchor_norm);
    let hinted_winner: Option<(UpstreamModel, Vec<Token>)> = anchor_rank.and_then(|anchor_rank| {
        uniq.iter()
            .filter_map(|candidate| {
                let rank = hint_rank(hints, &family, &candidate.norm)?;
                (rank > anchor_rank).then_some((rank, candidate))
            })
            .max_by_key(|(rank, _)| *rank)
            .map(|(_, candidate)| (candidate.model.clone(), candidate.rest.clone()))
    });

    let anchor_rest = remainder_tokens(&anchor_norm);
    let qualifying: Vec<&Candidate> = uniq
        .iter()
        .filter(|candidate| {
            if let (Some(anchor_rank), Some(candidate_rank)) =
                (anchor_rank, hint_rank(hints, &family, &candidate.norm))
            {
                // Known dated snapshots may look newer to the heuristic.
                if candidate_rank <= anchor_rank || hinted_winner.is_some() {
                    return false;
                }
            }
            version_cmp(&anchor_rest, &candidate.rest) == Ordering::Less
                && hinted_winner.as_ref().is_none_or(|(_, winner_rest)| {
                    version_cmp(winner_rest, &candidate.rest) == Ordering::Less
                })
        })
        .collect();
    if qualifying.is_empty() {
        return hinted_winner.map_or(Successor::None, |(winner, _)| Successor::Found(winner));
    }
    let winner = qualifying.iter().find(|candidate| {
        qualifying.iter().all(|other| {
            candidate.model.id == other.model.id
                || version_cmp(&candidate.rest, &other.rest) == Ordering::Greater
        })
    });
    winner.map_or(Successor::Ambiguous, |candidate| {
        Successor::Found(candidate.model.clone())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(id: &str, created: Option<i64>) -> UpstreamModel {
        UpstreamModel {
            id: id.to_string(),
            created,
        }
    }

    #[test]
    fn normalize_strips_context_markers_and_case() {
        assert_eq!(normalize_id("  MiniMax-M3.1 "), "minimax-m3.1");
        assert_eq!(normalize_id("claude-sonnet-4-5[1M]"), "claude-sonnet-4-5");
        assert_eq!(normalize_id("X[abc]Y"), "xy");
        assert_eq!(normalize_id("a/b-5.5:batch"), "a/b-5.5");
        assert_eq!(
            normalize_id("moonshotai/kimi-k3:free"),
            "moonshotai/kimi-k3"
        );
    }

    #[test]
    fn routing_variants_dedup_into_their_base_id() {
        // `:batch` is a queue marker: the base spelling wins the dedup and
        // a base anchor never "upgrades" into its own variant.
        let cands = [
            model("anthropic/claude-sonnet-5.5:batch", Some(9)),
            model("anthropic/claude-sonnet-5.5", Some(5)),
        ];
        assert_eq!(
            find_successor("anthropic/claude-sonnet-5", &cands),
            Successor::Found(model("anthropic/claude-sonnet-5.5", Some(9)))
        );
        assert_eq!(
            find_successor(
                "moonshotai/kimi-k3",
                &[model("moonshotai/kimi-k3:batch", Some(9))]
            ),
            Successor::None
        );
    }

    #[test]
    fn cheaper_or_experimental_tiers_never_upgrade() {
        // flash is a cheaper tier, not a newer version…
        assert_eq!(
            find_successor("glm-5.3", &[model("glm-5-3-flash-260828", Some(9))],),
            Successor::None
        );
        // …and an experimental build is not an upgrade of the stable one.
        assert_eq!(
            find_successor(
                "deepseek-v4-flash",
                &[model("deepseek-v4-flash-vision-exp", Some(9))],
            ),
            Successor::None
        );
        // Compound tier spellings gate as well (`flashx` is GLM's fast
        // tier), while unrelated tokens like `minimax` never trip the
        // `mini` rule.
        assert_eq!(
            find_successor("glm-5.3", &[model("glm-5.3-flashx", Some(9))]),
            Successor::None
        );
        assert_eq!(
            find_successor("MiniMax-M2.7", &[model("MiniMax-M3", Some(9))]),
            Successor::Found(model("MiniMax-M3", Some(9)))
        );
    }

    #[test]
    fn hinted_family_vetoes_contradicting_heuristic_winner() {
        // The bundled kimi table orders the dated snapshot below the
        // `-code` anchor; the heuristic reads 250905 as a huge version.
        assert_eq!(
            find_successor("kimi-k2.7-code", &[model("kimi-k2-250905", Some(9))],),
            Successor::None
        );
        // An unhinted, genuinely newer id still wins through.
        assert_eq!(
            find_successor("kimi-k2.7-code", &[model("kimi-k4", Some(9))]),
            Successor::Found(model("kimi-k4", Some(9)))
        );
    }

    #[test]
    fn duplicate_anchor_winners_deduplicate_in_plans() {
        // glm-5.3 and GLM-5.3 share a normalized id; the same winner must
        // be reported (and applied) once.
        use super::super::apply::plan_provider;
        use crate::database::dao::proxy_providers::ProxyProvider;
        let pp = ProxyProvider {
            id: "p".into(),
            name: "p".into(),
            provider_type: "anthropic".into(),
            api_key: String::new(),
            base_url: "https://upstream.example/v1".into(),
            models: vec!["glm-5.3".into(), "GLM-5.3".into()],
            enabled: true,
            timeout_ms: 60_000,
            weight: 1,
            circuit_breaker: Default::default(),
            model_mapping: Default::default(),
            extra_headers: Default::default(),
            auto_track_models: true,
            icon: None,
            icon_color: None,
        };
        let plan = plan_provider(&pp, &[], &[model("glm-5.3-pro", Some(9))]);
        assert_eq!(plan.added_models, vec!["glm-5.3-pro".to_string()]);
    }

    #[test]
    fn family_keys_split_on_first_digit_token() {
        assert_eq!(family_key(&normalize_id("minimax-m3.1")), "minimax");
        assert_eq!(
            family_key(&normalize_id("claude-sonnet-4-6")),
            "claude-sonnet"
        );
        assert_eq!(family_key(&normalize_id("gpt-6astsa")), "gpt");
        assert_eq!(family_key(&normalize_id("gpt-6")), "gpt");
        assert_eq!(family_key(&normalize_id("deepseek-chat")), "deepseek-chat");
        assert_eq!(
            family_key(&normalize_id("deepseek-reasoner")),
            "deepseek-reasoner"
        );
        assert_eq!(family_key(&normalize_id("qwen3-max")), "qwen3");
        assert_eq!(family_key(&normalize_id("gemini-2.5-pro")), "gemini");
    }

    #[test]
    fn minor_versions_rank_above_bare_anchor() {
        let cands = [
            model("MiniMax-M3", Some(100)),
            model("MiniMax-M3.1", Some(200)),
        ];
        assert_eq!(
            find_successor("MiniMax-M3", &cands),
            Successor::Found(model("MiniMax-M3.1", Some(200)))
        );
    }

    #[test]
    fn suffixed_same_number_ranks_above_bare_number() {
        let cands = [model("gpt-6astsa", Some(9))];
        assert_eq!(
            find_successor("gpt-6", &cands),
            Successor::Found(model("gpt-6astsa", Some(9)))
        );
    }

    #[test]
    fn never_downgrades_or_moves_laterally() {
        let cands = [model("minimax-m2", Some(1)), model("minimax-m3", Some(2))];
        assert_eq!(find_successor("minimax-m3.1", &cands), Successor::None);
    }

    #[test]
    fn numeric_base_beats_depth() {
        // claude-sonnet-5 > claude-sonnet-4-6 despite the shallower remainder.
        let cands = [
            model("claude-sonnet-4-6", Some(1)),
            model("claude-sonnet-5", Some(2)),
        ];
        assert_eq!(
            find_successor("claude-sonnet-4-6", &cands),
            Successor::Found(model("claude-sonnet-5", Some(2)))
        );
    }

    #[test]
    fn different_family_never_wins() {
        let cands = [model("claude-opus-5", Some(99))];
        assert_eq!(find_successor("claude-sonnet-4-6", &cands), Successor::None);
    }

    #[test]
    fn sibling_variants_never_upgrade_across_families() {
        // chat vs reasoner are different families by construction, so the
        // candidate is excluded before ranking even runs.
        let cands = [model("deepseek-reasoner", Some(2))];
        assert_eq!(find_successor("deepseek-chat", &cands), Successor::None);
    }

    #[test]
    fn equal_numeric_different_tails_are_incomparable() {
        // gpt-4o vs gpt-4.1 must not guess, so anchoring on gpt-4.1 with
        // gpt-4o listed yields no change.
        let cands = [model("gpt-4o", Some(1))];
        assert_eq!(find_successor("gpt-4.1", &cands), Successor::None);
    }

    #[test]
    fn duplicate_spellings_deduplicate_instead_of_ambiguating() {
        let cands = [
            model("MiniMax-M3.1", Some(5)),
            model("minimax-m3.1", Some(7)),
        ];
        assert_eq!(
            find_successor("minimax-m3", &cands),
            Successor::Found(model("MiniMax-M3.1", Some(7)))
        );
    }

    #[test]
    fn repeated_spellings_merge_identically_regardless_of_order() {
        // All four spellings share one normalized id: the variant-free
        // spelling wins the merged id only if it arrived while a routing
        // variant held the slot, and the newest `created` always wins.
        let spelled: Vec<UpstreamModel> = [
            "minimax-m3.1:free",
            "minimax-m3.1:batch",
            "minimax-m3.1",
            "MiniMax-M3.1",
        ]
        .iter()
        .enumerate()
        .map(|(i, id)| model(id, Some(10 + i as i64)))
        .collect();
        let mut reversed = spelled.clone();
        reversed.reverse();
        assert_eq!(
            find_successor("minimax-m3", &spelled),
            Successor::Found(model("minimax-m3.1", Some(13)))
        );
        assert_eq!(
            find_successor("minimax-m3", &reversed),
            Successor::Found(model("MiniMax-M3.1", Some(13)))
        );
    }

    #[test]
    fn equal_ranked_candidates_are_ambiguous() {
        // Two alpha-tail ids the heuristic cannot order relative to each
        // other, both above the anchor: refuse to guess.
        let cands = [model("x9-alpha", Some(1)), model("x9-beta", Some(2))];
        assert_eq!(find_successor("x9", &cands), Successor::Ambiguous);
    }

    #[test]
    fn family_hints_resolve_heuristic_ambiguity() {
        let hints: &[(&str, &[&str])] =
            &[("demo", &["demo-9-base", "demo-9-alpha", "demo-9-beta"])];
        // alpha vs beta is incomparable to the heuristic; the hint table
        // orders the family and picks the topmost candidate.
        let cands = [
            model("demo-9-beta", Some(1)),
            model("demo-9-alpha", Some(2)),
        ];
        assert_eq!(
            find_successor_with("demo-9-base", &cands, hints),
            Successor::Found(model("demo-9-beta", Some(1)))
        );
    }

    #[test]
    fn hint_fallback_uses_heuristics_for_unlisted_ids() {
        let hints: &[(&str, &[&str])] = &[("minimax", &["minimax-m1", "minimax-m2", "minimax-m3"])];
        // A brand-new id missing from the hint table still wins through the
        // numeric path (m4 > m3).
        let cands = [model("minimax-m4", Some(9))];
        assert_eq!(
            find_successor_with("minimax-m3", &cands, hints),
            Successor::Found(model("minimax-m4", Some(9)))
        );
    }

    #[test]
    fn shipped_hints_pick_newest_minimax() {
        let cands = [
            model("minimax-m3", Some(2)),
            model("minimax-m3.1", Some(3)),
            model("minimax-text-01", Some(4)),
        ];
        assert_eq!(
            find_successor("minimax-m2", &cands),
            Successor::Found(model("minimax-m3.1", Some(3)))
        );
    }

    #[test]
    fn parse_token_preserves_utf8_boundaries() {
        assert_eq!(
            parse_token("模型"),
            Token {
                head: "模型".into(),
                num: None,
                tail: "".into(),
            }
        );
        assert_eq!(
            parse_token("模型1"),
            Token {
                head: "模型".into(),
                num: Some(1),
                tail: "".into(),
            }
        );
        assert_eq!(
            parse_token("demo-2-β"),
            Token {
                head: "demo-".into(),
                num: Some(2),
                tail: "-β".into(),
            }
        );
        assert_eq!(
            parse_token("mβ3尾"),
            Token {
                head: "mβ".into(),
                num: Some(3),
                tail: "尾".into(),
            }
        );
    }

    #[test]
    fn unicode_model_ids_rank_without_boundary_panics() {
        assert_eq!(
            find_successor("demo-β1", &[model("demo-β2", Some(1))]),
            Successor::Found(model("demo-β2", Some(1)))
        );
        assert_eq!(
            find_successor("demo-模型1", &[model("demo-模型2", Some(1))]),
            Successor::Found(model("demo-模型2", Some(1)))
        );

        let ascii_anchor_catalog = [model("demo-2-β", Some(1)), model("demo-3-模型", Some(2))];
        assert_eq!(
            find_successor("demo-1", &ascii_anchor_catalog),
            Successor::Found(model("demo-3-模型", Some(2)))
        );
    }

    #[test]
    fn version_cmp_token_semantics() {
        let t = |s: &str| parse_token(s);
        // numeric base dominates at the first differing slot
        assert_eq!(version_cmp(&[t("4"), t("6")], &[t("5")]), Ordering::Less);
        // matching alpha heads order numerically (m3 < m4)
        assert_eq!(version_cmp(&[t("m3")], &[t("m4")]), Ordering::Less);
        // bare number loses to a suffixed number (both remainders end here)
        assert_eq!(version_cmp(&[t("6")], &[t("6astsa")]), Ordering::Less);
        // empty tail loses to a non-empty tail on the same numeric base
        assert_eq!(version_cmp(&[t("4")], &[t("4o")]), Ordering::Less);
        // suffix at slot 0 vs a longer remainder: incomparable (the
        // gpt-4.1 vs gpt-4o shape must not guess)
        assert_eq!(version_cmp(&[t("4"), t("1")], &[t("4o")]), Ordering::Equal);
        // different non-empty heads: incomparable
        assert_eq!(
            version_cmp(&[t("6"), t("alpha")], &[t("6"), t("beta")]),
            Ordering::Equal
        );
        // numeric vs alpha slot: incomparable
        assert_eq!(version_cmp(&[t("6")], &[t("beta")]), Ordering::Equal);
        // longer remainder wins when the common prefix ties
        assert_eq!(version_cmp(&[t("m3")], &[t("m3"), t("1")]), Ordering::Less);
    }

    #[test]
    fn mixed_catalog_prefers_newer_unlisted_release_over_hint_winner() {
        let candidates = [
            model("minimax-m3.1", Some(90)),
            model("minimax-m4", Some(1)),
            model("minimax-m5-lite", Some(100)),
        ];
        assert_eq!(
            find_successor("minimax-m2", &candidates),
            Successor::Found(model("minimax-m4", Some(1)))
        );
        let reversed: Vec<_> = candidates.into_iter().rev().collect();
        assert_eq!(
            find_successor("minimax-m2", &reversed),
            Successor::Found(model("minimax-m4", Some(1)))
        );
    }

    #[test]
    fn mixed_hint_catalog_keeps_dated_snapshot_veto() {
        let candidates = [
            model("kimi-k2-250905", Some(99)),
            model("kimi-k3", Some(2)),
            model("kimi-k4", Some(1)),
        ];
        assert_eq!(
            find_successor("kimi-k2.7-code", &candidates),
            Successor::Found(model("kimi-k4", Some(1)))
        );
    }
}
