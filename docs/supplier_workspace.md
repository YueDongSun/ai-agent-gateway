# Supplier workspace

Suppliers is the provider-management entry point. Its **Providers**, **Arena**,
and **Tailnet** tabs separate inventory editing from read-only intelligence.
Each tab retains a visible loading, ready, paused, empty, or error state. Client
pages show current-configuration router models and their Arena.ai position;
provider controls remain under Suppliers.

## Saving and recovering edits

**Save models** writes only the selected protocol's model inventory and keeps
the editor open. General settings, credential intent, endpoint edits and other
protocols remain visible and dirty. **Save** writes the complete form. If the
data is saved but gateway/client setup fails, the editor adopts the persisted
ID and generation, reports setup separately and retains the remaining draft.
Retrying setup does not create another supplier or resubmit saved credentials.

Supplier and import dialogs retain focus ownership and block Close/Escape
during non-cancellable mutations. Completion belongs to the originating dialog;
it cannot dismiss a newer editor. Closing an idle editor dismisses that draft
and does not assert that a backend operation was cancelled.

Route Save and Discard lock editing, Add/Delete/Rename, model pickers and both
keyboard/pointer reordering until their operation settles. Save is atomic per
route, not across all routes. It waits for every write and reads back only the
selected aliases: persisted changes, failed changes and runtime-apply errors
remain distinct. Failed drafts stay visible. If readback is unavailable, a
later Save first reconciles that unknown outcome before issuing more writes.
Discard reload failure keeps the draft. Unrelated concurrent routes never
become this editor's deletion scope.

Rename of a new, unsaved route changes only its local draft; Save persists it
and Discard removes it. Rename of an existing route applies immediately through
the atomic backend rename. A target conflict preserves the original route.
An uncertain rename outcome must be read back before another write. Save or
Discard route edits before changing the managed client or destination.

Every Coding Agents page also shows the model-position chart directly below
Current plan. It compares only that agent's configured model/tier selections,
using a fixed client API surface. Its graph clears on a plan or destination
change. No configured model produces a visible empty state rather than the
gateway's default model. Rankings, uncertainty and routing paths reuse the same
shared chart as Suppliers; the global last-observed request is not displayed as
that agent's activity. Refresh reads public evidence and sends no model request.

## Import consistency and endpoint ownership

`services/import_cc_switch/` reads the source database read-only. Future previews
exclude imported OAuth/account entries, including the declared Codex
`cliProxyAPI` bridge. Exclusion counts replace login/setup choices. Import
maintenance also reconciles already-imported AAG records before startup routing
and when Suppliers refreshes.

Removal requires source/provenance and current authentication/configuration
evidence. Renaming, recategorizing, or locking an unchanged OAuth record does
not turn it into a static API provider. A real static conversion or changed
valid static endpoint is preserved; known placeholder keys do not qualify.
Mixed suppliers retain valid static siblings or promote a valid alternate when
the removed primary was account-based. Native client account homes and the
source cc-switch database are outside the cleanup boundary.

Exact source ID/configuration also identifies account presets with no pool row.
An older source ID is reconciled only for an unowned `ccs-` projection with a
matching saved parent and the same current-source, account-only ChatGPT Codex
endpoint and empty key. Changed static coordinates, another client and supplier
ownership prevent that fallback. Unknown-origin account records remain stored;
unsupported account configurations are omitted from the management UI without
deleting them or exposing their login fields.

Before a mutating reconciliation, the DAO creates a private SQLite recovery
snapshot. One immediate transaction applies the plan only while its full input
revision still matches. Supplier/provider removals, owned projections and route
references are reconciled together. A durable setup-pending marker survives
restart until mirror/runtime reconciliation succeeds. Startup does not autostart
the proxy after failed reconciliation; the UI exposes the error and retry.
Maintenance receipts show removal, merge, preserved-static, conflict and backup
counts without credentials.

Same-provider imports merge only when the vendor identity and effective account
credentials agree. Protocol endpoints remain distinct. Multiple URLs for one
protocol are retained as `supplier_endpoints.alternatives_json`, including their
private credentials, headers, model metadata, priorities and routing projection
ownership. Original merged metadata is retained in private
`suppliers.import_origins_json`. Ordinary saves and model updates preserve these
variants. Genuine credential or manually changed-anchor conflicts stay separate
and are reported for review rather than silently overwritten.

