# Repeated code review on the Windows host — 2026-09-28

## Source and scope

A third review pass over the two commits that had not yet been independently
reviewed — `7e5b333` (macOS platform discovery and verification) and `5475a60`
(auto-resolve client config paths) — executed on a Windows x64 host, the
mirror image of the earlier macOS-host validations. Three parallel reviewers
covered the launcher services, the provider config-path services, and the
scripts/UI surface; every finding was re-verified in source before fixing.
The pass also surfaced seven Rust test failures and one Node suite failure
that reproduce only on a Windows host with a shared temporary directory that
carries untrusted writer ACEs.

## Findings fixed

Launcher discovery (`7e5b333`):

- Versioned sibling directories (`app-1.10.0` vs `app-1.9.0`, Codex version
  dirs) were selected by lexicographic sort, so a stale installation could be
  launched. `discovery::newest_versioned_executable` now compares numeric
  versions, falls back to lexicographic order only for unparsable names, and
  skips oversized/unreadable roots in full. Used by Claude Desktop discovery
  and `find_codex_cli_binary`; covered by three new tests.
- `claude_mirror_status` hard-failed when the best-effort desktop-binary
  probe failed (missing PowerShell, scan error, probe timeout); it now
  degrades to `claude_binary: null`. `claude_open_desktop` keeps hard errors.
- A directory with more than 128 entries aborted Claude Desktop discovery
  entirely; oversized roots are now skipped per-stem.
- The cc-switch discovery test restored `AAG_HOME_DIR` without drop safety; a
  panic mid-test would leak the override into other tests.

Provider configuration (`5475a60`):

- The Copilot JSONC stripper did not track `\"` escapes, so a valid settings
  file containing an escaped quote and a `//`-bearing URL value failed to
  parse and blocked the switch. Escape tracking (kilo.rs parity) ported, with
  regression tests.
- `rollback` in `config_writes.rs` aborted at the first mismatch, leaving
  earlier files of a batch unrestored; it now attempts every applied file
  independently, deliberately leaving third-party-modified files untouched,
  and reports failure only for what could not be restored. The transactional
  layer had zero tests; four now cover duplicate-target rejection
  (normalized spelling/case), identical-skip without a redundant backup, and
  both rollback outcomes.
- Duplicate-target detection compared raw `PathBuf`s; spelling and
  drive-letter case could bypass it. Normalized component comparison now.
- The portable-run isolation boundary used case-sensitive `Path::starts_with`;
  on Windows a contained override spelled with different case was silently
  dropped. Component-wise case-folded containment now (cfg(windows)).
- Gemini `.env` rewrites permanently dropped comments and unparsable lines,
  contradicting the module contract. The rewrite now preserves every
  non-managed line verbatim in order, replaces managed keys at first
  occurrence, drops managed duplicates, and clears managed keys the caller
  does not provide (stale-model leak). The `settings.json` read gained the
  same symlink re-check the `.env` read already had.
- `codex.rs` auto-heal logged `AppError` Display, which embeds raw local
  paths; it now logs the stable external message. `reset_mirror_session_state`
  wrote `auth.json` non-atomically without the link guard; it now hardens the
  mirror and uses the atomic write like every other mirror write.
- `claude_mirror.rs` carried a comment claiming malformed mirror settings are
  replaced; the code correctly blocks and retains. Comment fixed and a test
  now pins the fail-closed behavior.

Scripts and UI:

- `smoke_runtime.test.mjs` built darwin browser-candidate expectations with
  literal `/` separators while the implementation used the host path module,
  failing on Windows; expectations are now built with the same `join`, and
  the first two tests pass explicit empty `home`/`path`/`browser_path`
  overrides so host environment cannot interfere.
- `desktop_environment.mjs` npm discovery missed the mainstream Windows
  layout (no npm bundled beside `node.exe`): `desktop:doctor` run with bare
  `node` reported npm unavailable although npm worked, and the bare-`npm`
  fallback can never launch on Windows (`.cmd` shims need a shell). A
  candidate list (npm_execpath, node-adjacent, Program Files npm-cli.js) now
  feeds a node-based probe; bare `npm` is probed only on POSIX. New test
  covers all three layouts.
