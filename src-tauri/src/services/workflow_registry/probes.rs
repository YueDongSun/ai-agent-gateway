//! Explicit fixed --version reads. Success proves installation only.
use super::{config::Workflow_config, process, types::Observation};
use chrono::{Duration as Chrono_duration, Utc};
use std::time::Duration;

const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

pub fn probe(config: &Workflow_config, capability_id: &str) -> Observation {
    let supported = matches!(
        capability_id,
        "pi_cli" | "cline_cli" | "kimi_cli" | "minimax_cli"
    );
    if !supported {
        return Observation {
            stage: "installed".into(),
            status: "requires_host".into(),
            observed_at: None,
            expires_at: None,
            evidence_source: "host_owner".into(),
            summary: "AAG has no authorized local probe for this capability.".into(),
        };
    }
    let Some(path) = config.probe_paths.get(capability_id) else {
        return Observation {
            stage: "installed".into(),
            status: "unknown".into(),
            observed_at: None,
            expires_at: None,
            evidence_source: "workflow_config".into(),
            summary: "No private executable path is configured.".into(),
        };
    };
    let result = process::capture(path, &["--version"], &[], None, PROBE_TIMEOUT, 2048);
    let observed_at = Utc::now();
    Observation {
        stage: "installed".into(),
        status: if result.is_ok() {
            "installed"
        } else {
            "unavailable"
        }
        .into(),
        observed_at: Some(observed_at.to_rfc3339()),
        expires_at: Some((observed_at + Chrono_duration::minutes(5)).to_rfc3339()),
        evidence_source: "fixed_version_probe".into(),
        summary: if result.is_ok() {
            "Fixed --version command completed; callable workflow state remains unknown."
        } else {
            "Fixed --version command failed or timed out."
        }
        .into(),
    }
}
