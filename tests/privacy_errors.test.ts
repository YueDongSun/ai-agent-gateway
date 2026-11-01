import { afterEach, describe, expect, it, vi } from 'vitest';
import type { AppConfig, ProviderConfig } from '../src/config/schema.js';
import { createGatewayApp } from '../src/gateway/server.js';
import { makeProviderError, type OAIStreamChunk } from '../src/gateway/canonical.js';
import { OpenAIProvider, parseSSE } from '../src/gateway/providers/openai.js';
import { logger, safeLogRef, setLogLevel } from '../src/util/log.js';

const PRIVATE_CANARIES = [
  'CANARY_PROMPT_UNIQUE_91',
  'CANARY_RESPONSE_UNIQUE_92',
  'sk-private-93',
  'C:\\Users\\private-user\\secret.txt',
  'https://user:pass@private.example.invalid/path?token=94#fragment',
  'private.person@example.invalid',
  '100.64.12.34',
  'private-node.tail123.ts.net',
];

function provider_config(): ProviderConfig {
  return {
    id: 'provider-CANARY_PROMPT_UNIQUE_91',
    name: 'Private provider',
    type: 'openai',
    apiKey: 'sk-private-93',
    baseURL: 'https://private.example.invalid/v1',
    models: ['model-CANARY_RESPONSE_UNIQUE_92'],
    timeoutMs: 1_000,
    enabled: true,
    circuitBreaker: {
      failureThreshold: 2,
      cooldownMs: 1_000,
      halfOpenRetries: 1,
      successThreshold: 1,
    },
    weight: 1,
  };
}

function assert_private_canaries_absent(value: string) {
  for (const canary of PRIVATE_CANARIES) {
    expect(value).not.toContain(canary);
  }
}

afterEach(() => {
  vi.restoreAllMocks();
  setLogLevel('info');
});

