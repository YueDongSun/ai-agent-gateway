# AGENTS.md — AI Agent Gateway (AAG)

> Authoritative system instructions for AI coding agents working in this repo.
> Companion docs: `DESIGN.md` (migration plan + data model), `PROGRESS.md`
> (loop state — **read before any Tauri/Rust work**), `CLAUDE.md` (Claude Code
> notes, largely overlapping), `GEMINI.md` (Gemini notes). When docs disagree,
> **trust the code first, then this file, then `DESIGN.md`/`PROGRESS.md`**.
> `README.md` is significantly out of sync with the code — do not trust it as a
> source of truth for file layout, scripts, or config paths.

---

## 0. Mental model (read first)

AAG is a **unified gateway proxy + config switcher** for AI coding agents. It
exposes a local HTTP server that speaks both OpenAI (`/v1/chat/completions`) and
Anthropic (`/v1/messages`) wire formats, routes requests through ordered
provider fallback chains with per-provider **circuit breakers**, and can rewrite
the config files of multiple AI coding CLIs to point them at the gateway or at a
direct upstream provider.

The repo is **migrated**: the Tauri 2 desktop app (Rust backend + React
frontend) is the primary deliverable and is **feature-complete through
Phase 10** (see `PROGRESS.md`). **Two source trees coexist:**

| Tree | Language | Status | Role |
|------|----------|--------|------|
| `src/` | Node/TypeScript | ✅ Legacy, runnable | Original proxy + CLI/TUI. Still works standalone (`npm run gateway`) for headless use. ~19 files, ~1.7 kLOC. |
| `src-tauri/` + `ui/` | Rust (Tauri 2) + React | ✅ Feature-complete (Phase 0-10) | Desktop app: SQLite SSOT, provider CRUD + live config switching (Claude Code + Codex), local proxy (streaming SSE, OpenAI/Anthropic conversion, circuit breaker), backup/MCP/Skills/Sessions/Usage/S3+WebDAV sync/deep links/tray/auto-update, plus Codex CLI isolated CODEX_HOME. 59 Rust tests + 10 Node tests green. |

**The Node `src/` proxy stays runnable for headless use; the desktop app is the
primary surface.** Do not delete `src/` without a migration plan for its
headless CLI users.

The migration is driven by a **cron loop** that reads `PROGRESS.md`, does one
bounded sub-task per iteration, runs verification, and updates `PROGRESS.md`.
Respect this discipline: one sub-task per change, always verify, always update
the log.

---

## 1. Architecture

### 1.1 Canonical intermediate format

The single most important design decision: **the internal chat format is
OpenAI Chat Completions.** Every provider receives and returns this shape
regardless of its native wire format. Format conversion happens only at the
edges (client ↔ gateway) and at Anthropic-native providers.

- Node: `src/gateway/canonical.ts` defines `OAIChatResponse`, `OAIStreamChunk`,
  `ProviderError`, `makeError()`.
- Rust: `src-tauri/src/proxy/canonical.rs` (struct-for-struct port).
- Conversion (Node): `src/gateway/converter.ts` — bidirectional OpenAI ↔
  Anthropic for requests, responses, and SSE streams.
- Edge direction matters (see Quirks): `anthropicReqToOAIReq` runs at the
  gateway edge (`server.ts`); `oaiReqToAnthropicReq` runs inside the Anthropic
  provider. They are **paired functions used in opposite directions** — keep
  this straight when editing the converter.

### 1.2 Gateway core layers (Node `src/gateway/`)

```
HTTP edge ──► Router ──► Provider (owns CircuitBreaker)
  (server.ts)   (router.ts)   (providers/*)
       │             │
       └── converter ─┘  (Anthropic ↔ OAI, only at edges)
```

- **`server.ts`** — Hono app. Routes: `POST /v1/chat/completions` (OpenAI),
  `POST /v1/messages` (Anthropic), `GET /v1/models`, `GET /health`. Optional
  Bearer/`x-api-key` auth gated on `cfg.gateway.authToken`. SSE streaming via a
  `ReadableStream` wrapping an async generator (`streamToReadable`).
- **`router.ts`** — `Router` class. Resolves a model alias → `RouteConfig` of
  ordered `hops = [{provider, model}]`. Falls through hops on **retriable**
  failure or open circuit. Auto-routes (single hop) if any enabled provider
  directly serves the model. Streaming failover is **one-shot** (probes the
  first chunk before committing — see Quirks).
