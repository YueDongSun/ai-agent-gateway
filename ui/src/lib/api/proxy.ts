/// Proxy-side types + Tauri command wrappers.
/// Mirror Rust types from `src-tauri/src/proxy/`.
import { invoke } from '@tauri-apps/api/core';
import type { Provider_failure_health } from './provider_failure_health.js';

// ----- Types -----

export interface ProxyProvider {
  id: string;
  name: string;
  type: string;
  apiKey: string;
  baseURL: string;
  models: string[];
  enabled: boolean;
  timeoutMs: number;
  weight: number;
  circuitBreaker: CircuitBreakerConfig;
  /** Per-provider model mapping: display model -> upstream model.
   *  Example: { "claude-sonnet-5": "deepseek-v4-pro" } */
  modelMapping: Record<string, string>;
/** Per-provider extra request headers merged into every upstream request.
   *  Reserved auth/content headers are never overridden.
   *  Example: { "x-opencode-session": "..." } */
  extraHeaders: Record<string, string>;
  /** Model auto-discovery opt-out (discovery upgrades this provider). */
  autoTrackModels: boolean;
  /** Optional display icon: designed glyph id or literal character.
   *  Presentation-only; absent on rows saved before the icon fields. */
  icon?: string | null;
  /** Optional display tile color for `icon` (hex string). */
  iconColor?: string | null;
}

export interface CircuitBreakerConfig {
  failureThreshold: number;
  cooldownMs: number;
  halfOpenRetries: number;
  successThreshold: number;
}

export interface RouteHop {
  provider: string;
  model: string;
}

export interface Route {
  alias: string;
  description: string;
  hops: RouteHop[];
}

export interface ProxyConfig {
  host: string;
  port: number;
  authToken: string;
  enabled: boolean;
}

export interface ProxyStatus {
  running: boolean;
  config: ProxyConfig;
  /** True when the running proxy's routing snapshot no longer matches the
   *  current database configuration (routes or pool providers were added /
   *  edited / removed since the last `proxy_start`). False when the proxy
   *  is not running and when the snapshot is current. */
  /** True only when a listener/auth change or a failed hot apply remains;
      ordinary provider/route/supplier edits hot-apply and clear this. */
  config_changed: boolean;
  /** Hot-reload view of the live routing snapshot. `lastError` holds a stable
   *  error code when the most recent reload attempt failed while the previous
   *  snapshot kept serving. Absent while the proxy is not running. */
  hot_reload?: {
    generation: number;
    routingRevision: number;
    active: boolean;
    lastError: string | null;
  } | null;
}

export interface CircuitState {
  state: 'closed' | 'open' | 'half-open';
  failures: number;
  successes: number;
  openedAt?: number;
  cooldownMs: number;
}

export interface ProviderHealth {
  id: string;
  enabled: boolean;
  breaker: CircuitState;
  /** Derived terminal-attempt failure view for this provider. An
   *  `observed: false` view is unobserved, never a measured zero. */
  failure: Provider_failure_health;
}

export interface ProxyHealth {
  status: 'ok';
  runtime_kind?: 'desktop' | 'node';
  /** Configured runnable catalog, not individual inference validation. */
  availableModels?: string[];
  available_models?: string[];
  providers: ProviderHealth[];
  models: string[];
  lastUsed: string | null;
  pinnedProvider: string | null;
}

// ----- Tauri commands (defined in src-tauri/src/lib.rs) -----

/// Read the proxy listen config (host/port/authToken) for the endpoint card.
export async function getProxyConfig(): Promise<ProxyConfig> {
  return invoke<ProxyConfig>('proxy_config_get');
}

/// Update the proxy listen config (host/port/authToken). Applies on next start.
export async function saveProxyConfig(config: ProxyConfig): Promise<void> {
  await invoke('proxy_config_save', { config });
}

export async function getProxyStatus(): Promise<ProxyStatus> {
  return invoke<ProxyStatus>('proxy_status');
}

export async function startProxy(): Promise<string> {
  return invoke<string>('proxy_start');
}

