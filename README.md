# AI Agent Gateway (AAG)

AAG is a local gateway and configuration manager for AI coding tools. It
provides OpenAI and Anthropic compatible HTTP endpoints, ordered provider
failover, circuit breakers, and a Tauri desktop interface for managing routes
and agent integrations.

The current release line is the unsigned `v0.2.0` desktop preview. The Node
gateway and CLI remain independently runnable for headless use.

## What it provides

- OpenAI-compatible `POST /v1/chat/completions`.
- Anthropic-compatible `POST /v1/messages`.
- OpenAI Responses-compatible `POST /v1/responses` (Codex CLI native protocol).
- `GET /v1/models` and `GET /health`.
- Ordered route hops with retriable-error failover (network, 429, 5xx).
- Per-provider `closed`, `open`, and `half-open` circuit breakers.
- Prompt enhancement: expand a short Chinese requirement into a complete Chinese
  brief, then into one optimized English prompt, using the models already
  configured in the gateway.
- Provider, vendor, route, backup, MCP, skills, sessions, and usage
  management in the desktop UI.
- Additive supplier URL discovery: after a catalog refresh, AAG may read both
  same-origin OpenAPI 3.x documents for each configured source context,
  validate declared POST routes, reuse an existing API root, and append only
  missing protocol endpoints. Repeated runs are idempotent; existing endpoints,
  models, and admission records are unchanged.
  The EN/CN switch also changes discovery explanations, catalog-health labels,
  and the latest results, including reasons for skipped or excluded connections.
- cc-switch import and coexistence detection. Claude Code chains through AAG
  via cc-switch; Codex CLI runs natively against AAG in an isolated
  `CODEX_HOME` mirror.
- Claude Code, Codex, Copilot CLI, Gemini, Kilo, OpenCode, and Pi adapters with automatic platform config-path resolution; Antigravity remains detect-only
  surfaces where implemented.
- System tray with show/hide/quit and close-to-tray behavior.
- Lossless Anthropic thinking/signature relay for Anthropic-compatible
  upstreams, with normalized streaming usage and reusable upstream connections.
- Local product diagnostics with 30-minute Debug mode, bounded log rotation,
  request-stage timing, and native/frontend crash evidence.
- Deep links (`aag://switch/<id>`, `aag://provider/<id>`, `aag://mcp/<id>`,
  `aag://skill/<id>`).
- Experimental S3 and WebDAV connection helpers. Database transfer is disabled
  until an authenticated client-side encrypted payload format exists.
- Browser demo mode with synthetic IPC for safe UI exploration.

Streaming failover is one-shot: the first chunk is probed before the router
commits to a provider. A later stream error is returned to the client.

Runtime status separates listener reachability from configured model candidates
and live upstream verification. The desktop header identifies the responding
runtime, missing usable models, pending listener/authentication restart and failed
hot apply. Advertised models and configured candidates are not individually
tested models. The CLI reports an older unidentified AAG as unknown rather than
calling it Desktop. Set `AAG_DESKTOP_ENDPOINT` to a deliberately selected root
HTTP(S) endpoint for a non-default desktop port; credentials, query strings and
additional paths are rejected.

The Node `aag test` command preserves partial stream output and returns a nonzero
status for explicit errors, malformed/truncated streams, cancellation or missing
terminal success. Copilot switching preserves unrelated JSONC settings and exact
pre-write backup bytes; malformed existing content is left unchanged.

## Surfaces

| Surface | Start here | Owns |
| --- | --- | --- |
| Desktop | `npm run desktop:dev` | Tauri, SQLite, React UI, Rust proxy |
| Browser demo | `npm run ui:demo` | React UI with synthetic IPC |
| Node gateway | `npm run gateway` | Headless Hono gateway and JSON config |
| Node TUI | `npm run dev` | Legacy interactive CLI |
| Read-only MCP companion | `node scripts/mcp/aag_mcp.mjs` | Static AAG workflows and verification plans; no live state or writes |

See [the architecture guide](docs/architecture.md) for module ownership and
[the development guide](docs/development.md) for setup and verification.

## Quick start

Install both dependency sets from the repository root:

```text
npm ci --ignore-scripts
npm ci --prefix ui --ignore-scripts
```

Run the desktop application:

```text
npm run desktop:dev
```

Run the browser-only demo:

```text
npm run ui:demo
```

The browser demo uses synthetic provider data and does not send real upstream
requests or write live agent configuration.

## MCP workspace, verification, and DSH

