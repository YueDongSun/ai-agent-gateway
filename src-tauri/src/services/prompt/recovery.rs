//! Bounded output recovery. Only assistant-visible text is retained; reasoning
//! and upstream error bodies never become prompt output or diagnostics.
use super::{message, target, Prompt_usage, MIN_TOKENS};
use crate::{
    database::Database,
    proxy::{
        canonical::ChatCompletionRequest,
        router::{Router, SURFACE_CHAT},
    },
};
use std::sync::Arc;
use std::time::Duration;

pub const MAX_GENERATION_ATTEMPTS: u32 = 3;
pub const MAX_STAGE_CALLS: u32 = 6;
pub const MAX_TOTAL_OUTPUT_TOKENS: u32 = 32_768;
pub const CALL_TIMEOUT_SECS: u64 = 90;
const CONTEXT_MARGIN: u64 = 512;
const CONTINUE: &str = "Return only the missing continuation of your preceding answer. Continue exactly where it ended, without repeating completed sections or adding a preamble. Preserve all original requirements and finish the remaining sections. If the preceding answer is already complete, return only <complete/>.";

#[derive(Default)]
pub(super) struct Work_budget {
    pub calls: u32,
    pub allocated_tokens: u32,
    pub usage: Option<Prompt_usage>,
}

pub(super) struct Generation {
    pub text: String,
    pub model: String,
    pub provider: Option<String>,
    pub complete: bool,
    pub failure_code: Option<String>,
}

/// Conservative estimate, not a tokenizer claim: ASCII ~3 chars/token;
/// non-ASCII costs one token/character plus message framing and safety margin.
fn estimated_input(request: &ChatCompletionRequest) -> u64 {
    let conservative_input = request
        .messages
        .iter()
        .map(|message| {
            let text = message.content.as_str().unwrap_or("");
            let ascii = text.chars().filter(char::is_ascii).count() as u64;
            ascii.div_ceil(3) + text.chars().count() as u64 - ascii + 16
        })
        .sum::<u64>()
        + CONTEXT_MARGIN;
    let routing_input = crate::proxy::planner::request_features(request)
        .min_context_window
        .unwrap_or(0)
        .saturating_sub(u64::from(request.max_tokens.unwrap_or(0)));
    conservative_input.max(routing_input)
}

pub(super) fn output_budget(
    db: &Database,
    router: Option<&Arc<Router>>,
    target: &target::Prompt_target,
    request: &ChatCompletionRequest,
    desired: u32,
) -> Result<u32, String> {
    let hops = match target {
        target::Prompt_target::Gateway { model } => router
            .and_then(|router| {
                router
                    .preview_model(SURFACE_CHAT, model, Default::default(), None)
                    .ok()
            })
            .map(|hops| hops.as_ref().clone())
            .unwrap_or_default(),
        target::Prompt_target::Provider { provider_id, model } => {
            vec![crate::database::dao::routes::RouteHop {
                provider: provider_id.clone(),
                model: model.clone(),
            }]
        }
    };
    if hops.is_empty() {
        return Ok(desired);
    } // Dispatch owns missing-route classification.
    let snapshot = crate::services::suppliers::resolved_supplier_snapshot(db)
        .map_err(|_| "prompt_enhance_providers_unavailable".to_string())?;
    let mut limit = 0;
    let input = estimated_input(request);
    for hop in hops {
        let facts =
            crate::services::model_catalog::model_metadata(db, &format!("precise:{}", hop.model))
                .map_err(|_| "prompt_enhance_providers_unavailable".to_string())?
                .capabilities;
        let mut candidate_limit = u64::from(desired);
        // Endpoint limits apply only to their mapped provider and exact model.
        // A cheaper sibling or unavailable fallback cannot constrain this hop.
        let catalog_context = facts.as_ref().and_then(|facts| facts.context_window);
        let catalog_output = facts.as_ref().and_then(|facts| facts.max_output_tokens);
        let mut endpoint_limits = snapshot
            .endpoints
            .iter()
            .filter(|endpoint| {
                endpoint.legacy_proxy_provider_id.as_deref() == Some(hop.provider.as_str())
            })
            .flat_map(|endpoint| &endpoint.models)
            .filter(|model| model.enabled && model.model_id == hop.model)
            // Match planner enrichment: explicit endpoint facts win; public
            // catalog facts supply only fields the endpoint leaves unknown.
            .map(|model| {
                (
                    model.context_window.or(catalog_context),
                    model.max_output_tokens.or(catalog_output),
                )
            })
            .collect::<Vec<_>>();
        if endpoint_limits.is_empty() {
            endpoint_limits.push((catalog_context, catalog_output));
        }
        for (context, output) in endpoint_limits {
            if let Some(output) = output {
                candidate_limit = candidate_limit.min(output);
            }
            if let Some(context) = context {
                candidate_limit = candidate_limit.min(context.saturating_sub(input));
            }
        }
        // Dispatch repeats request-feature filtering with the resulting budget;
        // the strongest eligible capacity sets the request, not the weakest hop.
        limit = limit.max(candidate_limit);
    }

    if limit < u64::from(MIN_TOKENS) {
        return Err("prompt_enhance_context_budget_exhausted".into());
    }
    Ok(limit as u32)
}

