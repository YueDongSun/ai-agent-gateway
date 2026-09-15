# PROGRESS.md - Tauri migration state

> **This file is the loop's memory.** Each cron iteration must:
> 1. Read this file to find `current_phase`.
> 2. Read `DESIGN.md` for that phase's spec.
> 3. Implement a bounded slice (one sub-task per iteration max).
> 4. Run verification (see `## Verification commands`).
> 5. Update `current_phase`, append to `## Log`, set `next_action`.

### Phase 22n - PROGRESS.md archive + upstream model count (2026-08-25) ✅ DONE

Housekeeping + one small feature:

1. **PROGRESS.md slimmed 570 → ~290 lines**: phase logs up to 22c
   moved verbatim to `docs/PHASE-ARCHIVE.md` (with a header explaining
   the rotation). The loop's memory stays scannable; history is not
   lost.
2. **UpstreamList model count**: rows with a non-empty `models` list
   show a "N models / N 个模型" tag next to the URL (hover = full list
   via title). Users can see which upstreams feed hop-model suggestions
   without opening the edit modal.

i18n: `claude.upstream.modelCount` (en+zh).

**Verified:** ui tsc clean · vite build clean (AppPage 36.12 kB) ·
`npm run ui:smoke` 13/13 green.

### Phase 22m - Gateway settings modal (port + auth token) (2026-08-25) ✅ DONE

The gateway card was read-only — changing the proxy port or API key had
no UI surface. New ⚙ button in the card head opens a settings modal:

- Pre-filled from `getProxyConfig` (port + authToken; enabled bit rides
  along untouched on save).
- Port validated (integer 1–65535, `app.gateway.invalidPort` on bad
  input); save via `saveProxyConfig`, then a toast reminds that a
  gateway restart applies the new settings.
- GatewayPanel takes an optional `onEditSettings` prop (card stays
  self-contained; AppPage owns the modal state).
- i18n: `app.gateway.editSettings` / `portLabel` / `invalidPort` /
  `savedRestartHint` (en+zh).

Also: committed the Phase 22d–22l batch (`0aaf828`) before starting
this slice.

**Verified:** ui tsc clean · vite build clean (AppPage 35.92 kB) ·
`npm run ui:smoke` extended to 13 checks — all green, including a new
scripted check that opens the modal, changes the port via the native
React input setter, saves, and asserts the URL row shows :8990.

### Phase 22l - Smoke covers rename + enable toggle + keyboard reorder (2026-08-25) ✅ DONE

Extended `scripts/smoke-demo.mjs` from 9 to 12 checks, adding scripted
CDP coverage for the three newest interactions:

1. **Route rename**: overrides `window.prompt` to auto-answer, clicks
   Rename on the first route card, asserts the `.route-card-title`
   alias changed (Phase 22k end-to-end).
2. **Upstream enable toggle**: clicks the first UpstreamList switch,
   asserts the checkbox flipped and the row got the disabled styling
   path (Phase 22h).
3. **Hop keyboard reorder**: focuses a drag handle, dispatches
   ArrowDown, asserts the hop at idx 0 landed at idx 1 (Phase 22e).

Fix in the CDP helper: `Runtime.evaluate` now passes `awaitPromise:
true` — async IIFE checks were resolving to an object placeholder
(`[object Object]`) instead of their value, failing all three checks
on the first run.

**Verified:** `npm run ui:smoke` → 12/12 green, zero exceptions.

### Phase 22k - Route alias rename (2026-08-25) ✅ DONE

Renaming a route previously meant delete + recreate + rebuild every hop
by hand. Alias is the `routes` PRIMARY KEY, so rename is a single
in-place UPDATE — hops/description ride along on the same row.

**Backend:**
- `Database::rename_route(from, to)` (`dao/routes.rs`): unchecked
  transaction, `UPDATE routes SET alias=?1 WHERE alias=?2`, errors with
  `InvalidInput` when the source is missing; a taken target fails via
  the PK UNIQUE constraint.
