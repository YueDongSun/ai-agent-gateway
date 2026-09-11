/// Browser-only demo backend.
///
/// The Tauri desktop app wires the frontend to the Rust backend through
/// `window.__TAURI_INTERNALS__.invoke`. In a plain browser (e.g. `cd ui &&
/// npm run dev`) that bridge does not exist, so this module installs a mock
/// IPC handler with the official `mockIPC` helper and serves realistic seed
/// data for every command the UI invokes. It lets you exercise the whole
/// React UI — Providers, Proxy failover CRUD, Backup, MCP, Skills, Sessions,
/// Usage, Sync, Deep links, Codex CLI — without launching the Rust app.
///
/// Activation is automatic and exclusive:
///   - running in a Tauri webview  -> this module is a no-op (real IPC wins)
///   - running under `vite dev`    -> demo mode installs the mock handler
///   - production build in a plain browser -> no-op (not a supported target)
///
/// Debug hooks (open the browser console):
///   - `window.__aagDemoSetError('provider_list')`  — make a command reject
///   - `window.__aagDemoSetError(null)`             — clear the forced error
///   - `window.__aagDemoSeedDeeplink('aag://switch/claude-1?app=claude_code')`
///     — fire a deep-link event so DeepLinkPage renders + parses a URL
import type { InvokeArgs } from '@tauri-apps/api/core';
import { mockIPC } from '@tauri-apps/api/mocks';
import type { AppType, Provider } from '../api/types';
import type { BackupEntry } from '../api/backup';
import type { McpServer } from '../api/mcp';
import type { InstalledSkill } from '../api/skills';
import type { SessionMeta } from '../api/sessions';
import type {
  UsageRecordInput,
  UsageSummary,
  ModelUsage,
  ProviderUsage,
} from '../api/usage';
import type { SyncConfig } from '../api/sync';
import type { ProxyProvider, Route } from '../api/proxy';
import type { DeeplinkAction } from '../api/deeplink';

// Official Claude model ids — mirrors src-tauri/src/proxy/models.rs
// OFFICIAL_MODELS. Kept in sync so the browser demo advertises the same
// model list the Rust backend serves.
const OFFICIAL_CLAUDE_MODELS = [
  'claude-fable-5',
  'claude-opus-4-8',
  'claude-opus-4-7',
  'claude-opus-4-6',
  'claude-sonnet-5',
  'claude-sonnet-4-6',
  'claude-haiku-4-5',
];

interface TauriInternals {
  runCallback: (id: number, data: unknown) => void;
}

function internals(): TauriInternals | undefined {
  return (window as unknown as { __TAURI_INTERNALS__?: TauriInternals })
    .__TAURI_INTERNALS__;
}

export const isTauri = typeof window !== 'undefined' && Boolean(internals());
export const isDemo =
  typeof window !== 'undefined' &&
  !isTauri &&
  Boolean((import.meta as { env?: Record<string, unknown> }).env?.DEV);

// ---------------------------------------------------------------------------
// In-memory store (module-level, survives HMR only while the page is open)
// ---------------------------------------------------------------------------

const providers = new Map<string, Provider[]>();
const currentByApp = new Map<string, string>();

