//! Live request targets for the trusted manager UI. Never written to logs or disk.
#![allow(non_camel_case_types)]

use serde::Serialize;
use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

const REQUEST_STATUS_CAPACITY: usize = 200;
const MODEL_DISPLAY_LIMIT: usize = 512;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Request_status {
    pub request_id: String,
    pub surface: String,
    pub requested_model: String,
    pub resolved_model: Option<String>,
    pub phase: &'static str,
    pub fallback_count: usize,
    pub error_code: Option<String>,
    pub config_generation: Option<u64>,
    pub status: Option<u16>,
}

#[derive(Default)]
struct Request_status_store(Mutex<VecDeque<Request_status>>);

impl Request_status_store {
    fn begin(&self, request_id: &str, surface: &str, model: &str, generation: Option<u64>) {
        let mut rows = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if rows.iter().any(|row| row.request_id == request_id) {
            return;
        }
        if rows.len() >= REQUEST_STATUS_CAPACITY {
            let oldest_terminal = rows
                .iter()
                .position(|row| matches!(row.phase, "completed" | "error" | "cancelled"));
            if let Some(index) = oldest_terminal {
                rows.remove(index);
            } else {
                // Keep admitted active requests observable within the fixed bound.
                return;
            }
        }
        rows.push_back(Request_status {
            request_id: request_id.into(),
            surface: surface.into(),
            requested_model: display_model(model),
            resolved_model: None,
            phase: "requesting",
            fallback_count: 0,
            error_code: None,
            config_generation: generation,
            status: None,
        });
    }

    fn update(&self, request_id: &str, change: impl FnOnce(&mut Request_status)) {
        let mut rows = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(index) = rows.iter().position(|row| row.request_id == request_id) {
            let mut row = rows.remove(index).expect("matched request index exists");
            change(&mut row);
            rows.push_back(row);
        }
    }

    fn snapshot(&self) -> Vec<Request_status> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .rev()
            .cloned()
            .collect()
    }
}

fn display_model(model: &str) -> String {
    let mut characters = model.chars();
    let mut display: String = characters.by_ref().take(MODEL_DISPLAY_LIMIT).collect();
    if characters.next().is_some() {
        display.push('…');
    }
    display
}

fn safe_error_code(code: &str) -> &'static str {
    match code {
        "upstream_http_error" => "upstream_http_error",
        "upstream_timeout" => "upstream_timeout",
        "upstream_stream_error" | "proxy_stream_error" => "upstream_stream_error",
        "upstream_connection_error" => "upstream_connection_error",
        "all_hops_exhausted" => "all_hops_exhausted",
        "responses_feature_unsupported" => "responses_feature_unsupported",
        "responses_invalid_request" => "responses_invalid_request",
        "anthropic_feature_unsupported" => "anthropic_feature_unsupported",
        "anthropic_invalid_request" => "anthropic_invalid_request",
        _ => "provider_error",
    }
}

static REQUEST_STATUS: OnceLock<Request_status_store> = OnceLock::new();

fn store() -> &'static Request_status_store {
    REQUEST_STATUS.get_or_init(Request_status_store::default)
}

pub(crate) fn snapshot() -> Vec<Request_status> {
    store().snapshot()
}

pub(crate) fn begin(request_id: &str, surface: &str, model: &str, generation: Option<u64>) {
    store().begin(request_id, surface, model, generation);
}

pub(crate) fn generation(request_id: &str, generation: u64) {
    store().update(request_id, |row| row.config_generation = Some(generation));
}

pub(crate) fn target(request_id: &str, model: &str) {
    store().update(request_id, |row| {
        row.fallback_count += usize::from(row.resolved_model.is_some());
        row.resolved_model = Some(display_model(&crate::proxy::strip_one_m_suffix(model)));
        row.phase = "requesting";
        row.status = None;
    });
}

pub(crate) fn streaming(request_id: &str) {
    store().update(request_id, |row| row.phase = "streaming");
}

pub(crate) fn failed(request_id: &str, code: &str, status: Option<u16>) {
    store().update(request_id, |row| {
        row.error_code = Some(safe_error_code(code).into());
        row.status = status;
    });
}

pub(crate) fn finish(request_id: &str, status: u16) {
    store().update(request_id, |row| {
        row.status = Some(status);
        row.phase = match status {
            499 => "cancelled",
            400..=599 => "error",
            _ => "completed",
        };
        if status == 499 {
            row.error_code = Some("request_cancelled".into());
        } else if status >= 400 && row.error_code.is_none() {
            row.error_code = Some("request_failed".into());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_status_store_preserves_active_requests_and_separate_ids() {
        let store = Request_status_store::default();
        for index in 0..=REQUEST_STATUS_CAPACITY {
            store.begin(&index.to_string(), "responses", "5.6sol", Some(7));
            if index != 0 {
                store.update(&index.to_string(), |row| row.phase = "completed");
            }
        }
        store.update("199", |row| {
            row.resolved_model = Some("concrete-model".into())
        });
        let rows = store.snapshot();
        assert_eq!(rows.len(), REQUEST_STATUS_CAPACITY);
        assert_eq!(rows[0].request_id, "199");
        assert_eq!(rows[0].resolved_model.as_deref(), Some("concrete-model"));
        assert_eq!(rows[1].request_id, "200");
        assert_eq!(rows[1].resolved_model, None);
        assert!(rows
            .iter()
            .any(|row| row.request_id == "0" && row.phase == "requesting"));
        assert!(!rows.iter().any(|row| row.request_id == "1"));
        assert_eq!(rows[0].requested_model, "5.6sol");
        assert_eq!(rows[0].config_generation, Some(7));
    }

    #[test]
    fn overlapping_requests_show_the_most_recent_completion_first() {
        let store = Request_status_store::default();
        store.begin("A", "responses", "first_alias", Some(1));
        store.begin("B", "responses", "second_alias", Some(1));
        store.update("B", |row| row.phase = "completed");
        store.update("A", |row| row.phase = "error");
        for _ in 0..2 {
            let rows = store.snapshot();
            assert_eq!(rows[0].request_id, "A");
            assert_eq!(rows[0].phase, "error");
            assert_eq!(rows[1].request_id, "B");
            assert_eq!(rows[1].phase, "completed");
        }
    }

    #[test]
    fn error_text_is_reduced_to_fixed_codes_and_models_are_bounded() {
        assert_eq!(safe_error_code("secret_payload"), "provider_error");
        assert_eq!(safe_error_code("upstream_timeout"), "upstream_timeout");
        assert_eq!(display_model(&"界".repeat(600)).chars().count(), 513);
    }
}