- **`circuit-breaker.ts`** — `CircuitBreaker` class, closed/open/half-open
  state machine with lazy open→half-open transition inside `allow()`. Each
  `BaseProvider` owns one instance.
- **`providers/`** — `base.ts` (abstract `BaseProvider` wrapping a
  `CircuitBreaker`), `openai.ts`, `anthropic.ts`, `mock.ts`, `index.ts`
  (factory `createProvider(cfg)` by `cfg.type`).

### 1.3 Gateway core layers (Rust `src-tauri/src/proxy/`)

A direct port of the Node gateway, built on **axum** (not Hono):

- **`server.rs`** — axum router: `GET /health`, `GET /v1/models`,
  `POST /v1/chat/completions` (streaming + non-streaming, OpenAI-spec
  `[DONE]` sentinel), `POST /v1/messages` (Anthropic format, converted via
  `converter.rs`). Graceful shutdown via a `oneshot` channel.
- **`router.rs`** — `Router` struct. Same alias→hops→failover semantics as
  Node. Immutable after construction (providers + routes are a DB snapshot taken
  at `proxy_start`).
- **`circuit_breaker.rs`** — thread-safe (`std::sync::Mutex<Inner>`) port of the
  Node breaker. Same states, same lazy transition.
- **`canonical.rs`** — struct port of `canonical.ts`.
- **`converter.rs`** — bidirectional OpenAI ↔ Anthropic request/response + SSE
  stream conversion (stop<->end_turn, length<->max_tokens mapping).
- **`providers/`** — `Provider` trait (`async_trait`) + `build_provider()`
  factory. Implemented: `mock.rs`, `openai.rs` (reqwest, SSE parsing),
  `anthropic.rs` (converter-based).

### 1.4 Config layer

- **Node** (`src/config/`): Zod-validated `AppConfig` in `schema.ts`;
  `store.ts` loads/saves `~/.ai-agent-gateway/config.json` atomically
  (`.tmp` + rename), auto-seeds a working mock config on first run
  (`seedConfig()`), overridable via `AAG_CONFIG_DIR`; `manager.ts` provides
  immutable-ish mutation helpers.
- **Rust** (`src-tauri/src/database/`): SQLite SSOT at `~/.aag/aag.db`.
  `Database` wraps `Mutex<rusqlite::Connection>` (rusqlite `Connection` is not
  `Sync`). Schema + versioned migrations in `schema.rs` (`PRAGMA user_version`,
  `SCHEMA_VERSION` = 9). DAOs are `impl Database` blocks in `dao/`
  (`providers`, `proxy_providers`, `proxy_config`, `routes`, `backups`,
  `mcp_servers`, `skills`, `sessions`, `usage_records`, `sync_config`,
  `settings`/`app_meta`). First-run seeding in `seed.rs` mirrors `seedConfig()`.
  Override via `AAG_CONFIG_DIR`.

### 1.5 Agent integration (config switcher)

- **Node** (`src/agents/`): `adapters.ts` defines `AgentAdapter` interface +
  `JsonAgentAdapter` abstract base + 6 concrete adapters
  (`Copilot/Kilo/Cursor/Zcode/Minimax/Kimi`) + `createAdapter()` factory +
  `detectInstalledAgents()`. `switcher.ts` (`Switcher` class) does
  `pointToGateway` / `pointToProvider` / `pointToProfile`, each returning a
  `SwitchResult` with a `previous` field for restore.
- **Rust** (`src-tauri/src/services/provider/`): `AppType` enum (8 tools:
  ClaudeCode, ClaudeDesktop, Codex, Gemini, OpenCode, OpenClaw, Hermes,
  GrokBuild). `write_live_snapshot(app_type, provider, codex_cli_home)`
  dispatches to per-app modules. Implemented: `claude_code.rs` and `codex.rs`
  (Codex supports isolated CODEX_HOME via the `codex_cli_home` setting); the
  rest return "not yet implemented".

### 1.6 CLI / TUI / Frontend

- **Node CLI** (`src/index.ts` + `src/cli/`): `commander` parses `gateway`,
  `switch [agent]`, `status`, `models`, `test [model]`, `config-path`. No
  subcommand → `interactiveMenu()` (`menu.ts`, Inquirer). `commands.ts` holds
  subcommand handlers.