function seedProviders() {
  providers.set('claude_code', [
    {
      id: 'claude-1',
      name: 'Claude Sonnet 4',
      settingsConfig: {
        env: {
          ANTHROPIC_BASE_URL: 'https://api.anthropic.com',
          ANTHROPIC_AUTH_TOKEN: 'sk-ant-demo-token',
        },
      },
      notes: 'Official Anthropic API',
      createdAt: Math.floor(Date.now() / 1000) - 86400 * 30,
    },
    {
      id: 'claude-proxy',
      name: 'Via local proxy',
      settingsConfig: {
        env: {
          ANTHROPIC_BASE_URL: 'http://127.0.0.1:8787',
          ANTHROPIC_AUTH_TOKEN: 'demo',
        },
      },
      notes: 'Routes through AAG failover proxy',
      createdAt: Math.floor(Date.now() / 1000) - 86400 * 5,
    },
  ]);
  providers.set('codex', [
    {
      id: 'codex-oai',
      name: 'OpenAI GPT-5',
      settingsConfig: {
        auth: { tokens: { value: 'sk-demo', source: 'demo' } },
        config: 'model = "gpt-5"\ntemperature = 0.2\n',
      },
      createdAt: Math.floor(Date.now() / 1000) - 86400 * 20,
    },
    {
      id: 'codex-anth',
      name: 'Anthropic via proxy',
      settingsConfig: {
        auth: { tokens: { value: 'sk-ant-demo', source: 'demo' } },
        config: 'model = "claude-sonnet-4"\n',
      },
      createdAt: Math.floor(Date.now() / 1000) - 86400 * 10,
    },
  ]);
  providers.set('copilot', [
    {
      id: 'copilot-claude',
      name: 'Copilot + Claude Sonnet 4',
      settingsConfig: {
        model: 'claude-sonnet-4',
        env: {
          COPILOT_PROVIDER_BASE_URL: 'http://127.0.0.1:8787/v1',
          COPILOT_PROVIDER_API_KEY: 'demo',
          COPILOT_PROVIDER_TYPE: 'openai',
        },
      },
      createdAt: Math.floor(Date.now() / 1000) - 86400 * 3,
    },
  ]);
  providers.set('pi', [
    {
      id: 'pi-claude',
      name: 'Pi + Claude via proxy',
      settingsConfig: {
        providerId: 'pi-claude',
        baseUrl: 'http://127.0.0.1:8787/v1',
        apiKey: 'demo',
        model: 'claude-sonnet-4',
        models: ['claude-sonnet-4', 'gpt-5'],
      },
      createdAt: Math.floor(Date.now() / 1000) - 86400 * 2,
    },
  ]);
  providers.set('antigravity', [
    {
      id: 'ag-default',
      name: 'Antigravity default',
      settingsConfig: {},
      notes: 'Switching disabled in this version (no public config schema).',
      createdAt: Math.floor(Date.now() / 1000) - 86400 * 1,
    },
  ]);
  currentByApp.set('claude_code', 'claude-1');
  currentByApp.set('codex', 'codex-oai');
  currentByApp.set('copilot', 'copilot-claude');
}

seedProviders();

// ----- Proxy failover -----

let proxyRunning = false;

const proxyProviders: ProxyProvider[] = [
  {
    id: 'mock-primary',
    name: 'Mock primary',
    type: 'mock',
    apiKey: '',
    baseURL: '',
    models: ['claude-sonnet-4', 'gpt-5', ...OFFICIAL_CLAUDE_MODELS],
    enabled: true,
    timeoutMs: 60000,
    weight: 1,
    circuitBreaker: { failureThreshold: 3, cooldownMs: 30000, halfOpenRetries: 1, successThreshold: 2 },
  },
  {
    id: 'mock-fallback',
    name: 'Mock fallback',
    type: 'mock',
    apiKey: '',
    baseURL: '',
    models: ['claude-sonnet-4', 'gpt-5', ...OFFICIAL_CLAUDE_MODELS],
    enabled: true,
    timeoutMs: 60000,
    weight: 1,
    circuitBreaker: { failureThreshold: 5, cooldownMs: 30000, halfOpenRetries: 1, successThreshold: 2 },
  },
];