The desktop UI includes an MCP registry for stdio and HTTP definitions; client
selections stay in AAG until reviewed (see [MCP management](docs/mcp_management.md)).
The read-only companion shares its feature catalog with the local verification
runner: `npm run verify:features -- --scope all`, add `--run` to execute the
trusted, isolated local checks (see [MCP companion](docs/mcp_companion.md)).
The managed DSH extension stays isolated from native agent files (see
[DSH hardening](docs/dsh_hardening.md)).

## Node gateway

The legacy Node gateway defaults to `127.0.0.1:8787` and reads
`~/.ai-agent-gateway/config.json`. Set `AAG_CONFIG_DIR` to use another config
directory.

```text
npm run gateway
npm run gateway -- --host 127.0.0.1 --port 9000
```

The Node HTTP surface is:

```text
GET  /health
GET  /v1/models
POST /v1/chat/completions
POST /v1/messages
```

When `gateway.authToken` is non-empty, `/v1/*` requires either an Authorization
Bearer token or `x-api-key` with the same value.

## Module map

Each subtree owns a coherent piece of the product and has a README at its root
describing what lives there and which files to read first. Ownership rules,
UI state boundaries, and test/build boundaries are collected in
[docs/project_structure.md](docs/project_structure.md).

- [src/](src/README.md) — legacy Node gateway, CLI/TUI, config store, and
  agent adapters. Independently runnable headless surface.
- [src-tauri/](src-tauri/README.md) — Tauri 2 desktop shell: SQLite, IPC
  commands, Axum proxy, services, native lifecycle, and tray.
- [ui/](ui/README.md) — React desktop UI plus the browser demo with
  synthetic IPC.
- [scripts/](scripts/README.md) — desktop dev/build runners, icon generator,
  release preflight, and the owned browser smoke runner.
- [tests/](tests/README.md) — Vitest suites for the Node surface and the
  desktop runner/build helpers.
- [docs/](docs/README.md) — architecture, development, release notes, reviews,
  historical phase archive.
- [.github/](.github/README.md) — agent instructions and CI workflow.

Repo metadata:


## Runtime paths

The Rust database is at `~/.aag/aag.db`. Provider, route, supplier, model, pin,
and public catalog edits hot-apply to new requests; in-flight requests keep the
snapshot they started with. Listener host/port and gateway-authentication edits
still require a restart. Live agent configuration writes are atomic and back up
existing files.

## Verification

The full gate and its evidence boundaries are documented in
[the development guide](docs/development.md). Use the smallest relevant subset
for local changes; browser smoke is synthetic and must use its owned browser
profile and server.

## Security and limitations

- Never log or commit API keys, auth tokens, databases, or live agent files.
- Real provider requests, live CLI switching, native WebView2 behavior, signing,
  installation, upgrades, and updater delivery require separate acceptance.
- The current preview is unsigned. See [desktop release preparation](docs/desktop_release.md).
- Root/UI npm lockfiles and `src-tauri/Cargo.lock` are tracked. CI uses locked,
  script-disabled dependency restoration; lockfiles are not an SBOM or signature.
- Public publication must use the history-free process in
  [Privacy-safe public snapshot](docs/public_release_privacy.md).

## Documentation map

### Project docs

- [Architecture](docs/architecture.md) — module ownership, data flow,
  persistence, and compatibility boundaries.
- [Unified suppliers](docs/unified_suppliers.md) — supplier and endpoint
  models, per-source dual-document URL discovery, capability routing, and
  hot-apply behavior.
- [Development](docs/development.md) — setup, commands, verification gate,
  and release boundary.
- [Workspace interface QA](docs/interface_qa.md) — navigation, header surface,
  provider actions, dialogs, and the visual checklist.
- [Project structure](docs/project_structure.md) — ownership rules, UI state
  boundaries, and test/build boundaries.
- [Prompt enhancement](docs/prompt_enhance.md) — the two-stage pipeline, model
  targets, limits, templates, and data handling.
- [Desktop release](docs/desktop_release.md) — native build matrix, signing,
  notarization, manual acceptance, and rollback.
- [Privacy-safe public snapshot](docs/public_release_privacy.md) — automated
  privacy audit, history-free export, and new-repository procedure.
- [Migration design](DESIGN.md) — durable design decisions and migration
  summary.
- [v0.2.0 preview notes](docs/releases/v0_2_0.md) — preview changes,
  verification, and distribution boundary.

### Agent guides

- [AGENTS.md](AGENTS.md) — repository rules for coding agents.
- [CLAUDE.md](CLAUDE.md) — Claude Code repository guide.
- [GEMINI.md](GEMINI.md) — Gemini project context.
- [GitHub Copilot instructions](.github/copilot-instructions.md).
