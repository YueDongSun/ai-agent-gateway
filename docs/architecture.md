# AAG Architecture

This document is the current architecture reference for AI Agent Gateway
(AAG). Historical migration decisions remain in `DESIGN.md`; current test and
release evidence remains in `PROGRESS.md`.

## Product boundary

AAG has two independently runnable surfaces:

- The Node gateway and CLI in `src/` remain the headless compatibility path.
- The Tauri 2 desktop application in `src-tauri/` and `ui/` is the primary
  product surface.

The desktop application owns the SQLite configuration store, provider and
route management, local proxy lifecycle, browser demo, and release packaging.
The Node surface must not be removed or silently made dependent on Tauri.

## Runtime modules

### Node compatibility surface

| Module | Responsibility |
| --- | --- |
| `src/index.ts` | CLI entry point and interactive TUI selection |
| `src/cli/` | Commander commands and interactive prompts |
| `src/config/` | Zod schema, JSON persistence, and mutation helpers |
| `src/gateway/` | Hono HTTP edge, canonical format, routing, providers, SSE, and breakers |
| `src/agents/` | Agent config adapters and gateway/direct/profile switching |
| `tests/` | In-process Vitest coverage for the Node surface |

The canonical internal request and response shape is OpenAI Chat Completions.
Anthropic conversion happens at the HTTP edge and inside the Anthropic-native
provider. Routing and circuit-breaker decisions are made on the canonical shape.

### Tauri backend

| Module | Responsibility |
| --- | --- |
| `src-tauri/src/main.rs` | Native executable entry point |
| `src-tauri/src/lib.rs` | Tauri setup, managed state, plugin registration, and command registration |
| `src-tauri/src/startup.rs` | Release autostart registration and normal versus tray-only launch state |
| `src-tauri/src/tray.rs` | System tray icon, show/hide/quit actions, and close-to-tray behavior |
| `src-tauri/src/diagnostics.rs` | Log policy, temporary debug state, panic capture, and safe summaries |
| `src-tauri/src/commands/` | IPC orchestration grouped into provider, proxy, launcher, and settings concerns |
| `src-tauri/src/database/` | SQLite connection, schema migrations, seed data, and DAOs |
| `src-tauri/src/proxy/` | Axum HTTP server, canonical types, conversion, routing, providers, and breakers |
| `src-tauri/src/services/provider/` | Per-agent live configuration adapters and platform guards |
| `src-tauri/src/services/provider/config_paths.rs` | Read-only OS-aware resolution shared by provider status, client launch and configuration replacement |
| `src-tauri/src/services/launcher/` | Platform discovery, terminal/editor launches, bounded probes, child reaping and launcher DTOs |
| `src-tauri/src/services/detect/` | cc-switch coexistence detection and diagnostics |
| `src-tauri/src/services/import_cc_switch/` | Explicit cc-switch SQLite import and Claude provider auto-sync |
| `src-tauri/src/services/dsh/` | Isolated DSH lifecycle, plugin transactions and redacted upstream resolution |
| `src-tauri/src/services/mcp/` | Validated MCP registry management, redacted reviews, and backup/ACL-preserving Claude sync |
| `src-tauri/src/dsh_window.rs` | Restricted native DSH runtime window; no AAG command authority |
| `src-tauri/src/dsh_market_window.rs` | Bundled marketplace window with public catalog/source access only |
| `src-tauri/src/services/sync/` | S3 and WebDAV synchronization |
| `src-tauri/src/e2e.rs` | Full-stack Rust verification with disposable paths |

The backend uses `AppError` for domain and I/O failures. Tauri commands expose
`Result<T, String>` at the IPC boundary. All SQL belongs in DAO modules and all
live configuration writes use atomic-write helpers and backup-before-overwrite
semantics.

Launcher commands schedule synchronous filesystem/process work on the blocking
worker pool. Status DTOs match the frontend's camelCase contract, and environment
values are redacted in launch receipts. Platform-specific behavior and isolated
editor profiles are documented in [platform launchers](platform_launchers.md).

### Desktop lifecycle

The main window is created hidden so it cannot flash during login startup. A
normal desktop launch shows and focuses the window after native setup. A
packaged release launch registered by the autostart plugin receives
`--aag-autostart` and remains tray-only until the user clicks the tray icon.
The development command `npm run tauri:dev` still shows the same React UI, but
does not register a development executable as a login startup entry.

The tray icon uses `src-tauri/icons/tray_icon.png`. Left-click shows and focuses
the main window; Show, Hide, and Quit are also available from the tray menu.
Closing the main window is intercepted and hides it to the tray. The tray is a
required startup dependency because it is the recovery path for a hidden UI.

### React frontend

