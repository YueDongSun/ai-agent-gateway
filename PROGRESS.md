# AAG progress and functional closure

## Status: preview baseline verified
## Current phase: 23j - Desktop release baseline
## Next action: Complete native installation, tray, and upgrade acceptance on each supported OS before promoting the preview to a signed stable release.

## Closure scope

The UI stale-action repair, gateway feedback, credential masking, table layout,
npm/desktop build synchronization, and release asset contracts are verified.
This slice prepares the v0.2.0 preview. Browser observations and automated gates
do not close native installation, signing, or rendering-performance work.

The former 23b held-port fix was superseded by 23i. Old action-memo isolation
claims and historical test counts are superseded by this review. Live agent
configuration, model routing, and recurring jobs were not changed. Publication
must be checked against the actual remote tag and release assets, independently
of the local test results recorded here.

## Feature checklist

- [x] Current route actions preserve loaded routes and persist subsequent edits.
- [x] Loaded upstream providers can be enabled/disabled; pin can be cleared.
- [x] Batch selection, Discard, and pointer dragging use current state.
- [x] Pool and route contexts have separate modules with direct consumers.
- [x] Fresh browser smoke uses owned runtime resources and backend readback.
- [x] Fresh Node, UI, and isolated Rust verification is recorded below.
- [x] Header status, key masking, table alignment, and responsive containment pass automated regression checks.
- [x] npm development and built-demo modes pass the same 53 browser checks.
- [x] Desktop and browser builds share version metadata, brand assets, and clear mode badges.
- [x] Real PNG, ICO, ICNS, and macOS tray assets pass release preflight.
- [x] Rust startup test requests HTTP health from the actual port-0 listener before shutdown.
- [x] Native platform guards and tray close policy pass local tests.
- [x] Manual native build matrix is configured for Windows, macOS x64/arm64, and Linux.
- [ ] Final zoomed Computer Use acceptance; the user stopped input with Escape.
- [ ] Per-platform installation, tray interaction, deep links, signing, and upgrades.
- [ ] Per-row rendering and bundle improvements have measurement evidence (deferred).

## Verification commands

Run Node and Rust tests in a process with disposable `AAG_CONFIG_DIR` and
`AAG_HOME_DIR` values. Leave `AAG_TEST_CCSWITCH_DB` unset so optional real-database
and real-provider probes do not run. Do not execute the legacy root diagnostics.

From the repository root:

```text
npm run typecheck
npm test
npm run test:desktop_runner
npm run ui:build
npm run ui:build_demo
npm run ui:smoke
node scripts/smoke-demo.mjs --built-demo
npm run release:verify
```

After `ui/dist/` exists, from `src-tauri/`:

```text
cargo check --locked --offline
cargo test --locked --offline --lib -- --test-threads=1
```

Rust tests that change process-wide environment variables are run serially.
Browser smoke is synthetic IPC evidence, not native desktop integration or an
external model-provider request. Its owned Vite server fails on a port conflict;
it does not reuse an existing development server.

## Latest verification - 2026-08-28

- Node typecheck and 28/28 tests: passed with temporary configuration isolation.
- Desktop startup/build configuration tests: 7/7 passed.
- UI TypeScript, native production build, and explicit demo build: passed.
- Rust check and serial unit tests: 125 passed, zero failed, locked and offline.
  Two opt-in real-database/provider test bodies remained disabled.
- Browser smoke: 53/53 passed separately in development and built-demo modes.
  Both runs used owned browser profiles/servers and completed cleanup.
- Layout containment passed at 1200px, 900px, and 420px. Stable light/dark theme
  controls passed the contrast regression after transition settling.
- Release preflight: passed for desktop version, frontend output, and all icons.
- Computer Use observed the actual npm demo in light and dark themes with
  aligned routes, masked keys, and the version/demo badge. The subsequent zoom
  review was interrupted with Escape and is not claimed complete.
- Windows release executable compilation and NSIS installer packaging passed.
  Installation was not performed; remote publication must be verified separately.
