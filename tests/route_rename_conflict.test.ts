import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { listRoutes, renameRoute, saveRoute } from '../ui/src/lib/api/proxy.js';
import { installMockBackend } from '../ui/src/lib/mock/backend.js';

vi.mock('../ui/src/lib/build_info.js', () => ({
  app_version: 'test',
  build_channel: 'demo',
  is_demo_build: true,
}));

const route = (alias: string) => ({
  alias,
  description: '',
  hops: [{ provider: 'fixture_provider', model: 'fixture_model' }],
});

beforeEach(() => {
  vi.stubGlobal('window', {});
  installMockBackend();
});

afterEach(() => vi.unstubAllGlobals());

describe('demo route rename alias conflict', () => {
  it('rejects a rename onto an existing alias with the stable code and preserves both routes', async () => {
    await saveRoute(route('alpha'));
    await saveRoute(route('beta'));
    await expect(renameRoute('alpha', 'beta')).rejects.toThrow(
      'AAG_ROUTE_ALIAS_CONFLICT',
    );
    const aliases = (await listRoutes()).map((r) => r.alias);
    expect(aliases).toContain('alpha');
    expect(aliases).toContain('beta');
  });

  it('still renames onto a free alias', async () => {
    await saveRoute(route('alpha'));
    await renameRoute('alpha', 'gamma');
    const aliases = (await listRoutes()).map((r) => r.alias);
    expect(aliases).toContain('gamma');
    expect(aliases).not.toContain('alpha');
  });
});
