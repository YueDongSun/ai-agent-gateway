# AAG Development Guide

## Prerequisites

- Node.js 20 or newer.
- Rust 1.88 or newer (required by the locked dependencies), with the Tauri
  platform dependencies for the target OS.
- A browser supported by the owned browser smoke runner for UI smoke tests.

The repository tracks root/UI npm lockfiles and `src-tauri/Cargo.lock`. Update
them intentionally with their manifests; do not hand-edit lock contents or
silently fall back to unlocked dependency resolution.

## Install

From the repository root:

```text
npm ci --ignore-scripts
npm ci --prefix ui --ignore-scripts
```

Lifecycle scripts are disabled during dependency restoration. Run an explicitly
reviewed package build step afterward; never rely on install-time code for AAG
configuration or artifact generation.

## Common commands

| Command | Purpose |
| --- | --- |
| `npm run dev` | Start the legacy Node TUI |
| `npm run gateway` | Start the legacy Node gateway |
| `npm run typecheck` | Typecheck the Node surface |
| `npm run typecheck --prefix ui` | Typecheck the React frontend |
| `npm test` | Run Vitest tests, including focused UI domain and contract tests |
| `npm run ui:dev` | Serve the React development UI |
| `npm run ui:demo` | Serve the browser demo with synthetic IPC |
| `npm run ui:build` | Build the native frontend into `ui/dist` |
| `npm run ui:build_demo` | Build the browser demo into `ui/dist_demo` |
| `npm run ui:bundle_report` | Print per-build JS/CSS bundle sizes with a regression budget (run after the builds) |
| `npm run ui:preview` | Build and preview the browser demo |
| `npm run ui:smoke` | Run owned browser smoke against source-served demo |
| `npm run ui:smoke_demo` | Build and smoke-test the demo bundle |
| `npm run tauri:dev` | Start the Tauri desktop app with the React UI and tray lifecycle |
| `npm run desktop:dev` | Start one owned Vite server and the Tauri desktop app |
| `npm run desktop:build` | Build the frontend and native desktop bundles |
| `npm run desktop:doctor` | Automatically detect local desktop prerequisites; exit nonzero if anything required is missing |
| `npm run test:desktop_runner` | Test Tauri dev/build configuration helpers and native acceptance preflight |
| `npm run test:desktop_native` | Opt-in Windows release lifecycle checks with disposable AAG/WebView2 state; requires closing existing AAG |
| `npm run icons:generate` | Generate native icons from the shared SVG |
| `npm run release:verify` | Check release version, assets, and containers |
| `npm run privacy:audit` | Scan the current tracked/worktree publication surface with redacted findings |
| `npm run privacy:audit_history` | Confirm that legacy refs remain blocked from public export |
| `npm run privacy:export_snapshot` | Export one audited commit as a history-free public tree |
| `npm run test:privacy` | Test privacy scanning, false-positive, and snapshot containment contracts |
| `npm run test:automation` | Test feature plans, foreground cleanup semantics, and the actual stdio companion |
| `npm run verify:rust` | Plan ordinary Rust and Windows qualification profiles without execution |
| `npm run verify:rust -- --run` | Execute ordinary tests, then report Windows qualification separately; unavailable prerequisites are nonzero, never a pass |
| `npm run verify:rust -- --profile ordinary --run` | Run ordinary Rust tests; on Windows this excludes exactly the 26 qualification IDs |
| `npm run test:rust_acceptance` | Test exact Rust profile partitioning, prerequisite handling and execution receipts |
| `npm run verify:features -- --scope all` | Inspect the full local verification plan without execution |
| `npm run verify:features -- --scope all --run` | Execute the fixed gate with isolated AAG state and a structured receipt |
| `node scripts/mcp/aag_mcp.mjs` | Run the read-only MCP companion over protocol-only stdio |

### Automatic functional regression

[`functional_regression.yml`](../.github/workflows/functional_regression.yml)
runs on relevant pushes and pull requests. It derives Node, UI and Rust jobs
from the changed paths. UI changes also run the root domain regressions and
the Rust frontend consumer; Node-only changes do not start native builds.
Shared inputs and workflow changes select all surfaces. If the comparison base
is unavailable, the classifier selects from the complete checked-out tree.

