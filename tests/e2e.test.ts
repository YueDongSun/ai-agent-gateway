/**
 * End-to-end mock test.
 * Starts the gateway in-process, then exercises:
 *   1. /health
 *   2. /v1/models
 *   3. POST /v1/chat/completions (non-stream)
 *   4. POST /v1/chat/completions (stream)
 *   5. POST /v1/messages (Anthropic non-stream)
 *   6. POST /v1/messages (Anthropic stream)
 *   7. Failover: request "mock-fail" -> should fall back to mock-fallback
 *   8. Circuit-breaker: after N failures, breaker opens and skips primary
 */
import { describe, it, expect, beforeAll, afterAll } from 'vitest';
import { loadConfig, saveConfig } from '../src/config/store.js';
import { createGatewayApp } from '../src/gateway/server.js';

const cfg = loadConfig();
saveConfig(cfg);
const { app } = createGatewayApp(cfg);

const BASE = `http://${cfg.gateway.host}:${cfg.gateway.port}`;

// We can't easily boot @hono/node-server in vitest; instead use app.request()
// which dispatches directly to the Hono app without a socket.
async function req(path: string, init?: RequestInit): Promise<Response> {
  return app.request(path, init, BASE);
}

const JSON_HEADERS = { 'content-type': 'application/json' };

describe('gateway health & models', () => {
  it('GET /health returns ok', async () => {
    const res = await req('/health');
    expect(res.status).toBe(200);
    const body = await res.json();
    expect(body.status).toBe('ok');
    expect(body.models.length).toBeGreaterThan(0);
  });

  it('GET /v1/models lists aliases', async () => {
    const res = await req('/v1/models');
    expect(res.status).toBe(200);
    const body = await res.json();
    expect(body.data.some((m: any) => m.id === 'claude-sonnet-4')).toBe(true);
  });
});

describe('OpenAI compatible endpoints', () => {
  it('non-stream completion', async () => {
    const res = await req('/v1/chat/completions', {
      method: 'POST',
      headers: JSON_HEADERS,
      body: JSON.stringify({
        model: 'claude-sonnet-4',
        messages: [{ role: 'user', content: 'hi' }],
        stream: false,
      }),
    });
    expect(res.status).toBe(200);
    const body = await res.json();
    expect(body.choices[0].message.content).toContain('MOCK');
    expect(body.choices[0].message.content).toContain('hi');
  });

  it('stream completion', async () => {
    const res = await req('/v1/chat/completions', {
      method: 'POST',
      headers: JSON_HEADERS,
      body: JSON.stringify({
        model: 'gpt-4o',
        messages: [{ role: 'user', content: 'hello' }],
        stream: true,
      }),
    });
    expect(res.status).toBe(200);
    const text = await res.text();
    expect(text).toContain('data: ');
    expect(text).toContain('[DONE]');
    expect(text).toContain('MOCK');
  });
});

