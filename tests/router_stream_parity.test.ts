/**
 * Router streaming/failover parity tests: verification of the Node-gateway
 * port of the Rust proxy's streaming protections (src-tauri/src/proxy/
 * router.rs `stream_hops`). Covers empty-stream failover, the first-byte
 * timeout, terminal-4xx breaker immunity, tool-capability 400 failover,
 * auth-expired terminality, the missing-terminal-chunk watchdog, and the
 * header-scoped stream fetch timeout.
 */
import { describe, it, expect } from 'vitest';
import { Router } from '../src/gateway/router.js';
import { BaseProvider } from '../src/gateway/providers/base.js';
import {
  makeProviderError,
  makeStreamErrorChunk,
  classifyUpstream4xx,
  streamChunkError,
  streamErrorStatus,
} from '../src/gateway/canonical.js';
import {
  oaiChunksToAnthropicStream,
  anthropicStreamToOAIChunks,
} from '../src/gateway/converter.js';
import type { ProviderConfig, AppConfig, ChatCompletionRequest } from '../src/config/schema.js';
import type { OAIChatResponse, OAIStreamChunk } from '../src/gateway/canonical.js';

function providerConfig(id: string, overrides: Partial<ProviderConfig> = {}): ProviderConfig {
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
      failureThreshold: 3,
      cooldownMs: 1000,
      halfOpenRetries: 1,
      successThreshold: 1,
    },
    weight: 1,
    ...overrides,
  };
}

function appConfig(): AppConfig {
  return {
    version: 1,
    gateway: { host: '127.0.0.1', port: 8787, authToken: '', logLevel: 'warn' },
    providers: [],
    routes: [
      {
        alias: 'm',
        hops: [
          { provider: 'a', model: 'm' },
          { provider: 'b', model: 'm' },
        ],
        description: 'test',
      },
    ],
    profiles: [],
    agents: [],
  };
}

type Behavior =
  | { kind: 'ok' }
  | { kind: 'empty' }
  | { kind: 'stall' }
  | { kind: 'no_terminal' }
  | { kind: 'no_choices' }
  | { kind: 'error_chunk_first'; errorType: string }
  | { kind: 'error_chunk_mid'; errorType: string }
  | {
      kind: 'http_error';
      status: number;
      errorKind?: 'auth_expired' | 'tool_capability';
      stage?: string;
    };

class FakeProvider extends BaseProvider {
  readonly behavior: Behavior;
  calls = 0;
  lastSignal: AbortSignal | undefined;

  constructor(id: string, behavior: Behavior) {
    super(providerConfig(id));
    this.behavior = behavior;
  }

  async complete(_req: ChatCompletionRequest): Promise<OAIChatResponse> {
    this.calls += 1;
    const b = this.behavior;
    if (b.kind === 'http_error') {
      throw makeProviderError('upstream_http_error', {
        status: b.status,
        retriable: b.status >= 500 || b.status === 429,
        stage: b.stage ?? 'response_status',
        kind: b.errorKind,
      });
    }
    return {
      id: 'chatcmpl-fake',
      object: 'chat.completion',
      created: 1,
      model: 'm',
      choices: [
        { index: 0, message: { role: 'assistant', content: 'done' }, finish_reason: 'stop' },
      ],
      usage: { prompt_tokens: 1, completion_tokens: 1, total_tokens: 2 },
    };
  }

