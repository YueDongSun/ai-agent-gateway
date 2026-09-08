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
import { mkdtempSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import {
  CopilotAdapter,
  PiAdapter,
  AntigravityAdapter,
  defaultAgentTargets,
} from '../src/agents/adapters.js';
import type { AgentTarget } from '../src/config/schema.js';

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
