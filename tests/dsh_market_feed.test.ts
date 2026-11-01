import { afterEach, describe, expect, it, vi } from 'vitest';
import { fetch_market_catalog } from '../ui/src/lib/api/dsh_market_feed.js';
import { normalize_market_feed } from '../ui/src/lib/api/dsh_market_normalize.js';

const fetched_at = '2026-10-08T00:00:00.000Z';

afterEach(() => vi.useRealTimers());

function feed(plugin: Record<string, unknown>, generated_at = fetched_at): Record<string, unknown> {
  return { schemaVersion: 2, generatedAt: generated_at, plugins: [plugin] };
}

function plugin(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    fullName: 'example/plugin',
    name: 'Plugin display name',
    owner: 'Example owner',
    stars: 12,
    description: 'A small theme tool',
    descriptionZh: '主题工具',
    topics: ['theme'],
    updatedAt: fetched_at,
    ...overrides,
  };
}

function response_for(value: unknown, init: ResponseInit = {}): Response {
  return new Response(JSON.stringify(value), {
    status: 200,
    headers: { 'content-type': 'application/json' },
    ...init,
  });
}

describe('DSH live marketplace feed', () => {
  it('normalizes current site fields and derives safe repository metadata', () => {
    const catalog = normalize_market_feed(feed(plugin()), fetched_at);
    const entry = catalog.entries[0];
    expect(entry.repo_url).toBe('https://github.com/example/plugin');
    expect(entry.name).toBe('plugin');
    expect(entry.author).toBe('example');
    expect(entry.package_name).toBeNull();
    expect(entry.version).toBeNull();
    expect(entry.source_commit).toBeNull();
    expect(entry.source_commit_url).toBeNull();
    expect(entry.description_en).toBe('A small theme tool');
    expect(entry.description_zh).toBe('主题工具');
    expect(entry.categories).toContain('personalization');
    expect(entry.verification).toBe('not_audited');
    expect(catalog.registry_commit).toBe('');
    expect(catalog.cache_status).toBe('refreshed');
  });

  it('uses each live response and recalculates classification from changed content', async () => {
    const fetch_impl = vi.fn<Fetch_impl>()
      .mockResolvedValueOnce(response_for(feed(plugin({ description: 'A theme customization tool', topics: ['theme'] }))))
      .mockResolvedValueOnce(response_for(feed(plugin({ description: 'An MCP integration for messaging', topics: ['mcp', 'integration'] }), '2026-10-09T00:00:00.000Z')));
    const first = await fetch_market_catalog(fetch_impl);
    const second = await fetch_market_catalog(fetch_impl);
    expect(fetch_impl).toHaveBeenCalledTimes(2);
    expect(first.entries[0].categories).toContain('personalization');
    expect(second.entries[0].categories).toContain('integrations');
    expect(second.registry_generated_at).toBe('2026-10-09T00:00:00.000Z');
    expect(first.entries[0].description).not.toBe(second.entries[0].description);
  });

  it('does not invent a registry revision from feed metadata', () => {
    const normalized = normalize_market_feed(feed(plugin({ source_commit: 'a'.repeat(40), description_en: 'Maintained in English' })), fetched_at);
    expect(normalized.registry_commit).toBe('');
    expect(normalized.entries[0].source_commit).toBe('a'.repeat(40));
    expect(normalized.entries[0].description_en).toBe('Maintained in English');
  });

  it('preserves language, license, client, and validated install metadata', () => {
    const english = normalize_market_feed(feed(plugin({
      description: 'A private network client',
      descriptionZh: undefined,
      license: 'MIT email@example.com',
      hasClient: true,
      install: { source: 'github' },
      sourceCommit: 'b'.repeat(40),
    })), fetched_at).entries[0];
    const chinese = normalize_market_feed(feed(plugin({ description: '中文主题工具', descriptionEn: undefined, descriptionZh: undefined })), fetched_at).entries[0];
    expect(english.description_en).toBe('A private network client');
    expect(english.description_zh).toBeNull();
    expect(english.license).toBe('MIT [redacted_email]');
    expect(english.has_client).toBe(true);
    expect(english.install_method).toBe('github');
    expect(english.install_spec).toBe(`github:example/plugin#${'b'.repeat(40)}`);
    expect(chinese.description_en).toBeNull();
    expect(chinese.description_zh).toBe('中文主题工具');
    expect(normalize_market_feed(feed(plugin({ install: { source: 'npm' }, packageName: 'example-plugin', version: '1.2.3' })), fetched_at).entries[0].install_spec)
      .toBe('example-plugin@1.2.3');
    expect(normalize_market_feed(feed(plugin({ packageName: 'example-plugin', version: '1.2.3' })), fetched_at).entries[0].install_method)
      .toBe('unknown');
    expect(() => normalize_market_feed(feed(plugin({ hasClient: 'yes' })), fetched_at)).toThrow('dsh_market:invalid_client_flag');
  });

  it('scrubs private IPv6 link-local and private IPv4-mapped addresses', () => {
    const entry = normalize_market_feed(feed(plugin({
      description: 'Uses fe80::1 and [::ffff:192.168.1.8] on a local network',
    })), fetched_at).entries[0];
    expect(entry.description).not.toContain('fe80::1');
    expect(entry.description).not.toContain('192.168.1.8');
    expect(entry.description).toContain('[redacted_private_network]');
  });

  it('rejects unsupported schemas, duplicate names, and mismatched provenance URLs', () => {
    expect(() => normalize_market_feed({ ...feed(plugin()), schemaVersion: 1 }, fetched_at)).toThrow('dsh_market:invalid_feed');
    expect(() => normalize_market_feed({ ...feed(plugin()), plugins: [plugin(), plugin()] }, fetched_at)).toThrow('dsh_market:duplicate_entry');
    expect(() => normalize_market_feed(feed(plugin({ htmlUrl: 'https://attacker.invalid/example/plugin' })), fetched_at)).toThrow('dsh_market:invalid_repository_url');
    expect(() => normalize_market_feed(feed(plugin({ verifiedCommit: 'latest' })), fetched_at)).toThrow('dsh_market:invalid_commit');
  });

  it('bounds body size and sends anonymous no-cache requests to the fixed registry endpoint', async () => {
    const fetch_impl = vi.fn<Fetch_impl>().mockResolvedValue(response_for(feed(plugin())));
    await fetch_market_catalog(fetch_impl);
    const [url, options] = fetch_impl.mock.calls[0];
    expect(url).toBe('https://dsh.market/plugins.json');
    expect(options).toMatchObject({ credentials: 'omit', cache: 'no-store', redirect: 'error', method: 'GET' });
    const too_large = new Response('x', { headers: { 'content-length': String(64 * 1024 * 1024 + 1) } });
    await expect(fetch_market_catalog(vi.fn<Fetch_impl>().mockResolvedValue(too_large))).rejects.toThrow('dsh_market:feed_too_large');
    const streamed_too_large = new Response(new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(new Uint8Array(63 * 1024 * 1024));
        controller.enqueue(new Uint8Array(1024 * 1024 + 1));
      },
    }));
    await expect(fetch_market_catalog(vi.fn<Fetch_impl>().mockResolvedValue(streamed_too_large)))
      .rejects.toThrow('dsh_market:feed_too_large');
  });

  it('reports HTTP and malformed JSON failures with stable adapter errors', async () => {
    await expect(fetch_market_catalog(vi.fn<Fetch_impl>().mockResolvedValue(new Response('', { status: 503 }))))
      .rejects.toThrow('dsh_market:registry_http_error');
    await expect(fetch_market_catalog(vi.fn<Fetch_impl>().mockResolvedValue(new Response('{'))))
      .rejects.toThrow('dsh_market:invalid_json');
    let body_cancelled = false;
    const failed_response = new Response(new ReadableStream<Uint8Array>({ cancel() { body_cancelled = true; } }), { status: 503 });
    await expect(fetch_market_catalog(vi.fn<Fetch_impl>().mockResolvedValue(failed_response)))
      .rejects.toThrow('dsh_market:registry_http_error');
    expect(body_cancelled).toBe(true);
  });

  it('aborts the request after the total 30 second timeout', async () => {
    vi.useFakeTimers();
    let observed_signal: AbortSignal | undefined;
    const fetch_impl = vi.fn<Fetch_impl>((_input, init) => new Promise<Response>((_resolve, reject) => {
      observed_signal = init?.signal as AbortSignal;
      observed_signal.addEventListener('abort', () => reject(new DOMException('Aborted', 'AbortError')));
    }));
    const pending = fetch_market_catalog(fetch_impl);
    const rejection = expect(pending).rejects.toThrow('dsh_market:request_timeout');
    await vi.advanceTimersByTimeAsync(30_000);
    await rejection;
    expect(observed_signal?.aborted).toBe(true);
  });
});

type Fetch_impl = typeof fetch;
