//! Schema definition and version migrations.
//!
//! Tables mirror cc-switch's `database/schema.rs` (lines 27-60) for providers
//! and provider_endpoints. Additional tables (mcp_servers, prompts, skills,
//! routes, usage, sessions) land in later phases.

use super::{lock_conn, Database, SCHEMA_VERSION};
use crate::error::AppError;
use rusqlite::Connection;
use std::collections::{BTreeSet, HashMap, HashSet};

impl Database {
    /// Create all tables if they don't exist.
    pub(crate) fn create_tables(&self) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        Self::create_tables_on_conn(&conn)
    }

    pub(crate) fn create_tables_on_conn(conn: &Connection) -> Result<(), AppError> {
        // 1. Providers - one row per (id, app_type). `is_current` marks the
        //    active provider for each app_type. `settings_config` is a JSON
        //    blob whose shape depends on app_type (adapter-interpreted).
        conn.execute(
            "CREATE TABLE IF NOT EXISTS providers (
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
                in_failover_queue BOOLEAN NOT NULL DEFAULT 1,
                sync_locked BOOLEAN NOT NULL DEFAULT 0,
                PRIMARY KEY (id, app_type)
            )",
            [],
        )?;

        // 2. Provider endpoints - multiple URLs per provider (for speedtest/auto-select).
        conn.execute(
            "CREATE TABLE IF NOT EXISTS provider_endpoints (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                provider_id TEXT NOT NULL,
                app_type TEXT NOT NULL,
                url TEXT NOT NULL,
                added_at INTEGER,
                FOREIGN KEY (provider_id, app_type)
                    REFERENCES providers(id, app_type) ON DELETE CASCADE
            )",
            [],
        )?;

        // Index for efficient "list providers for app X ordered by sort"
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_providers_app_type
             ON providers(app_type, sort_index, created_at)",
            [],
        )?;

        // 3. Proxy providers - upstream API providers (mock/openai/anthropic)
        //    used by the local proxy for failover chains. Separate from the
        //    `providers` table (which holds per-agent config snapshots).
        conn.execute(
            "CREATE TABLE IF NOT EXISTS proxy_providers (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                type TEXT NOT NULL DEFAULT 'mock',
                api_key TEXT NOT NULL DEFAULT '',
                base_url TEXT NOT NULL DEFAULT '',
                models TEXT NOT NULL DEFAULT '[]',
                enabled BOOLEAN NOT NULL DEFAULT 1,
                timeout_ms INTEGER NOT NULL DEFAULT 60000,
                weight INTEGER NOT NULL DEFAULT 1,
                circuit_breaker TEXT NOT NULL DEFAULT '{}',
                model_mapping TEXT NOT NULL DEFAULT '{}',
                extra_headers TEXT NOT NULL DEFAULT '{}',
                icon TEXT,
                icon_color TEXT,
                created_at INTEGER
            )",
            [],
        )?;

        // 4. Routes - model alias -> ordered provider fallback chain.
        //    `hops` is a JSON array of {"provider": "<id>", "model": "<name>"}.
        conn.execute(
            "CREATE TABLE IF NOT EXISTS routes (
                alias TEXT PRIMARY KEY,
                description TEXT NOT NULL DEFAULT '',
                hops TEXT NOT NULL,
                created_at INTEGER
            )",
            [],
        )?;

        // 5. Proxy config - singleton row (id=1). Listen address, port, auth.
        conn.execute(
            "CREATE TABLE IF NOT EXISTS proxy_config (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                host TEXT NOT NULL DEFAULT '127.0.0.1',
                port INTEGER NOT NULL DEFAULT 8989,
                auth_token TEXT NOT NULL DEFAULT '',
                enabled BOOLEAN NOT NULL DEFAULT 0
            )",
            [],
        )?;

        // 6. Backups - serialized DB snapshots for restore. The `data` column
        //    holds a JSON dump of all table rows (excluding sync-skip tables).
        conn.execute(
            "CREATE TABLE IF NOT EXISTS backups (
                id TEXT PRIMARY KEY,
                label TEXT NOT NULL DEFAULT '',
                created_at INTEGER NOT NULL,
                size_bytes INTEGER NOT NULL DEFAULT 0,
                data BLOB NOT NULL
            )",
            [],
        )?;

        // 7. MCP servers - one row per server. `server_config` is a JSON blob
        //    describing the MCP server (command, args, env). Per-app enabled
        //    flags drive the bidirectional sync to Claude Code / Codex / etc.
        conn.execute(
            "CREATE TABLE IF NOT EXISTS mcp_servers (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                server_config TEXT NOT NULL,
                description TEXT,
                homepage TEXT,
                docs TEXT,
                tags TEXT NOT NULL DEFAULT '[]',
                enabled_claude BOOLEAN NOT NULL DEFAULT 0,
                enabled_codex BOOLEAN NOT NULL DEFAULT 0
            )",
            [],
        )?;

        // 8. Skills - installed skill metadata + per-app enabled flags.
        //    The skill files live at `directory` (mirrors cc-switch SSOT pattern).
        conn.execute(
            "CREATE TABLE IF NOT EXISTS skills (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                description TEXT,
                directory TEXT NOT NULL,
                repo_owner TEXT,
                repo_name TEXT,
                repo_branch TEXT DEFAULT 'main',
                content_hash TEXT,
                enabled_claude BOOLEAN NOT NULL DEFAULT 0,
                enabled_codex BOOLEAN NOT NULL DEFAULT 0,
                installed_at INTEGER NOT NULL DEFAULT 0,
                updated_at INTEGER NOT NULL DEFAULT 0
            )",
            [],
        )?;

        // 9. Sessions - chat session history (Claude Code, Codex, etc.).
        //    `source_path` points at the original JSONL on disk so the UI
        //    can load full messages on demand. Storing the path (not the
        //    content) keeps the DB small and avoids staleness.
        conn.execute(
            "CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                app_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                title TEXT,
                summary TEXT,
                project_dir TEXT,
                source_path TEXT,
                resume_command TEXT,
                message_count INTEGER NOT NULL DEFAULT 0,
                started_at INTEGER,
                last_active_at INTEGER
            )",
            [],
        )?;

        // 10. Usage records - per-request usage data for the dashboard.
        //     `ts` is Unix epoch seconds (INTEGER), matching cc-switch's
        //     `proxy_request_logs.created_at` (schema.rs:198) and the rest
        //     of AAG's schema (providers/sessions/etc.). DESIGN.md shows
        //     `ts TEXT` but we use INTEGER for consistency.
        conn.execute(
            "CREATE TABLE IF NOT EXISTS usage_records (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                ts INTEGER NOT NULL,
                app_id TEXT NOT NULL,
                provider_id TEXT NOT NULL,
                model TEXT NOT NULL,
                prompt_tokens INTEGER NOT NULL DEFAULT 0,
                completion_tokens INTEGER NOT NULL DEFAULT 0,
                cost_usd REAL NOT NULL DEFAULT 0,
                duration_ms INTEGER NOT NULL DEFAULT 0
            )",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_usage_ts ON usage_records(ts DESC)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_usage_provider ON usage_records(provider_id, ts DESC)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_usage_app ON usage_records(app_id, ts DESC)",
            [],
        )?;

        // 11. Sync config - cloud sync targets (S3 or WebDAV). One row per
        //    provider kind. Unified schema covers both: `access_key`/`secret_key`
        //    map to S3 access_key_id/secret_access_key OR WebDAV username/password;
        //    `bucket` is S3-only, `remote_path` is WebDAV-only, `region` is S3-only.
        conn.execute(
            "CREATE TABLE IF NOT EXISTS sync_config (
                provider TEXT PRIMARY KEY,
                enabled BOOLEAN NOT NULL DEFAULT 0,
                endpoint TEXT NOT NULL DEFAULT '',
                bucket TEXT,
                remote_path TEXT,
                access_key TEXT NOT NULL DEFAULT '',
                secret_key TEXT NOT NULL DEFAULT '',
                region TEXT,
                last_sync_at INTEGER,
                last_error TEXT
            )",
            [],
        )?;

        // 12. App meta - simple key/value settings store (device-level
        //    preferences such as the Codex CLI isolated CODEX_HOME).
        conn.execute(
            "CREATE TABLE IF NOT EXISTS app_meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            )",
            [],
        )?;

        // 13. Vendors - unified upstream API supplier management. One vendor
        //     may serve multiple protocols (anthropic, openai-chat,
        //     openai-responses) sharing a single API key. `endpoints` is a
        //     JSON object { protocol -> url } overriding `base_url` per
        //     protocol; `protocols` is a JSON array of enabled protocol ids.
        conn.execute(
            "CREATE TABLE IF NOT EXISTS vendors (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                base_url TEXT NOT NULL DEFAULT '',
                api_key TEXT NOT NULL DEFAULT '',
                protocols TEXT NOT NULL DEFAULT '[]',
                endpoints TEXT NOT NULL DEFAULT '{}',
                created_at INTEGER
            )",
            [],
        )?;

        Ok(())
    }

    /// Idempotently add the two `proxy_providers` discovery columns.
    ///
    /// `extra_headers` (custom per-provider request headers) and
    /// `auto_track_models` (model auto-discovery opt-out, default ON) were
    /// each claimed by an independent feature branch under schema version 19.
    /// SQLite has no `ADD COLUMN IF NOT EXISTS`, and a database stamped
    /// `user_version = 19` by an interim build carries only one of the two
    /// columns, so the version gate alone cannot re-add the missing one. Each
    /// column is guarded on `pragma_table_info`, which keeps this callable
    /// from both the v19 and v20 steps without error.
    fn ensure_proxy_provider_discovery_columns(conn: &Connection) -> Result<(), AppError> {
        let has_extra_headers: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('proxy_providers') WHERE name = 'extra_headers'",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);
        if has_extra_headers == 0 {
            conn.execute(
                "ALTER TABLE proxy_providers ADD COLUMN extra_headers TEXT NOT NULL DEFAULT '{}'",
                [],
            )?;
        }
        let has_atm: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('proxy_providers') WHERE name = 'auto_track_models'",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);
        if has_atm == 0 {
            conn.execute(
                "ALTER TABLE proxy_providers ADD COLUMN auto_track_models BOOLEAN NOT NULL DEFAULT 1",
                [],
            )?;
        }
        Ok(())
    }

    /// Idempotently add the two `proxy_providers` display columns.
    ///
    /// `icon` (designed glyph id or literal character) and `icon_color`
    /// (tile background) are presentation-only: the proxy runtime ignores
    /// them and they stay out of the routing fingerprint. Nullable because
    /// the UI falls back to a name monogram when both are absent.
    fn ensure_proxy_provider_display_columns(conn: &Connection) -> Result<(), AppError> {
        for column in ["icon", "icon_color"] {
            let present: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('proxy_providers') WHERE name = ?1",
                    [column],
                    |row| row.get(0),
                )
                .unwrap_or(0);
            if present == 0 {
                conn.execute(
                    &format!("ALTER TABLE proxy_providers ADD COLUMN {column} TEXT"),
                    [],
                )?;
            }
        }
        Ok(())
    }

    /// Idempotently add the two `suppliers` display columns.
    ///
    /// Same contract as [`Self::ensure_proxy_provider_display_columns`],
    /// one level up: the supplier is the source of truth for its icon and
    /// every managed legacy projection mirrors it. Presentation-only;
    /// nullable, so suppliers without a stored icon render a monogram.
    fn ensure_supplier_display_columns(conn: &Connection) -> Result<(), AppError> {
        for column in ["icon", "icon_color"] {
            let present: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('suppliers') WHERE name = ?1",
                    [column],
                    |row| row.get(0),
                )
                .unwrap_or(0);
            if present == 0 {
                conn.execute(
                    &format!("ALTER TABLE suppliers ADD COLUMN {column} TEXT"),
                    [],
                )?;
            }
        }
        Ok(())
    }

    /// Create the supplier data model and backfill safe API providers. This is
    /// invoked only inside the v20 -> v21 transaction, so every table, copied
    /// row, compatibility mapping, and version stamp rolls back together.
    fn migrate_supplier_schema_v21(conn: &Connection) -> Result<(), AppError> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS supplier_config_meta (
                 id INTEGER PRIMARY KEY CHECK (id = 1),
                 generation INTEGER NOT NULL CHECK (generation >= 0)
             );
             INSERT OR IGNORE INTO supplier_config_meta (id, generation) VALUES (1, 0);

             CREATE TABLE IF NOT EXISTS suppliers (
                 id TEXT PRIMARY KEY,
                 name TEXT NOT NULL,
                 base_url TEXT NOT NULL DEFAULT '',
                 api_key TEXT NOT NULL DEFAULT '',
                 enabled BOOLEAN NOT NULL DEFAULT 1,
                 priority INTEGER NOT NULL DEFAULT 100,
                 source TEXT NOT NULL DEFAULT 'manual',
                 provenance_json TEXT NOT NULL DEFAULT '{}',
                 created_at INTEGER,
                 updated_at INTEGER
             );
             CREATE INDEX IF NOT EXISTS idx_suppliers_priority
                 ON suppliers(enabled, priority, name COLLATE NOCASE, id);

             CREATE TABLE IF NOT EXISTS supplier_endpoints (
                 id TEXT PRIMARY KEY,
                 supplier_id TEXT NOT NULL,
                 protocol TEXT NOT NULL CHECK (
                     protocol IN ('anthropic', 'openai-chat', 'openai-responses', 'gemini')
                 ),
                 endpoint_url TEXT,
                 api_key_override TEXT,
                 enabled BOOLEAN NOT NULL DEFAULT 1,
                 priority INTEGER NOT NULL DEFAULT 100,
                 extra_headers TEXT NOT NULL DEFAULT '{}',
                 source TEXT NOT NULL DEFAULT 'manual',
                 provenance_json TEXT NOT NULL DEFAULT '{}',
                 created_at INTEGER,
                 updated_at INTEGER,
                 UNIQUE (supplier_id, protocol),
                 FOREIGN KEY (supplier_id) REFERENCES suppliers(id) ON DELETE CASCADE
             );
             CREATE INDEX IF NOT EXISTS idx_supplier_endpoints_order
                 ON supplier_endpoints(supplier_id, enabled, priority, protocol);

             CREATE TABLE IF NOT EXISTS supplier_models (
                 id TEXT PRIMARY KEY,
                 endpoint_id TEXT NOT NULL,
                 model_id TEXT NOT NULL,
                 display_name TEXT,
                 enabled BOOLEAN NOT NULL DEFAULT 1,
                 supports_tools BOOLEAN,
                 supports_images BOOLEAN,
                 context_window INTEGER CHECK (context_window IS NULL OR context_window > 0),
                 max_output_tokens INTEGER CHECK (
                     max_output_tokens IS NULL OR max_output_tokens > 0
                 ),
                 tier_override INTEGER CHECK (
                     tier_override IS NULL OR tier_override BETWEEN 0 AND 3
                 ),
                 sort_index INTEGER NOT NULL DEFAULT 0,
                 source TEXT NOT NULL DEFAULT 'manual',
                 provenance_json TEXT NOT NULL DEFAULT '{}',
                 created_at INTEGER,
                 updated_at INTEGER,
                 UNIQUE (endpoint_id, model_id),
                 FOREIGN KEY (endpoint_id) REFERENCES supplier_endpoints(id) ON DELETE CASCADE
             );
             CREATE INDEX IF NOT EXISTS idx_supplier_models_enabled
                 ON supplier_models(endpoint_id, enabled, sort_index, model_id);

             CREATE TABLE IF NOT EXISTS supplier_runtime_projections (
                 endpoint_id TEXT PRIMARY KEY,
                 proxy_provider_id TEXT NOT NULL UNIQUE,
                 managed BOOLEAN NOT NULL DEFAULT 1,
                 FOREIGN KEY (endpoint_id) REFERENCES supplier_endpoints(id) ON DELETE CASCADE
             );

             CREATE TABLE IF NOT EXISTS supplier_catalog_cache (
                 source TEXT PRIMARY KEY,
                 raw_json TEXT NOT NULL,
                 fetched_at INTEGER NOT NULL,
                 last_error TEXT,
                 CHECK (length(raw_json) <= 8388608)
             );",
        )?;

        backfill_legacy_vendors(conn)?;
        backfill_legacy_proxy_providers(conn)?;

        let supplier_count: i64 =
            conn.query_row("SELECT COUNT(*) FROM suppliers", [], |row| row.get(0))?;
        let initial_generation = if supplier_count > 0 { 1 } else { 0 };
        conn.execute(
            "UPDATE supplier_config_meta SET generation = ?1 WHERE id = 1",
            [initial_generation],
        )?;
        Ok(())
    }

    /// Apply incremental schema migrations based on `PRAGMA user_version`.
    ///
    /// Each `if version < N` block runs the v(N-1) -> vN migration, then sets
    /// `user_version = N`. New databases start at version 0 and run all
    /// migrations in order.
    pub(crate) fn apply_schema_migrations(&self) -> Result<(), AppError> {
        // Keep a recoverable data snapshot before applying the supplier schema
        // migration. The backup runs outside the connection lock held by the
        // migration so it can use its own transaction.
        let current_version = {
            let conn = lock_conn!(self.conn);
            Self::get_user_version(&conn)?
        };
        if current_version > 0 && current_version < SCHEMA_VERSION {
            self.create_backup(Some(format!(
                "pre-schema-v{SCHEMA_VERSION}-from-v{current_version}"
            )))?;
        }
        let conn = lock_conn!(self.conn);
        Self::apply_schema_migrations_on_conn(&conn)
    }

    pub(crate) fn apply_schema_migrations_on_conn(conn: &Connection) -> Result<(), AppError> {
        let mut version = Self::get_user_version(conn)?;

        // v0 -> v1: initial schema (tables already created by create_tables_on_conn,
        // but we stamp the version so fresh DBs skip the migration branches below).
        if version < 1 {
            // Tables already exist from create_tables_on_conn; just stamp version.
            conn.execute_batch("PRAGMA user_version = 1;")?;
            version = 1;
        }

        // v1 -> v2: proxy_providers, routes, proxy_config tables + seed.
        // Tables already created by create_tables_on_conn (IF NOT EXISTS), so
        // for existing v1 DBs we just need to stamp the version. Seeding happens
        // in `seed_proxy_if_empty` called from Database::init after migrations.
        if version < 2 {
            conn.execute_batch("PRAGMA user_version = 2;")?;
            version = 2;
        }

        // v2 -> v3: backups table. Already created above via IF NOT EXISTS,
        // so for existing v2 DBs we just stamp the version.
        if version < 3 {
            conn.execute_batch("PRAGMA user_version = 3;")?;
            version = 3;
        }

        // v3 -> v4: mcp_servers table. Already created above via IF NOT EXISTS,
        // so for existing v3 DBs we just stamp the version.
        if version < 4 {
            conn.execute_batch("PRAGMA user_version = 4;")?;
            version = 4;
        }

        // v4 -> v5: skills table. Already created above via IF NOT EXISTS,
        // so for existing v4 DBs we just stamp the version.
        if version < 5 {
            conn.execute_batch("PRAGMA user_version = 5;")?;
            version = 5;
        }

        // v5 -> v6: sessions table. Already created above via IF NOT EXISTS,
        // so for existing v5 DBs we just stamp the version.
        if version < 6 {
            conn.execute_batch("PRAGMA user_version = 6;")?;
            version = 6;
        }

        // v6 -> v7: usage_records table + indexes. Already created above
        // via IF NOT EXISTS, so for existing v6 DBs we just stamp the version.
        if version < 7 {
            conn.execute_batch("PRAGMA user_version = 7;")?;
            version = 7;
        }

        // v7 -> v8: sync_config table. Already created above via IF NOT EXISTS,
        // so for existing v7 DBs we just stamp the version.
        if version < 8 {
            conn.execute_batch("PRAGMA user_version = 8;")?;
            version = 8;
        }

        // v8 -> v9: app_meta key/value settings table. Already created above
        // via IF NOT EXISTS, so for existing v8 DBs we just stamp the version.
        if version < 9 {
            conn.execute_batch("PRAGMA user_version = 9;")?;
            version = 9;
        }

        // v9 -> v10: vendors table (unified upstream supplier management).
        // Already created above via IF NOT EXISTS, so for existing v9 DBs we
        // just stamp the version.
        if version < 10 {
            conn.execute_batch("PRAGMA user_version = 10;")?;
            version = 10;
        }

        // v10 -> v11: cc-switch coexistence detection.
        //
        // No new tables — the detector stores its state in `app_meta`
        // under the well-known keys `ccswitch_override_enabled` (string
        // "0"/"1") and `ccswitch_last_status_json` (JSON-serialized
        // CcSwitchStatus). For fresh DBs these rows are inserted on
        // demand by the detector; for existing DBs the migration only
        // needs to stamp the version.
        if version < 11 {
            conn.execute_batch("PRAGMA user_version = 11;")?;
            version = 11;
        }

        // v11 -> v12: default proxy port 8787 -> 8989.
        //
        // For users already on v11 the singleton `proxy_config` row
        // holds the old default; rewrite it so existing installs pick
        // up the new port on next proxy_start. Users who explicitly
        // configured a custom port (anything other than 8787) are
        // untouched.
        if version < 12 {
            conn.execute(
                "UPDATE proxy_config SET port = 8989 WHERE id = 1 AND port = 8787",
                [],
            )?;
            conn.execute_batch("PRAGMA user_version = 12;")?;
            version = 12;
        }

        // v12 -> v13: sweep the whole legacy fallback range.
        //
        // The old port-fallback persisted whatever port it landed on
        // (8787+1..+5), and v12 only rewrote the exact old default
        // (8787) — DBs that had drifted to 8788..8792 kept the legacy
        // port and could get stuck when that range is held. Rewrite
        // the entire legacy range to the new default.
        if version < 13 {
            conn.execute(
                "UPDATE proxy_config SET port = 8989 WHERE id = 1 AND port BETWEEN 8787 AND 8792",
                [],
            )?;
            conn.execute_batch("PRAGMA user_version = 13;")?;
            version = 13;
        }

        // v13 -> v14: add providers.sync_locked column for "do not
        // auto-overwrite this row when syncing from cc-switch". Existing
        // rows default to 0 (sync-friendly). SQLite has no IF NOT EXISTS
        // for ALTER TABLE ADD COLUMN, so we guard with a pragma check.
        if version < 14 {
            let has_sync_locked: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('providers') WHERE name = 'sync_locked'",
                    [],
                    |row| row.get(0),
                )
                .unwrap_or(0);
            if has_sync_locked == 0 {
                conn.execute(
                    "ALTER TABLE providers ADD COLUMN sync_locked BOOLEAN NOT NULL DEFAULT 0",
                    [],
                )?;
            }
            conn.execute_batch("PRAGMA user_version = 14;")?;
            version = 14;
        }

        // v14 -> v15: the per-app failover chain becomes universal (Phase 17,
        // cc-switch style). Every provider row defaults to being IN its app's
        // failover chain; the user opts rows out per card. Existing rows were
        // created before the toggle existed, so sweep them to 1 — the chain
        // they were already part of (claude_code) or never had (other apps)
        // both behave correctly with the bit on.
        if version < 15 {
            conn.execute(
                "UPDATE providers SET in_failover_queue = 1 WHERE in_failover_queue = 0",
                [],
            )?;
            conn.execute_batch("PRAGMA user_version = 15;")?;
            version = 15;
        }

        // v15 -> v16: add proxy_providers.model_mapping column for per-provider
        // model mapping (display model -> upstream model). Stored as a JSON
        // object `{ "claude-sonnet-5": "deepseek-v4-pro", ... }`. Used by the
        // proxy to auto-create routes from upstream pool mappings.
        if version < 16 {
            let has_mm: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('proxy_providers') WHERE name = 'model_mapping'",
                    [],
                    |row| row.get(0),
                )
                .unwrap_or(0);
            if has_mm == 0 {
                conn.execute(
                    "ALTER TABLE proxy_providers ADD COLUMN model_mapping TEXT NOT NULL DEFAULT '{}'",
                    [],
                )?;
            }
            conn.execute_batch("PRAGMA user_version = 16;")?;
            version = 16;
        }

        // v16 -> v17: backfill proxy_providers from existing claude_code providers.
        // Users who imported from cc-switch before the proxy_provider auto-creation
        // was added have providers in the `providers` table but NOT in `proxy_providers`.
        // This migration reads all claude_code providers with ANTHROPIC_BASE_URL and
        // creates corresponding proxy_providers (id prefixed with 'ccs-') so they
        // appear in the upstream pool on the Claude page.
        if version < 17 {
            // Use SQLite json_extract to parse settings_config without serde_json.
            let mut stmt = conn.prepare(
                "SELECT id, name, json_extract(settings_config, '$.env.ANTHROPIC_BASE_URL'),                  json_extract(settings_config, '$.env.ANTHROPIC_AUTH_TOKEN')                  FROM providers WHERE app_type = 'claude_code'"
            )?;
            let rows: Vec<(String, String, Option<String>, Option<String>)> = stmt
                .query_map([], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                })?
                .filter_map(|r| r.ok())
                .collect();
            for (id, name, base_url, api_key) in rows {
                let pp_id = format!("ccs-{}", id);
                let exists: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM proxy_providers WHERE id = ?1",
                        [&pp_id],
                        |r| r.get(0),
                    )
                    .unwrap_or(0);
                if exists > 0 {
                    continue;
                }
                let base_url = match base_url.as_deref() {
                    Some(u) if !u.is_empty() => u,
                    _ => continue,
                };
                let api_key = api_key.unwrap_or_default();
                conn.execute(
                    "INSERT INTO proxy_providers (id, name, type, api_key, base_url, models, enabled, timeout_ms, weight, circuit_breaker, model_mapping)
                     VALUES (?1, ?2, 'anthropic', ?3, ?4, '[]', 1, 60000, 1, '{}', '{}')",
                    rusqlite::params![pp_id, name, api_key, base_url],
                )?;
            }
            conn.execute_batch("PRAGMA user_version = 17;")?;
            version = 17;
        }

        // v17 -> v18: no-op schema change. Codex model routes are seeded
        // in `seed_proxy_if_empty` alongside the Claude routes.
        if version < 18 {
            conn.execute_batch("PRAGMA user_version = 18;")?;
            version = 18;
        }

        // v18 -> v19: `extra_headers` - per-provider custom request headers
        // (JSON object `{ "x-opencode-session": "...", ... }`). Lets the proxy
        // send provider-mandated headers (e.g. OpenCode's session id) that are
        // not part of the trusted auth/content header set.
        //
        // `auto_track_models` (model auto-discovery opt-out, default ON) was
        // claimed under the same version 19 by a parallel branch; both columns
        // are applied here through the shared guarded ALTER helper and the
        // v19 -> v20 step re-asserts them for interim builds. See
        // `ensure_proxy_provider_discovery_columns` for the idempotency note.
        if version < 19 {
            Self::ensure_proxy_provider_discovery_columns(conn)?;
            conn.execute_batch("PRAGMA user_version = 19;")?;
            version = 19;
        }

        // v19 -> v20: re-assert both `proxy_providers` discovery columns.
        //
        // `extra_headers` (upstream) and `auto_track_models` (model discovery)
        // were each merged under version 19 by separate branches, so an interim
        // build may have stamped `user_version = 19` after creating only one of
        // the two columns. The v19 gate above is skipped for such a database,
        // so a fresh version re-runs the guarded ALTERs and adds whichever
        // column it is missing. Both columns are pragma-guarded, so this is a
        // no-op for databases that already carry them.
        if version < 20 {
            Self::ensure_proxy_provider_discovery_columns(conn)?;
            conn.execute_batch("PRAGMA user_version = 20;")?;
            version = 20;
        }

        // v20 -> v21: normalized supplier -> endpoint -> model configuration.
        // DDL, legacy backfill, projection ownership, generation baseline, and
        // the version stamp are one SQLite transaction by design.
        if version < 21 {
            let transaction = conn.unchecked_transaction()?;
            Self::migrate_supplier_schema_v21(&transaction)?;
            transaction.execute_batch("PRAGMA user_version = 21;")?;
            transaction.commit()?;
            version = 21;
        }

        // v21 -> v22: `proxy_providers` display columns (`icon`, `icon_color`)
        // for the provider icon system. Presentation-only; nullable, guarded
        // ALTERs so interim databases re-run safely.
        if version < 22 {
            Self::ensure_proxy_provider_display_columns(conn)?;
            conn.execute_batch("PRAGMA user_version = 22;")?;
            version = 22;
        }

        // v22 -> v23: `suppliers` display columns (`icon`, `icon_color`) so
        // the unified supplier page renders the same tiles as the pool. The
        // supplier now owns its icon; managed legacy projections mirror it.
        // Presentation-only; nullable, guarded ALTERs.
        if version < 23 {
            Self::ensure_supplier_display_columns(conn)?;
            conn.execute_batch("PRAGMA user_version = 23;")?;
            version = 23;
        }

        // v23 -> v24 retains imported same-protocol endpoints privately while
        // preserving the established primary protocol identity for old callers.
        if version < 24 {
            let present: i64 = conn.query_row("SELECT COUNT(*) FROM pragma_table_info('supplier_endpoints') WHERE name = 'alternatives_json'", [], |row| row.get(0))?;
            if present == 0 { conn.execute("ALTER TABLE supplier_endpoints ADD COLUMN alternatives_json TEXT NOT NULL DEFAULT '[]'", [])?; }
            conn.execute_batch("PRAGMA user_version = 24;")?;
            version = 24;
        }

        // An interim v24 may already contain endpoint variants; add the private
        // origin journal idempotently without touching any existing rows.
        if version >= 24 {
            let present:i64=conn.query_row("SELECT COUNT(*) FROM pragma_table_info('suppliers') WHERE name='import_origins_json'",[],|row| row.get(0))?;
            if present==0 { conn.execute("ALTER TABLE suppliers ADD COLUMN import_origins_json TEXT NOT NULL DEFAULT '[]'",[])?; }
        }

        // v24 -> v25: catalog-driven model retirement bookkeeping on
        // `supplier_models`. `retire_misses` counts consecutive successful
        // upstream listings that omitted the model, `retired_at`/`retire_reason`
        // record the retirement decision so imports can be re-enabled when the
        // upstream lists the id again. Nullable/defaulted, pragma-guarded ALTERs
        // so interim databases re-run safely.
        if version < 25 {
            let mut add_column = |name: &str, ddl: &str| -> Result<(), AppError> {
                let present: i64 = conn.query_row(
                    &format!(
                        "SELECT COUNT(*) FROM pragma_table_info('supplier_models') WHERE name = '{name}'"
                    ),
                    [],
                    |row| row.get(0),
                )?;
                if present == 0 {
                    conn.execute(ddl, [])?;
                }
                Ok(())
            };
            add_column(
                "retire_misses",
                "ALTER TABLE supplier_models ADD COLUMN retire_misses INTEGER NOT NULL DEFAULT 0",
            )?;
            add_column(
                "retired_at",
                "ALTER TABLE supplier_models ADD COLUMN retired_at INTEGER",
            )?;
            add_column(
                "retire_reason",
                "ALTER TABLE supplier_models ADD COLUMN retire_reason TEXT",
            )?;
            conn.execute_batch("PRAGMA user_version = 25;")?;
            version = 25;
        }

        if version != SCHEMA_VERSION {
            return Err(AppError::Database(format!(
                "schema migration incomplete: at v{version}, expected v{SCHEMA_VERSION}"
            )));
        }
        Ok(())
    }
}

