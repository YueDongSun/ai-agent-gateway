/// Shared supplier DTOs and pure helpers.
///
/// This module is dependency-free (no React, no IPC) so the Node contract
/// tests and the browser demo mock can both import it. Field names mirror
/// the Rust serde views in `src-tauri/src/services/suppliers/types.rs`
/// exactly (`rename_all = "camelCase"`): unknown fields are rejected on the
/// Rust side, so the shapes here are a strict contract.

// ---------------------------------------------------------------------------
// Protocol contract (mirrors `database::dao::suppliers::SUPPORTED_PROTOCOLS`)
// ---------------------------------------------------------------------------

export const SUPPLIER_PROTOCOLS = [
  'anthropic',
  'openai-chat',
  'openai-responses',
  'gemini',
] as const;

export type Supplier_protocol = (typeof SUPPLIER_PROTOCOLS)[number];

export function is_supplier_protocol(value: string): value is Supplier_protocol {
  return (SUPPLIER_PROTOCOLS as readonly string[]).includes(value);
}

/** Short English label for tests and snippet titles; the UI translates
 *  through `suppliers.protocol.*` keys instead of using this. */
export function supplier_protocol_label_en(protocol: string): string {
  switch (protocol) {
    case 'anthropic':
      return 'Anthropic Messages';
    case 'openai-chat':
      return 'OpenAI Chat Completions';
    case 'openai-responses':
      return 'OpenAI Responses';
    case 'gemini':
      return 'Gemini';
    default:
      return protocol;
  }
}

/** Client-facing HTTP path suffix on the single AAG listen endpoint. */
export function supplier_protocol_path(protocol: string): string {
  switch (protocol) {
    case 'anthropic':
      return '/v1/messages';
    case 'openai-chat':
      return '/v1/chat/completions';
    case 'openai-responses':
      return '/v1/responses';
    case 'gemini':
      return '/v1beta/models/{model}:generateContent';
    default:
      return '';
  }
}

// ---------------------------------------------------------------------------
// DTOs (views serialize from Rust; inputs deserialize into Rust)
// ---------------------------------------------------------------------------

export interface Supplier_provenance {
  sourceKind: string | null;
  sourceRecordRef: string | null;
  sourceAppType: string | null;
  sourceEndpointRef: string | null;
  importBatchRef: string | null;
  fetchedAt: number | null;
  contentVersion: string | null;
  confidence: number | null;
}

export interface Supplier_model_input {
  modelId: string;
  displayName?: string | null;
  enabled: boolean;
  supportsTools?: boolean | null;
  supportsImages?: boolean | null;
  contextWindow?: number | null;
  maxOutputTokens?: number | null;
  /** 0..3 manual tier override; null = no override / pending evidence. */
  tierOverride?: number | null;
  source?: string | null;
  provenance?: Supplier_provenance | null;
}

export interface Supplier_endpoint_input {
  protocol: string;
  /** Omit to preserve the stored override; `''` clears it. */
  endpointUrl?: string | null;
  /** Omit to preserve the stored override; `''` disables endpoint auth. */
  apiKeyOverride?: string | null;
  useSharedApiKey: boolean;
  enabled?: boolean | null;
  priority?: number | null;
  /** Header values are write-only; reads return names only. */
  extraHeaders?: Record<string, string> | null;
  source?: string | null;
  provenance?: Supplier_provenance | null;
  /** Omit to preserve the stored models; `[]` clears them. */
  models?: Supplier_model_input[] | null;
}

export interface Supplier_save_input {
  /** Omit / null on create: the backend generates a stable id. */
  id?: string | null;
  expectedGeneration: number;
  name: string;
  baseUrl?: string | null;
  /** Omit to preserve the stored key; `''` clears it. Write-only. */
  apiKey?: string | null;
  enabled: boolean;
  priority: number;
  source?: string | null;
  provenance?: Supplier_provenance | null;
  /** Glyph id or literal character. Omit to preserve; `''` clears. */
  icon?: string | null;
  /** Tile background. Omit to preserve; `''` clears. */
  iconColor?: string | null;
  /** Omit to preserve stored endpoints; `[]` removes them. */
  endpoints?: Supplier_endpoint_input[] | null;
}

export interface Supplier_models_update_input {
  supplierId: string;
  protocol: string;
  expectedGeneration: number;
  models: Supplier_model_input[];
}

