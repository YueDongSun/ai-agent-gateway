import { createServer, type Server } from 'node:http';
import { once } from 'node:events';
import { serve } from '@hono/node-server';
import { describe, expect, it } from 'vitest';
import type { AppConfig } from '../src/config/schema.js';
import { createGatewayApp } from '../src/gateway/server.js';

const close_deadline_ms = 2000;

function fixture_config(upstream_port: number): AppConfig {
  return {
    version: 1,
    gateway: { host: '127.0.0.1', port: 0, authToken: '', logLevel: 'error' },
    providers: [{
      id: 'fixture', name: 'fixture', type: 'openai', enabled: true,
      baseURL: `http://127.0.0.1:${upstream_port}`, apiKey: '', models: ['m'],
      timeoutMs: 1000, weight: 1,
      circuitBreaker: { failureThreshold: 3, cooldownMs: 1000, halfOpenRetries: 1, successThreshold: 1 },
    }],
    routes: [{ alias: 'm', hops: [{ provider: 'fixture', model: 'm' }], description: '' }],
    agents: [], profiles: [],
  };
}

function listening_port(server: Server): number {
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('fixture_not_listening');
  return address.port;
}

async function close_server(server: Server): Promise<void> {
  const closed = new Promise<void>((resolve, reject) => server.close((error) => error ? reject(error) : resolve()));
  server.closeAllConnections();
  await closed;
}

async function within_deadline(completion: Promise<void>): Promise<void> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    await Promise.race([completion, new Promise<never>((_resolve, reject) => {
      timer = setTimeout(() => reject(new Error('upstream_connection_not_released')), close_deadline_ms);
    })]);
  } finally {
    clearTimeout(timer);
  }
}

describe('gateway HTTP disconnect propagation', () => {
  for (const path of ['/v1/chat/completions', '/v1/messages']) {
    it(`${path} closes the real loopback upstream connection without penalizing its breaker`, async () => {
      let notify_closed: () => void = () => {};
      const upstream_closed = new Promise<void>((resolve) => { notify_closed = resolve; });
      const upstream = createServer((_request, response) => {
        response.on('close', notify_closed);
        response.writeHead(200, { 'content-type': 'text/event-stream' });
        response.write(`data: ${JSON.stringify({
          id: 'fixture', object: 'chat.completion.chunk', created: 1, model: 'm',
          choices: [{ index: 0, delta: { role: 'assistant', content: 'fixture' }, finish_reason: null }],
        })}\n\n`);
      });
      upstream.listen(0, '127.0.0.1');
      await once(upstream, 'listening');
      const { app, router } = createGatewayApp(fixture_config(listening_port(upstream)));
      const gateway = serve({ fetch: app.fetch, hostname: '127.0.0.1', port: 0 }) as Server;
      const controller = new AbortController();
      let reader: ReadableStreamDefaultReader<Uint8Array> | undefined;
      try {
        await once(gateway, 'listening');
        const response = await fetch(`http://127.0.0.1:${listening_port(gateway)}${path}`, {
          method: 'POST', headers: { 'content-type': 'application/json' },
          body: JSON.stringify({ model: 'm', stream: true, max_tokens: 16, messages: [{ role: 'user', content: 'fixture' }] }),
          signal: controller.signal,
        });
        expect(response.status).toBe(200);
        reader = response.body!.getReader();
        expect((await reader.read()).done).toBe(false);
        controller.abort();
        await within_deadline(upstream_closed);
        expect(router.getProvider('fixture')!.breaker.snapshot().failures).toBe(0);
      } finally {
        controller.abort();
        if (reader) {
          try { await reader.cancel(); } catch { /* Aborted fetch readers reject cancellation. */ }
          reader.releaseLock();
        }
        await close_server(gateway);
        await close_server(upstream);
      }
    });
  }
});
