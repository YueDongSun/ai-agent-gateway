# User-focused reliability repairs, 2026-10-30

This report covers the local working-tree repairs requested as AAG-01 through
AAG-09. It distinguishes domain, HTTP/service, synthetic browser and native/live
evidence. It does not certify the whole application or authorize publication.

## Source and ownership

The actual starting revision is `90565e79b860a01bb5599bfcc2b2ee89ae207ce1`
on `master`. The requested review revision
`8a80f3d6ad6a90b4936551b2316ebadfbd3eeb80` is absent from the local object
store (`git cat-file -t` exited 128). No fetch, reset, branch, commit or push was
performed. The final candidate remains an uncommitted diff on the starting HEAD.

Pre-existing changes included the desktop-build workflow, package files, Node
canonical/router/server/index code, prompt page/API/translations and untracked
QQ/error-contract work. Those changes were retained. Runtime-identity fixtures
in the existing QQ dispatcher test were adapted to the corrected producer
contract; QQ production code was not changed by this repair.

Node JSON configuration and desktop SQLite remain independent. Rust owns IPC,
database and native proxy state. React owns drafts and operation lifetime; typed
IPC is its persistence boundary. Browser evidence uses synthetic IPC. Adapter
tests use disposable client paths and local upstream fixtures rather than live
client homes or paid model calls.

## Finding ledger

All rows refer to the starting revision plus the current repair diff.

| ID | User action and expected behavior | Observed cause and owning code | Disposition and evidence |
| --- | --- | --- | --- |
| AAG-01 | Switch Copilot between gateway/direct without losing unrelated JSONC/environment settings; malformed content must remain unchanged. | `src/agents/adapters.ts` merged the patch before reading the old nested environment and caught invalid input as an empty object. Regex comments damaged strings. | Fixed and verified: lexical JSONC reader, owned-key merge, fail-closed unsupported data, exact backups and atomic replacement. Seven initial regression assertions failed; final adapter suite passes 24 cases. Adjacent Kilo shares the corrected JSONC reader. |
| AAG-02 | Recheck one provider without changing global enforcement or importing other providers. | The named admission, subscription-source, screening command/UI and `recheck_existing` subsystem do not exist in this checkout. Current manual testing follows a different direct-provider contract. | Not reproduced against this tree; blocked from validating the reviewed implementation by the unavailable source revision. No replacement admission policy or subscription evidence was invented. |
| AAG-03 | Discover through the selected saved supplier/source/auth identity. | Current `services/suppliers/discovery.rs` resolves the selected supplier ID and protocol directly; there is no coordinate-based admission lookup. The current provider schema has no source binding or credential-scheme field. Static Anthropic inference/catalog paths both use API-key auth; inbound passthrough is excluded from discovery. | Selected-ID defect not reproduced. Source-bound Bearer acceptance is blocked by the unavailable reviewed implementation. Existing auth/discovery tests remain the contract; there is no claim that Bearer-only source-bound discovery was repaired. |
| AAG-04 | Save one protocol's models while retaining other edits; old completions must not close a newer dialog. | Scoped save reset the complete baseline and closed the editor; supplier/import callbacks lacked origin ownership and Close remained active. | Fixed and verified by synthetic interactions: scoped save, later full save, setup failure after persistence, synchronous duplicate guards and dialog ownership. Existing accessible-dialog behavior includes nested alert dialogs. |
| AAG-05 | Save/Discard/Rename routes without losing newer work or hiding partial commits. | Controls stayed mutable during async reconciliation; `Promise.all` returned before other writes settled; new-route Rename targeted a missing database row. IPC can reject after DAO commit. | Fixed and verified: complete mutation locks, `allSettled` plus selected-alias readback, exact retry baselines, unknown-outcome fencing and staged new-route rename. Five domain regressions and synthetic partial-write/apply-failure/retry/Discard interactions pass. |
| AAG-06 | Keep expansion/optimization attached to one task and retry only the failed stage. | History could replace active state; output stayed editable; generic Retry repeated expansion; navigation lost settings or damaged history. | Fixed and verified through delayed synthetic IPC: one synchronous task owner, locked conflicts, captured English retry inputs/context, invalidated stale Retry, local session draft/instruction/error recovery, dirty-settings guard and preserved Back/Forward/fragment history. No backend cancellation is claimed. |
| AAG-07 | Recover token counting after terminal errors, deadlines or caller cancellation. | Half-open permits leaked; deadline did not cover dispatch/body; abort identity was wrapped; error bodies were abandoned. | Fixed and verified: 25 token-count cases, including 400/401/404 recovery, header/body cancellation, silent deadlines, 401/503 unread-body cleanup and malformed counts; 63 adjacent cases pass. Initial reproductions failed before each repair. |
| AAG-08 | CLI test must preserve arbitrary UTF-8/SSE chunks and report failure honestly. | Per-chunk parsing dropped fragments; EOF/error frames could print success and leave exit status zero. Usage-only/malformed choices were additional review findings. | Fixed and verified using local HTTP fixtures: strict incremental UTF-8, bounded events, successful terminal choice plus DONE, partial-output retention, error/truncation/cancellation/nonzero outcomes. CLI stream suite passes 19 cases. |
| AAG-09 | Identify runtime and distinguish listener/configuration readiness from actual upstream success. | Generic discovery mislabeled Node as Desktop; safe reload events were redacted; header inferred readiness from listener/open-breaker state alone. A Codex mode apply could leave unknown status displayed after the write succeeded. | Runtime markers, safe enum reload metadata, effective/pending-restart state and configured candidate catalogs implemented. Bare discovery remains exactly four fields; runtime kind is opt-in. Unknown Codex mode status is recovered by backend readback, including a transient first-read failure; known-status stale-result handling is preserved. Node/domain and relevant Rust identity/readiness/discovery cases pass. Broader Rust evidence remains failing; see verification below. |