  async *stream(
    _req: ChatCompletionRequest,
    signal?: AbortSignal,
  ): AsyncGenerator<OAIStreamChunk> {
    this.calls += 1;
    this.lastSignal = signal;
    const b = this.behavior;
    if (b.kind === 'stall') {
      // Never resolves: the router must give up via its first-byte budget
      // and abort the attempt signal.
      await new Promise<void>(() => {});
    }
    if (b.kind === 'http_error') {
      throw makeProviderError('upstream_http_error', {
        status: b.status,
        retriable: b.status >= 500 || b.status === 429,
        stage: b.stage ?? 'stream_status',
        kind: b.errorKind,
      });
    }
    const id = 'chatcmpl-fake';
    const chunk = (delta: Record<string, unknown>, finish_reason: string | null): OAIStreamChunk => ({
      id,
      object: 'chat.completion.chunk',
      created: 1,
      model: 'm',
      choices: [{ index: 0, delta, finish_reason }],
    });
    const errorChunk = (type: string): OAIStreamChunk =>
      ({
        id,
        object: 'chat.completion.chunk',
        created: 1,
        model: 'm',
        choices: [],
        error: { type, message: 'upstream error', status: streamErrorStatus({ type }) },
      }) as OAIStreamChunk;
    if (b.kind === 'error_chunk_first') {
      yield errorChunk(b.errorType);
      return;
    }
    if (b.kind === 'ok') {
      yield chunk({ role: 'assistant' }, null);
      yield chunk({ content: 'hello' }, null);
      yield chunk({}, 'stop');
      return;
    }
    if (b.kind === 'error_chunk_mid') {
      yield chunk({ content: 'partial' }, null);
      yield errorChunk(b.errorType);
      return;
    }
    if (b.kind === 'no_choices') {
      // Raw SSE object with no choices array: must pass through untouched.
      yield { id, object: 'chat.completion.chunk', created: 1, model: 'm' } as OAIStreamChunk;
      yield chunk({}, 'stop');
      return;
    }
    if (b.kind === 'no_terminal') {
      yield chunk({ content: 'partial' }, null);
    }
    // 'empty': yield nothing.
  }
}

function buildRouter(
  behaviorA: Behavior,
  behaviorB: Behavior,
  opts: { firstByteTimeoutMs?: number } = {},
) {
  const router = new Router(appConfig(), opts);
  const fakeA = new FakeProvider('a', behaviorA);
  const fakeB = new FakeProvider('b', behaviorB);
  const providers = (router as any).providers as Map<string, BaseProvider>;
  providers.set('a', fakeA);
  providers.set('b', fakeB);
  return { router, fakeA, fakeB };
}

async function collect(gen: AsyncGenerator<OAIStreamChunk>): Promise<OAIStreamChunk[]> {
  const out: OAIStreamChunk[] = [];
  for await (const ch of gen) out.push(ch);
  return out;
}

function streamText(chunks: OAIStreamChunk[]): string {
  return chunks
    .flatMap((ch) => ch.choices.map((c) => (typeof c.delta.content === 'string' ? c.delta.content : '')))
    .join('');
}

