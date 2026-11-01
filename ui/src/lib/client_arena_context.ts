import type { AppType } from './api/types.js';
import type { Router_model } from './client_router_models.js';

const client_surfaces = {
  claude_code: 'claude', claude_vscode: 'claude', codex: 'codex', gemini: 'gemini',
  copilot: 'chat', opencode: 'chat', pi: 'chat', antigravity: 'chat',
  kilo_cli: 'chat', cline_cli: 'chat', kilo_vscode: 'chat',
} as const;

/** Model selectors cross this boundary literally; the global supply inventory
 * cannot add options or substitute a default for an unconfigured client. */
export function client_arena_context(app_type: AppType, models: readonly Router_model[]) {
  return {
    surface: client_surfaces[app_type],
    model_options: models.map(({ model }) => ({ selector: model, label: model })),
    initial_selector: (models.find(({ slots }) => slots.includes('default')) ?? models[0])?.model ?? null,
  };
}
