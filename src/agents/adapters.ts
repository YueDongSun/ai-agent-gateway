import { existsSync, readFileSync, writeFileSync, mkdirSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { homedir } from 'node:os';
import type { AgentId, AgentTarget } from '../config/schema.js';

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
        join(HOME, '.kilo', 'config.json'),
        join(HOME, '.config', 'kilo', 'config.json'),
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

export interface SwitchResult {
  ok: boolean;
  agent: AgentId;
  path: string;
  message: string;
  previous?: string;
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
      // prefer existing, else pick first writable parent candidate
      if (existsSync(p)) return p;
    }
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
    writeFileSync(p, content, 'utf8');
  }

  detectCurrentBaseURL(): string | undefined {
    const raw = this.read();
    if (!raw) return undefined;
    try {
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
  /** Copilot settings.json is JSONC — strip comments before JSON.parse. */
  read(): string | undefined {
    const p = this.target.configPaths.find((x) => existsSync(x));
    if (!p) return undefined;
    const raw = readFileSync(p, 'utf8');
    return raw.replace(/^\s*\/\/.*$/gm, '').replace(/\/\*[\s\S]*?\*\//g, '');
  }
  /** Merge the new config into the existing settings so user keys survive. */
  write(content: string): void {
    const p = this.resolveConfigPath();
    if (!p) throw new Error(`no config path for agent ${this.target.id}`);
    const dir = dirname(p);
    if (!existsSync(dir)) mkdirSync(dir, { recursive: true });

    let merged: Record<string, unknown> = {};
    if (existsSync(p)) {
      try {
        const existing = JSON.parse(this.read() ?? '{}');
        if (existing && typeof existing === 'object') merged = existing;
      } catch {
        merged = {};
      }
    }
    const patch = JSON.parse(content) as Record<string, unknown>;
    merged = { ...merged, ...patch };
    // merge env sub-object instead of replacing wholesale
    if (patch.env && typeof patch.env === 'object' && merged.env && typeof merged.env === 'object') {
      merged.env = { ...(merged.env as Record<string, unknown>), ...(patch.env as Record<string, unknown>) };
    }
    writeFileSync(p, JSON.stringify(merged, null, 2) + '\n', 'utf8');
  }
}

export class KiloAdapter extends JsonAgentAdapter {
  buildJson(gw: string, tok: string, model: string) {
    return {
      apiProvider: 'openai',
      baseURL: `${gw}/v1`,
      apiKey: tok || 'gateway',
      defaultModel: model,
      models: [model],
    };
  }
  buildDirectJson(baseURL: string, apiKey: string, model: string) {
    return { apiProvider: 'openai', baseURL, apiKey, defaultModel: model, models: [model] };
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
    const adapter = createAdapter(t);
    const p = t.configPaths.find((x) => existsSync(x));
    return !!p;
  });
}
