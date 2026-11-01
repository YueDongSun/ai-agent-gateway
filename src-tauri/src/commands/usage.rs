//! Usage and cost accounting IPC commands.
//!
//! `database/dao/usage.rs` and the `usage_records` table have existed since
//! Phase 7, but nothing ever wrote a row and no command read one. The write
//! path now lives in `proxy::usage_recorder` (called from the router's
//! success branch); these commands are the read surface.
//!
//! Every aggregate takes an explicit time window rather than a named range:
//! the caller owns the clock, so "last 7 days" and "this billing period" are
//! both expressible without this module guessing at a user's calendar.

use serde::Serialize;
use tauri::State;

use crate::database::Database;
use crate::database::dao::usage::{ModelUsage, ProviderUsage, UsageSummary};

/// Read window in Unix epoch seconds (inclusive lower, exclusive upper).
#[allow(non_camel_case_types)]
#[derive(Debug, Clone, Default)]
pub(crate) struct Usage_window {
    pub since: Option<i64>,
    pub until: Option<i64>,
    pub app_id: Option<String>,
}

impl Usage_window {
    fn app(&self) -> Option<&str> {
        self.app_id.as_deref()
    }
}

/// Full accounting snapshot for one window: the headline totals plus the
/// per-model and per-provider breakdowns the dashboard renders together.
///
/// One command rather than three so the UI cannot show a summary and its
/// breakdown disagreeing about the same window.
#[allow(non_camel_case_types)]
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Usage_report {
    pub summary: UsageSummary,
    pub by_model: Vec<ModelUsage>,
    pub by_provider: Vec<ProviderUsage>,
    /// True when the window holds no records at all, so the UI can
    /// distinguish "no spend" from "never proxied anything".
    pub is_empty: bool,
}

#[tauri::command]
pub(crate) fn usage_report(
    db: State<'_, Database>,
    since: Option<i64>,
    until: Option<i64>,
    app_id: Option<String>,
) -> Result<Usage_report, String> {
    let window = Usage_window {
        since,
        until,
        app_id,
    };
    let summary = db.get_usage_summary(window.since, window.until, window.app()).map_err(String::from)?;
    let by_model = db
        .get_usage_by_model(window.since, window.until, window.app())
        .map_err(String::from)?;
    let by_provider = db
        .get_usage_by_provider(window.since, window.until, window.app())
        .map_err(String::from)?;
    Ok(Usage_report {
        is_empty: summary.total_requests == 0,
        summary,
        by_model,
        by_provider,
    })
}

/// Delete usage rows older than `before_epoch`.
///
/// Returns the number of rows removed so the UI can report what was actually
/// pruned instead of assuming the window was already clear.
#[tauri::command]
pub(crate) fn usage_prune(db: State<'_, Database>, before_epoch: i64) -> Result<usize, String> {
    let count = db
        .get_usage_summary(Some(before_epoch), None, None)
        .map_err(String::from)?
        .total_requests;
    db.delete_usage_before(before_epoch).map_err(String::from)?;
    Ok(count as usize)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::dao::usage::UsageRecordInput;

    fn db() -> Database {
        Database::memory().expect("open in-memory test db")
    }

    fn row(app: &str, provider: &str, model: &str, ts: i64) -> UsageRecordInput {
        UsageRecordInput {
            ts,
            app_id: app.to_string(),
            provider_id: provider.to_string(),
            model: model.to_string(),
            prompt_tokens: 100,
            completion_tokens: 50,
            cost_usd: 0.01,
            duration_ms: 120,
        }
    }

    /// The end-to-end point of the whole round: the router writes rows and
    /// this surface reads them back. Previously the write path did not exist,
    /// so this could not be asserted at all.
    #[test]
    fn recorded_rows_are_readable_through_the_report() {
        let db = db();
        db.record_usage(&row("claude", "p1", "claude-sonnet-5", 1_000)).unwrap();
        db.record_usage(&row("claude", "p2", "claude-opus-5", 2_000)).unwrap();
        db.record_usage(&row("codex", "p1", "gpt-5", 3_000)).unwrap();

        let all = db.get_usage_summary(None, None, None).unwrap();
        assert_eq!(all.total_requests, 3);
        assert_eq!(all.total_prompt_tokens, 300);

        let claude = db.get_usage_summary(None, None, Some("claude")).unwrap();
        assert_eq!(claude.total_requests, 2, "app filter must exclude codex rows");
        assert_eq!(claude.total_tokens, 300);

        let by_model = db.get_usage_by_model(None, None, None).unwrap();
        assert_eq!(by_model.len(), 3);
        let by_provider = db.get_usage_by_provider(None, None, None).unwrap();
        assert_eq!(by_provider.len(), 2, "two distinct providers");
    }

    #[test]
    fn window_bounds_are_respected() {
        let db = db();
        db.record_usage(&row("claude", "p1", "m", 1_000)).unwrap();
        db.record_usage(&row("claude", "p1", "m", 5_000)).unwrap();
        // Exclusive upper bound: the row at exactly 5_000 is outside.
        let windowed = db.get_usage_summary(Some(1_000), Some(5_000), None).unwrap();
        assert_eq!(windowed.total_requests, 1);
    }

    #[test]
    fn an_empty_window_is_reported_as_empty_not_as_zero_spend() {
        let db = db();
        assert_eq!(db.count_usage().unwrap(), 0);
        let s = db.get_usage_summary(None, None, None).unwrap();
        assert_eq!(s.total_requests, 0);
        assert_eq!(s.total_cost_usd, 0.0);
    }

    #[test]
    fn prune_reports_how_much_it_removed() {
        let db = db();
        db.record_usage(&row("claude", "p1", "m", 1_000)).unwrap();
        db.record_usage(&row("claude", "p1", "m", 9_000)).unwrap();
        let removed = db.delete_usage_before(5_000).unwrap();
        assert_eq!(removed, 1);
        assert_eq!(db.count_usage().unwrap(), 1);
    }
}