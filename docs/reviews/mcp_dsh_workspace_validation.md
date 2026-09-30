# MCP, DSH, and workspace validation

Date: 2026-08-31. Status: **implemented and locally validated; ready for review**.
This is not a release, installation, live-client, or production-readiness claim.

## Tested identity and authority

- Branch: `master`; unchanged HEAD:
  `a48b927dfd47936a913852fb99214e1ed471c5c3`.
- The initial worktree was clean. All changes remain uncommitted; no branch,
  commit, push, release, scheduled task, live client write, or deployment was made.
- Source snapshot: 48 changed/nonignored new files excluding `docs/` and the
  root `README.md`. SHA-256:
  `19da815ec26214b0f546181274be65c58266f8272b22b9fb791e3fd7130eb1db`.
  This hashes the UTF-8, newline-joined, sorted `relative_path:lowercase_sha256`
  entries, with no final newline, using raw file bytes for each entry. The
  documentation/evidence files do not participate in the snapshot hash.
- Environment: Windows x64, Node `24.18.0`, Cargo `1.97.1`, existing local npm
  dependencies and offline Cargo cache. No new package dependency was added.
- Feature risk: persistent UI/registry changes are Amber. Credential-bearing
  file synchronization is Red and remains behind explicit trusted-manager
  confirmation. The companion has no write or approval authority.

The task used the repository/service-boundary and evaluation workflows to keep
MCP as a thin surface over existing state, require negative security scenarios,
and distinguish local evidence from external acceptance. The secure-release
gate was used for local review only, not to authorize publication.

## Delivered behavior

| Area | Implemented result | Boundary |
| --- | --- | --- |
| Workspace | Warm ivory/terracotta and charcoal themes, serif headings, grouped navigation, keyboard skip link, active-tab visibility, lazy MCP page | AAG identity retained; English default with explicit saved locale preferences preserved |
| MCP registry | Validated stdio/HTTP definitions, per-client flags, redacted inspection, search, create-only protection, revision-bound editing | Existing SQLite table remains authoritative; no package execution or endpoint probing |
| Claude sync | Reviewed single-use expiring preview, stale snapshot rejection, namespace-preserving merge, exact backup, atomic replace, Windows ACL protection | Only explicit apply changes a client; unrelated JSON values preserved; close external clients first |
| Codex MCP | Redacted TOML review | No Codex file read, merge, overwrite, or automatic setup |
| MCP companion | Three read-only stdio tools for capabilities, verification plans, and workflows | Static repository assistance; no live state, arbitrary file access, shell, network, or approval tools |
| DSH | Alternate HTTP-event authentication, declared executable binding, package/patch containment and digest checks, startup cancellation cleanup | No package install, real runtime start, or native credential adoption |
| Automation | Shared feature catalog, explicit plan/run CLI, isolated test state, fail-fast receipts, timeout/cancellation, bounded browser startup retry | Trusted tests only; detached descendant cleanup is not universally verified |

See [MCP management](../mcp_management.md),
[companion setup and automation](../mcp_companion.md), and
[DSH hardening](../dsh_hardening.md) for full contracts and recovery behavior.

## Fresh complete gate

Executed `npm run verify:features -- --scope all --run`. Its
The final structured receipt records all 14 stages,
durations, foreground exit evidence, exclusions, and temporary-state cleanup.
All stages passed and `unrun_checks` is empty.

| Command/stage | Result |
| --- | --- |
| `npm run typecheck` | Passed |
| `npm test` | 28 passed |
| `npm run test:automation` | 20 passed; 1 Unix-only process-group scenario skipped on Windows |
| `npm run test:desktop_runner` | 7 passed |
| `npm run test:dsh_transport` | 2 passed, including actual owned loopback HTTP/WebSocket traffic |
| `npm run test:dsh_market_catalog` | 11 passed |
| UI `tsc -b` | Passed |
| Native Vite build (`ui:build` build stage) | Passed |
| Demo Vite build (`ui:build_demo` build stage) | Passed |
| `npm run ui:smoke` | 112/112; zero uncaught errors; owned resources closed |
| `node scripts/smoke-demo.mjs --built-demo` | 112/112; zero uncaught errors; owned resources closed |
| `npm run release:verify` | Passed local version/icon/container preflight only |
| `cargo check --locked --offline` | Passed |
| `cargo test --locked --offline --lib -- --test-threads=1` | 247 passed, 0 failed, 5 explicitly ignored |

The focused MCP suite passed 25 tests before the final whole-library run; all
25 also passed in that integrated run. The DSH subset passed 60 with four
explicit opt-in ignores. The integrated Rust total includes two pre-existing
real-database/provider functions that return early when `AAG_TEST_CCSWITCH_DB`
is unset; they are **not** real upstream evidence.

