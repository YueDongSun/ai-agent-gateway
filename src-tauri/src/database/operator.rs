//! Explicit operators open an existing, current database without startup,
//! migration, seeding, or client/runtime reconciliation.
use super::{lock_conn, Database, SCHEMA_VERSION};
use crate::{config::private_fs, error::AppError};
use rusqlite::{Connection, OpenFlags};
use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

const BUSY_TIMEOUT: Duration = Duration::from_secs(1);

fn validate_existing_path(path: &Path) -> Result<(), AppError> {
    private_fs::validate_external_file_path(path)?;
    if !path.try_exists()? {
        return Err(AppError::InvalidInput(
            "database_operator_file_missing".into(),
        ));
    }
    for suffix in ["-wal", "-shm"] {
        let mut sidecar = path.as_os_str().to_owned();
        sidecar.push(suffix);
        private_fs::validate_external_file_path(Path::new(&sidecar))?;
    }
    Ok(())
}

fn validate_schema(conn: &Connection) -> Result<(), AppError> {
    if Database::get_user_version(conn)? != SCHEMA_VERSION {
        return Err(AppError::InvalidInput(
            "database_operator_schema_mismatch".into(),
        ));
    }
    // Check the owning import contracts too: a user_version stamp alone does
    // not identify an AAG database. Preparing these reads does not load secrets.
    for sql in [
        "SELECT generation FROM supplier_config_meta WHERE id=1",
        "SELECT id,source,provenance_json,import_origins_json,icon,icon_color FROM suppliers LIMIT 0",
        "SELECT id,supplier_id,protocol,alternatives_json FROM supplier_endpoints LIMIT 0",
        "SELECT id,endpoint_id,model_id,source,provenance_json FROM supplier_models LIMIT 0",
        "SELECT endpoint_id,proxy_provider_id,managed FROM supplier_runtime_projections LIMIT 0",
        "SELECT id,type,models,auto_track_models FROM proxy_providers LIMIT 0",
        "SELECT alias,hops FROM routes LIMIT 0",
        "SELECT key,value FROM app_meta LIMIT 0",
    ] {
        conn.prepare(sql).map_err(|_| AppError::InvalidInput("database_operator_schema_invalid".into()))?;
    }
    let generation: i64 = conn.query_row(
        "SELECT generation FROM supplier_config_meta WHERE id=1",
        [],
        |row| row.get(0),
    )?;
    if generation < 0 {
        return Err(AppError::InvalidInput(
            "database_operator_schema_invalid".into(),
        ));
    }
    Ok(())
}

fn open_connection(path: &Path, read_only: bool) -> Result<Connection, AppError> {
    let access = if read_only {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    } else {
        OpenFlags::SQLITE_OPEN_READ_WRITE
    };
    let conn = Connection::open_with_flags(path, access | OpenFlags::SQLITE_OPEN_NOFOLLOW)?;
    conn.busy_timeout(BUSY_TIMEOUT)?;
    conn.pragma_update(None, "foreign_keys", true)?;
    if read_only {
        conn.pragma_update(None, "query_only", true)?;
    }
    validate_schema(&conn)?;
    Ok(conn)
}

impl Database {
    /// No CREATE flag, schema migration, seeding, or application startup. The
    /// preview connection never changes ACLs or application records; SQLite
    /// retains its ordinary WAL reader coordination rather than ignoring WAL.
    pub fn open_existing_for_operator(path: &Path, read_only: bool) -> Result<Self, AppError> {
        validate_existing_path(path)?;
        let preview = open_connection(path, true)?;
        let conn = if read_only {
            preview
        } else {
            drop(preview);
            private_fs::preflight_sqlite_files(path)?;
            let conn = open_connection(path, false)?;
            private_fs::preflight_sqlite_files(path)?;
            conn
        };
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Full database snapshot, including suppliers, routes and recovery history.
    /// Only the new backup destination and its private directory are created.
    pub fn create_operator_snapshot(&self, path: &Path) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        validate_schema(&conn)?;
        if conn.pragma_query_value(None, "query_only", |row| row.get::<_, bool>(0))? {
            return Err(AppError::InvalidInput("database_operator_read_only".into()));
        }
        private_fs::validate_external_file_path(path)?;
        if path.try_exists()? {
            return Err(AppError::InvalidInput(
                "supplier_maintenance_backup_path_invalid".into(),
            ));
        }
        let parent = path.parent().ok_or_else(|| {
            AppError::InvalidInput("database_operator_backup_parent_invalid".into())
        })?;
        private_fs::prepare_managed_private_dir(parent)?;
        create_snapshot_on_conn(&conn, path)
    }
}

