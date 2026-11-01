# Client model views and supplier management

## Contract

Each client destination displays only the router model selections in that
client's currently selected configuration. This is the confirmed meaning of
"current plan"; it does not mean the global catalog or the live planner's
available-model catalog. Missing selection, missing record, or missing model
fields produce an empty state. Failed or changed reads produce a retryable error.

The Arena.ai panel below Current plan compares those same configured selectors
with public ranking evidence and previews the matching API's routing strategy.
Its selector is restricted to the current plan; it does not borrow the global
default or load supplier inventory. Missing/unreadable plans show an explicit
comparison state. Changing agent or plan clears the previous graph. This is a
configuration preview, not attribution of a shared gateway request to one client.

The existing client routes and navigation labels remain unchanged. All nine
visible client destinations, including editor variants, use the same model-only
page. Navigation contains client identity only and does not load connection-form
metadata. Changing tabs performs no configuration write.

| Client | Selected model fields |
| --- | --- |
| Claude and Claude VS Code | `env.ANTHROPIC_MODEL`, falling back to legacy top-level `model`; explicit `ANTHROPIC_DEFAULT_OPUS_MODEL`, `ANTHROPIC_DEFAULT_SONNET_MODEL`, `ANTHROPIC_DEFAULT_HAIKU_MODEL` |
| Gemini | `env.GEMINI_MODEL` |
| Codex, Copilot, Cline CLI | Top-level `model` |
| OpenCode, Kilo CLI, Kilo VS Code | Top-level `model`; otherwise the first configured `models` map key, matching the existing editor and native writer |

Only nonempty string selectors are displayed. Exact spelling is preserved and
duplicate selectors share a row with their configured roles. The OpenCode/Kilo
legacy selection above uses one map key, not the whole inventory. `modelCatalog`
inventories, upstream mappings, static suggestions, and global routes do not add
selections to a client page. These are the selected stored configuration's values,
not a claim about generated mirror catalogs or runtime model resolution.

## Ownership

- `components/app/AppPage.tsx` contains model-only loading, empty, failure and
  rendering state. It imports no provider display component, source record,
  management context, connection form, or global route inventory.
- `lib/api/client_router_plan.ts` reuses the existing `provider_current` and
  `provider_get` reads. It immediately projects the chosen configuration to
  `{ app_type, models }` and rechecks selection identity before returning. No
  source ID/name, endpoint, credential or upstream metadata enters client state.
- `lib/client_router_models.ts` owns the explicit model-field allowlist.
- `lib/client_arena_context.ts` projects that model-only plan into a fixed API
  surface and exact Arena selector options. `components/arena/arena_chart.tsx`
  owns the shared comparison; supplier-specific option mapping stays under
  `components/suppliers/arena_chart.tsx`.
- `lib/client_apps.ts` supplies lightweight labels/order for the shell, status
  selector and hash routes. The existing `APP_PAGE_ORDER` export uses that order;
  `appPages.ts` retains management-form contracts for Suppliers consumers.
- `components/suppliers/client_management_section.tsx` owns the collapsible,
  lazy **Client connections & routing** section. Opening it mounts the selected
  client controller; switching clients remounts that controller.
- `components/suppliers/client_management/` owns all retained provider cards,
  search, templates, forms, state/actions, route editing, gateway controls,
  client configuration and launchers formerly under `components/app/`.

Provider presentation formerly duplicated on client pages is available only
through Suppliers. Existing supplier inventory, editing, import, model discovery
and data remain intact. Existing tools/runtime pages are outside this CLI display
change and retain their own operational behavior.

No IPC command, request shape, persisted record, schema, native route or backend
implementation changed. Provider activation, reorder, queue membership, pinning,
tests, CRUD, route edits, gateway lifecycle, mirror controls and launchers are
retained under Suppliers instead of being hidden in a CLI rendering branch.

## Changed-file map

New model boundary: `lib/client_apps.ts`, `lib/client_router_models.ts`,
`lib/api/client_router_plan.ts`, `components/app/client_plan.css`.

Updated shell: `App.tsx`, `components/workspace_header.tsx`,
`lib/workspace_routes.ts`, `lib/shell_i18n.ts`, `lib/appPages.ts`.

Supplier entry: `components/suppliers/suppliers_page.tsx`,
`supplier_editor.tsx`, new `client_management_section.tsx` and `.css`.

Moved management modules (relative to `components/suppliers/client_management/`):

```text
supplier_client_settings.tsx  (formerly components/app/AppPage.tsx)
GatewayPanel.tsx              Launchers.tsx
RouteEditor.tsx               advanced_routes.tsx
client_config_status.tsx      config_editor_modal.tsx
gateway_flow.ts               gateway_state.ts
icon_picker.tsx               import_flow.ts
preset_picker.tsx             provider_card.tsx
provider_icon.tsx             provider_icon.css
provider_modal.tsx            provider_ui_copy.ts
providers_panel.tsx           providers_panel.css
providers_state.tsx           route_explanation.tsx
routes_state.tsx              tailnet_publication_controls.tsx
README.md
```

The previous `components/app/` copies were removed. The small replacement
`AppPage.tsx` is the client-only view. Management implementation and test IDs stay
with the moved modules; imports were adjusted to their new owner.

## Verification

The focused model tests cover each client shape, Claude tiers and precedence,
exact selector deduplication, foreign fields, catalog exclusion, empty values,
the read-only IPC sequence, and a selection changing during the read.

Browser tests inject selected configurations into the existing synthetic IPC
boundary. They check every client route, exact model output, absence of provider
markup/fields, absence of provider-list/global-route requests, empty/error states,
and stale-result rejection. Legacy management tests navigate through the actual
Suppliers disclosure and client buttons; mutation assertions remain in place.

```text
npx vitest run tests/client_router_plan.test.ts tests/cli_display_boundary.test.ts tests/app_page_actions.test.ts tests/cc_switch_import.test.ts tests/supplier_ui.test.ts tests/provider_reorder.test.ts
npm run typecheck --prefix ui
npm run ui:build
npm run ui:build_demo
node scripts/ui_smoke/cli_display_checks.mjs --built-demo
node scripts/ui_smoke/coding_agent_arena_checks.mjs --built-demo
npm run ui:smoke
node scripts/ui_smoke/provider_interaction_checks.mjs
node scripts/ui_smoke/workspace_checks.mjs --built-demo
git diff --check
```

The browser tests use disposable synthetic state and do not establish live
provider availability or native configuration writes. The running desktop
executable is separate from newly built frontend assets.

### Recorded results — 2026-10-26

| Check | Result |
| --- | --- |
| Focused model, ownership, gateway, import, supplier and ordering tests | 105/105 passed across six test files |
| UI typecheck | Passed |
| Native frontend build | Passed |
| Browser-demo build | Passed |
| Built-demo CLI boundary checks | 14/14 passed, independently run by the parent |
| Complete source browser regression | 183/183 passed: all prior 162 checks retained, plus 13 CLI boundary checks and 8 provider interaction checks |
| Built-demo workspace/status checks | 61/61 passed |
| Bundle budget and `git diff --check` | Passed |
| Existing Node/Rust backend, IPC types and supplier data adapters | No source changes |

The legacy OpenCode/Kilo model-map test was observed failing, then passed after
matching the native writer's selected-key behavior. An initial rehomed browser
run exposed a test holding a client-tab element across navigation to DSH; the
test now returns through the actual Suppliers controls and retains its original
assertions. The final complete browser run had no failures.
