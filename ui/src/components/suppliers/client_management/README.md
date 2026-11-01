# Supplier-owned client management

The supplier-owned per-client controller that renders the unified provider cards, the
gateway panel, the advanced route editor, and the per-app launchers.
Every managed client (Claude, Codex, Copilot, OpenCode, Kilo, VS Code variants)
gets the same management layout within Suppliers plus app-specific form fields and launchers driven
by [`AppPageConfig`](../../../lib/appPages.ts).

The provider area is the **single failover-chain surface**: one
cc-switch-style card list per selected client inside Suppliers merges the per-app preset chain
(`providers` table) and the linked `ccs-*` upstream pool rows
(`proxy_providers` table). Card order = failover priority. Per-alias
routes live in the collapsed "Advanced routing" section below the cards
(routing pages only).

The split-state contexts in this directory (providers data / providers
actions; routes data / routes actions / routes drag) are the result of
the Phase 23c–23e repair: replacing unsafe action memos that captured
the empty initial state with current callbacks. The split is enforced;
mixed `state.tsx` modules have been removed.

## Anchor files

- [`../supplier_discovery_details.tsx`](../supplier_discovery_details.tsx) —
  localized discovery help and the existing stored report. Report labels use the
  current shared EN/CN locale, so switching language does not rerun discovery.
  Refresh failures retain their stable codes until render-time translation.

- [`supplier_client_settings.tsx`](supplier_client_settings.tsx) — page orchestration. Loads providers,
  the upstream pool, routes, gateway status, and pinned state; merges
  the card list via [`mergeProviderCards`](../../../lib/unified_providers.ts);
  wires the data / actions contexts; renders `GatewayPanel`,
  `Client_config_status`, `ProvidersPanel`, `AdvancedRoutes` (routing
  pages), `Route_explanation` (routing pages), and `Launchers`. Owns the
  per-app route cache, dirty-state tracking, the gateway settings modal,
  and the three save paths (preset row / linked upstream / standalone
  upstream).
- [`GatewayPanel.tsx`](GatewayPanel.tsx) — start / stop control panel
  with the base URL + API key copy block, the Codex mirror row, the
  Claude mirror row, the `/v1/responses` step-by-step-reasoning toggle
  (Codex surface only; live — no restart), and a gear button that opens
  the settings modal owned by `Supplier_client_settings`.
- [`providers_panel.tsx`](providers_panel.tsx) — the unified card list.
  Header (Refresh / Add), empty state, a search field over card name and
  endpoint with a visible match count and no-results state, the
  standalone-upstream divider, and pointer-based drag orchestration (HTML5 DnD
  is unreliable in WebView2) with a keyboard reorder path (↑/↓/Home/End).
  Search filters by source index, so a filtered list can never persist a new
  priority; drag and keyboard reorder are disabled while filtered or busy, and
  Escape clears the query. Styles in
  [`providers_panel.css`](providers_panel.css) (cc-switch card layout
  over the shared `:root` tokens).
- [`provider_card.tsx`](provider_card.tsx) — one memoized card: drag
  handle, icon tile, name + status badges (CURRENT, P1/P2 priority, OUT,
  DISABLED, PINNED, ACTIVE, breaker dot), URL line, primary actions, and the
  test-result row. Secondary actions (chain, enable / pin / auto-track toggles,
  provider test, delete) stay behind an explicit `More` / `Less` disclosure with
  `aria-expanded` / `aria-controls`; Escape closes it and returns focus to the
  disclosure. The switch action renders unless `cfg.detectOnly` is
  set — no page config sets it today.
- [`provider_modal.tsx`](provider_modal.tsx) — the unified create/edit
  modal: Identity | per-app Connection fields (`cfg.fields`) | Gateway
  upstream section. Standalone cards edit the pool row directly. It is a
  labelled `role="dialog"` / `aria-modal` surface; `use_accessible_dialog`
  provides initial focus, Tab containment, scroll lock, and Escape (ignored
  while saving), and focus returns to the card's edit button on close.
- [`preset_picker.tsx`](preset_picker.tsx) — searchable, category-grouped
  vendor template picker using the same accessible-dialog contract.
- [`advanced_routes.tsx`](advanced_routes.tsx) — collapsible wrapper
  demoting the RouteEditor below the cards. Mounts `RouteEditor` only
  while open; open state survives tab switches via a module map.
