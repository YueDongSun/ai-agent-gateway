import chalk from 'chalk';
import { loadConfig, saveConfig, getConfigPath, desktop_database_exists, desktop_database_path } from '../config/store.js';
import { ConfigManager } from '../config/manager.js';
import { Switcher } from '../agents/switcher.js';
import { createGatewayApp, install_shutdown_handlers } from '../gateway/server.js';
import { start_config_watcher } from '../gateway/config_reload.js';
import { defaultAgentTargets, detectInstalledAgents } from '../agents/adapters.js';
import { logger, setLogLevel } from '../util/log.js';
import { parse_port_input } from '../util/parse_port.js';
import { consume_test_stream, Test_stream_error } from './test_stream.js';

/** Default desktop proxy listener (the Rust proxy binds 127.0.0.1:8989
 * unless configured otherwise). */
const DESKTOP_PROXY_DEFAULT_ENDPOINT = 'http://127.0.0.1:8989';
const DESKTOP_PROXY_PROBE_TIMEOUT_MS = 1500;

/**
 * Probe the explicitly selected/default desktop endpoint. Opt into runtime
 * identity while preserving the legacy four-field discovery contract. A
 * shared service marker alone cannot identify the runtime or prove readiness.
 */
export async function desktop_runtime_status(options: { endpoint?: string } = {}): Promise<
  { running: true; version: string; endpoint: string } | { running: false; installed: boolean; endpoint: string; runtime_kind?: 'node' | 'unknown'; reason: 'unreachable' | 'unrecognized' | 'invalid_endpoint' }
> {
  const installed = desktop_database_exists();
  const selected_endpoint = options.endpoint ?? process.env.AAG_DESKTOP_ENDPOINT ?? DESKTOP_PROXY_DEFAULT_ENDPOINT;
  let endpoint: string;
  try {
    const parsed = new URL(selected_endpoint);
    if (!['http:', 'https:'].includes(parsed.protocol) || parsed.username || parsed.password || parsed.search || parsed.hash || parsed.pathname !== '/') throw new Error('invalid endpoint');
    endpoint = parsed.origin;
  } catch {
    return { running: false, installed, endpoint: '', reason: 'invalid_endpoint' };
  }
  try {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), DESKTOP_PROXY_PROBE_TIMEOUT_MS);
    try {
      const res = await fetch(`${endpoint}/aag/discovery?runtime=1`, { signal: controller.signal });
      if (!res.ok) throw new Error(`status ${res.status}`);
      const body = (await res.json()) as { service?: unknown; version?: unknown; runtime_kind?: unknown };
      if (body.service !== 'ai-agent-gateway') return { running: false, installed, endpoint, runtime_kind: 'unknown', reason: 'unrecognized' };
      if (body.runtime_kind !== 'desktop') return { running: false, installed, endpoint, runtime_kind: body.runtime_kind === 'node' ? 'node' : 'unknown', reason: 'unrecognized' };
      return { running: true, version: typeof body.version === 'string' ? body.version : '?', endpoint };
    } finally {
      clearTimeout(timer);
    }
  } catch {
    return { running: false, installed, endpoint, reason: 'unreachable' };
  }
}

/* ---- gateway --------------------------------------------------- */

export async function startGatewayCmd(opts: { host?: string; port?: string }) {
  const cfg = loadConfig();
  const host_override = opts.host;
  let port_override: number | undefined;
  if (host_override) cfg.gateway.host = host_override;
  if (opts.port !== undefined) {
    // The CLI override is applied after AppConfig.parse, so it validates its
    // own input — an invalid value would otherwise surface as an async
    // listen error (or a NaN serialized as null into config.json).
    const port = parse_port_input(opts.port);
    if (port === null) {
      console.error(chalk.red(`--port must be an integer between 1 and 65535 (got "${opts.port}")`));
      process.exitCode = 1;
      return;
    }
    port_override = port;
    cfg.gateway.port = port;
  }
  // Reloads re-apply the validated CLI overrides so the pinned listener
  // identity matches what is actually serving.
  const apply_cli_overrides = (next: ReturnType<typeof loadConfig>) => {
    if (host_override) next.gateway.host = host_override;
    if (port_override !== undefined) next.gateway.port = port_override;
    return next;
  };
  const { start, reload_config } = createGatewayApp(cfg);
  const server = start();
  install_shutdown_handlers(server);
  if (desktop_database_exists()) {
    // The desktop runtime (Rust/SQLite) and this gateway (Node/JSON) keep
    // independent provider stores; without this hint a switch applied in
    // one silently never reaches the other.
    console.warn(chalk.yellow(
      `note: desktop runtime database found at ${desktop_database_path()}; ` +
        'the desktop app and this gateway keep separate provider/route stores — ' +
        'switching in one does not update the other.',
    ));
  }
  const config_watcher = start_config_watcher({
    config_path: getConfigPath(),
    reload: (next) => reload_config(apply_cli_overrides(next)),
  });
  console.log(chalk.dim(`watching config for hot reload: ${getConfigPath()}`));
  await new Promise(() => {});
}

