import { spawn } from 'node:child_process';
import { createRequire } from 'node:module';
import { existsSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { basename, dirname, join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

const browser_candidates = [
  'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe',
  'C:/Program Files/Microsoft/Edge/Application/msedge.exe',
  'C:/Program Files/Google/Chrome/Application/chrome.exe',
  'C:/Program Files (x86)/Google/Chrome/Application/chrome.exe',
];
const startup_timeout_ms = 20_000;
const shutdown_timeout_ms = 5_000;
const poll_interval_ms = 100;
const dev_vite_port = 5173;
const built_demo_vite_port = 5183;
const profile_prefix = 'aag_ui_smoke_';

export const sleep = (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds));

export function find_browser() {
  return browser_candidates.find((candidate) => existsSync(candidate)) ?? null;
}

function resolve_vite_module() {
  const require = createRequire(import.meta.url);
  const package_path = require.resolve('vite/package.json', { paths: [join(process.cwd(), 'ui')] });
  return join(dirname(package_path), 'dist', 'node', 'index.js');
}

function validate_profile_directory(profile_directory) {
  const resolved_profile = resolve(profile_directory);
  return dirname(resolved_profile) === resolve(tmpdir()) &&
    basename(resolved_profile).startsWith(profile_prefix);
}

function remove_profile_directory(profile_directory) {
  if (!validate_profile_directory(profile_directory)) {
    throw new Error('refusing to remove unexpected browser profile path: ' + profile_directory);
  }
  rmSync(profile_directory, { recursive: true, force: true, maxRetries: 8, retryDelay: 100 });
}

async function stop_owned_browser(browser_process, profile_directory) {
  if (browser_process.pid && browser_process.exitCode === null && browser_process.signalCode === null) {
    await new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        browser_process.removeListener('exit', on_exit);
        reject(new Error('owned browser did not exit within shutdown timeout'));
      }, shutdown_timeout_ms);
      const on_exit = () => { clearTimeout(timer); resolve(); };
      browser_process.once('exit', on_exit);
      if (!browser_process.kill()) {
        clearTimeout(timer);
        browser_process.removeListener('exit', on_exit);
        reject(new Error('could not stop owned browser'));
      }
    });
  }
  remove_profile_directory(profile_directory);
}

async function start_dev_vite_server() {
  const vite_module = await import(pathToFileURL(resolve_vite_module()).href);
  const create_server = vite_module.createServer ?? vite_module.default?.createServer;
  if (!create_server) throw new Error('local Vite module does not expose createServer');
  const vite_server = await create_server({
    root: join(process.cwd(), 'ui'),
    logLevel: 'error',
    clearScreen: false,
    server: { host: '127.0.0.1', port: dev_vite_port, strictPort: true },
  });
  try {
    await vite_server.listen();
    const demo_url = vite_server.resolvedUrls?.local?.[0];
    if (!demo_url) throw new Error('owned Vite server has no local URL');
    return { vite_server, demo_url };
  } catch (error) {
    await vite_server.close();
    throw error;
  }
}

async function start_built_demo_server() {
  const output_directory = join(process.cwd(), 'ui', 'dist_demo');
  if (!existsSync(output_directory)) {
    throw new Error('built demo output is missing: run npm run ui:build_demo first');
  }
  const vite_module = await import(pathToFileURL(resolve_vite_module()).href);
  const preview = vite_module.preview ?? vite_module.default?.preview;
  if (!preview) throw new Error('local Vite module does not expose preview');
  const preview_server = await preview({
    root: join(process.cwd(), 'ui'),
    logLevel: 'error',
    clearScreen: false,
    build: { outDir: 'dist_demo' },
    preview: { host: '127.0.0.1', port: built_demo_vite_port, strictPort: true },
  });
  const demo_url = preview_server.resolvedUrls?.local?.[0];
  if (!demo_url) {
    await preview_server.close();
    throw new Error('owned built-demo preview has no local URL');
  }
  return { vite_server: preview_server, demo_url };
}

function read_cdp_origin(profile_directory) {
  const file_path = join(profile_directory, 'DevToolsActivePort');
  if (!existsSync(file_path)) return null;
  const [port] = readFileSync(file_path, 'utf8').split(/\r?\n/);
  return /^\d+$/.test(port) ? 'http://127.0.0.1:' + port : null;
}

async function start_browser(browser_path) {
  const profile_directory = mkdtempSync(join(tmpdir(), profile_prefix));
  const browser_process = spawn(browser_path, [
    '--headless=new',
    '--disable-gpu',
    '--no-first-run',
    '--no-default-browser-check',
    '--user-data-dir=' + profile_directory,
    '--remote-debugging-port=0',
    'about:blank',
  ], { stdio: 'ignore', windowsHide: true });
  let spawn_error = null;
  browser_process.once('error', (error) => {
    spawn_error = error;
  });
  try {
    const deadline = Date.now() + startup_timeout_ms;
    while (Date.now() < deadline) {
      const cdp_origin = read_cdp_origin(profile_directory);
      if (cdp_origin) return { browser_process, profile_directory, cdp_origin };
      if (spawn_error || browser_process.exitCode !== null) break;
      await sleep(poll_interval_ms);
    }
    throw spawn_error ?? new Error('owned headless browser did not expose DevToolsActivePort within 20s');
  } catch (error) {
    try {
      await stop_owned_browser(browser_process, profile_directory);
    } catch (cleanup_error) {
      throw new AggregateError([error, cleanup_error], 'browser startup and cleanup failed');
    }
    throw error;
  }
}

export async function create_smoke_runtime(browser_path, { built_demo = false } = {}) {
  const { vite_server, demo_url } = built_demo
    ? await start_built_demo_server()
    : await start_dev_vite_server();
  try {
    const browser = await start_browser(browser_path);
    return {
      demo_url,
      cdp_origin: browser.cdp_origin,
      profile_directory: browser.profile_directory,
      async close() {
        try {
          await stop_owned_browser(browser.browser_process, browser.profile_directory);
        } finally {
          await vite_server.close();
        }
      },
    };
  } catch (error) {
    await vite_server.close();
    throw error;
  }
}
