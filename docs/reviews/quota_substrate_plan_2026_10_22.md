# Quota-aware scheduling substrate plan

Planning date: 2026-10-22. Baseline: AAG branch `feat/unified_suppliers` at
`3d059fe` (schema v23); the observed-usage mode landed in `3edf549` after the
first draft. Read-only references: the Magpie reference checkout (Go, Wails)
and the cc-switch reference checkout (Rust/Tauri). This document plans a
substrate; it does not describe implemented allowance behavior. Existing tests
cited in sections 1-5 are real at the baseline; the tests named in section 6
are planned.

## Sources

| Project | Role in this plan | Shape |
|---|---|---|
| AAG | target architecture, all behavioral claims citation-checked | Rust (Tauri 2) + React |
| Magpie | allowance model, refresh/staleness rules, pace and sink algorithms | Go |
| cc-switch | display-only quota precedent and domestic coding-plan endpoints | Rust (Tauri 2) + React |

AAG claims below were read from this checkout at the baseline commit. Magpie
and cc-switch claims are read from their reference checkouts. No behavior was
exercised against a real vendor quota API in this round.

## 1. What quota-aware scheduling means in AAG

### 1.1 What exists today

| Piece | Current state | Where |
|---|---|---|
| Routing modes | `Order` (default), `Rotate`, `Weight`, `Usage`; queue-built chains only | `src-tauri/src/proxy/router.rs:32-67` |
| Spread implementation | per-chain rotation keyed by provider list; SWRR by `proxy_providers.weight`; observed-token buckets for `Usage`; bounded state | `src-tauri/src/proxy/router.rs:92-125`, `:733-849` |
| Chain construction | per-surface chains from `providers.in_failover_queue`, `sort_index` order | `src-tauri/src/services/proxy_runtime/queues.rs:189-234`, `src-tauri/src/database/dao/providers.rs:77-98` (ORDER BY at `:86`) |
| Mode selection | `app_meta` key, decoded leniently (unknown → `order`) | `src-tauri/src/database/dao/settings.rs:92-100`, `src-tauri/src/services/proxy_runtime/snapshot.rs:70-76` |
| Mode IPC | read/set commands; unknown values rejected at the setter | `src-tauri/src/commands/proxy.rs:189-230` |
| Planner chains | tier-ordered candidates, deterministic, never spread | `src-tauri/src/services/proxy_runtime/planner.rs:141-218`, `src-tauri/src/services/model_catalog/select.rs:7-21` |
| Usage recording | one row per answered request, best effort, stream settles at end; the same success path feeds the in-memory usage ledger | `src-tauri/src/proxy/usage_recorder.rs:109-155`, `src-tauri/src/proxy/router.rs:1004-1026`, `:1408-1431` |
| Usage read surface | summary / by-model / by-provider IPC | `src-tauri/src/commands/usage.rs:51-75` |
| Hot apply | snapshot rebuild on fingerprint change; in-flight requests keep their snapshot | `src-tauri/src/services/proxy_runtime/live.rs:69-128`, `src-tauri/src/services/proxy_runtime/snapshot.rs:31-107` |

There is no allowance, quota-window, or vendor-usage code anywhere in the Rust
tree at this baseline; the only mention is the routing-mode comment that
Magpie's `pace` mode stays out of scope until AAG has vendor allowance windows
to pace against (`src-tauri/src/proxy/router.rs:32-36`). The gap
analysis ranked quota-aware scheduling as the deepest remaining Magpie
differentiator and placed it after catalog work
(`docs/reviews/ccswitch_magpie_gap_analysis_2026_10_14.md:104-130`,
`:189-207`).

### 1.2 Operational definition

Quota-aware scheduling in AAG means: **an ordering input for queue-built
chains, computed from vendor-declared allowance windows, that never removes a
member from a chain and never blocks or rejects a request.**

- It applies where `spread_chain` applies today — same-model queue matches,
  last-chance surface chains, and the `precise:` queue fallback
  (`src-tauri/src/proxy/router.rs:476-497`, `:503-542`, `:587-599`).
- It does not apply to explicit per-model routes, planner-assessed alias
  candidates, or pins. A hand-ordered route is a priority statement and the
  planner's order encodes tier assessment
  (`src-tauri/src/proxy/router.rs:32-42`, `:562-586`, `:642-649`; tests
  `explicit_routes_are_never_reordered` at `:3639-3666` and
  `usage_mode_never_reorders_explicit_routes_or_planner_plans` at `:3831-3878`).
- Quota is not health. The circuit breaker tracks whether a provider answers
  (`src-tauri/src/proxy/router.rs:851-892`); quota asks whether a healthy
  provider has allowance left for this model in this window. The two compose:
  a spent provider stays in the chain behind the breaker, tried only when the
  others cannot answer.
- The only enforcement point remains the vendor: a 429 or an explicit quota
  refusal is authoritative. A local reading at 100% reorders; it never returns
  an error on its own. This preserves the failover contract in
  `docs/unified_suppliers.md:125-139`.

### 1.3 Relationship to the observed-usage mode