| Module | Responsibility |
| --- | --- |
| `ui/src/App.tsx` | Application shell, app switcher, global status, theme, locale, and lazy page loading |
| `ui/src/components/app/AppPage.tsx` | Shared per-app provider, route, gateway, and launcher page |
| `ui/src/components/app/proxy_pool_state.tsx` | Upstream pool data/actions contexts |
| `ui/src/components/app/routes_state.tsx` | Route data/actions contexts |
| `ui/src/components/app/` | Gateway panel, provider list/chain, route editor, pool, and launchers |
| `ui/src/components/mcp/` | Lazy MCP registry, private-definition editing, and reviewed client configuration |
| `ui/src/components/diagnostics_modal.tsx` | Global diagnostics controls and local log access |
| `ui/src/lib/api/` | Typed Tauri IPC wrappers and shared DTOs |
| `ui/src/lib/mock/` | Synthetic IPC backend for browser demo mode only |
| `ui/src/hooks/` | Polling, cancellation, and gateway status reconciliation |
| `scripts/ui_smoke/` | Owned browser runtime, CDP transport, page checks, and regression scenarios |
| `scripts/verification/` | Shared feature command catalog and isolated foreground verification runner |
| `scripts/desktop_environment.mjs` | Bounded local prerequisite probes used by desktop doctor and automatic verification preflight |
| `scripts/mcp/` | Read-only stdio companion; static workflows and verification plans, no live-state or write authority |

`AppPage` owns page orchestration. Pool and route state have separate context
modules. Consumers must receive current state and callbacks; stale closures are
not an acceptable rendering optimization. Page modules are lazy-loaded from
`App.tsx` so the initial bundle contains the shell rather than every page.

The shell uses a warm light/dark workspace theme with grouped desktop navigation,
compact narrow-screen navigation, keyboard skip access, and English as the
default locale. Explicit saved locale/theme choices are preserved. AAG keeps its
own product identity; the Claude-inspired styling does not imply affiliation.

## Data flow

```text
AI client or agent
        |
        v
HTTP edge (Node Hono or Rust Axum)
        |
        v
Anthropic/OpenAI edge conversion
        |
        v
Canonical OpenAI request
        |
        v
Route alias -> ordered hops -> provider circuit breaker
        |
        v
OpenAI or Anthropic upstream
```

The Rust proxy takes a configuration snapshot at `proxy_start`. Provider and
route edits therefore require a proxy restart to affect routing. Streaming
failover is one-shot: the router probes the first chunk before committing; a
later stream failure is returned to the client.

For Anthropic clients routed to Anthropic-compatible upstreams, canonical
objects retain an internal source envelope that is excluded from every public
OpenAI wire. The provider changes only the routed model and stream flag, then
the edge relays the original Anthropic body or event. This preserves thinking,
redacted thinking, cache controls, tool-block order, opaque signatures, and
`signature_delta`. Cross-protocol routes still use the explicit canonical
converter. A same-provider compatibility retry is allowed once only after an
explicit thinking/signature validation error.

Every large JSON POST ingress (`/v1/messages`, `/v1/responses`,
`/v1/chat/completions`, and the `/v1beta` Gemini `generateContent`,
`streamGenerateContent`, and `countTokens` methods) shares one route-local
32 MiB JSON body cap so base64 image blocks, Codex/Claude long sessions with
large tool outputs, and Gemini inline data can pass the local edge without
disabling body protection across the proxy. Requests above the cap remain
rejected before routing; the configured upstream may impose a smaller
platform-specific limit.

Each provider owns one reusable HTTP client for the lifetime of the proxy
snapshot. Connection/header time, first event, completion, token usage, and
client disconnects are recorded against a request correlation id without
logging request or response content.

## Diagnostics and crash evidence

Production logging defaults to `Info`. The global Diagnostics dialog can raise
AAG-owned targets to `Debug` for 30 minutes; dependency targets remain at
`Warn`. The deadline is stored in `app_meta`, so a restart restores only the
remaining interval. Logs use local time, rotate at 2 MiB, and retain five
files in the Tauri application log directory.

Native panic/startup hooks, Tauri lifecycle events, tray/window actions, proxy
request stages, React error boundaries, and unhandled browser errors feed the
same local evidence boundary. Frontend IPC sends only a fixed error category,
bounded message/stack character counts, and a correlation id; raw messages and
stacks never cross the native boundary. Provider, route, and model identifiers
are represented by truncated SHA-256 references in diagnostics. External
errors use stable codes and generic messages with status, stage, retriable
classification, correlation id, and response byte count where available.
Prompts, responses, tool payloads, credentials, absolute paths, full upstream
URLs, domains, email addresses, and IP addresses are never diagnostic fields.
There is no remote telemetry or memory-dump upload.

## Persistence and ownership

- AAG database: `~/.aag/aag.db`, override with `AAG_CONFIG_DIR`.
- Node compatibility config: `~/.ai-agent-gateway/config.json`, also
  overridable with `AAG_CONFIG_DIR`, plus one exact rolling pre-overwrite
  backup.
- Agent paths: derived from the user home directory, with `AAG_HOME_DIR` only
  for controlled tests and portable runs.
