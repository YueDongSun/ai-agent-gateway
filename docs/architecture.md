# AAG Architecture

This document is the current architecture reference for AI Agent Gateway
(AAG). Historical migration decisions remain in `DESIGN.md`; current test and
release evidence remains in `PROGRESS.md`.

## Product boundary

AAG has two independently runnable surfaces:

- The Node gateway and CLI in `src/` remain the headless compatibility path.
- The Tauri 2 desktop application in `src-tauri/` and `ui/` is the primary
  product surface.

The desktop application owns the SQLite configuration store, provider and
route management, local proxy lifecycle, browser demo, and release packaging.
The Node surface must not be removed or silently made dependent on Tauri.

The protected Codex session authority is under preparation, with a separate
dedicated service/shim executable. Its source modules are not an installed
session-sharing backend. [Codex session authority](codex_session_authority.md)
records ownership, canonical references, read-only transitions, metadata-only
clients, migration recovery, and the pending Windows qualification boundary.

## Runtime modules

### Node compatibility surface

| Module | Responsibility |
| --- | --- |
| `src/index.ts` | CLI entry point and interactive TUI selection |
| `src/cli/` | Commander commands and interactive prompts |
| `src/config/` | Zod schema, JSON persistence, and mutation helpers |
| `src/gateway/` | Hono HTTP edge, canonical format, routing, providers, SSE, and breakers |
| `src/agents/` | Agent config adapters and gateway/direct/profile switching |
| `tests/` | In-process Vitest coverage for the Node surface |

The canonical internal request and response shape is OpenAI Chat Completions.
Anthropic conversion happens at the HTTP edge and inside the Anthropic-native
provider. Routing and circuit-breaker decisions are made on the canonical shape.

The headless gateway runs until interrupted. `createGatewayApp(...).start()`
returns the Node server, and the CLI installs SIGINT/SIGTERM handlers that
stop accepting connections and exit through the close callback. Open
keep-alive connections — an in-flight SSE response can hold `close()` open
indefinitely — are cut after a 3-second grace window, and a second signal
exits immediately.

### Tauri backend

| Module | Responsibility |
| --- | --- |
| `src-tauri/src/main.rs` | Native executable entry point |
| `src-tauri/src/lib.rs` | Tauri setup, managed state, plugin registration, and command registration |
| `src-tauri/src/startup.rs` | Release autostart registration and normal versus tray-only launch state |
| `src-tauri/src/tray.rs` | System tray icon, show/hide/quit actions, and close-to-tray behavior |
| `src-tauri/src/diagnostics.rs` | Log policy, temporary debug state, panic capture, and safe summaries |
| `src-tauri/src/commands/` | Feature IPC orchestration; see the [command module map](../src-tauri/src/commands/README.md) |
| `src-tauri/src/database/` | SQLite connection, schema migrations, seed data, and DAOs |
| `src-tauri/src/proxy/` | Axum HTTP server, canonical types, conversion, routing, providers, and breakers |
| `src-tauri/src/proxy/planner.rs` | Capability-alias planning contract (`Model_planner`, selectors, stable resolution errors) with no service dependency |
| `src-tauri/src/proxy/session_affinity.rs` | Bounded, memory-only session stickiness for explicit session markers only |
| `src-tauri/src/services/provider/` | Per-agent live configuration adapters and platform guards |
| `src-tauri/src/services/provider/config_paths.rs` | Read-only OS-aware resolution shared by provider status, client launch and configuration replacement |
| `src-tauri/src/services/launcher/` | Platform discovery, terminal/editor launches, bounded probes, child reaping and launcher DTOs |
| `src-tauri/src/services/detect/` | cc-switch coexistence detection and diagnostics |
| `src-tauri/src/services/import_cc_switch/` | Explicit cc-switch SQLite import and Claude provider auto-sync |
| `src-tauri/src/database/dao/suppliers/maintenance.rs`, `origins.rs` | Revision-bound imported-account cleanup, endpoint merge transaction, recovery snapshot and private merge provenance |
| `src-tauri/src/services/suppliers/` | Supplier, endpoint, model-offer, and legacy-projection ownership plus the safe supplier views and scoped per-projection revalidation with a 20-second bound |
| `src-tauri/src/services/supplier_setup.rs` | Default model selection, managed gateway client records, and backed-up mirror reconciliation after supplier mutations |
| `src-tauri/src/services/mirror_models.rs` | Client role catalogues projected from the same assessed capability planner used by request dispatch |
| `src-tauri/src/services/mirror_sync/` | Opt-in instruction/prompt inventories, supplemental plugin skill resources, two owned Codex agent scalars, private recovery copies, and one application-owned worker |
| `src-tauri/src/services/provider/claude_desktop.rs` | Isolated Claude Desktop third-party gateway profile with loopback and authentication preflight |
| `src-tauri/src/proxy/claude_desktop.rs` | Authenticated Desktop gateway edge and snapshot-bound role resolution without OAuth passthrough |
| `src-tauri/src/proxy/publication.rs` | Strict existing-key Tailnet frontend and owned loopback HTTP/SSE connection cancellation |
| `src-tauri/src/services/tailnet_publication/` | Opt-in publication settings, foreground Serve ownership, endpoint self metadata, and serialized native lifecycle |
| `src-tauri/src/services/tailnet_discovery/` | Request-owned, bounded checks of AAG discovery markers on trusted Tailnet inventory addresses |
| `src-tauri/src/services/model_catalog/` | Public catalog evidence, capability-tier assessment, and candidate planning |
| `src-tauri/src/services/model_catalog/arena.rs`, `arena_fetch.rs` | Strict Arena identity/condition matching and bounded collection from official dataset statistics and rows |
| `src-tauri/src/services/model_catalog/arena_visualization.rs` | Public rating distribution and side-effect-free router preview; unknown ratings remain absent |
| `src-tauri/src/services/model_catalog/refresh.rs` | Shared manual/automatic refresh admission, daily freshness policy, cancellation, and change reports |
| `src-tauri/src/services/proxy_runtime/` | Runtime snapshots, hot reload, supplier projection wiring, and the planner install |
| `src-tauri/src/services/model_discovery/` | Model auto-discovery: configured-supplier preflight, per-source dual-document OpenAPI search, upstream catalog probing, same-origin route validation with Gemini model-slot escape rejection, additive protocol URL discovery, same-family version ranking, and never-downgrade application to pool/hops/mappings |
| `src-tauri/src/services/dsh/` | Isolated DSH lifecycle, plugin transactions and redacted upstream resolution |
| `src-tauri/src/services/mcp/` | Validated MCP registry management, redacted reviews, and backup/ACL-preserving Claude sync |
| `src-tauri/src/services/prompt/` | Two-stage prompt enhancement (Chinese brief, then English prompt) over the local model pool |
| `src-tauri/src/services/prompt/workspaces.rs`, `database/dao/prompt_workspaces.rs` | Versioned workspace metadata and opt-in, content-bound instruction context for one prompt task |
| `src-tauri/src/services/workflow_registry/` | Read-only capability registry, per-stage observations, bounded local probes, and the authoritative router invocation |
| `src-tauri/src/dsh_window.rs` | Restricted native DSH runtime window; no AAG command authority |
| `src-tauri/src/dsh_market_window.rs` | Bundled marketplace window with public catalog/source access only |
| `src-tauri/src/services/sync/` | S3 and WebDAV synchronization |
| `src-tauri/src/e2e.rs` | Full-stack Rust verification with disposable paths |

