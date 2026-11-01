//! Backups DAO (backups table).
//!
//! Stores serialized snapshots of user-data tables so the UI can offer
//! "Create backup / Restore from backup" actions. The `data` column holds a
//! JSON document (see `BackupPayload`) with rows for four tables: providers,
//! proxy_providers, routes, and proxy_config. MCP servers, prompts, and
//! skills are not captured yet; ephemeral tables (proxy_request_logs,
//! stream_check_logs, provider_health, usage_daily_rollups) are intentionally
//! excluded.

use crate::database::{lock_conn, Database, SCHEMA_VERSION};
use crate::error::AppError;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

/// Metadata exposed to the UI for a single backup.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupEntry {
    pub id: String,
    pub label: String,
    pub size_bytes: u64,
    /// ISO 8601 timestamp.
    pub created_at: String,
}

/// Internal: a single table's rows included in a backup. Each table is stored
/// as an array of JSON objects (one object per row).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupPayload {
    pub version: u32,
    pub schema_version: i32,
    pub created_at: i64,
    pub providers: Vec<serde_json::Value>,
    pub proxy_providers: Vec<serde_json::Value>,
    pub routes: Vec<serde_json::Value>,
    pub proxy_config: Vec<serde_json::Value>,
}

/// Bound the number of plaintext credential snapshots retained in SQLite.
const DEFAULT_BACKUP_RETENTION: i64 = 10;

