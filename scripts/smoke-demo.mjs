#!/usr/bin/env node
/// Browser smoke test for the AAG demo mode (no Tauri needed).
///
/// Boots (or reuses) a vite dev server on :1420, drives a headless Edge
/// instance over the DevTools protocol, and asserts the app actually
/// renders and the key interactions work. Catches runtime crashes that
/// tsc / vite build cannot see (e.g. the Phase 22i TDZ white-screen).
///
/// Usage:
///   node scripts/smoke-demo.mjs [--keep]        from the repo root
///     --keep   leave the browser + vite server running afterwards
///
/// Exit code 0 = all checks passed; 1 = any failure (details on stderr).
import { spawn } from 'node:child_process';
import { existsSync, mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const DEMO_URL = 'http://localhost:1420/';
const CDP_PORT = 9223;
const VITE_PORT = 1420;

const log = (msg) => process.stdout.write(`${msg}\n`);
const fail = (msg) => {
  process.stderr.write(`FAIL: ${msg}\n`);
};

// ---------------------------------------------------------------------------
// 1. Find a Chromium-family browser.
// ---------------------------------------------------------------------------
function findBrowser() {
  const candidates = [
    'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe',
    'C:/Program Files/Microsoft/Edge/Application/msedge.exe',
    'C:/Program Files/Google/Chrome/Application/chrome.exe',
    'C:/Program Files (x86)/Google/Chrome/Application/chrome.exe',
  ];
  for (const c of candidates) if (existsSync(c)) return c;
  return null;
}

// ---------------------------------------------------------------------------
// 2. Ensure the vite dev server is up (start one if :1420 isn't answering).
// ---------------------------------------------------------------------------
async function httpOk(url, timeoutMs = 800) {
  try {
    const ac = new AbortController();
    const t = setTimeout(() => ac.abort(), timeoutMs);
    const res = await fetch(url, { signal: ac.signal });
    clearTimeout(t);
    return res.ok;
  } catch {
    return false;
  }
}

async function ensureVite({ keep }) {
  if (await httpOk(DEMO_URL)) return null; // already running — reuse
  log('[smoke] starting vite dev server…');
  const child = spawn('npm', ['run', 'dev', '--prefix', 'ui'], {
    // Designed to run from the repo root (npm run ui:smoke).
    cwd: process.cwd(),
    stdio: ['ignore', 'pipe', 'pipe'],
    shell: true,
  });
  child.stdout.on('data', () => {});
  child.stderr.on('data', () => {});
  const deadline = Date.now() + 20_000;
  while (Date.now() < deadline) {
    if (await httpOk(DEMO_URL)) return keep ? child : child;
    await sleep(400);
  }
  child.kill();
  throw new Error('vite dev server did not come up within 20s');
}

// ---------------------------------------------------------------------------
// 3. Launch headless browser with remote debugging.
// ---------------------------------------------------------------------------
async function launchBrowser(exe, { keep }) {
  const profile = mkdtempSync(join(tmpdir(), 'aag-smoke-'));
  const child = spawn(
    exe,
    [
      '--headless=new',
      '--disable-gpu',
      '--no-first-run',
      '--no-default-browser-check',
      `--user-data-dir=${profile}`,
      `--remote-debugging-port=${CDP_PORT}`,
      'about:blank',
    ],
    { stdio: 'ignore' },
  );
  const deadline = Date.now() + 15_000;
  while (Date.now() < deadline) {
    try {
      const res = await fetch(`http://127.0.0.1:${CDP_PORT}/json/list`);
      if (res.ok) return child;
    } catch {
      /* not up yet */
    }
    await sleep(300);
  }
  child.kill();
  throw new Error('headless browser did not expose CDP within 15s');
}

// ---------------------------------------------------------------------------
// 4. Minimal CDP client (native WebSocket, no deps).
// ---------------------------------------------------------------------------
class Cdp {
  constructor(ws) {
    this.ws = ws;
    this.id = 0;
    this.pending = new Map();
    this.exceptions = [];
    this.consoleErrors = [];
    ws.addEventListener('message', (ev) => this.#onMessage(ev));
  }

  #onMessage(ev) {
    const msg = JSON.parse(ev.data);
    if (msg.id && this.pending.has(msg.id)) {
      this.pending.get(msg.id)(msg);
      this.pending.delete(msg.id);
      return;
    }
    if (msg.method === 'Runtime.exceptionThrown') {
      const d = msg.params.exceptionDetails;
      this.exceptions.push(d.exception?.description ?? d.text ?? 'unknown');
    }
    if (
      msg.method === 'Runtime.consoleAPICalled' &&
      msg.params.type === 'error'
    ) {
      this.consoleErrors.push(
        msg.params.args.map((a) => a.value ?? a.description ?? '').join(' '),
      );
    }
  }

  connect() {
    return new Promise((resolve, reject) => {
      this.ws.addEventListener('open', resolve, { once: true });
      this.ws.addEventListener('error', reject, { once: true });
    });
  }

  send(method, params = {}) {
    return new Promise((resolve) => {
      const mid = ++this.id;
      this.pending.set(mid, resolve);
      this.ws.send(JSON.stringify({ id: mid, method, params }));
    });
  }

  async eval(expression) {
    const res = await this.send('Runtime.evaluate', {
      expression,
      returnByValue: true,
      // Async IIFE checks resolve via awaitPromise, not as plain objects.
      awaitPromise: true,
    });
    if (res.result?.exceptionDetails) {
      throw new Error(res.result.exceptionDetails.text);
    }
    return res.result?.result?.value;
  }
}

async function attachPage() {
  const list = await (await fetch(`http://127.0.0.1:${CDP_PORT}/json/list`)).json();
  const page = list.find((t) => t.type === 'page');
  if (!page) throw new Error('no page target in CDP list');
  const ws = new WebSocket(page.webSocketDebuggerUrl);
  const cdp = new Cdp(ws);
  await cdp.connect();
  await cdp.send('Runtime.enable');
  await cdp.send('Page.enable');
  return cdp;
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

let failed = false;
function check(name, ok, detail = '') {
  if (ok) {
    log(`  ✓ ${name}`);
  } else {
    failed = true;
    fail(`${name}${detail ? ` — ${detail}` : ''}`);
  }
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------
const keep = process.argv.includes('--keep');
let viteProc = null;
let browserProc = null;

try {
  const exe = findBrowser();
  check('browser found', Boolean(exe), 'no Edge/Chrome in standard paths');
  if (!exe) process.exit(1);

  viteProc = await ensureVite({ keep });
  browserProc = await launchBrowser(exe, { keep });
  const cdp = await attachPage();

  // --- load + render ---
  log('[smoke] loading demo page…');
  await cdp.send('Page.navigate', { url: DEMO_URL });
  await sleep(6000);

  const bodyText = (await cdp.eval('document.body.innerText')) ?? '';
  check('page renders (no "UI error" crash screen)', !bodyText.includes('UI error'));
  check('header present', bodyText.includes('AI Agent Gateway'));
  check('Claude page rendered', bodyText.includes('AAG 网关') || bodyText.includes('AAG Gateway'));

  // --- route editor landmarks ---
  const hopHandles = await cdp.eval("document.querySelectorAll('.hop-drag-handle').length");
  check('hop drag handles rendered', hopHandles > 0, `count=${hopHandles}`);

  // --- start the gateway ---
  await cdp.eval("[...document.querySelectorAll('button')].find(b => b.textContent.trim() === '启动' || b.textContent.trim() === 'Start')?.click(); 1");
  await sleep(1500);
  const runningText = (await cdp.eval('document.body.innerText')) ?? '';
  check(
    'gateway starts (running pill)',
    runningText.includes('● 运行中') || runningText.includes('● running'),
  );

  // --- restart button appears in route toolbar only while running ---
  const restartBtn = await cdp.eval(
    "[...document.querySelectorAll('button')].some(b => b.textContent.includes('重启代理') || b.textContent.toLowerCase().includes('restart proxy'))",
  );
  check('route-toolbar restart button visible while running', restartBtn);

  // --- route rename (auto-answer the prompt) ---
  // Auto-respond to window.prompt with a fresh alias, then click Rename on
  // the first route card and assert the alias changed in the DOM.
  const renamed = await cdp.eval(`(async () => {
    const before = document.querySelector('.route-card-title strong')?.textContent ?? '';
    if (!before) return 'no-route';
    window.prompt = () => before + '-renamed';
    [...document.querySelectorAll('.route-card-actions button')]
      .find(b => b.textContent.trim() === '重命名' || b.textContent.trim() === 'Rename')
      ?.click();
    await new Promise(r => setTimeout(r, 800));
    const after = document.querySelector('.route-card-title strong')?.textContent ?? '';
    return after === before + '-renamed' ? 'ok' : before + ' -> ' + after;
  })()`);
  check('route rename updates the alias', renamed === 'ok', String(renamed));

  // --- upstream enable toggle (UpstreamList switch) ---
  const toggled = await cdp.eval(`(async () => {
    const row = document.querySelector('.provider-list .provider-row');
    if (!row) return 'no-row';
    const sw = row.querySelector('.toggle-switch input');
    if (!sw) return 'no-switch';
    const wasChecked = sw.checked;
    sw.click();
    await new Promise(r => setTimeout(r, 800));
    const nowDisabledRow = document.querySelector('.provider-row.disabled');
    const sw2 = document.querySelector('.provider-list .provider-row .toggle-switch input');
    const flipped = sw2 && sw2.checked !== wasChecked;
    return flipped ? 'ok' : 'not-flipped' + (nowDisabledRow ? '+row-class-ok' : '');
  })()`);
  check('upstream enable toggle flips + persists', toggled === 'ok', String(toggled));

  // --- hop keyboard reorder (focus a handle, press ArrowDown) ---
  const reordered = await cdp.eval(`(async () => {
    const firstModel = document.querySelector('.hop-row[data-hop-idx="0"]')?.dataset.routeAlias;
    if (!firstModel) return 'no-hop';
    const aliasOf = (i) => document.querySelector('.hop-row[data-hop-idx="' + i + '"]')?.getAttribute('data-route-alias');
    const providerAt = (alias, i) =>
      document.querySelector('.hop-row[data-route-alias="' + alias + '"][data-hop-idx="' + i + '"] select')?.value ?? '';
    const alias = aliasOf(0);
    const p0 = providerAt(alias, 0);
    const p1 = providerAt(alias, 1);
    if (p0 === '' || p1 === '') return 'needs-two-hops-with-providers';
    const handle = document.querySelector('.hop-row[data-route-alias="' + alias + '"][data-hop-idx="0"] .hop-drag-handle');
    handle?.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowDown', bubbles: true, cancelable: true }));
    await new Promise(r => setTimeout(r, 500));
    const moved0 = providerAt(alias, 1);
    return moved0 === p0 ? 'ok' : 'expected ' + p0 + ' at idx1, got ' + moved0;
  })()`);
  check(
    'hop keyboard ArrowDown reorders',
    reordered === 'ok',
    String(reordered),
  );

  // --- gateway settings modal opens + saves ---
  const gwSettings = await cdp.eval(`(async () => {
    const gear = [...document.querySelectorAll('.gateway-panel button')].find(
      b => b.getAttribute('aria-label')?.includes('设置') || b.getAttribute('aria-label')?.toLowerCase().includes('settings'),
    );
    if (!gear) return 'no-gear';
    gear.click();
    await new Promise(r => setTimeout(r, 600));
    const modal = document.querySelector('.modal');
    if (!modal) return 'no-modal';
    const portInput = modal.querySelector('input[type="number"]');
    if (!portInput || portInput.value !== '8989') return 'port=' + (portInput?.value ?? 'none');
    // React-controlled inputs need the native value setter + input event
    // (a bare dispatchEvent(new Event('input')) is swallowed by React 18).
    const setValue = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set;
    setValue.call(portInput, '8990');
    portInput.dispatchEvent(new Event('input', { bubbles: true }));
    await new Promise(r => setTimeout(r, 200));
    [...modal.querySelectorAll('button')].find(b => b.textContent.trim() === '保存' || b.textContent.trim() === 'Save')?.click();
    await new Promise(r => setTimeout(r, 800));
    const urlRow = document.querySelector('.gateway-panel .endpoint-value')?.textContent ?? '';
    return urlRow.includes(':8990') ? 'ok' : 'url=' + urlRow;
  })()`);
  check(
    'gateway settings modal edits the port',
    gwSettings === 'ok',
    String(gwSettings),
  );

  // --- tab switch to Codex ---
  await cdp.eval("[...document.querySelectorAll('button')].find(b => b.textContent.trim() === 'Codex')?.click(); 1");
  await sleep(1500);
  const codexText = (await cdp.eval('document.body.innerText')) ?? '';
  check('Codex tab renders mirror row', codexText.includes('~/.codex-cli'));

  // --- exceptions ---
  const exc = [...cdp.exceptions];
  check('zero uncaught exceptions', exc.length === 0, exc[0]?.slice(0, 200));

  await cdp.eval('1').catch(() => {});
} catch (e) {
  failed = true;
  fail(e.message ?? String(e));
} finally {
  if (!keep) {
    if (browserProc) browserProc.kill();
    if (viteProc) viteProc.kill();
    // Give Windows a beat to release the ports before the process exits.
    await sleep(500);
  } else {
    log('[smoke] --keep: leaving vite + browser running.');
  }
}

if (failed) {
  process.exit(1);
}
log('[smoke] all checks passed.');
process.exit(0);
