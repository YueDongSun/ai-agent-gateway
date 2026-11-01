# `ui/src/hooks/` — Custom React hooks

Custom hooks for the desktop UI. They cover the gateway status indicator used
by the header, workspace-route navigation, adaptive header surface sampling, and
model auto-discovery reporting.

## Anchor files

- [`use_workspace_navigation.ts`](use_workspace_navigation.ts) — owns the
  selected workspace view, mirrors it to a `#/<view>` hash route, restores it on
  reload and Back/Forward, and scrolls/focuses the main region on every change.
- [`use_status_surface.ts`](use_status_surface.ts) — samples page colors under
  the sticky header, blends them into `--status_rgb` / `--status_opacity`, and
  publishes the measured header height for scroll padding. Observers are
  disconnected on unmount.
- [`use_discovery_report.ts`](use_discovery_report.ts) — reads the stored model
  auto-discovery report, polls every 60 s so background refreshes surface, and
  exposes `refresh_discovery_status` for a manual re-read.
- [`use_gateway_status.ts`](use_gateway_status.ts) — polls `proxy_status`
  and `/health` at a bounded interval. Returns
  `{ gateway_status: 'stopped' | 'running' | 'degraded' | 'unknown',
  refresh_gateway_status }`. Distinguishes stopped / running / degraded
  / unknown, aborts stale health requests via `AbortController`, and
  reports through `on_gateway_status_refresh` so a page refresh can
  immediately trigger a fresh status read.

## Key contracts

- Health checks use a 2.5 s `AbortController` timeout and a 5 s polling
  interval.
- Stale responses are dropped via a request-id counter; only the most
  recent call wins.
- Route restoration is a user-visible behavior: Back/Forward and reload must
  select the same destination, and an unknown or empty hash falls back to
  `suppliers`.
- Hooks never embed business logic. Business logic stays in the page or
  context module that owns the state.

## See also

- [`../components/suppliers/client_management/supplier_client_settings.tsx`](../components/suppliers/client_management/README.md) —
  consumer of gateway-management and discovery state.
- [`../components/workspace_header.tsx`](../components/README.md) — consumer of
  the gateway-status and surface-sampling hooks.
- [`../lib/api/proxy.ts`](../lib/api/README.md) — typed IPC wrappers
  these hooks call.
- [Root README](../../../README.md).
