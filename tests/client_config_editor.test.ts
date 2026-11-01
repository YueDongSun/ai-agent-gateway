import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
  read_client_config,
  save_client_config,
} from '../ui/src/lib/api/providers.js';
import { installMockBackend } from '../ui/src/lib/mock/backend.js';

vi.mock('../ui/src/lib/build_info.js', () => ({
  app_version: 'test',
  build_channel: 'demo',
  is_demo_build: true,
}));

const max_client_edit_bytes = 2 * 1024 * 1024;

beforeEach(() => {
  vi.stubGlobal('window', {});
  installMockBackend();
});

afterEach(() => vi.unstubAllGlobals());

describe('demo client-config editor size bound', () => {
  it('rejects an oversized submission without replacing the stored contents', async () => {
    const before = await read_client_config('kilo_cli');
    const oversized = `{"padding":"${'x'.repeat(max_client_edit_bytes)}"}`;
    await expect(save_client_config('kilo_cli', oversized)).rejects.toThrow(
      'AAG_CLIENT_CONFIG_EDIT_TOO_LARGE',
    );
    const after = await read_client_config('kilo_cli');
    expect(after.contents).toEqual(before.contents);
  });

  it('still accepts a normal-sized save', async () => {
    const edited = '{\n  // demo edit\n  "theme": "dark"\n}\n';
    const result = await save_client_config('kilo_cli', edited);
    expect(result.written).toHaveLength(1);
    expect((await read_client_config('kilo_cli')).contents).toEqual(edited);
  });
});