The Node job runs typechecking, Vitest, desktop-runner tests, Rust-runner tests
and browser-runner tests. The UI job typechecks and builds the native frontend,
then runs source and built-demo browser smoke sequentially. The Ubuntu Rust job
builds `ui/dist`, fetches the tracked Cargo lock into a cold cache, then runs
offline `cargo check`, compiles the test binary, and executes the serial ordinary
profile through the existing isolated-state runner. Precompiling keeps cold
compilation outside the runner's per-command test deadline. Root/UI installs
use `npm ci --ignore-scripts` and all Actions remain pinned. No live provider
probe is enabled.

These jobs report functional fixture and browser-demo evidence. The existing
privacy gate remains independent. The desktop packaging matrix remains manual;
Windows privilege qualification, native lifecycle, installation and real-provider
acceptance require their own prerequisites and receipts. A missing browser,
system dependency or toolchain is a failed or blocked check, not a pass.

### Local Rust verification

For Rust commands, run from `src-tauri/`:

```text
cargo check --locked --offline
cargo test --locked --offline --lib -- --test-threads=1
```

The serial flag is required for tests that use disposable process-wide
environment overrides. `ui/dist` must exist before Tauri `cargo check` or
`cargo build`, because the Tauri context reads the configured frontend output.

For ordinary Rust tests without Windows privilege/context prerequisites, use
`npm run verify:rust -- --profile ordinary --run`. On Windows this excludes
exactly the 26 IDs in the static qualification manifest; the four mirror symlink
cases remain in the ordinary profile on Unix. `--profile windows_qualification
--run` first inspects existing token capabilities, Robocopy metadata support and
symlink creation in a disposable fixture. It never grants rights, elevates,
enables Developer Mode or installs services. A missing or unproven prerequisite
returns nonzero as blocked, executes zero qualification tests, and lists their
unrun IDs. Every explicitly selected qualified test must execute exactly once;
an ignored, missing or zero-test result is not a pass. The default `all` profile
runs both streams independently and reports a combined nonzero result when the
qualification stream is unavailable.

The original full serial Cargo command above remains unchanged and may report
those 26 prerequisites as failures on an unqualified Windows host. An ordinary
profile pass is not full Rust acceptance, and a qualification preflight pass is
not qualification-test evidence.

The optional [Tailnet publication guide](tailnet_publication.md) records its
default-off settings, native ownership and shutdown contract, focused test
commands, and the separate live Serve/firewall/peer acceptance boundary. Its
loopback and fake-native tests do not establish a live Tailnet deployment.

The [Codex session-authority guide](codex_session_authority.md) records the
separate migration, protected service qualification, metadata-only client and
recovery boundaries. The existing homes have not migrated and the production
authority is not enabled. Keep those acceptance checks separate from source
fixtures and from Tailnet API publication.

Use `npm run verify:features -- --scope all --run` for unattended local
verification. It detects prerequisites first, creates private disposable
configuration/home/temp directories, resolves macOS temporary-directory aliases,
and sets `TMPDIR`, `TMP` and `TEMP` for every child automatically. No manual
macOS environment setup is required for this runner. Each command has a deadline,
and failures stop execution with a structured receipt and unrun-command list.

For direct Node/Rust test commands, use dedicated private (0700 on Unix)
`AAG_CONFIG_DIR` and `AAG_HOME_DIR` directories, and leave
`AAG_TEST_CCSWITCH_DB` unset so optional real-database and real-provider probes
do not run. The automated runner clears that variable for its child processes.
On macOS, also set `TMPDIR=/private/tmp` (or another canonical temporary
directory) so direct private-storage tests do not traverse the system `/var`
symbolic link.
See [platform launcher contracts](platform_launchers.md) for native call behavior,
profile isolation and the distinction between fixture tests and native acceptance.

### macOS automatic checks

`npm run desktop:doctor -- --json` reports the detected operating system and
architecture, Node/npm, installed project dependencies, Git, Rust/Cargo, Tauri, Xcode
Command Line Tools, clang, SDK, writable temporary storage and a supported
browser. `--scope node` and `--scope ui` check only the corresponding prerequisites.
Missing tools produce a failing exit code and a repair hint; the command does
not install dependencies or open agents. `verify:features --run` invokes the
same preflight automatically; omitting `--run` still prints only a static plan.

