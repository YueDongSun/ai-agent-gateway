# PROGRESS.md - Tauri migration state

> **This file is the loop's memory.** Each cron iteration must:
> 1. Read this file to find `current_phase`.
> 2. Read `DESIGN.md` for that phase's spec.
> 3. Implement a bounded slice (one sub-task per iteration max).
> 4. Run verification (see `## Verification commands`).
> 5. Update `current_phase`, append to `## Log`, set `next_action`.

### Phase 21 - Fix tool-use fidelity (Claude Code actually works) (2026-08-22) ✅ DONE

**Root cause:** `/v1/messages` end-to-end lost tool semantics. The converter
in Phase 18 was a "text flattener" — it extracted `text_of(content)` for
every Anthropic content block and emitted one big string, dropping
`tool_use`, `tool_result`, `image`, and `thinking` blocks. Upstream
therefore never received `tools` definitions, so Claude Code's request
fell through to text generation and the model regurgitated system-prompt
examples (e.g. `<title>BAD/LOST spikes...</title>`).

**Fix** — port the design from `cc-switch/providers/transform.rs` +
`cc-switch/providers/streaming.rs`:

**Backend:**
- `proxy/canonical.rs`: `ChatCompletionRequest` gains `tools: Option<Vec<Value>>`
  + `tool_choice: Option<Value>`; `ChatMessage` gains `tool_calls: Option<Vec<Value>>`
  + `tool_call_id: Option<String>`; `OAIMessage` gains `tool_calls` + `refusal`;
  `OAIStreamChunk`/`StreamChoice` gain `#[serde(flatten)] extra`;
  `StreamDelta` gains `tool_calls` + `reasoning`.
- `proxy/converter.rs`: rewrites both static and streaming converters.
  - `anthropic_req_to_oai_req`: walks each content block (text / image /
    tool_use / tool_result), promotes `tool_use` to `message.tool_calls`,
    promotes `tool_result` blocks into separate `role: tool` messages,
    copies `tools` / `tool_choice` from `extra`.
  - `oai_req_to_anthropic_req`: inverse; OpenAI `tool_calls` → Anthropic
    `tool_use` blocks; `role: tool` messages → Anthropic `user` messages
    with `tool_result` blocks; `tool_choice` mapped (required ↔ any,
    function ↔ tool+name).
  - `anthropic_resp_to_oai_resp` / `oai_resp_to_anthropic_resp`: `tool_use`
    blocks ↔ `tool_calls` array; `stop_reason` ↔ `finish_reason`
    (`tool_use` ↔ `tool_calls`).
  - `oai_chunks_to_anthropic_stream` (used for `/v1/messages` responses):
    - `HashMap<usize, ToolBlockState>` keyed by OpenAI `tool_calls.index`
    - Allocates `anthropic_index` immediately on first sight of an
      OpenAI index (id/name may arrive in later chunks), buffers
      `pending_args` fragments, flushes them as `input_json_delta`
      once the `content_block_start {type:tool_use, id, name}` fires.
    - Late-starts blocks whose id/name never arrived at finish_reason
      (fallback `id = "tool_call_{idx}"`, `name = "unknown_tool"`).
    - Caches the final `message_delta` (with stop_reason + usage) until
      stream end so duplicates never reach Claude Code (which aborts).
    - Adjusts `input_tokens` from `prompt_tokens - cache_read - cache_creation`
      (OpenAI bundles cache tokens into prompt).
  - `anthropic_stream_to_oai_chunks` (used for `/v1/chat/completions`
    responses from Anthropic upstreams): translates `tool_use` start →
    `delta.tool_calls[0].id/name`, `input_json_delta` → `function.arguments`,
    `message_delta {stop_reason: "tool_use"}` → `finish_reason: "tool_calls"`.
  - `responses_to_chat` carries Responses API `tools` into canonical
    `ChatCompletionRequest.tools`.
- `proxy/router.rs`: `sub_req` clones `tools` + `tool_choice` (the
  load-bearing reason the rest of the proxy — failover chains, queue,
  pinned provider — keeps working with the new fields).
- `proxy/providers/mock.rs`: 4 `StreamDelta` literals updated for new
  fields.
- `lib.rs:proxy_test_provider`: test request adds the new fields.
- System-prompt prefix strip: Anthropic's
  `x-anthropic-billing-header: ...` line is removed from the system
  block (it eats prompt-cache prefix and confuses some upstreams).

