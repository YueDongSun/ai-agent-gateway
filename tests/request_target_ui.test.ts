import { describe, expect, it } from 'vitest';
import { createRequire } from 'node:module';
import { Request_status_rows } from '../ui/src/components/app/request_status.js';
import { Codex_reasoning_selector } from '../ui/src/components/app/codex_reasoning_fields.js';
import { request_status_for_surface, visible_request_status, type Request_status } from '../ui/src/lib/api/request_status.js';
import { I18nCtx } from '../ui/src/lib/ctx.js';
import { t } from '../ui/src/i18n/index.js';

const ui_require = createRequire(new URL('../ui/package.json', import.meta.url));
const { createElement } = ui_require('react');
const { renderToStaticMarkup } = ui_require('react-dom/server');
function render(component: unknown, props: unknown) {
  return renderToStaticMarkup(createElement(I18nCtx.Provider, { value: { t: (key: string, vars: Record<string, string | number>) => t('en', key, vars) } }, createElement(component, props)));
}
const request: Request_status = {
  requestId: 'fixture_request', surface: 'responses', requestedModel: '5.6sol',
  resolvedModel: 'gpt-5.6-sol-backend', phase: 'requesting', fallbackCount: 0,
  errorCode: null, configGeneration: 17, status: null,
};

describe('live request target presentation', () => {
  it('renders the concrete backend model separately from the selected alias', () => {
    const html = render(Request_status_rows, { rows: [request] });
    expect(html).toContain('5.6sol → gpt-5.6-sol-backend');
    expect(html).toContain('Requesting');
  });
  it('renders stream progress and fallback/error information', () => {
    const html = render(Request_status_rows, { rows: [{ ...request, resolvedModel: 'fallback-model', phase: 'streaming', fallbackCount: 1, errorCode: 'upstream_timeout' }] });
    expect(html).toContain('5.6sol → fallback-model');
    expect(html).toContain('Streaming');
    expect(html).toContain('Fallback 1');
    expect(html).toContain('upstream_timeout');
  });
  it('selects requests for the current surface, keeping overlapping requests distinct', () => {
    const rows = [{ ...request, surface: 'chat', requestId: 'chat' }, request, { ...request, requestId: 'second' }];
    expect(request_status_for_surface(rows, 'codex').map((row) => row.requestId)).toEqual(['fixture_request', 'second']);
  });
  it('retains the latest terminal result while a newer request is still active', () => {
    const rows: Request_status[] = [request, { ...request, requestId: 'failed_earlier', phase: 'error' }, { ...request, requestId: 'old_result', phase: 'completed' }];
    expect(visible_request_status(rows).map((row) => row.requestId)).toEqual(['fixture_request', 'failed_earlier']);
  });
});

describe('Codex effort picker capabilities', () => {
  it('renders all declared tiers as an enabled selector', () => {
    const html = render(Codex_reasoning_selector, {
      models: [{ model: '5.6sol', supportedEfforts: ['low', 'medium', 'high', 'xhigh', 'max', 'ultra'] }],
      values: { model: '5.6sol', model_reasoning_effort: 'max' }, on_change: () => {},
    });
    expect(html).not.toContain('disabled');
    for (const effort of ['low', 'medium', 'high', 'xhigh', 'max', 'ultra']) expect(html).toContain(`value="${effort}"`);
    expect(html).toContain('value="max" selected=""');
  });
  it('offers no invented tiers for a nonreasoning model', () => {
    const html = render(Codex_reasoning_selector, { models: [{ model: 'plain', supportedEfforts: [] }], values: { model: 'plain' }, on_change: () => {} });
    expect(html).toContain('disabled');
    expect(html).not.toContain('value="high"');
  });
  it('uses the authoritative default instead of the alphabetically first model', () => {
    const html = render(Codex_reasoning_selector, { models: [
      { model: 'luna', supportedEfforts: [] },
      { model: 'sol', supportedEfforts: ['low', 'high'], isDefault: true },
    ], values: {}, on_change: () => {} });
    expect(html).toContain('value="sol" selected=""');
    expect(html).toContain('value="high"');
    expect(html).not.toContain('disabled');
  });
  it('shows and allows clearing a saved effort removed by a mapping update', () => {
    const html = render(Codex_reasoning_selector, { models: [{ model: 'plain', supportedEfforts: [] }], values: { model: 'plain', model_reasoning_effort: 'max' }, on_change: () => {} });
    expect(html).toContain('max (unavailable for this model)');
    expect(html).not.toContain('disabled');
  });
});
