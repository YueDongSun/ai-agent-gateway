# Desktop mirror model loading follow-up

This follow-up starts from private `master` at `f27ad619`. It corrects gaps
that the earlier window-launch checks did not cover: model catalogue adoption,
effective client configuration, and a real request through the installed client.
No commit, push, public export, or release was requested for this follow-up.

## Provider configuration

The owner explicitly requested importing existing cc-switch entries into AAG.
The source was read-only. The importer inspected 54 entries, imported 24 API-key
configurations, retained seven account-based entries as disabled pending
associations, and reported 23 unsupported or problematic source entries. A
complete private AAG database backup was created before the import.

Model discovery succeeded at seven imported endpoints. Failed or unavailable
catalogues did not erase the existing configured model lists. The resulting
gateway inventory contains 42 distinct model IDs. A successful catalogue fetch
does not prove that every model is a usable chat model or that every imported
credential is currently valid. Authentication, transport, and unsupported
catalogue failures remain visible in the local acceptance evidence.

The initial model is `MiniMax-M3.1-Flash`. Its real Claude-compatible gateway
request returned HTTP 200 and exactly `AAG_OK`. The installed Codex app-server
also completed a real ephemeral turn through AAG and returned exactly `AAG_OK`.
No user documents were supplied to the diagnostic prompt; tool requests were
declined by the probe. The earlier registered multi-provider canary was not
resumed or presented as a completed benchmark.

## Corrected contracts

- Codex's generated catalogue is now connected through `model_catalog_json`.
  It uses the installed client's supported schema, preserves exact model IDs,
  and does not invent official-model availability. The selected valid model
  has catalogue priority zero so the picker and configuration agree.
- Final Codex configuration is filtered by the admitted runtime's credentialless
  Codex routes and pin. An empty usable inventory blocks launch, because the
  installed client otherwise silently falls back to its bundled model list.
- Claude Desktop enables discovery and removes the old three-model administrator
  whitelist. Each eligible model receives a stable compatible wire selector and
  an honest visible model name. Requests resolve against the admitted snapshot;
  stale selectors fail rather than retargeting another model. Existing role
  selectors continue through the admitted default or first eligible model.
- Supplier import now runs the same gateway/client reconciliation as supplier
  mutations and reports persisted-import versus setup failure separately.
  Explicit desktop open prepares the listener and current mirror configuration.
- Windows mirror files rewritten by the native clients can carry the current
  token's Administrators default owner. A separate mirror-only writer admits
  that case only inside a strictly private current-user-owned parent with
  current-user-only allow entries. Generic private-file checks remain strict.
  Caller-bound snapshots supply both transformed content and write preconditions.
  Handle-based replacement preserves a competing client file and retains recovery
  data if restoration cannot complete; it never changes an existing object's owner.
- Reload checks identify the exact executable, profile and process creation time.
  Duplicate profile mains are refused. A stale mirror is closed gracefully and
  rechecked before relaunch. If it remains alive, AAG reports that the mirror must
  be quit instead of force-killing active work. The historical stale Claude mirror
  was restarted explicitly during this repair, preserving its profile files.
  Codex additionally binds a private reload receipt to its effective managed
  configuration and exact process identity. Native preference writes alone no
  longer trigger a false restart warning on the second open. The receipt's
  configuration identifier is consumed by this reload decision, not an audit
  checksum or a credential copy.
- Codex-owned bundled `.system` skills are excluded from personalization seeding.
  Custom skills and agents retain their existing synchronization and backup path.
- Complete inline message and function-item IDs are treated as client metadata.
  Real previous-response, explicit item-reference, encrypted, and incomplete
  state still requires its original source binding. This fixes first-turn and
  tool-result rejection without weakening opaque continuation ownership.
- The Responses `include` request for optional encrypted reasoning output is
  distinguished from actual encrypted input. Request-local namespace mappings
  translate function tools, forced choice, and history into flat upstream tools,
  then restore namespace fields in synthesized responses and SSE events. Native
  Responses envelopes and source events remain unchanged.

The managed translated Codex profile uses ordinary function tools and retains
MCP function tools through the namespace adapter. Dedicated freeform apply-patch,
namespace-based multi-agent tools, and native hosted web search are disabled in
this compatibility profile; shell-based editing remains available. These settings
apply to the AAG mirror, not the native Codex account.

