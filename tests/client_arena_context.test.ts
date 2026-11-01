import { describe, expect, it } from 'vitest';
import { client_arena_context } from '../ui/src/lib/client_arena_context.js';
import type { Router_model } from '../ui/src/lib/client_router_models.js';

describe('Coding Agent Arena model context', () => {
  it.each([
    ['claude_code', 'claude'], ['claude_vscode', 'claude'],
    ['codex', 'codex'], ['gemini', 'gemini'],
    ['copilot', 'chat'], ['opencode', 'chat'], ['pi', 'chat'],
    ['kilo_cli', 'chat'], ['cline_cli', 'chat'], ['kilo_vscode', 'chat'],
  ] as const)('uses the %s client surface without selecting a gateway default', (app_type, surface) => {
    expect(client_arena_context(app_type, [])).toEqual({
      surface, model_options: [], initial_selector: null,
    });
  });

  it('offers only the current Claude configuration model and tier selections', () => {
    expect(client_arena_context('claude_code', [
      { model: 'alias:fable', slots: ['default', 'opus'] },
      { model: 'fixture-balanced-route', slots: ['sonnet'] },
      { model: 'precise:Exact/Fast-v1', slots: ['haiku'] },
    ])).toEqual({
      surface: 'claude', initial_selector: 'alias:fable',
      model_options: [
        { selector: 'alias:fable', label: 'alias:fable' },
        { selector: 'fixture-balanced-route', label: 'fixture-balanced-route' },
        { selector: 'precise:Exact/Fast-v1', label: 'precise:Exact/Fast-v1' },
      ],
    });
  });

  it('keeps the selected Codex selector literal and case intact', () => {
    expect(client_arena_context('codex', [{ model: 'Exact/Router-ID', slots: ['default'] }])).toEqual({
      surface: 'codex', initial_selector: 'Exact/Router-ID',
      model_options: [{ selector: 'Exact/Router-ID', label: 'Exact/Router-ID' }],
    });
  });

  it('chooses the declared default even when role entries precede it', () => {
    expect(client_arena_context('claude_vscode', [
      { model: 'fixture-role', slots: ['sonnet'] },
      { model: 'fixture-primary', slots: ['default'] },
    ]).initial_selector).toBe('fixture-primary');
  });

  it('uses the first configured role if the current plan has no default slot', () => {
    expect(client_arena_context('claude_code', [
      { model: 'fixture-opus-route', slots: ['opus'] },
      { model: 'fixture-haiku-route', slots: ['haiku'] },
    ]).initial_selector).toBe('fixture-opus-route');
  });

  it('projects model-only options without passing extra source fields or mutating its input', () => {
    const configured_models = [Object.freeze({
      model: 'fixture-selected-route', slots: Object.freeze(['default']),
      inventory: ['fixture-unselected-route'], supplier_name: 'fixture-private-label',
    })] as unknown as Router_model[];
    const result = client_arena_context('copilot', configured_models);
    expect(result).toEqual({
      surface: 'chat', initial_selector: 'fixture-selected-route',
      model_options: [{ selector: 'fixture-selected-route', label: 'fixture-selected-route' }],
    });
    expect(configured_models[0].model).toBe('fixture-selected-route');
    expect(configured_models[0].slots).toEqual(['default']);
  });
});
