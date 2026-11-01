import { describe, expect, it } from 'vitest';
import { account_display_cards, is_unsupported_account_configuration } from '../ui/src/lib/account_display_policy.js';
import type { UnifiedCard } from '../ui/src/lib/unified_providers.js';

function preset_card(id: string, settings: Record<string, unknown> = {}): UnifiedCard {
  return { key: id, preset: { id, name: id, settingsConfig: settings }, upstream: null, proxyId: null, priority: 1, supplierManaged: false, kind: 'preset' };
}

function with_upstream(card: UnifiedCard, base_url: string, type = 'openai'): UnifiedCard {
  return { ...card, proxyId: `ccs-${card.key}`, upstream: {
    id: `ccs-${card.key}`, name: 'independent API connection', baseURL: base_url, type, apiKey: '',
    models: ['exact-model'], enabled: true, timeoutMs: 1000, weight: 1,
    circuitBreaker: { failureThreshold: 3, cooldownMs: 1000, halfOpenRetries: 1, successThreshold: 1 },
    modelMapping: {}, extraHeaders: {}, autoTrackModels: false,
  } };
}

describe('non-destructive account display admission', () => {
  it.each([
    { authMethod: 'OAuth2' }, { auth_mode: 'subscription' }, { auth: { type: 'chatgpt' } },
    { auth: { method: 'managed_account' } }, { auth: { tokens: { refresh_token: 'fixture-private-value' } } },
    { oauth: { account: { accessToken: 'fixture-private-value' } } }, { tokens: { id_token: 'fixture-private-value' } },
  ])('omits explicit account configurations without exposing their values', (settings) => {
    expect(is_unsupported_account_configuration(settings, 'codex')).toBe(true);
    const result = account_display_cards([preset_card('private-account', settings)], 'codex');
    expect(result).toEqual({ cards: [], omitted_count: 1 });
  });

  it('recognizes only a declared top-level Codex cliProxyAPI provider', () => {
    expect(is_unsupported_account_configuration({ config: '# config\nmodel_provider = "cliProxyAPI" # bridge\n[model_providers.cliProxyAPI]\nbase_url = "http://127.0.0.1:9000"' }, 'codex')).toBe(true);
    expect(is_unsupported_account_configuration({ config: "model_provider='cli-proxy-api'" }, 'codex')).toBe(true);
    for (const config of ['# model_provider = "cliProxyAPI"', '[profile]\nmodel_provider = "cliProxyAPI"', 'model_provider = cliProxyAPI', 'instructions = """\nmodel_provider = "cliProxyAPI"\n"""']) {
      expect(is_unsupported_account_configuration({ config }, 'codex')).toBe(false);
    }
    expect(is_unsupported_account_configuration({ config: 'model_provider = "cliProxyAPI"' }, 'claude_code')).toBe(false);
  });

  it('retains static APIs and malformed values without guessing from labels or API keys', () => {
    for (const settings of [null, [], {}, { auth: { OPENAI_API_KEY: 'fixture-static-value' } }, { authMethod: 'api_key', apiKey: 'fixture-static-value' }, { auth: { tokens: { value: 'fixture-static-value' } } }, { auth: { access_token: '' } }, { authMethod: 4, oauth: [] }, { name: 'OAuth', model_provider: 'cliProxyAPI' }]) {
      expect(is_unsupported_account_configuration(settings, 'codex')).toBe(false);
    }
  });

  it('retains original source indices and never mutates the stored card data', () => {
    const cards = [preset_card('api-first'), preset_card('account', { authMethod: 'oauth' }), preset_card('api-last')];
    const original = structuredClone(cards);
    expect(account_display_cards(cards, 'codex').cards.map(({ card, source_index }) => [card.key, source_index])).toEqual([['api-first', 0], ['api-last', 2]]);
    expect(cards).toEqual(original);
  });

  it('retains an unrelated static upstream as a standalone card without account fields', () => {
    const card = with_upstream(preset_card('account', { baseUrl: 'https://account.example.invalid', auth: { refresh_token: 'fixture-private-value' } }), 'http://127.0.0.1:11434/v1');
    const result = account_display_cards([card], 'codex');
    expect(result.omitted_count).toBe(1);
    expect(result.cards[0]).toMatchObject({ source_index: 0, card: { preset: null, kind: 'standalone', priority: null, upstream: card.upstream } });
    expect(JSON.stringify(result)).not.toContain('fixture-private-value');
  });

  it('never salvages the account origin or its token as a standalone API', () => {
    const source = preset_card('account', { baseUrl: 'https://api.anthropic.com', auth: { access_token: 'fixture-oauth-secret' } });
    for (const [base, key] of [['https://api.anthropic.com', 'fixture-oauth-secret'], ['https://api.anthropic.com/v1', 'fixture-different-key'], ['https://independent.example.invalid/v1', 'fixture-oauth-secret']]) {
      const card = with_upstream(source, base, 'anthropic');
      card.upstream!.apiKey = key;
      expect(account_display_cards([card], 'codex')).toEqual({ cards: [], omitted_count: 1 });
    }
  });

  it('requires independent coordinates and rejects copied account bearer headers', () => {
    const source = preset_card('account', { baseUrl: 'https://account.example.invalid', auth: { access_token: 'fixture-oauth-secret' } });
    const independent = with_upstream(source, 'https://api.example.invalid/v1');
    independent.upstream!.apiKey = 'fixture-independent-static-key';
    expect(account_display_cards([independent], 'codex').cards).toHaveLength(1);
    independent.upstream!.extraHeaders.Authorization = 'Bearer fixture-oauth-secret';
    expect(account_display_cards([independent], 'codex')).toEqual({ cards: [], omitted_count: 1 });
    const unknown = with_upstream(preset_card('unknown-account', { authMethod: 'oauth' }), 'https://api.example.invalid');
    unknown.upstream!.apiKey = 'fixture-unproven-key';
    expect(account_display_cards([unknown], 'codex')).toEqual({ cards: [], omitted_count: 1 });
  });

  it('omits linked account-only and passthrough upstreams with the account preset', () => {
    for (const card of [with_upstream(preset_card('account', { authMethod: 'oauth' }), 'https://chatgpt.com/backend-api/codex'), with_upstream(preset_card('account', { authMethod: 'oauth' }), 'https://api.anthropic.com', 'anthropic_passthrough'), with_upstream(preset_card('account', { authMethod: 'oauth', baseUrl: 'http://127.0.0.1:9000/v1' }), 'http://127.0.0.1:9000/v1')]) {
      expect(account_display_cards([card], 'codex')).toEqual({ cards: [], omitted_count: 1 });
    }
  });
});