The backend uses `AppError` for domain and I/O failures. Tauri commands expose
`Result<T, String>` at the IPC boundary. All SQL belongs in DAO modules and all
live configuration writes use atomic-write helpers and backup-before-overwrite
semantics.

Launcher commands schedule synchronous filesystem/process work on the blocking
worker pool. Status DTOs match the frontend's camelCase contract, and environment
values are redacted in launch receipts. Platform-specific behavior and isolated
editor profiles are documented in [platform launchers](platform_launchers.md).

Proxy shutdown retains the runtime in shared state until its server task finishes,
while waiting for graceful drain outside the shared state mutex. Status remains
responsive and reports the draining task as running; another start is rejected
until that task is terminal. Prompt and DSH consumers do not receive a stopping
runtime's router or endpoint. The lifecycle controller in
`src-tauri/src/services/proxy_runtime/lifecycle.rs` owns completion independently
of stop callers, so cancellation does not lose execution ownership. A completed
stop removes shared state only when its completion channel still identifies the
same runtime, leaving any replacement intact. Graceful drain has no new timeout
or force-abort policy.

The optional [Tailnet publication](tailnet_publication.md) has its own loopback
frontend over the active gateway state. Main gateway stop/replacement first
await publication cleanup; the publication frontend closes and joins its HTTP
and SSE tasks. A completion-source binding also closes publication when the
current main server ends unexpectedly, while ignoring older server completions.
The feature defaults off with port 8989 and preserves unrelated Serve routes and
the main gateway's bind configuration. Live Tailnet/firewall/peer acceptance is
separate from local fixture verification.

### Desktop lifecycle

The main window is created hidden so it cannot flash during login startup. A
normal desktop launch shows and focuses the window after native setup. A
packaged release launch registered by the autostart plugin receives
`--aag-autostart` and remains tray-only until the user clicks the tray icon.
The development command `npm run tauri:dev` still shows the same React UI, but
does not register a development executable as a login startup entry. The
independent `--aag-no-autostart` launch argument suppresses release registration
without changing window mode or an existing login-startup entry; disposable
native lifecycle checks use it.

The tray icon uses `src-tauri/icons/tray_icon.png`. Left-click shows and focuses
the main window; Show, Hide, and Quit are also available from the tray menu.
Closing the main window is intercepted and hides it to the tray. The tray is a
required startup dependency because it is the recovery path for a hidden UI.