The parallel observed-usage track landed after the first draft (commit
`3edf549`, "feat(proxy): add usage-aware routing mode"). It is
`Routing_mode::Usage` (`src-tauri/src/proxy/router.rs:59-66`), parsed from the
`usage` setting (`:73-90`): queue-built chains rank members by the token
traffic AAG itself recorded for them, lowest first. Its store is an in-memory
`Usage_ledger` (`:152-226`) of per-provider tumbling one-hour buckets
(`USAGE_WINDOW`, `:127-136`) that count prompt + completion tokens only
(`:187-211`), bound the provider set with a wholesale reset (`:138-142`,
`:194-199`), are carried across snapshot rebuilds
(`src-tauri/src/services/proxy_runtime/snapshot.rs:89-94`), and are never read
from SQLite. It is fed on the same success path as `usage_recorder`
(`src-tauri/src/proxy/router.rs:1004-1026`, streaming settle `:1408-1431`);
`usage_records` remains the reporting table
(`src-tauri/src/database/schema.rs:186-216`). This plan treats the mode as a
landed dependency with a fixed contract and defines the allowance substrate so
the two share one pipeline instead of competing.

| Question | Observed usage (landed) | Allowance (this plan) |
|---|---|---|
| What it measures | tokens AAG itself served: prompt + completion (`Usage_ledger::record`, `src-tauri/src/proxy/router.rs:187-211`) | what the vendor says is left in a window |
| Source of truth | in-memory `Usage_ledger` fed on the answered-request path (`:1019-1025`, `:1429-1431`); no schema change | vendor usage endpoints, response headers, 429 hints, manual entry |
| Availability | always after traffic in the running process; history is lost on restart | may be absent, stale, or family-specific |
| Keying | provider id | credential identity and family, mapped to provider ids at snapshot build (section 2.3) |
| Failure mode | none beyond "no traffic yet"; a restart forgets history | last reading kept; refresh failure never blocks a request |
| Where it is consumed | the `usage` arm of `spread_chain` (`:737-752`) | the same hook |

Non-duplication rules:

1. The allowance store never writes `usage_records` or the usage ledger; the
   observed track never reads vendor data. One fact has one owner.
2. Both feed one ranking function: allowance share is the primary key when
   present, observed usage breaks ties among equal allowance, and configured
   order is the final tie-breaker. This mirrors Magpie's `LeastUsed`, which
   sorts by vendor allowance share and then by locally served tokens
   (`internal/gateway/routing.go:1122-1149`).
3. Unknown allowance is neutral (section 5.3), not "fresh and free", and is
   not silently replaced by observed usage. An unread provider must not
   leapfrog every read one, and must not be starved behind `low`/`spent`
   members either.
4. `Routing_mode::Pace` (this plan) and `Routing_mode::Usage` are separate
   user-visible modes. A future `Smart` mode can combine them; v1 does not.

## 2. Allowance data model

### 2.1 Window and reading types

Ported from Magpie's `Allowance`/`Limit` (`internal/provider/routing.go:147-171`)
with AAG naming.

| Field | Meaning | Magpie origin |
|---|---|---|
| `window_ref` | stable identity of the window across readings (vendor cap id or a stable name) | `Limit.Name` (`routing.go:162`) |
| `used_pct` | share used, 0–100, clamped | `Limit.Used` (`routing.go:152`) |
| `amount`, `of`, `unit` | the vendor's own count, when it counts that way (credits, requests) | `Limit.Amount/Of/Unit` (`routing.go:158-159`) |
| `span_secs` | how long the window runs; absent when unknown | `Limit.Span` (`routing.go:154`) |
| `resets_at` | epoch seconds of the next reset; absent when unknown | `Limit.Resets` (`routing.go:153`) |
| `model_scope` | which models the window counts: whole account, or a model family | `Limit.Model` + `applies` (`routing.go:155`, `:217-219`) |
| `scope_kind` | whole-account vs pooled/model-family; decides "this model is out" vs "the account is out" | `Allowance.Pooled` (`routing.go:355-373`) |
| `source` | `manual`, `header`, `usage_endpoint`, `refusal_hint` | new; Magpie sources are per-provider code |

Derived queries per `(credential, model)`, all pure functions of `(windows, now)`:

| Query | Definition | Magpie origin |
|---|---|---|
| `used` | max `used_pct` over windows that apply to the model; a window whose `resets_at` has passed is empty again | `For` (`routing.go:225-248`) |
| `renews` | the applying windows' resets, longest span first | `For`/`Renewal` (`routing.go:238-245`, `:302-333`) |
| `fullest_reset` | reset of the window the `used` share belongs to | `Fullest` (`routing.go:250-271`) |
| `pace` | minimum over budget windows of `(100 - used) / max(hours_until_reset_or_span, 1)`; when only sub-day windows exist, the tightest short window; an unstarted window is taken to run its whole span from now | `Pace` (`routing.go:411-464`) |
| `pooled` | every window that is full at the spent threshold is model-scoped, so the model is out while the account is not | `Pooled` (`routing.go:355-373`) |

`budgetSpan` is 24 h: a window shorter than a day is a rate cap, not a budget
(`internal/provider/routing.go:378`). The spent threshold is Magpie's
`SpentShare = 98`, the low threshold 90 (`internal/provider/codex_switch.go:47-50`,
`internal/gateway/routing.go:945-946`).

AAG model scoping uses canonical identity and family keys rather than Magpie's
substring match: `canonical_model_id` and `family_of_id`
(`src-tauri/src/services/model_catalog/identity.rs:26-28`, `:129-131`). A
window scoped to `claude-opus` matches `claude-opus-4-8` and its dated
variants, never a bare substring hit inside an unrelated id.

### 2.2 Freshness and staleness semantics