- `route_rename` command (trims/empty-checks the target, no-op when
  unchanged), registered in `invoke_handler`.

**Frontend:**
- `renameRoute()` API wrapper; mock backend `route_rename` case.
- AppPage `handleRenameRoute`: prompt → duplicate check → backend
  rename → patch BOTH local routes and `baselineRef` (dirty flag stays
  truthful for unsaved hop edits, which survive the rename).
- RouteEditor route-card head gains a Rename button.
- i18n: `route.rename`, `route.rename.prompt`, `route.renamed` (en+zh).

**Verified:** cargo test --lib 122/122 · ui tsc clean · vite build clean
· `npm run ui:smoke` 9/9 green.

### Phase 22j - One-command browser smoke (`npm run ui:smoke`) (2026-08-25) ✅ DONE

Codified the Phase 22i CDP debugging into a permanent script:
`scripts/smoke-demo.mjs` (root `package.json` → `ui:smoke`). Zero new
deps — native fetch + WebSocket (Node ≥20).

What it does, end to end (~15s):
1. Finds Edge/Chrome in standard paths.
2. Reuses a vite dev server on :1420 if one is answering, else spawns
   `npm run dev --prefix ui` (20s startup budget).
3. Launches headless `--user-data-dir` browser with CDP on :9223
   (isolated profile — never touches the user's browser).
4. Navigates to the demo page and asserts: no "UI error" crash screen,
   header + Claude page landmarks, hop drag handles rendered.
5. Clicks Start → asserts the running pill; asserts the route-toolbar
   restart button appears only while running (Phase 22e wiring).
6. Switches to the Codex tab → asserts the mirror CODEX_HOME row.
7. Fails on ANY uncaught Runtime.exceptionThrown (this is the check
   that would have caught the Phase 22i TDZ white-screen).

`--keep` leaves vite + the browser up for manual inspection. Exit 0/1.

**Verified:** ran once from repo root — 9/9 checks pass, zero
exceptions. This is now the post-refactor gate for every UI round.

### Phase 22i - Browser smoke test catches white-screen TDZ crash (2026-08-25) ✅ DONE

Ran the first real browser smoke (vite dev + headless Edge via CDP:
navigate, console/exception capture, DOM text snapshot, scripted
clicks). **It caught a production-grade bug that tsc/vite build both
miss:**

**`ReferenceError: Cannot access 'breakerOf' before initialization`**
— the per-card Context refactor moved `proxyPoolValue`'s `useMemo`
above its handler dependencies (`breakerOf` / `breakerOfClass`
`handleTestProvider` / `clearTestResult`, all `const` arrow functions
declared hundreds of lines later). The memo factory runs at first
render → TDZ → AppPage crashes on mount → React unmount tears the tree
down while the index.html error handler has already overwritten
root.innerHTML → secondary `removeChild` NotFoundError. White screen in
demo AND in the packaged Tauri app.

**Fix:** moved the `proxyPoolValue` useMemo after `clearTestResult`
(just before return), with a comment at both sites explaining the TDZ
constraint.

**Post-fix smoke (CDP, 0 exceptions):**
- Claude page fully renders: gateway card, upstream pool flow nodes,
  2 seeded routes with hop tables, upstream list.
- Start button → "● running" pill; route toolbar shows the new
  "重启代理以生效" button only while running (Phase 22e wiring works).
- Codex tab switch renders mirror CODEX_HOME row + codex routes;
  tab-switch route cache path clean.

**Verified:** ui tsc clean; vite build clean (AppPage chunk 33.79 kB).

### Phase 22h - Upstream enable/disable switch (2026-08-25) ✅ DONE

Audit finding: `ProxyProvider.enabled` is load-bearing (the Rust router
skips disabled providers — `router.rs:119/150/276/396`, and
`commands/proxy.rs:132` refuses to test them), but the UI had **no way
to set it false**, and worse, `handleSaveProxy` hardcoded
`enabled: true`, silently re-enabling a deliberately disabled provider
on any edit.

**Fix:**
- `handleSaveProxy` preserves `editingProxy.enabled` instead of forcing
  true (new rows default to enabled).
- New `handleToggleEnabled(id, enabled)` in AppPage (full-row save +
  refresh), exposed via ProxyPoolState as `onToggleEnabled`.
- UpstreamList rows gain a toggle switch (persisted immediately) + a
  DISABLED tag + `.provider-row.disabled` dimmed/strikethrough styling.
  The failover flow viz keeps showing only enabled nodes by design;
  the list is where disabled ones stay visible.

i18n: `claude.upstream.disabledTag` + `claude.upstream.enabled.help`
(en+zh). CSS: `.provider-row.disabled` (+8 lines).

**Verified:** ui tsc clean; vite build clean (CSS 20.47→20.65 kB,
AppPage chunk 33.79 kB). No backend changes.

### Phase 22g - Port-conflict UX on gateway start (2026-08-25) ✅ DONE

Audited the backend's port handling first: `bind_with_fallback`
(`commands/proxy.rs`) already sweeps ports +0..+5 with a `/health`
ownership probe and returns a joined per-port error list — no backend
change needed. The gap was presentation:

1. **Start failure**: `handleStart` dumped the raw 6-port error string
   into the error banner. Errors matching `no free port` now render a
   human summary ("every nearby port is taken — free a port or change
   the base URL") with the raw detail appended (`app.gateway.portConflict`).
2. **Silent port fallback**: when the backend moves to a fallback port,
   `startProxy`'s message carries "port N was in use; saved as the new
   default" — it was dropped. `handleStart` now regexes that out and
   flashes `app.gateway.portFallback` so users know why the URL row
   changed.

i18n: `app.gateway.portFallback` + `app.gateway.portConflict` (en+zh).

**Verified:** cargo test --lib 122/122 (baseline); ui tsc clean; vite
build clean (AppPage chunk 33.19 kB).

### Phase 22f - Live health polling + upstream model list editor (2026-08-25) ✅ DONE

Two fixes in the Claude/Codex upstream-pool surface:

1. **Live breaker states**: `health` was fetched once at mount/refresh —
   breaker open/close transitions never reached the pool pills while the
   proxy ran. AppPage now polls `GET /health` every 10s via a dedicated
   effect (gated on `running` + pool-bearing pages, cleanup on
   stop/unmount/tab switch). Mock backend already intercepts `/health`,
   so demo mode stays live too.
2. **Upstream model list editor + silent-wipe fix**: `handleSaveProxy`
   hardcoded `models: []` — editing any provider through the modal
   silently wiped its model list (which feeds the route editor's hop
   suggestions and knownModels). The proxy modal now has a Models
   textarea (comma/newline separated), pre-filled from the provider on
   edit and parsed back on save.

i18n: `claude.upstream.modal.models` (en+zh).

**Verified:** ui tsc clean; vite build clean (AppPage chunk 33.02 kB).
No backend changes.

### Phase 22e - Restart-from-route-editor + keyboard hop reorder (2026-08-25) ✅ DONE

Two UX gaps closed in the route editor:

1. **One-click apply**: `route.restartHint` was text-only — users had to
   scroll up to the gateway card and Stop/Start manually. The route
   toolbar now shows a "Restart proxy to apply" button (only while the
   gateway is running). `handleRestartProxy` in AppPage does
   stopProxy (tolerant of not-running) → startProxy → refresh →
   `app.gateway.restarted` toast; busy state `restart` disables it.
   Wired through RoutesState (`proxyRunning` / `restartBusy` /
   `onRestartProxy`) so RouteEditor stays context-only.
2. **Keyboard hop reorder**: the ⋮⋮ drag handle had `role="button"` +
   tabIndex but no key handling (fake accessibility).
   `handleHopKeyDown` maps ↑/↓ (step), Home/End (to ends) onto
   `applyHopMove`, then refocuses the moved row's handle via
   `requestAnimationFrame` + `CSS.escape`d selector so ↑/↓ repeat
   cleanly. Handle tooltips updated to mention the keys (en+zh).

i18n: `route.restartNow`, `app.gateway.restarted` (en+zh);
`route.hopDragHint` mentions keyboard support.

**Verified:** ui tsc clean; vite build clean (AppPage chunk 32.48 kB).
No backend changes (proxy_start/stop already existed; mock backend covers
them).

### Phase 22d - Truthful route-editor persistence (staged add/remove + reconcile-save) (2026-08-25) ✅ DONE

Four real defects in the route editor's persistence semantics, all fixed
in `AppPage.tsx`:

1. **Dirty flag lied**: `handleAddRoute` persisted immediately
   (`saveRoute` + full reload) but never touched `baselineRef`, so the
   toolbar showed "Unsaved changes" for already-persisted state.
2. **Silent edit loss**: add/delete/batch-delete reloaded ALL routes from
   the backend (`setRoutes(await listRoutes())`), wiping unsaved hop
   edits on unrelated routes.
3. **Duplicate alias silently overwrote** an existing route's hops
   (`saveRoute` upserts by alias).
4. **Full serial save**: Save wrote every route (including untouched
   ones) one IPC call each.

New model — **stage locally, reconcile on Save**:
- Add / delete / batch-delete now mutate local state only (`setRoutes`),
  so the dirty indicator is truthful and Discard restores cleanly.
- Duplicate-alias guard on add (`route.exists` toast, en+zh).
- `handleSaveRoutes` reconciles against the baseline: saves only new-or-
  changed routes (`JSON.stringify` diff), deletes routes removed while
  editing (`baselineRef` − current), skips untouched ones.
- Newly added routes auto-expand so hops can be added right away.

**Verified:** ui tsc clean; vite build clean (AppPage chunk 31.29→31.61 kB).
No backend changes; mock backend already covered route_save/route_delete.
cargo test --lib 122/122 (baseline unchanged).

### Phase 22c - Fix re-import hop filling + pointer-based hop drag (2026-08-22) ✅ DONE

**Backend** (`services/import_cc_switch/mod.rs`): route inputs are now
collected at the end of the loop regardless of whether the provider was newly
imported or already present — so a re-import against an existing DB (providers
that predate the route builder) fills the hop models from cc-switch's tier
mapping instead of leaving them empty. Locked rows (sync_locked) still
excluded. New test `reimport_fills_hops_for_existing_providers`.

**Frontend** (`AppPage.tsx`): hop drag reimplemented with **pointer events**
(pointerdown/move/up + `elementFromPoint`) instead of HTML5 DnD, which is
unreliable in Tauri's WebView2. `hopRowAt()` resolves the target row under
the pointer; `applyHopMove` reorders in state. Removed the dead HTML5
`handleHopDrop`/`routeDragIdx` path. `.hop-row` rows carry
`data-route-alias`/`data-hop-idx` for hit-testing.

**Mock backend** (`lib/mock/backend.ts`): added missing
`proxy_get_pinned_provider`/`proxy_set_pinned_provider` (their absence made
`refresh()`'s `Promise.all` reject and silently blank the upstream pool in
demo mode). Added missing `claude.upstream.listTitle` i18n key (en/zh).

**Verified:** cargo test --lib 118/118; ui tsc + vite build clean; pointer
drag verified in-browser (hop swap works).

> 6. If verification fails, set `status: blocked` and describe the blocker - do NOT advance.

## Status: ready
## Current phase: 22n
## Next action: Phase 22n COMPLETE. Next slice candidates: smoke check for gateway modal port-validation error path; audit UpstreamPool empty-vs-disabled flow rendering; consider proxy_config host editing (modal covers port/key only).
