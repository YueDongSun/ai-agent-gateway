// Opt-in Windows native-window acceptance. Requires a built AAG executable.
// Uses an isolated AAG home and WebView2 profile; never starts DSH or a provider.
import assert from 'node:assert/strict';
import { spawn, execFileSync } from 'node:child_process';
import { mkdtemp, mkdir, rm, writeFile, readFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, resolve, basename } from 'node:path';
import net from 'node:net';
import { once } from 'node:events';
import { cdp_client } from '../ui_smoke/cdp_client.mjs';
import { cdp_timeout_ms, read_cdp_endpoint } from '../ui_smoke/cdp_endpoint.mjs';

if (process.platform !== 'win32') throw new Error('native_market_test_requires_windows');
const powershell = join(process.env.SystemRoot || 'C:/Windows', 'System32/WindowsPowerShell/v1.0/powershell.exe');
// Elevated hosts can never open the CDP endpoint: per Microsoft's WebView2
// security rules, an elevated host process ignores WEBVIEW2_* environment
// overrides AND HKCU policy overrides (only HKLM policy or flags set through
// the in-app WebView2 API are honored). Fail fast with the reason instead of
// timing out after 45 seconds.
const elevated = /True/i.test(execFileSync(powershell, ['-NoProfile', '-NonInteractive', '-Command',
  '([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)'],
  { encoding: 'utf8', windowsHide: true }).trim());