The UI shows retained alternate URLs in expandable protocol details, together
with enabled state, priority, key presence and header/model counts. Read DTOs
never return API key or header values. Schema v24 adds the alternate-endpoint
storage and idempotently ensures the private origin journal.

## URL discovery and per-entry results

Saving a supplier revalidates only the requested suppliers: each is read by ID
and every projection has its own 20-second bound, so a corrupt or slow entry is
reported without cancelling later ones.

**Refresh models** evaluates every configured source context before catalog or
discovery requests. Disabled suppliers, endpoints, alternatives or providers;
suppliers with no configured endpoint; auto-tracking opt-outs; non-probeable URL
types; invalid stored URLs; inactive, unavailable or expired subscriptions;
obsolete configuration; failed screening; and recent non-available catalog health
other than `catalog_invalid` inside the 300-second window are reported and
skipped, and each keeps its current inventory. Preflight may still refresh
subscription status. An endpoint whose effective URL requires review is never
probed.

A successful catalog refresh may append a protocol discovered from both
same-origin OpenAPI 3.x documents. A non-Gemini catalog may be empty; Gemini needs
at least one listed model for its probe route, and its declared root must match
the runtime-normalized route exactly. Gemini also rejects any catalog model ID
that would escape its path slot (`url_validation_model_path_invalid`). The append
is additive: existing endpoints, alternate URLs, models and admission records
remain intact; a candidate that equals the supplier base reuses it rather than
duplicating the URL; only an already-known protocol is a no-op. The new endpoint
is listing-only (`inference_status: "unverified"`), and discovery does not change
that status.

Each preflight, per-document search, or URL-discovery outcome is a separate
`report.providers` entry; skipped and failed entries carry a stable
`supplier_import_excluded:`, `supplier_import_skipped:`, `supplier_url_skipped:`
or `supplier_url_failed:` reason, and per-document search failures include the
document index. A corrupt supplier-owned projection is isolated as a per-entry
configuration failure and does not block later suppliers; a corrupt legacy row
keeps its prior failure contract. Client management shows the latest discovery
report in an expandable results panel, including completed entries, skips,
exclusions and failures. Connection names are used when available, otherwise the
existing provider identifier is shown with a localized connection/record label. The stored report and metadata-only logs
retain their machine-readable codes; displayed reasons do not echo unknown URLs,
credentials or upstream bodies.

### English and Chinese discovery explanations

Use the existing **EN/CN** header switch. The inventory's **How automatic URL
discovery works / 自动发现接口地址的工作方式** disclosure explains configured-only
filtering, validation before adding URLs, preservation, duplicate prevention,
the availability retry window, and the distinction between listing and successful
model calls. The import dialog includes the same preservation boundary in the
selected language; save/import discovery notices use the action's translator.
Editor discovery failures retain the stable code until rendering, so the
operation context and its cause are translated together. Setup failures also
use the shared backend-error display mapping.

Catalog-health status, model counts, timing, failure counts and the unverified
generation label use the shared locale. The **Last discovery results / 最近一次发现结果**
panel in client management translates each stored reason and its stage. These
help, health and report views update when the language changes, without another
catalog/model request. Supplier names, model IDs, protocol identifiers, backend
codes and configuration records are not translated or rewritten.

The catalogs own the `suppliers.discovery.*` keys. Components use the existing
shared context, and `supplier_discovery_text.ts` translates recognized stable
codes with a generic fallback for unknown details. Backend IPC/report fields and
admission rules remain unchanged.

## Arena model position and route preview

`model_catalog/arena_visualization.rs` combines the existing validated official
Arena cache with a router preview. The chart shows the overall score
distribution, selected model, confidence intervals and measured gap to a rated
candidate of the Fable routing tier. Fable is a routing tier, not a fabricated
benchmark row. Missing exact matches remain **unrated**; absent or stale evidence
has an explicit state and publication/source label.

