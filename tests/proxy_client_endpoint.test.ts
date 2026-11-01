/// The renderer addresses the proxy as a client, so it must translate the
/// listen configuration into a dialable endpoint — the same rule the Rust
/// writers apply when they point a client at AAG
/// (`ProxyConfig::client_endpoint_host` in
/// `src-tauri/src/database/dao/proxy_config.rs`).
import { describe, expect, it } from 'vitest';
import { proxy_client_url, type ProxyConfig } from '../ui/src/lib/api/proxy.js';

function config(host: string, port = 8989): ProxyConfig {
  return { host, port, authToken: '', enabled: true };
}

describe('proxy_client_url', () => {
  it('translates wildcard and alias binds to the dialable loopback', () => {
    for (const [host, expected] of [
      ['', 'http://127.0.0.1:8989'],
      ['0.0.0.0', 'http://127.0.0.1:8989'],
      ['localhost', 'http://127.0.0.1:8989'],
      // Surrounding whitespace is a configuration slip, not a host name.
      ['  0.0.0.0  ', 'http://127.0.0.1:8989'],
      ['::', 'http://[::1]:8989'],
      ['::1', 'http://[::1]:8989'],
    ]) {
      expect(proxy_client_url(config(host)), host).toBe(expected);
    }
  });

  it('leaves loopback and explicit non-loopback documentation literals intact', () => {
    expect(proxy_client_url(config('127.0.0.1'))).toBe('http://127.0.0.1:8989');
    // RFC 5737 TEST-NET-1 is documentation-only, not a deployed endpoint.
    expect(proxy_client_url(config('192.0.2.5', 9000))).toBe('http://192.0.2.5:9000');
  });
});
