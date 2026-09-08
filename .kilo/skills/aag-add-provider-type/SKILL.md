---
name: aag-add-provider-type
description: Add a new upstream API provider type (e.g. a new OpenAI-compatible or Anthropic-compatible backend) to either the Node src/gateway or the Rust src-tauri proxy. Covers the enum extension, the Provider implementation, the factory registration, and the test to add. Use when a task asks to support a new model backend / API flavor.
---

# Skill: aag-add-provider-type

Add a new **upstream API provider type** (the thing that actually calls an LLM
backend). This is distinct from adding an AI coding CLI adapter - see
`aag-add-agent-adapter` for that. Provider types live in the gateway/proxy layer
and speak the canonical OpenAI format internally.

## Decide which tree

- **Node** (`src/`) - the working proxy today. Default target unless the task
  explicitly targets the Tauri/Rust desktop app.
- **Rust** (`src-tauri/`) - only if the task is about the desktop proxy. Note
  the Rust proxy is Phase 2: only `mock` + non-streaming `/v1/chat/completions`
  exist; `openai`/`anthropic` return "not yet implemented". Adding a real
  provider type to Rust is Phase 2 Step 6 work.

## Node: add a provider type (`src/`)

### 1. Extend the enum
`src/config/schema.ts` - `ProviderType` is `z.enum(['openai','anthropic','gemini','mock'])`.
Add your new type string. (Note: `gemini` currently has NO class - it falls
through to `OpenAIProvider` in the factory. If you add a real Gemini class,
update the factory arm too.)

Also add Zod-inferred type usage stays automatic (`z.infer`).

### 2. Implement the provider
Create `src/gateway/providers/<name>.ts`:

```ts
import type { ChatCompletionRequest } from '../../config/schema.js';
import type { OAIChatResponse, OAIStreamChunk } from '../canonical.js';
import { BaseProvider, makeError } from './base.js';

export class MyProvider extends BaseProvider {
  async complete(req: ChatCompletionRequest): Promise<OAIChatResponse> {
    // call your backend, return canonical OAIChatResponse.
    // On HTTP failure: throw makeError(`${this.id} HTTP ${status}: ${body}`,
    //   { status, retriable: status >= 500 || status === 429 });
    // 4xx -> retriable defaults to false (non-retriable, propagates immediately).
  }
  async *stream(req: ChatCompletionRequest): AsyncGenerator<OAIStreamChunk> {
    // yield canonical OAIStreamChunk objects from the backend's SSE.
    // If the backend speaks Anthropic SSE, reuse converter helpers.
  }
}
```

Key rules:
- Speak **canonical OpenAI** in/out - never leak the backend's native shape to
  the router. If the backend is Anthropic-native, convert with `converter.ts`
  helpers INSIDE this provider (mirror `anthropic.ts`).
- Use `this.timeoutSignal()` (`AbortSignal.timeout(config.timeoutMs)`) on every
  `fetch`.
- Use `BaseProvider.extractText(req)` for the last user message text.
- Errors: `makeError(message, { status, retriable })`. Retriable drives failover.

### 3. Register in the factory
`src/gateway/providers/index.ts` - add a `case 'yourtype': return new MyProvider(cfg);`
arm. The `default` arm currently maps `openai`/`gemini`/unknown to
`OpenAIProvider` - be deliberate about where your arm goes.

### 4. Seed/test config (optional)
If you want it seeded on first run, add a provider entry in `seedConfig()`
(`src/config/store.ts`). Otherwise users add it via the TUI/`config.json`.

### 5. Test
Add a case in `tests/e2e.test.ts` using the mock substrate. If your provider
needs real keys, gate the test behind an env check and keep the mock-based
failover/breaker tests as the always-green baseline.

### 6. Verify
`npm run typecheck` + `npm test` (use `aag-verify`).

## Rust: add a provider type (`src-tauri/`)

### 1. Create the struct + impl
`src-tauri/src/proxy/providers/<name>.rs`:

```rust
use crate::proxy::canonical::{self, ChatCompletionRequest, OAIChatResponse, ProviderError};
use crate::proxy::circuit_breaker::CircuitBreaker;
use crate::database::dao::proxy_providers::CircuitBreakerConfig;
use async_trait::async_trait;

pub struct MyProvider {
    id: String,
    api_key: String,
    base_url: String,
    models: Vec<String>,
    enabled: bool,
    breaker: CircuitBreaker,
}

impl MyProvider {
    pub fn new(/* fields from ProxyProvider row */) -> Self { /* ... */ }
}

#[async_trait]
impl crate::proxy::providers::Provider for MyProvider {
    fn id(&self) -> &str { &self.id }
    fn enabled(&self) -> bool { self.enabled }
    fn models(&self) -> &[String] { &self.models }
    fn breaker(&self) -> &CircuitBreaker { &self.breaker }

    async fn complete(&self, req: &ChatCompletionRequest) -> Result<OAIChatResponse, ProviderError> {
        // call backend via reqwest, return canonical OAIChatResponse.
        // Errors: canonical::make_error(msg, Some(status), Some(retriable))
        // retriable = status >= 500 || status == 429; 4xx -> false.
    }
}
```

`reqwest` (rustls-tls, json, stream features) is already a dependency. Each
provider owns its `CircuitBreaker` (constructed from the row's
`CircuitBreakerConfig`).

### 2. Register in the factory
`src-tauri/src/proxy/providers/mod.rs` - `build_provider()` match arm:
```rust
"mytype" => Ok(Box::new(MyProvider::new(/* from pp */))),
```
The `ProxyProvider.type` DB column already accepts any string - **no schema
change needed**.

### 3. Test
Inline `#[cfg(test)] mod tests` exercising a forced-error path
(`make_error(..., Some(503), Some(true))`) to confirm retriable classification.

### 4. Verify
`cargo check` + `cargo test` (use `aag-verify`).

## Shared gotchas

- **`gemini` is a phantom type.** In Node it falls through to `OpenAIProvider`
  (`providers/index.ts:14`). If adding a real Gemini class, update the factory
  arm so it no longer falls through.
- **Anthropic-native providers need the converter.** Mirror `anthropic.ts` /
  the future Rust `convert/` module: convert canonical OAI -> Anthropic request
  on the way out, Anthropic response -> canonical OAI on the way back.
- **Never log API keys.** They live in `config.json` (Node) / SQLite (Rust).
- **Retriability is the failover contract.** Getting 4xx classified as retriable
  would cause wrong failover; getting 5xx as non-retriable would skip fallback.
  Match the Node convention exactly.
- **Streaming failover is first-chunk only.** A new provider's `stream` must
  throw BEFORE yielding the first chunk if it wants the router to fail over.
