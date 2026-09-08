import { readFileSync, writeFileSync, existsSync, mkdirSync, renameSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { homedir } from 'node:os';
import { AppConfig, type AppConfig as AppConfigType } from './schema.js';

const CONFIG_DIR =
  process.env.AAG_CONFIG_DIR ?? join(homedir(), '.ai-agent-gateway');
const CONFIG_FILE = join(CONFIG_DIR, 'config.json');

export function getConfigDir(): string {
  return CONFIG_DIR;
}

export function getConfigPath(): string {
  return CONFIG_FILE;
}

export function defaultConfig(): AppConfigType {
  return AppConfig.parse({
    gateway: { host: '127.0.0.1', port: 8787 },
    providers: [],
    routes: [],
    profiles: [],
    agents: [],
  });
}

/** Seed a brand-new config with a mock provider + route so the gateway works out-of-the-box. */
export function seedConfig(): AppConfigType {
  return AppConfig.parse({
    version: 1,
    gateway: { host: '127.0.0.1', port: 8787, authToken: '', logLevel: 'info' },
    providers: [
      {
        id: 'mock-primary',
        name: 'Mock Primary (slow)',
        type: 'mock',
        apiKey: 'mock',
        baseURL: 'mock://primary',
        models: ['mock-fast', 'mock-slow', 'claude-sonnet-4', 'gpt-4o'],
        enabled: true,
        weight: 1,
        circuitBreaker: {
          failureThreshold: 3,
          cooldownMs: 15_000,
          halfOpenRetries: 1,
          successThreshold: 2,
        },
      },
      {
        id: 'mock-fallback',
        name: 'Mock Fallback (always works)',
        type: 'mock',
        apiKey: 'mock',
        baseURL: 'mock://fallback',
        models: ['claude-sonnet-4', 'gpt-4o', 'mock-fast'],
        enabled: true,
        weight: 1,
        circuitBreaker: {
          failureThreshold: 5,
          cooldownMs: 30_000,
          halfOpenRetries: 1,
          successThreshold: 2,
        },
      },
    ],
    routes: [
      {
        alias: 'claude-sonnet-4',
        description: 'Sonnet via mock chain (primary -> fallback)',
        hops: [
          { provider: 'mock-primary', model: 'claude-sonnet-4' },
          { provider: 'mock-fallback', model: 'claude-sonnet-4' },
        ],
      },
      {
        alias: 'gpt-4o',
        description: 'GPT-4o via mock chain',
        hops: [
          { provider: 'mock-primary', model: 'gpt-4o' },
          { provider: 'mock-fallback', model: 'gpt-4o' },
        ],
      },
      {
        alias: 'mock-fail',
        description: 'Forces failures on primary to demo circuit-breaker',
        hops: [
          { provider: 'mock-primary', model: 'mock-fail' },
          { provider: 'mock-fallback', model: 'mock-fast' },
        ],
      },
    ],
    profiles: [],
    agents: [],
  });
}

export function loadConfig(): AppConfigType {
  try {
    if (!existsSync(CONFIG_FILE)) {
      const seeded = seedConfig();
      saveConfig(seeded);
      return seeded;
    }
    const raw = readFileSync(CONFIG_FILE, 'utf8');
    return AppConfig.parse(JSON.parse(raw));
  } catch (err) {
    process.stderr.write(
      `[config] failed to load ${CONFIG_FILE}: ${(err as Error).message}\n` +
        `falling back to seed config.\n`,
    );
    const seeded = seedConfig();
    return seeded;
  }
}

export function saveConfig(cfg: AppConfigType): void {
  if (!existsSync(CONFIG_DIR)) mkdirSync(CONFIG_DIR, { recursive: true });
  const tmp = CONFIG_FILE + '.tmp';
  writeFileSync(tmp, JSON.stringify(cfg, null, 2) + '\n', 'utf8');
  renameSync(tmp, CONFIG_FILE);
}
