//! Cloud sync services (Phase 8).
//!
//! - WebDAV: simple PUT/GET of `aag.db` (Step 3).
//! - S3:     AWS SigV4 signed PUT/GET/HEAD (Step 4).

pub mod s3;
pub mod webdav;

use crate::database::dao::sync_config::SyncTarget;
use crate::database::Database;

/// Record a sync outcome for the UI status section. Shared by both
/// transports so the failure log and sentinel error stay identical.
/// Stamp failures never mask the transport result. Private to this
/// module tree; the transport children reach it via `super::`.
fn stamp_sync_outcome<T, E>(
    db: &Database,
    target: SyncTarget,
    transport: &str,
    result: &Result<T, E>,
) {
    let err_msg = result.as_ref().err().map(|_| "sync_operation_failed");
    if db.record_sync_result(target, err_msg).is_err() {
        log::warn!("{transport}: sync result stamp failed code=database_error");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::dao::sync_config::SyncConfig;

    fn db_with_target() -> Database {
        let db = Database::memory().expect("memory db");
        db.save_sync_config(&SyncConfig {
            provider: "s3".into(),
            enabled: true,
            endpoint: "minio.local:9000".into(),
            bucket: None,
            remote_path: None,
            access_key: String::new(),
            secret_key: String::new(),
            region: None,
            last_sync_at: None,
            last_error: None,
        })
        .expect("seed sync config");
        db
    }

    #[test]
    fn failed_transports_stamp_only_the_fixed_sentinel() {
        let db = db_with_target();
        let result: Result<(), String> = Err("https://secret.example/inner detail".into());
        stamp_sync_outcome(&db, SyncTarget::S3, "s3", &result);
        let row = db.get_sync_config(SyncTarget::S3).unwrap().unwrap();
        assert_eq!(row.last_error.as_deref(), Some("sync_operation_failed"));
        assert!(row.last_sync_at.is_some());
    }

    #[test]
    fn successful_transports_clear_the_error_row() {
        let db = db_with_target();
        db.record_sync_result(SyncTarget::S3, Some("sync_operation_failed"))
            .unwrap();
        let result: Result<u8, String> = Ok(7);
        stamp_sync_outcome(&db, SyncTarget::S3, "webdav", &result);
        let row = db.get_sync_config(SyncTarget::S3).unwrap().unwrap();
        assert!(row.last_error.is_none());
    }

    #[test]
    fn a_missing_config_row_never_panics_or_masks_the_result() {
        let db = Database::memory().expect("memory db");
        let result: Result<(), String> = Err("late failure".into());
        // No row exists: the stamp fails, is only logged, and the transport
        // result stays untouched for the caller.
        stamp_sync_outcome(&db, SyncTarget::Webdav, "webdav", &result);
        assert!(db.get_sync_config(SyncTarget::Webdav).unwrap().is_none());
    }
}