## Verification boundaries

The installed Codex package changed during the work. Final app-server validation
uses version `26.1021.3705.0`, not the earlier `26.1016.8622.0`. It reported the
AAG provider, loopback gateway, 42 catalogue entries, the correct picker default,
no required OpenAI login, and a completed real response. Probe children were reaped.
Claude Desktop version `3.6556.2.0` passed two fresh opens and had a responsive
native window after its stale instance was restarted.
The final Codex Desktop service check also passed two actual opens with AAG's
configuration after the bundled-skill and repeated-open fixes.

Native GUI inspection was attempted through Computer Use, but its app-approval
request timed out before window contents were inspected. No screenshot or manual
GUI chat-interaction success is claimed. Installed-core execution, native launch,
gateway response, and synthetic browser checks are separate evidence.

| Check | Result |
| --- | --- |
| Node typecheck | Passed |
| Focused supplier UI tests | 37 passed |
| Desktop runner tests | 44 passed |
| Native frontend build | Passed |
| Browser demo build | Passed |
| Source and built-demo browser smoke | 161 of 161 each |
| Privacy regression suite | 32 passed |
| Rust production check | Passed |
| Final focused launcher suite | 55 passed; 1 native acceptance test ignored by default |
| Namespace conversion regressions | 5 passed |
| Routing/continuation runtime regressions | 18 passed before the additional tool-turn cases; the final full run includes those cases |
| Full Rust library binary after namespace integration | 1027 passed, 51 failed, 18 ignored |
| Full Node suite | 334 passed, 2 failed, 2 skipped; two suite initializations failed |

The 51 Rust failure names match the preceding recorded baseline exactly. They
include Windows ownership/temporary-fixture failures and an HTTP connection-reuse
assertion. The Node failures retain the recorded Windows ownership causes. The
full gate is therefore not green; focused success and real requests do not
override these failures. The final bundled-skill exclusion has its own focused
regression and native launch verification in the delivery evidence.

## Later role-routing and continuous-sync acceptance

The owner's later clarification makes the client-facing family names explicit
routing roles over existing supplier models. The current local opt-in role
catalogues therefore supersede the earlier 42-exact-choice mirror presentation;
the underlying 42-model supplier inventory remains available to gateway routing.
All seven canonical desktop roles currently share a 17-hop configured queue,
beginning with the selected `MiniMax-M3.1-Flash` offers. Backups have configuration
evidence; a successful request to the primary does not certify every backup.

The generated Codex catalogue contains exactly `gpt-5.6-sol`,
`gpt-5.6-terra`, and `gpt-5.6-luna`, marked as AAG routing choices. Installed
core `26.1021.3705.0` read the actual mirror configuration, selected Sol through
provider `aag`, required no native OpenAI login, and completed a real ephemeral
turn with `AAG_OK`. Claude Desktop `3.6556.2.0` visibly displayed Fable 5,
Opus 5, Sonnet 5, and Haiku 5.5, with Fable selected; the authenticated Fable
gateway smoke also returned `AAG_OK`. Both launchers passed two actual opens.
An old task-created Claude mirror process remained after its window closed;
its exact executable, profile, creation identity, and absence of an active window
were verified before a one-time restart. Profile files were preserved.
The separate native Claude instance remained running. Codex GUI inspection
failed when the computer-use connection closed; no visual picker success is
claimed for Codex in this round.

The synchronization worker now owns allowed instruction content while AAG runs.
It materializes admitted top-level skill links, reconciles source changes,
repairs mirror edits, and privately backs up stale allowed files before pruning.
The initial pass copied 396 Codex files, removed 625 stale Codex copies, and
copied 512 Claude files. A repeat pass made no changes. A separate read-only
byte comparison after desktop launches found:

| Client | Source files | Mirror files | Missing | Extra | Changed |
| --- | ---: | ---: | ---: | ---: | ---: |
| Codex | 1,021 | 1,021 | 0 | 0 | 0 |
| Claude | 512 | 512 | 0 | 0 | 0 |

