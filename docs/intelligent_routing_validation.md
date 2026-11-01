# Intelligent routing implementation validation — 2026-10-28

## Publication follow-up

After the implementation review, the requested all-changes publication gate
found and fixed a request-identity defect: an upstream dated response model could
be mistaken for the configured fallback hop. Router completion now returns the
actual request-local serving hop; recovery no longer relies on the shared
last-used display map. A barrier-synchronized regression checks concurrent
dispatch provenance, and the stalled-generation fixture reports different
upstream labels while requiring the configured fallback sequence.

Incidental test paths and synthetic credential labels were made explicitly
public-safe. Demo addresses use documentation ranges. The privacy scanner now
recognizes only the two complete publicly documented Tailscale protocol ranges;
individual peer addresses and narrower private allocations remain findings.
The [official range reference](https://tailscale.com/docs/reference/reserved-ip-addresses)
grounds that distinction. Earlier failed audit evidence below is historical.

Final follow-up checks: Node typecheck passed; Node **490 passed, one skipped**;
desktop runner **48/48**; qualification-runner **40/40**; automation **55 passed,
two skipped**; privacy **33/33**. Native/demo frontend builds passed, and source
and built-demo browser smoke each passed **183/183**. Final ordinary Rust passed
**1,595**, retained **40 ignored**, and excluded exactly the existing **26**
Windows qualification cases with process-local loopback `NO_PROXY`. Cargo check
passed. `release:verify` and the current-tree privacy audit passed with **zero
blockers and zero warnings**. These results do not qualify the Windows-only tests
or constitute installed/native, paid-inference, or public-release acceptance.

The private development branch is the requested publication target. Live supplier
cleanup/reimport has not been executed: scope selection remains separate from
source publication. Only aggregate, read-only inventory inspection was performed.

Source baseline: private `master` at `017308013d1bac04a739cacf71037a2664686fa8`
with this task's uncommitted changes and preserved pre-existing desktop work.
No commit, publication, installation, live client rewrite, or paid inference was
performed. The previously paused canary was not resumed.

## Root causes, design and changed owners

[Implementation design](intelligent_routing.md) records the source-traced root
causes, normalized scoring equations/defaults, metadata provenance, supplier slot
allocation, model reuse, health selection, and deterministic fallback behavior.
[Prompt enhancement](prompt_enhance.md) records stage compatibility and recovery.

Primary owners changed:

- `services/model_discovery`, supplier discovery/DAO, CC Switch import commands:
  bounded protocol-aware catalog retrieval, transactional offer reconciliation,
  independent listing health, save/import revalidation and automatic demotion.
- `services/model_catalog/{utility,select,tiers,parse}`, runtime planner and
  `proxy/model_health`: scoring, strict pricing identity, supplier slots,
  model-level health, and refill from healthy overflow candidates.
- `model_catalog/arena_visualization`, Coding Agents/Arena components: complete
  backend queue, provider/model identity, health/cost explanations and timed refresh.
- `services/prompt/{mod,recovery,review,target,templates}`, Prompts UI/contracts:
  budgeting, preserved partial content, bounded continuation, configured backend
  fallback, economical review/escalation and truthful completion status.

## Commands and evidence

| Check | Result |
| --- | --- |
| `npm run typecheck` | Passed |
| `npm test` outside the restricted sandbox | 490 passed; one skipped |
| `npm run ui:build` | Passed |
| `npm run ui:build_demo` | Passed, also rebuilt by smoke-demo command |
| `npm run ui:smoke_demo` | 183/183 passed against owned built-demo server/profile |
| Focused Coding Agents browser fixture | 23/23 passed, including 40-hop parity and timed refresh |
| `npm run test:privacy` | 32/32 passed |
| `npm run privacy:audit` | Failed: 22 blockers in existing migration/session-authority fixture paths |
| `git diff --check` | Passed |
| `cargo check --locked --offline` | Passed on final source; existing compiler warnings remain |
| `npm run verify:rust -- --profile ordinary --run` with process-local loopback `NO_PROXY` | 1,594 passed; 40 ignored; exactly the existing 26 Windows qualification tests excluded |
| Raw `cargo test --locked --offline --lib -- --test-threads=1` | 1,592 passed; 27 failed; 40 ignored before the final added stalled-backend recovery fixture |

The raw failures consist of the existing 26 Windows qualification cases and
`shared_client_reuses_an_idle_connection`. The latter also failed in isolation
under inherited routing, then passed with process-local
`NO_PROXY=127.0.0.1,localhost,::1`. This is explicit fixture-network isolation, not a
production networking change. Windows prerequisites were not changed or bypassed.

The initial restricted-sandbox Node run failed on ACL and loopback operations;
the supported outside-sandbox rerun passed. The first full source-browser run
lost its CDP target. Its retry could not bind port 5173 because an unrelated
development server occupied it. That process was preserved. Source-browser smoke
is therefore not claimed as passing; the separate built-demo and focused graph
results are passing evidence.

## Regression process

Before routing changes, new regressions observed six duplicate/unbounded hops
where two were required, and no affordable cross-tier candidate. Both are now
covered by the catalog suite. The graph browser regression first passed 21/22,
failing complete 40-hop visibility, then passed after queue rendering changes.

The first full Rust integration run reported 1,585 passed, 32 failed and 40
ignored. Five task integration failures were corrected: supplier slot expectations,
same-model native-protocol tie ordering, and a review fixture missing the required
alias planner. Later review added regressions for healthy overflow slot refill,
exact variant prices, malformed pagination, automatic demotion/restoration,
provider-authoritative context limits, short fragment preservation, router context
reservation and stalled continuation fallback. Tests and gates were not disabled
to obtain a passing feature result.

## External source checks

The existing ignored `live_public_sources_parse_with_the_shipped_rules` test was
explicitly executed and passed against Models.dev and EvalPlus. The existing
`live_reqwest_arena_rows_metadata` test first rejected a missing opt-in marker;
after inspecting and supplying its metadata-only marker, both official Arena
rows and statistics requests passed. Neither check updates a live supplier or
performs model generation. Arena transport success is not proof that every public
model has a comparable rating or is offered by a configured supplier.

## Remaining acceptance boundaries

- Automatic discovery uses the configured protocol/auth scheme. Arbitrary
  alternative wire protocols and authentication methods are not auto-detected.
- No real-provider generation or installed native desktop acceptance was run.
  Catalog availability is labeled separately from inference availability.
- Public prices are reference metadata. Unknown/conflicting prices remain
  unknown; negotiated supplier prices and cached-token billing are not inferred.
- Prompts requests nonstreaming completions. Arbitrary interrupted SSE fragment
  reconstruction is outside this implemented path. Genuine context/resource
  exhaustion returns recoverable content with an explicit incomplete status.
- Full Windows qualification and public release readiness remain open.
