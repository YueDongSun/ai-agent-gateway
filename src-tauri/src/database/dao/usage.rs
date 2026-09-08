//! Usage tracking DAO (usage_records table).
//!
//! Mirrors cc-switch's `proxy_request_logs` table (`database/schema.rs:186`)
//! and the aggregation methods in `services/usage_stats.rs` (`get_usage_summary`,
//! `get_model_stats`, `get_provider_stats`). AAG's schema is simpler — no
//! cache tokens / success rate — so the aggregates are correspondingly
//! simpler.
//!
//! Phase 7 Step 1: table + DAO + aggregate methods. Tauri commands and
//! dashboard charts land in later steps.

use crate::database::{lock_conn, Database};
use crate::error::AppError;
use rusqlite::params;
use serde::{Deserialize, Serialize};

/// A single usage record. One row per proxied request.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRecord {
    pub id: i64,
    /// Unix epoch seconds.
    pub ts: i64,
    pub app_id: String,
    pub provider_id: String,
    pub model: String,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub cost_usd: f64,
    pub duration_ms: i64,
}

/// Input for `record_usage` (id is auto-assigned by SQLite).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRecordInput {
    pub ts: i64,
    pub app_id: String,
    pub provider_id: String,
    pub model: String,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub cost_usd: f64,
    pub duration_ms: i64,
}

/// Aggregate usage summary over a time window. Mirrors cc-switch's
/// `UsageSummary` (`usage_stats.rs:20`) in shape, but simpler.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct UsageSummary {
    pub total_requests: i64,
    pub total_prompt_tokens: i64,
    pub total_completion_tokens: i64,
    pub total_tokens: i64,
    pub total_cost_usd: f64,
    pub avg_duration_ms: f64,
}

/// Per-model usage aggregate.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ModelUsage {
    pub model: String,
    pub request_count: i64,
    pub total_prompt_tokens: i64,
    pub total_completion_tokens: i64,
    pub total_tokens: i64,
    pub total_cost_usd: f64,
}

/// Per-provider usage aggregate.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProviderUsage {
    pub provider_id: String,
    pub request_count: i64,
    pub total_prompt_tokens: i64,
    pub total_completion_tokens: i64,
    pub total_tokens: i64,
    pub total_cost_usd: f64,
}

impl Database {
    /// Record a single usage event. Returns the new row id.
    #[allow(dead_code)]
    pub fn record_usage(&self, rec: &UsageRecordInput) -> Result<i64, AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute(
            "INSERT INTO usage_records (
                ts, app_id, provider_id, model,
                prompt_tokens, completion_tokens, cost_usd, duration_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                rec.ts,
                rec.app_id,
                rec.provider_id,
                rec.model,
                rec.prompt_tokens,
                rec.completion_tokens,
                rec.cost_usd,
                rec.duration_ms,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Aggregate usage summary over a time window. `since` and `until` are
    /// Unix epoch seconds (inclusive lower, exclusive upper). `None` means
    /// unbounded on that side. `app_id = None` aggregates across all apps.
    #[allow(dead_code)]
    pub fn get_usage_summary(
        &self,
        since: Option<i64>,
        until: Option<i64>,
        app_id: Option<&str>,
    ) -> Result<UsageSummary, AppError> {
        let conn = lock_conn!(self.conn);
        let (where_sql, args) = build_where(since, until, app_id);
        let sql = format!(
            "SELECT COUNT(*), COALESCE(SUM(prompt_tokens), 0), COALESCE(SUM(completion_tokens), 0),
                    COALESCE(SUM(cost_usd), 0.0), COALESCE(AVG(duration_ms), 0.0)
             FROM usage_records {where_sql}"
        );
        let mut stmt = conn.prepare(&sql)?;
        let (n, pt, ct, cost, avg): (i64, i64, i64, f64, f64) =
            stmt.query_row(rusqlite::params_from_iter(args.iter()), |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })?;
        Ok(UsageSummary {
            total_requests: n,
            total_prompt_tokens: pt,
            total_completion_tokens: ct,
            total_tokens: pt + ct,
            total_cost_usd: cost,
            avg_duration_ms: avg,
        })
    }

