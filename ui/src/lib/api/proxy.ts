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

// ----- Direct HTTP fetches (call the proxy as a client would) -----

export async function fetchProxyHealth(
  config: ProxyConfig,
): Promise<ProxyHealth> {
  const headers: Record<string, string> = {};
  if (config.authToken) headers['authorization'] = `Bearer ${config.authToken}`;
  const res = await fetch(`http://${config.host}:${config.port}/health`, {
    headers,
  });
  return (await res.json()) as ProxyHealth;
}

export async function fetchProxyModels(config: ProxyConfig): Promise<string[]> {
  const headers: Record<string, string> = {};
  if (config.authToken) headers['authorization'] = `Bearer ${config.authToken}`;
  const res = await fetch(`http://${config.host}:${config.port}/v1/models`, {
    headers,
  });
  const body = (await res.json()) as { data: Array<{ id: string }> };
  return body.data.map((m) => m.id);
}