### React frontend

| Module | Responsibility |
| --- | --- |
| `ui/src/App.tsx` | Application shell, grouped app switcher, lazy page boundary, and the selected workspace view |
| `ui/src/hooks/use_workspace_navigation.ts`, `ui/src/lib/workspace_routes.ts` | Hash-addressable workspace routes, Back/Forward restoration, and per-destination scroll/focus reset |
| `ui/src/components/workspace_header.tsx` | Sticky workspace header: destination selector, gateway status, diagnostics, theme, and locale |
| `ui/src/hooks/use_status_surface.ts`, `ui/src/lib/status_surface.ts` | Bounded page-color sampling for the header surface, resolved to a contrast-preserving color and opacity |
| `ui/src/components/use_accessible_dialog.ts` | Modal dialog semantics: initial focus, focus containment, Escape handling, scroll lock, and focus return |
| `ui/src/components/app/AppPage.tsx` | Current-configuration models and their Arena.ai position for each client, without provider presentation |
| `ui/src/components/arena/`, `ui/src/lib/client_arena_context.ts` | Shared model-position/routing preview and client-plan-only selector mapping |
| `ui/src/lib/api/client_router_plan.ts`, `ui/src/lib/client_router_models.ts` | Selected-configuration read boundary and model-only projection |
| `ui/src/components/suppliers/client_management_section.tsx` | Collapsible supplier-owned client management entry |
| `ui/src/components/suppliers/client_management/supplier_client_settings.tsx` | Retained provider, route, gateway, configuration and launcher orchestration |
| `ui/src/components/suppliers/client_management/gateway_state.ts` | Gateway snapshot, live controls, settings form, and page-health polling |
| `ui/src/components/suppliers/client_management/gateway_flow.ts`, `import_flow.ts` | Dependency-injected gateway actions and cc-switch import orchestration, tested without React or live IPC |
| `ui/src/components/suppliers/client_management/providers_state.tsx` | Unified provider-card data/actions contexts |
| `ui/src/components/suppliers/client_management/routes_state.tsx` | Route data/actions contexts plus the narrow pointer-drag context |
| `ui/src/components/suppliers/client_management/` | Gateway panel, provider list/chain, route editor, pool, and launchers; reachable only from Suppliers |
| `ui/src/components/mcp/` | Lazy MCP registry, private-definition editing, and reviewed client configuration |
| `ui/src/components/workflow/` | Read-only workflow capability panel mounted in the MCP page |
| `ui/src/components/dsh/`, `ui/src/components/dsh_market/` | Managed DSH runtime UI and the bundled marketplace UI |
| `ui/src/components/suppliers/` | Supplier, endpoint, model, import preview, catalog, routing explanation, and request-trace panels |
| `ui/src/components/suppliers/supplier_discovery_details.tsx`, `supplier_discovery_status.tsx` | EN/CN discovery help, catalog-health evidence, and the existing persisted report; labels derive from the shared locale during rendering |
| `ui/src/components/suppliers/arena_chart.tsx`, `tailnet_instances.tsx` | Independently mounted intelligence tabs with visible evidence/scan state and owned polling |
| `ui/src/components/page_actions.tsx` | Page-scoped contextual Save/Delete registration, modal-contained dock and operation feedback |
| `ui/src/components/diagnostics_modal.tsx` | Global diagnostics controls and local log access |
| `ui/src/lib/api/` | Typed Tauri IPC wrappers and shared DTOs |
| `ui/src/lib/supplier_discovery_text.ts` | Stable discovery-code explanations in the selected locale; unknown discovery details are replaced with a generic message |
| `ui/src/lib/provider_presets.ts` | Bundled vendor templates (GLM/Kimi/MiniMax/DeepSeek/MiMo) for the provider template picker |
| `ui/src/lib/mock/` | Synthetic IPC backend for browser demo mode only |
| `ui/src/hooks/` | Workspace navigation, header surface sampling, gateway status reconciliation, and model-discovery polling |
| `scripts/ui_smoke/` | Owned browser runtime, CDP transport, page checks, and regression scenarios |
| `scripts/verification/` | Shared feature command catalog and isolated foreground verification runner |
| `scripts/desktop_environment.mjs` | Bounded local prerequisite probes used by desktop doctor and automatic verification preflight |
| `scripts/mcp/` | Read-only stdio companion; static workflows and verification plans, no live-state or write authority |

`Supplier_client_settings` owns management orchestration. Pool and route state have separate context
modules. Consumers must receive current state and callbacks; stale closures are
not an acceptable rendering optimization. Page modules are lazy-loaded from
`App.tsx` so the initial bundle contains the shell rather than every page.

