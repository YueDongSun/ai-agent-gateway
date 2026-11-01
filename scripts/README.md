# `scripts/` — Desktop tooling, release preflight, and browser smoke

Cross-cutting development tools. Desktop dev / build runners, the icon
generator, the release preflight that verifies icons and the desktop
version triple, the desktop runner / build configuration helpers plus
their `node --test` unit tests, and the owned browser smoke runner.

## Anchor files

- [`tauri-dev.mjs`](tauri-dev.mjs) — `npm run desktop:dev`. Boots one
  owned Vite server (under `ui/`), resolves its actual bound URL, writes
  a `tauri.dev.json` overlay through `create_tauri_dev_config`, and
  spawns the installed Tauri CLI against the overlay.
- [`tauri_dev_config.mjs`](tauri_dev_config.mjs) — `create_tauri_dev_config`
  builder plus the `development_url`, `forwarded_arguments`, and
  `local_tauri_entry` helpers used by `tauri-dev.mjs` and its tests.
- [`tauri_dev_config.test.mjs`](tauri_dev_config.test.mjs) — `node --test`
  coverage for the dev-config helpers. Wired up to
  `npm run test:desktop_runner`.
- [`ui_build_config.test.mjs`](ui_build_config.test.mjs) — `node --test`
  coverage for the Vite build helper that reads the desktop version
  from `src-tauri/tauri.conf.json`.
- [`generate_icons.mjs`](generate_icons.mjs) — generates the RGBA PNG,
  ICO, and ICNS bundle from `ui/public/app_icon.svg`. Used by
  `npm run icons:generate` before a desktop build.
- [`release_preflight.mjs`](release_preflight.mjs) — `npm run release:verify`.
  Reads icon headers (no image library), checks the desktop version
  triple (`tauri.conf.json` / `ui/package.json` / `Cargo.toml`), and
  verifies RGBA square PNGs at 32 / 128 / 256 / 512 px and a real ICNS
  container with meaningful 32 / 128 / 256 / 1024 px layers.
- [`smoke-demo.mjs`](smoke-demo.mjs) — `npm run ui:smoke` and
  `npm run ui:smoke_demo`. Boots the owned Vite server (source or
  built demo), launches an owned headless browser profile, runs the
  page checks and regression scenarios, and cleans up afterwards.
- [`ui_smoke/`](ui_smoke/README.md) — owned browser runtime, CDP
  transport, page checks, and regression scenarios.
- [`upstream_router/`](upstream_router/README.md) — optional Python
  configuration adapter preserving the legacy Gemini/Grok LiteLLM route
  aliases and ordered fallbacks. Validates operator-owned JSON, references
  secrets through environment variables, and prepares a manual launch
  contract without starting a listener or managing login tasks.

## Key contracts

- Node scripts use `node --test` for unit coverage, NOT the Vitest
  suite. They are not subject to the `vitest.config.ts` glob.
- The optional `upstream_router` adapter uses standard-library Python
  tests documented in its README. It has no dependency on CC Switch or
  OpenClaw private storage; LiteLLM remains an operator-supplied runtime.
- The release preflight deliberately does not import an image library;
  it reads PNG / ICO / ICNS bytes directly so CI does not need
  Sharp / image-decoding dependencies.
- The desktop dev runner owns one Vite server. It fails fast on a port
  conflict — it does not reuse an unrelated server on the same port.
- The browser smoke profile is rooted under `tmpdir()` with the
  `aag_ui_smoke_` prefix. Profiles outside that prefix are rejected by
  the cleanup path.

## See also

- [`windows/`](windows/README.md) — Windows maintenance helpers (PowerShell).
- [`ui_smoke/`](ui_smoke/README.md) — owned browser smoke harness.
- [`../src-tauri/`](../src-tauri/README.md) — Tauri backend.
- [`../ui/`](../ui/README.md) — React frontend.
- [`../docs/desktop_release.md`](../docs/desktop_release.md) — release
  pipeline.
- [Root README](../README.md).