fn backfill_legacy_vendors(conn: &Connection) -> Result<(), AppError> {
    let rows = {
        let mut statement = conn.prepare(
            "SELECT id, name, base_url, api_key, protocols, endpoints, created_at
             FROM vendors ORDER BY id",
        )?;
        let mapped = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Option<i64>>(6)?,
            ))
        })?;
        let mut rows = Vec::new();
        for row in mapped {
            rows.push(row?);
        }
        rows
    };

    for (legacy_id, name, base_url, api_key, protocols_json, endpoints_json, created_at) in rows {
        let protocols: Vec<String> = serde_json::from_str(&protocols_json)?;
        let endpoint_overrides: HashMap<String, String> = serde_json::from_str(&endpoints_json)?;
        let mut protocol_set: BTreeSet<String> = protocols.into_iter().collect();
        protocol_set.extend(endpoint_overrides.keys().cloned());
        if protocol_set
            .iter()
            .any(|protocol| !crate::database::dao::suppliers::is_supported_protocol(protocol))
        {
            return Err(AppError::Database(
                "supplier v21 migration encountered an unsupported vendor protocol".into(),
            ));
        }

        let supplier_id = crate::database::dao::suppliers::legacy_supplier_id("vendor", &legacy_id);
        let now = created_at.unwrap_or_else(|| chrono::Utc::now().timestamp());
        let provenance = serde_json::json!({
            "sourceKind": "legacy_vendor",
            "sourceRecordRef": supplier_id,
        });
        conn.execute(
            "INSERT INTO suppliers (
                 id, name, base_url, api_key, enabled, priority, source,
                 provenance_json, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, 1, 100, 'legacy_vendor', ?5, ?6, ?6)",
            rusqlite::params![
                supplier_id,
                name,
                base_url,
                api_key,
                serde_json::to_string(&provenance)?,
                now,
            ],
        )?;

        for protocol in protocol_set {
            let endpoint_url = endpoint_overrides
                .get(&protocol)
                .filter(|url| !url.is_empty())
                .cloned();
            let endpoint_id =
                crate::database::dao::suppliers::endpoint_id_for(&supplier_id, &protocol);
            let endpoint_provenance = serde_json::json!({
                "sourceKind": "legacy_vendor",
                "sourceRecordRef": supplier_id,
            });
            conn.execute(
                "INSERT INTO supplier_endpoints (
                     id, supplier_id, protocol, endpoint_url, api_key_override, enabled,
                     priority, extra_headers, source, provenance_json, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, NULL, 1, 100, '{}',
                           'legacy_vendor', ?5, ?6, ?6)",
                rusqlite::params![
                    endpoint_id,
                    supplier_id,
                    protocol,
                    endpoint_url,
                    serde_json::to_string(&endpoint_provenance)?,
                    now,
                ],
            )?;
        }
    }
    Ok(())
}