if (elevated) {
  throw new Error('native_market_test_requires_nonelevated_host: an elevated host ignores WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS, so the debug endpoint cannot open; rerun from a standard (non-admin) shell');
}
const process_count = Number(execFileSync(powershell, ['-NoProfile', '-NonInteractive', '-Command', "@(Get-Process 'ai-agent-gateway' -ErrorAction SilentlyContinue).Count"], { encoding: 'utf8', windowsHide: true }).trim());
if (process_count !== 0) throw new Error('close_existing_aag_before_native_acceptance');
const expected_count = JSON.parse(await readFile('shared/dsh_market_catalog.json', 'utf8')).entry_count;
const binary = resolve(process.argv[2] || 'src-tauri/target/debug/ai-agent-gateway.exe');
const temp_root = resolve(tmpdir());
const directory = await mkdtemp(join(temp_root, 'aag_market_native_'));
assert.equal(dirname(directory), temp_root);
assert.ok(basename(directory).startsWith('aag_market_native_'));
const listener = net.createServer();
listener.listen(0, '127.0.0.1');
await once(listener, 'listening');
const port = listener.address().port;
await new Promise((done) => listener.close(done));
const origin = 'http://127.0.0.1:' + port;
const child = spawn(binary, ['--aag-autostart'], {
  windowsHide: true,
  env: {
    ...process.env,
    AAG_CONFIG_DIR: join(directory, 'config'),
    AAG_HOME_DIR: join(directory, 'home'),
    ...(process.env.AAG_DSH_TEST_RUNTIME_DIR ? { AAG_DSH_RUNTIME_DIR: dirname(resolve(process.env.AAG_DSH_TEST_RUNTIME_DIR)), PATH: dirname(process.execPath) + ';' + (process.env.PATH || '') } : {}),
    WEBVIEW2_USER_DATA_FOLDER: join(directory, 'webview'),
    WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: '--remote-debugging-port=' + port + ' --remote-debugging-address=127.0.0.1 --no-first-run',
  },
  stdio: 'ignore',
});
let spawn_error = null;
child.on('error', (error) => { spawn_error = error; });
const exited = once(child, 'exit');
const clients = [];
let checks = 0;
function check(name, value) {
  assert.ok(value, name); checks += 1; process.stdout.write('PASS ' + name + '\n');
}
async function until(operation, timeout = 30000) {
  const deadline = Date.now() + timeout;
  let last_error;
  while (Date.now() < deadline) {
    if (spawn_error || child.exitCode !== null) throw spawn_error ?? new Error('native_application_exited');
    try { const result = await operation(); if (result) return result; } catch (error) { last_error = error; }
    await new Promise((done) => setTimeout(done, 100));
  }
  throw new Error('native_readiness_timeout' + (last_error ? ': ' + last_error.message : ''));
}
async function targets(timeout_ms = cdp_timeout_ms) {
  try {
    return await read_cdp_endpoint(origin, '/json/list', { timeout_ms });
  } catch (error) {
    throw new Error('native_debug_endpoint_unavailable: ' + error.message);
  }
}
async function attach(target) {
  const client = new cdp_client(new WebSocket(target.webSocketDebuggerUrl));
  clients.push(client);
  await client.connect();
  await client.send('Runtime.enable');
  await client.send('Page.enable');
  return client;
}
try {
  const main_target = await until(async () => (await targets()).find((target) => target.type === 'page' && !target.url.includes('devtools') && target.url !== 'about:blank'), 45000);
  const main = await attach(main_target);
  await until(() => main.eval_value("Boolean(document.querySelector('[data-testid=\"dsh_nav\"]'))"));
  check('real native app has no synthetic IPC backend', await main.eval_value('window.__aagDemoIsDemo !== true && Boolean(window.__TAURI_INTERNALS__)'));
  if (process.env.AAG_DSH_TEST_RUNTIME_DIR) {
    await main.eval_value("(async()=>{await window.__TAURI_INTERNALS__.invoke('provider_save',{appType:'claude_code',provider:{id:'native_setup_fixture',name:'Native setup fixture',settingsConfig:{env:{ANTHROPIC_BASE_URL:'http://127.0.0.1:1',ANTHROPIC_AUTH_TOKEN:'native_setup_fixture_key',ANTHROPIC_MODEL:'native_setup_fixture_model'}},inFailoverQueue:false}});await window.__TAURI_INTERNALS__.invoke('provider_switch',{appType:'claude_code',id:'native_setup_fixture'});return true;})()");
  }
  await main.eval_value("document.querySelector('[data-testid=\"dsh_nav\"]').click(); true");
  await until(() => main.eval_value("Boolean(document.querySelector('[data-testid=\"dsh_open_marketplace\"]'))"));
  if (process.env.AAG_DSH_TEST_RUNTIME_DIR) {
    await until(() => main.eval_value("Boolean(document.querySelector('[data-testid=\"dsh_setup_review\"]:not(:disabled)'))"));
    const untouched = await main.eval_value("(async()=>{const s=await window.__TAURI_INTERNALS__.invoke('dsh_get_snapshot');return !s.initialized&&s.runtime.phase==='stopped';})()");
    check('native installation detection is passive', untouched);
    await main.eval_value("document.querySelector('[data-testid=\"dsh_setup_review\"]').click();true");
    await until(() => main.eval_value("Boolean(document.querySelector('[data-testid=\"dsh_preview_dialog\"]'))"));
    check('native setup requires reviewed confirmation', await main.eval_value("document.querySelector('[data-testid=\"dsh_confirm\"]').disabled"));
    await main.eval_value("document.querySelector('[data-testid=\"dsh_acknowledge\"]').click();true");
    await until(() => main.eval_value("document.querySelector('[data-testid=\"dsh_confirm\"]')?.disabled===false"));
    await main.eval_value("document.querySelector('[data-testid=\"dsh_confirm\"]').click();true");
    await until(() => main.eval_value("(async()=>{const s=await window.__TAURI_INTERNALS__.invoke('dsh_get_snapshot');return s.initialized;})()"));
    check('native setup adopts detected runtime and current supplier', await main.eval_value("(async()=>{const s=await window.__TAURI_INTERNALS__.invoke('dsh_get_snapshot');return Boolean(s.settings.node_path&&s.settings.dsh_entry_path&&s.settings.pnpm_entry_path)&&s.settings.upstream_id==='claude:native_setup_fixture'&&s.settings.model_id==='native_setup_fixture_model';})()"));
    check('native setup neither launches nor exposes credentials', await main.eval_value("(async()=>{const s=await window.__TAURI_INTERNALS__.invoke('dsh_get_snapshot');return s.runtime.phase==='stopped'&&!JSON.stringify(s).includes('native_setup_fixture_key');})()"));
    await mkdir(resolve('tmp'), { recursive: true });
    const setup_image = await main.send('Page.captureScreenshot', { format: 'png' });
    await writeFile(resolve('tmp/dsh_onboarding_native.png'), Buffer.from(setup_image.result.data, 'base64'));
  }
  const before = (await targets()).filter((target) => target.url.includes('view=dsh_market')).length;
  check('market not automatically opened by visiting DSH', before === 0);
  await main.eval_value("document.querySelector('[data-testid=\"dsh_open_marketplace\"]').click(); true");
  const market_target = await until(async () => (await targets()).find((target) => target.url.includes('view=dsh_market')));
  const market = await attach(market_target);
  await until(() => market.eval_value("Boolean(document.querySelector('[data-testid=\"market_card\"]'))"));
  check('button creates a distinct native WebView', market_target.id !== main_target.id);
  check('native market reads the real catalog', await market.eval_value("document.body.innerText.includes(" + JSON.stringify(expected_count.toLocaleString('en-US')) + ") && window.__aagDemoIsDemo !== true"));
  check('market cannot read upstream provider credentials', await market.eval_value("(async()=>{try{await window.__TAURI_INTERNALS__.invoke('provider_list',{app_type:'claude_code'});return false;}catch(error){return /not allowed|untrusted_native_subject|denied/i.test(String(error));}})()"));
  check('market cannot execute DSH plugins', await market.eval_value("(async()=>{try{await window.__TAURI_INTERNALS__.invoke('dsh_execute_operation',{preview_id:'not_authorized'});return false;}catch(error){return /not allowed|untrusted_native_subject|denied/i.test(String(error));}})()"));
  check('market source command resolves IDs instead of URLs', await market.eval_value("(async()=>{try{await window.__TAURI_INTERNALS__.invoke('dsh_market_open_source',{entry_id:'missing_entry',source_kind:'repository'});return false;}catch(error){return String(error).includes('dsh_market:entry_not_found');}})()"));
  await market.eval_value("document.querySelector('[data-testid=\"market_locale_zh\"]').click(); true");
  check('native marketplace switches to Chinese', await market.eval_value("document.body.innerText.includes('插件')"));
  await main.eval_value("document.querySelector('[data-testid=\"dsh_open_marketplace\"]').click(); true");
  await new Promise((done) => setTimeout(done, 350));
  const reopened = (await targets()).filter((target) => target.url.includes('view=dsh_market'));
  check('repeated button click reuses one native market', reopened.length === 1 && reopened[0].id === market_target.id);
  check('reopening preserves market language state', await market.eval_value("document.documentElement.lang === 'zh-CN' || document.documentElement.lang === 'zh'"));
  await mkdir(resolve('tmp'), { recursive: true });
  const screenshot = await market.send('Page.captureScreenshot', { format: 'png' });
  await writeFile(resolve('tmp/dsh_market_native.png'), Buffer.from(screenshot.result.data, 'base64'));
  check('no native market render exception', market.exceptions.length === 0);
  process.stdout.write('NATIVE_MARKET_CHECKS=' + checks + '\n');
} finally {
  for (const client of clients) client.web_socket.close();
  if (child.exitCode === null) child.kill();
  let exit_timer;
  try { await Promise.race([exited, new Promise((_, reject) => { exit_timer = setTimeout(() => reject(new Error('native_exit_timeout')), 10000); })]); } finally { clearTimeout(exit_timer); }
  let cleaned = false;
  for (let attempt = 0; attempt < 30; attempt += 1) {
    try { await rm(directory, { recursive: true, force: true }); cleaned = true; break; }
    catch { await new Promise((done) => setTimeout(done, 200)); }
  }
  if (!cleaned) throw new Error('native_profile_cleanup_failed');
  process.stdout.write('NATIVE_OWNED_RESOURCES_CLOSED\n');
}
