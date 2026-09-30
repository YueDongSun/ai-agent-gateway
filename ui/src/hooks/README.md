# `ui/src/hooks/` — Custom React hooks

Custom hooks for the desktop UI. The only hook here today is the gateway
status hook that powers the header status indicator and the
`AppPage` reconciliation callback.

## Anchor files

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
- Hooks never embed business logic. Business logic stays in the page or
  context module that owns the state.

## See also

- [`../components/app/AppPage.tsx`](../components/app/README.md) —
  primary consumer.
- [`../lib/api/proxy.ts`](../lib/api/README.md) — typed IPC wrappers
  this hook calls.
- [Root README](../../../README.md).