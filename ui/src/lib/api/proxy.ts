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

export async function getProxyStatus(): Promise<ProxyStatus> {
  return invoke<ProxyStatus>('proxy_status');
}

export async function startProxy(): Promise<string> {
  return invoke<string>('proxy_start');
}

export async function stopProxy(): Promise<void> {
  await invoke('proxy_stop');
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