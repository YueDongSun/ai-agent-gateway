//! Metadata-only last successful router model, already recorded by usage accounting.
use crate::{database::{lock_conn, Database}, error::AppError};
use rusqlite::OptionalExtension;
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Observed_router_model {
    pub model: String,
    pub app_type: String,
    pub observed_at: i64,
}

impl Database {
    pub(crate) fn last_observed_router_model(&self) -> Result<Option<Observed_router_model>, AppError> {
        let conn = lock_conn!(self.conn);
        Ok(conn.query_row("SELECT model, app_id, ts FROM usage_records ORDER BY ts DESC, id DESC LIMIT 1", [], |row| {
            Ok(Observed_router_model { model: row.get(0)?, app_type: row.get(1)?, observed_at: row.get(2)? })
        }).optional()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn empty_usage_does_not_invent_a_current_model() {
        let db = Database::memory().unwrap();
        assert!(db.last_observed_router_model().unwrap().is_none());
    }
}