- macOS/Linux native execution, real upstreams, live CLI switching, signing,
  updater delivery, and measured rendering performance remain unverified.

## Previous verification - 2026-08-27

- Node typecheck: passed.
- Node tests: 18/18 passed with a disposable configuration directory.
- UI TypeScript and production build: passed.
- Rust check: passed, including after the updated frontend build.
- Rust tests: 122 passed, zero failed, serial execution in 7.81 seconds.
  Two optional real-database/provider test functions were gated off.
- Final browser smoke: 37/37 passed, including six Phase 23 regressions and a
  mock-backend guard. Initial Save is checked before gateway startup.
- Failure-path probes: missing-browser startup cleans its owned resources;
  CDP protocol errors/disconnects reject requests; asynchronous page errors
  cannot turn successful-looking values into a passing check.
- Cleanup: no smoke browser/profile or owned Vite listener remained; the
  pre-existing development server remained untouched.
- History integrity and `git diff --check`: passed.
- Real upstreams, native WebView2, live configuration switching, rendering
  performance, and deployment: not verified in this slice.

## Log

### 2026-08-28 - Desktop preview closure

- Kept Node CLI entry points intact while synchronizing npm browser and Tauri
  builds. Separated `ui/dist_demo` from the native `ui/dist` boundary.
- Added version/channel presentation, shared vector branding, valid native icon
  containers, macOS tray template assets, and platform-aware launcher guards.
- Restored bounded HTTP readiness verification without held-port races.
- Completed the fresh gate above and documented the unsigned preview boundary.
- Prepared explicit source/asset publication; no signing credentials or updater
  endpoint were introduced. Native acceptance remains the promotion gate.

### 2026-08-28 - Frontend polish automated gate

- Preserved the interrupted visual-polish changes and reviewed their lifecycle,
  masking, table semantics, and responsive layout without finding a new defect.
- Added product regression coverage, including delayed-health cancellation and
  application-switch remasking. Fresh smoke passed 48/48.
- Kept phase 23i and blocked only final visual acceptance. The prior Computer
  Use URL safety stop was respected; no alternate tool was used to claim a
  completed Windows visual review.
- Kept the separate Rust readiness assertion and measured-rendering follow-ups.

### 2026-08-27 - Phase 23 review and structural closure

- Reviewed `100938b` and its context-split predecessors; remote master matched.
- Reproduced stale captures with source-extracted handlers and synthetic I/O:
  two baseline routes deleted, zero upstream saves, and two identical pin calls.
- Replaced unsafe action memos with current callbacks and split the mixed
  context module. Removed unsupported rendering-isolation claims.
- Separated smoke orchestration, runtime ownership, CDP transport, page checks,
  and regression scenarios; preserved the public npm entry point.
- Preserved all 33 old phase entries verbatim in the history archive, with
  normalized line endings verified against the reviewed commit.
- Closed this functional repair slice with the fresh gate above. Rust readiness
  coverage and measured rendering work remain explicit follow-ups.
- A pre-existing gateway-modal timing failure occurred during harness bring-up;
  subsequent independent runs passed. Its cause is not claimed fixed; see the
  review record before treating timing stability as closed.

## References

- [Current structure](docs/project_structure.md)
- [Phase 23 review and remaining recommendations](docs/reviews/phase_23_review.md)
- [Frontend product polish and visual evidence](docs/reviews/frontend_product_polish.md)
- [Desktop build and release guide](docs/desktop_release.md)
- [v0.2.0 preview notes](docs/releases/v0_2_0.md)
- [Preserved Phase 22c-23i history](docs/progress/phase_22_23_history.md)
- [Earlier history through Phase 22b](docs/PHASE-ARCHIVE.md)
- [Architecture and migration design](DESIGN.md)

## Follow-up discipline

Select one bounded follow-up and run its relevant gate before advancing a phase.
A failed gate keeps the feature unchecked and sets status to blocked with the
exact failure. Keep detailed historical logs in the archive, not in this file.
