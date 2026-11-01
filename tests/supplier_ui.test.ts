/// Supplier UI contract tests.
///
/// Covers the pure selection/secret/validation helpers the suppliers page is
/// built on, and the in-memory demo mock's contract against the Rust service
/// semantics (write-only secrets, optimistic generation CAS, endpoint auth
/// modes, model ordering, stable `supplier_*` codes). The mock never touches
/// files, networks, or real credentials — it is synthetic state by design.
import { describe, expect, it, vi } from 'vitest';
import { createRequire } from 'node:module';
import { model_catalog_model, supplier_discover_models } from '../ui/src/lib/api/suppliers.js';
import { Supplier_arena_evidence } from '../ui/src/components/suppliers/supplier_assessment.js';
import { I18nCtx, type I18nContextValue } from '../ui/src/lib/ctx.js';
import { t as translate, type Locale } from '../ui/src/i18n/index.js';
import {
  CAPABILITY_ALIASES,
  build_discover_models_input,
  capability_tier_for_alias,
  classify_supplier_error,
  client_snippets,
  endpoint_auth_fields,
  is_supplier_feature_unavailable,
  is_supplier_generation_conflict,
  is_valid_supplier_url,
  preview_routing,
  redact_url_for_display,
  resolve_model_tier,
  secret_write_value,
  sort_supplier_models,
  supplier_protocol_path,
  validate_supplier_draft,
  type Routing_candidate,
  type Model_arena_assessment,
  type Supplier_provenance,
} from '../ui/src/lib/api/suppliers_logic.js';
import { mock_supplier_command } from '../ui/src/lib/mock/suppliers.js';

const provenance: Supplier_provenance = {
  sourceKind: 'manual',
  sourceRecordRef: null,
  sourceAppType: null,
  sourceEndpointRef: null,
  importBatchRef: null,
  fetchedAt: null,
  contentVersion: null,
  confidence: null,
};

function model(overrides: Partial<Routing_candidate['model']> = {}): Routing_candidate['model'] {
  return {
    id: 'model_1',
    modelId: 'claude-opus-4-8',
    displayName: null,
    enabled: true,
    supportsTools: true,
    supportsImages: true,
    contextWindow: null,
    maxOutputTokens: null,
    tierOverride: null,
    sortIndex: 0,
    source: 'manual',
    provenance,
    ...overrides,
  };
}

function candidate(overrides: Partial<Routing_candidate> = {}): Routing_candidate {
  return {
    supplierId: 'sup_a',
    supplierName: 'A',
    supplierEnabled: true,
    supplierPriority: 100,
    endpointProtocol: 'anthropic',
    endpointEnabled: true,
    endpointPriority: 10,
    endpointConfigured: true,
    model: model(),
    ...overrides,
  };
}

describe('supplier error classification', () => {
  it('passes stable supplier_* codes through untouched', () => {
    const info = classify_supplier_error('supplier_endpoint_url_invalid');
    expect(info).toEqual({ kind: 'code', code: 'supplier_endpoint_url_invalid', text: 'supplier_endpoint_url_invalid' });
  });

  it('flags generation conflicts and in-use deletes distinctly', () => {
    expect(is_supplier_generation_conflict('supplier_config_generation_conflict')).toBe(true);
    expect(classify_supplier_error(new Error('supplier_in_use')).kind).toBe('in_use');
  });

  it('keeps the malformed vendor sentinel as a stable string', () => {
    const info = classify_supplier_error('malformed vendor record');
    expect(info.kind).toBe('malformed_vendor');
    expect(info.text).toBe('malformed vendor record');
  });

  it('treats missing commands as feature-unavailable, not fake success', () => {
    expect(is_supplier_feature_unavailable('Command supplier_import_preview not found')).toBe(true);
    expect(is_supplier_feature_unavailable('demo_command_unsupported')).toBe(true);
    expect(classify_supplier_error('Command supplier_import_preview not found').kind).toBe('feature_unavailable');
  });

  it('never leaks raw backend error text for unknown failures', () => {
    const raw = 'SqliteFailure: table suppliers, secret=sk-raw-value, url=https://private.example';
    const info = classify_supplier_error(raw);
    expect(info.kind).toBe('unknown');
    expect(info.text).toBe('supplier_error_unknown');
    expect(info.text).not.toContain('sk-raw-value');
    expect(info.text).not.toContain('private.example');
  });
});

