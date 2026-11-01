import { pathToFileURL } from 'node:url';
import { attach_page } from './cdp_client.mjs';
import { check_in_page } from './page_checks.mjs';
import { install_client_management_navigation, open_client_management } from './client_management_navigation.mjs';
import { create_smoke_runtime, find_browser, sleep } from './smoke_runtime.mjs';

export async function run_provider_interaction_checks({ cdp, check }) {
  await check_in_page(cdp, check, 'provider search filters names without changing stored order', async () => {
    const input = document.querySelector('[data-testid="providers-search"]');
    if (!input) return 'search-missing';
    const rows = () => [...document.querySelectorAll('.provider-card')];
    const initial_ids = rows().map((row) => row.dataset.testid);
    const target = rows().find((row) => row.dataset.presetIdx === '1');
    if (!target) return 'second-preset-fixture-missing';
    const target_id = target.dataset.testid;
    const name = target.querySelector('.provider-card-title').firstChild.textContent.trim();
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(input, `  ${name.toUpperCase()}  `);
    input.dispatchEvent(new Event('input', { bubbles: true }));
    await new Promise((resolve) => setTimeout(resolve, 80));
    const matches = rows();
    const correct_match = matches.some((row) => row.dataset.testid === target_id && row.dataset.presetIdx === '1');
    const handles = matches.flatMap((row) => [...row.querySelectorAll('.provider-card-drag')]);
    const disabled = handles.length > 0 && handles.every((handle) => handle.getAttribute('aria-disabled') === 'true' && handle.tabIndex === -1);
    handles[0]?.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowUp', bubbles: true }));
    document.querySelector('[data-testid="providers-search-clear"]')?.click();
    await new Promise((resolve) => setTimeout(resolve, 80));
    const final_ids = rows().map((row) => row.dataset.testid);
    const restored = JSON.stringify(initial_ids) === JSON.stringify(final_ids);
    return { ok: correct_match && disabled && restored && document.activeElement === input, correct_match, disabled, restored };
  });

  await check_in_page(cdp, check, 'provider endpoint search and no-results recover with Escape', async () => {
    const input = document.querySelector('[data-testid="providers-search"]');
    const first = document.querySelector('.provider-card');
    if (!input || !first) return 'missing-search-or-fixture';
    const initial_count = document.querySelectorAll('.provider-card').length;
    const url = first.querySelector('.provider-card-url').firstChild.textContent.split(' · ')[0].trim();
    const set_query = async (query) => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(input, query);
      input.dispatchEvent(new Event('input', { bubbles: true }));
      await new Promise((resolve) => setTimeout(resolve, 80));
    };
    await set_query(url);
    const endpoint_found = [...document.querySelectorAll('.provider-card')].some((row) => row.dataset.testid === first.dataset.testid);
    await set_query('no-provider-will-match-this-fixture');
    const empty = Boolean(document.querySelector('[data-testid="providers-search-empty"]')) && document.querySelectorAll('.provider-card').length === 0;
    input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
    await new Promise((resolve) => setTimeout(resolve, 80));
    return { ok: endpoint_found && empty && input.value === '' && document.querySelectorAll('.provider-card').length === initial_count, endpoint_found, empty };
  });

  await check_in_page(cdp, check, 'provider secondary actions require disclosure and Escape returns focus', async () => {
    const row = document.querySelector('.provider-card');
    const more = row?.querySelector('[data-testid^="provider-more-"]');
    const details = row?.querySelector('.provider_secondary_actions');
    const edit = row?.querySelector('[data-testid^="provider-edit-"]');
    const enable = row?.querySelector('[data-testid^="provider-switch-"]');
    if (!more || !details || !edit || !enable) return 'primary-actions-missing';
    const closed = details.hidden && more.getAttribute('aria-expanded') === 'false';
    const primary_visible = [edit, enable, more].every((button) => button.getBoundingClientRect().width > 0 && getComputedStyle(button.parentElement).opacity === '1');
    more.click();
    await new Promise((resolve) => setTimeout(resolve, 40));
    const expanded = !details.hidden && more.getAttribute('aria-expanded') === 'true' && more.getAttribute('aria-controls') === details.id;
    const secondary = details.querySelector('button');
    secondary?.focus();
    secondary?.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
    await new Promise((resolve) => setTimeout(resolve, 40));
    return { ok: closed && primary_visible && expanded && details.hidden && document.activeElement === more, closed, primary_visible, expanded };
  });

  await check_in_page(cdp, check, 'provider inference prerequisites are visible before the Test action sends a request', async () => {
    const button = document.querySelector('[data-testid^="provider-test-"]');
    const row = button?.closest('.provider-card');
    const more = row?.querySelector('[data-testid^="provider-more-"]');
    if (!button || !more) return 'missing-test-control';
    const bridge = window.__TAURI_INTERNALS__; const original = bridge.invoke;
    let inference_tests = 0;
    bridge.invoke = (command, args) => { if (command === 'proxy_test_provider') inference_tests++; return original(command, args); };
    try {
      more.click();
      await new Promise((resolve) => setTimeout(resolve, 40));
      const notice = document.getElementById(button.getAttribute('aria-describedby'));
      const visible = Boolean(notice?.getClientRects().length) && notice.textContent.includes('CC Switch');
      more.click();
      return { ok: visible && inference_tests === 0, visible, inference_tests };
    } finally { bridge.invoke = original; }
  });

  try {
    for (const width of [1440, 1024, 768, 390, 320]) {
      await cdp.send('Emulation.setDeviceMetricsOverride', { width, height: 900, deviceScaleFactor: 1, mobile: false });
      await sleep(60);
      await check_in_page(cdp, check, `provider actions fit and remain visible at ${width}px`, async () => {
        const row = document.querySelector('.provider-card');
        const more = row?.querySelector('[data-testid^="provider-more-"]');
        if (!row || !more) return 'provider-card-missing';
        more.click();
        await new Promise((resolve) => setTimeout(resolve, 40));
        const bounds = row.getBoundingClientRect();
        const controls = [...row.querySelectorAll('.provider-card-actions button, .provider_secondary_actions > button, .provider_control')];
        const fits = controls.every((control) => {
          const rect = control.getBoundingClientRect();
          return rect.width > 0 && rect.left >= bounds.left && rect.right <= bounds.right && rect.bottom <= bounds.bottom;
        });
        const touch_targets = window.innerWidth > 520 || controls.every((control) => control.getBoundingClientRect().height >= 44);
        more.click();
        return { ok: fits && touch_targets, fits, touch_targets };
      });
    }
  } finally {
    await cdp.send('Emulation.clearDeviceMetricsOverride');
  }
}

