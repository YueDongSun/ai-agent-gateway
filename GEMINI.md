# Gemini Project Context

AI Agent Gateway (AAG) is a local gateway and configuration manager for AI
coding tools. The current product is the unsigned `v0.2.0` Tauri desktop
preview. The legacy Node gateway and CLI remain independently runnable.

## Read first

- `AGENTS.md` — repository-wide agent rules (canonical).
- `docs/architecture.md` — current modules, ownership, and data flow.
- `docs/development.md` — setup, commands, and the verification gate.
- `PROGRESS.md` — current phase and fresh evidence.

## Setup

```text
npm ci --ignore-scripts
npm ci --prefix ui --ignore-scripts
```

Desktop app: `npm run desktop:dev`. Browser demo: `npm run ui:demo`. Node
gateway: `npm run gateway`.

## Core contracts

- Both proxies use an OpenAI Chat Completions-shaped canonical format.
  `/v1/chat/completions`, `/v1/messages`, and `/v1/responses` are the client
  surfaces.
- Failover advances for network, 429, and 5xx errors; 4xx errors propagate.
- The Rust proxy hot-applies provider, route, supplier, model, pin, and catalog
  changes to new requests; in-flight requests retain their admitted snapshot.
  Listener host/port and gateway-auth changes still require a restart.
- SQLite at `~/.aag/aag.db`, overridable with `AAG_CONFIG_DIR`. Live agent
  writes are atomic and back up existing files first.
- Use `snake_case` for new project-controlled identifiers and filenames.
  Preserve external command names, npm scripts, protocol fields, and generated
  asset names.
- Do not log or commit credentials, databases, or live configuration. Keep
  React callbacks current; stale closures are not a performance technique.

## Verification

Run the relevant subset from `docs/development.md` for a small change and the
complete gate for cross-tree, persistence, or release work. Separate
browser-demo evidence from native installation, signing, live agent, and real
upstream acceptance.

## Two-repository publication model

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