- **Rust Tauri commands** (`src-tauri/src/lib.rs`): `app_info`,
  `app_check_update`, `deeplink_parse`, `provider_{list,current,get,save,delete,switch}`,
  `settings_{get,set}`, `codex_cli_status`, `codex_open_cli_terminal`,
  `proxy_{status,start,stop}`, `backup_{create,list,delete}`,
  `mcp_{list,save,delete,toggle_app}`, `skill_{list,save,delete}`,
  `session_{list,get,delete}`, `usage_{record,summary,by_model,by_provider}`,
  `sync_{list,save,delete,record_result,check,upload,download}`.
  Registered in `invoke_handler!`. State: `Database` + `Mutex<Option<RunningProxy>>`.
- **React UI** (`ui/src/`): `App.tsx` (nav), `components/<feature>/` pages
  (Providers, Proxy, Backup, MCP, Skills, Sessions, Usage, Sync, Deep links,
  Codex CLI), `lib/api/` (typed `invoke` wrappers mirroring Rust types).

---

## 2. Design patterns in use

| Pattern | Where | Notes |
|---------|-------|-------|
| **Canonical/intermediate format** | `canonical.{ts,rs}` + `converter.ts` | OpenAI shape is the lingua franca; convert only at edges. |
| **Strategy** | retriable classification in `makeError` / `ProviderError` | `retriable` flag drives failover; 5xx/429/network=retriable, 4xx=not. |
| **State machine** | `CircuitBreaker` (closed/open/half-open) | Lazy transitions on `allow()`. |
| **Adapter** | `AgentAdapter` / per-app `services/provider/*.rs` | Each agent/CLI owns its config-file shape. |
| **Factory** | `createProvider()` / `createAdapter()` / `build_provider()` | Type-string → concrete class/struct. |
| **Template method** | `JsonAgentAdapter` base + `buildJson`/`buildDirectJson` hooks | Subclasses decide JSON shape only. |
| **DAO** | Rust `impl Database` blocks in `dao/` | All SQL lives in DAO methods, not services. |
| **Atomic write** | `saveConfig` (.tmp+rename) / Rust `atomic_write` | Prevents config corruption on crash. |
| **Zod-first schema** | `src/config/schema.ts` | Every config entity has a Zod schema + inferred type; add fields to **both**. |
| **Snapshot-at-start** | Rust `Router::new` from DB | Proxy is immutable per-run; restart to pick up config changes. |

---

## 3. Module dependency direction

**Node** (arrows = "depends on"):
```
index.ts ─► cli/{menu,commands} ─► gateway/server ─► gateway/router ─► gateway/providers/* ─► gateway/circuit-breaker
                                ─► agents/switcher ─► agents/adapters
                                ─► config/{store,manager} ─► config/schema
gateway/server ─► gateway/converter (edge conversion)
gateway/providers/anthropic ─► gateway/converter (provider-side conversion)
```
`config/schema.ts` is the shared root — nearly everything imports its types.
`agents/adapters.ts` and `gateway/` do **not** depend on each other.

**Rust** (arrows = "depends on"):
```
lib.rs (commands) ─► services/provider/* ─► config (paths/IO) + provider (model)
                 ─► database (Database) ─► database/dao/* ─► provider, error
                 ─► proxy/{server,router,providers} ─► proxy/{canonical,circuit_breaker} ─► database/dao/{proxy_providers,routes,proxy_config}
```
`error::AppError` and `provider::Provider` are the shared leaves.
`proxy/` depends on `database/dao/` for config rows but **not** on `services/`.

---

## 4. Coding conventions

### 4.1 TypeScript (`src/`, `ui/`)
- **ESM only** (`"type": "module"`, `module: NodeNext`). All relative imports
  **must include the `.js` extension** even though sources are `.ts`
  (e.g. `import { Router } from './router.js'`). Do not "fix" this.
- **No path aliases.** Import from `../config/schema.js`, `../../gateway/...`, etc.
- **TypeScript strict** (`strict: true`, `forceConsistentCasingInFileNames`).
  `noUnusedLocals`/`noUnusedParameters` are **off** — don't add unused-warning
  suppression; just don't leave dead code intentionally.
- **Zod-first config**: add new config fields to the Zod schema **and** the
  inferred type together. Defaults belong in the schema (`z.*.default(...)`).
