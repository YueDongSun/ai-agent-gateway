//! Invoke the authoritative router as fixed argv; Rust never reimplements policy.
use super::{config::Workflow_config, process, types::Route_preview};
use std::time::Duration;

const ROUTER_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_TASK_BYTES: usize = 8 * 1024;
const MAX_OUTPUT_BYTES: usize = 16 * 1024;

pub fn preview(config: &Workflow_config, task: &str) -> Route_preview {
    if task.trim().is_empty() || task.len() > MAX_TASK_BYTES || task.chars().any(|c| c == '\0') {
        return Route_preview::unavailable(
            "Task input is empty or exceeds the bounded policy interface.",
        );
    }
    let (Some(python_path), Some(router_path), Some(project_dir)) = (
        &config.python_path,
        &config.router_path,
        &config.project_dir,
    ) else {
        return Route_preview::unavailable(
            "Authoritative router paths and project directory are not configured.",
        );
    };
    if config.validate_router().is_err() {
        return Route_preview::unavailable("Authoritative router configuration is invalid.");
    }
    let Some(router_arg) = router_path.to_str() else {
        return Route_preview::unavailable(
            "Configured router path cannot be passed to the process.",
        );
    };
    let argv = process::python_arguments(router_arg, &["--policy-only", "--sandbox", "read-only"]);
    let output = match process::capture(
        python_path,
        &argv,
        task.as_bytes(),
        Some(project_dir),
        ROUTER_TIMEOUT,
        MAX_OUTPUT_BYTES,
    ) {
        Ok(bytes) => bytes,
        Err(_) => {
            return Route_preview::unavailable("Authoritative router invocation is unavailable.")
        }
    };
    let Ok(mut route) = serde_json::from_slice::<Route_preview>(&output) else {
        return Route_preview::unavailable(
            "Authoritative router returned an invalid policy preview.",
        );
    };
    if route.schema_version != 1
        || route.difficulty.trim().is_empty()
        || route.model.trim().is_empty()
        || route.reasoning_effort.trim().is_empty()
        || !route.score.is_finite()
        || route.signals.len() > 64
        || route.signals.iter().any(|s| s.len() > 256)
        || route.summary.len() > 2048
        || route.model_advice.is_some()
        || route.jev_evidence.is_some()
        || route.task_advice.is_some()
    {
        return Route_preview::unavailable(
            "Authoritative router returned an invalid policy preview.",
        );
    }
    route.model_advice = None;
    route
}
