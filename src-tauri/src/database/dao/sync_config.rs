//! Cloud sync config DAO (sync_config table).
//!
//! Phase 8 Step 1: table + DAO only. The actual S3/WebDAV network sync
//! (upload/download the DB snapshot) lands in a later step.
//!
//! Unified schema covers both S3 and WebDAV (cc-switch keeps them as separate
//! `S3Settings` / `WebDAVSettings` structs in `services/s3.rs:23` and
//! `services/webdav.rs`). Field mapping:
//! - S3:     `access_key` = access_key_id, `secret_key` = secret_access_key,
//!           `bucket` = bucket, `region` = region, `endpoint` = custom host.
//! - WebDAV: `access_key` = username, `secret_key` = password,
//!           `remote_path` = remote directory, `endpoint` = full URL.

use crate::database::{lock_conn, Database};
use crate::error::AppError;
use rusqlite::params;
use serde::{Deserialize, Serialize};

/// Which cloud sync backend a config row targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SyncTarget {
    S3,
    Webdav,
}

impl SyncTarget {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::S3 => "s3",
            Self::Webdav => "webdav",
        }
    }

    pub fn from_str(s: &str) -> Result<Self, AppError> {
        match s {
            "s3" => Ok(Self::S3),
            "webdav" => Ok(Self::Webdav),
            other => Err(AppError::InvalidInput(format!(
                "unknown sync provider: \"{other}\" (expected \"s3\" or \"webdav\")"
            ))),
        }
    }
}

/// A cloud sync configuration row. One row per `SyncTarget`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncConfig {
    pub provider: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub endpoint: String,
    /// S3 bucket name (NULL for WebDAV).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bucket: Option<String>,
    /// WebDAV remote directory (NULL for S3).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_path: Option<String>,
    /// S3 access_key_id OR WebDAV username.
    #[serde(default)]
    pub access_key: String,
    /// S3 secret_access_key OR WebDAV password.
    #[serde(default)]
    pub secret_key: String,
    /// S3 region (NULL for WebDAV).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_sync_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

impl Database {
    /// List all sync configs (at most 2 rows: s3, webdav).
    #[allow(dead_code)]
    pub fn get_all_sync_configs(&self) -> Result<Vec<SyncConfig>, AppError> {
        let conn = lock_conn!(self.conn);
        let mut stmt = conn.prepare(
            "SELECT provider, enabled, endpoint, bucket, remote_path, access_key,
                    secret_key, region, last_sync_at, last_error
             FROM sync_config ORDER BY provider ASC",
        )?;
        let rows = stmt.query_map([], row_to_sync_config)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Fetch a single sync config by provider ("s3" or "webdav").
    #[allow(dead_code)]
    pub fn get_sync_config(&self, target: SyncTarget) -> Result<Option<SyncConfig>, AppError> {
        let conn = lock_conn!(self.conn);
        let result = conn.query_row(
            "SELECT provider, enabled, endpoint, bucket, remote_path, access_key,
                    secret_key, region, last_sync_at, last_error
             FROM sync_config WHERE provider = ?1",
            params![target.as_str()],
            row_to_sync_config,
        );
        match result {
            Ok(c) => Ok(Some(c)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(AppError::Database(e.to_string())),
        }
    }

    /// Upsert a sync config.
    #[allow(dead_code)]
    pub fn save_sync_config(&self, cfg: &SyncConfig) -> Result<(), AppError> {
        // Validate provider string is a known target.
        let _target = SyncTarget::from_str(&cfg.provider)?;
        let conn = lock_conn!(self.conn);
        conn.execute(
            "INSERT INTO sync_config (
                provider, enabled, endpoint, bucket, remote_path, access_key,
                secret_key, region, last_sync_at, last_error
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            ON CONFLICT(provider) DO UPDATE SET
                enabled=excluded.enabled, endpoint=excluded.endpoint,
                bucket=excluded.bucket, remote_path=excluded.remote_path,
                access_key=excluded.access_key, secret_key=excluded.secret_key,
                region=excluded.region, last_sync_at=excluded.last_sync_at,
                last_error=excluded.last_error",
            params![
                cfg.provider,
                cfg.enabled,
                cfg.endpoint,
                cfg.bucket,
                cfg.remote_path,
                cfg.access_key,
                cfg.secret_key,
                cfg.region,
                cfg.last_sync_at,
                cfg.last_error,
            ],
        )?;
        Ok(())
    }

    /// Delete a sync config by provider.
    #[allow(dead_code)]
    pub fn delete_sync_config(&self, target: SyncTarget) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute(
            "DELETE FROM sync_config WHERE provider = ?1",
            params![target.as_str()],
        )?;
        Ok(())
    }

    /// Record the result of a sync attempt: stamp `last_sync_at` to now and
    /// store the error (or clear it on success). Called by the sync service
    /// (Phase 8 later step) after each run.
    #[allow(dead_code)]
    pub fn record_sync_result(
        &self,
        target: SyncTarget,
        error: Option<&str>,
    ) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        let now = chrono::Utc::now().timestamp();
        let updated = conn.execute(
            "UPDATE sync_config SET last_sync_at = ?1, last_error = ?2 WHERE provider = ?3",
            params![now, error, target.as_str()],
        )?;
        if updated == 0 {
            return Err(AppError::InvalidInput(format!(
                "sync config for \"{}\" not found",
                target.as_str()
            )));
        }
        Ok(())
    }
}

