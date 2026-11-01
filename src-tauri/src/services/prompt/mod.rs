//! Prompt-enhancement pipeline.
//!
//! One user-facing capability with two explicit stages, both executed against
//! models already configured in the local AAG gateway:
//!
//! 1. zh_expand   - a short Chinese requirement becomes a complete Chinese
//!    brief (the "long sentence").
//! 2. en_optimize - that Chinese brief becomes the final optimized English
//!    prompt.
//!
//! Each stage uses bounded non-streaming completions, and one stage per IPC call.
//! That lets the UI show the Chinese brief while the English stage is still
//! running, and lets the user edit the brief and re-run only the second stage.
//! Nothing is persisted here: requests are built from the stored templates and
//! results return to the caller, which owns history and presentation.
//!
//! Privacy: prompt and response text never reaches diagnostics or logs. Only
//! stage ids, character counts, timings, and a content-free target label are
//! recorded.

mod recovery;
#[cfg(test)]
mod recovery_tests;
#[cfg(test)]
mod anthropic_wire_tests;
mod review;
pub mod target;
pub mod templates;
pub mod workspaces;

#[cfg(test)]
mod live_acceptance;

use crate::database::Database;
use crate::proxy::canonical::{ChatCompletionRequest, ChatMessage};
use crate::proxy::router::Router;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use std::time::Instant;

/// Upper bound for the short Chinese requirement (characters).
pub const MAX_SHORT_INPUT_CHARS: usize = 32_000;
/// Upper bound for the Chinese brief handed to the English stage (characters).
pub const MAX_DRAFT_CHARS: usize = 64_000;
/// Upper bound for the optional extra-guidance field (characters).
pub const MAX_INSTRUCTION_CHARS: usize = 1_000;
/// Default Chinese-stage output budget, retained for existing callers.
pub const DEFAULT_MAX_TOKENS: u32 = 1_200;
/// English optimization reserves the existing ceiling for reasoning and the
/// final prompt, including a full Chinese brief rather than only a short input.
pub const DEFAULT_EN_MAX_TOKENS: u32 = 8_192;
/// Smallest accepted output budget.
pub const MIN_TOKENS: u32 = 64;
/// Hard ceiling for a caller-supplied output budget.
pub const MAX_TOKENS_LIMIT: u32 = 8_192;
/// Default sampling temperature: enough variation to be useful, low enough to
/// stay faithful to the requirement.
pub const DEFAULT_TEMPERATURE: f64 = 0.6;

/// One pipeline invocation.
#[allow(non_camel_case_types)]
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Prompt_stage_request {
    /// Stage id; see templates::STAGES.
    pub stage: String,
    /// Short Chinese requirement (zh_expand) or Chinese brief (en_optimize).
    pub text: String,
    /// Which local model backend serves this stage.
    pub target: target::Prompt_target,
    /// Optional extra guidance appended to the user message.
    #[serde(default)]
    pub instruction: Option<String>,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    /// Explicit per-task project context. Omitted means no filesystem read.
    #[serde(default)]
    pub workspace_context: Option<workspaces::Workspace_context_selection>,
}

/// Token accounting for one stage, when the upstream reported it.
#[allow(non_camel_case_types)]
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Prompt_usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

/// One completed stage.
#[allow(non_camel_case_types)]
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Prompt_stage_output {
    pub stage: String,
    /// Normalized stage text: fences and wrapping quotes removed.
    pub output: String,
    /// False means recoverable partial work, never a completed prompt.
    pub complete: bool,
    pub failure_code: Option<String>,
    pub attempts: u32,
    pub review_status: String,
    /// Model reported by the backend that answered.
    pub model: String,
    /// Provider id that served the request when the gateway resolved it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Content-free target label, e.g. gateway:claude-sonnet-5.
    pub target_label: String,
    pub elapsed_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Prompt_usage>,
    /// Content-free receipt for the instruction snapshot actually used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_context: Option<workspaces::Workspace_context_preview>,
}

