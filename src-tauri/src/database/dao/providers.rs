//! Provider DAO - CRUD operations on the `providers` table.
//!
//! Mirrors cc-switch's `database/dao/providers.rs` (lines 20-180), simplified
//! to skip endpoint joins (endpoints land in a later phase with speedtest).

use std::collections::HashSet;

use crate::database::{lock_conn, to_json_string, Database};
use crate::error::AppError;
use crate::provider::{Provider, ProviderMeta};
use indexmap::IndexMap;
use rusqlite::params;

/// Serialize the fields shared by the provider insert paths: settings JSON,
/// meta JSON, and the `created_at` fallback for new rows.
fn provider_payload(provider: &Provider) -> Result<(String, String, i64), AppError> {
    let settings_config_str = to_json_string(&provider.settings_config)?;
    let meta_str = to_json_string(provider.meta.as_ref().unwrap_or(&ProviderMeta::default()))?;
    let created_at = provider
        .created_at
        .unwrap_or_else(|| chrono::Utc::now().timestamp());
    Ok((settings_config_str, meta_str, created_at))
}

/// Bind the 13-column parameter list shared by `save_provider` and
/// `upsert_provider`.
macro_rules! provider_params {
    ($app_type:expr, $provider:expr, $settings:expr, $meta:expr, $created_at:expr) => {
        params![
            $provider.id,
            $app_type,
            $provider.name,
            $settings,
            $provider.website_url,
            $provider.category,
            $created_at,
            $provider.sort_index,
            $provider.notes,
            $provider.icon,
            $provider.icon_color,
            $meta,
            $provider.in_failover_queue,
        ]
    };
}

/// Column list shared by the provider read queries, so the two SELECT sites
/// and the row mapper cannot drift apart when a column is added.
const PROVIDER_COLUMNS: &str = "id, name, settings_config, website_url, category, created_at,
        sort_index, notes, icon, icon_color, meta, in_failover_queue, sync_locked";

/// Map a row of {@link PROVIDER_COLUMNS} in order into a `Provider`.
fn map_provider_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Provider> {
    let settings_config_str: String = row.get(2)?;
    let meta_str: String = row.get(10)?;
    let settings_config =
        serde_json::from_str(&settings_config_str).unwrap_or(serde_json::Value::Null);
    let meta: ProviderMeta = serde_json::from_str(&meta_str).unwrap_or_default();
    Ok(Provider {
        id: row.get(0)?,
        name: row.get(1)?,
        settings_config,
        website_url: row.get(3)?,
        category: row.get(4)?,
        created_at: row.get(5)?,
        sort_index: row.get(6)?,
        notes: row.get(7)?,
        icon: row.get(8)?,
        icon_color: row.get(9)?,
        meta: Some(meta),
        in_failover_queue: row.get(11)?,
        sync_locked: row.get(12)?,
    })
}

impl Database {
    /// List all providers for an app, ordered by sort_index then created_at.
    /// Returns an `IndexMap` keyed by provider id (insertion-ordered).
    pub fn get_all_providers(
        &self,
        app_type: &str,
    ) -> Result<IndexMap<String, Provider>, AppError> {
        let conn = lock_conn!(self.conn);
        let sql = format!(
            "SELECT {PROVIDER_COLUMNS}
             FROM providers
             WHERE app_type = ?1
             ORDER BY COALESCE(sort_index, 999999), created_at ASC, id ASC"
        );
        let mut stmt = conn.prepare(&sql)?;

        let rows = stmt.query_map(params![app_type], map_provider_row)?;

        let mut providers = IndexMap::new();
        for provider in rows {
            let provider = provider?;
            providers.insert(provider.id.clone(), provider);
        }
        Ok(providers)
    }

