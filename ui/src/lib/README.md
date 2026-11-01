# `ui/src/lib/` — Library code, IPC contracts, theme, mock backend

Cross-cutting UI library code. Owns the typed IPC wrappers, the per-app
page configuration, the theme manager, the build-info constants, and the
in-browser mock backend used by the demo.

## Anchor files

- [`api/`](api/README.md) — one typed wrapper per Tauri command and the
  shared DTO types. Frontend never calls `invoke` directly.
- [`client_apps.ts`](client_apps.ts) — lightweight client labels/order for navigation.
- [`api/client_router_plan.ts`](api/client_router_plan.ts) — current-configuration
  model DTO; `client_router_models.ts` projects only configured selections.
- [`appPages.ts`](appPages.ts) — supplier-owned `AppPageConfig` per managed client. Drives the
  fields the modal renders, the failover chain, the gateway card, the
  cc-switch upstream card, and the launchers. Hidden apps (e.g. `pi`) get
  `enabled: false`; `APP_PAGE_ORDER` filters them out of the AppSwitcher.
- [`build_info.ts`](build_info.ts) — `app_version`, `build_channel`,
  `is_demo_build`. Populated from Vite `define` constants emitted by
  `vite.config.ts` (which reads the desktop version from
  `src-tauri/tauri.conf.json`).
- [`ctx.ts`](ctx.ts) — global `I18nCtx` provider. Bundles locale, theme
  override, and the resolved theme into one context so pages can subscribe
  once.
- [`supplier_discovery_text.ts`](supplier_discovery_text.ts) — pure display
  mapping for discovery result/reason codes. Uses the supplied translator,
  preserves backend records, and never echoes unknown discovery details.
- [`mock/`](mock/README.md) — synthetic IPC backend installed by the
  browser demo. Demo-only.
- [`theme.ts`](theme.ts) — `useTheme()` hook. Reads / writes
  `aag.theme` in `localStorage`, follows the OS preference when the
  override is `auto`, applies the resolved theme to `<html>` via
  `data-theme`. `index.html` resolves the same saved override before first
  paint so the palette never flashes.
- [`status_surface.ts`](status_surface.ts) — pure color helpers
  (`parse_color`, `blend_rgb`, `contrast_ratio`, `resolve_status_surface`)
  behind the header's bounded page-color echo.
- [`workspace_routes.ts`](workspace_routes.ts) — the allowed workspace views and
  `parse_workspace_route`, which maps `#/<view>` to a destination or `null`.

## Key contracts

- IPC contracts live in `api/`. Components must call the typed wrappers,
  not `invoke` directly.
- `mock/backend.ts` only activates when `window.__TAURI_INTERNALS__` is
  absent (i.e. plain browser). In a Tauri webview the real IPC wins.
- Production builds in a plain browser intentionally skip the mock
  handler — `npm run ui:build` is not a supported browser target.
- `build_channel` is one of `'demo' | 'development' | 'release'`.
  `is_demo_build` is true for the `demo` Vite mode and the dedicated
  `build:demo` output.

## See also

- [`api/`](api/README.md) — typed IPC wrappers.
- [`mock/`](mock/README.md) — browser-only IPC backend.
- [`../components/app/AppPage.tsx`](../components/app/README.md) — the
  primary consumer.
- [Root README](../../../README.md).
