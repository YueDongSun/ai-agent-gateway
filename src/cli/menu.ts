import chalk from 'chalk';
import * as prompts from '@inquirer/prompts';
import ora from 'ora';
import { loadConfig, saveConfig, getConfigPath } from '../config/store.js';
import { ConfigManager } from '../config/manager.js';
import { Switcher } from '../agents/switcher.js';
import { createGatewayApp, install_shutdown_handlers } from '../gateway/server.js';
import { detectInstalledAgents, defaultAgentTargets } from '../agents/adapters.js';
import { logger } from '../util/log.js';
import { parse_port_input } from '../util/parse_port.js';
import type { AgentId } from '../config/schema.js';

const { select, input, confirm, password, checkbox } = prompts;

export async function interactiveMenu(): Promise<void> {
  while (true) {
    const choice = await select({
      message: chalk.bold.cyan('AI Agent Gateway'),
      choices: [
        { name: '🚀 Start gateway server', value: 'gateway' },
        { name: '🔀 Switch agent target', value: 'switch' },
        { name: '📋 View current status', value: 'status' },
        { name: '🔧 Manage providers', value: 'providers' },
        { name: '🛣  Manage routes', value: 'routes' },
        { name: '👤 Manage profiles', value: 'profiles' },
        { name: '⚙  Edit gateway settings', value: 'settings' },
        { name: '🧪 Run mock test', value: 'test' },
        { name: '📂 Open config file', value: 'config' },
        { name: 'Exit', value: 'exit' },
      ],
    });

    switch (choice) {
      case 'gateway': await startGateway(); break;
      case 'switch': await switchAgent(); break;
      case 'status': await showStatus(); break;
      case 'providers': await manageProviders(); break;
      case 'routes': await manageRoutes(); break;
      case 'profiles': await manageProfiles(); break;
      case 'settings': await editSettings(); break;
      case 'test': await runMockTest(); break;
      case 'config': console.log(chalk.gray(`Config: ${getConfigPath()}`)); break;
      case 'exit': return;
    }
  }
}

/* ---- gateway --------------------------------------------------- */

async function startGateway() {
  const cfg = loadConfig();
  const { start } = createGatewayApp(cfg);
  console.log(chalk.green(`\n  Starting gateway on ${cfg.gateway.host}:${cfg.gateway.port} ...\n`));
  const server = start();
  install_shutdown_handlers(server);
  // keep process alive
  await new Promise(() => {});
}

/* ---- switch --------------------------------------------------- */

async function switchAgent() {
  const cfg = loadConfig();
  if (!cfg.agents.length) cfg.agents = defaultAgentTargets();
  const sw = new Switcher(cfg);

  const agent = await select({
    message: 'Select agent',
    choices: sw.listAgents().map((a) => ({ name: a.name, value: a.id as AgentId })),
  });

  const mode = await select({
    message: 'Point at',
    choices: [
      { name: 'Gateway (unified proxy with failover)', value: 'gateway' },
      { name: 'Direct provider (bypass gateway)', value: 'direct' },
      { name: 'Profile', value: 'profile' },
    ],
  });

  const model = await input({ message: 'Model to use:', default: cfg.routes[0]?.alias ?? 'claude-sonnet-4' });

  let result;
  if (mode === 'gateway') {
    result = sw.pointToGateway(agent, model);
  } else if (mode === 'direct') {
    const baseURL = await input({ message: 'Provider base URL:', default: 'https://api.openai.com/v1' });
    const apiKey = await password({ message: 'API key:', mask: '*' });
    result = sw.pointToProvider(agent, baseURL, apiKey, model);
  } else {
    if (!cfg.profiles.length) {
      console.log(chalk.yellow('No profiles. Create one first.'));
      return;
    }
    const prof = await select({
      message: 'Profile',
      choices: cfg.profiles.map((p) => ({ name: `${p.name} (${p.type})`, value: p.id })),
    });
    result = sw.pointToProfile(agent, prof, model);
  }

  saveConfig(cfg);

  if (result.ok) {
    console.log(chalk.green(`\n  ✓ ${result.message}`));
    console.log(chalk.gray(`    file: ${result.path}`));
  } else {
    console.log(chalk.red(`\n  ✗ ${result.message}`));
  }
}

/* ---- status --------------------------------------------------- */

async function showStatus() {
  const cfg = loadConfig();
  console.log(chalk.bold.cyan('\n  Gateway:'));
  console.log(`    ${cfg.gateway.host}:${cfg.gateway.port}  log=${cfg.gateway.logLevel}`);

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
  console.log('');
}

/* ---- providers ------------------------------------------------ */

