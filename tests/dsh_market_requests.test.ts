import { afterAll, beforeEach, describe, expect, it, vi } from 'vitest';
import { dsh_market_get_catalog, dsh_market_refresh } from '../ui/src/lib/api/dsh_market.js';

const mocked_invoke = vi.fn<(...arguments_list: unknown[]) => Promise<unknown>>();

describe('marketplace request ownership', () => {
  beforeEach(() => {
    mocked_invoke.mockReset();
    vi.stubGlobal('window', { __TAURI_INTERNALS__: { invoke: mocked_invoke } });
  });
  afterAll(() => { vi.unstubAllGlobals(); });

  it('coalesces overlapping open and refresh requests without retaining a completed catalog', async () => {
    let finish_request: (value: unknown) => void = () => {};
    mocked_invoke.mockReturnValueOnce(new Promise(resolve => { finish_request = resolve; }));
    const first_open = dsh_market_get_catalog();
    const repeated_open = dsh_market_get_catalog();
    const overlapping_refresh = dsh_market_refresh();
    expect(mocked_invoke).toHaveBeenCalledTimes(1);
    expect(repeated_open).toBe(first_open);
    expect(overlapping_refresh).toBe(first_open);
    const first_catalog = { entry_count: 1, fetched_at: '2026-10-08T00:00:00Z' };
    finish_request(first_catalog);
    expect(await first_open).toBe(first_catalog);
    const next_catalog = { entry_count: 2, fetched_at: '2026-10-08T00:01:00Z' };
    mocked_invoke.mockResolvedValueOnce(next_catalog);
    expect(await dsh_market_get_catalog()).toBe(next_catalog);
    expect(mocked_invoke).toHaveBeenCalledTimes(2);
  });

  it('retries the live command after a rejected open instead of caching the failure', async () => {
    mocked_invoke.mockRejectedValueOnce(new Error('dsh_market:registry_unreachable'));
    await expect(dsh_market_get_catalog()).rejects.toThrow('registry_unreachable');
    mocked_invoke.mockResolvedValueOnce({ entry_count: 3 });
    await expect(dsh_market_get_catalog()).resolves.toEqual({ entry_count: 3 });
    expect(mocked_invoke).toHaveBeenCalledTimes(2);
    expect(mocked_invoke.mock.calls.at(-1)?.[0]).toBe('dsh_market_get_catalog');
  });
});
