import { afterEach, describe, expect, it, vi } from 'vitest';
import { desktop_runtime_status } from '../src/cli/commands.js';
import { createGatewayApp } from '../src/gateway/server.js';
import { AppConfig } from '../src/config/schema.js';
import { logger } from '../src/util/log.js';
import { start_config_watcher } from '../src/gateway/config_reload.js';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

vi.mock('../src/config/store.js', () => ({ desktop_database_exists: () => false }));
afterEach(() => { vi.restoreAllMocks(); vi.unstubAllGlobals(); });
const configuration = () => AppConfig.parse({ gateway: { host: '127.0.0.1', port: 8787 }, providers: [], routes: [] });

describe('runtime identity and effective Node configuration', () => {
  it.each(['node', undefined, 'unexpected'])('does not identify %s as a desktop runtime', async (runtime_kind) => {
    vi.stubGlobal('fetch', vi.fn(async () => Response.json({ service: 'ai-agent-gateway', version: 'fixture', runtime_kind })));
    expect((await desktop_runtime_status()).running).toBe(false);
  });

  it('recognizes explicit desktop identity at a selected non-default endpoint', async () => {
    const fetch_spy = vi.fn(async () => Response.json({ service: 'ai-agent-gateway', version: 'fixture', runtime_kind: 'desktop' }));
    vi.stubGlobal('fetch', fetch_spy);
    const result = await desktop_runtime_status({ endpoint: 'http://127.0.0.1:9999' });
    expect(result).toMatchObject({ running: true, version: 'fixture', endpoint: 'http://127.0.0.1:9999' });
    expect(fetch_spy.mock.calls[0][0]).toBe('http://127.0.0.1:9999/aag/discovery?runtime=1');
  });

  it('distinguishes unavailable discovery from legacy ambiguous identity', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => Response.json({ service: 'ai-agent-gateway', version: 'fixture' })));
    expect(await desktop_runtime_status()).toMatchObject({ running: false, runtime_kind: 'unknown' });
    vi.stubGlobal('fetch', vi.fn(async () => { throw new Error('fixture transport'); }));
    expect(await desktop_runtime_status()).toMatchObject({ running: false, reason: 'unreachable' });
  });

  it('publishes Node identity and pending restart without implying model readiness', async () => {
    const cfg = configuration();
    const { app, reload_config } = createGatewayApp(cfg);
    const legacy = await (await app.request('/aag/discovery')).json() as object;
    expect(Object.keys(legacy).sort()).toEqual(['schemaVersion', 'service', 'status', 'version']);
    const discovery = await (await app.request('/aag/discovery?runtime=1')).json();
    expect(discovery).toMatchObject({ service: 'ai-agent-gateway', runtime_kind: 'node' });
    const next = configuration();
    next.gateway.port = 9999;
    next.gateway.authToken = 'fixture-private-token';
    reload_config(next);
    const health = await (await app.request('/health')).json() as Record<string, any>;
    expect(health.runtime_kind).toBe('node');
    expect(health.models).toEqual([]);
    expect(health.ready).toBe(false);
    expect(health.configuration).toMatchObject({ listener_restart_required: true, auth_restart_required: true, restart_required: true });
    expect(JSON.stringify(health)).not.toContain('fixture-private-token');
    reload_config(configuration());
    expect((await (await app.request('/health')).json() as any).configuration.restart_required).toBe(false);
  });

  it('keeps listener liveness separate from an alias with no enabled provider', async () => {
    const cfg = configuration();
    cfg.routes = [{ alias: 'unavailable', description: '', hops: [{ provider: 'missing', model: 'exact' }] }];
    const { app } = createGatewayApp(cfg);
    const health = await (await app.request('/health')).json() as any;
    expect(health.status).toBe('ok');
    expect(health.models).toEqual(['unavailable']);
    expect(health.available_models).toEqual([]);
    expect(health.ready).toBe(false);
  });

  it('serializes safe reload events and typed reasons without raw error details', () => {
    const lines: string[] = [];
    vi.spyOn(process.stdout, 'write').mockImplementation(((line: unknown) => { lines.push(String(line)); return true; }) as typeof process.stdout.write);
    vi.spyOn(process.stderr, 'write').mockImplementation(((line: unknown) => { lines.push(String(line)); return true; }) as typeof process.stderr.write);
    for (const event of ['gateway_reloaded', 'gateway_reload_pinned', 'config_reload_rejected', 'config_watcher_failed']) {
      logger.warn('config', event, { reload_reason: 'invalid_json', reload_status: 'rejected', error: 'fixture-private-token', listener_restart_required: true, provider_count: 2 });
    }
    logger.warn('config', 'config_reload_rejected', { reload_reason: 'fixture_private_token', reload_status: 'fixture_private_token' });
    const serialized = lines.join('');
    for (const event of ['gateway_reloaded', 'gateway_reload_pinned', 'config_reload_rejected', 'config_watcher_failed']) expect(serialized).toContain(event);
    expect(serialized).toContain('"reload_reason":"invalid_json"');
    expect(serialized).toContain('"reload_status":"rejected"');
    expect(serialized).toContain('"listener_restart_required":true');
    expect(serialized).not.toContain('fixture-private-token');
    expect(serialized).not.toContain('fixture_private_token');
    expect(serialized).not.toContain('unsafe_event_redacted');
  });

  it('retains the last good runtime after an invalid file reload and reports a safe reason', async () => {
    const directory = mkdtempSync(join(tmpdir(), 'aag_reload_safety_'));
    const config_path = join(directory, 'config.json');
    writeFileSync(config_path, JSON.stringify(configuration()));
    const { app, reload_config } = createGatewayApp(configuration());
    const lines: string[] = [];
    vi.spyOn(process.stderr, 'write').mockImplementation(((line: unknown) => { lines.push(String(line)); return true; }) as typeof process.stderr.write);
    const watcher = start_config_watcher({ config_path, reload: reload_config, debounce_ms: 1 });
    try {
      writeFileSync(config_path, '{"fixture-private-token": broken}');
      await vi.waitFor(() => expect(lines.join('')).toContain('"reload_reason":"invalid_json"'));
      expect(lines.join('')).not.toContain('fixture-private-token');
      const health = await (await app.request('/health')).json() as any;
      expect(health.configuration.restart_required).toBe(false);
      expect(health.models).toEqual([]);
    } finally {
      watcher.close();
      rmSync(directory, { recursive: true, force: true });
    }
  });

  it('reports watcher startup failure without serializing a private path', () => {
    const lines: string[] = [];
    vi.spyOn(process.stderr, 'write').mockImplementation(((line: unknown) => { lines.push(String(line)); return true; }) as typeof process.stderr.write);
    expect(() => start_config_watcher({ config_path: join(tmpdir(), 'fixture-nonexistent-reload-parent', 'config.json'), reload: () => {} })).toThrow(/automatic reload is unavailable/);
    expect(lines.join('')).toContain('config_watcher_failed');
    expect(lines.join('')).toContain('"reload_reason":"watch_failed"');
    expect(lines.join('')).not.toContain('fixture-nonexistent-reload-parent');
  });
});