const routes: Route[] = [
  {
    alias: 'claude-sonnet-4',
    description: 'Claude alias with failover',
    hops: [
      { provider: 'mock-primary', model: 'claude-sonnet-4' },
      { provider: 'mock-fallback', model: 'claude-sonnet-4' },
    ],
  },
  { alias: 'gpt-5', description: 'GPT-5 alias', hops: [{ provider: 'mock-primary', model: 'gpt-5' }] },
  { alias: 'mock-fail', description: 'Always fails (tests failover)', hops: [{ provider: 'mock-fallback', model: 'claude-sonnet-4' }] },
  // Official Claude model routes — mirrors the Rust seed
  // (src-tauri/src/proxy/models.rs OFFICIAL_MODELS + database/seed.rs): each
  // official id is a failover alias so the proxy advertises and serves the
  // same model list as api.anthropic.com.
  ...OFFICIAL_CLAUDE_MODELS.map((id) => ({
    alias: id,
    description: 'Official Claude model via failover chain',
    hops: [
      { provider: 'mock-primary', model: id },
      { provider: 'mock-fallback', model: id },
    ],
  })),
];

// ----- Backup -----

const backups: BackupEntry[] = [
  {
    id: 'bak-demo-20260806',
    label: 'Demo seed backup',
    sizeBytes: 182_430,
    createdAt: new Date(Date.now() - 86400_000 * 2).toISOString(),
  },
];

// ----- MCP / Skills / Sessions -----

const mcpServers: McpServer[] = [
  {
    id: 'filesystem',
    name: 'Filesystem',
    serverConfig: { command: 'npx', args: ['-y', '@modelcontextprotocol/server-filesystem', '.'] },
    description: 'Exposes the current directory to the agent',
    tags: ['fs'],
    enabledClaude: true,
    enabledCodex: true,
  },
  {
    id: 'git',
    name: 'Git',
    serverConfig: { command: 'npx', args: ['-y', '@modelcontextprotocol/server-git'] },
    description: 'Git operations (status/diff/commit)',
    tags: ['git'],
    enabledClaude: true,
    enabledCodex: false,
  },
];

const skills: InstalledSkill[] = [
  {
    id: 'code-review',
    name: 'Code review',
    description: 'Review a diff across correctness/security/perf',
    directory: '~/.claude/skills/code-review',
    repoOwner: 'demo',
    repoName: 'skills',
    repoBranch: 'main',
    contentHash: 'abc123',
    enabledClaude: true,
    enabledCodex: false,
    installedAt: Math.floor(Date.now() / 1000) - 86400 * 4,
    updatedAt: Math.floor(Date.now() / 1000) - 86400,
  },
];

const now = Math.floor(Date.now() / 1000);
const sessions: SessionMeta[] = [
  {
    id: 'claude_code:ses-01',
    appId: 'claude_code',
    sessionId: 'ses-01',
    title: 'Refactor proxy router',
    summary: 'Split failover loop into its own module',
    projectDir: 'E:/LAB_CODE/cc_copilot_kilo',
    sourcePath: 'E:/LAB_CODE/cc_copilot_kilo/src-tauri/src/proxy/router.rs',
    resumeCommand: 'claude --resume ses-01',
    messageCount: 24,
    startedAt: now - 86400 * 3,
    lastActiveAt: now - 3600,
  },
  {
    id: 'codex:ses-02',
    appId: 'codex',
    sessionId: 'ses-02',
    title: 'SigV4 signing tests',
    summary: 'Verify canonical request shape against AWS reference',
    projectDir: 'E:/LAB_CODE/cc_copilot_kilo',
    sourcePath: 'E:/LAB_CODE/cc_copilot_kilo/src-tauri/src/services/sync/s3.rs',
    resumeCommand: 'codex --resume ses-02',
    messageCount: 9,
    startedAt: now - 86400,
    lastActiveAt: now - 1800,
  },
];

// ----- Usage -----