describe('write-only secret decisions', () => {
  it('omits the field to preserve and sends empty string to clear', () => {
    expect(secret_write_value('preserve', 'sk-new')).toBeUndefined();
    expect(secret_write_value('clear', 'sk-new')).toBe('');
    expect(secret_write_value('set', 'sk-new')).toBe('sk-new');
  });

  it('returns to the shared supplier key without a key payload', () => {
    expect(endpoint_auth_fields({ authMode: 'shared', keyIntent: 'set', keyValue: 'sk' })).toEqual({
      useSharedApiKey: true,
    });
  });

  it('disables endpoint auth with an explicit empty override', () => {
    expect(endpoint_auth_fields({ authMode: 'none', keyIntent: 'preserve', keyValue: '' })).toEqual({
      apiKeyOverride: '',
      useSharedApiKey: false,
    });
  });

  it('preserves an endpoint override when the key field is untouched', () => {
    const fields = endpoint_auth_fields({ authMode: 'override', keyIntent: 'preserve', keyValue: '' });
    expect('apiKeyOverride' in fields).toBe(false);
    expect(fields.useSharedApiKey).toBe(false);
  });

  it('sets and clears an endpoint override explicitly', () => {
    expect(endpoint_auth_fields({ authMode: 'override', keyIntent: 'set', keyValue: 'sk-e' })).toEqual({
      apiKeyOverride: 'sk-e',
      useSharedApiKey: false,
    });
    expect(endpoint_auth_fields({ authMode: 'override', keyIntent: 'clear', keyValue: '' })).toEqual({
      apiKeyOverride: '',
      useSharedApiKey: false,
    });
  });
});

describe('draft validation mirrors the Rust service', () => {
  it('accepts a clean draft', () => {
    expect(
      validate_supplier_draft({
        name: 'Acme',
        baseUrl: 'https://api.acme.example.com',
        priority: 100,
        endpoints: [{ protocol: 'anthropic', endpointUrl: '' }],
      }),
    ).toEqual([]);
  });

  it('rejects bad names, urls, priorities, and protocols', () => {
    // Synthetic fixture URLs: sentinel rejection cases, not live hosts.
    expect(is_valid_supplier_url('https://user:pass@host.example.com')).toBe(false);
    expect(is_valid_supplier_url('https://host.example.com/?token=1')).toBe(false);
    expect(is_valid_supplier_url('file:///etc/passwd')).toBe(false);
    const codes = validate_supplier_draft({
      name: ' ',
      baseUrl: 'ftp://host.example.com',
      priority: 9_000_000,
      endpoints: [{ protocol: 'smtp', endpointUrl: 'https://ok.example.com' }],
    });
    expect(codes).toContain('supplier_name_invalid');
    expect(codes).toContain('supplier_endpoint_url_invalid');
    expect(codes).toContain('supplier_priority_out_of_range');
    expect(codes).toContain('supplier_protocol_unsupported');
  });

  it('rejects duplicate protocols and more than four endpoints', () => {
    const duplicate = validate_supplier_draft({
      name: 'Acme',
      priority: 100,
      endpoints: [
        { protocol: 'anthropic', endpointUrl: '' },
        { protocol: 'anthropic', endpointUrl: '' },
      ],
    });
    expect(duplicate).toContain('supplier_endpoint_duplicate');
    const many = validate_supplier_draft({
      name: 'Acme',
      priority: 100,
      endpoints: ['anthropic', 'openai-chat', 'openai-responses', 'gemini', 'anthropic'].map((protocol) => ({
        protocol,
        endpointUrl: '',
      })),
    });
    expect(many).toContain('supplier_endpoints_too_many');
  });
});

describe('url display redaction', () => {
  it('strips credentials, query strings, and fragments and flags review', () => {
    const stripped = redact_url_for_display('https://user:pass@host.example.com/path?token=1#frag');
    expect(stripped.url).toBe('https://host.example.com/path');
    expect(stripped.needs_review).toBe(true);
  });

  it('passes clean urls through without a review flag', () => {
    const clean = redact_url_for_display('https://host.example.com');
    expect(clean.needs_review).toBe(false);
    expect(clean.url).toContain('host.example.com');
    expect(redact_url_for_display('not a url')).toEqual({ url: null, needs_review: true });
  });
});

