/**
 * Half-open admission ownership tests.
 *
 * The Node breaker enforced `halfOpenRetries` against a counter that nothing
 * incremented, so the recovery-probe limit never applied and concurrent
 * requests could storm a provider that had just failed. Admission now consumes
 * a slot and is generation-stamped, which makes settle-once a caller
 * obligation. These tests cover both halves: that the limit is real, and that
 * every admitted attempt in the router gives its slot back — including
 * terminal errors, streams a client abandons mid-flight, and settles that
 * arrive after the breaker has moved to a newer recovery cycle.
 */
import { describe, it, expect, vi, afterEach } from 'vitest';
import { CircuitBreaker } from '../src/gateway/circuit-breaker.js';
import { Router } from '../src/gateway/router.js';
import { BaseProvider } from '../src/gateway/providers/base.js';
import { makeProviderError } from '../src/gateway/canonical.js';
import type { ProviderConfig, AppConfig, ChatCompletionRequest } from '../src/config/schema.js';
import type { OAIChatResponse, OAIStreamChunk } from '../src/gateway/canonical.js';

const cooldown_ms = 1000;

function breaker_config(overrides: Partial<CircuitBreaker['cfg']> = {}): CircuitBreaker['cfg'] {
  return {
    failureThreshold: 2,
    cooldownMs: cooldown_ms,
    halfOpenRetries: 2,
    successThreshold: 2,
    ...overrides,
  };
}

/** Drives a fresh breaker into half-open with no probe yet admitted. */
function opened_breaker(overrides: Partial<CircuitBreaker['cfg']> = {}): CircuitBreaker {
  const breaker = new CircuitBreaker(breaker_config(overrides));
  breaker.recordFailure();
  breaker.recordFailure();
  expect(breaker.snapshot().state).toBe('open');
  vi.advanceTimersByTime(cooldown_ms);
  return breaker;
}

describe('half-open admission ownership', () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it('admits at most halfOpenRetries concurrent recovery probes', () => {
    vi.useFakeTimers();
    const breaker = opened_breaker({ halfOpenRetries: 2 });

    expect(breaker.acquire()).not.toBeNull();
    expect(breaker.acquire()).not.toBeNull();
    // A third concurrent probe must be refused even though nothing has
    // reported an outcome yet.
    expect(breaker.acquire()).toBeNull();
    expect(breaker.snapshot().halfOpenInFlight).toBe(2);
  });

  it('returns the probe slot once a probe reports success', () => {
    vi.useFakeTimers();
    const breaker = opened_breaker({ halfOpenRetries: 1 });

    const probe = breaker.acquire();
    expect(probe).not.toBeNull();
    expect(breaker.acquire()).toBeNull();
    probe!.settle('success');

    expect(breaker.snapshot().halfOpenInFlight).toBe(0);
    expect(breaker.acquire()).not.toBeNull();
  });

  it('returns the probe slot on cancellation without judging provider health', () => {
    vi.useFakeTimers();
    const breaker = opened_breaker({ halfOpenRetries: 1, successThreshold: 2 });

    const probe = breaker.acquire();
    expect(probe).not.toBeNull();
    const before = breaker.snapshot();
    probe!.settle('cancelled');

    const snapshot = breaker.snapshot();
    expect(snapshot.state).toBe('half-open');
    expect(snapshot.halfOpenInFlight).toBe(0);
    // A cancelled attempt is not a recovery success and not a new failure.
    expect(snapshot.successes).toBe(0);
    expect(snapshot.failures).toBe(before.failures);
    expect(breaker.acquire()).not.toBeNull();
  });

  it('clamps cancellation when no probe was admitted', () => {
    vi.useFakeTimers();
    const breaker = opened_breaker({ halfOpenRetries: 1 });

    breaker.recordCancelled();
    breaker.recordCancelled();

    expect(breaker.snapshot().halfOpenInFlight).toBe(0);
    expect(breaker.acquire()).not.toBeNull();
  });

  it('settles an admission exactly once', () => {
    vi.useFakeTimers();
    const breaker = opened_breaker({ halfOpenRetries: 1, successThreshold: 1 });

    const probe = breaker.acquire();
    probe!.settle('success');
    // The circuit closed on the first settle; a second settle must not
    // decrement a probe counter of the new (closed) generation.
    probe!.settle('failure');

    const snapshot = breaker.snapshot();
    expect(snapshot.state).toBe('closed');
    expect(snapshot.halfOpenInFlight).toBe(0);
  });

  it('re-opens immediately when a recovery probe fails', () => {
    vi.useFakeTimers();
    const breaker = opened_breaker({ halfOpenRetries: 2 });

    const probe = breaker.acquire();
    expect(probe).not.toBeNull();
    probe!.settle('failure');

    expect(breaker.snapshot().state).toBe('open');
    expect(breaker.snapshot().halfOpenInFlight).toBe(0);
  });

  it('closes only after the configured recovery successes are settled', () => {
    vi.useFakeTimers();
    const breaker = opened_breaker({ halfOpenRetries: 1, successThreshold: 2 });

    const first = breaker.acquire();
    first!.settle('success');
    expect(breaker.snapshot().state).toBe('half-open');
    const second = breaker.acquire();
    second!.settle('success');
    expect(breaker.snapshot().state).toBe('closed');
  });

  it('does not consume probe slots while the circuit is closed', () => {
    const breaker = new CircuitBreaker(breaker_config({ halfOpenRetries: 1 }));

    for (let attempt = 0; attempt < 25; attempt += 1) expect(breaker.acquire()).not.toBeNull();

    expect(breaker.snapshot().halfOpenInFlight).toBe(0);
    expect(breaker.snapshot().state).toBe('closed');
  });

  it('refuses admission while the circuit is still in cooldown', () => {
    vi.useFakeTimers();
    const breaker = new CircuitBreaker(breaker_config({ halfOpenRetries: 2 }));
    breaker.recordFailure();
    breaker.recordFailure();
    vi.advanceTimersByTime(cooldown_ms - 1);

    expect(breaker.acquire()).toBeNull();
    expect(breaker.snapshot().halfOpenInFlight).toBe(0);
  });
});

