# Workspace interaction and visual QA

The subsequent [client/provider boundary change](cli_provider_boundary.md) moves
provider controls into Suppliers and makes client pages model-only. The evidence
below records the earlier interaction-layer change; current checks use the
relocated Suppliers control surface.

## Scope and reference

The React frontend retains AAG's warm paper/charcoal palette, terracotta actions,
12/8px radii, editorial headings, type scale, and page spacing. Shared design-token
declarations are unchanged. No dependency, backend command, configuration writer,
or provider protocol was added.

The interaction baseline is **CC Switch v3.12.3**:
[provider switching](https://github.com/farion1231/cc-switch/blob/v3.12.3/docs/user-manual/en/2-providers/2.2-switch.md)
and [feature overview](https://github.com/farion1231/cc-switch/blob/v3.12.3/README.md).
The frontend adopts app-scoped provider lists, explicit activation, distinct
current/serving states, ordered cards, and searchable preset selection. AAG keeps
its supported agents and backend activation semantics. This is an interaction
adaptation, not an implementation of every CC Switch native feature.

## Component ownership

| Responsibility | Owner |
| --- | --- |
| Shell, keyed lazy pages, hook lifetimes | `ui/src/App.tsx` |
| Route validation and history/focus/scroll | `ui/src/lib/workspace_routes.ts`, `ui/src/hooks/use_workspace_navigation.ts` |
| Status controls and destination selector | `ui/src/components/workspace_header.tsx` and `.css` |
| Event-driven background sampling | `ui/src/hooks/use_status_surface.ts` |
| Bounded blending and contrast math | `ui/src/lib/status_surface.ts` |
| Palette before first paint, saved/system theme | `ui/index.html`, `ui/src/lib/theme.ts` |
| Provider search, cards and action disclosure | `ui/src/components/suppliers/client_management/providers_panel.tsx`, `provider_card.tsx`, `providers_panel.css` |
| Provider/preset dialog focus and scroll lifetime | `ui/src/components/use_accessible_dialog.ts` |

Destinations use validated `#/<view>` routes. New workspaces still open Suppliers.
Navigation, Back, Forward and reload resolve the selected page. Every destination
opens at its heading and focuses main content. A keyed page removes the previous
app's editor, pending display state, search and revealed values. Existing per-app
advanced-route expansion remains intact. Navigation never activates a provider.

The header remains in document flow and becomes sticky, including on mobile.
Its native selector keeps destinations available after horizontal navigation
scrolls away. Below 680px, context and controls occupy two rows; long status labels
wrap without hiding controls. Provider Enable/Reapply and Edit stay visible;
More reveals labeled secondary actions. Search preserves source indices and
disables reorder while filtered. Dialogs contain focus and restore the trigger.

Three points under the header sample computed page backgrounds, composited
through transparent ancestors. Floating dialogs are excluded. The page base
blends toward that sample by at most 8%, over the first 96px of scroll. A 92–96%
scrim bounds arbitrary content behind it; 12px backdrop blur and a shallow feather
soften the join. Unsupported blur and reduced transparency use the opaque base.
Images/gradients show through the scrim; their element's background color supplies
the echo, without image pixel readback. Text uses the existing foreground;
status dots use existing success/warning tokens.

Theme changes apply text and surfaces atomically before paint to avoid dark text
crossing an old dark surface. Same-theme surface changes ease over 180ms; page
entry uses 160ms and 3px. Reduced motion removes both. Sampling is coalesced to
one animation-frame callback per event burst. There is no idle animation loop,
full-page blur, canvas sampling, or new animation library. Observers, listeners
and pending frames are cleaned up.

## Run and verify

Run `npm run ui:demo` for the synthetic browser preview. The native entrypoint
remains `npm run desktop:dev`; that uses the real backend.

```text
npm run typecheck --prefix ui
npx vitest run tests/status_surface.test.ts tests/supplier_ui.test.ts
npm run ui:build
npm run ui:build_demo
node scripts/ui_smoke/workspace_checks.mjs --built-demo
node scripts/ui_smoke/provider_interaction_checks.mjs
npm run ui:smoke
npm run ui:bundle_report
git diff --check
```

Add `--screenshots <absolute-output-directory>` to the workspace check for
desktop/mobile light/dark screenshots at the top, while scrolled, and at provider
cards. Each suite owns and closes its server/browser profile. Run suites sharing
the same port sequentially. The browser suites use synthetic IPC only.

The supplier-discovery scenario in `ui:smoke` and `ui:smoke_demo` checks the real
EN/CN switch with health and persisted-result fixtures, including return to
English without another model refresh. It also verifies editor fetch-error
context and authentication explanations separately in each locale, closing the
dialog before operating the header switch. See the current
[supplier language contract](supplier_workspace.md#english-and-chinese-discovery-explanations).

## Evidence recorded 2026-10-26

Environment: Windows, Node 24.18.0, Vite 5.4.21, Edge 156.0.4314.42.

| Check | Result |
| --- | --- |
| UI typecheck | Passed |
| Surface unit tests | 3 passed; text ≥4.5:1, icons ≥3:1 over black/white extremes |
| Existing supplier UI/domain tests | 39 passed |
| Native frontend build | Passed; assets only, no native runtime claim |
| Browser-demo build | Passed |
| Dedicated provider browser interactions | 9/9 passed |
| Existing source-served browser smoke | 162/162 passed |
| Final built-demo workspace checks | 60/60 passed |
| Bundle report | Passed existing size budgets |
| Idle/scroll observation | Zero idle surface writes; zero long tasks in a 45-frame scroll probe |

The full source smoke passed before the final narrow header first-mount/opacity
fixes; the final built-demo workspace suite and both frontend builds verify those
fixes. An earlier smoke run rejected the obsolete unchanged-URL assertion. Its
replacement checks both the preserved marketplace opener route and the explicitly
restored Claude route. Expanded header checks caught the old stopped-dot opacity
rule and initial sampling/ref timing; both were corrected before the final pass.

## Visual and interaction checklist

- [x] Inspect desktop/mobile screenshots in both themes: existing palette,
  heading hierarchy, card shapes, whitespace and action styling remain coherent.
- [x] Check widths 320, 360, 390, 430, 521, 600, 680, 768, 900, 1024, 1280 and
  1440px, at top and scrolled positions, with all four gateway-status labels.
- [x] Verify rendered header text ≥4.5:1 and status icons ≥3:1 over black/white
  possible backdrops, with fully visible dots and contained controls.
- [x] Verify sampling on first mount, actual underlying-card color echo, sticky
  placement, a feathered body join, and no idle sampler writes.
- [x] Verify deep-link reload, Back/Forward, synchronized destination selector,
  focus/scroll reset and removal of a previous app's open provider editor.
- [x] Search name/endpoint, clear by button/Escape, recover from no results,
  preserve order, and prevent filtered reordering.
- [x] Keep primary actions visible; reach secondary actions through More with
  keyboard/touch; restore focus on Escape; verify mobile 44px action targets.
- [x] Verify named provider/preset dialogs, initial focus, Tab containment,
  Escape/focus return, and preserved pending-save/error contracts.
- [x] Retain existing regression coverage for switch writes, queue/pin actions,
  stale responses, secret remasking, gateway feedback and route edits.
- [x] Enable reduced motion and verify page/header animation is disabled.
- [ ] Repeat on physical iOS Safari, Android Chrome, desktop Firefox and Safari.
  This run used Edge viewport emulation, not those browser engines/devices.
- [ ] Repeat in packaged Tauri/WebView2 with real native switching, system
  scaling, screen readers, reduced transparency and forced colors.
- [ ] Profile sustained interaction on low-power devices. The bounded scroll
  observation is not a universal zero-jank or frame-rate guarantee.

The delivered frontend and browser evidence support the checked items. Identical
rendering on every device and real native activation require the remaining
platform acceptance checks.
