/**
 * `[1M]` context-window marker handling: the marker is a client hint, not
 * part of a model id, so the router must strip it before route resolution
 * and provider dispatch (Rust proxy parity, `strip_one_m_suffix` in
 * src-tauri/src/proxy/mod.rs).
 */
import { describe, it, expect } from 'vitest';
import { Router, strip_one_m_suffix } from '../src/gateway/router.js';
import { BaseProvider } from '../src/gateway/providers/base.js';
import type { AppConfig, ChatCompletionRequest, ProviderConfig } from '../src/config/schema.js';
import type { OAIChatResponse, OAIStreamChunk } from '../src/gateway/canonical.js';

function provider_config(id: string): ProviderConfig {
  return {
    id,
    name: id,
    type: 'mock',
    apiKey: '',
    baseURL: '',
    models: ['claude-fable-5'],
    timeoutMs: 60,
    enabled: true,
    circuitBreaker: {
      failureThreshold: 3,
      cooldownMs: 1000,
      halfOpenRetries: 1,
      successThreshold: 1,
    },
    weight: 1,
  };
}

function app_config(): AppConfig {
  return {
    version: 1,
    gateway: { host: '127.0.0.1', port: 8787, authToken: '', logLevel: 'warn' },
    providers: [],
    routes: [
      {
        alias: 'claude-fable-5',
        hops: [{ provider: 'a', model: 'claude-fable-5' }],
        description: 'test',
      },
    ],
    profiles: [],
    agents: [],
  };
}

class RecordingProvider extends BaseProvider {
  last_model: string | undefined;

  constructor(id: string) {
    super(provider_config(id));
  }

  async complete(req: ChatCompletionRequest): Promise<OAIChatResponse> {
    this.last_model = req.model;
    return {
      id: 'chatcmpl-fake',
      object: 'chat.completion',
      created: 1,
      model: req.model,
      choices: [
        { index: 0, message: { role: 'assistant', content: 'done' }, finish_reason: 'stop' },
      ],
      usage: { prompt_tokens: 1, completion_tokens: 1, total_tokens: 2 },
    };
  }

  async *stream(): AsyncGenerator<OAIStreamChunk> {
    throw new Error('streaming is not exercised by this suite');
  }
}

describe('router [1M] marker stripping', () => {
  it('strips the marker case-insensitively and tolerates whitespace', () => {
    expect(strip_one_m_suffix('claude-fable-5[1M]')).toBe('claude-fable-5');
    expect(strip_one_m_suffix('MiniMax-M3[1M]')).toBe('MiniMax-M3');
    expect(strip_one_m_suffix('deepseek-v4-pro[1m]')).toBe('deepseek-v4-pro');
    expect(strip_one_m_suffix('deepseek-v4-pro [1M] ')).toBe('deepseek-v4-pro');
    expect(strip_one_m_suffix('deepseek-v4-pro  [1m]')).toBe('deepseek-v4-pro');
    expect(strip_one_m_suffix('claude-fable-5')).toBe('claude-fable-5');
  });

  it('routes a marker-suffixed request to the plain model', async () => {
    const router = new Router(app_config());
    const provider = new RecordingProvider('a');
    const providers = (router as any).providers as Map<string, BaseProvider>;
    providers.set('a', provider);
    const resp = await router.complete({
      model: 'claude-fable-5[1M]',
      messages: [{ role: 'user', content: 'hi' }],
    } as ChatCompletionRequest);
    expect(provider.last_model).toBe('claude-fable-5');
    expect(resp.model).toBe('claude-fable-5');
  });
});
