# Windows mirror recovery and unified provider acceptance

Date: 2026-10-24. Scope: local private development integration and the explicitly
requested provider reset. No public export, push, release, or external model
generation was performed.

## Source integration

The integration combines `master` at `aa62b772` with the unified supplier branch
at `9a229083`. The latter contains 15 commits absent from the original master.
Pre-existing launcher edits were backed up and reapplied before resolving the
merge. Lifecycle safeguards, strict private-storage validation, and the supplier
branch's hot-reload behavior were retained. The delivery commit has both source
tips as parents; the branch name remains `master`.

## Desktop mirrors

The Windows launch chain now selects the exact Store application package and
its real executable. Codex uses the installed `OpenAI.Codex` application's
`ChatGPT.exe`; a package companion or non-existent `Codex.exe` is not accepted.
Claude uses the current Store executable, with a verified local-install fallback.

A standalone process reproduction identified a second failure: constructing
`cmd.exe` with mixed Windows path separators caused command dispatch to fail
with exit code 1. Normalizing absolute executable paths to native components
fixed the reproduction. The launcher uses the same boundary and reaps its
detached dispatch process. Relative and simulated non-Windows paths retain
their original spelling.

This host's token default owner differs from its user SID. Newly created private
files and directories therefore need an explicit user owner and private DACL at
creation. The corrected Windows creation helpers provide that security descriptor
atomically, retain collision/reparse/ownership rejection for existing objects,
and clean up only matching task-owned identities. SQLite's absent sidecars use
the same creation boundary. No existing foreign-owned profile was adopted.
A fresh AAG-managed Codex home was selected, with the old setting retained for
recovery.

Claude Desktop requires more than the CLI environment variables. Inspection of
the installed application established its supported third-party gateway profile
and process-local data-root contract. AAG now precreates the isolated
`desktop_local/Claude-3p` profile, supplies the local gateway bearer, and launches
with that profile. The writer rejects an empty bearer or nonloopback client URL
before writing. The gateway authenticates requests, resolves the desktop role
to a viable static route in the frozen snapshot, and uses the existing Anthropic
message, stream and token-count handlers. It never forwards the AAG token to an
upstream or falls back to Claude OAuth.

Actual acceptance used the ignored, explicitly enabled
`desktop_mirror_launch_real_twice` test against the live AAG configuration:

| Observation | Result |
| --- | --- |
| Codex actual launches | 2 of 2 passed |
| Claude actual launches | 2 of 2 passed |
| Both mirror windows | Nonzero main-window handles and responding processes |
| Claude renderer mode | Third-party true; first-party false |
| Mirror endpoint and local credential | Matched AAG configuration; values omitted |
| Focused launcher suite | 38 passed; 1 native acceptance test ignored by default |

Native process/window observations establish launch and profile selection.
They do not establish a successful answer from a real upstream: the provider
inventory was deliberately empty for the owner's next manual test.

## Provider reset and add flow

The explicit reset created and checked a complete private SQLite recovery
snapshot before a transaction removed 64 client providers, 18 proxy providers,
11 routes, 16 suppliers, 16 endpoints, 23 offered models and 16 projections.
Provider endpoint and vendor tables were already empty. The transaction also
disabled provider seeding and automatic cc-switch import. Unrelated application
settings, native accounts, usage data and model catalog data were retained.
The exact local recovery location is recorded in the delivery message.

After restart, read-only queries found zero rows in all seven active provider
and route tables. The real local gateway returned HTTP 200 for `/health` and
an empty HTTP 200 model catalog for authenticated `/claude-desktop/v1/models`;
an invalid bearer returned HTTP 401. These checks did not expose credentials or
generate model responses.

The provider workspace is the initial page. Its main action opens a compact
editor for name, API protocol, base URL and API key. Supported protocols are
Anthropic, OpenAI Chat Completions, OpenAI Responses and Gemini, with multiple
endpoints available through advanced options. Fetch models works on the draft
before saving and presents selectable results. Discovery supports shared or
per-endpoint credentials, stored-key preservation on edit, protocol-specific
authentication and full endpoint URL normalization. Empty catalogs and failures
remain visible; providers without a catalog can use manual model entry.

Saving selected models selects an enabled default, starts or refreshes the local
gateway, applies routing, and updates Codex and Claude mirror configuration.
It also prepares managed gateway records for ten AAG client targets, whose
existing adapters apply configuration at launch. These records cannot become
upstream loops and contain no supplier credentials. Delete and model-update
mutations use the same reconciliation. Persisted changes with setup failures
are reported as partial success to avoid duplicate creation on retry.

The browser demonstration used synthetic IPC and a placeholder provider URL.
It visibly exercised protocol/URL input, model fetching, selection, and the
fixed visible Save footer. An isolated backend test separately saved one supplier,
started a real loopback gateway and routed a request to a mock upstream. Another
test changed and deleted selected models and verified default/client reconciliation.

## Verification and limits

Commands were run separately. Node used the installed Node 24.18.0 runtime and
the repository lockfiles; Rust used rustc 1.97.0 with locked offline
dependencies. Rust tests ran serially. The final full Rust run used disposable
`AAG_CONFIG_DIR` and `AAG_HOME_DIR` with the optional live cc-switch test override
unset. Earlier diagnostic runs and their failures remain in local logs.

| Check | Result |
| --- | --- |
| `npm run typecheck` | Passed |
| `npm test` | 334 passed; 2 failed; 2 skipped; 2 failed suite initializations (3 failed files total) |
| `npm run test:desktop_runner` | 44 passed on the normal host; an earlier restricted run's signal failures were retained |
| Focused supplier UI tests | 37 passed |
| `npm run ui:build` | Passed |
| `npm run ui:build_demo` | Passed |
| `npm run ui:smoke` | 161 passed |
| `npm run ui:smoke_demo` | 161 passed |
| `npm run test:privacy` | 32 passed |
| `npm run privacy:audit` | 608 files scanned; zero blockers, warnings or findings |
| `npm run release:verify` | Passed with zero blockers and warnings |
| `cargo check --locked --offline` | Passed; three dead-code warnings |
| Full serial Rust library tests | 977 passed; 51 failed; 14 ignored |

The Node failures report Windows owner mismatch in private-storage fixtures or
suite initialization. Most remaining Rust failures similarly involve ordinary
filesystem-created fixtures and workflow-process temporary storage; the HTTP
connection-reuse assertion also failed. These results are not a passing full
gate, and this round does not establish that every remaining failure predates
the merge. Focused coverage for the added discovery, reset, setup, gateway,
private-creation and launch behavior passed in the final run.

Browser checks establish the synthetic UI contract, while native mirror launch,
real local gateway checks and isolated mock-upstream integration are separate
evidence. Installation, signing, upgrade, paid/live upstream inference, and the
previously paused real-generation canary remain outside this acceptance. The
prior canary remains at 11 of 16 attempts; this task consumed no new attempt.

## Design references

The requested research used the GitHub integration to inspect
[Magpie](https://github.com/yetone/magpie) and
[cc-switch](https://github.com/farion1231/cc-switch). They informed the unified
provider workspace and gateway-backed desktop behavior. Actual installed
application behavior and the repository's own source were the authority for
the local launch implementation.
