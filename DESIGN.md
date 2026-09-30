# AAG Design

This file records durable design decisions and the migration summary. Current
module ownership, data flow, and runtime contracts live in
`docs/architecture.md`; current state and evidence live in `PROGRESS.md`. The
phase-by-phase migration record is preserved in `docs/PHASE-ARCHIVE.md` and
`docs/progress/phase_22_23_history.md`.

## Goals

- Provide one local gateway for OpenAI and Anthropic client protocols.
- Route requests through ordered upstream providers with circuit-breaker
  failover.
- Manage agent integrations without coupling the Node and desktop runtimes.
- Give the desktop app a durable SQLite source of truth and a safe browser demo.
- Keep release, native acceptance, and external-provider evidence explicit.

## Non-goals

- Replacing the legacy Node gateway or CLI.
- Transparent mid-stream recovery after a provider has emitted its first chunk.
- Treating browser-demo or mock-provider results as real-provider validation.
- Sharing private databases or runtime state across projects.
- Claiming signing, installation, upgrade, or updater readiness before separate
  acceptance evidence exists.

## Canonical protocol contract

OpenAI Chat Completions is the internal format for both runtimes. The router
does not know whether a request arrived through OpenAI or Anthropic.

- `/v1/chat/completions`: OpenAI request and response shape.
- `/v1/messages`: Anthropic request and response shape at the HTTP boundary.
- `/v1/responses`: OpenAI Responses shape (Codex CLI native protocol).
- `/v1/models`: model catalog for clients.
- `/health`: provider and breaker health.

Anthropic conversion occurs at the edge and at an Anthropic-native upstream
provider. Current conversion behavior is text-oriented; tool and multimodal
fidelity must be treated as an explicit compatibility concern. Anthropic
clients routed to Anthropic-compatible upstreams keep the original source
envelope so thinking blocks, signatures, and tool order survive without
appearing on OpenAI wire shapes.

Upstream base URLs are normalized through one shared helper per runtime
(Rust `proxy/endpoint.rs`, Node `gateway/providers/endpoint.ts`): a stored
base may be an API root, a versioned root, or a full endpoint, and every
consumer resolves it identically.

## Routing and failure semantics

1. Resolve the requested model to an explicit alias route.
2. If no alias matches, use an enabled provider that advertises the model.
3. Walk hops in order, consulting each provider breaker.
4. Advance only on network errors, HTTP 429, or HTTP 5xx responses.
5. Propagate 4xx errors without failover.

The Rust router snapshots providers and routes when `proxy_start` runs. A
restart is required after persistent route or provider changes. Streaming
failover probes the first chunk before committing and cannot transparently
restart a stream after commitment. Providers whose upstream targets AAG's own
listen address are skipped in every provider-build path (self-loop guard).

## Persistence decisions

- The Rust database at `~/.aag/aag.db` (schema version defined in
  `src-tauri/src/database/mod.rs`) is the desktop source of truth; all SQL
  lives in DAO modules and Tauri commands never become a second persistence
  layer. The UI reads and writes through typed IPC only.
- The Node runtime keeps its own Zod-validated JSON configuration at
  `~/.ai-agent-gateway/config.json`; the two runtimes are compatible surfaces,
  not a shared runtime.
- Live agent files are backed up before replacement and written atomically.
- API keys and auth tokens are never logged, committed, or copied into docs.

## Agent integration

The Node adapter family remains in `src/agents/`; the desktop adapter family
is in `src-tauri/src/services/provider/`. Each adapter owns its target file
shape, path detection, validation, atomic write, and backup behavior. AAG does
not assume that every app uses the same provider schema. The Claude workflow
can import and synchronize cc-switch provider data; cc-switch remains the
owner of the Claude Code live configuration where that integration is enabled.

## Historical migration summary

The project moved from a Node-only gateway toward a Tauri desktop product in
bounded phases: database and provider CRUD, proxy/failover, streaming and
conversion, backup/MCP/skills, sessions/usage/sync, deep links/tray/update,
agent coexistence, and UI/release hardening. The migration is now in the
desktop preview closure stage; consult `PROGRESS.md` rather than this summary
for the next action.

## Open acceptance items

The current preview still requires separate evidence for native installation,
tray behavior, deep links, signing, updater delivery, upgrades, macOS/Linux
execution, live agent switching, real upstream requests, and measured row
rendering performance. These are acceptance boundaries, not implied failures of
the local unit or browser-demo gates.
