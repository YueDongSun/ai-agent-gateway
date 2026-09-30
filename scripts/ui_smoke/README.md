# `scripts/ui_smoke/` — Owned browser smoke harness

Browser-based regression coverage for the desktop UI. Drives the React
demo through Chrome DevTools Protocol using an owned headless browser
profile and an owned Vite server. The harness fails fast on missing
resources and cleans up afterwards; it never reuses a pre-existing
server or browser profile.

## Anchor files

- [`smoke_runtime.mjs`](smoke_runtime.mjs) — owned browser bootstrap.
  Locates a headless browser (Microsoft Edge or Google Chrome),
  resolves the Vite CLI entry, allocates a temp profile directory under
  the system `tmpdir()` with the `aag_ui_smoke_` prefix, and exposes
  lifecycle helpers (`find_browser`, `create_smoke_runtime`, `stop_owned_browser`).
- [`cdp_endpoint.mjs`](cdp_endpoint.mjs) — bounded CDP HTTP reads shared by the
  browser and native-window runners. The deadline includes response-body reads.
- [`cdp_client.mjs`](cdp_client.mjs) — minimal WebSocket CDP transport.
  Used by every page check below to drive the browser without external
  test-runner dependencies.
- [`page_checks.mjs`](page_checks.mjs) — `check_in_page(cdp, check, name,
  fn)` helper. Executes a synchronous page function, surfaces async
  errors, and reports structured pass / fail with a value detail string.
- [`demo_api_adapter.mjs`](demo_api_adapter.mjs) — installs a
  `window.__aag_smoke_api` helper that invokes the mock IPC commands
  used by the smoke scenarios. Refuses to install unless the page
  confirms it is running in demo mode (`window.__aagDemoIsDemo === true`).
- [`product_checks.mjs`](product_checks.mjs) — product regression
  coverage: initial stopped state, start / stop feedback, degraded and
  unknown states, delayed-health cancellation, masked / revealed /
  copied keys, route table column alignment, theme contrast, and layout
  containment at 1200 / 900 / 420 px viewport widths.
- [`phase_23_regressions.mjs`](phase_23_regressions.mjs) — targeted
  scenarios for the Phase 23 state-lifetime repairs (route save with
  the gateway stopped, pin / unpin, batch deletion, drag reorder).

## Key contracts

- The smoke harness owns its own Vite server (`dev_vite_port = 5173`,
  `built_demo_vite_port = 5183`). It fails on a port conflict rather
  than reusing a pre-existing server.
- Browser profiles are rooted under the system `tmpdir()` with the
  `aag_ui_smoke_` prefix. Any profile path outside that root is rejected
  before `stop_owned_browser` terminates a child or removes its profile. Windows
  cleanup stops the owned process tree and uses bounded profile-removal retries.
- Smoke evidence is synthetic IPC. It does not prove native WebView2
  behavior, real upstream availability, live CLI switching, or signed
  installation / upgrade.
- Async errors thrown inside a page function are surfaced verbatim and
  cannot turn a successful-looking value into a passing check.

Run `npm run test:ui_smoke_runner` for endpoint-timeout and cleanup regressions.
These use disposable loopback servers and owned child processes; they do not
replace actual browser or native WebView2 acceptance.

## See also

- [`../README.md`](../README.md) — sibling tooling (`tauri-dev`,
  `release_preflight`, `generate_icons`).
- [`../../ui/src/lib/mock/backend.ts`](../../ui/src/lib/mock/README.md)
  — the synthetic IPC backend the harness drives.
- [`../../docs/desktop_release.md`](../../docs/desktop_release.md) —
  release verification boundary.
- [Root README](../../README.md).
