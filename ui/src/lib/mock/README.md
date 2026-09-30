# `ui/src/lib/mock/` — Browser-only synthetic IPC backend

`backend.ts` installs a `@tauri-apps/api/mocks::mockIPC` handler that
serves realistic seed data for every command the UI invokes. Lets you
exercise the entire React UI — Claude (endpoint + failover chain) and
Codex (gateway + providers + launchers) — without launching the Tauri
runtime.

## Anchor files

- [`backend.ts`](backend.ts) — `initDemo()` registers the mock handler
  through `mockIPC`. Activation rules:
  - Tauri webview → no-op (real IPC wins).
  - `vite dev` → demo mode installs the mock handler.
  - Production build in a plain browser → no-op (not a supported target).

  Exports `isTauri` and `isDemo` for downstream code that needs to
  branch on the runtime. Also exposes `window.__aagDemoSetError(cmd)`
  and `window.__aagDemoSetError(null)` debug hooks for forcing a command
  to reject in the browser console.

## Key contracts

- The mock backend must **refuse to run** outside a demo context
  (`window.__aagDemoIsDemo === true`). The smoke API adapter in
  [`../../../scripts/ui_smoke/demo_api_adapter.mjs`](../../../../scripts/ui_smoke/README.md)
  re-asserts this guard before exposing its helpers.
- Every Tauri command the UI can invoke must have a corresponding mock
  implementation here; otherwise the UI receives a `mockIPC` rejection.
- Seed data lives at module scope (survives HMR for the lifetime of the
  page). It is intentionally NOT persisted across reloads.

## See also

- [`../api/`](../api/README.md) — typed wrappers the mock satisfies.
- [`../../../scripts/ui_smoke/`](../../../../scripts/ui_smoke/README.md)
  — owned browser smoke that drives the mock backend through CDP.
- [Root README](../../../../README.md).