export async function stopProxy(): Promise<void> {
  await invoke('proxy_stop');
}

// ----- Proxy providers + Routes CRUD (failover config) -----

export async function listProxyProviders(): Promise<ProxyProvider[]> {
  return invoke<ProxyProvider[]>('proxy_provider_list');
}

export async function saveProxyProvider(provider: ProxyProvider): Promise<void> {
  await invoke('proxy_provider_save', { provider });
}

export async function deleteProxyProvider(id: string): Promise<void> {
  await invoke('proxy_provider_delete', { id });
}

/// Proxy-provider ids referenced by a supplier endpoint projection. Supplier
/// saves own those rows' contents, and the backend refuses direct deletion
/// (`AAG_SUPPLIER_PROJECTION_IN_USE`), so the providers page hides direct
/// edit/delete actions for them.
export async function listSupplierMappedProviders(): Promise<string[]> {
  return invoke<string[]>('proxy_provider_supplier_mapped');
}

export async function listRoutes(): Promise<Route[]> {
  return invoke<Route[]>('route_list');
}

export async function saveRoute(route: Route): Promise<void> {
  await invoke('route_save', { route });
}

export async function deleteRoute(alias: string): Promise<void> {
  await invoke('route_delete', { alias });
}

/// Rename a route's alias in place (hops/description preserved).
/// Rejects an empty target or a target that already exists.
export async function renameRoute(from: string, to: string): Promise<void> {
  await invoke('route_rename', { from, to });
}

/// Review fix: `reorderRouteHops` was the Phase 18 drag-and-drop backend
/// round-trip. The current route editor reorders hops locally (see AppPage
/// `applyHopMove`), and no `route_reorder_hops` command exists in the
/// backend — re-adding a wrapper would target a missing command.

/// Get the currently pinned proxy provider (force-pin mode). Returns null if none.
export async function getPinnedProvider(): Promise<string | null> {
  return invoke<string | null>('proxy_get_pinned_provider');
}

/// Set (or clear) the pinned proxy provider. Empty string clears (restores normal failover).
export async function setPinnedProvider(providerId: string): Promise<void> {
  await invoke('proxy_set_pinned_provider', { providerId });
}

/// Failover-queue spread mode: `order` (configured order — the default),
/// `rotate` (round-robin the chain's first position), `weight` (smooth
/// weighted round-robin by each provider's weight), or `usage` (least
/// recorded token traffic over the last hour first). Only queue-built
/// chains are spread; explicit routes keep their order in every mode.
export type RoutingMode = 'order' | 'rotate' | 'weight' | 'usage';

export async function getRoutingMode(): Promise<RoutingMode> {
  return invoke<RoutingMode>('proxy_get_routing_mode');
}

/// Persist and hot-apply the spread mode. Invalid values are rejected
/// backend-side with `routing_mode_invalid`.
export async function setRoutingMode(mode: RoutingMode): Promise<void> {
  await invoke('proxy_set_routing_mode', { mode });
}

/// Read the `/v1/responses` chain-of-thought prompt toggle (true =
/// built-in reasoning system prompt injected into Codex-surface requests).
export async function getResponsesCot(): Promise<boolean> {
  return invoke<boolean>('proxy_get_responses_cot');
}

/// Toggle the built-in chain-of-thought prompt for `/v1/responses`.
/// Immediate effect on the running proxy; persisted for the next start.
export async function setResponsesCot(enabled: boolean): Promise<void> {
  await invoke('proxy_set_responses_cot', { enabled });
}

/// Test a proxy provider's connectivity by sending a minimal request.
export interface TestResult {
  success: boolean;
  latencyMs: number;
  model?: string;
  reply?: string;
  error?: string;
  providerId: string;
}

export async function testProvider(providerId: string, testModel?: string): Promise<TestResult> {
  return invoke<TestResult>('proxy_test_provider', { providerId, testModel: testModel ?? null });
}