- `verification/runner.mjs` could throw from its cleanup `finally`, discarding
  the whole receipt; a failed removal now retains the owned root and reports
  `runtime_state: retained` instead.
- Launcher buttons treated "status unknown" as "unsupported", briefly
  disabling them on every load and permanently when the status errored; the
  props are now tri-state (disable/hint only on an explicit false).
- `Client_config_status` re-fetched on every mount because the page-level
  refresh bumped the refresh version after the card's own initial fetch; the
  first load per app type no longer double-bumps.
- The backend's stable reason identifiers were rendered as raw text;
  localized keys were added (en/zh) together with the missing
  `client_config.platform.unsupported` key.

Windows-host test-infrastructure fixes (verified root causes, no checks
weakened):

- The host's shared `%TEMP%` carries inherited Modify ACEs for sandbox SIDs,
  which AAG's own fail-closed `external_parent_safe` check correctly refuses.
  Test fixtures that build homes under `std::env::temp_dir()` now harden them
  with `prepare_managed_private_dir` (the established kilo/opencode pattern):
  `claude_mirror` `fresh_home`, the gemini test root, and the codex auto-heal
  path (which hardens the mirror itself, matching the switch path).
- Rust's `PathBuf::push` onto a canonicalized (verbatim) base normalizes `..`
  on Windows, so traversal spellings built with `join` never reached the
  validator. The resolver tests now build traversal inputs by string
  concatenation.
- `config_paths.rs` multi-segment literals produced mixed separators in
  user-visible paths on Windows; stepwise joins keep spellings uniform (the
  kilo string-comparison test depends on this).

Considered and not changed: the "stale model catalog" finding is unreachable
(OFFICIAL_MODELS always fill the fallback), and a dangling-symlink mirror
file already fails closed at `validate_external_file_path` before the
existence flag is computed.

## Per-command evidence (Windows x64 host)

Disposable `AAG_CONFIG_DIR`/`AAG_HOME_DIR` were canonical Windows-absolute
directories; `AAG_TEST_CCSWITCH_DB` unset. Note: Git Bash `mktemp -d` yields
`/tmp/...` spellings that native Windows processes cannot treat as absolute —
early baseline runs failed from that alone (including the Node e2e suite at
load, previously recorded as a Windows ACL boundary; it passes with canonical
paths).

| Command | Result | Boundary |
| --- | --- | --- |
| `cargo check --locked --offline` | Passed, no new warnings | Windows target |
| `cargo test --locked --offline --lib -- --test-threads=1` | 472 passed, 0 failed, 5 ignored (baseline before fixes: 453 passed, 7 failed) | Local fixtures and loopback only |
| `npm run typecheck` | Passed | Node surface |
| `npm test` | 76 passed, 1 skipped, 9/9 files (e2e suite included, with canonical dirs) | Local fixtures |
| `npm run test:desktop_runner` | 17 passed | Prerequisite discovery and desktop build config |
| `npm run ui:build` + `npm run ui:build_demo` | Passed | UI typecheck and both bundles |
| `node --test scripts/desktop_environment.test.mjs scripts/verification/runner.test.mjs scripts/ui_smoke/smoke_runtime.test.mjs` | 39 passed, 2 platform-skips | Script fixtures |
| `npm run release:verify` | 0 blockers, 0 warnings | Local preflight and tree scan; no release |
| `npm run ui:smoke` / built-demo | Both stop inside the MCP check section (~79-81 of 121) with `CDP Runtime.evaluate` timeouts | Pre-existing on this host: reproduces identically on the pristine tree (stash-verified). The 121/121 evidence remains the macOS round. |

## Limits

All evidence is local: no real client configuration, upstream provider,
signing, installation, or upgrade acceptance is claimed. The desktop mirror
launchers remain Windows-only by design. The Windows smoke/MCP-section hang
is recorded here as an open Windows-host acceptance item for a later bounded
follow-up; it was deliberately not chased in this round.
