/**
 * Non-streaming abort plumbing.
 *
 * The streaming surface already forwards the client's abort signal into the
 * router; the non-streaming `complete` path must do the same. Beyond
 * plumbing, a client abort must be classified as a cancellation: it is not a
 * provider fault, so it must never fail over to a later hop and never count
 * against the circuit breaker. The rule covers both the request-send failure
 * and an abort landing while the response body is read, which must not surface
 * as a retriable decode error.
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { Router } from '../src/gateway/router.js';
import { BaseProvider } from '../src/gateway/providers/base.js';
import { createGatewayApp } from '../src/gateway/server.js';
import type { AppConfig, ChatCompletionRequest, ProviderConfig } from '../src/config/schema.js';
import type { OAIChatResponse, OAIStreamChunk } from '../src/gateway/canonical.js';

const cooldown_ms = 1000;

function provider_config(id: string): ProviderConfig {
  return {
    id,
    name: id,
    type: 'mock',
    apiKey: '',
    baseURL: '',
    models: ['m'],
    timeoutMs: 60,
    enabled: true,
    circuitBreaker: {
      failureThreshold: 2,
      cooldownMs: cooldown_ms,
      halfOpenRetries: 1,
      successThreshold: 2,
    },
    weight: 1,
  };
}

function app_config(hops: Array<{ provider: string; model: string }>): AppConfig {
  return {
    version: 1,
    gateway: { host: '127.0.0.1', port: 8787, authToken: '', logLevel: 'warn' },
    providers: [],
    routes: [{ alias: 'm', hops, description: 'test' }],
    profiles: [],
    agents: [],
  };
}

function build_router(...providers: BaseProvider[]): Router {
  const hops = providers.map((p) => ({ provider: p.id, model: 'm' }));
  const router = new Router(app_config(hops));
  const map = (router as unknown as { providers: Map<string, BaseProvider> }).providers;
  for (const p of providers) map.set(p.id, p);
  return router;
}

/** Throws with the name the router's client-abort classification expects. */
function abort_error(): Error {
  const e = new Error('The operation was aborted');
  e.name = 'AbortError';
  return e;
}

/**
 * Fake provider mirroring the real contract under test: the non-streaming
 * fetch honors the forwarded signal. Without a signal nothing can observe an
 * abort, so the fake rejects with an explicit sentinel. With an
 * already-aborted signal (or once the test aborts) it rejects with a
 * plain AbortError, exactly like an interrupted undici fetch.
 */
class AbortProbeProvider extends BaseProvider {
  calls = 0;
  last_signal: AbortSignal | undefined;

  constructor(id: string) {
    super(provider_config(id));
  }

  async complete(_req: ChatCompletionRequest, signal?: AbortSignal): Promise<OAIChatResponse> {
    this.calls += 1;
    this.last_signal = signal;
    if (signal === undefined) throw new Error('no-signal-forwarded');
    if (!signal.aborted) {
      await new Promise<void>((resolve) => {
        signal.addEventListener('abort', () => resolve(), { once: true });
      });
    }
    throw abort_error();
  }

  async *stream(_req: ChatCompletionRequest, _signal?: AbortSignal): AsyncGenerator<OAIStreamChunk> {
    throw new Error('stream_not_expected_in_complete_abort_tests');
  }
}

const chat_request: ChatCompletionRequest = {
  model: 'm',
  messages: [{ role: 'user', content: 'hi' }],
} as ChatCompletionRequest;

