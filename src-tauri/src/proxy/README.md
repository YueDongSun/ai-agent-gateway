# `src-tauri/src/proxy/` — Axum HTTP server, canonical format, routing

The Rust local proxy. Hosts the same HTTP surface as the Node gateway
(`/health`, `/v1/models`, `/v1/chat/completions`, `/v1/messages`,
`/v1/responses`) but uses axum 0.7 and is embedded inside the Tauri
process. Initial state is built at `proxy_start`, and subsequent supplier,
provider, model, route, pin, and catalog changes hot-apply to new requests;
in-flight requests retain their admitted snapshot. Listener host/port and
gateway-auth changes require a restart.

## Anchor files

- [`mod.rs`](mod.rs) — module declarations, the shared `ProxyHandle` /
  `ServerRouter` aliases, and the `ONE_M_CONTEXT_MARKER` (`[1M]`) helper
  that strips Claude Code's local capability suffix before forwarding.
- [`server.rs`](server.rs) — Axum router and request handlers.
  `serve_detached` binds immediately on `tokio::net::TcpListener`, hands
  the accept loop to a task, and returns the actual bound port. Defines
  `ProxyAppState` (router, auth token, pinned provider, database handle,
  `last_used` map) and `BoundInfo`.
- [`router.rs`](router.rs) — `Router` struct. Resolves an alias through
  per-surface failover queues (`claude` / `codex` / `chat`) and executes
  hops with each provider's circuit breaker. Tracks `last_used` so
  `persist_failover_winner` can mirror cc-switch's hot-switch behavior.
- [`converter/`](converter/) — bidirectional OpenAI ↔ Anthropic
  conversion at the HTTP edge and at the Anthropic provider boundary.
  Also converts between Chat Completions and the OpenAI Responses API
  shape used by Codex CLI.
- [`canonical.rs`](canonical.rs) — the canonical OpenAI request /
  response / stream-chunk shapes plus `ProviderError` and `makeError`.
- [`responses.rs`](responses.rs) — OpenAI Responses API wire types
  (`ResponsesRequest`, `ResponsesResponse`, streaming event variants).
- [`circuit_breaker.rs`](circuit_breaker.rs) — thread-safe
  `closed / open / half-open` breaker with `CircuitBreakerSnapshot`.
- [`models.rs`](models.rs) — `OFFICIAL_MODELS` (Claude tier order) used
  to seed failover routes and to advertise model ids on `/v1/models`.
- [`providers/`](providers/README.md) — provider trait + Rust provider
  implementations.
- [`thinking_compat.rs`](thinking_compat.rs) — one bounded repair retry for
  explicit thinking/signature validation errors.

## Key contracts

- Canonical internal format is OpenAI Chat Completions. Anthropic
  conversion happens at the edge and inside the Anthropic provider.
- Anthropic client requests and upstream responses/events carry non-serialized
  source envelopes. Same-protocol routing relays them losslessly while the
  canonical projection remains available for routing and diagnostics.
- The router advances only for network errors, HTTP 429, and HTTP 5xx.
  4xx errors propagate without failover.
- Streaming failover probes the first chunk before committing. A later
  stream error surfaces to the client — there is no transparent mid-stream
  recovery.
- The `[1M]` model-name suffix is stripped before the request reaches
  upstream. Upstream endpoints do not understand the marker and would
  404 or return an empty body.
- `pinned_provider` forces all requests through one provider, bypassing
  failover. Cleared by setting to empty string.

## See also

- [`providers/`](providers/README.md) — provider implementations.
- [`../commands/proxy.rs`](../commands/proxy.rs) — IPC that starts / stops
  this server.
- [Root README](../../README.md).
