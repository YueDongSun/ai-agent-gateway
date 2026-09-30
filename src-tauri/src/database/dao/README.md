# `src-tauri/src/database/dao/` — Data Access Objects

Each DAO module owns the CRUD methods for one or more tables. Every method
is an `impl Database` block so callers see them as methods on the shared
`Database` state. SQL never escapes this directory; commands, services, and
the proxy call DAO methods exclusively.

## Anchor files

- [`mod.rs`](mod.rs) — re-exports every DAO submodule.
- [`providers.rs`](providers.rs) — CRUD for the `providers` table (per-app
  config snapshots keyed by `(id, app_type)`). Implements
  `get_all_providers`, `get_current_provider`, `get_provider_by_id`,
  `save_provider`, `delete_provider`, `set_current_provider`.
- [`proxy_providers.rs`](proxy_providers.rs) — CRUD for `proxy_providers`
  (upstream API credentials, models, mappings, breaker config). Defines
  the `ProxyProvider` row, the `CircuitBreakerConfig`, and the
  `ModelMapping`.
- [`proxy_config.rs`](proxy_config.rs) — singleton row read / write for
  `proxy_config`. Hosts `DEFAULT_PROXY_PORT` (`8989`).
- [`routes.rs`](routes.rs) — CRUD for `routes`. A route maps an alias
  (client-requested model) to an ordered list of `(provider_id, model)`
  hops stored as JSON.
- [`settings.rs`](settings.rs) — CRUD for `app_meta`. Defines the
  well-known setting keys (`codex_cli_home`, `outbound_proxy`,
  `ccswitch_override_enabled`, `ccswitch_block_enabled`,
  `sync_with_cc_switch_enabled`, `ccswitch_last_status_json`,
  `proxy_pinned_provider`).
- [`backup.rs`](backup.rs) — full-table snapshots in `backups.data`
  (providers, proxy_providers, routes, proxy_config, mcp_servers, prompts,
  skills). Ephemeral tables are intentionally excluded.
- [`mcp.rs`](mcp.rs) — `mcp_servers` table. Stores MCP server definitions
  as JSON `server_config`.
- [`skills.rs`](skills.rs) — `skills` table. Tracks installed skills
  (per-app enabled flags, repo metadata, content hash). Filesystem symlink
  layer lives outside this DAO.
- [`sessions.rs`](sessions.rs) — `sessions` table. Tracks chat session
  history per app; `source_path` points at the original JSONL on disk.
- [`usage.rs`](usage.rs) — `usage_records` table plus aggregate methods
  (`get_usage_summary`, `get_model_stats`, `get_provider_stats`).
- [`sync_config.rs`](sync_config.rs) — `sync_config` table. Unified row
  schema covers both S3 and WebDAV (cc-switch keeps them separate).
- [`vendors.rs`](vendors.rs) — `vendors` table. A vendor is a unified
  upstream supplier (one name + one API key) that may serve multiple
  protocols. Providers reference vendors by id.

## Key contracts

- Every DAO method is `impl Database`. They take `&self`, lock the
  connection through `lock_conn!`, and return `Result<T, AppError>`.
- Serialization uses camelCase via `#[serde(rename_all = "camelCase")]`
  so DTOs match the IPC contract used by the React frontend.
- No transaction spans a DAO boundary unless explicitly noted; callers
  that need atomicity must own the lock manually.

## See also

- [`../schema.rs`](../schema.rs) — table definitions and migrations.
- [`../seed.rs`](../seed.rs) — first-run demo data.
- [`../commands/`](../../commands/README.md) — IPC commands that call these
  DAOs.
- [Root README](../../README.md).