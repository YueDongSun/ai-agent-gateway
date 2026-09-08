---
name: aag-add-agent-adapter
description: Add a new AI coding CLI/agent adapter (e.g. a new tool whose config file AAG should rewrite to point at the gateway or a direct provider). Covers both the Node src/agents adapter + switcher wiring and the Rust src-tauri services/provider live-config adapter. Use when a task asks to support switching a new coding tool.
---

# Skill: aag-add-agent-adapter

Add support for a **new AI coding CLI/agent** so AAG can rewrite its config file
to point at the gateway or a direct upstream provider. This is the config-
switcher side (rewriting a tool's config file), NOT a new LLM backend - see
`aag-add-provider-type` for that.

## Decide which tree

- **Node** (`src/agents/`) - the working CLI/TUI switcher today (6 agents:
  Copilot, Kilo, Cursor, Zcode, MiniMax, Kimi).
- **Rust** (`src-tauri/src/services/provider/`) - the desktop live-config
  switcher (8 apps in `AppType`: ClaudeCode, ClaudeDesktop, Codex, Gemini,
  OpenCode, OpenClaw, Hermes, GrokBuild; only ClaudeCode + Codex implemented).
- These two trees have DIFFERENT agent sets by design (Node targets the
  Copilot/Kilo/Cursor family; Rust targets the cc-switch Claude/Codex family).
  Add to whichever the task specifies.

## Node: add an agent (`src/agents/`)

### 1. Extend the enum
`src/config/schema.ts` - `AgentId` is `z.enum(['copilot','kilo','cursor','zcode','minimax','kimi'])`.
Add your new id string.

### 2. Add default config paths
`src/agents/adapters.ts` - `defaultAgentTargets()` returns an `AgentTarget[]`.
Add an entry with `id`, `name`, `format: 'openai'` (note: `format` is currently
**unused** - all adapters write OpenAI-shaped JSON), and `configPaths` covering:
- Windows: `%APPDATA%` (`process.env.APPDATA ?? join(HOME,'AppData','Roaming')`)
- macOS: `~/Library/...`
- Linux: `~/.config/...` or `~/.<agent>/...`

List candidate paths in priority order; first existing/writable wins
(`resolveConfigPath`).

### 3. Add the adapter subclass
`src/agents/adapters.ts` - extend `JsonAgentAdapter` (the template-method base).
Override ONLY the two JSON-shape hooks:

```ts
export class MyAdapter extends JsonAgentAdapter {
  buildJson(gw: string, tok: string, model: string) {
    return { /* the exact JSON shape this agent's config file expects,
               pointing at `${gw}/v1` with apiKey tok||'gateway' and model */ };
  }
  buildDirectJson(baseURL: string, apiKey: string, model: string) {
    return { /* same shape but pointing at the direct provider */ };
  }
}
```

The base class handles `resolveConfigPath`/`read`/`write`/`detectCurrentBaseURL`
and serializes to pretty JSON. Look at `CursorAdapter` (nested `openai:` key),
`MinimaxAdapter` (nested `llm:` key), `KimiAdapter` (flat) for shape variety.

### 4. Register in the factory
`src/agents/adapters.ts` - add to the `ADAPTERS` map:
```ts
const ADAPTERS = { /* ... */ , myagent: MyAdapter } as const;
```
`createAdapter(target)` and `detectInstalledAgents()` pick it up automatically.

### 5. Verify + test
- `npm run typecheck` + `npm test`.
- The switcher (`Switcher` in `switcher.ts`) needs no change - it dispatches via
  `createAdapter`. `pointToGateway`/`pointToProvider`/`pointToProfile` all work.
- Optionally exercise via `npm run dev` -> "Switch agent target".

### Node gotchas
- **`Switcher.status(id).path` is unreliable** (`switcher.ts:91-97`) - it always
  returns the first config path. Don't rely on it for "does the file exist".
- **`buildJson`/`buildDirectJson` must back up first.** The base `write` does
  not back up; `Switcher.pointTo*` reads `adapter.read()` into `previous` BEFORE
  writing. Keep that ordering if you touch the switcher.
- **`APPDATA` is trusted on Windows** and falls back to a wrong path on Unix
  (`adapters.ts:11`). Agent paths are Windows-oriented; don't "fix" this without
  checking all existing paths.

## Rust: add an app (`src-tauri/src/services/provider/`)

### 1. Extend AppType
`src-tauri/src/services/provider/mod.rs` - add a variant to the `AppType` enum
and its three arms: `from_db_str` (string -> enum), `to_db_str` (enum ->
`app_type` column string), `display_name` (human label). The `to_db_str` value
is what's stored in the `providers.app_type` column and what the UI sends.

### 2. Add config path helpers
`src-tauri/src/config.rs` - add `get_<app>_config_dir()` / `get_<app>_<file>_path()`
mirroring `get_claude_settings_path()` / `get_codex_config_path()`. Use
`dirs::home_dir()`; respect `AAG_CONFIG_DIR` only for AAG's own DB, not agent
paths.

### 3. Create the adapter module
`src-tauri/src/services/provider/<name>.rs` - implement three fns:

```rust
pub fn detect() -> bool { /* config dir/file exists */ }
pub fn read_live() -> Result<Option<Value>, AppError> { /* read current config */ }
pub fn write_live(provider: &Provider) -> Result<LiveWriteResult, AppError> {
    // 1. Resolve target path(s).
    // 2. Backup existing file(s) via backup_file() into get_backups_dir()?.
    // 3. Write atomically via write_json_file / write_text_file.
    // 4. Return LiveWriteResult { app_type, provider_id, written: Vec, backup: Option }.
}
```

Mirror the reference: `claude_code.rs` (single `settings.json`, with a
`sanitize()` stripping internal-only fields like `api_format`/
`openrouter_compat_mode`) and `codex.rs` (two files: `config.toml` TOML string +
`auth.json` JSON, settings_config shape `{auth, config}`).

### 4. Dispatch
`src-tauri/src/services/provider/mod.rs` - add the arm to `write_live_snapshot()`:
```rust
AppType::MyApp => my_app::write_live(provider),
```
Remove it from the "not yet implemented" stub list.

### 5. Expose in the UI (when ready)
`ui/src/lib/api/types.ts` - add the value to the `AppType` union AND to the
`APP_TYPES` array (label). The `ProvidersPage` tab list is driven by `APP_TYPES`.

### 6. Verify + test
- Add a `#[cfg(test)]` test (e.g. sanitize stripping, or shape validation).
- `cargo check` + `cargo test`; `cd ui && npx tsc --noEmit` if you touched types.

## Shared rules
- **Always back up before overwriting** a live config file (Node `previous` /
  Rust `backup_file` + `LiveWriteResult.backup`). Users must be able to restore.
- **Never log secrets.** API keys/tokens pass through `settings_config`.
- **Match the tool's real config-file shape.** When unsure, read cc-switch's
  `services/provider/` reference adapters (path in `DESIGN.md`) or the tool's
  own docs - the on-disk shape is the contract.
- **The Node and Rust agent sets intentionally differ.** Don't try to unify them
  unless the task explicitly asks for cross-tree parity.