Ported from Magpie's `usedCache` and `quota_last.go`:

| State | Meaning | Transition |
|---|---|---|
| unknown | no reading for this credential | first successful read → fresh |
| fresh | last read succeeded within the source's TTL | TTL expiry → due (background re-read); read failure → stale |
| stale | last read failed or is older than 2× TTL | any successful read → fresh; a quota refusal marks stale immediately and asks for a re-read |
| expired window | `resets_at <= now` | treated as `used = 0`, next reset unknown; if `span_secs` is known, the window is taken to renew `span` from now (`internal/provider/routing.go:310-313`) |
| renewed | a window that was full is read not-full | store notification: emit diagnostics and clear any sink marker; there are no sign-in or credential hooks in AAG (`docs/unified_suppliers.md:153-157`; `internal/provider/routing.go:553-588`) |

Rules kept from Magpie:

- The last reading survives failures and restarts. Magpie persists readings to
  disk precisely so a vendor rate-limiting its usage endpoint does not turn an
  account into "unknown" (`internal/provider/quota_last.go:1-6`, `:59-118`).
- A refusal marks the reading stale and forces the next ask to hit the vendor
  rather than trust an in-flight reading that predates the refusal
  (`internal/provider/routing.go:660-698`).
- The soft TTL is one minute for subscription-style reads; a reading taken
  for a UI surface counts toward it
  (`internal/provider/routing.go:470-551`, `internal/provider/login_usage.go:183-188`).

Retention and aging:

- A row keeps its last successful reading across refresh failures and
  restarts: `read_at`, `windows_json`, and `stale_since` persist, so a vendor
  that rate-limits or removes its usage endpoint does not turn an account into
  "unknown" (`internal/provider/quota_last.go:1-6`, `:59-118`).
- A stale reading is retired to `unknown` only when both hold: the source has
  had no successful read for `max(24 h, 4 × its TTL)`, and none of its
  applying windows has a future `resets_at`. Retirement clears
  `windows_json` but keeps the source settings and the `last_error`.
- A stale reading with a future `resets_at` never ages out on its own: the
  vendor's reset instant still bounds the window, so the last reading stays
  the best available estimate until a refresh succeeds.
- Manual readings never age. They persist until the user edits or clears
  them.
- Rows are pruned outright only when their configuration identity is deleted
  (supplier, endpoint, or proxy-provider row removed), never because the
  credential is absent from a transient snapshot: disabled endpoints are
  skipped at snapshot build (`src-tauri/src/services/suppliers/mod.rs:224-227`)
  and queue providers are skipped when `queue_endpoint_of` returns `None`
  (`src-tauri/src/services/proxy_runtime/queues.rs:236-249`).

Deviation, deliberate: Magpie lets the very first request wait up to 3 s for
the first reading of an agent (`internal/provider/routing.go:142-145`,
`:537-542`). AAG does not port that wait. `resolve_model` is synchronous
(`src-tauri/src/proxy/router.rs:556-607`) and a request path that waits on a
vendor network call would contradict the best-effort posture already used for
usage recording (`src-tauri/src/proxy/usage_recorder.rs:7-11`). Instead the
store is primed by the background refresh at listener start and persisted
across restarts, and a cold start degrades to configured order.

### 2.3 Credential identity

Routing sees only proxy provider ids: the snapshot registers one provider per
`proxy_providers` row plus synthesized queue providers
(`src-tauri/src/services/proxy_runtime/snapshot.rs:109-139`,
`src-tauri/src/services/proxy_runtime/queues.rs:235-310`). Allowance identity
therefore maps onto, but is not identical to, that id:

| Routing-visible row | Credential identity | Basis |
|---|---|---|
| supplier endpoint using the supplier's shared key | `supplier:<supplier_id>` | `api_key` falls back to `supplier.api_key` (`src-tauri/src/services/suppliers/mod.rs:236-240`); endpoints with the same shared key consume one allowance, reported per protocol family (section 2.4) |
| supplier endpoint with `api_key_override` | `supplier:<supplier_id>#<endpoint_id>` | per-endpoint key (`src-tauri/src/services/suppliers/types.rs:70-76`, schema `supplier_endpoints.api_key_override` at `src-tauri/src/database/schema.rs:392`) |
| standalone `proxy_providers` row | `provider:<id>` | `src-tauri/src/database/dao/proxy_providers.rs:12-19` |
| queue-synthesized provider | `queue:<synthetic_id>` | built from a `providers` row (`src-tauri/src/services/proxy_runtime/queues.rs:262-284`); manual readings only in v1 |

The identity is derived from configuration ids only. No key bytes, key
digests, or key-derived hashes are stored. Magpie tags a key's card by a
SHA-256 of the key (`internal/provider/quota_last.go:49-55`); AAG deliberately
does not, because the configuration ids already provide a stable local
identity, credentials never appear in serializable read views
(`src-tauri/src/services/suppliers/types.rs:1-5`), and a key-derived value in
a database is an unnecessary second secret (`AGENTS.md:119-120`).

At snapshot build, a `provider_id -> credential_ref` map is produced beside
the existing weights map (`src-tauri/src/services/proxy_runtime/snapshot.rs:77-80`)
and installed on the router the same way
(`src-tauri/src/proxy/router.rs:425-430`). Two protocol endpoints of one
supplier that share a key therefore share one credential identity and one
allowance; each selects the reading rows for its own protocol family (section
2.4). One supplier holds at most four protocol endpoints on that shared key
(`UNIQUE (supplier_id, protocol)` at `src-tauri/src/database/schema.rs:385-402`;
`MAX_ENDPOINTS = 4` at `src-tauri/src/services/suppliers/mod.rs:29`), so a
shared credential can carry up to four family-scoped readings.

