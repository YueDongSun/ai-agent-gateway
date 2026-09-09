# DESIGN.md — Migrating `ai-agent-gateway` to cc-switch feature parity

> Status: design phase. The current `src/` (Node/TypeScript proxy + CLI/TUI) remains
> the working proxy. The Tauri desktop shell (`src-tauri/`, `ui/`) is being added
> alongside and will eventually subsume the CLI/TUI.

## Reference implementation

The reference is the upstream project **cc-switch** by Jason Young (MIT, 121k stars):

- Source: <https://github.com/farion1231/cc-switch>
- Site: <https://ccswitch.io/zh/>
- Local source checkout at `~/Documents/Codex/2026-07-05/ben/work/cc-switch-src/`
  (read-only reference for this migration).

Upstream stack: Tauri 2.8 + Rust backend + React 18 + TypeScript + Vite + Tailwind + shadcn/ui + TanStack Query + react-i18next + Zod + SQLite (via `rusqlite`).

## Goals

| # | Goal | Notes |
|---|------|-------|
| G1 | Native desktop UI with system tray, deep links, multi-window | Tauri 2.x |
| G2 | SQLite SSOT with atomic writes and migrations | `rusqlite` (or `sqlx`) |
| G3 | Multi-tool provider switching for ≥8 AI coding CLIs | Claude Code, Codex, Gemini CLI, Grok Build, OpenCode, OpenClaw, Hermes, Claude Desktop |
| G4 | Local proxy with circuit breaker + failover + health | Reuse logic from existing `src/gateway/`, port to Rust |
| G5 | MCP server bidirectional sync | New feature |
| G6 | Skills symlink management | New feature |
| G7 | Session manager (history browse/search/restore) | New feature |
| G8 | Cloud sync (S3 + WebDAV) | Optional, lower priority |
| G9 | Usage dashboard with per-model pricing | New feature |
| G10 | Deep link import (`aag://...`) | Tauri plugin |
| G11 | i18n (zh, en) | `react-i18next` |
| G12 | Backup/restore with rotation | `~/.aag/backups/` |

## Non-goals (this iteration)

- Full parity with all 50+ cc-switch provider presets (start with 6: Claude, OpenAI, Gemini, Anthropic, OpenRouter, Moonshot/Kimi).
- App Store / Microsoft Store distribution.
- Mobile (Android/iOS) bundles — Tauri 2 supports them, but skipping initially.
- Auto-update channel.

## Architecture

### High-level layout

