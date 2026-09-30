//! Schema definition and version migrations.
//!
//! Tables mirror cc-switch's `database/schema.rs` (lines 27-60) for providers
//! and provider_endpoints. Additional tables (mcp_servers, prompts, skills,
//! routes, usage, sessions) land in later phases.

use super::{lock_conn, Database, SCHEMA_VERSION};
use crate::error::AppError;
use rusqlite::Connection;

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

    /// Apply incremental schema migrations based on `PRAGMA user_version`.
    ///
    /// Each `if version < N` block runs the v(N-1) -> vN migration, then sets
    /// `user_version = N`. New databases start at version 0 and run all
    /// migrations in order.
    pub(crate) fn apply_schema_migrations(&self) -> Result<(), AppError> {
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

        // v18 -> v19: add proxy_providers.extra_headers column for
        // per-provider custom request headers (JSON object
        // `{ "x-opencode-session": "...", ... }`). Lets the proxy send
        // provider-mandated headers (e.g. OpenCode's session id) that are not
        // part of the trusted auth/content header set. Follows the
        // model_mapping guarded-ALTER pattern from v16.
        if version < 19 {
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
            conn.execute_batch("PRAGMA user_version = 19;")?;
            version = 19;
        }

        if version != SCHEMA_VERSION {
            return Err(AppError::Database(format!(
                "schema migration incomplete: at v{version}, expected v{SCHEMA_VERSION}"
            )));
        }
        Ok(())
    }
}
