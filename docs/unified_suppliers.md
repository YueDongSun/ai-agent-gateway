# Unified suppliers, four-protocol gateway, and capability routing

AAG now owns the whole path from "where do I buy models" to "which model
answers this request". This document is the operator-facing contract: what a
supplier is, how the single local port serves every client protocol, how
capability aliases are planned, and what is deliberately not done yet.

## 1. The model of the system

| Concept | Meaning |
| --- | --- |
| **Supplier** | One vendor you pay. Holds a name, a base URL, an API key, and one or more endpoints. |
| **Endpoint** | One protocol an upstream speaks at that supplier: `anthropic`, `openai-chat`, `openai-responses`, or `gemini`. A supplier can expose several (same account, different protocol paths) or one. |
| **Model offer** | A model a supplier endpoint claims to serve, with optional capability facts (tools, images, context window, max output) and an optional manual tier override. |
| **Projection** | The link from a supplier endpoint to the legacy `proxy_providers` tree, so one supplier edit drives every client surface at once. |
| **Capability tier** | 0 = fast, 1 = balanced, 2 = strong, 3 = strongest. Assessed from public evidence or set by hand. |
| **Selector** | What a client asks for: an exact model id, a capability alias, or an explicit namespace. |

Supplier configuration lives in the `suppliers` / `supplier_endpoints` /
`supplier_models` / `supplier_runtime_projections` tables (schema v21). The
legacy `proxy_providers` and `routes` tables stay valid and are still edited
from the old provider/route pages — supplier endpoints project into them
rather than replacing them.

Supplier rows also carry optional `icon` / `icon_color` display columns
(schema v23): the supplier page renders them as the same designed tiles as
the provider pool, and managed projections mirror them. They are
presentation-only and never routing inputs.

## 2. One local port, four client protocols

The gateway binds one host:port and serves every client protocol on it:

| Client | Base URL | Endpoint |
| --- | --- | --- |
| Claude Code / Anthropic SDK | `http://127.0.0.1:<port>` | `POST /v1/messages` |
| Codex CLI / OpenAI Responses | `http://127.0.0.1:<port>/v1` | `POST /v1/responses` |
| OpenAI Chat Completions | `http://127.0.0.1:<port>/v1` | `POST /v1/chat/completions` |
| Gemini CLI / Google SDK | `http://127.0.0.1:<port>` | `POST /v1beta/models/<model>:generateContent`, `:streamGenerateContent`, `:countTokens` |
| Model discovery (all clients) | `http://127.0.0.1:<port>/v1` | `GET /v1/models`, `GET /v1beta/models` |

AAG decodes each ingress into a canonical routing request, retains private
native source envelopes, selects an eligible supplier endpoint, and encodes the
answer for the original client. These four ingress codecs share one planner;
choosing a role does not require the supplier to speak the client's protocol.
Cross-protocol conversion still has explicit representability limits.

Anthropic-to-Anthropic routing preserves the original body/events, thinking
signatures and tool ordering. Native Responses routing preserves the source
envelope subject to the upstream's supported state model. Gemini-to-Gemini
routing preserves native request bodies, response objects and SSE data objects,
including opaque signatures and native controls; the routed model belongs in
the upstream URL. Internal source envelopes never leak onto a foreign wire.

For a foreign Gemini hop, AAG admits only the supported text/image/function-tool
projection. Function calls retain their identity, name and arguments while
Gemini-only signature metadata stays off the foreign wire. Unsupported native
controls, thought text or text-block signatures, media types, or tool-choice constraints return
`gemini_feature_unsupported` before a foreign upstream is called. Foreign
clients likewise reject native Gemini output they cannot represent, including
multiple candidates and native-only media/code parts. Native Gemini callers
retain those source objects rather than passing through the foreign projection.

`GET /v1/models` lists two kinds of entries, marked with a `kind` field:

