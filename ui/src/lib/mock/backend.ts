/// Browser-only demo backend.
///
/// The Tauri desktop app wires the frontend to the Rust backend through
/// `window.__TAURI_INTERNALS__.invoke`. In a plain browser (e.g. `cd ui &&
/// npm run dev`) that bridge does not exist, so this module installs a mock
/// IPC handler with the official `mockIPC` helper and serves realistic seed
/// data for every command the UI invokes. It lets you exercise the whole
/// React UI - Claude (endpoint + failover chain) and Codex (gateway +
/// providers + launchers) - without launching the Rust app.
///
/// Activation is automatic and exclusive:
///   - running in a Tauri webview  -> this module is a no-op (real IPC wins)
///   - running under `vite dev`    -> demo mode installs the mock handler
///   - production build in a plain browser -> no-op (not a supported target)
///
/// Debug hooks (open the browser console):
///   - `window.__aagDemoSetError('provider_list')`  - make a command reject
///   - `window.__aagDemoSetError(null)`             - clear the forced error
import type { InvokeArgs } from '@tauri-apps/api/core';
import { mockIPC } from '@tauri-apps/api/mocks';
import { app_version, is_demo_build } from '../build_info.js';
import type { Provider } from '../api/types';
import type { ProxyProvider, Route } from '../api/proxy';

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
  (Boolean((import.meta as { env?: Record<string, unknown> }).env?.DEV) || is_demo_build);

// ---------------------------------------------------------------------------
// In-memory store (module-level, survives HMR only while the page is open)
// ---------------------------------------------------------------------------

const providers = new Map<string, Provider[]>();
const currentByApp = new Map<string, string>();

function seedProviders() {
  providers.set('claude_code', [
    {
      id: 'claude-1',
      name: 'Claude Sonnet 5',
      settingsConfig: {
        env: { ANTHROPIC_BASE_URL: 'https://api.anthropic.com', ANTHROPIC_AUTH_TOKEN: 'sk-ant-demo' },
        model: 'claude-sonnet-5',
      },
      inFailoverQueue: true,
      createdAt: Math.floor(Date.now() / 1000) - 86400 * 30,
    },
    {
      id: 'claude-2',
      name: 'Anthropic via proxy',
      settingsConfig: {
        env: { ANTHROPIC_BASE_URL: 'http://127.0.0.1:8989', ANTHROPIC_AUTH_TOKEN: 'demo' },
        model: 'claude-opus-4-8',
      },
      inFailoverQueue: true,
      createdAt: Math.floor(Date.now() / 1000) - 86400 * 5,
    },
  ]);
  providers.set('codex', [
    {
      id: 'codex-oai',
      name: 'OpenAI GPT-5',
      settingsConfig: {
        baseUrl: 'https://api.openai.com',
        apiKey: 'sk-demo',
        model: 'gpt-5',
        auth: { tokens: { value: 'sk-demo', source: 'demo' } },
        config: 'model = "gpt-5"\ntemperature = 0.2\n',
      },
      inFailoverQueue: true,
      createdAt: Math.floor(Date.now() / 1000) - 86400 * 20,
    },
    {
      id: 'codex-aag',
      name: 'Via AAG gateway',
      settingsConfig: {
        baseUrl: 'http://127.0.0.1:8989',
        apiKey: 'demo',
        model: 'gpt-5',
        auth: { tokens: { value: 'demo', source: 'aag' } },
        config:
          'model = "gpt-5"\n' +
          '[model_providers.aag]\n' +
          'name = "AAG Gateway"\n' +
          'base_url = "http://127.0.0.1:8989/v1"\n' +
          'wire_api = "responses"\n',
      },
      inFailoverQueue: true,
      createdAt: Math.floor(Date.now() / 1000) - 86400 * 5,
    },
  ]);
  providers.set('copilot', [
    {
      id: 'copilot-1',
      name: 'Copilot via AAG',
      settingsConfig: {
        model: 'claude-sonnet-5',
        env: { COPILOT_PROVIDER_BASE_URL: 'http://127.0.0.1:8989/v1', COPILOT_PROVIDER_TYPE: 'openai' },
      },
      inFailoverQueue: true,
      createdAt: Math.floor(Date.now() / 1000) - 86400 * 3,
    },
  ]);
  providers.set('pi', [
    {
      id: 'pi-1',
      name: 'pi via AAG',
      settingsConfig: {
        providerId: 'aag',
        baseUrl: 'http://127.0.0.1:8989/v1',
        apiKey: 'demo',
        api: 'openai-completions',
        model: 'claude-sonnet-5',
        models: ['claude-sonnet-5'],
      },
      inFailoverQueue: true,
      createdAt: Math.floor(Date.now() / 1000) - 86400 * 2,
    },
  ]);
}
seedProviders();

let proxyRunning = false;
const proxyConfig = {
  host: '127.0.0.1',
  port: 8989,
  authToken: 'aag-demo-token',
  enabled: true,
};

let pinnedProvider: string | null = null;