/** Probe payload for `supplier_discover_models` (mirrors
 *  `services::suppliers::SupplierDiscoverModelsInput`). With `supplierId`,
 *  omitted fields fall back to the stored supplier/endpoint state —
 *  including the stored credential, which stays write-only. */
export interface Supplier_discover_models_input {
  supplierId?: string | null;
  protocol: string;
  baseUrl?: string | null;
  apiKey?: string | null;
  endpointUrl?: string | null;
  apiKeyOverride?: string | null;
  useSharedApiKey?: boolean;
  extraHeaders?: Record<string, string> | null;
}

export interface Supplier_model_view {
  id: string;
  modelId: string;
  displayName: string | null;
  enabled: boolean;
  supportsTools: boolean | null;
  supportsImages: boolean | null;
  contextWindow: number | null;
  maxOutputTokens: number | null;
  tierOverride: number | null;
  sortIndex: number;
  source: string;
  provenance: Supplier_provenance;
}

export type Supplier_auth_mode = 'shared' | 'override' | 'none';

export interface Supplier_discovery_health {
  status: string;
  checkedAt: number;
  lastSuccessAt: number | null;
  latencyMs: number;
  consecutiveFailures: number;
  error: string | null;
  listedModelIds: string[];
  /** Catalog GETs do not validate generation or API protocol compatibility. */
  inferenceStatus: 'unverified';
}

/** Discovery can fail after configuration committed. Do not invite a duplicate save. */
export function supplier_discovery_notice(error?: string | null, translate?: (key: string) => string): string {
  if (!error) return '';
  // Keep the English fallback for existing non-UI callers; views pass their translator.
  return ' ' + (translate?.('suppliers.discovery.incomplete_notice')
    ?? 'Model discovery is incomplete. Check the endpoint catalog status; saved settings are preserved.');
}

export interface Supplier_endpoint_view {
  id: string;
  protocol: string;
  endpointUrl: string | null;
  effectiveBaseUrl: string | null;
  enabled: boolean;
  priority: number;
  authMode: Supplier_auth_mode;
  headerNames: string[];
  source: string;
  provenance: Supplier_provenance;
  urlRequiresReview: boolean;
  legacyProjectionAvailable: boolean;
  discoveryHealth?: Supplier_discovery_health | null;
  models: Supplier_model_view[];
  alternatives?: Array<{ id: string; endpointUrl: string | null; enabled: boolean; priority: number; apiKeyConfigured: boolean; headerNames: string[]; modelIds: string[]; source: string; urlRequiresReview: boolean }>;
}

export type Supplier_availability =
  | 'pending_unsupported'
  | 'not_configured'
  | 'configured'
  | 'needs_configuration';

export interface Supplier_view {
  id: string;
  name: string;
  baseUrl: string | null;
  enabled: boolean;
  priority: number;
  source: string;
  provenance: Supplier_provenance;
  apiKeyConfigured: boolean;
  /** Glyph id or literal character; `null` renders a name monogram. */
  icon: string | null;
  /** Tile background; `null` uses the deterministic monogram hue. */
  iconColor: string | null;
  availability: Supplier_availability;
  urlRequiresReview: boolean;
  createdAt: number | null;
  updatedAt: number | null;
  endpoints: Supplier_endpoint_view[];
}

export interface Supplier_list_view {
  generation: number;
  suppliers: Supplier_view[];
}

export interface Supplier_get_result {
  generation: number;
  supplier: Supplier_view | null;
}

export interface Supplier_mutation_result {
  supplierId: string;
  generation: number;
  /** Safe code from the automatic post-save gateway start / hot-apply and
   *  client-mirror write. `null` (or absent) means setup succeeded or did
   *  not run. A non-empty value never means the save failed: the row is
   *  already persisted, so retrying is an edit of `supplierId`, not a new
   *  supplier. */
  setupError?: string | null;
  discoveryError?: string | null;
}

export interface Supplier_delete_result {
  generation: number;
  setupError?: string | null;
}

export interface Supplier_status {
  generation: number;
  supplierCount: number;
  enabledSupplierCount: number;
  endpointCount: number;
  enabledEndpointCount: number;
  configuredEndpointCount: number;
  legacyProjectedEndpointCount: number;
  pendingUnsupportedCount: number;
  /** Always false until official-account authorization forwarding is
   *  implemented and separately authorized. */
  officialAuthForwardingSupported: boolean;
}

