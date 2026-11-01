/**
 * HTTP-edge error-conversion parity with the Rust proxy.
 *
 * The Rust proxy (src-tauri/src/proxy/server.rs) is the product anchor for
 * error wire shapes; the Node gateway is its headless-compatible surface and
 * must translate the same failures the same way:
 *   - server-side failures carry type `proxy_error`, never `server_error`;
 *   - router/provider failures on /v1/messages always carry `api_error`,
 *     regardless of status;
 *   - a stream setup failure returns the JSON error envelope with its real
 *     status before the 200/SSE commit, instead of a committed stream that
 *     only carries an error event.
 */
import { describe, it, expect } from 'vitest';
import { createGatewayApp } from '../src/gateway/server.js';
import { publicProviderErrorMessage } from '../src/gateway/canonical.js';
import type { AppConfig } from '../src/config/schema.js';

/** Route points at a provider that does not exist: no hop can run, so the
 *  router fails with all_hops_exhausted status 503 (a server-side error). */
function cfg_with_dead_route(): AppConfig {
  return {
    version: 1,
    gateway: { host: '127.0.0.1', port: 8787, authToken: '', logLevel: 'error' },
    providers: [],
    routes: [{ alias: 'm', hops: [{ provider: 'missing', model: 'm' }], description: 'parity' }],
    profiles: [],
    agents: [],
  };
}

const JSON_HEADERS = { 'content-type': 'application/json' };

const request = {
  method: 'POST',
  headers: JSON_HEADERS,
  body: JSON.stringify({ model: 'm', messages: [{ role: 'user', content: 'hi' }] }),
};

describe('HTTP error conversion parity', () => {
  it('maps the invalid_request code to the Rust message text', () => {
    // Mirrors public_error_message in src-tauri/src/proxy/canonical.rs.
    expect(publicProviderErrorMessage('invalid_request')).toBe('The request is invalid.');
  });

  it('identifies server-side failures as proxy_error on the OpenAI surface', async () => {
    const { app } = createGatewayApp(cfg_with_dead_route());
    const res = await app.request('/v1/chat/completions', request);
    expect(res.status).toBe(503);
    const body = (await res.json()) as Record<string, any>;
    expect(body.error.type).toBe('proxy_error');
    expect(body.error.type).not.toBe('server_error');
    expect(body.error.code).toBe('all_hops_exhausted');
    expect(body.error.message).toBe('No configured provider completed the request.');
    expect(typeof body.error.correlation_id).toBe('string');
  });

  it('returns a stream setup failure as JSON before the SSE commit', async () => {
    const { app } = createGatewayApp(cfg_with_dead_route());
    const res = await app.request('/v1/chat/completions', {
      ...request,
      body: JSON.stringify({ model: 'm', messages: [{ role: 'user', content: 'hi' }], stream: true }),
    });
    expect(res.status).toBe(503);
    expect(res.headers.get('content-type')).not.toMatch(/text\/event-stream/);
    const text = await res.text();
    expect(text).not.toContain('data: ');
    const body = JSON.parse(text) as Record<string, any>;
    expect(body.error.type).toBe('proxy_error');
  });

  it('carries api_error for router failures on the Anthropic surface (non-stream)', async () => {
    const { app } = createGatewayApp(cfg_with_dead_route());
    const res = await app.request('/v1/messages', {
      ...request,
      body: JSON.stringify({ model: 'm', max_tokens: 16, messages: [{ role: 'user', content: 'hi' }] }),
    });
    expect(res.status).toBe(503);
    const body = (await res.json()) as Record<string, any>;
    expect(body.type).toBe('error');
    expect(body.error.type).toBe('api_error');
    expect(typeof body.request_id).toBe('string');
  });

  it('returns an Anthropic stream setup failure as JSON before the SSE commit', async () => {
    const { app } = createGatewayApp(cfg_with_dead_route());
    const res = await app.request('/v1/messages', {
      ...request,
      body: JSON.stringify({
        model: 'm',
        max_tokens: 16,
        stream: true,
        messages: [{ role: 'user', content: 'hi' }],
      }),
    });
    expect(res.status).toBe(503);
    expect(res.headers.get('content-type')).not.toMatch(/text\/event-stream/);
    const text = await res.text();
    expect(text).not.toContain('event: message');
    expect(text).not.toContain('data: [DONE]');
    const body = JSON.parse(text) as Record<string, any>;
    expect(body.type).toBe('error');
    expect(body.error.type).toBe('api_error');
  });
});
