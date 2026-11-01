//! Shared local adviser process adapter. Policy and execution remain separate.
#![allow(non_upper_case_globals, non_camel_case_types)]

use super::{
    config::Workflow_config,
    process,
    types::{Route_preview, Task_advice},
};
use serde::Deserialize;
use serde_json::Value;
use std::{collections::HashMap, time::Duration};

const expected_checkpoint: &str = "StrandsAgents/strands-decider-2B-hobson-v19";
const process_timeout: Duration = Duration::from_secs(12);
const task_byte_limit: usize = 8192;
const response_byte_limit: usize = 16 * 1024;
const choice_labels: [&str; 3] = ["artifact_change", "artifact_assessment", "other_or_unclear"];
const statuses: [&str; 6] = [
    "observation_only",
    "below_confidence_floor",
    "disabled",
    "busy",
    "unavailable",
    "recovery_required",
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Advice_response {
    schema_version: u8,
    request_id: String,
    policy_snapshot: Value,
    task_advice: Task_advice,
}

pub fn unavailable(reason_code: &str) -> Task_advice {
    Task_advice {
        status: "unavailable".into(),
        source: "strands_decider_local".into(),
        checkpoint: expected_checkpoint.into(),
        workflow_class: None,
        confidence: None,
        probabilities: HashMap::new(),
        latency_ms: None,
        observed_at: chrono::Utc::now().to_rfc3339(),
        reason_code: reason_code.into(),
        summary: "Local task advice is unavailable. The deterministic route and authorization remain unchanged.".into(),
    }
}

fn policy_snapshot(route: &Route_preview) -> Value {
    serde_json::json!({
        "schema_version": route.schema_version,
        "difficulty": route.difficulty,
        "model": route.model,
        "fallback_model": route.fallback_model,
        "final_gate_model": route.final_gate_model,
        "reasoning_effort": route.reasoning_effort,
        "score": route.score,
        "signals": route.signals,
        "advice_status": route.advice_status,
        "summary": route.summary,
    })
}

fn parse_response(bytes: &[u8], request_id: &str, policy: &Value) -> Task_advice {
    let response: Advice_response = match serde_json::from_slice(bytes) {
        Ok(response) => response,
        Err(_) => return unavailable("response_invalid"),
    };
    if response.schema_version != 1
        || response.request_id != request_id
        || &response.policy_snapshot != policy
    {
        return unavailable("response_binding_invalid");
    }
    let advice = response.task_advice;
    if advice.source != "strands_decider_local"
        || advice.checkpoint != expected_checkpoint
        || !statuses.contains(&advice.status.as_str())
        || advice.summary.len() > 512
        || advice.summary.chars().any(char::is_control)
        || advice.reason_code.is_empty()
        || advice.reason_code.len() > 64
        || !advice
            .reason_code
            .chars()
            .all(|c| c.is_ascii_lowercase() || c == '_')
        || chrono::DateTime::parse_from_rfc3339(&advice.observed_at).is_err()
        || advice
            .latency_ms
            .is_some_and(|value| !value.is_finite() || value < 0.0)
        || advice
            .confidence
            .is_some_and(|value| !value.is_finite() || !(0.0..=1.0).contains(&value))
    {
        return unavailable("response_invalid");
    }
    let complete = matches!(
        advice.status.as_str(),
        "observation_only" | "below_confidence_floor"
    );
    if complete {
        let Some(label) = &advice.workflow_class else {
            return unavailable("response_invalid");
        };
        let Some(confidence) = advice.confidence else {
            return unavailable("response_invalid");
        };
        if advice.latency_ms.is_none()
            || !choice_labels.contains(&label.as_str())
            || advice.probabilities.len() != choice_labels.len()
            || advice.probabilities.iter().any(|(key, value)| {
                !choice_labels.contains(&key.as_str())
                    || !value.is_finite()
                    || !(0.0..=1.0).contains(value)
            })
        {
            return unavailable("response_invalid");
        }
        let sum: f64 = advice.probabilities.values().sum();
        let maximum = advice
            .probabilities
            .values()
            .copied()
            .fold(0.0_f64, f64::max);
        let selected = advice.probabilities[label];
        if (sum - 1.0).abs() > 0.001
            || selected + 0.00001 < maximum
            || (confidence - ((3.0 * maximum - 1.0) / 2.0).clamp(0.0, 1.0)).abs() > 0.001
        {
            return unavailable("response_invalid");
        }
    } else if advice.workflow_class.is_some()
        || advice.confidence.is_some()
        || !advice.probabilities.is_empty()
    {
        return unavailable("response_invalid");
    }
    advice
}

pub fn advise(config: &Workflow_config, task: &str, route: &Route_preview) -> Task_advice {
    if !config.strands_advice_enabled {
        return unavailable("disabled");
    }
    if task.trim().is_empty()
        || task.len() > task_byte_limit
        || task.contains('\0')
        || route.advice_status != "deterministic"
    {
        return unavailable("input_invalid");
    }
    if config.validate_advice().is_err() {
        return unavailable("configuration_unavailable");
    }
    let (Some(python_path), Some(advice_path), Some(advice_config_path)) = (
        &config.python_path,
        &config.strands_advice_path,
        &config.strands_advice_config_path,
    ) else {
        return unavailable("configuration_unavailable");
    };
    let (Some(advice_arg), Some(config_arg)) = (advice_path.to_str(), advice_config_path.to_str())
    else {
        return unavailable("configuration_unavailable");
    };
    let mut random_bytes = [0_u8; 16];
    if getrandom::getrandom(&mut random_bytes).is_err() {
        return unavailable("request_identity_unavailable");
    }
    let request_id = format!("aag_{}", hex::encode(random_bytes));
    let policy = policy_snapshot(route);
    let request = serde_json::json!({"schema_version":1,"request_id":request_id,"task":task,"policy_snapshot":policy});
    let input = match serde_json::to_vec(&request) {
        Ok(input) => input,
        Err(_) => return unavailable("input_invalid"),
    };
    let argv = process::python_arguments(advice_arg, &["--config", config_arg]);
    match process::capture(
        python_path,
        &argv,
        &input,
        config.project_dir.as_deref(),
        process_timeout,
        response_byte_limit,
    ) {
        Ok(bytes) => parse_response(&bytes, &request_id, &policy),
        Err(_) => unavailable("process_unavailable"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> Value {
        serde_json::json!({"schema_version":1,"difficulty":"hard","model":"gpt-6-astra","fallback_model":"gpt-6-sol","final_gate_model":"gpt-6-sol","reasoning_effort":"xhigh","score":75,"signals":["design_or_architecture_principles"],"advice_status":"deterministic","summary":"Policy only"})
    }

    fn fixture() -> Value {
        serde_json::json!({"schema_version":1,"request_id":"r1","policy_snapshot":policy(),"task_advice":{"status":"observation_only","source":"strands_decider_local","checkpoint":expected_checkpoint,"workflow_class":"artifact_change","confidence":0.97,"probabilities":{"artifact_change":0.98,"artifact_assessment":0.01,"other_or_unclear":0.01},"latency_ms":500,"observed_at":"2026-10-04T00:00:00Z","reason_code":"classification_observed","summary":"Observation only; policy unchanged."}})
    }

    #[test]
    fn typed_observation_preserves_policy_binding() {
        let value = fixture();
        let response = parse_response(&serde_json::to_vec(&value).unwrap(), "r1", &policy());
        assert_eq!(response.status, "observation_only");
        assert_eq!(response.workflow_class.as_deref(), Some("artifact_change"));
        let mut changed = policy();
        changed["model"] = "gpt-6-luna".into();
        assert_eq!(
            parse_response(&serde_json::to_vec(&value).unwrap(), "r1", &changed).reason_code,
            "response_binding_invalid"
        );
        assert_eq!(
            parse_response(&serde_json::to_vec(&value).unwrap(), "r2", &policy()).reason_code,
            "response_binding_invalid"
        );
    }

    #[test]
    fn rejects_distribution_identity_and_confidence_corruption() {
        for (field, invalid) in [
            ("checkpoint", Value::String("wrong_model".into())),
            ("confidence", serde_json::json!(0.5)),
            (
                "workflow_class",
                Value::String("artifact_assessment".into()),
            ),
        ] {
            let mut value = fixture();
            value["task_advice"][field] = invalid;
            assert_eq!(
                parse_response(&serde_json::to_vec(&value).unwrap(), "r1", &policy()).status,
                "unavailable"
            );
        }
        let mut value = fixture();
        value["task_advice"]["probabilities"]["artifact_change"] = 0.2.into();
        assert_eq!(
            parse_response(&serde_json::to_vec(&value).unwrap(), "r1", &policy()).status,
            "unavailable"
        );
    }

    #[test]
    fn unavailable_result_never_contains_a_classification() {
        let result = unavailable("process_unavailable");
        assert!(
            result.workflow_class.is_none()
                && result.confidence.is_none()
                && result.probabilities.is_empty()
        );
    }
}
