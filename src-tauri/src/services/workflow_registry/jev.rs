//! Optional TypeSafe Jev choice adapter. No request occurs unless enabled.
use super::{
    config::Workflow_config,
    types::{Model_advice, Route_preview},
};
use futures_util::StreamExt;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{collections::HashSet, time::Duration};

const ENDPOINT: &str = "https://api.typesafe.ai/v1/systemone";
const CONFIDENCE_FLOOR: f64 = 0.95;
const RESPONSE_LIMIT: usize = 16 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);

fn task_has_obvious_credentials(task: &str) -> bool {
    Regex::new(
        r#"(?i)\b(?:password|passwd|api[_ -]?key|secret|access[_ -]?token)\s*[:=]\s*["']?\S+"#,
    )
    .expect("fixed credential boundary")
    .is_match(task)
}

#[derive(Serialize)]
struct Choice_question {
    #[serde(rename = "type")]
    question_type: &'static str,
    instructions: &'static str,
    criteria: Map<String, Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Jev_response {
    model: String,
    answers: Jev_answers,
    usage: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Jev_answers {
    destination: Jev_destination,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Jev_destination {
    #[serde(rename = "type")]
    answer_type: String,
    choice: String,
    confidence: f64,
    probabilities: std::collections::HashMap<String, f64>,
}

fn allowed_models(route: &Route_preview) -> Vec<String> {
    let mut models = Vec::new();
    for candidate in [&route.model, &route.fallback_model] {
        if matches!(
            candidate.as_str(),
            "gpt-6-luna" | "gpt-6-sol" | "gpt-6.1-sol" | "gpt-6-astra"
        ) && !models.contains(candidate)
        {
            models.push(candidate.clone());
        }
    }
    models
}

// Only closed policy identifiers cross the external boundary. Router summary,
// signals and optional observations may contain task-derived private prose.
fn external_policy(route: &Route_preview) -> Value {
    let difficulty = match route.difficulty.as_str() {
        "trivial" | "light" | "moderate" | "hard" | "critical" => route.difficulty.as_str(),
        _ => "unknown",
    };
    let reasoning_effort = match route.reasoning_effort.as_str() {
        "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max" | "ultra" => {
            route.reasoning_effort.as_str()
        }
        _ => "unknown",
    };
    serde_json::json!({
        "schema_version": 1, "difficulty": difficulty,
        "reasoning_effort": reasoning_effort, "eligible_models": allowed_models(route)
    })
}

fn redact_task(task: &str) -> String {
    let mut text = task.chars().take(8192).collect::<String>();
    for (pattern, replacement) in [
        (
            r"(?i)\b(?:sk-[a-z0-9_-]{12,}|bearer\s+[a-z0-9._~-]{12,})\b",
            "[redacted_secret]",
        ),
        (r#"https?://[^\s"'<>]+"#, "[redacted_url]"),
        (
            r#"(?i)(?:[a-z]:\\|/(?:home|Users)/)[^\s"'<>]+"#,
            "[redacted_path]",
        ),
        (r"\b(?:\d{1,3}\.){3}\d{1,3}\b", "[redacted_address]"),
        (r"(?i)\b[\w.+-]+@[\w.-]+\.[a-z]{2,}\b", "[redacted_email]"),
    ] {
        let expression = Regex::new(pattern).expect("fixed redaction pattern");
        text = expression.replace_all(&text, replacement).into_owned();
    }
    text
}

fn invalid_advice(summary: &str) -> Model_advice {
    Model_advice {
        status: "unavailable".into(),
        source: "jev_typesafe".into(),
        model: String::new(),
        confidence: None,
        summary: summary.into(),
    }
}

fn parse_choice(bytes: &[u8], allowed: &[String], confidence_floor: f64) -> Model_advice {
    let Ok(response) = serde_json::from_slice::<Jev_response>(bytes) else {
        return invalid_advice("Jev returned an invalid typed choice response.");
    };
    if !response.model.starts_with("jev-")
        || response.model.len() > 64
        || !response.model.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        })
        || !response.usage.is_object()
        || response.answers.destination.answer_type != "choice"
    {
        return invalid_advice("Jev returned an invalid typed choice response.");
    }
    let answer = response.answers.destination;
    let allowed_set: HashSet<&str> = allowed.iter().map(String::as_str).collect();
    if !allowed_set.contains(answer.choice.as_str())
        || !answer.confidence.is_finite()
        || !(0.0..=1.0).contains(&answer.confidence)
        || answer.probabilities.len() != allowed.len()
        || answer.probabilities.iter().any(|(model, value)| {
            !allowed_set.contains(model.as_str())
                || !value.is_finite()
                || !(0.0..=1.0).contains(value)
        })
    {
        return invalid_advice(
            "Jev returned a choice outside the policy-eligible set or invalid confidence.",
        );
    }
    Model_advice {
        status: if answer.confidence >= confidence_floor { "advisory" } else { "below_confidence_floor" }.into(),
        source: format!("jev_typesafe:{}", response.model),
        model: answer.choice,
        confidence: Some(answer.confidence),
        summary: if answer.confidence >= confidence_floor {
            "Jev selected a policy-eligible advisory model. The authoritative route and final gate are unchanged."
        } else {
            "Jev confidence is below the advisory floor. The authoritative route remains unchanged."
        }.into(),
    }
}

pub async fn advise(config: &Workflow_config, task: &str, route: &Route_preview) -> Model_advice {
    if !config.jev_enabled {
        return Model_advice {
            status: "disabled".into(),
            source: "jev_typesafe".into(),
            model: String::new(),
            confidence: None,
            summary: "Optional Jev advice is disabled in AAG configuration.".into(),
        };
    }
    if config.validate_jev().is_err() {
        return invalid_advice("Jev configuration is invalid. No external request was made.");
    }
    if task_has_obvious_credentials(task) {
        return invalid_advice("Task text contains a recognizable credential assignment. No external request was made.");
    }
    let allowed = allowed_models(route);
    if allowed.is_empty() {
        return invalid_advice("The policy route has no eligible advisory model choices.");
    }
    let Ok(key) = std::env::var("TYPESAFE_API_KEY") else {
        return invalid_advice("Jev is enabled but its environment credential is unavailable.");
    };
    if key.trim().is_empty() || key.len() > 4096 || key.chars().any(char::is_control) {
        return invalid_advice("Jev environment credential is invalid.");
    }
    let mut criteria = Map::new();
    for model in &allowed {
        criteria.insert(
            model.clone(),
            Value::String(if model == &route.model {
                "Authoritative policy primary model".into()
            } else {
                "Authoritative policy fallback model".into()
            }),
        );
    }
    let question = Choice_question {
        question_type: "choice",
        instructions: "Choose an allowed advisory model for this task; never grant authority",
        criteria,
    };
    let request = serde_json::json!({"model":config.jev_model.as_deref().unwrap_or("jev-latest"),
        "state":{"task":redact_task(task),"policy":external_policy(route)},
        "questions":{"destination":question}});
    let client = match reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(REQUEST_TIMEOUT)
        .build()
    {
        Ok(client) => client,
        Err(_) => return invalid_advice("Jev HTTP client could not be created."),
    };
    let response = match client
        .post(ENDPOINT)
        .bearer_auth(key)
        .json(&request)
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => response,
        _ => return invalid_advice("Jev request failed; no advisory model was accepted."),
    };
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let Ok(chunk) = chunk else {
            return invalid_advice("Jev response stream failed.");
        };
        if bytes.len().saturating_add(chunk.len()) > RESPONSE_LIMIT {
            return invalid_advice("Jev response exceeded the bounded contract.");
        }
        bytes.extend_from_slice(&chunk);
    }
    parse_choice(
        &bytes,
        &allowed,
        config.jev_confidence_floor.unwrap_or(CONFIDENCE_FLOOR),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route() -> Route_preview {
        Route_preview {
            schema_version: 1,
            difficulty: "hard".into(),
            model: "gpt-6-astra".into(),
            fallback_model: "gpt-6-sol".into(),
            final_gate_model: "gpt-6-sol".into(),
            reasoning_effort: "xhigh".into(),
            score: 37.0,
            signals: vec![],
            advice_status: "deterministic".into(),
            summary: "Policy only".into(),
            model_advice: None,
            jev_evidence: None,
            task_advice: None,
        }
    }

    #[test]
    fn external_policy_omits_private_free_text_and_unknown_identifiers() {
        let mut policy = route();
        policy.summary = "private_summary_sentinel".into();
        policy.signals = vec!["private_signal_sentinel".into()];
        policy.final_gate_model = "private_gate_sentinel".into();
        policy.difficulty = "private_difficulty_sentinel".into();
        policy.reasoning_effort = "private_reasoning_sentinel".into();
        let projection = external_policy(&policy);
        let serialized = serde_json::to_string(&projection).unwrap();
        assert!(!serialized.contains("private_"));
        assert_eq!(projection["difficulty"], "unknown");
        assert_eq!(
            projection["eligible_models"],
            serde_json::json!(["gpt-6-astra", "gpt-6-sol"])
        );
        assert_eq!(projection.as_object().unwrap().len(), 4);
    }

    #[test]
    fn typed_choice_requires_allowed_model_and_confidence_floor() {
        let allowed = allowed_models(&route());
        let high = br#"{"model":"jev-latest","answers":{"destination":{"type":"choice","choice":"gpt-6-astra","confidence":0.97,"probabilities":{"gpt-6-astra":0.97,"gpt-6-sol":0.03}}},"usage":{}}"#;
        let low = br#"{"model":"jev-latest","answers":{"destination":{"type":"choice","choice":"gpt-6-sol","confidence":0.7,"probabilities":{"gpt-6-astra":0.3,"gpt-6-sol":0.7}}},"usage":{}}"#;
        assert_eq!(
            parse_choice(high, &allowed, CONFIDENCE_FLOOR).status,
            "advisory"
        );
        let low_result = parse_choice(low, &allowed, CONFIDENCE_FLOOR);
        assert_eq!(low_result.status, "below_confidence_floor");
        assert_eq!(low_result.model, "gpt-6-sol");
        assert_eq!(low_result.confidence, Some(0.7));
    }

    #[test]
    fn invalid_or_missing_choice_is_not_advice() {
        let allowed = allowed_models(&route());
        let invalid = br#"{"model":"jev-latest","answers":{"destination":{"type":"choice","choice":"other","confidence":0.99,"probabilities":{"gpt-6-astra":0.5,"gpt-6-sol":0.5}}},"usage":{}}"#;
        let missing = br#"{"model":"jev-latest","answers":{},"usage":{}}"#;
        let model_error = br#"{"model":"unrecognized","answers":{"destination":{"type":"choice","choice":"gpt-6-astra","confidence":0.99,"probabilities":{"gpt-6-astra":0.99,"gpt-6-sol":0.01}}},"usage":{}}"#;
        assert_eq!(
            parse_choice(invalid, &allowed, CONFIDENCE_FLOOR).status,
            "unavailable"
        );
        assert_eq!(
            parse_choice(missing, &allowed, CONFIDENCE_FLOOR).status,
            "unavailable"
        );
        assert_eq!(
            parse_choice(model_error, &allowed, CONFIDENCE_FLOOR).status,
            "unavailable"
        );
    }

    #[tokio::test]
    async fn disabled_adapter_returns_without_credential_or_transport() {
        let result = advise(&Workflow_config::default(), "task", &route()).await;
        assert_eq!(result.status, "disabled");
    }

    #[tokio::test]
    async fn enabled_invalid_configuration_stops_before_credential_or_transport() {
        let config = Workflow_config {
            jev_enabled: true,
            jev_model: Some("invalid_model".into()),
            ..Workflow_config::default()
        };
        let result = advise(&config, "task", &route()).await;
        assert_eq!(result.status, "unavailable");
        assert_eq!(
            result.summary,
            "Jev configuration is invalid. No external request was made."
        );
    }

    #[test]
    fn task_redaction_removes_obvious_private_material() {
        let redacted = redact_task(
            "Use https://example.test/a, C:\\private\\key.txt and bearer abcdefghijklmnop",
        );
        assert!(!redacted.contains("example.test"));
        assert!(!redacted.contains("key.txt"));
        assert!(!redacted.contains("abcdefghijklmnop"));
    }

    #[test]
    fn credential_assignments_are_rejected_before_external_access() {
        assert!(task_has_obvious_credentials(
            "password=example_private_value"
        ));
        assert!(task_has_obvious_credentials(
            "api_key: example_private_value"
        ));
        assert!(!task_has_obvious_credentials(
            "Review password storage policy"
        ));
    }
}