// ---------------------------------------------------------------------------
// Import + public catalog contracts
//
// Import commands are owned by `src-tauri/src/commands/supplier_import.rs`
// (`supplier_import_preview` / `supplier_import_apply`); their DTOs live in
// `services/import_cc_switch/preview.rs` and `supplier_import.rs` and are
// mirrored here. Public catalog status, refresh, and model assessments use
// `commands/supplier_catalog.rs`; browser demo lookups remain explicitly
// unavailable rather than fabricating public evidence.
// ---------------------------------------------------------------------------

/** One supplier the apply would create or refresh. Secrets are never part
 *  of this view: `apiKeyConfigured` is a boolean only. */
export interface Supplier_import_plan_entry {
  supplierId: string;
  sourceAppType: string;
  sourceId: string;
  name: string;
  protocol: string;
  /** Sanitized display URL (credentials, query, fragment removed). */
  baseUrl: string | null;
  apiKeyConfigured: boolean;
  headerNames: string[];
  /** Model ids in source order (bounded for display). */
  modelIds: string[];
  modelCount: number;
  /** `create`, `refresh`, or `unchanged` against the current database. */
  change: 'create' | 'refresh' | 'unchanged' | string;
  legacyProjectionPlanned: boolean;
  sourceEndpointUrls: string[];
}

/** Official/managed authentication rows: only the non-sensitive association
 *  is recorded; the supplier stays unusable until official-account
 *  authorization forwarding exists. */
export interface Supplier_import_pending_entry {
  supplierId: string;
  sourceAppType: string;
  sourceId: string;
  name: string;
  reason: string;
}

/** A source record the import cannot use, explained instead of dropped. */
export interface Supplier_import_problem {
  sourceAppType: string | null;
  sourceId: string | null;
  code: string;
  detail: string;
}

export interface Supplier_import_preview {
  excludedOauthCount?: number;
  /** Self-certifying plan id binding the source fingerprint, the target
   *  generation, and the entry digest. Opaque: pass verbatim to apply. */
  planId: string;
  sourceFingerprint: string;
  targetGeneration: number;
  createdAt: number;
  /** Safe source label — never a filesystem path. */
  sourceLabel: string;
  totalRows: number;
  plans: Supplier_import_plan_entry[];
  pendingUnsupported: Supplier_import_pending_entry[];
  problems: Supplier_import_problem[];
}

export interface Supplier_import_apply_result {
  excludedOauthCount?: number;
  setupError?: string | null;
  discoveryError?: string | null;
  planId: string;
  appliedGeneration: number;
  alreadyApplied: boolean;
  createdCount: number;
  refreshedCount: number;
  pendingUnsupportedCount: number;
  problemCount: number;
}

export interface Supplier_catalog_view {
  source: string;
  fetchedAt: number | null;
  lastError: string | null;
  stale: boolean;
  incompleteRows?: number;
}

export interface Supplier_catalog_status {
  sources: Supplier_catalog_view[];
}

export interface Model_assessment_source {
  source: string;
  fetchedAt: number | null;
  stale: boolean;
  lastError: string | null;
}

/** Public preference evidence and the declared routing policy applied to it.
 *  The rating interval is statistical evidence; the assessment's separate
 *  confidence field describes evidence attribution, not this interval. */
export interface Model_arena_assessment {
  modelName: string;
  publishedDate: string;
  category: string;
  sourceId: string;
  sourceUrl: string;
  sourceConfig: string;
  sourceSplit: string;
  policy: string;
  rank: number;
  totalModels: number;
  rating: number;
  ratingLower: number;
  ratingUpper: number;
  voteCount: number;
  condition: string | null;
  conditionAmbiguous: boolean;
  tier: number;
  comparedOfferedModels: number | null;
}

export interface Model_assessment {
  modelId: string;
  canonicalId: string;
  family: string | null;
  tier: number | null;
  origin: 'manual' | 'inferred' | 'pending';
  basis: 'manualOverride' | 'arenaEvidence' | 'codingEvidence' | 'familyOrdinal' | 'familyRole' | 'pending';
  confidence: number | null;
  condition: string | null;
  roleTierHint: number | null;
  coding: {
    humanevalPlus: number;
    mbppPlus: number;
    prompted: boolean;
    average: number;
    entryKeys: string[];
  } | null;
  arena?: Model_arena_assessment | null;
  sources: Model_assessment_source[];
}