const proxyProviders: ProxyProvider[] = [
  {
    id: 'anthropic-primary',
    name: 'Anthropic official',
    type: 'anthropic',
    apiKey: 'sk-ant-demo',
    baseURL: 'https://api.anthropic.com',
    models: ['claude-fable-5', 'claude-opus-4-8', 'claude-sonnet-5'],
    enabled: true,
    timeoutMs: 30_000,
    weight: 1,
    circuitBreaker: { failureThreshold: 3, cooldownMs: 5_000, halfOpenRetries: 1, successThreshold: 2 },
    modelMapping: {},
  },
  {
    id: 'openai-fallback',
    name: 'OpenAI fallback',
    type: 'openai',
    apiKey: 'sk-oai-demo',
    baseURL: 'https://api.openai.com',
    models: ['gpt-5', 'gpt-4o'],
    enabled: true,
    timeoutMs: 30_000,
    weight: 1,
    circuitBreaker: { failureThreshold: 5, cooldownMs: 10_000, halfOpenRetries: 1, successThreshold: 2 },
    modelMapping: { 'claude-sonnet-5': 'gpt-5' },
  },
];

const routes: Route[] = [
  { alias: 'claude-sonnet-5', description: 'official', hops: [
    { provider: 'anthropic-primary', model: 'claude-sonnet-5' },
    { provider: 'openai-fallback', model: 'gpt-5' },
  ]},
  { alias: 'claude-fable-5', description: 'Claude Fable 5 via failover chain', hops: [
    { provider: 'anthropic-primary', model: 'claude-fable-5' },
    { provider: 'openai-fallback', model: 'o3' },
  ]},
  { alias: 'codex-5-6-sol', description: '5.6Sol — configure upstream models', hops: [
    { provider: 'openai-fallback', model: 'gpt-5' },
  ]},
  { alias: 'codex-5-6-luna', description: '5.6Luna — configure upstream models', hops: [
    { provider: 'openai-fallback', model: 'gpt-5' },
  ]},
  { alias: 'codex-5-6-terra', description: '5.6Terra — configure upstream models', hops: [
    { provider: 'openai-fallback', model: 'gpt-4o' },
  ]},
  { alias: 'codex-5-5', description: '5.5 — configure upstream models', hops: [
    { provider: 'openai-fallback', model: 'gpt-4o' },
  ]},
];

const settings = new Map<string, string>([
  ['codex_cli_home', ''],
  ['outbound_proxy', ''],
]);

// ---------------------------------------------------------------------------
// Forced-error debug hook
// ---------------------------------------------------------------------------

let forcedError: string | null = null;
function setMockError(cmd: string | null) {
  forcedError = cmd;
}
function fail(cmd: string): never {
  throw new Error(`forced demo error for "${cmd}"`);
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
        return { name: 'AI Agent Gateway', version: app_version, phase: '16' };
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
        if (!(providers.get(appType) ?? []).some((x) => x.id === id)) {
          throw new Error(`provider "${id}" not found for app "${appType}"`);
        }
        currentByApp.set(appType, id);
        // Mirror the Rust live-write split (Phase 17): codex writes the
        // mirror CODEX_HOME, copilot merges settings.json, pi writes
        // models.json + settings.json, claude_code writes nothing.
        const written: string[] = [];
        if (appType === 'codex') written.push('~/.codex-cli/config.toml', '~/.codex-cli/auth.json');
        else if (appType === 'copilot') written.push('~/.copilot/settings.json');
        else if (appType === 'pi') written.push('~/.pi/agent/models.json', '~/.pi/agent/settings.json');
        return { app_type: appType, provider_id: id, written, backup: null };
      }
      case 'provider_set_failover_queue': {
        const appType = String(p.appType);
        const id = String(p.id);
        const inQueue = Boolean(p.inQueue);
        const list = providers.get(appType) ?? [];
        const target = list.find((x) => x.id === id);
        if (!target) throw new Error(`provider "${id}" not found for app "${appType}"`);
        target.inFailoverQueue = inQueue;
        return null;
      }

      // ---- import_cc_switch_providers ----
