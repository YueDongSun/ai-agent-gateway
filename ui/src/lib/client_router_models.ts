import type { AppType } from './api/types.js';

export type Model_slot = 'default' | 'fable' | 'opus' | 'sonnet' | 'haiku';
export interface Router_model { model: string; slots: Model_slot[] }

const claude_slots = [
  ['fable', 'ANTHROPIC_DEFAULT_FABLE_MODEL'],
  ['opus', 'ANTHROPIC_DEFAULT_OPUS_MODEL'],
  ['sonnet', 'ANTHROPIC_DEFAULT_SONNET_MODEL'],
  ['haiku', 'ANTHROPIC_DEFAULT_HAIKU_MODEL'],
] as const;

function record_of(value: unknown): Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value) ? value as Record<string, unknown> : {};
}

function selector_of(value: unknown): string | null {
  return typeof value === 'string' && value.trim().length > 0 ? value : null;
}

/** Allowlist configured selections, never inventories, endpoints, credentials,
 * source identities or upstream mappings. Exact selector spelling is retained. */
export function project_router_models(app_type: AppType, settings: unknown): Router_model[] {
  const config = record_of(settings);
  const env = record_of(config.env);
  const models = new Map<string, Router_model>();
  const add_model = (slot: Model_slot, value: unknown) => {
    const model = selector_of(value);
    if (!model) return;
    const existing = models.get(model);
    if (existing) existing.slots.push(slot);
    else models.set(model, { model, slots: [slot] });
  };
  if (app_type === 'claude_code' || app_type === 'claude_vscode') {
    add_model('default', selector_of(env.ANTHROPIC_MODEL) ?? config.model);
    for (const [slot, key] of claude_slots) add_model(slot, env[key]);
  } else if (app_type === 'gemini') {
    add_model('default', env.GEMINI_MODEL);
  } else if (app_type === 'opencode' || app_type === 'kilo_cli' || app_type === 'kilo_vscode') {
    // Existing editors store their selected model as the first map key.
    // The native OpenCode/Kilo writers use that key when model is absent.
    add_model('default', selector_of(config.model) ?? Object.keys(record_of(config.models))[0]);
  } else {
    add_model('default', config.model);
  }
  return [...models.values()];
}
