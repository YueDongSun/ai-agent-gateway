import { describe, expect, it } from 'vitest';
import { APP_PAGES } from '../ui/src/lib/appPages.js';
import type { Provider } from '../ui/src/lib/api/types.js';

describe('Codex reasoning configuration', () => {
  it('round trips the chosen model and effort through the provider form', () => {
    const original = { settingsConfig: { baseUrl: 'https://example.invalid', model: '5.6sol', model_reasoning_effort: 'high' } } as Provider;
    const values = APP_PAGES.codex.extractForm(original);
    expect(values.model).toBe('5.6sol');
    expect(values.model_reasoning_effort).toBe('high');
    const saved = APP_PAGES.codex.buildConfig('fixture', { ...values, model_reasoning_effort: 'max' });
    expect(saved.model).toBe('5.6sol');
    expect(saved.model_reasoning_effort).toBe('max');
    expect(saved.config).toContain('model_reasoning_effort = "max"');
  });

  it('leaves effort absent when the user selects the model default', () => {
    const saved = APP_PAGES.codex.buildConfig('fixture', { baseUrl: '', apiKey: '', model: 'plain-model', model_reasoning_effort: '' });
    expect(saved).not.toHaveProperty('model_reasoning_effort');
    expect(saved.config).not.toContain('model_reasoning_effort');
  });

  it('preserves the existing non-Codex provider form behavior', () => {
    const values = { baseUrl: 'https://example.invalid', apiKey: 'fixture_key', model: 'claude-fixture', model_reasoning_effort: 'high' };
    expect(APP_PAGES.claude_code.buildConfig('fixture', values)).toEqual({
      env: { ANTHROPIC_BASE_URL: values.baseUrl, ANTHROPIC_AUTH_TOKEN: values.apiKey },
      model: values.model,
    });
    expect(APP_PAGES.opencode.buildConfig('fixture', values)).not.toHaveProperty('model_reasoning_effort');
  });
});