* `kind: "exact"` — a concrete model id. Requesting it always means that model.
* `kind: "alias"` — a capability selector (`fable`, `opus`, `sonnet`, `haiku`,
  their explicit `alias:<name>` forms, or an admitted client role below).
  Requesting it means "the best available eligible model for this tier".

The provider workspace opens first. Add a provider, select its supported API
formats, enter a shared URL or per-format URLs, then fetch and select models
before saving. Discovery reads the provider's catalog without persisting the
draft or generating a model response. A failed or empty catalog remains visible;
manual model entry is available when the provider has no catalog endpoint.

### Additive URL discovery

The existing EN/CN switch controls the discovery help, catalog-health labels and
latest results. See [English and Chinese discovery explanations](supplier_workspace.md#english-and-chinese-discovery-explanations)
for the visible controls and the distinction between localized display text and
unchanged backend records.

A saved supplier can gain a protocol it did not declare. Saving or importing
revalidates only the requested suppliers: each is read by ID and every projection
has its own 20-second bound, so a corrupt or slow entry is reported without
cancelling later ones. After a successful catalog refresh, AAG runs discovery
once per configured source context and reads both same-origin OpenAPI 3.x
documents derived from the `/models` root and the configured origin. It accepts
only POST routes the documents declare for `openai-chat`, `openai-responses`,
`anthropic` or `gemini`, merging candidates and reporting per-document failures.
The search is bounded to five seconds, 512 KiB, 512 paths and 16 candidates;
redirects, remote servers, templated server URLs, non-HTTP(S) targets, and URLs
with embedded credentials, query strings or fragments are refused. Catalog fetch
preserves the configured query parameters; only URL expansion skips a
query-bearing source, and catalog candidates drop fragments.

A route counts as validated when a GET answers `405` with `Allow: POST` and a
sibling-path negative control relative to the probe URL answers `404`. A
non-Gemini catalog may be empty; Gemini needs at least one listed model for its
probe route. For Gemini, the declared root must equal the runtime-normalized
`.../models/<model>:generateContent` route, otherwise validation fails with
`url_validation_runtime_path_mismatch` before I/O. Every Gemini catalog model ID
must also normalize to one path slot; an empty, `.`, `..`, or slash, backslash,
query, fragment or percent-bearing ID fails `url_validation_model_path_invalid`
before the probe.

A validated missing protocol is appended with its models and listing health in
one generation-checked transaction. If the candidate URL equals the supplier
base, the endpoint reuses that base rather than storing a duplicate; different
protocols may share an API root. Existing endpoints, alternate URLs, models and
admission records are never replaced, and only an existing protocol is a no-op,
so a repeated refresh changes nothing. Every skipped or failed attempt appears
as its own report entry
(`supplier_url_skipped:<protocol>:<stage>:<reason>` or
`supplier_url_failed:<protocol>:<stage>:<reason>`), including per-document
search failures, while the rest of the supplier's refresh still completes.

Suppliers excluded or skipped before any catalog or discovery request — disabled
supplier, endpoint, alternative or provider; no configured endpoint;
auto-tracking off; non-probeable URL type; invalid URL; inactive, unavailable or
expired subscription; obsolete configuration; failed screening; or recent
non-available catalog health other than `catalog_invalid` inside the 300-second
window — are reported as `supplier_import_excluded:` or
`supplier_import_skipped:` and keep their current inventory. Preflight may still
refresh subscription status through the existing owner. URL discovery creates no
new supplier, source binding or admission record; the added endpoint stays
listing-only with `inference_status: "unverified"`, and discovery does not change
that status. A corrupt supplier-owned projection is isolated as a per-entry
failure and does not block later suppliers; a corrupt legacy row keeps its prior
failure contract.

Saving with enabled models starts the local gateway when needed and waits for
the current routing snapshot to apply. The enabled model inventory routes exact
model requests automatically; no route rows are required. AAG selects an enabled
default model and writes its endpoint and local token through the existing
backed-up Codex and Claude mirror adapters. Claude Desktop receives an isolated
third-party profile with static AAG authentication through
`/claude-desktop`. When assessed mirror roles are enabled, discovery presents
the four Claude role IDs below and keeps the requested role through planning,
so the whole eligible failover chain remains available. The role label is a
gateway policy choice, not an upstream-model identity. Legacy exact-model
catalogues retain their compatible selectors when role mode is disabled.
The Desktop edge requires a valid local bearer and never uses Claude OAuth
passthrough. Native account homes remain separate.

AAG also prepares managed gateway records for its ten supported client targets.
Their existing launch adapters apply configuration when opened. These records
never enter upstream failover queues and carry only the local AAG credential.
An explicit later direct-provider selection is preserved. Deleting a supplier
or changing enabled models runs the same default-model and client reconciliation.
An already saved mutation whose automatic setup fails is reported as persisted
with a setup error, so retry does not create a duplicate supplier. Manual
snippets and routing controls remain under advanced setup for external clients
and optional overrides.

Codex loads its generated catalogue through `model_catalog_json`, with the
default and available choices filtered against the current runtime routes and
pin. Its translated compatibility profile retains ordinary function tools and
MCP namespaces; dedicated freeform apply-patch, multi-agent namespace tools and
native hosted search are disabled. A stale running mirror is closed gracefully
before configuration reload, or AAG reports that it must be quit if graceful
closure cannot finish. Native account directories remain separate.

### Client-facing role IDs

With `mirror_model_roles_enabled=1`, these compatibility names request tiers
through the same frozen planner on all four ingress protocols:

| Role ID | Requested tier | Desktop catalogue |
| --- | --- | --- |
| `claude-fable-5` | 3, strongest available band | Claude |
| `claude-opus-5` | 2, strong available band | Claude |
| `claude-sonnet-5` | 1, balanced available band | Claude |
| `claude-haiku-5-5` | 0, fast available band | Claude |
| `gpt-5.6-sol` | 3, strongest available band | Codex |
| `gpt-5.6-terra` | 1, balanced available band | Codex |
| `gpt-5.6-luna` | 0, fast available band | Codex |

The ID remains the client's choice; the planner chooses actual offered upstream
IDs after checking tools, images, context and output requirements. It prefers
the requested band, permits visible downgrades to lower bands, and orders
eligible supply by its assessed priority. An unranked selected default is not
copied into every role queue. Empty eligible supply is unavailable, and an
actually offered bare ID still means that exact model. These names do not claim
that AAG owns a native Claude or OpenAI model subscription.

A role request whose tier has no admitted supply degrades to the next
lower-tier role of the same vendor family instead of failing, and the response
carries the requested role in `x-aag-role-degraded`; only a family with no
supplied tier at all reports `mirror_role_supply_unavailable`. `/v1/models`
advertises these unified role names; a legacy route-alias spelling of a tier a
role already covers is suppressed from the listing unless the id is genuinely
offered as an exact model (requests for the legacy spelling still resolve).

Native protocol preference is a hard ordering inside one capability tier: an
anthropic hop leads its tier on the Claude surface (and likewise
`openai-responses`/`gemini`/`openai-chat` on their native surfaces), ahead of
better-priority cross-protocol supply, while the tier boundary itself is never
crossed and cross-protocol supply stays in the chain as fallback. It does not
select a weaker capability tier merely to avoid translation, and explicit route
order remains intact. The `native_protocol_preference` setting (explicit `"0"`)
opts out and restores priority ordering inside the tier.

The explicit local provider reset preserves a complete private SQLite recovery
snapshot, clears supplier and legacy provider state plus dependent routes, and
disables automatic reimport and default seeding. Listener settings, mirror homes,
native accounts, and unrelated application data remain intact.

### Source-authoritative mirror instructions

Instruction synchronization is enabled by default: only a stored
`mirror_instruction_sync_enabled=0` opts out (a stored `"1"` keeps its legacy
meaning; nothing is rewritten on upgrade). One application-owned worker runs
with a five-second reconciliation interval and the same admission gate used at
launch. The originals remain authoritative for these files and directories:

| Client | Instruction files | Directories |
| --- | --- | --- |
| Codex | `AGENTS.md`, `AGENTS.override.md` | `agent_policy/`, `rules/`, `skills/`, `agents/`, `prompts/` |
| Claude | `CLAUDE.md`, `CLAUDE.local.md` | `skills/`, `agents/`, `commands/`, `rules/`, `prompts/` |

Additions, edits and deletions propagate after a complete valid source inventory.
Obsolete or independently added mirror instruction files in these roots are
backed up and pruned. Recovery copies stay outside client discovery. An
unavailable source does not authorize deletion, and vendor `skills/.system`
remains owned by the client.

Codex has a supplemental instruction-only plugin projection under
`skills/aag_plugin_instructions/`. It reads enabled native plugin configuration
and validated package manifests, copying only declared skill trees and their
resources when the actual installed version is unambiguous. Skill names already
provided by user/native skills, shared skills, or configured mirror plugins are
excluded. This does not copy a plugin's servers/account state or activate it in
the mirror.

A package cache `latest` junction is admitted only as a duplicate reference to
an already enumerated ordinary direct version directory within the same
package. No external target, chained alias, directory-name ranking, mtime guess,
or `latest` pointer may choose between multiple actual versions. Ambiguity blocks
the supplemental projection and retains the last good inventory.

The configuration merge owns only Codex `agents.enabled` and
`agents.max_concurrent_threads_per_session`. It preserves the rest of the mirror
TOML and uses a caller-bound snapshot plus backup at commit. Routing, local
authentication, permissions, MCP, compatibility features and plugin activation
continue through their separate owners. This contract describes reconciliation
scope; prepared resource counts alone do not establish applied-file or native
consumer parity.

### Skills single source of truth

Trusted shared roots (`~/.cc-switch/skills`, `~/.agents/skills`) are imported
into the AAG-owned SSOT at `~/.aag/skills/` before each reconciliation pass:
every skill directory gets a deterministic content hash, bounded copies replace
changed content, and the `skills` DAO catalog records the hash, source root and
per-app enable flags. A skill removed from its source (or whose whole shared
root disappeared) is pruned from the SSOT and the catalog together; a failed
import keeps the previous copy and manifest entry. Mirror instruction sync
projects skills from the SSOT root instead of the shared roots directly, and
falls back to the raw shared roots only if the SSOT reconcile fails. Windows
needs no symlinks or developer mode — projection is a copy plus hash
verification. First-run seeding also covers the Claude mirror
(`CLAUDE.md`, `skills/`, `agents/`, `commands/`) through the same shared root
vocabulary as continuous sync; the seed set is always a subset of what sync
manages.

## 3. Selectors: exact ids versus capability aliases

Three selector forms are accepted on every surface:

| Selector | Meaning |
| --- | --- |
| `claude-opus-4-8` | An exact model. The request is only ever served by an offer of that same canonical model — never a substitute. |
| `fable` / `alias:fable` | A capability alias. AAG picks a qualifying model at or below the requested tier, preferring the exact tier, and records any downgrade. |
| `precise:claude-opus-4-8` | Forces the literal model id, even when the name looks like an alias. |

Planning order for an alias: model semantics → hard capability filter (tools,
images, context window, output budget) → tier selection → upstream priority →
bounded retry. Hard requirements are never silently dropped: an alias that
cannot satisfy them returns an error instead of degrading the request.

### Where the capability facts come from

A supplier can state a model's capabilities itself (tools, images, context
window, output budget). Whatever it leaves unknown is filled from the public
catalog (models.dev), so an offer can qualify on public metadata alone:

