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
pub(crate) mod operator;
mod schema;
pub(crate) mod seed;

use crate::config::{get_db_path, private_fs};
use crate::error::AppError;
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use std::path::Path;
use std::sync::{Arc, Mutex};

/// Current schema version. Increment on every schema change and add a migration
/// branch in `schema.rs::apply_schema_migrations`.
pub(crate) const SCHEMA_VERSION: i32 = 25;

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
/// Cheaply cloneable (the underlying `Mutex<Connection>` is wrapped in
/// `Arc` so every clone shares the same connection).
#[derive(Clone)]
pub struct Database {
    pub(crate) conn: Arc<Mutex<Connection>>,
}

impl Database {
    /// Open the on-disk database at `~/.aag/aag.db`, creating it if absent.
    /// Creates tables and applies any pending schema migrations.
    pub fn init() -> Result<Self, AppError> {
        let db_path = get_db_path();
        Self::init_at_path_with_policy(&db_path, crate::config::app_config_dir_is_managed_default())
    }

    #[cfg(test)]
    pub(crate) fn init_at_path(db_path: &Path) -> Result<Self, AppError> {
        Self::init_at_path_with_policy(db_path, true)
    }

    fn init_at_path_with_policy(
        db_path: &Path,
        harden_existing_directory: bool,
    ) -> Result<Self, AppError> {
        if let Some(parent) = db_path.parent() {
            if harden_existing_directory {
                private_fs::prepare_managed_private_dir(parent)?;
            } else {
                private_fs::prepare_private_dir(parent)?;
            }
        }
        private_fs::ensure_private_file(db_path)?;
        private_fs::preflight_sqlite_files(db_path)?;
        // SQLite opens these with OPEN_ALWAYS. Create absent WAL sidecars
        // privately first so an elevated token's default group owner cannot
        // make this new database fail its own post-open ownership check.
        // Existing sidecars still pass the unchanged preflight above.
        for suffix in ["-wal", "-shm"] {
            let mut sidecar = db_path.as_os_str().to_owned();
            sidecar.push(suffix);
            private_fs::ensure_private_file(Path::new(&sidecar))?;
        }

        let conn = Connection::open_with_flags(
            db_path,
            OpenFlags::default() | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        conn.execute("PRAGMA foreign_keys = ON;", [])?;
        // Backup payloads can contain provider keys and proxy credentials.
        // Secure deletion limits plaintext remnants when retention removes rows.
        conn.pragma_update(None, "secure_delete", "ON")?;
        // PRAGMA journal_mode returns a result row, so use `pragma_update`
        // which silently applies it. (Calling `.execute` on a pragma that
        // returns rows panics with `ExecuteReturnedResults - did you mean to
        // call query?` — this was a latent bug surfaced by the full-stack
        // e2e test in iteration 26; the same pattern was broken in
        // `memory()` below.)
        conn.pragma_update(None, "journal_mode", "WAL")?;
        // Cross-process readers (the read-only MCP companion scripts use the
        // same ~/.aag/aag.db) can hold the write lock briefly. Without a
        // busy timeout those windows surface as immediate SQLITE_BUSY, which
        // callers like the periodic model-discovery run silently swallow —
        // a whole discovery cycle skipped for a microsecond of contention.
        // 1 s is the ceiling, not the typical wait: sync Tauri commands run
        // on the UI thread, so a longer timeout would freeze the window on
        // contention. Brief cross-process reader stalls recover well inside
        // 1 s; anything longer surfaces as SQLITE_BUSY again.
        conn.pragma_update(None, "busy_timeout", "1000")?;

        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        db.create_tables()?;
        db.apply_schema_migrations()?;
        db.seed_proxy_if_empty()?;
        private_fs::preflight_sqlite_files(db_path)?;
        Ok(db)
    }

    /// Open an in-memory database (for tests).
    #[allow(dead_code)]
    pub fn memory() -> Result<Self, AppError> {
        let conn = Connection::open_in_memory()?;
        conn.execute("PRAGMA foreign_keys = ON;", [])?;
        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
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

#[cfg(test)]
mod private_storage_tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let mut nonce = [0u8; 8];
            getrandom::getrandom(&mut nonce).expect("test nonce");
            let path = std::env::temp_dir().join(format!(
                "aag_database_permissions_{}_{}",
                std::process::id(),
                hex::encode(nonce)
            ));
            fs::create_dir(&path).expect("create test root");
            Self(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn disk_database_main_wal_shm_and_directory_are_private() {
        let root = TestDir::new();
        let directory = root.0.join("state");
        let path = directory.join("aag.db");
        let db = Database::init_at_path(&path).expect("disk database");
        {
            let conn = db.conn.lock().expect("database lock");
            conn.execute(
                "INSERT OR REPLACE INTO app_meta (key, value) VALUES ('permission_test', 'secret')",
                [],
            )
            .expect("create WAL activity");
        }
        private_fs::preflight_sqlite_files(&path).expect("secure SQLite files");

        private_fs::verify_private_dir(&directory).expect("private database directory");
        private_fs::verify_private_file(&path).expect("private database");
        for suffix in ["-wal", "-shm"] {
            let sidecar = PathBuf::from(format!("{}{suffix}", path.to_string_lossy()));
            assert!(
                sidecar.exists(),
                "{suffix} must exist while WAL connection is open"
            );
            private_fs::verify_private_file(&sidecar).expect("private SQLite sidecar");
        }
    }

    #[cfg(unix)]
    #[test]
    fn disk_database_rejects_linked_wal_before_sqlite_opens_it() {
        use std::os::unix::fs::symlink;

        let root = TestDir::new();
        let directory = root.0.join("state");
        let path = directory.join("aag.db");
        private_fs::prepare_private_dir(&directory).unwrap();
        private_fs::ensure_private_file(&path).unwrap();

        let sentinel = root.0.join("sentinel");
        fs::write(&sentinel, b"must remain unchanged").unwrap();
        let mut wal_name = path.as_os_str().to_os_string();
        wal_name.push("-wal");
        let wal = PathBuf::from(wal_name);
        symlink(&sentinel, &wal).unwrap();

        let error = Database::init_at_path(&path)
            .err()
            .expect("linked WAL must be rejected before opening SQLite");
        assert!(error.to_string().contains("private_storage:linked_path"));
        assert_eq!(fs::read(&sentinel).unwrap(), b"must remain unchanged");
    }
}
