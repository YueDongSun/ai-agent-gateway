import { attach_page } from './cdp_client.mjs';
import { check_in_page } from './page_checks.mjs';
import { create_smoke_runtime, find_browser, sleep } from './smoke_runtime.mjs';

let checks = 0; let failures = 0;
const check = (name, ok, detail = '') => { checks++; if (!ok) failures++; process.stdout.write(`${ok ? 'PASS' : 'FAIL'} ${name}${detail ? ': ' + detail : ''}\n`); };
const browser = find_browser();
if (!browser) throw new Error('A supported isolated headless browser is required');
const runtime = await create_smoke_runtime(browser, { built_demo: process.argv.includes('--built-demo') });
try {
  const cdp = await attach_page(runtime.cdp_origin);
  await cdp.send('Page.navigate', { url: runtime.demo_url });
  for (let attempt = 0; attempt < 100 && !await cdp.eval_value('window.__aagDemoIsDemo && !!document.querySelector("[data-testid=suppliers_page]")'); attempt++) await sleep(100);
  await cdp.eval_value(`(() => {
    if (window.__aagDemoIsDemo !== true) throw new Error('Synthetic IPC required');
    const bridge = window.__TAURI_INTERNALS__; const original = bridge.invoke;
    const state = { original, cached: false, error: null, block: true, refresh_fails: false, release: null, refreshes: [], snapshots: [] };
    state.wait = async (predicate) => { for (let n = 0; n < 100; n++) { if (predicate()) return true; await new Promise((r) => setTimeout(r, 40)); } return false; };
    state.fresh = () => ({ source: 'lmarena', fetchedAt: Math.floor(Date.now() / 1000), stale: false, lastError: null });
    bridge.invoke = (command, args) => {
      if (command === 'provider_current') return Promise.resolve('fixture-current-plan');
      if (command === 'provider_get') return Promise.resolve({ settingsConfig: { model: 'demo-current' } });
      if (command === 'supplier_arena_snapshot') {
        state.snapshots.push(args);
        return original(command, args).then((snapshot) => ({ ...snapshot,
          source: { ...snapshot.source, fetchedAt: state.cached ? snapshot.source.fetchedAt : null, stale: !state.cached || state.error !== null, lastError: state.error },
          entries: state.cached ? snapshot.entries : [], selected: state.cached ? snapshot.selected : null,
          candidates: snapshot.candidates.map((candidate) => ({ ...candidate, arena: state.cached ? candidate.arena : null })),
          fableCandidates: state.cached ? snapshot.fableCandidates : [],
        }));
      }
      if (command === 'supplier_catalog_refresh') {
        state.refreshes.push(args);
        if (state.refresh_fails) { state.error = 'fetch_http_error'; return Promise.resolve({ ...state.fresh(), stale: true, lastError: state.error }); }
        if (state.block) return new Promise((resolve) => { state.release = () => { state.cached = true; state.error = null; resolve(state.fresh()); }; });
        state.cached = true; state.error = null; return Promise.resolve(state.fresh());
      }
      return original(command, args);
    };
    window.__arena_cache = state;
    document.querySelector('[data-testid="app-tab-codex"]').click();
  })()`);
  await check_in_page(cdp, check, 'a cold graph loads public ratings automatically with stale-only admission', async () => {
    const state = window.__arena_cache;
    if (!await state.wait(() => state.refreshes.length === 1 && document.querySelector('[data-testid="arena-state"]')?.textContent === 'Loading ratings')) return 'automatic-refresh-not-started';
    document.dispatchEvent(new Event('visibilitychange'));
    document.querySelector('[data-testid="arena-needs-tools"]').click();
    document.querySelector('[data-testid="arena-refresh"]').click();
    await new Promise((r) => setTimeout(r, 100));
    const admitted_once = state.refreshes.length === 1 && state.refreshes[0].onlyIfStale === true && state.refreshes[0].input.source === 'lmarena';
    state.release(); state.block = false;
    const rendered = await state.wait(() => Boolean(document.querySelector('.arena_selected circle')));
    return { ok: admitted_once && rendered && state.snapshots.at(-1)?.needsTools === true && !document.querySelector('[data-testid="arena-refresh-error"]') };
  });
  await check_in_page(cdp, check, 'failed resolved refreshes keep validated ratings and show a retry state', async () => {
    const state = window.__arena_cache; state.refresh_fails = true;
    document.querySelector('[data-testid="arena-refresh"]').click();
    const failed = await state.wait(() => document.querySelector('[data-testid="arena-refresh-error"]')?.textContent.includes('source returned an error'));
    const before = state.refreshes.length;
    document.dispatchEvent(new Event('visibilitychange'));
    await new Promise((r) => setTimeout(r, 100));
    return { ok: failed && Boolean(document.querySelector('.arena_selected circle')) && document.querySelector('[data-testid="arena-state"]')?.textContent === 'Cached · refresh failed' && state.refreshes.length === before && state.refreshes.at(-1).onlyIfStale !== true };
  });
  await check_in_page(cdp, check, 'a stored initial cache failure is visible instead of No cached ratings and does not loop', async () => {
    const state = window.__arena_cache; state.cached = false; state.error = 'fetch_timeout';
    const before = state.refreshes.length;
    document.querySelector('[data-testid="cli-plan-refresh"]').click();
    const failed = await state.wait(() => document.querySelector('[data-testid="arena-state"]')?.textContent === 'Ratings unavailable');
    document.dispatchEvent(new Event('visibilitychange'));
    await new Promise((r) => setTimeout(r, 100));
    const no_loop = state.refreshes.length === before;
    const timeout_visible = Boolean(document.querySelector('[data-testid="arena-refresh-error"]')?.textContent.includes('timed out'));
    const empty_message_hidden = !document.querySelector('[data-testid="arena-empty"]');
    const retry_visible = document.querySelector('[data-testid="arena-refresh"]')?.textContent === 'Retry Arena evidence';
    return { ok: failed && no_loop && timeout_visible && empty_message_hidden && retry_visible, failed, no_loop, timeout_visible, empty_message_hidden, retry_visible };
  });
  await check_in_page(cdp, check, 'manual retry recovers the missing cache and draws positions', async () => {
    const state = window.__arena_cache; state.refresh_fails = false;
    document.querySelector('[data-testid="arena-refresh"]').click();
    const rendered = await state.wait(() => Boolean(document.querySelector('.arena_selected circle')) && !document.querySelector('[data-testid="arena-refresh-error"]'));
    return { ok: rendered && state.refreshes.at(-1).onlyIfStale !== true && state.refreshes.at(-1).input.source === 'lmarena' };
  });
  await check_in_page(cdp, check, 'arbitrary cache errors never reveal raw text', async () => {
    const state = window.__arena_cache; state.cached = false; state.error = 'PRIVATE_RAW_SOURCE_ERROR_MUST_STAY_PRIVATE';
    document.querySelector('[data-testid="cli-plan-refresh"]').click();
    const failed = await state.wait(() => document.querySelector('[data-testid="arena-state"]')?.textContent === 'Ratings unavailable');
    const error_visible = Boolean(document.querySelector('[data-testid="arena-refresh-error"]'));
    const raw_error_hidden = !document.querySelector('main').textContent.includes('PRIVATE_RAW_SOURCE_ERROR');
    return { ok: failed && error_visible && raw_error_hidden, failed, error_visible, raw_error_hidden };
  });
  await cdp.eval_value(`(() => { const state = window.__arena_cache; state.release?.(); window.__TAURI_INTERNALS__.invoke = state.original; delete window.__arena_cache; })()`);
  check('zero uncaught cache-loading errors', cdp.exceptions.length === 0, cdp.exceptions[0] ?? '');
} finally { await runtime.close(); }
process.stdout.write(`${checks - failures}/${checks} Arena cache checks passed\n`);
if (failures) process.exitCode = 1;
