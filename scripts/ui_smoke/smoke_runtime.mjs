import { execFile, spawn } from 'node:child_process';
import { createRequire } from 'node:module';
import { accessSync, constants, existsSync, mkdtempSync, readFileSync, statSync } from 'node:fs';
import { rm } from 'node:fs/promises';
import { homedir, tmpdir } from 'node:os';
import { basename, dirname, isAbsolute, join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { promisify } from 'node:util';

const browser_candidates = {
  win32: [
    'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe',
    'C:/Program Files/Microsoft/Edge/Application/msedge.exe',
    'C:/Program Files/Google/Chrome/Application/chrome.exe',
    'C:/Program Files (x86)/Google/Chrome/Application/chrome.exe',
  ],
  darwin: [
    '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
    '/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge',
    '/Applications/Chromium.app/Contents/MacOS/Chromium',
  ],
  linux: ['/usr/bin/google-chrome', '/usr/bin/google-chrome-stable', '/usr/bin/chromium', '/usr/bin/chromium-browser', '/snap/bin/chromium'],
};
const startup_timeout_ms = 20_000;
const shutdown_timeout_ms = 5_000;
const poll_interval_ms = 100;
const dev_vite_port = 5173;
const built_demo_vite_port = 5183;
const profile_prefix = 'aag_ui_smoke_';
const profile_cleanup_timeout_ms = 6_000;
const profile_cleanup_command_timeout_ms = 5_000;
const windows_profile_environment_key = 'AAG_UI_SMOKE_PROFILE';
const windows_profile_cleanup_script = '$profile_path = [Environment]::GetEnvironmentVariable("AAG_UI_SMOKE_PROFILE"); if ([string]::IsNullOrWhiteSpace($profile_path)) { throw "AAG_UI_SMOKE_PROFILE is unavailable" }; $comparison = [StringComparison]::OrdinalIgnoreCase; Get-CimInstance -ClassName Win32_Process -ErrorAction Stop | Where-Object { $_.CommandLine -and $_.CommandLine.IndexOf($profile_path, $comparison) -ge 0 } | ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }';
const exec_file = promisify(execFile);

export const sleep = (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds));

function browser_executable(candidate, platform) {
  try {
    if (!isAbsolute(candidate) || !statSync(candidate).isFile()) return false;
    accessSync(candidate, platform === 'win32' ? constants.F_OK : constants.X_OK);
    return true;
  } catch { return false; }
}

export function find_browser(platform = process.platform, path_exists, {
  home = homedir(), path = process.env.PATH, browser_path = process.env.AAG_BROWSER_PATH,
} = {}) {
  const usable = path_exists ?? ((candidate) => browser_executable(candidate, platform));
  if (!browser_candidates[platform]) return null;
  if (browser_path) return isAbsolute(browser_path) && usable(browser_path) ? browser_path : null;
  const candidates = [...browser_candidates[platform]];
  if (platform === 'darwin' && isAbsolute(home)) {
    candidates.push(...browser_candidates.darwin.map((candidate) => join(home, candidate.slice(1))));
  }
  const names = platform === 'win32' ? ['chrome.exe', 'msedge.exe', 'chromium.exe']
    : ['google-chrome', 'google-chrome-stable', 'chromium', 'chromium-browser', 'microsoft-edge'];
  for (const directory of (path ?? '').split(platform === 'win32' ? ';' : ':').slice(0, 128)) {
    if (isAbsolute(directory)) candidates.push(...names.map((name) => join(directory, name)));
  }
  return [...new Set(candidates)].find(usable) ?? null;
}

function resolve_vite_module() {
  const require = createRequire(import.meta.url);
  const package_path = require.resolve('vite/package.json', { paths: [join(process.cwd(), 'ui')] });
  return join(dirname(package_path), 'dist', 'node', 'index.js');
}

export function validate_profile_directory(profile_directory) {
  const resolved_profile = resolve(profile_directory);
  return dirname(resolved_profile) === resolve(tmpdir()) &&
    basename(resolved_profile).startsWith(profile_prefix);
}

export function windows_taskkill_path(system_root = process.env.SystemRoot, path_exists = existsSync) {
  if (!system_root) throw new Error('Windows SystemRoot is unavailable');
  const taskkill_path = join(system_root, 'System32', 'taskkill.exe');
  if (!path_exists(taskkill_path)) throw new Error('Windows taskkill.exe is unavailable');
  return taskkill_path;
}

export function windows_powershell_path(system_root = process.env.SystemRoot, path_exists = existsSync) {
  if (!system_root) throw new Error('Windows SystemRoot is unavailable');
  const powershell_path = join(system_root, 'System32', 'WindowsPowerShell', 'v1.0', 'powershell.exe');
  if (!path_exists(powershell_path)) throw new Error('Windows PowerShell is unavailable');
  return powershell_path;
}

