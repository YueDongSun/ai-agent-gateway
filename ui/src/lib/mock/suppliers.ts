/** Synthetic supplier registry for the browser demo. Native IPC never
 *  dispatches here.
 *
 *  Everything is in-memory synthetic state: the demo never opens a database,
 *  reads a real file, touches a credential store, or reaches a network. The
 *  store mirrors the Rust service contract exactly — write-only secrets
 *  (omission preserves, `''` clears), optimistic generation CAS, endpoint
 *  auth modes, model ordering, and the stable `supplier_*` error codes — so
 *  the UI exercises the same semantics it will see on native IPC. */
import {
  is_supplier_protocol,
  redact_url_for_display,
  validate_supplier_draft,
  type Supplier_auth_mode,
  type Supplier_catalog_status,
  type Supplier_catalog_view,
  type Supplier_discover_models_input,
  type Supplier_endpoint_input,
  type Supplier_endpoint_view,
  type Supplier_get_result,
  type Supplier_import_apply_result,
  type Supplier_import_preview,
  type Supplier_list_view,
  type Supplier_model_input,
  type Supplier_model_view,
  type Supplier_models_update_input,
  type Supplier_mutation_result,
  type Supplier_protocol,
  type Supplier_provenance,
  type Supplier_save_input,
  type Supplier_status,
  type Supplier_view,
} from '../api/suppliers_logic.js';

interface Mock_model {
  id: string;
  model_id: string;
  display_name: string | null;
  enabled: boolean;
  supports_tools: boolean | null;
  supports_images: boolean | null;
  context_window: number | null;
  max_output_tokens: number | null;
  tier_override: number | null;
  sort_index: number;
  source: string;
  provenance: Supplier_provenance;
}

interface Mock_endpoint {
  id: string;
  protocol: string;
  /** Stored override; `''` means "use the supplier base URL". */
  endpoint_url: string;
  /** Stored override; `null` means "no endpoint-specific key". */
  api_key_override: string | null;
  enabled: boolean;
  priority: number;
  extra_headers: Record<string, string>;
  source: string;
  provenance: Supplier_provenance;
  models: Mock_model[];
  alternatives?: Mock_endpoint[];
}

interface Mock_supplier {
  id: string;
  name: string;
  base_url: string;
  api_key: string;
  enabled: boolean;
  priority: number;
  source: string;
  provenance: Supplier_provenance;
  icon: string | null;
  icon_color: string | null;
  created_at: number;
  updated_at: number;
  endpoints: Mock_endpoint[];
}

const empty_provenance = (): Supplier_provenance => ({
  sourceKind: 'manual',
  sourceRecordRef: null,
  sourceAppType: null,
  sourceEndpointRef: null,
  importBatchRef: null,
  fetchedAt: null,
  contentVersion: null,
  confidence: null,
});

const suppliers = new Map<string, Mock_supplier>();
/** Supplier ids still referenced by demo route hops (legacy projection guard). */
const route_referenced_supplier_ids = new Set<string>();
let generation = 1;
let id_sequence = 0;

function next_id(prefix: string): string {
  id_sequence += 1;
  return `${prefix}_${id_sequence.toString(16).padStart(8, '0')}`;
}

function now_seconds(): number {
  return Math.floor(Date.now() / 1000);
}

function require_generation(expected: number): void {
  if (expected !== generation) throw new Error('supplier_config_generation_conflict');
}

function bump_generation(expected: number): number {
  require_generation(expected);
  generation += 1;
  return generation;
}

function model_view(model: Mock_model): Supplier_model_view {
  return {
    id: model.id,
    modelId: model.model_id,
    displayName: model.display_name,
    enabled: model.enabled,
    supportsTools: model.supports_tools,
    supportsImages: model.supports_images,
    contextWindow: model.context_window,
    maxOutputTokens: model.max_output_tokens,
    tierOverride: model.tier_override,
    sortIndex: model.sort_index,
    source: model.source,
    provenance: structuredClone(model.provenance),
  };
}

