/// Per-app UI configuration for the unified provider-chain page
/// (Phase 17, cc-switch style). Every app gets the same card list —
/// drag-reorder, CURRENT badge, per-card failover toggle, Enable/In-Use —
/// with app-specific form fields and endpoint/launcher slots.
import type { AppType, Provider } from './api/types';

export type FieldType = 'text' | 'password' | 'select';

export interface FieldDef {
  key: string;
  label: string;
  type: FieldType;
  placeholder?: string;
  options?: string[];
  secret?: boolean;
}

export interface AppPageConfig {
  appType: AppType;
  label: string;
  /** Form fields (smart form; the modal renders them in order). */
  fields: FieldDef[];
  /** The settings_config keys the form reads/writes flat. */
  extractForm: (p: Provider) => Record<string, string>;
  /** Build settings_config from form values. */
  buildConfig: (id: string, values: Record<string, string>) => Record<string, unknown>;
  /** URL shown on the card (extracted from settings_config). */
  cardUrl: (cfg: Record<string, unknown>) => string;
  /** Secondary line shown on the card (model etc.). */
  cardMeta: (cfg: Record<string, unknown>) => string;
  /** Endpoint card: which proxy surface this app's chain serves. */
  surface: 'claude' | 'codex' | 'chat';
  /** Show the cc-switch import + endpoint-paste card (Claude only). */
  ccSwitchUpstream: boolean;
  /** Show the gateway status card. */
  showGatewayCard: boolean;
  /** Show the launcher buttons (Codex page). */
  launchers: 'codex' | 'copilot' | 'none';
  /** Model options for the model select. */
  models?: string[];
  /** Hidden from the AppSwitcher (code kept, tab disabled). */
  enabled?: boolean;
}

const CODEX_MODELS = ['codex-5-6-sol', 'codex-5-6-luna', 'codex-5-6-terra', 'codex-5-5'];
const CODEX_DISPLAY: Record<string, string> = {
  'codex-5-6-sol': '5.6Sol',
  'codex-5-6-luna': '5.6Luna',
  'codex-5-6-terra': '5.6Terra',
  'codex-5-5': '5.5',
};
const CLAUDE_MODELS = [
  'claude-fable-5',
  'claude-opus-4-8',
  'claude-opus-4-7',
  'claude-sonnet-5',
  'claude-sonnet-4-6',
  'claude-haiku-4-5',
];
const COPILOT_MODELS = ['claude-sonnet-5', 'claude-sonnet-4-6', 'gpt-5', 'gpt-4o'];

const envOf = (cfg: Record<string, unknown>): Record<string, unknown> =>
  (cfg.env as Record<string, unknown>) ?? {};

/// claude_code: `{ env: { ANTHROPIC_BASE_URL, ANTHROPIC_AUTH_TOKEN }, model }`
const claudeCfg: AppPageConfig = {
  appType: 'claude_code',
  label: 'Claude',
  fields: [
    { key: 'baseUrl', label: 'baseUrl', type: 'text', placeholder: 'https://api.anthropic.com' },
    { key: 'apiKey', label: 'apiKey', type: 'password' },
    { key: 'model', label: 'model', type: 'select', options: CLAUDE_MODELS },
  ],
  extractForm: (p) => {
    const env = envOf(p.settingsConfig as Record<string, unknown>);
    return {
      baseUrl: String(env.ANTHROPIC_BASE_URL ?? ''),
      apiKey: String(env.ANTHROPIC_AUTH_TOKEN ?? ''),
      model: String((p.settingsConfig as Record<string, unknown>).model ?? CLAUDE_MODELS[1]),
    };
  },
  buildConfig: (_id, v) => ({
    env: {
      ANTHROPIC_BASE_URL: v.baseUrl,
      ...(v.apiKey ? { ANTHROPIC_AUTH_TOKEN: v.apiKey } : {}),
    },
    ...(v.model ? { model: v.model } : {}),
  }),
  cardUrl: (cfg) => String(envOf(cfg).ANTHROPIC_BASE_URL ?? ''),
  cardMeta: (cfg) => String(cfg.model ?? ''),
  surface: 'claude',
  ccSwitchUpstream: true,
  showGatewayCard: true,
  launchers: 'none',
};