describe('stale admissions from a retired generation are ignored', () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it('a stale cancellation cannot release another generation probe', () => {
    vi.useFakeTimers();
    // Mirrors the Rust breaker regression
    // stale_cancel_cannot_release_another_generation_probe: an attempt that
    // outlives its recovery cycle must not free a slot belonging to the next
    // cycle. A stalled attempt can outlive the cooldown: the provider timeout
    // is independent of `cooldownMs`.
    const breaker = opened_breaker({ halfOpenRetries: 2 });
    const stale = breaker.acquire();
    expect(stale).not.toBeNull();
    expect(breaker.acquire()).not.toBeNull();

    breaker.recordFailure(); // re-open: the stale generation is retired
    vi.advanceTimersByTime(cooldown_ms);
    expect(breaker.acquire()).not.toBeNull(); // new cycle probe
    expect(breaker.snapshot().halfOpenInFlight).toBe(1);

    stale!.settle('cancelled');

    expect(breaker.snapshot().state).toBe('half-open');
    expect(breaker.snapshot().halfOpenInFlight).toBe(1);
  });

  it('a stale failure cannot re-open a newer recovery cycle', () => {
    vi.useFakeTimers();
    const breaker = opened_breaker({ halfOpenRetries: 2 });
    const stale = breaker.acquire();

    breaker.recordFailure(); // re-open
    vi.advanceTimersByTime(cooldown_ms);
    expect(breaker.acquire()).not.toBeNull();

    stale!.settle('failure');

    expect(breaker.snapshot().state).toBe('half-open');
    expect(breaker.snapshot().halfOpenInFlight).toBe(1);
  });

  it('a stale success cannot complete a newer recovery cycle', () => {
    vi.useFakeTimers();
    // successThreshold 1: a live success would close the breaker, so the
    // assertion is decisive for the guard.
    const breaker = opened_breaker({ halfOpenRetries: 1, successThreshold: 1 });
    const stale = breaker.acquire();

    breaker.recordFailure(); // re-open
    vi.advanceTimersByTime(cooldown_ms);
    expect(breaker.acquire()).not.toBeNull();

    stale!.settle('success');

    const snapshot = breaker.snapshot();
    expect(snapshot.state).toBe('half-open');
    expect(snapshot.successes).toBe(0);
  });
});

function provider_config(id: string, overrides: Partial<ProviderConfig> = {}): ProviderConfig {
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
    ...overrides,
  };
}