function endpoint_view(supplier: Mock_supplier, endpoint: Mock_endpoint): Supplier_endpoint_view {
  const override_display = endpoint.endpoint_url === '' ? null : endpoint.endpoint_url;
  const effective_raw = endpoint.endpoint_url === '' ? supplier.base_url : endpoint.endpoint_url;
  const override_redacted = redact_url_for_display(override_display);
  const effective_redacted = redact_url_for_display(effective_raw === '' ? null : effective_raw);
  const auth_mode: Supplier_auth_mode =
    endpoint.api_key_override !== null && endpoint.api_key_override !== ''
      ? 'override'
      : endpoint.api_key_override !== null
        ? 'none'
        : supplier.api_key !== ''
          ? 'shared'
          : 'none';
  return {
    id: endpoint.id,
    protocol: endpoint.protocol,
    endpointUrl: override_redacted.url,
    effectiveBaseUrl: effective_redacted.url,
    enabled: endpoint.enabled,
    priority: endpoint.priority,
    authMode: auth_mode,
    headerNames: Object.keys(endpoint.extra_headers).sort((a, b) => a.toLowerCase().localeCompare(b.toLowerCase())),
    source: endpoint.source,
    provenance: structuredClone(endpoint.provenance),
    urlRequiresReview: override_redacted.needs_review || effective_redacted.needs_review,
    legacyProjectionAvailable: true,
    models: endpoint.models.map(model_view),
    alternatives: (endpoint.alternatives ?? []).map((alternative) => {
      const display = redact_url_for_display(alternative.endpoint_url || supplier.base_url);
      return { id: alternative.id, endpointUrl: display.url, enabled: alternative.enabled, priority: alternative.priority,
        apiKeyConfigured: Boolean(alternative.api_key_override ?? supplier.api_key), headerNames: Object.keys(alternative.extra_headers).sort(),
        modelIds: alternative.models.map((model) => model.model_id), source: alternative.source, urlRequiresReview: display.needs_review };
    }),
  };
}

function supplier_view(supplier: Mock_supplier): Supplier_view {
  const endpoints = supplier.endpoints.map((endpoint) => endpoint_view(supplier, endpoint));
  const base_redacted = redact_url_for_display(supplier.base_url === '' ? null : supplier.base_url);
  const availability =
    supplier.source === 'official_auth_pending' || supplier.source === 'legacy_passthrough_pending'
      ? 'pending_unsupported'
      : endpoints.length === 0
        ? 'not_configured'
        : endpoints.some((endpoint) => endpoint.effectiveBaseUrl !== null)
          ? 'configured'
          : 'needs_configuration';
  return {
    id: supplier.id,
    name: supplier.name,
    baseUrl: base_redacted.url,
    enabled: supplier.enabled,
    priority: supplier.priority,
    source: supplier.source,
    provenance: structuredClone(supplier.provenance),
    apiKeyConfigured: supplier.api_key !== '',
    icon: supplier.icon,
    iconColor: supplier.icon_color,
    availability: availability,
    urlRequiresReview: base_redacted.needs_review || endpoints.some((endpoint) => endpoint.urlRequiresReview),
    createdAt: supplier.created_at,
    updatedAt: supplier.updated_at,
    endpoints: endpoints,
  };
}

function validate_models(models: Supplier_model_input[]): void {
  if (models.length > 1000) throw new Error('supplier_models_too_many');
  const seen = new Set<string>();
  for (const model of models) {
    if (!model.modelId || model.modelId.trim() === '' || model.modelId.length > 256 || seen.has(model.modelId)) {
      throw new Error('supplier_model_id_invalid');
    }
    seen.add(model.modelId);
    if (model.displayName != null && (model.displayName.length > 128 || /[\u0000-\u001f]/.test(model.displayName))) {
      throw new Error('supplier_model_name_invalid');
    }
    const bad_limit =
      (model.contextWindow != null && (model.contextWindow === 0 || model.contextWindow > 100_000_000)) ||
      (model.maxOutputTokens != null && (model.maxOutputTokens === 0 || model.maxOutputTokens > 100_000_000)) ||
      (model.tierOverride != null && (!Number.isInteger(model.tierOverride) || model.tierOverride > 3));
    if (bad_limit) throw new Error('supplier_model_capability_invalid');
  }
}

function persist_models(models: Supplier_model_input[]): Mock_model[] {
  validate_models(models);
  return models.map((model, index) => ({
    id: next_id('model'),
    model_id: model.modelId,
    display_name: model.displayName ?? null,
    enabled: model.enabled !== false,
    supports_tools: model.supportsTools ?? null,
    supports_images: model.supportsImages ?? null,
    context_window: model.contextWindow ?? null,
    max_output_tokens: model.maxOutputTokens ?? null,
    tier_override: model.tierOverride ?? null,
    sort_index: index,
    source: model.source ?? 'manual',
    provenance: model.provenance ? structuredClone(model.provenance) : empty_provenance(),
  }));
}