/// Row mapper shared by `get_all_sync_configs` and `get_sync_config`.
fn row_to_sync_config(row: &rusqlite::Row<'_>) -> rusqlite::Result<SyncConfig> {
    Ok(SyncConfig {
        provider: row.get(0)?,
        enabled: row.get(1)?,
        endpoint: row.get(2)?,
        bucket: row.get(3)?,
        remote_path: row.get(4)?,
        access_key: row.get(5)?,
        secret_key: row.get(6)?,
        region: row.get(7)?,
        last_sync_at: row.get(8)?,
        last_error: row.get(9)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_db() -> Database {
        Database::memory().expect("memory db")
    }

    fn s3_cfg() -> SyncConfig {
        SyncConfig {
            provider: "s3".into(),
            enabled: true,
            endpoint: "minio.local:9000".into(),
            bucket: Some("aag-backups".into()),
            remote_path: None,
            access_key: "AKID".into(),
            secret_key: "SECRET".into(),
            region: Some("us-east-1".into()),
            last_sync_at: None,
            last_error: None,
        }
    }

    fn webdav_cfg() -> SyncConfig {
        SyncConfig {
            provider: "webdav".into(),
            enabled: false,
            endpoint: "https://dav.example.com/dav/".into(),
            bucket: None,
            remote_path: Some("/aag/".into()),
            access_key: "user".into(),
            secret_key: "pass".into(),
            region: None,
            last_sync_at: None,
            last_error: None,
        }
    }

    #[test]
    fn sync_target_roundtrip() {
        assert_eq!(SyncTarget::from_str("s3").unwrap(), SyncTarget::S3);
        assert_eq!(SyncTarget::from_str("webdav").unwrap(), SyncTarget::Webdav);
        assert_eq!(SyncTarget::S3.as_str(), "s3");
        assert_eq!(SyncTarget::Webdav.as_str(), "webdav");
        assert!(SyncTarget::from_str("ftp").is_err());
    }

    #[test]
    fn save_get_delete_sync_config() {
        let db = make_db();
        db.save_sync_config(&s3_cfg()).unwrap();
        db.save_sync_config(&webdav_cfg()).unwrap();

        let s3 = db.get_sync_config(SyncTarget::S3).unwrap().unwrap();
        assert_eq!(s3.bucket.as_deref(), Some("aag-backups"));
        assert_eq!(s3.region.as_deref(), Some("us-east-1"));
        assert!(s3.remote_path.is_none());

        let wd = db.get_sync_config(SyncTarget::Webdav).unwrap().unwrap();
        assert_eq!(wd.remote_path.as_deref(), Some("/aag/"));
        assert!(wd.bucket.is_none());
        assert!(!wd.enabled);

        let all = db.get_all_sync_configs().unwrap();
        assert_eq!(all.len(), 2); // s3 + webdav

        db.delete_sync_config(SyncTarget::S3).unwrap();
        assert!(db.get_sync_config(SyncTarget::S3).unwrap().is_none());
        assert_eq!(db.get_all_sync_configs().unwrap().len(), 1);
    }

    #[test]
    fn save_rejects_unknown_provider() {
        let db = make_db();
        let mut bad = s3_cfg();
        bad.provider = "ftp".into();
        assert!(db.save_sync_config(&bad).is_err());
    }

    #[test]
    fn record_sync_result_stamps_and_clears_error() {
        let db = make_db();
        db.save_sync_config(&s3_cfg()).unwrap();

        // Failed sync: stamp error
        db.record_sync_result(SyncTarget::S3, Some("connection refused"))
            .unwrap();
        let s3 = db.get_sync_config(SyncTarget::S3).unwrap().unwrap();
        assert!(s3.last_sync_at.is_some());
        assert_eq!(s3.last_error.as_deref(), Some("connection refused"));

        // Successful sync: clear error
        db.record_sync_result(SyncTarget::S3, None).unwrap();
        let s3 = db.get_sync_config(SyncTarget::S3).unwrap().unwrap();
        assert!(s3.last_error.is_none());

        // Unknown target -> error
        assert!(db.record_sync_result(SyncTarget::Webdav, None).is_err());
    }
}