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

fn route_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Route> {
    let alias: String = row.get(0)?;
    let description: String = row.get(1)?;
    let hops_json: String = row.get(2)?;
    let hops: Vec<RouteHop> = serde_json::from_str(&hops_json).map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            2,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "invalid persisted route hops JSON",
            )),
        )
    })?;

    Ok(Route {
        alias,
        description,
        hops,
    })
}

impl Database {
    /// List all routes.
    pub fn get_all_routes(&self) -> Result<Vec<Route>, AppError> {
        let conn = lock_conn!(self.conn);
        let mut stmt =
            conn.prepare("SELECT alias, description, hops FROM routes ORDER BY alias ASC")?;
        let rows = stmt.query_map([], route_from_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Get a single route by alias.
    pub fn get_route(&self, alias: &str) -> Result<Option<Route>, AppError> {
        let conn = lock_conn!(self.conn);
        get_route_on_conn(&conn, alias)
    }

    /// Upsert a route.
    pub fn save_route(&self, r: &Route) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        save_route_on_conn(&conn, r)
    }

    /// Delete a route.
    pub fn delete_route(&self, alias: &str) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute("DELETE FROM routes WHERE alias = ?1", params![alias])?;
        Ok(())
    }

    /// Rename a route's alias in place (hops + description ride along —
    /// they live on the same row). Fails if the target alias already
    /// exists or the source is missing. Mirrors the style of the
    /// sibling single-UPDATE DAOs (one statement, no implicit
    /// transaction wrapper needed).
    pub fn rename_route(&self, from: &str, to: &str) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        let updated = conn
            .execute(
                "UPDATE routes SET alias = ?1 WHERE alias = ?2",
                params![to, from],
            )
            .map_err(|error| match error {
                rusqlite::Error::SqliteFailure(failure, _)
                    if failure.code == rusqlite::ErrorCode::ConstraintViolation =>
                {
                    AppError::InvalidInput("AAG_ROUTE_ALIAS_CONFLICT".into())
                }
                other => AppError::from(other),
            })?;
        if updated == 0 {
            return Err(AppError::InvalidInput("AAG_ROUTE_NOT_FOUND".into()));
        }
        Ok(())
    }
}

/// Connection-level DAO used within an existing transaction.
pub(crate) fn get_route_on_conn(
    conn: &rusqlite::Connection,
    alias: &str,
) -> Result<Option<Route>, AppError> {
    let mut stmt = conn.prepare("SELECT alias, description, hops FROM routes WHERE alias = ?1")?;
    let mut rows = stmt.query_map(params![alias], route_from_row)?;
    match rows.next() {
        Some(r) => Ok(Some(r?)),
        None => Ok(None),
    }
}