function apply_endpoint_input(existing: Mock_endpoint | null, input: Supplier_endpoint_input): Mock_endpoint {
  if (input.useSharedApiKey && input.apiKeyOverride != null) {
    throw new Error('supplier_endpoint_auth_ambiguous');
  }
  const base: Mock_endpoint = existing
    ? structuredClone(existing)
    : {
        id: next_id('endpoint'),
        protocol: input.protocol,
        endpoint_url: '',
        api_key_override: null,
        enabled: true,
        priority: 100,
        extra_headers: {},
        source: 'manual',
        provenance: empty_provenance(),
        models: [],
      };
  // None preserves the stored override; `''` clears it (endpoint auth off).
  if (input.endpointUrl != null) base.endpoint_url = input.endpointUrl;
  if (input.useSharedApiKey) base.api_key_override = null;
  else if (input.apiKeyOverride != null) base.api_key_override = input.apiKeyOverride;
  if (input.enabled != null) base.enabled = input.enabled;
  if (input.priority != null) {
    if (!Number.isInteger(input.priority) || input.priority < -1_000_000 || input.priority > 1_000_000) {
      throw new Error('supplier_priority_out_of_range');
    }
    base.priority = input.priority;
  }
  if (input.extraHeaders != null) base.extra_headers = structuredClone(input.extraHeaders);
  if (input.source != null) base.source = input.source;
  if (input.provenance != null) base.provenance = structuredClone(input.provenance);
  // None preserves the stored models; an explicit list replaces them.
  if (input.models != null) base.models = persist_models(input.models);
  return base;
}

function apply_save(input: Supplier_save_input): Supplier_mutation_result {
  require_generation(input.expectedGeneration);
  const supplier_id = persist_supplier_row(input);
  // The demo has no gateway or client mirrors to set up, so the automatic
  // post-save setup always reports success here.
  return { supplierId: supplier_id, generation: bump_generation(input.expectedGeneration), setupError: null };
}

/** Row write without generation bookkeeping — used by the single-transaction
 *  import apply so one plan advances the generation exactly once. */
function persist_supplier_row(input: Supplier_save_input): string {
  const draft_codes = validate_supplier_draft({
    name: input.name,
    baseUrl: input.baseUrl ?? null,
    priority: input.priority,
    endpoints: (input.endpoints ?? []).map((endpoint) => ({
      protocol: endpoint.protocol,
      endpointUrl: endpoint.endpointUrl ?? null,
    })),
  });
  if (draft_codes.length > 0) throw new Error(draft_codes[0]);
  if (input.endpoints != null && input.endpoints.length > 4) throw new Error('supplier_endpoints_too_many');
  if (input.apiKey != null && /[\u0000-\u001f]/.test(input.apiKey)) throw new Error('supplier_api_key_invalid');

  // Mirrors the service: an omitted id generates a fresh one (must-be-new),
  // while an explicit id upserts exactly like the DAO's INSERT … ON CONFLICT.
  const generated = input.id == null;
  const id = generated ? next_id('supplier') : String(input.id);
  const existing = suppliers.get(id);
  if (generated && existing) throw new Error('supplier_id_conflict');

  const target: Mock_supplier = existing
    ? structuredClone(existing)
    : {
        id: id,
        name: input.name,
        base_url: '',
        api_key: '',
        enabled: true,
        priority: 100,
        source: 'manual',
        provenance: empty_provenance(),
        icon: null,
        icon_color: null,
        created_at: now_seconds(),
        updated_at: now_seconds(),
        endpoints: [],
      };
  target.name = input.name;
  if (input.baseUrl != null) target.base_url = input.baseUrl;
  // Write-only key: omission preserves, explicit '' clears.
  if (input.apiKey != null) target.api_key = input.apiKey;
  // Display fields: omission preserves; '' clears back to the monogram.
  if (input.icon != null) target.icon = input.icon === '' ? null : input.icon;
  if (input.iconColor != null) target.icon_color = input.iconColor === '' ? null : input.iconColor;
  target.enabled = input.enabled !== false;
  target.priority = input.priority;
  if (input.source != null) target.source = input.source;
  if (input.provenance != null) target.provenance = structuredClone(input.provenance);
  if (input.endpoints != null) {
    const seen = new Set<string>();
    for (const endpoint_input of input.endpoints) {
      if (seen.has(endpoint_input.protocol)) throw new Error('supplier_endpoint_duplicate');
      seen.add(endpoint_input.protocol);
      const current = target.endpoints.find((endpoint) => endpoint.protocol === endpoint_input.protocol) ?? null;
      const next = apply_endpoint_input(current, endpoint_input);
      if (current) {
        target.endpoints[target.endpoints.indexOf(current)] = next;
      } else {
        target.endpoints.push(next);
      }
    }
  }
  target.updated_at = now_seconds();
  suppliers.set(id, target);
  return id;
}

