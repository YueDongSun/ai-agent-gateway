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
  services/
    provider/                per-app live-config adapters (mirror isolation)
    detect/                  agent detection
    import_cc_switch/        cc-switch import
    launcher/                desktop, terminal, and VS Code launchers
    model_discovery/         configured-supplier preflight, upstream catalog and per-source same-origin URL discovery
    mcp/                     validated registry, redacted review, safe client writes
    dsh/                     isolated runtime, setup, and plugin transactions
    prompt/                  two-stage Chinese-brief to English-prompt pipeline
    proxy_runtime/           proxy bind, snapshot, and queue lifecycle
    sync/                    S3 and WebDAV backups
    workflow_registry/       workflow catalog, probes, and routing
  lib.rs                     Tauri setup and command registration
ui/src/
  components/app/            model-only current-configuration client pages
  components/suppliers/      supplier inventory and EN/CN discovery help, health and results
  components/suppliers/client_management/  provider, route, gateway and launcher controls
  components/dsh/            managed DSH runtime UI
  components/dsh_market/     DSH marketplace UI
  components/mcp/            registry editor and reviewed client sync
  components/prompt/         prompt-enhancement page and its stylesheet
  components/workflow/       workflow registry panel
  hooks/                     workspace routes, header surface, gateway status, discovery
  lib/api/                   typed IPC wrappers and DTOs
  lib/supplier_discovery_text.ts  localized stable discovery-code explanations
  lib/mock/                  browser-demo IPC backend
  i18n/                      locale store and the en/zh catalogs
  App.tsx                    desktop shell, grouped navigation, and lazy page boundary
scripts/
  ui_smoke/                  owned browser runtime and regression checks
  verification/              feature-aware plans and foreground verification
  mcp/                       independent read-only stdio companion
  dsh_market/                marketplace catalog and native checks
  privacy/                   repository audit and public snapshot export
  router_performance/        routing benchmark harness (separate Cargo crate)
  upstream_router/           standalone upstream-router experiments (Python)
  windows/                   host tray and startup checks
  workflow/                  workflow page tools (client + server)
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

`AppPage.tsx` coordinates the selected client view and renders model-only
projections from `client_router_models.ts`. Retained provider, route, gateway,
configuration and launcher controls are centralized in `supplier_client_settings.tsx`
under `ui/src/components/suppliers/client_management/` and mounted inside
Suppliers. `providers_state.tsx` owns the unified card data/actions; `routes_state.tsx`
owns route data/actions plus the narrow pointer-drag context. The prompt page
owns its own two-stage run loop and keeps history in the browser, not in SQLite.
`GatewayPanel.tsx`, `ProvidersPanel`, `ProviderCard`, and `RouteEditor.tsx`
consume those boundaries. Callbacks must see current state; omitted dependencies
are not a valid optimization.

The selected destination is shell state: `use_workspace_navigation.ts` maps it
to a hash route through `workspace_routes.ts`, restores it on reload and
Back/Forward, and resets scroll and focus on every change. The page container is
keyed by destination, so page-local provider, route, and modal state cannot leak
into another app. `workspace_header.tsx` renders the destination selector and
global status controls; it does not own page data.

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