### 2.4 Persistence

New schema v24 tables (current version is 23, `src-tauri/src/database/mod.rs:30`;
migrations live in `src-tauri/src/database/schema.rs`):

```text
allowance_sources (
  credential_ref TEXT NOT NULL,
  source         TEXT NOT NULL,     -- manual | header | usage_endpoint | refusal_hint
  enabled        INTEGER NOT NULL,  -- opt-in per source; manual starts enabled
  updated_at     INTEGER NOT NULL,
  PRIMARY KEY (credential_ref, source)
)

allowance_readings (
  credential_ref TEXT NOT NULL,
  source         TEXT NOT NULL,     -- which source produced this row
  family         TEXT NOT NULL,     -- anthropic | openai | openai-responses | gemini | other
  windows_json   TEXT NOT NULL,     -- bounded array of the window type above
  read_at        INTEGER NOT NULL,  -- epoch seconds of the last successful read
  stale_since    INTEGER,           -- NULL while the last read succeeded
  last_error     TEXT,              -- stable code only; never a URL, body, or key
  PRIMARY KEY (credential_ref, source, family)
)
```

Sources are concurrent, not exclusive: most vendor rows legitimately have
manual entry, response-header observations, and refusal hints at once. Both
tables are therefore keyed by `(credential_ref, source)`, and readings also
carry the `family` because one credential can span protocol families (section
2.3).

Merge and precedence: within one `(credential_ref, family)`, windows from all
enabled sources are merged by `window_ref`, the stable window identity. When
two sources report the same `window_ref`, precedence is `manual` >
`usage_endpoint` > `header` > `refusal_hint`: the higher-precedence source's
value wins, and a lower-precedence source only fills windows the higher one
does not report. An empty read never erases another source's windows; it only
clears that source's own row. Derived queries in section 2.1 read the merged
view. A source that reports an account-wide window under one protocol family
is not assumed to apply to the other families of a shared credential.

Bounds follow the supplier boundary style already in use: at most 32 windows,
a bounded JSON payload (64 KiB), no control characters, no URLs in error
columns (`src-tauri/src/services/suppliers/mod.rs:25-37`, `:615-620`). Rows
are pruned only when their configuration identity is deleted (section 2.2),
never because a credential is absent from the current runtime snapshot.

Persistence is local only. Readings contain no credentials, but they are
runtime state, not configuration: they never enter the routing fingerprint
query list (`src-tauri/src/database/dao/routing_revision.rs:7-21`) and no IPC
surface exports them; the existing cloud sync copies the whole `aag.db`
(`src-tauri/src/services/sync/mod.rs:1-4`), so a synced database carries
readings exactly as it carries the rest of the local state.

## 3. Where allowance data can come from, per provider family

AAG's provider types are fixed at the factory
(`src-tauri/src/proxy/providers/mod.rs:135-204`).

| Provider type | v1 sources | Notes |
|---|---|---|
| `mock` | none | synthetic URLs, tests only (`src-tauri/src/proxy/providers/mod.rs:139-144`) |
| `anthropic_passthrough` | none | no stored credential; the client's inbound bearer is relayed per request (`src-tauri/src/proxy/providers/mod.rs:178-189`); model discovery excludes it for the same reason (`src-tauri/src/services/model_discovery/mod.rs:66-75`) |
| `anthropic` (vendor host) | manual; response headers; 429 hints | header and 429 readings need the boundary extension (section 3.1). The documented Anthropic Usage and Cost API authenticates with an admin-scoped key — a second secret — so it is deferred (section 3.2) |
| `anthropic` (third-party relay) | manual; response headers; 429 hints where the relay forwards vendor rate-limit headers; allowlisted coding-plan usage endpoint behind opt-in | no generic usage API; never scrape |
| `openai` / `openai-responses` | manual; response headers; 429 hints; allowlisted coding-plan usage endpoint behind opt-in | the organization usage API authenticates with an admin-scoped organization key, a second secret from the inference key; deferred (section 3.2) |
| `gemini` | manual; 429 hints | no per-key quota read in the Generative Language API; Cloud billing is project-scoped and out of scope |

The source classes:

1. **Documented usage endpoints that accept the provider's own stored key.**
   v1 eligibility is narrow: the provider type is not `anthropic_passthrough`;
   the configured base URL host exactly matches a compiled-in vendor allowlist
   entry; the endpoint URL is a fixed literal; the user opted in per
   credential. Provider type alone never qualifies a row — an `anthropic` row
   pointing at a third-party relay host and an `anthropic` row pointing at the
   vendor host are decided only by the host match. This covers the domestic
   coding-plan endpoints cc-switch
   already reads for display — Kimi `api.kimi.com/coding/v1/usages`,
   Zhipu/MiniMax/Volcengine plan endpoints parsing `limit`/`remaining`/
   `resetTime` (`cc-switch-src/src-tauri/src/services/coding_plan.rs:1-16`,
   `:93-110`, `:176-179`) — and nothing else. cc-switch uses them for display
   only; it does not schedule on them.