    /// Per-model aggregate, sorted by total_tokens DESC.
    #[allow(dead_code)]
    pub fn get_usage_by_model(
        &self,
        since: Option<i64>,
        until: Option<i64>,
        app_id: Option<&str>,
    ) -> Result<Vec<ModelUsage>, AppError> {
        let conn = lock_conn!(self.conn);
        let (where_sql, args) = build_where(since, until, app_id);
        let sql = format!(
            "SELECT model, COUNT(*), COALESCE(SUM(prompt_tokens), 0),
                    COALESCE(SUM(completion_tokens), 0), COALESCE(SUM(cost_usd), 0.0)
             FROM usage_records {where_sql}
             GROUP BY model ORDER BY (SUM(prompt_tokens) + SUM(completion_tokens)) DESC"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(args.iter()), |row| {
            let pt: i64 = row.get(2)?;
            let ct: i64 = row.get(3)?;
            Ok(ModelUsage {
                model: row.get(0)?,
                request_count: row.get(1)?,
                total_prompt_tokens: pt,
                total_completion_tokens: ct,
                total_tokens: pt + ct,
                total_cost_usd: row.get(4)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows { out.push(r?); }
        Ok(out)
    }

    /// Per-provider aggregate, sorted by total_tokens DESC.
    #[allow(dead_code)]
    pub fn get_usage_by_provider(
        &self,
        since: Option<i64>,
        until: Option<i64>,
        app_id: Option<&str>,
    ) -> Result<Vec<ProviderUsage>, AppError> {
        let conn = lock_conn!(self.conn);
        let (where_sql, args) = build_where(since, until, app_id);
        let sql = format!(
            "SELECT provider_id, COUNT(*), COALESCE(SUM(prompt_tokens), 0),
                    COALESCE(SUM(completion_tokens), 0), COALESCE(SUM(cost_usd), 0.0)
             FROM usage_records {where_sql}
             GROUP BY provider_id ORDER BY (SUM(prompt_tokens) + SUM(completion_tokens)) DESC"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(args.iter()), |row| {
            let pt: i64 = row.get(2)?;
            let ct: i64 = row.get(3)?;
            Ok(ProviderUsage {
                provider_id: row.get(0)?,
                request_count: row.get(1)?,
                total_prompt_tokens: pt,
                total_completion_tokens: ct,
                total_tokens: pt + ct,
                total_cost_usd: row.get(4)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows { out.push(r?); }
        Ok(out)
    }

    /// Delete a usage record by id. Mainly for tests / cleanup.
    #[allow(dead_code)]
    pub fn delete_usage(&self, id: i64) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute("DELETE FROM usage_records WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Count all usage records. Useful for tests + empty-state checks.
    #[allow(dead_code)]
    pub fn count_usage(&self) -> Result<i64, AppError> {
        let conn = lock_conn!(self.conn);
        conn.query_row("SELECT COUNT(*) FROM usage_records", [], |row| row.get(0))
            .map_err(AppError::from)
    }
}

/// Build a `WHERE` clause for the time window + app filter. Returns the
/// SQL fragment (without the leading `WHERE`) and the ordered args.
fn build_where(
    since: Option<i64>,
    until: Option<i64>,
    app_id: Option<&str>,
) -> (String, Vec<Box<dyn rusqlite::ToSql>>) {
    let mut clauses: Vec<String> = Vec::new();
    let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if let Some(s) = since {
        clauses.push(format!("ts >= ?{}", args.len() + 1));
        args.push(Box::new(s));
    }
    if let Some(u) = until {
        clauses.push(format!("ts < ?{}", args.len() + 1));
        args.push(Box::new(u));
    }
    if let Some(a) = app_id {
        clauses.push(format!("app_id = ?{}", args.len() + 1));
        args.push(Box::new(a.to_string()));
    }
    let where_sql = if clauses.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", clauses.join(" AND "))
    };
    (where_sql, args)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_db() -> Database {
        Database::memory().expect("memory db")
    }

    fn rec(app: &str, prov: &str, model: &str, ts: i64, pt: i64, ct: i64, cost: f64, dur: i64) -> UsageRecordInput {
        UsageRecordInput {
            ts,
            app_id: app.into(),
            provider_id: prov.into(),
            model: model.into(),
            prompt_tokens: pt,
            completion_tokens: ct,
            cost_usd: cost,
            duration_ms: dur,
        }
    }

    #[test]
    fn record_and_count() {
        let db = make_db();
        let id1 = db.record_usage(&rec("claude_code", "anthropic", "claude-3-5-sonnet", 100, 10, 20, 0.001, 500)).unwrap();
        let id2 = db.record_usage(&rec("claude_code", "anthropic", "claude-3-5-sonnet", 200, 30, 40, 0.002, 600)).unwrap();
        assert!(id1 > 0 && id2 > id1);
        assert_eq!(db.count_usage().unwrap(), 2);
    }

    #[test]
    fn summary_aggregates_tokens_cost_and_duration() {
        let db = make_db();
        db.record_usage(&rec("claude_code", "anthropic", "claude-3-5-sonnet", 100, 100, 200, 0.010, 400)).unwrap();
        db.record_usage(&rec("claude_code", "anthropic", "claude-3-5-sonnet", 200, 50,  150, 0.005, 800)).unwrap();
        db.record_usage(&rec("codex", "openai", "gpt-4o", 300, 200, 100, 0.020, 200)).unwrap();

        let sum = db.get_usage_summary(None, None, None).unwrap();
        assert_eq!(sum.total_requests, 3);
        assert_eq!(sum.total_prompt_tokens, 350);
        assert_eq!(sum.total_completion_tokens, 450);
        assert_eq!(sum.total_tokens, 800);
        // Cost: 0.010 + 0.005 + 0.020 = 0.035
        assert!((sum.total_cost_usd - 0.035).abs() < 1e-9, "cost={}", sum.total_cost_usd);
        // Avg duration: (400 + 800 + 200) / 3 = 466.666...
        assert!((sum.avg_duration_ms - 466.6667).abs() < 0.01, "avg_dur={}", sum.avg_duration_ms);
    }

    #[test]
    fn summary_filters_by_time_window_and_app() {
        let db = make_db();
        db.record_usage(&rec("claude_code", "anthropic", "claude-3-5-sonnet", 100, 10, 20, 0.001, 100)).unwrap();
        db.record_usage(&rec("claude_code", "anthropic", "claude-3-5-sonnet", 200, 30, 40, 0.002, 100)).unwrap();
        db.record_usage(&rec("codex",       "openai",     "gpt-4o",            300, 50, 60, 0.003, 100)).unwrap();
        db.record_usage(&rec("codex",       "openai",     "gpt-4o",            400, 70, 80, 0.004, 100)).unwrap();

        // Time window: [150, 350) -> 200, 300
        let sum = db.get_usage_summary(Some(150), Some(350), None).unwrap();
        assert_eq!(sum.total_requests, 2);
        assert_eq!(sum.total_prompt_tokens, 30 + 50);

        // App filter: codex -> 300, 400
        let sum = db.get_usage_summary(None, None, Some("codex")).unwrap();
        assert_eq!(sum.total_requests, 2);
        assert_eq!(sum.total_cost_usd, 0.007);
    }

    #[test]
    fn by_model_groups_and_sums() {
        let db = make_db();
        db.record_usage(&rec("claude_code", "anthropic", "claude-3-5-sonnet", 100, 10, 20, 0.001, 100)).unwrap();
        db.record_usage(&rec("claude_code", "anthropic", "claude-3-5-sonnet", 200, 30, 40, 0.002, 100)).unwrap();
        db.record_usage(&rec("claude_code", "anthropic", "claude-3-haiku", 300, 5,  10, 0.0005, 100)).unwrap();

        let by_model = db.get_usage_by_model(None, None, None).unwrap();
        assert_eq!(by_model.len(), 2);
        // sonnet has more total tokens -> first
        assert_eq!(by_model[0].model, "claude-3-5-sonnet");
        assert_eq!(by_model[0].request_count, 2);
        assert_eq!(by_model[0].total_tokens, 100);
        assert_eq!(by_model[1].model, "claude-3-haiku");
        assert_eq!(by_model[1].request_count, 1);
    }

    #[test]
    fn by_provider_groups_and_sums() {
        let db = make_db();
        db.record_usage(&rec("claude_code", "anthropic", "m", 100, 10, 20, 0.001, 100)).unwrap();
        db.record_usage(&rec("claude_code", "anthropic", "m", 200, 30, 40, 0.002, 100)).unwrap();
        db.record_usage(&rec("claude_code", "openai",     "m", 300, 50, 60, 0.003, 100)).unwrap();

        // anthropic: 2 records, 10+20+30+40 = 100 total tokens
        // openai:     1 record,  50+60        = 110 total tokens
        // openai has more total tokens -> sorted first
        let by_prov = db.get_usage_by_provider(None, None, None).unwrap();
        assert_eq!(by_prov.len(), 2);
        assert_eq!(by_prov[0].provider_id, "openai");
        assert_eq!(by_prov[0].request_count, 1);
        assert_eq!(by_prov[0].total_tokens, 110);
        assert_eq!(by_prov[1].provider_id, "anthropic");
        assert_eq!(by_prov[1].request_count, 2);
        assert_eq!(by_prov[1].total_tokens, 100);
    }

    #[test]
    fn delete_removes_record() {
        let db = make_db();
        let id = db.record_usage(&rec("claude_code", "anthropic", "m", 100, 1, 2, 0.01, 50)).unwrap();
        assert_eq!(db.count_usage().unwrap(), 1);
        db.delete_usage(id).unwrap();
        assert_eq!(db.count_usage().unwrap(), 0);
    }
}