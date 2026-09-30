# Platform launcher validation — 2026-09-11

Scope: asynchronous launcher IPC, operating-system-specific discovery and
terminal dispatch, isolated editor environments, launcher DTO serialization,
native configuration root selection, and host-aware browser smoke discovery.
The earlier Unix database test assertion repair remains in the working tree.
No release or publication was performed; PROGRESS.md's release phase is unchanged.

## Per-command results

All results below are from the local macOS arm64 host. Node/Rust tests used
dedicated AAG_CONFIG_DIR and AAG_HOME_DIR directories, with AAG_TEST_CCSWITCH_DB
unset. TMPDIR was set to `/private/tmp`. Rust commands ran from `src-tauri/`.

| Command | Result | Evidence boundary |
| --- | --- | --- |
| `npm run typecheck` | Passed | Node TypeScript |
| `npm test` | 50 passed | Local Node fixtures |
| `npm run test:desktop_runner` | 7 passed | Desktop dev/build configuration |
| `npm run test:ui_smoke_runner` | 16 passed, 2 Windows-only skips | Local browser discovery, deadlines and cleanup fixtures |
| `npm run ui:build` | Passed | Native frontend TypeScript and bundle |
| `npm run ui:build_demo` | Passed | Explicit synthetic demo bundle |
| `npm run ui:smoke` | 113/113 passed | Source-served synthetic IPC; owned browser/profile cleaned |
| `npm run ui:smoke_demo` | 113/113 passed | Built synthetic IPC; owned browser/profile cleaned |
| `cargo check --locked --offline` | Passed | Native host compilation |
| `cargo test --locked --offline --lib -- --test-threads=1` | 344 passed, 5 ignored | Local Rust fixtures, including real child processes and loopback proxy tests |
| `npm run release:verify` | Passed | Release preflight and current-tree privacy audit, zero blockers/warnings |
| `npm run test:privacy` | 17 passed | Disposable export/audit fixtures |
| `git diff --check` | Passed | Whitespace/integrity check |
| `npm run tauri:dev -- --exit-on-panic -- --locked --offline` | Running | Native development process and owned Vite frontend, using the prior isolated test configuration |

## New regression evidence

- Status serialization supplies the frontend's exact camelCase fields.
- The default Codex CLI home is isolated; native-directory aliases are rejected.
- Platform installation candidates and editor-specific extension directories are
  checked for Windows, macOS and Linux using fixture paths.
- Literal environment values containing whitespace, apostrophes, dollar syntax,
  percent signs, shell operators and newlines reach a real local child intact.
- Missing working directories fail without executing an agent. Invalid
  environment names and NUL-containing values fail before dispatch.
- Private Unix launcher scripts remove themselves before the child starts.
- Bounded probes reject oversized output, hide child error content, time out and
  reap the timed-out process.
- Editor child argv preserves profile paths and environment values; explicit
  environment changes select a different isolated editor profile.
- Portable/test homes take priority over inherited APPDATA/XDG_CONFIG_HOME.
- Browser discovery selects only the requested host platform's candidates.

## Limits and recovered environment failures

The first launcher test run exposed an overly broad extension-directory match;
the matcher was tightened before the full successful run. Initial socket-based
runner tests were denied by the execution sandbox; rerunning with local process
and socket access passed. npm packaging checks required a writable temporary
npm cache. Privacy export fixtures initially traversed macOS's system temporary
directory symlink; rerunning with canonical TMPDIR passed.

Existing build warnings remain: an unrelated unused mutable binding in DSH and
the large bundled marketplace catalog. This change makes no measured rendering
or throughput claim.

Windows console/Store launch behavior and Linux desktop terminal behavior were
not executed on native hosts in this session. The Codex Desktop mirror remains
Windows-only. Fixture tests and synthetic browser smoke do not prove external
editor extension activation, real-provider access, native installation, signing,
tray interaction or upgrades. The unbundled macOS development window could not
be addressed by the available UI automation tool, so GUI click acceptance is not
claimed.

See [platform launcher contracts](../platform_launchers.md) for module ownership
and persistent editor-profile behavior.
