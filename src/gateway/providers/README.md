# `src/gateway/providers/` — Provider implementations

Every upstream API provider implements the same `BaseProvider` interface so
the router can dispatch without caring about the wire format. Conversion to
the canonical OpenAI shape happens inside each provider or at the HTTP edge.

## Anchor files

- [`base.ts`](base.ts) — abstract `BaseProvider` with a built-in
  `CircuitBreaker`. Subclasses implement `complete` (non-streaming) and
  `stream` (async generator of canonical OAI chunks).
- [`index.ts`](index.ts) — `createProvider(cfg)` factory. Maps
  `cfg.type` to `MockProvider`, `OpenAIProvider`, or `AnthropicProvider`.
- [`openai.ts`](openai.ts) — `OpenAIProvider`. Speaks
  `POST /v1/chat/completions` against any base URL. Used for OpenAI,
  DeepSeek, Kimi / Moonshot, Together, Ollama, vLLM, and similar services.
- [`anthropic.ts`](anthropic.ts) — `AnthropicProvider`. Speaks
  `POST /v1/messages`. Performs Anthropic-native conversion at the provider
  boundary so the rest of the router stays canonical.
- [`mock.ts`](mock.ts) — `MockProvider`. Used by tests and by the seed
  config. Special model names: `mock-fail` always returns 503 (retriable),
  `mock-slow` waits 2 s then succeeds, anything else echoes the last user
  message.

## Key contracts

- All providers return canonical `OAIChatResponse` / `OAIStreamChunk` values.
  No provider leaks its native wire format up to the router.
- Providers must classify upstream 4xx as non-retriable and 429 / 5xx as
  retriable so the router can fail over cleanly.
- Timeout is enforced via `AbortSignal.timeout(config.timeoutMs)`.

## See also

- [`src/gateway/router.ts`](../README.md) — consumes providers.
- [`src/gateway/canonical.ts`](../README.md) — wire types.
- [Root README](../../README.md).