/// Built-in templates and limits, so the frontend can offer override editing
/// without duplicating prompt text.
#[allow(non_camel_case_types)]
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Prompt_defaults {
    pub zh_system: String,
    pub en_system: String,
    pub zh_setting_key: String,
    pub en_setting_key: String,
    pub max_system_prompt_chars: usize,
    pub max_short_input_chars: usize,
    pub max_draft_chars: usize,
    pub max_instruction_chars: usize,
    pub default_max_tokens: u32,
    pub default_en_max_tokens: u32,
    pub min_tokens: u32,
    pub max_tokens_limit: u32,
    pub default_temperature: f64,
}

pub fn defaults() -> Prompt_defaults {
    Prompt_defaults {
        zh_system: templates::ZH_SYSTEM_PROMPT.to_string(),
        en_system: templates::EN_SYSTEM_PROMPT.to_string(),
        zh_setting_key: templates::KEY_ZH_SYSTEM.to_string(),
        en_setting_key: templates::KEY_EN_SYSTEM.to_string(),
        max_system_prompt_chars: templates::MAX_SYSTEM_PROMPT_CHARS,
        max_short_input_chars: MAX_SHORT_INPUT_CHARS,
        max_draft_chars: MAX_DRAFT_CHARS,
        max_instruction_chars: MAX_INSTRUCTION_CHARS,
        default_max_tokens: DEFAULT_MAX_TOKENS,
        default_en_max_tokens: DEFAULT_EN_MAX_TOKENS,
        min_tokens: MIN_TOKENS,
        max_tokens_limit: MAX_TOKENS_LIMIT,
        default_temperature: DEFAULT_TEMPERATURE,
    }
}

/// Run one stage. The caller owns presentation; this function owns validation,
/// request construction, dispatch, and output normalization.
pub async fn run_stage(
    db: &Database,
    router: Option<Arc<Router>>,
    request: Prompt_stage_request,
) -> Result<Prompt_stage_output, String> {
    let stage = request.stage.trim().to_string();
    if templates::default_system_prompt(&stage).is_none() {
        return Err("prompt_enhance_unknown_stage".to_string());
    }
    request.target.validate()?;
    let text = normalize_text(&request.text, max_input_chars(&stage))?;
    let instruction = normalize_instruction(request.instruction)?;
    let system = templates::effective_system_prompt(db, &stage)?;
    let default_max_tokens = if stage == templates::STAGE_EN_OPTIMIZE {
        DEFAULT_EN_MAX_TOKENS
    } else {
        DEFAULT_MAX_TOKENS
    };

    let database = db.clone();
    let context_selection = request.workspace_context.clone();
    let prepared_context = tokio::task::spawn_blocking(move || {
        workspaces::prepare_for_stage(&database, context_selection.as_ref())
    })
    .await
    .map_err(|_| "prompt_workspace_worker_failed".to_string())??;
    let mut user_message = compose_user_message(&text, instruction.as_deref());
    if let Some(context) = prepared_context
        .as_ref()
        .and_then(workspaces::message_from_context)
    {
        user_message.push_str(&context);
    }
    let mut chat_request = ChatCompletionRequest {
        model: request.target.model().to_string(),
        messages: vec![message("system", system), message("user", user_message)],
        stream: false,
        temperature: Some(
            request
                .temperature
                .unwrap_or(DEFAULT_TEMPERATURE)
                .clamp(0.0, 2.0),
        ),
        max_tokens: Some(
            request
                .max_tokens
                .unwrap_or(default_max_tokens)
                .clamp(MIN_TOKENS, MAX_TOKENS_LIMIT),
        ),
        tools: None,
        tool_choice: None,
        extra: Default::default(),
        source_anthropic_request: None,
        source_gemini_request: None,
        source_responses_request: None,
        request_id: None,
        client_credential: None,
    };

    // A longer source needs more room than a short expansion; an explicit
    // caller cap remains a per-call ceiling, never silently increased.
    if request.max_tokens.is_none() && stage == templates::STAGE_ZH_EXPAND {
        chat_request.max_tokens = Some(
            (text.chars().count() as u32)
                .saturating_mul(2)
                .clamp(DEFAULT_MAX_TOKENS, MAX_TOKENS_LIMIT),
        );
    }
    let started = Instant::now();
    let mut budget = recovery::Work_budget::default();
    let mut generation = recovery::generate(
        db,
        router.clone(),
        &request.target,
        chat_request.clone(),
        &mut budget,
        recovery::MAX_GENERATION_ATTEMPTS,
    )
    .await?;
    let review_status = if stage == templates::STAGE_EN_OPTIMIZE {
        review::review(
            db,
            router,
            &request.target,
            &chat_request,
            &mut generation,
            &mut budget,
        )
        .await
    } else {
        "not_requested".into()
    };
    let elapsed_ms = started.elapsed().as_millis() as u64;
    log::info!("prompt_enhance: stage={} target_ref={} input_chars={} output_chars={} elapsed_ms={} complete={} attempts={}",
        stage, crate::diagnostics::safe_reference(&request.target.label()), text.chars().count(),
        generation.text.chars().count(), elapsed_ms, generation.complete, budget.calls);
    Ok(Prompt_stage_output {
        stage,
        output: generation.text,
        complete: generation.complete,
        failure_code: generation.failure_code,
        attempts: budget.calls,
        review_status,
        model: generation.model,
        provider: generation.provider,
        target_label: request.target.label(),
        elapsed_ms,
        usage: budget.usage,
        workspace_context: prepared_context.map(|context| context.preview),
    })
}

