/**
 * Shared upstream base-URL normalization for the Node gateway providers.
 *
 * Mirrors src-tauri/src/proxy/endpoint.rs 1:1 — the two runtimes must resolve
 * the same stored base_url to the same request URL. A base may be (a) an API
 * root (`https://host`), (b) a versioned root (`https://host/v1`), or (c) a
 * full endpoint (`https://host/v1/messages`). Detection is anchored on parsed
 * path segments, not raw string suffixes; query strings are preserved
 * (Azure-style `?api-version=` full endpoints), fragments are dropped.
 */

export type Protocol = 'anthropic' | 'openai';

const ENDPOINT_TAIL: Record<Protocol, string[]> = {
  anthropic: ['messages'],
  openai: ['chat', 'completions'],
};
const FULL_PATH: Record<Protocol, string> = {
  anthropic: '/v1/messages',
  openai: '/v1/chat/completions',
};
const LEAF: Record<Protocol, string> = {
  anthropic: '/messages',
  openai: '/chat/completions',
};

/** A version segment like `v1` / `v2` / `v10` (lowercase `v` + digits). */
function isVersionSegment(segment: string): boolean {
  return /^v\d+$/.test(segment);
}

/** Trim trailing `/` and split the path into non-empty segments. */
function pathSegments(url: URL): string[] {
  return url.pathname.replace(/\/+$/, '').split('/').filter((s) => s.length > 0);
}

function endsWithSegments(segments: string[], tail: string[]): boolean {
  if (segments.length < tail.length) return false;
  const offset = segments.length - tail.length;
  return tail.every((segment, i) => segments[offset + i] === segment);
}

/**
 * Parse `baseUrl`, rejecting anything that is not an absolute http(s) URL
 * with a host and no embedded credentials. Returns null instead of throwing
 * so callers can fall back to their protocol default.
 */
function parseBase(baseUrl: string): URL | null {
  const trimmed = baseUrl.trim();
  if (!trimmed || /[\x00-\x1f\x7f]/.test(trimmed)) return null;
  let url: URL;
  try {
    url = new URL(trimmed);
  } catch {
    return null;
  }
  if (url.protocol !== 'http:' && url.protocol !== 'https:') return null;
  if (!url.host) return null;
  if (url.username || url.password) return null;
  return url;
}

/** Serialize a URL preserving its query but dropping any fragment. */
function render(url: URL): string {
  url.hash = '';
  let out = url.toString();
  const queryAt = out.indexOf('?');
  if (queryAt >= 0) {
    out = out.slice(0, queryAt).replace(/\/+$/, '') + out.slice(queryAt);
  } else {
    out = out.replace(/\/+$/, '');
  }
  return out;
}

/**
 * Resolve the concrete request endpoint for a provider base URL.
 *
 * - Full endpoint (path already ends in the protocol tail) → used as-is,
 *   query preserved.
 * - Versioned root (`.../v1`) → append the leaf only.
 * - Bare root → append the full versioned path.
 *
 * Falls back to the protocol default host when `baseUrl` is empty; an
 * unparseable non-empty base is normalized by string append as a last resort
 * (the Node gateway has no construction-time validation path like Rust's
 * `Provider::new`).
 */
export function endpointUrl(baseUrl: string, protocol: Protocol): string {
  const fallback =
    protocol === 'anthropic' ? 'https://api.anthropic.com' : 'https://api.openai.com/v1';
  const url = parseBase(baseUrl || fallback);
  if (!url) {
    const base = (baseUrl || fallback).replace(/\/+$/, '');
    return `${base}${FULL_PATH[protocol]}`;
  }
  const segments = pathSegments(url);
  if (endsWithSegments(segments, ENDPOINT_TAIL[protocol])) {
    return render(url);
  }
  const basePath = url.pathname.replace(/\/+$/, '');
  const last = segments[segments.length - 1];
  url.pathname =
    last !== undefined && isVersionSegment(last)
      ? `${basePath}${LEAF[protocol]}`
      : `${basePath}${FULL_PATH[protocol]}`;
  return render(url);
}

/**
 * Ordered, deduplicated candidate URLs for a model-catalog probe
 * (`GET .../models`). Full-endpoint bases are reduced to their root before
 * the `/models` candidates are built, so a stored `.../v1/messages` base
 * probes `.../v1/models`, not `.../v1/messages/v1/models`.
 */
export function modelsUrls(baseUrl: string, protocol: Protocol): string[] {
  const url = parseBase(baseUrl);
  if (!url) return [];
  const segments = pathSegments(url);
  if (endsWithSegments(segments, ENDPOINT_TAIL[protocol])) {
    segments.length -= ENDPOINT_TAIL[protocol].length;
  }
  const hasVersion = segments.length > 0 && isVersionSegment(segments[segments.length - 1]);
  const prefix = `${url.protocol}//${url.host}`;
  const root = segments.join('/');
  const join = (path: string): string =>
    root ? `${prefix}/${root}${path}` : `${prefix}${path}`;
  const out: string[] = [];
  const push = (candidate: string): void => {
    if (!out.includes(candidate)) out.push(candidate);
  };
  if (hasVersion) {
    push(join('/models'));
    const unversioned = segments.slice(0, -1).join('/');
    push(unversioned ? `${prefix}/${unversioned}/models` : `${prefix}/models`);
  } else {
    push(join('/v1/models'));
    push(join('/models'));
  }
  return out;
}

/** Derive the count_tokens endpoint from a resolved messages endpoint
 * (Rust parity: `anthropic_count_tokens_url` in
 * src-tauri/src/proxy/endpoint.rs). Only a URL whose final path segment is
 * `messages` can carry the suffix; anything else is not a messages
 * endpoint. */
export function anthropicCountTokensUrl(messagesEndpoint: string): string | undefined {
  return messagesEndpoint.endsWith('/messages')
    ? `${messagesEndpoint}/count_tokens`
    : undefined;
}