async function manageProviders() {
  const cfg = loadConfig();
  const mgr = new ConfigManager(cfg);

  const action = await select({
    message: 'Providers',
    choices: [
      { name: 'Add provider', value: 'add' },
      { name: 'Remove provider', value: 'remove' },
      { name: 'Toggle enabled', value: 'toggle' },
      { name: 'Back', value: 'back' },
    ],
  });

  if (action === 'back') return;

  if (action === 'add') {
    const type = await select({
      message: 'Type',
      choices: [
        { name: 'OpenAI compatible', value: 'openai' as const },
        { name: 'Anthropic', value: 'anthropic' as const },
        { name: 'Mock (no key needed)', value: 'mock' as const },
      ],
    });
    const name = (await input({ message: 'Display name:' })).trim();
    if (!name) { console.log(chalk.yellow('  display name required')); return; }
    const id = await input({ message: 'ID (optional):', default: '' });
    const baseURL = await input({
      message: 'Base URL:',
      default: type === 'anthropic' ? 'https://api.anthropic.com' : 'https://api.openai.com/v1',
    });
    const apiKey = type === 'mock' ? 'mock' : await password({ message: 'API key:', mask: '*' });
    const modelsRaw = await input({ message: 'Models (comma-separated):', default: 'gpt-4o' });
    const models = modelsRaw.split(',').map((s) => s.trim()).filter(Boolean);

    mgr.addProvider({
      id: id || undefined,
      name,
      type,
      apiKey,
      baseURL,
      models,
      enabled: true,
      weight: 1,
      circuitBreaker: { failureThreshold: 5, cooldownMs: 30_000, halfOpenRetries: 1, successThreshold: 2 },
      timeoutMs: 60_000,
    });
    saveConfig(cfg);
    console.log(chalk.green('  ✓ provider added'));
  } else if (action === 'remove') {
    if (!cfg.providers.length) { console.log(chalk.yellow('  no providers')); return; }
    const id = await select({ message: 'Remove', choices: cfg.providers.map((p) => ({ name: `${p.name} [${p.id}]`, value: p.id })) });
    mgr.removeProvider(id);
    saveConfig(cfg);
    console.log(chalk.green('  ✓ removed'));
  } else if (action === 'toggle') {
    if (!cfg.providers.length) { console.log(chalk.yellow('  no providers')); return; }
    const id = await select({ message: 'Toggle', choices: cfg.providers.map((p) => ({ name: `${p.name} [${p.id}]`, value: p.id })) });
    const p = cfg.providers.find((x) => x.id === id)!;
    mgr.updateProvider(id, { enabled: !p.enabled });
    saveConfig(cfg);
    console.log(chalk.green(`  ✓ ${p.enabled ? 'disabled' : 'enabled'}`));
  }
}

/* ---- routes --------------------------------------------------- */

async function manageRoutes() {
  const cfg = loadConfig();
  const mgr = new ConfigManager(cfg);

  const action = await select({
    message: 'Routes',
    choices: [
      { name: 'Add route', value: 'add' },
      { name: 'Remove route', value: 'remove' },
      { name: 'Back', value: 'back' },
    ],
  });
  if (action === 'back') return;

  if (action === 'add') {
    const alias = await input({ message: 'Model alias (what clients request):', default: 'claude-sonnet-4' });
    const hops = [];
    const providerIds = cfg.providers.map((p) => p.id);
    if (!providerIds.length) { console.log(chalk.yellow('  add providers first')); return; }
    while (true) {
      const provId: string = await select({ message: `Hop ${hops.length + 1} - provider`, choices: [...providerIds.map((id) => ({ name: id, value: id })), { name: '(done)', value: '__done__' }] });
      if (provId === '__done__') break;
      const prov = cfg.providers.find((p) => p.id === provId)!;
      const model = await select({ message: 'Model at this provider', choices: prov.models.map((m) => ({ name: m, value: m })) });
      hops.push({ provider: provId, model });
    }
    if (!hops.length) { console.log(chalk.yellow('  no hops')); return; }
    const desc = await input({ message: 'Description:', default: '' });
    mgr.addRoute(alias, hops, desc);
    saveConfig(cfg);
    console.log(chalk.green('  ✓ route added'));
  } else if (action === 'remove') {
    if (!cfg.routes.length) { console.log(chalk.yellow('  no routes')); return; }
    const alias = await select({ message: 'Remove', choices: cfg.routes.map((r) => ({ name: r.alias, value: r.alias })) });
    mgr.removeRoute(alias);
    saveConfig(cfg);
    console.log(chalk.green('  ✓ removed'));
  }
}

/* ---- profiles ------------------------------------------------- */

