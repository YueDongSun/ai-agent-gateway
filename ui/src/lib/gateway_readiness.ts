import type { ProxyHealth, ProxyStatus } from './api/proxy.js';

export type Gateway_readiness_reason = 'ready' | 'no_models' | 'restart_required' | 'reload_failed' | 'circuit_open' | 'unverified';
export interface Gateway_readiness {
  runtime_kind: 'desktop' | 'node' | 'unknown';
  available_model_count: number | null;
  reason: Gateway_readiness_reason;
}

/** Health proves listener reachability; its effective catalog proves eligibility only. */
export function gateway_readiness(status: ProxyStatus, health: ProxyHealth): Gateway_readiness {
  const available = health.availableModels ?? health.available_models;
  const runtime_kind = health.runtime_kind === 'desktop' || health.runtime_kind === 'node' ? health.runtime_kind : 'unknown';
  const available_model_count = Array.isArray(available) ? available.length : null;
  const reason = status.hot_reload?.lastError ? 'reload_failed'
    : status.config_changed ? 'restart_required'
    : available_model_count === 0 ? 'no_models'
    : available_model_count === null || runtime_kind === 'unknown' ? 'unverified'
    : health.providers.some((provider) => provider.breaker?.state === 'open') ? 'circuit_open'
    : 'ready';
  return { runtime_kind, available_model_count, reason };
}