- [`RouteEditor.tsx`](RouteEditor.tsx) — alias → ordered hop editor with
  batch selection, drag-to-reorder, and per-hop provider / model editing.
  Reads `RoutesData` / `RoutesActions` / `RoutesDrag`.
- [`route_explanation.tsx`](route_explanation.tsx) — read-only
  `details` panel that restates the effective chain per route (provider,
  pinned/active/disabled badges, applied model mapping) plus the breaker
  settings of every referenced provider. Contents mount only while
  expanded.
- [`Launchers.tsx`](Launchers.tsx) — per-app launcher cards: Codex
  Desktop + CLI, Claude Desktop + CLI (both through the mirror homes),
  Copilot CLI, Gemini CLI, VS Code with an injected environment, and the
  managed CLI (Kilo / Cline).
- [`client_config_status.tsx`](client_config_status.tsx) — read-only
  per-app configuration discovery card (detected paths, platform, mode);
  hosts the scoped direct-JSON editor button for the Kilo family.
- [`config_editor_modal.tsx`](config_editor_modal.tsx) — the cc-switch
  style direct JSON/JSONC editor behind that button: verbatim read,
  debounced validation against the shared JSONC grammar, atomic save
  with a backup.
- [`providers_state.tsx`](providers_state.tsx) — split context.
  `ProvidersData` (cards, currentId, pinnedId, health, testResults,
  busy, breaker helpers) and `ProvidersActions` (switch, chain toggle,
  enable, pin, test, edit, delete, reorder, refresh). Two contexts on
  purpose; a component can subscribe to data only when it does not need
  actions.
- [`routes_state.tsx`](routes_state.tsx) — split context.
  `RoutesData` (routes, filteredRoutes, isRouteDirty, selection,
  collapsed state, known models, fetched models, proxyProviders),
  `RoutesDrag` (pointer-drag state, deliberately separate so a
  pointermove does not churn the shared data value), and
  `RoutesActions` (select, add, save, discard, hop edits, drag, restart).
  The per-tab route cache is AppPage-local and not part of the context.
- [`gateway_state.ts`](gateway_state.ts) — the gateway-only hook
  (`use_gateway_state`): running/url/auth/health/pin/CoT state, the
  health poll with visibility pause, and the actions built by
  [`gateway_flow.ts`](gateway_flow.ts).
- [`gateway_flow.ts`](gateway_flow.ts) — gateway actions (start/stop,
  settings save with baseline re-read, pin, CoT), testable without React
  via injected commands.
- [`import_flow.ts`](import_flow.ts) — the cc-switch import flow
  (stop/restart the proxy around an import, then refresh), testable via
  injected commands.

## Key contracts

- `Supplier_client_settings` is the only place that holds the full state for a page.
  Children consume the context APIs; they must never copy data out of
  the context and render against the copy.
- Callbacks exposed by `RoutesActions` and `ProvidersActions` always see
  current state. Memo dependencies cannot be omitted.
- A card's two stores stay in sync through the `ccs-<presetId>`
  convention ([`unified_providers.ts`](../../../lib/unified_providers.ts)):
  saving a preset also upserts the linked `ccs-<id>` upstream (skipped
  for empty or self-loop base URLs, mirroring the importer's guard).
  Deleting a linked card removes both rows in one confirm.
- `RouteEditor` reuses the `RoutesData` / `RoutesActions` contexts;
  there is no per-page route copy.
- The `React.lazy` boundary in `App.tsx` keeps the initial bundle
  small: page modules are loaded on demand.

## See also

- [`ui/src`](../../../README.md) — source directories and their ownership.
- [`../../../lib/appPages.ts`](../../../lib/appPages.ts) — per-app config that
  drives `Supplier_client_settings`.
- [`../../../lib/unified_providers.ts`](../../../lib/unified_providers.ts) —
  card merge + `ccs-<id>` linkage helpers.
- [`../../../lib/api/`](../../../lib/api/README.md) — typed IPC wrappers used
  by `Supplier_client_settings`.
- [`../../../hooks/`](../../../hooks/README.md) — gateway status, discovery, and
  workspace/surface hooks.
- [Root README](../../../../../README.md).
