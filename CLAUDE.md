# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Repo shape (read this first)

This repo is mid-migration from a Node/TypeScript CLI/TUI proxy to a Tauri desktop app aiming for feature parity with **cc-switch** (<https://github.com/farion1231/cc-switch>, MIT, 121k stars). The full plan, data model, and phasing live in **`DESIGN.md`** - read it before doing anything in `src-tauri/` or `ui/`.

Three source trees coexist today:

- **`src/`** - the original Node/TypeScript proxy + CLI/TUI. **Still the only functional proxy.** All commands below in "Build / Run / Test (Node `src/` tree)" refer to this tree unless noted.
- **`src-tauri/`** - NEW Tauri 2 Rust backend. Phase 0 skeleton only: boots a window, exposes one `app_info` command. `cargo check` passes. Not yet feature-complete - see `DESIGN.md` phasing.
- **`ui/`** - NEW React 18 + Vite frontend for the Tauri shell. Phase 0 skeleton only: single `App.tsx` that calls `app_info` and renders the result.

The reference cc-switch source is available read-only at `~/Documents/Codex/2026-07-05/ben/work/cc-switch-src/` - consult it when porting features.

## Build / Run / Test (Node `src/` tree)

```
npm install
npm run typecheck        # tsc --noEmit (CURRENTLY FAILS — see Known Issues)
npm test                 # vitest run, single e2e file (CURRENTLY FAILS — see Known Issues)
npm run dev              # tsx src/index.ts — launches interactive TUI (no args)
npm run gateway          # tsx src/index.ts gateway        (--host, --port flags)
npm run switch           # tsx src/index.ts switch [agent] (--to gateway|direct|profile)
npm run build            # tsc -p tsconfig.json → dist/
npm start                # node dist/index.js (run the built artifact)
```

There is no `vitest.config`; tests boot the Hono app in-process via `app.request(path, init, BASE)` — no socket, no real network. To run a single test by name: `npx vitest run -t "failover"`. Test files live under `tests/`. **The only existing test today is `tests/e2e.test.ts`.**

Single-node, no CI workflow files, no lint config, no formatter config.

## Build / Run (Tauri `src-tauri/` + `ui/` trees - Phase 0)

```
cd ui && npm install                 # install React deps (also done by tauri:dev)
cd src-tauri && cargo check          # Rust typecheck - passes today
cd ui && npx tsc --noEmit            # UI typecheck - passes today
cd ui && npx vite build              # produce ui/dist/ that Tauri expects at build time
npm run tauri:dev                    # from repo root: boots Tauri window pointed at ui dev server
npm run tauri:build                  # from repo root: production bundle (.msi/.dmg/etc.)
```

`tauri.conf.json` references `../ui/dist` as `frontendDist` - **`ui/dist/` must exist before `cargo check` / `cargo build` will succeed** (the `generate_context!` proc macro panics otherwise). Running `npx vite build` in `ui/` once is enough. `npm run tauri:dev` does not need `dist/` because it points at the dev server, but `cargo check` is invoked as part of `tauri dev` and still wants `frontendDist` to resolve - so build `ui/dist/` at least once before first `tauri:dev`.

Icon files under `src-tauri/icons/` are placeholders copied from the cc-switch reference checkout. Replace with AAG-branded icons before any release.

## Architecture (what the code actually is, not what README.md claims)

The runtime is small (~19 TS files, ~1.7 kLOC) and is best understood as three layers wired through one shared `AppConfig`.

**Entry / CLI** (`src/index.ts`, `src/cli/`)
- `commander` parses: `gateway`, `switch [agent]`, `status`, `models`, `test [model]`, `config-path`.
- No subcommand → `interactiveMenu()` from `src/cli/menu.ts` (Inquirer prompts).
- `npm run dev` is the only way to launch the TUI; everything else is scriptable.

**Config layer** (`src/config/`)
- `schema.ts` defines Zod-validated `AppConfig` (version, gateway, providers[], routes[], profiles[], agents[]).
- `store.ts` loads `~/.ai-agent-gateway/config.json` (overridable via `AAG_CONFIG_DIR`). **First run auto-seeds** a working config with two mock providers (`mock-primary` w/ failure threshold 3, `mock-fallback`) and three routes (`claude-sonnet-4`, `gpt-4o`, `mock-fail`).
- `manager.ts` provides immutable mutation helpers (add/update/remove for providers, routes, profiles, agents). Save with `saveConfig(cfg)` — writes atomically via `.tmp` + rename.
- Persists to disk on every `switch` / `status` invocation.

**Gateway** (`src/gateway/`)
- `server.ts` — Hono app. Routes: `POST /v1/chat/completions` (OpenAI), `POST /v1/messages` (Anthropic), `GET /v1/models`, `GET /health`. Optional Bearer/`x-api-key` auth gated on `cfg.gateway.authToken`. SSE streaming via `ReadableStream` wrapping an async generator.
- `router.ts` — `Router` class. Resolves a model alias → `RouteConfig` of ordered `hops = [{provider, model}]`. Falls through hops on retriable failure or open circuit. Falls back to single-hop auto-route if any enabled provider directly serves the model.
- `canonical.ts` — internal OpenAI-shaped types (`OAIChatResponse`, `OAIStreamChunk`, `ProviderError`, `makeError`).
- `converter.ts` — bidirectional OpenAI ↔ Anthropic request/response + SSE chunk conversion. The Anthropic endpoint internally converts to OAI, calls `router.complete`/`router.stream`, then converts back.
- `circuit_breaker.ts` — `CircuitBreaker` class with closed/open/half-open state machine + `CircuitBreakerSnapshot` type. Each `BaseProvider` owns one. Note the **underscore** filename — see Known Issues.
- `providers/` — `base.ts` (abstract `BaseProvider` wrapping `CircuitBreaker`), `openai.ts`, `anthropic.ts`, `mock.ts`, `index.ts` (factory by `provider.type`).

**Agents / switcher** (`src/agents/`)
- `adapters.ts` — `AgentAdapter` interface + `JsonAgentAdapter` abstract base. Six concrete subclasses: `CopilotAdapter`, `KiloAdapter`, `CursorAdapter`, `ZcodeAdapter`, `MinimaxAdapter`, `KimiAdapter`. Each owns the JSON shape its agent's config file expects (`CopilotAdapter` writes `{provider, baseURL, apiKey, model, models}`; `CursorAdapter` writes `{openai: {...}}`; etc.). **The actual on-disk paths differ from README.md** — see Known Issues.
- `switcher.ts` — `Switcher` class wrapping `AppConfig`. Three operations: `pointToGateway(agent, model)`, `pointToProvider(agent, baseURL, apiKey, model)`, `pointToProfile(agent, profileId, model)`. Each returns a `SwitchResult`. `pointAllToGateway` iterates all agents.
- Detection = "is any candidate path on disk?". `detectInstalledAgents()` filters `defaultAgentTargets()` by `existsSync`.

**Util** (`src/util/log.ts`) — minimal logger writing timestamped lines to stdout (info/debug) or stderr (warn/error). `setLogLevel()` mutates module-global state; called by `createGatewayApp()` at boot. `aag switch` does not pass through `createGatewayApp`, so log level is whatever the previous process left it at.

## Quirks / Non-Obvious Behavior

- **Streaming failover is one-shot.** `Router.stream` awaits the **first chunk** before committing (`src/gateway/router.ts:~102`). Once a provider yields a chunk, mid-stream errors surface to the client — there is no silent failover halfway through a stream. Don't promise transparent streaming failover in user-facing copy.
- **`Switcher.status(id).path` is unreliable.** `src/agents/switcher.ts:92-97` wraps `existsSync` in a `try { return true } catch { return false }` that always returns `true`, so `.path` is always the first config path regardless of existence. Treat it as "first candidate path", not "on-disk path".
- **`--menu` flag is a no-op.** `src/index.ts:12` declares it for `--help`, but the launch decision only checks `process.argv.slice(2).length === 0` (`src/index.ts:55`). The flag has no effect.
- **`AgentTarget.format` is unused.** All adapters write OpenAI-shaped JSON regardless of `'openai' | 'anthropic'`. The field is informational only.
- **`ProviderType` includes `'gemini'` but no Gemini class exists.** `createProvider` falls through to `OpenAIProvider` for `'gemini'` and the default (`src/gateway/providers/index.ts:14-15`). Gemini is assumed OpenAI-compatible.
- **`ConfigManager.ensureAgents()` is broken at runtime.** `src/config/manager.ts:81` uses `require()` inside an ESM module. The method has no call sites today, so it's latent — but if you wire it up, replace with a static or dynamic `import()`.
- **No clean shutdown.** `startGatewayCmd` and `interactiveMenu`'s gateway branch both `await new Promise(() => {})` after `start()`. SIGINT hard-kills the process; there are no signal handlers.
- **`oaiReqToAnthropicReq` and `anthropicReqToOAIReq` are not symmetric callers.** The first is used by `AnthropicProvider` (provider side); the second by the gateway edge (`server.ts:92`). They are paired functions but used in opposite directions — keep this straight when editing the converter.
- **Converter only handles text content.** `textOf` and the request builders silently flatten any non-string content (tool calls, images, function blocks) to its `.text` field or a user-role text message. Multi-modal or tool-use content is dropped on the floor.
- **Anthropic SSE emitter is synthesized, not parsed.** `oaiChunksToAnthropicStream` emits well-formed Anthropic events from OAI chunks; `anthropicStreamToOAIChunks` only listens for `message_start` / `content_block_delta` / `message_delta` / `message_stop`, ignoring `content_block_start`/`stop`/`ping`.
- **First `loadConfig()` writes `~/.ai-agent-gateway/config.json` to disk** (`src/config/store.ts:100-103`). Tests that delete the file re-create it on the next call.
- **`APPDATA` env var is trusted on Windows** (`src/agents/adapters.ts:11`). On Linux/macOS this falls back to `~/AppData/Roaming`, which is wrong — but agent paths are Windows-oriented anyway.

## Conventions

- TypeScript strict, ESM (`"type": "module"`, `module: "NodeNext"`). All relative imports **must include the `.js` extension** even though sources are `.ts` (e.g. `import { Router } from './router.js'`). Do not "fix" this.
- No path aliases. Import from `../config/schema.js`, `../gateway/...`, etc.
- `noUnusedLocals` and `noUnusedParameters` are both **off** in `tsconfig.json`. Don't worry about unused private helpers.

## Known Issues (must be resolved before `typecheck` / `test` can pass)

1. **Filename typo blocks all compilation.** `src/gateway/providers/base.ts:2` imports `'../circuit-breaker.js'`, but the file on disk is `src/gateway/circuit_breaker.ts` (underscore). `tsc --noEmit` fails with `TS2307: Cannot find module '../circuit-breaker.js'`. Every other reference (README, package.json, router.ts, mock.ts, store.ts, e2e.test.ts prose) uses the hyphenated form. Fix: rename the file to `circuit-breaker.ts` (the rename preserves the export name `CircuitBreaker`, which is unaffected).
2. **ESM/CJS interop bug.** `src/config/manager.ts:81` uses `require('../agents/adapters.js')` inside an ESM module. This works in `tsx` (dev) but is fragile in `node` and bundlers. Replace with a top-level `import` or lazy dynamic `import()`.
3. **README.md is significantly out of sync with the code.** It documents files that don't exist (`src/agents/detectors.ts`, `src/agents/types.ts`, `src/gateway/handlers/`, `src/gateway/types.ts`, `src/types/`, `src/utils/{error,crypto,http}.ts`, `tests/{unit,integration,fixtures,utils.ts}`, `docs/`), test scripts that aren't in `package.json` (`test:unit`, `test:integration`, `test:e2e`, `test:performance`), and config-path tables that disagree with the actual adapter defaults (Cursor = `cursor-mcp.json` / `~/.cursor/config.json`, NOT `settings.json`; Copilot adapter writes JSON, not YAML). When in doubt, trust the code over the README.
4. **Root-level scripts are dead weight.** `check_codex.py`, `check_db.py`, `check-db.cjs` are unreferenced, machine-specific (`C:\Users\15063\.cc-switch\`), and not part of the build. Safe to delete.