```
.
├── src/                       # Existing Node/TypeScript proxy (unchanged short-term)
│   ├── gateway/               # Reused by both CLI and embedded in Tauri shell
│   ├── agents/                # Reused for switching
│   ├── cli/                   # Kept for `aag` CLI; Tauri shell calls into this layer
│   ├── config/                # JSON config; replaced by SQLite as SSOT later
│   └── util/
│
├── src-tauri/                 # NEW: Rust backend
│   ├── Cargo.toml
│   ├── tauri.conf.json
│   ├── build.rs
│   ├── icons/
│   ├── capabilities/
│   └── src/
│       ├── main.rs            # Tauri app entry
│       ├── lib.rs             # `pub fn run()` — registers commands and plugins
│       ├── app_config.rs      # Tauri store plugin wrapper
│       ├── database/          # SQLite SSOT (schema + DAOs + migrations + backup)
│       ├── proxy/             # Local proxy (port of src/gateway/)
│       │   ├── server.rs
│       │   ├── circuit_breaker.rs
│       │   ├── failover_switch.rs
│       │   ├── forwarder.rs
│       │   ├── provider_router.rs
│       │   └── providers/
│       ├── services/          # Business logic
│       │   ├── provider/      # Adapter registry (8 tools)
│       │   ├── profile.rs
│       │   ├── prompt.rs
│       │   ├── mcp.rs
│       │   ├── skill.rs
│       │   ├── session_manager.rs
│       │   ├── usage.rs
│       │   └── sync/          # S3 + WebDAV (later)
│       └── commands/          # `#[tauri::command]` surface, mirrors services/
│
├── ui/                        # NEW: React frontend
│   ├── index.html
│   ├── vite.config.ts
│   ├── tailwind.config.cjs
│   ├── src/
│   │   ├── main.tsx
│   │   ├── App.tsx
│   │   ├── routes/            # Page-level components
│   │   ├── components/        # UI primitives + composite views
│   │   │   ├── providers/
│   │   │   ├── proxy/
│   │   │   ├── mcp/
│   │   │   ├── skills/
│   │   │   ├── sessions/
│   │   │   ├── usage/
│   │   │   └── settings/
│   │   ├── hooks/             # React hooks (TanStack Query wrappers)
│   │   ├── lib/
│   │   │   ├── api/           # `invoke<T>(...)` typed wrappers
│   │   │   └── query/         # QueryClient + key factory
│   │   ├── locales/           # en.json, zh.json
│   │   └── styles/
│   └── tests/
│
├── docs/                      # NEW: user + dev docs
│
└── DESIGN.md (this file)
```

### Data model (SQLite, draft)

```sql
-- Top-level config + sync metadata
CREATE TABLE app_meta (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

-- Provider configurations (one row per tool × preset combo)
CREATE TABLE providers (
  id TEXT PRIMARY KEY,                  -- e.g. "claude-anthropic-prod"
  app_id TEXT NOT NULL,                  -- 'claude_code' | 'codex' | 'gemini_cli' | 'opencode' | 'openclaw' | 'hermes' | 'claude_desktop' | 'grok_build'
  name TEXT NOT NULL,
  preset_key TEXT,                       -- e.g. "anthropic-claude-pro"; NULL if custom
  api_key TEXT NOT NULL DEFAULT '',     -- stored as-is; encryption TBD
  base_url TEXT NOT NULL DEFAULT '',
  model TEXT,
  models TEXT NOT NULL DEFAULT '[]',     -- JSON array
  enabled INTEGER NOT NULL DEFAULT 1,
  weight INTEGER NOT NULL DEFAULT 1,
  settings TEXT NOT NULL DEFAULT '{}',   -- provider-specific JSON
  notes TEXT NOT NULL DEFAULT '',
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE INDEX idx_providers_app ON providers(app_id);

-- A "current" pointer per app: which provider is active for that AI coding tool
CREATE TABLE active_providers (
  app_id TEXT PRIMARY KEY,
  provider_id TEXT NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
  switched_at TEXT NOT NULL
);

-- Ordered failover chain per model alias
CREATE TABLE routes (
  id TEXT PRIMARY KEY,
  alias TEXT NOT NULL,                   -- "claude-sonnet-4"
  description TEXT NOT NULL DEFAULT '',
  hops TEXT NOT NULL,                    -- JSON: [{"provider_id":"...","model":"..."}]
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  UNIQUE(alias)
);

-- Per-provider circuit breaker settings
CREATE TABLE circuit_breaker_configs (
  provider_id TEXT PRIMARY KEY REFERENCES providers(id) ON DELETE CASCADE,
  failure_threshold INTEGER NOT NULL DEFAULT 5,
  cooldown_ms INTEGER NOT NULL DEFAULT 30000,
  half_open_retries INTEGER NOT NULL DEFAULT 1,
  success_threshold INTEGER NOT NULL DEFAULT 2
);

-- Backup snapshots
CREATE TABLE backups (
  id TEXT PRIMARY KEY,
  label TEXT NOT NULL,
  created_at TEXT NOT NULL,
  data BLOB NOT NULL                     -- JSON dump of providers + routes
);

-- MCP servers (per-app list)
CREATE TABLE mcp_servers (
  id TEXT PRIMARY KEY,
  app_id TEXT NOT NULL,
  name TEXT NOT NULL,
  config TEXT NOT NULL,                  -- JSON {command, args, env, ...}
  enabled INTEGER NOT NULL DEFAULT 1,
  UNIQUE(app_id, name)
);

-- Skills (symlink targets)
CREATE TABLE skills (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL UNIQUE,
  source_path TEXT NOT NULL,
  enabled_apps TEXT NOT NULL DEFAULT '[]'  -- JSON array of app_ids
);

-- Session history (for session manager)
CREATE TABLE sessions (
  id TEXT PRIMARY KEY,
  app_id TEXT NOT NULL,
  started_at TEXT NOT NULL,
  ended_at TEXT,
  message_count INTEGER NOT NULL DEFAULT 0,
  summary TEXT NOT NULL DEFAULT '',
  raw_path TEXT                          -- filesystem location of original log
);

CREATE INDEX idx_sessions_app_time ON sessions(app_id, started_at DESC);

-- Usage records (for dashboard)
CREATE TABLE usage_records (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  ts TEXT NOT NULL,
  app_id TEXT NOT NULL,
  provider_id TEXT NOT NULL,
  model TEXT NOT NULL,
  prompt_tokens INTEGER NOT NULL DEFAULT 0,
  completion_tokens INTEGER NOT NULL DEFAULT 0,
  cost_usd REAL NOT NULL DEFAULT 0,
  duration_ms INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX idx_usage_ts ON usage_records(ts DESC);
CREATE INDEX idx_usage_provider ON usage_records(provider_id, ts DESC);
```

### Tauri command surface (draft — mirrors `services/`)

| Command | Args → Result | Purpose |
|---------|----------------|---------|
| `provider_list` | `(app_id?) → Provider[]` | List providers, optionally filtered |
| `provider_get` | `(id) → Provider` | Fetch one |
| `provider_create` | `(ProviderInput) → Provider` | Insert |
| `provider_update` | `(id, ProviderPatch) → Provider` | Patch |
| `provider_delete` | `(id) → ()` | Remove |
| `provider_switch` | `(app_id, provider_id) → ()` | Set active; rewrites live config |
| `provider_active` | `(app_id) → Provider?` | Current pointer |
| `route_list` / `route_get` / `route_upsert` / `route_delete` | ... | Route CRUD |
| `proxy_start` / `proxy_stop` / `proxy_status` | ... | Lifecycle |
| `proxy_chat` | `(ChatRequest, stream?) → ChatResponse` | Direct call, bypasses HTTP |
| `proxy_health` | `() → ProxyHealth` | Provider health snapshot |
| `breaker_status` / `breaker_reset` | `(provider_id?)` | Circuit breaker ops |
| `mcp_list` / `mcp_upsert` / `mcp_delete` | per-app | MCP management |
| `mcp_sync_to_apps` / `mcp_sync_from_apps` | `(server_ids?)` | Bidirectional sync |
| `skill_list` / `skill_link` / `skill_unlink` | `(name, app_ids)` | Skill symlinks |
| `session_list` / `session_get` / `session_restore` | ... | History |
| `usage_summary` / `usage_by_model` / `usage_export_csv` | ... | Dashboard |
| `backup_create` / `backup_list` / `backup_restore` / `backup_delete` | ... | Backup |
| `deeplink_handle` | `(url)` | Parse `aag://...` |
| `settings_get` / `settings_set` | ... | App-level settings |

### Proxy subsystem

Port `src/gateway/` semantics to Rust:
- `proxy/server.rs` — hyper-based HTTP server (not Hono). Listens on `127.0.0.1:<port>` (configurable).
- `proxy/circuit_breaker.rs` — closed/open/half-open state machine, same shape as the current Node version, but Rust.
- `proxy/failover_switch.rs` — hop iteration with retriable-error classification.
- `proxy/provider_router.rs` — alias → hops resolution, with single-hop auto-route fallback.
- `proxy/providers/{openai,anthropic,gemini,claude,codex,openai_compat}.rs` — each implements a common `Provider` trait.
- `proxy/forwarder.rs` — request/response streaming with mid-stream abort detection.

The existing Node proxy stays as a fallback path until the Rust one is feature-complete.

### Adapter pattern for AI coding CLIs

Mirrors `src/agents/adapters.ts` but per app:

| App | Detected by | Config file(s) written | Format |
|-----|-------------|------------------------|--------|
| `claude_code` | `~/.claude.json` or `~/.config/claude/...` | `settings.json` (OpenAI-style provider block) | JSON |
| `claude_desktop` | macOS app bundle | `~/Library/Application Support/Claude/claude_desktop_config.json` | JSON |
| `codex` | `~/.codex/config.toml` | `config.toml` | TOML |
| `gemini_cli` | `~/.gemini/settings.json` | `settings.json` | JSON |
| `opencode` | `~/.config/opencode/opencode.json` | `opencode.json` | JSON |
| `openclaw` | `~/.openclaw/config.json` | `config.json` | JSON |
| `hermes` | `~/.hermes/config.json` | `config.json` | JSON |
| `grok_build` | `~/.grok/config.json` | `config.json` | JSON |

Each adapter exposes `detect()`, `read()`, `write(provider)`, `restore_backup()`, `watch()` (for live-file back-sync).

## Phasing

**Phase 0 — Tauri skeleton (this iteration)** ✅ in progress
- `src-tauri/Cargo.toml` + `tauri.conf.json` + minimal `main.rs`/`lib.rs`.
- `ui/` with Vite + React + Tailwind + shadcn/ui baseline, an empty `App.tsx` that opens a window saying "AAG".
- No functional parity yet. Tauri `dev` boots, window appears.

**Phase 1 — Provider CRUD + active switching**
- Implement `providers` + `active_providers` tables + DAOs.
- Implement `provider_*` and `provider_switch` commands.
- Port `src/agents/adapters.ts` adapters to Rust (start with Claude Code + Codex).
- Frontend: `Providers` page with list/edit/switch.

**Phase 2 — Local proxy + failover**
- Port `src/gateway/*` to `src-tauri/src/proxy/*`.
- `proxy_start/stop/status/chat/breaker_*` commands.
- Frontend: `Proxy` page with health visualization.

**Phase 3 — Routes + breaker configs**
- `routes` table + DAOs + commands.
- Frontend: `Routes` page.

**Phase 4 — Backup/restore**
- `backups` table, auto-rotation (keep 10), restore from disk.

**Phase 5 — MCP + Skills**
- New tables, new services, new UI pages.

**Phase 6 — Session manager**
- `sessions` table, ingest from each app's history location, restore = symlink/copy.

**Phase 7 — Usage tracking + dashboard**
- `usage_records` table, populate from proxy responses, frontend charts.

**Phase 8 — Cloud sync**
- S3 + WebDAV plugins (defer until 1–7 stable).

**Phase 9 — Deep links + auto-update + tray polish**

## Migration of existing code

| Existing | Becomes |
|----------|---------|
| `src/config/schema.ts` (Zod) | SQLite schema in `database/schema.rs` + DAOs |
| `src/config/store.ts` | Database connection + migrations |
| `src/config/manager.ts` | `services/*` mutators |
| `src/agents/adapters.ts` | `services/provider/{claude_code,codex,...}.rs` |
| `src/agents/switcher.ts` | `commands::provider_switch` |
| `src/gateway/server.ts` | `proxy/server.rs` (hyper) |
| `src/gateway/router.ts` | `proxy/provider_router.rs` |
| `src/gateway/circuit_breaker.ts` | `proxy/circuit_breaker.rs` |
| `src/gateway/converter.ts` | `proxy/convert/` |
| `src/gateway/providers/{base,openai,anthropic,mock}.rs` | `proxy/providers/{openai,anthropic,gemini,...}.rs` |
| `src/cli/commands.ts` | Removed; replaced by `ui/` and minimal CLI shim |
| `src/cli/menu.ts` | Removed (TUI subsumed by desktop UI) |
| `src/util/log.ts` | `tauri-plugin-log` + a thin Rust wrapper |
| `tests/e2e.test.ts` | `src-tauri/tests/` Rust integration tests + `ui/tests/` Vitest |

The Node proxy remains in `src/` and is still runnable via `npm run gateway` until Phase 2 ships. CLI is preserved as a thin shim calling the Tauri commands via `tauri-cli` (or directly invoking the Rust binary with JSON args) so that headless / CI workflows keep working.

## Open questions

1. **Encryption at rest** for `providers.api_key` — cc-switch does NOT encrypt. We should at minimum defer to OS keychain (macOS Keychain / Windows Credential Manager / Linux Secret Service) via `keyring` crate. Decide before Phase 1.
2. **pnpm vs npm** — Tauri works with npm, but cc-switch uses pnpm. Stick with npm (already wired) or add pnpm? Recommend: stay on npm for this repo.
3. **Bundle signing / auto-update** — defer to Phase 9.
4. **i18n priority** — start with `en.json` only, add `zh.json` once UI stabilizes.
5. **Distribution** — `tauri build` produces `.msi/.dmg/.deb/.rpm/.AppImage`. Decide which to publish first.

## Immediate next steps (this iteration)

- [x] Decide scope and toolchain (done).
- [x] Read cc-switch source structure (done in parallel sweeps).
- [ ] Produce minimal Tauri skeleton that boots a window.
- [ ] Add SQLite dependency + a minimal `migrations/0001_init.sql`.
- [ ] Wire one Tauri command end-to-end (`provider_list → []`) so the React side can `invoke` it.
- [ ] Update `CLAUDE.md` to reflect the new repo shape.

Subsequent loops will deepen each module according to the phasing above.