async function manageProfiles() {
  const cfg = loadConfig();
  const mgr = new ConfigManager(cfg);

  const action = await select({
    message: 'Profiles',
    choices: [
      { name: 'Add profile', value: 'add' },
      { name: 'Remove profile', value: 'remove' },
      { name: 'Back', value: 'back' },
    ],
  });
  if (action === 'back') return;

  if (action === 'add') {
    const name = await input({ message: 'Profile name:' });
    const type = await select({ message: 'Type', choices: [{ name: 'OpenAI', value: 'openai' as const }, { name: 'Anthropic', value: 'anthropic' as const }] });
    const baseURL = await input({ message: 'Base URL:', default: type === 'anthropic' ? 'https://api.anthropic.com' : 'https://api.openai.com/v1' });
    const apiKey = await password({ message: 'API key:', mask: '*' });
    const modelsRaw = await input({ message: 'Models (comma-separated):', default: '' });
    mgr.addProfile({ name, type, baseURL, apiKey, models: modelsRaw.split(',').map((s) => s.trim()).filter(Boolean), notes: '' });
    saveConfig(cfg);
    console.log(chalk.green('  ✓ profile added'));
  } else if (action === 'remove') {
    if (!cfg.profiles.length) { console.log(chalk.yellow('  no profiles')); return; }
    const id = await select({ message: 'Remove', choices: cfg.profiles.map((p) => ({ name: `${p.name}`, value: p.id })) });
    mgr.removeProfile(id);
    saveConfig(cfg);
    console.log(chalk.green('  ✓ removed'));
  }
}

/* ---- settings ------------------------------------------------- */

async function editSettings() {
  const cfg = loadConfig();
  const host = (await input({ message: 'Host:', default: cfg.gateway.host })).trim();
  const port_raw = await input({ message: 'Port:', default: String(cfg.gateway.port) });
  // setGateway bypasses AppConfig.parse; an unvalidated NaN would be
  // serialized as null and make the saved config unparsable on next load.
  const port = parse_port_input(port_raw);
  if (!host || port === null) {
    console.log(chalk.red('  ✗ host must be non-empty; port must be an integer between 1 and 65535'));
    return;
  }
  const authToken = await input({ message: 'Auth token (empty=none):', default: cfg.gateway.authToken });
  const logLevel = await select({ message: 'Log level', choices: (['debug', 'info', 'warn', 'error'] as const).map((v) => ({ name: v, value: v })), default: cfg.gateway.logLevel });
  const mgr = new ConfigManager(cfg);
  mgr.setGateway({ host, port, authToken, logLevel });
  saveConfig(cfg);
  console.log(chalk.green('  ✓ settings saved'));
}

/* ---- mock test ------------------------------------------------ */

async function runMockTest() {
  const cfg = loadConfig();
  const url = `http://${cfg.gateway.host}:${cfg.gateway.port}`;
  const model = await select({
    message: 'Test model',
    choices: (cfg.routes.length ? cfg.routes : [{ alias: 'claude-sonnet-4' }]).map((r) => ({ name: r.alias, value: r.alias })),
  });
  const stream = await confirm({ message: 'Stream?', default: true });

  console.log(chalk.gray(`  POST ${url}/v1/chat/completions  model=${model} stream=${stream}\n`));
  const spin = ora('sending request...').start();

  try {
    const body = {
      model,
      messages: [{ role: 'user', content: 'Hello! Reply in one short sentence.' }],
      stream,
    };
    const headers: Record<string, string> = { 'content-type': 'application/json' };
    if (cfg.gateway.authToken) headers['authorization'] = `Bearer ${cfg.gateway.authToken}`;

    const res = await fetch(`${url}/v1/chat/completions`, { method: 'POST', headers, body: JSON.stringify(body) });
    if (!res.ok) {
      spin.fail(chalk.red(`HTTP ${res.status}`));
      console.log(await res.text());
      return;
    }

    if (stream) {
      spin.succeed('streaming');
      const decoder = new TextDecoder();
      for await (const chunk of res.body as any) {
        const text = decoder.decode(chunk as Uint8Array);
        for (const line of text.split('\n')) {
          if (!line.startsWith('data:')) continue;
          const d = line.slice(5).trim();
          if (d === '[DONE]') continue;
          try {
            const obj = JSON.parse(d);
            const delta = obj.choices?.[0]?.delta?.content;
            if (delta) process.stdout.write(delta);
          } catch {}
        }
      }
      console.log(chalk.green('\n\n  ✓ stream complete'));
    } else {
      const obj = (await res.json()) as { choices?: Array<{ message?: { content?: string } }>; usage?: unknown };
      spin.succeed('response');
      console.log(chalk.gray('  content:'), obj.choices?.[0]?.message?.content);
      console.log(chalk.gray('  usage:'), obj.usage);
    }
  } catch (e) {
    spin.fail(chalk.red((e as Error).message));
    console.log(chalk.yellow(`\n  Is the gateway running? Start it with: aag gateway`));
  }
}