const usageSeeds: UsageRecordInput[] = [
  { ts: now - 3600 * 2, appId: 'claude_code', providerId: 'claude-1', model: 'claude-sonnet-4', promptTokens: 1200, completionTokens: 800, costUsd: 0.042, durationMs: 3400 },
  { ts: now - 3600 * 5, appId: 'claude_code', providerId: 'claude-1', model: 'claude-sonnet-4', promptTokens: 300, completionTokens: 150, costUsd: 0.009, durationMs: 1200 },
  { ts: now - 86400 * 2, appId: 'codex', providerId: 'codex-oai', model: 'gpt-5', promptTokens: 4000, completionTokens: 1200, costUsd: 0.18, durationMs: 8900 },
  { ts: now - 86400 * 6, appId: 'codex', providerId: 'codex-oai', model: 'gpt-5', promptTokens: 2000, completionTokens: 600, costUsd: 0.09, durationMs: 4200 },
  { ts: now - 86400 * 20, appId: 'claude_code', providerId: 'claude-1', model: 'claude-opus-4', promptTokens: 8000, completionTokens: 2500, costUsd: 0.6, durationMs: 15000 },
];
const usageRecords: UsageRecordInput[] = [...usageSeeds];

// ----- Sync -----

const syncConfigs: SyncConfig[] = [
  {
    provider: 's3',
    enabled: false,
    endpoint: 'https://s3.us-east-1.amazonaws.com',
    bucket: 'my-aag-backups',
    region: 'us-east-1',
    accessKey: 'AKIDDEMO',
    secretKey: '',
    lastSyncAt: null,
    lastError: null,
  },
  {
    provider: 'webdav',
    enabled: true,
    endpoint: 'https://dav.example.com',
    remotePath: 'backups',
    accessKey: 'demo-user',
    secretKey: 'demo-pass',
    lastSyncAt: now - 7200,
    lastError: null,
  },
];

// ----- Settings (app_meta) -----

const settings = new Map<string, string>([
  ['codex_cli_home', 'C:\\Users\\dev\\.codex-cli'],
  ['outbound_proxy', 'http://127.0.0.1:8787'],
]);

// ---------------------------------------------------------------------------
// Debug hooks
// ---------------------------------------------------------------------------

let forcedError: string | null = null;

export function setMockError(cmd: string | null) {
  forcedError = cmd;
}

const deeplinkListeners = new Map<string, number>();

export function seedDeeplink(url: string) {
  const intr = internals();
  if (!intr) return;
  for (const handlerId of deeplinkListeners.values()) {
    intr.runCallback(handlerId, {
      event: 'aag://deeplink',
      id: 'mock-' + Math.random().toString(36).slice(2),
      payload: { urls: [url] },
    });
  }
}

// ---------------------------------------------------------------------------
// Command handlers
// ---------------------------------------------------------------------------

function delay(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 120));
}

function liveWrite(appType: string, providerId: string): string[] {
  const base =
    appType === 'codex'
      ? '~/.codex-cli/config.toml'
      : appType === 'copilot'
        ? '~/.copilot/settings.json'
        : appType === 'pi'
          ? '~/.pi/agent/models.json'
          : '~/.claude/settings.json';
  return [base, `${base}.bak`];
}

function parseDeeplink(url: string): DeeplinkAction {
  try {
    const parsed = new URL(url);
    if (parsed.protocol !== 'aag:') {
      return { kind: 'unknown', raw: url, reason: `wrong scheme: ${parsed.protocol}` };
    }
    const action = parsed.hostname;
    const id = parsed.pathname.replace(/^\/+/, '') || '';
    const app = parsed.searchParams.get('app');
    switch (action) {
      case 'switch':
        if (!id) return { kind: 'unknown', raw: url, reason: 'missing id' };
        return { kind: 'switchProvider', providerId: id, appType: app ?? 'claude_code' };
      case 'focus':
        return { kind: 'focusProvider', providerId: id };
      case 'focus-mcp':
        return { kind: 'focusMcp', serverId: id };
      case 'focus-skill':
        return { kind: 'focusSkill', skillId: id };
      case 'deeplinks':
        return { kind: 'showDeeplinks' };
      default:
        return { kind: 'unknown', raw: url, reason: `unknown action: ${action}` };
    }
  } catch {
    return { kind: 'unknown', raw: url, reason: 'unparseable URL' };
  }
}

function fail(cmd: string): never {
  throw new Error(`mock error forced for "${cmd}" (window.__aagDemoSetError)`);
}