2. **Admin-scoped vendor usage APIs (deferred).** Anthropic's Usage and Cost
   API and OpenAI's organization usage API read an account's usage with a key
   that is a different secret from the inference key, so they violate the
   "a usage read uses only the provider's own stored key" rule until a
   second-secret model exists. Such a model needs storage separate from the
   inference key, write-only IPC, bounded validation, the same redaction and
   logging rules, and a guarantee that the admin secret is never usable for
   inference. v1 excludes these sources; the security rules in section 3.2
   restate the boundary.
3. **Response headers and refusals.** Anthropic's
   `anthropic-ratelimit-unified-*` headers and the in-stream `rate_limit_event`
   carry utilization and reset per window
   (`internal/gateway/claude_subscription.go:3743-3769`, `:2197`, `:2422`);
   OpenAI-compatible APIs commonly expose `x-ratelimit-*`; a 429 body often
   names the reset (`internal/gateway/routing.go:518-620`). Header readings
   are free in data terms: they ride traffic AAG already carries. The provider
   boundary does not expose them today, so increments 5 and the refusal-hint
   source depend on the extension in section 3.1.
4. **Manual entry.** The universal baseline; the only source for passthrough
   rows (no stable stored credential) and vendors without a readable signal.
   Third-party relays are manual plus header/429 observations where the relay
   forwards vendor rate-limit headers. The user states used percent, span,
   reset time, and scope per provider.

### 3.1 Provider-boundary extension for header and refusal readings

The current boundary cannot carry a header or a 429 body to the router:

- `Provider::complete` and `Provider::stream` return only
  `OAIChatResponse`/chunks (`src-tauri/src/proxy/providers/mod.rs:47-51`,
  `:80-84`).
- `OAIChatResponse` has no header field
  (`src-tauri/src/proxy/canonical.rs:159-176`).
- `ProviderError` carries `status`, `code`, `stage`, `kind`, and
  `response_bytes` only; no upstream headers and no 429 body text
  (`src-tauri/src/proxy/canonical.rs:304-317`).
- `UpstreamErrorKind` has only `AuthExpired` and `ToolCapability`
  (`src-tauri/src/proxy/canonical.rs:292-302`).

The extension named here is a bounded, content-free observation channel:

- A new `Upstream_observation` value: family plus the numeric window fields of
  section 2.1 (`window_ref`, `used_pct`, `resets_at`, `span_secs`, scope).
  Parsing happens at the provider boundary and the raw header names and
  values are discarded there; the observation never carries strings beyond a
  window identity.
- Both trait methods return it alongside the answer — either a wrapper
  (`Provider_answer { response, observation }`,
  `Provider_stream { stream, observation }`) or an out-parameter. Providers
  already hold the `reqwest::Response` before reading the body (for example
  `src-tauri/src/proxy/providers/openai.rs:204-231`), so header extraction is
  local; the mock provider needs a fixture hook.
- The error path needs the same channel: `ProviderError` gains an observation
  field so a 429's headers and reset hint survive the failover walk and are
  recorded at the settle points (`src-tauri/src/proxy/router.rs:1037-1062`,
  `:1216-1243`).
- Mid-stream `rate_limit_event` / in-stream 429 hints arrive as chunks, not at
  the trait boundary; the streaming wrapper already scans chunk usage fields
  (`src-tauri/src/proxy/router.rs:1364-1375`) and records them at the settle
  point (`:1427-1431`), which is where a header-independent hint would land.

Cost: it touches all five provider implementations and both trait methods
(`src-tauri/src/proxy/providers/mod.rs:26-85`), the canonical response and
error types (`src-tauri/src/proxy/canonical.rs:159-176`, `:304-317`), every
provider test double, and the two router settle points. It is mechanical but
broad and sits on the proxy's hottest trait.

Risks: per-request allocation and copying of observation values; an allowlist
bug could capture sensitive header values, so parsing must happen at the
provider boundary with raw values dropped immediately; headers exist only at
response open, so mid-stream hints need the separate chunk scan; error-path
observations must not be lost when the walker advances hops.

v1 sequencing: increments 1-4 do not need this extension. If it is not
funded, v1 ships manual readings plus same-key usage endpoints only, and
increments 5 and the refusal-hint source are dropped with the extension.

### 3.2 Security rules

| Rule | Enforcement | Citation |
|---|---|---|
| No credential forwarding | a usage read uses only the provider's own stored key, against its own vendor; the inbound client credential is never captured for quota: passthrough rows have no stored key, the client's bearer is relayed per request, and probe-style refresh excludes them; the router's credential-scope guard never grants stored-key use to a passthrough credential | `src-tauri/src/proxy/providers/mod.rs:178-189`, `src-tauri/src/services/model_discovery/mod.rs:66-75`, `src-tauri/src/proxy/router.rs:356-367` |
| No second secret in v1 | admin-scoped vendor usage APIs are excluded; a later design must store the admin secret separately from the inference key, write-only over IPC, validated by one bounded read, and redacted like any inference key | new rule; `AGENTS.md:119-120` |
| No paid probing | never issue an inference request to learn quota; only documented usage endpoints, headers, and refusals produced by real traffic | capability tiers already follow this rule: public metadata only (`docs/unified_suppliers.md:107-117`) |
| Opt-in only | each source defaults to disabled (`manual` starts enabled); `pace` mode without readings is exactly `order`; enabling a source is an explicit per-credential action | settings-key precedent `src-tauri/src/database/dao/settings.rs:92-100`; discovery eligibility `src-tauri/src/services/model_discovery/mod.rs:66-75` |
| Local storage only | readings live in the local SQLite database; no IPC surface exports them, and cloud sync copies the whole local database, not a per-table selection | `src-tauri/src/services/sync/mod.rs:1-4` |
| Keys never logged or exported | every diagnostic reference goes through a keyed HMAC (`safe_reference`); fetch failures surface as stable codes only; keys are never logged, committed, or exposed | `AGENTS.md:119-120`; `src-tauri/src/diagnostics.rs:454-468`; `src-tauri/src/commands/supplier_catalog.rs:1-6` |
| No user-supplied usage URLs | v1 usage endpoint URLs are fixed per vendor with an exact host allowlist; a configurable URL would let a stored key be sent to an arbitrary host | new rule, consequence of the rule above |
| Header capture is content-free | only numeric window fields are stored from headers; no bodies, prompts, or responses | timeline policy `docs/unified_suppliers.md:174-183`, `AGENTS.md:104-105` |

