/** Synthetic DSH state is reachable only through the browser demo dispatcher. */
import type {
  Dsh_operation_request, Dsh_plugin, Dsh_preview, Dsh_receipt, Dsh_settings,
  Dsh_snapshot, Dsh_upstream, Dsh_discovery, Dsh_runtime_candidate, Dsh_setup_plan, Dsh_setup_request,
} from '../api/dsh.js';

const demo_settings: Dsh_settings = {
  node_path: 'C:\\Demo\\node.exe', dsh_entry_path: 'C:\\Demo\\dsh\\cli.js', pnpm_entry_path: 'C:\\Demo\\pnpm.cjs',
  port: 3141, startup_timeout_secs: 60, operation_timeout_secs: 180,
  theme: 'system', upstream_id: null, model_id: null,
};

const demo_catalog: Dsh_plugin[] = [
  { plugin_id: 'base', package_name: '@deepseek-ai/dsh-base', display_name: 'DSH base', category: 'core',
    description: 'Official core composition, including tools and filesystem Skills.', source_url: 'https://github.com/deepseek-ai/deepseek-harness', essential: true },
  { plugin_id: 'web_app', package_name: '@deepseek-ai/dsh-web-app', display_name: 'DSH WebUI', category: 'desktop',
    description: 'Official web application opened in a restricted desktop window.', source_url: 'https://github.com/deepseek-ai/deepseek-harness', essential: true },
  { plugin_id: 'skins', package_name: '@dsh-external/dsh-web-skins', display_name: 'Appearance skins', category: 'personalization',
    description: 'Community appearance module. The inspected version has a legacy client declaration and is not an enableable bundle.', source_url: 'https://github.com/Moeblack/dsh-skins', essential: false },
  { plugin_id: 'cost_meter', package_name: '@steven-wu/dsh-cost-meter', display_name: 'Cost meter', category: 'practical',
    description: 'Community estimated usage display. Estimates are not billing evidence.', source_url: 'https://github.com/Sttrevens/dsh-cost-meter', essential: false },
  { plugin_id: 'mcp_client', package_name: '@deepseek-ai/dsh-mcp-client', display_name: 'MCP client', category: 'practical',
    description: 'Official MCP tool adapter. Installing a plain module does not configure a server or load a bundle.', source_url: 'https://github.com/deepseek-ai/deepseek-harness', essential: false },
].map((plugin) => ({ ...plugin, installed_version: null, enabled: false, runtime_phase: 'unknown' }));

const demo_upstreams: Dsh_upstream[] = [
  { id: 'claude:demo_native', name: 'Native Claude (synthetic)', protocol: 'anthropic-messages',
    models: ['claude-sonnet-4-6', 'claude-opus-4-6'], source_kind: 'claude_native', available: true },
  { id: 'proxy:demo_pool', name: 'AAG provider (synthetic)', protocol: 'openai-completions',
    models: ['deepseek-chat'], source_kind: 'proxy_pool', available: true },
  { id: 'gateway', name: 'AAG local gateway (synthetic)', protocol: 'openai-completions',
    models: ['claude-sonnet-4-6', 'deepseek-chat'], source_kind: 'gateway', available: true },
  { id: 'proxy:demo_unsupported', name: 'Unsupported provider (synthetic)', protocol: 'openai-responses',
    models: [], source_kind: 'proxy_pool', available: false, reason: 'DSH_PROTOCOL_UNSUPPORTED' },
];

let snapshot: Dsh_snapshot = {
  settings: { ...demo_settings }, settings_digest: 'demo_settings_0', initialized: false,
  managed_home: 'C:\\Demo\\AAG\\dsh', plugins: demo_catalog, runtime: { phase: 'stopped', pid: null, web_url: null, error_code: null },
  job: null, receipts: [],
};
let settings_revision = 0;
let state_revision = 0;
let operation_sequence = 0;
let cancellation_requested = false;
const setup_plans = new Map<string, { plan: Dsh_setup_plan; revision: number }>();
const previews = new Map<string, { preview: Dsh_preview; request: Dsh_operation_request; revision: number }>();
const copy = <T,>(value: T): T => structuredClone(value);

