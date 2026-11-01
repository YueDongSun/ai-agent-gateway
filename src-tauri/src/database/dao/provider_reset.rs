//! Explicit provider-only reset with a complete private recovery snapshot.
//!
//! This is never called by startup. The caller owns stopping live writers and
//! refreshing the proxy after a successful reset; client files are untouched.

use crate::config::private_fs;
use crate::database::{lock_conn, Database};
use crate::error::AppError;
use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const KEY_PROVIDER_SEED_DISABLED: &str = "provider_seed_disabled";

// Child tables precede parents. The public catalog cache, usage, sessions,
// MCP, skills, proxy listener/auth, and all unrelated settings are retained.
const PROVIDER_TABLES: &[&str] = &[
    "supplier_runtime_projections",
    "supplier_models",
    "supplier_endpoints",
    "suppliers",
    "provider_endpoints",
    "providers",
    "routes",
    "proxy_providers",
    "vendors",
];

const PROVIDER_STATE_KEYS: &[&str] = &[
    "unified_default_model",
    "proxy_pinned_provider",
    "model_discovery_last_run_epoch",
    "model_discovery_last_report_json",
    "model_discovery_pending_restart",
    "supplier_import_setup_pending",
];

/// Local recovery metadata only. Do not log or send the backup path externally.
#[derive(Debug)]
pub struct ProviderResetResult {
    pub backup_path: PathBuf,
    pub removed_rows: BTreeMap<String, u64>,
    pub generation: u64,
}

impl Database {
    /// Clear provider configuration only after a full, verified SQLite backup.
    /// The backup destination must be a new absolute path inside a private
    /// directory. Existing files are never overwritten. Failure during deletion
    /// rolls back all changes; the backup remains available for recovery.
    pub fn reset_provider_configuration(
        &self,
        backup_path: &Path,
    ) -> Result<ProviderResetResult, AppError> {
        self.reset_provider_configuration_with(backup_path, None, |_| Ok(()))
            .map(|(receipt, ())| receipt)
    }

    /// Private binding for an explicit reset preview. Values are hashed only
    /// inside the owner; credentials and this intermediate revision never leave IPC.
    pub(crate) fn provider_reset_revision(&self) -> Result<String, AppError> {
        let conn = lock_conn!(self.conn);
        let transaction = conn.unchecked_transaction()?;
        let revision = reset_revision_on_conn(&transaction)?;
        transaction.commit()?;
        Ok(revision)
    }

    /// Shared atomic boundary: insertion failure rolls back the reset as well.
    pub(crate) fn reset_provider_configuration_with<T>(
        &self,
        backup_path: &Path,
        expected_revision: Option<&str>,
        rebuild: impl FnOnce(&Transaction<'_>) -> Result<T, AppError>,
    ) -> Result<(ProviderResetResult, T), AppError> {
        if !backup_path.is_absolute() || backup_path.try_exists()? {
            return Err(AppError::InvalidInput(
                "provider_reset_backup_path_invalid".into(),
            ));
        }
        let mut conn = lock_conn!(self.conn);
        if let Some(expected) = expected_revision {
            if reset_revision_on_conn(&conn)? != expected {
                return Err(AppError::Message("provider_reset_stale_plan".into()));
            }
        }
        let data_version: i64 = conn.pragma_query_value(None, "data_version", |row| row.get(0))?;
        private_fs::ensure_private_file(backup_path)?;
        conn.execute("VACUUM INTO ?1", [backup_path.to_string_lossy().as_ref()])?;
        private_fs::secure_existing_file(backup_path)?;
        verify_snapshot(backup_path)?;

        // The mutex excludes this process's writers. Detect any other process
        // that committed during backup; begin IMMEDIATE then excludes later
        // external writes until the reset commits. Never reset newer state
        // than the retained recovery snapshot.
        let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current_version: i64 =
            transaction.pragma_query_value(None, "data_version", |row| row.get(0))?;
        if current_version != data_version {
            return Err(AppError::Message("provider_reset_concurrent_change".into()));
        }
        if let Some(expected) = expected_revision {
            if reset_revision_on_conn(&transaction)? != expected {
                return Err(AppError::Message("provider_reset_stale_plan".into()));
            }
        }
        let (removed_rows, generation) = reset_on_transaction(&transaction)?;
        let rebuilt = rebuild(&transaction)?;
        transaction.commit()?;
        Ok((
            ProviderResetResult {
                backup_path: backup_path.to_path_buf(),
                removed_rows,
                generation,
            },
            rebuilt,
        ))
    }
}

fn reset_on_transaction(
    transaction: &Transaction<'_>,
) -> Result<(BTreeMap<String, u64>, u64), AppError> {
    let mut removed_rows = BTreeMap::new();
    for table in PROVIDER_TABLES {
        let count = transaction.execute(&format!("DELETE FROM {table}"), [])?;
        removed_rows.insert((*table).to_string(), count as u64);
    }
    for key in PROVIDER_STATE_KEYS {
        transaction.execute("DELETE FROM app_meta WHERE key = ?1", [key])?;
    }
    transaction.execute(
        "DELETE FROM app_meta WHERE key GLOB 'supplier_discovery_health:*'",
        [],
    )?;
    transaction.execute(
        "INSERT INTO app_meta (key, value) VALUES ('sync_with_cc_switch_enabled', '0')
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [],
    )?;
    transaction.execute(
        "INSERT INTO app_meta (key, value) VALUES (?1, '1')
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [KEY_PROVIDER_SEED_DISABLED],
    )?;
    transaction.execute(
        "UPDATE supplier_config_meta SET generation = generation + 1 WHERE id = 1",
        [],
    )?;
    let generation = transaction.query_row(
        "SELECT generation FROM supplier_config_meta WHERE id = 1",
        [],
        |row| row.get::<_, u64>(0),
    )?;
    Ok((removed_rows, generation))
}

fn reset_revision_on_conn(conn: &Connection) -> Result<String, AppError> {
    let mut digest = Sha256::new();
    for table in PROVIDER_TABLES
        .iter()
        .copied()
        .chain(["supplier_config_meta", "proxy_config"])
    {
        digest.update(table.as_bytes());
        let mut statement = conn.prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))?;
        let columns = statement.column_count();
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            digest.update([255]);
            for index in 0..columns {
                use rusqlite::types::ValueRef;
                match row.get_ref(index)? {
                    ValueRef::Null => digest.update([0]),
                    ValueRef::Integer(value) => {
                        digest.update([1]);
                        digest.update(value.to_be_bytes());
                    }
                    ValueRef::Real(value) => {
                        digest.update([2]);
                        digest.update(value.to_bits().to_be_bytes());
                    }
                    ValueRef::Text(value) | ValueRef::Blob(value) => {
                        digest.update([3]);
                        digest.update((value.len() as u64).to_be_bytes());
                        digest.update(value);
                    }
                }
            }
        }
    }
    let mut statement = conn.prepare("SELECT key,value FROM app_meta WHERE key IN ('unified_default_model','proxy_pinned_provider','model_discovery_last_run_epoch','model_discovery_last_report_json','model_discovery_pending_restart','supplier_import_setup_pending','sync_with_cc_switch_enabled','provider_seed_disabled') OR key GLOB 'supplier_discovery_health:*' ORDER BY key")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        for index in 0..2 {
            let value: String = row.get(index)?;
            digest.update((value.len() as u64).to_be_bytes());
            digest.update(value.as_bytes());
        }
    }
    Ok(hex::encode(digest.finalize()))
}

