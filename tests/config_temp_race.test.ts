import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterAll, beforeAll, describe, expect, it, vi } from 'vitest';

vi.mock('node:crypto', () => ({
  randomBytes: () => Buffer.alloc(16, 0x2a),
}));

const originalConfigDir = process.env.AAG_CONFIG_DIR;
const testRoot = mkdtempSync(join(tmpdir(), 'aag_node_temp_race_'));
process.env.AAG_CONFIG_DIR = join(testRoot, 'state');

const { getConfigPath, saveConfig, seedConfig } = await import('../src/config/store.js');

beforeAll(() => {
  saveConfig(seedConfig());
});

afterAll(() => {
  if (originalConfigDir === undefined) {
    delete process.env.AAG_CONFIG_DIR;
  } else {
    process.env.AAG_CONFIG_DIR = originalConfigDir;
  }
  if (testRoot.startsWith(tmpdir()) && existsSync(testRoot)) {
    rmSync(testRoot, { recursive: true, force: true });
  }
});

describe('Node atomic temporary race boundary', () => {
  it('fails closed without overwriting a colliding file or the current config', () => {
    const current = readFileSync(getConfigPath(), 'utf8');
    const suffix = Buffer.alloc(16, 0x2a).toString('hex');
    const collision = join(
      process.env.AAG_CONFIG_DIR!,
      `.config.json.aag_tmp_${suffix}`,
    );
    writeFileSync(collision, 'foreign-file', { flag: 'wx' });

    const replacement = seedConfig();
    replacement.gateway.authToken = 'must-not-commit';
    expect(() => saveConfig(replacement)).toThrow('AAG_PRIVATE_STORAGE_TEMPORARY_COLLISION');
    expect(readFileSync(getConfigPath(), 'utf8')).toBe(current);
    expect(readFileSync(collision, 'utf8')).toBe('foreign-file');
  });
});