Temporary harmless resource files exercised addition, modification, independent
mirror-edit repair, and deletion through the running app-owned worker for both
clients; all four phases passed, and temporary resources were removed. Installed
Codex `skills/list` with `forceReload` returned 215 unique paths: 163 mirror-home
skills, 44 shared-user skills, and eight plugin skills, with zero loader errors.
Probe children were reaped. Authentication, sessions, model settings, and bundled
vendor `.system` skills remain outside instruction synchronization.

| Fresh command or acceptance check | Result |
| --- | --- |
| `cargo check --locked --offline` | Passed |
| `cargo test --locked --offline --lib services::mirror_sync:: -- --test-threads=1` | 15 passed |
| Fresh binary filter `mirror_roles` | 4 passed |
| Fresh binary filter `mirror_role_` | 4 passed, one live check ignored |
| Fresh binary filter `proxy::claude_desktop::tests::` | 14 passed |
| Fresh binary filter `services::launcher::codex_catalog::tests::` | 7 passed |
| Managed Claude role configuration regression | Passed |
| Existing supplier setup regression | Passed |
| Fresh binary filter `structured_quota` | 2 passed |
| Local HTTP provider filter `structured_five_hour_limit` | Passed |
| Routing revision tests | 5 passed |
| Full serial Rust library binary with fresh private home/config/temp | 1,065 passed, 49 failed, 22 ignored |
| Full failure-name comparison with earlier 51-failure record | No new failing names; two earlier Claude fixtures now pass |
| Current-tree privacy audit | 618 files, zero blockers, warnings, or findings |
| Live role setup and instruction reconciliation | Passed |
| Installed Codex model/turn/skill probes | Passed |
| Authenticated Claude catalog and Fable turn | Passed |
| Repeated native Claude and Codex opens | Passed |
| Independent final instruction byte comparison | Passed |

HTTP 429 and the structured five-hour/usage-limit fixtures fail over before
output begins; ordinary authentication and invalid-parameter cases remain
terminal. Tests did not force a real account to exhaust its quota. The existing
circuit breaker does not infer a five-hour reset deadline, partially emitted
streams are not replayed, and native opaque Responses continuations retain
their upstream source binding. HTTP 2xx application-error envelopes retain their
existing provider handling. These checks do not make the retained full gate
green, certify all imported endpoints, or authorize publication.

## Sources for the current follow-up