/* ---- switch --------------------------------------------------- */

export async function switchCmd(
  agent: string | undefined,
  opts: { model?: string; to?: string; baseUrl?: string; apiKey?: string; profile?: string },
) {
  const cfg = loadConfig();
  if (!cfg.agents.length) cfg.agents = defaultAgentTargets();
  const sw = new Switcher(cfg);

  const agentId = agent ?? (await chooseAgent(cfg));
  if (!agentId) { console.error(chalk.red('no agent specified')); return; }

  const model = opts.model ?? cfg.routes[0]?.alias ?? 'claude-sonnet-4';
  const target = opts.to ?? 'gateway';

  let result;
  if (target === 'gateway') {
    result = sw.pointToGateway(agentId as any, model);
  } else if (target === 'direct') {
    const baseURL = opts.baseUrl ?? '';
    const apiKey = opts.apiKey ?? '';
    if (!baseURL) { console.error(chalk.red('--base-url required for direct')); return; }
    result = sw.pointToProvider(agentId as any, baseURL, apiKey, model);
  } else if (target === 'profile') {
    const pid = opts.profile ?? '';
    if (!pid) { console.error(chalk.red('--profile required')); return; }
    result = sw.pointToProfile(agentId as any, pid, model);
  } else {
    console.error(chalk.red(`--to must be gateway|direct|profile`)); return;
  }

  saveConfig(cfg);
  if (result.ok) console.log(chalk.green(`✓ ${result.message}`) + chalk.gray(`  (${result.path})`));
  else console.log(chalk.red(`✗ ${result.message}`));
}

async function chooseAgent(cfg: any): Promise<string | undefined> {
  const inquirer = await import('@inquirer/prompts');
  return inquirer.select({
    message: 'Select agent',
    choices: cfg.agents.map((a: any) => ({ name: a.name, value: a.id })),
  });
}

/* ---- status --------------------------------------------------- */

export async function statusCmd() {
  const cfg = loadConfig();
  console.log(chalk.bold.cyan('\n  Gateway:'));
  console.log(`    ${cfg.gateway.host}:${cfg.gateway.port}  log=${cfg.gateway.logLevel}`);

  console.log(chalk.bold.cyan('\n  Desktop runtime:'));
  const desktop = await desktop_runtime_status();
  if (desktop.running) {
    console.log(
      `    ${chalk.green('●')} desktop proxy running at ${desktop.endpoint} (v${desktop.version}) — separate provider store`,
    );
  } else if (desktop.reason === 'invalid_endpoint') {
    console.log(chalk.yellow('    invalid AAG_DESKTOP_ENDPOINT; use an HTTP(S) endpoint without credentials, query or path'));
  } else if (desktop.runtime_kind) {
    console.log(chalk.yellow(`    ${desktop.runtime_kind === 'node' ? 'Node gateway' : 'unidentified/older runtime'} at ${desktop.endpoint}; desktop runtime is not verified`));
  } else if (desktop.installed) {
    console.log(
      `    ${chalk.yellow('○')} installed (${desktop_database_path()}) — proxy not detected at ${desktop.endpoint}; set AAG_DESKTOP_ENDPOINT for a custom listener`,
    );
  } else {
    console.log(chalk.gray(`    not installed; no desktop proxy detected at ${desktop.endpoint}`));
  }

  console.log(chalk.bold.cyan('\n  Providers:'));
  if (!cfg.providers.length) console.log(chalk.gray('    (none)'));
  for (const p of cfg.providers) {
    console.log(`    ${p.enabled ? chalk.green('●') : chalk.red('○')} ${p.name} [${p.id}] (${p.type})`);
    console.log(chalk.gray(`      ${p.baseURL || '(no url)'}  models: ${p.models.join(', ')}`));
  }

  console.log(chalk.bold.cyan('\n  Routes:'));
  if (!cfg.routes.length) console.log(chalk.gray('    (none)'));
  for (const r of cfg.routes) {
    console.log(`    ${r.alias} ${chalk.gray('->')} ${r.hops.map((h) => `${h.provider}/${h.model}`).join(' -> ')}`);
  }

  console.log(chalk.bold.cyan('\n  Agents:'));
  if (!cfg.agents.length) cfg.agents = defaultAgentTargets();
  const installed = detectInstalledAgents(cfg.agents);
  for (const a of cfg.agents) {
    const isInstalled = installed.some((i) => i.id === a.id);
    const marker = a.pointedAtGateway ? chalk.green('[gateway]') : chalk.blue('[direct]');
    const inst = isInstalled ? '' : chalk.yellow(' (not detected)');
    console.log(`    ${a.name} ${marker}${inst}`);
  }

  console.log(chalk.bold.cyan('\n  Profiles:'));
  if (!cfg.profiles.length) console.log(chalk.gray('    (none)'));
  for (const p of cfg.profiles) {
    console.log(`    ${p.name} [${p.id}] (${p.type}) ${chalk.gray(p.baseURL)}`);
  }
  console.log('');
}

