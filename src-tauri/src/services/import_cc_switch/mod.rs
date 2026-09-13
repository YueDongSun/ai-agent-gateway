//! Import provider rows from a cc-switch SQLite database.
//!
//! cc-switch (https://github.com/farion1231/cc-switch) stores its
//! provider catalogue in `~/.cc-switch/cc-switch.db` — a SQLite file
//! whose `providers` table is schema-compatible with AAG's (same
//! column order and types, see
//! `~/Documents/.../cc-switch-src/src-tauri/src/database/schema.rs:27`).
//!
//! The importer:
//! 1. Opens the cc-switch DB read-only (`SQLITE_OPEN_READ_ONLY`).
//! 2. SELECTs every row from `providers`.
//! 3. Skips rows whose `(id, app_type)` already exists in AAG
//!    (`INSERT OR IGNORE` semantics) — re-runnable.
//! 4. Returns the list of `(id, app_type, name)` actually imported so
//!    the UI can show a toast / confirmation.
//!
//! Privacy: the DB lives outside the repo and is user-specific, so it's
//! already excluded from version control via `.gitignore`. The import
//! is explicit (one Tauri command invocation), not automatic.

use crate::database::Database;
use crate::error::AppError;
use crate::provider::Provider;
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use std::path::PathBuf;

/// One row of import feedback for the UI.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportedProvider {
    pub id: String,
    pub app_type: String,
    pub name: String,
}

/// Result of an import run.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub source_db: String,
    pub imported: Vec<ImportedProvider>,
    pub skipped_existing: Vec<ImportedProvider>,
    pub total_in_source: usize,
}

/// Resolve the cc-switch DB path under the user's home directory.
/// Honours `AAG_HOME_DIR` (for tests) and `CC_SWITCH_DB_PATH` (for users
/// who installed cc-switch to a non-default location).
pub fn cc_switch_db_path() -> PathBuf {
    if let Ok(custom) = std::env::var("CC_SWITCH_DB_PATH") {
        let trimmed = custom.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    crate::config::get_home_dir().join(".cc-switch").join("cc-switch.db")
}

/// Map cc-switch's `app_type` values onto AAG's. cc-switch uses short
/// hyphenated ids (`claude`, `claude-desktop`, `grokbuild`); AAG uses
/// snake_case (`claude_code`, `claude_desktop`, `grok_build`). Values
/// that already match pass through unchanged; unknown values pass
/// through as-is so the row still lands (switching it may fail with
/// "unknown app_type", but the data isn't lost).
pub fn map_app_type(cc_switch_app_type: &str) -> String {
    match cc_switch_app_type {
        "claude" => "claude_code".into(),
        "claude-desktop" => "claude_desktop".into(),
        "grokbuild" => "grok_build".into(),
        other => other.into(),
    }
}

/// One-shot full import: all providers (any app_type) from cc-switch.
/// UI uses this via the "Import from cc-switch" button.
pub fn import_from_db<P: AsRef<std::path::Path>>(
    db: &Database,
    cc_switch_path: P,
) -> Result<ImportReport, AppError> {
    let rows = read_cc_switch_providers(cc_switch_path.as_ref())?;
    insert_aag_providers(db, rows, /*respect_sync_locked=*/ false)
}

/// Auto-sync: only `claude_code` providers (those are the ones used in the
/// failover queue), honouring each row's `sync_locked` bit — manually
/// tuned providers are skipped, not overwritten. Used by `proxy_start`
/// when the user has flipped the auto-sync switch on.
pub fn sync_claude_code_from_cc_switch<P: AsRef<std::path::Path>>(
    db: &Database,
    cc_switch_path: P,
) -> Result<ImportReport, AppError> {
    let all = read_cc_switch_providers(cc_switch_path.as_ref())?;
    let rows: Vec<CcSwitchRow> = all
        .into_iter()
        .filter(|r| r.app_type == "claude")
        .collect();
    insert_aag_providers(db, rows, /*respect_sync_locked=*/ true)
}

/// Read every row from the cc-switch `providers` table. Pure I/O — no
/// AAG DB touched. Returns an empty Vec if the DB doesn't exist.
fn read_cc_switch_providers(path: &std::path::Path) -> Result<Vec<CcSwitchRow>, AppError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| AppError::Database(format!("failed to open cc-switch DB: {e}")))?;

    let exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='providers'",
            [],
            |r| r.get(0),
        )
        .map_err(|e| AppError::Database(format!("cc-switch DB probe failed: {e}")))?;
    if exists == 0 {
        return Err(AppError::Database(format!(
            "cc-switch DB at {} has no `providers` table — wrong file?",
            path.display()
        )));
    }

    let mut stmt = conn
        .prepare(
            "SELECT id, app_type, name, settings_config FROM providers",
        )
        .map_err(|e| AppError::Database(format!("cc-switch DB prepare failed: {e}")))?;
    let rows: Vec<CcSwitchRow> = stmt
        .query_map([], |r| {
            Ok(CcSwitchRow {
                id: r.get(0)?,
                app_type: map_app_type(&r.get::<_, String>(1)?),
                name: r.get(2)?,
                settings_config: r.get(3)?,
            })
        })
        .map_err(|e| AppError::Database(format!("cc-switch DB query failed: {e}")))?
        .filter_map(Result::ok)
        .collect();
    Ok(rows)
}