## Mutation and recovery contracts

| Action | Target and write scope | Owner/completion | Recovery |
| --- | --- | --- | --- |
| Supplier Save models | Selected supplier generation and one protocol model inventory | Originating editor plus synchronous operation ref; baseline advances only for that inventory | Other dirty values remain. Saved data and later setup failure are separate. |
| Supplier full Save | Complete current form, preserving backend generation/conflict checks | Originating editor identity; persisted receipt supplies ID/generation | Adopt persisted identity/clear only saved secret intent before setup retry; avoid duplicate creation. |
| Import Apply | The reviewed import plan | Originating import dialog; Close/Escape blocked during mutation | Report backend conflict/error, refresh committed state; dismissal is not cancellation. |
| Route Save | Submitted changed/deleted aliases only; each DAO mutation atomic | Synchronous route operation lock; every command and selected readback settle | Acknowledge exact observed effects, retain failed drafts, reconcile unknown outcomes before another write. Unrelated rows never enter deletion scope. |
| Route Discard | Reload persisted route baseline; no route writes | Same route lock | A failed reload keeps the draft and can be retried. |
| Route Rename | Existing row immediately; new row only in draft | Same lock plus exact readback on rejected IPC | Reject conflicts; reconcile uncertain target/source before further writes. Discard removes staged new aliases. |
| Prompt generation/Retry | One stage/task with captured text, target selection and workspace context | Synchronous task lock plus mounted sequence guard | Retain partial work; English Retry does not expand again. Editing/history/non-generation actions invalidate stale Retry ownership. |
| Codex mirror mode | The selected isolated Codex mirror, through its existing backend command | Existing request-ID protection; unknown-status recovery has a synchronous submission lock | Recover actual status after apply or rejected-command readback, preserve newer status fields, clear stale status errors and retain any separate operation error. No live mirror write was used as evidence. |
| Navigation | Current destination/draft, including browser history | Shared cancelable transition guard and indexed history restoration | Save/reset/discard before leaving dirty settings; wait for active operations. Prompt text recovers in tab-local session storage. No frontend action is labeled backend cancellation. |

Node switching retains its exact rolling backup and same-directory atomic
replacement. Invalid/unsupported existing JSONC returns failure before backup or
write. Restore uses the retained pre-write bytes. No live client settings were
modified to verify this behavior.

## Verification ledger

Verification used installed Node 24.18.0, Vitest 2.1.9 and the repository's locked
dependencies. Default-sandbox dependency resolution sometimes failed with EPERM;
reviewed scoped execution was used for the same fixture commands. Assertions,
qualification exclusions and filesystem protections were retained.

