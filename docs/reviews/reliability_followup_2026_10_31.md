# Provider reliability follow-up, 2026-10-31

## Source and delivery scope

The 2026-10-30 repair report used local `90565e7`, where the reviewed admission
implementation was absent. Upstream subsequently supplied `5a59879`, `d5a52b9`
and `8a80f3d`. The prior reliability checkpoint was committed as `652695e` and
rebased onto that private upstream as `464e8ce`; one Rust server conflict was
resolved by retaining both authenticated catalog refresh and runtime identity
tests. No published history was rewritten.

This follow-up closes AAG-02 and AAG-03 against that source. The unrelated QQ,
package, entrypoint and manual-workflow edits were preserved in a task-owned
stash, restored after integration and excluded from the reliability commits.
Only the established private GitHub origin is a delivery destination. No public
export, native installation, live provider request or live client write is part
of this work.

## Findings and resulting behavior

| Finding | Reproduction | Repair and decisive evidence |
| --- | --- | --- |
| AAG-02: failed recheck enables enforcement | Registered missing-model/source and provisional checks changed policy or imported unselected candidates. Native red: eight passed, two failed. | Preflight before mutation; Recheck is check-only. Enforcement and registration mutation flags are always false. Eligible provisional records await the separate Import action. Current admission suite includes 16 passing cases. |
| AAG-02: partial outcomes lose backend truth | Check/finish/reconcile/readback could fail after earlier writes. | Receipts identify selected outcome, own committed source/admission changes and version, safe errors and actual overview. Readback is independent of runtime success; unknown acknowledgement is not cancellation or automatic retry. API/domain and delayed UI regressions pass. |
| AAG-02: duplicate active checks | UI ownership alone could not prevent an acknowledged/unknown duplicate backend command. | Tauri-managed process-local coordinator retains a per-provider permit through check, reconciliation and readback. Same-provider duplicate returns busy; B can proceed while A is pending; completion/drop permits a later check. Persistent CAS remains the cross-process fence. No exactly-once upstream guarantee is claimed. |
| AAG-02: UI callback still maintained all imports | Recheck's parent refresh invoked `supplier_import_maintain`. | The panel now uses a stable read-only refresh branch. Browser checks assert zero maintenance calls for preflight, success, partial failure and lost acknowledgement. Normal explicit maintenance remains separate. |
| AAG-02: runtime rebuild still refreshed B | Generic reload renewed/revoked stale unselected B while applying A. The real live-seam red failed on changed B bytes. | Persisted-admission-only reload shares generation/publication/failure handling without subscription refresh. B source/admission bytes stay unchanged; A offers apply; expired B is excluded without renewal; prior snapshot and no-op retry are preserved. |
| AAG-03: coordinate twin borrows admission | Higher-weight or disabled/unscreened equal-coordinate rows changed the selected supplier's result. | Resolve the selected endpoint's exact `proxy_provider_id`; compare its unchanged connection. Under enforcement changed/unbound drafts fail before I/O. Supplier discovery red: ten passed, five failed; final suite 17 passed. |
| AAG-03: wrong catalog credential header | Bearer-only Anthropic fixture returned 401 during catalog fetch. | Explicit validated scheme flows through supplier, manual and background paths. Exactly Bearer or API-key auth is sent, protected against inverse extra-header injection. Inference and catalog fixtures exercise both schemes. |
| AAG-03: stale source survives policy-off/retained runtime | Changed credential-field identity with the same bytes still reached catalog or old transport. | Validate known bindings before protocol defaults. Identity derives from the exact source/connection independently of observations and grants no eligibility. New attempts compare captured/current identity before the policy-off shortcut; manual unbound behavior and admitted streams remain intact. |

Manual provider Test now explains its configured-model and verified-source
prerequisite before execution. It does not invent evidence when enforcement is
off. Catalog discovery is a separate metadata operation, not inference or pool
eligibility proof. Changed panel actions/notices support English and Chinese;
dirty thresholds survive outcomes and block navigation until saved or locally
discarded.

## Integration corrections retained as evidence

The full gate exposed an old import fixture that bypassed `screen_import` and
called low-level apply directly. It now follows the actual explicit-source
production sequence: screening, source binding, apply, Bearer-only discovery,
idempotent repeat and same-byte credential-field-change rejection. It asserts
one local probe and one catalog request, retained path/ref/scheme/tested model,
and no further request after the change. Production fail-closed validation was
not relaxed.

The refusal fixture initially observed an actual HTTP 502 through an inherited
proxy. The classifier correctly returned ProviderError. A test-local loopback
proxy-bypass guard now isolates the refused endpoint and restores the prior
environment; the owning suite also requires actual HTTP 502 to remain a
ProviderError. Production proxy/classification policy is unchanged.

The merged legacy prompt error fallback also conflicted with the upstream
recovery test. Unknown strings now remain unknown without invented retry
authority; known admission messages and the structured public-error contract
remain intact. Its original failing assertion and seven passing follow-up
cases are retained in the verification record.

## Verification

