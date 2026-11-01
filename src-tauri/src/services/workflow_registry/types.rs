//! Public workflow DTOs. A stage is a claim with its own observation source.
#![allow(non_camel_case_types)]
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Observation {
    pub stage: String,
    pub status: String,
    pub observed_at: Option<String>,
    pub expires_at: Option<String>,
    pub evidence_source: String,
    pub summary: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Capability {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub group: String,
    pub owner: String,
    pub access_method: String,
    pub host_scope: String,
    pub source_ref: String,
    pub mcp_server_id: Option<String>,
    pub summary: String,
    #[serde(default)]
    pub probe_supported: bool,
    #[serde(default)]
    pub observations: Vec<Observation>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Capability_list {
    pub schema_version: u8,
    pub capabilities: Vec<Capability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Health_observation {
    pub capability_id: String,
    #[serde(flatten)]
    pub observation: Observation,
}

#[derive(Clone, Debug, Serialize)]
pub struct Health_snapshot {
    pub schema_version: u8,
    pub owner_status: String,
    pub observations: Vec<Health_observation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Model_advice {
    pub status: String,
    pub source: String,
    pub model: String,
    pub confidence: Option<f64>,
    pub summary: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Task_advice {
    pub status: String,
    pub source: String,
    pub checkpoint: String,
    pub workflow_class: Option<String>,
    pub confidence: Option<f64>,
    pub probabilities: HashMap<String, f64>,
    pub latency_ms: Option<f64>,
    pub observed_at: String,
    pub reason_code: String,
    pub summary: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Route_preview {
    pub schema_version: u8,
    pub difficulty: String,
    pub model: String,
    pub fallback_model: String,
    pub final_gate_model: String,
    pub reasoning_effort: String,
    pub score: f64,
    pub signals: Vec<String>,
    pub advice_status: String,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_advice: Option<Model_advice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jev_evidence: Option<Model_advice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_advice: Option<Task_advice>,
}

impl Route_preview {
    pub fn unavailable(summary: &str) -> Self {
        Self {
            schema_version: 1,
            difficulty: "unknown".into(),
            model: "".into(),
            fallback_model: "".into(),
            final_gate_model: "".into(),
            reasoning_effort: "unknown".into(),
            score: 0.0,
            signals: Vec::new(),
            advice_status: "unavailable".into(),
            summary: summary.into(),
            model_advice: None,
            jev_evidence: None,
            task_advice: None,
        }
    }
}
