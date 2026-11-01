# Provider discovery, allocation and prompt recovery

This implementation extends the desktop supplier registry and existing routing
planner. The Node headless gateway remains independently runnable. Changes are
local source changes; they do not install a release or migrate live client files.

## Root causes

- Catalog discovery retried authoritative authentication/rate-limit errors as
  if they were endpoint mismatches, and a successful HTML response could stop
  discovery. Refresh only upgraded existing model families in legacy provider
  rows; new families never reached supplier offers used by the planner.
- Alias planning used capability tiers and provider priority, ignoring parsed
  price and release metadata. Runtime observations were provider-wide, so
  model-specific failures could not guide subsequent alias selection.
- The graph deduplicated model names across providers, capped backend results at
  32, and displayed only eight candidates. Unrated intermediate candidates were
  missing from the scatter-plot connection, making it an incomplete queue.
- Prompt optimization sent one nonstreaming request, used fixed output ceilings,
  and rejected token-limit termination before retaining its visible content.
  Input limits of 2,000/8,000 characters also rejected ordinary technical briefs.

## Data flow and ownership

CC Switch import retains the existing static-credential/account exclusion rules.
Supplier save and import revalidation is scoped: each requested supplier is read
by ID, and each projection is refreshed under its own 20-second wall-clock bound.
A corrupt supplier or timed-out projection is reported without cancelling later
entries or spending their 20-second budgets; committed changes still set the
pending-reload signal. Failures aggregate as `supplier_discovery_partial:<reason>`
or `supplier_discovery_timeout`.

Catalog fetch preserves nonstandard path prefixes and the configured query
parameters; fragments are dropped. Only URL expansion skips query-bearing
sources: a source URL with a query string or fragment is not expanded into
discovered routes. GET discovery uses existing authentication, refuses redirects,
limits time, bytes, pages and model count, and distinguishes authoritative HTTP
errors from unsupported paths. No paid inference is necessary to list models.

Before any catalog or discovery request, supplier-owned providers pass one
preflight check: supplier, endpoint or alternative, and provider enabled;
auto-tracking enabled; a probeable URL type; a valid absolute HTTP(S) URL with a
host and no embedded credentials; current subscription evidence when a
subscription source is bound; a current matching admission record when one is
stored; and no recent non-available catalog-health observation inside the
300-second retry window. `catalog_invalid` is exempt, so malformed catalog data
remains immediately retryable; other recent unavailable health waits 300
seconds. Excluded entries are reported as
`supplier_import_excluded:<reason>` or `supplier_import_skipped:<reason>` and
make no catalog or discovery request; preflight may still refresh subscription
status through the existing owner. Disabled, unavailable, expired, obsolete, or
non-probeable suppliers keep their existing inventory; a supplier with no
configured endpoint is reported as
`supplier_import_excluded:no_configured_endpoint` and is not probed. Legacy pool
providers retain their existing opt-in and invocation behavior. When enforcement
is enabled, the existing catalog admission check still runs after this preflight
and is not replaced by URL discovery.

After a supplier's catalog refresh succeeds, AAG may search for missing
protocols. Discovery runs once per configured source context (supplier plus
provider configuration identity), not once per supplier. It reads both
same-origin OpenAPI 3.x documents derived from the `/models` root and the
configured origin, merges candidates from both, and reports per-document
failures separately instead of stopping at the first parseable document. It
follows operation → path → document server precedence and accepts only declared
`post` operations for `openai-chat`, `openai-responses`, `anthropic`, and
`gemini`. Redirects, remote servers, templated server URLs, non-HTTP(S) targets,
and URLs with embedded credentials, query strings, or fragments are refused; the
search is bounded to 512 KiB, 512 paths, 16 candidates, and five seconds.

