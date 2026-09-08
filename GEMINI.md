# AI Agent Gateway (AAG) — Project Context

> **Status note (2026-08-03):** This file describes the legacy Node `src/` tree,
> which remains runnable standalone (`npm run gateway`) for headless use. The
> **primary deliverable is now the Tauri 2 desktop app** (`src-tauri/` + `ui/`),
> feature-complete through Phase 10 — see `DESIGN.md` (architecture) and
> `PROGRESS.md` (loop state) for the current source of truth. The desktop app
> adds: SQLite SSOT, provider switching for Claude Code/Codex (incl. isolated
> Codex CLI CODEX_HOME), local proxy with streaming SSE + circuit breaker,
> backup/MCP/Skills/Sessions/Usage/S3+WebDAV sync, deep links, tray,
> auto-update.

## Project Overview

**AI Agent Gateway (AAG)** is a unified **Gateway Proxy** + **Config Switcher** for AI coding agents. It provides a local OpenAI & Anthropic-compatible proxy server with **circuit-breaker auto-failover**, plus the ability to dynamically rewrite configuration files for multiple AI coding agents (Cursor, Copilot, Kilo, Zcode, MiniMax, Kimi) to point them either at the gateway or at a direct upstream provider.

The project is inspired by `cc-switch` capabilities but with broader agent support and a production-grade proxy infrastructure. It is implemented in **TypeScript** on **Node.js ≥ 20** and uses **Hono** for the HTTP layer, **Zod** for runtime config validation, **Commander** for CLI parsing, and **Inquirer** for the interactive TUI. Tests run via **Vitest**.

### Key Capabilities

- **Unified HTTP Gateway** — `POST /v1/chat/completions` (OpenAI), `POST /v1/messages` (Anthropic), `GET /v1/models`, `GET /health`, with full SSE streaming and bidirectional request/response format conversion.
- **Circuit Breaker** — Per-provider breaker with `closed` / `open` / `half-open` states, configurable failure threshold, cooldown, half-open retries, and success threshold for recovery.
- **Intelligent Routing** — Alias-based model mapping (e.g. `claude-sonnet-4`) → ordered `(provider, model)` hop chains with automatic sequential failover.
- **Agent Config Switcher** — Detects installed agents and rewrites their config files (Cursor JSON settings, Copilot YAML hosts, Kilo/Zcode/MiniMax/Kimi JSON configs) to point at either the local gateway or a direct provider.
- **Profiles** — Reusable credential sets (`baseURL + apiKey + models`) that can be applied to any agent.
- **Mock Provider** — Complete simulation layer for testing without real API keys.
- **Hot-Swappable Config** — JSON config at `~/.ai-agent-gateway/config.json`, mutatable at runtime via CLI/TUI.

## Architecture

```
┌──────────────────────────────────────────────────────────┐
│  Agents (Cursor / Copilot / Kilo / Zcode / MiniMax / Kimi)│
└────────────────────────┬─────────────────────────────────┘
                         │ HTTP
┌────────────────────────▼─────────────────────────────────┐
│              Gateway Core (Hono)                        │
│  ┌──────────┐  ┌──────────────┐  ┌───────────────────┐   │
│  │ Router   │  │ Converter    │  │ Circuit Breaker   │   │
│  │ (alias→  │→ │ (OpenAI ↔    │→ │ (per-provider     │   │
│  │  hops)   │  │  Anthropic)  │  │  state machine)   │   │
│  └──────────┘  └──────────────┘  └───────────────────┘   │
└────────────────────────┬─────────────────────────────────┘
                         │
┌────────────────────────▼─────────────────────────────────┐
│   Provider Layer: OpenAI / Anthropic / Gemini / Mock    │
└──────────────────────────────────────────────────────────┘
```

## Directory Structure

```
src/
├── index.ts              # CLI entry (Commander) + TUI default
├── agents/
│   ├── adapters.ts       # Per-agent config read/write + builders
│   └── switcher.ts       # Gateway/Direct/Profile switching logic
├── cli/
│   ├── commands.ts       # Subcommand handlers (gateway/status/test/...)
│   └── menu.ts           # Interactive TUI (Inquirer)
├── config/
│   ├── schema.ts         # Zod schemas + TS types (Provider, Route, Agent, Profile, AppConfig)
│   ├── store.ts          # JSON load/save at ~/.ai-agent-gateway/config.json
│   └── manager.ts        # Configuration mutation helpers
├── gateway/
│   ├── canonical.ts      # Internal OpenAI-shaped types + error helpers
│   ├── circuit_breaker.ts# Breaker state machine (closed/open/half-open)
│   ├── converter.ts      # OpenAI ↔ Anthropic request/response conversion
│   ├── router.ts         # Alias resolution + hop failover
│   ├── server.ts         # Hono app: routes + SSE streaming
│   └── providers/
│       ├── base.ts       # Abstract provider interface
│       ├── mock.ts       # Mock provider (simulates delays/errors)
│       ├── openai.ts     # OpenAI-compatible implementation
│       ├── anthropic.ts  # Anthropic-compatible implementation
│       └── index.ts      # Provider factory + registry
└── util/
    └── log.ts            # Logger
tests/
└── e2e.test.ts           # 8 vitest e2e tests (health, models, OAI, Anthropic, failover, breaker)
cc-switch/                # Placeholder directory (not submodule); populate yourself if needed
```

## Building, Running, and Testing

### Prerequisites
- **Node.js ≥ 20** (ESM modules, `NodeNext` resolution)
- **TypeScript 5.7** (handled by devDeps)

### Install
```bash
npm install
```

