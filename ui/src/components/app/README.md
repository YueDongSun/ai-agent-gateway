# `ui/src/components/app/` — Per-app page subtree

The shared per-app page that renders the unified provider cards, the
gateway panel, the advanced route editor, and the per-app launchers.
Every app (Claude, Codex, Copilot, OpenCode, Kilo, VS Code variants)
gets the same layout plus app-specific form fields and launchers driven
by [`AppPageConfig`](../../lib/appPages.ts).

The provider area is the **single failover-chain surface**: one
cc-switch-style card list per app page merges the per-app preset chain
(`providers` table) and the linked `ccs-*` upstream pool rows
(`proxy_providers` table). Card order = failover priority. Per-alias
routes live in the collapsed "Advanced routing" section below the cards
(routing pages only).

The split-state contexts in this directory (providers data / providers
actions, routes data / routes actions) are the result of the Phase
23c–23e repair: replacing unsafe action memos that captured the empty
initial state with current callbacks. The split is enforced; mixed
`state.tsx` modules have been removed.

## Anchor files

- [`AppPage.tsx`](AppPage.tsx) — page orchestration. Loads providers,
  the upstream pool, routes, gateway status, and pinned state; merges
  the card list via [`mergeProviderCards`](../../lib/unified_providers.ts);
  wires the data / actions contexts; renders `GatewayPanel`,
  `ProvidersPanel`, `AdvancedRoutes` (routing pages), and `Launchers`.
  Owns the per-app route cache and dirty-state tracking, and the three
  save paths (preset row / linked upstream / standalone upstream).
- [`GatewayPanel.tsx`](GatewayPanel.tsx) — start / stop control panel
  with the base URL + API key copy block, the Codex mirror row (Windows
  only), the `/v1/responses` step-by-step-reasoning toggle (Codex surface
  only; live — no restart), and an edit-settings modal.
- [`providers_panel.tsx`](providers_panel.tsx) — the unified card list.
  Header (Refresh / Add), empty state, standalone-upstream divider, and
  pointer-based drag orchestration (HTML5 DnD is unreliable in
  WebView2) with a keyboard reorder path (↑/↓/Home/End). Styles in
  [`providers_panel.css`](providers_panel.css) (cc-switch card layout
  over the shared `:root` tokens).
- [`provider_card.tsx`](provider_card.tsx) — one memoized card: drag
  handle, icon tile, name + status badges (CURRENT, P1/P2 priority, OUT,
  DISABLED, PINNED, ACTIVE, breaker dot), URL line, hover-revealed
  actions, and the test-result row. The switch action is hidden on the
  claude_code page (cc-switch owns `~/.claude/settings.json`; AAG is
  only an upstream there).
- [`provider_modal.tsx`](provider_modal.tsx) — the unified create/edit
  modal: Identity | per-app Connection fields (`cfg.fields`) | Gateway
  upstream section. Standalone cards edit the pool row directly.
- [`advanced_routes.tsx`](advanced_routes.tsx) — collapsible wrapper
  demoting the RouteEditor below the cards. Mounts `RouteEditor` only
  while open; open state survives tab switches via a module map.
- [`RouteEditor.tsx`](RouteEditor.tsx) — alias → ordered hop editor with
  batch selection, drag-to-reorder, and per-hop provider / model editing.
  Reads `RoutesData` / `RoutesActions`.
- [`Launchers.tsx`](Launchers.tsx) — Codex Desktop + CLI / Copilot CLI
  launcher cards.
- [`providers_state.tsx`](providers_state.tsx) — split context.
  `ProvidersData` (cards, currentId, pinnedId, health, testResults,
  busy, breaker helpers) and `ProvidersActions` (switch, chain toggle,
  enable, pin, test, edit, delete, reorder, refresh). Two contexts on
  purpose; a component can subscribe to data only when it does not need
  actions.
- [`routes_state.tsx`](routes_state.tsx) — split context.
  `RoutesData` (routes, filteredRoutes, isRouteDirty, selection, drag
  state, fetched models, proxyProviders, per-tab cache) and
  `RoutesActions` (select, add, save, discard, hop edits, drag, restart).

## Key contracts

- `AppPage` is the only place that holds the full state for a page.
  Children consume the context APIs; they must never copy data out of
  the context and render against the copy.
- Callbacks exposed by `RoutesActions` and `ProvidersActions` always see
  current state. Memo dependencies cannot be omitted.
- A card's two stores stay in sync through the `ccs-<presetId>`
  convention ([`unified_providers.ts`](../../lib/unified_providers.ts)):
  saving a preset also upserts the linked `ccs-<id>` upstream (skipped
  for empty or self-loop base URLs, mirroring the importer's guard).
  Deleting a linked card removes both rows in one confirm.
- `RouteEditor` reuses the `RoutesData` / `RoutesActions` contexts;
  there is no per-page route copy.
- The `LazyLoad` boundary from `App.tsx` keeps the initial bundle
  small: page modules are loaded on demand.

## See also

- [`../`](../README.md) — sibling directories in `ui/src`.
- [`../../lib/appPages.ts`](../../lib/appPages.ts) — per-app config that
  drives `AppPage`.
- [`../../lib/unified_providers.ts`](../../lib/unified_providers.ts) —
  card merge + `ccs-<id>` linkage helpers.
- [`../../lib/api/`](../../lib/api/README.md) — typed IPC wrappers used
  by `AppPage`.
- [`../../hooks/`](../../hooks/README.md) — `use_gateway_status`.
- [Root README](../../../README.md).
