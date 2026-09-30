# `src/gateway/` — Hono HTTP edge, routing, providers, conversion

The local proxy implemented in TypeScript. Hosts the OpenAI Chat Completions
endpoint, the Anthropic Messages endpoint, the model listing, and the health
probe. Resolves a requested model alias through an ordered route chain and
forwards to whichever provider answers first.

## Anchor files

- [`server.ts`](server.ts) — `createGatewayApp(cfg)`. Wires Hono routes,
  applies auth middleware, and serves `/health`, `/v1/models`,
  `/v1/chat/completions`, `/v1/messages`.
- [`router.ts`](router.ts) — `Router` class. Resolves an alias to an ordered
  hop chain, executes them, and uses each provider's circuit breaker.
- [`canonical.ts`](canonical.ts) — the canonical OpenAI request / response
  / stream-chunk shapes plus the `ProviderError` / `makeError` helper.
- [`converter.ts`](converter.ts) — bidirectional OpenAI ↔ Anthropic
  conversion at the HTTP edge.
- [`circuit-breaker.ts`](circuit-breaker.ts) — `closed / open / half-open`
  breaker with configurable failure threshold, cooldown, half-open retries,
  and success threshold.
- [`providers/`](providers/README.md) — provider implementations behind a
  shared `BaseProvider` trait.

## Key contracts

- The internal canonical format is OpenAI Chat Completions. The HTTP edge
  converts Anthropic requests and responses; the Anthropic provider does
  the same in the other direction.
- Auth: when `cfg.gateway.authToken` is non-empty, `/v1/*` requires
  `Authorization: Bearer <token>` or `x-api-key: <token>`.
- Failover advances only for network errors, HTTP 429, and HTTP 5xx.
  Streaming failover is one-shot after the first chunk.
- `Router.resolveRoute` falls back to a single-hop route when a provider
  directly advertises the requested model and no alias matches.

## See also

- [`src/gateway/providers/`](providers/README.md) — provider implementations.
- [`src/config/`](../config/README.md) — config schema and persistence.
- [Root README](../../README.md).