function request(withTools: boolean): ChatCompletionRequest {
  return {
    model: 'm',
    messages: [{ role: 'user', content: 'hi' }],
    ...(withTools ? { tools: [{ type: 'function', function: { name: 'f' } }] } : {}),
  };
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

describe('router stream parity: empty stream', () => {
  it('fails over to the next hop instead of a blank success', async () => {
    const { router, fakeA, fakeB } = buildRouter({ kind: 'empty' }, { kind: 'ok' });
    const chunks = await collect(router.stream(request(false)));
    expect(streamText(chunks)).toContain('hello');
    expect(fakeB.calls).toBe(1);
    expect(fakeA.breaker.snapshot().failures).toBe(1);
  });

  it('returns the stream error when every hop is empty', async () => {
    const { router } = buildRouter({ kind: 'empty' }, { kind: 'empty' });
    await expect(collect(router.stream(request(false)))).rejects.toMatchObject({
      code: 'upstream_stream_error',
      status: 502,
      stage: 'stream_empty',
    });
  });
});

describe('router stream parity: first-byte timeout', () => {
  it('fails over a stalled hop within the budget', async () => {
    const { router, fakeA, fakeB } = buildRouter({ kind: 'stall' }, { kind: 'ok' }, {
      firstByteTimeoutMs: 25,
    });
    const started = Date.now();
    const chunks = await collect(router.stream(request(false)));
    expect(streamText(chunks)).toContain('hello');
    expect(fakeB.calls).toBe(1);
    expect(fakeA.breaker.snapshot().failures).toBe(1);
    expect(Date.now() - started).toBeLessThan(2000);
  });

  it('aborts the abandoned attempt signal so the fetch is reclaimed', async () => {
    const { router, fakeA } = buildRouter({ kind: 'stall' }, { kind: 'ok' }, {
      firstByteTimeoutMs: 25,
    });
    await collect(router.stream(request(false)));
    expect(fakeA.lastSignal).toBeDefined();
    expect(fakeA.lastSignal?.aborted).toBe(true);
  });
});

describe('router parity: breaker settlement', () => {
  it('terminal 4xx does not count against the breaker', async () => {
    const { router, fakeA, fakeB } = buildRouter(
      { kind: 'http_error', status: 400 },
      { kind: 'ok' },
    );
    await expect(collect(router.stream(request(false)))).rejects.toMatchObject({
      code: 'upstream_http_error',
      status: 400,
    });
    expect(fakeA.breaker.snapshot().failures).toBe(0);
    expect(fakeB.calls).toBe(0);
  });

  it('terminal 4xx on complete() does not count against the breaker', async () => {
    const { router, fakeA, fakeB } = buildRouter(
      { kind: 'http_error', status: 401 },
      { kind: 'ok' },
    );
    await expect(router.complete(request(false))).rejects.toMatchObject({
      status: 401,
    });
    expect(fakeA.breaker.snapshot().failures).toBe(0);
    expect(fakeB.calls).toBe(0);
  });
});

describe('router parity: tool-capability 400 failover', () => {
  it('fails over a classified tool-capability 400 when tools are present', async () => {
    const { router, fakeA, fakeB } = buildRouter(
      { kind: 'http_error', status: 400, errorKind: 'tool_capability', stage: 'stream_status' },
      { kind: 'ok' },
    );
    const chunks = await collect(router.stream(request(true)));
    expect(streamText(chunks)).toContain('hello');
    expect(fakeA.breaker.snapshot().failures).toBe(1);
    expect(fakeB.calls).toBe(1);
  });

  it('complete() also routes around a tool-capability 400', async () => {
    const { router, fakeA, fakeB } = buildRouter(
      { kind: 'http_error', status: 400, errorKind: 'tool_capability', stage: 'response_status' },
      { kind: 'ok' },
    );
    const resp = await router.complete(request(true));
    expect(resp.choices[0].message.content).toBe('done');
    expect(fakeA.breaker.snapshot().failures).toBe(1);
    expect(fakeB.calls).toBe(1);
  });

  it('an auth-expired 400 with tools stays terminal', async () => {
    const { router, fakeA, fakeB } = buildRouter(
      { kind: 'http_error', status: 400, errorKind: 'auth_expired', stage: 'stream_status' },
      { kind: 'ok' },
    );
    await expect(collect(router.stream(request(true)))).rejects.toMatchObject({
      status: 400,
    });
    expect(fakeA.breaker.snapshot().failures).toBe(0);
    expect(fakeB.calls).toBe(0);
  });

  it('an unclassified 400 with tools stays terminal', async () => {
    const { router, fakeB } = buildRouter(
      { kind: 'http_error', status: 400, stage: 'stream_status' },
      { kind: 'ok' },
    );
    await expect(collect(router.stream(request(true)))).rejects.toMatchObject({
      status: 400,
    });
    expect(fakeB.calls).toBe(0);
  });
});

describe('router parity: terminal-chunk watchdog', () => {
  it('a stream that ends without a terminal chunk yields an error chunk', async () => {
    const { router, fakeA, fakeB } = buildRouter({ kind: 'no_terminal' }, { kind: 'ok' });
    const chunks = await collect(router.stream(request(false)));
    const last = chunks[chunks.length - 1] as OAIStreamChunk & {
      error?: { status?: number; type?: string };
    };
    expect(last.error?.status).toBe(502);
    expect(last.error?.type).toBe('proxy_stream_error');
    expect(fakeA.breaker.snapshot().failures).toBe(1);
    // Committed mid-stream: no failover to the second hop.
    expect(fakeB.calls).toBe(0);
  });
});

describe('canonical: upstream 4xx classification', () => {
  it('classifies OAuth/JSON auth failures', () => {
    expect(
      classifyUpstream4xx(400, JSON.stringify({ error: { code: 'invalid_grant' } })),
    ).toBe('auth_expired');
    expect(
      classifyUpstream4xx(400, JSON.stringify({ error: { code: 'invalid_api_key' } })),
    ).toBe('auth_expired');
    expect(classifyUpstream4xx(403, 'Please sign in again')).toBe('auth_expired');
  });

  it('classifies tool-capability rejections', () => {
    expect(
      classifyUpstream4xx(
        400,
        JSON.stringify({ error: { type: 'invalid_request_error', message: 'unsupported tool format' } }),
      ),
    ).toBe('tool_capability');
    expect(classifyUpstream4xx(400, 'tools are not supported on this endpoint')).toBe('tool_capability');
  });

  it('does not classify non-4xx or unrelated bodies', () => {
    expect(classifyUpstream4xx(500, 'sign in again')).toBeUndefined();
    expect(classifyUpstream4xx(200, '{}')).toBeUndefined();
    expect(
      classifyUpstream4xx(400, JSON.stringify({ error: { message: 'Something unrelated' } })),
    ).toBeUndefined();
  });

  it('a tools[0] JSON-path echo is not a tool-capability rejection', () => {
    expect(
      classifyUpstream4xx(400, JSON.stringify({ error: { message: 'tools[0] has an invalid shape' } })),
    ).toBeUndefined();
  });

  it('a present-but-scalar error field suppresses top-level structured fields', () => {
    expect(
      classifyUpstream4xx(400, JSON.stringify({ error: 'boom', code: 'invalid_api_key' })),
    ).toBeUndefined();
  });

  it('the scan window is 8192 bytes, not code units', () => {
    // 4100 two-byte characters = 8200 bytes; the keyword sits past the
    // byte budget and must not be scanned.
    expect(classifyUpstream4xx(400, 'é'.repeat(4100) + 'invalid function')).toBeUndefined();
    expect(classifyUpstream4xx(400, 'é'.repeat(2000) + ' invalid function ')).toBe('tool_capability');
  });
});

describe('canonical: stream error chunk', () => {
  it('never echoes the client model', () => {
    const chunk = makeStreamErrorChunk(502, 'proxy_stream_error') as OAIStreamChunk & {
      error?: { status?: number };
    };
    expect(chunk.model).toBe('unavailable');
    expect(chunk.choices).toEqual([]);
    expect(chunk.error?.status).toBe(502);
  });
});

describe('canonical: stream error status mapping', () => {
  it('maps Anthropic error types to statuses', () => {
    expect(streamErrorStatus({ type: 'overloaded_error' })).toBe(529);
    expect(streamErrorStatus({ type: 'rate_limit_error' })).toBe(429);
    expect(streamErrorStatus({ type: 'authentication_error' })).toBe(401);
    expect(streamErrorStatus({ type: 'permission_error' })).toBe(403);
    expect(streamErrorStatus({ type: 'api_error' })).toBe(502);
    expect(streamErrorStatus({ type: 'whatever' })).toBe(400);
  });

  it('prefers explicit numeric statuses and accepts string digits', () => {
    expect(streamErrorStatus({ error: { status: 503 } })).toBe(503);
    expect(streamErrorStatus({ error: { status_code: '429' } })).toBe(429);
    expect(streamErrorStatus({ error: { code: 529 } })).toBe(529);
    expect(streamErrorStatus({ error: { status: 999 } })).toBe(400);
    expect(streamErrorStatus({ error: { code: 'server_error' } })).toBe(400);
  });

  it('classifies retriability from the mapped status', () => {
    expect(
      streamChunkError(makeStreamErrorChunk(529, 'overloaded_error') as OAIStreamChunk)?.retriable,
    ).toBe(true);
    expect(
      streamChunkError(makeStreamErrorChunk(401, 'authentication_error') as OAIStreamChunk)?.retriable,
    ).toBe(false);
    expect(streamChunkError({ id: 'x', object: 'chat.completion.chunk', created: 1, model: 'm', choices: [] })).toBeUndefined();
  });
});

describe('router parity: in-stream error events', () => {
  it('a retriable error first chunk fails over to the next hop', async () => {
    const { router, fakeA, fakeB } = buildRouter(
      { kind: 'error_chunk_first', errorType: 'overloaded_error' },
      { kind: 'ok' },
    );
    const chunks = await collect(router.stream(request(false)));
    expect(streamText(chunks)).toContain('hello');
    expect(fakeA.breaker.snapshot().failures).toBe(1);
    expect(fakeB.calls).toBe(1);
    // The abandoned attempt is aborted so the upstream connection closes.
    expect(fakeA.lastSignal?.aborted).toBe(true);
  });

  it('a terminal error first chunk throws without polluting the breaker', async () => {
    const { router, fakeA, fakeB } = buildRouter(
      { kind: 'error_chunk_first', errorType: 'authentication_error' },
      { kind: 'ok' },
    );
    await expect(collect(router.stream(request(false)))).rejects.toMatchObject({
      code: 'upstream_stream_error',
      status: 401,
      stage: 'stream_event',
    });
    expect(fakeA.breaker.snapshot().failures).toBe(0);
    expect(fakeB.calls).toBe(0);
  });

  it('a mid-stream retriable error chunk is yielded as-is and settles a failure', async () => {
    const { router, fakeA } = buildRouter({ kind: 'error_chunk_mid', errorType: 'overloaded_error' }, { kind: 'ok' });
    const chunks = await collect(router.stream(request(false)));
    const last = chunks[chunks.length - 1] as OAIStreamChunk & { error?: { status?: number } };
    expect(last.error?.status).toBe(529);
    expect(fakeA.breaker.snapshot().failures).toBe(1);
  });

  it('a mid-stream terminal error chunk settles nothing', async () => {
    const { router, fakeA } = buildRouter({ kind: 'error_chunk_mid', errorType: 'authentication_error' }, { kind: 'ok' });
    const chunks = await collect(router.stream(request(false)));
    const last = chunks[chunks.length - 1] as OAIStreamChunk & { error?: { status?: number } };
    expect(last.error?.status).toBe(401);
    expect(fakeA.breaker.snapshot().failures).toBe(0);
  });

  it('a raw choice-less chunk passes through without crashing the watchdog', async () => {
    const { router, fakeA } = buildRouter({ kind: 'no_choices' }, { kind: 'ok' });
    const chunks = await collect(router.stream(request(false)));
    expect(chunks.length).toBe(2);
    expect(fakeA.breaker.snapshot().failures).toBe(0);
  });
});

describe('converter: error events cross the Anthropic surface', () => {
  async function* single(chunk: OAIStreamChunk): AsyncGenerator<OAIStreamChunk> {
    yield chunk;
  }

  it('the watchdog error chunk becomes a terminal Anthropic error event', async () => {
    const lines: string[] = [];
    for await (const line of oaiChunksToAnthropicStream(
      single(makeStreamErrorChunk(502, 'proxy_stream_error')),
      'claude-m',
    )) {
      lines.push(line);
    }
    const joined = lines.join('');
    expect(joined).toContain('event: error');
    expect(joined).not.toContain('event: message_stop');
  });

  it('an upstream Anthropic error event becomes a classified error chunk', async () => {
    async function* upstream(): AsyncGenerator<string> {
      yield 'event: message_start';
      yield 'data: {"type":"message_start","message":{}}';
      yield 'event: error';
      yield 'data: {"type":"error","error":{"type":"overloaded_error","message":"overloaded"}}';
    }
    const chunks: OAIStreamChunk[] = [];
    for await (const ch of anthropicStreamToOAIChunks(upstream(), 'm')) {
      chunks.push(ch);
    }
    const last = chunks[chunks.length - 1] as OAIStreamChunk & {
      error?: { status?: number; type?: string };
      model?: string;
      id?: string;
    };
    expect(last.error?.status).toBe(529);
    // Normalized envelope: fixed type/id/model so upstream strings never
    // ride client-visible error chunks (Rust parity).
    expect(last.error?.type).toBe('upstream_error');
    expect(last.model).toBe('unavailable');
    expect(last.id).toBe('proxy_error');
    expect(streamChunkError(last)?.retriable).toBe(true);
  });

  it('message_stop without a stop_reason delta still yields a terminal chunk', async () => {
    async function* upstream(): AsyncGenerator<string> {
      yield 'data: {"type":"message_start","message":{}}';
      yield 'data: {"type":"content_block_delta","delta":{"type":"text_delta","text":"hi"}}';
      yield 'data: {"type":"message_delta","delta":{"stop_sequence":null}}';
      yield 'data: {"type":"message_stop"}';
    }
    const chunks: OAIStreamChunk[] = [];
    for await (const ch of anthropicStreamToOAIChunks(upstream(), 'm')) {
      chunks.push(ch);
    }
    const last = chunks[chunks.length - 1];
    expect(last.choices[0].finish_reason).toBe('stop');
  });

  it('a top-level status survives a null error field (serde or_else parity)', () => {
    expect(streamErrorStatus({ type: 'error', error: null, status: 503 })).toBe(503);
    expect(streamErrorStatus({ error: 'boom', type: 'rate_limit_error' })).toBe(400);
  });

  it('a scalar error chunk is terminal 400, not an ordinary chunk', () => {
    const chunk = {
      id: 'x',
      object: 'chat.completion.chunk',
      created: 1,
      model: 'm',
      choices: [],
      error: 'overloaded',
    } as unknown as OAIStreamChunk;
    const err = streamChunkError(chunk);
    expect(err).toBeDefined();
    expect(err?.status).toBe(400);
    expect(err?.retriable).toBe(false);
  });

  it('a 429 error chunk is retriable via the default rule', () => {
    const chunk = makeStreamErrorChunk(429, 'rate_limit_error');
    expect(streamChunkError(chunk)?.retriable).toBe(true);
  });
});

describe('providers: header-scoped stream timeout', () => {
  class SignalProbe extends BaseProvider {
    constructor() {
      super(providerConfig('probe'));
    }
    async complete(): Promise<OAIChatResponse> {
      throw new Error('unused');
    }
    async *stream(): AsyncGenerator<OAIStreamChunk> {
      // unused
    }
    probe(outer?: AbortSignal): {
      signal: AbortSignal;
      cancel: () => void;
      dispose: () => void;
    } {
      return this.headerTimeoutController(outer);
    }
  }

  it('cancel() prevents the timer abort: long streams are not truncated', async () => {
    const probe = new SignalProbe().probe(); // timeoutMs = 60 from providerConfig
    probe.cancel();
    await sleep(150);
    expect(probe.signal.aborted).toBe(false);
  });

  it('aborts when headers never arrive within the timeout', async () => {
    const probe = new SignalProbe().probe();
    await sleep(150);
    expect(probe.signal.aborted).toBe(true);
  });

  it('the outer (router) abort still fires AFTER headers arrived', async () => {
    // Regression pin: cancel() must clear only the header timer. The
    // router's first-byte timeout lands after headers and must still
    // reach the body read of an abandoned hop.
    const outer = new AbortController();
    const probe = new SignalProbe().probe(outer.signal);
    probe.cancel();
    outer.abort();
    expect(probe.signal.aborted).toBe(true);
  });

  it('dispose() detaches the outer abort listener', async () => {
    const outer = new AbortController();
    const probe = new SignalProbe().probe(outer.signal);
    probe.cancel();
    probe.dispose();
    outer.abort();
    expect(probe.signal.aborted).toBe(false);
  });

  it('a pre-aborted outer signal aborts immediately', () => {
    const outer = new AbortController();
    outer.abort();
    const probe = new SignalProbe().probe(outer.signal);
    expect(probe.signal.aborted).toBe(true);
  });
});

describe('providers: parseSSE malformed-line termination', () => {
  it('a garbage data line ends the stream with an explicit error chunk', async () => {
    const { parseSSE } = await import('../src/gateway/providers/openai.js');
    const body = new ReadableStream<Uint8Array>({
      start(controller) {
        const encoder = new TextEncoder();
        controller.enqueue(encoder.encode('data: {"id":"a","object":"chat.completion.chunk","created":1,"model":"m","choices":[]}\n\n'));
        controller.enqueue(encoder.encode('data: not-json-at-all\n\n'));
        controller.close();
      },
    });
    const chunks: unknown[] = [];
    for await (const ch of parseSSE(body, 'probe')) {
      chunks.push(ch);
    }
    expect(chunks.length).toBe(2);
    const last = chunks[1] as { error?: { status?: number; type?: string } };
    expect(last.error?.status).toBe(502);
    expect(last.error?.type).toBe('proxy_stream_error');
  });
});