// ---------------------------------------------------------------------------
// Seeds — clearly synthetic, no real hosts or credentials
// ---------------------------------------------------------------------------

function seed() {
  const acme: Mock_supplier = {
    id: 'supplier_acme',
    name: 'Acme API (demo)',
    base_url: 'https://api.acme.example.com',
    api_key: 'sk-demo-acme',
    enabled: true,
    priority: 100,
    source: 'manual',
    provenance: empty_provenance(),
    icon: 'flask',
    icon_color: '#B45309',
    created_at: now_seconds() - 86_400 * 30,
    updated_at: now_seconds() - 86_400,
    endpoints: [
      {
        id: 'endpoint_acme_anthropic',
        protocol: 'anthropic',
        endpoint_url: '',
        api_key_override: null,
        enabled: true,
        priority: 10,
        extra_headers: {},
        source: 'manual',
        provenance: empty_provenance(),
        models: [
          {
            id: 'model_acme_opus',
            model_id: 'claude-opus-4-8',
            display_name: 'Claude Opus 4.8',
            enabled: true,
            supports_tools: true,
            supports_images: true,
            context_window: 200_000,
            max_output_tokens: 32_000,
            tier_override: null,
            sort_index: 0,
            source: 'manual',
            provenance: empty_provenance(),
          },
          {
            id: 'model_acme_sonnet',
            model_id: 'claude-sonnet-5',
            display_name: 'Claude Sonnet 5',
            enabled: true,
            supports_tools: true,
            supports_images: true,
            context_window: 200_000,
            max_output_tokens: 16_000,
            tier_override: null,
            sort_index: 1,
            source: 'manual',
            provenance: empty_provenance(),
          },
        ],
      },
      {
        id: 'endpoint_acme_chat',
        protocol: 'openai-chat',
        endpoint_url: 'https://chat.acme.example.com',
        api_key_override: 'sk-demo-acme-chat',
        enabled: true,
        priority: 20,
        extra_headers: { 'x-acme-tenant': 'demo' },
        source: 'manual',
        provenance: empty_provenance(),
        models: [
          {
            id: 'model_acme_gpt',
            model_id: 'gpt-5',
            display_name: 'GPT-5',
            enabled: true,
            supports_tools: true,
            supports_images: false,
            context_window: 128_000,
            max_output_tokens: 8_000,
            tier_override: 2,
            sort_index: 0,
            source: 'manual',
            provenance: empty_provenance(),
          },
        ],
      },
    ],
  };
  suppliers.set(acme.id, acme);
}
seed();

// ---------------------------------------------------------------------------
// Import / catalog fixtures (synthetic; mirrors the real plan-based contract)
// ---------------------------------------------------------------------------

interface Import_row {
  supplierId: string;
  sourceAppType: string;
  sourceId: string;
  name: string;
  protocol: string;
  baseUrl: string | null;
  hasApiKey: boolean;
  headerNames: string[];
  modelIds: string[];
  legacyProjectionPlanned: boolean;
  sourceEndpointUrls: string[];
}

const import_rows: Import_row[] = [
  {
    supplierId: 'supplier_ccs_demo_merged',
    sourceAppType: 'claude_code',
    sourceId: 'ccs-demo-anthropic',
    name: 'cc-switch · Multi-endpoint (demo)',
    protocol: 'anthropic',
    baseUrl: 'https://ccs.demo.example.com',
    hasApiKey: true,
    headerNames: [],
    modelIds: ['claude-opus-4-8', 'claude-sonnet-5'],
    legacyProjectionPlanned: true,
    sourceEndpointUrls: [],
  },
  {
    supplierId: 'supplier_ccs_demo_merged',
    sourceAppType: 'codex',
    sourceId: 'ccs-demo-openai',
    name: 'cc-switch · Multi-endpoint (demo)',
    protocol: 'openai-responses',
    baseUrl: 'https://codex.demo.example.com',
    hasApiKey: true,
    headerNames: ['OpenAI-Organization'],
    modelIds: ['gpt-5'],
    legacyProjectionPlanned: true,
    sourceEndpointUrls: ['https://codex.demo.example.com/v1'],
  },
];