fn backfill_legacy_proxy_providers(conn: &Connection) -> Result<(), AppError> {
    let rows = {
        let mut statement = conn.prepare(
            "SELECT id, name, type, api_key, base_url, models, enabled, weight,
                    extra_headers, created_at
             FROM proxy_providers ORDER BY id",
        )?;
        let mapped = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, bool>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, Option<i64>>(9)?,
            ))
        })?;
        let mut rows = Vec::new();
        for row in mapped {
            rows.push(row?);
        }
        rows
    };

    for (
        legacy_id,
        name,
        provider_type,
        api_key,
        base_url,
        models_json,
        enabled,
        weight,
        headers_json,
        created_at,
    ) in rows
    {
        // Parse every legacy JSON field strictly, including rows that are not
        // API suppliers. A malformed record blocks this transaction rather
        // than being partially copied or silently normalized.
        let models: Vec<String> = serde_json::from_str(&models_json)?;
        let extra_headers: HashMap<String, String> = serde_json::from_str(&headers_json)?;
        if weight < 0 {
            return Err(AppError::Database(
                "supplier v21 migration encountered an invalid provider priority".into(),
            ));
        }

        if provider_type == "mock" {
            // Local test providers remain in the untouched legacy table; they
            // are not external suppliers and must not appear as usable APIs.
            continue;
        }
        if provider_type == "anthropic_passthrough" {
            // Official-account forwarding is intentionally not implemented.
            // Preserve only a disabled, credential-free pending marker; do not
            // copy its upstream URL or imply that a refresh token is usable.
            let supplier_id =
                crate::database::dao::suppliers::legacy_supplier_id("passthrough", &legacy_id);
            let now = created_at.unwrap_or_else(|| chrono::Utc::now().timestamp());
            let provenance = serde_json::json!({
                "sourceKind": "official_auth_pending",
                "sourceRecordRef": supplier_id,
            });
            conn.execute(
                "INSERT INTO suppliers (
                     id, name, base_url, api_key, enabled, priority, source,
                     provenance_json, created_at, updated_at
                 ) VALUES (?1, ?2, '', '', 0, 100, 'official_auth_pending', ?3, ?4, ?4)",
                rusqlite::params![supplier_id, name, serde_json::to_string(&provenance)?, now,],
            )?;
            continue;
        }

        let protocol = match provider_type.as_str() {
            "anthropic" => crate::database::dao::suppliers::PROTOCOL_ANTHROPIC,
            "openai" => crate::database::dao::suppliers::PROTOCOL_OPENAI_CHAT,
            "gemini" => crate::database::dao::suppliers::PROTOCOL_GEMINI,
            _ => {
                return Err(AppError::Database(
                    "supplier v21 migration encountered an unknown proxy provider type".into(),
                ));
            }
        };
        let mut seen_models = HashSet::new();
        if models
            .iter()
            .any(|model| model.trim().is_empty() || !seen_models.insert(model.clone()))
        {
            return Err(AppError::Database(
                "supplier v21 migration encountered an invalid model list".into(),
            ));
        }

        let supplier_id =
            crate::database::dao::suppliers::legacy_supplier_id("proxy_provider", &legacy_id);
        let endpoint_id = crate::database::dao::suppliers::endpoint_id_for(&supplier_id, protocol);
        let now = created_at.unwrap_or_else(|| chrono::Utc::now().timestamp());
        let priority = 201 - weight.min(1_000_000);
        let provenance = serde_json::json!({
            "sourceKind": "legacy_proxy_provider",
            "sourceRecordRef": supplier_id,
        });
        conn.execute(
            "INSERT INTO suppliers (
                 id, name, base_url, api_key, enabled, priority, source,
                 provenance_json, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'legacy_proxy_provider', ?7, ?8, ?8)",
            rusqlite::params![
                supplier_id,
                name,
                base_url,
                api_key,
                enabled,
                priority,
                serde_json::to_string(&provenance)?,
                now,
            ],
        )?;
        conn.execute(
            "INSERT INTO supplier_endpoints (
                 id, supplier_id, protocol, endpoint_url, api_key_override, enabled,
                 priority, extra_headers, source, provenance_json, created_at, updated_at
             ) VALUES (?1, ?2, ?3, NULL, NULL, 1, 0, ?4,
                       'legacy_proxy_provider', ?5, ?6, ?6)",
            rusqlite::params![
                endpoint_id,
                supplier_id,
                protocol,
                serde_json::to_string(&extra_headers)?,
                serde_json::to_string(&provenance)?,
                now,
            ],
        )?;

        for (sort_index, model) in models.into_iter().enumerate() {
            let model_id = crate::database::dao::suppliers::model_id_for(&endpoint_id, &model);
            conn.execute(
                "INSERT INTO supplier_models (
                     id, endpoint_id, model_id, display_name, enabled, source,
                     provenance_json, sort_index, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, NULL, 1, 'legacy_proxy_provider', ?4, ?5, ?6, ?6)",
                rusqlite::params![
                    model_id,
                    endpoint_id,
                    model,
                    serde_json::to_string(&provenance)?,
                    sort_index as i64,
                    now,
                ],
            )?;
        }

        // The old proxy row remains the same row referenced by existing routes.
        // Ownership=0 prevents supplier deletion from deleting a legacy row.
        conn.execute(
            "INSERT INTO supplier_runtime_projections (
                 endpoint_id, proxy_provider_id, managed
             ) VALUES (?1, ?2, 0)",
            rusqlite::params![endpoint_id, legacy_id],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn column_count(conn: &Connection, name: &str) -> i64 {
        conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('proxy_providers') WHERE name = ?1",
            [name],
            |row| row.get(0),
        )
        .unwrap()
    }

    /// An interim build that stamped `user_version = 19` while creating only
    /// one of the two `proxy_providers` discovery columns must still receive
    /// the missing column from the v19 -> v20 step. Both cases are exercised
    /// because either branch could have been merged alone.
    #[test]
    fn v19_interim_build_backfills_missing_discovery_column() {
        // Case A: interim build carried `extra_headers` only.
        let conn = Connection::open_in_memory().unwrap();
        Database::create_tables_on_conn(&conn).unwrap();
        conn.execute_batch("PRAGMA user_version = 19;").unwrap();
        Database::apply_schema_migrations_on_conn(&conn).unwrap();
        assert_eq!(Database::get_user_version(&conn).unwrap(), SCHEMA_VERSION);
        assert_eq!(column_count(&conn, "extra_headers"), 1);
        assert_eq!(column_count(&conn, "auto_track_models"), 1);

        // Case B: interim build carried `auto_track_models` only.
        let conn = Connection::open_in_memory().unwrap();
        Database::create_tables_on_conn(&conn).unwrap();
        conn.execute_batch(
            "ALTER TABLE proxy_providers DROP COLUMN extra_headers;
             ALTER TABLE proxy_providers ADD COLUMN auto_track_models BOOLEAN NOT NULL DEFAULT 1;
             PRAGMA user_version = 19;",
        )
        .unwrap();
        Database::apply_schema_migrations_on_conn(&conn).unwrap();
        assert_eq!(Database::get_user_version(&conn).unwrap(), SCHEMA_VERSION);
        assert_eq!(column_count(&conn, "extra_headers"), 1);
        assert_eq!(column_count(&conn, "auto_track_models"), 1);
    }

    /// A fresh in-memory database must land on the current version with both
    /// discovery columns present.
    #[test]
    fn fresh_database_reaches_current_version_with_discovery_columns() {
        let db = Database::memory().unwrap();
        let conn = db.conn.lock().unwrap();
        assert_eq!(Database::get_user_version(&conn).unwrap(), SCHEMA_VERSION);
        assert_eq!(column_count(&conn, "extra_headers"), 1);
        assert_eq!(column_count(&conn, "auto_track_models"), 1);
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM supplier_config_meta WHERE id = 1 AND generation = 0",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
            1
        );
    }

    #[test]
    fn v20_to_v21_backfills_vendors_and_api_providers_without_changing_routes() {
        let conn = Connection::open_in_memory().unwrap();
        Database::create_tables_on_conn(&conn).unwrap();
        conn.execute(
            "INSERT INTO vendors (id, name, base_url, api_key, protocols, endpoints, created_at)
             VALUES ('vendor-one', 'Acme Relay', 'https://shared.example/v1', 'sk-vendor',
                     '[\"openai-chat\",\"gemini\"]',
                     '{\"openai-chat\":\"https://chat.example/v1\"}', 100)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO proxy_providers (
                 id, name, type, api_key, base_url, models, enabled, weight,
                 circuit_breaker, model_mapping, extra_headers, created_at
             ) VALUES ('legacy-openai', 'Acme Relay', 'openai', 'sk-legacy',
                       'https://legacy.example/v1', '[\"model-a\",\"model-b\"]',
                       1, 3, '{}', '{}', '{\"x-safe\":\"value\"}', 101)",
            [],
        )
        .unwrap();
        let old_hops = r#"[{"provider":"legacy-openai","model":"model-a"}]"#;
        conn.execute(
            "INSERT INTO routes (alias, description, hops, created_at)
             VALUES ('model-a', 'keep this route', ?1, 102)",
            [old_hops],
        )
        .unwrap();
        conn.execute_batch("PRAGMA user_version = 20;").unwrap();

        Database::apply_schema_migrations_on_conn(&conn).unwrap();

        assert_eq!(Database::get_user_version(&conn).unwrap(), SCHEMA_VERSION);
        let migrated: Vec<(String, String, String)> = {
            let mut statement = conn
                .prepare("SELECT name, api_key, source FROM suppliers ORDER BY api_key")
                .unwrap();
            let rows = statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
                .unwrap();
            rows.map(Result::unwrap).collect()
        };
        assert_eq!(migrated.len(), 2);
        assert_eq!(
            migrated[0],
            (
                "Acme Relay".into(),
                "sk-legacy".into(),
                "legacy_proxy_provider".into()
            )
        );
        assert_eq!(
            migrated[1],
            (
                "Acme Relay".into(),
                "sk-vendor".into(),
                "legacy_vendor".into()
            )
        );

        let protocol_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM supplier_endpoints", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(protocol_count, 3);
        let model_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM supplier_models", [], |row| row.get(0))
            .unwrap();
        assert_eq!(model_count, 2);
        let projection: (String, bool) = conn
            .query_row(
                "SELECT proxy_provider_id, managed FROM supplier_runtime_projections",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(projection, ("legacy-openai".into(), false));
        let route_after: String = conn
            .query_row(
                "SELECT hops FROM routes WHERE alias = 'model-a'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(route_after, old_hops);
        let legacy_key: String = conn
            .query_row(
                "SELECT api_key FROM proxy_providers WHERE id = 'legacy-openai'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(legacy_key, "sk-legacy");
    }

    #[test]
    fn failed_v21_backfill_rolls_back_ddl_data_and_version_stamp() {
        let conn = Connection::open_in_memory().unwrap();
        Database::create_tables_on_conn(&conn).unwrap();
        conn.execute(
            "INSERT INTO vendors (id, name, base_url, api_key, protocols, endpoints, created_at)
             VALUES ('a-good', 'Good', 'https://good.example/v1', 'sk-good',
                     '[\"anthropic\"]', '{}', 100)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO vendors (id, name, base_url, api_key, protocols, endpoints, created_at)
             VALUES ('z-bad', 'Bad', 'https://bad.example/v1', 'sk-bad',
                     'not-json', '{}', 101)",
            [],
        )
        .unwrap();
        conn.execute_batch("PRAGMA user_version = 20;").unwrap();

        assert!(Database::apply_schema_migrations_on_conn(&conn).is_err());
        assert_eq!(Database::get_user_version(&conn).unwrap(), 20);
        let supplier_table_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'suppliers'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(supplier_table_count, 0);
        let vendor_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM vendors", [], |row| row.get(0))
            .unwrap();
        assert_eq!(vendor_count, 2);
    }
}