function app_config(): AppConfig {
  return {
    version: 1,
    gateway: { host: '127.0.0.1', port: 8787, authToken: '', logLevel: 'warn' },
    providers: [],
    routes: [{ alias: 'm', hops: [{ provider: 'a', model: 'm' }], description: 'test' }],
    profiles: [],
    agents: [],
  };
}

type Behavior =
  | { kind: 'ok' }
  | { kind: 'http_error'; status: number }
  | { kind: 'stall_after_first' }
  | { kind: 'stall_before_first' }
  | { kind: 'error_chunk_mid_400' };

class FakeProvider extends BaseProvider {
  behavior: Behavior;
  calls = 0;
  last_signal: AbortSignal | undefined;

  constructor(id: string, behavior: Behavior) {
    super(provider_config(id));
    this.behavior = behavior;
  }

  async complete(_req: ChatCompletionRequest): Promise<OAIChatResponse> {
    this.calls += 1;
    const behavior = this.behavior;
    if (behavior.kind === 'http_error') {
      throw makeProviderError('upstream_http_error', {
        status: behavior.status,
        retriable: behavior.status >= 500 || behavior.status === 429,
        stage: 'response_status',
      });
    }
    return {
      id: 'chatcmpl-fake',
      object: 'chat.completion',
      created: 1,
      model: 'm',
      choices: [{ index: 0, message: { role: 'assistant', content: 'done' }, finish_reason: 'stop' }],
      usage: { prompt_tokens: 1, completion_tokens: 1, total_tokens: 2 },
    };
  }

  async *stream(
    _req: ChatCompletionRequest,
    signal?: AbortSignal,
  ): AsyncGenerator<OAIStreamChunk> {
    this.calls += 1;
    this.last_signal = signal;
    const behavior = this.behavior;
    if (behavior.kind === 'http_error') {
      throw makeProviderError('upstream_http_error', {
        status: behavior.status,
        retriable: behavior.status >= 500 || behavior.status === 429,
        stage: 'stream_status',
      });
    }
    const chunk = (delta: Record<string, unknown>, finish_reason: string | null): OAIStreamChunk => ({
      id: 'chatcmpl-fake',
      object: 'chat.completion.chunk',
      created: 1,
      model: 'm',
      choices: [{ index: 0, delta, finish_reason }],
    });
    if (behavior.kind === 'stall_before_first') {
      // Never emits a first byte: the router must give up via its
      // first-byte budget and reclaim the attempt.
      await new Promise<void>(() => {});
    }
    if (behavior.kind === 'stall_after_first') {
      // Emits a real first byte, then never finishes. Only a client that stops
      // consuming can end this attempt.
      yield chunk({ role: 'assistant' }, null);
      await new Promise<void>(() => {});
    }
    if (behavior.kind === 'error_chunk_mid_400') {
      yield chunk({ content: 'partial' }, null);
      yield {
        id: 'chatcmpl-fake',
        object: 'chat.completion.chunk',
        created: 1,
        model: 'm',
        choices: [],
        error: { type: 'invalid_request_error', message: 'bad input', status: 400 },
      } as OAIStreamChunk;
      return;
    }
    yield chunk({ content: 'hello' }, null);
    yield chunk({}, 'stop');
  }
}

const chat_request: ChatCompletionRequest = {
  model: 'm',
  messages: [{ role: 'user', content: 'hi' }],
} as ChatCompletionRequest;

function build_router(provider: FakeProvider, opts: { firstByteTimeoutMs?: number } = {}): Router {
  const router = new Router(app_config(), opts);
  const providers = (router as unknown as { providers: Map<string, BaseProvider> }).providers;
  providers.set('a', provider);
  return router;
}

/** Trips the breaker into open and advances past cooldown into half-open. */
async function half_open(provider: FakeProvider, router: Router): Promise<void> {
  await expect(router.complete(chat_request)).rejects.toThrow();
  await expect(router.complete(chat_request)).rejects.toThrow();
  expect(provider.breaker.snapshot().state).toBe('open');
  vi.advanceTimersByTime(cooldown_ms);
}