- No comments unless asked. Prefer explicit types, type guards, object spread.
- `ui/`: React 18, function components, plain hooks. Types in
  `lib/api/types.ts` must mirror the Rust structs in `provider.rs`
  (camelCase via serde `rename`). `AppType` union must stay in sync with the
  Rust `AppType` enum's `to_db_str` values.

### 4.2 Rust (`src-tauri/`)
- Edition 2021, `rust-version = "1.77.0"`. `serde_json` built with
  `preserve_order` (keep object key order stable).
- **Errors**: `AppError` (`thiserror`) in `error.rs`. Tauri commands return
  `Result<T, String>` — convert via the `From<AppError> for String` impl. Add
  new error variants here, don't ad-hoc `String`.
- **Database access**: `Database` wraps `Mutex<Connection>`. Always acquire via
  the `lock_conn!($self.conn)` macro (defined in `database/mod.rs`). Put new SQL
  in `impl Database` blocks under `database/dao/`, **not** in services.
- **Schema changes**: add the `CREATE TABLE` to `schema.rs::create_tables_on_conn`
  **and** a migration branch in `apply_schema_migrations_on_conn`, then bump
  `SCHEMA_VERSION` in `database/mod.rs`. New tables get a DAO module registered
  in `dao/mod.rs`.
- **Atomic writes**: use `config::atomic_write` / `write_json_file` /
  `write_text_file` for any live config file. Never `fs::write` directly to a
  config path.
- **Serde naming**: Rust fields are `snake_case`; expose JS-facing `camelCase`
  via per-field `#[serde(rename = "...")]` (the codebase uses per-field rename,
  not `rename_all`, for providers — match the surrounding file). DB column
  names are `snake_case` and map manually in DAOs.
- **Providers**: implement the `Provider` trait (`async_trait`) in
  `proxy/providers/<name>.rs`; register in `build_provider()` (`providers/mod.rs`).
  Each provider owns its `CircuitBreaker`.
- **Live config adapters**: `services/provider/<name>.rs` exposes
  `detect()`, `read_live()`, `write_live(provider) -> LiveWriteResult`. Always
  `backup_file()` before overwriting. Dispatch new apps in
  `write_live_snapshot()` (`services/provider/mod.rs`) and add to `AppType`.
- **Logging**: use the `log` crate (`log::info!`/`warn!`/`error!`); routed to
  `tauri-plugin-log`. Match Node log tag conventions (`router:`, `circuit:`,
  `proxy:`).
- **Tests**: `#[cfg(test)] mod tests` inline. DB tests use `Database::memory()`.
  Run with `cargo test`.

### 4.3 Cross-cutting
- **Secrets**: API keys live in `~/.ai-agent-gateway/config.json` (Node) or the
  SQLite DB (Rust). Never log them, never commit them. `authToken` is optional.
- **Backups before overwrite**: every adapter write must preserve the previous
  content (`SwitchResult.previous` in Node; `backup_file` + `LiveWriteResult.backup`
  in Rust) so users can restore.
- **No comments** unless explicitly requested.

---

## 5. Build / Run / Test (verification gate)

Run the relevant subset after every change. All five green is the baseline
(verified 2026-07-25). The `aag-verify` skill automates this.

```bash
# Node src/ tree (the working proxy)
npm run typecheck        # tsc --noEmit — must be clean
npm test                 # vitest run — 8 e2e tests, mock-based, no API keys

# Rust backend
cd src-tauri && cargo check   # must be clean (warnings ok; "never used" expected mid-phase)
cd src-tauri && cargo test    # DAO + breaker + sanitize unit tests

# React UI
cd ui && npx tsc --noEmit     # must be clean
cd ui && npx vite build       # must produce ui/dist/ (Tauri build needs it)
```

Run commands:
- `npm run dev` — interactive TUI (default entry).
- `npm run gateway` — start proxy on `127.0.0.1:8787` (`-p PORT`, `-h HOST`).
- `npm run switch` — point an agent at gateway/direct/profile.
- `npm run build` / `npm start` — compiled `aag` binary.
- `npm run tauri:dev` / `npm run tauri:build` — desktop app (needs `ui/dist/` first).

**There is no `vitest.config`; tests boot the Hono app in-process via
`app.request(path, init, BASE)` — no socket, no real network.** Single test by
name: `npx vitest run -t "failover"`. No CI, no lint, no formatter config.

