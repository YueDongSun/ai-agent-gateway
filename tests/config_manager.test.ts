import { describe, expect, it } from 'vitest';
import { ConfigManager } from '../src/config/manager.js';
import { AppConfig, type AppConfig as AppConfigType } from '../src/config/schema.js';

const empty_config = (): AppConfigType =>
  AppConfig.parse({ gateway: {}, providers: [], routes: [], profiles: [], agents: [] });

describe('ConfigManager.ensureAgents', () => {
  it('seeds default agent targets without a runtime require (strict ESM)', () => {
    const cfg = empty_config();
    const mgr = new ConfigManager(cfg);

    const seeded = mgr.ensureAgents();
    expect(seeded.length).toBeGreaterThan(0);
    expect(seeded.every((agent) => agent.configPaths.length > 0)).toBe(true);

    // Idempotent: an already-populated list is returned untouched.
    expect(mgr.ensureAgents()).toBe(seeded);

    // The mutated document still validates at the persistence boundary.
    expect(() => AppConfig.parse(cfg)).not.toThrow();
  });
});

describe('ConfigManager provider and route mutations', () => {
  it('removing a provider also drops its hops and empty routes', () => {
    const mgr = new ConfigManager(empty_config());
    mgr.addProvider({ id: 'p1', name: 'Primary', type: 'mock', apiKey: 'mock', baseURL: 'mock://p', models: ['m1'], enabled: true, weight: 1 });
    mgr.addProvider({ id: 'p2', name: 'Secondary', type: 'mock', apiKey: 'mock', baseURL: 'mock://s', models: ['m1'], enabled: true, weight: 1 });
    mgr.addRoute('alias-1', [{ provider: 'p1', model: 'm1' }, { provider: 'p2', model: 'm1' }], 'both');
    mgr.addRoute('alias-2', [{ provider: 'p1', model: 'm1' }], 'only primary');

    mgr.removeProvider('p1');

    expect(mgr.raw.providers.map((p) => p.id)).toEqual(['p2']);
    const alias_1 = mgr.raw.routes.find((r) => r.alias === 'alias-1')!;
    expect(alias_1.hops).toEqual([{ provider: 'p2', model: 'm1' }]);
    expect(mgr.raw.routes.some((r) => r.alias === 'alias-2')).toBe(false);
    expect(() => AppConfig.parse(mgr.raw)).not.toThrow();
  });

  it('addRoute updates an existing alias in place', () => {
    const mgr = new ConfigManager(empty_config());
    mgr.addRoute('alias-1', [{ provider: 'p1', model: 'm1' }], 'first');
    mgr.addRoute('alias-1', [{ provider: 'p2', model: 'm2' }], 'second');

    expect(mgr.raw.routes).toHaveLength(1);
    expect(mgr.raw.routes[0].hops).toEqual([{ provider: 'p2', model: 'm2' }]);
    expect(mgr.raw.routes[0].description).toBe('second');
  });
});

describe('provider type surface', () => {
  it("rejects 'gemini' providers instead of silently using the OpenAI wire", () => {
    const cfg = empty_config();
    cfg.providers.push({
      id: 'g',
      name: 'Gemini upstream',
      type: 'gemini',
      apiKey: '',
      baseURL: '',
      models: [],
      timeoutMs: 60_000,
      enabled: true,
      circuitBreaker: {
        failureThreshold: 3,
        cooldownMs: 1000,
        halfOpenRetries: 1,
        successThreshold: 2,
      },
      weight: 1,
    });
    const result = AppConfig.safeParse(cfg);
    expect(result.success).toBe(false);
    if (!result.success) {
      const message = JSON.stringify(result.error.issues);
      expect(message).toContain('gemini');
      expect(message).toContain('desktop runtime');
    }
  });

  it('createProvider refuses a gemini row even when constructed directly', async () => {
    const { createProvider } = await import('../src/gateway/providers/index.js');
    const provider = {
      id: 'g',
      name: 'Gemini upstream',
      type: 'gemini',
      apiKey: '',
      baseURL: '',
      models: [],
      timeoutMs: 60_000,
      enabled: true,
      circuitBreaker: {
        failureThreshold: 3,
        cooldownMs: 1000,
        halfOpenRetries: 1,
        successThreshold: 2,
      },
      weight: 1,
    } as unknown as import('../src/config/schema.js').ProviderConfig;
    expect(() => createProvider(provider)).toThrow(/no Node gateway provider/);
  });
});
