# Codex home migration and session authority

This work is in preparation. The approved Windows service qualification passed
with three distinct normal virtual accounts, a protected policy process, and
native reader/runtime app-server initialization with zero conversation
mutations. The two existing Codex homes have not moved, and managed session
sharing is not enabled. Qualification and synthetic fixture results do not
establish a production authority, native client interoperability, or immutable
live sessions.

The requested mirror destination is `~/.codex-cli`, alongside `~/.claude-cli`.
The previous mirror is `~/.aag/mirrors/codex`. The native, unmanaged `~/.codex`
home is outside this migration. The default mirror resolver already chooses
`~/.codex-cli`; a persisted `codex_cli_home` override still controls legacy
launches until an explicit migration commits its configuration cutover.

## Storage and ownership

The existing destination contains a distinct history. Replacing it with the
previous mirror, merging native SQLite files, or resetting login state would
not satisfy this migration. Both homes must be inventoried and backed up before
cutover; both native histories must remain addressable afterward.

The authority design uses separate normal Windows service accounts:

| Role | Responsibility | Content access |
| --- | --- | --- |
| Policy authority A | Own policy, admission, source generations, and recovery records | Own authoritative native objects; execute no model or tool work |
| Runtime B for one source generation | Run the native app-server and its tools | Write only that admitted source generation |
| Trusted reader C | Read native history through the native protocol | Read sealed content; keep its mutable index outside the source |
| CLI, Desktop, remote gateway | Present native history and send admitted requests | Metadata homes and owner-mediated reads |

The planned `~/.codex-cli` facade contains `.authority/sources/` and
`clients/cli`, `clients/desktop` metadata homes. Each native source retains its
format, native session/tree and thread identifiers, message order, attachments,
and opaque fields. The canonical index stores aliases and references; it does
not store a second copy of message or attachment bodies. Recovery backups and
quarantined generations remain inactive recovery material.

The source registry distinguishes a native session/tree identifier from a
native thread identifier. Repeated observations of the same owner and native
identifier are idempotent. Conflicting aliases or payload references are errors.
An unresolved write admission survives a process restart and requires terminal
native evidence before it can be released.

## Read-only transition

A read-only flag in an HTTP handler cannot protect files from an independent
native process or a tool. A source transition must stop new admissions, prove
native quiescence, and publish a new physical generation whose objects are
owned by A. The old generation is quarantined. Its previously opened handles
must never become handles to the new authoritative objects.

The read-only generation gives no writer grant to B. C reads the sealed native
source with a separate mutable metadata index. The registry rejects mutation
requests, tool replies without the exact active admission, caller-controlled
path/history/execution overrides, and unclassified native methods. A native
resume for a read-only source requires a stored-history projection rather
than a native continuation. None of these source contracts has yet been
qualified against a migrated live session.

Windows namespace and object leases protect copying and sealing critical
sections. Directory leases can also block native rename, archive, and SQLite
operations. Their fixture results are not permission to hold the same leases
through a native writer's lifetime; that interoperability remains a separate
qualification requirement.

## Client launch and legacy writers

Managed launch is mandatory when `codex_session_authority_enabled` is `1` or
any `.codex-cli/.authority` object exists. A marker or setting can close legacy
paths; it cannot certify an installation. A qualified launch needs the actual
policy service identity, committed installation, and protected pinned images.
Unavailable or malformed authority state fails before launch preparation and
does not fall back to an unmanaged app-server.

CLI uses the native remote transport. Desktop uses the protected
`CODEX_CLI_PATH` shim, which relays stdio to the authenticated loopback session
endpoint. Both use the accepting gateway's actual port and existing key in
process environment, with `history.persistence="none"`. The key is absent from
URLs, metadata files, and launch receipts. Metadata homes contain the model
catalogue, minimal provider configuration, and `aag_client_metadata.json`;
preparation rejects conversation, history, attachment, credential, and native
session database files there.

The database-aware provider, selection, catalogue, supplier, and instruction
sync paths refuse legacy Codex writes once managed mode is required. Low-level
auth, reset, automatic healing, configuration, and instruction writers also
refuse them when the authority object exists. Claude supplier preparation and
instruction synchronization continue independently.

## Recovery boundary

The whole-home migration engine records operation identity, original object
identity, complete inventories, backup receipts, phase, and pending intent.
Directory replacement uses no-replace operations. A changed object, unknown
occupied destination, or conflicting journal stops recovery for review.

Complete backup verification includes content, alternate streams, extended
attributes, timestamps, and security metadata. Robocopy can change source
access timestamps simply by reading. Its adapter therefore requires a durable
per-object metadata record and identity-bound reconciliation before an
interrupted backup can resume; omitting access times from equality is not an
acceptable repair. The ordinary full inventory check remains the final gate.

Do not run a live migration while backup fidelity, source-generation sealing,
writer exclusion, native history projection, and both-home admission remain
unqualified. Do not delete either existing home or its recovery material to
make a verification pass.

## Windows qualification procedure

`scripts/session_service_qualification.ps1` defaults to a read-only plan. Its
`-SelfTest` path runs in-memory receipt and recovery checks without loading
native code. `-Apply` creates exactly the normal virtual-account services and
protected empty directories named in a reviewed plan, then performs native
version and app-server initialization checks with zero conversation/model
mutations. `-Rollback` uses the same protected ownership journal; it must not
adopt an unrelated service, file, or directory.

The prepared qualification plan is an operator artifact under
`src-tauri/target/session_fabric/service_qualification_plan.json`. Its first
elevated application was rejected by automatic approval review before process
creation. The operator then explicitly approved the exact plan. Its application
succeeded: all three actual normal principals were verified, policy-process
protection covered seven threads, reader/runtime initialized the pinned native
app-server, and each receipt recorded zero conversation mutations. The source
binary transfer and current SCM account/running-state readback also passed.
The earlier rejected attempt remains part of the failure record. Neither
attempt migrated a live home or enabled a production session authority.

Qualification checks the service roles; it does not itself migrate data or
enable the production authority. Production source admission and client
acceptance follow only after the required checks succeed.

## Verification

Use an isolated `AAG_HOME_DIR`, `AAG_CONFIG_DIR`, and temporary directory for
source tests. Relevant filters include:

```text
cargo test --locked --offline --lib services::codex_home_migration::tests -- --test-threads=1
cargo test --locked --offline --lib services::codex_home_migration::native_storage -- --test-threads=1
cargo test --locked --offline --lib services::session_broker -- --test-threads=1
cargo test --locked --offline --lib services::session_authority -- --test-threads=1
cargo test --locked --offline --lib managed_authority_ -- --test-threads=1
```

The explicitly ignored shim process fixtures use a freshly built
`aag_session_authority.exe` supplied through `AAG_SESSION_SHIM_TEST_EXE`. They
run only synthetic localhost transports and do not qualify SCM or live clients.

Live acceptance must record each home's session, message, history, attachment,
metadata and ordering counts before and after migration; exact cross-client
native and canonical references; one active authoritative payload per object;
read-only denials through CLI, Desktop, remote and indirect tools; and recovery
after interrupted copy and cutover. Missing checks stay open.

Tailnet API publication has a separate lifecycle and authentication boundary.
See [Tailnet publication](tailnet_publication.md) for the configured port,
existing-key requirement, foreground Serve ownership, firewall preflight,
peer proof, and publication-stop checks.
