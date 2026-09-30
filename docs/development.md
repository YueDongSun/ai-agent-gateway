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
| `npm test` | Run Node Vitest tests |
| `npm run ui:dev` | Serve the React development UI |
| `npm run ui:demo` | Serve the browser demo with synthetic IPC |
| `npm run ui:build` | Build the native frontend into `ui/dist` |
| `npm run ui:build_demo` | Build the browser demo into `ui/dist_demo` |
| `npm run ui:preview` | Build and preview the browser demo |
| `npm run ui:smoke` | Run owned browser smoke against source-served demo |
| `npm run ui:smoke_demo` | Build and smoke-test the demo bundle |
| `npm run tauri:dev` | Start the Tauri desktop app with the React UI and tray lifecycle |
| `npm run desktop:dev` | Start one owned Vite server and the Tauri desktop app |
| `npm run desktop:build` | Build the frontend and native desktop bundles |
| `npm run desktop:doctor` | Automatically detect local desktop prerequisites; exit nonzero if anything required is missing |
| `npm run test:desktop_runner` | Test Tauri dev/build configuration helpers |
| `npm run icons:generate` | Generate native icons from the shared SVG |
| `npm run release:verify` | Check release version, assets, and containers |
| `npm run privacy:audit` | Scan the current tracked/worktree publication surface with redacted findings |
| `npm run privacy:audit_history` | Confirm that legacy refs remain blocked from public export |
| `npm run privacy:export_snapshot` | Export one audited commit as a history-free public tree |
| `npm run test:privacy` | Test privacy scanning, false-positive, and snapshot containment contracts |
| `npm run test:automation` | Test feature plans, foreground cleanup semantics, and the actual stdio companion |
| `npm run verify:features -- --scope all` | Inspect the full local verification plan without execution |
| `npm run verify:features -- --scope all --run` | Execute the fixed gate with isolated AAG state and a structured receipt |
| `node scripts/mcp/aag_mcp.mjs` | Run the read-only MCP companion over protocol-only stdio |

For Rust commands, run from `src-tauri/`:

```text
cargo check --locked --offline
cargo test --locked --offline --lib -- --test-threads=1
```

The serial flag is required for tests that use disposable process-wide
environment overrides. `ui/dist` must exist before Tauri `cargo check` or
`cargo build`, because the Tauri context reads the configured frontend output.

Use `npm run verify:features -- --scope all --run` for unattended local
verification. It detects prerequisites first, creates private disposable
configuration/home/temp directories, resolves macOS temporary-directory aliases,
and sets `TMPDIR`, `TMP` and `TEMP` for every child automatically. No manual
macOS environment setup is required for this runner. Each command has a deadline,
and failures stop execution with a structured receipt and unrun-command list.

For direct Node/Rust test commands, use dedicated private (0700 on Unix)
`AAG_CONFIG_DIR` and `AAG_HOME_DIR` directories. On macOS, also set
`TMPDIR=/private/tmp` (or another canonical temporary directory) so direct
private-storage tests do not traverse the system `/var` symbolic link.
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

Provider pages show the resolved client config path and source. Choosing or
reapplying a provider updates that path on Windows, macOS and Linux; launching
Claude Code, Codex CLI, Copilot CLI or a VS Code extension refreshes it before
starting the client. Unchanged content is left intact. Changed files are
backed up in AAG's private backup directory, and a failed multi-file update
restores any earlier file in the batch. The CLI's documented absolute home or
file overrides are used when present; the resolver never recursively searches
the disk or reads shell startup scripts. Native Claude/Codex login directories
remain separate from their AAG mirror directories.

## Verification gates

For a Node-only change, run Node typecheck and tests. For a UI-only change,
run the UI typecheck and build. For a cross-tree, release, or persistence
change, run the complete current gate:

```text
npm run typecheck
npm test
npm run test:desktop_runner
npm run ui:build
npm run ui:build_demo
npm run ui:smoke
npm run ui:smoke_demo
npm run release:verify
cd src-tauri
cargo check --locked --offline
cargo test --locked --offline --lib -- --test-threads=1
```

Record each command independently. Browser smoke uses synthetic IPC and owned
browser/server resources. It does not authorize real API requests or prove
native WebView2 behavior.

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
