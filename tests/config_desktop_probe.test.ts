/**
 * Desktop-runtime coexistence probe (src/config/store.ts): the Node gateway
 * must notice the Rust/SQLite store (`AAG_CONFIG_DIR`/`~/.aag` +
 * `aag.db`) so gateway startup can warn about the two runtimes' separate
 * provider stores.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { homedir } from 'node:os';
import {
  desktop_database_path,
  desktop_database_exists,
} from '../src/config/store.js';
import { desktop_runtime_status } from '../src/cli/commands.js';

const temp_dirs: string[] = [];
beforeEach(() => { vi.stubEnv('AAG_DESKTOP_ENDPOINT', 'http://127.0.0.1:8989'); });
afterEach(() => {
  vi.unstubAllGlobals();
  vi.unstubAllEnvs();
  delete process.env.AAG_CONFIG_DIR;
  for (const dir of temp_dirs.splice(0)) rmSync(dir, { recursive: true, force: true });
});

describe('desktop database probe', () => {
  it('follows the AAG_CONFIG_DIR override', () => {
    process.env.AAG_CONFIG_DIR = 'C:\\aag\\shared';
    expect(desktop_database_path()).toBe('C:\\aag\\shared\\aag.db');
  });

  it('defaults to ~/.aag/aag.db without an override', () => {
    delete process.env.AAG_CONFIG_DIR;
    expect(desktop_database_path()).toBe(join(homedir(), '.aag', 'aag.db'));
  });

  it('detects a database in a shared override directory and its absence', () => {
    const dir = mkdtempSync(join(tmpdir(), 'aag_probe_'));
    temp_dirs.push(dir);
    process.env.AAG_CONFIG_DIR = dir;
    expect(desktop_database_exists()).toBe(false);
    writeFileSync(join(dir, 'aag.db'), '');
    expect(desktop_database_exists()).toBe(true);
    // Store files other than aag.db do not count as the desktop store.
    const empty_dir = mkdtempSync(join(tmpdir(), 'aag_probe_'));
    temp_dirs.push(empty_dir);
    mkdirSync(join(empty_dir, 'state'), { recursive: true });
    process.env.AAG_CONFIG_DIR = empty_dir;
    expect(desktop_database_exists()).toBe(false);
  });
});

describe('desktop_runtime_status', () => {
  it('reports a running desktop proxy only from explicit runtime identity', async () => {
    const fetch_spy = vi.fn(async () => Response.json({ service: 'ai-agent-gateway', schemaVersion: 1, status: 'running', version: '0.2.0', runtime_kind: 'desktop' }));
    vi.stubGlobal('fetch', fetch_spy);
    process.env.AAG_CONFIG_DIR = '';
    await expect(desktop_runtime_status()).resolves.toEqual({
      running: true,
      version: '0.2.0',
      endpoint: 'http://127.0.0.1:8989',
    });
    expect(fetch_spy.mock.calls[0]?.[0]).toBe('http://127.0.0.1:8989/aag/discovery?runtime=1');
  });

  it.each(['node', undefined])('does not call a %s identity the Desktop runtime', async (runtime_kind) => {
    const directory = mkdtempSync(join(tmpdir(), 'aag_probe_'));
    temp_dirs.push(directory);
    process.env.AAG_CONFIG_DIR = directory;
    vi.stubGlobal('fetch', vi.fn(async () => Response.json({ service: 'ai-agent-gateway', schemaVersion: 1, status: 'running', version: '0.2.0', runtime_kind })));
    await expect(desktop_runtime_status()).resolves.toEqual({ running: false, installed: false, endpoint: 'http://127.0.0.1:8989', runtime_kind: runtime_kind ?? 'unknown', reason: 'unrecognized' });
  });

  it('falls back to the installed probe when the proxy is absent', async () => {
    const dir = mkdtempSync(join(tmpdir(), 'aag_probe_'));
    temp_dirs.push(dir);
    process.env.AAG_CONFIG_DIR = dir;
    vi.stubGlobal('fetch', vi.fn(async () => {
      throw new Error('ECONNREFUSED');
    }));
    await expect(desktop_runtime_status()).resolves.toEqual({
      running: false,
      installed: false,
      endpoint: 'http://127.0.0.1:8989',
      reason: 'unreachable',
    });
    writeFileSync(join(dir, 'aag.db'), '');
    await expect(desktop_runtime_status()).resolves.toEqual({
      running: false,
      installed: true,
      endpoint: 'http://127.0.0.1:8989',
      reason: 'unreachable',
    });
  });

  it('treats a foreign responder on the port as not running', async () => {
    vi.stubGlobal('fetch', vi.fn(async () =>
      Response.json({ service: 'something-else' }),
    ));
    const empty_dir = mkdtempSync(join(tmpdir(), 'aag_probe_'));
    temp_dirs.push(empty_dir);
    process.env.AAG_CONFIG_DIR = empty_dir;
    await expect(desktop_runtime_status()).resolves.toEqual({
      running: false,
      installed: false,
      endpoint: 'http://127.0.0.1:8989',
      runtime_kind: 'unknown',
      reason: 'unrecognized',
    });
  });
});