**Tauri gotcha**: `tauri.conf.json` references `../ui/dist` as `frontendDist`.
**`ui/dist/` must exist before `cargo check`/`cargo build`** (the
`generate_context!` proc macro panics otherwise). Run `npx vite build` in `ui/`
once before first `tauri:dev`.

---

## 6. Known quirks & traps (verified against code)

- **Streaming failover is one-shot.** `Router.stream` probes the **first chunk**
  before committing (`src/gateway/router.ts:~102`). Once a provider yields, mid-
  stream errors surface to the client — no transparent mid-stream failover.
  Don't promise it in user-facing copy.
- **`Switcher.status(id).path` is unreliable.** `src/agents/switcher.ts:91-97`
  wraps `existsSync` in `try { return true } catch { return false }` that always
  returns `true`, so `.path` is always the first candidate path. Treat as
  "first candidate", not "on-disk path".
- **`--menu` flag is a no-op** (`src/index.ts:12,55`). Launch decision checks
  `process.argv.slice(2).length === 0` only.
- **`AgentTarget.format` is unused.** All adapters write OpenAI-shaped JSON
  regardless of `'openai' | 'anthropic'`.
- **`ProviderType` includes `'gemini'` but no Gemini class exists.** `createProvider`
  falls through to `OpenAIProvider` for `'gemini'` and the default
  (`src/gateway/providers/index.ts:14-15`). Gemini is assumed OpenAI-compatible.
- **`ConfigManager.ensureAgents()` uses `require()` inside ESM**
  (`src/config/manager.ts:81`). Works under `tsx` (dev), fragile under `node`.
  Latent — no call sites today. If wired up, replace with dynamic `import()`.
- **No clean shutdown.** `startGatewayCmd` and `interactiveMenu`'s gateway
  branch both `await new Promise(() => {})`. SIGINT hard-kills; no signal
  handlers. (Rust proxy **does** have graceful shutdown via `oneshot`.)
- **Converter is text-only.** `textOf` and the request builders silently flatten
  non-string content (tool calls, images, function blocks) to `.text` or a user
  text message. Multi-modal / tool-use content is dropped.
- **Anthropic SSE emitter is synthesized, not parsed.** `oaiChunksToAnthropicStream`
  emits well-formed Anthropic events from OAI chunks; `anthropicStreamToOAIChunks`
  only listens for `message_start` / `content_block_delta` / `message_delta` /
  `message_stop`, ignoring `content_block_start`/`stop`/`ping`.
- **First `loadConfig()` writes `~/.ai-agent-gateway/config.json` to disk**
  (`src/config/store.ts:100-103`). Tests that delete the file re-create it.
- **`APPDATA` env var trusted on Windows** (`src/agents/adapters.ts:11`). On
  Linux/macOS it falls back to `~/AppData/Roaming` (wrong, but agent paths are
  Windows-oriented anyway).
- **Rust proxy is incomplete.** Only `mock` provider, non-streaming,
  `/v1/chat/completions` only. No `/v1/messages`, no SSE, no `openai`/`anthropic`
  providers yet (Phase 2 Steps 5-6).

---

## 7. Behavioral rules for the agent

1. **Inspect before editing.** Identify which tree (`src/` vs `src-tauri/` vs
   `ui/`) the task touches. Read the relevant files + this file + `DESIGN.md`/
   `PROGRESS.md` before changing anything. Prefer `rg`/reads over assumptions.
2. **One tree, one concern.** Don't mix Node and Rust changes in one pass unless
   porting. Keep edits scoped; follow existing patterns, naming, imports,
   ownership boundaries. No dependency upgrades, global formatting, or unrelated
   refactors unless required.
3. **Always verify.** After any change, run the smallest meaningful check from
   §5 (typecheck for `src/`/`ui/`, `cargo check`/`cargo test` for `src-tauri/`,
   `npm test` for gateway routing/conversion/breaker logic). Use the `aag-verify`
   skill for the full gate. If validation can't run, say why and name the next
   best command. Report what passed and what couldn't be verified.
4. **Trust code over docs.** `README.md` is stale. When in doubt, read the code.
5. **Migration discipline.** For Tauri/Rust work: read `PROGRESS.md` first, do
   **one bounded sub-task**, verify, then update `PROGRESS.md` (`current_phase`,
   `next_action`, append to `## Log`). Use the `aag-phase-loop` skill. Never
   advance a phase without green verification. Port Node semantics faithfully —
   the Rust proxy must match `src/gateway/` behavior, not invent new behavior.