The shell uses a warm light/dark workspace theme with grouped desktop navigation,
compact narrow-screen navigation, keyboard skip access, and English as the
default locale. Explicit saved locale/theme choices are preserved. AAG keeps its
own product identity; the Claude-inspired styling does not imply affiliation.
The [interface QA checklist](interface_qa.md) records the versioned interaction
reference, surface behavior, reproducible checks, and remaining platform acceptance. [Client/provider boundaries](cli_provider_boundary.md)
record the subsequent model-only client view and retained Suppliers controls.
The [supplier workspace contract and QA checklist](supplier_workspace.md) covers
import reconciliation, endpoint alternatives, Arena, Tailnet detection, prompt
workspace context, floating actions and explicit Desktop mirror preparation.

Each destination is a hash route (`#/claude_code`, `#/suppliers`, `#/usage`,
`#/prompt`, `#/mcp`, `#/dsh`). Reload, Back, and Forward restore the route, and
every destination opens at its heading with focus on the page content rather
than carrying the previous page's scroll position or focus. The page container
is keyed by view, so per-page local state does not survive a destination change.
Modals declare dialog semantics and own initial focus, Tab containment, Escape,
scroll lock, and focus return through `use_accessible_dialog.ts`.

## Data flow

The desktop request path is:

```text
Messages / Responses / Chat / Gemini client
        |
        v
Rust Axum edge: protocol decode and admission
        |
        v
Canonical request + private native source envelopes
        |
        v
Exact model or assessed capability role
        |
        v
Ordered eligible hops -> provider circuit breaker
        |
        v
Anthropic / Responses / Chat / Gemini upstream
```

The Rust proxy admits each request against one immutable runtime snapshot.
Provider, route, supplier, model, pin, and catalog edits rebuild and atomically
publish routing for new requests; in-flight requests keep their admitted
snapshot. Model discovery updates the offered list without rewriting exact
model hops. Before catalog or discovery requests it filters configured suppliers
through the preflight owner, so disabled, unavailable, invalid-URL, or recently
non-available entries are reported and skipped; `catalog_invalid` stays
immediately retryable while other unavailable health waits 300 seconds.
Supplier save and import revalidation uses scoped supplier reads by ID, then
refreshes each projection under its own 20-second wall-clock bound. A corrupt
supplier or timed-out projection is reported without cancelling later entries;
committed changes still set the pending-reload signal.
Discovery enumerates provider identities before decoding rows, so a corrupt
supplier-owned projection is isolated per entry and does not abort later
suppliers. It searches both same-origin metadata documents for each configured
source context, may reuse an existing API root, and appends a validated missing
protocol with its models, projection and listing health in one generation-checked
transaction; existing endpoints, alternatives, models and admission evidence are
preserved. Listener host/port and gateway authentication changes still need a
restart. Streaming failover is one-shot: the router probes the first chunk before
committing; a later stream failure is returned to the client. Rebuilds retry at
most three times when concurrent edits change the inputs, and a failed rebuild
retains the previous snapshot. Status exposes safe apply state, never the
internal fingerprint or its source credentials. Listener comparison retains the
original configured host separately from the bound socket address.

Node SSE response bodies pull on downstream demand and abort the admitted
upstream attempt on cancellation. Both Node provider readers bound an individual
SSE line to 8 MiB, mirroring the Rust `MAX_EVENT_BYTES` event cap; this is not
an overall response-size or process-memory cap. Non-2xx upstream bodies are
read capped at 64 KiB in both runtimes (`MAX_ERROR_BODY_BYTES`): the observed
byte count is reported, 4xx classification scans the first 8 KiB, and the
unread remainder is cancelled rather than buffered.
Rust Chat, Messages and Responses ingresses deserialize directly into their
request types; Gemini admission retains its native JSON source body. The
OpenAI provider serializes a borrowed per-hop wire view to
bytes; the marketplace retains and returns one shared Rust catalog allocation,
which is still serialized across IPC.

For Anthropic clients routed to Anthropic-compatible upstreams, canonical
objects retain an internal source envelope that is excluded from every public
OpenAI wire. The provider changes only the routed model and stream flag, then
the edge relays the original Anthropic body or event. This preserves thinking,
redacted thinking, cache controls, tool-block order, opaque signatures, and
`signature_delta`. Cross-protocol routes still use the explicit canonical
converter. A same-provider compatibility retry is allowed once only after an
explicit thinking/signature validation error.

Gemini-to-Gemini requests likewise retain their native body, including opaque
thought signatures and native controls; the routed model is carried by the
upstream method URL. Native response objects and SSE data objects remain
available to the Gemini client. A foreign hop receives only the supported,
strict canonical projection. Unsupported native controls, media, tool-choice
semantics, or source-body mutations fail admission instead of being silently
discarded. A foreign client also rejects Gemini output that its protocol cannot
represent, such as multiple candidates or native-only media/code parts. Private
source envelopes never serialize on another protocol's wire.

The Responses provider preserves native Responses envelopes when applicable.
Recognized MiniMax API endpoints enforce their stateless subset before sending:
a nonempty `previous_response_id` or non-null `conversation` returns
`responses_upstream_state_unsupported`. Fully materialized input history remains
eligible. Neither a `store` flag nor a successful response establishes remote
conversation persistence, and other native Responses providers retain their
supported linkage behavior.

