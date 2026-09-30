# MCP management

AAG provides a CC-Switch-style registry for MCP server definitions with separate
Claude Code and Codex enable flags. The existing `mcp_servers` SQLite table is
the only registry; this feature does not create a second store, launch an MCP
server, install packages, authenticate to a service, or probe the network.

## Product boundary

- Claude Code: explicit preview, review, and apply to user-scope MCP entries in
  `~/.claude.json`. This is not `.claude/settings.json` and does not alter
  project trust, permissions, providers, models, or project-local MCP entries.
- Codex: redacted TOML review only. AAG does not read or overwrite Codex files,
  including the shared desktop `~/.codex/config.toml` or isolated CLI homes.
  Manual integration requires restoring private values and choosing the correct
  user/project or isolated CLI configuration. A redacted preview is not a usable
  configuration export.
- Registry edits and enable toggles are local database operations. Nothing is
  automatically synchronized on refresh, page navigation, save, or startup.
- This manager is distinct from an MCP server exposing AAG tools to an agent.
  The repository assistance server is documented separately and cannot invoke
  native MCP synchronization or configuration mutations.

## Definitions and secrets

New IDs use lowercase `snake_case`, start with a letter, and are at most 64
ASCII characters. Definitions are bounded to 32 KiB, 64 arguments, 64 map
entries, and 4 KiB per value. At most 128 definitions may be managed. Names
are bounded to 128 bytes and descriptions to 1024 bytes.

Existing imported IDs retain their established ASCII letters, digits, hyphens,
and underscores (up to 128 characters). Saving or synchronizing an existing
entry does not silently rename it. The stricter naming policy applies to new
AAG-controlled records only.

The shared validated subset is:

```json
{
  "type": "stdio",
  "command": "node",
  "args": ["/path/to/trusted/server.js"],
  "env": {"SERVICE_TOKEN": "${SERVICE_TOKEN}"}
}
```

```json
{
  "type": "http",
  "url": "https://example.test/mcp",
  "headers": {"Authorization": "Bearer ${SERVICE_TOKEN}"}
}
```

The `streamable-http` alias is normalized to `http`. A missing `type` is
inferred from `url` or normalized to `stdio`. Remote endpoints require HTTPS;
HTTP is supported only for explicit loopback hosts. URL user information and
fragments, unknown definition fields, control characters, shell command
strings, shell executables, malformed map keys, non-string arguments, and
literal `[REDACTED]` placeholders are rejected. Commands are a single executable
name or absolute path with separate arguments. AAG does not evaluate arguments.
Runtime interpreters and package runners still execute code when a client
starts them: validation is a syntax boundary, not a trust/security audit.

Do not paste credentials into metadata, commands, or argument arrays. Private
definition values are stored in the existing local SQLite database; it is not
an encrypted credential vault. Prefer client-supported environment references.
Claude expansion syntax is not automatically translated into Codex environment
forwarding semantics; inspect and adapt the redacted Codex review manually.

Read APIs return redacted definitions only: all argument values, environment
values, header values, and the entire URL are masked. Absolute executable paths
are reduced to the executable basename. Unknown/invalid imported definitions
are entirely masked and reported with a stable validation error. Updating
metadata with a missing/null `server_config` preserves the previous private
definition; replacing it requires a complete valid definition. The UI must
never submit a redacted summary as replacement configuration.

Add uses `create_only: true` and rejects an existing ID. Every summary includes
an opaque revision; updates require the matching `expected_revision`. A stale
editor cannot restore old enable flags or replace a newer private definition.
If the record was deleted while its editor was open, the old revision also
prevents recreating it; only a fresh explicit Add may create a new definition.
The checks run under the same service lock as persistence and review invalidation.

## Claude synchronization transaction

1. An explicit preview reads a bounded, regular, non-linked `.claude.json` file
   and computes additions, replacements, removals, and unchanged entries.
2. Registry ID `local_tools` maps to live key `aag_local_tools`. Only namespaced
   keys represented in the current registry are changed. Existing keys with
   these exact names are replaced/removed as listed in the review; the prefix
   is a namespace, not proof of historical ownership.
3. The preview returns only redacted managed entries and change metadata, not
   the surrounding client document (which can contain authentication tokens).
4. A cryptographically random, one-use review token expires after five minutes.
   It binds the entire registry snapshot, exact original client bytes, target,
   and computed replacement. At most 16 pending reviews exist in memory.
5. Applying consumes the token before checking or writing. A registry mutation,
   changed client file, expired token, missing token, duplicate apply, or
   conflicting writer prevents the write and requires a fresh review.
6. AAG obtains an exclusive `.claude.json.aag_lock`, writes a complete private
   temporary file, flushes it, and retains an exact private backup beside the
   original as `.claude.json.aag_backup_<review_token>` before atomic replacement.
   A second content check occurs immediately before replacement, followed by
   read-back verification. Owned temporary/lock files are cleaned up normally.