pub(super) async fn call(
    db: &Database,
    router: Option<Arc<Router>>,
    target: &target::Prompt_target,
    request: &ChatCompletionRequest,
    budget: &mut Work_budget,
) -> Result<target::Prompt_dispatch, String> {
    call_selected(db, router, target, request, budget, None).await
}

async fn call_selected(
    db: &Database,
    router: Option<Arc<Router>>,
    target: &target::Prompt_target,
    request: &ChatCompletionRequest,
    budget: &mut Work_budget,
    hop: Option<&crate::database::dao::routes::RouteHop>,
) -> Result<target::Prompt_dispatch, String> {
    let allocation = request.max_tokens.unwrap_or(0);
    if budget.calls >= MAX_STAGE_CALLS
        || budget.allocated_tokens.saturating_add(allocation) > MAX_TOTAL_OUTPUT_TOKENS
    {
        return Err("prompt_enhance_recovery_budget_exhausted".into());
    }
    budget.calls += 1;
    budget.allocated_tokens += allocation;
    let dispatch = tokio::time::timeout(Duration::from_secs(CALL_TIMEOUT_SECS), async {
        match hop {
            Some(hop) => {
                target::dispatch_hop(
                    router.ok_or_else(|| "prompt_enhance_gateway_not_running".to_string())?,
                    request,
                    hop,
                )
                .await
            }
            None => target::dispatch(db, router, target, request).await,
        }
    })
    .await
    .map_err(|_| "prompt_enhance_upstream_timeout".to_string())??;
    if let Some(usage) = &dispatch.response.usage {
        let sum = budget.usage.get_or_insert(Prompt_usage {
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
        });
        sum.prompt_tokens = sum.prompt_tokens.saturating_add(usage.prompt_tokens);
        sum.completion_tokens = sum
            .completion_tokens
            .saturating_add(usage.completion_tokens);
        sum.total_tokens = sum.total_tokens.saturating_add(usage.total_tokens);
    }
    Ok(dispatch)
}

fn alternate_hop(
    db: &Database,
    router: Option<&Arc<Router>>,
    target: &target::Prompt_target,
    request: &ChatCompletionRequest,
    desired: u32,
    exhausted: &std::collections::HashSet<(String, String)>,
) -> Option<crate::database::dao::routes::RouteHop> {
    let target::Prompt_target::Gateway { model } = target else {
        return None;
    };
    let router = router?;
    let exact = model
        .strip_prefix("precise:")
        .map(str::trim)
        .or_else(|| router.offers_exact_model(model).then_some(model.as_str()));
    let candidates = router
        .preview_model(SURFACE_CHAT, model, Default::default(), None)
        .ok()?;
    for hop in candidates.iter() {
        if exhausted.contains(&(hop.provider.clone(), hop.model.clone()))
            || exact.is_some_and(|exact| hop.model != exact)
        {
            continue;
        }
        let candidate_target = target::Prompt_target::Provider {
            provider_id: hop.provider.clone(),
            model: hop.model.clone(),
        };
        let mut candidate_request = request.clone();
        let Ok(tokens) = output_budget(
            db,
            Some(router),
            &candidate_target,
            &candidate_request,
            desired,
        ) else {
            continue;
        };
        candidate_request.max_tokens = Some(tokens);
        let Ok(eligible) = router.preview_model(
            SURFACE_CHAT,
            model,
            crate::proxy::planner::request_features(&candidate_request),
            None,
        ) else {
            continue;
        };
        if eligible
            .iter()
            .any(|candidate| candidate.provider == hop.provider && candidate.model == hop.model)
        {
            return Some(hop.clone());
        }
    }
    None
}

