# `src-tauri/src/services/import_cc_switch/` — cc-switch SQLite import

One-shot importer for providers stored in a cc-switch SQLite database
(`~/.cc-switch/cc-switch.db`, overridable with `CC_SWITCH_DB_PATH`). The
cc-switch `providers` table is schema-compatible with AAG's (same column
order and types), so the importer reads every column, fills every AAG
`Provider` field, and preserves cc-switch's `in_failover_queue` order in
the auto-built route hops.

## Anchor files

- [`mod.rs`](mod.rs) — full implementation. Public entry points are
  `import_from_db(db, source_path)` (used by the `aag-setup` helper) and
  the IPC command `import_cc_switch_providers` (the in-app button).
  Exports `cc_switch_db_path` so both call sites agree on the default.

## Key contracts

- The importer is idempotent. Already-present `(id, app_type)` rows are
  skipped (`INSERT OR IGNORE`), not overwritten.
- It SELECTs every column, so subsequent schema additions in cc-switch
  surface as new `Provider` fields without code changes.
- `in_failover_queue=0` rows are imported but excluded from auto-built
  route hops (cc-switch's `claude-official` entry behaves this way).
- Codex providers without an API key (e.g. ChatGPT-login `codex-official`,
  `default`) are skipped and counted as `ImportReport.skippedNoKey` so
  the UI toast can surface the count.
- The `[1M]` Claude context suffix is preserved verbatim — it is part of
  the upstream model id for 1M-context vendors.
- When `sync_with_cc_switch_enabled = "1"`, `proxy_start` runs an
  automatic pass through the same importer so manual AAG changes still
  survive (skipped if `sync_locked = true`).

## See also

- [`../detect/`](../detect/README.md) — coexistence signals that affect
  how aggressively the importer behaves.
- [`../commands/proxy.rs`](../../commands/proxy.rs) — IPC entry point.
- [Root README](../../README.md).