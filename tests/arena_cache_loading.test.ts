import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { refresh_arena_evidence } from '../ui/src/lib/api/arena_visualization.js';

const invoke = vi.fn();
const fresh_source = { source: 'lmarena', fetchedAt: 1_750_000_000, stale: false, lastError: null };

beforeEach(() => {
  invoke.mockReset();
  vi.stubGlobal('window', { __TAURI_INTERNALS__: { invoke } });
});
afterEach(() => vi.unstubAllGlobals());

describe('Arena cache refresh result', () => {
  it('requests stale-only admission for automatic cache loading', async () => {
    invoke.mockResolvedValue(fresh_source);
    await expect(refresh_arena_evidence(true)).resolves.toEqual(fresh_source);
    expect(invoke.mock.calls[0]?.slice(0, 2)).toEqual(['supplier_catalog_refresh', {
      input: { source: 'lmarena' }, onlyIfStale: true,
    }]);
  });
  it('retains the existing forced manual refresh command', async () => {
    invoke.mockResolvedValue(fresh_source);
    await expect(refresh_arena_evidence()).resolves.toEqual(fresh_source);
    expect(invoke.mock.calls[0]?.slice(0, 2)).toEqual(['supplier_catalog_refresh', { input: { source: 'lmarena' } }]);
  });
  it('rejects an initial fetch error returned as a resolved source view', async () => {
    invoke.mockResolvedValue({ source: 'lmarena', fetchedAt: null, stale: true, lastError: 'fetch_timeout' });
    await expect(refresh_arena_evidence()).rejects.toThrow('fetch_timeout');
  });
  it('does not present a retained stale document as a successful refresh', async () => {
    invoke.mockResolvedValue({ ...fresh_source, stale: true, lastError: 'fetch_transport' });
    await expect(refresh_arena_evidence()).rejects.toThrow('fetch_transport');
  });
  it.each([
    { ...fresh_source, stale: true },
    { ...fresh_source, fetchedAt: null },
    { ...fresh_source, source: 'another-source' },
    undefined,
  ])('requires a fresh, persisted Arena source result: %j', async (result) => {
    invoke.mockResolvedValue(result);
    await expect(refresh_arena_evidence()).rejects.toThrow('arena_evidence_unavailable');
  });
  it('never exposes arbitrary returned error text', async () => {
    invoke.mockResolvedValue({ ...fresh_source, stale: true, lastError: 'PRIVATE_RAW_ERROR_MUST_STAY_PRIVATE' });
    await expect(refresh_arena_evidence()).rejects.toThrow('arena_evidence_unavailable');
  });
});