| Command | Observed result |
| --- | --- |
| `npm run typecheck` | Pass on final candidate. |
| `npm run typecheck --prefix ui` | Pass on final candidate. |
| `node node_modules/vitest/vitest.mjs run tests/adapters.test.ts tests/cli_stream_reliability.test.ts tests/node_runtime_reliability.test.ts tests/gateway_config_reload.test.ts tests/util_log.test.ts tests/sse_lines.test.ts tests/gateway_cli_port.test.ts --pool=threads --poolOptions.threads.singleThread` | Initial integrated focused run: 69 passed; later CLI strictness/identity fixture follow-ups were run separately below. |
| `node node_modules/vitest/vitest.mjs run tests/cli_stream_reliability.test.ts tests/config_desktop_probe.test.ts tests/sse_lines.test.ts tests/router_stream_parity.test.ts tests/privacy_errors.test.ts --pool=threads --poolOptions.threads.singleThread` | 80 passed after review repairs. |
| `node node_modules/vitest/vitest.mjs run tests/qq_bridge_dispatcher.test.ts tests/config_desktop_probe.test.ts tests/cli_stream_reliability.test.ts --pool=threads --poolOptions.threads.singleThread` | 31 passed after adapting only the necessary synthetic consumer fixture. |
| `node node_modules/vitest/vitest.mjs run tests/gateway_count_tokens.test.ts tests/circuit_breaker_admission.test.ts tests/proxy_provider_timeout.test.ts tests/gateway_complete_abort.test.ts tests/router_model_marker.test.ts tests/anthropic_envelope.test.ts` | 63 passed after unread-body cleanup repair. |
| `node node_modules/vitest/vitest.mjs run tests/route_persistence_reliability.test.ts tests/supplier_ui.test.ts tests/route_rename_conflict.test.ts` | 46 passed: five receipt cases, 39 supplier cases, two conflict cases. |
| `node node_modules/vitest/vitest.mjs run tests/gateway_readiness.test.ts` | Four domain cases passed: dead declared alias, pending restart/reload failure, legacy uncertainty and Node configured candidates. |
| `npm test` | Final run nonzero: 566 passed, two failed, two skipped; 64 files passed and three failed. `config_permissions` has two Windows owner-mismatch failures; `config_temp_race` and `e2e` fail during setup with the same owner-mismatch code. New identity/QQ failures in the first broad run were repaired and are not hidden. |
| `npm run ui:build` | Final native frontend build passed. |
| `npm run ui:build_demo` | Final demo frontend build passed. |
| `node scripts/ui_smoke/supplier_reliability_checks.mjs` | Final 12/12 passed, including unknown mirror status, one mutation/two recovery reads after transient readback failure, retained operation error, zero uncaught errors and successful owned runtime/profile cleanup. Earlier 11/11 and normal-readback 12/12 receipts remain separate. |
| `node scripts/ui_smoke/prompt_reliability_checks.mjs` | Three checks passed: delayed task/history/fragment ownership, exact failed-stage retry plus settings recovery, zero uncaught errors. Owned cleanup succeeded. |
| `npm run ui:smoke` | 197/197 passed, zero uncaught errors; owned runtime closed and profile removed. |
| `npm run ui:smoke_demo` | Final demo build plus 197/197 passed, zero uncaught errors; owned runtime closed and profile removed. |
| `npm run test:desktop_runner` | 48 passed. Runner-unit evidence, not native lifecycle. |
| `npm run test:rust_acceptance` | 40 passed. Runner-unit evidence, not Rust application acceptance. |
| `npm run test:ui_smoke_runner` | Nonzero: 18 passed, two failed, one skipped. Windows taskkill/PowerShell profile-cleanup fixture commands time out. Graceful real-browser closure does not relabel these fallback tests as passing. |
| `cargo metadata --format-version 1 --no-deps --offline` | Passed; build-script target exists. |
| `cargo test --manifest-path src-tauri/Cargo.toml --locked --offline --lib runtime_identity -- --test-threads=1` | One new runtime-identity fixture passed. Earlier restricted execution failed before tests with missing OUT_DIR; later actual compiler failure from a missing import was fixed. |
| `npm run verify:rust -- --profile ordinary --run` | Both completed runs: 1,570 passed, 49 failed, 40 ignored, exactly 26 qualification IDs excluded. New runtime identity, empty-catalog readiness, discovery privacy/compatibility and existing supplier/auth discovery regressions pass within that failing run. This is not full Rust qualification. |
| `cargo check --manifest-path src-tauri/Cargo.toml --locked --offline` | Passed after final native frontend build. Existing warnings remain; no broad formatting or warning cleanup was applied. |
| `npm run test:privacy` | 33 passed. |
| `npm run release:verify` | Passed: release-input preflight plus current-tree privacy audit, zero blockers/warnings. No export or release was performed. |