export interface Model_catalog_entry {
  selector: string;
  selectorKind: string;
  aliasName: string | null;
  aliasTier: number | null;
  modelId: string | null;
  canonicalId: string | null;
  family: string | null;
  releaseDate: string | null;
  assessment: Model_assessment | null;
  capabilities: {
    supportsTools: boolean | null;
    supportsImages: boolean | null;
    contextWindow: number | null;
    maxOutputTokens: number | null;
    inputModalities: string[];
    outputModalities: string[];
  } | null;
  cost: {
    inputPerMillion: number | null;
    outputPerMillion: number | null;
  } | null;
  catalogOnly: boolean;
  offeredBy: Array<{
    supplierId: string;
    endpointId: string;
    protocol: string;
    modelId: string;
  }>;
  sources: Supplier_catalog_view[];
  aliasPreview: Array<{
    modelId: string;
    endpointRef: string;
    supplierRef: string;
    tier: number | null;
    basis: string;
    downgraded: boolean;
    reason: string;
  }>;
}

// ---------------------------------------------------------------------------
// Error classification (stable codes only; never log or display raw errors)
// ---------------------------------------------------------------------------

export type Supplier_error_kind =
  | 'code'
  | 'generation_conflict'
  | 'in_use'
  | 'malformed_vendor'
  | 'feature_unavailable'
  | 'unknown';

export interface Supplier_error_info {
  kind: Supplier_error_kind;
  /** Stable `supplier_*` code when one was recognized. */
  code: string | null;
  /** Safe display text: either the stable code or a generic sentence. */
  text: string;
}

const SUPPLIER_CODE_PATTERN = /^supplier_[a-z0-9_]+$/;
const MALFORMED_VENDOR_DETAIL = 'malformed vendor record';
const UNAVAILABLE_PATTERN =
  /not found|unsupported_command|demo_command_unavailable|demo_command_unsupported|unknown command|command\s+\S+\s+not\s+found/i;

/** Classify an IPC rejection into a stable, secret-free display payload.
 *  Raw backend error context is never surfaced verbatim. */
export function classify_supplier_error(raw: unknown): Supplier_error_info {
  const message = raw instanceof Error ? raw.message : String(raw ?? '');
  const trimmed = message.trim();
  if (SUPPLIER_CODE_PATTERN.test(trimmed)) {
    const kind: Supplier_error_kind =
      trimmed === 'supplier_config_generation_conflict'
        ? 'generation_conflict'
        : trimmed === 'supplier_in_use'
          ? 'in_use'
          : 'code';
    return { kind, code: trimmed, text: trimmed };
  }
  if (trimmed === MALFORMED_VENDOR_DETAIL) {
    return {
      kind: 'malformed_vendor',
      code: null,
      text: MALFORMED_VENDOR_DETAIL,
    };
  }
  if (UNAVAILABLE_PATTERN.test(trimmed)) {
    return { kind: 'feature_unavailable', code: null, text: 'feature_unavailable' };
  }
  return { kind: 'unknown', code: null, text: 'supplier_error_unknown' };
}

export function is_supplier_feature_unavailable(raw: unknown): boolean {
  return classify_supplier_error(raw).kind === 'feature_unavailable';
}

export function is_supplier_generation_conflict(raw: unknown): boolean {
  return classify_supplier_error(raw).kind === 'generation_conflict';
}

// ---------------------------------------------------------------------------
// Secret write-only payload decisions
// ---------------------------------------------------------------------------

export type Secret_intent = 'preserve' | 'set' | 'clear';

/** Decide the write-only `apiKey` field: omitted preserves, `''` clears. */
export function secret_write_value(intent: Secret_intent, value: string): string | undefined {
  if (intent === 'preserve') return undefined;
  if (intent === 'clear') return '';
  return value;
}

export interface Endpoint_secret_intent {
  /** `shared` uses the supplier key; `override` stores a per-endpoint key. */
  authMode: Supplier_auth_mode;
  /** What to do with the per-endpoint key when authMode is `override`. */
  keyIntent: Secret_intent;
  keyValue: string;
}

/** Build the write-only endpoint auth fields. `useSharedApiKey` is explicit:
 *  `true` returns to the supplier key and drops any stored override. */