/// Convert a `rusqlite::types::Value` to `serde_json::Value` for JSON
/// serialization in backups.
fn rusqlite_value_to_json(v: &rusqlite::types::Value) -> serde_json::Value {
    use rusqlite::types::Value;
    match v {
        Value::Null => serde_json::Value::Null,
        Value::Integer(i) => serde_json::Value::Number((*i).into()),
        Value::Real(f) => serde_json::Number::from_f64(*f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        Value::Text(s) => serde_json::Value::String(s.clone()),
        Value::Blob(b) => serde_json::Value::String(format!("<blob: {} bytes>", b.len())),
    }
}

impl Database {
    /// Create a backup. Returns the BackupEntry with id + size.
    /// `label` is an optional human label; if empty, a timestamp is used.
    pub fn create_backup(&self, label: Option<String>) -> Result<BackupEntry, AppError> {
        let conn = lock_conn!(self.conn);
        let now = chrono::Utc::now().timestamp_millis();
        let id = format!("backup-{now}");
        let label =
            label.unwrap_or_else(|| chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string());
        let payload = Self::dump_all_to_payload(&conn)?;
        let json = serde_json::to_vec(&payload)?;
        let size_bytes = json.len() as u64;
        let transaction = conn.unchecked_transaction()?;
        transaction.execute(
            "INSERT INTO backups (id, label, created_at, size_bytes, data)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, label, now, size_bytes as i64, json],
        )?;
        transaction.execute(
            "DELETE FROM backups
             WHERE id IN (
                 SELECT id FROM backups
                 ORDER BY created_at DESC, id DESC
                 LIMIT -1 OFFSET ?1
             )",
            params![DEFAULT_BACKUP_RETENTION],
        )?;
        transaction.commit()?;
        Ok(BackupEntry {
            id,
            label,
            size_bytes,
            created_at: chrono::DateTime::<chrono::Utc>::from_timestamp(now, 0)
                .map(|dt| dt.to_rfc3339())
                .unwrap_or_default(),
        })
    }

    /// List all backups, newest first.
    #[allow(dead_code)]
    pub fn list_backups(&self) -> Result<Vec<BackupEntry>, AppError> {
        let conn = lock_conn!(self.conn);
        let mut stmt = conn.prepare(
            "SELECT id, label, created_at, size_bytes FROM backups ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            let created_at: i64 = row.get(2)?;
            let ts = chrono::DateTime::<chrono::Utc>::from_timestamp(created_at, 0)
                .map(|dt| dt.to_rfc3339())
                .unwrap_or_default();
            Ok(BackupEntry {
                id: row.get(0)?,
                label: row.get(1)?,
                size_bytes: row.get::<_, i64>(3)? as u64,
                created_at: ts,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Delete a single backup by id.
    #[allow(dead_code)]
    pub fn delete_backup(&self, id: &str) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute("DELETE FROM backups WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Auto-rotate: keep only the `keep` most recent backups. Returns the ids
    /// of backups that were deleted. Runs in one transaction so a mid-rotate
    /// failure cannot leave a partially rotated set.
    #[allow(dead_code)]
    pub fn cleanup_rotated(&self, keep: u32) -> Result<Vec<String>, AppError> {
        let conn = lock_conn!(self.conn);
        let mut stmt =
            conn.prepare("SELECT id FROM backups ORDER BY created_at DESC LIMIT -1 OFFSET ?1")?;
        let to_delete: Vec<String> = stmt
            .query_map(params![keep as i64], |row| row.get::<_, String>(0))?
            .filter_map(|r| r.ok())
            .collect();
        drop(stmt);
        let transaction = conn.unchecked_transaction()?;
        for id in &to_delete {
            transaction.execute("DELETE FROM backups WHERE id = ?1", params![id])?;
        }
        transaction.commit()?;
        Ok(to_delete)
    }

    /// Dump all included tables to a BackupPayload.
    fn dump_all_to_payload(conn: &Connection) -> Result<BackupPayload, AppError> {
        let dump_table = |table: &str| -> Result<Vec<serde_json::Value>, AppError> {
            let sql = format!("SELECT * FROM {table}");
            let mut stmt = conn.prepare(&sql)?;
            let cols = stmt.column_count();
            let col_names: Vec<String> = (0..cols)
                .map(|i| stmt.column_name(i).unwrap_or("?").to_string())
                .collect();
            let rows = stmt.query_map([], |row| {
                let mut obj = serde_json::Map::new();
                for (i, name) in col_names.iter().enumerate() {
                    let v = row.get::<_, rusqlite::types::Value>(i)?;
                    let json_val = rusqlite_value_to_json(&v);
                    obj.insert(name.clone(), json_val);
                }
                Ok(serde_json::Value::Object(obj))
            })?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r?);
            }
            Ok(out)
        };
        Ok(BackupPayload {
            version: 1,
            schema_version: SCHEMA_VERSION,
            created_at: chrono::Utc::now().timestamp(),
            providers: dump_table("providers")?,
            proxy_providers: dump_table("proxy_providers")?,
            routes: dump_table("routes")?,
            proxy_config: dump_table("proxy_config")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_db() -> Database {
        Database::memory().expect("memory db")
    }

    #[test]
    fn create_and_list_backup() {
        let db = make_db();
        let entry = db.create_backup(Some("test backup".into())).unwrap();
        assert_eq!(entry.label, "test backup");
        assert!(entry.size_bytes > 0);
        let list = db.list_backups().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, entry.id);
    }

    #[test]
    fn cleanup_rotated_keeps_recent() {
        let db = make_db();
        for i in 0..5 {
            db.create_backup(Some(format!("backup {i}"))).unwrap();
            // ensure distinct timestamps
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let deleted = db.cleanup_rotated(2).unwrap();
        assert_eq!(deleted.len(), 3);
        assert_eq!(db.list_backups().unwrap().len(), 2);
    }

    #[test]
    fn delete_backup() {
        let db = make_db();
        let entry = db.create_backup(None).unwrap();
        db.delete_backup(&entry.id).unwrap();
        assert!(db.list_backups().unwrap().is_empty());
    }

    #[test]
    fn create_backup_automatically_bounds_plaintext_retention() {
        let db = make_db();
        for i in 0..(DEFAULT_BACKUP_RETENTION + 2) {
            db.create_backup(Some(format!("bounded backup {i}")))
                .unwrap();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let backups = db.list_backups().unwrap();
        assert_eq!(backups.len(), DEFAULT_BACKUP_RETENTION as usize);
        assert_eq!(backups[0].label, "bounded backup 11");
        assert_eq!(backups.last().unwrap().label, "bounded backup 2");
    }
}