// Demo mode can't open the user's cc-switch SQLite DB. Return a
// small synthetic payload so the import button showcases the Phase 22
// report toast (imported + skipped counts + auto-seeded routes) —
// empty reports in the UI make the feature look broken.
case 'import_cc_switch_providers':
  return {
    sourceDb: '(demo: synthetic cc-switch payload)',
    imported: [
      {
        id: 'ccs-demo-anthropic',
        appType: 'claude_code',
        name: 'cc-switch · Anthropic demo',
      },
    ],
    skippedExisting: [],
    skippedNoKey: [],
    totalInSource: 1,
    importedRoutes: ['claude-fable-5', 'claude-sonnet-5'],
  };

      // ---- proxy ----
      case 'proxy_status':
        return { running: proxyRunning, config: proxyConfig };
      case 'proxy_start':
        proxyRunning = true;
        return 'mock proxy started (demo: no real HTTP listener)';
      case 'proxy_stop':
        proxyRunning = false;
        return null;
      case 'proxy_config_get':
        return proxyConfig;
      case 'proxy_config_save':
        Object.assign(proxyConfig, p.config);
        return null;

      // ---- proxy providers + routes (failover chain editor) ----
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
      case 'proxy_get_pinned_provider':
        return pinnedProvider;
      case 'proxy_set_pinned_provider': {
        pinnedProvider = p.providerId ? String(p.providerId) : null;
        return null;
      }
      // Demo mock: simulate fetching upstream models from a provider.
      // Returns a few realistic model ids so the ⤓ button is exercisable.
      case 'proxy_fetch_models':
        return [
          { id: 'claude-fable-5', owned_by: 'anthropic' },
          { id: 'claude-opus-4-8', owned_by: 'anthropic' },
          { id: 'claude-opus-4-7', owned_by: 'anthropic' },
          { id: 'claude-opus-4-6', owned_by: 'anthropic' },
          { id: 'claude-sonnet-5', owned_by: 'anthropic' },
          { id: 'claude-sonnet-4-6', owned_by: 'anthropic' },
          { id: 'claude-haiku-4-5', owned_by: 'anthropic' },
          { id: 'glm-5.2', owned_by: 'zhipu' },
          { id: 'glm-5.2[1M]', owned_by: 'zhipu' },
          { id: 'glm-5.3[1M]', owned_by: 'zhipu' },
        ];
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
      case 'route_rename': {
        const from = String(p.from);
        const to = String(p.to).trim();
        if (!to) throw new Error('new alias must not be empty');
        if (to === from) return null;
        if (routes.some((x) => x.alias === to)) {
          throw new Error(`UNIQUE constraint failed: routes.alias (${to})`);
        }
        const r = routes.find((x) => x.alias === from);
        if (!r) throw new Error(`route "${from}" not found`);
        r.alias = to;
        return null;
      }

      // ---- settings ----
      case 'settings_get':
        return settings.get(String(p.key)) ?? null;
      case 'settings_set': {
        const key = String(p.key);
        const value = String(p.value);
        if (value) settings.set(key, value);
        else settings.delete(key);
        return null;
      }

      // ---- codex cli / desktop (mirror install) ----
      case 'codex_cli_status':
        return {
          isolationEnabled: true,
          configDir: '~/.codex-cli',
          initialized: (providers.get('codex') ?? []).length > 0,
          codexCliHome: settings.get('codex_cli_home') || null,
          outboundProxy: settings.get('outbound_proxy') ?? null,
          codexBinary: 'codex.exe',
          currentProviderId: currentByApp.get('codex') ?? null,
          currentProviderName:
            (providers.get('codex') ?? []).find((p) => p.id === currentByApp.get('codex'))?.name ??
            null,
        };
      case 'codex_open_cli_terminal':
        return {
          codexHome: '~/.codex-cli',
          codexBinary: 'codex.exe',
          env: [
            ['CODEX_HOME', '~/.codex-cli'],
            ['HTTP_PROXY', settings.get('outbound_proxy') ?? ''],
            ['HTTPS_PROXY', settings.get('outbound_proxy') ?? ''],
          ],
        };
      case 'codex_open_desktop_window':
        return {
          codexHome: '~/.codex-cli',
          codexBinary: 'C:\\Program Files\\WindowsApps\\OpenAI.Codex_*\\app\\ChatGPT.exe',
          mirror: true,
          store: true,
        };

      // ---- copilot cli ----
      case 'copilot_open_cli_terminal':
        return {
          copilotBinary: 'copilot.cmd',
          providerId: currentByApp.get('copilot') ?? 'copilot-claude',
          env: [
            ['COPILOT_PROVIDER_BASE_URL', 'http://127.0.0.1:8989/v1'],
            ['COPILOT_PROVIDER_TYPE', 'openai'],
            ['COPILOT_MODEL', 'claude-sonnet-4'],
          ],
        };

      default:
        console.warn(`[aag-demo] unhandled command: ${cmd}`, p);
        throw new Error(`unknown command "${cmd}" (demo backend)`);
    }
  });
}

// ---------------------------------------------------------------------------
// Bootstrap
// ---------------------------------------------------------------------------

function demo_proxy_base() {
  return `http://${proxyConfig.host}:${proxyConfig.port}`;
}

function mock_proxy_path(url: string, path: string) {
  return url === `${demo_proxy_base()}${path}` || url.startsWith(`${demo_proxy_base()}${path}?`);
}

/// Intercept the direct proxy fetches (the pages poll `/health` and
/// `/v1/models` via `fetch`, not invoke) so the demo shows live status.
/// Any other URL is delegated to the real fetch.
function installFetchInterceptor() {
  const realFetch = window.fetch.bind(window);
  window.fetch = async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = input instanceof Request ? input.url : String(input);
    if (mock_proxy_path(url, '/health')) {
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
    if (mock_proxy_path(url, '/v1/models')) {
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
  Object.assign(window, {
    __aagDemoSetError: setMockError,
    __aagDemoIsDemo: true,
  });
  console.info(
    '[aag-demo] running with an in-browser mock backend (no Tauri). ' +
      'Debug: window.__aagDemoSetError(cmd|null)',
  );
}
