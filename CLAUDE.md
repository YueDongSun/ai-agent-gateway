# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Read first

Read these documents in order:

1. `AGENTS.md` for repository-wide agent rules.
2. `docs/architecture.md` for module ownership and runtime contracts.
3. `docs/development.md` for commands and verification.
4. `PROGRESS.md` before Rust, Tauri, release, or migration work.

## Setup

```text
npm ci --ignore-scripts
npm ci --prefix ui --ignore-scripts
```

There are two dependency trees: the repository root and `ui/`. Root and UI npm
lockfiles and `src-tauri/Cargo.lock` are tracked; use `npm ci --ignore-scripts`
and `cargo ... --locked --offline` for reproducible local checks.

Run the desktop app with `npm run desktop:dev`, the browser-only demo with
`npm run ui:demo`, and the legacy Node gateway with `npm run gateway`.

## Runtime contracts

- Node TypeScript is strict ESM: relative imports need explicit `.js`
  extensions. Node tests live in `tests/` (Vitest); Rust tests are inline and
  in `src-tauri/src/e2e.rs`.
- HTTP surfaces: `/v1/chat/completions` (OpenAI), `/v1/messages` (Anthropic),
  `/v1/responses` (OpenAI Responses / Codex CLI), `/v1/models`, `/health`.
- Persistence: AAG SQLite at `~/.aag/aag.db` and Node JSON config at
  `~/.ai-agent-gateway/config.json`, both overridable with `AAG_CONFIG_DIR`;
  `AAG_HOME_DIR` shifts agent paths for tests only. Current schema version is
  in `src-tauri/src/database/mod.rs`.
- Tauri commands expose `Result<T, String>` and map from `AppError`. SQL
  belongs in `src-tauri/src/database/dao/`, orchestration in `commands/`, and
  domain logic in `services/`.

## Runtime choice

- Use `src/` for the independently runnable Node gateway, CLI, TUI, and legacy
  agent adapters.
- Use `src-tauri/` for SQLite, Tauri commands, native lifecycle, and the Rust
  proxy.
- Use `ui/` for the React desktop surface and synthetic browser demo.

The desktop application is the primary product. The Node runtime remains a
supported headless compatibility surface.

## Architecture

Both proxies (Node Hono in `src/gateway/`, Rust Axum in
`src-tauri/src/proxy/`) use an OpenAI Chat Completions-shaped canonical format
internally. Anthropic conversion happens at the HTTP edge and the
Anthropic-native provider boundary; routing and breaker decisions are made on
the canonical shape. Requests flow through a route alias to ordered hops with
per-provider circuit breakers. Retriable failures are network errors, 429, and
5xx; other 4xx errors propagate without failover. Anthropic clients routed to
Anthropic-compatible upstreams keep the original source envelope so thinking
blocks, signatures, and tool order survive without appearing on OpenAI wire
shapes.

## Module map

- Rust IPC orchestration: `src-tauri/src/commands/`
- SQLite schema and DAOs: `src-tauri/src/database/`
- Proxy and failover: `src-tauri/src/proxy/`
- Agent adapters and coexistence services: `src-tauri/src/services/`
  (`provider/`, `detect/`, `import_cc_switch/`)
- Managed DSH extension: `src-tauri/src/services/dsh/`
  (isolated lifecycle, plugins, redacted upstream resolution)
- MCP registry and Claude sync: `src-tauri/src/services/mcp/`
- Backups and remote sync: `src-tauri/src/services/sync/`
- Restricted DSH runtime and marketplace windows: `src-tauri/src/dsh_window.rs`,
  `src-tauri/src/dsh_market_window.rs`
- Shared app page: `ui/src/components/app/AppPage.tsx`
- UI pool state: `ui/src/components/app/providers_state.tsx`
- UI route state: `ui/src/components/app/routes_state.tsx`
- DSH manager UI: `ui/src/components/dsh/`
- DSH marketplace UI: `ui/src/components/dsh_market/`
- MCP registry UI: `ui/src/components/mcp/`
- Global diagnostics dialog: `ui/src/components/diagnostics_modal.tsx`
- React error boundary: `ui/src/components/error_boundary.tsx`
- Typed IPC: `ui/src/lib/api/`
- Synthetic demo-only IPC: `ui/src/lib/mock/`
- Hooks, theme, locale, build metadata: `ui/src/hooks/`,
  `ui/src/lib/theme.ts`, `ui/src/lib/shell_i18n.ts`, `ui/src/lib/build_info.ts`