## 4. Refresh cadence, caching, and failure behavior

### 4.1 Lifecycle

The refresh task is owned by the running proxy and dies with it, like the
listener itself (`RunningProxy` at `src-tauri/src/commands/proxy.rs:16-49`).
Serialization follows the model-discovery precedent: one global refresh lock
with `try_lock` deduplication, so a manual refresh and the periodic pass never
overlap (`src-tauri/src/services/model_discovery/mod.rs:64`, `:126-127`),
plus a single-flight map per credential so concurrent asks share one read
(`internal/provider/login_usage.go:197-233`,
`internal/provider/routing.go:470-534`).

| Source | Cadence | Rationale |
|---|---|---|
| manual | none (edit-driven) | user owns the number |
| header | on response, persisted at most once per credential per 15 s | the data arrives with traffic; the debounce protects SQLite |
| usage_endpoint | 1 min to 5 min per vendor, jittered | Magpie's one-minute soft TTL and Codex once-a-minute precedent (`internal/provider/routing.go:470-551`, `internal/provider/codex_limits.go:11-12`) |
| refusal_hint | on 429, merged into the existing reading | free signal, no fetch |

### 4.2 Failure behavior

| Failure | Behavior |
|---|---|
| usage endpoint unreachable / 5xx / timeout | keep the last reading, set `stale_since`, back off (1 min → 15 min cap), never touch the request path |
| usage endpoint 429 | keep the last reading, mark stale, longer backoff; do not retry until the backoff elapses |
| auth failure on the usage endpoint | disable that source, surface a stable status code, keep the last reading under the retention rule in section 2.2 (retired to unknown only after `max(24 h, 4 × TTL)` with no future reset) |
| malformed body | reject the reading wholesale, keep the previous one, record a stable code; never partially merge windows |
| clock skew / reset in the past | treat the window as empty; do not fabricate a reset |
| store lock poisoned | rank by configured order; same degradation as `spread_chain` (`src-tauri/src/proxy/router.rs:740-743`) |

### 4.3 Hot path versus background

| Work | Where | Why |
|---|---|---|
| read the allowance view for a resolve | request path, in-memory | ordering must be free and synchronous (`src-tauri/src/proxy/router.rs:556-607`) |
| mark a 429 as quota-left (sink input, stale hint) | request path, in-memory | the decision is already being made there (`:1037-1062`, `:1216-1243`) |
| network reads of usage endpoints | background task | never block a request |
| SQLite writes of readings / manual edits | background task or the edit command | request path must not write |
| logging | background task and request path via `safe_reference` | no secrets, no URLs |

State is carried across snapshot generations exactly like `chain_spread`,
`last_used`, and `usage_ledger`, so a hot apply does not wipe readings
(`src-tauri/src/services/proxy_runtime/snapshot.rs:89-94`). A new reading
updates the shared store; it does not bump `routing_revision` and therefore
does not rebuild the router (`src-tauri/src/database/dao/routing_revision.rs:7-21`).
Mode selection remains a settings-level routing input, so switching to `pace`
hot-applies like any mode change (`src-tauri/src/database/dao/settings.rs:92-100`,
`src-tauri/src/commands/proxy.rs:211-230`).

## 5. How pace and sink consume allowances

### 5.1 Scope

Ordering happens where queue-built chains are ordered today: `spread_chain`
(`src-tauri/src/proxy/router.rs:733-849`), called from `resolve_route`,
`resolve_route_for`, and the `precise:` queue fallback. The allowance ordering
must not reuse the rotation key: that key excludes models so several models
share one rotation (`:753-759`), while allowance windows are model-scoped, so
the pace order is computed per resolved chain (the hops carry the upstream
model).

### 5.2 Pace ordering algorithm

Given the resolved hops of a queue-built chain and a clock `now`:

1. For each hop, resolve its credential, compute `used`, `renews`, `pace`,
   `pooled` for the hop's upstream model (section 2.1). A hop whose credential
   is `supplier:<id>` shares the credential with its sibling endpoints and
   selects the windows for its own protocol family (section 2.4).
2. Partition into `fine` (`used < 90`), `low` (`90 <= used < 98`), and `spent`
   (`used >= 98`). Thresholds are Magpie's (`internal/gateway/routing.go:945-946`,
   `internal/provider/codex_switch.go:47-50`).