The five Rust ignores are the live Anthropic canary, two pinned DSH acceptance
tests, public GitHub catalog refresh acceptance, and pinned pnpm plugin
lifecycle. No ignored test was enabled. Browser demo checks use synthetic IPC,
not installed WebView2/native command invocation.

## Negative and independent review coverage

- Duplicate Add cannot overwrite a saved private definition; stale metadata
  edits cannot restore old client selections.
  A stale complete edit also cannot recreate a record deleted while the editor
  was open; a fresh explicit Add remains supported.
- Review cancel releases its native slot. Missing, expired, discarded, stale,
  and repeated apply tokens cannot write the disposable client file.
- Metadata edits omit private configuration; list/review responses mask private
  arguments, environment/header values, URLs, and unknown definitions.
- Malformed/oversized client files, read-only files, path links, interrupted
  locks, and backup collisions fail without silently replacing source data.
- Existing Windows owner/group/DACL is preserved on replacement and backup;
  first creation receives an owner-only protected DACL. Exclusive creation
  prevents a reader from retaining a handle before the ACL is tightened.
- DSH unauthorized Expect traffic was reproduced as HTTP 200 before repair;
  it is now denied. Three initial Rust regressions also reproduced startup,
  executable-identity, and patch-containment defects before repair.
- GUI tests cover cancel, acknowledgement, expiry, busy-state keyboard focus,
  error recovery, per-client selection, and registry-only deletion.
- The companion's actual stdio process rejects unknown tools, file/shell
  arguments, approval forgery, malformed input, and oversized messages.
- The verification runner reports detached-child cleanup as `not_verified`.
  A real detached-helper fixture confirms this limitation instead of converting
  a foreground exit into a complete process-tree cleanup claim.

The final structured evaluation validates with zero
failures in the required local scenarios. Its `conditionally_ready` result is
limited to this local evaluation and names the unexecuted Unix scenario. It is
not an authorization to deploy, enable live writes, or control hardware.

## Preserved failure and repair evidence

The first full automated run stopped at browser startup with `EBUSY` while
Edge was writing `DevToolsActivePort`. It correctly left later stages unrun.
The startup reader now treats only `ENOENT`/`EBUSY` as not-ready within the
existing 20-second deadline; permanent errors remain failures. Six deterministic
regressions passed, then the **entire** gate was rerun successfully. The initial
failed run is not relabeled as a pass.

Earlier 108-check browser evidence preceded create-only/revision, expiry/focus,
and review-disposal additions. The next complete gate passed 111 browser checks
in each mode and 246 Rust tests; its private receipt and evaluation are preserved with their original
source fingerprint. A final deletion-versus-stale-edit regression then exposed
and fixed a missing-record CAS boundary. The entire gate was rerun on the final
fingerprint above: 112 browser checks per mode and 247 Rust tests. Earlier
evidence is not silently reused for the changed source.

## Visual and source review

Owned synthetic browser checks covered Claude/MCP/DSH at 1440, 1024, 900, 768,
390, and 320 pixels. The active navigation item remained visible, keyboard skip
access worked, and a real `navigator.language = zh-CN` override still defaulted
to English. Explicit saved language choices survived reload. Measured light/dark
header, route-input, and primary-button contrasts exceeded 4.5:1 in those checks.

Final screenshots were visually inspected after the MCP form styling fix:
`tmp/workspace_final_screenshots/mcp_desktop_light.png`,
`mcp_mobile_light.png`, `mcp_desktop_dark.png`, and
`claude_desktop_light.png`. They are local, ignored QA outputs rather than
versioned release assets. The illustrated server existed only in synthetic
state. All owned screenshot browser/server/profile resources were closed.

Native Git whitespace checks and the read-only release-scope inspector passed.
The changed-file private-pattern audit found zero matching files. These checks
do not constitute a comprehensive independent security audit. No unrelated
project, credential store, or live client configuration was changed.

## Remaining limits and next authorized steps

- Real Claude startup/loading, remote MCP availability/OAuth, installed native
  GUI behavior, DSH/pnpm package acceptance, native installation/tray/upgrades,
  macOS/Linux execution, and signing remain unverified.
- Codex is preview-only, and redacted output is not a runnable export.
- MCP registry values are local SQLite data, not an encrypted credential vault.
  Plugin and MCP executable trust still requires user review.
- The Windows adapter preserves owner/group/DACL, not audit SACLs or unrelated
  filesystem metadata. Another application's uncooperative concurrent writes
  cannot participate in AAG's lock; close clients before applying.
- The existing approximately 8.27 MB uncompressed lazy marketplace-catalog
  chunk still emits a Vite bundle-size warning. It was not hidden or counted as
  a measured performance improvement.
- No commit or publication is authorized by this record. Historical release
  status in `PROGRESS.md` was deliberately not rewritten for this feature task.

The next practical use is to open the browser demo, inspect the MCP workspace,
and review the companion template. Live client adoption or package installation
requires a separate explicit user action and its own acceptance evidence.