// ----- Aggregate helpers (mirror Rust DAO SQL) -----

function filterRecords(filter: { since?: number | null; until?: number | null; appId?: string | null }) {
  return usageRecords.filter((r) => {
    if (filter.since && r.ts < filter.since) return false;
    if (filter.until && r.ts > filter.until) return false;
    if (filter.appId && r.appId !== filter.appId) return false;
    return true;
  });
}

function summarize(records: UsageRecordInput[]): UsageSummary {
  const s = records.reduce(
    (acc, r) => {
      acc.requests += 1;
      acc.prompt += r.promptTokens;
      acc.completion += r.completionTokens;
      acc.cost += r.costUsd;
      acc.duration += r.durationMs;
      return acc;
    },
    { requests: 0, prompt: 0, completion: 0, cost: 0, duration: 0 },
  );
  return {
    totalRequests: s.requests,
    totalPromptTokens: s.prompt,
    totalCompletionTokens: s.completion,
    totalTokens: s.prompt + s.completion,
    totalCostUsd: s.cost,
    avgDurationMs: s.requests ? Math.round(s.duration / s.requests) : 0,
  };
}

function byModel(records: UsageRecordInput[]): ModelUsage[] {
  const map = new Map<string, ModelUsage>();
  for (const r of records) {
    const cur = map.get(r.model) ?? {
      model: r.model,
      requestCount: 0,
      totalPromptTokens: 0,
      totalCompletionTokens: 0,
      totalTokens: 0,
      totalCostUsd: 0,
    };
    cur.requestCount += 1;
    cur.totalPromptTokens += r.promptTokens;
    cur.totalCompletionTokens += r.completionTokens;
    cur.totalTokens += r.promptTokens + r.completionTokens;
    cur.totalCostUsd += r.costUsd;
    map.set(r.model, cur);
  }
  return [...map.values()].sort((a, b) => b.totalTokens - a.totalTokens);
}

function byProvider(records: UsageRecordInput[]): ProviderUsage[] {
  const map = new Map<string, ProviderUsage>();
  for (const r of records) {
    const cur = map.get(r.providerId) ?? {
      providerId: r.providerId,
      requestCount: 0,
      totalPromptTokens: 0,
      totalCompletionTokens: 0,
      totalTokens: 0,
      totalCostUsd: 0,
    };
    cur.requestCount += 1;
    cur.totalPromptTokens += r.promptTokens;
    cur.totalCompletionTokens += r.completionTokens;
    cur.totalTokens += r.promptTokens + r.completionTokens;
    cur.totalCostUsd += r.costUsd;
    map.set(r.providerId, cur);
  }
  return [...map.values()].sort((a, b) => b.totalTokens - a.totalTokens);
}

// ---------------------------------------------------------------------------
// Mock handler
// ---------------------------------------------------------------------------

type Args = Record<string, unknown>;

