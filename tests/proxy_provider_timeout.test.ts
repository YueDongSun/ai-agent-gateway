import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { listProxyProviders, saveProxyProvider } from '../ui/src/lib/api/proxy.js';
import { installMockBackend } from '../ui/src/lib/mock/backend.js';

vi.mock('../ui/src/lib/build_info.js', () => ({
  app_version: 'test',
  build_channel: 'demo',
  is_demo_build: true,
}));

beforeEach(() => {
  vi.stubGlobal('window', {});
  installMockBackend();
});

afterEach(() => vi.unstubAllGlobals());

describe('demo proxy provider timeout contract', () => {
  it.each([0, 999])('rejects timeoutMs=%s without storing the row', async (timeout_ms) => {
    const provider = {
      id: 'timeout_fixture',
      name: 'timeout fixture',
      type: 'anthropic',
      apiKey: '',
      baseUrl: 'https://upstream.example/v1',
      models: ['claude-sonnet-5'],
      enabled: true,
      timeoutMs: timeout_ms,
      weight: 1,
      circuitBreaker: { failureThreshold: 3, cooldownMs: 5_000, halfOpenRetries: 1, successThreshold: 2 },
      modelMapping: {},
      extraHeaders: {},
      autoTrackModels: true,
    };
    await expect(saveProxyProvider(provider)).rejects.toThrow(
      'AAG_PROXY_PROVIDER_TIMEOUT_INVALID',
    );
    expect(
      (await listProxyProviders()).some((stored) => stored.id === provider.id),
    ).toBe(false);
  });

  it('still accepts a contract-minimum timeout', async () => {
    const provider = {
      id: 'timeout_ok_fixture',
      name: 'timeout ok fixture',
      type: 'anthropic',
      apiKey: '',
      baseUrl: 'https://upstream.example/v1',
      models: ['claude-sonnet-5'],
      enabled: true,
      timeoutMs: 1000,
      weight: 1,
      circuitBreaker: { failureThreshold: 3, cooldownMs: 5_000, halfOpenRetries: 1, successThreshold: 2 },
      modelMapping: {},
      extraHeaders: {},
      autoTrackModels: true,
    };
    await expect(saveProxyProvider(provider)).resolves.toBeUndefined();
  });
});