Close Claude Code before applying. The lock serializes AAG writers, but another
application does not honor that lock; there is no cross-application transaction.
AAG preserves all unrelated parsed JSON values, but whitespace/object formatting
may change. Backups can contain the original client's credentials and must stay
private. Existing linked/reparse-point targets or ancestors are rejected. A
custom `CLAUDE_CONFIG_DIR` is refused unless the explicit `AAG_HOME_DIR`
test/portable override is active; AAG does not guess an alternate target.

On Windows, existing owner, group, DACL bytes, and inheritance protection are
applied and verified on both empty replacement and backup files before any
credential-bearing bytes are written. Missing/null or invalid DACLs, unreadable
security descriptors, insufficient ordinary owner/ACL permissions, and unequal
read-back security fail closed. AAG never elevates privileges or takes ownership
to force a write. Audit SACLs and non-ACL filesystem features are not copied by
this adapter. New Windows client files receive a verified protected owner-only
DACL before private bytes are written. Exclusive sharing starts at creation
and stays in force through ACL verification and writing, preventing another
reader from retaining a handle during the inherited-ACL interval. Owned handles
close before cleanup removes temporary or lock files. Unix backup files start
with mode 0600 and replacement files preserve the original mode.
Read-only original files are refused before creating a lock, backup, or temporary
file; AAG never removes their read-only protection to force synchronization.

If the process is interrupted, an existing lock blocks further writes. Inspect
the original, backup, and any pending file before manually resolving the lock.
AAG never deletes another writer's lock or silently restores over newer edits.
An atomic-replace or read-back failure retains the backup for recovery and is
reported as failure, not success. No automatic retry occurs. The bounded local
apply is one non-cancellable critical section. Closing a review without applying
discards its token and releases its slot without filesystem effects; expiry is
the fallback if the frontend is interrupted before discard.

To remove a live server, first disable it for Claude, preview/apply its removal,
then delete the registry entry. Deleting only the registry never edits a live
file and deliberately leaves any existing client entry untouched.

## IPC contracts and authority

Only the trusted `main` manager window receives these commands. DSH runtime and
marketplace windows have neither the capability permissions nor dispatch
authority. UI wrappers do not access SQLite or client files directly.

| Command | Input | Effect |
| --- | --- | --- |
| `mcp_list_servers` | None | Redacted database summaries only |
| `mcp_save_server` | `input` with ID, name, optional private config, description, enable flags, `create_only`, and update `expected_revision` | Create-or-reviewed-update with collision/stale checks; invalidate reviews |
| `mcp_delete_server` | `id` | Registry deletion only; invalidate reviews |
| `mcp_set_enabled` | `id`, `client`, `enabled` | Registry flag only; invalidate reviews |
| `mcp_preview_sync` | `client`: `claude` or `codex` | Read-only review; optional expiring Claude token |
| `mcp_apply_sync` | `review_token` | One reviewed Claude write and private backup |
| `mcp_discard_review` | `review_token` | Idempotent in-memory review cancellation; no filesystem writes |

Public DTOs and command arguments use `snake_case`; the established internal
`McpServer` DAO serialization remains compatible. Definition JSON uses required
external keys. Error codes have a stable `mcp:` prefix and never include raw
client content, definitions, credentials, or parser source excerpts.

No command is exposed as an automatic agent approval or a generic file API.
Preview grants no execution authority on its own: the manager must show the
changes, client, warnings, and explicit apply confirmation. A repeat apply
cannot replay the consumed token. Start a new review after any failure.

## Verification and limitations

The focused Rust suite is `services::mcp::tests`, with disposable directories
and an in-memory SQLite database. It covers validation, secret masking, missing
and expired approval, stale client/registry content, duplicate apply, redacted
metadata editing, namespace isolation, exact backup, atomic-file cleanup,
interrupted locks, malformed/oversized JSON, persistence corruption, Codex
non-mutation, preview bounds, and restricted native subjects.

```text
cd src-tauri
cargo test --locked --offline --lib services::mcp::tests -- --test-threads=1
```

This is local service/file integration evidence, not real-client acceptance.
Live Claude/Codex startup, OAuth, remote MCP availability, package installation,
and cross-platform native execution require separate explicit authorization.

## External contract references

The implementation was checked against official documentation on 2026-08-31:

- [Claude Code MCP configuration and scopes](https://code.claude.com/docs/en/mcp)
  documents user-scope `.claude.json`, `mcpServers`, and stdio/HTTP definitions.
- [Codex MCP configuration](https://developers.openai.com/codex/mcp/)
  documents `config.toml`, `mcp_servers`, `env`, `http_headers`, and the shared
  client configuration boundary.
- [Windows security descriptor reading](https://learn.microsoft.com/en-us/windows/win32/api/aclapi/nf-aclapi-getnamedsecurityinfow)
  and [setting](https://learn.microsoft.com/en-us/windows/win32/api/aclapi/nf-aclapi-setnamedsecurityinfow)
  define the bounded owner/group/DACL adapter without privilege escalation.