export function installMockBackend() {
  mockIPC(async (cmd: string, payload: InvokeArgs = {}) => {
    if (forcedError && forcedError === cmd) fail(cmd);
    const p = payload as Args;

    switch (cmd) {
      // ---- app ----
      case 'app_info':
        return { name: 'AI Agent Gateway (demo)', version: '0.2.0', phase: '12' };
      case 'app_check_update':
        return null;

      // ---- providers ----
      case 'provider_list':
        return providers.get(String(p.appType)) ?? [];
      case 'provider_current':
        return currentByApp.get(String(p.appType)) ?? null;
      case 'provider_get': {
        const list = providers.get(String(p.appType)) ?? [];
        return list.find((x) => x.id === p.id) ?? null;
      }
      case 'provider_save': {
        const appType = p.appType as string;
        const provider = p.provider as Provider;
        const list = providers.get(appType) ?? [];
        const idx = list.findIndex((x) => x.id === provider.id);
        if (idx >= 0) list[idx] = provider;
        else list.push(provider);
        providers.set(appType, list);
        return null;
      }
      case 'provider_delete': {
        const appType = String(p.appType);
        providers.set(appType, (providers.get(appType) ?? []).filter((x) => x.id !== p.id));
        if (currentByApp.get(appType) === p.id) currentByApp.delete(appType);
        return null;
      }
      case 'provider_switch': {
        const appType = String(p.appType);
        const id = String(p.id);
        if (appType === 'antigravity') {
          throw new Error(
            'Antigravity switching is not supported yet (no public config schema)',
          );
        }
        if (!(providers.get(appType) ?? []).some((p) => p.id === id)) {
          throw new Error(`provider "${id}" not found for app "${appType}"`);
        }
        currentByApp.set(appType, id);
        return {
          app_type: appType,
          provider_id: id,
          written: liveWrite(appType, id),
          backup: null,
        };
      }

      // ---- proxy ----
      case 'proxy_status':
        return {
          running: proxyRunning,
          config: { host: '127.0.0.1', port: 8787, authToken: '', enabled: true },
        };
      case 'proxy_start':
        proxyRunning = true;
        return 'mock proxy started (demo: no real HTTP listener)';
      case 'proxy_stop':
        proxyRunning = false;
        return null;
      case 'proxy_provider_list':
        return proxyProviders;
      case 'proxy_provider_save': {
        const provider = p.provider as ProxyProvider;
        const idx = proxyProviders.findIndex((x) => x.id === provider.id);
        if (idx >= 0) proxyProviders[idx] = provider;
        else proxyProviders.push(provider);
        return null;
      }
      case 'proxy_provider_delete': {
        const id = String(p.id);
        const idx = proxyProviders.findIndex((x) => x.id === id);
        if (idx < 0) throw new Error(`proxy provider "${id}" not found`);
        proxyProviders.splice(idx, 1);
        return null;
      }
      case 'route_list':
        return routes;
      case 'route_save': {
        const r = p.route as Route;
        const idx = routes.findIndex((x) => x.alias === r.alias);
        if (idx >= 0) routes[idx] = r;
        else routes.push(r);
        return null;
      }
      case 'route_delete': {
        const alias = String(p.alias);
        const idx = routes.findIndex((x) => x.alias === alias);
        if (idx < 0) throw new Error(`route "${alias}" not found`);
        routes.splice(idx, 1);
        return null;
      }

      // ---- backup ----
      case 'backup_create': {
        const label = (p.label as string | null) ?? 'manual';
        const entry: BackupEntry = {
          id: `bak-${Date.now()}`,
          label,
          sizeBytes: 64_000 + Math.floor(Math.random() * 200_000),
          createdAt: new Date().toISOString(),
        };
        backups.unshift(entry);
        return entry;
      }
      case 'backup_list':
        return backups;
      case 'backup_delete': {
        const idx = backups.findIndex((b) => b.id === p.id);
        if (idx < 0) throw new Error(`backup "${p.id}" not found`);
        backups.splice(idx, 1);
        return null;
      }

      // ---- MCP ----
      case 'mcp_list':
        return mcpServers;
      case 'mcp_save': {
        const s = p.server as McpServer;
        const idx = mcpServers.findIndex((x) => x.id === s.id);
        if (idx >= 0) mcpServers[idx] = s;
        else mcpServers.push(s);
        return null;
      }
      case 'mcp_delete': {
        const idx = mcpServers.findIndex((x) => x.id === p.id);
        if (idx < 0) throw new Error(`MCP server "${p.id}" not found`);
        mcpServers.splice(idx, 1);
        return null;
      }
      case 'mcp_toggle_app': {
        const s = mcpServers.find((x) => x.id === p.id);
        if (!s) throw new Error(`MCP server "${p.id}" not found`);
        if (p.app === 'claude') s.enabledClaude = Boolean(p.enabled);
        else if (p.app === 'codex') s.enabledCodex = Boolean(p.enabled);
        else throw new Error(`unknown app "${p.app}"`);
        return null;
      }

      // ---- skills ----
      case 'skill_list':
        return skills;
      case 'skill_save': {
        const s = p.skill as InstalledSkill;
        const idx = skills.findIndex((x) => x.id === s.id);
        if (idx >= 0) skills[idx] = s;
        else skills.push(s);
        return null;
      }
      case 'skill_delete': {
        const idx = skills.findIndex((x) => x.id === p.id);
        if (idx < 0) throw new Error(`skill "${p.id}" not found`);
        skills.splice(idx, 1);
        return null;
      }

      // ---- sessions ----
      case 'session_list': {
        const appId = p.appId as string | null;
        const list = appId ? sessions.filter((s) => s.appId === appId) : sessions;
        return [...list].sort((a, b) => (b.lastActiveAt ?? 0) - (a.lastActiveAt ?? 0));
      }
      case 'session_get':
        return sessions.find((s) => s.id === p.id) ?? null;
      case 'session_delete': {
        const idx = sessions.findIndex((s) => s.id === p.id);
        if (idx < 0) throw new Error(`session "${p.id}" not found`);
        sessions.splice(idx, 1);
        return null;
      }

      // ---- usage ----
      case 'usage_record': {
        usageRecords.push(p.record as UsageRecordInput);
        return usageRecords.length;
      }
      case 'usage_summary': {
        const filter = p as { since?: number | null; until?: number | null; appId?: string | null };
        return summarize(filterRecords(filter));
      }
      case 'usage_by_model': {
        const filter = p as { since?: number | null; until?: number | null; appId?: string | null };
        return byModel(filterRecords(filter));
      }
      case 'usage_by_provider': {
        const filter = p as { since?: number | null; until?: number | null; appId?: string | null };
        return byProvider(filterRecords(filter));
      }

      // ---- sync ----
      case 'sync_list':
        return syncConfigs;
      case 'sync_save': {
        const c = p.config as SyncConfig;
        const idx = syncConfigs.findIndex((x) => x.provider === c.provider);
        if (idx >= 0) syncConfigs[idx] = c;
        else syncConfigs.push(c);
        return null;
      }
      case 'sync_delete': {
        const idx = syncConfigs.findIndex((x) => x.provider === p.provider);
        if (idx < 0) throw new Error(`sync config "${p.provider}" not found`);
        syncConfigs.splice(idx, 1);
        return null;
      }
      case 'sync_record_result': {
        const c = syncConfigs.find((x) => x.provider === p.provider);
        if (!c) throw new Error(`sync config "${p.provider}" not found`);
        c.lastSyncAt = Math.floor(Date.now() / 1000);
        c.lastError = (p.error as string | null) ?? null;
        return null;
      }
      case 'sync_check':
        return null;
      case 'sync_upload':
        return { status: 'uploaded', bytes: 214_572 };
      case 'sync_download':
        return { status: 'downloaded', bytes: 214_572 };

      // ---- settings / codex cli ----
      case 'settings_get':
        return settings.get(String(p.key)) ?? null;
      case 'settings_set': {
        const key = String(p.key);
        const value = String(p.value);
        if (value) settings.set(key, value);
        else settings.delete(key);
        return null;
      }
      case 'codex_cli_status':
        return {
          isolationEnabled: true,
          configDir: 'C:\\Users\\dev\\.codex-cli',
          initialized: true,
          codexCliHome: 'C:\\Users\\dev\\.codex-cli',
          outboundProxy: settings.get('outbound_proxy') ?? null,
          codexBinary: 'codex.exe',
          currentProviderId: currentByApp.get('codex') ?? null,
          currentProviderName:
            (providers.get('codex') ?? []).find((p) => p.id === currentByApp.get('codex'))?.name ??
            null,
        };
      case 'codex_open_cli_terminal':
        return {
          codexHome: 'C:\\Users\\dev\\.codex-cli',
          codexBinary: 'codex.exe',
          env: [
            ['CODEX_HOME', 'C:\\Users\\dev\\.codex-cli'],
            ['HTTP_PROXY', settings.get('outbound_proxy') ?? ''],
            ['HTTPS_PROXY', settings.get('outbound_proxy') ?? ''],
          ],
        };

      // ---- copilot cli ----
      case 'copilot_open_cli_terminal':
        return {
          copilotBinary: 'copilot.cmd',
          providerId: currentByApp.get('copilot') ?? 'copilot-claude',
          env: [
            ['COPILOT_PROVIDER_BASE_URL', 'http://127.0.0.1:8787/v1'],
            ['COPILOT_PROVIDER_TYPE', 'openai'],
            ['COPILOT_MODEL', 'claude-sonnet-4'],
          ],
        };

      // ---- deep links ----
      case 'deeplink_parse':
        return parseDeeplink(String(p.url));

      // ---- events (subscribeDeeplink -> listen) ----
      case 'plugin:event|listen': {
        const event = String(p.event);
        const handlerId = Number(p.handler);
        const eventId = `evt-${handlerId}`;
        if (event === 'aag://deeplink') deeplinkListeners.set(eventId, handlerId);
        return eventId;
      }
      case 'plugin:event|unlisten': {
        const eventId = String(p.id);
        deeplinkListeners.delete(eventId);
        return null;
      }

      default:
        console.warn(`[aag-demo] unhandled command: ${cmd}`, p);
        throw new Error(`unknown command "${cmd}" (demo backend)`);
    }
  });
}