6. **Never commit unless explicitly asked.** Never amend/push/force-push/create
   PRs unprompted. Before committing (when asked): inspect `git status`,
   `git diff`, `git log --oneline -10`; stage only intended files; never commit
   secrets. Write a concise message matching repo style.
7. **Secrets & live config.** Never log API keys/auth tokens. When an adapter
   writes a live agent config, always back up the previous file first (Node:
   return `previous` in `SwitchResult`; Rust: `backup_file` + `LiveWriteResult.backup`).
8. **Add tests for behavior changes.** Gateway routing, conversion, breaker, or
   adapter behavior changes must add or update a test. Node tests live in
   `tests/e2e.test.ts` (in-process Hono, mock substrate, no API keys). Rust
   tests are inline `#[cfg(test)]` modules.
9. **Don't over-promise streaming failover.** It's one-shot (first-chunk only).
   Don't claim transparent mid-stream failover in code comments or docs.
10. **No comments** unless asked. Match surrounding style. No emojis.

---

## 8. Common task playbooks (quick reference)

### Add a new upstream provider type (Node)
1. Extend `ProviderType` in `src/config/schema.ts` (currently `openai | anthropic | gemini | mock`).
2. Implement `BaseProvider` in `src/gateway/providers/<name>.ts` (`complete` + `stream`, speak canonical OAI).
3. Register in the `createProvider` factory (`src/gateway/providers/index.ts`).
4. If Anthropic-native, use `converter.ts` helpers at the provider edge.
5. Add a test in `tests/e2e.test.ts` using the mock substrate.

### Add a new upstream provider type (Rust)
1. Add the struct in `src-tauri/src/proxy/providers/<name>.rs`, implement the
   `Provider` trait (`async_trait`), own a `CircuitBreaker`.
2. Register in `build_provider()` (`proxy/providers/mod.rs`).
3. The `ProxyProvider` row's `type` column already accepts any string; no schema
   change needed.
4. Add a `#[cfg(test)]` test.

### Add a new AI coding agent/CLI (Node)
1. Add to `AgentId` enum in `src/config/schema.ts`.
2. Add an `AgentAdapter` subclass in `src/agents/adapters.ts` (override
   `buildJson`/`buildDirectJson` only) + register in `ADAPTERS` map.
3. Add default `configPaths` (Windows `%APPDATA%`, macOS `~/Library/...`,
   Linux `~/.config/...`) in `defaultAgentTargets()`.

### Add a new AI coding agent/CLI (Rust)
1. Add to `AppType` enum (`services/provider/mod.rs`) + `from_db_str`/`to_db_str`/`display_name`.
2. Create `services/provider/<name>.rs` with `detect()`/`read_live()`/`write_live()`
   (backup before overwrite). Add config path helpers in `config.rs`.
3. Dispatch in `write_live_snapshot()`.
4. Add the value to `AppType` / `APP_TYPES` in `ui/src/lib/api/types.ts` once
   the UI should expose it.

### Port a Node gateway module to Rust
Use the `aag-port-to-rust` skill. Mapping table is in `DESIGN.md` ("Migration of
existing code"). Preserve canonical-format + breaker + retriable semantics
exactly. Verify with `cargo check` + `cargo test`.

### Debug the proxy / circuit breaker
Use the `aag-gateway-debug` skill. Hit `GET /health` for breaker snapshots; use
`mock-fail` to reproduce failover; check `retriable` classification; remember
streaming failover is first-chunk only.

---

## 9. Skills available in this repo

Project skills live in `.kilo/skills/<name>/SKILL.md` and are auto-discovered.

| Skill | When to use |
|-------|-------------|
| `aag-verify` | Run the full 5-command verification gate after changes. |
| `aag-port-to-rust` | Port a `src/gateway/*` (or config/agents) module to `src-tauri/src/proxy/*`. |
| `aag-add-provider-type` | Add a new upstream API provider type (either tree). |
| `aag-add-agent-adapter` | Add a new AI coding CLI adapter (either tree). |
| `aag-gateway-debug` | Debug proxy routing, failover, or circuit-breaker behavior. |
| `aag-phase-loop` | Execute one bounded PROGRESS.md-driven migration sub-task. |

Load the matching skill before starting work that fits its description.
