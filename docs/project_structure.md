# Project Structure

Use this page to find the owner of a change. Read
[`architecture.md`](architecture.md) for contracts and
[`development.md`](development.md) for commands.

## Repository tree

```text
src/                         Node gateway, CLI/TUI, config, and adapters
src-tauri/src/
  commands/                  Tauri IPC orchestration
  database/                  SQLite schema, migrations, seed, and DAOs
  proxy/                     Axum server, routing, providers, breakers
  services/                  adapters, detection/import, and sync
    mcp/                     validated registry, redacted review, safe client writes
    dsh/                     isolated runtime, setup, and plugin transactions
  lib.rs                     Tauri setup and command registration
ui/src/
  components/app/            shared app page, pool, routes, gateway, launchers
  components/mcp/            registry editor and reviewed client sync
  hooks/                     gateway status and cancellation
  lib/api/                   typed IPC wrappers and DTOs
  lib/mock/                  browser-demo IPC backend
  App.tsx                    desktop shell and lazy page boundary
scripts/
  ui_smoke/                  owned browser runtime and regression checks
  verification/              feature-aware plans and foreground verification
  mcp/                       independent read-only stdio companion
  tauri_dev_config.mjs       development URL/config contract
  tauri-dev.mjs              one-server desktop development runner
tests/                       Node gateway and adapter tests
docs/                        current and historical documentation
```

## Ownership rules

- Keep Node gateway logic in `src/gateway/`; do not make it import Tauri state.
- Put Rust SQL only in `src-tauri/src/database/dao/`.
- Put Tauri orchestration only in `src-tauri/src/commands/`.
- Put agent file formats and live writes in one adapter under
  `src-tauri/src/services/provider/`.
- Keep UI state in the owning page, context, or hook.
- Keep mock IPC and browser smoke separate from native integration.

## UI state boundaries

`AppPage.tsx` coordinates the selected app page. `proxy_pool_state.tsx` owns
upstream pool data/actions. `routes_state.tsx` owns route data/actions.
`GatewayPanel.tsx`, `UpstreamPool.tsx`, `UpstreamList.tsx`, and `RouteEditor.tsx`
consume those boundaries. Callbacks must see current state; omitted dependencies
are not a valid optimization.

## Build boundaries

- `ui/dist`: native Tauri frontend output.
- `ui/dist_demo`: browser-demo output.
- `desktop:dev`: starts one owned Vite server and passes its actual URL to
  Tauri.
- `desktop:build`: builds the native frontend before native packaging.
- `release:verify`: checks versions, generated assets, and image containers.
- `ui:smoke` and `ui:smoke_demo`: use owned browser profiles and servers.

The root `npm run dev`, `npm run build`, and `npm start` remain Node CLI
commands. They do not launch the desktop frontend.

## Test boundaries

- Node tests use the in-process Hono app and mock providers.
- Rust tests use inline modules and the full-stack `e2e` test.
- Rust tests that mutate process-wide environment variables run serially.
- Browser smoke uses synthetic IPC and verifies persistence through the demo
  adapter; it does not prove native WebView2 or real upstream behavior.
- Release checks do not prove installation, signing, upgrades, or publication.

## Documentation ownership

- `README.md`: user-facing overview and quick start.
- `docs/architecture.md`: current module and contract reference.
- `docs/development.md`: setup and verification.
- `DESIGN.md`: durable design and migration summary.
- `PROGRESS.md`: current status and fresh evidence.
- `docs/desktop_release.md`: native build and distribution boundary.
