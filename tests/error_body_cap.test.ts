import { afterEach, describe, expect, it, vi } from 'vitest';
import type { ChatCompletionRequest, ProviderConfig } from '../src/config/schema.js';
import {
  max_error_body_bytes,
  read_capped_error_body,
  upstream_http_error,
} from '../src/gateway/providers/error_body.js';
import { AnthropicProvider } from '../src/gateway/providers/anthropic.js';
import { OpenAIProvider } from '../src/gateway/providers/openai.js';

const oversized_body = 'invalid_grant ' + 'x'.repeat(256 * 1024);

function fixture_config(type: ProviderConfig['type']): ProviderConfig {
  return {
    id: 'prov_error_body',
    name: 'error_body_fixture',
    type,
    apiKey: 'fixture_key',
    baseURL: 'http://127.0.0.1:9/v1',
    models: ['fixture_model'],
    timeoutMs: 10_000,
    enabled: true,
    circuitBreaker: {},
    weight: 1,
  };
}

const request: ChatCompletionRequest = {
  model: 'fixture_model',
  messages: [{ role: 'user', content: 'fixture' }],
};

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('read_capped_error_body', () => {
  it('caps a 256 KiB error body at the 64 KiB limit and reports observed bytes', async () => {
    const capped = await read_capped_error_body(new Response(oversized_body, { status: 502 }));
    expect(capped.response_bytes).toBe(max_error_body_bytes);
    expect(capped.text.length).toBe(max_error_body_bytes);
    expect(capped.text.startsWith('invalid_grant')).toBe(true);
  });

  it('reports the exact byte count for a small error body', async () => {
    const body = '{"error":{"message":"please sign in again"}}';
    const capped = await read_capped_error_body(new Response(body, { status: 401 }));
    expect(capped.response_bytes).toBe(new TextEncoder().encode(body).length);
    expect(capped.text).toBe(body);
  });

  it('lossy-decodes a multibyte character sliced at the cap boundary', async () => {
    // The cap cuts one byte into the first 4-byte emoji sequence.
    const body = 'a'.repeat(max_error_body_bytes - 1) + '🎉'.repeat(64);
    const capped = await read_capped_error_body(new Response(body, { status: 500 }));
    expect(capped.response_bytes).toBe(max_error_body_bytes);
    expect(capped.text.endsWith('�')).toBe(true);
  });

  it('cancels the unread remainder of the upstream body', async () => {
    let cancelled = false;
    const stream = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(new TextEncoder().encode('invalid_grant '));
        controller.enqueue(new TextEncoder().encode('x'.repeat(256 * 1024)));
      },
      cancel() {
        cancelled = true;
      },
    });
    const capped = await read_capped_error_body(new Response(stream, { status: 502 }));
    expect(cancelled).toBe(true);
    expect(capped.response_bytes).toBe(max_error_body_bytes);
  });

  it('returns an empty read for a response without a body', async () => {
    const capped = await read_capped_error_body(new Response(null, { status: 502 }));
    expect(capped).toEqual({ text: '', response_bytes: 0 });
  });

  it('keeps the observed prefix when the body read fails mid-stream', async () => {
    const prefix = 'invalid_grant prefix';
    let pull_count = 0;
    const stream = new ReadableStream<Uint8Array>({
      pull(controller) {
        pull_count += 1;
        if (pull_count === 1) controller.enqueue(new TextEncoder().encode(prefix));
        else controller.error(new Error('upstream_gone'));
      },
    });
    const capped = await read_capped_error_body(new Response(stream, { status: 502 }));
    expect(capped.response_bytes).toBe(prefix.length);
    expect(capped.text).toBe(prefix);
  });
});

describe('upstream_http_error', () => {
  it('classifies the 4xx from the capped text and reports the capped size', async () => {
    await expect(
      upstream_http_error(new Response(oversized_body, { status: 401 }), 'response_status'),
    ).rejects.toMatchObject({
      status: 401,
      retriable: false,
      stage: 'response_status',
      kind: 'auth_expired',
      responseBytes: max_error_body_bytes,
    });
  });

  it('does not classify a keyword outside the first 8 KiB scan window', async () => {
    const body = 'x'.repeat(9_000) + ' invalid_grant';
    await expect(
      upstream_http_error(new Response(body, { status: 401 }), 'response_status'),
    ).rejects.toMatchObject({ kind: undefined, responseBytes: 9_000 + ' invalid_grant'.length });
  });

  it('uses the fallback status when the platform reports status 0', async () => {
    // A status of 0 cannot be constructed through the Response constructor;
    // shadow it the same way an already-lost connection reports it.
    const response = new Response(null, { status: 599 });
    Object.defineProperty(response, 'status', { value: 0 });
    await expect(upstream_http_error(response, 'stream_status', 502)).rejects.toMatchObject({
      status: 502,
      retriable: true,
      stage: 'stream_status',
      responseBytes: 0,
    });
  });
});

describe('provider error paths use the capped read', () => {
  it('OpenAI complete() caps the error body and keeps the classification', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => new Response(oversized_body, { status: 401 })));
    const provider = new OpenAIProvider(fixture_config('openai'));
    const err = await provider.complete(request).catch((e) => e);
    expect(err).toMatchObject({
      status: 401,
      retriable: false,
      kind: 'auth_expired',
      responseBytes: max_error_body_bytes,
    });
  });

  it('OpenAI stream() throws the capped stream_status error', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => new Response(oversized_body, { status: 502 })));
    const provider = new OpenAIProvider(fixture_config('openai'));
    const err = await provider.stream(request).next().then(
      () => { throw new Error('expected stream_status rejection'); },
      (e) => e,
    );
    expect(err).toMatchObject({
      status: 502,
      retriable: true,
      stage: 'stream_status',
      responseBytes: max_error_body_bytes,
    });
  });

  it('Anthropic complete() reports the exact size for a small error body', async () => {
    const body = '{"error":{"type":"authentication_error","message":"invalid_grant"}}';
    vi.stubGlobal('fetch', vi.fn(async () => new Response(body, { status: 401 })));
    const provider = new AnthropicProvider(fixture_config('anthropic'));
    const err = await provider.complete(request).catch((e) => e);
    expect(err).toMatchObject({
      status: 401,
      kind: 'auth_expired',
      responseBytes: new TextEncoder().encode(body).length,
    });
  });
});
