import { afterEach, describe, expect, it, vi } from 'vitest';
import { createGatewayApp } from '../src/gateway/server.js';
import type { AppConfig } from '../src/config/schema.js';

function config(): AppConfig {
  return {
    version: 1,
    gateway: { host: '127.0.0.1', port: 8787, authToken: '', logLevel: 'error' },
    providers: [{
      id: 'native', name: 'native', type: 'anthropic', baseURL: 'https://fixture.invalid',
      apiKey: '', enabled: true, models: ['upstream'], timeoutMs: 1000, weight: 1,
      circuitBreaker: { failureThreshold: 3, cooldownMs: 1000, halfOpenRetries: 1, successThreshold: 1 },
    }],
    routes: [{ alias: 'alias', hops: [{ provider: 'native', model: 'upstream' }], description: '' }],
    profiles: [], agents: [],
  };
}

const request_body = {
  model: 'alias', max_tokens: 128,
  system: [{ type: 'text', text: 'fixture', cache_control: { type: 'ephemeral' } }],
  messages: [{ role: 'assistant', content: [
    { type: 'thinking', thinking: 'fixture thought', signature: 'opaque_signature' },
    { type: 'tool_use', id: 'tool_1', name: 'lookup', input: { count: 1 } },
  ] }, { role: 'user', content: [
    { type: 'tool_result', tool_use_id: 'tool_1', content: 'fixture result' },
  ] }],
  tools: [{ name: 'lookup', input_schema: { type: 'object' } }],
  thinking: { type: 'enabled', budget_tokens: 32 },
};
const response_body = {
  id: 'msg_fixture', type: 'message', role: 'assistant', model: 'upstream',
  content: [
    { type: 'redacted_thinking', data: 'opaque_data' },
    { type: 'thinking', thinking: 'fixture thought', signature: 'opaque_signature' },
    { type: 'tool_use', id: 'tool_2', name: 'lookup', input: { count: 2 } },
  ],
  stop_reason: 'tool_use', stop_sequence: null,
  usage: { input_tokens: 11, output_tokens: 23, cache_read_input_tokens: 7 },
};

afterEach(() => vi.unstubAllGlobals());

describe('Anthropic source envelope relay', () => {
  it('rejects event-name framing injection before committing SSE', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => new Response('data: ' + JSON.stringify({
      type: 'ping\r\n\r\ndata: injected\r\n\r\n',
    }) + '\n\n')));
    const { app } = createGatewayApp(config());
    const response = await app.request('/v1/messages', {
      method: 'POST', headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ ...request_body, stream: true }),
    });
    expect(response.status).toBeGreaterThanOrEqual(500);
    expect(await response.text()).not.toContain('injected');
  });

  it('reports missing message_stop rather than accepting a stop_reason as completion', async () => {
    const events = [
      { type: 'message_start', message: { ...response_body, content: [] } },
      { type: 'message_delta', delta: { stop_reason: 'end_turn' } },
    ];
    vi.stubGlobal('fetch', vi.fn(async () => new Response(
      events.map((event) => `data: ${JSON.stringify(event)}\n\n`).join(''),
    )));
    const { app, router } = createGatewayApp(config());
    const response = await app.request('/v1/messages', {
      method: 'POST', headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ ...request_body, stream: true }),
    });
    expect(await response.text()).toContain('event: error');
    expect(router.getProvider('native')!.breaker.snapshot().failures).toBe(1);
  });

  it('rejects an HTTP-200 error body instead of relaying a successful malformed message', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => Response.json({ error: { message: 'fixture_private' } })));
    const { app } = createGatewayApp(config());
    const response = await app.request('/v1/messages', {
      method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(request_body),
    });
    expect(response.status).toBeGreaterThanOrEqual(500);
    expect(await response.text()).not.toContain('fixture_private');
  });

  it('preserves opaque blocks and tools in both directions, rewriting only model and stream', async () => {
    let sent: unknown;
    vi.stubGlobal('fetch', vi.fn(async (_url, init) => {
      sent = JSON.parse(init.body);
      return Response.json(response_body);
    }));
    const { app } = createGatewayApp(config());
    const response = await app.request('/v1/messages', {
      method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(request_body),
    });
    expect(response.status).toBe(200);
    expect(sent).toEqual({ ...request_body, model: 'upstream', stream: false });
    expect(await response.json()).toEqual({ ...response_body, model: 'alias' });
  });

  it('preserves event order, thinking signatures, tools and native usage', async () => {
    const events = [
      { type: 'message_start', message: { ...response_body, content: [], stop_reason: null } },
      { type: 'content_block_start', index: 0, content_block: { type: 'thinking', thinking: '' } },
      { type: 'content_block_delta', index: 0, delta: { type: 'thinking_delta', thinking: 'fixture' } },
      { type: 'content_block_delta', index: 0, delta: { type: 'signature_delta', signature: 'opaque_signature' } },
      { type: 'content_block_stop', index: 0 },
      { type: 'content_block_start', index: 1, content_block: response_body.content[2] },
      { type: 'content_block_delta', index: 1, delta: { type: 'input_json_delta', partial_json: '{}' } },
      { type: 'content_block_stop', index: 1 },
      { type: 'message_delta', delta: { stop_reason: 'tool_use' }, usage: { output_tokens: 23 } },
      { type: 'message_stop' },
    ];
    vi.stubGlobal('fetch', vi.fn(async () => new Response(
      events.map((event) => `event: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`).join(''),
    )));
    const { app } = createGatewayApp(config());
    const response = await app.request('/v1/messages', {
      method: 'POST', headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ ...request_body, stream: true }),
    });
    const text = await response.text();
    const observed = text.split('\n').filter((line) => line.startsWith('data: '))
      .map((line) => JSON.parse(line.slice(6)));
    expect(observed).toEqual(events.map((event) => event.type === 'message_start'
      ? { ...event, message: { ...event.message, model: 'alias' } } : event));
    expect(text).not.toContain('[DONE]');
  });

  it('never serializes native source envelopes into OpenAI responses', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => Response.json(response_body)));
    const { app } = createGatewayApp(config());
    const response = await app.request('/v1/chat/completions', {
      method: 'POST', headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ model: 'alias', messages: [{ role: 'user', content: 'fixture' }] }),
    });
    const text = await response.text();
    expect(JSON.parse(text).object).toBe('chat.completion');
    for (const forbidden of ['opaque_signature', 'redacted_thinking', 'source_anthropic', 'thinking_delta']) {
      expect(text).not.toContain(forbidden);
    }
  });

  it('normalizes native error events instead of relaying private upstream errors', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => new Response('data: ' + JSON.stringify({
      type: 'error', error: { type: 'overloaded_error', message: 'https://secret.invalid/key?token=private' },
    }) + '\n\n')));
    const { app } = createGatewayApp(config());
    const response = await app.request('/v1/messages', {
      method: 'POST', headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ ...request_body, stream: true }),
    });
    expect(response.status).toBeGreaterThanOrEqual(500);
    expect(await response.text()).not.toContain('secret.invalid');
  });
});