3. Sort `fine` by `pace` descending — the account with the most left per hour
   until renewal has the most to lose at that renewal
   (`internal/gateway/routing.go:1150-1246`). Ties break by observed usage
   ascending (`Usage_ledger::tokens`, `src-tauri/src/proxy/router.rs:216-225`;
   a memory-only counter that resets on restart), then by configured order.
   Magpie additionally bands paces within a tenth (`:1195-1203`); v1 can ship
   without banding and add it if ordering proves jittery.
4. Append `low`, then `spent`, each sorted by `used` ascending. They remain in
   the chain as failover. This is the same standby semantics the weighted mode
   already gives weight 0 (`src-tauri/src/proxy/router.rs:56-58`, test
   `weight_zero_is_standby_only_but_still_in_the_chain` at `:3621-3636`).
5. Unknown allowance (section 5.3) is not sorted: it follows the sorted
   `fine` group, keeps configured order among unknowns, and stays ahead of
   `low`/`spent`, so an unread provider is neither promoted nor starved.
6. A `pooled` full window means the model is out while the account is not;
   because the chain is already per-model (`matching_model_hops`,
   `src-tauri/src/proxy/router.rs:703-727`), this only changes the window set
   used for the decision, never chain membership.

A cold start with no readings at all produces exact configured order, which is
the current default behavior; pace is therefore safe to enable before any
source is configured.

### 5.3 Unknown, stale, and spent semantics

| Case | Ordering effect | Never happens |
|---|---|---|
| unknown (no reading) | neutral group between `fine` and `low` | not treated as 0% used; not treated as spent |
| stale (last read failed) | same rank as its last reading until the retention rule retires it to unknown | stale does not promote; the UI marks it |
| window reset passed | window counts as empty for `used` | no fabricated reset time |
| spent (`>= 98`) | last, still failover | never removed from the chain |
| pinned provider | bypasses chains entirely | allowance never overrides a pin (`src-tauri/src/proxy/router.rs:642-649`) |
| planner chain / explicit route | keeps its order | allowance never touches it (`:32-42`, `:562-586`) |

### 5.4 Sink

Magpie's `Sink` sends a key or account that answered a 429 while it still had
quota to the back of the order, so load goes round instead of returning to the
first account the moment its rest ends (`internal/gateway/sink.go:1-19`,
`internal/provider/provider.go:118-124`).

AAG port:

- Trigger: a 429 classified as a rate limit on a hop whose reading says the
  model is not spent (no applying window at or above 98). This is the same
  point where the walkers already settle a retriable failure
  (`src-tauri/src/proxy/router.rs:1037-1062`, streaming setup
  `:1216-1243`). The trigger needs only `ProviderError.status`; parsing a reset
  hint out of the 429 body additionally needs section 3.1.
- State: an in-memory `HashMap<provider_id, {at, seq}>` carried across
  snapshots like `chain_spread`, bounded and wholesale-cleared at the cap
  (`src-tauri/src/proxy/router.rs:112-125`). Never persisted.
- Effect: a stable partition of the final pace-ordered chain, sunk members
  behind non-sunk ones, first-sunk ahead of later-sunk
  (`internal/gateway/sink.go:63-102`).
- Clear: on that provider's next successful response. (Magpie keeps the mark
  until restart or a re-sink; clearing on success is simpler and cannot pin a
  recovered provider to the back indefinitely.)
- Not applied in `rotate`/`weight`, which already go round
  (`internal/gateway/sink.go:59-61`).

### 5.5 Invariants

| Invariant | Enforcement |
|---|---|
| explicit routes and planner chains keep order in every mode | ordering only inside `spread_chain`; tests `:3639-3666`, `:3831-3878` |
| in-flight requests keep their admitted snapshot | snapshot cloned per request (`src-tauri/src/services/proxy_runtime/live.rs:52-58`); documented at `docs/unified_suppliers.md:185-194` |
| hot apply affects only new requests | generation-checked reload (`src-tauri/src/services/proxy_runtime/live.rs:69-128`) |
| breakers still gate every hop | `admit_hop` unchanged (`src-tauri/src/proxy/router.rs:857-892`) |
| session affinity stays within the chain | affinity preference runs after resolution (`:974-976`, `:1203-1205`); allowance orders the chain, affinity pins the session provider within it |
| no request waits on quota I/O | resolution is synchronous; only in-memory reads |
| the routing fingerprint excludes readings | readings are runtime state, not configuration |
| failover contract unchanged | retriable failures still advance; non-retriable 4xx still stop (`docs/unified_suppliers.md:125-139`, `src-tauri/src/proxy/router.rs:1037-1062`) |

Traceability: the request timeline already records content-free plan codes
(`docs/unified_suppliers.md:174-183`, `src-tauri/src/proxy/router.rs:622-640`).
Add an allowance decision block — mode, used band, pace bucket, sink state —
as stable codes only. No window contents, no amounts beyond a bucket.

## 6. Smallest shippable increments

Each increment is independently shippable and leaves the default mode
(`order`) unchanged.