Every large JSON POST ingress (`/v1/messages`, `/v1/responses`,
`/v1/chat/completions`, and the `/v1beta` Gemini `generateContent`,
`streamGenerateContent`, and `countTokens` methods) shares one route-local
32 MiB JSON body cap so base64 image blocks, Codex/Claude long sessions with
large tool outputs, and Gemini inline data can pass the local edge without
disabling body protection across the proxy. Requests above the cap remain
rejected before routing; the configured upstream may impose a smaller
platform-specific limit.

Each provider owns one reusable HTTP client for the lifetime of the proxy
snapshot. Connection/header time, first event, completion, token usage, and
client disconnects are recorded against a request correlation id without
logging request or response content.

## Unified supplier routing

A supplier edit reaches the wire through one path, and the layers keep their
ownership:

```text
supplier / endpoint / model offer / catalog evidence   (services, SQLite)
        -> supplier_runtime_projections                 (endpoint -> proxy provider)
        -> runtime snapshot: providers, routes, planner (services/proxy_runtime)
        -> router resolution                            (proxy layer, no service deps)
        -> upstream attempt + failover                  (proxy/providers, breakers)
```

The proxy layer never imports the supplier or catalog services. The capability
planner is a trait (`proxy::planner::Model_planner`) that the supplier and
catalog integration implements; the router only sees resolutions
(`Exact`, `Precise`, `Alias`) and stable error codes. A supplier endpoint
without a routable projection is not offered, and a manual proxy provider
without a supplier projection keeps working for exact calls. Additive URL
discovery stays inside the supplier owner: it may append a validated endpoint,
its models and projection, and a new protocol may share an existing API root;
protocol identity, not URL uniqueness, decides whether the endpoint already
exists. It creates no admission record and does not promote catalog listing to
inference success.

Resolution first enforces selector semantics (`precise:` / `alias:` namespaces,
offered exact models, and known capability aliases). Explicit routes and pins
can restrict eligible hops but cannot change an exact model or bypass required
capabilities. Unclaimed legacy selectors retain their explicit-route and
exact/queue behavior. A precise resolution with no eligible offer fails with
`model_precise_unavailable`, and an alias with no qualifying offer fails with
`model_alias_no_candidates`; neither falls through to a blind send.

