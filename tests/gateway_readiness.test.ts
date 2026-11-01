import { describe, expect, it } from 'vitest';
import { gateway_readiness } from '../ui/src/lib/gateway_readiness.js';
import type { ProxyHealth, ProxyStatus } from '../ui/src/lib/api/proxy.js';

const status: ProxyStatus = { running: true, config: { host: '127.0.0.1', port: 1234, authToken: '', enabled: true }, config_changed: false };
const health: ProxyHealth = { status: 'ok', runtime_kind: 'desktop', providers: [], models: ['declared-only'], availableModels: [], lastUsed: null, pinnedProvider: null };

describe('effective gateway readiness', () => {
  it('does not treat a live listener and advertised alias as an available model', () => {
    expect(gateway_readiness(status, health)).toEqual({ runtime_kind: 'desktop', available_model_count: 0, reason: 'no_models' });
  });
  it('keeps saved listener edits pending and exposes failed hot apply', () => {
    expect(gateway_readiness({ ...status, config_changed: true }, { ...health, availableModels: ['offered'] }).reason).toBe('restart_required');
    expect(gateway_readiness({ ...status, hot_reload: { generation: 1, routingRevision: 2, active: true, lastError: 'snapshot_failed' } }, health).reason).toBe('reload_failed');
  });
  it('reports old runtime readiness as unverified', () => {
    expect(gateway_readiness(status, { ...health, runtime_kind: undefined, availableModels: undefined })).toEqual({ runtime_kind: 'unknown', available_model_count: null, reason: 'unverified' });
  });
  it('identifies Node and uses its configured candidate catalog', () => {
    expect(gateway_readiness(status, { ...health, runtime_kind: 'node', availableModels: undefined, available_models: ['exact-offered'] })).toEqual({ runtime_kind: 'node', available_model_count: 1, reason: 'ready' });
  });
});