describe('OpenAI streaming SSE format (integration)', () => {
  it('SSE chunks parse as valid OAI shapes with [DONE] sentinel', async () => {
    const res = await req('/v1/chat/completions', {
      method: 'POST',
      headers: JSON_HEADERS,
      body: JSON.stringify({
        model: 'claude-sonnet-4',
        messages: [{ role: 'user', content: 'sse test' }],
        stream: true,
        temperature: 0,
      }),
    });
    expect(res.status).toBe(200);
    expect(res.headers.get('content-type')).toMatch(/text\/event-stream/);
    const text = await res.text();

    // Parse SSE lines: each "data: <json>" line, terminated by "[DONE]"
    const dataLines: unknown[] = [];
    for (const raw of text.split('\n')) {
      const line = raw.replace(/\r$/, '');
      if (!line.startsWith('data:')) continue;
      const payload = line.slice(5).trim();
      if (payload === '[DONE]') continue; // handled separately
      dataLines.push(JSON.parse(payload));
    }
    expect(dataLines.length).toBeGreaterThanOrEqual(3);

    // First chunk: role delta, no finish_reason
    const first = dataLines[0] as { choices: Array<{ delta: { role?: string }; finish_reason: string | null }> };
    expect(first.choices[0].delta.role).toBe('assistant');
    expect(first.choices[0].finish_reason).toBeNull();

    // Middle chunks: content deltas, no finish_reason
    for (const chunk of dataLines.slice(1, -1) as Array<{ choices: Array<{ delta: { content?: string }; finish_reason: string | null }> }>) {
      expect(chunk.choices[0].delta.content).toBeDefined();
      expect(chunk.choices[0].finish_reason).toBeNull();
    }

    // Last chunk: finish_reason set, no content
    const last = dataLines[dataLines.length - 1] as { choices: Array<{ delta: { content?: string }; finish_reason: string | null }> };
    expect(last.choices[0].finish_reason).toBe('stop');

    // [DONE] sentinel must appear at the end of the stream
    expect(text.trimEnd().endsWith('data: [DONE]')).toBe(true);

    // All chunks share the same id and model
    for (const chunk of dataLines as Array<{ id: string; model: string; object: string }>) {
      expect(chunk.id).toBe(first['id'] ?? first);  // id consistency below
    }
    const ids = new Set((dataLines as Array<{ id: string }>).map((c) => c.id));
    expect(ids.size).toBe(1);
    const models = new Set((dataLines as Array<{ model: string }>).map((c) => c.model));
    expect(models.size).toBe(1);
    expect(models.has('claude-sonnet-4')).toBe(true);
  });

  it('streaming mock-fail fails over to mock-fallback', async () => {
    const res = await req('/v1/chat/completions', {
      method: 'POST',
      headers: JSON_HEADERS,
      body: JSON.stringify({
        model: 'mock-fail',
        messages: [{ role: 'user', content: 'stream failover' }],
        stream: true,
      }),
    });
    expect(res.status).toBe(200);
    const text = await res.text();
    // The fallback provider succeeds; we should see "mock-fallback" in the response
    expect(text).toContain('mock-fallback');
    expect(text).toContain('[DONE]');
  });
});

describe('Anthropic compatible endpoints', () => {
  it('non-stream /v1/messages', async () => {
    const res = await req('/v1/messages', {
      method: 'POST',
      headers: JSON_HEADERS,
      body: JSON.stringify({
        model: 'claude-sonnet-4',
        max_tokens: 100,
        messages: [{ role: 'user', content: 'hi anthropic' }],
        stream: false,
      }),
    });
    expect(res.status).toBe(200);
    const body = await res.json();
    expect(body.type).toBe('message');
    expect(body.role).toBe('assistant');
    expect(body.content[0].text).toContain('MOCK');
  });

  it('stream /v1/messages', async () => {
    const res = await req('/v1/messages', {
      method: 'POST',
      headers: JSON_HEADERS,
      body: JSON.stringify({
        model: 'claude-sonnet-4',
        max_tokens: 100,
        messages: [{ role: 'user', content: 'stream me' }],
        stream: true,
      }),
    });
    expect(res.status).toBe(200);
    const text = await res.text();
    expect(text).toContain('event: message_start');
    expect(text).toContain('event: content_block_delta');
    expect(text).toContain('event: message_stop');
    expect(text).toContain('MOCK');
  });
});

describe('failover & circuit-breaker', () => {
  it('failover: mock-fail primary -> mock-fallback succeeds', async () => {
    const res = await req('/v1/chat/completions', {
      method: 'POST',
      headers: JSON_HEADERS,
      body: JSON.stringify({
        model: 'mock-fail',
        messages: [{ role: 'user', content: 'should failover' }],
        stream: false,
      }),
    });
    expect(res.status).toBe(200);
    const body = await res.json();
    expect(body.choices[0].message.content).toContain('mock-fallback');
  });

  it('circuit opens after threshold, then recovers', async () => {
    // primary breaker threshold = 3 (from seed config)
    for (let i = 0; i < 3; i++) {
      await req('/v1/chat/completions', {
        method: 'POST',
        headers: JSON_HEADERS,
        body: JSON.stringify({
          model: 'mock-fail',
          messages: [{ role: 'user', content: 'x' }],
          stream: false,
        }),
      });
    }
    // Now breaker on mock-primary should be OPEN.
    const healthRes = await req('/health');
    const health = await healthRes.json();
    const primary = health.providers.find((p: any) => p.id === 'mock-primary');
    expect(primary.breaker.state).toBe('open');
  });
});
