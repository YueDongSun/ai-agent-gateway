import { existsSync, readFileSync, writeFileSync, mkdirSync, copyFileSync, renameSync, rmSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { homedir } from 'node:os';
import type { AgentId, AgentTarget } from '../config/schema.js';
import { is_json_object, parse_jsonc_object } from './jsonc.js';

/* ------------------------------------------------------------------ */
/*  Detection of where each agent keeps its config                     */
/* ------------------------------------------------------------------ */

const HOME = homedir();
const APPDATA = process.env.APPDATA ?? join(HOME, 'AppData', 'Roaming');

export function defaultAgentTargets(): AgentTarget[] {
  return [
    {
      id: 'copilot',
      name: 'GitHub Copilot CLI',
      format: 'openai',
      configPaths: [
        join(HOME, '.copilot', 'settings.json'),
      ],
      pointedAtGateway: false,
      activeProfileId: '',
    },
    {
      id: 'pi',
      name: 'Pi (MS Coding Agent)',
      format: 'openai',
      configPaths: [
        join(HOME, '.pi', 'agent', 'models.json'),
        join(HOME, '.pi', 'agent', 'settings.json'),
      ],
      pointedAtGateway: false,
      activeProfileId: '',
    },
    {
      id: 'antigravity',
      name: 'Antigravity (desktop)',
      format: 'openai',
      configPaths: [
        join(APPDATA, 'Antigravity', 'Preferences'),
        join(HOME, '.config', 'Antigravity', 'Preferences'),
      ],
      pointedAtGateway: false,
      activeProfileId: '',
    },
    {
      id: 'kilo',
      name: 'Kilo Code',
      format: 'openai',
      configPaths: [
        join(HOME, '.config', 'kilo', 'kilo.jsonc'),
        join(HOME, '.config', 'kilo', 'kilo.json'),
        join(HOME, '.config', 'kilo', 'opencode.jsonc'),
        join(HOME, '.config', 'kilo', 'opencode.json'),
        join(HOME, '.kilo', 'config.json'),
      ],
      pointedAtGateway: false,
      activeProfileId: '',
    },
    {
      id: 'cursor',
      name: 'Cursor',
      format: 'openai',
      configPaths: [
        join(APPDATA, 'Cursor', 'User', 'globalStorage', 'cursor-mcp.json'),
        join(HOME, '.cursor', 'config.json'),
      ],
      pointedAtGateway: false,
      activeProfileId: '',
    },
    {
      id: 'zcode',
      name: 'Zcode',
      format: 'openai',
      configPaths: [
        join(HOME, '.zcode', 'config.json'),
        join(APPDATA, 'zcode', 'config.json'),
      ],
      pointedAtGateway: false,
      activeProfileId: '',
    },
    {
      id: 'minimax',
      name: 'MiniMax Code',
      format: 'openai',
      configPaths: [
        join(HOME, '.minimax', 'config.json'),
        join(HOME, '.config', 'minimax', 'config.json'),
      ],
      pointedAtGateway: false,
      activeProfileId: '',
    },
    {
      id: 'kimi',
      name: 'Kimi Code',
      format: 'openai',
      configPaths: [
        join(HOME, '.kimi', 'config.json'),
        join(HOME, '.config', 'kimi', 'config.json'),
      ],
      pointedAtGateway: false,
      activeProfileId: '',
    },
  ];
}

/* ------------------------------------------------------------------ */
/*  Agent adapter interface                                           */
/* ------------------------------------------------------------------ */

/**
 * Atomic config replacement with a rolling backup. The previous file (if
 * any) is copied to `<path>.bak` first, then the new content goes to a
 * sibling staging file and is renamed into place — a crash mid-write can
 * never leave a truncated agent config, and the prior state stays
 * recoverable. A failed rename removes the staging file so the agent's
 * config directory is not left with partial artifacts.
 */
function write_config_with_backup(path: string, content: string): void {
  if (existsSync(path)) copyFileSync(path, `${path}.bak`);
  const staging_path = `${path}.aag-tmp`;
  writeFileSync(staging_path, content, 'utf8');
  try {
    renameSync(staging_path, path);
  } catch (error) {
    rmSync(staging_path, { force: true });
    throw error;
  }
}

export interface SwitchResult {
  ok: boolean;
  agent: AgentId;
  path: string;
  message: string;
}

export interface AgentAdapter {
  readonly target: AgentTarget;
  /** Find the first existing/writable config path. */
  resolveConfigPath(): string | undefined;
  /** Read the current config file as text. */
  read(): string | undefined;
  /** Write the new config (string content). Creates dirs as needed. */
  write(content: string): void;
  /** Detect the current base URL from the config (for display). */
  detectCurrentBaseURL(): string | undefined;
  /** Produce config content pointing at the gateway. */
  buildGatewayConfig(gatewayURL: string, authToken: string, model: string): string;
  /** Produce config content pointing at a direct provider. */
  buildDirectConfig(baseURL: string, apiKey: string, model: string): string;
}

/* ------------------------------------------------------------------ */
/*  Generic JSON adapter (most agents store JSON)                     */
/* ------------------------------------------------------------------ */

export abstract class JsonAgentAdapter implements AgentAdapter {
  readonly target: AgentTarget;
  constructor(target: AgentTarget) {
    this.target = target;
  }

  resolveConfigPath(): string | undefined {
    for (const p of this.target.configPaths) {
      if (existsSync(p)) return p;
    }
    // No existing file: create it at the first candidate path.
    return this.target.configPaths[0];
  }

  read(): string | undefined {
    const p = this.target.configPaths.find((x) => existsSync(x));
    if (!p) return undefined;
    return readFileSync(p, 'utf8');
  }

  write(content: string): void {
    const p = this.resolveConfigPath();
    if (!p) throw new Error(`no config path for agent ${this.target.id}`);
    const dir = dirname(p);
    if (!existsSync(dir)) mkdirSync(dir, { recursive: true });
    write_config_with_backup(p, content);
  }

  detectCurrentBaseURL(): string | undefined {
    try {
      const raw = this.read();
      if (!raw) return undefined;
      const obj = JSON.parse(raw);
      return obj.baseURL ?? obj.base_url ?? obj.endpoint ?? obj.apiBase ?? undefined;
    } catch {
      return undefined;
    }
  }

  /** Subclasses decide the exact JSON shape. */
  abstract buildJson(gatewayURL: string, authToken: string, model: string): Record<string, unknown>;
  abstract buildDirectJson(baseURL: string, apiKey: string, model: string): Record<string, unknown>;

  buildGatewayConfig(gatewayURL: string, authToken: string, model: string): string {
    return JSON.stringify(this.buildJson(gatewayURL, authToken, model), null, 2) + '\n';
  }

  buildDirectConfig(baseURL: string, apiKey: string, model: string): string {
    return JSON.stringify(this.buildDirectJson(baseURL, apiKey, model), null, 2) + '\n';
  }
}

/* ------------------------------------------------------------------ */
/*  Per-agent implementations                                         */
/* ------------------------------------------------------------------ */

export class CopilotAdapter extends JsonAgentAdapter {
  buildJson(gw: string, tok: string, model: string) {
    return {
      model,
      env: {
        COPILOT_PROVIDER_BASE_URL: `${gw}/v1`,
        COPILOT_PROVIDER_API_KEY: tok || 'gateway',
        COPILOT_PROVIDER_TYPE: 'openai',
      },
    };
  }
  buildDirectJson(baseURL: string, apiKey: string, model: string) {
    return {
      model,
      env: {
        COPILOT_PROVIDER_BASE_URL: baseURL,
        COPILOT_PROVIDER_API_KEY: apiKey,
        COPILOT_PROVIDER_TYPE: 'openai',
      },
    };
  }
  /** The public read contract is parseable JSON, preserving JSONC values. */
  read(): string | undefined {
    const p = this.target.configPaths.find((x) => existsSync(x));
    if (!p) return undefined;
    const raw = readFileSync(p, 'utf8');
    return JSON.stringify(parse_jsonc_object(raw, 'Copilot config'));
  }
  /** Merge the new config into the existing settings so user keys survive. */
  write(content: string): void {
    const p = this.resolveConfigPath();
    if (!p) throw new Error(`no config path for agent ${this.target.id}`);
    const existing = existsSync(p) ? parse_jsonc_object(readFileSync(p, 'utf8'), 'Copilot config') : {};
    const patch = parse_jsonc_object(content, 'Copilot config patch');
    if ((existing.env !== undefined && !is_json_object(existing.env)) || (patch.env !== undefined && !is_json_object(patch.env))) {
      throw new Error('Copilot config env must be an object; original file was not changed');
    }
    const merged = { ...existing, ...patch };
    if (patch.env !== undefined) merged.env = { ...(existing.env as Record<string, unknown> | undefined), ...patch.env as Record<string, unknown> };
    const dir = dirname(p);
    if (!existsSync(dir)) mkdirSync(dir, { recursive: true });
    write_config_with_backup(p, JSON.stringify(merged, null, 2) + '\n');
  }
}

export class KiloAdapter extends JsonAgentAdapter {
  buildJson(gw: string, tok: string, model: string) {
    return { model: `aag/${model}`, provider: { aag: { npm: '@ai-sdk/openai-compatible', options: { baseURL: `${gw}/v1`, apiKey: tok || 'gateway' }, models: { [model]: { name: model } } } } };
  }
  buildDirectJson(baseURL: string, apiKey: string, model: string) {
    return { model: `aag/${model}`, provider: { aag: { npm: '@ai-sdk/openai-compatible', options: { baseURL, apiKey }, models: { [model]: { name: model } } } } };
  }
  read(): string | undefined {
    const raw = super.read();
    if (!raw) return undefined;
    return JSON.stringify(parse_jsonc_object(raw, 'Kilo config'));
  }

  write(content: string): void {
    const p = this.resolveConfigPath();
    if (!p) throw new Error('no config path for agent ' + this.target.id);
    const dir = dirname(p);
    if (!existsSync(dir)) mkdirSync(dir, { recursive: true });
    let merged: Record<string, unknown> = {};
    const existing = this.read();
    if (existing) {
      const parsed: unknown = JSON.parse(existing);
      if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) throw new Error('Kilo config must be a JSON object');
      merged = parsed as Record<string, unknown>;
      if (merged.provider !== undefined && (!merged.provider || typeof merged.provider !== 'object' || Array.isArray(merged.provider))) throw new Error('Kilo config provider must be an object');
    }
    const patch = JSON.parse(content) as Record<string, unknown>;
    if (!patch.provider || typeof patch.provider !== 'object' || Array.isArray(patch.provider)) throw new Error('Kilo provider patch must contain an object provider');
    const providers = (merged.provider as Record<string, unknown> | undefined) ?? {};
    merged.provider = { ...providers, ...(patch.provider as Record<string, unknown>) };
    if (typeof patch.model === 'string' && patch.model) merged.model = patch.model;
    write_config_with_backup(p, JSON.stringify(merged, null, 2) + '\n');
  }

  detectCurrentBaseURL(): string | undefined {
    try {
      const raw = this.read();
      if (!raw) return undefined;
      const cfg = JSON.parse(raw) as Record<string, unknown>;
      const providers = cfg.provider as Record<string, unknown> | undefined;
      const aag = providers?.aag as Record<string, unknown> | undefined;
      const options = aag?.options as Record<string, unknown> | undefined;
      return String(options?.baseURL ?? aag?.baseURL ?? '') || undefined;
    } catch {
      return undefined;
    }
  }
}