function assert_idle() {
  if (snapshot.job) throw new Error('DSH_OPERATION_BUSY');
  if (snapshot.runtime.phase === 'web_ready' || snapshot.runtime.phase === 'starting') throw new Error('DSH_RUNTIME_RUNNING');
}

function validate_settings(settings: Dsh_settings) {
  if (!Number.isInteger(settings.port) || settings.port < 1024 || settings.port > 65535) throw new Error('DSH_INVALID_PORT');
  if (!['system', 'light', 'dark'].includes(settings.theme)) throw new Error('DSH_INVALID_THEME');
  for (const key of ['startup_timeout_secs', 'operation_timeout_secs'] as const) {
    if (!Number.isInteger(settings[key]) || settings[key] < 5 || settings[key] > (key === 'startup_timeout_secs' ? 180 : 600)) throw new Error('DSH_INVALID_TIMEOUT');
  }
  for (const key of ['node_path', 'dsh_entry_path', 'pnpm_entry_path'] as const) {
    if (settings[key] && !/^(?:[A-Za-z]:[\\/]|\/)/.test(settings[key])) throw new Error('DSH_PATH_MUST_BE_ABSOLUTE');
  }
  if (settings.upstream_id && !demo_upstreams.some((upstream) => upstream.id === settings.upstream_id && upstream.available)) throw new Error('DSH_UPSTREAM_UNAVAILABLE');
  if (settings.upstream_id && !settings.model_id?.trim()) throw new Error('DSH_MODEL_REQUIRED');
}

function operation_plugin(request: Dsh_operation_request): Dsh_plugin {
  const plugin = snapshot.plugins.find((entry) => entry.package_name === request.package_name);
  if (!plugin) throw new Error('DSH_PACKAGE_NOT_ALLOWLISTED');
  return plugin;
}

function validate_request(request: Dsh_operation_request) {
  assert_idle();
  if (request.action === 'initialize') {
    if (snapshot.initialized) throw new Error('DSH_ALREADY_INITIALIZED');
    return;
  }
  if (!snapshot.initialized) throw new Error('DSH_NOT_INITIALIZED');
  const plugin = operation_plugin(request);
  if (request.action === 'install') {
    if (!request.version || !/^\d+\.\d+\.\d+(?:-[0-9A-Za-z]+(?:[.-][0-9A-Za-z]+)*)?(?:\+[0-9A-Za-z]+(?:[.-][0-9A-Za-z]+)*)?$/.test(request.version)) throw new Error('DSH_EXACT_VERSION_REQUIRED');
    if (!snapshot.settings.node_path || !snapshot.settings.pnpm_entry_path) throw new Error('DSH_RUNTIME_PATH_REQUIRED');
  } else {
    if (!plugin.installed_version) throw new Error('DSH_PACKAGE_NOT_INSTALLED');
    if (plugin.essential && ['remove', 'disable'].includes(request.action)) throw new Error('DSH_ESSENTIAL_PLUGIN');
    if (request.action === 'enable' && ['skins', 'mcp_client'].includes(plugin.plugin_id)) throw new Error('DSH_BUNDLE_PATCH_REQUIRED');
  }
}

function demo_discovery(): Dsh_discovery {
  const candidates: Dsh_runtime_candidate[] = [];
  const recommended = { node_path: '', dsh_entry_path: '', pnpm_entry_path: '' };
  for (const [kind, key, version] of [
    ['node', 'node_path', null], ['dsh', 'dsh_entry_path', '0.1.1-rc.2'], ['pnpm', 'pnpm_entry_path', '10.34.5'],
  ] as const) {
    const canonical_path = demo_settings[key];
    const configured_path = snapshot.settings[key];
    if (configured_path && configured_path !== canonical_path) candidates.push({ candidate_id: `demo_${kind}_configured`, kind, path: configured_path,
      source: 'configured', version: null, compatible: false, validation: 'missing', reason: 'runtime_file_missing' });
    candidates.push({ candidate_id: `demo_${kind}`, kind, path: canonical_path, source: configured_path === canonical_path ? 'configured' : 'user_install',
      version, compatible: true, validation: kind === 'node' ? 'binary_format_only' : 'manifest_verified', reason: null });
    recommended[key] = canonical_path;
  }
  return { candidates, recommended, issues: ['node_version_unverified'], scanned_locations: 6, scan_limited: false };
}