export function endpoint_auth_fields(intent: Endpoint_secret_intent): {
  apiKeyOverride?: string | null;
  useSharedApiKey: boolean;
} {
  if (intent.authMode === 'shared') {
    return { useSharedApiKey: true };
  }
  if (intent.authMode === 'none') {
    // Explicit empty override disables endpoint-specific authentication.
    return { apiKeyOverride: '', useSharedApiKey: false };
  }
  const value = secret_write_value(intent.keyIntent, intent.keyValue);
  return {
    ...(value === undefined ? {} : { apiKeyOverride: value }),
    useSharedApiKey: false,
  };
}

export interface Discover_models_state {
  supplierId?: string | null;
  protocol: string;
  /** Typed base URL; `''` omits the field so editing probes the stored one. */
  baseUrl?: string | null;
  endpointUrl?: string | null;
  apiKeyIntent: Secret_intent;
  apiKeyValue: string;
  authMode: Supplier_auth_mode;
  endpointKeyIntent: Secret_intent;
  endpointKeyValue: string;
  /** Parsed headers, or `null` when untouched (stored headers apply). */
  extraHeaders?: Record<string, string> | null;
}

/** Build the `supplier_discover_models` payload from editor state with the
 *  same secret precedence as saving: untouched keys are omitted so the
 *  backend reuses the stored credential, an explicit empty override probes
 *  anonymously, and `shared` drops any override. */
export function build_discover_models_input(state: Discover_models_state): Supplier_discover_models_input {
  const input: Supplier_discover_models_input = { protocol: state.protocol };
  if (state.supplierId) input.supplierId = state.supplierId;
  if (state.baseUrl) input.baseUrl = state.baseUrl;
  if (state.endpointUrl) input.endpointUrl = state.endpointUrl;
  const key = secret_write_value(state.apiKeyIntent, state.apiKeyValue);
  // A blank `set` has nothing to send (create has no stored key); `clear`
  // deliberately probes anonymously against a stored key.
  if (key !== undefined && (key !== '' || state.apiKeyIntent === 'clear')) input.apiKey = key;
  const auth = endpoint_auth_fields({
    authMode: state.authMode,
    keyIntent: state.endpointKeyIntent,
    keyValue: state.endpointKeyValue,
  });
  input.useSharedApiKey = auth.useSharedApiKey;
  if (auth.apiKeyOverride != null) input.apiKeyOverride = auth.apiKeyOverride;
  if (state.extraHeaders != null) input.extraHeaders = state.extraHeaders;
  return input;
}

// ---------------------------------------------------------------------------
// Client-side pre-validation (mirrors services::suppliers checks)
// ---------------------------------------------------------------------------

export const SUPPLIER_PRIORITY_MIN = -1_000_000;
export const SUPPLIER_PRIORITY_MAX = 1_000_000;
export const SUPPLIER_MAX_ENDPOINTS = 4;
export const SUPPLIER_MAX_MODELS_PER_ENDPOINT = 1000;
export const SUPPLIER_TIER_MIN = 0;
export const SUPPLIER_TIER_MAX = 3;

/** Validate a URL the way the Rust service does; `''` means "not set". */
export function is_valid_supplier_url(value: string): boolean {
  if (value === '') return true;
  if (/[\u0000-\u001f]/.test(value) || value.length > 2048) return false;
  let parsed: URL;
  try {
    parsed = new URL(value);
  } catch {
    return false;
  }
  return (
    (parsed.protocol === 'http:' || parsed.protocol === 'https:') &&
    Boolean(parsed.host) &&
    parsed.username === '' &&
    parsed.password === '' &&
    parsed.search === '' &&
    parsed.hash === ''
  );
}

export function is_valid_supplier_name(value: string): boolean {
  const trimmed = value.trim();
  return trimmed.length > 0 && trimmed.length <= 128 && !/[\u0000-\u001f]/.test(value);
}

export function is_valid_supplier_priority(value: number): boolean {
  return Number.isInteger(value) && value >= SUPPLIER_PRIORITY_MIN && value <= SUPPLIER_PRIORITY_MAX;
}

export function is_valid_tier_override(value: number | null | undefined): boolean {
  return value === null || value === undefined || (Number.isInteger(value) && value >= SUPPLIER_TIER_MIN && value <= SUPPLIER_TIER_MAX);
}

