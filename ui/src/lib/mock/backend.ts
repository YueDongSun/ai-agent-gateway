import { mock_arena_snapshot } from './arena_visualization.js';
import { mock_tailnet_discovery_command } from './tailnet_discovery.js';
import { mock_prompt_workspace_command } from './prompt_workspace.js';
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
import type { Provider, Provider_config_status, Config_edit_target, LiveWriteResult } from '../api/types';
import type { ProxyProvider, Route } from '../api/proxy';
import { proxy_client_url } from '../api/proxy';
import { mock_dsh_command } from './dsh.js';
import { mock_dsh_market_command } from './dsh_market.js';
import { mock_mcp_command } from './mcp.js';
import { mock_prompt_command } from './prompt.js';
import { mock_supplier_command } from './suppliers.js';
import { mock_provider_screening_command } from './provider_screening.js';
import { mock_usage_command } from './usage.js';
import { mock_workflow_command } from './workflow_registry.js';

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
const demo_written_config = new Set<string>();

/** In-memory contents for the direct JSON editor (browser demo only; the
 *  demo never touches real files — this is a synthetic string store). */
const demo_edited_config = new Map<string, string>();

/** Synthetic locations only; browser demo never discovers or writes native files. */
function demo_provider_config_status(app_type: string): Provider_config_status {
  const config_paths: Record<string, string[]> = {
    claude_code: ['~/.claude-cli/settings.json'],
    codex: [`${settings.get('codex_cli_home') || '~/.codex-cli'}/config.toml`, `${settings.get('codex_cli_home') || '~/.codex-cli'}/auth.json`],
    copilot: ['~/.copilot/settings.json'],
    gemini: ['~/.gemini/.env', '~/.gemini/settings.json'],
    opencode: ['~/.config/opencode/opencode.json'],
    pi: ['~/.pi/agent/models.json', '~/.pi/agent/settings.json'],
    kilo_cli: ['~/.config/kilo/kilo.jsonc'],
    cline_cli: ['~/.cline_cli/data/settings/providers.json', '~/.cline_cli/data/settings/models.json'],
    claude_vscode: [],
    kilo_vscode: ['~/.config/kilo/kilo.jsonc'],
  };
  const supported = Object.prototype.hasOwnProperty.call(config_paths, app_type);
  const existing = supported && (app_type === 'claude_code'
    ? settings.get('claude_mirror_written') === '1'
    : demo_written_config.has(app_type) || currentByApp.has(app_type));
  return {
    appType: app_type,
    platform: 'browser',
    supported,
    mode: !supported ? 'unsupported' : app_type === 'claude_code' || app_type === 'codex' || app_type === 'cline_cli'
      ? 'mirror' : app_type === 'claude_vscode' ? 'environment' : 'config',
    source: !supported ? 'unsupported' : app_type === 'codex' && settings.get('codex_cli_home')
      ? 'setting' : existing ? 'existing' : 'default',
    configPaths: supported ? config_paths[app_type] : [],
    existing,
    reason: null,
  };
}

/** Direct-JSON editor mock: synthetic in-memory file (Kilo family only),
 *  mirroring the scoping and object-root contract of the Rust service. */
const EDITABLE_APPS = new Set(['kilo_cli', 'kilo_vscode']);
const demo_config_skeleton = '{\n  // Kilo reads JSONC with comments\n  "provider": {},\n  "model": ""\n}\n';
const max_client_edit_bytes = 2 * 1024 * 1024;

function demo_client_config_read(app_type: string): Config_edit_target {
  if (!EDITABLE_APPS.has(app_type)) {
    throw new Error('AAG_CLIENT_CONFIG_EDIT_UNSUPPORTED');
  }
  return {
    appType: app_type,
    path: '~/.config/kilo/kilo.jsonc',
    contents: demo_edited_config.get(app_type) ?? demo_config_skeleton,
    existing: demo_edited_config.has(app_type),
  };
}

