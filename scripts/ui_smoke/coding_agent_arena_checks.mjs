import { attach_page } from './cdp_client.mjs';
import { check_in_page } from './page_checks.mjs';
import { create_smoke_runtime, find_browser, sleep } from './smoke_runtime.mjs';
import { install_client_management_navigation } from './client_management_navigation.mjs';
import { run_cli_display_checks } from './cli_display_checks.mjs';

let checks = 0; let failures = 0;
const check = (name, ok, detail = '') => { checks++; if (!ok) failures++; process.stdout.write(`${ok ? 'PASS' : 'FAIL'} ${name}${detail ? ': ' + detail : ''}\n`); };
const browser = find_browser();
if (!browser) throw new Error('A supported headless browser is required');
const runtime = await create_smoke_runtime(browser, { built_demo: process.argv.includes('--built-demo') });
try {
  const cdp = await attach_page(runtime.cdp_origin);
  await cdp.send('Page.navigate', { url: runtime.demo_url });
  for (let attempt = 0; attempt < 100 && !await cdp.eval_value('window.__aagDemoIsDemo && !!document.querySelector("[data-testid=suppliers_page]")'); attempt++) await sleep(100);
  await install_client_management_navigation(cdp);
  await run_cli_display_checks({ cdp, check });
  await cdp.eval_value(`(() => {
    if (window.__aagDemoIsDemo !== true) throw new Error('Synthetic IPC required');
    const bridge = window.__TAURI_INTERNALS__;
    const original = bridge.invoke;
    const state = { original, calls: [], delay: false, release: null, model: 'fixture-codex-first', full_queue: false };
    state.wait = async (predicate) => { for (let n = 0; n < 100; n++) { if (predicate()) return true; await new Promise((r) => setTimeout(r, 40)); } return false; };
    bridge.invoke = (command, args) => {
      if (command === 'provider_current') return Promise.resolve('fixture-' + args.appType);
      if (command === 'provider_get') return Promise.resolve({ settingsConfig: args.appType === 'claude_code' ? { env: { ANTHROPIC_MODEL: 'alias:fable', ANTHROPIC_DEFAULT_SONNET_MODEL: 'precise:demo-balanced' } } : { model: state.model } });
      if (command === 'supplier_arena_snapshot') {
        state.calls.push(args);
        if (state.full_queue) return original(command, args).then((snapshot) => ({ ...snapshot, planSource: 'admitted_runtime', candidates: Array.from({ length: 40 }, (_, index) => ({
          model: index < 2 ? 'shared-unrated-model' : 'fixture-model-' + index,
          provider: 'fixture-provider-' + index, protocol: 'openai', tier: null, downgraded: false,
          basis: 'admitted_runtime', arena: null, preferred: index === 0, pinned: false,
          health: { providerState: 'closed', samples: index, successRate: index ? 0.95 : null, latencyMs: index ? 120 : null, unavailable: false },
          cost: index ? { inputPerMillion: 0.5, outputPerMillion: 2, source: 'models_dev', stale: false } : null,
        })) }));
        if (state.delay && args.surface === 'claude') return new Promise((resolve, reject) => { state.release = () => original(command, args).then(resolve, reject); });
        // Reproduce an admitted custom route whose saved alias is unrated,
        // while its concrete first candidate has verified benchmark evidence.
        if (args.selector === 'alias:fable') return original(command, args).then((snapshot) => ({ ...snapshot, planSource: 'admitted_runtime', selected: null }));
      }
      return original(command, args);
    };
    window.__coding_arena = state;
  })()`);
  await check_in_page(cdp, check, 'configured model-tier selection changes the matching routing preview', async () => {
    const state = window.__coding_arena;
    document.querySelector('[data-testid="app-tab-claude_code"]').click();
    if (!await state.wait(() => document.querySelector('[data-testid="arena-selector"]')?.value === 'alias:fable' && document.querySelector('[data-testid="arena-route-flow"]')?.textContent.includes('alias:fable') && document.querySelector('.arena_selected circle'))) return 'rated-runtime-primary-not-plotted';
    const selector = document.querySelector('[data-testid="arena-selector"]');
    Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, 'value').set.call(selector, 'precise:demo-balanced');
    selector.dispatchEvent(new Event('change', { bubbles: true }));
    const updated = await state.wait(() => document.querySelector('[data-testid="arena-route-flow"]')?.textContent.includes('precise:demo-balanced'));
    return { ok: updated && state.calls.at(-1)?.surface === 'claude' && state.calls.at(-1)?.selector === 'precise:demo-balanced' && !document.querySelector('[data-testid="arena-current"]') };
  });
  await check_in_page(cdp, check, 'delayed Arena data from another agent never replaces the current model', async () => {
    const state = window.__coding_arena; state.delay = true;
    document.querySelector('[data-testid="arena-needs-tools"]').click();
    if (!await state.wait(() => Boolean(state.release))) return 'delayed-snapshot-not-started';
    document.querySelector('[data-testid="app-tab-codex"]').click();
    if (!await state.wait(() => document.querySelector('[data-testid="arena-route-flow"]')?.textContent.includes(state.model))) return 'codex-plan-not-ready';
    state.release(); state.delay = false;
    await new Promise((r) => setTimeout(r, 120));
    const chart = document.querySelector('[data-testid="arena-chart"]');
    return { ok: chart?.dataset.surface === 'codex' && chart.dataset.selector === state.model && document.querySelector('[data-testid="arena-selector"]')?.value === state.model && !document.querySelector('[data-testid="arena-route-flow"]')?.textContent.includes('precise:demo-balanced') };
  });
  await check_in_page(cdp, check, 'refreshing a changed plan replaces the Arena selection with that plan only', async () => {
    const state = window.__coding_arena; state.model = 'fixture-codex-next';
    document.querySelector('[data-testid="cli-plan-refresh"]').click();
    const changed = await state.wait(() => document.querySelector('[data-testid="arena-route-flow"]')?.textContent.includes(state.model));
    const options = [...document.querySelector('[data-testid="arena-selector"]').options].map((option) => option.value);
    return { ok: changed && options.length === 1 && options[0] === state.model && state.calls.at(-1)?.selector === state.model };
  });
  await check_in_page(cdp, check, 'complete routing graph retains backend order, shared models and unrated candidates', async () => {
    const state = window.__coding_arena; state.full_queue = true;
    document.querySelector('[data-testid="cli-plan-refresh"]').click();
    if (!await state.wait(() => document.querySelectorAll('[data-testid="routing-candidate"]').length === 40)) return 'complete-40-candidate-queue-not-rendered';
    const cards = [...document.querySelectorAll('[data-testid="routing-candidate"]')];
    return { ok: cards.every((card, index) => card.dataset.provider === 'fixture-provider-' + index && card.dataset.model === (index < 2 ? 'shared-unrated-model' : 'fixture-model-' + index))
      && cards[0].textContent.includes('Preferred') && cards[0].textContent.includes('Price unknown')
      && cards[1].textContent.includes('95%') && cards[1].textContent.includes('120 ms')
      && document.querySelectorAll('[data-testid="routing-fallback-arrow"]').length === 39 };
  });
  await cdp.eval_value(`window.__coding_arena.model = 'fixture-codex-polled'`);
  for (let attempt = 0; attempt < 80 && !await cdp.eval_value(`document.querySelector('[data-testid="arena-selector"]')?.value === window.__coding_arena.model`); attempt++) await sleep(250);
  await check_in_page(cdp, check, 'an open Coding Agents page picks up configuration changes automatically', () => {
    const state = window.__coding_arena;
    return { ok: document.querySelector('[data-testid="arena-selector"]')?.value === state.model
      && document.querySelector('[data-testid="arena-route-flow"]')?.textContent.includes(state.model) };
  });
  for (const width of [1440, 390, 320]) {
    await cdp.send('Emulation.setDeviceMetricsOverride', { width, height: 920, deviceScaleFactor: 1, mobile: width < 600 });
    await sleep(120);
    await check_in_page(cdp, check, `Coding Agents model position fits ${width}px`, () => {
      const plot = document.querySelector('.arena_scroll');
      return { ok: document.documentElement.scrollWidth <= innerWidth + 1 && plot && plot.scrollWidth <= plot.clientWidth + 1 };
    });
  }
  await cdp.send('Emulation.clearDeviceMetricsOverride');
  await cdp.eval_value(`(() => { const state = window.__coding_arena; state.release?.(); window.__TAURI_INTERNALS__.invoke = state.original; delete window.__coding_arena; document.querySelector('[data-testid="suppliers_nav"]').click(); })()`);
  await check_in_page(cdp, check, 'Suppliers retains its full Arena controls', async () => {
    for (let n = 0; n < 100 && !document.querySelector('[data-testid="supplier-section-arena"]'); n++) await new Promise((r) => setTimeout(r, 40));
    document.querySelector('[data-testid="supplier-section-arena"]').click();
    for (let n = 0; n < 100 && !document.querySelector('[data-testid="arena-selector"]'); n++) await new Promise((r) => setTimeout(r, 40));
    const options = [...document.querySelector('[data-testid="arena-selector"]').options].map((option) => option.value);
    return { ok: options.includes('') && options.includes('alias:fable') && document.querySelector('[data-testid="arena-chart"]').querySelectorAll('select').length === 2 };
  });
  check('zero uncaught coding-agent Arena errors', cdp.exceptions.length === 0, cdp.exceptions[0] ?? '');
} finally { await runtime.close(); }
process.stdout.write(`${checks - failures}/${checks} Coding Agents Arena checks passed\n`);
if (failures) process.exitCode = 1;