function demo_setup_plan(request: Dsh_setup_request): Dsh_setup_plan {
  assert_idle();
  if (request.expected_digest !== snapshot.settings_digest) throw new Error('DSH_SETTINGS_CHANGED');
  const settings = copy(snapshot.settings);
  const discovery = demo_discovery();
  const changes: string[] = [];
  const blockers: string[] = [];
  const warnings = ['managed_scope_only', 'explicit_start_required', 'node_version_unverified'];
  for (const [kind, key] of [['node', 'node_path'], ['dsh', 'dsh_entry_path'], ['pnpm', 'pnpm_entry_path']] as const) {
    if (!settings[key]) { settings[key] = discovery.recommended[key]; if (settings[key]) changes.push(`fill_${key}`); }
    if (!settings[key]) { if (kind === 'pnpm') warnings.push('pnpm_not_found'); else blockers.push(`runtime_${kind}_required`); }
    else if (!discovery.candidates.some((candidate) => candidate.kind === kind && candidate.path === settings[key] && candidate.compatible)) blockers.push(`runtime_${kind}_invalid`);
  }
  if (settings.upstream_id && request.upstream_id && settings.upstream_id !== request.upstream_id) blockers.push('explicit_selection_conflict');
  if (settings.model_id && request.model_id && settings.model_id !== request.model_id) blockers.push('explicit_selection_conflict');
  let source_reason = 'preserved';
  if (!settings.upstream_id) {
    settings.upstream_id = request.upstream_id || 'claude:demo_native';
    source_reason = request.upstream_id ? 'explicit' : 'current_claude';
    changes.push('select_upstream');
  }
  const source = demo_upstreams.find((upstream) => upstream.id === settings.upstream_id && upstream.available);
  if (!source) blockers.push('upstream_unavailable');
  else {
    if (!settings.model_id) {
      settings.model_id = request.model_id || (source_reason === 'current_claude' ? 'claude-sonnet-4-6' : source.models.length === 1 ? source.models[0] : null);
      if (settings.model_id) changes.push('select_model'); else blockers.push('model_selection_required');
    }
    if (settings.model_id && !source.models.includes(settings.model_id)) blockers.push('model_unavailable');
  }
  if (!snapshot.initialized) changes.push('initialize_profile');
  const plan: Dsh_setup_plan = { plan_id: `demo_setup_${++operation_sequence}`, expected_digest: request.expected_digest, expires_at_ms: Date.now() + 300_000,
    settings, changes, warnings, blockers: [...new Set(blockers)], source_reason, can_apply: blockers.length === 0 };
  setup_plans.set(plan.plan_id, { plan: copy(plan), revision: state_revision });
  return plan;
}

