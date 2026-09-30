# `src-tauri/src/proxy/providers/` — Rust provider implementations

The Rust equivalent of `src/gateway/providers/`. Each upstream API provider
implements the `Provider` trait so the router can dispatch without caring
about the wire format. All providers return canonical `OAIChatResponse` /
`OAIStreamChunk` values.

## Anchor files

- [`mod.rs`](mod.rs) — `Provider` trait (`id`, `enabled`, `models`,
  `breaker`, `complete`, `stream`) plus the `build_provider` factory that
  maps a `ProxyProvider` row to a boxed `Provider`.
- [`anthropic.rs`](anthropic.rs) — `AnthropicProvider`. Speaks
  `POST /v1/messages` against a configurable base URL. Performs canonical
  OpenAI ↔ Anthropic conversion via `converter::oai_req_to_anthropic_req`
  and `anthropic_resp_to_oai_resp`.
- [`openai.rs`](openai.rs) — `OpenAIProvider`. Speaks
  `POST /v1/chat/completions`. Used for OpenAI, DeepSeek, Kimi / Moonshot,
  Together, Ollama, vLLM, and similar OpenAI-compatible services.
- [`mock.rs`](mock.rs) — `MockProvider`. Special model names: `mock-fail`
  always returns 503 (retriable) for breaker + failover demos;
  `mock-slow` waits 2 s then succeeds; anything else echoes the last user
  message.
- [`sse.rs`](sse.rs) — shared SSE byte-stream parser used by both OpenAI
  and Anthropic streaming providers. Yields raw `data:` payloads (or
  `[DONE]`) so each provider can apply its own format conversion.
- [`http_client.rs`](http_client.rs) — reusable connection-pooled client
  construction and content-free transport error classification.

## Key contracts

- `Provider::stream` returns `Ok(stream)` only after the upstream HTTP
  status is known. A 4xx/5xx surfaces as `Err(ProviderError)` BEFORE any
  streaming begins so the router can fail over cleanly instead of
  swallowing the error into an empty stream.
- Once streaming begins, mid-stream errors surface to the client. There is
  no transparent failover after commitment.
- Each provider owns its own `CircuitBreaker`. The router checks the
  breaker before attempting the request.
- Each provider also owns one HTTP client for the proxy snapshot lifetime;
  stream setup bounds response-header time without imposing a total stream
  timeout. OpenAI-compatible streams request usage when the caller did not
  provide `stream_options`.

## See also

- [`../router.rs`](../README.md) — calls these providers.
- [`../converter.rs`](../README.md) — format conversion.
- [`../canonical.rs`](../README.md) — wire types.
- [Root README](../../README.md).