Browser discovery supports system and user `Applications` folders on macOS,
standard platform locations and absolute PATH entries. `AAG_BROWSER_PATH` can
select a custom executable; an invalid explicit path fails without falling back.
Runtime agent/editor discovery is separate and is described in
[platform launchers](platform_launchers.md).

Client management under Suppliers shows the configuration mode and source, plus
resolved paths for file-backed integrations; client views in the main navigation
render the active router models for each configuration. Choosing or reapplying a
provider updates those files on Windows, macOS and Linux; launching Claude Code,
Copilot CLI, Gemini CLI or a VS Code extension refreshes configuration before
starting the client. Codex CLI launches with its existing isolated `CODEX_HOME`
instead; select or reapply its provider to refresh the mirror configuration.
Unchanged content is left intact.
Changed files are backed up in AAG's private backup directory. A failed multi-file
update attempts to restore earlier files without overwriting intervening
third-party changes; incomplete rollback is reported. The CLI's documented
absolute home or file overrides are used when present; the resolver never
recursively searches the disk or reads shell startup scripts. Native Claude/Codex
login directories remain separate from their AAG mirror directories.

An explicit Codex Desktop mirror launch prepares a validated model catalog and
top-level AAG configuration in that same isolated home, then records exclusive
mode. Ordinary CLI coexist writes retain their prior behavior until the Desktop
action. Auth/session/history files and the native home remain outside that
preparation. Verify the returned mode and actual isolated child process separately
from a synthetic success receipt. See [supplier workspace](supplier_workspace.md#explicit-desktop-mirror-launch).

### Continuous mirror instruction checks

When instruction synchronization is explicitly enabled, both clients include
their `prompts/` directory alongside the allowlisted skill, agent and instruction
roots. Codex can also project supplemental instruction-only plugin skill trees
and resources from enabled native configuration and validated manifests. Skill
names already supplied by user/shared roots or configured mirror plugins are
excluded from that projection. Plugin activation, servers and account state are
not part of the copied instruction inventory.

Version admission requires one actual ordinary package-version directory. A
`latest` junction may duplicate an already enumerated direct directory in the
same package; it is not a selected-version pointer and does not resolve several
actual versions. Invalid or ambiguous input keeps the last good inventory.
Tests should cover both the admitted duplicate and a `latest` alias beside
multiple actual versions, plus prompt additions, edits and deletions.

Codex preference checks own only `agents.enabled` and
`agents.max_concurrent_threads_per_session`. Their TOML merge and backup must use
the same caller-bound snapshot as commit. Verify that routing/authentication,
permissions, MCP, features and plugin activation retain their own values.
Prepared skill/resource counts are inventory expectations; claim live parity
only after comparing the applied files and actual consumer discovery. See the
[instruction ownership contract](unified_suppliers.md#source-authoritative-mirror-instructions).

## Verification gates

For a Node-only change, run Node typecheck and tests. For a UI-only change,
run the UI typecheck and build. Workspace shell, header surface, provider
search, and dialog changes also run the focused browser suites listed in
[workspace interface QA](interface_qa.md). For a cross-tree, release, or
persistence change, run the complete current gate:

```text
npm run typecheck
npm test
npm run test:desktop_runner
npm run ui:build
npm run ui:build_demo
npm run ui:smoke
npm run ui:smoke_demo
npm run release:verify
npm run test:privacy
npm run privacy:audit
cd src-tauri
cargo check --locked --offline
cargo test --locked --offline --lib -- --test-threads=1
```

Record each command independently. Browser smoke uses synthetic IPC and owned
browser/server resources. It does not authorize real API requests or prove
native WebView2 behavior.

### Supplier workspace checks

The [supplier workspace guide](supplier_workspace.md) records persistence,
discovery and prompt-context ownership, visible states and a visual QA checklist.
Run the focused synthetic feature suite after building the demo:

```text
npm run ui:build_demo
node scripts/ui_smoke/supplier_workspace_checks.mjs --built-demo
node scripts/ui_smoke/coding_agent_arena_checks.mjs --built-demo
node scripts/ui_smoke/arena_cache_checks.mjs --built-demo
npx vitest run tests/supplier_ui.test.ts tests/arena_geometry.test.ts tests/prompt_workspaces.test.ts tests/tailnet_discovery_ui.test.ts
```

The browser suite owns its server/profile and covers supplier reconciliation,
Arena selectors, Tailnet display, prompt workspace state, contextual actions,
dialog containment and 1440/900/390/320-pixel layouts. Its Tailnet addresses,
ratings and provider records are synthetic; it proves no live peers or supplier
calls. Source and built-demo smoke remain separate checks.

From `src-tauri/`, use disposable AAG configuration/home directories and keep
the focused native suites serial:

```text
cargo test --locked --offline --lib services::import_cc_switch:: -- --test-threads=1
cargo test --locked --offline --lib database::dao::suppliers::maintenance:: -- --test-threads=1
cargo test --locked --offline --lib services::prompt:: -- --test-threads=1
cargo test --locked --offline --lib services::tailnet_discovery:: -- --test-threads=1
cargo test --locked --offline --lib services::model_catalog:: -- --test-threads=1
cargo test --locked --offline --lib proxy::publication:: -- --test-threads=1
cargo test --locked --offline --lib arena_preview_does_not_advance_live_rotation_and_retains_pin -- --test-threads=1
```

Verify account removal against current provenance/authentication evidence, real
static conversion preservation, stale-plan rejection, alternate promotion,
endpoint variant retention, backup/retry behavior and auth-bound discovery.
Prompt checks must exercise two-stage context binding and file changes between
preview and execution. Do not use real instruction contents, database files,
credentials or peer inventory as shareable fixtures.

Actual Desktop mirror launches, native WebView2 behavior and real Tailnet scans
require their own local acceptance receipts. Model generation and account login
are excluded from the ordinary gate. A focused passing suite does not erase
failures in the full Rust gate; record both results independently in `PROGRESS.md`.

### Supplier URL discovery checks

Additive supplier URL discovery has focused Rust coverage in
`services::model_discovery::supplier_url_tests`. The focused suite and
`cargo check --locked --offline` passed for this revision; rerun the focused
suite serially from `src-tauri/`:

```text
cargo test --locked --offline --lib services::model_discovery::supplier_url_tests -- --test-threads=1
```

The fixtures use loopback servers and synthetic supplier rows only. They do not
prove live upstream availability, inference success, or native behavior. Record
the focused result separately from the full Rust, UI, and native gates.

For the EN/CN discovery surface, run:

```text
npx vitest run tests/supplier_discovery_health.test.ts tests/supplier_discovery_localization.test.ts tests/i18n_key_parity.test.ts tests/supplier_ui.test.ts
npm run typecheck --prefix ui
npm run ui:build
npm run ui:smoke
npm run ui:smoke_demo
```

The browser scenario switches English → Chinese → English through the real
header button and requires the same stored results to update with only one
model-refresh invocation. It separately opens the editor in each locale and
requires a synthetic authentication failure to show localized context and cause,
without a raw backend code. It closes the modal before changing the header
locale. The scenario restores the original IPC bridge, locale and fixture state.
Key parity includes interpolation variables.

The repository ignores local SQLite databases and their WAL, SHM and rollback
journal companions, including `.sqlite3` sidecars. These rules do not hide the
discovery source, tests or documentation. Inspect a rule with
`git check-ignore --no-index -v -- scratch/catalog.sqlite3-wal`.

### Ranked multi-protocol gateway checks

The public evidence cache has three independent inputs: models.dev capability
metadata, EvalPlus coding scores, and the official LMArena overall text ranking.
When `mirror_model_roles_enabled=1`, one application-owned worker checks them at
startup and hourly, refreshing missing, errored, or older-than-24-hour caches.
Manual IPC refresh shares the same admission gate. Failed fetches/validation
keep the previous good document with stale/error metadata; parseable retained
evidence remains available with that status. Successful document or freshness
changes rebuild routing for new requests. This worker is separate from
upstream supplier-model discovery and ends with the application.

Arena fetches use the official dataset's complete category statistics plus
bounded `/rows` pages, followed by exact overall-count and same-publication-date
validation. The bound is 200 pages / 20,000 dataset rows, 8 MiB cumulative input,
and 90 seconds. A partial collection never replaces a good cache. The
[official dataset](https://huggingface.co/datasets/lmarena-ai/leaderboard-dataset)
and [statistics API documentation](https://huggingface.co/docs/dataset-viewer/statistics)
define the public source; they do not establish which supplier models are
actually offered or prove an atomic remote revision.

The seven client-facing role IDs are tier requests shared by Messages,
Responses, Chat Completions, and Gemini ingress. Their queues come from assessed
eligible supply, not from copying the selected default. Arena routing bands
compare distinct rated models in admitted enabled credentialless supply while
retaining original rank, interval, votes, condition, date and cohort size.
`MiniMax-M3.1-Flash` has no exact row in the inspected Arena snapshot; its Arena
result must remain absent rather than borrowing another MiniMax row. The full
role/evidence contract is in [unified suppliers](unified_suppliers.md).

For changes to those contracts, use the relevant focused Rust suites before
expanding to the full gate above, from `src-tauri/`:

```text
cargo test --locked --offline --lib services::model_catalog:: -- --test-threads=1
cargo test --locked --offline --lib services::mirror_models:: -- --test-threads=1
cargo test --locked --offline --lib services::mirror_sync:: -- --test-threads=1
cargo test --locked --offline --lib proxy::protocol_matrix_tests:: -- --test-threads=1
cargo test --locked --offline --lib proxy::router_runtime_tests:: -- --test-threads=1
cargo test --locked --offline --lib stateless_minimax -- --test-threads=1
```

These suites cover parser/pagination rejection, refresh admission/cancellation,
four-codec routing and streams, exact-model precedence, request-time capability
filtering, native Gemini preservation and strict foreign projection, and quota
fallback. Account allowance exhaustion skips other protocol/model rows with the
same resolved credential and HTTP origin within the request; generic model/rate
limits retain same-account alternatives. Neither case invents a vendor reset
deadline. Streaming fallback ends once output commits.

Native MiniMax Responses endpoints reject nonempty `previous_response_id` and
non-null `conversation` before sending, using
`responses_upstream_state_unsupported`; fully materialized `input` history is
the supported stateless path. Do not remove state references or unsupported
Gemini fields to make a fixture pass. Same-protocol native envelopes and opaque
signatures must stay private to their matching codec, and foreign projections
must reject semantics they cannot represent.

These are verification instructions, not recorded pass results. Fixture tests,
public-source fetches, real supplier calls, and native desktop acceptance are
separate evidence classes. Record current results and unresolved live limits in
`PROGRESS.md` and the owning acceptance review instead of inferring them from
this guide.

## Product diagnostics

For the MCP/DSH workspace slice, `verify:features` runs the checks above plus the
automation and DSH-specific checks in a fixed dependency order. Focused scopes
are `node`, `ui`, `mcp`, and `dsh`. The default is a read-only plan; execution
requires `--run`. Missing dependencies remain failures and are not installed by
the runner. See [companion and verification automation](mcp_companion.md).

The runner's receipts distinguish foreground exit evidence from detached-helper
cleanup, which remains the owning test suite's responsibility. Native/real-client
acceptance is excluded. This wrapper does not replace the per-command evidence
or turn an unsigned preview into a production release.

Open **Diagnostics** in the application header to inspect the local log policy,
open the log directory, copy a content-free environment summary, or enable
Debug for 30 minutes. Debug takes effect immediately and automatically returns
to Info; a restart restores only the unexpired remainder.

The same dialog shows the last 200 native HTTP request records in memory only.
Refresh reads the current ring; Clear removes its current records. Records use
opaque request/model references, a fixed surface label, timestamp, duration,
status, streaming flag, and nullable route-traversal indicator. A `?` means route
traversal was not observed, not that failover did not occur. Streaming records
are finalized when the response body finishes or is dropped; 499 denotes an
unconsumed/disconnected stream and 502 denotes an observed stream failure.
Browser demo timelines are empty unless a smoke test supplies synthetic metadata.

The gateway card reports routing apply state separately from listener changes.
Supplier, model, provider, route, pin, and public catalog updates hot-apply to
new requests; in-flight requests keep their admitted snapshot. A failed rebuild
keeps the previous snapshot and reports the failure. Host/port and gateway
authentication edits still require an explicit restart. The Responses prompt
toggle remains live. The collapsible route explanation describes the editor's
current routes, not a live execution trace; the diagnostics timeline describes
observed requests.

The logger retains five files of at most 2 MiB each. Request records contain a
correlation id, surface, hashed model/provider references, stage timings,
content-size counters, usage, and a fixed stop-reason category. External
failure envelopes use stable codes and generic messages. Frontend diagnostics
send only a fixed category, bounded message/stack character counts, and a
correlation id; raw errors and absolute log-directory paths do not cross IPC.
Never add prompts, system instructions, response text, tool inputs/results,
API keys, auth headers, absolute paths, full URLs, domains, email addresses, or
IP addresses to product logs.

Real-provider checks are separate from the normal gate. Run the ignored live
canary only with explicit authorization and configured local credentials; one
successful request verifies that observed protocol path only, not sustained
latency, deployment, or crash closure.

```text
cd src-tauri
cargo test --locked --offline --lib proxy::server::tests::live_anthropic_thinking_canary -- --ignored --exact --nocapture --test-threads=1
```

The canary defaults to `claude-opus-4-8`; set `AAG_LIVE_CANARY_MODEL` only when
an explicitly authorized route alias should be used instead. It selects the
first hop only and does not persist a failover winner.

## Development boundaries

- Keep Node, Rust, and UI changes scoped to the owning tree unless a contract
  change requires a coordinated update.
- Add tests for routing, conversion, breaker, persistence, importer, and state
  lifetime changes.
- Use `snake_case` for new project-controlled identifiers and filenames.
- Keep adjustable values centralized and validate them at boundaries.
- Never log or commit API keys, auth tokens, database files, or live agent
  configuration.
- Never use a browser response from a pre-existing process as proof that this
  checkout was tested.

## Credential-storage verification

Never run storage tests against a live home or configuration directory. The
focused Node tests create disposable roots and cover the private directory,
atomic replacement, exact rolling backup, Windows owner-only ACL, linked-path
rejection, and a deterministic temporary-file collision:

```text
npx vitest run tests/config_permissions.test.ts tests/config_temp_race.test.ts
```

The focused Rust tests cover private atomic files and backups, Windows
owner-only DACL/reparse handling, SQLite main/WAL/SHM files, and bounded
in-database backup retention. Keep Rust tests serial when process-wide state or
umask is involved:

```text
cd src-tauri
cargo test --locked --offline --lib config::private_fs::tests -- --test-threads=1
cargo test --locked --offline --lib database::private_storage_tests -- --test-threads=1
cargo test --locked --offline --lib database::dao::backup::tests -- --test-threads=1
```

Windows ACL tests prove the local no-elevation path on Windows. POSIX mode and
umask tests run only on Unix and must be executed on a Unix host before making
a cross-platform release claim. The files remain plaintext at rest; these
checks prove filesystem isolation and retention limits, not encryption,
keychain integration, malware resistance, or secure media erasure.

## Release boundary

The current release is the unsigned `v0.2.0` preview. Windows packaging has
been verified locally; installation, signing, updater delivery, macOS/Linux
native execution, live provider requests, and upgrade acceptance remain
separate evidence requirements. See `docs/desktop_release.md`.

## DSH verification

For DSH changes also run npm run test:dsh_transport and the DSH browser cases
inside the existing smoke commands. The pinned real-package acceptance is an
explicit opt-in test with disposable state and a local mock Claude endpoint.
See [DSH validation](dsh_validation.md) for the exact command and prerequisite
manifest. It must not be replaced by a real provider request without authorization.

Run npm run test:ui_smoke_runner when changing the smoke infrastructure. It
checks bounded CDP response reads and owned browser cleanup independently of
browser installation or page rendering. Browser/native smoke remain separate
checks and do not gain a passing result from these harness tests alone.

Marketplace changes also require npm run test:dsh_market_catalog. The existing
browser smoke includes the standalone market/new-tab flow. For explicit Windows
native acceptance, first finish cargo build --locked --offline --bin ai-agent-gateway,
then close any existing AAG and run npm run test:dsh_market_native. The test uses
disposable AAG and WebView2 state. Do not overlap native build and execution.