// ---------------------------------------------------------------------------
// Bootstrap
// ---------------------------------------------------------------------------

const DEMO_PROXY_BASE = 'http://127.0.0.1:8787';

/// Intercept the two direct proxy fetches (ProxyPage polls these via `fetch`,
/// not invoke) so the demo shows the failover health table. Any other URL is
/// delegated to the real fetch.
function installFetchInterceptor() {
  const realFetch = window.fetch.bind(window);
  window.fetch = async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input);
    if (url.startsWith(`${DEMO_PROXY_BASE}/health`)) {
      return new Response(
        JSON.stringify({
          status: 'ok',
          providers: proxyProviders.map((p) => ({
            id: p.id,
            enabled: p.enabled,
            breaker: { state: 'closed', failures: 0, successes: 0, cooldownMs: p.circuitBreaker.cooldownMs },
          })),
          models: [...new Set(routes.map((r) => r.alias))],
        }),
        { status: 200, headers: { 'content-type': 'application/json' } },
      );
    }
    if (url.startsWith(`${DEMO_PROXY_BASE}/v1/models`)) {
      const ids = [...new Set(routes.map((r) => r.alias))].sort();
      return new Response(
        JSON.stringify({
          object: 'list',
          data: ids.map((id) => ({
            id,
            type: 'model',
            display_name: id,
            object: 'model',
            created: 1_700_000_000,
            owned_by: 'ai-agent-gateway',
          })),
          has_more: false,
          first_id: ids[0] ?? null,
          last_id: ids[ids.length - 1] ?? null,
        }),
        { status: 200, headers: { 'content-type': 'application/json' } },
      );
    }
    return realFetch(input, init);
  };
}

export function initDemo() {
  if (!isDemo) return;
  installMockBackend();
  installFetchInterceptor();
  // Expose debug hooks on the console.
  Object.assign(window, {
    __aagDemoSetError: setMockError,
    __aagDemoSeedDeeplink: seedDeeplink,
    __aagDemoIsDemo: true,
  });
  console.info(
    '[aag-demo] running with an in-browser mock backend (no Tauri). ' +
      'Debug: window.__aagDemoSetError(cmd|null) · window.__aagDemoSeedDeeplink(url)',
  );
  // Exercise the deep-link flow after mount so DeepLinkPage has a sample URL.
  const injected = new URLSearchParams(window.location.search).get('deeplink');
  if (injected) {
    setTimeout(() => seedDeeplink(injected), 1500);
  }
}