A supplier's own capability facts always win. The facts a supplier left
unknown are filled from the public catalog (models.dev), so an offer can
qualify for an alias on public metadata alone — including a vendor-prefixed
id (`anthropic/claude-fable-5.1` resolves to the catalog's `claude-fable-5.1`).
A model the catalog does not know stays unknown and is never pulled into an
alias automatically.

Seven client-facing role IDs use this same planner on Messages, Responses,
Chat Completions, and Gemini ingress: four Claude roles and three GPT-5.6 roles.
They request capability tiers; they do not identify the supplier model that
actually answers. Desktop catalogues expose their client-specific subset, while
dispatch repeats capability filtering for the actual request. An unranked
selected default cannot become a fallback for every role. An actually offered
bare model name and `precise:` retain exact-model precedence. The complete role
table is in [unified suppliers](unified_suppliers.md#client-facing-role-ids).

Public assessment has three separate evidence sources: models.dev capability
metadata, EvalPlus coding scores, and the official LMArena overall text ranking.
Manual overrides take precedence, followed by an exact Arena match, comparable
coding evidence, reliable same-family ordinal evidence, a known role hint, then
pending. Price and context length never become strength evidence. Arena matching
keeps version/variant/date identity and evaluation conditions; an unqualified
model with several thinking conditions uses a conservative result and exposes
the ambiguity.

For routing, Arena-rated models in admitted enabled credentialless supply form
one distinct-model cohort. Multiple protocol offers of the same rated identity
do not enlarge it. Published rank orders that cohort into four relative bands
(`available_model_rank_quartiles_v1`); the original rank, leaderboard size,
rating interval, votes, publication date, and compared cohort size remain visible.
This is a declared routing policy, not a universal intelligence measurement.
Missing evidence stays missing: `MiniMax-M3.1-Flash` cannot inherit an Arena row
from `MiniMax-M3` or another MiniMax version/variant.

When mirror roles are enabled, one application-owned catalog worker checks at
startup and hourly, refreshing sources whose successful cache is older than
24 hours, missing, or marked with an error. Manual refresh shares its admission
gate. A failed fetch/parse preserves the last good document with stale/error
metadata and retries on a later check. Parseable last-good evidence remains
available with those stale/error flags; a partial ranking never replaces it.
The Arena collector obtains an independent overall
count from complete official statistics, scans bounded row pages, and validates
the final same-date ranking. The remote API does not provide an atomic revision
binding between those reads. See the [official dataset](https://huggingface.co/datasets/lmarena-ai/leaderboard-dataset)
and [Dataset Viewer statistics contract](https://huggingface.co/docs/dataset-viewer/statistics).

Quota exhaustion also has account-level scope within one admitted request.
Endpoints with the same resolved API credential and HTTP origin share an opaque
quota identity, so an explicit account/plan allowance rejection skips their
other protocol/model hops. Ordinary 429/RPM/TPM errors and model rate limits
retain eligible same-account alternatives; they do not prove that an account
bucket is exhausted. Network failures retain ordinary wire fallback;
authentication and invalid-request failures remain terminal. This state is
request-local: AAG neither invents a five-hour reset deadline nor polls vendor
allowance. Stream fallback remains limited to the period before output commits.

The routing fingerprint (`routing_revision()`) covers proxy providers, routes,
per-app providers, the supplier configuration generation, the pinned provider,
and the public catalog cache. Every one of those changes moves the fingerprint,
and the snapshot rebuild is published atomically only when the generation is
unchanged. In-flight streams keep the snapshot they started with, and
connection pools plus circuit breakers survive the rebuild. A failed rebuild
keeps the previous snapshot and surfaces the failure through the proxy status
(`hot_reload`). `proxy_status.config` is the config the running listener
actually uses, and the saved config while the gateway is stopped, so the UI
displays and health-checks that one endpoint instead of re-reading the saved
config and guessing which one is live.

The user-facing contract for all of this — selector forms, error codes,
session stability, import, and the accepted bare-offered-name-over-alias rule
— is documented in [unified suppliers](unified_suppliers.md).

## Diagnostics and crash evidence

Production logging defaults to `Info`. The global Diagnostics dialog can raise
AAG-owned targets to `Debug` for 30 minutes; dependency targets remain at
`Warn`. The deadline is stored in `app_meta`, so a restart restores only the
remaining interval. Logs use local time, rotate at 2 MiB, and retain five
files in the Tauri application log directory. With an explicit nonempty
`AAG_CONFIG_DIR`, all application, panic and bootstrap logs instead use its
managed-private `logs/` directory. Invalid overrides fail closed rather than
falling back to live desktop logs. The main window is created explicitly during
setup so an explicit `AAG_CONFIG_DIR` can also bind WebView2 to its
managed-private `webview/` directory before Tauri applies the live local-data
default. Without that override, the normal application profile is preserved.

Native panic/startup hooks, Tauri lifecycle events, tray/window actions, proxy
request stages, React error boundaries, and unhandled browser errors feed the
same local evidence boundary. Frontend IPC sends only a fixed error category,
bounded message/stack character counts, and a correlation id; raw messages and
stacks never cross the native boundary. Provider, route, and model identifiers
are represented by truncated SHA-256 references in diagnostics. External
errors use stable codes and generic messages with status, stage, retriable
classification, correlation id, and response byte count where available.
Prompts, responses, tool payloads, credentials, absolute paths, full upstream
URLs, domains, email addresses, and IP addresses are never diagnostic fields.
There is no remote telemetry or memory-dump upload. A process-local 200-entry
request timeline retains only bounded safe references and fixed metadata. Its
body-lifetime guard records consumed streams, stream errors, and disconnects
instead of treating successful SSE headers as completed requests. Only the
trusted manager window can read or clear the timeline.

## Persistence and ownership

- AAG database: `~/.aag/aag.db`, override with `AAG_CONFIG_DIR`.
- The config tree is AAG-owned wherever it lives: an explicit
  `AAG_CONFIG_DIR` is created/hardened through the same managed-private
  policy as the default `~/.aag` (Windows inherited group-writer ACEs on the
  parent tree would otherwise fail every private write).
- Node compatibility config: `~/.ai-agent-gateway/config.json`, also
  overridable with `AAG_CONFIG_DIR`, plus one exact rolling pre-overwrite
  backup.
- Agent paths: derived from the user home directory, with `AAG_HOME_DIR` only
  for controlled tests and portable runs.
- Schema version: v25 (v21 adds unified suppliers, endpoints, model offers,
  runtime projections, public catalog cache, and supplier configuration
  generation; v22 and v23 add the `icon` / `icon_color` display columns on
  `proxy_providers` and `suppliers`; v24 adds endpoint alternatives and ensures
  the private merged-import origin journal; v25 adds catalog-retirement
  bookkeeping on `supplier_models` — `retire_misses`, `retired_at`,
  `retire_reason`).
- Provider rows are keyed by `(id, app_type)` and route hops reference proxy
  provider ids.
- `app_meta` stores device-level settings such as isolated Codex CLI state.
- Temporary diagnostics state uses the `diagnostics_debug_until` app-meta key;
  it does not require a schema migration.
- Backups and sync metadata are separate from provider and route state.

### Mirror instruction ownership

Instruction synchronization is on by default (only a stored
`mirror_instruction_sync_enabled=0` opts out) and selects source authority for
the documented Codex and Claude instruction roots, including `prompts/` on both
clients. One application-owned worker and launcher reconciliation share the
mutation gate. Complete inventories authorize backed-up additions, changes and
deletions; unavailable or ambiguous source input retains the last good mirror
rather than being interpreted as an empty source. Vendor `skills/.system` stays
excluded. Skills project from the AAG-owned SSOT root (`services/skills_ssot`,
`~/.aag/skills`), which reconciles the trusted shared roots with a content-hash
manifest and the `skills` DAO catalog before each pass. The exact roots are
listed in [unified suppliers](unified_suppliers.md#source-authoritative-mirror-instructions).

Codex additionally projects missing instruction-only skill trees from enabled
native plugin configuration and validated package manifests. The projection
uses the sole unambiguous actual installed version, and excludes skill names
already available from user/shared skills or configured mirror plugins. A cache
`latest` junction is accepted only as a duplicate of an already enumerated
ordinary direct version directory in that same package; it cannot choose
between multiple actual versions. Ambiguity blocks projection and preserves the
last good inventory. Plugin servers, account state and activation stay with
their existing owners.

The only merged Codex configuration preferences are `agents.enabled` and
`agents.max_concurrent_threads_per_session`. The desired TOML derives from the
caller-bound mirror snapshot and commits through the private backed-up mirror
writer, so an intervening client write is rejected. Routing, authentication,
permissions, MCP, compatibility features and plugin activation remain separate
configuration responsibilities; instruction synchronization does not clone the
whole native configuration.

Credential-bearing local state has one private filesystem boundary. Before
writing bytes, AAG rejects symlink/reparse components and owner mismatch,
creates private directories, uses exclusive random same-directory temporary
files, preserves tighter safe permissions, and verifies the result after
atomic replacement. POSIX uses `0700` directories and at most `0600` files.
Windows uses protected current-owner-only DACLs without elevation. SQLite's
main, WAL and SHM files use the same boundary; in-database plaintext backups
are capped at ten and use SQLite secure deletion.

These controls provide account-level isolation, not encryption at rest.
Provider keys, MCP environment/header values, proxy tokens, and backup payloads
remain plaintext to the AAG process. A compromised process running as the same
OS account can still read them; secret references should be preferred where a
provider integration supports them.

The backend owns persistent state. The UI owns view state and calls typed IPC
wrappers; it must not access SQLite or another project's private database.

## Compatibility boundaries

- Node and Rust proxies both use OpenAI-shaped canonical types internally.
- Chat clients use `/v1/chat/completions`, Responses clients `/v1/responses`,
  Anthropic clients `/v1/messages`, and Gemini clients the native `/v1beta` methods.
- The Anthropic surface also serves `/v1/messages/count_tokens` (Claude Code
  measures context every turn): Anthropic-family hops relay upstream with the
  routed upstream model and the client's relayed credential, other hops serve
  a deterministic local estimate. The `/v1/messages` auth relaxation (loopback
  bind + enabled `anthropic_passthrough` row) covers it identically, and the
  client's `anthropic-beta` flag list rides along on passthrough hops only.
- Empty proxy auth disables authentication; configured auth accepts Bearer or
  `x-api-key` according to the server surface.
- Retriable failures are network errors, 429, 5xx, recognized structured
  quota/rate rejections, and the explicit pre-output tool-capability HTTP 400
  exception for requests with tools. Other 4xx errors, including ordinary 402
  and 404 responses, propagate without failover; authentication never becomes quota.
- Browser demo data is synthetic and is not evidence of native IPC, real
  upstreams, live agent writes, signing, or installation behavior.

## Change ownership

Use the smallest module boundary that owns the behavior:

- Provider persistence: `database/dao/providers.rs` and provider IPC commands.
- Proxy lifecycle and route persistence: `commands/proxy.rs` and proxy DAOs.
- Agent file formats: one adapter under `services/provider/`.
- UI state lifetime: the relevant page context, hook, or client-management provider/route state.
- Browser evidence: `scripts/ui_smoke/`, never ad-hoc calls to an unrelated server.

Do not introduce a shared cross-language library, move private state, or merge
the Node and Tauri runtimes without a separate compatibility plan.

## Managed DSH extension

The DSH page is a separate lazy-loaded management surface. It reuses saved
Claude providers and the running gateway without editing native Claude files.
It also contains an AAG-owned Claude-style workspace shell for reviewing
provider/model selection; the actual prompt execution remains in the isolated
native DSH WebUI.
Its Rust service owns a dedicated filesystem profile below the AAG configuration
directory; SQLite remains authoritative for upstream provider records.
See [Managed DSH integration](dsh_management.md) for supported runtime versions,
credentials, transaction boundaries and deferred capabilities.

Within that service, runtime_environment.rs owns managed directory preparation
and filtered child environments, owned_process.rs owns child lifetime and
descendant cleanup, and runtime.rs coordinates the DSH instance/readiness.
Plugin transactions share the environment and process adapters rather than
depending on the DSH lifecycle for those responsibilities. The managed home
isolates configuration; it does not restrict filesystem permissions.

The DSH Plugin Marketplace is a separate bundled route and window. It reads a
public, commit-bound catalog and exposes only catalog read/refresh and validated
source links. It does not mount the manager shell or receive provider/runtime
permissions. See [DSH marketplace](dsh_marketplace.md).

## Automatic DSH setup and stream completion

The DSH manager passively discovers installed runtime metadata and prepares
review-bound setup plans. Application writes only managed settings/profile
state, preserving existing provider/model choices and native app configuration.
Automatic exact-model routing uses surface priority then stable provider IDs;
explicit routes and pins remain unchanged. Stream attempts settle success only
after real completion; errors and cancellation retain distinct outcomes.
See [plug-and-play contracts](dsh_plug_and_play.md).

## MCP management and companion boundaries

The existing `mcp_servers` table remains the registry source of truth. Manager
IPC validates definitions and returns only redacted summaries. New definitions
cannot overwrite existing IDs; updates use revision checks. Client flags change
only the registry. Claude synchronization requires a one-use, expiring preview
bound to registry/client snapshots, explicit confirmation, a retained backup,
and atomic replacement. Windows replacements preserve the existing owner/group/
DACL before writing private content. Codex is redacted preview-only.

The separately runnable stdio companion imports only a static shared workflow
and verification catalog. It cannot call manager IPC, read live state, launch a
server, execute a verification plan, or approve a write. The local CLI runner is
an explicit user-invoked execution surface, not an MCP tool or recurring job.

See [MCP management](mcp_management.md), [companion and automation](mcp_companion.md),
and [DSH hardening](dsh_hardening.md) for contracts and limitations. Local and
synthetic evidence for this feature slice is recorded separately from historical
release status in [workspace validation](reviews/mcp_dsh_workspace_validation.md).

## Workflow registry and local read facade

`src-tauri/src/services/workflow_registry/` owns AAG's read model for workflow
capabilities: descriptors from `shared/workflow_catalog.json` (embedded at build
time) plus redacted MCP registry summaries, and per-stage observations that each
carry their own evidence source and expiry. Catalog presence is a description
claim, not proof of installation, configuration, access, or a successful
invocation.

- Manager commands are `workflow_list_capabilities`, `workflow_get_health`,
  `workflow_route_preview`, `workflow_probe_capability`, and
  `workflow_evaluate_route`. Only the explicit probe runs a fixed `--version`
  read, and a success establishes `installed` alone.
- `workflow_route_preview` invokes the authoritative Python router as fixed
  argv with bounded stdin, output, working directory, and deadline, then
  validates the returned policy. Rust never reimplements scoring, and local
  adviser or Jev advice stays observation-only and cannot change the
  deterministic route or the final gate.
- Host interpreter, router, project, and probe paths are private
  `workflow_config.json` values validated through the existing private-file
  policy; no host-specific path is bundled in source.
- On Windows the process owns one named pipe with a current-user ACL for the
  Node MCP facade in `scripts/workflow/`. It serves `list_capabilities`,
  `get_health`, and `route_preview` with bounded frames and cursor paging, and
  stays unavailable on other platforms.

See [workflow management](workflow_management.md) for the command table, the
private configuration contract, and the evidence boundary.

## Prompt enhancement

The prompt-enhancement pipeline turns a short Chinese requirement into two
artifacts: a complete Chinese brief, and one optimized English prompt. It is
owned by `src-tauri/src/services/prompt/` and exposed through the IPC commands
`prompt_enhance_run` and `prompt_enhance_defaults`; the frontend page is
`ui/src/components/prompt/`.

- `zh_expand` and `en_optimize` are separate stages and separate IPC calls, so
  the UI renders the Chinese brief while the English stage is still running, and
  the user can edit that brief before re-running only the second stage.
- A stage target is either a `gateway` route alias, resolved by the RUNNING
  router so route hops, failover, and circuit breakers apply, or a
  `provider`/`model` pair called directly, which also works while the gateway
  is stopped.
- Requests are non-streaming chat completions built from `templates.rs`. User
  overrides live in the `app_meta` settings rows `prompt_enhance_zh_system` and
  `prompt_enhance_en_system`: no new table and no schema migration.
- Stage input and generated response text are not persisted server-side. Custom
  stage templates remain in SQLite settings; history and the selected backend
  live in browser `localStorage`. History retains up to eight entries, including
  their source input and Chinese brief. Prompt or response text is never a
  diagnostic field — logs carry stage ids, character counts, timings, and an
  opaque target reference only.
- Errors cross IPC as stable codes (`prompt_enhance_gateway_not_running`,
  `prompt_enhance_upstream_failed:<code>:<status>`, and so on). Upstream messages
  are never forwarded, because a provider error can echo request text.
- Optional workspace context reads only explicitly selected root instruction
  files. Both stages bind to the same workspace revision and content version;
  new tasks default to no file context. Workspace metadata uses revision-checked
  `app_meta` JSON rather than a new table. See [prompt workspaces](supplier_workspace.md#prompt-workspaces).