/** Return stable error codes for a draft, in display order. */
export function validate_supplier_draft(draft: {
  name: string;
  baseUrl?: string | null;
  priority: number;
  endpoints: Array<{ protocol: string; endpointUrl?: string | null }>;
}): string[] {
  const codes: string[] = [];
  if (!is_valid_supplier_name(draft.name)) codes.push('supplier_name_invalid');
  if (!is_valid_supplier_priority(draft.priority)) codes.push('supplier_priority_out_of_range');
  if (draft.baseUrl !== undefined && draft.baseUrl !== null && !is_valid_supplier_url(draft.baseUrl)) {
    codes.push('supplier_endpoint_url_invalid');
  }
  if (draft.endpoints.length > SUPPLIER_MAX_ENDPOINTS) codes.push('supplier_endpoints_too_many');
  const seen = new Set<string>();
  for (const endpoint of draft.endpoints) {
    if (!is_supplier_protocol(endpoint.protocol)) {
      codes.push('supplier_protocol_unsupported');
    } else if (seen.has(endpoint.protocol)) {
      codes.push('supplier_endpoint_duplicate');
    }
    seen.add(endpoint.protocol);
    if (endpoint.endpointUrl != null && !is_valid_supplier_url(endpoint.endpointUrl)) {
      codes.push('supplier_endpoint_url_invalid');
    }
  }
  return [...new Set(codes)];
}

// ---------------------------------------------------------------------------
// Model ordering and capability tiers
// ---------------------------------------------------------------------------

/** Stable order used for model lists: sort_index, then modelId. */
export function sort_supplier_models<T extends { sortIndex: number; modelId: string }>(
  models: readonly T[],
): T[] {
  return [...models].sort((left, right) =>
    left.sortIndex !== right.sortIndex ? left.sortIndex - right.sortIndex : left.modelId.localeCompare(right.modelId),
  );
}

/** Tier order contract: 0 lowest … 3 highest. `null` = pending evidence. */
export type Supplier_tier = 0 | 1 | 2 | 3;

export interface Tier_resolution {
  tier: Supplier_tier | null;
  /** `manual` when a human override wins, `inferred` when family evidence was
   *  used, `pending` when nothing reliable is known. */
  origin: 'manual' | 'inferred' | 'pending';
  confidence: number | null;
}

/** Manual override wins; otherwise infer from the Anthropic family evidence
 *  (fable strongest, opus next), then from the documented name priors that
 *  mirror the backend chain in `services/model_catalog/tiers.rs`. This is the
 *  offline fallback only — the routing panel also queries the backend
 *  assessment for the authoritative queue. Unknown ids stay `pending`. */
export function resolve_model_tier(
  modelId: string,
  override: number | null | undefined,
  provenanceConfidence?: number | null,
): Tier_resolution {
  if (is_valid_tier_override(override) && override !== null && override !== undefined) {
    return { tier: override as Supplier_tier, origin: 'manual', confidence: 1 };
  }
  const family = infer_anthropic_family_tier(modelId);
  if (family !== null) {
    return {
      tier: family,
      origin: 'inferred',
      confidence: provenanceConfidence ?? null,
    };
  }
  const prior = infer_name_prior_tier(modelId);
  if (prior !== null) {
    return { tier: prior, origin: 'inferred', confidence: null };
  }
  return { tier: null, origin: 'pending', confidence: null };
}

/** Tokens mirroring `rank::is_low_tier_token`: cheap or experimental
 *  variants mark the fast tier. */
const LOW_TIER_TOKENS = ['flash', 'lite', 'mini', 'nano', 'air', 'exp', 'free', 'batch'] as const;

/** Flagship positioning vocabulary mirroring `tiers::FLAGSHIP_TOKENS`. */
const FLAGSHIP_TOKENS = ['max', 'pro', 'ultra', 'opus', 'fable'] as const;

/** Vendor flagship priors mirroring `tiers::VENDOR_TIER_PRIORS`, keyed by the
 *  discovery family key (leading digit-free tokens of the id). */
const VENDOR_TIER_PRIORS: Record<string, Supplier_tier> = {
  glm: 2,
  'deepseek-chat': 2,
  'deepseek-reasoner': 2,
  kimi: 2,
  minimax: 2,
  grok: 2,
};

function id_tokens(modelId: string): string[] {
  return modelId
    .toLowerCase()
    .replace(/\[[^\]]*\]/g, '')
    .split(/[-_.\/\s]+/)
    .filter((token) => token.length > 0 && !token.includes(':'));
}

/** Documented name priors for families without public evidence: cheap
 *  variant tokens first, then flagship tokens, then vendor flagship lines. */