function demo_client_config_save(app_type: string, contents: string): LiveWriteResult {
  if (!EDITABLE_APPS.has(app_type)) {
    throw new Error('AAG_CLIENT_CONFIG_EDIT_UNSUPPORTED');
  }
  if (new TextEncoder().encode(contents).length > max_client_edit_bytes) {
    throw new Error('AAG_CLIENT_CONFIG_EDIT_TOO_LARGE');
  }
  // Same fail-closed shape as the real command: strip comments, parse,
  // require an object root.
  const stripped = contents.replace(/\/\*[\s\S]*?\*\//g, '').replace(/(^|[^:])\/\/.*$/gm, '$1');
  let parsed: unknown;
  try {
    parsed = JSON.parse(stripped);
  } catch {
    throw new Error('AAG_CLIENT_CONFIG_EDIT_INVALID_JSON');
  }
  if (parsed === null || typeof parsed !== 'object' || Array.isArray(parsed)) {
    throw new Error('AAG_CLIENT_CONFIG_EDIT_ROOT_NOT_OBJECT');
  }
  demo_edited_config.set(app_type, contents);
  demo_written_config.add(app_type);
  return { app_type, provider_id: '', written: ['~/.config/kilo/kilo.jsonc'], backup: null };
}

function seedProviders() {

  providers.set('claude_code', [
    {
      id: 'claude-1',
      name: 'Claude Sonnet 5',
      icon: 'spark',
      iconColor: '#d97757',
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
      icon: 'spark',
      iconColor: '#b35c3f',
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
      icon: 'knot',
      iconColor: '#10a37f',
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
      icon: 'hub',
      iconColor: '#5b5bd6',
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
      icon: 'robot',
      iconColor: '#6f42c1',
      settingsConfig: {
        model: 'claude-sonnet-5',
        env: { COPILOT_PROVIDER_BASE_URL: 'http://127.0.0.1:8989/v1', COPILOT_PROVIDER_TYPE: 'openai' },
      },
      inFailoverQueue: true,
      createdAt: Math.floor(Date.now() / 1000) - 86400 * 3,
    },
  ]);
  providers.set('opencode', [
    {
      id: 'opencode-aag',
      name: 'OpenCode via AAG',
      icon: 'lotus',
      iconColor: '#334155',
      settingsConfig: {
        npm: '@ai-sdk/openai-compatible',
        options: { baseURL: 'http://127.0.0.1:8989/v1', apiKey: 'demo' },
        models: { 'claude-sonnet-5': { name: 'claude-sonnet-5' } },
      },
      inFailoverQueue: true,
      createdAt: Math.floor(Date.now() / 1000) - 86400,
    },
  ]);
  providers.set('kilo_cli', [
    {
      id: 'kilo-aag',
      name: 'Kilo via AAG',
      icon: 'motion',
      iconColor: '#0d9488',
      settingsConfig: {
        npm: '@ai-sdk/openai-compatible',
        options: { baseURL: 'http://127.0.0.1:8989/v1', apiKey: 'demo' },
        models: { 'claude-sonnet-5': { name: 'claude-sonnet-5' } },
      },
      inFailoverQueue: true,
      createdAt: Math.floor(Date.now() / 1000) - 86400,
    },
  ]);
  providers.set('cline_cli', [
    {
      id: 'cline-aag',
      name: 'Cline CLI via AAG',
      icon: 'keyboard',
      iconColor: '#ea580c',
      settingsConfig: { protocol: 'openai', baseUrl: 'http://127.0.0.1:8989/v1', apiKey: 'demo', model: 'claude-sonnet-5' },
      inFailoverQueue: true,
      createdAt: Math.floor(Date.now() / 1000) - 3600,
    },
  ]);
  providers.set('claude_vscode', [
    {
      id: 'claude-vscode-aag',
      name: 'Claude Code (VS Code) via AAG',
      icon: 'spark',
      iconColor: '#d97757',
      settingsConfig: {
        env: {
          ANTHROPIC_BASE_URL: 'http://127.0.0.1:8989',
          ANTHROPIC_AUTH_TOKEN: 'demo',
        },
        model: 'claude-sonnet-5',
      },
      inFailoverQueue: true,
      createdAt: Math.floor(Date.now() / 1000) - 86400,
    },
  ]);
  providers.set('kilo_vscode', [
    {
      id: 'kilo-vscode-aag',
      name: 'Kilo Code (VS Code) via AAG',
      icon: 'motion',
      iconColor: '#0f766e',
      settingsConfig: {
        npm: '@ai-sdk/openai-compatible',
        options: { baseURL: 'http://127.0.0.1:8989/v1', apiKey: 'demo' },
        models: { 'claude-sonnet-5': { name: 'claude-sonnet-5' } },
      },
      inFailoverQueue: true,
      createdAt: Math.floor(Date.now() / 1000) - 86400,
    },
  ]);
  providers.set('pi', [
    {
      id: 'pi-1',
      name: 'pi via AAG',
      icon: 'chip',
      iconColor: '#7c3aed',
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
let running_routing_signature: string | null = null;
function routing_signature(): string {
  return JSON.stringify([proxyProviders, routes, [...providers], currentByApp.get('codex'), proxyConfig]);
}
const proxyConfig = {
  host: '127.0.0.1',
  port: 8989,
  authToken: 'aag-demo-token',
  enabled: true,
};

let pinnedProvider: string | null = null;

// Failover-queue spread mode (order | rotate | weight | usage); same
// lenient default as the Rust settings decode.
let routingMode: string = 'order';
/** /v1/responses chain-of-thought prompt toggle (Codex surface). */
let responsesCotEnabled = false;

const proxyProviders: ProxyProvider[] = [
  {
    id: 'anthropic-primary',
    name: 'Anthropic official',
    icon: 'spark',
    iconColor: '#d97757',
    type: 'anthropic',
    apiKey: 'sk-ant-demo',
    baseURL: 'https://api.anthropic.com',
    models: ['claude-fable-5', 'claude-opus-4-8', 'claude-sonnet-5'],
    enabled: true,
    timeoutMs: 30_000,
    weight: 1,
    circuitBreaker: { failureThreshold: 3, cooldownMs: 5_000, halfOpenRetries: 1, successThreshold: 2 },
    modelMapping: {},
    extraHeaders: {},
    autoTrackModels: true,
  },
  {
    id: 'openai-fallback',
    name: 'OpenAI fallback',
    icon: 'knot',
    iconColor: '#10a37f',
    type: 'openai',
    apiKey: 'sk-oai-demo',
    baseURL: 'https://api.openai.com',
    models: ['gpt-5', 'gpt-4o'],
    enabled: true,
    timeoutMs: 30_000,
    weight: 1,
    circuitBreaker: { failureThreshold: 5, cooldownMs: 10_000, halfOpenRetries: 1, successThreshold: 2 },
    modelMapping: { 'claude-sonnet-5': 'gpt-5' },
    extraHeaders: {},
    autoTrackModels: true,
  },
  // Linked upstreams (ccs-<presetId> convention) so the unified provider
  // cards demo the preset ↔ upstream pairing. claude-2 is deliberately
  // preset-only: its base URL is AAG itself (self-loop, no upstream row).
  {
    id: 'ccs-claude-1',
    name: 'Claude Sonnet 5',
    icon: 'spark',
    iconColor: '#d97757',
    type: 'anthropic',
    apiKey: 'sk-ant-demo',
    baseURL: 'https://api.anthropic.com',
    models: ['claude-fable-5', 'claude-opus-4-8', 'claude-sonnet-5'],
    enabled: true,
    timeoutMs: 60_000,
    weight: 1,
    circuitBreaker: { failureThreshold: 3, cooldownMs: 5_000, halfOpenRetries: 1, successThreshold: 2 },
    modelMapping: {},
    extraHeaders: {},
    autoTrackModels: true,
  },
  {
    id: 'ccs-codex-oai',
    name: 'OpenAI GPT-5',
    icon: 'knot',
    iconColor: '#10a37f',
    type: 'openai',
    apiKey: 'sk-demo',
    baseURL: 'https://api.openai.com',
    models: ['gpt-5', 'gpt-4o'],
    enabled: true,
    timeoutMs: 60_000,
    weight: 1,
    circuitBreaker: { failureThreshold: 3, cooldownMs: 5_000, halfOpenRetries: 1, successThreshold: 2 },
    modelMapping: {},
    extraHeaders: {},
    autoTrackModels: true,
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

let diagnosticsDebugExpiresAt: number | null = null;

interface Demo_timeline_row {
  requestId: string;
  surface: string;
  modelRef: string;
  timestampMs: number;
  status: number;
  durationMs: number;
  streamed: boolean;
  failedOver: boolean | null;
  planKind: string | null;
  planTier: number | null;
  planDowngraded: boolean | null;
  planBasis: string | null;
  planReason: string | null;
  configGeneration: number | null;
}

/** Synthetic request-timeline rows for the browser demo (diagnostics modal +
 *  supplier downgrade-trace panel). In-memory only: no real requests, model
 *  names, or hosts. Cleared by `diagnostics_clear_request_timeline`. */
function demo_timeline_fixture(): Demo_timeline_row[] {
  return [
    {
      requestId: 'demo_req_1',
      surface: 'chat_completions',
      modelRef: 'claude-opus-4-8',
      timestampMs: Date.now() - 60_000,
      status: 200,
      durationMs: 812,
      streamed: true,
      failedOver: null,
      planKind: 'precise',
      planTier: 2,
      planDowngraded: false,
      planBasis: 'manual_override',
      planReason: 'precise_same_model',
      configGeneration: 4,
    },
    {
      requestId: 'demo_req_2',
      surface: 'messages',
      modelRef: 'claude-sonnet-5',
      timestampMs: Date.now() - 30_000,
      status: 200,
      durationMs: 1450,
      streamed: true,
      // Demo trace of a visible downgrade: the alias asked for a stronger tier
      // and the request completed on a lower one after failover.
      failedOver: true,
      planKind: 'alias',
      planTier: 3,
      planDowngraded: true,
      planBasis: 'family_role',
      planReason: 'alias_tier_downgrade',
      configGeneration: 4,
    },
    {
      requestId: 'demo_req_3',
      surface: 'responses',
      modelRef: 'gpt-5',
      timestampMs: Date.now() - 10_000,
      status: 200,
      durationMs: 640,
      streamed: false,
      failedOver: null,
      planKind: null,
      planTier: null,
      planDowngraded: null,
      planBasis: null,
      planReason: null,
      configGeneration: 4,
    },
  ];
}

const demo_request_timeline: Demo_timeline_row[] = demo_timeline_fixture();

/** Re-install the synthetic timeline fixture (window hook `__aagDemoSeedRequestTimeline`).
 *
 *  The demo has no live traffic: once a scenario exercises
 *  `diagnostics_clear_request_timeline` the timeline would stay empty forever.
 *  Smoke scenarios use this hook to re-seed labelled `demo_req_*` fixture rows
 *  before asserting timeline rendering. It is fixture setup, not evidence of
 *  real requests. */
function seed_demo_request_timeline(): void {
  demo_request_timeline.length = 0;
  demo_request_timeline.push(...demo_timeline_fixture());
}

function diagnosticsState() {
  if (diagnosticsDebugExpiresAt && diagnosticsDebugExpiresAt <= Math.floor(Date.now() / 1000)) {
    diagnosticsDebugExpiresAt = null;
  }
  return {
    debug_enabled: diagnosticsDebugExpiresAt !== null,
    debug_expires_at: diagnosticsDebugExpiresAt,
    log_directory: 'C:\\Demo\\AI Agent Gateway\\logs',
    max_file_size_bytes: 2 * 1024 * 1024,
    retained_file_count: 5,
  };
}

// ---------------------------------------------------------------------------
// Forced-error debug hook
// ---------------------------------------------------------------------------

let forcedError: string | null = null;
const forcedErrorCodes: Readonly<Record<string, string>> = Object.freeze({
  dsh_discover_runtime: 'demo_forced_dsh_discover_runtime',
  dsh_execute_operation: 'demo_forced_dsh_execute_operation',
  dsh_get_snapshot: 'demo_forced_dsh_get_snapshot',
  dsh_market_refresh: 'demo_forced_dsh_market_refresh',
  mcp_list_servers: 'demo_forced_mcp_list_servers',
  workflow_list_capabilities: 'demo_forced_workflow_list_capabilities',
  provider_list: 'demo_forced_provider_list',
  provider_config_status: 'demo_forced_provider_config_status',
  provider_switch: 'demo_forced_provider_switch',
  claude_open_cli_terminal: 'demo_forced_claude_open_cli_terminal',
  proxy_status: 'demo_forced_proxy_status',
  supplier_list: 'demo_forced_supplier_list',
  supplier_save: 'demo_forced_supplier_save',
  supplier_import_preview: 'demo_forced_supplier_import_preview',
});
function setMockError(cmd: string | null) {
  forcedError = cmd;
}
function fail(cmd: string): never {
  throw new Error(forcedErrorCodes[cmd] ?? 'demo_forced_error');
}

// ---------------------------------------------------------------------------
// Mock handler
// ---------------------------------------------------------------------------

type Args = Record<string, unknown>;

export function installMockBackend() {
  mockIPC(async (cmd: string, payload: InvokeArgs = {}) => {
    if (forcedError && forcedError === cmd) fail(cmd);
    const p = payload as Args;
    if (cmd.startsWith('provider_screening_')) return mock_provider_screening_command(cmd, p, settings.get('provider_screening_policy'));
    if (cmd.startsWith('tailnet_discovery_')) return mock_tailnet_discovery_command(cmd, p, proxyConfig.port);
    if (cmd.startsWith('prompt_workspace_')) return mock_prompt_workspace_command(cmd, p);
    if (cmd.startsWith('prompt_enhance_')) return mock_prompt_command(cmd, p);
    if (cmd.startsWith('workflow_')) return mock_workflow_command(cmd, p);
    if (cmd.startsWith('mcp_')) return mock_mcp_command(cmd, p);
    if (cmd.startsWith('dsh_market_') || cmd === 'dsh_open_marketplace') return mock_dsh_market_command(cmd);
    if (cmd.startsWith('dsh_')) return mock_dsh_command(cmd, p);
    if (cmd === 'supplier_arena_snapshot') return mock_arena_snapshot(p);
    if (cmd.startsWith('supplier_') || cmd === 'model_catalog_model') return mock_supplier_command(cmd, p);
    if (cmd === 'usage_report' || cmd === 'usage_prune') return mock_usage_command(cmd, p);

    switch (cmd) {
      // ---- app ----
      case 'app_info':
        return { name: 'AI Agent Gateway', version: app_version, phase: '16' };
      case 'app_check_update':
        return null;
      case 'diagnostics_request_timeline':
        return structuredClone(demo_request_timeline);
      case 'diagnostics_clear_request_timeline':
        // Demo-only breadcrumb so smoke investigations can see who cleared
        // the synthetic timeline instead of guessing.
        (window as unknown as { __aagTimelineClearStack?: string }).__aagTimelineClearStack =
          String(new Error('demo timeline cleared').stack ?? '');
        demo_request_timeline.length = 0;
        return null;
      case 'diagnostics_get':
        return diagnosticsState();
      case 'diagnostics_enable_debug':
        diagnosticsDebugExpiresAt = Math.floor(Date.now() / 1000) + 30 * 60;
        return diagnosticsState();
      case 'diagnostics_disable_debug':
        diagnosticsDebugExpiresAt = null;
        return diagnosticsState();
      case 'diagnostics_open_log_dir':
      case 'diagnostics_record_frontend_error':
        return null;
      case 'diagnostics_get_summary':
        return 'AI Agent Gateway demo\nplatform=browser\ndebug_enabled=' +
          String(diagnosticsState().debug_enabled);

      // ---- providers ----
      // List/get commands return a fresh clone per invoke — real Tauri IPC
      // serializes a new array on every call. Returning the stored instance
      // would let in-place mutations (provider_save etc.) collide with
      // React's identical-reference setState bailout: refresh() would set
      // the exact array already in state and the UI would never re-render.
      case 'provider_list':
        return structuredClone(providers.get(String(p.appType)) ?? []);
      case 'provider_current':
        return currentByApp.get(String(p.appType)) ?? null;
      case 'provider_config_status':
        return demo_provider_config_status(String(p.appType));
      case 'client_config_read':
        return demo_client_config_read(String(p.appType));
      case 'client_config_save':
        return demo_client_config_save(String(p.appType), String(p.contents));
      case 'provider_get': {
        const list = providers.get(String(p.appType)) ?? [];
        return structuredClone(list.find((x) => x.id === p.id) ?? null);
      }
      case 'provider_save': {
        const appType = p.appType as string;
        // Clone at the intake boundary: real IPC serializes arguments, so the
        // backend must never share (and later mutate) a caller's live state
        // object. Outputs already clone — see provider_list above.
        const provider = structuredClone(p.provider as Provider);
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
          throw new Error('AAG_PROVIDER_NOT_FOUND');
        }
        const status = demo_provider_config_status(appType);
        if (!status.supported) throw new Error('provider_config_unsupported');
        const written = status.configPaths;
        if (appType === 'claude_code') settings.set('claude_mirror_written', '1');
        if (written.length > 0) demo_written_config.add(appType);
        currentByApp.set(appType, id);
        return { app_type: appType, provider_id: id, written, backup: null };
      }
      case 'provider_set_failover_queue': {
        const appType = String(p.appType);
        const id = String(p.id);
        const inQueue = Boolean(p.inQueue);
        const list = providers.get(appType) ?? [];
        const target = list.find((x) => x.id === id);
        if (!target) throw new Error('AAG_PROVIDER_NOT_FOUND');
        target.inFailoverQueue = inQueue;
        return null;
      }
      case 'provider_reorder': {
        const appType = String(p.appType);
        const orderedIds = (p.orderedIds as string[]) ?? [];
        const list = providers.get(appType) ?? [];
        const byId = new Map(list.map((x) => [x.id, x]));
        const reordered: Provider[] = [];
        const seen_ids = new Set<string>();
        for (const id of orderedIds) {
          if (seen_ids.has(id)) {
            throw new Error('AAG_PROVIDER_REORDER_DUPLICATE_ID');
          }
          seen_ids.add(id);
          const row = byId.get(id);
          if (!row) throw new Error('AAG_PROVIDER_REORDER_UNKNOWN_ID');
          reordered.push(row);
          byId.delete(id);
        }
        providers.set(appType, [...reordered, ...byId.values()]);
        return null;
      }

      // ---- import_cc_switch_providers ----
      // Demo mode can't open the user's cc-switch SQLite DB. Return a
      // small synthetic payload so the import button showcases the Phase 22
      // report toast (imported + skipped counts + auto-seeded routes +
      // auto-queued + passthrough + restart suffixes) — empty reports in the
      // UI make the feature look broken.
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
          importedRoutes: [
            { alias: 'claude-fable-5', provider: 'ccs-demo-anthropic', model: 'demo-fable' },
            { alias: 'claude-sonnet-5', provider: 'ccs-demo-anthropic', model: 'demo-sonnet' },
          ],
          queuedProviders: [
            {
              id: 'ccs-demo-anthropic',
              appType: 'claude_code',
              name: 'cc-switch · Anthropic demo',
            },
          ],
          passthroughProviders: [],
        };

      // ---- proxy ----
      case 'proxy_request_status':
        return [];
      case 'codex_reasoning_models':
        return [{ model: 'codex-5-6-sol', supportedEfforts: ['low', 'medium', 'high', 'xhigh', 'max'], isDefault: true }];
      case 'proxy_status':
        return {
          running: proxyRunning, config: structuredClone(proxyConfig),
          config_changed: proxyRunning && running_routing_signature !== routing_signature(),
        };
      case 'proxy_start':
        proxyRunning = true;
        running_routing_signature = routing_signature();
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
      // Fresh clone per call — see the provider_list note above.
      case 'proxy_provider_list':
        return structuredClone(proxyProviders);
      case 'proxy_provider_save': {
        // Intake clone: `proxy_set_auto_track` mutates stored rows in place,
        // which must never reach a caller's live object (real IPC serializes).
        const provider = structuredClone(p.provider as ProxyProvider);
        if (Number(provider.timeoutMs) < 1000) {
          throw new Error('AAG_PROXY_PROVIDER_TIMEOUT_INVALID');
        }
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
      // Demo supplier saves never materialize projection rows (the mock
      // supplier store is synthetic), so no upstream is supplier-managed.
      case 'proxy_provider_supplier_mapped':
        return [];
      // Demo mock: minimal-chat connectivity probe returns a canned success
      // so the per-card test button is exercisable without a real upstream.
      case 'proxy_test_provider':
        return {
          success: true,
          latencyMs: 214,
          model: 'claude-sonnet-5',
          reply: 'AAG_TEST_OK (demo)',
          providerId: String(p.providerId ?? ''),
        };
      case 'proxy_get_pinned_provider':
        return pinnedProvider;
      case 'proxy_set_pinned_provider': {
        pinnedProvider = p.providerId ? String(p.providerId) : null;
        return null;
      }
      case 'proxy_get_routing_mode':
        return routingMode;
      case 'proxy_set_routing_mode': {
        const mode = String(p.mode ?? '');
        if (
          mode !== 'order' &&
          mode !== 'rotate' &&
          mode !== 'weight' &&
          mode !== 'usage'
        ) {
          throw new Error('routing_mode_invalid');
        }
        routingMode = mode;
        return null;
      }
      case 'proxy_get_responses_cot':
        return responsesCotEnabled;
      case 'proxy_set_responses_cot': {
        responsesCotEnabled = Boolean(p.enabled);
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
      // Demo mock: model auto-discovery. No real upstream exists in the
      // demo, so a manual refresh reports an honest no-op pass and the
      // auto-track toggle flips the synthetic row only.
      case 'proxy_refresh_models': {
        const onlyId = p.providerId ? String(p.providerId) : null;
        const targets = onlyId
          ? proxyProviders.filter((pp) => pp.id === onlyId)
          : proxyProviders.filter(
              (pp) =>
                pp.autoTrackModels &&
                pp.type !== 'mock' &&
                pp.type !== 'anthropic_passthrough',
            );
        return {
          ranAt: new Date().toISOString(),
          trigger: 'manual',
          hasChanges: false,
          deduplicated: false,
          providers: targets.map((pp) => ({
            providerId: pp.id,
            ok: true,
            addedModels: [],
            hopRewrites: [],
            mappingRewrites: [],
            skippedAmbiguous: [],
          })),
        };
      }
      case 'model_discovery_status':
        return { report: null, pendingRestart: false, intervalHours: 24, lastRunEpoch: 0 };
      case 'proxy_set_auto_track': {
        const trackId = String(p.providerId);
        const trackRow = proxyProviders.find((pp) => pp.id === trackId);
        if (!trackRow) throw new Error(`proxy provider "${trackId}" not found`);
        trackRow.autoTrackModels = Boolean(p.enabled);
        return null;
      }
      case 'route_list':
        return structuredClone(routes);
      case 'route_save': {
        // Intake clone: `route_rename` mutates the stored row in place, which
        // must never reach a caller's live object (real IPC serializes).
        const r = structuredClone(p.route as Route);
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
          throw new Error('AAG_ROUTE_ALIAS_CONFLICT');
        }
        const r = routes.find((x) => x.alias === from);
        if (!r) throw new Error('AAG_ROUTE_NOT_FOUND');
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
            (providers.get('codex') ?? []).find((x) => x.id === currentByApp.get('codex'))?.name ??
            null,
          mirrorMode: settings.get('codex_mirror_mode') || 'exclusive',
          desktopSupported: true,
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
      case 'codex_open_desktop_window': {
        const mirror_mode_changed = settings.get('codex_mirror_mode') === 'coexist';
        settings.set('codex_mirror_mode', 'exclusive');
        return {
          codexHome: '~/.codex-cli',
          codexBinary: 'C:\\Program Files\\WindowsApps\\OpenAI.Codex_*\\app\\ChatGPT.exe',
          mirror: true,
          store: true,
          mirrorMode: 'exclusive',
          mirrorModeChanged: mirror_mode_changed,
        };
      }
      case 'codex_mirror_set_mode': {
        const mode = String(p.mode ?? '');
        if (mode !== 'exclusive' && mode !== 'coexist') {
          throw new Error('codex_mirror_mode_invalid');
        }
        settings.set('codex_mirror_mode', mode);
        return {
          mode,
          written: (providers.get('codex') ?? []).length > 0 ? ['<mirror>/config.toml'] : [],
        };
      }

      // ---- claude mirror (CLAUDE_CONFIG_DIR) ----
      case 'claude_mirror_status':
        return {
          mirrorDir: '~/.claude-cli',
          initialized: settings.get('claude_mirror_written') === '1',
          baseUrlWritten: settings.get('claude_mirror_written') === '1',
          authTokenWritten:
            settings.get('claude_mirror_written') === '1' &&
            settings.get('claude_mirror_mode') !== 'passthrough',
          mode: settings.get('claude_mirror_mode') || 'token',
          claudeBinary: 'C:\\Users\\demo\\AppData\\Local\\AnthropicClaude\\claude.exe',
          desktopSupported: true,
        };
      case 'claude_mirror_set_mode': {
        const mode = String(p.mode ?? '');
        if (mode !== 'token' && mode !== 'passthrough') {
          throw new Error('claude_mirror_mode_invalid');
        }
        settings.set('claude_mirror_mode', mode);
        return {
          mode,
          written: settings.get('claude_mirror_written') === '1' ? ['<mirror>/settings.json'] : [],
        };
      }
      case 'claude_open_desktop':
        settings.set('claude_mirror_written', '1');
        return {
          claudeHome: '~/.claude-cli',
          claudeBinary: 'C:\\Users\\demo\\AppData\\Local\\AnthropicClaude\\claude.exe',
          mirror: true,
        };
      case 'claude_open_cli_terminal':
        settings.set('claude_mirror_written', '1');
        return {
          claudeHome: '~/.claude-cli',
          claudeBinary: 'claude',
          env: [
            ['CLAUDE_CONFIG_DIR', '<redacted>'],
            ['ANTHROPIC_BASE_URL', '<redacted>'],
            ['ANTHROPIC_AUTH_TOKEN', '<redacted>'],
          ],
        };

      // ---- copilot cli ----
      case 'managed_cli_status': {
        const managed_app = String(p.appType);
        if (managed_app === 'kilo_cli') return { appType: managed_app, available: true, binary: 'kilo', installUrl: 'https://kilo.ai/docs/code-with-ai/platforms/cli' };
        if (managed_app === 'cline_cli') return { appType: managed_app, available: true, binary: 'cline', installUrl: 'https://github.com/cline/cline/tree/main/apps/cli' };
        throw new Error(`unsupported managed CLI: ${managed_app}`);
      }
      case 'managed_cli_open_terminal': {
        const managed_app = String(p.appType);
        if (managed_app !== 'kilo_cli' && managed_app !== 'cline_cli') throw new Error(`unsupported managed CLI: ${managed_app}`);
        return { appType: managed_app, binary: managed_app === 'kilo_cli' ? 'kilo' : 'cline', env: [['AAG_DEMO', 'synthetic']] };
      }

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
      // ---- gemini cli ----
      case 'gemini_open_cli_terminal':
        return {
          geminiBinary: 'gemini.cmd',
          providerId: currentByApp.get('gemini') ?? 'gemini-claude',
          env: [
            ['NO_PROXY', 'localhost,127.0.0.1,::1'],
          ],
        };

      // ---- vscode launcher (claude_vscode / kilo_vscode) ----
      case 'vscode_open_with_extension': {
        const vscode_app = String(p.appType);
        if (vscode_app !== 'claude_vscode' && vscode_app !== 'kilo_vscode') {
          throw new Error(`unsupported vscode app: ${vscode_app}`);
        }
        return {
          binary: 'code.exe',
          appType: vscode_app,
          baseUrl: demo_proxy_base(),
          authTokenSet: proxyConfig.authToken !== '',
          // Same contract as the native launcher: names are retained, values
          // are redacted.
          env:
            vscode_app === 'kilo_vscode'
              ? [
                  ['KILO_CONFIG', '<redacted>'],
                  ['KILO_CONFIG_CONTENT', '<redacted>'],
                ]
              : [
                  ['ANTHROPIC_BASE_URL', '<redacted>'],
                  ['ANTHROPIC_AUTH_TOKEN', '<redacted>'],
                ],
        };
      }
      case 'vscode_extension_status':
        return {
          binary: 'code.exe',
          extensionsDir: '~/.vscode-aag/extensions',
          claudeCodeInstalled: true,
          kiloCodeInstalled: false,
        };

      default:
        console.warn('[aag-demo] unsupported_command');
        throw new Error('demo_command_unsupported');
    }
  });
}

// ---------------------------------------------------------------------------
// Bootstrap
// ---------------------------------------------------------------------------

function demo_proxy_base() {
  // Same endpoint rule as the app (`proxy_client_url`), so the interceptor
  // matches whatever the UI actually dials for this configuration.
  return proxy_client_url(proxyConfig);
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
          runtime_kind: 'desktop',
          availableModels: [...new Set(routes.filter((route) => route.hops.some((hop) => proxyProviders.some((provider) => provider.id === hop.provider && provider.enabled))).map((route) => route.alias))],
          providers: proxyProviders.map((p) => ({
            id: p.id,
            enabled: p.enabled,
            breaker: { state: 'closed', failures: 0, successes: 0, cooldownMs: p.circuitBreaker.cooldownMs },
          })),
          models: [...new Set(routes.map((r) => r.alias))],
          lastUsed: null,
          pinnedProvider,
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
    __aagDemoSeedRequestTimeline: seed_demo_request_timeline,
    __aagDemoIsDemo: true,
  });
  console.info(
    '[aag-demo] running with an in-browser mock backend (no Tauri). ' +
      'Debug: window.__aagDemoSetError(cmd|null)',
  );
}
