/// Supplier-owned client management forms and retained configuration contracts
/// (Phase 17, cc-switch style). Every app gets the same card list —
/// drag-reorder, CURRENT badge, per-card failover toggle, Enable/In-Use —
/// with app-specific form fields and endpoint/launcher slots.
import type { AppType, Provider } from './api/types';
import { client_label, client_page_order } from './client_apps.js';

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
  surface: 'claude' | 'codex' | 'chat' | 'gemini';
  /** Show the cc-switch import + endpoint-paste card (Claude only). */
  ccSwitchUpstream: boolean;
  /** Show the gateway status card. */
  showGatewayCard: boolean;
  /** Show this app's supported terminal or desktop launcher. */
  launchers: 'codex' | 'claude' | 'copilot' | 'gemini' | 'vscode' | 'managed_cli' | 'none';
  /** Model options for the model select. */
  models?: string[];
  /** Hidden from the AppSwitcher (code kept, tab disabled). */
  enabled?: boolean;
  /**
   * When set, the page renders a detect-only notice and no live write
   * happens. Use this for VS Code extensions where the JSON key namespace
   * is not yet source-verified.
   */
  detectOnly?: { reason: string };
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

const PRETTY_TOKENS: Record<string, string> = {
  gpt: 'GPT',
  glm: 'GLM',
  ai: 'AI',
  minimax: 'MiniMax',
  deepseek: 'DeepSeek',
  qwen: 'Qwen',
  kimi: 'Kimi',
  gemini: 'Gemini',
  claude: 'Claude',
  codex: 'Codex',
  grok: 'Grok',
  llama: 'Llama',
  mistral: 'Mistral',
  openai: 'OpenAI',
};

/// Human-readable label for a model id (`minimax-m3.1` -> "MiniMax M3.1").
/// Mirrors Rust `proxy::models::pretty_display_name`; ids stay the source
/// of truth — this is only for display.
export function prettyModelName(id: string): string {
  return id
    .replace(/\[1m\]$/i, '')
    .split(/[-_/ ]+/)
    .filter((t) => t.length > 0)
    .map((t) => PRETTY_TOKENS[t] ?? t.charAt(0).toUpperCase() + t.slice(1))
    .join(' ');
}

const envOf = (cfg: Record<string, unknown>): Record<string, unknown> =>
  (cfg.env as Record<string, unknown>) ?? {};

/// claude_code: `{ env: { ANTHROPIC_BASE_URL, ANTHROPIC_AUTH_TOKEN }, model }`
const claudeCfg: AppPageConfig = {
  appType: 'claude_code',
  label: client_label('claude_code'),
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
  launchers: 'claude',
};