export function infer_name_prior_tier(modelId: string): Supplier_tier | null {
  const tokens = id_tokens(modelId);
  if (tokens.length === 0) return null;
  if (tokens.some((token) => LOW_TIER_TOKENS.some((low) => token === low || (token.length > low.length && token.startsWith(low) && token.length - low.length <= 2)))) return 0;
  if (tokens.some((token) => (FLAGSHIP_TOKENS as readonly string[]).includes(token))) return 2;
  const family = tokens
    .filter((token) => !/[0-9]/.test(token))
    .slice(0, 2)
    .join('-');
  return VENDOR_TIER_PRIORS[family] ?? null;
}

/** Family-relative tier for the known Anthropic families only. Version
 *  numbers are never compared across families. */
export function infer_anthropic_family_tier(modelId: string): Supplier_tier | null {
  const id = modelId.toLowerCase();
  if (!id.includes('claude') && !id.includes('fable')) return null;
  if (id.includes('fable')) return 3;
  if (id.includes('opus')) return 2;
  if (id.includes('sonnet')) return 1;
  if (id.includes('haiku')) return 0;
  return null;
}

/** Capability aliases clients may send instead of a concrete model id.
 *  `fable` maps to the strongest tier and `opus` follows; unknown names
 *  return `null` and must not take over routing. */
export function capability_tier_for_alias(alias: string): Supplier_tier | null {
  const name = alias.toLowerCase();
  if (name.includes('fable')) return 3;
  if (name.includes('opus')) return 2;
  if (name.includes('sonnet')) return 1;
  if (name.includes('haiku')) return 0;
  return null;
}

/** Capability alias options use the unified client-facing role vocabulary
 *  (`services/mirror_models.rs`); historical official spellings stay reachable
 *  as exact model ids. */
export const CAPABILITY_ALIASES = ['claude-fable-5', 'claude-opus-5', 'claude-sonnet-5', 'claude-haiku-5-5'] as const;

// ---------------------------------------------------------------------------
// Routing preview (client-side explanation of the selection rules)
// ---------------------------------------------------------------------------

export interface Routing_candidate {
  supplierId: string;
  supplierName: string;
  supplierEnabled: boolean;
  supplierPriority: number;
  endpointProtocol: string;
  endpointEnabled: boolean;
  endpointPriority: number;
  endpointConfigured: boolean;
  model: Supplier_model_view;
}

export type Routing_exclusion_reason =
  | 'supplier_disabled'
  | 'endpoint_disabled'
  | 'endpoint_unconfigured'
  | 'model_disabled'
  | 'missing_tools'
  | 'missing_images';

export interface Routing_requirement {
  needsTools: boolean;
  needsImages: boolean;
  /** Minimum acceptable tier for the requested alias (downgrades allowed but
   *  recorded); `null` means no tier constraint. */
  minTier: Supplier_tier | null;
}

export interface Routing_preview {
  eligible: Array<Routing_candidate & { tier: Supplier_tier | null; tier_origin: 'manual' | 'inferred' | 'pending' }>;
  excluded: Array<{ candidate: Routing_candidate; reason: Routing_exclusion_reason }>;
}

/** Order candidates the way the routing backend is specified to: hard
 *  capability filter first, then tier (highest first), then ascending
 *  supplier/endpoint priority, then model id. Downgrades below the alias
 *  tier stay eligible but sort after same-tier options so every downgrade
 *  is an explicit, traceable choice. */
export function preview_routing(
  candidates: readonly Routing_candidate[],
  requirement: Routing_requirement,
): Routing_preview {
  const eligible: Routing_preview['eligible'] = [];
  const excluded: Routing_preview['excluded'] = [];
  for (const candidate of candidates) {
    const resolution = resolve_model_tier(
      candidate.model.modelId,
      candidate.model.tierOverride,
      candidate.model.provenance.confidence,
    );
    const reason = exclusion_reason(candidate, requirement);
    if (reason) {
      excluded.push({ candidate, reason });
      continue;
    }
    eligible.push({ ...candidate, tier: resolution.tier, tier_origin: resolution.origin });
  }
  eligible.sort((left, right) => {
    const left_tier = left.tier ?? -1;
    const right_tier = right.tier ?? -1;
    if (left_tier !== right_tier) return right_tier - left_tier;
    if (left.supplierPriority !== right.supplierPriority) return left.supplierPriority - right.supplierPriority;
    if (left.endpointPriority !== right.endpointPriority) return left.endpointPriority - right.endpointPriority;
    return left.model.modelId.localeCompare(right.model.modelId);
  });
  return { eligible, excluded };
}