The client surface, role/exact selector, tools and image requirements change the
vector trajectory. A running gateway previews one admitted routing snapshot,
including explicit routes and pin; a stopped gateway uses saved configuration.
Preview copies rotation state and does not advance live queues or send a model
request. Session affinity and circuit admission still settle during actual
requests. The last recorded router model is labeled separately from
the preview. At most eight unique preview candidates are drawn, with unrated
candidates retained in the logical flow without invented plot coordinates.

The mounted panel rereads its local snapshot every 15 seconds. Refreshing public
evidence uses the existing shared catalog-refresh admission gate. Leaving the
tab stops panel polling. Vector feedback is short and disabled under
`prefers-reduced-motion`. Demo evidence is visibly synthetic.

Opening a graph with no ratings now starts one public-source refresh, even when
automatic model-role synchronization is off. Automatic loads recheck staleness
inside the native shared refresh gate; a queued reader reuses a fresh cache.
Explicit Refresh/Retry still forces an attempt. Failed source views are treated
as failures even when IPC succeeds: the graph shows a safe error and Retry,
retains any validated previous ratings, and does not loop empty-cache downloads.
No supplier settings or model-role synchronization preference is changed.

Source interpretation follows the [official Arena dataset](https://huggingface.co/datasets/lmarena-ai/leaderboard-dataset).
Human-preference ratings and confidence intervals do not establish universal
capability or the availability of a model at a particular supplier.

## Tailnet AAG detection

`services/tailnet_discovery/` enumerates peers through the trusted installed
Tailscale executable, then probes only validated Tailnet addresses from that
inventory. The current trusted executable resolver supports Windows; unsupported
platforms and missing/unavailable Tailscale have visible errors. Discovery does
not install software, modify Serve/firewall settings, scan address ranges or
send gateway credentials.

The port comes from enabled Tailnet publication settings, otherwise the saved
gateway port. Checks are bounded to 64 peers, eight concurrent probes and a
15-second scan deadline. Requests disable redirects and proxy inheritance,
require bounded JSON, and accept only the exact AAG service/schema/status marker.
The report separates confirmed online instances from auth-required, offline,
unverified, failed and skipped peers. An online card shows device name, actual
responding address/port, version and latency. This response check is not
cryptographic process attestation.

The gateway exposes **GET `/aag/discovery`** with exactly `service`,
`schemaVersion`, `status`, and `version`, plus `Cache-Control: no-store`. Only
that exact GET bypasses publication authentication; other methods and protected
routes retain authentication. The endpoint contains no provider or user data.
The optional `/aag/discovery?runtime=1` response adds the fixed `runtime_kind`
value `desktop` or `node`. The bare endpoint keeps its four-field legacy shape.
The marker identifies the responding implementation; it does not prove model
availability or an upstream request.

Manual checks work on demand. The default-off automatic option checks every
minute only while the Tailnet panel is open. The panel owns cancellation; the
native scan lease remains owned until terminal cleanup even if an IPC waiter
disappears. No background watcher or notification is created.

## Prompt workspaces

The prompt page can register, rename, select and remove up to 32 existing project
folders. Removing an entry removes metadata only. `prompt/workspaces.rs` owns
path/file validation; `database/dao/prompt_workspaces.rs` stores a versioned JSON
registry and recoverable previous value in `app_meta`. Compare-and-swap updates
reject stale registry edits.

Workspace text is default-off for new tasks. A task can opt into root-level
`AGENTS.md`, `CLAUDE.md`, or both. Names are matched case-insensitively, with
ambiguous case variants, unsafe/linked files, invalid UTF-8 and size errors
reported visibly. Each file is limited to 32 KiB and combined context to 48 KiB;
missing files contribute no text. There is no recursive project scan.

Preview returns file states and a content-bound version. The backend rereads
and validates the selected workspace and files for each stage; changed content
or workspace revision requires a refreshed preview. Both stages retain the
original task's workspace/file selection even if the global selection changes.
A new task resets the opt-in. Selected file text goes to the chosen model backend
as task context; it is never a diagnostic field. Optional `workspaceContext`
preserves requests that do not use workspaces.

## Contextual floating actions

`components/page_actions.tsx` provides square Save/Delete buttons with the active
target and clean, unsaved, busy or error state. Editable pages register their
existing callbacks; navigation removes the previous target. Read-only pages do
not inherit mutations. Saves retain validation and any review/acknowledgement
step, and deletes retain their confirmation. Duplicate in-flight actions are
disabled; stale completions cannot update a different page's action state.

The dock reserves content space and uses established theme tokens. In a modal it
is portaled into the top visible dialog so keyboard containment includes the
buttons. Narrow-screen controls remain square and clear of content and dialog
footers. Supplier forms, workspace metadata, MCP, DSH review, client JSON and
retained gateway/provider/route settings register the actions they actually own.

## Explicit Desktop mirror launch

Codex Desktop does not receive a supported profile-selection flag from AAG.
An explicit **Open Codex Desktop mirror** action therefore prepares the catalog
and selects AAG at the top level of the existing isolated Codex home, then records
exclusive mode. Complete model/TOML validation precedes guarded, backed-up file
replacement. The native home, `auth.json`, sessions and history are preserved.
Normal CLI coexist configuration remains unchanged until that explicit Desktop
action; the launch receipt reports the resulting mode so the UI reflects it.

Desktop preparation, mirror mode changes, supplier mirror writes and provider
switches share one admission guard. Asynchronous gateway preparation finishes
before that guard is taken. Initial instruction seeding preserves existing
`AGENTS.md`, `skills` and `agents` roots; ongoing refresh belongs to the optional
instruction-sync feature. An initialized Codex sandbox retains both setup files
and its runtime-owned ACLs. Partial sandbox setup is reported before copying.

Claude Desktop retains its isolated third-party gateway profile and existing
preflight. A source-level or synthetic launch receipt is separate from verifying
an installed application's actual isolated process. No model generation or
account login is part of the ordinary verification gate.

## Visual QA checklist

Use the synthetic browser demo for shareable screenshots. Record native,
real-network and installed-client observations separately.

The current automated round passes 26 feature checks plus 183 general UI checks.
It verifies all four widths, the complete narrow-screen plot, reduced motion,
workspace binding, validation/retry states, contextual actions, modal keyboard
containment and omission of a token-linked account without deleting its record.
The checklist below also retains manual and environment-dependent checks.

- [ ] At 1440, 900, 390 and 320 CSS pixels, all three tabs fit, panels have no
  page-wide horizontal overflow, and floating actions clear content and footers.
- [ ] Light/dark palettes, radii, typography and spacing use the existing design
  tokens; header/body blending remains continuous when changing tabs or scrolling.
- [ ] Tabs work by keyboard; loading/error/paused states remain visible after
  retries and tab changes. No previous page's selection or mutation target leaks.
- [ ] Reconciliation shows counts/backup/conflicts; account-login entries and
  forwarding options are absent. A preserved static connection remains usable.
- [ ] A merged vendor occupies one card and its protocol/alternate details retain
  all URLs and their individual state. Secrets remain concealed.
- [ ] Refresh models on a supplier with a skipped sibling and a validated new
  protocol: the skipped entry is reported without a catalog or discovery request,
  the endpoint appears once, and a second refresh reports no change.
- [ ] A prefix-wide `405` handler fails the sibling-path control, a Gemini route
  that runtime normalization would rewrite fails before I/O, and a validated
  protocol sharing the supplier base stores no duplicate URL.
- [ ] Arena shows source/date, uncertainty and the selected-model gap; empty,
  stale, unrated and missing-Fable states do not invent scores. Changing selectors
  redraws the vector route without generating a model request.
- [ ] Reduced motion removes route animation without losing the route itself.
- [ ] Tailnet is idle by default; manual scan, cancellation, offline/auth-required
  counts, confirmed address, automatic check and panel-close pause are visible.
- [ ] Workspace create/select/rename/remove persists. Context is off for a new
  task, file states are visible, unsafe/changed files block contextual execution,
  and the second stage retains the task's original context selection.
- [ ] Floating saves show validation/busy/success/error; deletes remain cancellable.
  Tab/Shift+Tab includes the dock inside dialogs, and navigation clears its target.
- [ ] Client pages and CLI pagination contain only their selected configuration's
  router models; all provider management remains in Suppliers.

Automated browser coverage currently checks the four widths, feature flows,
workspace binding, contextual actions and uncaught errors. A completed checklist
is evidence for the tested environment only; it does not prove native IPC,
real Tailnet peers, upstream models, installation or a production release.