describe('model order and capability tiers', () => {
  it('keeps model order stable by sort index', () => {
    const sorted = sort_supplier_models([
      { sortIndex: 2, modelId: 'b' },
      { sortIndex: 1, modelId: 'c' },
      { sortIndex: 1, modelId: 'a' },
    ]);
    expect(sorted.map((row) => row.modelId)).toEqual(['a', 'c', 'b']);
  });

  it('prefers manual overrides and infers family tiers with confidence', () => {
    expect(resolve_model_tier('claude-opus-4-8', 0)).toMatchObject({ tier: 0, origin: 'manual' });
    expect(resolve_model_tier('claude-fable-5', null)).toMatchObject({ tier: 3, origin: 'inferred' });
    expect(resolve_model_tier('claude-opus-4-8', null, 0.42)).toMatchObject({ tier: 2, origin: 'inferred', confidence: 0.42 });
    expect(resolve_model_tier('mystery-model-9', null)).toMatchObject({ tier: null, origin: 'pending' });
  });

  it('maps capability aliases to tiers and leaves unknown names pending', () => {
    expect(capability_tier_for_alias('claude-fable-5')).toBe(3);
    expect(capability_tier_for_alias('claude-opus-4-8')).toBe(2);
    expect(capability_tier_for_alias('claude-sonnet-5')).toBe(1);
    expect(capability_tier_for_alias('totally-unknown')).toBeNull();
    expect(CAPABILITY_ALIASES.length).toBeGreaterThan(0);
  });
});

describe('routing preview rules', () => {
  it('filters hard capability requirements and orders by tier then priority', () => {
    const strong = candidate({ supplierId: 'strong', supplierPriority: 200, model: model({ modelId: 'claude-fable-5' }) });
    const weak = candidate({ supplierId: 'weak', supplierPriority: 10, model: model({ modelId: 'claude-sonnet-5' }) });
    const no_tools = candidate({
      supplierId: 'no_tools',
      supplierPriority: 5,
      model: model({ modelId: 'claude-opus-4-8', supportsTools: false }),
    });
    const disabled = candidate({ supplierId: 'off', model: model({ modelId: 'claude-opus-4-8', enabled: false }) });
    const preview = preview_routing([weak, no_tools, strong, disabled], {
      needsTools: true,
      needsImages: false,
      minTier: 3,
    });
    expect(preview.eligible.map((row) => row.supplierId)).toEqual(['strong', 'weak']);
    const reasons = new Map(preview.excluded.map((row) => [row.candidate.supplierId, row.reason]));
    expect(reasons.get('no_tools')).toBe('missing_tools');
    expect(reasons.get('off')).toBe('model_disabled');
  });

  it('marks endpoint and supplier exclusions distinctly', () => {
    const preview = preview_routing(
      [
        candidate({ supplierId: 's_off', supplierEnabled: false }),
        candidate({ supplierId: 'e_off', endpointEnabled: false }),
        candidate({ supplierId: 'e_unconf', endpointConfigured: false }),
      ],
      { needsTools: false, needsImages: false, minTier: null },
    );
    expect(preview.eligible).toEqual([]);
    expect(new Set(preview.excluded.map((row) => row.reason))).toEqual(
      new Set(['supplier_disabled', 'endpoint_disabled', 'endpoint_unconfigured']),
    );
  });
});

describe('client snippets', () => {
  it('exposes the four protocol paths without embedding secrets', () => {
    const snippets = client_snippets('http://127.0.0.1:8989');
    expect(snippets.map((snippet) => snippet.protocol)).toEqual([
      'anthropic',
      'openai-chat',
      'openai-responses',
      'gemini',
    ]);
    expect(supplier_protocol_path('anthropic')).toBe('/v1/messages');
    expect(supplier_protocol_path('openai-chat')).toBe('/v1/chat/completions');
    expect(supplier_protocol_path('openai-responses')).toBe('/v1/responses');
    expect(supplier_protocol_path('gemini')).toBe('/v1beta/models/{model}:generateContent');
    for (const snippet of snippets) {
      expect(snippet.body).not.toContain('sk-');
      expect(snippet.body).toContain('http://127.0.0.1:8989');
      expect(snippet.body).toContain('<aag-gateway-token>');
    }
    expect(snippets.find((snippet) => snippet.protocol === 'openai-responses')?.body).toContain('wire_api = "responses"');
    expect(snippets.find((snippet) => snippet.protocol === 'gemini')?.body).toContain('GOOGLE_GEMINI_BASE_URL');
  });
});

