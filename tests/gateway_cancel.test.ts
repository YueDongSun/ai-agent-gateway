import { describe, expect, it, vi } from 'vitest';
import { createGatewayApp } from '../src/gateway/server.js';
import type { AppConfig } from '../src/config/schema.js';
import type { OAIStreamChunk } from '../src/gateway/canonical.js';

const config: AppConfig = {
  version: 1,
  gateway: { host: '127.0.0.1', port: 8787, authToken: '', logLevel: 'error' },
  providers: [{ id: 'fixture', name: 'fixture', type: 'mock', baseURL: '', apiKey: '', enabled: true,
    models: ['m'], weight: 1, timeoutMs: 1000,
    circuitBreaker: { failureThreshold: 3, cooldownMs: 1000, halfOpenRetries: 1, successThreshold: 1 } }],
  routes: [{ alias: 'm', hops: [{ provider: 'fixture', model: 'm' }], description: '' }], profiles: [], agents: [],
};
const first: OAIStreamChunk = {
  id: 'fixture', object: 'chat.completion.chunk', created: 1, model: 'm',
  choices: [{ index: 0, delta: { role: 'assistant', content: 'fixture' }, finish_reason: null }],
};

async function cancel_and_release(reader: ReadableStreamDefaultReader<Uint8Array>): Promise<void> {
  try {
    await reader.cancel();
  } finally {
    reader.releaseLock();
  }
}

for (const path of ['/v1/chat/completions', '/v1/messages']) {
  describe(`${path} cancellation`, () => {
    for (const read_first of [false, true]) {
      it(`aborts upstream with pending reads (read_first=${read_first})`, async () => {
        const { app, router } = createGatewayApp(config);
        const provider = router.getProvider('fixture')!;
        let released = false;
        let attempt_signal: AbortSignal | undefined;
        let notify_released: () => void = () => {};
        const upstream_released = new Promise<void>((resolve) => { notify_released = resolve; });
        vi.spyOn(provider, 'stream').mockImplementation(async function* (_request, signal) {
          attempt_signal = signal;
          try {
            yield first;
            await new Promise<void>((resolve) => {
              if (signal!.aborted) resolve();
              else signal!.addEventListener('abort', () => resolve(), { once: true });
            });
          } finally {
            released = true;
            notify_released();
          }
        });
        const response = await app.request(path, {
          method: 'POST', headers: { 'content-type': 'application/json' },
          body: JSON.stringify({ model: 'm', stream: true, max_tokens: 16, messages: [{ role: 'user', content: 'fixture' }] }),
        });
        const reader = response.body!.getReader();
        try {
          if (read_first) await reader.read();
          await reader.cancel();
          await upstream_released;

          expect(attempt_signal?.aborted).toBe(true);
          expect(released).toBe(true);
          expect(provider.breaker.snapshot().failures).toBe(0);
        } finally {
          await cancel_and_release(reader);
        }
      });
    }

    it('aborts_upstream_while_the_first_probe_is_pending', async () => {
      const { app, router } = createGatewayApp(config);
      const provider = router.getProvider('fixture')!;
      let attempt_signal: AbortSignal | undefined;
      let notify_probe_started: () => void = () => {};
      const probe_started = new Promise<void>((resolve) => { notify_probe_started = resolve; });
      let notify_upstream_released: () => void = () => {};
      const upstream_released = new Promise<void>((resolve) => { notify_upstream_released = resolve; });
      vi.spyOn(provider, 'stream').mockImplementation(async function* (_request, signal) {
        attempt_signal = signal;
        notify_probe_started();
        try {
          await new Promise<void>((resolve) => {
            if (signal!.aborted) resolve();
            else signal!.addEventListener('abort', () => resolve(), { once: true });
          });
        } finally {
          notify_upstream_released();
        }
      });

      const request_controller = new AbortController();
      const request_promise = app.request(path, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ model: 'm', stream: true, max_tokens: 16, messages: [{ role: 'user', content: 'fixture' }] }),
        signal: request_controller.signal,
      });
      let response: Response | undefined;
      try {
        await probe_started;
        request_controller.abort();
        response = await request_promise;
        await upstream_released;

        expect(attempt_signal?.aborted).toBe(true);
        expect(provider.breaker.snapshot().failures).toBe(0);
      } finally {
        request_controller.abort();
        if (response?.body) await response.body.cancel();
      }
    });
  });
}