- Browser smoke: `scripts/ui_smoke/`
- Verification runner and feature catalog: `scripts/verification/`
- Read-only MCP companion and automation: `scripts/mcp/`
- DSH marketplace catalog and native checks: `scripts/dsh_market/`

Keep domain logic in its owning module. Do not put SQL in commands, put live
file writes in the UI, or use browser-demo behavior as native evidence.

## Working rules

- Inspect `git status` and preserve unrelated changes.
- Use `snake_case` for new project-controlled identifiers and filenames.
- Preserve public Tauri command names, npm scripts, protocol fields, and
  generated asset names.
- Validate at boundaries, preserve error context, and never silently drop bad
  records.
- Protect API keys, auth tokens, databases, and live agent configuration.
- Use atomic writes and backups before replacing agent configuration.
- Keep current callbacks in React contexts; stale closures are not a valid
  performance optimization.

## Verification

The current full gate is recorded in `docs/development.md` and reproduced in
`AGENTS.md`. The Node, Rust, browser-demo, native, and external evidence
streams remain independent — do not claim signing, installation, upgrade,
real upstream, or native WebView2 acceptance from local unit or demo tests.

UI subset:

```text
npm run typecheck
npm test
npm run test:desktop_runner
npm run ui:build
npm run ui:build_demo
npm run ui:smoke
npm run ui:smoke_demo
npm run release:verify
```

Rust subset (after `ui/dist` exists):

```text
cd src-tauri
cargo check --locked --offline
cargo test --locked --offline --lib -- --test-threads=1
```

`ui/dist` must exist before Tauri cargo checks; run `npm run ui:build` first.
The serial Rust flag protects tests that mutate process-wide environment
overrides. Run Node and Rust tests with disposable `AAG_CONFIG_DIR` and
`AAG_HOME_DIR` values and leave `AAG_TEST_CCSWITCH_DB` unset so optional
real-database probes do not run.

Single tests:

```text
npx vitest run tests/adapters.test.ts
cd src-tauri && cargo test --locked --offline --lib <module_path>::tests::<name> -- --exact --test-threads=1
```

For cross-tree or release work, use the complete gate in
`docs/development.md`. Report Node, Rust, browser-demo, native, and external
evidence separately. Do not claim signing, installation, upgrade, real
upstream, or native WebView2 acceptance from local unit or demo tests.

MCP/DSH workspace slice uses `npm run verify:features -- --scope <node|ui|mcp|dsh>`
(append `--run` to execute); the runner is read-only by default and never
installs missing dependencies. Companion and DSH transport checks live under
`npm run test:automation`, `npm run test:dsh_transport`,
`npm run test:dsh_market_catalog`, and `npm run test:dsh_market_native`.

## Known behavior

- Rust routing hot-applies supplier, provider, model, route, pin, and catalog
  changes to new requests; in-flight requests keep their admitted snapshot.
  Listener and gateway-auth changes still require a restart.
- Streaming failover probes the first chunk only; it does not transparently
  recover a stream that fails after commitment.
- Browser demo IPC is synthetic and must refuse real mutations.
- The current release is the unsigned `v0.2.0` preview.

## Two-repository publication model

- This checkout is the private development worktree: `master` and `feat/*`
  branches stay private and are never pushed to a public remote.
- Develop here. The public release repository receives history-free
  snapshots only: run `npm run privacy:sync_public -- --public-clone <absolute-path> --release vX.Y.Z --notes <text>` (see `scripts/public_sync.mjs`). It enforces the public remote allowlist, the noreply publisher identity, release-tag format, and the full audit gate.
- Never push, pull, fetch, or merge between the private and public repositories, and never add one remote to the other checkout.
- `private-export-*` branches and their sanitized source commits must never be pushed anywhere.
- Keep `AGENTS.md`, `CLAUDE.md`, and `GEMINI.md` free of personal names, emails, machine paths, and credentials: these files ship verbatim in every public snapshot.

## Public-release model

This public repository contains release snapshots only. Day-to-day
development happens in a separate private repository; nothing is merged,
pushed, fetched, or pulled between the two histories. Each public commit
is an audited, history-free export plus the documented release shaping
below, committed under a single public publisher identity. Do not import
external git histories, remotes, tags, or reflogs here. The export,
privacy-audit, and snapshot-manifest procedure is documented in
[Privacy-safe public snapshot](docs/public_release_privacy.md); every
release delta beyond the manifest envelope is listed in
[CHANGELOG_PUBLIC_BASELINE.md](CHANGELOG_PUBLIC_BASELINE.md) and
[CHANGELOG.md](CHANGELOG.md). Process references in older docs (migration
loops, phase logs) describe the private development workflow, not this
release tree.
