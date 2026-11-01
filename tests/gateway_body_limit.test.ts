/**
 * Request-body cap parity with the Rust proxy.
 *
 * `src-tauri/src/proxy/server.rs` wraps every JSON POST surface in
 * `DefaultBodyLimit::max(LARGE_REQUEST_BODY_BYTES)` and maps an overrun to
 * `400 invalid_request_error` through its `JsonRejection` catch-all. The Node
 * gateway must accept and reject exactly the same payloads with the same
 * wire shape, and must not buffer an oversized body whole while doing so.
 */
import { describe, it, expect } from 'vitest';
import { createGatewayApp, LARGE_REQUEST_BODY_BYTES } from '../src/gateway/server.js';
import type { AppConfig } from '../src/config/schema.js';

function cfg_without_routes(): AppConfig {
  return {
    version: 1,
    gateway: { host: '127.0.0.1', port: 8787, authToken: '', logLevel: 'error' },
    providers: [],
    routes: [],
    profiles: [],
    agents: [],
  };
}

const JSON_HEADERS = { 'content-type': 'application/json' };

function chat_body(min_content_bytes: number): string {
  return JSON.stringify({
    model: 'm',
    messages: [{ role: 'user', content: 'x'.repeat(min_content_bytes) }],
  });
}

function messages_body(min_content_bytes: number): string {
  return JSON.stringify({
    model: 'm',
    max_tokens: 16,
    messages: [{ role: 'user', content: 'x'.repeat(min_content_bytes) }],
  });
}

describe('request body cap parity', () => {
  it('pins the cap to the Rust LARGE_REQUEST_BODY_BYTES value', () => {
    // src-tauri/src/proxy/server.rs: `const LARGE_REQUEST_BODY_BYTES: usize = 32 * 1024 * 1024`.
    expect(LARGE_REQUEST_BODY_BYTES).toBe(32 * 1024 * 1024);
  });

  it('rejects an oversized OpenAI body with the invalid-request envelope', async () => {
    const { app } = createGatewayApp(cfg_without_routes(), { max_request_body_bytes: 64 });
    const res = await app.request('/v1/chat/completions', {
      method: 'POST',
      headers: JSON_HEADERS,
      body: chat_body(512),
    });
    expect(res.status).toBe(400);
    const body = (await res.json()) as Record<string, any>;
    expect(body.error.type).toBe('invalid_request_error');
    expect(body.error.message).toBe('request body too large');
    // The Anthropic envelope must not leak onto the OpenAI surface.
    expect(body.type).toBeUndefined();
  });

  it('rejects an oversized Anthropic body with the Anthropic envelope', async () => {
    const { app } = createGatewayApp(cfg_without_routes(), { max_request_body_bytes: 64 });
    const res = await app.request('/v1/messages', {
      method: 'POST',
      headers: JSON_HEADERS,
      body: messages_body(512),
    });
    expect(res.status).toBe(400);
    const body = (await res.json()) as Record<string, any>;
    expect(body.type).toBe('error');
    expect(body.error.type).toBe('invalid_request_error');
    expect(body.error.message).toBe('request body too large');
  });

  it('counts a chunked body with no content-length across reads', async () => {
    // A string body carries no content-length through app.request, so this
    // exercises the middleware's streaming byte counter rather than the
    // header pre-check; three individually-small chunks only exceed the cap
    // when summed.
    const { app } = createGatewayApp(cfg_without_routes(), { max_request_body_bytes: 200 });
    const encoder = new TextEncoder();
    const chunk = 'x'.repeat(90);
    const stream = new ReadableStream({
      start(controller) {
        for (let i = 0; i < 3; i += 1) controller.enqueue(encoder.encode(chunk));
        controller.close();
      },
    });
    const res = await app.request('/v1/chat/completions', {
      method: 'POST',
      headers: JSON_HEADERS,
      body: stream,
      duplex: 'half',
    } as RequestInit);
    expect(res.status).toBe(400);
    const body = (await res.json()) as Record<string, any>;
    expect(body.error.message).toBe('request body too large');
  });

  it('passes an under-cap body through to routing with the default cap', async () => {
    // The middleware re-wraps the request stream; the handler's c.req.json()
    // must still parse it and reach the router (404 no_route on an empty
    // route table proves the body survived the limit layer intact).
    const { app } = createGatewayApp(cfg_without_routes());
    const res = await app.request('/v1/chat/completions', {
      method: 'POST',
      headers: JSON_HEADERS,
      body: chat_body(8),
    });
    expect(res.status).toBe(404);
    const body = (await res.json()) as Record<string, any>;
    expect(body.error.code).toBe('no_route');
  });

  it('passes an under-cap Anthropic body through with an explicit small cap', async () => {
    const { app } = createGatewayApp(cfg_without_routes(), { max_request_body_bytes: 4096 });
    const res = await app.request('/v1/messages', {
      method: 'POST',
      headers: JSON_HEADERS,
      body: messages_body(8),
    });
    expect(res.status).toBe(404);
    const body = (await res.json()) as Record<string, any>;
    expect(body.type).toBe('error');
  });
});