Initial supplier browser attempts retained their failures: sandbox startup stall,
Windows process-cleanup timeouts, missing test synchronization and a CDP DOM
object serialization error. Those harness issues were corrected without dropping
the behavioral assertions. A first prompt-history assertion incorrectly required
the fragment form of the initial Suppliers URL; the final assertion accepts the
actual unfragmented initial route and still checks restored page/history state.

The independent native Sol source review raised five actionable findings:
unread token-count bodies, completion-free CLI streams, incorrect prompt retry,
lost idle prompt settings and damaged blocked history. All five were repaired and
closed on the final source reread. Its additional unknown-mode readback finding
was also repaired, source-closed and exercised by the final browser fixture.
Native Chrome review initialization timed out;
reconciliation found the created tab but its debugger was unattached. No Pro
payload or review round was submitted (0/10); no Pro PASS is claimed. Claude's
visible Workbench supplied read-only test-coverage advice. OpenCode readiness
timed out, so current-document updates used the disclosed native fallback.

The 49 ordinary Rust failures are retained in their original categories:
Windows private-storage owner checks and their provider/configuration fixtures,
Codex stale-auth healing, process-helper execution/cleanup and one idle HTTP
connection reuse assertion (two connections observed, one expected). These are
observations, not a claim that every failure is caused by the same environment
condition. No qualification IDs were added to the exclusion list. The synthetic
mode recovery follow-up also retained its initial broad-lock regression against
the existing stale-outcome test before narrowing the repair to unknown status.

## Remaining acceptance and manual checklist

The unavailable reviewed revision is required to validate AAG-02 and the
source-bound Bearer portion of AAG-03. Supply that implementation in an authorized
checkout before claiming those workflows. No subscription eligibility is inferred
from this older/different source tree.

Full Rust, Windows qualification, actual installed client switching/restoration,
native lifecycle, macOS/Linux native execution and live provider requests were not
qualified by this repair. The ordinary Windows failures and storage-owner setup
failures require an execution environment that satisfies the existing ownership
and qualification contracts; privileges and assertions were not changed. The
cross-feature real-client request/failure/restart journey remains blocked by those
prerequisites and explicit live-call/client-write authority.

1. Check the header/CLI runtime identity. Treat unidentified older instances as
   unknown and configured candidates as untested upstreams.
2. Open a supplier, edit general settings and two protocol model inventories.
   Save one inventory; confirm the editor and other dirty values remain.
3. Save the form. If setup failed after save, retain the reported saved state and
   retry setup rather than adding the supplier again.
4. Add/rename a draft route, then Discard. Repeat and Save. Existing-row Rename
   applies immediately; wait for any pending operation before switching clients.
5. On partial route failure, inspect the persisted/failed summary and retry Save.
   An unknown readback must reconcile before any further writes.
6. Expand a brief, edit it and optimize English. During generation, history,
   output edits and navigation remain locked. Retry an English failure without
   re-expanding; partial output remains recoverable.
7. Use isolated fixtures to verify fallback/recovery and process restart. Run
   native/live acceptance only when its storage, ownership and authorization
   prerequisites are met; browser demo results do not satisfy that step.

## Follow-up source availability, 2026-10-31

The reviewed `8a80f3d` implementation subsequently became available through the
private upstream and was integrated with the reliability checkpoint. The
source-missing dispositions above remain the historical 2026-10-30 observation.
The completed AAG-02/AAG-03 repairs, new integration evidence and remaining
Windows/native limits are recorded in the separate
[2026-10-31 follow-up](reliability_followup_2026_10_31.md).
