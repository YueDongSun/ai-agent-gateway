# `ui/src/lib/api/` — Typed Tauri IPC wrappers

One file per Tauri command surface plus the shared DTO types. Frontend
code calls these wrappers, never `invoke` directly. Names mirror the
Tauri command names in
[`src-tauri/src/commands/`](../../../../src-tauri/src/commands/README.md).

## Anchor files

- [`index.ts`](index.ts) — re-exports every API module. UI code imports
  from here.
- [`app.ts`](app.ts) — `appCheckUpdate` (auto-update check) plus the
  `AppCheckUpdateOptions` shape.
- [`codexCli.ts`](codexCli.ts) — Codex CLI isolation status and the
  isolated-terminal / desktop launchers (`codexCliStatus`,
  `codexOpenCliTerminal`, `codexOpenDesktopWindow`, plus the
  `settingsGet` / `settingsSet` wrappers used by the Codex isolation
  flow).
- [`copilot.ts`](copilot.ts) — `copilotOpenCliTerminal`. Injects
  `COPILOT_PROVIDER_*` env vars into a new terminal session.
- [`import.ts`](import.ts) — `importCcSwitchProviders`. One-shot cc-switch
  SQLite import.
- [`providers.ts`](providers.ts) — provider CRUD + failover queue
  (`listProviders`, `getCurrentProvider`, `getProvider`, `saveProvider`,
  `deleteProvider`, `switchProvider`, `reorderProviders`,
  `setProviderFailoverQueue`).
- [`proxy.ts`](proxy.ts) — proxy lifecycle, config, providers, routes,
  pinned provider, test results, health, and model fetch.
- [`diagnostics.ts`](diagnostics.ts) — temporary Debug state, local log access,
  safe summary, and frontend error reporting.
- [`types.ts`](types.ts) — shared DTOs (`AppType`, `Provider`,
  `LiveWriteResult`, `ImportedProvider`, `ImportedRoute`,
  `ImportReport`). `AppType` matches the
  [`AppType`](../../../../src-tauri/src/services/provider/mod.rs) enum
  in Rust.

## Key contracts

- Every wrapper is `async` and either returns the typed payload or
  throws on a Rust `Err`.
- IPC names use `snake_case` in Rust and `camelCase` in TypeScript. The
  wrappers and `types.ts` handle the rename at the boundary.
- New Tauri commands must ship with a wrapper here in the same change.
- DTOs must mirror each Rust serialization contract. Diagnostics deliberately
  uses snake-case fields; existing command DTOs keep their documented camelCase
  adapters. A drift here causes silent UI breakage.

## See also

- [`../../../src-tauri/src/commands/`](../../../../src-tauri/src/commands/README.md)
  — Rust IPC implementation.
- [`../`](../README.md) — sibling modules (`appPages`, `theme`,
  `mock`, `ctx`, `build_info`).
- [Root README](../../../../README.md).