    /// Return the id of the current (active) provider for an app, if any.
    pub fn get_current_provider(&self, app_type: &str) -> Result<Option<String>, AppError> {
        let conn = lock_conn!(self.conn);
        let result = conn.query_row(
            "SELECT id FROM providers WHERE app_type = ?1 AND is_current = 1 LIMIT 1",
            params![app_type],
            |row| row.get::<_, String>(0),
        );
        match result {
            Ok(id) => Ok(Some(id)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(AppError::Database(e.to_string())),
        }
    }

    /// Fetch a single provider by `(id, app_type)`.
    pub fn get_provider_by_id(
        &self,
        id: &str,
        app_type: &str,
    ) -> Result<Option<Provider>, AppError> {
        let conn = lock_conn!(self.conn);
        let sql = format!(
            "SELECT {PROVIDER_COLUMNS}
             FROM providers WHERE id = ?1 AND app_type = ?2"
        );
        let result = conn.query_row(&sql, params![id, app_type], map_provider_row);
        match result {
            Ok(provider) => Ok(Some(provider)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(AppError::Database(e.to_string())),
        }
    }

    /// Insert a provider if absent. Existing rows are left untouched, so
    /// re-imports and re-runs never clobber local edits (the cc-switch
    /// importer and DSH seeding rely on this). Use `upsert_provider` to
    /// persist edits to an existing row.
    pub fn save_provider(&self, app_type: &str, provider: &Provider) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        let (settings_config_str, meta_str, created_at) = provider_payload(provider)?;

        conn.execute(
            "INSERT OR IGNORE INTO providers (
                id, app_type, name, settings_config, website_url, category,
                created_at, sort_index, notes, icon, icon_color, meta,
                is_current, in_failover_queue, sync_locked
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 0, ?13, 0)",
            provider_params!(
                app_type,
                provider,
                settings_config_str,
                meta_str,
                created_at
            ),
        )?;
        Ok(())
    }

    /// Upsert a provider: insert when absent, otherwise update the editable
    /// fields while preserving the stored `is_current`, `sync_locked`, and
    /// `created_at` (those bits belong to their dedicated commands). Backs
    /// the `provider_save` Tauri command.
    pub fn upsert_provider(&self, app_type: &str, provider: &Provider) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        let (settings_config_str, meta_str, created_at) = provider_payload(provider)?;

