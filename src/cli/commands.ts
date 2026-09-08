import chalk from 'chalk';
import { loadConfig, saveConfig, getConfigPath } from '../config/store.js';
import { ConfigManager } from '../config/manager.js';
import { Switcher } from '../agents/switcher.js';
import { createGatewayApp } from '../gateway/server.js';
import { defaultAgentTargets, detectInstalledAgents } from '../agents/adapters.js';
import { logger, setLogLevel } from '../util/log.js';

/* ---- gateway --------------------------------------------------- */

export async function startGatewayCmd(opts: { host?: string; port?: string }) {
  const cfg = loadConfig();
  if (opts.host) cfg.gateway.host = opts.host;
  if (opts.port) cfg.gateway.port = parseInt(opts.port, 10);
  const { start } = createGatewayApp(cfg);
  start();
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

export async function testCmd(model: string | undefined, opts: { noStream?: boolean }) {
  const cfg = loadConfig();
  const url = `http://${cfg.gateway.host}:${cfg.gateway.port}`;
  const mdl = model ?? cfg.routes[0]?.alias ?? 'claude-sonnet-4';
  const stream = !opts.noStream;

  console.log(chalk.gray(`  POST ${url}/v1/chat/completions  model=${mdl} stream=${stream}\n`));
  const headers: Record<string, string> = { 'content-type': 'application/json' };
  if (cfg.gateway.authToken) headers['authorization'] = `Bearer ${cfg.gateway.authToken}`;

  try {
    const body = {
      model: mdl,
      messages: [{ role: 'user', content: 'Hello! Reply in one short sentence.' }],
      stream,
    };
    const res = await fetch(`${url}/v1/chat/completions`, { method: 'POST', headers, body: JSON.stringify(body) });
    if (!res.ok) {
      console.error(chalk.red(`HTTP ${res.status}`));
      console.error(await res.text());
      return;
    }
    if (stream) {
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
      console.log(chalk.gray('  content:'), obj.choices?.[0]?.message?.content);
      console.log(chalk.gray('  usage:'), obj.usage);
    }
  } catch (e) {
    console.error(chalk.red(`Error: ${(e as Error).message}`));
    console.error(chalk.yellow('  Is the gateway running? Start with: aag gateway'));
  }
}

/* ---- config-path ---------------------------------------------- */

export function configPathCmd() {
  console.log(getConfigPath());
}
