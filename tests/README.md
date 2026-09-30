# `tests/` — Vitest suites for the Node surface

Vitest coverage for the Node compatibility surface. Runs in-process with
the Hono gateway and mock providers. `vitest.config.ts` excludes the
unrelated reference-checkout tests under `cc-switch/` so `npm test`
stays scoped to AAG.

## Anchor files

- [`adapters.test.ts`](adapters.test.ts) — agent config adapters and
  the `Switcher`. Covers gateway / direct / profile rewrites, backup
  capture, and the round trip from JSON config → agent config file →
  detect → read.
- [`e2e.test.ts`](e2e.test.ts) — end-to-end coverage for the Node
  gateway. Boots the Hono app, sends real HTTP requests through `fetch`
  against a bound ephemeral port, and exercises `/health`,
  `/v1/models`, `/v1/chat/completions` (non-streaming + SSE), and the
  Anthropic edge conversion.
- [`release_preflight.test.ts`](release_preflight.test.ts) — coverage
  for the icon / version triple checks in
  [`../scripts/release_preflight.mjs`](../scripts/README.md). Verifies
  that the preflight rejects malformed PNG / ICO / ICNS bytes and
  accepts the real Tauri bundle assets.

## Key contracts

- Vitest config (`vitest.config.ts`) includes only `tests/` and `src/`.
  Reference-checkout tests under `cc-switch/` are excluded on purpose.
- Tests mutate process-wide environment overrides (`AAG_CONFIG_DIR`,
  `AAG_HOME_DIR`). Run with disposable values; do not commit the
  resulting SQLite files.
- Browser-smoke-style evidence is NOT produced here. Real WebView2,
  real upstream, live CLI switching, signing, installation, and upgrade
  tests live elsewhere (or remain acceptance work).

## See also

- [`../src/`](../src/README.md) — code under test.
- [`../scripts/ui_smoke/`](../scripts/ui_smoke/README.md) — owned
  browser smoke (synthetic IPC, not the same as these unit tests).
- [`../docs/development.md`](../docs/development.md) — verification
  gate.
- [Root README](../README.md).