/// codex: flat `{ baseUrl, apiKey, auth, config }` (mirror-write shape).
/// The `model` field is removed — model selection is handled entirely by the
/// model catalog editor. The default model is derived from the first catalog
/// entry (in handleSave).
const codexCfg: AppPageConfig = {
  appType: 'codex',
  label: 'Codex',
  fields: [
    { key: 'baseUrl', label: 'baseUrl', type: 'text', placeholder: 'http://127.0.0.1:8989' },
    { key: 'apiKey', label: 'apiKey', type: 'password' },
  ],
  extractForm: (p) => {
    const cfg = p.settingsConfig as Record<string, unknown>;
    const auth = cfg.auth as Record<string, unknown> | undefined;
    const authVal = (auth as { tokens?: { value?: unknown } } | undefined)?.tokens?.value
      ?? (auth as Record<string, unknown> | undefined)?.['OPENAI_API_KEY'];
    const toml = String(cfg.config ?? '');
    const tomlBase = toml
      .split('\n')
      .map((l) => l.trim())
      .filter((l) => l.startsWith('base_url'))
      .find((l) => l.includes('='))
      ?.split('=')
      .slice(1)
      .join('=')
      .trim()
      .trim();
    return {
      baseUrl: String(cfg.baseUrl ?? tomlBase ?? ''),
      apiKey: String(cfg.apiKey ?? authVal ?? ''),
    };
  },
  buildConfig: (_id, v) => {
    // model is set later from the first modelCatalog entry (in handleSave).
    const toml = [
      '',
      '[model_providers.aag]',
      'name = "AAG Gateway"',
      `base_url = "${v.baseUrl.replace(/\/+$/, '')}/v1"`,
      'wire_api = "responses"',
      '',
    ].join('\n');
    return {
      baseUrl: v.baseUrl,
      apiKey: v.apiKey,
      auth: v.apiKey
        ? { tokens: { value: v.apiKey, source: 'aag' } }
        : { OPENAI_API_KEY: null },
      config: toml,
    };
  },
  cardUrl: (cfg) => String(cfg.baseUrl ?? ''),
  cardMeta: (cfg) => {
    const mc = (cfg as Record<string, unknown>).modelCatalog as { models?: Array<{ display?: string }> } | undefined;
    return mc?.models?.[0]?.display ?? String((cfg as Record<string, unknown>).model ?? '');
  },
  surface: 'codex',
  ccSwitchUpstream: true,
  showGatewayCard: true,
  launchers: 'codex',
};

/// copilot: `{ model, env: { COPILOT_PROVIDER_BASE_URL/_API_KEY/_TYPE } }`
const copilotCfg: AppPageConfig = {
  appType: 'copilot',
  label: 'Copilot CLI',
  fields: [
    { key: 'baseUrl', label: 'baseUrl', type: 'text', placeholder: 'http://127.0.0.1:8989/v1' },
    { key: 'apiKey', label: 'apiKey', type: 'password' },
    { key: 'model', label: 'model', type: 'select', options: COPILOT_MODELS },
  ],
  extractForm: (p) => {
    const env = envOf(p.settingsConfig as Record<string, unknown>);
    return {
      baseUrl: String(env.COPILOT_PROVIDER_BASE_URL ?? ''),
      apiKey: String(env.COPILOT_PROVIDER_API_KEY ?? ''),
      model: String((p.settingsConfig as Record<string, unknown>).model ?? COPILOT_MODELS[0]),
    };
  },
  buildConfig: (_id, v) => ({
    model: v.model,
    env: {
      COPILOT_PROVIDER_BASE_URL: v.baseUrl,
      ...(v.apiKey ? { COPILOT_PROVIDER_API_KEY: v.apiKey } : {}),
      COPILOT_PROVIDER_TYPE: 'openai',
    },
  }),
  cardUrl: (cfg) => String(envOf(cfg).COPILOT_PROVIDER_BASE_URL ?? ''),
  cardMeta: (cfg) => String(cfg.model ?? ''),
  surface: 'chat',
  ccSwitchUpstream: true,
  showGatewayCard: true,
  launchers: 'copilot',
};

/// pi: flat `{ providerId, baseUrl, apiKey, model }`
const piCfg: AppPageConfig = {
  appType: 'pi',
  label: 'pi',
  fields: [
    { key: 'providerId', label: 'providerId', type: 'text', placeholder: 'aag' },
    { key: 'baseUrl', label: 'baseUrl', type: 'text', placeholder: 'http://127.0.0.1:8989/v1' },
    { key: 'apiKey', label: 'apiKey', type: 'password' },
    { key: 'model', label: 'model', type: 'text', placeholder: 'claude-sonnet-5' },
  ],
  extractForm: (p) => {
    const cfg = p.settingsConfig as Record<string, unknown>;
    return {
      providerId: String(cfg.providerId ?? p.id),
      baseUrl: String(cfg.baseUrl ?? ''),
      apiKey: String(cfg.apiKey ?? ''),
      model: String(cfg.model ?? ''),
    };
  },
  buildConfig: (id, v) => ({
    providerId: v.providerId || id,
    baseUrl: v.baseUrl,
    apiKey: v.apiKey,
    api: 'openai-completions',
    model: v.model,
    models: v.model ? [v.model] : [],
  }),
  cardUrl: (cfg) => String(cfg.baseUrl ?? ''),
  cardMeta: (cfg) => String(cfg.model ?? ''),
  surface: 'chat',
  ccSwitchUpstream: false,
  showGatewayCard: true,
  launchers: 'none',
  // Disabled: cc-switch now supports pi, so this page is hidden but kept.
  enabled: false,
};

export const APP_PAGES: Record<string, AppPageConfig> = {
  claude_code: claudeCfg,
  codex: codexCfg,
  copilot: copilotCfg,
  pi: piCfg,
};

export const APP_PAGE_ORDER: AppType[] = (['claude_code', 'codex', 'copilot', 'pi'] as AppType[]).filter(
  (at) => APP_PAGES[at].enabled !== false,
);

export { CODEX_MODELS, CLAUDE_MODELS, CODEX_DISPLAY };
