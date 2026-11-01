import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
  setProviderFailoverQueue,
  switchProvider,
} from '../ui/src/lib/api/providers.js';
import { renameRoute } from '../ui/src/lib/api/proxy.js';
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

describe('demo missing-row conditions', () => {
  it('reports the stable provider not-found code for switch and queue toggles', async () => {
    await expect(switchProvider('absent_provider', 'claude_code')).rejects.toThrow(
      'AAG_PROVIDER_NOT_FOUND',
    );
    await expect(
      setProviderFailoverQueue('absent_provider', 'claude_code', true),
    ).rejects.toThrow('AAG_PROVIDER_NOT_FOUND');
  });

  it('reports the stable route not-found code for an unknown rename source', async () => {
    await expect(renameRoute('absent_alias', 'fresh_alias')).rejects.toThrow(
      'AAG_ROUTE_NOT_FOUND',
    );
  });
});
