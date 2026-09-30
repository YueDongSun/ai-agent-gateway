//! Provider DAO - CRUD operations on the `providers` table.
//!
//! Mirrors cc-switch's `database/dao/providers.rs` (lines 20-180), simplified
//! to skip endpoint joins (endpoints land in a later phase with speedtest).

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

impl Database {
    /// List all providers for an app, ordered by sort_index then created_at.
    /// Returns an `IndexMap` keyed by provider id (insertion-ordered).
    pub fn get_all_providers(
        &self,
        app_type: &str,
    ) -> Result<IndexMap<String, Provider>, AppError> {
        let conn = lock_conn!(self.conn);
        let mut stmt = conn.prepare(
            "SELECT id, name, settings_config, website_url, category, created_at,
                    sort_index, notes, icon, icon_color, meta, in_failover_queue, sync_locked
             FROM providers
             WHERE app_type = ?1
             ORDER BY COALESCE(sort_index, 999999), created_at ASC, id ASC",
        )?;

        let rows = stmt.query_map(params![app_type], |row| {
            let id: String = row.get(0)?;
            let name: String = row.get(1)?;
            let settings_config_str: String = row.get(2)?;
            let website_url: Option<String> = row.get(3)?;
            let category: Option<String> = row.get(4)?;
            let created_at: Option<i64> = row.get(5)?;
            let sort_index: Option<usize> = row.get(6)?;
            let notes: Option<String> = row.get(7)?;
            let icon: Option<String> = row.get(8)?;
            let icon_color: Option<String> = row.get(9)?;
            let meta_str: String = row.get(10)?;
            let in_failover_queue: bool = row.get(11)?;
            let sync_locked: bool = row.get(12)?;

            let settings_config =
                serde_json::from_str(&settings_config_str).unwrap_or(serde_json::Value::Null);
            let meta: ProviderMeta = serde_json::from_str(&meta_str).unwrap_or_default();

            Ok((
                id,
                Provider {
                    id: String::new(), // filled below
                    name,
                    settings_config,
                    website_url,
                    category,
                    created_at,
                    sort_index,
                    notes,
                    icon,
                    icon_color,
                    meta: Some(meta),
                    in_failover_queue,
                    sync_locked,
                },
            ))
        })?;

        let mut providers = IndexMap::new();
        for row in rows {
            let (id, mut provider) = row?;
            provider.id = id.clone();
            providers.insert(id, provider);
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
        let result = conn.query_row(
            "SELECT name, settings_config, website_url, category, created_at,
                    sort_index, notes, icon, icon_color, meta, in_failover_queue, sync_locked
             FROM providers WHERE id = ?1 AND app_type = ?2",
            params![id, app_type],
            |row| {
                let name: String = row.get(0)?;
                let settings_config_str: String = row.get(1)?;
                let website_url: Option<String> = row.get(2)?;
                let category: Option<String> = row.get(3)?;
                let created_at: Option<i64> = row.get(4)?;
                let sort_index: Option<usize> = row.get(5)?;
                let notes: Option<String> = row.get(6)?;
                let icon: Option<String> = row.get(7)?;
                let icon_color: Option<String> = row.get(8)?;
                let meta_str: String = row.get(9)?;
                let in_failover_queue: bool = row.get(10)?;
                let sync_locked: bool = row.get(11)?;

                let settings_config =
                    serde_json::from_str(&settings_config_str).unwrap_or(serde_json::Value::Null);
                let meta: ProviderMeta = serde_json::from_str(&meta_str).unwrap_or_default();

                Ok(Provider {
                    id: id.to_string(),
                    name,
                    settings_config,
                    website_url,
                    category,
                    created_at,
                    sort_index,
                    notes,
                    icon,
                    icon_color,
                    meta: Some(meta),
                    in_failover_queue,
                    sync_locked,
                })
            },
        );
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
    /// row from the supplied id list. Ids not present in the list keep
    /// their existing sort_index (and fall after the reordered ones).
    pub fn reorder_providers(
        &self,
        app_type: &str,
        ordered_ids: &[String],
    ) -> Result<(), AppError> {
        let mut conn = lock_conn!(self.conn);
        let tx = conn.transaction()?;
        for (idx, id) in ordered_ids.iter().enumerate() {
            tx.execute(
                "UPDATE providers SET sort_index = ?3 WHERE id = ?1 AND app_type = ?2",
                params![id, app_type, (idx as i64) + 1],
            )?;
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
            return Err(AppError::InvalidInput(format!(
                "provider \"{id}\" not found for app \"{app_type}\""
            )));
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
            return Err(AppError::InvalidInput(format!(
                "provider \"{id}\" not found for app \"{app_type}\""
            )));
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
            return Err(AppError::InvalidInput(format!(
                "provider \"{id}\" not found for app \"{app_type}\""
            )));
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
        assert!(matches!(err, AppError::InvalidInput(_)));
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
}
