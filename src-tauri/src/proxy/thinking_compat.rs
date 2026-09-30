//! Bounded compatibility repair for Anthropic thinking signatures.
//!
//! The normal path preserves thinking blocks byte-for-byte. A retry is only
//! allowed after an Anthropic-compatible endpoint explicitly rejects those
//! blocks or their signatures with a validation response.

use crate::proxy::converter::AnthropicRequest;
use serde_json::Value;

#[derive(Debug, Default, PartialEq, Eq)]
#[allow(non_camel_case_types)]
pub struct Thinking_rectification {
    pub removed_thinking_blocks: usize,
    pub removed_signature_fields: usize,
    pub removed_thinking_config: bool,
}

pub fn should_retry_without_thinking(status: u16, response_text: &str) -> bool {
    if !matches!(status, 400 | 422) {
        return false;
    }
    let lower = response_text.to_ascii_lowercase();
    if lower.contains("budget_tokens") {
        return false;
    }
    let signature_error = lower.contains("signature")
        && [
            "invalid",
            "required",
            "missing",
            "unsupported",
            "extra",
            "verify",
        ]
        .iter()
        .any(|needle| lower.contains(needle));
    let block_error = (lower.contains("thinking") || lower.contains("redacted_thinking"))
        && [
            "expected",
            "unsupported",
            "not supported",
            "extra_forbidden",
            "invalid block",
        ]
        .iter()
        .any(|needle| lower.contains(needle));
    signature_error || block_error
}

pub fn response_rejects_thinking_config(response_text: &str) -> bool {
    let lower = response_text.to_ascii_lowercase();
    lower.contains("thinking")
        && [
            "unsupported",
            "not supported",
            "extra_forbidden",
            "unknown field",
        ]
        .iter()
        .any(|needle| lower.contains(needle))
}

pub fn rectify_anthropic_request(
    request: &mut AnthropicRequest,
    remove_thinking_config: bool,
) -> Thinking_rectification {
    let mut result = Thinking_rectification::default();
    for message in &mut request.messages {
        let Value::Array(blocks) = &mut message.content else {
            continue;
        };
        blocks.retain_mut(|block| {
            let block_type = block.get("type").and_then(Value::as_str);
            if matches!(block_type, Some("thinking" | "redacted_thinking")) {
                result.removed_thinking_blocks += 1;
                return false;
            }
            if let Some(object) = block.as_object_mut() {
                if object.remove("signature").is_some() {
                    result.removed_signature_fields += 1;
                }
            }
            true
        });
    }
    if remove_thinking_config && request.extra.remove("thinking").is_some() {
        result.removed_thinking_config = true;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::converter::AnthropicMessage;
    use serde_json::json;

    fn request_with_thinking() -> AnthropicRequest {
        AnthropicRequest {
            model: "model".into(),
            max_tokens: 2048,
            system: None,
            messages: vec![AnthropicMessage {
                role: "assistant".into(),
                content: json!([
                    {"type": "thinking", "thinking": "hidden", "signature": "opaque"},
                    {"type": "text", "text": "answer"},
                    {"type": "tool_use", "id": "tool_1", "name": "lookup", "input": {}, "signature": "stray"}
                ]),
            }],
            temperature: None,
            stream: Some(true),
            stop_sequences: None,
            extra: [(
                "thinking".into(),
                json!({"type": "enabled", "budget_tokens": 1024}),
            )]
            .into_iter()
            .collect(),
        }
    }

    #[test]
    fn retries_only_explicit_thinking_compatibility_errors() {
        assert!(should_retry_without_thinking(
            400,
            "messages.1.content.0.signature: invalid signature"
        ));
        assert!(should_retry_without_thinking(
            422,
            "Expected thinking or redacted_thinking block"
        ));
        assert!(!should_retry_without_thinking(401, "invalid signature"));
        assert!(!should_retry_without_thinking(
            400,
            "budget_tokens must be less than max_tokens"
        ));
        assert!(!should_retry_without_thinking(400, "model not found"));
    }

    #[test]
    fn rectification_preserves_non_thinking_block_order() {
        let mut request = request_with_thinking();
        let result = rectify_anthropic_request(&mut request, true);
        assert_eq!(result.removed_thinking_blocks, 1);
        assert_eq!(result.removed_signature_fields, 1);
        assert!(result.removed_thinking_config);
        let blocks = request.messages[0].content.as_array().unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["type"], "text");
        assert_eq!(blocks[1]["type"], "tool_use");
        assert!(blocks[1].get("signature").is_none());
    }
}