function exclusion_reason(
  candidate: Routing_candidate,
  requirement: Routing_requirement,
): Routing_exclusion_reason | null {
  if (!candidate.supplierEnabled) return 'supplier_disabled';
  if (!candidate.endpointEnabled) return 'endpoint_disabled';
  if (!candidate.endpointConfigured) return 'endpoint_unconfigured';
  if (!candidate.model.enabled) return 'model_disabled';
  if (requirement.needsTools && candidate.model.supportsTools === false) return 'missing_tools';
  if (requirement.needsImages && candidate.model.supportsImages === false) return 'missing_images';
  return null;
}

// ---------------------------------------------------------------------------
// Client snippet generation (display only — never writes client files)
// ---------------------------------------------------------------------------

export interface Client_snippet {
  protocol: Supplier_protocol;
  title: string;
  path: string;
  body: string;
}

/** Build the four client-connection snippets for one AAG listen URL. The
 *  gateway credential is a placeholder: snippets must not embed secrets. */
export function client_snippets(listen_url: string): Client_snippet[] {
  const base = listen_url.replace(/\/+$/, '');
  return [
    {
      protocol: 'anthropic',
      title: supplier_protocol_label_en('anthropic'),
      path: supplier_protocol_path('anthropic'),
      body:
        `# Claude Code / Anthropic SDK\n` +
        `export ANTHROPIC_BASE_URL=${base}\n` +
        `export ANTHROPIC_AUTH_TOKEN=<aag-gateway-token>\n` +
        `# requests: POST ${base}/v1/messages\n`,
    },
    {
      protocol: 'openai-chat',
      title: supplier_protocol_label_en('openai-chat'),
      path: supplier_protocol_path('openai-chat'),
      body:
        `# OpenAI SDK (Chat Completions)\n` +
        `base_url = "${base}/v1"\n` +
        `api_key = "<aag-gateway-token>"\n` +
        `# requests: POST ${base}/v1/chat/completions\n`,
    },
    {
      protocol: 'openai-responses',
      title: supplier_protocol_label_en('openai-responses'),
      path: supplier_protocol_path('openai-responses'),
      body:
        `# Codex CLI (config.toml)\n` +
        `# auth: OPENAI_API_KEY=<aag-gateway-token>\n` +
        `[model_providers.aag]\n` +
        `name = "AAG Gateway"\n` +
        `base_url = "${base}/v1"\n` +
        `wire_api = "responses"\n` +
        `# requests: POST ${base}/v1/responses\n`,
    },
    {
      protocol: 'gemini',
      title: supplier_protocol_label_en('gemini'),
      path: supplier_protocol_path('gemini'),
      body:
        `# Gemini CLI / Gemini SDK\n` +
        `export GOOGLE_GEMINI_BASE_URL=${base}\n` +
        `# auth: x-goog-api-key=<aag-gateway-token>\n` +
        `# requests: POST ${base}/v1beta/models/{model}:generateContent\n`,
    },
  ];
}

// ---------------------------------------------------------------------------
// URL display safety
// ---------------------------------------------------------------------------

/** Strip credentials, query strings and fragments for display. Returns
 *  `null` plus `needs_review` for unparseable stored values; stripping
 *  anything also sets `needs_review` (matching `url_requires_review`). */
export function redact_url_for_display(value: string | null | undefined): {
  url: string | null;
  needs_review: boolean;
} {
  if (value === null || value === undefined || value === '') return { url: null, needs_review: false };
  let parsed: URL;
  try {
    parsed = new URL(value);
  } catch {
    return { url: null, needs_review: true };
  }
  if ((parsed.protocol !== 'http:' && parsed.protocol !== 'https:') || !parsed.host) {
    return { url: null, needs_review: true };
  }
  const stripped =
    parsed.username !== '' ||
    parsed.password !== '' ||
    parsed.search !== '' ||
    parsed.hash !== '';
  parsed.username = '';
  parsed.password = '';
  parsed.search = '';
  parsed.hash = '';
  // `toString()` normalizes like the Rust `Url::to_string()` view does.
  return { url: parsed.toString(), needs_review: stripped };
}