fn message(role: &str, content: String) -> ChatMessage {
    ChatMessage {
        role: role.to_string(),
        content: Value::String(content),
        tool_calls: None,
        tool_call_id: None,
        extra: Default::default(),
    }
}

/// Per-stage input ceiling: the English stage receives a full Chinese brief,
/// which is legitimately longer than the short requirement it came from.
fn max_input_chars(stage: &str) -> usize {
    if stage == templates::STAGE_EN_OPTIMIZE {
        MAX_DRAFT_CHARS
    } else {
        MAX_SHORT_INPUT_CHARS
    }
}

fn normalize_text(text: &str, limit: usize) -> Result<String, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("prompt_enhance_empty_input".to_string());
    }
    if trimmed.chars().count() > limit {
        return Err("prompt_enhance_input_too_long".to_string());
    }
    Ok(trimmed.to_string())
}

fn normalize_instruction(instruction: Option<String>) -> Result<Option<String>, String> {
    let Some(value) = instruction else {
        return Ok(None);
    };
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.chars().count() > MAX_INSTRUCTION_CHARS {
        return Err("prompt_enhance_instruction_too_long".to_string());
    }
    Ok(Some(trimmed.to_string()))
}

/// Wrap the source text in a delimiter so the stage instruction can treat it as
/// data rather than as directives.
fn compose_user_message(text: &str, instruction: Option<&str>) -> String {
    let mut composed = format!("<requirement>\n{text}\n</requirement>");
    if let Some(instruction) = instruction {
        composed.push_str(&format!(
            "\n\n<extra_guidance>\n{instruction}\n</extra_guidance>"
        ));
    }
    composed
}

/// Strip one layer of fenced code block or a matching pair of surrounding
/// quotes. Only one layer is removed so genuine content is never rewritten.
fn strip_wrapping(raw: &str) -> String {
    let trimmed = raw.trim();
    let unfenced = strip_code_fence(trimmed).unwrap_or(trimmed);
    strip_matching_quotes(unfenced.trim()).trim().to_string()
}

/// Remove a surrounding ``` pair. A non-empty first line followed by more
/// content is treated as the fence language tag; an empty first line is left in
/// place because the caller trims the result anyway.
fn strip_code_fence(text: &str) -> Option<&str> {
    let rest = text.strip_prefix("```")?.strip_suffix("```")?;
    match rest.split_once('\n') {
        Some((first_line, remainder))
            if !first_line.trim().is_empty() && !remainder.trim().is_empty() =>
        {
            Some(remainder)
        }
        _ => Some(rest),
    }
}

