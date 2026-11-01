/**
 * Unit tests for the agent adapters (src/agents/adapters.ts).
 *
 * Covers the agents AAG targets for independent switching:
 *   - Copilot CLI: settings.json + COPILOT_PROVIDER_* env block, JSONC-safe
 *     read, and merge-on-write so unrelated user keys survive.
 *   - pi (MS coding agent): models.json providers map + settings.json.
 *   - Antigravity: detect-only; build* must throw (disabled).
 */
import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import { mkdtempSync, writeFileSync, readFileSync, rmSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import {
  CopilotAdapter,
  KiloAdapter,
  PiAdapter,
  AntigravityAdapter,
  defaultAgentTargets,
} from '../src/agents/adapters.js';
import { Switcher } from '../src/agents/switcher.js';
import type { AgentTarget, AppConfig } from '../src/config/schema.js';

let tmp: string;

beforeEach(() => {
  tmp = mkdtempSync(join(tmpdir(), 'aag-adapters-'));
});

afterEach(() => {
  rmSync(tmp, { recursive: true, force: true });
});

function targetFor(id: string): AgentTarget {
  const t = defaultAgentTargets().find((a) => a.id === id);
  if (!t) throw new Error(`no default target for ${id}`);
  return { ...t, configPaths: [join(tmp, 'config.json')] };
}

describe('Copilot CLI adapter', () => {
  it('preserves nested user environment across gateway/direct switches and exact backups', () => {
    const adapter = new CopilotAdapter(targetFor('copilot'));
    const path = adapter.resolveConfigPath()!;
    const original = '{\r\n  "theme": "dark", "env": {"HTTPS_PROXY": "http://proxy.invalid", "CUSTOM": "keep"}\r\n}\r\n';
    writeFileSync(path, original);
    adapter.write(adapter.buildGatewayConfig('http://127.0.0.1:8787', 'fixture', 'exact-model'));
    expect(readFileSync(`${path}.bak`, 'utf8')).toBe(original);
    expect(JSON.parse(adapter.read()!).env).toMatchObject({ HTTPS_PROXY: 'http://proxy.invalid', CUSTOM: 'keep' });
    const gateway_bytes = readFileSync(path, 'utf8');
    adapter.write(adapter.buildDirectConfig('https://direct.invalid/v1', 'direct', 'exact-model'));
    expect(readFileSync(`${path}.bak`, 'utf8')).toBe(gateway_bytes);
    expect(JSON.parse(adapter.read()!).env).toMatchObject({ HTTPS_PROXY: 'http://proxy.invalid', CUSTOM: 'keep', COPILOT_PROVIDER_API_KEY: 'direct' });
    const direct_bytes = readFileSync(path, 'utf8');
    adapter.write(adapter.buildDirectConfig('https://direct.invalid/v1', 'direct', 'exact-model'));
    expect(readFileSync(path, 'utf8')).toBe(direct_bytes);
    // The retained bytes are sufficient to restore the last pre-write state.
    writeFileSync(path, readFileSync(`${path}.bak`));
    expect(readFileSync(path, 'utf8')).toBe(direct_bytes);
  });

  it('supports inline JSONC comments/trailing commas while preserving comment-like strings', () => {
    const adapter = new CopilotAdapter(targetFor('copilot'));
    const path = adapter.resolveConfigPath()!;
    writeFileSync(path, '{ "url": "https://example.invalid/a//b", "text": "/* literal */", // inline\n "env": { "CUSTOM": "quoted \\\" // value", }, /* block */ }');
    adapter.write(adapter.buildGatewayConfig('http://127.0.0.1:8787', '', 'm'));
    expect(JSON.parse(adapter.read()!)).toMatchObject({ url: 'https://example.invalid/a//b', text: '/* literal */', env: { CUSTOM: 'quoted " // value' } });
  });

  it.each(['{bad', '{"secret":"fixture-secret", /* unfinished', '[]', '{"env":null}', '{"env":["custom"]}', '{"env":{,}}'])('rejects unsupported existing content without any write: %s', (original) => {
    const adapter = new CopilotAdapter(targetFor('copilot'));
    const path = adapter.resolveConfigPath()!;
    writeFileSync(path, original);
    expect(() => adapter.write(adapter.buildGatewayConfig('http://127.0.0.1:8787', 'fixture', 'm'))).toThrow(/Copilot config/);
    expect(readFileSync(path, 'utf8')).toBe(original);
    expect(existsSync(`${path}.bak`)).toBe(false);
    expect(existsSync(`${path}.aag-tmp`)).toBe(false);
  });

  it('reports failed switching without changing the target state or exposing malformed content', () => {
    const target = targetFor('copilot');
    const original = '{"credential":"fixture-private-value",broken}';
    writeFileSync(target.configPaths[0], original);
    const cfg: AppConfig = { version: 1, gateway: { host: '127.0.0.1', port: 8787, authToken: '', logLevel: 'info' }, providers: [], routes: [], profiles: [], agents: [target] };
    const result = new Switcher(cfg).pointToGateway('copilot', 'exact-model');
    expect(result.ok).toBe(false);
    expect(result.message).toContain('Copilot config');
    expect(result.message).not.toContain('fixture-private-value');
    expect(target.pointedAtGateway).toBe(false);
    expect(readFileSync(target.configPaths[0], 'utf8')).toBe(original);
    expect(new CopilotAdapter(target).detectCurrentBaseURL()).toBeUndefined();
  });

  it('buildGatewayConfig emits model + COPILOT_PROVIDER_* env block', () => {
    const a = new CopilotAdapter(targetFor('copilot'));
    const out = JSON.parse(a.buildGatewayConfig('http://127.0.0.1:8787', 'tok', 'claude-sonnet-4'));
    expect(out.model).toBe('claude-sonnet-4');
    expect(out.env.COPILOT_PROVIDER_BASE_URL).toBe('http://127.0.0.1:8787/v1');
    expect(out.env.COPILOT_PROVIDER_API_KEY).toBe('tok');
    expect(out.env.COPILOT_PROVIDER_TYPE).toBe('openai');
  });

  it('read() strips JSONC comments before parsing', () => {
    const a = new CopilotAdapter(targetFor('copilot'));
    const p = a.resolveConfigPath()!;
    writeFileSync(p, `{
      // user model choice
      "model": "claude-sonnet-4", /* inline */
      "theme": "auto"
    }`, 'utf8');
    const raw = a.read();
    expect(raw).toBeTruthy();
    const obj = JSON.parse(raw!);
    expect(obj.model).toBe('claude-sonnet-4');
    expect(obj.theme).toBe('auto');
  });

  it('write() merges into existing settings, preserving user keys', () => {
    const a = new CopilotAdapter(targetFor('copilot'));
    const p = a.resolveConfigPath()!;
    writeFileSync(p, JSON.stringify({ theme: 'dark', banner: true }), 'utf8');

    a.write(a.buildGatewayConfig('http://127.0.0.1:8787', 'tok', 'gpt-4o'));

    const merged = JSON.parse(a.read()!);
    expect(merged.theme).toBe('dark'); // preserved
    expect(merged.banner).toBe(true); // preserved
    expect(merged.model).toBe('gpt-4o'); // updated
    expect(merged.env.COPILOT_PROVIDER_BASE_URL).toBe('http://127.0.0.1:8787/v1');
  });
});

describe('Kilo/OpenCode-compatible adapter', () => {
  it('builds the provider/model projection consumed by Kilo VS Code', () => {
    const a = new KiloAdapter(targetFor('kilo'));
    const out = JSON.parse(a.buildGatewayConfig('http://127.0.0.1:8787', 'secret', 'gpt-5'));
    expect(out.model).toBe('aag/gpt-5');
    expect(out.provider.aag.npm).toBe('@ai-sdk/openai-compatible');
    expect(out.provider.aag.options.baseURL).toBe('http://127.0.0.1:8787/v1');
    expect(out.provider.aag.options.apiKey).toBe('secret');
    expect(out.provider.aag.models['gpt-5'].name).toBe('gpt-5');
  });

  it('creates a missing config and merges stale AAG state without duplicate providers', () => {
    const a = new KiloAdapter(targetFor('kilo'));
    const p = a.resolveConfigPath()!;
    a.write(a.buildGatewayConfig('http://127.0.0.1:8787', 'secret', 'gpt-5'));
    expect(JSON.parse(a.read()!).provider.aag.options.baseURL).toBe('http://127.0.0.1:8787/v1');

    writeFileSync(p, `{
      // Kilo VS Code user setting
      "small_model": "aag/gpt-mini",
      "theme": "dark",
      "provider": { "other": { "npm": "custom" }, "aag": { "options": { "baseURL": "https://stale.invalid" } } }
    }`, 'utf8');

    a.write(a.buildGatewayConfig('http://127.0.0.1:8787', 'secret', 'gpt-5'));
    a.write(a.buildGatewayConfig('http://127.0.0.1:8989', 'secret-2', 'gpt-5-1'));

    const merged = JSON.parse(a.read()!);
    expect(merged.theme).toBe('dark');
    expect(merged.small_model).toBe('aag/gpt-mini');
    expect(merged.provider.other.npm).toBe('custom');
    expect(Object.keys(merged.provider).filter((id) => id === 'aag')).toHaveLength(1);
    expect(merged.provider.aag.options.baseURL).toBe('http://127.0.0.1:8989/v1');
    expect(merged.provider.aag.options.apiKey).toBe('secret-2');
    expect(merged.model).toBe('aag/gpt-5-1');
  });

  it('rejects malformed config without replacing the original or exposing secrets', () => {
    const a = new KiloAdapter(targetFor('kilo'));
    const p = a.resolveConfigPath()!;
    const api_key = ['fixture', 'key'].join('-');
    const malformed = JSON.stringify({ provider: ['bad'], apiKey: api_key });
    writeFileSync(p, malformed, 'utf8');
    expect(() => a.write(a.buildGatewayConfig('http://127.0.0.1:8787', api_key, 'gpt-5'))).toThrow(/provider must be an object/);
    expect(readFileSync(p, 'utf8')).toBe(malformed);
  });
});
describe('pi adapter', () => {
  it('buildGatewayConfig emits providers map with baseUrl/api/models', () => {
    const a = new PiAdapter(targetFor('pi'));
    const out = JSON.parse(a.buildGatewayConfig('http://127.0.0.1:8787', 'tok', 'claude-sonnet-4'));
    expect(out.providers.aag.baseUrl).toBe('http://127.0.0.1:8787/v1');
    expect(out.providers.aag.api).toBe('openai-completions');
    expect(out.providers.aag.apiKey).toBe('tok');
    expect(out.providers.aag.models).toEqual([{ id: 'claude-sonnet-4' }]);
  });

  it('detectCurrentBaseURL reads the aag provider baseUrl', () => {
    const a = new PiAdapter(targetFor('pi'));
    const p = a.resolveConfigPath()!;
    writeFileSync(p, JSON.stringify({
      providers: { aag: { baseUrl: 'http://127.0.0.1:9999/v1', api: 'openai-completions' } },
    }), 'utf8');
    expect(a.detectCurrentBaseURL()).toBe('http://127.0.0.1:9999/v1');
  });
});

describe('atomic write with rolling backup', () => {
  it('CopilotAdapter.write backs up the previous content and leaves no staging file', () => {
    const a = new CopilotAdapter(targetFor('copilot'));
    const p = a.resolveConfigPath()!;
    writeFileSync(p, JSON.stringify({ theme: 'dark' }), 'utf8');

    a.write(a.buildGatewayConfig('http://127.0.0.1:8787', 'tok', 'gpt-4o'));
    expect(JSON.parse(readFileSync(`${p}.bak`, 'utf8')).theme).toBe('dark');
    expect(JSON.parse(readFileSync(p, 'utf8')).model).toBe('gpt-4o');
    expect(existsSync(`${p}.aag-tmp`)).toBe(false);

    // A second switch rolls the backup to the most recent previous state.
    a.write(a.buildGatewayConfig('http://127.0.0.1:8989', 'tok', 'gpt-4o'));
    expect(JSON.parse(readFileSync(`${p}.bak`, 'utf8')).env.COPILOT_PROVIDER_BASE_URL)
      .toBe('http://127.0.0.1:8787/v1');
  });

  it('KiloAdapter.write backs up the previous content', () => {
    const a = new KiloAdapter(targetFor('kilo'));
    const p = a.resolveConfigPath()!;
    writeFileSync(p, JSON.stringify({ theme: 'dark' }), 'utf8');

    a.write(a.buildGatewayConfig('http://127.0.0.1:8787', 'secret', 'gpt-5'));
    expect(JSON.parse(readFileSync(`${p}.bak`, 'utf8')).theme).toBe('dark');
    expect(existsSync(`${p}.aag-tmp`)).toBe(false);
  });

  it('the base JsonAgentAdapter.write backs up the previous content', () => {
    const a = new PiAdapter(targetFor('pi'));
    const p = a.resolveConfigPath()!;
    writeFileSync(p, JSON.stringify({ providers: { other: { baseUrl: 'https://direct.invalid' } } }), 'utf8');

    a.write(a.buildDirectConfig('https://direct.invalid', 'secret', 'claude-sonnet-4'));
    expect(JSON.parse(readFileSync(`${p}.bak`, 'utf8')).providers.other.baseUrl).toBe('https://direct.invalid');
    expect(existsSync(`${p}.aag-tmp`)).toBe(false);
  });
});

describe('Switcher status', () => {
  const switcher_config = (kilo_path: string): AppConfig => ({
    version: 1,
    gateway: { host: '127.0.0.1', port: 8787, authToken: '', logLevel: 'info' },
    providers: [],
    routes: [],
    profiles: [],
    agents: [{
      id: 'kilo',
      name: 'Kilo Code',
      format: 'openai',
      configPaths: [kilo_path, join(kilo_path, '..', 'missing.json')],
      pointedAtGateway: false,
      activeProfileId: '',
    }],
  });

  it('reports the existing config path, base URL, and gateway flag across a switch', () => {
    const kilo_path = join(tmp, 'kilo.json');
    writeFileSync(kilo_path, JSON.stringify({
      provider: { aag: { options: { baseURL: 'https://old.invalid/v1' } } },
    }), 'utf8');

    const sw = new Switcher(switcher_config(kilo_path));
    const before = sw.status('kilo');
    expect(before.path).toBe(kilo_path);
    expect(before.baseURL).toBe('https://old.invalid/v1');
    expect(before.pointedAtGateway).toBe(false);

    const result = sw.pointToGateway('kilo', 'gpt-5');
    expect(result.ok).toBe(true);

    const after = sw.status('kilo');
    expect(after.path).toBe(kilo_path);
    expect(after.pointedAtGateway).toBe(true);
    expect(after.baseURL).toBe('http://127.0.0.1:8787/v1');
    // The switch left a backup of the pre-switch state.
    expect(JSON.parse(readFileSync(`${kilo_path}.bak`, 'utf8')).provider.aag.options.baseURL)
      .toBe('https://old.invalid/v1');
  });
});

describe('Antigravity adapter', () => {
  it('default target resolves to APPDATA Preferences path on Windows', () => {
    const t = defaultAgentTargets().find((a) => a.id === 'antigravity')!;
    expect(t.configPaths[0]).toMatch(/Antigravity/);
  });

  it('buildGatewayConfig throws (switching disabled)', () => {
    const a = new AntigravityAdapter(targetFor('antigravity'));
    expect(() => a.buildGatewayConfig('http://x', 'k', 'm')).toThrow(/not supported/i);
  });

  it('buildDirectConfig throws (switching disabled)', () => {
    const a = new AntigravityAdapter(targetFor('antigravity'));
    expect(() => a.buildDirectConfig('http://x', 'k', 'm')).toThrow(/not supported/i);
  });
});