/// codex: flat `{ baseUrl, apiKey, auth, config }` (mirror-write shape).
/// Model and effort selectors use the backend's generated Codex catalog.
const codexCfg: AppPageConfig = {
  appType: 'codex',
  label: client_label('codex'),
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
      .trim();
    return {
      baseUrl: String(cfg.baseUrl ?? tomlBase ?? ''),
      apiKey: String(cfg.apiKey ?? authVal ?? ''),
      model: String(cfg.model ?? ''),
      model_reasoning_effort: String(cfg.model_reasoning_effort ?? ''),
    };
  },
  buildConfig: (_id, v) => {
    const toml = [
      ...(v.model ? [`model = ${JSON.stringify(v.model)}`] : []),
      ...(v.model_reasoning_effort ? [`model_reasoning_effort = ${JSON.stringify(v.model_reasoning_effort)}`] : []),
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
      ...(v.model ? { model: v.model } : {}),
      ...(v.model_reasoning_effort ? { model_reasoning_effort: v.model_reasoning_effort } : {}),
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
  label: client_label('copilot'),
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

/// Shared OpenCode-family projection: `{ npm, options, models }` — the
/// same settings shape consumed by OpenCode, the Kilo CLI, and the Kilo
/// VS Code extension (whose `globalFiles()` reads the Kilo JSONC). The
/// pages differ only in identity, label, and launcher.
function opencode_projection_config(
  app_type: AppType,
  label: string,
  launchers: AppPageConfig['launchers'],
): AppPageConfig {
  return {
    appType: app_type,
    label,
    fields: [
      { key: 'baseUrl', label: 'baseUrl', type: 'text', placeholder: 'https://api.example.com/v1' },
      { key: 'apiKey', label: 'apiKey', type: 'password' },
      { key: 'model', label: 'model', type: 'text', placeholder: 'model-id' },
    ],
    extractForm: (p) => {
      const cfg = p.settingsConfig as Record<string, unknown>;
      const options = (cfg.options as Record<string, unknown> | undefined) ?? {};
      const models = cfg.models as Record<string, unknown> | undefined;
      return {
        baseUrl: String(options.baseURL ?? ''),
        apiKey: String(options.apiKey ?? ''),
        model: String(Object.keys(models ?? {})[0] ?? ''),
      };
    },
    buildConfig: (id, v) => ({
      npm: '@ai-sdk/openai-compatible',
      name: id,
      options: { baseURL: v.baseUrl, ...(v.apiKey ? { apiKey: v.apiKey } : {}) },
      models: v.model ? { [v.model]: { name: v.model } } : {},
    }),
    cardUrl: (cfg) => String((cfg.options as Record<string, unknown> | undefined)?.baseURL ?? ''),
    cardMeta: (cfg) => String(Object.keys((cfg.models as Record<string, unknown> | undefined) ?? {})[0] ?? ''),
    surface: 'chat',
    ccSwitchUpstream: false,
    showGatewayCard: true,
    launchers,
  };
}

/// opencode: OpenCode/Kilo provider entry `{ npm, options, models }`.
const opencodeCfg = opencode_projection_config('opencode', client_label('opencode'), 'none');
/// pi: flat `{ providerId, baseUrl, apiKey, model }`
const piCfg: AppPageConfig = {
  appType: 'pi',
  label: client_label('pi'),
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

/// kilo_cli: OpenCode-compatible JSONC projection mirrored from the Node
/// `KiloAdapter` (src/agents/adapters.ts:48-60, 250-297). Same field shape
/// as opencodeCfg; AAG writes the merged payload into the first existing of
/// `~/.config/kilo/{kilo,opencode}.json(c)` or `~/.kilo/config.json`.
const kiloCfg = opencode_projection_config('kilo_cli', client_label('kilo_cli'), 'managed_cli');

const cline_cfg: AppPageConfig = {
  appType: 'cline_cli',
  label: client_label('cline_cli'),
  fields: [
    { key: 'baseUrl', label: 'baseUrl', type: 'text', placeholder: 'http://127.0.0.1:8989/v1' },
    { key: 'apiKey', label: 'apiKey', type: 'password' },
    { key: 'model', label: 'model', type: 'text', placeholder: 'model-id' },
  ],
  extractForm: (p) => {
    const cfg = p.settingsConfig as Record<string, unknown>;
    return { baseUrl: String(cfg.baseUrl ?? ''), apiKey: String(cfg.apiKey ?? ''), model: String(cfg.model ?? '') };
  },
  buildConfig: (_id, v) => ({
    protocol: 'openai',
    baseUrl: v.baseUrl,
    ...(v.apiKey ? { apiKey: v.apiKey } : {}),
    model: v.model,
  }),
  cardUrl: (cfg) => String(cfg.baseUrl ?? ''),
  cardMeta: (cfg) => String(cfg.model ?? ''),
  surface: 'chat',
  ccSwitchUpstream: false,
  showGatewayCard: true,
  launchers: 'managed_cli',
};

/// claude_vscode: Claude Code VS Code extension. AAG is the unified
/// upstream — no live write (cc-switch owns ~/.claude/settings.json);
/// the Open VS Code launcher injects ANTHROPIC_BASE_URL. Provider
/// rows carry the same env shape as claude_code.
const claudeVscodeCfg: AppPageConfig = {
  appType: 'claude_vscode',
  label: client_label('claude_vscode'),
  fields: [
    { key: 'baseUrl', label: 'baseUrl', type: 'text', placeholder: 'http://127.0.0.1:8989' },
    { key: 'apiKey', label: 'apiKey', type: 'password' },
    { key: 'model', label: 'model', type: 'text', placeholder: 'claude-sonnet-5' },
  ],
  extractForm: (p) => {
    const env = envOf(p.settingsConfig as Record<string, unknown>);
    return {
      baseUrl: String(env.ANTHROPIC_BASE_URL ?? ''),
      apiKey: String(env.ANTHROPIC_AUTH_TOKEN ?? ''),
      model: String((p.settingsConfig as Record<string, unknown>).model ?? ''),
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
  ccSwitchUpstream: false,
  showGatewayCard: true,
  launchers: 'vscode',
};

/// kilo_vscode: Kilo Code VS Code extension. The extension's
/// `globalFiles()` reads the same OpenCode JSONC projection as the Kilo
/// CLI (~/.config/kilo/kilo.jsonc …), so switching delegates to the
/// kilo adapter; the launcher additionally injects KILO_CONFIG pointing
/// at that file.
const kiloVscodeCfg = opencode_projection_config('kilo_vscode', client_label('kilo_vscode'), 'vscode');

/// gemini: `{ env: { GOOGLE_GEMINI_BASE_URL, GEMINI_API_KEY, GEMINI_MODEL }, api, config }`
/// The provider's own upstream drives the SURFACE_GEMINI failover chain.
/// Switching writes AAG's proxy into ~/.gemini/.env so the CLI uses AAG as
/// its unified upstream (mirror contract, like Codex).
const geminiCfg: AppPageConfig = {
  appType: 'gemini',
  label: client_label('gemini'),
  fields: [
    {
      key: 'baseUrl',
      label: 'baseUrl',
      type: 'text',
      placeholder: 'https://generativelanguage.googleapis.com',
    },
    { key: 'apiKey', label: 'apiKey', type: 'password' },
    {
      key: 'api',
      label: 'upstream API',
      type: 'select',
      options: ['gemini', 'anthropic', 'openai'],
    },
    { key: 'model', label: 'model', type: 'text', placeholder: 'gemini-2.5-pro' },
  ],
  extractForm: (p) => {
    const cfg = p.settingsConfig as Record<string, unknown>;
    const env = envOf(cfg);
    return {
      baseUrl: String(env.GOOGLE_GEMINI_BASE_URL ?? ''),
      apiKey: String(env.GEMINI_API_KEY ?? ''),
      api: String((cfg.api as string) ?? 'gemini'),
      model: String(env.GEMINI_MODEL ?? ''),
    };
  },
  buildConfig: (_id, v) => {
    const env: Record<string, string | undefined> = {
      GOOGLE_GEMINI_BASE_URL: v.baseUrl,
      ...(v.apiKey ? { GEMINI_API_KEY: v.apiKey } : {}),
      ...(v.model ? { GEMINI_MODEL: v.model } : {}),
    };
    return {
      env,
      api: v.api || 'gemini',
      config: {},
    };
  },
  cardUrl: (cfg) => String(envOf(cfg).GOOGLE_GEMINI_BASE_URL ?? ''),
  cardMeta: (cfg) => String(envOf(cfg).GEMINI_MODEL ?? ''),
  surface: 'gemini',
  ccSwitchUpstream: true,
  showGatewayCard: true,
  launchers: 'gemini',
};

export const APP_PAGES: Record<string, AppPageConfig> = {
  claude_code: claudeCfg,
  codex: codexCfg,
  copilot: copilotCfg,
  gemini: geminiCfg,
  opencode: opencodeCfg,
  pi: piCfg,
  kilo_cli: kiloCfg,
  cline_cli: cline_cfg,
  claude_vscode: claudeVscodeCfg,
  kilo_vscode: kiloVscodeCfg,
};

export const APP_PAGE_ORDER: AppType[] = client_page_order;

export { CODEX_MODELS, CLAUDE_MODELS, CODEX_DISPLAY };