A candidate is validated when a GET returns `405 Method Not Allowed` with
`Allow: POST` and a sibling-path negative control relative to the probe URL
returns `404`. The sibling control rejects an API-prefix-wide method handler thatwould also answer the missing path. A non-Gemini catalog may be empty; Gemini
additionally requires at least one listed model to construct its probe route
(`url_validation_empty_catalog` otherwise). For Gemini, the declared root must
produce exactly the runtime-normalized
`.../models/<model>:generateContent` URL; a route that runtime normalization
would rewrite fails `url_validation_runtime_path_mismatch` before I/O. Each
Gemini catalog model ID must also normalize to a single path slot: empty, `.`,
`..`, or any ID containing `/`, `\`, `?`, `#`, or `%` fails
`url_validation_model_path_invalid` before the probe, so a catalog ID cannot
escape its route slot or select another protocol route. A shared catalog or an
API-prefix-wide method handler cannot validate a new protocol by itself.

A validated missing protocol is appended in one generation-checked transaction
with its models, compatibility projection, and listing health. When the
candidate URL equals the supplier's configured base, the endpoint reuses that
base instead of storing a duplicate URL; different protocols may share one API
root. Only an existing protocol is a no-op, so a repeated refresh changes
nothing. The append creates no new supplier, source binding, or admission
record, and existing endpoints, alternatives, models, and admission evidence
remain intact. Each skipped or failed URL attempt is a separate report entry
with a stable `supplier_url_skipped:` or `supplier_url_failed:` reason;
per-document search failures include the document index. Reports and logs exclude
URLs, credentials, and upstream bodies.

Provider identities are enumerated before row decoding. A corrupt supplier-owned
projection is isolated as a `supplier_import_failed:configuration:<reason>`
entry and does not abort later suppliers; a corrupt legacy row keeps its prior
failure contract.

