/// Smart-provider form schema per app type.
///
/// Each schema describes the fields the user fills in (apiKey, baseUrl,
/// model, etc.) plus a `build` function that converts the field values
/// into the `settingsConfig` JSON the backend writes to disk. Users
/// always have a "Show JSON" toggle to edit raw JSON for advanced cases.
///
/// The fields are deliberately opinionated (no `endpoint` FreeText for
/// Claude Code — the schema knows the keys are ANTHROPIC_BASE_URL and
/// ANTHROPIC_AUTH_TOKEN) so the form auto-generates a working config.

/// The protocol each appType speaks natively. Used to auto-pick the
/// vendor protocol when a provider is created from a vendor: claude_code
/// wants Anthropic Messages; codex/copilot/pi speak OpenAI shapes.
/// Keep in sync with `lib/api/vendors.ts` PROTOCOLS.
export const APP_PROTOCOL: Record<string, string> = {
  claude_code: 'anthropic',
  codex: 'openai-chat',
  copilot: 'openai-chat',
  pi: 'openai-chat',
};

export type FieldType = 'text' | 'password' | 'number' | 'select';

export interface FieldDef {
  key: string;
  label: string;
  type: FieldType;
  placeholder?: string;
  options?: { value: string; label: string }[];
  /** If set, marks the field as required for save. */
  required?: boolean;
  /** If true, this value is treated as secret (e.g. api key) — never echo
   *  back into shared examples. */
  secret?: boolean;
}

export interface AppSchema {
  /** Human label for the provider type (e.g. "Anthropic"). */
  label: string;
  /** Field order in the form. */
  fields: FieldDef[];
  /** Convert the form values to the settingsConfig JSON the backend
   *  writes. Receives { fieldKey: value, ... }; returns the config object. */
  build: (values: Record<string, string | number | null>) => Record<string, unknown>;
  /** Field values the backend stores that we want to pre-populate when
   *  editing an existing provider (best-effort; missing keys just leave
   *  the field empty). */
  extract?: (config: Record<string, unknown>) => Record<string, string>;
}

/* ============================================================
   Anthropic (Claude Code)
   ============================================================ */

const claudeCodeSchema: AppSchema = {
  label: 'Anthropic (Claude Code)',
  fields: [
    {
      key: 'baseUrl',
      label: 'API base URL',
      type: 'text',
      placeholder: 'https://api.anthropic.com',
      required: false,
    },
    {
      key: 'apiKey',
      label: 'API key',
      type: 'password',
      placeholder: 'sk-ant-...',
      secret: true,
    },
    {
      key: 'model',
      label: 'Default model',
      type: 'select',
      options: [
        { value: 'claude-opus-4-8', label: 'Claude Opus 4.8' },
        { value: 'claude-sonnet-5', label: 'Claude Sonnet 5' },
        { value: 'claude-fable-5', label: 'Claude Fable 5' },
        { value: 'claude-haiku-4-5', label: 'Claude Haiku 4.5' },
        { value: 'claude-sonnet-4-6', label: 'Claude Sonnet 4.6' },
      ],
    },
  ],
  build: (values) => {
    const env: Record<string, string> = {};
    if (values.baseUrl) env.ANTHROPIC_BASE_URL = String(values.baseUrl);
    if (values.apiKey) env.ANTHROPIC_AUTH_TOKEN = String(values.apiKey);
    const cfg: Record<string, unknown> = {
      env,
      alwaysThinkingEnabled: false,
    };
    if (values.model) cfg.model = String(values.model);
    return cfg;
  },
  extract: (config) => {
    const env = (config?.env as Record<string, string>) ?? {};
    return {
      baseUrl: env.ANTHROPIC_BASE_URL ?? '',
      apiKey: env.ANTHROPIC_AUTH_TOKEN ?? '',
      model: (config?.model as string) ?? '',
    };
  },
};

/* ============================================================
   OpenAI-compatible (Codex uses an openai_compat block)
   ============================================================ */

const openaiCompatSchema: AppSchema = {
  label: 'OpenAI-compatible (Codex)',
  fields: [
    {
      key: 'baseUrl',
      label: 'API base URL',
      type: 'text',
      placeholder: 'https://api.openai.com/v1',
      required: false,
    },
    {
      key: 'apiKey',
      label: 'API key',
      type: 'password',
      placeholder: 'sk-...',
      secret: true,
    },
    {
      key: 'model',
      label: 'Default model',
      type: 'select',
      options: [
        { value: 'gpt-5', label: 'GPT-5' },
        { value: 'gpt-4o', label: 'GPT-4o' },
        { value: 'gpt-4o-mini', label: 'GPT-4o mini' },
        { value: 'o3', label: 'o3' },
        { value: 'o4-mini', label: 'o4-mini' },
      ],
    },
  ],
  build: (values) => {
    const cfg: Record<string, unknown> = {};
    if (values.baseUrl) cfg.baseUrl = String(values.baseUrl);
    if (values.apiKey) cfg.apiKey = String(values.apiKey);
    if (values.model) cfg.model = String(values.model);
    return cfg;
  },
  extract: (config) => {
    return {
      baseUrl: (config?.baseUrl as string) ?? '',
      apiKey: (config?.apiKey as string) ?? '',
      model: (config?.model as string) ?? '',
    };
  },
};