const excluded_account_rows = 1;

const import_problem_rows = [
  {
    sourceAppType: 'codex',
    sourceId: 'ccs-demo-broken',
    code: 'supplier_import_row_invalid',
    detail: 'demo row without a usable endpoint or key',
  },
];

const source_fingerprint = 'demofingerprint01';

function demo_plan_digest(): string {
  // Synthetic stand-in for the self-certifying plan digest: content-bound so
  // apply refuses drift exactly like the Rust plan binding does.
  const parts = import_rows.map((row) => `${row.supplierId}:${row.name}`).join('|');
  let hash = 0;
  for (let index = 0; index < parts.length; index += 1) {
    hash = (hash * 31 + parts.charCodeAt(index)) >>> 0;
  }
  return hash.toString(16).padStart(8, '0');
}

function demo_plan_id(): string {
  return `demo_plan|${source_fingerprint.slice(0, 12)}|${generation}|${demo_plan_digest()}`;
}

function parse_demo_plan_id(plan_id: string): { fingerprint12: string; target: number; digest12: string } | null {
  const parts = plan_id.split('|');
  if (parts.length !== 4 || parts[0] !== 'demo_plan') return null;
  return { fingerprint12: parts[1], target: Number(parts[2]), digest12: parts[3] };
}

function import_change_for(row: Import_row): 'create' | 'refresh' | 'unchanged' {
  const existing = suppliers.get(row.supplierId);
  if (!existing) return 'create';
  const endpoint = existing.endpoints.find((entry) => entry.protocol === row.protocol);
  return existing.name === row.name && endpoint && (endpoint.endpoint_url || existing.base_url) === row.baseUrl && row.modelIds.every((id) => endpoint.models.some((model) => model.model_id === id)) ? 'unchanged' : 'refresh';
}

function build_import_preview(): Supplier_import_preview {
  return {
    planId: demo_plan_id(),
    sourceFingerprint: source_fingerprint,
    targetGeneration: generation,
    createdAt: now_seconds(),
    sourceLabel: '(demo: synthetic cc-switch payload)',
    totalRows: import_rows.length + excluded_account_rows + import_problem_rows.length,
    plans: import_rows.map((row) => ({
      supplierId: row.supplierId,
      sourceAppType: row.sourceAppType,
      sourceId: row.sourceId,
      name: row.name,
      protocol: row.protocol,
      baseUrl: row.baseUrl,
      apiKeyConfigured: row.hasApiKey,
      headerNames: [...row.headerNames],
      modelIds: [...row.modelIds],
      modelCount: row.modelIds.length,
      change: import_change_for(row),
      legacyProjectionPlanned: row.legacyProjectionPlanned,
      sourceEndpointUrls: [...row.sourceEndpointUrls],
    })),
    pendingUnsupported: [],
    excludedOauthCount: excluded_account_rows,
    problems: import_problem_rows.map((row) => ({ ...row })),
  };
}

/** Content-based idempotency: a plan is applied when every row already
 *  matches the source (the Rust check is content-based, not id-based). */
function plan_already_applied(): boolean {
  return import_rows.every((row) => import_change_for(row) === 'unchanged');
}

const catalog_sources = new Map<string, Supplier_catalog_view>([
  ['models_dev', { source: 'models_dev', fetchedAt: null, lastError: null, stale: true }],
  ['evalplus', { source: 'evalplus', fetchedAt: null, lastError: null, stale: true }],
  ['lmarena', { source: 'lmarena', fetchedAt: null, lastError: null, stale: true }],
]);

// ---------------------------------------------------------------------------
// Command dispatcher
// ---------------------------------------------------------------------------

let last_maintenance: { generation: number; removedOauthSuppliers: number; removedOauthProviders: number; mergedSuppliers: number; backupCreated: boolean; preservedStaticSuppliers: number; mergeConflictCount: number; mergeConflictCodes: string[]; setupError: null } | null = null;
const maintenance_recovery: Array<Map<string, Mock_supplier>> = [];

function static_demo_configuration(url: string, key: string): boolean {
  if (['', 'dummy', 'sk-local', 'cliproxyapi', 'not-needed', 'placeholder'].includes(key.trim().toLowerCase())) return false;
  try { const parsed = new URL(url); return ['http:', 'https:'].includes(parsed.protocol) && !parsed.username && !parsed.password && !parsed.search && !parsed.hash; } catch { return false; }
}

