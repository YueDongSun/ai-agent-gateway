# Frontend product polish

## Scope and status

The implementation and automated checks are verified on 2026-08-28. Computer
Use observed the actual npm demo in light and dark themes. The user stopped
the later zoom review with Escape, so full visual acceptance remains open.
The preview release includes these changes; native installation and live
provider configuration switching remain separate acceptance work.

The original Computer Use inspection on 2026-08-27 observed three defects in
the actual browser demo: the header said running while the gateway card said
stopped, route table headings did not align with the inputs, and the gateway
key appeared as plaintext in the overview. A later browser-URL safety stop was
honored; it does not count as acceptance of the changed UI.

## Implemented changes

- `ui/src/hooks/use_gateway_status.ts` owns header status reconciliation. It
  checks runtime status before health, distinguishes stopped, running, degraded,
  and unknown states, bounds health requests, and rejects stale results.
- `AppPage.tsx` reports successful refreshes through an explicit callback. The
  existing state/action split and routing behavior remain intact.
- `ui/src/components/secret_value.tsx` masks keys by default, supports explicit
  reveal/hide, and resets on value or application changes. Copy receives the
  original value. The gateway settings key input uses password rendering.
- `RouteEditor.tsx` and `styles.css` restore table-row semantics, share column
  tracks between headings and cells, and contain narrow table overflow within
  a scroll region. Header layout follows the content width and wraps on smaller
  viewports; light and dark theme tokens remain in use.
- English and Chinese labels cover status and key visibility controls.

## Fresh evidence

- `npm run typecheck`: passed.
- `npm test`: 28/28 passed with an isolated temporary configuration directory.
- Desktop startup/build configuration tests: 7/7 passed.
- `npm run ui:build`: TypeScript and Vite production build passed.
- `cargo check --manifest-path src-tauri/Cargo.toml --locked --offline`: passed.
- Rust serial unit suite: 125/125 passed, including actual HTTP startup readiness.
- Browser smoke: 53/53 passed independently against source-served and built-demo
  output with an owned headless browser and synthetic backend. No user-browser
  clipboard value was overwritten; the copy check intercepts that call.

The added product checks cover initial stopped state, start/stop feedback,
degraded and unknown states with recovery, cancellation of delayed health,
masked/revealed/copied keys, remasking on application change, aligned route
columns, stable light/dark theme control contrast, and layout containment at
1200px, 900px, and 420px viewport widths. Both modes show the shared application
version, an explicit demo badge, and the generated application icon.

Actual Computer Use observations confirmed consistent stopped labels, masked
credentials, aligned route columns, and readable stable light/dark themes in
the npm demo. An immediate theme-transition frame was not treated as a defect;
the regression waits for transitions before inspecting computed colors.

## Remaining acceptance

- Resume the interrupted narrow/zoomed Computer Use review only when the user
  is ready for desktop interaction; also check visible keyboard focus states.
- Keep native WebView2, real upstream availability, and live CLI switching
  separate from browser-demo evidence. They were not tested in this slice.
- Do not claim a rendering-performance improvement from the layout repair or
  from a successful build. The earlier measured-performance follow-up remains
  open. The separate Rust HTTP startup-readiness assertion is now restored.