/// Remove one matching pair of surrounding quotes. A single character is never
/// treated as a pair, so a lone quote is preserved.
fn strip_matching_quotes(text: &str) -> &str {
    if text.chars().count() < 2 {
        return text;
    }
    const PAIRS: [(char, char); 3] = [('"', '"'), ('\'', '\''), ('\u{201c}', '\u{201d}')];
    let (Some(first), Some(last)) = (text.chars().next(), text.chars().last()) else {
        return text;
    };
    for (open, close) in PAIRS {
        if first == open && last == close {
            let start = first.len_utf8();
            let end = text.len() - last.len_utf8();
            return &text[start..end];
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::dao::proxy_providers::{CircuitBreakerConfig, ProxyProvider};
    use crate::database::dao::routes::RouteHop;
    use crate::proxy::providers::mock::MockProvider;
    use crate::proxy::providers::Provider;
    use std::collections::HashMap;

    const ROUTE: &str = "prompt-route";
    const MODEL: &str = "mock-model";
    const PROVIDER: &str = "mock-1";

    fn mock_router() -> Arc<Router> {
        let provider = Arc::new(MockProvider::new(
            PROVIDER.to_string(),
            vec![MODEL.to_string()],
            true,
            CircuitBreakerConfig::default(),
        ));
        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        providers.insert(PROVIDER.to_string(), provider);
        let routes = HashMap::from([(
            ROUTE.to_string(),
            vec![RouteHop {
                provider: PROVIDER.to_string(),
                model: MODEL.to_string(),
            }],
        )]);
        Arc::new(Router::new(providers, routes))
    }

    pub(super) fn mock_provider_row() -> ProxyProvider {
        ProxyProvider {
            id: PROVIDER.to_string(),
            name: "Mock".into(),
            provider_type: "mock".into(),
            api_key: String::new(),
            base_url: String::new(),
            models: vec![MODEL.to_string()],
            enabled: true,
            timeout_ms: 1_000,
            weight: 1,
            circuit_breaker: CircuitBreakerConfig::default(),
            model_mapping: HashMap::new(),
            extra_headers: HashMap::new(),
            // Model auto-discovery opt-out defaults ON for real providers; the
            // mock row mirrors that default so routing tests exercise the same
            // discovery-eligible path.
            auto_track_models: true,
            icon: None,
            icon_color: None,
        }
    }

    pub(super) fn gateway_request(stage: &str, text: &str) -> Prompt_stage_request {
        Prompt_stage_request {
            stage: stage.into(),
            text: text.into(),
            target: target::Prompt_target::Gateway {
                model: ROUTE.to_string(),
            },
            instruction: None,
            temperature: None,
            max_tokens: None,
            workspace_context: None,
        }
    }

    #[tokio::test]
    async fn gateway_stage_returns_normalized_text_and_serving_provider() {
        let db = Database::memory().expect("memory db");
        let result = run_stage(
            &db,
            Some(mock_router()),
            gateway_request("zh_expand", "写一个登录页"),
        )
        .await
        .expect("stage succeeds");
        assert_eq!(result.stage, "zh_expand");
        assert_eq!(result.model, MODEL);
        assert_eq!(result.provider.as_deref(), Some(PROVIDER));
        assert_eq!(result.target_label, format!("gateway:{ROUTE}"));
        // The mock echoes the last user message, so the delimiter contract and
        // the source text are both observable end to end.
        assert!(result.output.contains("MOCK["), "{}", result.output);
        assert!(result.output.contains("写一个登录页"), "{}", result.output);
        assert!(result.output.contains("<requirement>"), "{}", result.output);
        assert!(result.usage.is_some());
    }

    #[tokio::test]
    async fn second_stage_accepts_a_full_chinese_brief() {
        let db = Database::memory().expect("memory db");
        let brief = "需".repeat(MAX_SHORT_INPUT_CHARS + 500);
        let result = run_stage(
            &db,
            Some(mock_router()),
            gateway_request("en_optimize", &brief),
        )
        .await
        .expect("the english stage accepts a full brief");
        assert_eq!(result.stage, "en_optimize");
    }

    #[tokio::test]
    async fn both_stages_receive_the_opted_in_workspace_snapshot_and_return_content_free_receipts()
    {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("AGENTS.md"),
            "Project convention: use bounded retries.",
        )
        .unwrap();
        std::fs::write(
            root.path().join("CLAUDE.md"),
            "Project convention: preserve source inputs.",
        )
        .unwrap();
        let db = Database::memory().unwrap();
        let registry = workspaces::save(
            &db,
            workspaces::Workspace_save_input {
                expected_revision: 0,
                id: None,
                name: "Team".into(),
                root: root.path().to_string_lossy().into_owned(),
            },
        )
        .unwrap();
        let item = &registry.workspaces[0];
        let mut choice = workspaces::Workspace_context_selection {
            workspace_id: item.id.clone(),
            workspace_revision: item.revision,
            read_agents: true,
            read_claude: true,
            expected_context_version: None,
        };
        let preview = workspaces::preview(&db, choice.clone()).unwrap();
        choice.expected_context_version = Some(preview.context_version.clone());
        for stage in ["zh_expand", "en_optimize"] {
            let mut request = gateway_request(stage, "Implement the requested task");
            request.workspace_context = Some(choice.clone());
            let output = run_stage(&db, Some(mock_router()), request).await.unwrap();
            assert!(output.output.contains("bounded retries"));
            assert!(output.output.contains("preserve source inputs"));
            let receipt = output.workspace_context.unwrap();
            assert_eq!(receipt.context_version, preview.context_version);
            assert!(!serde_json::to_string(&receipt)
                .unwrap()
                .contains("bounded retries"));
        }
        let output = run_stage(
            &db,
            Some(mock_router()),
            gateway_request("zh_expand", "No workspace requested"),
        )
        .await
        .unwrap();
        assert!(output.workspace_context.is_none());
        assert!(!output.output.contains("bounded retries"));
    }

    #[tokio::test]
    async fn gateway_target_requires_a_running_gateway() {
        let db = Database::memory().expect("memory db");
        let error = run_stage(&db, None, gateway_request("zh_expand", "写一个登录页"))
            .await
            .expect_err("no router");
        assert_eq!(error, "prompt_enhance_gateway_not_running");
    }

    #[tokio::test]
    async fn provider_target_runs_while_the_gateway_is_stopped() {
        let db = Database::memory().expect("memory db");
        db.save_proxy_provider(&mock_provider_row()).expect("saved");
        let mut stage_request = gateway_request("zh_expand", "写一个登录页");
        stage_request.target = target::Prompt_target::Provider {
            provider_id: PROVIDER.to_string(),
            model: MODEL.to_string(),
        };
        let result = run_stage(&db, None, stage_request)
            .await
            .expect("direct provider dispatch works without a gateway");
        assert_eq!(result.provider.as_deref(), Some(PROVIDER));
        assert!(result.output.contains("写一个登录页"));
    }

    #[tokio::test]
    async fn missing_and_disabled_providers_are_rejected() {
        let db = Database::memory().expect("memory db");
        let mut stage_request = gateway_request("zh_expand", "hello");
        stage_request.target = target::Prompt_target::Provider {
            provider_id: "missing".into(),
            model: MODEL.to_string(),
        };
        assert_eq!(
            run_stage(&db, None, stage_request).await.unwrap_err(),
            "prompt_enhance_provider_not_found"
        );

        let mut disabled = mock_provider_row();
        disabled.enabled = false;
        db.save_proxy_provider(&disabled).expect("saved");
        let mut stage_request = gateway_request("zh_expand", "hello");
        stage_request.target = target::Prompt_target::Provider {
            provider_id: PROVIDER.to_string(),
            model: MODEL.to_string(),
        };
        assert_eq!(
            run_stage(&db, None, stage_request).await.unwrap_err(),
            "prompt_enhance_provider_disabled"
        );
    }

    #[tokio::test]
    async fn input_validation_happens_before_dispatch() {
        let db = Database::memory().expect("memory db");
        assert_eq!(
            run_stage(&db, Some(mock_router()), gateway_request("nope", "hello"))
                .await
                .unwrap_err(),
            "prompt_enhance_unknown_stage"
        );
        assert_eq!(
            run_stage(
                &db,
                Some(mock_router()),
                gateway_request("zh_expand", "   ")
            )
            .await
            .unwrap_err(),
            "prompt_enhance_empty_input"
        );
        let too_long = "字".repeat(MAX_SHORT_INPUT_CHARS + 1);
        assert_eq!(
            run_stage(
                &db,
                Some(mock_router()),
                gateway_request("zh_expand", &too_long)
            )
            .await
            .unwrap_err(),
            "prompt_enhance_input_too_long"
        );
        let mut with_instruction = gateway_request("zh_expand", "hello");
        with_instruction.instruction = Some("x".repeat(MAX_INSTRUCTION_CHARS + 1));
        assert_eq!(
            run_stage(&db, Some(mock_router()), with_instruction)
                .await
                .unwrap_err(),
            "prompt_enhance_instruction_too_long"
        );
    }

    #[test]
    fn user_message_wraps_the_source_and_optional_guidance() {
        assert_eq!(
            compose_user_message("hi", None),
            "<requirement>\nhi\n</requirement>"
        );
        assert_eq!(
            compose_user_message("hi", Some("use React")),
            "<requirement>\nhi\n</requirement>\n\n<extra_guidance>\nuse React\n</extra_guidance>"
        );
        assert_eq!(normalize_instruction(Some("  ".into())).unwrap(), None);
    }

    #[test]
    fn wrapping_is_removed_one_layer_at_a_time() {
        assert_eq!(strip_wrapping("  plain  "), "plain");
        assert_eq!(strip_wrapping("\"quoted\""), "quoted");
        assert_eq!(strip_wrapping("\u{201c}quoted\u{201d}"), "quoted");
        assert_eq!(strip_wrapping("```\nbody\n```"), "body");
        assert_eq!(strip_wrapping("```json\n{\"a\":1}\n```"), "{\"a\":1}");
        assert_eq!(strip_wrapping("```text```"), "text");
        // A single character must never be sliced as a quote pair.
        assert_eq!(strip_wrapping("\""), "\"");
        // Inner quotes stay untouched.
        assert_eq!(strip_wrapping("say \"hi\" now"), "say \"hi\" now");
    }

    #[tokio::test]
    async fn english_minimax_controls_leave_room_for_visible_output_on_the_anthropic_wire() {
        use axum::{extract::Json, routing::post, Router as HttpRouter};
        use serde_json::json;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = Arc::clone(&observed);
        let upstream = tokio::spawn(async move {
            axum::serve(listener, HttpRouter::new().route("/v1/messages", post(move |Json(body): Json<Value>| {
                let captured = Arc::clone(&captured);
                async move {
                    captured.lock().unwrap().push(body.clone());
                    let english = body["system"].as_str().unwrap().contains("English prompt optimization");
                    let minimax = body["model"] == "MiniMax-M3.1-Flash";
                    // This provider spends an unbounded-effort allowance on
                    // thinking before returning any visible answer.
                    let exhausted = english && minimax &&
                        (body["output_config"]["effort"] != "low" || body["max_tokens"] == 800);
                    let mut content = vec![json!({"type":"thinking","thinking":"synthetic non-final deliberation","signature":"fixture"})];
                    if !exhausted {
                        content.push(json!({"type":"text","text":"Implement the requested CSV processing tool."}));
                    }
                    Json(json!({"id":"fixture","type":"message","role":"assistant","model":body["model"],
                        "content":content,"stop_reason":if exhausted {"max_tokens"} else {"end_turn"},
                        "usage":{"input_tokens":20,"output_tokens":800}}))
                }
            }))).await.unwrap();
        });
        let db = Database::memory().unwrap();
        let mut provider = mock_provider_row();
        provider.provider_type = "anthropic".into();
        provider.base_url = base_url;
        provider.models = vec!["MiniMax-M3.1-Flash".into(), "other-model".into()];
        db.save_proxy_provider(&provider).unwrap();
        for (stage, model, budget, succeeds) in [
            ("en_optimize", "MiniMax-M3.1-Flash", None, true),
            ("en_optimize", "MiniMax-M3.1-Flash", Some(800), false),
            ("zh_expand", "MiniMax-M3.1-Flash", None, true),
            ("en_optimize", "other-model", None, true),
        ] {
            let mut request = gateway_request(stage, "请整理CSV文件并保留原文件。");
            request.target = target::Prompt_target::Provider {
                provider_id: PROVIDER.into(),
                model: model.into(),
            };
            request.max_tokens = budget;
            let result = run_stage(&db, None, request).await;
            if succeeds {
                assert_eq!(
                    result.unwrap().output,
                    "Implement the requested CSV processing tool."
                );
            } else {
                assert_eq!(result.unwrap_err(), "prompt_enhance_continuation_stalled");
            }
        }
        let captured = observed.lock().unwrap();
        assert_eq!(
            captured.len(),
            5,
            "an empty exhausted answer receives one bounded continuation before stalling"
        );
        assert_eq!(captured[0]["max_tokens"], 8192);
        assert_eq!(captured[0]["output_config"]["effort"], "low");
        assert_eq!(captured[1]["max_tokens"], 800);
        assert_eq!(captured[2]["max_tokens"], 800);
        assert_eq!(captured[3]["max_tokens"], 1200);
        assert_eq!(captured[3]["output_config"]["effort"], "low");
        assert!(captured[4].get("output_config").is_none());
        assert!(captured.iter().all(|body| body.get("thinking").is_none()));
        drop(captured);
        upstream.abort();
        let _ = upstream.await;
    }

    #[tokio::test]
    async fn english_stage_has_reasoning_headroom_and_preserves_explicit_budgets() {
        use axum::{extract::Json, routing::post, Router as HttpRouter};
        use serde_json::json;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = Arc::clone(&observed);
        let upstream = tokio::spawn(async move {
            axum::serve(listener,HttpRouter::new().route("/v1/chat/completions",post(move |Json(body):Json<Value>| {
                let captured=Arc::clone(&captured);
                async move {
                    captured.lock().unwrap().push(body.clone());
                    let limit=body["max_tokens"].as_u64().unwrap();
                    let english=body["messages"][0]["content"].as_str().unwrap().contains("English prompt optimization");
                    let exhausted=english && limit<4096;
                    Json(json!({"id":"fixture","object":"chat.completion","created":0,"model":"fixture-model","choices":[{"index":0,"finish_reason":if exhausted {"length"} else {"stop"},"message":{"role":"assistant","content":if exhausted {Value::Null} else {json!("Write the requested CSV processing tool.")},"reasoning_content":"non-final fixture reasoning"}}]}))
                }
            }))).await.unwrap();
        });
        let db = Database::memory().unwrap();
        let mut provider = mock_provider_row();
        provider.provider_type = "openai".into();
        provider.base_url = base_url;
        db.save_proxy_provider(&provider).unwrap();
        for (stage, budget, succeeds) in [
            ("zh_expand", None, true),
            ("en_optimize", None, true),
            ("en_optimize", Some(800), false),
        ] {
            let mut request = gateway_request(stage, "请整理CSV文件并保留原文件。");
            request.target = target::Prompt_target::Provider {
                provider_id: PROVIDER.into(),
                model: MODEL.into(),
            };
            request.max_tokens = budget;
            let result = run_stage(&db, None, request).await;
            if succeeds {
                assert_eq!(
                    result.unwrap().output,
                    "Write the requested CSV processing tool."
                );
            } else {
                assert_eq!(result.unwrap_err(), "prompt_enhance_continuation_stalled");
            }
        }
        let captured = observed.lock().unwrap();
        assert_eq!(
            captured.len(),
            4,
            "explicit per-call caps survive bounded continuation"
        );
        assert_eq!(captured[0]["max_tokens"], 1200);
        assert_eq!(captured[1]["max_tokens"], 8192);
        assert_eq!(captured[2]["max_tokens"], 800);
        drop(captured);
        upstream.abort();
        let _ = upstream.await;
    }

    #[test]
    fn defaults_expose_the_shipped_templates_and_caps() {
        let defaults = defaults();
        assert_eq!(defaults.zh_system, templates::ZH_SYSTEM_PROMPT);
        assert_eq!(defaults.en_system, templates::EN_SYSTEM_PROMPT);
        assert_eq!(defaults.zh_setting_key, templates::KEY_ZH_SYSTEM);
        assert_eq!(defaults.en_setting_key, templates::KEY_EN_SYSTEM);
        assert!(defaults.min_tokens < defaults.default_max_tokens);
        assert!(defaults.default_max_tokens < defaults.max_tokens_limit);
        assert!(defaults.max_short_input_chars < defaults.max_draft_chars);
    }
}
