# macOS automatic discovery and verification — 2026-09-23

## Source and scope

The private development branch was fast-forwarded from `1fe326b` to `9150d0a`
(21 upstream commits). Existing uncommitted platform-launcher work was saved in
`pre_mac_update_2026_09_23`, restored and integrated with the new Codex/Claude
mirror contracts. No feature commit, branch, push or release was created.

The changes cover passive CLI/editor/browser discovery, macOS Terminal dispatch,
platform configuration roots, automatic development prerequisites and verification
isolation. `desktop:doctor` checks the host without installing dependencies;
`verify:features -- --scope all --run` performs the complete local gate with
private canonical temporary directories and no manual macOS environment exports.

The locked native dependencies require Rust 1.88 or newer. Both the package
manifest and doctor now express that requirement. Finder/Dock discovery handles
Homebrew on arm64 and Intel, nvm, fnm, asdf and mise without executing shell
startup files. Fixture tests verify selected CLI/Node consistency, not just
candidate path strings. cc-switch database discovery now uses the effective home
when no explicit home argument is supplied.

The integration also repaired ten existing native-command permission omissions
in the app manifest and main-window capability. The market/runtime authority
boundary remains covered by the existing DSH transport regression. Its guard
assertion now tolerates Rust formatting without weakening the required call.

## Per-command evidence

Local host: macOS arm64. All agent configuration used by tests is disposable;
optional real-database/provider probes remain disabled. The automatic runner
sets isolated AAG_CONFIG_DIR, AAG_HOME_DIR, TMPDIR, TMP and TEMP itself.

| Command | Result | Boundary |
| --- | --- | --- |
| `npm run verify:features -- --scope all --run` | Passed: 15 preflight checks and all 15 stages; no unrun checks; runtime state removed | Complete unattended local/synthetic gate |
| `npm run desktop:doctor -- --json` | 15 prerequisite checks passed | Local toolchain and browser discovery |
| `npm run typecheck` | Passed | Node TypeScript |
| `npm test` | 77 passed | Local fixtures |
| `npm run test:automation` | 37 passed, 2 Windows-only skips | Scope plans, temporary isolation, cleanup and MCP fixtures |
| `npm run test:desktop_runner` | 16 passed | Dev/build helpers and prerequisite detection |
| `npm run test:ui_smoke_runner` | 18 passed, 2 Windows-only skips | Browser discovery, bounded I/O and owned cleanup |
| `npm run test:dsh_transport` | 2 passed | Local transport and exact native authority boundary |
| `npm run test:dsh_market_catalog` | 17 passed | Local catalog fixtures |
| `npm run ui:build` | Passed | Native frontend typecheck and bundle |
| `npm run ui:build_demo` | Passed | Explicit synthetic demo bundle |
| `npm run ui:smoke` | 121/121 passed | Source-served synthetic IPC |
| `npm run ui:smoke_demo` | 121/121 passed | Built synthetic IPC |
| `cargo check --locked --offline` | Passed | macOS compilation |
| `cargo test --locked --offline --lib -- --test-threads=1` | 455 passed, 6 explicitly ignored | Rust fixtures and local loopback traffic |
| `cargo test --locked --offline --lib services::launcher::tests::macos_terminal_native_fixture_receives_environment_and_cleans_up -- --ignored --exact --test-threads=1` | 1 passed | Native Terminal executes a disposable script and preserves literal environment values |
| `npm run release:verify` | Passed, zero privacy blockers/warnings | Preflight only; no release |
| `npm run privacy:audit` | Passed, zero blockers/warnings | Explicit audit stage in the automatic gate |
| `npm run test:privacy` | 17 passed | Disposable audit/export fixtures |
| `git diff --check` | Passed | Patch integrity |

Rust commands run from `src-tauri/`. The automatic gate invokes UI typechecking
and Vite build stages separately; the table also records the directly run npm
build commands. Native Terminal may retain its completed window according to
user preferences.

## Failures resolved and limits

Initial sandboxed socket tests failed with `listen EPERM`; they passed with
local process/socket access. An initial manually created test directory used
0755 and failed private storage validation; the automatic runner explicitly
creates its private directories as 0700. No storage restriction was relaxed.
The first complete gate correctly stopped on the missing native permissions;
those declarations and the formatting-sensitive assertion were fixed before
rerunning the gate. A subsequent run passed all browser checks but exposed a
macOS process-group cleanup edge case: signaling a group containing only zombies
returns EPERM. A native fixture reproduced this behavior. The runner handles
that case only after foreground exit and a bounded process-state probe confirms
no live group members; permission failures with live or uninspectable members
remain failures. The retained fixture directory from the failed run was removed
after confirming its processes were gone.

Codex and Claude Desktop **mirror launching remains Windows-only**. The macOS
work covers CLI terminals, editor discovery/dispatch and environment detection;
it does not claim support for those desktop mirror launches. Custom installs
outside the supported conventions require a PATH/manager-root/browser override.
There is no promise of arbitrary full-disk installation discovery.

The native Terminal test runs a local fixture, not a real coding agent. Browser
smoke uses synthetic IPC. Real editor extension activation, provider access,
macOS Intel execution, installation, signing and upgrades remain unverified.
Existing DSH unused-mut and marketplace chunk-size warnings are unrelated and
remain. The release phase in PROGRESS.md is unchanged.
