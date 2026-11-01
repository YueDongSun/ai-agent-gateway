/**
 * Node gateway count_tokens + discovery surfaces (Rust proxy parity:
 * `messages_count_tokens` and `discovery_identity` in
 * src-tauri/src/proxy/server.rs, `estimate_input_tokens` in
 * src-tauri/src/proxy/providers/mod.rs, `anthropic_count_tokens_url` in
 * src-tauri/src/proxy/endpoint.rs).
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { createGatewayApp } from '../src/gateway/server.js';
import { estimate_input_tokens } from '../src/gateway/count_tokens.js';
import { anthropicCountTokensUrl } from '../src/gateway/providers/endpoint.js';
import type { AppConfig } from '../src/config/schema.js';
import { Router } from '../src/gateway/router.js';

function config(type: 'mock' | 'anthropic'): AppConfig {
  return {
    version: 1,
    gateway: { host: '127.0.0.1', port: 8787, authToken: '', logLevel: 'error' },
    providers: [
      {
        id: 'p',
        name: 'p',
        type,
        baseURL: 'https://fixture.invalid',
        apiKey: '',
        enabled: true,
        models: ['upstream'],
        timeoutMs: 1000,
        weight: 1,
        circuitBreaker: { failureThreshold: 3, cooldownMs: 1000, halfOpenRetries: 1, successThreshold: 1 },
      },
    ],
    routes: [{ alias: 'alias', hops: [{ provider: 'p', model: 'upstream' }], description: '' }],
    profiles: [],
    agents: [],
  };
}

afterEach(() => {
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

function recovery_router(type: 'mock' | 'anthropic' = 'anthropic'): Router {
  const cfg = config(type);
  cfg.providers[0].circuitBreaker.failureThreshold = 1;
  cfg.providers.push({ ...cfg.providers[0], id: 'fallback', name: 'fallback' });
  cfg.routes[0].hops.push({ provider: 'fallback', model: 'upstream' });
  return new Router(cfg);
}

function pending_until_abort(signal?: AbortSignal | null): Promise<never> {
  return new Promise((_, reject) => {
    if (signal?.aborted) reject(signal.reason);
    else signal?.addEventListener('abort', () => reject(signal.reason), { once: true });
  });
}

describe('token-count attempt ownership', () => {
  it.each([401, 503])('reclaims an unread HTTP %s body before returning or retrying', async (status) => {
    vi.useFakeTimers();
    const router = recovery_router();
    let cancelled = 0;
    let aborted = 0;
    let cleanup_at_retry: { cancelled: number; aborted: number } | undefined;
    const fetch_mock = vi.fn(async (_url: string, init: RequestInit) => {
      if (fetch_mock.mock.calls.length > 1) {
        cleanup_at_retry = { cancelled, aborted };
        return Response.json({ input_tokens: 17 });
      }
      init.signal!.addEventListener('abort', () => { aborted += 1; }, { once: true });
      return new Response(new ReadableStream({
        start(controller) { controller.enqueue(new TextEncoder().encode('{"error":')); },
        cancel() { cancelled += 1; },
      }), { status });
    });
    vi.stubGlobal('fetch', fetch_mock);

    if (status === 401) {
      await expect(router.count_tokens('alias', {})).rejects.toMatchObject({ status, retriable: false });
      expect(fetch_mock).toHaveBeenCalledTimes(1);
    } else {
      await expect(router.count_tokens('alias', {})).resolves.toBe(17);
      expect(fetch_mock).toHaveBeenCalledTimes(2);
      expect(cleanup_at_retry).toEqual({ cancelled: 1, aborted: 1 });
    }
    expect(cancelled).toBe(1);
    expect(aborted).toBe(1);
    expect(vi.getTimerCount()).toBe(0);
  });

  it.each([400, 401, 404])('releases a half-open permit after terminal HTTP %s', async (status) => {
    vi.useFakeTimers();
    const router = recovery_router();
    const breaker = router.getProvider('p')!.breaker;
    breaker.recordFailure();
    await vi.advanceTimersByTimeAsync(1000);
    const fetch_mock = vi.fn()
      .mockResolvedValueOnce(new Response('{}', { status }))
      .mockResolvedValueOnce(Response.json({ input_tokens: 42 }));
    vi.stubGlobal('fetch', fetch_mock);

    await expect(router.count_tokens('alias', {})).rejects.toMatchObject({ status, retriable: false });
    expect(breaker.snapshot()).toMatchObject({ state: 'half-open', halfOpenInFlight: 0, failures: 1 });
    await expect(router.count_tokens('alias', {})).resolves.toBe(42);
    expect(breaker.snapshot()).toMatchObject({ state: 'closed', halfOpenInFlight: 0, failures: 0 });
    expect(fetch_mock).toHaveBeenCalledTimes(2);
  });

  it.each(['anthropic', 'mock'] as const)('rejects an already cancelled %s request before dispatch or local estimation', async (type) => {
    const router = recovery_router(type);
    const fetch_mock = vi.fn();
    vi.stubGlobal('fetch', fetch_mock);
    const caller = new AbortController();
    caller.abort(new Error('caller stopped'));
    await expect(router.count_tokens('alias', {}, caller.signal)).rejects.toMatchObject({ code: 'client_abort', status: 499 });
    expect(fetch_mock).not.toHaveBeenCalled();
    expect(router.getProvider('p')!.breaker.snapshot()).toMatchObject({ failures: 0, halfOpenInFlight: 0 });
  });

  it.each(['headers', 'body'])('caller cancellation during %s stops dispatch and leaves provider health unchanged', async (phase) => {
    vi.useFakeTimers();
    const router = recovery_router();
    const breaker = router.getProvider('p')!.breaker;
    breaker.recordFailure();
    await vi.advanceTimersByTimeAsync(1000);
    const caller = new AbortController();
    let entered: (() => void) | undefined;
    const started = new Promise<void>((resolve) => { entered = resolve; });
    const fetch_mock = vi.fn(async (_url: string, init: RequestInit) => {
      if (fetch_mock.mock.calls.length > 1) return Response.json({ input_tokens: 17 });
      if (phase === 'headers') {
        entered!();
        return pending_until_abort(init.signal!);
      }
      return new Response(new ReadableStream({
        start(controller) {
          controller.enqueue(new TextEncoder().encode('{"input_tokens":'));
          init.signal?.addEventListener('abort', () => controller.error(init.signal!.reason), { once: true });
          entered!();
        },
      }));
    });
    vi.stubGlobal('fetch', fetch_mock);
    const result = router.count_tokens('alias', {}, caller.signal);
    await started;
    caller.abort(new Error('caller stopped'));
    await expect(result).rejects.toMatchObject({ code: 'client_abort', status: 499, retriable: false });
    expect(fetch_mock).toHaveBeenCalledTimes(1);
    expect(breaker.snapshot()).toMatchObject({ state: 'half-open', failures: 1, halfOpenInFlight: 0 });
    expect(vi.getTimerCount()).toBe(0);
  });

  it.each(['headers', 'body'])('bounds silent %s by the provider deadline and recovers through the next eligible hop', async (phase) => {
    vi.useFakeTimers();
    const router = recovery_router();
    let first_signal: AbortSignal | undefined;
    const fetch_mock = vi.fn(async (_url: string, init: RequestInit) => {
      if (fetch_mock.mock.calls.length > 1) return Response.json({ input_tokens: 17 });
      first_signal = init.signal!;
      if (phase === 'headers') return pending_until_abort(init.signal!);
      return new Response(new ReadableStream({
        start(controller) {
          controller.enqueue(new TextEncoder().encode('{"input_tokens":'));
          init.signal?.addEventListener('abort', () => controller.error(init.signal!.reason), { once: true });
        },
      }));
    });
    vi.stubGlobal('fetch', fetch_mock);
    let observed: number | undefined;
    const result = router.count_tokens('alias', {}).then((tokens) => { observed = tokens; });
    await vi.advanceTimersByTimeAsync(999);
    expect(observed).toBeUndefined();
    expect(fetch_mock).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(1);
    expect(observed).toBe(17);
    await result;
    expect(first_signal?.aborted).toBe(true);
    expect(fetch_mock).toHaveBeenCalledTimes(2);
    expect(router.getProvider('p')!.breaker.snapshot()).toMatchObject({ state: 'open', failures: 1, halfOpenInFlight: 0 });
  });

  it('reopens on retriable failure, skips the unavailable hop, and closes after successful recovery', async () => {
    vi.useFakeTimers();
    const router = recovery_router();
    const fetch_mock = vi.fn()
      .mockResolvedValueOnce(new Response('{}', { status: 503 }))
      .mockResolvedValueOnce(Response.json({ input_tokens: 17 }))
      .mockResolvedValueOnce(Response.json({ input_tokens: 18 }))
      .mockResolvedValueOnce(Response.json({ input_tokens: 42 }));
    vi.stubGlobal('fetch', fetch_mock);
    await expect(router.count_tokens('alias', {})).resolves.toBe(17);
    expect(router.getProvider('p')!.breaker.snapshot()).toMatchObject({ state: 'open', failures: 1 });
    await expect(router.count_tokens('alias', {})).resolves.toBe(18);
    expect(fetch_mock).toHaveBeenCalledTimes(3);
    await vi.advanceTimersByTimeAsync(1000);
    await expect(router.count_tokens('alias', {})).resolves.toBe(42);
    expect(router.getProvider('p')!.breaker.snapshot()).toMatchObject({ state: 'closed', failures: 0, halfOpenInFlight: 0 });
  });

  it.each([null, -1, 1.5, '42'])('rejects malformed token count %s and uses only an eligible fallback', async (input_tokens) => {
    vi.useFakeTimers();
    const router = recovery_router();
    let abandoned_signal: AbortSignal | undefined;
    let aborted_at_retry: boolean | undefined;
    const fetch_mock = vi.fn(async (_url: string, init: RequestInit) => {
      if (fetch_mock.mock.calls.length === 1) {
        abandoned_signal = init.signal!;
        return Response.json({ input_tokens });
      }
      aborted_at_retry = abandoned_signal?.aborted;
      return Response.json({ input_tokens: 17 });
    });
    vi.stubGlobal('fetch', fetch_mock);
    await expect(router.count_tokens('alias', {})).resolves.toBe(17);
    expect(router.getProvider('p')!.breaker.snapshot()).toMatchObject({ state: 'open', failures: 1, halfOpenInFlight: 0 });
    expect(aborted_at_retry).toBe(true);
    expect(vi.getTimerCount()).toBe(0);
  });
});

describe('estimate_input_tokens', () => {
  it('is deterministic and splits by script', () => {
    const ascii = { system: 'abcdefgh', messages: [{ role: 'user', content: 'ijklmnop' }] };
    const cjk = { system: '你好世界', messages: [{ role: 'user', content: '测试文本' }] };
    const first = estimate_input_tokens(ascii);
    expect(first).toBe(estimate_input_tokens(ascii));
    expect(first).toBeGreaterThan(0);
    // Nine CJK characters dominate: the estimate must exceed the count the
    // same characters would produce as ASCII (9/4 = 2).
    expect(estimate_input_tokens(cjk)).toBeGreaterThanOrEqual(9);
  });

  it('never returns zero', () => {
    expect(estimate_input_tokens({})).toBe(1);
  });
});

describe('anthropicCountTokensUrl', () => {
  it('appends the suffix to a messages endpoint', () => {
    expect(anthropicCountTokensUrl('https://api.anthropic.com/v1/messages')).toBe(
      'https://api.anthropic.com/v1/messages/count_tokens',
    );
  });

  it('refuses anything that is not a messages endpoint', () => {
    expect(anthropicCountTokensUrl('https://api.anthropic.com/v1')).toBeUndefined();
    expect(anthropicCountTokensUrl('')).toBeUndefined();
  });
});

describe('POST /v1/messages/count_tokens', () => {
  it('answers with the deterministic estimate for a non-Anthropic hop', async () => {
    const { app } = createGatewayApp(config('mock'));
    const response = await app.request('/v1/messages/count_tokens', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({
        model: 'alias',
        messages: [{ role: 'user', content: '你好，这是测试' }],
      }),
    });
    expect(response.status).toBe(200);
    const body = (await response.json()) as { input_tokens: number };
    expect(Number.isInteger(body.input_tokens)).toBe(true);
    expect(body.input_tokens).toBeGreaterThan(0);
  });

  it('relays an Anthropic hop upstream and returns the exact count', async () => {
    const fetch_mock = vi.fn(async () => Response.json({ input_tokens: 42 }));
    vi.stubGlobal('fetch', fetch_mock);
    const { app } = createGatewayApp(config('anthropic'));
    const response = await app.request('/v1/messages/count_tokens', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ model: 'alias', messages: [{ role: 'user', content: 'hi' }] }),
    });
    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({ input_tokens: 42 });
    const [url, init] = fetch_mock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe('https://fixture.invalid/v1/messages/count_tokens');
    expect(JSON.parse(String(init.body)).model).toBe('upstream');
  });

  it('rejects a body without a model', async () => {
    const { app } = createGatewayApp(config('mock'));
    const response = await app.request('/v1/messages/count_tokens', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ messages: [] }),
    });
    expect(response.status).toBe(400);
  });

  it('fails closed on an unknown model', async () => {
    const { app } = createGatewayApp(config('mock'));
    const response = await app.request('/v1/messages/count_tokens', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ model: 'missing', messages: [] }),
    });
    expect(response.status).toBe(404);
  });
});

describe('GET /aag/discovery', () => {
  it('exposes the service identity without caching', async () => {
    const { app } = createGatewayApp(config('mock'));
    const response = await app.request('/aag/discovery');
    expect(response.status).toBe(200);
    expect(response.headers.get('cache-control')).toBe('no-store');
    const body = (await response.json()) as Record<string, unknown>;
    expect(body.service).toBe('ai-agent-gateway');
    expect(body.schemaVersion).toBe(1);
    expect(body.status).toBe('running');
    expect(typeof body.version).toBe('string');
  });
});