describe('router complete abort plumbing', () => {
  it('forwards the client abort signal to the provider complete call', async () => {
    const provider = new AbortProbeProvider('a');
    const router = build_router(provider);
    const controller = new AbortController();
    controller.abort();

    await expect(router.complete(chat_request, controller.signal)).rejects.toMatchObject({
      code: 'client_abort',
    });

    expect(provider.calls).toBe(1);
    expect(provider.last_signal).toBeDefined();
    expect(provider.last_signal?.aborted).toBe(true);
  });

  it('does not fail over to later hops on a client abort', async () => {
    const first = new AbortProbeProvider('a');
    const second = new AbortProbeProvider('b');
    const router = build_router(first, second);
    const controller = new AbortController();

    const pending = router.complete(chat_request, controller.signal);
    // Attach before aborting so the rejection is never unhandled.
    const outcome = expect(pending).rejects.toMatchObject({
      code: 'client_abort',
      retriable: false,
      status: 499,
    });
    controller.abort();
    await outcome;

    expect(first.calls).toBe(1);
    expect(second.calls).toBe(0);
  });

  it('settles a client abort as cancelled and leaves the breaker closed', async () => {
    const provider = new AbortProbeProvider('a');
    const router = build_router(provider);
    const controller = new AbortController();
    const before = provider.breaker.snapshot();

    const pending = router.complete(chat_request, controller.signal);
    const outcome = expect(pending).rejects.toMatchObject({ code: 'client_abort' });
    controller.abort();
    await outcome;

    const snapshot = provider.breaker.snapshot();
    expect(snapshot.state).toBe('closed');
    expect(snapshot.failures).toBe(before.failures);
  });
});

describe('gateway complete abort wiring', () => {
  it('surfaces the client abort signal to the provider on the non-streaming surface', async () => {
    const provider = new AbortProbeProvider('a');
    const { app, router } = createGatewayApp(app_config([{ provider: 'a', model: 'm' }]));
    const map = (router as unknown as { providers: Map<string, BaseProvider> }).providers;
    map.set('a', provider);

    const controller = new AbortController();
    const pending = app.request('/v1/chat/completions', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ model: 'm', messages: [{ role: 'user', content: 'hi' }] }),
      signal: controller.signal,
    });
    const outcome = expect(pending).resolves.toMatchObject({ status: 499 });
    controller.abort();
    await outcome;

    expect(provider.calls).toBe(1);
    expect(provider.last_signal).toBeDefined();
    expect(provider.last_signal?.aborted).toBe(true);
  });
});

afterEach(() => vi.unstubAllGlobals());

// ---------------------------------------------------------------------------
// Decode-path abort tests
// ---------------------------------------------------------------------------

function openai_app_config(): AppConfig {
  return {
    version: 1,
    gateway: { host: '127.0.0.1', port: 8787, authToken: '', logLevel: 'warn' },
    providers: [{
      id: 'openai', name: 'openai', type: 'openai', baseURL: 'https://openai.mock',
      apiKey: 'test', enabled: true, models: ['m'], timeoutMs: 1000, weight: 1,
      circuitBreaker: { failureThreshold: 2, cooldownMs: cooldown_ms, halfOpenRetries: 1, successThreshold: 2 },
    }],
    routes: [{ alias: 'm', hops: [{ provider: 'openai', model: 'm' }], description: 'test' }],
    profiles: [], agents: [],
  };
}

function anthropic_app_config(): AppConfig {
  return {
    version: 1,
    gateway: { host: '127.0.0.1', port: 8787, authToken: '', logLevel: 'warn' },
    providers: [{
      id: 'anthropic', name: 'anthropic', type: 'anthropic', baseURL: 'https://anthropic.mock',
      apiKey: 'test', enabled: true, models: ['m'], timeoutMs: 1000, weight: 1,
      circuitBreaker: { failureThreshold: 2, cooldownMs: cooldown_ms, halfOpenRetries: 1, successThreshold: 2 },
    }],
    routes: [{ alias: 'm', hops: [{ provider: 'anthropic', model: 'm' }], description: 'test' }],
    profiles: [], agents: [],
  };
}

