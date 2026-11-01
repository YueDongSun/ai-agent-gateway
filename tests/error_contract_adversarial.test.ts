import { describe, expect, it } from 'vitest';
import { createGatewayApp } from '../src/gateway/server.js';
import {
  make_public_error_envelope,
  type Public_error_envelope,
} from '../src/gateway/error_contract.js';
import { makeProviderError } from '../src/gateway/canonical.js';
import type { AppConfig } from '../src/config/schema.js';

const JSON_HEADERS = { 'content-type': 'application/json' };

function config_with_mock(): AppConfig {
  return {
    version: 1,
    gateway: { host: '127.0.0.1', port: 8787, authToken: '', logLevel: 'error' },
    providers: [{
      id: 'mock',
      name: 'Mock',
      type: 'mock',
      apiKey: '',
      baseURL: '',
      models: ['mock-model'],
      timeoutMs: 1000,
      enabled: true,
      circuitBreaker: { failureThreshold: 3, cooldownMs: 1000, halfOpenRetries: 1, successThreshold: 1 },
      weight: 1,
    }],
    routes: [{ alias: 'prompt-model', hops: [{ provider: 'mock', model: 'mock-model' }], description: '' }],
    profiles: [],
    agents: [],
  };
}

function config_with_dead_route(): AppConfig {
  return {
    ...config_with_mock(),
    providers: [],
    routes: [{ alias: 'dead-model', hops: [{ provider: 'missing', model: 'dead-model' }], description: '' }],
  };
}

describe('unified backend error contract', () => {
  it('maps unknown model ids to invalid_model with a traceable request id', async () => {
    const { app } = createGatewayApp(config_with_mock());
    const res = await app.request('/v1/chat/completions', {
      method: 'POST',
      headers: JSON_HEADERS,
      body: JSON.stringify({ model: 'does-not-exist', messages: [{ role: 'user', content: 'hello' }] }),
    });

    expect(res.status).toBe(404);
    const body = await res.json() as Public_error_envelope;
    expect(body.error.code).toBe('invalid_model');
    expect(body.error.field).toBe('model');
    expect(body.error.retryable).toBe(false);
    expect(body.error.message).toMatch(/model/i);
    expect(body.request_id).toMatch(/^[0-9a-f-]{36}$/i);
    expect(body.upstream_model).toBeUndefined();
    expect(body.error.correlation_id).toBe(body.request_id);
    expect(body.error.retriable).toBe(false);
  });

  it('preserves dead-route semantics while returning the unified fields', async () => {
    const { app } = createGatewayApp(config_with_dead_route());
    const res = await app.request('/v1/chat/completions', {
      method: 'POST',
      headers: JSON_HEADERS,
      body: JSON.stringify({ model: 'dead-model', messages: [{ role: 'user', content: 'hello' }] }),
    });

    expect(res.status).toBe(503);
    const body = await res.json() as Public_error_envelope;
    expect(body.error.code).toBe('all_hops_exhausted');
    expect(body.error.field).toBeNull();
    expect(body.error.retryable).toBe(true);
    expect(body.request_id).toMatch(/^[0-9a-f-]{36}$/i);
    expect(body.upstream_model).toBe('dead-model');
  });

  it('sanitizes transport and 502 failures without exposing internal status details', () => {
    const error = makeProviderError('upstream_transport_error', {
      status: 502,
      retriable: true,
      stage: 'request_send',
    });
    const body = make_public_error_envelope(error, {
      status: 502,
      request_id: 'req_transport_fixture',
      upstream_model: 'model-x',
    });

    expect(body.error.code).toBe('upstream_unavailable');
    expect(body.error.message).toBe('The upstream model is temporarily unavailable, please retry shortly.');
    expect(body.error.retryable).toBe(true);
    expect(body.error.field).toBeNull();
    expect(body.request_id).toBe('req_transport_fixture');
    expect(body.upstream_model).toBe('model-x');
    expect(JSON.stringify(body)).not.toContain('upstream_transport_error:502');
  });

  it('returns the concrete model selected by the router', async () => {
    const { app } = createGatewayApp(config_with_mock());
    const res = await app.request('/v1/chat/completions', {
      method: 'POST',
      headers: JSON_HEADERS,
      body: JSON.stringify({ model: 'prompt-model', messages: [{ role: 'user', content: 'hello' }] }),
    });

    expect(res.status).toBe(200);
    const body = await res.json() as { resolved_model?: string; model?: string };
    expect(body.resolved_model).toBe('mock-model');
    expect(body.model).toBe('mock-model');
  });
});

