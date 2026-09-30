/// Proxy-side types + Tauri command wrappers.
/// Mirror Rust types from `src-tauri/src/proxy/`.
import { invoke } from '@tauri-apps/api/core';

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
}

export interface ProxyHealth {
  status: 'ok';
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
/// round-trip. The current route editor uses pointer-based local
/// reordering (see AppPage `applyHopMove`), so this export has no consumer
/// in the UI. The underlying `route_reorder_hops` Tauri command is
/// retained in lib.rs in case other code paths need it later — only the
/// dead frontend wrapper is removed.

/// Get the currently pinned proxy provider (force-pin mode). Returns null if none.
export async function getPinnedProvider(): Promise<string | null> {
  return invoke<string | null>('proxy_get_pinned_provider');
}

/// Set (or clear) the pinned proxy provider. Empty string clears (restores normal failover).
export async function setPinnedProvider(providerId: string): Promise<void> {
  await invoke('proxy_set_pinned_provider', { providerId });
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

// ----- Direct HTTP fetches (call the proxy as a client would) -----

export async function fetchProxyHealth(
  config: ProxyConfig,
  signal?: AbortSignal,
): Promise<ProxyHealth> {
  const headers: Record<string, string> = {};
  if (config.authToken) headers['authorization'] = `Bearer ${config.authToken}`;
  const res = await fetch(`http://${config.host}:${config.port}/health`, {
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