**Tests (added in `proxy/converter.rs`):**
- `roundtrip_anthropic_req_with_tools` — Anthropic → OAI → Anthropic
  with a tool_use + tool_result, asserts both blocks survive.
- `roundtrip_anthropic_resp_with_tool_use` — `tool_use` response block
  → `tool_calls` array; `tool_use` stop_reason → `tool_calls` finish_reason.
- `tool_choice_mapping_round_trip` — required ↔ any, function ↔ tool+name.
- `streaming_oai_tool_calls_to_anthropic` — id-first, name-second,
  arguments fragments across chunks → correct Anthropic event sequence
  with content_block_start + input_json_delta + content_block_stop +
  message_delta(stop_reason=tool_use).
- `streaming_oai_text_then_tool_calls` — text block closes before
  tool block opens.
- `streaming_anthropic_tool_use_to_oai` — reverse direction: tool_use
  start → tool_calls delta with id/name; input_json_delta →
  function.arguments; finish_reason tool_use → tool_calls.

**Verified:** cargo check ✅ · cargo test --lib **115/115** ✅
(was 108/108 before; +7 new) · ui tsc ✅ · vite build ✅ (no UI
changes; sizes identical).

### Phase 20 - Fix hop model picker (cc-switch pattern) (2026-08-21) ✅ DONE

Root cause: `ModelSuggest.accept()` called `inputRef.current.focus()` which triggered
`onFocus={() => setOpen(true)}`, reopening the menu in the same React 18 batch —
user saw the menu never close, thought the pick didn't land. Value actually saved,
but invisible to the user.

**Fix:** Rewrite `ModelSuggest.tsx` following cc-switch's `ModelInputWithFetch` pattern:
- Menu rendered via `createPortal(document.body)` + `position: fixed` (input rect from
  `getBoundingClientRect`) — immune to table/sticky-toolbar clipping.
- Separate ▾ chevron button toggles menu (NOT onFocus).
- ⤓ fetch-upstream button: calls `fetchProviderModels(providerId)`, caches results,
  merges into `knownModels`. Only shown when hop.provider is non-empty.
- Item click: `onMouseDown` + `preventDefault` + `accept(opt)`. NO `inputRef.focus()`.
- Outside-close: document mousedown checks BOTH wrapRef AND portalRef.
- Close on scroll/resize (capture listener).
- Keyboard: ↓/↑/Enter/Escape (same as before, Enter requires highlighted item).

**Additional defensive fixes:**
- `handleAddHop`/`handleRemoveHop`/`handleEditHop`/`handleHopDrop` converted to
  functional `setRoutes(prev => ...)` form (guards against stale-closure edge cases).
- CSS: `.model-suggest` changed from absolute-positioned toggles to flex row
  (input + buttons side-by-side, no overlap); `.model-suggest-menu` z-index 9999;
  portal menu uses inline fixed coords (no CSS position overrides needed).
- Mock backend: `proxy_fetch_models` case returns 10 demo models (7 Anthropic + 3 Zhipu).
- i18n: `route.fetchModels.empty`, `route.fetchModels.failed` (en + zh).

**Verified:** ui tsc ✅; vite build ✅ (50 modules, CSS 19.1 KB); cargo check ✅.

### Phase 19 - cc-switch model mapping import + route editor polish (2026-08-21) ✅ DONE

cc-switch stores per-tier model mapping inside `providers.settings_config.env`
(`ANTHROPIC_DEFAULT_HAIKU/SONNET/OPUS/FABLE_MODEL` + `ANTHROPIC_MODEL`). The
Phase 18 importer only copied provider rows; it never translated those env
keys into AAG's `routes[].hops[].model` form. Phase 19 closes that loop end to
end and rebuilds the route editor UX so the drag-and-drop actually works.

**Backend:**
- `services/import_cc_switch/mod.rs`: importer now reads each imported
  Claude provider's `env.ANTHROPIC_DEFAULT_*_MODEL` keys and auto-seeds a
  failover route for every official Claude model (fable-5, opus-4-8/7/6,
  sonnet-5/4-6, haiku-4-5) using the imported provider as the head hop. If a
  route already exists, the new provider is prepended (preserving any
  user-tuned order); re-imports are idempotent.
- `ImportReport.imported_routes` field added so the UI toast can report the
  route count alongside the provider count.
- 2 new tests: `import_seeds_routes_from_anthropic_default_models` and
  `reimport_prepends_new_provider_to_existing_route`.

