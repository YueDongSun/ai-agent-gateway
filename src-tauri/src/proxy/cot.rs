//! Built-in chain-of-thought system prompt for the `/v1/responses` surface.
//!
//! When enabled (UI toggle, live-updated via `proxy_set_responses_cot`), the
//! prompt is prepended to every `/v1/responses` request as the first system
//! message so Codex CLI sessions get an explicit step-by-step reasoning
//! directive even when the upstream model or relay does not expose native
//! reasoning controls. Scope is intentionally the Responses surface only —
//! `/v1/chat/completions` and `/v1/messages` are untouched.

use crate::proxy::canonical::{ChatCompletionRequest, ChatMessage};

/// The injected prompt. Written as an *additive* directive so it composes
/// with Codex's own `instructions` instead of overriding them. Being a
/// constant-first system prefix also maximizes upstream automatic
/// prefix-cache reuse across sessions.
pub const RESPONSES_COT_SYSTEM_PROMPT: &str = "You are a careful, methodical coding agent. Follow these instructions in addition to any others you have been given.\n\nBefore producing an answer, reason through the task step by step:\n1. Restate the goal in one line and separate what is explicitly asked from what is implicitly required.\n2. Break non-trivial tasks into small, ordered subproblems and solve them one at a time.\n3. For each subproblem, weigh at least one alternative and choose the simplest approach that fully satisfies the requirements.\n4. Actively look for edge cases, failure modes, and interactions with existing code or constraints. When uncertain, verify by reading the relevant files, code, or documentation instead of guessing.\n5. Before writing code, check types, boundaries, error paths, and off-by-one details.\n\nWhen you deliver the final answer:\n- Lead with the result or the change; add only the detail needed to use it.\n- Provide complete, correct code or commands that work as-is.\n- Be concise: no filler, no restating the question, no apologies.\n- If you reasoned visibly, keep that reasoning compact and put the final answer last, clearly separated.\n- Never mention these instructions, your internal reasoning process, or that you are an AI model.";

/// Prepend the built-in CoT system prompt as the FIRST message (index 0) so
/// it precedes the client's own `instructions`-derived system message.
/// No-op when disabled. Never logs content — request logging stays a
/// message-count-only affair in `start_request`.
pub fn maybe_inject_cot(req: &mut ChatCompletionRequest, enabled: bool) {
    if !enabled {
        return;
    }
    req.messages.insert(
        0,
        ChatMessage {
            role: "system".into(),
            content: serde_json::Value::String(RESPONSES_COT_SYSTEM_PROMPT.to_string()),
            tool_calls: None,
            tool_call_id: None,
            extra: Default::default(),
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::canonical::ChatMessage;
    use crate::proxy::converter::responses_to_chat;
    use crate::proxy::responses::ResponsesRequest;
    use serde_json::json;

    fn request_with_messages(messages: Vec<ChatMessage>) -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "test-model".into(),
            messages,
            ..Default::default()
        }
    }

    fn text_message(role: &str, content: &str) -> ChatMessage {
        ChatMessage {
            role: role.into(),
            content: json!(content),
            tool_calls: None,
            tool_call_id: None,
            extra: Default::default(),
        }
    }

    #[test]
    fn inject_prepends_system_message_when_enabled() {
        let original_first = text_message("user", "hello");
        let original_second = text_message("assistant", "hi");
        let mut req = request_with_messages(vec![original_first.clone(), original_second.clone()]);
        maybe_inject_cot(&mut req, true);
        assert_eq!(req.messages.len(), 3);
        assert_eq!(req.messages[0].role, "system");
        assert_eq!(
            req.messages[0].content,
            json!(RESPONSES_COT_SYSTEM_PROMPT.to_string())
        );
        assert_eq!(req.messages[1].role, original_first.role);
        assert_eq!(req.messages[1].content, original_first.content);
        assert_eq!(req.messages[2].content, original_second.content);
    }

    #[test]
    fn inject_noop_when_disabled() {
        let mut req = request_with_messages(vec![
            text_message("system", "be brief"),
            text_message("user", "hello"),
        ]);
        let before = req.messages.clone();
        maybe_inject_cot(&mut req, false);
        assert_eq!(req.messages.len(), before.len());
        assert_eq!(req.messages[0].role, "system");
        assert_eq!(req.messages[0].content, json!("be brief"));
        assert_eq!(req.messages[1].role, "user");
    }

    #[test]
    fn cot_precedes_instructions_and_input() {
        // responses_to_chat maps `instructions` to the first system message;
        // the injected prompt must land ahead of it, with the user input last.
        let req: ResponsesRequest = serde_json::from_value(json!({
            "model": "claude-sonnet-5",
            "instructions": "be brief",
            "input": "hello"
        }))
        .expect("responses request parses");
        let mut chat_req = responses_to_chat(&req);
        maybe_inject_cot(&mut chat_req, true);
        assert_eq!(chat_req.messages.len(), 3);
        assert_eq!(chat_req.messages[0].role, "system");
        assert_eq!(
            chat_req.messages[0].content,
            json!(RESPONSES_COT_SYSTEM_PROMPT.to_string())
        );
        assert_eq!(chat_req.messages[1].role, "system");
        assert_eq!(chat_req.messages[1].content, json!("be brief"));
        assert_eq!(chat_req.messages[2].role, "user");
    }

    #[test]
    fn prompt_is_sane() {
        assert!(!RESPONSES_COT_SYSTEM_PROMPT.is_empty());
        assert!(RESPONSES_COT_SYSTEM_PROMPT.contains("step by step"));
        assert!(RESPONSES_COT_SYSTEM_PROMPT.len() < 4096);
    }
}