describe('content-free Node error boundary', () => {
  it('blocks browser origins when auth is empty and protects health when auth is set', async () => {
    const empty_auth: AppConfig = {
      version: 1,
      gateway: { host: '127.0.0.1', port: 8787, authToken: '', logLevel: 'error' },
      providers: [], routes: [], profiles: [], agents: [],
    };
    const empty_app = createGatewayApp(empty_auth).app;
    expect((await empty_app.request('/health')).status).toBe(200);
    const blocked = await empty_app.request('/health', {
      headers: { origin: 'https://attacker.example.invalid' },
    });
    expect(blocked.status).toBe(403);
    expect(await blocked.text()).not.toContain('providers');

    const authenticated = {
      ...empty_auth,
      gateway: { ...empty_auth.gateway, authToken: 'private-gateway-token' },
    };
    const authenticated_app = createGatewayApp(authenticated).app;
    expect((await authenticated_app.request('/health')).status).toBe(401);
    expect((await authenticated_app.request('/v1/models')).status).toBe(401);
    const allowed = await authenticated_app.request('/health', {
      headers: {
        origin: 'https://trusted-client.example.invalid',
        authorization: 'Bearer private-gateway-token',
      },
    });
    expect(allowed.status).toBe(200);
    expect(allowed.headers.get('access-control-allow-origin')).toBe(
      'https://trusted-client.example.invalid',
    );

    const anthropic_auth = await authenticated_app.request('/v1/messages', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: '{}',
    });
    expect(anthropic_auth.status).toBe(401);
    expect((await anthropic_auth.json() as Record<string, unknown>).type).toBe('error');
  });

  it('keeps Anthropic errors in the top-level Anthropic envelope', async () => {
    const cfg: AppConfig = {
      version: 1,
      gateway: { host: '127.0.0.1', port: 8787, authToken: '', logLevel: 'error' },
      providers: [], routes: [], profiles: [], agents: [],
    };
    const response = await createGatewayApp(cfg).app.request('/v1/messages', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({
        model: 'CANARY_PROMPT_UNIQUE_91',
        max_tokens: 32,
        messages: [{ role: 'user', content: 'CANARY_RESPONSE_UNIQUE_92' }],
      }),
    });
    const body = await response.json() as Record<string, any>;
    expect(response.status).toBe(404);
    expect(body.type).toBe('error');
    // Rust parity: router failures on the Anthropic surface always carry
    // api_error regardless of status.
    expect(body.error.type).toBe('api_error');
    expect(body.error.message).toBeTypeOf('string');
    expect(body.error.error).toBeUndefined();
    assert_private_canaries_absent(JSON.stringify(body));

    const stream_response = await createGatewayApp(cfg).app.request('/v1/messages', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({
        model: 'CANARY_PROMPT_UNIQUE_91',
        max_tokens: 32,
        stream: true,
        messages: [{ role: 'user', content: 'CANARY_RESPONSE_UNIQUE_92' }],
      }),
    });
    const stream_body = await stream_response.text();
    // Pre-commit probe: a stream setup failure is the JSON error envelope
    // with the real status (Rust parity), not a committed 200 SSE stream
    // that only carries an error event.
    expect(stream_response.status).toBe(404);
    expect(stream_body).not.toContain('event: error');
    expect(stream_body).not.toContain('data: [DONE]');
    const stream_error = JSON.parse(stream_body) as Record<string, any>;
    expect(stream_error.type).toBe('error');
    expect(stream_error.error.type).toBe('api_error');
    assert_private_canaries_absent(stream_body);
  });

  it('never invokes a fallback provider after the first stream chunk', async () => {
    const first_provider = { ...provider_config(), id: 'first', type: 'mock' as const, models: ['upstream-a'] };
    const second_provider = { ...provider_config(), id: 'second', type: 'mock' as const, models: ['upstream-b'] };
    const cfg: AppConfig = {
      version: 1,
      gateway: { host: '127.0.0.1', port: 8787, authToken: '', logLevel: 'error' },
      providers: [first_provider, second_provider],
      routes: [{
        alias: 'client-model',
        description: 'stream ownership regression',
        hops: [
          { provider: 'first', model: 'upstream-a' },
          { provider: 'second', model: 'upstream-b' },
        ],
      }],
      profiles: [], agents: [],
    };
    const { router } = createGatewayApp(cfg);
    let fallback_calls = 0;
    const first = router.getProvider('first')!;
    const second = router.getProvider('second')!;
    first.stream = async function* () {
      yield stream_chunk('first-chunk');
      throw makeProviderError('upstream_stream_error', {
        status: 502,
        retriable: true,
        stage: 'stream_read',
      });
    };
    second.stream = async function* () {
      fallback_calls += 1;
      yield stream_chunk('must-not-appear');
    };

    const stream = router.stream({
      model: 'client-model',
      messages: [{ role: 'user', content: 'hello' }],
      stream: true,
    });
    const first_result = await stream.next();
    expect(first_result.value.choices[0].delta.content).toBe('first-chunk');
    await expect(stream.next()).rejects.toMatchObject({ code: 'upstream_stream_error' });
    expect(fallback_calls).toBe(0);
  });

  it('uses a process-local keyed reference instead of a public digest', async () => {
    const value = 'private-provider-identity';
    const first = safeLogRef(value);
    expect(first).toBe(safeLogRef(value));
    expect(first).toMatch(/^ref_[a-f0-9]{12}$/);
    vi.resetModules();
    const fresh_module = await import('../src/util/log.js');
    expect(fresh_module.safeLogRef(value)).not.toBe(first);
  });

  it('does not retain an upstream response body or private provider identity', async () => {
    vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      new Response(
        `CANARY_RESPONSE_UNIQUE_92 ${PRIVATE_CANARIES.slice(2).join(' ')}`,
        { status: 502 },
      ),
    );
    const provider = new OpenAIProvider(provider_config());

    const error = await provider.complete({
      model: 'model-CANARY_RESPONSE_UNIQUE_92',
      messages: [{ role: 'user', content: 'CANARY_PROMPT_UNIQUE_91' }],
    }).catch((value: unknown) => value as Error & Record<string, unknown>);

    const rendered = JSON.stringify({
      message: error.message,
      code: error.code,
      stage: error.stage,
      response_bytes: error.responseBytes,
    });
    assert_private_canaries_absent(rendered);
    expect(error.code).toBe('upstream_http_error');
    expect(error.stage).toBe('response_status');
    expect(error.responseBytes).toBeGreaterThan(0);
  });

  it('returns stable HTTP and SSE error envelopes without echoing an unknown model', async () => {
    const cfg: AppConfig = {
      version: 1,
      gateway: { host: '127.0.0.1', port: 8787, authToken: '', logLevel: 'error' },
      providers: [],
      routes: [],
      profiles: [],
      agents: [],
    };
    const { app } = createGatewayApp(cfg);
    const request = {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({
        model: 'CANARY_PROMPT_UNIQUE_91',
        messages: [{ role: 'user', content: 'CANARY_RESPONSE_UNIQUE_92' }],
      }),
    };
    const response = await app.request('/v1/chat/completions', request);
    const response_text = await response.text();
    assert_private_canaries_absent(response_text);
    expect(response.status).toBe(404);
    expect(response_text).toContain('no_route');

    const stream_response = await app.request('/v1/chat/completions', {
      ...request,
      body: JSON.stringify({
        model: 'CANARY_PROMPT_UNIQUE_91',
        messages: [{ role: 'user', content: 'CANARY_RESPONSE_UNIQUE_92' }],
        stream: true,
      }),
    });
    const stream_text = await stream_response.text();
    assert_private_canaries_absent(stream_text);
    expect(stream_text).toContain('no_route');
  });

  it('logs malformed SSE metadata without the malformed payload or private tag', async () => {
    setLogLevel('debug');
    let output = '';
    vi.spyOn(process.stdout, 'write').mockImplementation((chunk: string | Uint8Array) => {
      output += String(chunk);
      return true;
    });
    const malformed = `data: {"bad":"${PRIVATE_CANARIES.join('|')}"}\n\n`;
    const body = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(new TextEncoder().encode(malformed));
        controller.close();
      },
    });

    for await (const _item of parseSSE(body, 'provider-CANARY_PROMPT_UNIQUE_91')) {
      // The malformed event is intentionally skipped.
    }

    assert_private_canaries_absent(output);
    expect(output).toContain('sse_parse_failed');
    expect(output).toContain('data_bytes');
  });

  it('drops non-allowlisted and nested logger metadata', () => {
    let output = '';
    vi.spyOn(process.stderr, 'write').mockImplementation((chunk: string | Uint8Array) => {
      output += String(chunk);
      return true;
    });

    logger.warn('router', 'provider_failed', {
      status: 502,
      retriable: true,
      code: PRIVATE_CANARIES[0],
      stage: PRIVATE_CANARIES[1],
      provider_ref: PRIVATE_CANARIES[5],
      prompt: PRIVATE_CANARIES[0],
      nested: { response: PRIVATE_CANARIES[1] },
      url: PRIVATE_CANARIES[4],
    });

    assert_private_canaries_absent(output);
    expect(output).toContain('provider_failed');
    expect(output).toContain('"status":502');
    expect(output).toContain('"retriable":true');
  });
});

function stream_chunk(content: string): OAIStreamChunk {
  return {
    id: 'chunk',
    object: 'chat.completion.chunk',
    created: 1,
    model: 'upstream-a',
    choices: [{ index: 0, delta: { content }, finish_reason: null }],
  };
}
