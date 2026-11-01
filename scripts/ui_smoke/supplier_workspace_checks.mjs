import { create_smoke_runtime, find_browser, sleep } from './smoke_runtime.mjs';
import { attach_page } from './cdp_client.mjs';
import { check_in_page } from './page_checks.mjs';
import { run_prompt_workspace_checks } from './prompt_workspace_checks.mjs';
import { run_contextual_action_checks } from './contextual_action_checks.mjs';
import { install_client_management_navigation, open_client_management } from './client_management_navigation.mjs';
import { writeFile, mkdir } from 'node:fs/promises';
import { resolve, join } from 'node:path';

let total = 0; let failures = 0;
const check = (name, ok, detail = '') => { total++; if (!ok) failures++; process.stdout.write(`${ok ? 'PASS' : 'FAIL'} ${name}${detail ? ': ' + detail : ''}\n`); };
const browser = find_browser();
if (!browser) throw new Error('A supported browser is required');
const runtime = await create_smoke_runtime(browser, { built_demo: process.argv.includes('--built-demo') });
try {
  const cdp = await attach_page(runtime.cdp_origin);
  await cdp.send('Page.navigate', { url: runtime.demo_url });
  const ready = Date.now() + 30000;
  while (Date.now() < ready && !await cdp.eval_value('window.__aagDemoIsDemo && !!document.querySelector("[data-testid=supplier-maintenance]")')) await sleep(80);
  if (!await cdp.eval_value('window.__aagDemoIsDemo === true')) throw new Error('Synthetic data required');
  await check_in_page(cdp, check, 'import reconciliation is visible and login imports are absent', async () => {
    for (let i = 0; i < 80 && !document.querySelector('[data-testid="suppliers_card_supplier_acme"]'); i++) await new Promise((r) => setTimeout(r, 40));
    const list = await window.__TAURI_INTERNALS__.invoke('supplier_list');
    return { ok: Boolean(document.querySelector('[data-testid="supplier-maintenance"]')) && list.suppliers.length > 0 && list.suppliers.every((entry) => entry.availability !== 'pending_unsupported') && !document.querySelector('[data-testid="suppliers_official_notice"]') };
  });
  await check_in_page(cdp, check, 'failed runtime setup remains visible and blocks connection edits until retry succeeds', async () => {
    const bridge = window.__TAURI_INTERNALS__; const original = bridge.invoke;
    bridge.invoke = async (command, args) => {
      const result = await original(command, args);
      return command === 'supplier_import_maintain' ? { ...result, setupError: 'supplier_setup_pending' } : result;
    };
    let blocked = false;
    try {
      document.querySelector('[data-testid="supplier-maintenance-run"]').click();
      for (let i = 0; i < 80 && document.querySelector('[data-testid="suppliers_import_state"]')?.textContent !== 'Retry'; i++) await new Promise((r) => setTimeout(r, 40));
      blocked = document.querySelector('[data-testid="suppliers_add"]')?.disabled && document.querySelector('[data-testid="supplier-maintenance"]')?.textContent.includes('Routing setup needs attention');
    } finally { bridge.invoke = original; }
    document.querySelector('[data-testid="supplier-maintenance-run"]').click();
    for (let i = 0; i < 80 && (document.querySelector('[data-testid="suppliers_add"]')?.disabled || document.querySelector('[data-testid="suppliers_import_state"]')?.textContent !== 'Ready'); i++) await new Promise((r) => setTimeout(r, 40));
    return { ok: blocked && !document.querySelector('[data-testid="suppliers_add"]')?.disabled && document.querySelector('[data-testid="suppliers_import_state"]')?.textContent === 'Ready' };
  });
  await cdp.eval_value(`document.querySelector('[data-testid="supplier-section-arena"]').click()`);
  await check_in_page(cdp, check, 'Arena chart reports measured coordinates, uncertainty and a rating gap', async () => {
    for (let i = 0; i < 80 && !document.querySelector('[data-testid="arena-gap"]'); i++) await new Promise((r) => setTimeout(r, 40));
    const panel = document.querySelector('[data-testid="arena-chart"]');
    return { ok: panel?.textContent.includes('Synthetic demo scores') && panel.querySelector('[data-testid="routing-candidate"]') && document.querySelector('[data-testid="arena-gap"]')?.textContent.includes('-50.0') && panel.textContent.includes('intervals') };
  });
  await check_in_page(cdp, check, 'router selection changes the trajectory without sending a model request', async () => {
    const bridge = window.__TAURI_INTERNALS__; const original = bridge.invoke; const calls = [];
    bridge.invoke = (command, args) => { calls.push(command); return original(command, args); };
    try {
      const selector = document.querySelector('[data-testid="arena-selector"]');
      Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, 'value').set.call(selector, 'alias:fable');
      selector.dispatchEvent(new Event('change', { bubbles: true }));
      for (let i = 0; i < 80 && !document.querySelector('[data-testid="arena-route-flow"]')?.textContent.includes('alias:fable'); i++) await new Promise((r) => setTimeout(r, 40));
      return { ok: calls.includes('supplier_arena_snapshot') && !calls.some((command) => command === 'proxy_test_provider' || command === 'prompt_enhance_run') && document.querySelector('[data-testid="arena-route-flow"]')?.textContent.includes('alias:fable') };
    } finally { bridge.invoke = original; }
  });
  await cdp.eval_value(`document.querySelector('[data-testid="supplier-section-tailnet"]').click()`);
  await sleep(120);
  await check_in_page(cdp, check, 'Tailnet detection is on demand and displays confirmed service addresses', async () => {
    const button = document.querySelector('[data-testid="tailnet-discovery-scan"]');
    const automatic = document.querySelector('[data-testid="tailnet-discovery-auto"]');
    if (!button || !automatic || automatic.checked) return 'missing-or-unexpected-auto-scan';
    button.click();
    for (let i = 0; i < 80 && !document.querySelector('[data-testid="tailnet-instance-list"]'); i++) await new Promise((r) => setTimeout(r, 40));
    const list = document.querySelector('[data-testid="tailnet-instance-list"]');
    return { ok: Boolean(list) && list.textContent.includes('192.0.2.1') && list.textContent.includes('192.0.2.2') && Boolean(document.querySelector('[data-testid="tailnet-discovery-summary"]')) };
  });
  await cdp.eval_value(`document.querySelector('[data-testid="supplier-section-inventory"]').click()`);
  await sleep(100);
  await check_in_page(cdp, check, 'floating supplier save uses the actual draft and validation state', async () => {
    document.querySelector('[data-testid="suppliers_add"]').click();
    for (let i = 0; i < 80 && !document.querySelector('[data-testid="suppliers_editor"]'); i++) await new Promise((r) => setTimeout(r, 40));
    const name = document.querySelector('[data-testid="suppliers_editor_name"]');
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(name, 'Floating supplier draft'); name.dispatchEvent(new Event('input', { bubbles: true }));
    const base = document.querySelector('[data-testid="suppliers_editor_base_url"]');
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(base, 'invalid endpoint'); base.dispatchEvent(new Event('input', { bubbles: true }));
    await new Promise((r) => setTimeout(r, 80));
    const target = document.querySelector('[data-testid="page-action-dock"]');
    const dirty = target?.textContent.includes('Unsaved changes') && !target.querySelector('[data-testid="page-action-save"]').disabled;
    target.querySelector('[data-testid="page-action-save"]').click();
    await new Promise((r) => setTimeout(r, 150));
    const visible_error = document.querySelector('[data-testid="suppliers_editor"]')?.textContent;
    document.querySelector('[data-testid="suppliers_editor_cancel"]')?.click();
    return { ok: dirty && Boolean(visible_error), dirty };
  });
  await cdp.eval_value(`document.querySelector('[data-testid="supplier-section-arena"]').click()`);
  await sleep(150);
  for (const width of [1440, 900, 390, 320]) {
    await cdp.send('Emulation.setDeviceMetricsOverride', { width, height: 920, deviceScaleFactor: 1, mobile: width < 600 });
    await sleep(120);
    await check_in_page(cdp, check, `feature panels and square actions fit ${width}px`, () => {
      const dock = document.querySelector('[data-testid="page-action-dock"]');
      const bounds = dock.getBoundingClientRect();
      const buttons = [...dock.querySelectorAll('button')].map((button) => button.getBoundingClientRect());
      const plot = document.querySelector('.arena_scroll');
      const svg = plot?.querySelector('svg');
      const coordinates = [...svg.querySelectorAll('.arena_selected circle, .arena_comparator circle')].map((circle) => Number(circle.getAttribute('cx')));
      return { ok: document.documentElement.scrollWidth <= innerWidth + 1 && bounds.left >= 0 && bounds.right <= innerWidth && buttons.every((button) => button.width >= 44 && button.height >= 44 && Math.abs(button.width - button.height) < 1) && plot.scrollWidth <= plot.clientWidth + 1 && coordinates.length === 2 && coordinates.every((x) => x > 0 && x < svg.viewBox.baseVal.width) };
    });
  }
  await cdp.send('Emulation.setEmulatedMedia', { features: [{ name: 'prefers-reduced-motion', value: 'reduce' }] });
  await check_in_page(cdp, check, 'reduced motion retains the complete route without animation', () => ({ ok: getComputedStyle(document.querySelector('[data-testid="routing-candidate"]')).animationName === 'none' && document.querySelectorAll('[data-testid="routing-candidate"]').length > 0 && document.querySelectorAll('[data-testid="routing-fallback-arrow"]').length === document.querySelectorAll('[data-testid="routing-candidate"]').length - 1 }));
  await cdp.send('Emulation.setEmulatedMedia', { features: [] });
  await cdp.send('Emulation.clearDeviceMetricsOverride');
  const output_index = process.argv.indexOf('--screenshots');
  if (output_index >= 0) {
    await cdp.eval_value(`document.querySelector('[data-testid="supplier-section-arena"]').click()`);
    await sleep(180);
    const directory = resolve(process.argv[output_index + 1]); await mkdir(directory, { recursive: true });
    for (const width of [1440, 390]) {
      await cdp.send('Emulation.setDeviceMetricsOverride', { width, height: 1000, deviceScaleFactor: 1, mobile: width < 600 });
      await cdp.eval_value('document.querySelector("[data-testid=arena-chart]").scrollIntoView({block:"start"})'); await sleep(200);
      const screenshot = await cdp.send('Page.captureScreenshot', { format: 'png' });
      await writeFile(join(directory, `supplier_features_${width}.png`), Buffer.from(screenshot.result.data, 'base64'));
    }
    await cdp.send('Emulation.clearDeviceMetricsOverride');
  }
  await run_prompt_workspace_checks({ cdp, check });
  await run_contextual_action_checks({ cdp, check });
  await install_client_management_navigation(cdp);
  await cdp.eval_value(`(async () => {
    const invoke = window.__TAURI_INTERNALS__.invoke;
    await invoke('provider_save', { appType: 'codex', provider: { id: 'demo_unsupported_account', name: 'Synthetic account must stay private', settingsConfig: { baseUrl: 'https://account.example.invalid', auth: { refresh_token: 'synthetic-fixture-only' } }, inFailoverQueue: false } });
    const template = (await invoke('proxy_provider_list'))[0];
    await invoke('proxy_provider_save', { provider: { ...template, id: 'ccs-demo_unsupported_account', name: 'Synthetic linked account must stay private', baseURL: 'https://account.example.invalid', apiKey: 'synthetic-fixture-only', enabled: false, autoTrackModels: false } });
  })()`);
  await open_client_management(cdp, 'codex');
  await check_in_page(cdp, check, 'unsupported accounts remain stored without exposing provider cards or login data', async () => {
    for (let i = 0; i < 80 && !document.querySelector('[data-testid="unsupported-account-configurations"]'); i++) await new Promise((r) => setTimeout(r, 40));
    const stored = await window.__TAURI_INTERNALS__.invoke('provider_get', { appType: 'codex', id: 'demo_unsupported_account' });
    const panel = document.querySelector('[data-testid="providers-panel"]');
    return { ok: stored?.settingsConfig?.auth?.refresh_token === 'synthetic-fixture-only' && Boolean(document.querySelector('[data-testid="unsupported-account-configurations"]')) && panel && !panel.textContent.includes('Synthetic account must stay private') && !panel.textContent.includes('Synthetic linked account must stay private') && !panel.textContent.includes('synthetic-fixture-only') };
  });
  check('zero uncaught feature errors', cdp.exceptions.length === 0, cdp.exceptions[0] ?? '');
} finally { await runtime.close(); }
process.stdout.write(`${total - failures}/${total} supplier workspace feature checks passed\n`);
if (failures) process.exitCode = 1;
