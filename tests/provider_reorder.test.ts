import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
  deleteProvider,
  listProviders,
  reorderProviders,
  saveProvider,
} from '../ui/src/lib/api/providers.js';
import { installMockBackend } from '../ui/src/lib/mock/backend.js';

vi.mock('../ui/src/lib/build_info.js', () => ({
  app_version: 'test',
  build_channel: 'demo',
  is_demo_build: true,
}));

const app_type = 'reorder_fixture';
const other_app_type = 'reorder_other_fixture';

beforeEach(async () => {
  vi.stubGlobal('window', {});
  installMockBackend();
  for (const target_app_type of [app_type, other_app_type]) {
    for (const provider of await listProviders(target_app_type)) {
      await deleteProvider(provider.id, target_app_type);
    }
  }
  for (const [id, sort_index] of [['first', 10], ['second', 20], ['omitted', 0]] as const) {
    await saveProvider(app_type, { id, name: id, settingsConfig: {}, sortIndex: sort_index });
  }
  await saveProvider(other_app_type, {
    id: 'foreign', name: 'foreign', settingsConfig: {}, sortIndex: 40,
  });
});

afterEach(() => vi.unstubAllGlobals());

describe('demo provider reorder validation', () => {
  it.each([
    { ordered_ids: ['second', 'first', 'second'], error_code: 'AAG_PROVIDER_REORDER_DUPLICATE_ID' },
    { ordered_ids: ['second', 'missing'], error_code: 'AAG_PROVIDER_REORDER_UNKNOWN_ID' },
    { ordered_ids: ['second', 'foreign'], error_code: 'AAG_PROVIDER_REORDER_UNKNOWN_ID' },
    { ordered_ids: ['second', ''], error_code: 'AAG_PROVIDER_REORDER_UNKNOWN_ID' },
  ])('rejects $ordered_ids without modifying either app', async ({ ordered_ids, error_code }) => {
    const before = await listProviders(app_type);
    const other_before = await listProviders(other_app_type);
    await expect(reorderProviders(app_type, ordered_ids)).rejects.toThrow(error_code);
    expect(await listProviders(app_type)).toEqual(before);
    expect(await listProviders(other_app_type)).toEqual(other_before);
  });

  it('accepts a complete order repeatedly without modifying another app', async () => {
    const other_before = await listProviders(other_app_type);
    const ordered_ids = ['omitted', 'second', 'first'];
    for (let attempt = 0; attempt < 2; attempt += 1) {
      await reorderProviders(app_type, ordered_ids);
      expect((await listProviders(app_type)).map((provider) => provider.id)).toEqual(ordered_ids);
    }
    expect(await listProviders(other_app_type)).toEqual(other_before);
  });

  it('accepts partial and empty lists without changing omitted indexes', async () => {
    const before = await listProviders(app_type);
    await reorderProviders(app_type, ['second']);
    const after_partial = await listProviders(app_type);
    for (const id of ['first', 'omitted']) {
      expect(after_partial.find((provider) => provider.id === id))
        .toEqual(before.find((provider) => provider.id === id));
    }
    await reorderProviders(app_type, []);
    expect(await listProviders(app_type)).toEqual(after_partial);
  });
});
