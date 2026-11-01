# Provider screening and admission

Subscription-source integration, protected admission records, import filtering,
runtime gates, the operator and the Suppliers review panel are implemented.
Local integration checks passed. The initial bare-ok assessment produced an empty
pool; the approved instruction follow-up admitted three connections. Separate
dated results below preserve both observations.

This contract belongs to the Rust/Tauri database-backed runtime. Import,
discovery, direct tests, prompt dispatch and routed calls share its admission
owner. Public catalog visibility, a configured key, an enabled provider and
CC Switch's current selection are not subscription evidence.

## Subscription authority

CC Switch supplies the exact source record and usage-query configuration.
The subscription_source service reads that source without modifying it and
binds evidence to its canonical file path, mapped source identity, source
configuration and corresponding AAG connection. Credentials remain backend-only.

Native templates use fixed HTTPS destinations, exact host checks, no redirects,
a 15-second deadline and bounded status bodies. Stored JavaScript is never
evaluated. The contracts follow CC Switch's official
[coding-plan service](https://github.com/farion1231/cc-switch/blob/main/src-tauri/src/services/coding_plan.rs)
and [balance service](https://github.com/farion1231/cc-switch/blob/main/src-tauri/src/services/balance.rs).

| CC Switch template | Implemented adapters |
| --- | --- |
| token_plan | Kimi, Zhipu individual plans, MiniMax |
| balance | DeepSeek, StepFun, SiliconFlow, OpenRouter, Novita |

Meaningful typed allowance fields are required. HTTP 200, empty tiers and an
enabled usage-query flag alone cannot establish eligibility. Exhausted quota
or balance blocks the ok probe without being relabeled as expired credentials.
A status endpoint's HTTP 401 or 403 produces inactive evidence with reason
credential_rejected: the configured credentials were rejected; this is not
proof that a paid subscription expired.

Missing, disabled or unsupported usage configuration yields unknown and sends
no status query or inference probe. **Volcengine remains unsupported even when
CC Switch contains its control-plane access credentials.** Zhipu team plans and
other unimplemented templates also remain unknown. There is no fallback to an
unrelated template or arbitrary script execution. Account/OAuth-backed source
rows remain excluded from static import.

Positive native status observations currently have **300 seconds of evidence freshness**.
This is not a vendor subscription expiry date; quota reset times are not used
as subscription expiry. Stable configuration retains the same evidence identity
when an authorized refresh observes renewal.

A matching cached inactive result permits no automatic status query or ok
request. Explicit Recheck provider can query again after renewal. Previously
admitted positive evidence may be refreshed when stale through the existing
request/runtime-refresh path without repeating its successful inference probe.
Source or connection changes invalidate the binding. No standalone recurring
screening task is created.

Subscription-free local use requires a separate explicit owner assertion bound
to an exact CC Switch source reference and configuration on a loopback endpoint.
It is never inferred from a provider name, a missing key or loopback alone.
A local assertion has no invented vendor expiry; its exact source and connection
binding must still match on every admission lookup.
A local Ollama connection receives the same measured probe and latency limit;
its name establishes no performance result.

## Probe and policy

The screening engine accepts provider-bound evidence. Inactive, unknown,
expired or mismatched evidence excludes a candidate before provider construction
or network activity. The subscription owner also checks allowance first.

An eligible connection receives one nonstreaming canonical user message,
`Reply with exactly: ok`, without system instructions, business context or tools.
Admission requires exactly one normally completed assistant reply whose trimmed
text is ok, followed by the latency check. Latency is actual elapsed time around
the provider call, including transport and response parsing.

The short health-only instruction was authorized on 2026-10-30 after the earlier
bare-ok request produced empty or conversational replies. Probe contract v2
distinguishes these measurements from the earlier v1 receipts; an old pass must
be rechecked before it can enter the current pool. Output validation, the token
cap and the latency/deadline policy are unchanged.

The validated provider_screening_policy setting defaults to:

    {
      "max_latency_ms": 5000,
      "probe_deadline_ms": 15000,
      "max_output_tokens": 64,
      "max_response_chars": 1024
    }

The Suppliers panel edits maximum latency and probe deadline while preserving
the other limits. Unknown fields and invalid ranges are rejected. The latency
threshold must be positive and no greater than the deadline; the deadline is
bounded at 120,000 ms. Policy changes invalidate prior admission until rechecked.
Evidence freshness also bounds an in-progress probe's deadline.

Screening instances cap successful response bodies at 16 KiB. HTTP error
classification retains its existing 64 KiB read bound; private captures are
bounded again. Actual abnormal assistant, HTTP and non-JSON replies can be
reviewed with truncation and redaction flags. Known credentials are removed
before capture limits, including decoded JSON string values and object keys.
Ordinary instances retain their normal response contract and never retain
probe-response captures. Captured bodies and credentials never enter logs or
the terminal operator's receipts.

Failure records distinguish subscription exclusion, timeout, connection refusal,
abnormal/incomplete output, authentication, rate limits, upstream errors and
excess latency. Sanitized upstream status/code supplements the private record;
a local decode error does not impersonate an observed upstream HTTP 502.

## Registration and revocation

The provider_admission service owns source caching and screening orchestration;
its DAO owns versioned compare-and-swap records. Raw source/cache/probe data is
blocked from generic settings IPC.

Each connection is screened using one recorded probe_model. Its other admitted
models are **advertised offers from that screened connection**, not independently
tested models. The review panel displays this distinction. Catalog discovery can
extend offered inventory only on an admitted connection; it does not manufacture
per-model inference evidence.

Explicit import binds source candidates, checks status, probes eligible
connections, then filters registration using current admission. New, existing
and alternative connections keep their actual projection identities. Long
screening batches reconcile positive evidence freshness without repeating the
successful ok probe; current admission is checked again during import filtering.
Repeated source declarations for one actual provider are coalesced before any
subscription request or probe. Their current model declarations are merged before registration,
so the last alternative cannot erase models from an earlier declaration.

Receipts bind connection configuration, source evidence and screening policy.
Expiry, revocation or changed bindings remove eligible pairs. Source and existing
AAG configuration are checked around screening I/O, and late results cannot
replace newer admission revisions. Subsequent dispatch and catalog requests use
the owner gate rather than trusting an old UI snapshot. This does not undo
requests already sent before revocation.

## Terminal operator and Suppliers UI

The operator is a repository example using supported database and service APIs:

    cargo run --locked --offline --example provider_screening -- --help

    provider_screening preview [--database PATH] [--source PATH]
    provider_screening apply --plan PLAN_ID --backup NEW_ABSOLUTE_PATH [--database PATH] [--source PATH]

Omitted paths use the existing AAG and CC Switch resolvers. Preview opens the
existing AAG database read-only and performs local reads only. It lists a bound
plan, source references, names and configured models; these are candidates,
not verified subscriptions.

Apply requires the current plan and a new absolute backup destination. The
database owner creates a full private snapshot **before any screening/cache/import
writes**. Screening then precedes the admission-filtered import. Repeat
--subscription-free-source SOURCE_REF only for exact explicitly authorized local
source identities; the backend validates the loopback endpoint and source
binding. There is no wildcard exemption or provider-name shortcut.

Terminal receipts contain eligibility, stages, reasons, timings and import
counts. They exclude URLs, credentials, scripts and actual probe responses.
Failure after screening can leave protected admission records changed while
import is unapplied; receipts and the retained backup distinguish those states.
This operator does not perform a full provider reset, launch an app, reload a
different process or write client configuration. See the separate
[helper documentation](../src-tauri/examples/README.md) for reset/import behavior.

The Suppliers Provider screening panel displays admission activation, provider
and model-pair counts, subscription/exclusion reasons, tested model, latency and
bounded response details. Refresh status only reads the review state. Recheck
provider performs an explicit status-first check and requests a runtime reload
through its owning command. Saving thresholds does not silently re-probe
excluded providers.

### Selected-provider rechecks

Recheck is a check-only action. It preflights the selected connection, a real
configured probe model, the policy and the exact source before starting. It
preserves the existing enforcement setting and does not register providers or
run a source-wide import. A successful provisional record remains
`awaiting_registration` until the separate explicit Import action.

The command returns a receipt separating its own committed source/admission
changes from check, runtime-apply and overview-readback failures. Policy and
registration mutation flags remain false. The latest overview describes actual
backend state, including unrelated concurrent policy changes; those changes are
not attributed to this check. Missing readback remains unknown. A lost IPC
acknowledgement refreshes status without automatically resubmitting a probe or
claiming cancellation.

A Tauri-managed, process-local coordinator holds ownership for one provider
through check, runtime reconciliation and readback. Concurrent checks for that
same provider return busy; independent providers remain usable. Dropping or
finishing the operation releases its permit. Persistent compare-and-swap still
fences stale results across processes. This is not an exactly-once upstream
execution or cross-process probe-lock guarantee.

The panel's post-check supplier/status refresh is read-only and does not invoke
import maintenance. Its runtime reconciliation builds from persisted admission
without refreshing unselected subscriptions. Ordinary explicit/request evidence
refresh retains its separate authority. Expired evidence can therefore affect
effective eligibility without this check rewriting another provider's records.

Threshold drafts survive recheck/refresh outcomes. Active operations and dirty
threshold edits block destination changes; Discard threshold edits restores the
current displayed policy without a settings write. Saving thresholds remains an
explicit global policy action. The panel labels these scopes in English and
Chinese.

The ordinary provider Test control explains its configured-model and verified
CC Switch source prerequisite before execution. A manual connection does not
receive invented evidence when enforcement is off. Catalog discovery is a
separate metadata operation; a returned inventory proves neither inference
success nor pool eligibility.

### Catalog connection and authentication identity

Saved supplier discovery uses the selected endpoint's exact runtime projection.
Under enforcement a changed or unbound draft cannot borrow another provider's
admission, even if URL, key, headers and protocol match. The validated source
credential scheme flows through supplier discovery, manual catalog fetch and
scheduled refresh. Anthropic catalog requests send either Bearer authorization
or `x-api-key`, including protected-header handling, rather than both.

Known source metadata must still match with enforcement off; missing, malformed
or changed bindings do not silently become static API-key connections. Truly
unbound manual providers retain their static default. Source identity represents
the validated source/connection binding independently of status observations;
that identity is not positive subscription evidence. A retained old runtime
checks current versus captured identity before a new attempt, including with
enforcement off. Already admitted streams retain their existing snapshot.

### Additive URL discovery

URL discovery is downstream of admission and never a substitute for it. It runs
only for an already configured supplier whose provider row passes the same
eligibility preflight as catalog refresh: enabled supplier, endpoint,
alternative and provider; auto-tracking on; a probeable URL type; a valid
absolute HTTP(S) URL; current subscription evidence when a subscription source
is bound; a current matching admission record when one is stored; and no recent
non-available catalog-health observation inside the 300-second retry window.
`catalog_invalid` is exempt and stays immediately retryable; other recent
unavailable health waits 300 seconds.
An excluded entry is reported before any catalog or discovery request and
retains its existing inventory; preflight may still refresh subscription status
through the existing owner. Supplier save/import revalidation reads only the
requested suppliers by ID and gives each projection its own 20-second bound; a
corrupt or slow entry is reported without cancelling the rest.

Discovery runs once per configured source context and reads both same-origin
OpenAPI 3.x documents (the protocol `/models` root and the configured origin),
merging candidates and reporting per-document failures. It refuses redirects,
remote servers, templated server URLs, and URLs carrying credentials, query
strings or fragments. Catalog fetch preserves configured query parameters; only
URL expansion skips a query-bearing source, and catalog candidates drop
fragments. A candidate is appended only after a GET returns `405`
with `Allow: POST` and a sibling-path negative control relative to the probe URL
returns `404`. A non-Gemini catalog may be empty; Gemini additionally needs at
least one listed model to build its probe route. For Gemini, the declared root
must equal the runtime-normalized `.../models/<model>:generateContent` route; a
rewritten route fails `url_validation_runtime_path_mismatch` before I/O. Every
Gemini catalog model ID must also normalize to one path slot; an empty, `.`,
`..`, or slash, backslash, query, fragment or percent-bearing ID fails
`url_validation_model_path_invalid` before the probe. A shared catalog or an
API-prefix-wide method handler cannot validate a protocol on its own.

The append is one generation-checked transaction that adds the endpoint, models,
compatibility projection and listing health. If the candidate URL equals the
supplier base, the endpoint reuses that base instead of duplicating it; different
protocols may share an API root. It creates no supplier, source binding or
admission record, and it preserves existing endpoints, alternatives, models and
admission evidence. Only an existing protocol is a no-op, so a repeated refresh
reports no change. Every skipped or failed URL attempt produces its own report
entry with a `supplier_url_skipped:` or `supplier_url_failed:` reason;
per-document search failures include the document index. Reports omit URLs,
credentials and upstream bodies. A validated route and a listed model are still
listing evidence, not inference success, and the supplier remains
`inference_status: "unverified"`. Corrupt supplier-owned projection rows are
isolated as per-entry configuration failures and do not block other suppliers;
corrupt legacy rows keep their prior failure contract.

The supplier UI provides English/Chinese explanations for discovery exclusions,
skips and failures through the existing EN/CN switch. This is display formatting
of the persisted report, not a new screening decision or an admission change.
Unknown discovery details use a generic message rather than being echoed into
the results panel.

## Queue and Prompts contracts

Catalog-driven retirement extends screening's listing reconciliation: a
successful listing that stops containing a model id disables it. Observed
`provider_catalog` rows flip immediately; import-owned rows (`cc_switch`,
`url_discovery`) retire after `supplier_catalog_retire_after_misses`
consecutive successful-but-unlisted fetches (default 2); manual rows are only
flagged (`retire_reason='catalog_unlisted'`) and stay enabled. Fetch failures
never retire anything, and relisting restores what reconciliation retired.
Retired rows leave the projected pool and every CLI-visible model list.

Alias fallback retention keeps every eligible mapped provider + model pair;
it no longer truncates each supplier to two candidates. The same model on
different providers remains distinct, and reuse across aliases is allowed.
Preferred order starts with different providers and models, then retains the
remaining eligible pairs in stable order. Tier fit and reference cost affect
preference rather than discarding otherwise eligible fallback supply.

Admission does not invent capabilities or rankings. Alias selection still
requires a tier from the assessment chain — public evidence first, documented
name/metadata inference (`name_inference`) as the weakest accepted basis that
orders behind every evidence-assessed candidate in the same band, pending
otherwise — plus hard request capabilities. Exact model requests remain
exact, explicit routes retain their ordering contract, and every dispatched
candidate remains subject to admission. The legacy models_per_supplier field
remains parseable but no longer limits fallback membership.

The diversity helper also runs after dynamic health selection. An untried policy
head is not displaced solely because a fallback accumulated success on another
role. Prompt optimization uses admitted routing; direct targets cannot bypass
the admission owner.

The earlier MiniMax prompt-budget repair applies low effort to the actual
Anthropic hop, including aliases and both prompt stages. Explicit effort,
native source envelopes, other models and ordinary gateway calls retain their
contracts. This does not establish that every reported Anthropic failure is fixed.

CC Switch's selected credential field also controls static Anthropic
authentication: AUTH_TOKEN uses Bearer authorization and API_KEY uses x-api-key,
with the source's explicit bearer_only mode preserved. Only one static auth
header is sent. Scheme changes rebuild the cached transport, and an old routing
snapshot cannot borrow admission for a newly changed source binding.

## Verification and deployment boundary

The following are **prior-phase evidence**, preceding subscription-source,
cache, import/runtime and UI integration: the ordinary Rust profile passed
1,650 tests with 40 ignored and 26 existing Windows qualification exclusions;
a later engine-only correction passed 14 screening tests. The broader proxy
run had first recorded 481 passes and one cold-head ranking failure, which was
fixed before that ordinary-profile pass. The earlier release-input/privacy gate
reported zero findings. These results are not current integration acceptance.

The integrated ordinary Rust profile passed **1,692 tests**, with 40 ignored and
the same 26 Windows qualification exclusions. Node tests passed 496 with one
platform skip; native desktop-runner fixtures passed 48; privacy fixtures passed
33. Type checking, UI and demo builds passed, and the headless demo passed
183 checks. The operator's three fixture tests passed. Sandbox-only runs first
encountered loopback, temporary SQLite and child-process restrictions; permitted
native reruns supplied the passing evidence. Ordinary tests do not establish
Windows privilege qualification, signing, installation or native WebView IPC.

### Live assessment, 2026-10-29

The operator created a private database recovery snapshot before activation.
Both the live database and snapshot passed integrity checks with zero foreign-key
errors. CC Switch was read-only. Existing configuration retained 18 suppliers
and 28 proxy providers; eligibility is a separate filtered pool.

The initial run returned 29 outcomes for 28 actual connections because one
source declaration was duplicated. That preparation defect was subsequently
fixed and regression-tested; the historical run was not rewritten or repeated.
The 28 persisted connection outcomes were:

| Outcome | Connections |
| --- | ---: |
| Unverified subscription evidence | 19 |
| Connection refused | 3 |
| Abnormal or non-ok response | 3 |
| Quota exhausted before inference | 1 |
| HTTP 429 during the probe | 2 |
| Admitted | 0 |

Kimi's HTTP 429 replies described a monthly usage limit. MiniMax reported
exhausted allowance before an inference probe. Zhipu and DeepSeek produced empty
visible text or a conversational reply instead of literal ok; their measured
probe times were 1,396–3,565 ms. Empty/incomplete replies now retain the bounded
wire envelope for future checks; unavailable details from that earlier run were
not reconstructed. Missing, disabled or unsupported usage configuration stayed
unknown and did not receive inference probes.

Ollama's three configured connections failed with **connection refused**, after
about 2,022–2,031 ms. This is not evidence of response latency above 5,000 ms.
No provider-name exclusion or fabricated slow result was used.

After rebuilding and restarting AAG, authenticated health returned HTTP 200 and
the model list returned HTTP 200 with zero entries. No successful live forwarding
or live failover is claimed because the admitted pool was empty. Provider/model
diversity and revocation were verified with local fixtures. This assessment used
the earlier bare-ok request. The exact-ok response requirement remains in force;
the subsequently approved instruction is described above.
An authenticated bare-ok alias request returned HTTP 404 with the specific
model_alias_no_candidates code, without selecting an upstream.

Source publication, application reload and upstream availability are separate
outcomes. Browser demo checks do not establish native IPC or upstream readiness.

### Exact-ok instruction follow-up, 2026-10-30

The approved request wording is now `Reply with exactly: ok`. Response matching,
the 5,000 ms admission threshold, 15,000 ms deadline and 64-token cap are unchanged.
Contract v2 requires fresh receipts. The bounded operator recheck selects explicit
registered connections with prior abnormal-response receipts, snapshots the
database before mutation, and has no source-wide import fallback.

The three selected connections passed a fresh subscription check and returned
exact ok responses:

| Connection | Probed model | Direct probe latency |
| --- | --- | ---: |
| DeepSeek Anthropic | deepseek-v4-pro | 1,225 ms |
| DeepSeek Responses | deepseek-flash | 844 ms |
| Zhipu Anthropic | glm-5.3-flash | 1,760 ms |

Their existing inventories contribute **15 provider/model pairs**. Only the
three listed models were probed; the other entries are advertised offers from
those accepted connections. No other previously excluded connection was retested.
The private snapshot and live database both passed integrity checks with zero
foreign-key errors, and only the three selected admission records changed.

After reload, authenticated health returned HTTP 200 and the model endpoint
returned 28 model/selector entries. One health-only instruction through the
gateway to precise:glm-5.3-flash returned HTTP 200, exact ok and normal completion.
That end-to-end request took **6,563 ms**; the direct admission measurement above
does not guarantee that every later gateway request finishes within 5,000 ms.

The request-boundary regression first failed on the old bare-ok payload and then
passed with the instruction. The final ordinary Rust profile passed **1,694 tests**
(40 ignored; the existing 26 Windows qualification cases excluded). All seven
operator fixtures passed. The prior assessment remains historical evidence.
