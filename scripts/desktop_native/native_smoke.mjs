import { spawn, execFile } from 'node:child_process';
import { mkdtemp, mkdir, readFile, rm, stat, writeFile } from 'node:fs/promises';
import net from 'node:net';
import { once } from 'node:events';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { cdp_client } from '../ui_smoke/cdp_client.mjs';
import { read_cdp_endpoint } from '../ui_smoke/cdp_endpoint.mjs';
import { stop_owned_child } from '../verification/runner.mjs';
import { inspect_windows_image } from './pe_image.mjs';

const root = fileURLToPath(new URL('../../', import.meta.url));
const driver_path = fileURLToPath(new URL('./windows_driver.ps1', import.meta.url));
const powershell = join(process.env.SystemRoot || 'C:/Windows', 'System32/WindowsPowerShell/v1.0/powershell.exe');
const binary = resolve(process.argv[2] || join(root, 'src-tauri/target/release/ai-agent-gateway.exe'));
const readiness_timeout_ms = 45_000;
const exit_timeout_ms = 30_000;
const fixture_token = 'desktop_native_fixture_auth';
const exec_file = promisify(execFile);
const pause = (duration) => new Promise((done) => setTimeout(done, duration));
let checks = 0;
let application;
let directory;
let state = 'blocked';
let retain_state = false;
let secondary;
let cancellation_code = 0;
let before;
let current_step = 'preflight';
const cancel_int = () => { cancellation_code ||= 130; };
const cancel_term = () => { cancellation_code ||= 143; };
process.on('SIGINT', cancel_int);
process.on('SIGTERM', cancel_term);

function ensure_not_cancelled() {
  if (cancellation_code) throw new Error('desktop_native_cancelled');
}

function check(name, value) {
  ensure_not_cancelled();
  if (!value) throw new Error('desktop_native_check_failed:' + name);
  checks += 1;
  process.stdout.write('PASS ' + name + '\n');
}

async function driver(action, target = application, { cleanup = false } = {}) {
  if (!cleanup || action !== 'preflight') ensure_not_cancelled();
  const args = ['-NoProfile', '-NonInteractive', '-File', driver_path, '-Action', action];
  if (action !== 'preflight') {
    args.push('-ProcessId', String(target.child.pid), '-ExecutablePath', binary);
    if (target.start_ticks) args.push('-StartTicks', target.start_ticks);
  }
  const { stdout } = await exec_file(powershell, args, { windowsHide: true, timeout: 15_000, maxBuffer: 64 * 1024 });
  return stdout.trim() ? JSON.parse(stdout) : null;
}

async function free_port() {
  const listener = net.createServer();
  listener.listen(0, '127.0.0.1');
  await once(listener, 'listening');
  const port = listener.address().port;
  await new Promise((done) => listener.close(done));
  return port;
}

async function until(operation, name, timeout_ms = readiness_timeout_ms) {
  const deadline = Date.now() + timeout_ms;
  while (Date.now() < deadline) {
    ensure_not_cancelled();
    if (application?.error || application?.child.exitCode !== null || application?.child.signalCode) {
      throw new Error('desktop_native_application_exited');
    }
    try { const value = await operation(); if (value) return value; } catch { /* Retain the bounded readiness failure. */ }
    await pause(100);
  }
  throw new Error('desktop_native_readiness_timeout:' + name);
}

async function launch(args, env) {
  ensure_not_cancelled();
  const child = spawn(binary, ['--aag-no-autostart', ...args], { cwd: root, env, stdio: 'ignore', windowsHide: true });
  const owned = { child, clients: [], start_ticks: null, error: null };
  owned.exit = new Promise((done) => child.once('close', (code, signal) => done({ code, signal })));
  child.once('error', (error) => { owned.error = error; });
  application = owned;
  const window = await until(async () => {
    const result = await driver('inspect');
    return result.main_window && result.tray_window && result;
  }, 'native_windows');
  owned.start_ticks = window.start_ticks;
  check('native process has no console attached', window.has_console === false);
  return window;
}