| # | Increment | Contents | Test strategy | Evidence boundary |
|---|---|---|---|---|
| 1 | data model + store | window/reading types, derived queries, in-memory store with TTL/stale, schema v24 tables and DAO | table-driven unit tests for `used`/`renews`/`pace`/`pooled`, reset-passed, stale-keeps-last, atomic replace, persistence roundtrip, poisoned-lock fallback | unit tests only; no routing effect |
| 2 | manual entry + IPC | `allowance_get`/`allowance_set` commands, validation, source settings; no network | command and serde tests, bounds and control-character rejection, empty-state, demo-mode refusal (`CLAUDE.md:172`) | IPC tests; browser-demo evidence is synthetic, not native |
| 3 | `pace` mode | extend `Routing_mode` (`parse`/`as_str`), IPC setter, UI picker, ordering arm consuming the store; no sink, no fetch | resolution-order tests with an injected clock and fixture stores: invariants above, spent last, unknown neutral, deterministic ties, single-hop untouched | unit tests; no real quota values exercised |
| 4 | sink | quota-left 429 detection, bounded state, stable partition, clear on success | mock-provider 429 scenarios, ordering assertions, state cap, unrelated 4xx unaffected | mock-level; real 429 classification is external evidence |
| 5 | header readings | provider-boundary observation extension (section 3.1), per-family header parsers, refusal-hint parsing, debounced persistence, stale marking | fixture headers per family, clamping, reset parsing, no secret in logs, observation survives a 429 failover | unit tests with recorded-shape fixtures; real header shapes need observed traffic; the trait change is broad and needs its own review |
| 6 | usage-endpoint readers | background reader writing the store directly (no provider-trait change), opt-in per family, compiled-in host allowlist, fixed URLs, single-flight, backoff, status surface; the read uses the same stored key | mock HTTP server per family, host-mismatch refusal, failure matrix, stable error codes, key never in URL or log | mock only; live vendor APIs are external evidence, never claimed from unit tests |
| 7 | UI surface | source settings row, quota chips, mode help text | typed IPC plus browser smoke; demo synthetic | browser demo only; native acceptance stays open |

Dependency on the observed-usage track: it has landed (commit `3edf549`), so
increment 3 extends the existing four-mode router rather than rebasing onto
it: one `Routing_mode` variant and `spread_chain` arm
(`src-tauri/src/proxy/router.rs:43-67`, `:733-849`), one `parse` branch
(`:73-90`), one settings/IPC branch (`src-tauri/src/commands/proxy.rs:211-230`),
and one UI option. The allowance store is a new carried field beside
`usage_ledger` (`src-tauri/src/proxy/router.rs:283-287`, `:405-406`;
`src-tauri/src/services/proxy_runtime/snapshot.rs:89-94`); the observed
counters are only read as its tie-breaker, never written.

## 7. Non-goals and risks

### 7.1 Non-goals

| Non-goal | Why |
|---|---|
| OAuth subscription reuse or credential refresh | AAG never reads or rewrites native credential stores; passthrough stays the boundary (`docs/reviews/ccswitch_magpie_gap_analysis_2026_10_14.md:140-162`, `:200`) |
| Running vendor CLIs or browsers to read quota | Magpie runs Claude Code's `/usage` (`internal/provider/claude_usage_cli.go:67-119`); AAG will not spawn agents or scrape dashboards as a side effect of routing |
| Hard admission control at 100% | the vendor's 429 is the enforcement point; a stale local no would turn a routing preference into an outage |
| Cross-device or remote quota aggregation | local storage only |
| Auto check-in, credit spending, or top-up | side effects on the user's account, outside routing |
| Paid probes to measure capacity | capability tiers already exclude this (`docs/unified_suppliers.md:107-117`) |
| Mid-stream recovery based on quota | documented non-goal (`docs/reviews/ccswitch_magpie_gap_analysis_2026_10_14.md:222-228`) |
| Vendor billing accounting | `usage_recorder`'s price table stays a separate local estimate (`src-tauri/src/proxy/usage_recorder.rs:17-48`) |

### 7.2 Risks

| Risk | Mitigation |
|---|---|
| a stale or wrong reading reorders traffic badly | reorder-only; explicit routes and pins untouched; expired windows empty; stale surfaced in UI; mode opt-in |
| vendor usage endpoints drift or disappear | fixed per-family readers, stable error codes, last reading retained, manual fallback |
| key exfiltration through a configurable URL | no user-supplied usage URLs in v1; endpoints are fixed literals |
| secrets in logs or errors | `safe_reference` for identifiers; stable codes for failures; no bodies or URLs stored |
| refresh state accidentally moves the routing fingerprint, rebuilding on every read | readings live in a carried store, never in `routing_revision` queries |
| hot-path lock contention | bounded maps, short critical sections, minimal cloned view; poisoning degrades to configured order |
| clock skew mis-ages windows | `now` injected into every ranking function; reset times compared as vendor absolute instants; past resets mean empty, not negative |
| model-scope mismatch | canonical id + family keys from `identity.rs`, never substring matching |
| operator surprise ("pace picked the more expensive provider") | the mode is a deliberate user choice; UI help text and trace codes state the band and reason |
| evidence inflation | each increment names what unit tests can prove and what needs real vendor APIs; no increment claims external acceptance |

## Evidence notes

All AAG claims were read from source at `3d059fe` on `feat/unified_suppliers`;
the first draft was written at `cfdfda7`, and every line reference was
re-checked after the observed-usage mode landed in `3edf549`. The Magpie and
cc-switch claims were read from their read-only reference checkouts. No live
vendor quota endpoint was called, no allowance mode was implemented, and no
runtime behavior is claimed. Unit-test names in section 6 are planned tests,
not existing ones; existing tests cited in sections 1-5 are real at the
baseline. The current four-mode behavior and its tests are the only routing
behavior verified in this document
(`src-tauri/src/proxy/router.rs:3520-3909`).