async function main() {
  let runtime;
  let cdp;
  let failures = 0;
  let checks = 0;
  const check = (name, passed, detail = '') => {
    checks += 1;
    if (!passed) failures += 1;
    process.stdout.write(`${passed ? 'PASS' : 'FAIL'} ${name}${detail ? ': ' + detail : ''}\n`);
  };
  try {
    const browser = find_browser();
    if (!browser) throw new Error('No supported browser found');
    runtime = await create_smoke_runtime(browser);
    cdp = await attach_page(runtime.cdp_origin);
    await cdp.send('Page.navigate', { url: runtime.demo_url });
    process.stdout.write('Provider checks: waiting for synthetic shell\n');
    const deadline = Date.now() + 30_000;
    while (Date.now() < deadline) {
      if (await cdp.eval_value('window.__aagDemoIsDemo === true && Boolean(document.querySelector("[data-testid=workspace_destination]"))')) break;
      await sleep(100);
    }
    if (!await cdp.eval_value('window.__aagDemoIsDemo === true')) throw new Error('Synthetic backend required');
    await install_client_management_navigation(cdp);
    await open_client_management(cdp);
    process.stdout.write('Provider checks: waiting for provider list\n');
    const page_deadline = Date.now() + 10_000;
    while (Date.now() < page_deadline) {
      if (await cdp.eval_value('document.querySelectorAll(".provider-card").length >= 2')) break;
      await sleep(100);
    }
    process.stdout.write('Provider checks: exercising search and disclosures\n');
    await run_provider_interaction_checks({ cdp, check });
    check('zero uncaught browser exceptions', cdp.exceptions.length === 0);
  } finally {
    await runtime?.close(cdp);
    cdp?.web_socket.close();
  }
  process.stdout.write(`${checks - failures}/${checks} provider interaction checks passed\n`);
  if (failures) process.exitCode = 1;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().catch((error) => {
    process.stderr.write(`${error.stack ?? error.message}${error.cause ? '\nCause: ' + error.cause.message : ''}\n`);
    process.exitCode = 1;
  });
}
