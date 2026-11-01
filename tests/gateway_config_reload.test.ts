/**
 * Hot config reload for the Node gateway (Rust runtime parity: live
 * routing rebuild). Covers the app-level `reload_config` swap and the
 * config file watcher in src/gateway/config_reload.ts.
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createGatewayApp } from '../src/gateway/server.js';
import {
  start_config_watcher,
  CONFIG_RELOAD_DEBOUNCE_MS,
} from '../src/gateway/config_reload.js';
import type { AppConfig } from '../src/config/schema.js';

function config(model: string, alias: string): AppConfig {
  return {
    version: 1,
    gateway: { host: '127.0.0.1', port: 8787, authToken: '', logLevel: 'error' },
    providers: [
      {
        id: 'p',
        name: 'p',
        type: 'mock',
        baseURL: '',
        apiKey: '',
        enabled: true,
        models: [model],
        timeoutMs: 1000,
        weight: 1,
        circuitBreaker: { failureThreshold: 3, cooldownMs: 1000, halfOpenRetries: 1, successThreshold: 1 },
      },
    ],
    routes: [{ alias, hops: [{ provider: 'p', model }], description: '' }],
    profiles: [],
    agents: [],
  };
}

const temp_dirs: string[] = [];
afterEach(() => {
  for (const dir of temp_dirs.splice(0)) rmSync(dir, { recursive: true, force: true });
});

describe('createGatewayApp reload_config', () => {
  it('swaps the routing surface for subsequent requests', async () => {
    const { app, reload_config } = createGatewayApp(config('m1', 'a1'));
    const before = await app.request('/health');
    expect(((await before.json()) as { models: string[] }).models).toContain('a1');

    reload_config(config('m2', 'a2'));

    const after = await app.request('/health');
    const after_models = ((await after.json()) as { models: string[] }).models;
    expect(after_models).toContain('a2');
    expect(after_models).not.toContain('a1');

    const completion = await app.request('/v1/chat/completions', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ model: 'a2', messages: [{ role: 'user', content: 'hi' }] }),
    });
    expect(completion.status).toBe(200);
    const stale = await app.request('/v1/chat/completions', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ model: 'a1', messages: [{ role: 'user', content: 'hi' }] }),
    });
    expect(stale.status).toBe(404);
  });

  it('keeps the router getter live across reloads', async () => {
    const { app, router, reload_config } = createGatewayApp(config('m1', 'a1'));
    expect((router as unknown as { providers: unknown[] }).providers).toHaveLength(1);
    reload_config(config('m2', 'a2'));
    void app;
    expect((router as unknown as { providers: unknown[] }).providers).toHaveLength(1);
  });
});

describe('start_config_watcher', () => {
  const settle = () =>
    new Promise((resolve) => setTimeout(resolve, CONFIG_RELOAD_DEBOUNCE_MS + 350));

  it('reloads on config changes and rejects broken files without reloading', async () => {
    const dir = mkdtempSync(join(tmpdir(), 'aag_reload_'));
    temp_dirs.push(dir);
    const config_path = join(dir, 'config.json');
    writeFileSync(config_path, JSON.stringify(config('m1', 'a1')));

    const reloaded: string[] = [];
    const watcher = start_config_watcher({
      config_path,
      reload: (cfg) => reloaded.push(cfg.routes[0]?.alias ?? ''),
    });

    // Coalesced write: valid change applies once.
    writeFileSync(config_path, JSON.stringify(config('m2', 'a2')));
    await settle();
    expect(reloaded).toEqual(['a2']);

    // Unrelated file in the same directory is ignored.
    writeFileSync(join(dir, 'other.json'), '{}');
    await settle();
    expect(reloaded).toEqual(['a2']);

    // A broken config keeps the previous one serving (no reload call).
    writeFileSync(config_path, '{not json');
    await settle();
    expect(reloaded).toEqual(['a2']);

    // Recovery applies again.
    writeFileSync(config_path, JSON.stringify(config('m3', 'a3')));
    await settle();
    expect(reloaded).toEqual(['a2', 'a3']);

    watcher.close();
    writeFileSync(config_path, JSON.stringify(config('m4', 'a4')));
    await settle();
    expect(reloaded).toEqual(['a2', 'a3']);
  });

  it('does not fire while the debounce window is still open', async () => {
    const dir = mkdtempSync(join(tmpdir(), 'aag_reload_'));
    temp_dirs.push(dir);
    const config_path = join(dir, 'config.json');
    writeFileSync(config_path, JSON.stringify(config('m1', 'a1')));

    const reload = vi.fn();
    const watcher = start_config_watcher({ config_path, reload, debounce_ms: 200 });
    writeFileSync(config_path, JSON.stringify(config('m2', 'a2')));
    writeFileSync(config_path, JSON.stringify(config('m3', 'a3')));
    await new Promise((resolve) => setTimeout(resolve, 100));
    expect(reload).not.toHaveBeenCalled();
    await new Promise((resolve) => setTimeout(resolve, 500));
    expect(reload).toHaveBeenCalledTimes(1);
    expect(reload.mock.calls[0][0].routes[0].alias).toBe('a3');
    watcher.close();
  });
});
