# `src-tauri/src/services/` — Domain logic above the database

Business logic that sits between the DAO layer and the Tauri command
layer. Each submodule owns a coherent concern: per-agent live config
writers, cc-switch coexistence detection and import, and S3 / WebDAV
synchronization of the SQLite database.

## Anchor files

- [`mod.rs`](mod.rs) — module declarations: `detect`, `import_cc_switch`,
  `provider`, `sync`.
- [`provider/`](provider/README.md) — per-app live config adapters.
- [`detect/`](detect/README.md) — external-app coexistence detection.
- [`import_cc_switch/`](import_cc_switch/README.md) — explicit one-shot
  cc-switch import and the Claude provider auto-sync path.
- [`sync/`](sync/README.md) — S3 and WebDAV sync of the SQLite store.

## Key contracts

- Services may call DAOs and read / write files outside the database, but
  they must not register Tauri commands and must not embed SQL.
- Tauri commands call services for business logic and DAOs for direct
  reads.
- All live agent file writes go through the matching `services/provider/`
  adapter, which performs the atomic write and the backup-before-overwrite.

## See also

- [`../commands/`](../commands/README.md) — IPC commands that call these
  services.
- [`../database/`](../database/README.md) — DAO layer.
- [Root README](../../README.md).