export function format_error(error) {
  if (error instanceof AggregateError) {
    const nested = error.errors.map(format_error).filter(Boolean);
    return nested.length ? error.message + ': ' + nested.join('; ') : error.message;
  }
  return error?.message ?? String(error);
}

export async function terminate_windows_profile_processes(profile_directory) {
  if (!validate_profile_directory(profile_directory)) {
    throw new Error('refusing to clean processes for unexpected browser profile path: ' + profile_directory);
  }
  await exec_file(windows_powershell_path(), [
    '-NoProfile',
    '-NonInteractive',
    '-Command',
    windows_profile_cleanup_script,
  ], {
    windowsHide: true,
    timeout: profile_cleanup_command_timeout_ms,
    env: { ...process.env, [windows_profile_environment_key]: resolve(profile_directory) },
  });
}

async function remove_profile_directory(profile_directory) {
  if (!validate_profile_directory(profile_directory)) {
    throw new Error('refusing to remove unexpected browser profile path: ' + profile_directory);
  }
  const deadline = Date.now() + profile_cleanup_timeout_ms;
  let last_error = new Error('cleanup deadline expired before first attempt');
  while (Date.now() < deadline) {
    try {
      await rm(profile_directory, { recursive: true, force: true, maxRetries: 0 });
      return;
    } catch (error) {
      last_error = error;
      await sleep(Math.max(0, Math.min(200, deadline - Date.now())));
    }
  }
  throw last_error;
}

function browser_has_exited(browser_process) {
  return browser_process.exitCode !== null || browser_process.signalCode !== null;
}

async function wait_for_browser_exit(browser_process) {
  if (browser_has_exited(browser_process)) return;
  await new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      browser_process.removeListener('exit', on_exit);
      reject(new Error('owned browser did not exit within shutdown timeout'));
    }, shutdown_timeout_ms);
    const on_exit = () => { clearTimeout(timer); resolve(); };
    browser_process.once('exit', on_exit);
  });
}

export async function stop_owned_browser(browser_process, profile_directory) {
  if (!validate_profile_directory(profile_directory)) {
    throw new Error('refusing to stop browser for unexpected profile path: ' + profile_directory);
  }
  const termination_errors = [];
  if (browser_process.pid && !browser_has_exited(browser_process)) {
    if (process.platform === 'win32') {
      try {
        await exec_file(windows_taskkill_path(), ['/PID', String(browser_process.pid), '/T', '/F'], {
          windowsHide: true,
          timeout: shutdown_timeout_ms,
        });
      } catch (error) {
        termination_errors.push(error);
      }
    } else if (!browser_process.kill() && !browser_has_exited(browser_process)) {
      termination_errors.push(new Error('could not stop owned browser'));
    }
  }
  if (process.platform === 'win32') {
    try {
      // The root can exit before Chromium descendants release the profile.
      await terminate_windows_profile_processes(profile_directory);
    } catch (error) {
      termination_errors.push(error);
    }
  }
  if (browser_process.pid && !browser_has_exited(browser_process)) {
    try {
      await wait_for_browser_exit(browser_process);
    } catch (error) {
      termination_errors.push(error);
      throw new AggregateError(termination_errors, 'could not stop owned browser');
    }
  }
  try {
    await remove_profile_directory(profile_directory);
  } catch (error) {
    throw new AggregateError([...termination_errors, error], 'could not stop owned browser');
  }
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

export function read_cdp_origin(profile_directory, read_file = readFileSync) {
  const file_path = join(profile_directory, 'DevToolsActivePort');
  let contents;
  try { contents = read_file(file_path, 'utf8'); }
  catch (error) {
    // Chromium can create, replace or exclusively open this file during boot.
    // Only these transient states return to the existing bounded startup poll.
    if (error?.code === 'ENOENT' || error?.code === 'EBUSY') return null;
    throw error;
  }
  const [port] = contents.split(/\r?\n/);
  const port_number = Number(port);
  return /^\d+$/.test(port) && Number.isInteger(port_number) && port_number >= 1 && port_number <= 65535
    ? 'http://127.0.0.1:' + port : null;
}

export async function wait_for_cdp_origin(profile_directory, browser_process, {
  read_file = readFileSync, read_spawn_error = () => null, now = Date.now, pause = sleep,
} = {}) {
  const deadline = now() + startup_timeout_ms;
  while (now() < deadline) {
    const cdp_origin = read_cdp_origin(profile_directory, read_file);
    if (cdp_origin) return cdp_origin;
    if (read_spawn_error() || browser_process.exitCode !== null) break;
    await pause(poll_interval_ms);
  }
  throw read_spawn_error() ?? new Error('owned headless browser did not expose DevToolsActivePort within 20s');
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
    const cdp_origin = await wait_for_cdp_origin(profile_directory, browser_process, { read_spawn_error: () => spawn_error });
    return { browser_process, profile_directory, cdp_origin };
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