/// Internal: rows as read from cc-switch's DB. `app_type` already
/// mapped onto AAG's snake_case.
struct CcSwitchRow {
    id: String,
    app_type: String,
    name: String,
    settings_config: String,
}

fn insert_aag_providers(
    db: &Database,
    rows: Vec<CcSwitchRow>,
    respect_sync_locked: bool,
) -> Result<ImportReport, AppError> {
    let source_db = cc_switch_db_path().to_string_lossy().into_owned();
    let total_in_source = rows.len();
    let mut imported: Vec<ImportedProvider> = Vec::new();
    let mut skipped_existing: Vec<ImportedProvider> = Vec::new();

    for row in rows {
        let settings_value: serde_json::Value = serde_json::from_str(&row.settings_config)
            .map_err(|e| AppError::Database(format!(
                "cc-switch provider {}/{}: invalid settings_config JSON: {e}",
                row.id, row.app_type
            )))?;
        let provider = Provider {
            id: row.id.clone(),
            name: row.name.clone(),
            settings_config: settings_value,
            website_url: None,
            category: None,
            created_at: Some(chrono::Utc::now().timestamp()),
            sort_index: None,
            notes: None,
            icon: None,
            icon_color: None,
            meta: Some(crate::provider::ProviderMeta {
                custom_endpoints: Default::default(),
            }),
            in_failover_queue: false,
            sync_locked: false,
        };
        let existing = db.get_provider_by_id(&row.id, &row.app_type).map_err(AppError::from)?;
        match existing {
            Some(p) if respect_sync_locked && p.sync_locked => {
                // Manually locked — leave alone.
            }
            Some(_) => {
                skipped_existing.push(ImportedProvider {
                    id: row.id,
                    app_type: row.app_type,
                    name: row.name,
                });
            }
            None => {
                db.save_provider(&row.app_type, &provider).map_err(AppError::from)?;
                imported.push(ImportedProvider {
                    id: row.id,
                    app_type: row.app_type,
                    name: row.name,
                });
            }
        }
    }

    Ok(ImportReport {
        source_db,
        imported,
        skipped_existing,
        total_in_source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::Database;
    use rusqlite::Connection;
    use std::io::Write;

    fn write_fake_cc_switch_db(path: &std::path::Path) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE providers (
                id TEXT NOT NULL,
                app_type TEXT NOT NULL,
                name TEXT NOT NULL,
                settings_config TEXT NOT NULL,
                website_url TEXT,
                category TEXT,
                created_at INTEGER,
                sort_index INTEGER,
                notes TEXT,
                icon TEXT,
                icon_color TEXT,
                meta TEXT NOT NULL DEFAULT '{}',
                is_current BOOLEAN NOT NULL DEFAULT 0,
                in_failover_queue BOOLEAN NOT NULL DEFAULT 0,
                PRIMARY KEY (id, app_type)
            )",
        )
        .unwrap();
        // Two providers: one will be skipped (claude_code already in AAG),
        // one will be imported.
        conn.execute(
            "INSERT INTO providers (id, app_type, name, settings_config, is_current, in_failover_queue) \
             VALUES ('existing-claude', 'claude_code', 'Pre-existing', '{\"env\":{}}', 0, 0)",
            [],
        )
        .unwrap();
        // Use cc-switch's native app_type ids ("claude" not "claude_code")
        // so the mapping code path is exercised.
        conn.execute(
            "INSERT INTO providers (id, app_type, name, settings_config, is_current, in_failover_queue) \
             VALUES ('imported-codex', 'codex', 'Imported Codex', '{\"auth\":{},\"config\":\"model=\\\"gpt-5\\\"\"}', 1, 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO providers (id, app_type, name, settings_config, is_current, in_failover_queue) \
             VALUES ('imported-grok', 'grokbuild', 'Imported Grok', '{}', 0, 1)",
            [],
        )
        .unwrap();
    }

    #[test]
    fn missing_db_is_noop() {        let aag = Database::memory().unwrap();
        let report = import_from_db(
            &aag,
            std::path::Path::new("/tmp/definitely-does-not-exist-aag-test.db"),
        )
        .unwrap();
        assert_eq!(report.total_in_source, 0);
        assert!(report.imported.is_empty());
        assert!(report.skipped_existing.is_empty());
    }

    #[test]
    fn missing_providers_table_errors() {
        let aag = Database::memory().unwrap();
        let tmp = std::env::temp_dir().join(format!(
            "aag_ccswitch_noprov_{}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&tmp);
        // Create a SQLite file with only a different table.
        let conn = Connection::open(&tmp).unwrap();
        conn.execute_batch("CREATE TABLE something_else (x INTEGER)")
            .unwrap();
        drop(conn);

        let err = import_from_db(&aag, &tmp).unwrap_err();
        assert!(
            err.to_string().contains("no `providers` table"),
            "expected schema probe error, got: {err}"
        );
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn import_inserts_missing_and_skips_existing() {
        let aag = Database::memory().unwrap();
        // Seed AAG with one provider that overlaps the source.
        aag.save_provider(
            "claude_code",
            &Provider::new(
                "existing-claude".into(),
                "Pre-existing".into(),
                serde_json::json!({"env": {}}),
            ),
        )
        .unwrap();

        let tmp = std::env::temp_dir().join(format!(
            "aag_ccswitch_real_{}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&tmp);
        write_fake_cc_switch_db(&tmp);

        let report = import_from_db(&aag, &tmp).unwrap();
        assert_eq!(report.total_in_source, 3);
        // existing-claude was already in AAG → skipped.
        assert_eq!(report.skipped_existing.len(), 1);
        assert_eq!(report.skipped_existing[0].id, "existing-claude");
        // imported-codex + imported-grok → imported.
        assert_eq!(report.imported.len(), 2);
        let ids: Vec<&str> = report.imported.iter().map(|r| r.id.as_str()).collect();
        assert!(ids.contains(&"imported-codex"));
        assert!(ids.contains(&"imported-grok"));

        // Verify AAG now has the rows.
        let codex = aag
            .get_provider_by_id("imported-codex", "codex")
            .unwrap()
            .expect("imported-codex should exist");
        assert_eq!(codex.name, "Imported Codex");
        assert_eq!(
            codex.settings_config["config"].as_str(),
            Some("model=\"gpt-5\"")
        );

        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn reimporting_is_idempotent() {
        let aag = Database::memory().unwrap();
        let tmp = std::env::temp_dir().join(format!(
            "aag_ccswitch_idem_{}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&tmp);
        write_fake_cc_switch_db(&tmp);

        let r1 = import_from_db(&aag, &tmp).unwrap();
        let r2 = import_from_db(&aag, &tmp).unwrap();
        assert_eq!(r1.imported.len(), 3);
        assert_eq!(r2.imported.len(), 0);
        assert_eq!(r2.skipped_existing.len(), 3);

        let _ = std::fs::remove_file(&tmp);
    }

    /// End-to-end test against the user's real cc-switch SQLite DB.
    /// Gated behind `AAG_TEST_CCSWITCH_DB` env var so CI / other devs
    /// don't accidentally hit a 27 MB personal DB. Run with:
    ///
    ///   AAG_TEST_CCSWITCH_DB="/c/Users/ydsun/.cc-switch/cc-switch.db" \
    ///     cargo test --lib -- --test-threads=1 real_cc_switch
    ///
    /// Prints the full report so the user can sanity-check the import
    /// against their actual provider catalogue.
    #[test]
    fn real_cc_switch_db_smoke_test() {
        let Ok(path) = std::env::var("AAG_TEST_CCSWITCH_DB") else {
            eprintln!(
                "real_cc_switch_db_smoke_test skipped: set \
                 AAG_TEST_CCSWITCH_DB=/path/to/cc-switch.db to enable"
            );
            return;
        };
        let path = std::path::PathBuf::from(&path);
        if !path.exists() {
            panic!("AAG_TEST_CCSWITCH_DB points at non-existent file: {}", path.display());
        }
        let aag = Database::memory().unwrap();
        let report = import_from_db(&aag, &path).expect("import_from_db failed");

        eprintln!(
            "\n=== real_cc_switch_db_smoke_test ===\n\
             source: {}\n\
             total in source: {}\n\
             imported:        {}\n\
             skipped:         {}",
            report.source_db,
            report.total_in_source,
            report.imported.len(),
            report.skipped_existing.len(),
        );
        eprintln!("\n--- imported ---");
        for r in &report.imported {
            eprintln!("  + [{:>14}] {:<32} ({})", r.app_type, r.name, r.id);
        }
        eprintln!("\n--- skipped (already in AAG) ---");
        for r in &report.skipped_existing {
            eprintln!("  = [{:>14}] {:<32} ({})", r.app_type, r.name, r.id);
        }
    }
}