/* ---- models --------------------------------------------------- */

export async function modelsCmd() {
  const cfg = loadConfig();
  const url = `http://${cfg.gateway.host}:${cfg.gateway.port}`;
  try {
    const headers: Record<string, string> = {};
    if (cfg.gateway.authToken) headers['authorization'] = `Bearer ${cfg.gateway.authToken}`;
    const res = await fetch(`${url}/v1/models`, { headers });
    if (!res.ok) { console.error(chalk.red(`HTTP ${res.status}`)); return; }
    const obj = (await res.json()) as { data?: Array<{ id: string }> };
    console.log(chalk.bold.cyan('\n  Available models:'));
    for (const m of obj.data ?? []) console.log(`    ${m.id}`);
    console.log('');
  } catch (e) {
    console.error(chalk.red(`Cannot reach gateway at ${url}: ${(e as Error).message}`));
    console.error(chalk.yellow('  Start it with: aag gateway'));
  }
}

/* ---- test ----------------------------------------------------- */

export async function testCmd(model: string | undefined, opts: { noStream?: boolean; stream?: boolean; signal?: AbortSignal }) {
  const cfg = loadConfig();
  const url = `http://${cfg.gateway.host}:${cfg.gateway.port}`;
  const mdl = model ?? cfg.routes[0]?.alias ?? 'claude-sonnet-4';
  const stream = opts.stream ?? !opts.noStream;

  console.log(chalk.gray(`  POST ${url}/v1/chat/completions  model=${mdl} stream=${stream}\n`));
  const headers: Record<string, string> = { 'content-type': 'application/json' };
  if (cfg.gateway.authToken) headers['authorization'] = `Bearer ${cfg.gateway.authToken}`;

  const controller = new AbortController();
  const cancel = () => controller.abort();
  if (!opts.signal) process.once('SIGINT', cancel);
  const signal = opts.signal ?? controller.signal;

  try {
    const body = {
      model: mdl,
      messages: [{ role: 'user', content: 'Hello! Reply in one short sentence.' }],
      stream,
    };
    const res = await fetch(`${url}/v1/chat/completions`, { method: 'POST', headers, body: JSON.stringify(body), signal });
    if (!res.ok) {
      process.exitCode = 1;
      console.error(chalk.red(`Test failed: HTTP ${res.status}`));
      await res.body?.cancel();
      return;
    }
    if (stream) {
      if (!res.body) throw new Test_stream_error('stream incomplete: response body is missing');
      await consume_test_stream(res.body, (text) => { process.stdout.write(text); });
      signal.throwIfAborted();
      console.log(chalk.green('\n\n  ✓ stream complete'));
    } else {
      const obj = (await res.json()) as { error?: unknown; choices?: Array<{ message?: { content?: string } }>; usage?: unknown };
      if (obj.error !== undefined || typeof obj.choices?.[0]?.message?.content !== 'string') throw new Test_stream_error('Test failed: invalid completion response');
      console.log(chalk.gray('  content:'), obj.choices?.[0]?.message?.content);
      console.log(chalk.gray('  usage:'), obj.usage);
    }
  } catch (e) {
    process.exitCode = 1;
    const message = e instanceof Test_stream_error ? e.message : 'Test failed: transport or response decoding error';
    console.error(chalk.red(signal.aborted ? 'Test cancelled; any displayed output is incomplete.' : message));
  } finally {
    if (!opts.signal) process.removeListener('SIGINT', cancel);
  }
}

/* ---- config-path ---------------------------------------------- */

export function configPathCmd() {
  console.log(getConfigPath());
}