        conn.execute(
            "INSERT INTO providers (
                id, app_type, name, settings_config, website_url, category,
                created_at, sort_index, notes, icon, icon_color, meta,
                is_current, in_failover_queue, sync_locked
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 0, ?13, 0)
            ON CONFLICT(id, app_type) DO UPDATE SET
                name = excluded.name,
                settings_config = excluded.settings_config,
                website_url = excluded.website_url,
                category = excluded.category,
                sort_index = excluded.sort_index,
                notes = excluded.notes,
                icon = excluded.icon,
                icon_color = excluded.icon_color,
                meta = excluded.meta,
                in_failover_queue = excluded.in_failover_queue",
            provider_params!(
                app_type,
                provider,
                settings_config_str,
                meta_str,
                created_at
            ),
        )?;
        Ok(())
    }

    /// Delete a provider. If it was current, the app will have no current
    /// provider until `set_current_provider` is called.
    pub fn delete_provider(&self, id: &str, app_type: &str) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute(
            "DELETE FROM providers WHERE id = ?1 AND app_type = ?2",
            params![id, app_type],
        )?;
        Ok(())
    }

    /// Reorder an app's providers by writing a new `sort_index` for each
    /// supplied id. Supplied ids must be unique and exist for the app. Omitted
    /// rows keep their existing `sort_index` and may interleave under normal
    /// query ordering.
    pub fn reorder_providers(
        &self,
        app_type: &str,
        ordered_ids: &[String],
    ) -> Result<(), AppError> {
        let mut conn = lock_conn!(self.conn);
        let tx = conn.transaction()?;
        let mut seen_ids = HashSet::with_capacity(ordered_ids.len());
        for (idx, id) in ordered_ids.iter().enumerate() {
            if !seen_ids.insert(id) {
                return Err(AppError::InvalidInput(
                    "AAG_PROVIDER_REORDER_DUPLICATE_ID".into(),
                ));
            }
            let updated = tx.execute(
                "UPDATE providers SET sort_index = ?3 WHERE id = ?1 AND app_type = ?2",
                params![id, app_type, (idx as i64) + 1],
            )?;
            if updated == 0 {
                return Err(AppError::InvalidInput(
                    "AAG_PROVIDER_REORDER_UNKNOWN_ID".into(),
                ));
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Toggle the `sync_locked` bit on a single (id, app_type) row.
    pub fn set_sync_locked(&self, id: &str, app_type: &str, locked: bool) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        let updated = conn.execute(
            "UPDATE providers SET sync_locked = ?3 WHERE id = ?1 AND app_type = ?2",
            params![id, app_type, locked as i64],
        )?;
        if updated == 0 {
            return Err(AppError::InvalidInput("AAG_PROVIDER_NOT_FOUND".into()));
        }
        Ok(())
    }

    /// Toggle the `in_failover_queue` bit on a single (id, app_type) row.
    /// Rows with the bit off are excluded from the app's failover chain
    /// (the per-card "join the chain" switch, cc-switch style).
    pub fn set_in_failover_queue(
        &self,
        id: &str,
        app_type: &str,
        in_queue: bool,
    ) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        let updated = conn.execute(
            "UPDATE providers SET in_failover_queue = ?3 WHERE id = ?1 AND app_type = ?2",
            params![id, app_type, in_queue as i64],
        )?;
        if updated == 0 {
            return Err(AppError::InvalidInput("AAG_PROVIDER_NOT_FOUND".into()));
        }
        Ok(())
    }

    /// Set the active provider for an app. Clears `is_current` on all other
    /// providers in the same app_type, sets it on the target.
    pub fn set_current_provider(&self, id: &str, app_type: &str) -> Result<(), AppError> {
        let mut conn = lock_conn!(self.conn);
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE providers SET is_current = 0 WHERE app_type = ?1",
            params![app_type],
        )?;
        let updated = tx.execute(
            "UPDATE providers SET is_current = 1 WHERE id = ?1 AND app_type = ?2",
            params![id, app_type],
        )?;
        if updated == 0 {
            return Err(AppError::InvalidInput("AAG_PROVIDER_NOT_FOUND".into()));
        }
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_db() -> Database {
        Database::memory().expect("memory db")
    }

    #[test]
    fn save_get_delete_provider() {
        let db = make_db();
        let p = Provider::new(
            "test-prov".into(),
            "Test Provider".into(),
            json!({"baseURL": "https://example.com/v1", "apiKey": "sk-x"}),
        );
        db.save_provider("claude_code", &p).unwrap();

        let fetched = db.get_provider_by_id("test-prov", "claude_code").unwrap();
        assert!(fetched.is_some());
        assert_eq!(fetched.unwrap().name, "Test Provider");

        let all = db.get_all_providers("claude_code").unwrap();
        assert_eq!(all.len(), 1);

        db.delete_provider("test-prov", "claude_code").unwrap();
        assert!(db
            .get_provider_by_id("test-prov", "claude_code")
            .unwrap()
            .is_none());
    }

    #[test]
    fn set_current_provider_clears_others() {
        let db = make_db();
        for i in 0..3 {
            let p = Provider::new(format!("p{i}"), format!("P{i}"), json!({}));
            db.save_provider("claude_code", &p).unwrap();
        }
        db.set_current_provider("p1", "claude_code").unwrap();
        assert_eq!(
            db.get_current_provider("claude_code").unwrap(),
            Some("p1".into())
        );

        db.set_current_provider("p2", "claude_code").unwrap();
        assert_eq!(
            db.get_current_provider("claude_code").unwrap(),
            Some("p2".into())
        );
    }

    #[test]
    fn set_current_nonexistent_fails() {
        let db = make_db();
        let err = db.set_current_provider("nope", "claude_code").unwrap_err();
        assert_eq!(err.external_message(), "AAG_PROVIDER_NOT_FOUND");
    }

    #[test]
    fn provider_row_toggles_surface_the_not_found_code_without_leaking_the_id() {
        // The three single-UPDATE toggles share one not-found condition; the
        // detail must stay a stable code because it crosses IPC verbatim.
        let db = make_db();
        for error in [
            db.set_sync_locked("missing", "claude_code", true)
                .unwrap_err(),
            db.set_in_failover_queue("missing", "claude_code", true)
                .unwrap_err(),
            db.set_current_provider("missing", "claude_code")
                .unwrap_err(),
        ] {
            assert_eq!(error.external_message(), "AAG_PROVIDER_NOT_FOUND");
            match error {
                AppError::InvalidInput(detail) => {
                    assert_eq!(detail, "AAG_PROVIDER_NOT_FOUND");
                    assert!(!detail.contains("missing"));
                    assert!(!detail.contains("claude_code"));
                }
                other => panic!("expected invalid-input error, got {other:?}"),
            }
        }
    }

    #[test]
    fn upsert_updates_existing_preserving_state_bits() {
        let db = make_db();
        let p = Provider::new(
            "prov-1".into(),
            "Original".into(),
            json!({"env": {"ANTHROPIC_BASE_URL": "https://a.example"}}),
        );
        db.save_provider("claude_code", &p).unwrap();
        db.set_current_provider("prov-1", "claude_code").unwrap();
        db.set_sync_locked("prov-1", "claude_code", true).unwrap();

        let mut edited = p.clone();
        edited.name = "Renamed".into();
        edited.settings_config = json!({"env": {"ANTHROPIC_BASE_URL": "https://b.example"}});
        edited.in_failover_queue = false;
        db.upsert_provider("claude_code", &edited).unwrap();

        let fetched = db
            .get_provider_by_id("prov-1", "claude_code")
            .unwrap()
            .unwrap();
        assert_eq!(fetched.name, "Renamed");
        assert_eq!(
            fetched.settings_config["env"]["ANTHROPIC_BASE_URL"],
            "https://b.example"
        );
        assert!(!fetched.in_failover_queue);
        // is_current / sync_locked are owned by their dedicated commands and
        // must survive an edit.
        assert!(fetched.sync_locked);
        assert_eq!(
            db.get_current_provider("claude_code").unwrap(),
            Some("prov-1".into())
        );
    }

    #[test]
    fn save_provider_leaves_existing_row_untouched() {
        // Import idempotency: a re-save of a known id must not clobber the
        // stored row (the cc-switch importer depends on this).
        let db = make_db();
        let p = Provider::new("prov-1".into(), "Original".into(), json!({}));
        db.save_provider("claude_code", &p).unwrap();

        let mut stale = p.clone();
        stale.name = "Stale Import Copy".into();
        db.save_provider("claude_code", &stale).unwrap();

        let fetched = db
            .get_provider_by_id("prov-1", "claude_code")
            .unwrap()
            .unwrap();
        assert_eq!(fetched.name, "Original");
    }

    fn save_reorder_provider_fixture(
        db: &Database,
        app_type: &str,
        id: &str,
        sort_index: Option<usize>,
        created_at: i64,
    ) {
        let mut provider = Provider::new(id.to_string(), id.to_string(), json!({}));
        provider.sort_index = sort_index;
        provider.created_at = Some(created_at);
        db.save_provider(app_type, &provider).unwrap();
    }

    fn reorder_provider_sort_indexes(
        db: &Database,
        app_type: &str,
    ) -> Vec<(String, Option<usize>)> {
        db.get_all_providers(app_type)
            .unwrap()
            .into_iter()
            .map(|(id, provider)| (id, provider.sort_index))
            .collect()
    }

    fn assert_reorder_invalid_input(error: AppError, expected_code: &str, supplied_id: &str) {
        assert_eq!(error.external_message(), expected_code);
        match error {
            AppError::InvalidInput(detail) => {
                assert_eq!(detail, expected_code);
                assert!(!detail.contains(supplied_id));
            }
            other => panic!("expected invalid-input error, got {other:?}"),
        }
    }

    #[test]
    fn reorder_providers_partial_list_preserves_omitted_absolute_indexes() {
        let db = make_db();
        let app_type = "claude_code";
        save_reorder_provider_fixture(&db, app_type, "omitted_zero", Some(0), 5);
        save_reorder_provider_fixture(&db, app_type, "omitted_tie_one", Some(1), 30);
        save_reorder_provider_fixture(&db, app_type, "supplied_later", Some(7), 20);
        save_reorder_provider_fixture(&db, app_type, "omitted_tie_two", Some(2), 5);
        save_reorder_provider_fixture(&db, app_type, "supplied_earlier", Some(8), 10);
        save_reorder_provider_fixture(&db, app_type, "omitted_after", Some(3), 0);
        save_reorder_provider_fixture(&db, app_type, "omitted_null", None, -1);

        db.reorder_providers(
            app_type,
            &["supplied_later".into(), "supplied_earlier".into()],
        )
        .unwrap();

        assert_eq!(
            reorder_provider_sort_indexes(&db, app_type),
            vec![
                ("omitted_zero".into(), Some(0)),
                ("supplied_later".into(), Some(1)),
                ("omitted_tie_one".into(), Some(1)),
                ("omitted_tie_two".into(), Some(2)),
                ("supplied_earlier".into(), Some(2)),
                ("omitted_after".into(), Some(3)),
                ("omitted_null".into(), None),
            ]
        );
    }

    #[test]
    fn reorder_providers_empty_list_is_no_op() {
        let db = make_db();
        let app_type = "claude_code";
        save_reorder_provider_fixture(&db, app_type, "zero", Some(0), 1);
        save_reorder_provider_fixture(&db, app_type, "indexed", Some(4), 2);
        save_reorder_provider_fixture(&db, app_type, "unindexed", None, 3);
        let before = reorder_provider_sort_indexes(&db, app_type);

        db.reorder_providers(app_type, &[]).unwrap();

        assert_eq!(reorder_provider_sort_indexes(&db, app_type), before);
    }

    #[test]
    fn reorder_providers_full_list_and_repeated_same_list_succeed() {
        let db = make_db();
        let app_type = "claude_code";
        save_reorder_provider_fixture(&db, app_type, "first", Some(9), 1);
        save_reorder_provider_fixture(&db, app_type, "second", Some(2), 2);
        save_reorder_provider_fixture(&db, app_type, "third", None, 3);
        let ordered_ids = vec![
            "third".to_string(),
            "first".to_string(),
            "second".to_string(),
        ];

        db.reorder_providers(app_type, &ordered_ids).unwrap();
        db.reorder_providers(app_type, &ordered_ids).unwrap();

        assert_eq!(
            reorder_provider_sort_indexes(&db, app_type),
            vec![
                ("third".into(), Some(1)),
                ("first".into(), Some(2)),
                ("second".into(), Some(3)),
            ]
        );
    }

    #[test]
    fn reorder_providers_late_duplicate_returns_code_without_partial_updates() {
        let db = make_db();
        let app_type = "claude_code";
        save_reorder_provider_fixture(&db, app_type, "first", Some(4), 1);
        save_reorder_provider_fixture(&db, app_type, "second", Some(5), 2);
        let before = reorder_provider_sort_indexes(&db, app_type);

        let error = db
            .reorder_providers(app_type, &["first".into(), "second".into(), "first".into()])
            .unwrap_err();

        assert_reorder_invalid_input(error, "AAG_PROVIDER_REORDER_DUPLICATE_ID", "first");
        assert_eq!(reorder_provider_sort_indexes(&db, app_type), before);
    }

    #[test]
    fn reorder_providers_late_unknown_id_returns_code_without_partial_updates() {
        let db = make_db();
        let app_type = "claude_code";
        save_reorder_provider_fixture(&db, app_type, "first", Some(4), 1);
        save_reorder_provider_fixture(&db, app_type, "second", Some(5), 2);
        let before = reorder_provider_sort_indexes(&db, app_type);
        let unknown_id = "unknown_provider_fixture";

        let error = db
            .reorder_providers(app_type, &["first".into(), unknown_id.into()])
            .unwrap_err();

        assert_reorder_invalid_input(error, "AAG_PROVIDER_REORDER_UNKNOWN_ID", unknown_id);
        assert_eq!(reorder_provider_sort_indexes(&db, app_type), before);
    }

    #[test]
    fn reorder_providers_wrong_app_id_returns_code_without_partial_updates() {
        let db = make_db();
        let app_type = "claude_code";
        let foreign_app = "codex";
        save_reorder_provider_fixture(&db, app_type, "first", Some(4), 1);
        save_reorder_provider_fixture(&db, foreign_app, "foreign_only", Some(7), 2);
        let app_before = reorder_provider_sort_indexes(&db, app_type);
        let foreign_before = reorder_provider_sort_indexes(&db, foreign_app);

        let error = db
            .reorder_providers(app_type, &["first".into(), "foreign_only".into()])
            .unwrap_err();

        assert_reorder_invalid_input(error, "AAG_PROVIDER_REORDER_UNKNOWN_ID", "foreign_only");
        assert_eq!(reorder_provider_sort_indexes(&db, app_type), app_before);
        assert_eq!(
            reorder_provider_sort_indexes(&db, foreign_app),
            foreign_before
        );
    }

    #[test]
    fn reorder_providers_isolated_by_composite_app_key() {
        let db = make_db();
        save_reorder_provider_fixture(&db, "claude_code", "shared_first", Some(1), 1);
        save_reorder_provider_fixture(&db, "claude_code", "shared_second", Some(2), 2);
        save_reorder_provider_fixture(&db, "codex", "shared_first", Some(8), 1);
        save_reorder_provider_fixture(&db, "codex", "shared_second", Some(7), 2);
        let other_app_before = reorder_provider_sort_indexes(&db, "codex");

        db.reorder_providers(
            "claude_code",
            &["shared_second".into(), "shared_first".into()],
        )
        .unwrap();

        assert_eq!(
            reorder_provider_sort_indexes(&db, "claude_code"),
            vec![
                ("shared_second".into(), Some(1)),
                ("shared_first".into(), Some(2)),
            ]
        );
        assert_eq!(
            reorder_provider_sort_indexes(&db, "codex"),
            other_app_before
        );
    }
}
