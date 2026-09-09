//! Routes DAO (routes table).
//!
//! Each route maps a model alias to an ordered list of (provider_id, model) hops.
//! `hops` is stored as a JSON array in the DB.

use crate::database::{lock_conn, Database};
use crate::error::AppError;
use rusqlite::params;
use serde::{Deserialize, Serialize};

/// A single hop in a route's fallback chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteHop {
    pub provider: String,
    pub model: String,
}

/// A route mapping a model alias to an ordered fallback chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Route {
    pub alias: String,
    #[serde(default)]
    pub description: String,
    pub hops: Vec<RouteHop>,
}

impl Database {
    /// List all routes.
    pub fn get_all_routes(&self) -> Result<Vec<Route>, AppError> {
        let conn = lock_conn!(self.conn);
        let mut stmt = conn.prepare(
            "SELECT alias, description, hops FROM routes ORDER BY alias ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            let hops_str: String = row.get(2)?;
            let hops: Vec<RouteHop> = serde_json::from_str(&hops_str).unwrap_or_default();
            Ok(Route {
                alias: row.get(0)?,
                description: row.get(1)?,
                hops,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Upsert a route.
    pub fn save_route(&self, r: &Route) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        let hops_json = serde_json::to_string(&r.hops)?;
        let now = chrono::Utc::now().timestamp();
        conn.execute(
            "INSERT INTO routes (alias, description, hops, created_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(alias) DO UPDATE SET
                description = excluded.description, hops = excluded.hops",
            params![r.alias, r.description, hops_json, now],
        )?;
        Ok(())
    }

    /// Delete a route.
    #[allow(dead_code)]
    pub fn delete_route(&self, alias: &str) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute("DELETE FROM routes WHERE alias = ?1", params![alias])?;
        Ok(())
    }
}