- Schema version: v18.
- Provider rows are keyed by `(id, app_type)` and route hops reference proxy
  provider ids.
- `app_meta` stores device-level settings such as isolated Codex CLI state.
- Temporary diagnostics state uses the `diagnostics_debug_until` app-meta key;
  it does not require a schema migration.
- Backups and sync metadata are separate from provider and route state.

Credential-bearing local state has one private filesystem boundary. Before
writing bytes, AAG rejects symlink/reparse components and owner mismatch,
creates private directories, uses exclusive random same-directory temporary
files, preserves tighter safe permissions, and verifies the result after
atomic replacement. POSIX uses `0700` directories and at most `0600` files.
Windows uses protected current-owner-only DACLs without elevation. SQLite's
main, WAL and SHM files use the same boundary; in-database plaintext backups
are capped at ten and use SQLite secure deletion.

These controls provide account-level isolation, not encryption at rest.
Provider keys, MCP environment/header values, proxy tokens, and backup payloads
remain plaintext to the AAG process. A compromised process running as the same
OS account can still read them; secret references should be preferred where a
provider integration supports them.

The backend owns persistent state. The UI owns view state and calls typed IPC
wrappers; it must not access SQLite or another project's private database.

## Compatibility boundaries

- Node and Rust proxies both use OpenAI-shaped canonical types internally.
- OpenAI clients use `/v1/chat/completions`; Anthropic clients use `/v1/messages`.
- Empty proxy auth disables authentication; configured auth accepts Bearer or
  `x-api-key` according to the server surface.
- Retriable failures are network errors, 429, and 5xx responses. 4xx errors
  propagate without failover.
- Browser demo data is synthetic and is not evidence of native IPC, real
  upstreams, live agent writes, signing, or installation behavior.

## Change ownership

Use the smallest module boundary that owns the behavior:

- Provider persistence: `database/dao/providers.rs` and provider IPC commands.
- Proxy lifecycle and route persistence: `commands/proxy.rs` and proxy DAOs.
- Agent file formats: one adapter under `services/provider/`.
- UI state lifetime: the relevant AppPage context or hook.
- Browser evidence: `scripts/ui_smoke/`, never ad-hoc calls to an unrelated server.

Do not introduce a shared cross-language library, move private state, or merge
the Node and Tauri runtimes without a separate compatibility plan.

## Managed DSH extension

The DSH page is a separate lazy-loaded management surface. It reuses saved
Claude providers and the running gateway without editing native Claude files.
It also contains an AAG-owned Claude-style workspace shell for reviewing
provider/model selection; the actual prompt execution remains in the isolated
native DSH WebUI.
Its Rust service owns a dedicated filesystem profile below the AAG configuration
directory; SQLite remains authoritative for upstream provider records.
See [Managed DSH integration](dsh_management.md) for supported runtime versions,
credentials, transaction boundaries and deferred capabilities.

Within that service, runtime_environment.rs owns managed directory preparation
and filtered child environments, owned_process.rs owns child lifetime and
descendant cleanup, and runtime.rs coordinates the DSH instance/readiness.
Plugin transactions share the environment and process adapters rather than
depending on the DSH lifecycle for those responsibilities. The managed home
isolates configuration; it does not restrict filesystem permissions.

The DSH Plugin Marketplace is a separate bundled route and window. It reads a
public, commit-bound catalog and exposes only catalog read/refresh and validated
source links. It does not mount the manager shell or receive provider/runtime
permissions. See [DSH marketplace](dsh_marketplace.md).

## Automatic DSH setup and stream completion

The DSH manager passively discovers installed runtime metadata and prepares
review-bound setup plans. Application writes only managed settings/profile
state, preserving existing provider/model choices and native app configuration.
Automatic exact-model routing uses surface priority then stable provider IDs;
explicit routes and pins remain unchanged. Stream attempts settle success only
after real completion; errors and cancellation retain distinct outcomes.
See [plug-and-play contracts](dsh_plug_and_play.md).

## MCP management and companion boundaries

The existing `mcp_servers` table remains the registry source of truth. Manager
IPC validates definitions and returns only redacted summaries. New definitions
cannot overwrite existing IDs; updates use revision checks. Client flags change
only the registry. Claude synchronization requires a one-use, expiring preview
bound to registry/client snapshots, explicit confirmation, a retained backup,
and atomic replacement. Windows replacements preserve the existing owner/group/
DACL before writing private content. Codex is redacted preview-only.

The separately runnable stdio companion imports only a static shared workflow
and verification catalog. It cannot call manager IPC, read live state, launch a
server, execute a verification plan, or approve a write. The local CLI runner is
an explicit user-invoked execution surface, not an MCP tool or recurring job.

See [MCP management](mcp_management.md), [companion and automation](mcp_companion.md),
and [DSH hardening](dsh_hardening.md) for contracts and limitations. Local and
synthetic evidence for this feature slice is recorded separately from historical
release status in [workspace validation](reviews/mcp_dsh_workspace_validation.md).