Commands ran with installed locked dependencies and disposable fixture state.
No assertions, ignored-test meaning or qualification exclusions were weakened.

| Command | Result and boundary |
| --- | --- |
| `npm run typecheck` and `npm run typecheck --prefix ui` | Passed before checkpoint; affected UI typecheck passed after enhancement. Final builds also typechecked the complete UI. |
| Focused adapter/token/CLI/runtime/route/readiness Vitest batch | 95 passed before checkpoint. |
| `node node_modules/vitest/vitest.mjs run tests/provider_screening_ui.test.ts tests/provider_recheck_receipt.test.ts tests/supplier_ui.test.ts tests/i18n_key_parity.test.ts tests/gateway_readiness.test.ts --pool=threads --poolOptions.threads.singleThread` | 55 passed. |
| `node node_modules/vitest/vitest.mjs run tests/prompt_recovery.test.ts tests/error_contract_adversarial.test.ts --pool=threads --poolOptions.threads.singleThread` | Seven passed after the merge correction. The unrelated adversarial file stays outside these commits. |
| `cargo test --locked --offline --lib -- services::provider_admission::tests:: services::model_discovery:: services::subscription_source:: services::proxy_runtime::snapshot:: services::proxy_runtime::live:: services::suppliers::discovery::tests:: --test-threads=1` | 110 passed, zero failed, one ignored. Includes coordinator, identity, auth, retained-runtime and persisted-only reconciliation. |
| `cargo test --locked --offline --lib -- imported_nonstandard_endpoint_discovers_models_and_remains_idempotent services::import_cc_switch::screening_tests:: services::suppliers::discovery::tests:: --test-threads=1` | 24 passed after correcting the import fixture. |
| `cargo test --locked --offline --lib services::provider_screening::tests:: -- --test-threads=1` | 15 passed after fixture isolation; literal-ok, token/latency/deadline, evidence, HTTP status and privacy assertions retained. |
| `node scripts/ui_smoke/provider_recheck_checks.mjs` | Six delayed synthetic interactions passed, including zero hidden maintenance, truthful partial/unknown outcomes and navigation/discard. |
| `node scripts/ui_smoke/provider_interaction_checks.mjs` | Passed with the prerequisite-before-Test check and existing responsive/keyboard checks. |
| `npm run ui:build` | Final native frontend build passed. |
| `npm run ui:smoke` | 203/203 passed; owned runtime closed/profile removed. Synthetic IPC, not native acceptance. |
| `npm run ui:smoke_demo` | Final demo build and 203/203 passed; owned runtime closed/profile removed. |
| `cargo check --manifest-path src-tauri/Cargo.toml --locked --offline --all-targets` | Passed, including production, examples/operators and test targets. Existing warnings retained. |
| `npm run test:desktop_runner` | 48 passed; fixture/runner evidence only. |
| `npm run test:privacy` | 33 passed. |
| `npm run release:verify` | Release-input preflight and current-tree privacy audit passed with zero blockers or warnings. No export/release was performed. |
| `git diff --cached --check` | Passed for the explicit enhancement-only staging set. |
| `npm test` | Nonzero: 577 passed, two failed, two skipped; three files fail. The remaining failures are Windows owner-mismatch checks/setup in config permissions, temporary-race and e2e fixtures. Includes retained uncommitted test files in this working tree. |
| `npm run verify:rust -- --profile ordinary --run` with child-process `NO_PROXY`/`no_proxy=127.0.0.1,localhost,::1` | Nonzero: 1,665 passed, 49 failed, 40 ignored; exactly 26 existing qualification IDs excluded. Loopback bypass confines fixture traffic to its local endpoint and does not change application/global proxy settings. |

The initial integrated ordinary run recorded 1,662 passed and 52 failed. The
import fixture, refusal fixture and loopback proxy interference were examined
and corrected/isolated before the final result. Remaining failures include
Windows private-storage ownership and dependent client/backup fixtures plus
process-helper execution/cleanup. They are not relabeled as passed. Full Rust
and Windows qualification, actual installed-client switching, native lifecycle
and live-provider acceptance remain unqualified.

Independent native Sol review found and then closed the two hidden Recheck
write-set leaks. That source review is separate from the executed tests above.
The user chose continuation without Local CLI; its unsubmitted preparation
session was stopped and the receipt revoked. No Pro request was submitted.

## Recovery and next acceptance

Recheck has no registration rollback because it performs no registration. A
partial receipt may retain selected source/admission data while runtime apply
is pending. Refresh reads actual state and never asserts cancellation. Import
and global threshold changes keep their separately labeled scope.

Changed or unavailable source bindings fail closed; repair/recheck the exact
source instead of borrowing another connection. Catalog additions retain the
original tested model and leave per-model inference status unverified.

For native acceptance use a current GUI-subsystem build, no competing AAG
instance, and the documented non-elevated/current-owner environment. Do not
change privileges or relax storage/qualification gates to obtain green results.
Live requests/client writes require their existing scoped authority. The next
unqualified work is that Windows/native acceptance boundary, not another
automatic provider probe or public release.