describe('discover-models payload decisions', () => {
  it('sends only typed values on create and defaults to the shared supplier key', () => {
    const input = build_discover_models_input({
      protocol: 'anthropic',
      baseUrl: 'https://api.acme.example.com',
      endpointUrl: '',
      apiKeyIntent: 'set',
      apiKeyValue: 'sk-typed',
      authMode: 'shared',
      endpointKeyIntent: 'keep',
      endpointKeyValue: '',
      extraHeaders: null,
    });
    expect(input).toEqual({
      protocol: 'anthropic',
      baseUrl: 'https://api.acme.example.com',
      apiKey: 'sk-typed',
      useSharedApiKey: true,
    });
  });

  it('omits untouched fields on edit so the backend reuses stored state', () => {
    const input = build_discover_models_input({
      supplierId: 'supplier_acme',
      protocol: 'anthropic',
      baseUrl: '',
      endpointUrl: '',
      apiKeyIntent: 'preserve',
      apiKeyValue: '',
      authMode: 'override',
      endpointKeyIntent: 'preserve',
      endpointKeyValue: '',
      extraHeaders: null,
    });
    expect(input.supplierId).toBe('supplier_acme');
    expect('apiKey' in input).toBe(false);
    expect('baseUrl' in input).toBe(false);
    expect('endpointUrl' in input).toBe(false);
    expect(input.useSharedApiKey).toBe(false);
    expect('apiKeyOverride' in input).toBe(false);
    expect('extraHeaders' in input).toBe(false);
  });

  it('sends replacements, anonymous auth, cleared keys, and edited headers explicitly', () => {
    const replaced = build_discover_models_input({
      supplierId: 'supplier_acme',
      protocol: 'gemini',
      baseUrl: '',
      endpointUrl: 'https://edge.example.com',
      apiKeyIntent: 'set',
      apiKeyValue: 'sk-new',
      authMode: 'shared',
      endpointKeyIntent: 'keep',
      endpointKeyValue: '',
      extraHeaders: null,
    });
    expect(replaced.apiKey).toBe('sk-new');
    expect(replaced.endpointUrl).toBe('https://edge.example.com');
    expect(replaced.useSharedApiKey).toBe(true);

    const anonymous = build_discover_models_input({
      protocol: 'openai-chat',
      baseUrl: 'https://relay.example.com',
      endpointUrl: '',
      apiKeyIntent: 'set',
      apiKeyValue: '',
      authMode: 'none',
      endpointKeyIntent: 'preserve',
      endpointKeyValue: '',
      extraHeaders: null,
    });
    // A blank `set` has no key to send; auth-off probes anonymously.
    expect('apiKey' in anonymous).toBe(false);
    expect(anonymous.apiKeyOverride).toBe('');
    expect(anonymous.useSharedApiKey).toBe(false);

    const cleared = build_discover_models_input({
      supplierId: 'supplier_acme',
      protocol: 'anthropic',
      baseUrl: '',
      endpointUrl: '',
      apiKeyIntent: 'clear',
      apiKeyValue: '',
      authMode: 'shared',
      endpointKeyIntent: 'keep',
      endpointKeyValue: '',
      extraHeaders: null,
    });
    expect(cleared.apiKey).toBe('');

    const edited = build_discover_models_input({
      protocol: 'anthropic',
      baseUrl: 'https://api.acme.example.com',
      endpointUrl: '',
      apiKeyIntent: 'set',
      apiKeyValue: 'sk-typed',
      authMode: 'shared',
      endpointKeyIntent: 'keep',
      endpointKeyValue: '',
      extraHeaders: { 'x-tenant': 'a' },
    });
    expect(edited.extraHeaders).toEqual({ 'x-tenant': 'a' });
  });
});

