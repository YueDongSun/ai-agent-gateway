import type { AppType } from './api/types.js';
import type { UnifiedCard } from './unified_providers.js';

const account_methods = new Set(['oauth', 'oauth2', 'chatgpt', 'subscription', 'managed_account']);
const token_keys = new Set(['refresh_token', 'refreshtoken', 'access_token', 'accesstoken', 'id_token']);
const account_containers = new Set(['auth', 'tokens', 'oauth', 'account']);

function record_of(value: unknown): Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value) ? value as Record<string, unknown> : {};
}

function account_token_values(value: unknown, tokens = new Set<string>()): Set<string> {
  for (const [key, entry] of Object.entries(record_of(value))) {
    const normalized = key.toLowerCase();
    if (token_keys.has(normalized) && typeof entry === 'string' && entry.length > 0) tokens.add(entry.trim().replace(/^Bearer\s+/i, ''));
    if (account_containers.has(normalized)) account_token_values(entry, tokens);
  }
  return tokens;
}

function quoted_value(line: string, key: string): string | null {
  const match = line.match(new RegExp(`^${key}\\s*=\\s*("(?:[^"\\\\]|\\\\.)*"|'[^']*')\\s*(?:#.*)?$`));
  if (!match) return null;
  if (match[1].startsWith("'")) return match[1].slice(1, -1);
  try { return JSON.parse(match[1]) as string; } catch { return null; }
}

/** Only an explicit, quoted top-level assignment is evidence. Comments,
 * table/profile overrides and instruction strings cannot declare a provider. */
function codex_provider(config: unknown): string | null {
  if (typeof config !== 'string') return null;
  let multiline: string | null = null;
  for (const raw_line of config.split(/\r?\n/)) {
    const line = raw_line.trim();
    if (multiline) {
      if (line.includes(multiline)) multiline = null;
      continue;
    }
    if (line.startsWith('#')) continue;
    const block = line.match(/=\s*("""|''')/);
    if (block) {
      const rest = line.slice((block.index ?? 0) + block[0].length);
      if (!rest.includes(block[1])) multiline = block[1];
      continue;
    }
    if (line.startsWith('[')) break;
    const provider = quoted_value(line, 'model_provider');
    if (provider !== null) return provider;
  }
  return null;
}

/** A display-only policy. It never changes stored records, route admission,
 * credentials, or native client state, and never infers accounts from names. */
export function is_unsupported_account_configuration(settings: unknown, app_type: AppType): boolean {
  const config = record_of(settings);
  if (account_token_values(config).size > 0) return true;
  const auth = record_of(config.auth);
  const method = config.authMethod ?? config.auth_mode ?? auth.type ?? auth.method;
  if (typeof method === 'string' && account_methods.has(method.toLowerCase())) return true;
  return app_type === 'codex' && codex_provider(config.config)?.toLowerCase().replace(/[^a-z0-9]/g, '') === 'cliproxyapi';
}

function endpoint_identity(value: unknown): string | null {
  if (typeof value !== 'string') return null;
  try {
    const endpoint = new URL(value);
    if (!['http:', 'https:'].includes(endpoint.protocol) || endpoint.username || endpoint.password) return null;
    return `${endpoint.origin}${endpoint.pathname.replace(/\/+$/, '')}`;
  } catch { return null; }
}

function independent_upstream(card: UnifiedCard): boolean {
  const upstream = card.upstream;
  if (!upstream || upstream.type === 'anthropic_passthrough') return false;
  const endpoint = endpoint_identity(upstream.baseURL);
  if (!endpoint) return false;
  const url = new URL(endpoint);
  if ((url.hostname === 'chatgpt.com' || url.hostname === 'chat.openai.com') && url.pathname.startsWith('/backend-api/')) return false;
  const config = record_of(card.preset?.settingsConfig);
  // Credential shape is not independence: an OAuth bearer can occupy apiKey.
  // Compare only in memory; neither the values nor a digest leave this policy.
  const account_tokens = account_token_values(config);
  const credential_values = [upstream.apiKey, ...Object.values(upstream.extraHeaders), ...new URL(upstream.baseURL).searchParams.values()];
  if (credential_values.some((value) => account_tokens.has(value.trim().replace(/^Bearer\s+/i, '')))) return false;
  const env = record_of(config.env);
  const configured_urls: unknown[] = [config.baseUrl, config.baseURL, config.base_url, env.ANTHROPIC_BASE_URL, env.OPENAI_BASE_URL];
  if (typeof config.config === 'string') {
    for (const line of config.config.split(/\r?\n/)) {
      const base = quoted_value(line.trim(), 'base_url');
      if (base !== null) configured_urls.push(base);
    }
  }
  const account_origins = configured_urls.flatMap((base) => {
    const identity = endpoint_identity(base);
    return identity ? [new URL(identity).origin] : [];
  });
  // Without a distinct configured origin the linked row may still be the
  // account connection. Keep its data privately rather than guessing.
  return account_origins.length > 0 && !account_origins.includes(url.origin);
}

/** Keep backend source indices for reorder; a retained independent API
 * connection is projected without its unsupported account preset fields. */
export function account_display_cards(cards: UnifiedCard[], app_type: AppType): {
  cards: { card: UnifiedCard; source_index: number }[]; omitted_count: number;
} {
  let omitted_count = 0;
  const admitted = cards.flatMap((card, source_index) => {
    if (!card.preset || !is_unsupported_account_configuration(card.preset.settingsConfig, app_type)) return [{ card, source_index }];
    omitted_count++;
    if (!independent_upstream(card)) return [];
    return [{ source_index, card: { ...card, key: card.upstream!.id, preset: null, priority: null, kind: 'standalone' as const } }];
  });
  return { cards: [...admitted.filter(({ card }) => card.kind === 'preset'), ...admitted.filter(({ card }) => card.kind === 'standalone')], omitted_count };
}