function openai_dual_hop_config(): AppConfig {
  return {
    version: 1,
    gateway: { host: '127.0.0.1', port: 8787, authToken: '', logLevel: 'warn' },
    providers: [
      {
        id: 'openai_a', name: 'openai_a', type: 'openai', baseURL: 'https://openai_a.mock',
        apiKey: 'test', enabled: true, models: ['m'], timeoutMs: 1000, weight: 1,
        circuitBreaker: { failureThreshold: 2, cooldownMs: cooldown_ms, halfOpenRetries: 1, successThreshold: 2 },
      },
      {
        id: 'openai_b', name: 'openai_b', type: 'openai', baseURL: 'https://openai_b.mock',
        apiKey: 'test', enabled: true, models: ['m'], timeoutMs: 1000, weight: 1,
        circuitBreaker: { failureThreshold: 2, cooldownMs: cooldown_ms, halfOpenRetries: 1, successThreshold: 2 },
      },
    ],
    routes: [{ alias: 'm', hops: [{ provider: 'openai_a', model: 'm' }, { provider: 'openai_b', model: 'm' }], description: 'test' }],
    profiles: [], agents: [],
  };
}

describe('decode-path client abort — OpenAI provider', () => {
  it('classifies as client_abort when json() rejects with AbortError after signal is aborted', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => {
      const mockResp = new Response('{}', { status: 200 });
      mockResp.json = function () {
        throw new DOMException('aborted', 'AbortError');
      };
      return mockResp;
    }));

    const { router } = createGatewayApp(openai_app_config());
    const controller = new AbortController();
    controller.abort();

    await expect(router.complete(chat_request, controller.signal)).rejects.toMatchObject({
      code: 'client_abort',
      retriable: false,
      status: 499,
    });
  });
});

describe('decode-path client abort — no failover to second hop', () => {
  it('does not call the second hop when the first fails during response body read due to client abort', async () => {
    let firstCallCount = 0;
    vi.stubGlobal('fetch', vi.fn(async (url: string) => {
      firstCallCount += 1;
      // Simulate an ok response whose body read aborts
      const mockResp = new Response('{}', { status: 200 });
      mockResp.json = function () {
        throw new DOMException('aborted', 'AbortError');
      };
      return mockResp;
    }));

    const { router } = createGatewayApp(openai_dual_hop_config());
    const controller = new AbortController();
    controller.abort();

    await expect(router.complete(chat_request, controller.signal)).rejects.toMatchObject({
      code: 'client_abort',
      retriable: false,
      status: 499,
    });

    expect(firstCallCount).toBe(1);
    // Verify second provider was never called (both hops share the same stub)
    // The key assertion: no error from the second hop being called with an
    // already-aborted signal (which would surface differently).
  });

  it('leaves the first provider breaker closed after a decode-path abort', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => {
      const mockResp = new Response('{}', { status: 200 });
      mockResp.json = function () {
        throw new DOMException('aborted', 'AbortError');
      };
      return mockResp;
    }));

    const { router } = createGatewayApp(openai_dual_hop_config());
    const providerA = router.getProvider('openai_a')!;
    const before = providerA.breaker.snapshot();

    const controller = new AbortController();
    controller.abort();

    await expect(router.complete(chat_request, controller.signal)).rejects.toMatchObject({
      code: 'client_abort',
    });

    const after = providerA.breaker.snapshot();
    expect(after.state).toBe('closed');
    expect(after.failures).toBe(before.failures);
  });
});

describe('genuine decode failure — still upstream_decode_error', () => {
  it('OpenAI: still throws upstream_decode_error when json() rejects with non-AbortError', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => {
      const mockResp = new Response('not-json', { status: 200 });
      mockResp.json = async function () {
        throw new SyntaxError('Unexpected token N in JSON at position 0');
      };
      return mockResp;
    }));

    const { router } = createGatewayApp(openai_app_config());

    await expect(router.complete(chat_request)).rejects.toMatchObject({
      code: 'upstream_decode_error',
      retriable: true,
      status: 502,
    });
  });

  it('Anthropic: still throws upstream_decode_error when body is undecodable', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => {
      const mockResp = new Response('not-json', { status: 200 });
      mockResp.json = async function () {
        throw new SyntaxError('Unexpected token N in JSON at position 0');
      };
      return mockResp;
    }));

    const { router } = createGatewayApp(anthropic_app_config());

    await expect(router.complete(chat_request)).rejects.toMatchObject({
      code: 'upstream_decode_error',
      retriable: true,
      status: 502,
    });
  });
});