function maintain_demo_suppliers() {
  if (last_maintenance?.generation === generation) return structuredClone(last_maintenance);
  const next = new Map([...suppliers].map(([id, supplier]) => [id, structuredClone(supplier)]));
  let removed = 0;
  let merged = 0;
  let preserved = 0;
  let conflicts = 0;
  const groups = new Map<string, Mock_supplier>();
  for (const supplier of next.values()) {
    if (supplier.source === 'official_auth_pending' || supplier.source === 'legacy_passthrough_pending') {
      const has_static = static_demo_configuration(supplier.base_url, supplier.api_key) || supplier.endpoints.some((endpoint) => static_demo_configuration(endpoint.endpoint_url || supplier.base_url, endpoint.api_key_override ?? supplier.api_key) || (endpoint.alternatives ?? []).some((variant) => static_demo_configuration(variant.endpoint_url || supplier.base_url, variant.api_key_override ?? supplier.api_key)));
      if (has_static) { supplier.source = 'manual'; supplier.provenance = empty_provenance(); preserved++; }
      else { next.delete(supplier.id); removed++; }
      continue;
    }
    if (supplier.source !== 'cc_switch' || !supplier.api_key) continue;
    if (supplier.endpoints.some((endpoint) => (endpoint.api_key_override ?? supplier.api_key) !== supplier.api_key || (endpoint.alternatives ?? []).some((variant) => (variant.api_key_override ?? supplier.api_key) !== supplier.api_key))) { conflicts++; continue; }
    let host: string;
    try { host = new URL(supplier.base_url).hostname; } catch { continue; }
    const known = ['open.bigmodel.cn', 'api.z.ai'].includes(host) ? 'glm' : ['api.minimax.io', 'api.minimaxi.com'].includes(host) ? 'minimax' : supplier.name.toLowerCase().replace(/\b(?:claude|codex|anthropic|responses?|openai)\b/g, '').replace(/[^\p{L}\p{N}]/gu, '');
    const identity = known + '\0' + supplier.api_key;
    const keeper = groups.get(identity);
    if (!keeper) { groups.set(identity, supplier); continue; }
    if (!keeper.enabled && supplier.enabled) { for (const endpoint of keeper.endpoints) { endpoint.enabled = false; for (const alternative of endpoint.alternatives ?? []) alternative.enabled = false; } }
    for (const endpoint of supplier.endpoints) {
      const incoming = structuredClone(endpoint);
      incoming.endpoint_url ||= supplier.base_url;
      incoming.api_key_override ??= supplier.api_key;
      incoming.enabled &&= supplier.enabled;
      incoming.priority += supplier.priority - keeper.priority;
      const target = keeper.endpoints.find((item) => item.protocol === incoming.protocol);
      if (target) {
        target.alternatives ??= [];
        const nested = incoming.alternatives ?? [];
        delete incoming.alternatives;
        target.alternatives.push(incoming, ...nested);
        if (target.alternatives.length > 64) throw new Error('supplier_alternatives_too_many');
      } else keeper.endpoints.push(incoming);
    }
    keeper.enabled ||= supplier.enabled;
    next.delete(supplier.id);
    merged++;
  }
  const changed = removed > 0 || merged > 0 || preserved > 0;
  if (changed) {
    maintenance_recovery.push(new Map([...suppliers].map(([id, supplier]) => [id, structuredClone(supplier)])));
    if (maintenance_recovery.length > 8) maintenance_recovery.shift();
    suppliers.clear(); for (const [id, supplier] of next) suppliers.set(id, supplier);
    bump_generation(generation);
  }
  last_maintenance = { generation, removedOauthSuppliers: removed, removedOauthProviders: 0, mergedSuppliers: merged, backupCreated: changed, preservedStaticSuppliers: preserved, mergeConflictCount: conflicts, mergeConflictCodes: conflicts ? ['supplier_merge_credential_divergence'] : [], setupError: null };
  return structuredClone(last_maintenance);
}