### Common Commands
```bash
npm run dev          # Interactive TUI menu (default entry)
npm run gateway      # Start proxy server on 127.0.0.1:8787
npm run gateway -p 9000
npm run switch       # Switch agent config (interactive)
npm run build        # Production build → ./dist
npm run start        # Run compiled binary (aag)
npm run typecheck    # tsc --noEmit
npm test             # Vitest e2e suite (8 tests, mock-based, no API keys needed)
```

### CLI Surface (`aag` command, installed via `bin`)
```
aag                       # Interactive TUI menu (default)
aag gateway [-h HOST] [-p PORT]
aag switch [agent] [--to gateway|direct|profile] [-m MODEL] [--base-url URL --api-key KEY] [--profile ID]
aag status                # Show providers, routes, agents
aag models                # List models from running gateway
aag test [model] [--no-stream]
aag config-path           # Print ~/.ai-agent-gateway/config.json path
```

### Tauri / UI (optional)
```bash
npm run ui:install        # Install ui/ workspace deps
npm run ui:dev            # Vite dev server for UI
npm run ui:build          # Build UI assets
npm run tauri:dev         # Full Tauri desktop app dev mode
npm run tauri:build       # Produce Tauri desktop bundle
```

## Development Conventions

### Code Style & Type System
- **TypeScript strict mode** is enabled in `tsconfig.json` (`strict: true`, `forceConsistentCasingInFileNames: true`).
- **ESM only** (`"type": "module"`, `module: NodeNext`); all source imports use explicit `.js` extensions even when source files are `.ts` (e.g. `import { foo } from './bar.js'`).
- **Zod-first config design**: every config entity (`ProviderConfig`, `RouteConfig`, `AgentTarget`, `Profile`, `AppConfig`) has a Zod schema and an inferred TS type. New config fields MUST be added to both schema and type.
- **No unused-warning suppression** in `tsconfig` (`noUnusedLocals/Parameters: false`); prefer clean code over disabling checks.
- **No hidden type hacks** — explicit types, type guards, and object spread preferred.

### Module Layout
- `src/gateway/providers/base.ts` defines the abstract `Provider` interface; every concrete provider (`openai`, `anthropic`, `mock`) implements it and is registered via the factory in `providers/index.ts`.
- The **canonical intermediate format** (OpenAI-shaped) lives in `gateway/canonical.ts`; the `Converter` translates to/from Anthropic at the edges.
- The **Router** is the single point that resolves a model alias to a route, walks hops, and consults the circuit breaker.

### Agent Adapter Pattern
Each supported agent has an `AgentAdapter` in `agents/adapters.ts` exposing:
- `resolveConfigPath()` → OS-specific path
- `read()` / `write()` → safe file ops
- `buildGatewayConfig(url, token, model)` / `buildDirectConfig(baseURL, key, model)` → config content builders
- `detectCurrentBaseURL()` → for status reporting

To add a new agent: add to `AgentId` enum in `config/schema.ts`, add an adapter, and register default `configPaths` for Windows/Unix/macOS.

### Configuration
- Stored at `~/.ai-agent-gateway/config.json` (load via `loadConfig()` / save via `saveConfig()` in `config/store.ts`).
- Loaded and validated through `AppConfig` Zod schema on every startup.
- Includes: `gateway`, `providers[]`, `routes[]`, `profiles[]`, `agents[]`.

### Testing
- **E2E tests** in `tests/e2e.test.ts` use the in-process Hono app via `app.request()` (no socket). They cover: health, models listing, OpenAI non-stream + stream, Anthropic non-stream + stream, failover across hops, and circuit breaker open-after-threshold.
- **Mock provider is the default test substrate** — tests do not require real API keys.
- Run a focused test: `npx vitest run -t "circuit opens"`.
- **Always add or update a test** when changing gateway routing, conversion, breaker logic, or adapter behavior.

### Error Handling
- Provider errors carry typed metadata (e.g. `isRetriable`, HTTP status). The router decides whether to advance to the next hop based on this.
- Retriable: network timeouts, 5xx, 429, temporary unavailability.
- Non-retriable: 400/401/403/404/422 — bubble up immediately.

### Security
- `authToken` on the gateway is optional; when set, the server checks it.
- API keys live in plain JSON in `~/.ai-agent-gateway/` — never log them, never commit them.
- Adapter file writes overwrite user config — always read previous content (`adapter.read()`) and pass it back as `previous` in `SwitchResult` so the user can restore.

## Common Tasks

### Add a new provider type
1. Extend `ProviderType` enum in `src/config/schema.ts` (currently: `openai | anthropic | gemini | mock`).
2. Implement `Provider` in `src/gateway/providers/<name>.ts`.
3. Register in `src/gateway/providers/index.ts` factory.

### Add a new agent
1. Add to `AgentId` enum in `src/config/schema.ts`.
2. Add an `AgentAdapter` in `src/agents/adapters.ts` with the correct config builders per platform.
3. Add default `configPaths` covering Windows (`%APPDATA%`), macOS (`~/Library/...`), and Linux (`~/.config/...`).

### Add a new route alias
Edit `~/.ai-agent-gateway/config.json` → `routes[]`, or use the CLI to add it programmatically. Each `RouteConfig` has `alias`, `hops: [{provider, model}, ...]`, and optional `description`.

### Debug circuit-breaker behavior
- Hit `GET /health` to see each provider's breaker state.
- Use mock provider with low `failureThreshold` (e.g. 3) for reproducible behavior in tests.

## Notes

- The `cc-switch/` directory is **a placeholder, not a git submodule**. Populate it manually only if comparing upstream behavior; `src/agents/switcher.ts` already implements the equivalent rewriting flow for every supported agent.
- Provider list currently ships with **mock** by default for zero-friction local startup.
- Streaming responses use Server-Sent Events (SSE) on both OpenAI and Anthropic endpoints.