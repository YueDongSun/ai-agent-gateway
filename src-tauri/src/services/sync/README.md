# `src-tauri/src/services/sync/` — S3 and WebDAV cloud sync

Experimental cloud transports for the AAG SQLite database
(`~/.aag/aag.db`). The sync feature is opt-in; both transports use the
`sync_config` row written by [`dao/sync_config.rs`](../../database/dao/README.md)
and define a future single-object protocol. Database upload/download is
currently disabled until an authenticated client-side encrypted payload format
exists; connection checks alone do not transfer the database.

## Anchor files

- [`mod.rs`](mod.rs) — module entry point.
- [`webdav.rs`](webdav.rs) — HTTPS-only WebDAV endpoint/path validation and a
  credential check. Historical PUT/GET helpers fail before file or network
  transfer until the encrypted payload protocol exists.
- [`s3.rs`](s3.rs) — S3 HTTP transport with AWS Signature Version 4
  signing on top of `reqwest`. Supports both virtual-hosted style
  (`{bucket}.s3.{region}.amazonaws.com/{key}`) and custom S3-compatible
  endpoints (path style). Bucket and region values are validated before a
  signed connection check; historical PUT/GET helpers are disabled.

## Key contracts

- Protocol v1 is retained as historical design code only. Public builds fail
  closed with `cloud_sync_encryption_required` before database transfer.
- Remote endpoints require HTTPS. Plain HTTP is accepted only for explicit
  loopback development checks.
- There is no registered desktop IPC or UI action that transfers the database.
  Code-level connection checks transfer no database bytes.
- Local state is never replaced by the disabled download helpers.

## See also

- [`../../database/dao/sync_config.rs`](../../database/dao/README.md) —
  `sync_config` row schema shared by both transports.
- [Root README](../../README.md).