export async function mock_dsh_command(command: string, payload: Record<string, unknown>): Promise<unknown> {
  switch (command) {
    case 'dsh_get_snapshot': return copy(snapshot);
    case 'dsh_discover_runtime': return copy(demo_discovery());
    case 'dsh_preview_setup': return copy(demo_setup_plan(payload.request as Dsh_setup_request));
    case 'dsh_apply_setup': {
      assert_idle();
      const pending = setup_plans.get(String(payload.plan_id));
      setup_plans.delete(String(payload.plan_id));
      if (!pending) throw new Error('DSH_SETUP_PLAN_NOT_FOUND');
      if (pending.plan.expires_at_ms <= Date.now()) throw new Error('DSH_SETUP_PLAN_EXPIRED');
      if (!pending.plan.can_apply) throw new Error('DSH_SETUP_PLAN_BLOCKED');
      if (pending.revision !== state_revision || pending.plan.expected_digest !== snapshot.settings_digest) throw new Error('DSH_SETTINGS_CHANGED');
      validate_settings(pending.plan.settings);
      if (pending.plan.changes.length) {
        snapshot.settings = copy(pending.plan.settings);
        snapshot.settings_digest = `demo_settings_${++settings_revision}`;
        if (pending.plan.changes.includes('initialize_profile')) {
          snapshot.initialized = true;
          snapshot.plugins.filter((plugin) => plugin.essential).forEach((plugin) => { plugin.enabled = true; });
        }
        state_revision += 1;
      }
      previews.clear(); setup_plans.clear();
      return copy(snapshot);
    }
    case 'dsh_list_upstreams': return copy(demo_upstreams);
    case 'dsh_save_settings': {
      assert_idle();
      if (payload.expected_digest !== snapshot.settings_digest) throw new Error('DSH_SETTINGS_CHANGED');
      const settings = copy(payload.settings as Dsh_settings);
      validate_settings(settings);
      snapshot.settings = settings;
      snapshot.settings_digest = `demo_settings_${++settings_revision}`;
      state_revision += 1;
      return copy(snapshot);
    }
    case 'dsh_preview_operation': {
      const request = copy(payload.request as Dsh_operation_request);
      validate_request(request);
      const preview: Dsh_preview = {
        preview_id: `demo_preview_${++operation_sequence}`, action: request.action, package_name: request.package_name, version: request.version,
        changes: request.action === 'initialize' ? ['Create an isolated DSH home and managed workspace (synthetic demo).']
          : [`${request.action}: ${request.package_name}${request.version ? `@${request.version}` : ''} in the managed profile (synthetic demo).`, 'Record a separate operation receipt.'],
        warnings: ['Browser demo only: no file, package, process, or network changes are made.',
          'In native mode, third-party package code runs with local user permissions when enabled. Installation is not a trust certification.',
          'Installed, enabled, and runtime-loaded states are tracked separately.'],
        expires_at_ms: Date.now() + 120_000,
      };
      previews.set(preview.preview_id, { preview, request, revision: state_revision });
      return copy(preview);
    }
    case 'dsh_execute_operation': {
      const approved = previews.get(String(payload.preview_id));
      previews.delete(String(payload.preview_id));
      if (!approved || approved.preview.expires_at_ms <= Date.now()) throw new Error('DSH_PREVIEW_EXPIRED');
      if (approved.revision !== state_revision) throw new Error('DSH_PREVIEW_STALE');
      validate_request(approved.request);
      const { request } = approved;
      const operation_id = `demo_operation_${++operation_sequence}`;
      const started_at_ms = Date.now();
      cancellation_requested = false;
      snapshot.job = { operation_id, action: request.action, package_name: request.package_name, phase: 'running' };
      await new Promise((resolve) => window.setTimeout(resolve, request.action === 'install' ? 1400 : 450));
      const receipt: Dsh_receipt = {
        operation_id, action: request.action, package_name: request.package_name, version: request.version,
        status: cancellation_requested ? 'cancelled' : 'succeeded', error_code: cancellation_requested ? 'DSH_CANCELLED' : null,
        started_at_ms, finished_at_ms: Date.now(), rollback_status: cancellation_requested ? 'restored' : 'not_needed', restart_required: !cancellation_requested,
      };
      if (!cancellation_requested) {
        if (request.action === 'initialize') {
          snapshot.initialized = true;
          snapshot.plugins.filter((plugin) => plugin.essential).forEach((plugin) => { plugin.enabled = true; });
        } else {
          const plugin = operation_plugin(request);
          if (request.action === 'install') plugin.installed_version = request.version;
          if (request.action === 'remove') { plugin.installed_version = null; plugin.enabled = false; }
          if (request.action === 'enable') plugin.enabled = true;
          if (request.action === 'disable') plugin.enabled = false;
        }
        state_revision += 1;
      }
      snapshot.receipts.unshift(receipt);
      snapshot.job = null;
      return copy(receipt);
    }
    case 'dsh_cancel_operation':
      if (!snapshot.job) return false;
      cancellation_requested = true;
      return true;
    case 'dsh_start':
      assert_idle();
      if (!snapshot.initialized) throw new Error('DSH_NOT_INITIALIZED');
      if (!snapshot.settings.node_path || !snapshot.settings.dsh_entry_path) throw new Error('DSH_RUNTIME_PATH_REQUIRED');
      snapshot.runtime = { phase: 'web_ready', pid: 4242, web_url: `http://127.0.0.1:${snapshot.settings.port}`, error_code: null };
      return copy(snapshot);
    case 'dsh_stop':
      snapshot.runtime = { phase: 'stopped', pid: null, web_url: null, error_code: null };
      return copy(snapshot);
    case 'dsh_open_web_ui':
      if (snapshot.runtime.phase !== 'web_ready') throw new Error('DSH_NOT_RUNNING');
      return null;
    default: throw new Error(`Unknown DSH demo command: ${command}`);
  }
}