/// Connection-level DAO used within an existing transaction.
pub(crate) fn save_route_on_conn(conn: &rusqlite::Connection, r: &Route) -> Result<(), AppError> {
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

#[cfg(test)]
pub(crate) fn install_discovery_read_failure(db: &Database) {
    let conn = db.conn.lock().unwrap();
    conn.execute(
        "INSERT INTO routes (alias, description, hops) VALUES (?1, ?2, ?3)",
        params!["invalid_route", "", "{"],
    )
    .unwrap();
}

/// In-memory regression fixture: fail the later route write to prove rollback.
#[cfg(test)]
pub(crate) fn install_discovery_write_failure(db: &Database) {
    let conn = db.conn.lock().unwrap();
    conn.execute_batch(
        "CREATE TRIGGER discovery_write_failure BEFORE UPDATE ON routes
         WHEN NEW.alias = 'blocked_route'
         BEGIN SELECT RAISE(FAIL, 'injected_route_write_failure'); END;",
    )
    .unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(alias: &str) -> Route {
        Route {
            alias: alias.to_owned(),
            description: String::new(),
            hops: vec![RouteHop {
                provider: "fixture_provider".to_owned(),
                model: "fixture_model".to_owned(),
            }],
        }
    }

    fn invalid_route_hops_examples() -> [(&'static str, &'static str); 6] {
        [
            ("malformed JSON", "{"),
            ("null value", "null"),
            ("wrong top-level shape", "{}"),
            ("hop missing model", r#"[{"provider":"fixture_provider"}]"#),
            (
                "hop provider has wrong type",
                r#"[{"provider":7,"model":"fixture_model"}]"#,
            ),
            (
                "malformed suffix with canary",
                concat!(
                    r#"[{"provider":"fixture_provider","model":"fixture_model"}] "#,
                    "ROUTE_HOPS_CANARY_PRIVATE_PAYLOAD_20261018"
                ),
            ),
        ]
    }

    fn set_raw_route_hops(db: &Database, alias: &str, raw_hops: &str) {
        let conn = db.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("UPDATE routes SET hops = ?1 WHERE alias = ?2")
            .unwrap();
        assert_eq!(stmt.execute(params![raw_hops, alias]).unwrap(), 1);
    }

    fn stored_route_hops(db: &Database, alias: &str) -> String {
        let conn = db.conn.lock().unwrap();
        conn.query_row(
            "SELECT hops FROM routes WHERE alias = ?1",
            params![alias],
            |row| row.get(0),
        )
        .unwrap()
    }

    fn assert_route_read_rejected(
        db: &Database,
        alias: &str,
        raw_hops: &str,
        case_name: &str,
        error: AppError,
    ) {
        let internal_error = error.to_string();
        let external_error = String::from(error);
        assert_eq!(external_error, "database_error", "{case_name}");
        assert!(
            internal_error.contains("invalid persisted route hops JSON"),
            "{case_name}: {internal_error}"
        );
        assert!(
            !internal_error.contains("ROUTE_HOPS_CANARY_PRIVATE_PAYLOAD_20261018"),
            "{case_name}: {internal_error}"
        );
        assert!(
            !external_error.contains("ROUTE_HOPS_CANARY_PRIVATE_PAYLOAD_20261018"),
            "{case_name}: {external_error}"
        );
        assert_eq!(stored_route_hops(db, alias), raw_hops, "{case_name}");
    }

    fn assert_ordered_hops(hops: &[RouteHop]) {
        assert_eq!(hops.len(), 2);
        assert_eq!(hops[0].provider.as_str(), "first_provider");
        assert_eq!(hops[0].model.as_str(), "first_model");
        assert_eq!(hops[1].provider.as_str(), "second_provider");
        assert_eq!(hops[1].model.as_str(), "second_model");
    }

    #[test]
    fn single_route_read_rejects_invalid_persisted_hops_without_leaking_data() {
        let db = Database::memory().unwrap();
        db.save_route(&route("invalid_route")).unwrap();

        for (case_name, raw_hops) in invalid_route_hops_examples() {
            set_raw_route_hops(&db, "invalid_route", raw_hops);
            let error = db.get_route("invalid_route").unwrap_err();
            assert_route_read_rejected(&db, "invalid_route", raw_hops, case_name, error);
        }
    }

    #[test]
    fn list_route_read_rejects_invalid_persisted_hops_without_partial_results() {
        let db = Database::memory().unwrap();
        db.save_route(&route("a_valid_sibling")).unwrap();
        db.save_route(&route("z_invalid_route")).unwrap();
        assert!(db.get_route("a_valid_sibling").unwrap().is_some());

        for (case_name, raw_hops) in invalid_route_hops_examples() {
            set_raw_route_hops(&db, "z_invalid_route", raw_hops);
            let error = db.get_all_routes().unwrap_err();
            assert_route_read_rejected(&db, "z_invalid_route", raw_hops, case_name, error);
        }
    }

    #[test]
    fn route_reads_preserve_empty_and_ordered_hops_and_missing_alias_behavior() {
        let db = Database::memory().unwrap();
        let empty_route = Route {
            alias: "empty_route".to_owned(),
            description: "empty route fixture".to_owned(),
            hops: vec![],
        };
        let ordered_route = Route {
            alias: "ordered_route".to_owned(),
            description: "ordered route fixture".to_owned(),
            hops: vec![
                RouteHop {
                    provider: "first_provider".to_owned(),
                    model: "first_model".to_owned(),
                },
                RouteHop {
                    provider: "second_provider".to_owned(),
                    model: "second_model".to_owned(),
                },
            ],
        };
        db.save_route(&empty_route).unwrap();
        db.save_route(&ordered_route).unwrap();

        let single_empty = db.get_route("empty_route").unwrap().unwrap();
        assert_eq!(single_empty.alias, "empty_route");
        assert_eq!(single_empty.description, "empty route fixture");
        assert!(single_empty.hops.is_empty());
        let single_ordered = db.get_route("ordered_route").unwrap().unwrap();
        assert_eq!(single_ordered.alias, "ordered_route");
        assert_eq!(single_ordered.description, "ordered route fixture");
        assert_ordered_hops(&single_ordered.hops);
        assert!(db.get_route("missing_route").unwrap().is_none());

        let all_routes = db.get_all_routes().unwrap();
        assert_eq!(all_routes.len(), 2);
        assert_eq!(all_routes[0].alias, "empty_route");
        assert_eq!(all_routes[0].description, "empty route fixture");
        assert!(all_routes[0].hops.is_empty());
        assert_eq!(all_routes[1].alias, "ordered_route");
        assert_eq!(all_routes[1].description, "ordered route fixture");
        assert_ordered_hops(&all_routes[1].hops);
    }

    #[test]
    fn rename_onto_an_existing_alias_surfaces_a_stable_code_and_preserves_rows() {
        let db = Database::memory().unwrap();
        db.save_route(&route("alpha")).unwrap();
        db.save_route(&route("beta")).unwrap();
        let error = db.rename_route("alpha", "beta").unwrap_err();
        // The alias PK collision must not collapse into the opaque
        // database_error fallback: AppPage renders the IPC string verbatim.
        assert_eq!(error.external_message(), "AAG_ROUTE_ALIAS_CONFLICT");
        // The failed rename must not clobber either existing row.
        assert_eq!(db.get_all_routes().unwrap().len(), 2);
        assert!(db.get_route("alpha").unwrap().is_some());
        assert!(db.get_route("beta").unwrap().is_some());
    }

    #[test]
    fn rename_round_trip_moves_the_alias() {
        let db = Database::memory().unwrap();
        db.save_route(&route("alpha")).unwrap();
        db.rename_route("alpha", "gamma").unwrap();
        assert!(db.get_route("alpha").unwrap().is_none());
        assert_eq!(db.get_route("gamma").unwrap().unwrap().alias, "gamma");
    }

    #[test]
    fn rename_from_an_unknown_alias_surfaces_a_stable_code() {
        let db = Database::memory().unwrap();
        db.save_route(&route("alpha")).unwrap();
        let error = db.rename_route("missing", "gamma").unwrap_err();
        assert_eq!(error.external_message(), "AAG_ROUTE_NOT_FOUND");
        assert_eq!(db.get_all_routes().unwrap().len(), 1);
    }
}