async function attach(origin) {
  const target = await until(async () => (await read_cdp_endpoint(origin, '/json/list')).find((item) => item.type === 'page' && item.url !== 'about:blank' && !item.url.startsWith('devtools:')), 'native_webview');
  const client = new cdp_client(new WebSocket(target.webSocketDebuggerUrl));
  application.clients.push(client);
  await client.connect();
  await client.send('Runtime.enable');
  await client.send('Page.enable');
  await until(() => client.eval_value('Boolean(document.querySelector(".aag-header")) && Boolean(window.__TAURI_INTERNALS__)'), 'native_interface');
  check('actual WebView uses native IPC rather than the browser demo', await client.eval_value('window.__aagDemoIsDemo !== true'));
  const profile = await until(async () => {
    const value = await stat(join(directory, 'config', 'webview', 'EBWebView')).catch(() => null);
    return value?.isDirectory() === true;
  }, 'webview_profile', 15_000);
  check('effective WebView profile is inside the disposable directory', profile === true);
  return client;
}

async function invoke(client, command, parameters = {}) {
  ensure_not_cancelled();
  return client.eval_value('window.__TAURI_INTERNALS__.invoke(' + JSON.stringify(command) + ',' + JSON.stringify(parameters) + ')');
}

async function bounded_exit(exit) {
  let timer;
  try {
    return await Promise.race([exit, new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error('desktop_native_exit_timeout')), exit_timeout_ms);
    })]);
  } finally { clearTimeout(timer); }
}

async function verify_live_state(before) {
  const after = await driver('preflight');
  check('native run preserves both Windows startup registration values', JSON.stringify(before.autostart_registration) === JSON.stringify(after.autostart_registration));
  check('native run preserves top-level live application-data metadata', JSON.stringify(before.live_directory) === JSON.stringify(after.live_directory));
  return after;
}

async function quit() {
  const owned = application;
  for (const client of owned.clients) client.web_socket.close();
  await driver('tray_quit', owned);
  const result = await bounded_exit(owned.exit);
  check('tray Quit exits gracefully with code zero', result.code === 0 && result.signal === null);
  application = null;
}