MiniMax's [official error-code reference](https://platform.minimax.io/docs/api-reference/errorcode)
identifies `2056` as five-hour usage exhaustion and documents the other handled
rate/usage codes. The [official skill documentation](https://learn.chatgpt.com/docs/build-skills)
describes shared user skill discovery; the installed-core probe supplies current
consumer evidence on this host.

The supported catalogue setting is documented in the
[official configuration reference](https://learn.chatgpt.com/docs/config-file/config-reference).
The [official Responses API reference](https://developers.openai.com/api/reference/resources/responses/methods/create)
defines namespace tool declarations and the namespace field on function calls.
The [cc-switch Claude Desktop manual](https://github.com/farion1231/cc-switch/blob/main/docs/user-manual/en/2-providers/2.6-claude-desktop.md)
provided the requested gateway/model-discovery reference. The installed clients
and actual request shapes were used to resolve version-specific behavior.

## Ranked multi-protocol follow-up: current code verified, native rollout pending

The later owner clarification explicitly requests Claude/OpenAI family names
at the client boundary and actual provider IDs at dispatch. Seven roles now use
the assessed model planner on Anthropic, Responses, Chat and Gemini ingresses.
The configuration-only current results are:

| Client role | First eligible upstream model | Planned hops |
| --- | --- | ---: |
| `claude-fable-5` | `glm-5.3` | 43 |
| `claude-opus-5` | `glm-5` | 30 |
| `claude-sonnet-5` | `glm-4.6` | 21 |
| `claude-haiku-5-5` | `glm-4.5` | 10 |
| `gpt-5.6-sol` | `mimo-v2.6-pro` | 43 |
| `gpt-5.6-terra` | `mimo-v2.6-flash` | 21 |
| `gpt-5.6-luna` | `glm-4.5` | 10 |

These are first candidates before request-specific filtering and failover.
They are not a record of every supplier successfully serving a request.
Native wire preference applies only within equal tier/priority bands; explicit
routes, exact offered IDs, pins and opaque continuation ownership retain their
existing boundaries.

The current public cache contains all 418 overall text rows dated 2026-10-22
from the [official Arena dataset](https://huggingface.co/datasets/lmarena-ai/leaderboard-dataset).
The production collector reads complete statistics and bounded rows pages,
rejecting partial, truncated, mixed-date and inconsistent data. Automatic
refresh retains a last good snapshot on failure. Relative offered-model rank
bands supply operational tiers; global rank, score, confidence interval,
observations and date are preserved independently. General text preference
rank is not an independently measured coding benchmark.

`MiniMax-M3.1-Flash` has no exact row in that snapshot and receives no invented
Arena tier. It remains available as a literal model request. The two existing
official MiniMax suppliers gained native Responses endpoints while retaining
Anthropic endpoints and their original credentials. Both direct native
protocol smoke requests returned `AAG_OK`. This uses the documented
[MiniMax Responses endpoint](https://platform.minimax.io/docs/api-reference/responses-create)
and the subscription's [shared usage contract](https://platform.minimax.io/docs/m-plan/faq).
Ignored state linkage is rejected before native MiniMax dispatch; materialized
history is supported. No new Gemini upstream is invented for MiniMax.

Native Gemini envelopes and signatures are preserved on a Gemini hop.
Supported foreign projections translate text, functions and history; unsupported
native-only fields fail before an upstream request. A wrapper regression now
checks the actual transport cache, preventing a default `unknown` protocol from
disabling native preservation or protocol preference.

Generic 429 and model rate-limit errors remain eligible for same-account
alternate-model failover. Only explicit account/plan allowance exhaustion
(`2056`/`2067`, `usage_limit_reached`, `insufficient_quota`) skips other model
and protocol hops sharing the resolved credential and HTTP origin. MiniMax
`2062` remains an ordinary retriable rate rejection. Authentication stays
terminal; streaming never replays after committed output. No live account was
deliberately exhausted.

### Fresh verification in this follow-up

| Check | Observed result |
| --- | --- |
| Production public Arena refresh | Complete 418-row snapshot, no source error |
| Direct MiniMax Anthropic and native Responses smoke | Both returned `AAG_OK` |
| Current-code temporary listener: ranked Fable via Anthropic | HTTP 200, `AAG_OK` |
| Current-code temporary listener: ranked Sol via Responses | HTTP 200, `AAG_OK` |
| Current-code temporary listener: literal MiniMax via Chat | HTTP 200, `AAG_OK` |
| Current-code temporary listener: literal MiniMax via Gemini | HTTP 200, `AAG_OK` |
| Current-code temporary listener: literal MiniMax via Gemini SSE | HTTP 200, `AAG_OK` |
| Sixteen protocol-pair fixtures plus strict Gemini admission | Passed |
| Runtime transport wrapper and quota regressions | Passed |
| Instruction synchronization/projection suite | 21 passed; prompt resources verified with a failing-then-passing regression |
| Final serial Rust library suite | 1,138 passed, 49 failed, 28 ignored |
| Full Rust failing-name comparison | Same 49 names as the preceding record |
| Node and UI typechecks, native UI build | Passed |
| Focused UI/i18n checks | 41 passed |
| Desktop runner checks | 44 passed |
| Source and built-demo browser smoke | 161 checks each passed |
| Privacy regression suite | 32 passed |
| Full Node suite | 336 passed, two tests failed, two suites failed initialization; recorded Windows ownership errors remain |
| Installed Codex metadata/skill consumer before supplemental projection | Three intended AAG roles, Sol default, Responses wire, no native OpenAI auth, 215 skills, zero loader errors |

The first temporary-listener Responses check returned HTTP 200 but failed its
answer assertion because the diagnostic combined reasoning and answer content.
The original failure log is retained. Filtering message `output_text` content
correctly produced `AAG_OK`; the corrected case and remaining surfaces passed.
No production response handling was weakened to satisfy that assertion.
Each temporary listener was stopped and each diagnostic process completed.

The independent instruction comparison again finds 1,021 source/mirror Codex
files and 512 source/mirror Claude files, with zero missing, extra or changed
files in the declared source roots. The eight existing vendor plugin skill
trees also match byte for byte across 198 files per side. A broader audit found
six plugin skills absent from the mirror and shared roots: five OpenAI
Developers skills and Local CLI Workbench, totaling 24 complete resource files.
The new managed projection resolves enabled source plugin records and declared
skill roots, copies their local resources, and reconciles changes/removals.
It does not guess among multiple installed versions. Missing, invalid or
ambiguous source metadata retains the last good content and reports a block.
Only source `[agents].enabled` and `max_concurrent_threads_per_session` are
merged into the Codex configuration, with backups and concurrent-write checks.
Other provider, authentication, permission, MCP, feature and plugin-runtime
configuration retains its separate ownership. One source agent profile selects
`gpt-6-luna` through `custom`; source-file parity does not prove that profile's
runtime provider is available.

The supplemental projection has not yet been applied to the live mirror.
The earlier AAG process still owns continuous synchronization and would prune
the new projection paths. The native restart-window question is pending, so
the old process remains in place. A recovery copy of that exact executable and
the prepared replacement are retained locally. The new port-8989 process,
the supplemental six-skill discovery and native two-turn follow-up remain
acceptance work after that controlled restart.

The final production `cargo check --locked --offline` passed. The final binary
linked successfully into the dependency output and was retained as the prepared
replacement. Installing it at the ordinary development executable path remains
blocked by the running process's Windows file lock. The final release asset and
current-tree privacy gate scanned 623 files with zero blockers, warnings or
findings. This gate is separate from the retained full-suite failures above.

Computer Use was stopped with physical Escape and was not used again. No new
visual picker claim is made. The full project gate remains non-green, and no
commit, push, public export, installer or release was created.

## Approved live rollout and final synchronization correction

This later record supersedes the preceding pending-rollout state. The owner
explicitly approved replacement after the current response finished. The
restart guard checked the exact task-owned executable, runner and listener,
required zero unmatched active streams and a quiet request log, and retained
a recovery copy before stopping the old process. The supported hidden
development runner started the new gateway on port 8989. No unrelated client
or process was stopped.

Actual port-8989 requests returned `AAG_OK` for ranked Fable over Anthropic
Messages and ranked Sol over Responses. The Responses result identified
`mimo-v2.6-pro`. Literal `MiniMax-M3.1-Flash` requests also passed Chat
Completions, Gemini generation and Gemini SSE. Gemini count returned an
explicit estimate. Installed Codex 26.1021.3705.0 completed two ephemeral,
read-only diagnostic turns through Sol/Responses; both returned `AAG_OK`,
and the diagnostic child was reaped. No native account was deliberately
exhausted, and no live mid-stream replay was attempted.

A separate ranked Sol request on the Chat surface returned an upstream
HTTP 400 after one attempt. The retained metadata establishes the status,
195 response bytes and terminal classification; it does not establish the
vendor reason or exact supplier identity. The existing inventory alone is
not sufficient to identify that failed hop. The failure was not hidden by
changing the ordinary 4xx policy, subscription endpoint, credentials or
billing model. Successful literal MiniMax Chat and ranked Responses checks
do not verify every configured supplier offer.

The first replacement process reported `mirror_sync_source_link_forbidden`
for Codex. The native AAG launcher exposed that same error before launching a
client. Inspection established that both Chrome plugin caches contain a
`latest` Windows junction to an ordinary direct version directory in the
same package. Plugin-version discovery now admits that alias only as a
duplicate reference to a version already enumerated through a concrete
path. Outside, nested, dangling and differently named links are rejected;
two actual versions remain ambiguous even with a valid `latest` alias.
General directory scans and trusted skill-link rules are unchanged.

The actual-junction update regression then exposed a Windows ownership-key
defect: forward and backslash spellings of the same projected path could
enter the replacement and deletion lists separately. The second operation
failed at an exclusive duplicate backup. Windows keys now normalize path
components before case folding. Regressions require a resource update to
retain the unchanged skill, a skill update to retain its unchanged resource,
and a following reconciliation to perform no copies or deletions. Unix keys
retain literal backslashes. The fixture-command, storage-diagnostic and
original failure logs are retained; temporary diagnostic source was removed.

All 25 synchronization tests pass, including actual Windows junctions. The
final serial library run reports **1,142 passed, 49 failed and 28 ignored**.
Its 49 failing names exactly match `rust_full_delivery.log`; no new failing
name was introduced. The matching development library build passed. After
the active Claude response completed, the correction replaced only the
task-owned gateway; the supported runner's final binary build took 5.31
seconds. The final process is PID 61904, started 2026-10-24 at 23:49:54 +08:00,
and owns the port-8989 listener. It remains running under runner PID 125596.
Process IDs are an observation from this acceptance run, not stable identity.

### Final instruction and consumer evidence

| Scope | Original | Mirror | Result |
| --- | --- | --- | --- |
| Codex declared instruction roots | `~/.codex` | `~/.aag/mirrors/codex` | 1,021 files, byte equal |
| Codex supplemental plugin resources | Enabled source plugin manifests and concrete skill trees | `skills/aag_plugin_instructions/` inside the Codex mirror | Six skills, 24 files, byte equal |
| Codex complete logical instruction inventory | Declared roots plus supplemental resources | Codex mirror | 1,045 files; zero missing, extra or changed |
| Claude declared instruction roots | `~/.claude` | `~/.claude-cli` | 512 files; zero missing, extra or changed |
| Existing vendor plugin skill trees | Native plugin cache | Corresponding mirror plugin cache | 198 resource files, byte equal |

The startup pass copied 25 instruction resources and deleted none: the 24
supplemental resources plus a changed existing source-owned manifest. Earlier
strict reconciliation had already backed up and pruned 625 obsolete Codex
instruction files; that historical deletion count is separate from this pass.
The earlier native-root propagation checks for both clients remain recorded.
The final running-worker probe added one inert resource to the enabled Local
CLI Workbench skill tree, modified it, repaired a mirror-only edit, and
propagated its source deletion. All four observations passed in 39.88 seconds.
The source fixture was removed. A full independent byte comparison after
deletion again reported zero discrepancies.

Installed Codex now discovers **221 skills with zero loader errors**, including
exactly one projected instance of each of the six supplemental skills. Its
actual configuration reads `gpt-5.6-sol`, provider `aag`, wire API `responses`,
and no native OpenAI authentication requirement. It exposes Sol, Terra and
Luna, with Sol default. It also reads source `[agents].enabled = true` and
`max_concurrent_threads_per_session = 4`. The consumer process was reaped.

Final authenticated discovery exposes 57 OpenAI entries, 57 Gemini entries
and the four intended Claude roles. The effective Claude settings select
`claude-fable-5`, and each Fable/Opus/Sonnet/Haiku environment default contains
its corresponding exact client role ID. The managed native desktop profile
uses discovery. The actual Claude picker was verified during the earlier
role-loading phase; a new Codex/Claude GUI picker check was not claimed for
this final correction. After the later restart approval, Computer Use was
used on AAG to obtain the precise launch-blocking error described above.

Instruction parity does not imply full home-directory or runtime parity.
Native source and mirror retain separate authentication, permission, MCP,
feature and plugin-activation configuration. One source agent profile names
`gpt-6-luna` through `custom`; its identical mirrored file is not evidence
that the separate custom provider is usable. Projected plugin instructions
do not install or enable their MCP servers or account-bound capabilities.

Primary evidence is retained under `src-tauri/target/ranked_gateway/`:
`mirror_sync_final.log`, `rust_full_rollout_final.log`,
`desktop_correction_prebuild.log`, `desktop_final_stdout.log`,
`desktop_final_stderr.log`, `rollout_anthropic.log`, `rollout_responses.log`,
`rollout_literal_chat.log`, `rollout_literal_gemini.log`,
`rollout_literal_gemini_stream.log`, `codex_consumer_two_turns.log`,
`codex_consumer_final.log`, `rollout_final_metadata.log`,
`live_projection_sync_final.log` and `instruction_parity_after_deletion.log`.
The final full project gate retains the documented Rust and Node failures.
No commit, push, export, installer or release was created.

The post-rollout `npm run release:verify` passed the release preflight and
current-tree privacy audit: 623 files scanned, zero blockers, warnings or
findings. Its fresh receipt is `release_verify_after_rollout.log`; it is
separate from the earlier pre-rollout receipt. `git diff --check` also passed.