pub(super) async fn generate(
    db: &Database,
    router: Option<Arc<Router>>,
    target: &target::Prompt_target,
    request: ChatCompletionRequest,
    budget: &mut Work_budget,
    attempts: u32,
) -> Result<Generation, String> {
    let mut result = Generation {
        text: String::new(),
        model: target.model().into(),
        provider: None,
        complete: false,
        failure_code: None,
    };
    let original_messages = request.messages.clone();
    let desired = request.max_tokens.unwrap_or(super::DEFAULT_EN_MAX_TOKENS);
    let mut next = request;
    let mut selected_hop: Option<crate::database::dao::routes::RouteHop> = None;
    let mut exhausted = std::collections::HashSet::new();
    for index in 0..attempts {
        let dispatch = async {
            let selected_target =
                selected_hop
                    .as_ref()
                    .map(|hop| target::Prompt_target::Provider {
                        provider_id: hop.provider.clone(),
                        model: hop.model.clone(),
                    });
            let mut limit = output_budget(
                db,
                router.as_ref(),
                selected_target.as_ref().unwrap_or(target),
                &next,
                desired,
            );
            if limit.as_ref().err().map(String::as_str)
                == Some("prompt_enhance_context_budget_exhausted")
            {
                if let Some(hop) = &selected_hop {
                    exhausted.insert((hop.provider.clone(), hop.model.clone()));
                }
                if let Some(alternate) =
                    alternate_hop(db, router.as_ref(), target, &next, desired, &exhausted)
                {
                    let alternate_target = target::Prompt_target::Provider {
                        provider_id: alternate.provider.clone(),
                        model: alternate.model.clone(),
                    };
                    limit = output_budget(db, router.as_ref(), &alternate_target, &next, desired);
                    selected_hop = Some(alternate);
                }
            }
            next.max_tokens = Some(limit?);
            call_selected(
                db,
                router.clone(),
                target,
                &next,
                budget,
                selected_hop.as_ref(),
            )
            .await
        }
        .await;
        let dispatch = match dispatch {
            Ok(dispatch) => dispatch,
            Err(code) if !result.text.is_empty() => {
                result.failure_code = Some(code);
                break;
            }
            Err(code) => return Err(code),
        };
        result.model = dispatch.response.model;
        result.provider = dispatch.provider;
        let choice = dispatch.response.choices.first();
        let finish = choice.and_then(|choice| choice.finish_reason.as_deref());
        let content = choice
            .and_then(|choice| choice.message.content.as_deref())
            .unwrap_or("");
        if matches!(
            finish,
            Some("content_filter" | "tool_calls" | "function_call")
        ) {
            result.failure_code = Some("prompt_enhance_unexpected_termination".into());
            break;
        }
        let previous = result.text.clone();
        if content.trim() != "<complete/>" {
            result.text = append_continuation(&result.text, content);
        }
        let truncated =
            matches!(finish, Some("length" | "max_tokens")) || unmatched_fence(&result.text);
        if index > 0 && previous == result.text && content.trim() != "<complete/>" {
            result.failure_code = Some("prompt_enhance_continuation_stalled".into());
            exhausted.insert((
                dispatch.serving_hop.provider.clone(),
                dispatch.serving_hop.model.clone(),
            ));
            selected_hop = alternate_hop(db, router.as_ref(), target, &next, desired, &exhausted);
            if selected_hop.is_none() {
                break;
            }
            continue;
        }
        if !truncated && finish == Some("stop") && !result.text.trim().is_empty() {
            result.complete = true;
            break;
        }
        result.failure_code = Some(
            if truncated {
                "prompt_enhance_recovery_budget_exhausted"
            } else {
                "prompt_enhance_incomplete_response"
            }
            .into(),
        );
        next.messages = original_messages.clone();
        if !result.text.is_empty() {
            next.messages
                .push(message("assistant", result.text.clone()));
        }
        next.messages.push(message("user", CONTINUE.into()));
    }
    if result.complete {
        result.failure_code = None;
    }
    if result.text.trim().is_empty() {
        return Err(result
            .failure_code
            .unwrap_or_else(|| "prompt_enhance_empty_response".into()));
    }
    result.text = super::strip_wrapping(&result.text);
    Ok(result)
}

fn unmatched_fence(text: &str) -> bool {
    text.lines()
        .filter(|line| line.trim_start().starts_with("```"))
        .count()
        % 2
        == 1
}

/// Exact suffix/prefix overlap only: fuzzy deduplication can delete constraints.
pub(super) fn append_continuation(previous: &str, next: &str) -> String {
    if previous.is_empty() || next.is_empty() {
        return format!("{previous}{next}");
    }
    // Prefix-function matching bounds the work linearly even for long briefs
    // with repeated sections. Whitespace and Unicode boundaries stay intact.
    let bytes = next.as_bytes();
    let mut prefix = vec![0; bytes.len()];
    for index in 1..bytes.len() {
        let mut length = prefix[index - 1];
        while length > 0 && bytes[index] != bytes[length] {
            length = prefix[length - 1];
        }
        if bytes[index] == bytes[length] {
            length += 1;
        }
        prefix[index] = length;
    }
    let mut overlap = 0;
    for byte in previous.bytes() {
        while overlap > 0 && (overlap == bytes.len() || byte != bytes[overlap]) {
            overlap = prefix[overlap - 1];
        }
        if byte == bytes[overlap] {
            overlap += 1;
        }
    }
    // Short suffixes are often the start of the next word, not repetition:
    // "map" + "ping" must produce "mapping", never "maping". Require a
    // substantial exact overlap or a complete repeated response.
    if !next.is_char_boundary(overlap) || (overlap < 12 && previous != next) {
        overlap = 0;
    }
    format!("{previous}{}", &next[overlap..])
}
