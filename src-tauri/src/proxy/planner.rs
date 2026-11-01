//! Capability-alias planning hook for the router.
//!
//! Selector namespaces are resolved here as data only; the actual tier
//! assessment and candidate planning live behind [`Model_planner`], which is
//! implemented by the supplier/model-catalog integration so this module (and
//! the whole proxy layer) stays free of service dependencies.
//!
//! Selection contract, in order:
//!
//! 1. `precise:<id>` — the literal model id only. Never substituted.
//! 2. `alias:<name>` — always a capability alias.
//! 3. A bare name that is canonically identical to an actually offered model
//!    is an exact request: only the same model is ever selected.
//! 4. Other bare names classify through the known alias/family rules and
//!    become capability aliases (traceable downgrades allowed).
//! 5. Anything else stays on the legacy resolution chain.

use crate::proxy::canonical::{self, ChatCompletionRequest, ProviderError};

/// Keep the selected head, prefer new providers/models next, then retain every
/// remaining candidate in its prior order. Diversity never removes a fallback.
pub(crate) fn diverse_fallback_order<T>(items: Vec<T>, identity: impl for<'a> Fn(&'a T) -> (&'a str, &'a str)) -> Vec<T> {
    let mut providers = std::collections::HashSet::new();
    let mut models = std::collections::HashSet::new();
    let mut first = Vec::new();
    let mut rest = Vec::new();
    for item in items {
        let (provider, model) = identity(&item);
        if !providers.contains(provider) && !models.contains(model) {
            providers.insert(provider.to_string());
            models.insert(model.to_string());
            first.push(item);
        } else { rest.push(item); }
    }
    first.extend(rest);
    first
}

/// Request-derived hard requirements. A coarse lower bound is fine for the
/// context window: it only has to keep clearly-undersized models out, never
/// to be a precise tokenizer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[allow(non_camel_case_types)]
pub struct Request_features {
    pub needs_tools: bool,
    pub needs_images: bool,
    pub min_context_window: Option<u64>,
    pub min_max_output_tokens: Option<u64>,
}

/// Extract the hard requirements a candidate must satisfy. Unknown content
/// shapes count as "no extra requirement" rather than blocking the request.
pub fn request_features(req: &ChatCompletionRequest) -> Request_features {
    let needs_tools =
        req.tools.as_ref().is_some_and(|tools| !tools.is_empty()) || req.tool_choice.is_some();
    let mut text_chars: usize = 0;
    let mut needs_images = false;
    for message in &req.messages {
        text_chars = text_chars.saturating_add(count_text_chars(&message.content));
        if !needs_images && content_has_image(&message.content) {
            needs_images = true;
        }
    }
    // Coarse floor: ~4 characters per token plus the requested completion.
    let prompt_floor = (text_chars as u64 / 4).saturating_add(64);
    let min_context_window = prompt_floor
        .checked_add(req.max_tokens.unwrap_or(0) as u64)
        .map(|total| total.saturating_add(1024));
    Request_features {
        needs_tools,
        needs_images,
        min_context_window,
        min_max_output_tokens: req.max_tokens.map(|value| value as u64),
    }
}

fn count_text_chars(value: &serde_json::Value) -> usize {
    match value {
        serde_json::Value::String(text) => text.chars().count(),
        serde_json::Value::Array(items) => items.iter().map(count_text_chars).sum(),
        serde_json::Value::Object(map) => map.get("text").map(count_text_chars).unwrap_or_default(),
        _ => 0,
    }
}

fn content_has_image(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Array(items) => items.iter().any(content_has_image),
        serde_json::Value::Object(map) => {
            matches!(
                map.get("type").and_then(|kind| kind.as_str()),
                Some("image") | Some("image_url") | Some("input_image")
            ) || map
                .get("image_url")
                .is_some_and(|nested| nested.is_object() || nested.is_string())
                || map.get("source").is_some_and(content_has_image)
        }
        _ => false,
    }
}

/// One planned hop plus the planning metadata recorded for tracing. The
/// metadata is content-free: model ids, tier numbers, and stable reason
/// codes only.
#[derive(Debug, Clone)]
#[allow(non_camel_case_types)]
pub struct Planned_hop {
    pub provider: String,
    pub model: String,
    pub tier: Option<u8>,
    /// Configured supplier/endpoint priority. Native-protocol preference may
    /// reorder ties only inside the same assessed tier and this priority.
    pub priority: i64,
    pub basis: String,
    pub downgraded: bool,
    pub reason: String,
}

