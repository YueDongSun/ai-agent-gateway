import type { Market_catalog } from './dsh_market.js';
import { registry_url, max_feed_bytes, request_timeout_ms } from './dsh_market_config.js';
import { normalize_market_feed } from './dsh_market_normalize.js';

type Fetch_impl = typeof fetch;

function fail(code: string): never {
  throw new Error(`dsh_market:${code}`);
}

async function read_bounded_body(response: Response, controller: AbortController): Promise<string> {
  const declared_length = Number(response.headers.get('content-length') ?? 0);
  if (Number.isFinite(declared_length) && declared_length > max_feed_bytes) {
    await response.body?.cancel();
    fail('feed_too_large');
  }
  if (!response.body) fail('empty_response');
  const reader = response.body.getReader();
  const chunks: Uint8Array[] = [];
  let length = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      length += value.byteLength;
      if (length > max_feed_bytes) {
        controller.abort();
        await reader.cancel();
        fail('feed_too_large');
      }
      chunks.push(value);
    }
  } finally {
    reader.releaseLock();
  }
  const bytes = new Uint8Array(length);
  let offset = 0;
  for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.byteLength; }
  try { return new TextDecoder('utf-8', { fatal: true }).decode(bytes); }
  catch { fail('invalid_utf8'); }
}

export async function fetch_market_catalog(fetch_impl: Fetch_impl = fetch): Promise<Market_catalog> {
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), request_timeout_ms);
  try {
    let response: Response;
    try {
      response = await fetch_impl(registry_url, {
        method: 'GET',
        signal: controller.signal,
        redirect: 'error',
        credentials: 'omit',
        cache: 'no-store',
        headers: { accept: 'application/json' },
      });
    } catch (error) {
      if (controller.signal.aborted) fail('request_timeout');
      throw new Error('dsh_market:network_error', { cause: error });
    }
    if (!response.ok) {
      await response.body?.cancel();
      fail(response.status === 429 ? 'registry_rate_limited' : 'registry_http_error');
    }
    let body: string;
    try { body = await read_bounded_body(response, controller); }
    catch (error) {
      if (controller.signal.aborted && !(error instanceof Error && error.message === 'dsh_market:feed_too_large')) fail('request_timeout');
      throw error;
    }
    let feed: unknown;
    try { feed = JSON.parse(body); }
    catch { fail('invalid_json'); }
    return normalize_market_feed(feed);
  } finally {
    clearTimeout(timeout);
  }
}
