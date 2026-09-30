# `src-tauri/src/database/` — SQLite connection, schema, seed, DAOs

The persistent source of truth for the desktop application. The
`Database` struct wraps a `Mutex<rusqlite::Connection>` (rusqlite's
`Connection` is not `Sync`) so it can be shared as Tauri managed state and
cloned across commands cheaply.

## Anchor files

- [`mod.rs`](mod.rs) — `Database::init()` opens the database file at
  `~/.aag/aag.db` (override via `AAG_CONFIG_DIR`), enables foreign keys,
  secure deletion and WAL journal mode, creates tables, applies migrations,
  explicitly secures the main/WAL/SHM files, and seeds the mock failover demo
  on first run. Also exposes
  `Database::memory()` for tests.
- [`schema.rs`](schema.rs) — `CREATE TABLE` statements for every
  `providers`, `proxy_providers`, `proxy_config`, `routes`, `app_meta`,
  `vendors`, `backups`, `mcp_servers`, `skills`, `sessions`,
  `usage_records`, `sync_config`, and migration branches that bring older
  schemas up to the current version.
- [`seed.rs`](seed.rs) — `Database::seed_proxy_if_empty()`. Inserts the
  `proxy_config` singleton (`127.0.0.1:8989`), two mock providers
  (`mock-primary` with a low breaker threshold, `mock-fallback`), and
  three demo routes so a fresh install has a working gateway.
- [`dao/`](dao/README.md) — per-table DAO modules with `impl Database`
  blocks. Every `SELECT` / `INSERT` / `UPDATE` lives here; commands and
  services do not embed SQL.

## Key contracts

- The current schema version lives at `SCHEMA_VERSION` in [`mod.rs`](mod.rs).
  Bump it on every schema change and add a migration branch in `schema.rs`.
- `Database` derives `Clone`. The inner `Arc<Mutex<Connection>>` is shared
  across every clone.
- All access is serialized through `lock_conn!(self.conn)`; the macro maps
  poison errors to `AppError::Lock`.
- `Database::init` is the only entry point that opens a file. All other
  call sites assume the database is already initialised.
- The database directory is owner-only before SQLite opens. POSIX creates the
  directory as `0700` and the database, WAL and SHM as `0600`; Windows uses a
  protected owner-only DACL and refuses owner mismatch or reparse paths.
- Backup rows contain plaintext JSON snapshots of credential-bearing tables.
  Creation automatically retains at most ten rows, and SQLite secure deletion
  is enabled so removed cells are overwritten when SQLite reuses their pages.
  This reduces retained copies but is not encrypted storage or guaranteed
  media sanitisation.

## See also

- [`dao/`](dao/README.md) — per-table DAOs.
- [`commands/`](../commands/README.md) — IPC surface that calls these DAOs.
- [Root README](../../README.md).
