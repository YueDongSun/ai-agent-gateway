//! Process-local routing input identity. No credentials or fingerprint cross IPC.
use super::super::{lock_conn, Database};
use crate::error::AppError;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

const ROUTING_QUERIES: &[&str] = &[
    "SELECT id, type, api_key, base_url, models, enabled, timeout_ms, weight, circuit_breaker, model_mapping, extra_headers FROM proxy_providers ORDER BY id",
    "SELECT alias, hops FROM routes ORDER BY alias",
    "SELECT id, app_type, settings_config, sort_index, created_at, in_failover_queue FROM providers ORDER BY app_type, id",
    "SELECT id FROM providers WHERE app_type = 'codex' AND is_current = 1 ORDER BY id",
    "SELECT generation FROM supplier_config_meta WHERE id = 1",
    // The pin is a routing input: always one row (NULL when unset) so
    // set/clear moves the fingerprint exactly once each.
    "SELECT (SELECT value FROM app_meta WHERE key = 'proxy_pinned_provider')",
    // The failover-queue spread mode is a routing input for the same reason.
    "SELECT (SELECT value FROM app_meta WHERE key = 'routing_mode')",
    "SELECT (SELECT value FROM app_meta WHERE key = 'model_utility_policy')",
    "SELECT (SELECT value FROM app_meta WHERE key = 'provider_screening_policy')",
    // Receipt invalidation/replacement and authoritative evidence changes
    // rebuild visible candidate snapshots; the per-attempt gate checks sooner.
    "SELECT key,value FROM app_meta WHERE key GLOB 'provider_admission:*' OR key GLOB 'provider_subscription:*' ORDER BY key",
    // Desktop role selectors follow the unified default in the same generation.
    "SELECT (SELECT value FROM app_meta WHERE key = 'unified_default_model')",
    "SELECT (SELECT value FROM app_meta WHERE key = 'mirror_model_roles_enabled')",
    // Public catalog evidence feeds capability tiers, so a refresh must move
    // the fingerprint (and thus hot-apply) even without supplier edits.
    "SELECT source, fetched_at, last_error, raw_json FROM supplier_catalog_cache ORDER BY source",
];

impl Database {
    pub(crate) fn routing_revision(&self) -> Result<u64, AppError> {
        let mut conn = lock_conn!(self.conn);
        let transaction = conn.transaction()?;
        let mut fingerprint = DefaultHasher::new();
        for query in ROUTING_QUERIES {
            query.hash(&mut fingerprint);
            let mut statement = transaction.prepare(query)?;
            let column_count = statement.column_count();
            let mut rows = statement.query([])?;
            while let Some(row) = rows.next()? {
                column_count.hash(&mut fingerprint);
                for column in 0..column_count {
                    use rusqlite::types::ValueRef;
                    let value = row.get_ref(column)?;
                    std::mem::discriminant(&value).hash(&mut fingerprint);
                    match value {
                        ValueRef::Null => {}
                        ValueRef::Integer(value) => value.hash(&mut fingerprint),
                        ValueRef::Real(value) => value.to_bits().hash(&mut fingerprint),
                        ValueRef::Text(value) | ValueRef::Blob(value) => {
                            value.hash(&mut fingerprint)
                        }
                    }
                }
            }
        }
        transaction.commit()?;
        Ok(fingerprint.finish())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn revision_tracks_routing_inputs_but_not_diagnostics_or_winner_markers() {
        let db = Database::memory().unwrap();
        let baseline = db.routing_revision().unwrap();
        db.set_setting("diagnostics_debug_until", "1").unwrap();
        assert_eq!(baseline, db.routing_revision().unwrap());
        {
            let conn = db.conn.lock().unwrap();
            conn.execute("INSERT INTO routes (alias,hops) VALUES ('test','[]')", [])
                .unwrap();
        }
        let saved = db.routing_revision().unwrap();
        assert_ne!(baseline, saved);
        db.conn
            .lock()
            .unwrap()
            .execute("DELETE FROM routes WHERE alias='test'", [])
            .unwrap();
        assert_eq!(baseline, db.routing_revision().unwrap());
    }

    #[test]
    fn revision_tracks_the_pinned_provider_setting() {
        let db = Database::memory().unwrap();
        let baseline = db.routing_revision().unwrap();
        db.set_setting("proxy_pinned_provider", "supplier-a")
            .unwrap();
        let pinned = db.routing_revision().unwrap();
        assert_ne!(baseline, pinned);
        db.set_setting("proxy_pinned_provider", "supplier-b")
            .unwrap();
        assert_ne!(pinned, db.routing_revision().unwrap());
        db.set_setting("proxy_pinned_provider", "").unwrap();
        assert_ne!(pinned, db.routing_revision().unwrap());
    }

    #[test]
    fn revision_tracks_the_routing_mode_setting() {
        let db = Database::memory().unwrap();
        let baseline = db.routing_revision().unwrap();
        db.set_setting("routing_mode", "rotate").unwrap();
        let rotated = db.routing_revision().unwrap();
        assert_ne!(baseline, rotated);
        db.set_setting("routing_mode", "weight").unwrap();
        assert_ne!(rotated, db.routing_revision().unwrap());
        // Back to default: the row is deleted, so the revision returns to
        // the same baseline hash (order == absent for the fingerprint row).
        db.set_setting("routing_mode", "").unwrap();
        assert_eq!(baseline, db.routing_revision().unwrap());
    }

    #[test]
    fn revision_tracks_mirror_role_enablement() {
        let db = Database::memory().unwrap();
        let baseline = db.routing_revision().unwrap();
        db.set_setting("mirror_model_roles_enabled", "1").unwrap();
        let enabled = db.routing_revision().unwrap();
        assert_ne!(baseline, enabled);
        db.set_setting("mirror_model_roles_enabled", "").unwrap();
        assert_eq!(baseline, db.routing_revision().unwrap());
    }

    #[test]
    fn revision_tracks_catalog_cache_refreshes() {
        let db = Database::memory().unwrap();
        let baseline = db.routing_revision().unwrap();
        {
            let conn = db.conn.lock().unwrap();
            conn.execute(
                "INSERT INTO supplier_catalog_cache (source, raw_json, fetched_at, last_error)
                 VALUES ('models_dev', '{}', 1, NULL)",
                [],
            )
            .unwrap();
        }
        let cached = db.routing_revision().unwrap();
        assert_ne!(baseline, cached);
        {
            let conn = db.conn.lock().unwrap();
            conn.execute(
                "UPDATE supplier_catalog_cache SET raw_json = '{\"x\":1}', fetched_at = 2
                 WHERE source = 'models_dev'",
                [],
            )
            .unwrap();
        }
        assert_ne!(cached, db.routing_revision().unwrap());
    }
}