**Frontend:**
- `AppPage.tsx`: route editor rebuilt as a real `<table>` with a sticky
  toolbar (title + batch delete + add route + save/discard), per-route
  collapse/expand, batch-select checkboxes, fixed column widths via
  `<colgroup>`. Drag handle moved to its own column so children
  `<select>`/`<input>` no longer swallow mousedown (Phase 18 bug — drag
  only fired from the cosmetic handle anyway). Model input now uses a
  `<datalist>` populated from `CLAUDE_MODELS + CODEX_MODELS + each upstream
  provider's models[]`.
- `styles.css`: new `.route-editor` / `.route-toolbar` / `.route-card` /
  `.hop-table` / `.hop-drag-handle` / `.restart-hint` classes (CSS went
  from 13.9 KB → 17.4 KB). The previous `.hop-row.drag-over` rule now also
  matches `.model-catalog-row.drag-over` so the hover highlight actually
  fires.
- `lib/api/types.ts` + `lib/mock/backend.ts`: `ImportReport.importedRoutes`
  typed end-to-end.
- `i18n/{en,zh}.ts`: 9 new keys (`route.deleteSelected`,
  `route.deleteSelectedConfirm`, `route.selectForBatch`, `route.hopCount`,
  `route.noHops`, `route.expand`, `route.collapse`,
  `chain.importCcSwitch.routes`). Import toast now reads "Imported N ·
  Skipped M · routes auto-seeded: K" when applicable.

**Verified:** cargo test --lib 108/108 (+2 new); ui tsc clean; vite build
clean (49 modules, CSS 13.9 KB → 17.4 KB); Node typecheck + 18/18 tests
pass.

### Phase 18 - Unified failover route editor + Codex model mapping (2026-08-20) ✅ DONE

Replaced the fragmented model mapping systems (unified editor, per-provider modelMapping,
model catalog in modal) with a single **route-based failover editor** shared by Claude and
Codex pages. Each route = client-facing model name → ordered hops (provider + upstream model).

**Backend:**
- `proxy/models.rs`: added `CODEX_MODELS` constant (codex-5-6-sol, codex-5-6-luna, codex-5-6-terra, codex-5-5).
- `database/schema.rs`: schema v18 (no-op stamp; Codex routes seeded in `seed_proxy_if_empty`).
- `database/seed.rs`: `seed_proxy_if_empty` now seeds 4 default Codex routes alongside Claude official routes.
- `database/dao/routes.rs`: added `get_route(alias)` method for single-route lookups.
- `services/import_cc_switch/mod.rs`: import now auto-creates failover routes for all official Claude models using imported providers as hops.
- `e2e.rs`: updated route count assertion to include Codex models (105/105 tests pass).

**Frontend:**
- `appPages.ts`: replaced dead `CODEX_MODELS` with new constants (`codex-5-6-sol` etc.) + `CODEX_DISPLAY` map.
- `AppPage.tsx`: removed unified model mapping editor (`editingMapping`/`handleSaveMapping`/`loadMappingFromProviders`); removed model catalog from Codex provider modal; removed model mapping from proxy provider modal; added route CRUD state + handlers (`handleAddRoute`/`handleDeleteRoute`/`handleAddHop`/`handleRemoveHop`/`handleEditHop`/`handleHopDrop`/`handleSaveRoutes`); added route editor UI on both Claude (inside upstream pool section) and Codex (standalone card) pages with drag-reorderable hops.
- `i18n/en.ts` + `zh.ts`: added `route.*` keys for the route editor.
- `mock/backend.ts`: added Claude + Codex route seed data.

**Verified:** cargo check clean; cargo test --lib 105/105; ui tsc clean; vite build clean (49 modules).

### Phase 17 - Per-app provider chains (cc-switch style) ✅ DONE (2026-08-19)

All 4 apps (Claude Code, Codex, Copilot CLI, pi) now have independent, drag-reorderable
provider chains with per-card failover toggle, analogous to cc-switch's universal card list.

**Backend:**
- Schema v15 migration: `in_failover_queue` default flipped to 1. `DAO::set_in_failover_queue` added.
- `provider_set_failover_queue` Tauri command (per-card toggle).
- `provider_switch` expanded: Codex writes mirror CODEX_HOME, Copilot merges settings.json, pi upserts models.json+settings.json, claude_code stays write-less.
- `queue_endpoint_of` extracts upstream URL/key from all 4 app shapes.
- `start_proxy_impl` builds per-app queue-<id> proxy providers for all 4 apps; Router gets `with_queues` + `complete_for`/`stream_for`.
- `server.rs`: `/v1/messages` -> Claude chain, `/v1/responses` -> Codex chain, `/v1/chat/completions` -> Copilot+pi chain.
- 3 new router tests + TOML base_url parsing fix.