* a supplier's own facts always win — a manual statement is never overwritten;
* catalog facts are model-level, not supplier-level: they say what the model
  can do, never that a supplier serves it;
* a model the catalog does not know stays unknown, and an unknown capability
  never satisfies a hard requirement — such an offer is skipped for an alias
  instead of being tried and failing upstream;
* a vendor-prefixed id (`anthropic/claude-fable-5.1`) resolves to the
  catalog's bare id (`claude-fable-5.1`).

Errors are explicit and stable:

| Code | Meaning |
| --- | --- |
| `model_precise_unavailable` | No offer of that exact model exists. The request fails instead of being served by a different model. |
| `model_alias_no_candidates` | No offer qualifies for the requested tier under the request's hard requirements. |
| `model_alias_unknown` | The alias name is not a known capability alias. |
| `model_selector_invalid` | The selector namespace is empty or malformed. |
| `mirror_role_supply_unavailable` | An enabled client role has no admitted credentialless supply; no selected-default queue is invented. |
| `gemini_feature_unsupported` | Native Gemini request semantics cannot be represented by the selected foreign protocol. |
| `responses_upstream_state_unsupported` | A stateless MiniMax Responses endpoint cannot honor requested response/conversation linkage. |

### Bare names remain exact when actually offered

A bare name that is actually offered (for example `claude-opus-4-8` is in
the supply list) is an **exact** call, even when its spelling also resembles a
capability alias. This protects the rule that an exact model request must
never become another model. Use `alias:opus` to request capability routing
unambiguously, or `precise:claude-opus-4-8` to request an exact model explicitly.
Explicit routes and provider pins cannot bypass these selector constraints.
Public metadata lookup may strip an organization prefix; exact routing does
not use that lookup rule to substitute a differently named model.

