# Project structure

The desktop application and the headless gateway remain independently runnable.
This document maps current responsibilities; `DESIGN.md` also contains historical
migration plans and must not be treated as an exact file inventory.

## Runtime ownership

- `src/`: the Node gateway, CLI, adapters, and JSON configuration. Keep this tree
  for headless users; it does not implement the desktop state store.
- `src-tauri/src/`: desktop command orchestration, SQLite DAOs, provider adapters,
  and the Rust proxy. SQL belongs in `database/dao/`; live configuration writes
  belong behind the existing adapter and atomic-write boundaries.
- `ui/src/`: the React desktop interface and browser demo. `lib/api/` owns IPC
  contracts; `lib/mock/` supplies synthetic browser data, not native integration
  evidence.
- `tests/`: Node gateway and adapter regression tests. `vitest.config.ts` excludes
  unrelated reference-checkout tests.
- `cc-switch/`: an ignored external reference checkout, not an AAG source tree.

## UI state boundary

The app components keep their existing public names and IPC field names.

- `ui/src/components/app/AppPage.tsx` owns page state and passes current action
  callbacks to its children.
- `ui/src/components/app/proxy_pool_state.tsx` owns pool data/action contracts,
  context providers, and hooks.
- `ui/src/components/app/routes_state.tsx` owns route data/action contracts,
  context providers, and hooks.
- `UpstreamPool.tsx`, `UpstreamList.tsx`, and `RouteEditor.tsx` consume their domain
  modules directly. The mixed `state.tsx` compatibility layer is removed.
- `ui/src/hooks/use_gateway_status.ts` owns header status polling, request
  cancellation, and reconciliation after explicit page refresh callbacks.
- `ui/src/components/secret_value.tsx` owns temporary key visibility. It does not
  own configuration persistence or clipboard transport.
- `ui/src/components/release_badge.tsx` renders the version and build channel.
  `ui/src/lib/build_info.ts` exposes build metadata; `ui/vite.config.ts` reads
  the desktop version from `src-tauri/tauri.conf.json`.

Separate contexts do not imply field-level subscriptions. A component consuming
an entire context can rerender when any field in that context changes. Action
callbacks must see current state; omitted memo dependencies are not a valid
performance optimization.

## Build and release boundaries

- `npm run ui:dev` and `npm run ui:demo` serve the current React source. A
  browser without the native bridge uses synthetic IPC. The explicit demo
  command also opens the local preview in the default browser.
- `npm run ui:preview` rebuilds and serves `ui/dist_demo`; it cannot replace
  the native `ui/dist` bundle. Both outputs are ignored by Git.
- `npm run desktop:dev` owns one Vite server and passes its actual bound URL
  to the installed Tauri CLI through `scripts/tauri_dev_config.mjs`.
- `npm run desktop:build` rebuilds `ui/dist` before compiling the desktop app.
  `tauri:dev` and `tauri:build` remain compatible entry points.
- `ui/public/app_icon.svg` is the shared brand source for the favicon, header,
  and generated application icons. `tray_mark.svg` supplies the macOS template
  mark. `scripts/generate_icons.mjs` generates and validates the native assets.
- `scripts/release_preflight.mjs` verifies version, native output, and image
  container contracts. `docs/desktop_release.md` owns distribution guidance.
- `.github/workflows/desktop_build.yml` builds native platform candidates on
  demand; it neither signs them nor creates releases.

The legacy root `npm run dev`, `npm run build`, and `npm start` still target the
Node CLI. Desktop versioning does not change the independent CLI version.

## Test tooling

`npm run ui:smoke` remains the browser test entry point. Its implementation lives
in `scripts/smoke-demo.mjs`, with reusable runtime and regression responsibilities
under `scripts/ui_smoke/`. New helper names use underscores; the existing npm
command and entry filename remain compatible.

- `smoke_runtime.mjs`: Vite/browser ownership and validated profile cleanup.
- `cdp_client.mjs`: DevTools transport, deadlines, and protocol errors.
- `page_checks.mjs`: page-result and asynchronous-error assertions.
- `demo_api_adapter.mjs`: guarded access to the synthetic IPC backend in both
  source-served and built-demo smoke runs.
- `phase_23_regressions.mjs`: state-lifetime and persistence regressions.
- `product_checks.mjs`: gateway feedback, credential presentation, table
  alignment, theme readability, and responsive layout regressions.

`npm run ui:smoke_demo` rebuilds the demonstration output and runs the same
suite against it. The runtime uses a separate owned port for each mode.

Tests must own their browser profile and development server. A response from an
unrelated server on a familiar port is not proof that the checkout was tested.
Browser mock checks do not authorize real provider calls or live configuration
changes.

## Progress documents

- `PROGRESS.md`: current phase, closure boundary, next action, fresh verification,
  and the most recent log entry. Read this first.
- `docs/reviews/phase_23_review.md`: reviewed commits, defects, repair decisions,
  and remaining follow-ups.
- `docs/progress/phase_22_23_history.md`: the previous long progress log, retained
  as historical evidence with its original completion claims.
- `docs/PHASE-ARCHIVE.md`: the existing archive through Phase 22b.

Archive detailed history instead of appending every past phase to the current
status file. Historical test counts and completion labels are not fresh proof.

## Deferred structural work

Do not rename or move the three runtime roots, extract a shared cross-language
library, or move private database state as part of this cleanup. If `AppPage.tsx`
needs further reduction, extract one state-owning domain hook at a time and keep
its persistence regression tests with that boundary.

The root `check_codex.py`, `check_db.py`, and `check-db.cjs` are unreferenced,
machine-specific diagnostics. They were not executed or moved. A separate cleanup
should remove them or replace them with an explicit-path, read-only, redacted
diagnostic command; merely relocating them would preserve unsafe output behavior.