**Frontend:**
- New `lib/appPages.ts` per-app config (fields, buildConfig, extractForm, cardUrl, surface, launchers).
- New `components/app/AppPage.tsx`: gateway card + provider chain (drag-reorder, CURRENT, +chain/-chain toggle, Enable) + cc-switch import (Claude) + Codex launchers.
- `App.tsx`: AppSwitcher iterates `APP_PAGE_ORDER` (claude_code/codex/copilot/pi).
- `setProviderFailoverQueue` wrapper + CSS `.drag-handle`/`.tag-paused`/`.drag-over` + i18n keys + mock backend with all 4 apps.

**Verified:** tsc clean; vite build; Node 18/18; Rust 105/105; headless Edge confirms 4 tabs.
### Phase 16 - Two-page refactor: Claude-as-upstream + Codex mirror install (2026-08-19) ✅ DONE

Full-scope refactor per user direction ("主体只需要留agent工具+下面对应的provider").

**Backend:**
- `POST /v1/responses` (OpenAI Responses API) added to the proxy (`proxy/responses.rs` types + `converter.rs` conversions + `server.rs` route). Both streaming (named SSE events: response.created / output_item.added / output_text.delta / output_item.done / response.completed + `data: [DONE]`) and non-streaming. Codex CLI/Desktop speak this natively. +5 tests.
- `provider_switch` live-write split: `codex` re-writes live config via `codex::write_live` into the **mirror** CODEX_HOME; `claude_code` never writes (cc-switch owns `~/.claude/settings.json` - AAG is its upstream).
- `config::get_codex_mirror_dir` - unset `codex_cli_home` now defaults to `~/.codex-cli` (NOT the native `~/.codex`), enforcing the mirror-install contract across provider_switch / codex_open_cli_terminal / codex_open_desktop_window / codex_cli_status.
- New commands: `codex_open_desktop_window` (launches a NEW Codex Desktop process with `CODEX_HOME=<mirror>` injected; existing desktop instance unaffected), `route_reorder_hops` (drag-reorder persistence for the failover chain editor).
- ~35 dead commands unregistered from `invoke_handler` (deeplink_*, backup_*, mcp_*, skill_*, session_*, usage_*, sync_*, vendor_*, detect/set/get cc-switch coexistence, sync_with_ccswitch, provider_sync_locked). Rust fn bodies + DAOs retained (library-level utilities).

**Frontend (6,751 -> ~1,300 LOC):**
- 12-page nav replaced by an AppSwitcher segmented control with 2 pages: **Claude** (endpoint copy-card for cc-switch + two-pane failover editor: upstream pool <-> routes with drag-reorderable hops + one-click "Import from cc-switch") and **Codex** (gateway status card + provider list with Switch writing the mirror + launchers: Codex Desktop (mirror) / Codex CLI terminal / Copilot CLI terminal).
- Deleted: Providers/Proxy/Vendors/CodexCli/Backup/MCP/Skills/Sessions/Usage/Sync/DeepLink/Settings pages, `providerForm.ts`, `jsonHighlight.ts`, `models.ts`, 9 dead `lib/api/*` files.
- `styles.css` 1,911 -> ~700 LOC, cc-switch design language: 64px sticky header, systemBlue (#0A84FF dark / #2563EB light), emerald-500 running pill, rounded-xl cards, segmented control.
- i18n catalogs rewritten (en/zh) around `claude.*` / `codex.*` namespaces.
- Mock backend rewritten to the new command surface (mocks `codex_open_desktop_window` + `route_reorder_hops`; cc-switch coexistence hooks removed).

**Verified:** cargo check clean; cargo test --lib 102/102 (was 97; +5 Responses tests); ui tsc clean; vite build clean (49 modules, CSS 37KB -> 10KB); Node legacy tree typecheck + 18/18 tests pass; vite dev serves.
> 6. If verification fails, set `status: blocked` and describe the blocker - do NOT advance.

## Status: ready
## Current phase: 21
## Next action: Phase 21 COMPLETE. Configure Claude Code to point at AAG (`ANTHROPIC_BASE_URL=http://127.0.0.1:8989`, `x-api-key` passthrough) and start a real session with tool calls (Read / Bash / Edit). Verify: Tauri dev log shows `router: OK claude-opus-4-8 <- <provider>/<model>`; no more `tags` output; tool_use blocks visible in client UI.
