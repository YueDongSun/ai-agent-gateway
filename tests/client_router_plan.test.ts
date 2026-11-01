import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { project_router_models } from '../ui/src/lib/client_router_models.js';

const invoke = vi.fn();
import { get_current_client_plan } from '../ui/src/lib/api/client_router_plan.js';

beforeEach(() => vi.stubGlobal('window', { __TAURI_INTERNALS__: { invoke } }));
afterEach(() => vi.unstubAllGlobals());

describe('current configuration router model boundary', () => {
  it('uses Claude model and tier settings, without expanding unrelated inventory', () => {
    expect(project_router_models('claude_code', {
      name: 'private supplier', model: 'legacy-default',
      env: { ANTHROPIC_MODEL: 'route-default', ANTHROPIC_DEFAULT_SONNET_MODEL: 'route-balanced', ANTHROPIC_DEFAULT_OPUS_MODEL: 'route-default', ANTHROPIC_API_KEY: 'secret' },
      models: { 'unselected-model': {} },
    })).toEqual([
      { model: 'route-default', slots: ['default', 'opus'] },
      { model: 'route-balanced', slots: ['sonnet'] },
    ]);
  });
  it.each(['codex', 'copilot', 'opencode', 'kilo_cli', 'cline_cli', 'kilo_vscode', 'pi'] as const)('shows only the selected model for %s', (app_type) => {
    expect(project_router_models(app_type, { model: 'Exact/Router-ID', models: { 'other-model': {} }, modelCatalog: { models: [{ display: 'other-route', model: 'private-upstream' }] }, baseUrl: 'private-endpoint' }))
      .toEqual([{ model: 'Exact/Router-ID', slots: ['default'] }]);
  });
  it('uses the Gemini model only on the Gemini page', () => {
    const settings = { env: { GEMINI_MODEL: 'gemini-route', ANTHROPIC_MODEL: 'foreign-route' } };
    expect(project_router_models('gemini', settings)).toEqual([{ model: 'gemini-route', slots: ['default'] }]);
    expect(project_router_models('codex', settings)).toEqual([]);
  });
  it('supports Claude VS Code and the existing legacy top-level model', () => {
    expect(project_router_models('claude_vscode', { model: 'legacy-selector' })).toEqual([{ model: 'legacy-selector', slots: ['default'] }]);
  });
  it.each(['opencode', 'kilo_cli', 'kilo_vscode'] as const)('uses the existing models-map selection for %s when no explicit model exists', (app_type) => {
    expect(project_router_models(app_type, { models: { 'configured-router': { name: 'display label' }, 'another-choice': {} } }))
      .toEqual([{ model: 'configured-router', slots: ['default'] }]);
  });
  it('does not invent models from catalogs, endpoint strings, malformed values, or missing fields', () => {
    for (const settings of [null, [], {}, { model: ' ' }, { model: 3 }, { model: { id: 'not-a-selector' } }, { models: ['catalog-only'], modelCatalog: { models: [{ display: 'not-selected' }] } }]) {
      expect(project_router_models('codex', settings)).toEqual([]);
    }
  });
  it('returns only the selected record model data without a provider list or write', async () => {
    invoke.mockReset();
    invoke.mockResolvedValueOnce('selected-id').mockResolvedValueOnce({ id: 'selected-id', name: 'supplier-name', settingsConfig: { model: 'current-route', apiKey: 'secret', baseUrl: 'private-endpoint' } }).mockResolvedValueOnce('selected-id');
    expect(await get_current_client_plan('codex')).toEqual({ app_type: 'codex', models: [{ model: 'current-route', slots: ['default'] }] });
    expect(invoke.mock.calls.map(([command]) => command)).toEqual(['provider_current', 'provider_get', 'provider_current']);
    expect(invoke.mock.calls[1][1]).toEqual({ id: 'selected-id', appType: 'codex' });
  });
  it('returns an empty plan when no active record exists', async () => {
    invoke.mockReset();
    invoke.mockResolvedValueOnce(null);
    expect(await get_current_client_plan('copilot')).toEqual({ app_type: 'copilot', models: [] });
    expect(invoke).toHaveBeenCalledTimes(1);
  });
  it('rejects a selection that changes during the read instead of displaying the previous plan', async () => {
    invoke.mockReset();
    invoke.mockResolvedValueOnce('old').mockResolvedValueOnce({ settingsConfig: { model: 'old-model' } }).mockResolvedValueOnce('new');
    await expect(get_current_client_plan('codex')).rejects.toThrow('client_plan_changed');
  });
});