/// Fetch available models from a provider's /v1/models endpoint.
export async function fetchProviderModels(providerId: string): Promise<Array<{ id: string; owned_by?: string }>> {
  return invoke<Array<{ id: string; owned_by?: string }>>('proxy_fetch_models', { providerId });
}

// ----- Model auto-discovery -----

export interface ModelHopRewrite {
  alias: string;
  oldModel: string;
  newModel: string;
}

export interface ModelMappingRewrite {
  display: string;
  oldModel: string;
  newModel: string;
}

export interface ModelDiscoveryProviderReport {
  providerId: string;
  ok: boolean;
  error?: string;
  addedModels: string[];
  hopRewrites: ModelHopRewrite[];
  mappingRewrites: ModelMappingRewrite[];
  skippedAmbiguous: string[];
}

export type ModelDiscoveryTrigger = 'startup' | 'periodic' | 'manual';

export interface ModelDiscoveryReport {
  ranAt: string;
  trigger: ModelDiscoveryTrigger;
  hasChanges: boolean;
  deduplicated: boolean;
  providers: ModelDiscoveryProviderReport[];
}

export interface ModelDiscoveryStatus {
  report: ModelDiscoveryReport | null;
  pendingRestart: boolean;
  intervalHours: number;
  lastRunEpoch: number;
}

/// Run model discovery manually: one provider when `providerId` is given,
/// every eligible provider otherwise. Returns the applied report.
export async function refreshProviderModels(providerId?: string): Promise<ModelDiscoveryReport> {
  return invoke<ModelDiscoveryReport>('proxy_refresh_models', { providerId: providerId ?? null });
}

/// Read the stored discovery status (last report + pending hot-apply flag).
export async function getDiscoveryStatus(): Promise<ModelDiscoveryStatus> {
  return invoke<ModelDiscoveryStatus>('model_discovery_status');
}

/// Toggle model auto-discovery for one pool provider.
export async function setProviderAutoTrack(providerId: string, enabled: boolean): Promise<void> {
  await invoke('proxy_set_auto_track', { providerId, enabled });
}

// ----- Client endpoint (bind host -> dialable target) -----

/// The host a local client must dial for this listen configuration. A
/// wildcard bind is not a client target: `0.0.0.0`/`::` mean "accept on
/// every interface", and the `localhost` alias or an unset host both mean
/// the local loopback. Any other host is a real endpoint the user chose.
/// Mirrors `ProxyConfig::client_endpoint_host` in
/// `src-tauri/src/database/dao/proxy_config.rs`, which applies the same
/// rule when it writes a client's endpoint configuration.
function client_endpoint_host(host: string): string {
  switch (host.trim()) {
    case '':
    case '0.0.0.0':
    case 'localhost':
      return '127.0.0.1';
    case '::':
    case '::1':
      return '[::1]';
    default:
      return host.trim();
  }
}

/// The endpoint URL to display, copy, and dial for this listen
/// configuration — never the raw bind host.
export function proxy_client_url(config: ProxyConfig): string {
  return `http://${client_endpoint_host(config.host)}:${config.port}`;
}

// ----- Direct HTTP fetches (call the proxy as a client would) -----

export async function fetchProxyHealth(
  config: ProxyConfig,
  signal?: AbortSignal,
): Promise<ProxyHealth> {
  const headers: Record<string, string> = {};
  if (config.authToken) headers['authorization'] = `Bearer ${config.authToken}`;
  const res = await fetch(`${proxy_client_url(config)}/health`, {
    headers,
    signal,
  });
  if (!res.ok) throw new Error(`Gateway health request failed (${res.status})`);
  return (await res.json()) as ProxyHealth;
}

export async function getGatewayHealth(signal?: AbortSignal): Promise<ProxyHealth> {
  const cfg = await getProxyConfig();
  return fetchProxyHealth(cfg, signal);
}

// Refactor cleanup: `fetchProxyModels` (direct /v1/models HTTP fetch) was
// exported but never called - the UI uses `fetchProviderModels` (the
// `proxy_fetch_models` Tauri command) instead. Removed.
