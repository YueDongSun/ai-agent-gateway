//! Request-local generation controls for the explicit prompt workflow.
//! This policy is applied after hop selection, never from a client alias.
use crate::proxy::canonical::ChatCompletionRequest;
use std::borrow::Cow;

pub const PROMPT_APP_TYPE: &str = "prompt_enhance";
const KNOWN_ADAPTIVE_MODEL: &str = "MiniMax-M3.1-Flash";
const PROMPT_REASONING_EFFORT: &str = "low";

/// The known adaptive model otherwise spends the small prompt-generation
/// allowance on reasoning before returning visible text. Preserve any explicit
/// effort and native source envelope; all other purposes/protocols/models borrow
/// the original request unchanged. This adds no private field to the wire.
pub(crate) fn request_for_hop<'a>(
    purpose: &str,
    request: &'a ChatCompletionRequest,
    upstream_model: &str,
    wire_protocol: &str,
) -> Cow<'a, ChatCompletionRequest> {
    if purpose != PROMPT_APP_TYPE
        || wire_protocol != "anthropic"
        || upstream_model != KNOWN_ADAPTIVE_MODEL
        || request.extra.contains_key("reasoning_effort")
        || request.source_anthropic_request.is_some()
    {
        return Cow::Borrowed(request);
    }
    let mut adjusted = request.clone();
    adjusted
        .extra
        .insert("reasoning_effort".into(), PROMPT_REASONING_EFFORT.into());
    Cow::Owned(adjusted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_known_prompt_hop_receives_default_effort_and_explicit_controls_survive() {
        let request = ChatCompletionRequest {
            model: "logical-alias".into(),
            max_tokens: Some(1200),
            ..Default::default()
        };
        let adjusted =
            request_for_hop(PROMPT_APP_TYPE, &request, KNOWN_ADAPTIVE_MODEL, "anthropic");
        assert_eq!(adjusted.extra.get("reasoning_effort"), Some(&json!("low")));
        assert_eq!(adjusted.max_tokens, Some(1200));
        assert!(!request.extra.contains_key("reasoning_effort"));
        for (purpose, model, protocol) in [
            ("claude_code", KNOWN_ADAPTIVE_MODEL, "anthropic"),
            (PROMPT_APP_TYPE, "another-model", "anthropic"),
            (PROMPT_APP_TYPE, KNOWN_ADAPTIVE_MODEL, "openai"),
        ] {
            assert!(matches!(
                request_for_hop(purpose, &request, model, protocol),
                Cow::Borrowed(_)
            ));
        }
        for effort in [json!("high"), json!(null)] {
            let mut explicit = request.clone();
            explicit
                .extra
                .insert("reasoning_effort".into(), effort.clone());
            let adjusted = request_for_hop(
                PROMPT_APP_TYPE,
                &explicit,
                KNOWN_ADAPTIVE_MODEL,
                "anthropic",
            );
            assert!(matches!(adjusted, Cow::Borrowed(_)));
            assert_eq!(adjusted.extra["reasoning_effort"], effort);
        }
        let mut native = request;
        native.source_anthropic_request = Some(json!({"output_config":{"effort":"high"}}));
        assert!(matches!(
            request_for_hop(PROMPT_APP_TYPE, &native, KNOWN_ADAPTIVE_MODEL, "anthropic"),
            Cow::Borrowed(_)
        ));
    }
}