Client management renders the stored discovery report with EN/CN explanations
for each entry and failure stage. Changing language rerenders the same report
without another model request; the backend codes and routing decisions remain
unchanged. The [supplier workspace guide](supplier_workspace.md#english-and-chinese-discovery-explanations)
describes the help, health and results views.

Supplier-owned discovery transactions compare the exact admitted provider row
before updating offers and projections. Listing health records its timestamp,
latency, failures and provenance separately from inference health. A listed model
is **not** represented as having passed generation. Credentials remain write-only
in frontend DTOs and do not enter discovery reports.

### Catalog-driven retirement

A successful catalog listing that no longer contains a model id retires it.
Observed `provider_catalog` rows flip immediately (existing behavior, now also
stamped with `retired_at`/`retire_reason`); import-owned rows (`cc_switch`,
`url_discovery`) retire only after `supplier_catalog_retire_after_misses`
consecutive successful-but-unlisted fetches (default 2, clamped 1..=10), so a
single flaky listing cannot disable live supply; manual rows are flagged stale
(`retire_reason`) but never auto-disabled. A fetch failure never retires
anything. Relisting restores every row reconciliation retired; user-disabled
rows stay disabled. Retired rows leave the projected provider pool and the
CLI-visible model lists; they remain in `supplier_models` for recovery.

Legacy pool providers prune delisted ids after a successful refresh when the
id is not pinned by a route hop or `model_mapping` value and its family is
still visible in the listing (a vanished family may be a listing-style change
and keeps its pool entries). Capability-alias `model_mapping` values whose
mapped generation was delisted re-point to the newest still-listed
same-family member — retirement re-pointing is explicitly exempt from the
never-downgrade rule, because a live older generation beats a dead newest
one. Cheap/experimental variants are never chosen as re-point targets.

## Native protocol preference

Inside one capability tier, planned hops order native-protocol upstreams
ahead of cross-protocol supply: the Claude surface prefers `anthropic` hops,
Codex prefers `openai-responses`, Gemini prefers `gemini`, and chat prefers
`openai-chat`. Protocol affinity outranks configured priority within the tier
(verbatim request/response envelopes, typed errors, and untouched SSE justify
it) while the capability-tier boundary itself is never crossed; cross-protocol
supply stays in the chain behind the native hops. Explicit manual routes keep
their exact order. The `native_protocol_preference` setting opts out (explicit
`"0"`; on by default).

## Typed errors on native surfaces

Anthropic-protocol upstream errors relay their own documented `error.type`
(`rate_limit_error`, `overloaded_error`, `not_found_error`, `permission_error`,
`authentication_error`, `request_too_large`, `invalid_request_error`,
`api_error`) when the token is on the public allowlist; unknown spellings are
never synthesized. Gateway-raised Anthropic-surface errors map their status to
the same documented types (429 → `rate_limit_error`, 529 → `overloaded_error`,
401/403/404/413 accordingly), so Claude Code's typed retry logic works on both
relayed and local failures. The Responses surface relays the stable
`responses_state_unsupported` / `responses_feature_unsupported` /
`responses_invalid_request` codes verbatim, making state/feature gaps on
non-native hops identifiable instead of collapsing into `invalid_request`.

## Mirror role degradation

A mirror role request whose tier has no admitted supply is served from the
next lower-tier role of the same vendor family (fable→opus→sonnet→haiku;
sol→terra→luna) instead of failing; the response carries the requested role
in `x-aag-role-degraded`. Discovery projections stay honest — a role without
supply is still not advertised in `/v1/models` — and only a family with no
supplied tier at all raises `mirror_role_supply_unavailable`.

The existing public metadata worker retains verified cache contents on refresh
failure. Arena uses its [official public dataset](https://arena.ai/blog/arena-leaderboard-dataset)
through the [published dataset schema](https://huggingface.co/datasets/lmarena-ai/leaderboard-dataset).
[Models.dev](https://models.dev/) supplies capability, price and release metadata;
EvalPlus remains separate coding evidence. No unofficial Arena API is assumed.

Discovery uses the imported/configured wire protocol and its defined
authentication scheme for catalog reads. It does not infer arbitrary protocols
or authentication schemes by probing; it follows only routes that the
same-origin OpenAPI documents explicitly declare. A gateway that declares no
usable route or requires a different scheme still needs an explicit compatible
configuration.
Catalog-unsupported connections retain their configured models and report
unverified generation. This is not a successful inference check.

## Allocation policy

Hard capability constraints run before scoring. Exact model requests keep their
model identity. Aliases require a tier from the assessment chain; models with
neither evidence nor a documented prior stay pending and remain available for
explicit requests without becoming invented high-quality candidates.

The tier chain is, strongest first: manual override, Arena relative ranking,
EvalPlus coding evidence, same-family ordinal inheritance, known family-role
names, then documented name/metadata inference (`name_inference` basis: cheap
variant tokens, flagship tokens, vendor flagship priors for unranked lines
such as `glm`/`deepseek-chat`/`kimi`/`minimax`/`grok`, and models.dev
chat-capable metadata presence). Inference is the weakest accepted basis:
inside one preference band every evidence-assessed candidate orders ahead of
inferred supply, and the UI labels the basis separately.

Alias queues keep only the newest generations of each family lane: distinct
suppliers offering the same generation all stay, strictly older generations of
a family are trimmed once `family_generation_keep_n` (default 1, clamped to
1..=4) newest generations are filled, and cheap/experimental variants form
their own lane so a base id never evicts the family's fast variant.

Each alias ranks eligible offers with a weighted normalized utility:

- Quality: Arena relative rank, then comparable coding evidence, then the
  existing explicitly identified tier assessment.
- Cost: `1 / (1 + input_price + output_ratio * output_price)` using exact model
  reference prices. Conflicting hosting prices and unpriced variants stay unknown.
- Freshness: exponential decay of a verified day-precision release date. Missing,
  month-only and future dates receive no freshness bonus.
- Suitability: coding evidence/tool capability for tool tasks; general quality
  for ordinary tasks. Fast/review roles increase the cost weight.

Prices are catalog reference data, **not negotiated supplier prices or billed
costs**. Unknown prices receive no cost bonus. Stale retained metadata remains
usable and is labeled stale. Expensive models do not acquire capability from price.
Known-affordable higher-tier models receive earlier fallback preference for a
cheaper alias. Other assessed higher-tier models remain later fallbacks without
an affordability claim. The same model can therefore appear in several roles.

The `model_utility_policy` setting is validated JSON with these defaults.
`models_per_supplier` is retained for saved-policy compatibility; current fallback
retention does not use it as a queue cap:

```json
{
  "quality_weight": 0.50,
  "cost_weight": 0.20,
  "freshness_weight": 0.15,
  "suitability_weight": 0.15,
  "freshness_half_life_days": 180,
  "output_token_ratio": 3,
  "fast_reference_cost_ceiling": 3,
  "models_per_supplier": 2
}
```

Absent fields use defaults; unknown fields and out-of-range values are rejected.
The setting participates in routing revision identity. Every eligible fallback
pair is retained; duplicate mapped provider/model pairs are removed. Distinct
providers offering the same model remain separate. Tier fit, utility, static
priority and stable identifiers establish the base order; a diverse prefix
prefers different providers and models without discarding the remaining pairs.

Runtime observations are bounded, memory-only and keyed by provider plus model.
Success rate and completion latency use exponential smoothing. Failures cause a
60-second model cooldown; observations expire after one hour. The router chooses
one preferred eligible head while retaining the remaining fallback order. Existing
provider circuit breakers, admission limits, manual routes, exact-model semantics,
session affinity and no-failover-after-stream-commit remain in force. Session-bound
requests may intentionally prefer their established provider.

Model cooldown exclusion happens before supplier slot allocation, so a healthy
third candidate can fill a slot when both previously preferred models fail.

The Coding Agents queue uses the backend runtime preview, retains every model/hop
including unrated models, and shows directed fallback arrows. Runtime health and
saved allocation evidence are labeled separately. Visible pages refresh every
15 seconds and on focus. Observed request telemetry is distinct from a preview.

## Prompt workflow

Source and intermediate limits are now 32,000 and 64,000 characters. The existing
two user-facing stages remain compatible. Generation estimates input demand,
reserves at least the router's admission margin, and uses matching provider/model
limits when known. An unrelated small-context fallback cannot cap a capable target.

Output-limit termination, missing terminal metadata and unmatched fences trigger
bounded recovery. Visible content and original constraints survive continuation;
meaningful exact overlap is removed without deleting short mid-word fragments.
No-progress continuation stops with recoverable content. Normalized usage accounts
for all completed calls. The shared budget is six calls, 32,768 allocated output
tokens, three generation attempts and 90 seconds per call.

For gateway targets, stalled continuation can move to the next configured,
capability-eligible candidate within the same attempt budget. The alternate keeps
router admission and accounting; shared routes are not mutated. Explicit provider
targets stay on their selected provider, and exact model selectors never switch
to another model.

Long or recovered English outputs receive an economical `alias:haiku` review,
with one `alias:opus` escalation for unusable/uncertain review and at most one
correction. A failed correction preserves the complete original draft. Short
outputs explicitly report that external review was not requested.

IPC includes `complete`, `failureCode`, `attempts` and `reviewStatus`. The UI keeps
the input and partial work, labels incomplete output, and presents progress.
The generation path remains nonstreaming; this is not a claim of arbitrary SSE
transport-fragment reconstruction. Unknown model limits remain unknown. Recovery
can still stop at a genuine context or resource limit and returns its specific
failure status rather than silently truncating the result.

## Verification boundary

Deterministic fixtures cover catalog errors and pagination, CC Switch import,
supplier reconciliation, allocation/reuse, full graph parity, output continuation,
budgeting, review escalation and partial preservation. Browser fixtures use
synthetic metadata. Current command results belong in `PROGRESS.md`; neither
fixtures nor public-source reads establish live inference, installed desktop
acceptance, real supplier pricing, signing or deployment.
