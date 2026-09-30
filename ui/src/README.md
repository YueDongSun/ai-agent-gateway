# `ui/src/` — React application source

The application shell, global context, lazy page boundary, and entry
point. Per-app pages, hooks, components, and library code live in their
own subdirectories.

## Anchor files

- [`App.tsx`](App.tsx) — application shell. Owns the app switcher,
  global status (`use_gateway_status` polling), theme, locale, the
  release / development / demo badge, and the lazy page boundary.
- [`main.tsx`](main.tsx) — Vite entry point. Mounts `<App />` and
  installs the in-browser mock IPC backend through
  `initDemo()` when no Tauri bridge is present.
- [`styles.css`](styles.css) — single global stylesheet. Defines the
  design tokens, theme variables for light / dark, layout primitives,
  and component-specific rules.
- [`lib/shell_i18n.ts`](lib/shell_i18n.ts) — workspace navigation labels
  and the native versus synthetic-preview context copy.
- [`components/`](components/README.md) — shared icons, reusable widgets,
  and the per-app page subtree.
- [`hooks/`](hooks/README.md) — `use_gateway_status` (and any future
  custom hooks).
- [`i18n/`](i18n/README.md) — locale store plus English and Chinese
  message catalogs.
- [`lib/`](lib/README.md) — typed IPC wrappers, per-app page configs,
  theme manager, build-info constants, and the browser-only mock IPC
  backend.
- [`lib/frontend_diagnostics.ts`](lib/frontend_diagnostics.ts) — global window
  error and rejection forwarding to the sanitized native diagnostics command.

## Key contracts

- All React components are strict ESM with explicit `.js` extensions in
  relative imports.
- Page modules are lazy-loaded from `App.tsx` so the initial bundle
  contains the shell, not every page.
- The desktop shell groups coding agents separately from MCP tools and
  the managed DSH runtime. At widths of 900px or less the sidebar becomes
  horizontal navigation; the active destination remains visible after
  navigation or resizing. The shell does not change runtime settings.
- New workspaces start in English. Existing explicit locale choices and
  theme overrides remain respected. Both light and dark themes share warm
  neutral surfaces, terracotta actions, and serif page headings.
- Navigation exposes the current page, a keyboard skip link, and visible
  focus indicators. The browser preview is labeled as synthetic evidence.
- React callbacks must always see current state. Stale closures are not
  a valid performance optimization; memo dependencies cannot be omitted
  to "reduce renders".
- Stale state lifetime (route actions, provider-card actions) is the
  responsibility of the owning context (`providers_state.tsx`,
  `routes_state.tsx`).

## See also

- [`components/`](components/README.md) — per-app page subtree.
- [`hooks/`](hooks/README.md) — custom hooks.
- [`lib/`](lib/README.md) — IPC, page configs, theme, and mock backend.
- [Root README](../../README.md).