export class CursorAdapter extends JsonAgentAdapter {
  buildJson(gw: string, tok: string, model: string) {
    return {
      openai: {
        baseURL: `${gw}/v1`,
        apiKey: tok || 'gateway',
        defaultModel: model,
      },
    };
  }
  buildDirectJson(baseURL: string, apiKey: string, model: string) {
    return { openai: { baseURL, apiKey, defaultModel: model } };
  }
}

export class ZcodeAdapter extends JsonAgentAdapter {
  buildJson(gw: string, tok: string, model: string) {
    return {
      provider: 'openai',
      endpoint: `${gw}/v1`,
      apiKey: tok || 'gateway',
      model,
    };
  }
  buildDirectJson(baseURL: string, apiKey: string, model: string) {
    return { provider: 'openai', endpoint: baseURL, apiKey, model };
  }
}

export class MinimaxAdapter extends JsonAgentAdapter {
  buildJson(gw: string, tok: string, model: string) {
    return {
      llm: {
        provider: 'openai',
        apiBase: `${gw}/v1`,
        apiKey: tok || 'gateway',
        model,
      },
    };
  }
  buildDirectJson(baseURL: string, apiKey: string, model: string) {
    return { llm: { provider: 'openai', apiBase: baseURL, apiKey, model } };
  }
}