describe('demo supplier mock contract', () => {
  it('lists seeded suppliers without ever exposing stored keys', async () => {
    const list = (await mock_supplier_command('supplier_list', {})) as {
      generation: number;
      suppliers: Array<{ id: string; apiKeyConfigured: boolean; [key: string]: unknown }>;
    };
    expect(list.suppliers.length).toBeGreaterThan(0);
    for (const supplier of list.suppliers) {
      expect('apiKey' in supplier).toBe(false);
      expect(JSON.stringify(supplier)).not.toContain('sk-demo');
    }
    const acme = list.suppliers.find((row) => row.id === 'supplier_acme');
    expect(acme?.apiKeyConfigured).toBe(true);
  });

  it('creates, preserves, and clears secrets with explicit intents', async () => {
    const before = (await mock_supplier_command('supplier_list', {})) as { generation: number };
    // Synthetic fixture secret: a sentinel value for the mock command layer.
    const created = (await mock_supplier_command('supplier_save', {
      input: {
        expectedGeneration: before.generation,
        name: 'Contract supplier',
        baseUrl: 'https://contract.example.com',
        apiKey: 'sk-contract-1', // fixture sentinel value
        enabled: true,
        priority: 50,
        endpoints: [{ protocol: 'anthropic', useSharedApiKey: true, enabled: true, priority: 10 }],
      },
    })) as { supplierId: string; generation: number; setupError?: string | null };
    expect(created.supplierId).toBeTruthy();
    // The save itself succeeds; the automatic setup outcome rides along
    // separately and reports success (null) in the demo.
    expect(created.setupError ?? null).toBeNull();

    const after_create = (await mock_supplier_command('supplier_get', { id: created.supplierId })) as {
      supplier: { apiKeyConfigured: boolean; endpoints: Array<{ authMode: string; models: unknown[] }> };
    };
    expect(after_create.supplier.apiKeyConfigured).toBe(true);
    expect(after_create.supplier.endpoints[0].authMode).toBe('shared');
    expect(after_create.supplier.endpoints[0].models).toEqual([]);

    // Omitting apiKey preserves the stored key.
    const kept = (await mock_supplier_command('supplier_save', {
      input: {
        id: created.supplierId,
        expectedGeneration: created.generation,
        name: 'Contract supplier renamed',
        enabled: true,
        priority: 50,
      },
    })) as { generation: number };
    const after_keep = (await mock_supplier_command('supplier_get', { id: created.supplierId })) as {
      supplier: { name: string; apiKeyConfigured: boolean };
    };
    expect(after_keep.supplier.name).toBe('Contract supplier renamed');
    expect(after_keep.supplier.apiKeyConfigured).toBe(true);

    // Explicit empty apiKey clears it.
    await mock_supplier_command('supplier_save', {
      input: {
        id: created.supplierId,
        expectedGeneration: kept.generation,
        name: 'Contract supplier renamed',
        apiKey: '',
        enabled: true,
        priority: 50,
      },
    });
    const after_clear = (await mock_supplier_command('supplier_get', { id: created.supplierId })) as {
      supplier: { apiKeyConfigured: boolean };
    };
    expect(after_clear.supplier.apiKeyConfigured).toBe(false);
  });

  it('rejects stale generations with the stable conflict code', async () => {
    const list = (await mock_supplier_command('supplier_list', {})) as { generation: number };
    await expect(
      mock_supplier_command('supplier_save', {
        input: {
          expectedGeneration: list.generation - 1,
          name: 'Stale',
          enabled: true,
          priority: 100,
        },
      }),
    ).rejects.toThrow('supplier_config_generation_conflict');
  });

  it('replaces one endpoint model list and keeps list order', async () => {
    const list = (await mock_supplier_command('supplier_list', {})) as {
      generation: number;
      suppliers: Array<{ id: string }>;
    };
    const acme = list.suppliers.find((row) => row.id === 'supplier_acme')!;
    const first = (await mock_supplier_command('supplier_models_update', {
      input: {
        supplierId: acme.id,
        protocol: 'anthropic',
        expectedGeneration: list.generation,
        models: [
          { modelId: 'claude-sonnet-5', enabled: true, tierOverride: 1 },
          { modelId: 'claude-opus-4-8', enabled: true },
          { modelId: 'claude-haiku-4-5', enabled: false, supportsTools: false, supportsImages: false },
        ],
      },
    })) as { generation: number };
    const detail = (await mock_supplier_command('supplier_get', { id: acme.id })) as {
      supplier: {
        endpoints: Array<{ protocol: string; models: Array<{ modelId: string; sortIndex: number }> }>;
      };
    };
    const anthropic = detail.supplier.endpoints.find((endpoint) => endpoint.protocol === 'anthropic')!;
    expect(anthropic.models.map((row) => row.modelId)).toEqual(['claude-sonnet-5', 'claude-opus-4-8', 'claude-haiku-4-5']);
    expect(anthropic.models.map((row) => row.sortIndex)).toEqual([0, 1, 2]);
    expect(first.generation).toBeGreaterThan(list.generation);
  });

  it('reports status counts and refuses official forwarding claims', async () => {
    const status = (await mock_supplier_command('supplier_status', {})) as {
      supplierCount: number;
      configuredEndpointCount: number;
      pendingUnsupportedCount: number;
      officialAuthForwardingSupported: boolean;
    };
    expect(status.supplierCount).toBeGreaterThan(0);
    expect(status.pendingUnsupportedCount).toBe(0);
    expect(status.officialAuthForwardingSupported).toBe(false);
  });

  it('previews and applies cc-switch plans: drift refused, then idempotent', async () => {
    const first = (await mock_supplier_command('supplier_import_preview', {})) as {
      planId: string;
      sourceFingerprint: string;
      targetGeneration: number;
      totalRows: number;
      plans: Array<{ supplierId: string; change: string; modelCount: number; apiKeyConfigured: boolean }>;
      pendingUnsupported: Array<{ reason: string }>;
      excludedOauthCount: number;
      problems: Array<{ code: string; detail: string }>;
    };
    expect(first.planId).toBeTruthy();
    expect(first.plans.length).toBeGreaterThan(0);
    expect(first.plans.every((row) => row.change === 'create')).toBe(true);
    expect(first.plans.every((row) => !('apiKey' in row))).toBe(true);
    expect(first.pendingUnsupported).toEqual([]);
    expect(first.excludedOauthCount).toBeGreaterThan(0);
    expect(first.problems.length).toBeGreaterThan(0);
    expect(first.problems.every((row) => row.detail.length > 0)).toBe(true);

    // An unrelated write moves the generation the plan was bound to: the
    // unapplied plan must be refused as stale without writing anything.
    const list = (await mock_supplier_command('supplier_list', {})) as { generation: number };
    await mock_supplier_command('supplier_save', {
      input: {
        expectedGeneration: list.generation,
        name: 'Drift supplier',
        enabled: true,
        priority: 300,
      },
    });
    await expect(
      mock_supplier_command('supplier_import_apply', { planId: first.planId }),
    ).rejects.toThrow('supplier_import_stale_preview');

    const fresh = (await mock_supplier_command('supplier_import_preview', {})) as {
      planId: string;
      targetGeneration: number;
    };
    const applied = (await mock_supplier_command('supplier_import_apply', {
      planId: fresh.planId,
    })) as {
      appliedGeneration: number;
      alreadyApplied: boolean;
      createdCount: number;
      refreshedCount: number;
      problemCount: number;
    };
    expect(applied.alreadyApplied).toBe(false);
    expect(applied.createdCount).toBe(new Set(first.plans.map((row) => row.supplierId)).size);
    expect(applied.refreshedCount).toBe(0);
    expect(applied.appliedGeneration).toBe(fresh.targetGeneration + 1);
    expect(applied.problemCount).toBe(first.problems.length);

    const second = (await mock_supplier_command('supplier_import_preview', {})) as {
      planId: string;
      plans: Array<{ change: string }>;
    };
    expect(second.plans.every((row) => row.change === 'unchanged')).toBe(true);

    const replay = (await mock_supplier_command('supplier_import_apply', {
      planId: second.planId,
    })) as { alreadyApplied: boolean; createdCount: number };
    expect(replay.alreadyApplied).toBe(true);
    expect(replay.createdCount).toBe(0);

    await expect(
      mock_supplier_command('supplier_import_apply', {
        planId: 'demo_plan|deadbeefdead|1|00000000',
      }),
    ).rejects.toThrow('supplier_import_plan_mismatch');
  });

  it('sends only the model selector through the registered assessment command', async () => {
    const result = { selector: 'precise:claude-fable-5.1', catalogOnly: true };
    const invoke = vi.fn().mockResolvedValue(result);
    vi.stubGlobal('window', { __TAURI_INTERNALS__: { invoke } });
    try {
      expect(await model_catalog_model('precise:claude-fable-5.1')).toBe(result);
      expect(invoke).toHaveBeenCalledTimes(1);
      expect(invoke).toHaveBeenCalledWith('model_catalog_model', { selector: 'precise:claude-fable-5.1' }, undefined);
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it('refuses real model assessments in the browser demo without fabricating evidence', async () => {
    await expect(mock_supplier_command('model_catalog_model', { selector: 'alias:fable' }))
      .rejects.toThrow('demo_command_unavailable');
    expect(classify_supplier_error('demo_command_unavailable').kind).toBe('feature_unavailable');
  });

  it('tracks catalog cache freshness for public sources', async () => {
    const before = (await mock_supplier_command('supplier_catalog_status', {})) as {
      sources: Array<{ source: string; stale: boolean }>;
    };
    expect(before.sources.map((row) => row.source).sort()).toEqual(['evalplus', 'lmarena', 'models_dev']);
    for (const source of ['models_dev', 'lmarena']) {
      const refreshed = (await mock_supplier_command('supplier_catalog_refresh', {
        input: { source },
      })) as { source: string; stale: boolean; fetchedAt: number | null };
      expect(refreshed.source).toBe(source);
      expect(refreshed.stale).toBe(false);
      expect(refreshed.fetchedAt).not.toBeNull();
      expect(refreshed).not.toHaveProperty('rank');
    }
    const after = (await mock_supplier_command('supplier_catalog_status', {})) as {
      sources: Array<{ source: string; stale: boolean }>;
    };
    expect(after.sources.find((row) => row.source === 'lmarena')?.stale).toBe(false);
    await expect(
      mock_supplier_command('supplier_catalog_refresh', { input: { source: 'unknown_source' } }),
    ).rejects.toThrow('supplier_catalog_shape_invalid');
  });

  it('sends the whole discover payload through the registered IPC command', async () => {
    const invoke = vi.fn().mockResolvedValue(['claude-opus-4-8']);
    vi.stubGlobal('window', { __TAURI_INTERNALS__: { invoke } });
    try {
      const input = build_discover_models_input({
        protocol: 'anthropic',
        baseUrl: 'https://probe.example.com',
        endpointUrl: '',
        apiKeyIntent: 'set',
        apiKeyValue: 'sk-probe',
        authMode: 'shared',
        endpointKeyIntent: 'keep',
        endpointKeyValue: '',
        extraHeaders: null,
      });
      await expect(supplier_discover_models(input)).resolves.toEqual(['claude-opus-4-8']);
      expect(invoke).toHaveBeenCalledTimes(1);
      expect(invoke).toHaveBeenCalledWith('supplier_discover_models', { input }, undefined);
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it('returns deterministic sorted catalogs and reuses stored credentials on edit', async () => {
    const draft = (await mock_supplier_command('supplier_discover_models', {
      input: { protocol: 'anthropic', baseUrl: 'https://draft.example.com', apiKey: 'sk-probe', useSharedApiKey: true },
    })) as string[];
    expect(draft).toEqual(['claude-fable-5.1', 'claude-haiku-4-5', 'claude-opus-4-8', 'claude-sonnet-5']);
    expect([...draft].sort()).toEqual(draft);

    // Acme's stored openai-chat endpoint carries its own URL override and
    // key; a supplierId-only probe must ride them, mirroring the backend's
    // stored-state fallback.
    const stored = (await mock_supplier_command('supplier_discover_models', {
      input: { supplierId: 'supplier_acme', protocol: 'openai-chat', useSharedApiKey: false },
    })) as string[];
    expect(stored).toEqual(['gpt-5', 'gpt-5-mini', 'o4-mini']);
  });

  it('rejects missing targets, unknown suppliers, and bad protocols with stable codes', async () => {
    await expect(
      mock_supplier_command('supplier_discover_models', { input: { protocol: 'anthropic' } }),
    ).rejects.toThrow('supplier_endpoint_url_required');
    await expect(
      mock_supplier_command('supplier_discover_models', { input: { supplierId: 'missing_supplier', protocol: 'anthropic' } }),
    ).rejects.toThrow('supplier_not_found');
    await expect(
      mock_supplier_command('supplier_discover_models', { input: { protocol: 'smtp', baseUrl: 'https://x.example.com' } }),
    ).rejects.toThrow('supplier_protocol_unsupported');
  });

  it('deletes suppliers and reports the stable in-use code for route references', async () => {
    const list = (await mock_supplier_command('supplier_list', {})) as {
      generation: number;
      suppliers: Array<{ id: string }>;
    };
    expect(list.suppliers.some((row) => row.id === 'supplier_pending_official')).toBe(false);
    const created = await mock_supplier_command('supplier_save', { input: { name: 'Disposable valid API', baseUrl: 'https://delete.example.invalid', apiKey: 'fixture_key', enabled: true, priority: 100, expectedGeneration: list.generation, endpoints: [] } }) as { supplierId: string; generation: number };
    const target = { id: created.supplierId };
    list.generation = created.generation;
    const result = (await mock_supplier_command('supplier_delete', {
      id: target.id,
      expectedGeneration: list.generation,
    })) as { generation: number; setupError: string | null };
    const generation = result.generation;
    expect(result.setupError).toBeNull();
    expect(generation).toBeGreaterThan(list.generation);
    const after = (await mock_supplier_command('supplier_get', { id: target.id })) as {
      supplier: unknown;
    };
    expect(after.supplier).toBeNull();
    await expect(
      mock_supplier_command('supplier_delete', { id: 'missing_supplier', expectedGeneration: generation }),
    ).rejects.toThrow('supplier_id_invalid');
  });
});

describe('Arena assessment presentation', () => {
  // Resolve the UI's existing React installation; this adds no root dependency.
  const ui_require = createRequire(new URL('../ui/package.json', import.meta.url));
  const { createElement } = ui_require('react');
  const { renderToStaticMarkup } = ui_require('react-dom/server');
  const synthetic_arena: Model_arena_assessment = {
    modelName: 'synthetic-model-version', publishedDate: '2026-10-20', category: 'overall',
    sourceId: 'lmarena', sourceUrl: 'https://huggingface.co/datasets/lmarena-ai/leaderboard-dataset',
    sourceConfig: 'text_style_control', sourceSplit: 'latest',
    policy: 'available_model_rank_quartiles_v1', rank: 3, totalModels: 120,
    rating: 1331, ratingLower: 1320, ratingUpper: 1340, voteCount: 42,
    condition: 'synthetic evaluation condition', conditionAmbiguous: true,
    tier: 3, comparedOfferedModels: 5,
  };

  function render_arena(arena: Model_arena_assessment, locale: Locale = 'en'): string {
    const context: I18nContextValue = {
      locale, setLocale: () => {}, t: (key, vars) => translate(locale, key, vars),
      theme: 'light', themeOverride: 'auto', setThemeOverride: () => {},
    };
    return renderToStaticMarkup(createElement(
      I18nCtx.Provider, { value: context }, createElement(Supplier_arena_evidence, { arena }),
    ));
  }

  it('keeps public ranking facts distinct from the active supplied cohort policy', () => {
    const rendered = render_arena(synthetic_arena);
    expect(rendered).toContain('3 / 120 published models');
    expect(rendered).toContain('2026-10-20');
    expect(rendered).toContain('Rating confidence interval');
    expect(rendered).toContain('1320');
    expect(rendered).toContain('1340');
    expect(rendered).toContain('within 5 unique enabled, Arena-rated models');
    expect(rendered).toContain('available_model_rank_quartiles_v1');
    expect(rendered).toContain('Evaluation condition is ambiguous');
    expect(rendered).toContain('do not establish model equivalence');
    expect(rendered).toContain('text_style_control');
    expect(rendered).toContain('https://huggingface.co/datasets/lmarena-ai/leaderboard-dataset');
  });

  it('labels catalog-only evidence separately and localizes the Arena facts', () => {
    const catalog_only = { ...synthetic_arena, policy: 'published_rank_quartiles_v1', comparedOfferedModels: null };
    const rendered = render_arena(catalog_only);
    expect(rendered).toContain('For catalog knowledge, routing tier 3');
    expect(rendered).not.toContain('unique enabled, Arena-rated models');
    const chinese = render_arena(synthetic_arena, 'zh');
    expect(chinese).toContain('公开总榜排名');
    expect(chinese).toContain('评分置信区间');
    expect(chinese).toContain('5 个已启用且有 Arena 评分的去重模型');
    expect(chinese).toContain('不证明模型等价');
  });
});
