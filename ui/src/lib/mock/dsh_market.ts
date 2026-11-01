import type { Market_catalog } from '../api/dsh_market.js';
import { fetch_market_catalog } from '../api/dsh_market_feed.js';

let pending_catalog: Promise<Market_catalog> | null = null;

/** Browser marketplace uses the same public feed as the native adapter. */
function load_catalog(): Promise<Market_catalog> {
  pending_catalog ??= fetch_market_catalog().finally(() => { pending_catalog = null; });
  return pending_catalog;
}

export async function mock_dsh_market_command(command: string): Promise<unknown> {
  if (command === 'dsh_market_get_catalog' || command === 'dsh_market_refresh') return load_catalog();
  if (command === 'dsh_open_marketplace' || command === 'dsh_market_open_source') return null;
  throw new Error(`Unknown DSH marketplace command: ${command}`);
}
