//! SQLite database layer.
//!
//! Mirrors cc-switch's `database/` module (`src-tauri/src/database/mod.rs`).
//! `Database` wraps a `Mutex<rusqlite::Connection>` so it can be shared as
//! Tauri managed state (rusqlite's `Connection` is not `Sync`).
//!
//! ## Layout
//! ```text
//! database/
//! ├── mod.rs        - Database struct + init
//! ├── schema.rs     - CREATE TABLE statements + version migrations
//! └── dao/
//!     ├── mod.rs
//!     └── providers.rs - provider CRUD
//! ```

pub mod dao;
pub(crate) mod seed;
mod schema;

use crate::config::get_db_path;
use crate::error::AppError;
use rusqlite::Connection;
use serde::Serialize;
use std::sync::Mutex;

/// Current schema version. Increment on every schema change and add a migration
/// branch in `schema.rs::apply_schema_migrations`.
pub(crate) const SCHEMA_VERSION: i32 = 18;

/// Serialize a value to a JSON string, mapping errors to AppError.
pub(crate) fn to_json_string<T: Serialize>(value: &T) -> Result<String, AppError> {
    serde_json::to_string(value).map_err(AppError::Json)
}

/// Safely acquire the Mutex-guarded connection.
macro_rules! lock_conn {
    ($mutex:expr) => {
        $mutex
            .lock()
            .map_err(|e| AppError::Lock(format!("Mutex lock failed: {}", e)))?
    };
}
pub(crate) use lock_conn;

/// Database connection wrapper. Share via `tauri::State<'_, Database>`.
pub struct Database {
    pub(crate) conn: Mutex<Connection>,
}

impl Database {
    /// Open the on-disk database at `~/.aag/aag.db`, creating it if absent.
    /// Creates tables and applies any pending schema migrations.
    pub fn init() -> Result<Self, AppError> {
        let db_path = get_db_path();
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| AppError::io(parent, e))?;
        }

        let conn = Connection::open(&db_path)?;
        conn.execute("PRAGMA foreign_keys = ON;", [])?;
        // PRAGMA journal_mode returns a result row, so use `pragma_update`
        // which silently applies it. (Calling `.execute` on a pragma that
        // returns rows panics with `ExecuteReturnedResults - did you mean to
        // call query?` — this was a latent bug surfaced by the full-stack
        // e2e test in iteration 26; the same pattern was broken in
        // `memory()` below.)
        conn.pragma_update(None, "journal_mode", "WAL")?;

        let db = Self {
            conn: Mutex::new(conn),
        };
        db.create_tables()?;
        db.apply_schema_migrations()?;
        db.seed_proxy_if_empty()?;
        Ok(db)
    }

    /// Open an in-memory database (for tests).
    #[allow(dead_code)]
    pub fn memory() -> Result<Self, AppError> {
        let conn = Connection::open_in_memory()?;
        conn.execute("PRAGMA foreign_keys = ON;", [])?;
        let db = Self {
            conn: Mutex::new(conn),
        };
        db.create_tables()?;
        db.apply_schema_migrations()?;
        // No seed on in-memory DB - tests bring their own data.
        Ok(db)
    }

    /// Read the persisted `user_version` pragma.
    pub(crate) fn get_user_version(conn: &Connection) -> Result<i32, AppError> {
        conn.query_row("PRAGMA user_version;", [], |row| row.get(0))
            .map_err(AppError::from)
    }
}