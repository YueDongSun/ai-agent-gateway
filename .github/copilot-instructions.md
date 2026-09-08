# Copilot Instructions for AI Agent Gateway (AAG)

## Build / Run / Test

```bash
npm install                         # one-time setup (Node >= 20)
npm run dev                         # launch interactive TUI
npm run gateway                     # start proxy server (default :8787)
npm run gateway -- -p 9000          # custom port
npm run switch [agent]              # point agent at gateway or direct provider
npm run typecheck                   # tsc --noEmit (currently fails — see Known Issues)
npm test                            # vitest run (single e2e file, tests/ in-process)
npx vitest run -t "failover"        # run a specific test by name
npm run build                       # tsc → dist/
npm start                           # node dist/index.js (built artifact)
npm run tauri:dev                   # Tauri desktop shell (Phase 0 skeleton)
```

There is no lint config, no formatter config, no CI workflow files.

## Architecture

The project has **two parallel tracks** that share the same repo but target different runtimes:

1. **Node/TypeScript proxy + CLI** (`src/`) — the working runtime today
2. **Tauri desktop shell** (`src-tauri/` + `ui/`) — Phase 0 skeleton, being built alongside

### Node runtime (`src/` — ~19 TS files, ~1.7 kLOC)

Three layers wired through one shared `AppConfig` (Zod-validated JSON):

- **Entry/CLI** (`src/index.ts`, `src/cli/`) — `commander` parses subcommands (`gateway`, `switch`, `status`, `models`, `test`, `config-path`). No subcommand → `interactiveMenu()` (Inquirer TUI prompts). The `--menu` flag is a no-op — launch is gated on `process.argv.slice(2).length === 0`.

- **Config layer** (`src/config/`) — `schema.ts` (Zod `AppConfig` with providers, routes, profiles, agents), `store.ts` (loads/saves `~/.ai-agent-gateway/config.json`, overridable via `AAG_CONFIG_DIR`, first-run auto-seeds a working config with mock providers), `manager.ts` (immutable mutation helpers, saves atomically via `.tmp` + rename).

- **Gateway** (`src/gateway/`) — Hono app (`server.ts`) with routes: `POST /v1/chat/completions` (OpenAI), `POST /v1/messages` (Anthropic), `GET /v1/models`, `GET /health`. Optional Bearer/`x-api-key` auth. SSE streaming via `ReadableStream` wrapping async generators. `router.ts` resolves model alias → ordered hop chain with per-provider circuit breaker (`circuit_breaker.ts`) and automatic failover. `converter.ts` handles bidirectional OpenAI ↔ Anthropic format conversion.

- **Agents** (`src/agents/`) — `adapters.ts` has 6 concrete adapters (Copilot, Kilo, Cursor, Zcode, MiniMax, Kimi) that rewrite each agent's on-disk config files. `switcher.ts` provides `pointToGateway`, `pointToProvider`, `pointToProfile` operations.

### Tauri desktop shell (`src-tauri/` + `ui/`)

- **Rust backend** (`src-tauri/`) — minimal Phase 0 skeleton: Tauri 2.x app with `app_info` command only. `main.rs` / `lib.rs`. Dependencies: `tauri`, `tauri-plugin-log`, `serde`, `serde_json`, `log`.
- **React frontend** (`ui/`) — Vite + React 18 + TypeScript. Calls Rust via `@tauri-apps/api/core` `invoke()`. Currently displays skeleton `AppInfo`.
- Full migration plan in `DESIGN.md`: 9 phases from skeleton → provider CRUD → proxy port → routes → backup → MCP/skills → session manager → usage tracking → cloud sync → polish.

## Key Conventions

- **ESM with `.js` extensions in imports** — all relative imports must use `.js` even for `.ts` sources (e.g., `import { Router } from './router.js'`). This is required by `module: "NodeNext"`. Do not "fix" this.
- **No path aliases** — import paths are always relative (e.g., `../config/schema.js`, `../gateway/router.js`).
- **`noUnusedLocals` and `noUnusedParameters` are off** — don't worry about unused private helpers.
- **Tests boot the Hono app in-process** via `app.request(path, init, BASE)` — no socket, no real network. Only one test file exists: `tests/e2e.test.ts`.
- **Config auto-seeds on first run** — the first `loadConfig()` writes `~/.ai-agent-gateway/config.json` to disk with working mock providers. Tests that delete this file re-create it on the next call.

## Non-Obvious Behavior

- **Streaming failover is one-shot.** `Router.stream` commits to a provider after the first chunk. Mid-stream errors surface to the client — no silent failover halfway through a stream.
- **`Switcher.status(id).path` is unreliable.** The `existsSync` wrapper always returns `true`, so `.path` is always the first candidate path regardless of existence. Treat it as "first candidate", not "on-disk path".
- **`AgentTarget.format` is unused.** All adapters write OpenAI-shaped JSON regardless of `'openai' | 'anthropic'`. The field is informational only.
- **`ProviderType` includes `'gemini'` but no Gemini class exists.** `createProvider` falls through to `OpenAIProvider` for `'gemini'` (assumed OpenAI-compatible).
- **Converter only handles text content.** Non-string content (tool calls, images, function blocks) is silently flattened. Multi-modal or tool-use content is dropped.
- **No clean shutdown.** `startGatewayCmd` and `interactiveMenu`'s gateway branch both `await new Promise(() => {})` after `start()`. SIGINT hard-kills.
- **The converter functions are not symmetric callers.** `oaiReqToAnthropicReq` is used on the provider side; `anthropicReqToOAIReq` is used on the gateway edge. Keep this directionality straight when editing.

## Known Issues (must be resolved before `typecheck` / `test` pass)

1. **Filename typo** — `src/gateway/providers/base.ts:2` imports `'../circuit-breaker.js'`, but the file is `src/gateway/circuit_breaker.ts` (underscore). Fix: rename file to `circuit-breaker.ts`.
2. **ESM/CJS interop bug** — `src/config/manager.ts:81` uses `require()` in an ESM module. Replace with dynamic `import()`.
3. **README.md is significantly out of sync with the code** — it documents files, test scripts, and config paths that don't exist. Trust the code over the README.
4. **Root-level scripts are dead weight** — `check_codex.py`, `check_db.py`, `check-db.cjs` are unreferenced and machine-specific. Safe to delete.