/// Shared by import maintenance and explicit operators; retain the existing
/// private-file, no-overwrite, full-copy and integrity-check contract.
pub(crate) fn create_snapshot_on_conn(conn: &Connection, path: &Path) -> Result<(), AppError> {
    if !path.is_absolute() || path.try_exists()? {
        return Err(AppError::InvalidInput(
            "supplier_maintenance_backup_path_invalid".into(),
        ));
    }
    private_fs::ensure_private_file(path)?;
    conn.execute("VACUUM INTO ?1", [path.to_string_lossy().as_ref()])?;
    private_fs::secure_existing_file(path)?;
    let snapshot = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    if snapshot.query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0))? != "ok" {
        return Err(AppError::Message(
            "supplier_maintenance_backup_invalid".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, std::path::PathBuf, Database) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory
            .path()
            .canonicalize()
            .unwrap()
            .join("state")
            .join("aag.db");
        let db = Database::init_at_path(&path).unwrap();
        (directory, path, db)
    }

    #[test]
    fn preview_rejects_missing_files_and_writes_without_creating_state() {
        let (directory, path, writer) = fixture();
        let absent = directory.path().join("missing").join("aag.db");
        for read_only in [true, false] {
            assert!(Database::open_existing_for_operator(&absent, read_only).is_err());
            assert!(!absent.parent().unwrap().exists());
        }
        assert!(Database::open_existing_for_operator(Path::new("relative.db"), true).is_err());
        assert!(Database::open_existing_for_operator(directory.path(), true).is_err());
        writer.set_setting("operator_fixture", "retained").unwrap();
        let reader = Database::open_existing_for_operator(&path, true).unwrap();
        assert_eq!(
            reader.get_setting("operator_fixture").unwrap().as_deref(),
            Some("retained")
        );
        assert!(reader.set_setting("operator_fixture", "changed").is_err());
        let backup = directory.path().join("readonly_snapshot").join("backup.db");
        assert!(reader.create_operator_snapshot(&backup).is_err());
        assert!(!backup.parent().unwrap().exists());
        assert_eq!(
            writer.get_setting("operator_fixture").unwrap().as_deref(),
            Some("retained")
        );
    }

    #[test]
    fn operator_refuses_old_and_forged_schema_without_migrating() {
        let (directory, path, writer) = fixture();
        writer
            .conn
            .lock()
            .unwrap()
            .pragma_update(None, "user_version", SCHEMA_VERSION - 1)
            .unwrap();
        for read_only in [true, false] {
            assert!(Database::open_existing_for_operator(&path, read_only).is_err());
        }
        assert_eq!(
            Database::get_user_version(&writer.conn.lock().unwrap()).unwrap(),
            SCHEMA_VERSION - 1
        );
        let forged = directory.path().join("forged.db");
        let conn = Connection::open(&forged).unwrap();
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)
            .unwrap();
        drop(conn);
        assert!(Database::open_existing_for_operator(&forged, true).is_err());
    }

    #[test]
    fn operator_snapshot_preserves_suppliers_routes_and_refuses_overwrite() {
        let (directory, path, writer) = fixture();
        crate::services::suppliers::save_supplier(&writer,serde_json::from_value(serde_json::json!({
            "id":"operator_fixture","expectedGeneration":0,"name":"Fixture supplier","endpoints":[]
        })).unwrap()).unwrap();
        writer
            .save_route(&crate::database::dao::routes::Route {
                alias: "fixture-route".into(),
                description: "retained".into(),
                hops: vec![],
            })
            .unwrap();
        drop(writer);
        let operator = Database::open_existing_for_operator(&path, false).unwrap();
        let destination = directory.path().join("snapshots").join("before_import.db");
        operator.create_operator_snapshot(&destination).unwrap();
        let snapshot = Database::open_existing_for_operator(&destination, true).unwrap();
        assert!(
            crate::services::suppliers::get_supplier(&snapshot, "operator_fixture")
                .unwrap()
                .supplier
                .is_some()
        );
        assert!(snapshot
            .get_all_routes()
            .unwrap()
            .iter()
            .any(|route| route.alias == "fixture-route" && route.description == "retained"));
        drop(snapshot);
        let original = std::fs::read(&destination).unwrap();
        assert!(operator.create_operator_snapshot(&destination).is_err());
        assert_eq!(std::fs::read(&destination).unwrap(), original);
        private_fs::verify_private_file(&destination).unwrap();
        private_fs::verify_private_dir(destination.parent().unwrap()).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn operator_refuses_linked_database_and_sidecars() {
        use std::os::unix::fs::symlink;
        let (directory, path, _writer) = fixture();
        let linked = directory.path().join("linked.db");
        symlink(&path, &linked).unwrap();
        assert!(Database::open_existing_for_operator(&linked, true).is_err());
        let sidecar = directory.path().join("sidecar_target");
        std::fs::write(&sidecar, "fixture").unwrap();
        let candidate = directory.path().join("linked_sidecar.db");
        std::fs::write(&candidate, "fixture").unwrap();
        symlink(&sidecar, directory.path().join("linked_sidecar.db-wal")).unwrap();
        assert!(Database::open_existing_for_operator(&candidate, true).is_err());
    }
}