describe('router settles every admitted hop', () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it('releases the probe slot when a hop ends in a terminal client error', async () => {
    const provider = new FakeProvider('a', { kind: 'http_error', status: 503 });
    const router = build_router(provider);

    vi.useFakeTimers();
    await half_open(provider, router);

    // A terminal 400 is not the provider's fault, but it still consumed an
    // admitted probe slot: it must release it instead of leaking it.
    provider.behavior = { kind: 'http_error', status: 400 };
    await expect(router.complete(chat_request)).rejects.toThrow();

    const snapshot = provider.breaker.snapshot();
    expect(snapshot.state).toBe('half-open');
    expect(snapshot.halfOpenInFlight).toBe(0);
  });

  it('releases the probe slot and reclaims the upstream when a client abandons a stream', async () => {
    const provider = new FakeProvider('a', { kind: 'http_error', status: 503 });
    const router = build_router(provider);

    vi.useFakeTimers();
    await half_open(provider, router);

    provider.behavior = { kind: 'stall_after_first' };
    const stream = router.stream(chat_request);
    await stream.next();
    expect(provider.breaker.snapshot().halfOpenInFlight).toBe(1);

    // The client disconnects here: the router's generator is closed early.
    await stream.return(undefined);

    expect(provider.breaker.snapshot().halfOpenInFlight).toBe(0);
    // The upstream attempt must not keep reading until the provider timeout:
    // the abandonment path aborts the attempt signal.
    expect(provider.last_signal?.aborted).toBe(true);
  });

  it('releases the probe slot when the first-byte budget expires', async () => {
    const provider = new FakeProvider('a', { kind: 'http_error', status: 503 });
    const router = build_router(provider, { firstByteTimeoutMs: 50 });

    vi.useFakeTimers();
    await half_open(provider, router);

    provider.behavior = { kind: 'stall_before_first' };
    // Attach the rejection expectation before advancing the fake clock: the
    // attempt rejects while the timers advance, and a handler attached after
    // that leaves a rejected promise unhandled for a microtask checkpoint.
    const attempt_promise = expect(router.stream(chat_request).next()).rejects.toThrow();
    await vi.advanceTimersByTimeAsync(60);
    await attempt_promise;

    // The probe failed retriably: the breaker re-opens with its slot released.
    const snapshot = provider.breaker.snapshot();
    expect(snapshot.state).toBe('open');
    expect(snapshot.halfOpenInFlight).toBe(0);
    expect(provider.last_signal?.aborted).toBe(true);
  });

  it('settles a mid-stream terminal error chunk as a cancellation', async () => {
    const provider = new FakeProvider('a', { kind: 'http_error', status: 503 });
    const router = build_router(provider);

    vi.useFakeTimers();
    await half_open(provider, router);

    provider.behavior = { kind: 'error_chunk_mid_400' };
    const stream = router.stream(chat_request);
    const chunks = [await stream.next()];
    for (let index = 0; index < 3; index += 1) {
      const step = await stream.next();
      if (step.done) break;
      chunks.push(step);
    }

    // A 400 error event is the client's fault, not the provider's: the probe
    // slot is released (via the finally) without judging the provider, so the
    // breaker stays half-open with no recovery success recorded.
    const snapshot = provider.breaker.snapshot();
    expect(snapshot.state).toBe('half-open');
    expect(snapshot.halfOpenInFlight).toBe(0);
    expect(snapshot.successes).toBe(0);
  });

  it('settles a consumer-thrown error as a cancellation, not a provider failure', async () => {
    const provider = new FakeProvider('a', { kind: 'http_error', status: 503 });
    const router = build_router(provider);

    vi.useFakeTimers();
    await half_open(provider, router);

    provider.behavior = { kind: 'stall_after_first' };
    const stream = router.stream(chat_request);
    await stream.next();
    expect(provider.breaker.snapshot().halfOpenInFlight).toBe(1);

    // The consumer injects an abort into the suspended yield. This is a
    // client-side unwind: it must not re-open the breaker (Rust settles
    // Cancelled on any unwind from the client side). The surfaced error is
    // normalized by the existing catch path; only the verdict matters here.
    await expect(stream.throw(new Error('client disconnected'))).rejects.toThrow();

    const snapshot = provider.breaker.snapshot();
    expect(snapshot.state).toBe('half-open');
    expect(snapshot.halfOpenInFlight).toBe(0);
    expect(provider.last_signal?.aborted).toBe(true);
  });
});