fn verify_snapshot(path: &Path) -> Result<(), AppError> {
    let snapshot = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    let integrity: String = snapshot.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
    if integrity != "ok" {
        return Err(AppError::Message("provider_reset_backup_invalid".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::suppliers::{save_supplier, SupplierSaveInput};
    use serde_json::json;

    #[test]
    #[ignore = "clears real provider configuration only with AAG_RESET_PROVIDER_CONFIGURATION=CLEAR_AAG_PROVIDERS"]
    fn reset_existing_provider_configuration_explicitly() {
        assert_eq!(
            std::env::var("AAG_RESET_PROVIDER_CONFIGURATION").as_deref(),
            Ok("CLEAR_AAG_PROVIDERS")
        );
        assert!(std::env::var_os("AAG_HOME_DIR").is_none());
        assert!(std::env::var_os("AAG_CONFIG_DIR").is_none());
        assert!(crate::config::get_db_path().is_file());
        let db = Database::init().expect("existing AAG database");
        let directory = crate::config::get_app_config_dir().join("provider_reset_backups");
        private_fs::prepare_managed_private_dir(&directory).unwrap();
        let backup_path = directory.join(format!(
            "before_provider_reset_{}.db",
            chrono::Utc::now().format("%Y%m%dT%H%M%S%fZ")
        ));
        let receipt = db
            .reset_provider_configuration(&backup_path)
            .expect("backed-up provider reset");
        drop(db);
        let reopened = Database::init().expect("reopen after reset");
        let conn = reopened.conn.lock().unwrap();
        for table in PROVIDER_TABLES {
            let count: u64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(count, 0, "{table} must stay empty after reopen");
        }
        println!(
            "{}",
            json!({"backup": receipt.backup_path,"removed":receipt.removed_rows,"generation":receipt.generation,"empty_after_reopen":true})
        );
    }

    fn populate(db: &Database) -> u64 {
        db.seed_proxy_if_empty().unwrap();
        let input: SupplierSaveInput = serde_json::from_value(json!({
            "expectedGeneration": 0, "name": "Fixture supplier", "baseUrl": "https://example.invalid",
            "apiKey": "fixture_private_key", "endpoints": [{"protocol": "openai-chat", "models": [{"modelId": "fixture_model"}]}]
        })).unwrap();
        let generation = save_supplier(db, input).unwrap().generation;
        db.set_setting("theme", "dark").unwrap();
        db.set_setting("codex_cli_home", "fixture_isolated_home")
            .unwrap();
        db.set_setting("proxy_pinned_provider", "mock-primary")
            .unwrap();
        db.set_setting("sync_with_cc_switch_enabled", "1").unwrap();
        let conn = db.conn.lock().unwrap();
        conn.execute("INSERT INTO providers (id, app_type, name, settings_config) VALUES ('legacy', 'codex', 'Legacy', '{}')", []).unwrap();
        conn.execute("INSERT INTO provider_endpoints (provider_id, app_type, url) VALUES ('legacy', 'codex', 'https://example.invalid')", []).unwrap();
        conn.execute("INSERT INTO vendors (id, name, api_key) VALUES ('vendor', 'Vendor', 'fixture_vendor_key')", []).unwrap();
        conn.execute("INSERT INTO supplier_catalog_cache (source, raw_json, fetched_at) VALUES ('models_dev', '{}', 1)", []).unwrap();
        generation
    }

    #[test]
    fn reset_preserves_unrelated_state_and_full_backup_and_stays_empty_after_reopen() {
        let root =
            private_fs::private_temp_dir(&std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let state_dir = root.path().join("state");
        let db_path = state_dir.join("aag.db");
        let backup_path = state_dir.join("before_reset.db");
        let db = Database::init_at_path(&db_path).unwrap();
        let generation = populate(&db);
        let config = db.get_proxy_config().unwrap();
        let result = db.reset_provider_configuration(&backup_path).unwrap();
        assert_eq!(result.backup_path, backup_path);
        assert_eq!(result.generation, generation + 1);
        assert_eq!(result.removed_rows["suppliers"], 1);
        assert_eq!(result.removed_rows["supplier_models"], 1);
        assert_eq!(result.removed_rows["providers"], 1);
        assert_eq!(db.get_setting("theme").unwrap().as_deref(), Some("dark"));
        assert_eq!(
            db.get_setting("codex_cli_home").unwrap().as_deref(),
            Some("fixture_isolated_home")
        );
        assert_eq!(
            db.get_setting("sync_with_cc_switch_enabled")
                .unwrap()
                .as_deref(),
            Some("0")
        );
        assert!(db.get_setting("proxy_pinned_provider").unwrap().is_none());
        assert_eq!(db.get_proxy_config().unwrap().port, config.port);
        assert!(db
            .get_supplier_catalog_cache("models_dev")
            .unwrap()
            .is_some());
        private_fs::verify_private_file(&backup_path).unwrap();
        let snapshot =
            Connection::open_with_flags(&backup_path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        assert_eq!(
            snapshot
                .query_row("SELECT COUNT(*) FROM suppliers", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            snapshot
                .query_row("SELECT api_key FROM suppliers", [], |row| row
                    .get::<_, String>(0))
                .unwrap(),
            "fixture_private_key"
        );
        assert_eq!(
            snapshot
                .query_row("SELECT COUNT(*) FROM supplier_models", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        drop(snapshot);
        drop(db);
        let reopened = Database::init_at_path(&db_path).unwrap();
        assert!(reopened.get_supplier_snapshot().unwrap().1.is_empty());
        assert!(reopened.get_all_proxy_providers().unwrap().is_empty());
        assert!(reopened.get_all_routes().unwrap().is_empty());
        assert!(reopened.get_all_providers("codex").unwrap().is_empty());
        assert!(reopened.get_all_vendors().unwrap().is_empty());
        let conn = reopened.conn.lock().unwrap();
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
            0
        );
    }

    #[test]
    fn backup_path_failure_keeps_provider_configuration() {
        let db = Database::memory().unwrap();
        populate(&db);
        let root =
            private_fs::private_temp_dir(&std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let existing = root.path().join("sentinel");
        std::fs::write(&existing, b"preserved").unwrap();
        assert!(db.reset_provider_configuration(&existing).is_err());
        assert_eq!(std::fs::read(existing).unwrap(), b"preserved");
        assert_eq!(db.get_supplier_snapshot().unwrap().1.len(), 1);
        assert!(db
            .get_setting(KEY_PROVIDER_SEED_DISABLED)
            .unwrap()
            .is_none());
    }

    #[test]
    fn reset_failure_rolls_back_all_deletes_and_keeps_recovery_snapshot() {
        let db = Database::memory().unwrap();
        let generation = populate(&db);
        db.conn.lock().unwrap().execute_batch(
            "CREATE TRIGGER fail_reset BEFORE DELETE ON suppliers BEGIN SELECT RAISE(ABORT, 'fixture failure'); END;"
        ).unwrap();
        let root =
            private_fs::private_temp_dir(&std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let backup_path = root.path().join("recovery").join("before_reset.db");
        assert!(db.reset_provider_configuration(&backup_path).is_err());
        assert!(backup_path.exists());
        assert_eq!(db.get_supplier_snapshot().unwrap().1.len(), 1);
        assert_eq!(db.supplier_config_generation().unwrap(), generation);
        assert_eq!(
            db.get_setting("sync_with_cc_switch_enabled")
                .unwrap()
                .as_deref(),
            Some("1")
        );
        assert!(db
            .get_setting(KEY_PROVIDER_SEED_DISABLED)
            .unwrap()
            .is_none());
        assert_eq!(
            db.conn
                .lock()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM supplier_models", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
}