export async function mock_supplier_command(command: string, args: Record<string, unknown>): Promise<unknown> {
  switch (command) {
    case 'supplier_import_maintain': return maintain_demo_suppliers();
    case 'supplier_list': {
      const list: Supplier_list_view = {
        generation: generation,
        suppliers: [...suppliers.values()]
          .sort((left, right) => left.priority - right.priority || left.id.localeCompare(right.id))
          .map(supplier_view),
      };
      return structuredClone(list);
    }
    case 'supplier_get': {
      const id = String(args.id);
      const result: Supplier_get_result = {
        generation: generation,
        supplier: suppliers.has(id) ? supplier_view(suppliers.get(id)!) : null,
      };
      return structuredClone(result);
    }
    case 'supplier_save': {
      const input = structuredClone(args.input) as Supplier_save_input;
      return apply_save(input);
    }
    case 'supplier_delete': {
      const id = String(args.id);
      const expected = Number(args.expectedGeneration);
      require_generation(expected);
      if (route_referenced_supplier_ids.has(id)) throw new Error('supplier_in_use');
      if (!suppliers.has(id)) throw new Error('supplier_id_invalid');
      suppliers.delete(id);
      return { generation: bump_generation(expected), setupError: null };
    }
    case 'supplier_models_update': {
      const input = structuredClone(args.input) as Supplier_models_update_input;
      require_generation(input.expectedGeneration);
      const supplier = suppliers.get(input.supplierId);
      if (!supplier) throw new Error('supplier_id_invalid');
      const endpoint = supplier.endpoints.find((item) => item.protocol === input.protocol);
      if (!endpoint) throw new Error('supplier_protocol_unsupported');
      endpoint.models = persist_models(input.models);
      supplier.updated_at = now_seconds();
      return {
        supplierId: supplier.id,
        generation: bump_generation(input.expectedGeneration),
      } satisfies Supplier_mutation_result;
    }
    case 'supplier_discover_models': {
      const input = structuredClone(args.input ?? {}) as Partial<Supplier_discover_models_input>;
      const protocol = String(input.protocol ?? '');
      if (!is_supplier_protocol(protocol)) throw new Error('supplier_protocol_unsupported');
      const existing = input.supplierId != null ? suppliers.get(String(input.supplierId)) : undefined;
      if (input.supplierId != null && !existing) throw new Error('supplier_not_found');
      const endpoint = existing?.endpoints.find((item) => item.protocol === protocol);
      // Address and credential precedence mirror
      // `services/suppliers/discovery.rs`: omitted fields fall back to the
      // stored supplier/endpoint values, the endpoint override wins over the
      // base URL, and `useSharedApiKey` drops any override.
      const base_url = input.baseUrl ?? existing?.base_url ?? '';
      const endpoint_url = input.endpointUrl ?? endpoint?.endpoint_url ?? '';
      if ((endpoint_url !== '' ? endpoint_url : base_url) === '') {
        throw new Error('supplier_endpoint_url_required');
      }
      const shared_key = input.apiKey ?? existing?.api_key ?? '';
      const override_key = input.useSharedApiKey
        ? null
        : (input.apiKeyOverride ?? endpoint?.api_key_override ?? null);
      const api_key = override_key ?? shared_key;
      if (/[\u0000-\u001f]/.test(api_key)) throw new Error('supplier_api_key_invalid');
      // No network in the demo: a deterministic synthetic catalog stands in
      // for the upstream `/v1/models` call, sorted and deduplicated like the
      // real command's result.
      const catalogs: Record<Supplier_protocol, string[]> = {
        anthropic: ['claude-fable-5.1', 'claude-haiku-4-5', 'claude-opus-4-8', 'claude-sonnet-5'],
        'openai-chat': ['gpt-5', 'gpt-5-mini', 'o4-mini'],
        'openai-responses': ['codex-max', 'gpt-5', 'gpt-5-mini'],
        gemini: ['gemini-2.5-flash', 'gemini-2.5-pro'],
      };
      return structuredClone(catalogs[protocol]);
    }
    case 'supplier_status': {
      const all = [...suppliers.values()];
      const endpoints = all.flatMap((supplier) => supplier.endpoints.map((endpoint) => ({ supplier, endpoint })));
      const status: Supplier_status = {
        generation: generation,
        supplierCount: all.length,
        enabledSupplierCount: all.filter((supplier) => supplier.enabled).length,
        endpointCount: endpoints.length,
        enabledEndpointCount: endpoints.filter((item) => item.endpoint.enabled).length,
        configuredEndpointCount: endpoints.filter(
          (item) =>
            item.endpoint.enabled &&
            (item.endpoint.endpoint_url !== '' || item.supplier.base_url !== ''),
        ).length,
        legacyProjectedEndpointCount: endpoints.filter((item) => item.endpoint.enabled).length,
        pendingUnsupportedCount: all.filter(
          (supplier) => supplier.source === 'official_auth_pending' || supplier.source === 'legacy_passthrough_pending',
        ).length,
        // Official-account authorization forwarding is deliberately deferred.
        officialAuthForwardingSupported: false,
      };
      return structuredClone(status);
    }

    // ---- import (mirrors commands/supplier_import.rs) ----
    case 'supplier_import_preview': {
      return structuredClone(build_import_preview());
    }
    case 'supplier_import_apply': {
      const plan_id = String(args.planId ?? '');
      const binding = parse_demo_plan_id(plan_id);
      if (!binding || binding.fingerprint12 !== source_fingerprint.slice(0, 12) || binding.digest12 !== demo_plan_digest()) {
        throw new Error('supplier_import_plan_mismatch');
      }
      // Content idempotency runs before the generation check so a repeated
      // apply of a materialized plan is a stable no-op.
      if (plan_already_applied()) {
        return {
          planId: plan_id,
          appliedGeneration: generation,
          alreadyApplied: true,
          createdCount: 0,
          refreshedCount: 0,
          pendingUnsupportedCount: 0,
          excludedOauthCount: excluded_account_rows,
          problemCount: import_problem_rows.length,
        } satisfies Supplier_import_apply_result;
      }
      if (binding.target !== generation) throw new Error('supplier_import_stale_preview');
      const created_ids = new Set<string>();
      const refreshed_ids = new Set<string>();
      const original_ids = new Set(suppliers.keys());
      for (const row of import_rows) {
        const change = import_change_for(row);
        if (change === 'unchanged') continue;
        if (!original_ids.has(row.supplierId)) created_ids.add(row.supplierId);
        else refreshed_ids.add(row.supplierId);
        persist_supplier_row({
          id: row.supplierId,
          expectedGeneration: generation,
          name: row.name,
          baseUrl: suppliers.get(row.supplierId)?.base_url || row.baseUrl || '',
          apiKey: row.hasApiKey ? 'sk-demo-imported' : '',
          enabled: true,
          priority: 150,
          source: 'cc_switch',
          provenance: {
            sourceKind: 'cc_switch',
            sourceRecordRef: row.sourceId,
            sourceAppType: row.sourceAppType,
            sourceEndpointRef: null,
            importBatchRef: 'demo_batch_1',
            fetchedAt: null,
            contentVersion: null,
            confidence: null,
          },
          endpoints: [
            {
              protocol: row.protocol,
              endpointUrl: row.baseUrl ?? '',
              useSharedApiKey: true,
              enabled: true,
              priority: 100,
              extraHeaders: Object.fromEntries(row.headerNames.map((name) => [name, '[REDACTED]'])),
              models: row.modelIds.map((modelId) => ({ modelId, enabled: true })),
            },
          ],
        });
        const saved = suppliers.get(row.supplierId)!;
        const endpoint = saved.endpoints.find((item) => item.protocol === row.protocol)!;
        endpoint.alternatives ??= [];
        for (const url of row.sourceEndpointUrls) {
          if (url === row.baseUrl || endpoint.alternatives.some((item) => item.endpoint_url === url)) continue;
          const alternative = structuredClone(endpoint);
          alternative.id = next_id('endpoint_variant');
          alternative.endpoint_url = url;
          alternative.api_key_override = saved.api_key;
          delete alternative.alternatives;
          endpoint.alternatives.push(alternative);
        }
      }
      // One transactional apply advances the generation exactly once.
      const applied_generation = bump_generation(generation);
      return {
        planId: plan_id,
        appliedGeneration: applied_generation,
        alreadyApplied: false,
        createdCount: created_ids.size,
        refreshedCount: refreshed_ids.size,
        pendingUnsupportedCount: 0,
          excludedOauthCount: excluded_account_rows,
        problemCount: import_problem_rows.length,
      } satisfies Supplier_import_apply_result;
    }
    case 'model_catalog_model':
      throw new Error('demo_command_unavailable');
    case 'supplier_catalog_status': {
      const status: Supplier_catalog_status = {
        sources: [...catalog_sources.values()].map((entry) => structuredClone(entry)),
      };
      return status;
    }
    case 'supplier_catalog_refresh': {
      const input = args.input as { source: string };
      const source = String(input?.source ?? '');
      const entry = catalog_sources.get(source);
      if (!entry) throw new Error('supplier_catalog_shape_invalid');
      const next: Supplier_catalog_view = {
        source: source,
        fetchedAt: now_seconds(),
        lastError: null,
        stale: false,
      };
      catalog_sources.set(source, next);
      return structuredClone(next);
    }
    default:
      throw new Error('demo_command_unsupported');
  }
}