try {
  if (process.platform !== 'win32') throw new Error('desktop_native_requires_windows');
  if (process.argv.length > 3) throw new Error('desktop_native_unexpected_arguments');
  check('executable is compiled for the Windows GUI subsystem', inspect_windows_image(await readFile(binary)).windows_gui);
  before = await driver('preflight');
  if (before.elevated) throw new Error('desktop_native_requires_nonelevated_webview_host');
  if (before.running_count !== 0 || before.single_instance_present !== false) throw new Error('desktop_native_close_existing_aag_first');
  directory = await mkdtemp(join(tmpdir(), 'aag_desktop_native_'));
  const home = join(directory, 'home');
  const sentinels = new Map([
    ['.codex/auth.json', '{"native_auth":"fixture"}'],
    ['.codex/config.toml', '# native fixture\nmodel = "native_fixture"\n'],
    ['.codex-cli/auth.json', '{"isolated_auth":"fixture"}'],
    ['.codex-cli/sessions/fixture.jsonl', '{"session":"fixture"}\n'],
    ['.claude/credentials.json', '{"native_auth":"fixture"}'],
    ['.aag/mirrors/claude/sessions/fixture.jsonl', '{"session":"fixture"}\n'],
  ]);
  for (const [relative, content] of sentinels) {
    const path = join(home, relative);
    await mkdir(dirname(path), { recursive: true });
    await writeFile(path, content);
  }
  const debug_port = await free_port();
  let gateway_port = await free_port();
  for (let attempt = 0; gateway_port === debug_port && attempt < 3; attempt++) gateway_port = await free_port();
  if (gateway_port === debug_port) throw new Error('desktop_native_distinct_ports_unavailable');
  const origin = 'http://127.0.0.1:' + debug_port;
  const gateway = 'http://127.0.0.1:' + gateway_port;
  const env = { ...process.env };
  for (const key of Object.keys(env)) {
    if (/^(AAG_|WEBVIEW2_|CC_SWITCH_DB_PATH$|CODEX_HOME$|CLAUDE_CONFIG_DIR$)/i.test(key)) delete env[key];
  }
  Object.assign(env, {
    AAG_CONFIG_DIR: join(directory, 'config'), AAG_HOME_DIR: home,
    CC_SWITCH_DB_PATH: join(home, '.cc-switch/absent.db'),
    WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: '--remote-debugging-port=' + debug_port + ' --remote-debugging-address=127.0.0.1 --no-first-run',
  });
  state = 'failed';
  current_step = 'native_launch';
  await launch([], env);
  await until(async () => (await driver('inspect')).visible, 'normal_window_visible');
  check('normal launch shows the main native window', true);
  current_step = 'native_attach';
  let main = await attach(origin);
  current_step = 'proxy_config_save';
  await invoke(main, 'proxy_config_save', { config: { host: '127.0.0.1', port: gateway_port, authToken: fixture_token, enabled: true } });
  const suppliers = await invoke(main, 'supplier_list');
  current_step = 'supplier_save';
  const saved = await invoke(main, 'supplier_save', { input: { id: 'desktop_native_fixture', expectedGeneration: suppliers.generation, name: 'Desktop lifecycle fixture', enabled: false, endpoints: [] } });
  check('native supplier save completes without a mirror setup error', saved.setupError === null);
  current_step = 'proxy_start';
  await invoke(main, 'proxy_start');
  current_step = 'local_health';
  const health = await fetch(gateway + '/health', { headers: { authorization: 'Bearer ' + fixture_token }, signal: AbortSignal.timeout(5000) });
  check('authenticated local health succeeds without an upstream generation', health.ok && (await health.json()).status === 'ok');
  const rejected = await fetch(gateway + '/health', { signal: AbortSignal.timeout(5000) });
  check('local health still rejects a missing gateway token', rejected.status === 401);
  current_step = 'native_diagnostics';
  await rejected.body?.cancel();
  await main.eval_value('document.querySelector("[data-testid=diagnostics_open]").click(); true');
  await until(() => main.eval_value('Boolean(document.querySelector("[data-testid=diagnostics_backdrop]"))'), 'diagnostics_dialog');
  check('native diagnostics opens through the actual interface', await main.eval_value('Boolean(document.querySelector("[role=dialog]"))'));
  await mkdir(join(root, 'tmp'), { recursive: true });
  const screenshot = await main.send('Page.captureScreenshot', { format: 'png' });
  await writeFile(join(root, 'tmp/desktop_native_normal.png'), Buffer.from(screenshot.result.data, 'base64'));
  await main.eval_value('document.querySelector("[data-testid=diagnostics_backdrop]").click(); true');
  await driver('close');
  current_step = 'native_lifecycle';
  await until(async () => !(await driver('inspect')).visible, 'close_to_tray');
  check('closing the native window leaves the gateway running', (await invoke(main, 'proxy_status')).running);
  await driver('tray_show');
  await until(async () => (await driver('inspect')).visible, 'tray_reopen');
  check('tray left-click reopens the main native window', true);
  await driver('tray_hide');
  await until(async () => !(await driver('inspect')).visible, 'tray_hide');
  check('tray Hide hides without exiting the application', application.child.exitCode === null);
  const forwarded = spawn(binary, ['--aag-no-autostart'], { cwd: root, env, stdio: 'ignore', windowsHide: true });
  secondary = forwarded;
  const forwarded_exit = await new Promise((done, reject) => {
    const timer = setTimeout(() => { forwarded.kill(); reject(new Error('desktop_native_second_instance_timeout')); }, 10_000);
    forwarded.once('error', (error) => { clearTimeout(timer); reject(error); });
    forwarded.once('close', (code) => { clearTimeout(timer); done(code); });
  });
  secondary = null;
  check('second launch forwards to the existing instance and exits', forwarded_exit === 0);
  await until(async () => (await driver('inspect')).visible, 'single_instance_reopen');
  await invoke(main, 'proxy_stop');
  check('explicit proxy stop is observed as stopped', !(await invoke(main, 'proxy_status')).running);
  await invoke(main, 'proxy_start');
  check('explicit proxy restart succeeds on the same saved port', (await invoke(main, 'proxy_status')).running);
  check('normal WebView has no rendering exceptions after CDP attachment', application.clients.every((client) => client.exceptions.length === 0));
  await quit();
  await verify_live_state(before);
  let released = false;
  try { await fetch(gateway + '/health', { signal: AbortSignal.timeout(2000) }); } catch { released = true; }
  check('graceful application exit releases the proxy listener', released);
  const tray_only = await launch(['--aag-autostart'], env);
  current_step = 'tray_only_attach';
  check('autostart-mode launch remains tray-only', !tray_only.visible);
  main = await attach(origin);
  await until(async () => (await invoke(main, 'proxy_status')).running, 'proxy_autostart');
  const retained = await invoke(main, 'supplier_list');
  check('saved supplier survives application restart', retained.suppliers.some((item) => item.id === 'desktop_native_fixture' && item.name === 'Desktop lifecycle fixture' && item.enabled === false));
  const status = await invoke(main, 'proxy_status');
  check('saved gateway configuration survives restart', status.config.port === gateway_port && status.config.authToken === fixture_token);
  await driver('tray_show');
  await until(async () => (await driver('inspect')).visible, 'autostart_reopen');
  check('tray-only launch can reopen its native window', true);
  check('tray-only WebView has no rendering exceptions after CDP attachment', application.clients.every((client) => client.exceptions.length === 0));
  await quit();
  for (const [relative, content] of sentinels) check('client fixture preserved: ' + relative, (await readFile(join(home, relative), 'utf8')) === content);
  const after = await verify_live_state(before);
  check('all owned native app instances have exited', after.running_count === 0 && after.single_instance_present === false);
  state = 'passed';
} catch (error) {
  const code = error.message?.startsWith('desktop_native_') ? error.message
    : typeof error.code === 'string' && /^[A-Z0-9_]+$/.test(error.code) ? error.code : 'desktop_native_operation_failed';
  process.stderr.write('DESKTOP_NATIVE_STEP=' + current_step + '\n');
  process.stderr.write('DESKTOP_NATIVE_ERROR=' + code + '\n');
  process.exitCode = 1;
} finally {
  if (secondary) {
    retain_state = true;
    try { await stop_owned_child(secondary); }
    catch { retain_state = true; state = 'cleanup_failed'; process.exitCode = 1; }
  }
  if (application) {
    retain_state = true;
    process.stderr.write('DESKTOP_NATIVE_DESCENDANT_CLEANUP=not_verified; disposable state retained after abnormal exit\n');
    for (const client of application.clients) client.web_socket.close();
    try { await stop_owned_child(application.child); await bounded_exit(application.exit); }
    catch { application.child.unref(); retain_state = true; state = 'cleanup_failed'; process.exitCode = 1; }
  }
  if (directory && before) {
    try {
      const after = await driver('preflight', null, { cleanup: true });
      if (after.running_count !== 0 || after.single_instance_present !== false
          || JSON.stringify(before.autostart_registration) !== JSON.stringify(after.autostart_registration)
          || JSON.stringify(before.live_directory) !== JSON.stringify(after.live_directory)) {
        throw new Error('desktop_native_persistent_state_changed');
      }
    } catch {
      process.stderr.write('DESKTOP_NATIVE_ISOLATION=unproven_or_changed\n');
      retain_state = true; state = 'cleanup_failed'; process.exitCode = 1;
    }
  }
  if (directory && !retain_state) {
    try { await rm(directory, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 }); }
    catch { retain_state = true; state = 'cleanup_failed'; process.exitCode = 1; }
  }
  if (cancellation_code && state !== 'cleanup_failed') {
    state = 'cancelled';
    process.exitCode = cancellation_code;
  }
  process.removeListener('SIGINT', cancel_int);
  process.removeListener('SIGTERM', cancel_term);
  process.stdout.write('DESKTOP_NATIVE_STATUS=' + state + ' CHECKS=' + checks + ' STATE=' + (retain_state ? 'retained' : 'removed') + ' DESCENDANT_CLEANUP=not_verified\n');
}