/* ============================================================
   Copilot CLI (uses COPILOT_PROVIDER_* env vars + model)
   ============================================================ */

const copilotSchema: AppSchema = {
  label: 'Copilot CLI',
  fields: [
    {
      key: 'baseUrl',
      label: 'Provider base URL',
      type: 'text',
      placeholder: 'http://127.0.0.1:8787/v1',
      required: true,
    },
    {
      key: 'apiKey',
      label: 'API key',
      type: 'password',
      placeholder: 'sk-...',
      secret: true,
    },
    {
      key: 'providerType',
      label: 'Provider type',
      type: 'select',
      options: [
        { value: 'openai', label: 'openai' },
        { value: 'anthropic', label: 'anthropic' },
      ],
    },
    {
      key: 'model',
      label: 'Default model',
      type: 'select',
      options: [
        { value: 'claude-fable-5', label: 'Claude Fable 5' },
        { value: 'claude-opus-4-8', label: 'Claude Opus 4.8' },
        { value: 'claude-sonnet-5', label: 'Claude Sonnet 5' },
        { value: 'gpt-5', label: 'GPT-5' },
        { value: 'gpt-4o', label: 'GPT-4o' },
      ],
    },
  ],
  build: (values) => {
    const env: Record<string, string> = {
      COPILOT_PROVIDER_BASE_URL: String(values.baseUrl ?? ''),
      COPILOT_PROVIDER_TYPE: String(values.providerType ?? 'openai'),
    };
    if (values.apiKey) env.COPILOT_PROVIDER_API_KEY = String(values.apiKey);
    return {
      model: values.model ?? 'gpt-5',
      env,
    };
  },
  extract: (config) => {
    const env = (config?.env as Record<string, string>) ?? {};
    return {
      baseUrl: env.COPILOT_PROVIDER_BASE_URL ?? '',
      apiKey: env.COPILOT_PROVIDER_API_KEY ?? '',
      providerType: env.COPILOT_PROVIDER_TYPE ?? 'openai',
      model: (config?.model as string) ?? '',
    };
  },
};

/* ============================================================
   Pi (MS Coding Agent)
   ============================================================ */

const piSchema: AppSchema = {
  label: 'Pi',
  fields: [
    {
      key: 'providerId',
      label: 'Provider id',
      type: 'text',
      placeholder: 'my-pi-provider',
      required: true,
    },
    {
      key: 'baseUrl',
      label: 'Provider base URL',
      type: 'text',
      placeholder: 'http://127.0.0.1:8787/v1',
    },
    {
      key: 'apiKey',
      label: 'API key',
      type: 'password',
      placeholder: 'sk-...',
      secret: true,
    },
    {
      key: 'model',
      label: 'Default model',
      type: 'text',
      placeholder: 'claude-fable-5',
    },
    {
      key: 'models',
      label: 'Available models (comma-separated)',
      type: 'text',
      placeholder: 'claude-fable-5, claude-opus-4-8, gpt-5',
    },
  ],
  build: (values) => {
    const cfg: Record<string, unknown> = {
      providerId: String(values.providerId ?? ''),
    };
    if (values.baseUrl) cfg.baseUrl = String(values.baseUrl);
    if (values.apiKey) cfg.apiKey = String(values.apiKey);
    if (values.model) cfg.model = String(values.model);
    const modelsStr = String(values.models ?? '').trim();
    if (modelsStr) {
      cfg.models = modelsStr
        .split(',')
        .map((s) => s.trim())
        .filter(Boolean);
    }
    return cfg;
  },
  extract: (config) => {
    const models = (config?.models as string[] | undefined) ?? [];
    return {
      providerId: (config?.providerId as string) ?? '',
      baseUrl: (config?.baseUrl as string) ?? '',
      apiKey: (config?.apiKey as string) ?? '',
      model: (config?.model as string) ?? '',
      models: models.join(', '),
    };
  },
};

/* ============================================================
   Antigravity — detect-only; switching not supported yet
   ============================================================ */

const antigravitySchema: AppSchema = {
  label: 'Antigravity',
  fields: [],
  build: () => ({}),
};

/* ============================================================
   Schema registry
   ============================================================ */

export const SCHEMAS: Record<string, AppSchema> = {
  claude_code: claudeCodeSchema,
  codex: openaiCompatSchema,
  copilot: copilotSchema,
  pi: piSchema,
  antigravity: antigravitySchema,
};

/** Render a `settingsConfig` object to a stable JSON string for the
 *  form's JSON editor view. */
export function stringifyConfig(config: Record<string, unknown>): string {
  return JSON.stringify(config, null, 2);
}