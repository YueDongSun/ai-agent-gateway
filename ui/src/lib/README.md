# `ui/src/lib/` — Library code, IPC contracts, theme, mock backend

Cross-cutting UI library code. Owns the typed IPC wrappers, the per-app
page configuration, the theme manager, the build-info constants, and the
in-browser mock backend used by the demo.

## Anchor files

- [`api/`](api/README.md) — one typed wrapper per Tauri command and the
  shared DTO types. Frontend never calls `invoke` directly.
- [`appPages.ts`](appPages.ts) — `AppPageConfig` per app. Drives the
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
- [`mock/`](mock/README.md) — synthetic IPC backend installed by the
  browser demo. Demo-only.
- [`theme.ts`](theme.ts) — `useTheme()` hook. Reads / writes
  `aag.theme` in `localStorage`, follows the OS preference when the
  override is `auto`, applies the resolved theme to `<html>` via
  `data-theme`.

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