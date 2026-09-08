---
name: aag-port-to-rust
description: Port a Node src/gateway/* (or config/agents) module to src-tauri/src/proxy/* (or services/database). Use when advancing a Phase 2+ migration step - preserving canonical-format, circuit-breaker, and retriable-failover semantics exactly. Includes the DESIGN.md mapping table and a per-module porting checklist.
---

# Skill: aag-port-to-rust

Port a Node `src/gateway/*` (or `src/config|agents/*`) module to its Rust
counterpart under `src-tauri/src/`. This is the active migration work (Phase 2+).
**The Rust proxy must match Node behavior, not invent new behavior.**

Before starting: read `PROGRESS.md` (`current_phase`, `next_action`) and
`DESIGN.md` ("Migration of existing code"). Do ONE module per change, verify,
then update `PROGRESS.md`.

## Mapping table (from DESIGN.md)

| Node source | Rust target |
|-------------|-------------|
| `src/config/schema.ts` (Zod) | SQLite schema in `database/schema.rs` + DAO structs in `database/dao/` |
| `src/config/store.ts` | `database/mod.rs` (Connection + migrations) + `database/seed.rs` |
| `src/config/manager.ts` | `services/*` mutators + Tauri commands in `lib.rs` |
| `src/agents/adapters.ts` | `services/provider/{claude_code,codex,...}.rs` |
| `src/agents/switcher.ts` | `provider_switch` command (`lib.rs`) + `write_live_snapshot` |
| `src/gateway/server.ts` | `proxy/server.rs` (axum, not Hono) |
| `src/gateway/router.ts` | `proxy/router.rs` |
| `src/gateway/circuit-breaker.ts` | `proxy/circuit_breaker.rs` |
| `src/gateway/converter.ts` | `proxy/convert/` (not yet created) |
| `src/gateway/providers/{base,openai,anthropic,mock}.ts` | `proxy/providers/{mod,openai,anthropic,mock}.rs` |
| `src/util/log.ts` | `log` crate + `tauri-plugin-log` |
| `tests/e2e.test.ts` | inline `#[cfg(test)]` modules + `src-tauri/tests/` integration |

## Semantics that MUST be preserved exactly

1. **Canonical OpenAI format.** Internal request/response/stream shapes are
   `OAIChatResponse` / `OAIStreamChunk` / `ProviderError`. The Rust
   `proxy/canonical.rs` must stay struct-for-struct compatible with
   `src/gateway/canonical.ts`. Conversion to/from Anthropic happens ONLY at
   edges (gateway edge + Anthropic-native provider) - never inside the router.
2. **Retriable classification.** `retriable = true` for 5xx / 429 / network;
   `false` for 4xx (400/401/403/404/422). In Rust, `make_error(msg, status,
   retriable)` defaults retriable to `status >= 500 || status.is_none()`. The
   router advances to the next hop ONLY when `retriable`; non-retriable errors
   propagate immediately. Compare `router.ts:73-81` with `router.rs:104-127`.
3. **Circuit breaker state machine.** closed/open/half-open. Lazy open->half-open
   transition happens INSIDE `allow()` when `cooldown` elapsed. `record_success`
   in half-open counts toward `success_threshold` then closes; `record_failure`
   in half-open immediately re-opens. In closed, success resets failures,
   failure increments and opens at `failure_threshold`. Node: single-threaded
   fields. Rust: `std::sync::Mutex<Inner>` (lock held microseconds).
4. **Streaming failover is one-shot.** Probe the FIRST chunk before committing;
   after the first byte, mid-stream errors surface to the client (no transparent
   failover). Match `router.ts:85-133`. (Rust streaming is Phase 2 Step 5 - not
   yet implemented; when porting, preserve this exactly.)
5. **Auto-route fallback.** If no explicit route alias matches, build a single-hop
   route from any enabled provider that directly lists the model.
   `router.ts:40-49` <-> `router.rs:48-62`.
6. **Snapshot-at-start.** Rust `Router::new` takes a DB snapshot of providers +
   routes at `proxy_start`. It is immutable per-run. (Node re-reads config at
   `Router` construction too.) Don't add hot-reload - restart to pick up changes.
7. **Auth.** Optional Bearer / `x-api-key` checked against `auth_token`. Empty
   token = auth disabled. Match `server.ts:22-32` <-> `server.rs:63-85`.
8. **Atomic writes + backup.** Config writes use `atomic_write` (temp + rename).
   Live agent config writes `backup_file()` first and return the backup path in
   `LiveWriteResult.backup` (mirrors Node `SwitchResult.previous`).

## Per-module porting checklist

### Porting a provider (`src/gateway/providers/<name>.ts` -> `proxy/providers/<name>.rs`)
- [ ] Struct owns `id`, `models`, `enabled`, and a `CircuitBreaker`.
- [ ] `impl Provider for ...` (`async_trait`): `id()`, `enabled()`, `models()`,
      `breaker()`, `async fn complete()`.
- [ ] `complete` returns `Result<OAIChatResponse, ProviderError>`. Errors via
      `canonical::make_error(msg, status, retriable)`.
- [ ] Retriability matches Node: 5xx/429/network -> retriable true; 4xx -> false.
- [ ] (When streaming lands) `async fn stream` returns an async chunk iterator;
      first-chunk probe semantics preserved.
- [ ] Register in `build_provider()` (`providers/mod.rs`) match arm. The
      `ProxyProvider.type` column already accepts any string - no schema change.
- [ ] Add `#[cfg(test)] mod tests` using a `mock-fail`-style forced error to
      exercise the retriable path.
- [ ] `cargo check` + `cargo test` green.

### Porting the converter (`converter.ts` -> `proxy/convert/`)
- [ ] Keep the TWO paired functions and their OPPOSITE edge directions straight:
      - `anthropicReqToOAIReq` runs at the **gateway edge** (incoming Anthropic
        client request -> canonical OAI).
      - `oaiReqToAnthropicReq` runs at the **Anthropic provider** (canonical OAI
        -> native Anthropic request).
- [ ] Preserve text-only flattening behavior (`textOf`) EXACTLY - do not silently
      "improve" multi-modal handling in a port; that's a separate feature. Match
      current behavior so tests stay green, then propose enhancements separately.
- [ ] SSE: `oaiChunksToAnthropicStream` synthesizes well-formed Anthropic events;
      `anthropicStreamToOAIChunks` listens for `message_start` /
      `content_block_delta` / `message_delta` / `message_stop` only. Preserve.
- [ ] Stop-reason map: `stop<->end_turn`, `length<->max_tokens`,
      `tool_calls<->tool_use`, `content_filter->end_turn`.

### Porting config/agents (Node -> Rust services/database)
- [ ] New table: add `CREATE TABLE IF NOT EXISTS` to `schema.rs::create_tables_on_conn`
      AND a `if version < N` branch in `apply_schema_migrations_on_conn`, then
      bump `SCHEMA_VERSION` in `database/mod.rs`.
- [ ] New DAO: `impl Database` block in `database/dao/<name>.rs`, register module
      in `dao/mod.rs`. All SQL lives in DAO methods, never in services.
- [ ] Serde: Rust `snake_case` fields, JS-facing `camelCase` via per-field
      `#[serde(rename = "...")]` (match surrounding file - providers use per-field
      rename, not `rename_all`).
- [ ] Live-config adapter (`services/provider/<name>.rs`): `detect()` /
      `read_live()` / `write_live(provider) -> LiveWriteResult`. ALWAYS
      `backup_file()` before overwrite; return backup path. Dispatch in
      `write_live_snapshot()` and add to `AppType`.
- [ ] Mirror the Node adapter's exact config-file JSON shape (e.g. Claude Code =
      `settings.json` with `sanitize_claude_settings_for_live` stripping
      `api_format`/`openrouter_compat_mode`; Codex = `config.toml` + `auth.json`
      with `{auth, config: <toml string>}`).

## Verification (run after every port)

```
cd src-tauri && cargo check    # must be clean
cd src-tauri && cargo test     # new test + existing pass
```
If the port touches behavior shared with Node, also run `npm test` to confirm
the Node reference still passes (it should - don't modify `src/` while porting
unless explicitly asked).

## Update the loop

After a green port: append a dated entry to `PROGRESS.md` `## Log`, update
`next_action` to the next bounded sub-task, and (if a step completed) tick its
checkbox. Never advance `current_phase` without the full `aag-verify` gate green.