export class KimiAdapter extends JsonAgentAdapter {
  buildJson(gw: string, tok: string, model: string) {
    return {
      baseURL: `${gw}/v1`,
      apiKey: tok || 'gateway',
      model,
      provider: 'openai',
    };
  }
  buildDirectJson(baseURL: string, apiKey: string, model: string) {
    return { baseURL, apiKey, model, provider: 'openai' };
  }
}

export class PiAdapter extends JsonAgentAdapter {
  buildJson(gw: string, tok: string, model: string) {
    return {
      providers: {
        aag: {
          baseUrl: `${gw}/v1`,
          api: 'openai-completions',
          apiKey: tok || 'gateway',
          models: [{ id: model }],
        },
      },
    };
  }
  buildDirectJson(baseURL: string, apiKey: string, model: string) {
    return {
      providers: {
        aag: {
          baseUrl: baseURL,
          api: 'openai-completions',
          apiKey,
          models: [{ id: model }],
        },
      },
    };
  }
  detectCurrentBaseURL(): string | undefined {
    const raw = this.read();
    if (!raw) return undefined;
    try {
      const obj = JSON.parse(raw);
      return obj.providers?.aag?.baseUrl;
    } catch {
      return undefined;
    }
  }
}

export class AntigravityAdapter extends JsonAgentAdapter {
  /** Antigravity switching is disabled — only detection is supported. */
  buildJson(_gw: string, _tok: string, _model: string): Record<string, unknown> {
    throw new Error(
      'Antigravity desktop switching is not supported yet (no public on-disk config schema). Configure the model in Antigravity itself.',
    );
  }
  buildDirectJson(_baseURL: string, _apiKey: string, _model: string): Record<string, unknown> {
    throw new Error(
      'Antigravity desktop switching is not supported yet (no public on-disk config schema). Configure the model in Antigravity itself.',
    );
  }
}

/* ------------------------------------------------------------------ */
/*  Registry                                                          */
/* ------------------------------------------------------------------ */

const ADAPTERS = {
  copilot: CopilotAdapter,
  kilo: KiloAdapter,
  cursor: CursorAdapter,
  zcode: ZcodeAdapter,
  minimax: MinimaxAdapter,
  kimi: KimiAdapter,
  pi: PiAdapter,
  antigravity: AntigravityAdapter,
} as const;

export function createAdapter(target: AgentTarget): AgentAdapter {
  const Cls = ADAPTERS[target.id];
  if (!Cls) throw new Error(`no adapter for agent "${target.id}"`);
  return new Cls(target);
}

export function detectInstalledAgents(targets: AgentTarget[]): AgentTarget[] {
  return targets.filter((t) => {
    createAdapter(t); // validates the target maps to a known adapter
    return t.configPaths.some((x) => existsSync(x));
  });
}