## 4. Capability tiers and evidence

Tiers use declared public evidence and optional manual overrides. Refresh does
not run paid model generations or an automatic capability benchmark.

| Source | Evidence used | Limit |
| --- | --- | --- |
| [models.dev](https://models.dev/api.json) | Family, modalities, tool support, context and output limits | Capability facts and price metadata do not measure model strength. |
| [EvalPlus](https://evalplus.github.io/results.json) | HumanEval+/MBPP+ pass@1 coding scores, with suites and conditions preserved | Coding performance is a separate fallback, not general intelligence. |
| [Official LMArena dataset](https://huggingface.co/datasets/lmarena-ai/leaderboard-dataset) | Published overall text ranking in `latest` / `text_style_control`, including rank, rating interval, votes and condition | The routing bands are a relative operational policy, not a universal intelligence scale. |

Precedence is manual override → exact Arena evidence → comparable coding
evidence → reliable same-family ordinal evidence → known family-role hint →
pending. Unknown evidence is never zero-filled or assigned to Haiku merely
because it is unknown. Arena matches preserve model versions, variants and
release dates. Friendly spelling and an organization prefix may normalize;
thinking conditions remain visible. An unqualified ID with several published
conditions uses a conservative result and exposes the ambiguity.

The runtime compares distinct Arena-rated model identities in its admitted,
enabled credentialless supply. Multiple supplier/protocol rows for one rated
identity count once. It orders those models by published rank and assigns the
four available-model quartile bands using
`available_model_rank_quartiles_v1`. The original published rank and leaderboard
size remain separate from `comparedOfferedModels`, the current cohort size.
Changing the offered cohort can therefore change a relative routing band
without changing the model's published Arena rank. Standalone public-model
lookup may retain the published-leaderboard quartile policy when no offered
cohort applies.

`MiniMax-M3.1-Flash` has no exact row in the inspected 2026-10-22 Arena snapshot.
It does not borrow the row for `MiniMax-M3`, `MiniMax-M2.7`, or another MiniMax
variant. Its Arena result remains absent. A separately supported manual or
other evidence path may still assess it; missing Arena evidence alone supplies
neither a rank nor a tier.

The bounded Arena collector first reads complete category statistics, then
scans row pages until the independent overall count is met. It processes the
whole final page, rejects excess/duplicate/incomplete rows, and requires one
publication date. Its limits are 200 pages, 20,000 dataset rows, 8 MiB cumulative
input, and 90 seconds. Statistics and pages do not share an atomic remote
revision token, so matching totals and dates provide best-effort consistency.
See the [official statistics API contract](https://huggingface.co/docs/dataset-viewer/statistics).

When mirror roles are enabled, one application-owned worker checks sources at
startup and hourly. A missing, errored, or older-than-24-hour cache is due for
refresh; explicit refresh uses the same admission gate. A fetch or validation
failure retains the last good document, marks the source stale with a safe error
code, and retries on a later check. Parseable retained evidence can still inform
an assessment, with its stale/error status attached. No partial ranking replaces
the good cache, and no external watcher or notification job is created.

A catalog refresh is a routing input: it moves the routing fingerprint, so new
evidence hot-applies to new requests without restarting the listener.

## 4a. When AAG switches suppliers

A request walks the planned chain in order and stops at the first answer.
The failures that advance the chain are:

| Failure | Behaviour |
| --- | --- |
| Network error, ordinary 429, 5xx, or recognized model/rate rejection | Try the next eligible hop; another model/protocol on the same account remains possible. |
| Explicit account/plan allowance exhaustion | Skip remaining model/protocol hops sharing the same configured credential and HTTP origin, then try independent eligible supply. |
| Explicit tool-capability HTTP 400 before streaming output, for a request with tools | Try the next eligible hop; this does not exhaust account quota. |
| Ordinary 4xx other than 429, including 400, 401, 402, 403, 404, and 422 | Stop and return the failure. Account, permission, model-availability, and request errors do not authorize a hidden supplier switch. |

The per-supplier circuit breaker still applies: a supplier that keeps failing
is skipped until its cooldown expires, so one dead supplier cannot consume
every request.

The structured exception uses error types/codes, not prose or request echoes.
MiniMax `2056` and `2067`, or explicit `usage_limit_reached` /
`insufficient_quota` codes, identify account allowance exhaustion. MiniMax
`2062`, generic rate-limit errors, and ordinary 429 responses are retriable but
do not exhaust every model on the account. Authentication errors remain
terminal. The shared quota scope groups the
resolved API credential and HTTP origin across protocol paths and duplicate
supplier rows. It is request-local and stores opaque equality identities in the
router. AAG does not infer a five-hour reset time or claim remaining allowance.
See the [MiniMax error-code reference](https://platform.minimax.io/docs/api-reference/errorcode).
Streaming may advance only before the first committed output; a later quota
event ends that stream rather than replaying it on another supplier.

### Spread modes

The configured routing mode decides the starting order of a queue-built chain
before failover begins:

| Mode | Behaviour |
| --- | --- |
| `order` (default) | Configured priority; the next supplier is tried only after a retriable failure. |
| `rotate` | Each chain member takes a turn as the first attempt; failover walks the rotated order. |
| `weight` | Smooth weighted round-robin over the members' weights; weight 0 is standby-only, still present as failover. |
| `usage` | Least recorded traffic first, over a tumbling one-hour window of prompt and completion tokens AAG observed on the success path. Ties keep the configured order. |

Spread modes reorder only chains the queues build — same-model queues,
last-chance chains, and `precise:` matches. Explicit per-model routes and
planner-assessed alias chains keep their order in every mode: a hand-ordered
route is a priority statement, and the planner's order is its tier
assessment. `usage` counts traffic AAG itself recorded; it is not vendor
quota. Allowance-aware pacing is design-only in
`docs/reviews/quota_substrate_plan_2026_10_22.md`.

### Projected upstreams are supplier-owned

Each supplier endpoint that has a legacy projection owns the corresponding
`proxy_providers` row: supplier saves rewrite its name, type, key, URL,
models, enabled state, extra headers, and weight. The providers page therefore
hides edit, delete, and the enable toggle for those rows and labels them as
supplier-managed. The ownership is enforced, not just presented: deleting the
row directly while a projection references it fails with
`AAG_SUPPLIER_PROJECTION_IN_USE`. Deleting the supplier or endpoint removes
the projection (and the managed row) with it, but only while no route still
hop-references that row; a route that still points at it makes the delete fail
with `supplier_in_use` until the route is repointed.

## 5. cc-switch import

Import is a read-only preview followed by an explicit apply:

1. `supplier_import_preview` reads the cc-switch database and returns a plan —
   it writes nothing.
2. `supplier_import_apply` re-checks the source fingerprint and the target
   configuration generation, then applies the plan in one transaction.

Protocol comes from the source's declared metadata (`apiFormat`, the active
Codex `wire_api`), never from guessing at a URL. Rows that cannot be imported
are listed with a stable reason code; healthy rows still import. Re-applying
the same plan is a no-op.

**Official accounts are not forwarded.** A cc-switch entry that only carries a
managed/official subscription is imported as an association with status
`official_auth_pending`: no refresh token is read or copied, nothing is
refreshed, nothing is written back, and no native Claude/Codex/Gemini
credential file is scanned. cc-switch itself is never modified.

## 6. Session stability

A request may carry an explicit session marker:

* a bound-source Responses id (`previous_response_id`),
* the Anthropic `metadata.user_id`,
* the OpenAI `user` field.

When one is present, the provider that served the session is preferred within
the planned chain, and the session rebinds only when a hop fails. Per-request
ids, client IPs, and API keys are never treated as session identities, and
requests without a marker make no stickiness claim at all — the planned order
is used unchanged. The table is memory-only, capacity-bounded, and expires
entries after 30 minutes.

Native MiniMax Responses endpoints have an additional stateless boundary.
On the recognized MiniMax API hosts, a nonempty `previous_response_id` or
non-null `conversation` is rejected before the request is sent, using
`responses_upstream_state_unsupported`. Send the complete materialized history
in `input` for a stateless turn. `store` does not establish upstream conversation
storage. Other Responses providers keep their supported native linkage behavior;
the gateway does not erase linkage merely to make a different backend accept it.

## 7. Traceability

Each request records a metadata-only timeline entry: request id, surface, safe
model reference, status, duration, failover flag, the capability decision
(`planKind`, `planTier`, `planDowngraded`, `planBasis`, `planReason`), and the
routing generation the request was admitted under. Codes only — never
prompts, responses, tool payloads, credentials, URLs, or raw upstream errors.
The supplier page's trace panel and the global diagnostics dialog both show it;
streaming requests settle their usage and session binding when the stream
ends.

## 8. Hot-apply behaviour

Supplier, model, route, provider, pin, and catalog changes all move the
routing fingerprint and are applied to new requests without restarting the
listener. In-flight streams keep the snapshot they started with. Endpoint
connection pools and circuit breakers survive a rebuild; only a change of
endpoint, credential identity, or runtime parameters rebuilds the affected
instance. A failed rebuild keeps the previous snapshot and reports the failure
through the proxy status, which distinguishes saved / pending / applied /
failed in the UI.

## Intelligent allocation and recovery follow-up

The current utility policy, discovery reconciliation, model-level health and
complete Coding Agents queue are documented in
[provider discovery and intelligent routing](intelligent_routing.md). That policy
extends the earlier tier-only ordering above: exact-priced affordable models may
serve multiple roles, each supplier contributes up to two distinct candidates per
role, and runtime health selects a preferred head with ordered fallback. Saved
allocation evidence and admitted runtime order remain distinct in the UI.