/// What the planner decided about one requested model name.
#[derive(Debug, Clone)]
#[allow(non_camel_case_types)]
pub enum Resolution {
    /// Not an alias decision: continue the legacy chain with this model name
    /// (the `precise:` namespace has already been stripped).
    Exact(String),
    /// An exact model call: only offers of this canonical model qualify.
    /// An empty chain means no eligible same-model supply and must surface
    /// `model_precise_unavailable` — never a substitute, never a blind send.
    Precise {
        model: String,
        hops: Vec<Planned_hop>,
    },
    /// A capability-alias plan. An empty chain means no eligible candidate
    /// and must surface `model_alias_no_candidates` — never a blind send.
    Alias {
        alias: String,
        tier: u8,
        hops: Vec<Planned_hop>,
    },
}

/// Strip the planned hop metadata down to the executable chain.
pub fn planned_hops(
    hops: Vec<Planned_hop>,
) -> std::sync::Arc<Vec<crate::database::dao::routes::RouteHop>> {
    std::sync::Arc::new(
        hops.into_iter()
            .map(|hop| crate::database::dao::routes::RouteHop {
                provider: hop.provider,
                model: hop.model,
            })
            .collect(),
    )
}

/// Stable capability-alias selectors advertised on every model list next to
/// the exact ids. Names are bare because clients request them bare; the
/// explicit `alias:` namespace stays accepted and is listed in parallel so
/// the two selector classes are unambiguous in UIs. The rank is relative
/// only (larger = stronger); mapping ranks to real models is the planner's
/// decision, never a fixed model id.
pub const CAPABILITY_ALIAS_SELECTORS: &[(&str, u8)] =
    &[("fable", 3), ("opus", 2), ("sonnet", 1), ("haiku", 0)];

/// Client compatibility names and their requested routing tiers. These are
/// policy selectors, not evidence about any underlying offered model.
/// Codex Sol requests strongest, Terra balanced, and Luna fast capability.
pub const CLIENT_ROLE_SELECTORS: &[(&str, u8)] = &[
    ("claude-fable-5", 3),
    ("claude-opus-5", 2),
    ("claude-sonnet-5", 1),
    ("claude-haiku-5-5", 0),
    ("gpt-5.6-sol", 3),
    ("gpt-5.6-terra", 1),
    ("gpt-5.6-luna", 0),
];

pub(crate) fn client_role_tier(selector: &str) -> Option<u8> {
    let selector = crate::proxy::strip_one_m_suffix(selector);
    CLIENT_ROLE_SELECTORS
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(selector.trim()))
        .map(|(_, tier)| *tier)
}

/// Implemented by the supplier/model-catalog integration and installed on
/// the router at snapshot build time. Implementations must be pure readers:
/// no database writes, no network, no logging of secrets.
#[allow(non_camel_case_types)]
pub trait Model_planner: Send + Sync {
    /// Exclude recent model failures before allocating supplier slots. Exact
    /// selectors and implementations without allocation keep their contract.
    fn resolve_excluding(&self, raw_model: &str, features: Request_features,
        _excluded: &std::collections::HashSet<(String, String)>) -> Result<Resolution, ProviderError> {
        self.resolve(raw_model, features)
    }
    /// Optional safe metadata from the planner's existing immutable inputs.
    /// This cannot perform I/O or affect candidate resolution.
    fn routing_identity(&self, _provider_id: &str) -> Option<crate::proxy::routing_receipts::contract::RoutingIdentity> { None }

    fn resolve(
        &self,
        raw_model: &str,
        features: Request_features,
    ) -> Result<Resolution, ProviderError>;
}

/// The explicit error for an alias with no eligible candidates. Stable code,
/// non-retriable: retrying cannot create an offer that does not exist.
pub fn alias_no_candidates_error(alias: &str) -> ProviderError {
    let _ = alias;
    canonical::make_provider_error(
        "model_alias_no_candidates",
        "route_resolution",
        Some(404),
        Some(false),
        None,
    )
}

/// The explicit error for `precise:<id>` when no offer carries that exact
/// model. Never substituted, never blind-queued.
pub fn precise_unavailable_error(model: &str) -> ProviderError {
    let _ = model;
    canonical::make_provider_error(
        "model_precise_unavailable",
        "route_resolution",
        Some(404),
        Some(false),
        None,
    )
}

/// The explicit error for `alias:<name>` when the name is not a known
/// capability alias.
pub fn alias_unknown_error(alias: &str) -> ProviderError {
    let _ = alias;
    canonical::make_provider_error(
        "model_alias_unknown",
        "route_resolution",
        Some(404),
        Some(false),
        None,
    )
}

/// The explicit error for an empty or malformed selector namespace.
pub fn selector_invalid_error() -> ProviderError {
    canonical::make_provider_error(
        "model_selector_invalid",
        "route_resolution",
        Some(400),
        Some(false),
        None,
    )
}
