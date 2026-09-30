import type { Market_catalog } from '../api/dsh_market.js';

let bundled_catalog: Promise<Market_catalog> | null = null;

/** Load the actual checked-in GitHub snapshot only when the market is opened. */
function load_catalog(): Promise<Market_catalog> {
  bundled_catalog ??= import('../../../../shared/dsh_market_catalog.json').then((module) => ({
    ...module.default,
    cache_status: 'bundled',
    warning: null,
  }) as Market_catalog);
  return bundled_catalog;
}

export async function mock_dsh_market_command(command: string): Promise<unknown> {
  if (command === 'dsh_market_get_catalog' || command === 'dsh_market_refresh') return load_catalog();
  if (command === 'dsh_open_marketplace' || command === 'dsh_market_open_source') return null;
  throw new Error(`Unknown DSH marketplace command: ${command}`);
}
