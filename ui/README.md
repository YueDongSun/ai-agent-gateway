# `ui/` — React desktop frontend and browser demo

The React 18 + Vite frontend. In a Tauri webview, calls go through
`window.__TAURI_INTERNALS__.invoke` to the Rust backend. In a plain
browser, the in-browser mock backend in
[`src/lib/mock/backend.ts`](src/lib/mock/README.md) installs an
`@tauri-apps/api/mocks::mockIPC` handler so the entire UI can be
exercised without launching the Tauri runtime.

## Anchor files

- [`package.json`](package.json) — declares the Vite dev server, the
  Tauri-aware build, and the demo build (`build:demo`) that writes
  `dist_demo` instead of `dist`. Version `0.2.0` is shared with the
  Tauri config.
- [`vite.config.ts`](vite.config.ts) — Vite configuration. Reads the
  desktop version from `src-tauri/tauri.conf.json` via
  `vite_build_config.mjs` so the build channel and version render in
  the badge.
- [`vite_build_config.mjs`](vite_build_config.mjs) — small helper used
  by `vite.config.ts` to surface desktop version metadata to the bundle.
- [`tsconfig.json`](tsconfig.json) — TypeScript configuration. Strict ESM
  with explicit `.js` extensions in relative imports, matching the
  project-wide convention.
- [`index.html`](index.html) — Vite entry HTML.
- `dist/` — native Tauri build output (`frontendDist`). Built
  by `npm run ui:build`. Ignored by Git.
- `dist_demo/` — browser-demo build output. Built by
  `npm run ui:build_demo`. Ignored by Git.
- [`public/`](public) — static assets shared between the desktop and
  demo bundles (`app_icon.svg`, etc.).
- [`src/`](src/README.md) — application source tree.

## Key contracts

- The npm scripts come in two flavors: source-served (used by `ui:dev`,
  `ui:demo`) and built (`ui:build`, `ui:build_demo`). The demo output
  cannot replace the native `ui/dist` bundle.
- `ui/dist` must exist before `cargo check` / `cargo build` in
  `src-tauri/`, because `tauri.conf.json` reads it as `frontendDist`.
- The same React code runs in both Tauri and the browser. Differences
  are isolated behind the `isDemo` / `isTauri` checks in
  [`src/lib/mock/backend.ts`](src/lib/mock/README.md).

## See also

- [`src/`](src/README.md) — application source tree.
- [`../src-tauri/`](../src-tauri/README.md) — Rust backend.
- [`../scripts/`](../scripts/README.md) — desktop dev runner and browser
  smoke harness.
- [Root README](../README.md).
