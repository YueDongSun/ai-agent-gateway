import { mkdir, writeFile } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { create_smoke_runtime, find_browser, sleep } from './smoke_runtime.mjs';
import { attach_page } from './cdp_client.mjs';
import { check_in_page } from './page_checks.mjs';
import { install_client_management_navigation, open_client_management } from './client_management_navigation.mjs';

// A separate, owned synthetic runtime; never attach to a live desktop profile.
const screenshot_argument = process.argv.indexOf('--screenshots');
const screenshot_directory = screenshot_argument >= 0 ? resolve(process.argv[screenshot_argument + 1]) : null;
const viewports = [
  { width: 1440, height: 1000 }, { width: 1280, height: 800 },
  { width: 1024, height: 768 }, { width: 900, height: 900 },
  { width: 768, height: 1024 }, { width: 680, height: 900 }, { width: 600, height: 900 }, { width: 521, height: 900 }, { width: 430, height: 932 },
  { width: 390, height: 844 }, { width: 360, height: 800 }, { width: 320, height: 740 },
];
let failed = 0;
let total = 0;
const check = (name, ok, detail = '') => {
  total += 1;
  if (!ok) failed += 1;
  process.stdout.write(`${ok ? 'PASS' : 'FAIL'} ${name}${detail ? ': ' + detail : ''}\n`);
};
const browser = find_browser();
if (!browser) throw new Error('A supported browser is required');
const runtime = await create_smoke_runtime(browser, { built_demo: process.argv.includes('--built-demo') });
try {
  const cdp = await attach_page(runtime.cdp_origin);
  await cdp.send('Page.navigate', { url: runtime.demo_url });
  const deadline = Date.now() + 30_000;
  while (Date.now() < deadline && !await cdp.eval_value('window.__aagDemoIsDemo && !!document.querySelector("[data-testid=suppliers_page]")')) await sleep(100);
  if (!await cdp.eval_value('window.__aagDemoIsDemo === true')) throw new Error('Synthetic IPC marker required');
  if (screenshot_directory) await mkdir(screenshot_directory, { recursive: true });
  await cdp.eval_value('window.scrollTo(0, 420)');
  await sleep(250);
  check('surface samples on first mount before a theme or route change', await cdp.eval_value(`Number(document.querySelector('.workspace_header').style.getPropertyValue('--status_progress')) > 0`));

  for (const theme of ['light', 'dark']) {
    await cdp.eval_value(`document.documentElement.dataset.theme === '${theme}' || document.querySelector('.theme-btn').click()`);
    for (const viewport of viewports) {
      await cdp.send('Emulation.setDeviceMetricsOverride', { ...viewport, deviceScaleFactor: 1, mobile: viewport.width < 600 });
      for (const scroll of [0, 420]) {
        await cdp.eval_value(`window.scrollTo(0, ${scroll})`);
        await sleep(240);
        await check_in_page(cdp, check, `${theme} ${viewport.width}px scroll ${scroll}: containment, glass, contrast`, () => {
          const header = document.querySelector('.workspace_header');
          const rect = header.getBoundingClientRect();
          const style = getComputedStyle(header);
          const parse = (value) => value.match(/[\d.]+/g).map(Number);
          const background = parse(style.backgroundColor);
          const luminance = (rgb) => rgb.slice(0, 3).reduce((sum, value, index) => {
            const unit = value / 255;
            return sum + (unit <= .04045 ? unit / 12.92 : ((unit + .055) / 1.055) ** 2.4) * [.2126, .7152, .0722][index];
          }, 0);
          const ratio = (fg, bg) => (Math.max(luminance(fg), luminance(bg)) + .05) / (Math.min(luminance(fg), luminance(bg)) + .05);
          const backdrops = [0, 255].map((under) => background.slice(0, 3).map((value) => value * (background[3] ?? 1) + under * (1 - (background[3] ?? 1))));
          const controls = [...header.querySelectorAll('button, select, .gw-status')];
          const minimum_contrast = Math.min(...controls.flatMap((element) => backdrops.map((bg) => ratio(parse(getComputedStyle(element).color), bg))));
          const fits = (element) => {
            const bounds = element.getBoundingClientRect();
            return bounds.width > 0 && bounds.left >= rect.left && bounds.right <= rect.right + 1 && bounds.top >= rect.top && bounds.bottom <= rect.bottom + 1;
          };
          const status = header.querySelector('.gw-status');
          const original_class = status.className;
          const original_label = status.lastChild.textContent;
          let controls_fit = true;
          let icon_contrast = Infinity;
          for (const state of ['running', 'stopped', 'degraded', 'unknown']) {
            status.className = 'gw-status ' + state;
            status.lastChild.textContent = ('Gateway ' + state).toUpperCase();
            controls_fit &&= controls.every(fits);
            const dot = getComputedStyle(status.querySelector('.gw-status-dot'));
            icon_contrast = Math.min(icon_contrast, ...backdrops.map((bg) => ratio(parse(dot.color), bg)));
            controls_fit &&= dot.opacity === '1';
          }
          status.className = original_class;
          status.lastChild.textContent = original_label;
          return { ok: document.documentElement.scrollWidth <= innerWidth + 1 && controls_fit && minimum_contrast >= 4.5 && icon_contrast >= 3 && style.position === 'sticky' && background[3] >= .92 && background[3] < 1 && (scrollY === 0 || Math.abs(rect.top) <= 1), minimum_contrast, icon_contrast, width: innerWidth, scroll: scrollY, controls_fit, background: style.backgroundColor };
        });
        if (screenshot_directory && [1440, 390].includes(viewport.width)) {
          const screenshot = await cdp.send('Page.captureScreenshot', { format: 'png' });
          await writeFile(join(screenshot_directory, `${theme}_${viewport.width}_${scroll}.png`), Buffer.from(screenshot.result.data, 'base64'));
        }
      }
    }
  }

  await cdp.send('Emulation.clearDeviceMetricsOverride');
  await check_in_page(cdp, check, 'header echoes the actual underlying card surface', async () => {
    const header = document.querySelector('.workspace_header');
    window.scrollTo(0, 0);
    await new Promise((resolve) => setTimeout(resolve, 250));
    const initial = getComputedStyle(header).backgroundColor;
    const card = document.querySelector('.supplier_overview');
    if (!card) return 'surface-fixture-missing';
    window.scrollTo(0, card.getBoundingClientRect().top + scrollY - header.clientHeight / 2 + 16);
    await new Promise((resolve) => setTimeout(resolve, 300));
    const echoed = getComputedStyle(header).backgroundColor;
    // Compare RGB as well as opacity: scrolling must sample the content.
    const channels = (value) => value.match(/[\d.]+/g).slice(0, 3).join(',');
    return { ok: channels(initial) !== channels(echoed), initial, echoed };
  });
  await cdp.eval_value(`document.querySelector('[data-testid="app-tab-claude_code"]').click()`);
  await sleep(600);
  await check_in_page(cdp, check, 'client navigation owns route and focus without provider presentation', async () => {
    document.querySelector('[data-testid="app-tab-codex"]').click();
    await new Promise((resolve) => setTimeout(resolve, 350));
    const page = document.querySelector('[data-testid="cli-router-page"]');
    return { ok: location.hash === '#/codex' && scrollY === 0 && page?.dataset.app === 'codex'
      && !document.querySelector('.provider-card, .providers-panel, [data-testid="provider-modal"]')
      && document.activeElement.id === 'workspace_content' && document.querySelector('.page-header h2')?.textContent === 'Codex' };
  });
  await cdp.eval_value('history.back()');
  await sleep(350);
  check('Back restores the selected application', await cdp.eval_value(`location.hash === '#/claude_code' && document.querySelector('[data-testid=workspace_destination]').value === 'claude_code'`));
  await cdp.eval_value('history.forward()');
  await sleep(350);
  check('Forward restores the selected application', await cdp.eval_value(`location.hash === '#/codex' && document.querySelector('[data-testid=workspace_destination]').value === 'codex'`));
  await cdp.send('Page.reload');
  await sleep(600);
  check('reload honors the route', await cdp.eval_value(`document.querySelector('.page-header h2')?.textContent === 'Codex'`));
  await install_client_management_navigation(cdp);
  await open_client_management(cdp);
  await check_in_page(cdp, check, 'Suppliers client selection isolates the prior provider editor', async () => {
    let edit;
    for (let attempt = 0; attempt < 40; attempt++) {
      edit = document.querySelector('[data-testid^="provider-edit-"]');
      if (edit) break;
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
    if (!edit) return 'missing-provider-edit';
    edit.click();
    await new Promise((resolve) => setTimeout(resolve, 50));
    if (!document.querySelector('[data-testid="provider-modal"]')) return 'editor-not-open';
    document.querySelector('[data-testid="client-settings-tab-codex"]').click();
    await new Promise((resolve) => setTimeout(resolve, 350));
    return { ok: location.hash === '#/suppliers' && !document.querySelector('[data-testid="provider-modal"]')
      && document.querySelector('.client_management_body .page-header h2')?.textContent === 'Codex' };
  });


  if (screenshot_directory) {
    for (const theme of ['light', 'dark']) {
      await cdp.eval_value(`document.documentElement.dataset.theme === '${theme}' || document.querySelector('.theme-btn').click()`);
      for (const width of [1440, 390]) {
        await cdp.send('Emulation.setDeviceMetricsOverride', { width, height: 1000, deviceScaleFactor: 1, mobile: width < 600 });
        await cdp.eval_value(`(() => { const panel = document.querySelector('[data-testid="providers-panel"]'); window.scrollTo(0, panel.getBoundingClientRect().top + scrollY - document.querySelector('.workspace_header').clientHeight - 24); })()`);
        await sleep(300);
        const screenshot = await cdp.send('Page.captureScreenshot', { format: 'png' });
        await writeFile(join(screenshot_directory, `providers_${theme}_${width}.png`), Buffer.from(screenshot.result.data, 'base64'));
      }
    }
    await cdp.send('Emulation.clearDeviceMetricsOverride');
  }

  await cdp.eval_value(`document.querySelector('[data-testid^="provider-edit-"]').focus(); document.activeElement.click()`);
  await sleep(100);
  check('provider editor is a named dialog with initial focus', await cdp.eval_value(`(() => { const dialog = document.querySelector('[data-testid="provider-modal"]'); return dialog?.getAttribute('role') === 'dialog' && !!document.getElementById(dialog.getAttribute('aria-labelledby'))?.textContent && dialog.contains(document.activeElement); })()`));
  await cdp.send('Input.dispatchKeyEvent', { type: 'keyDown', key: 'Tab', code: 'Tab', modifiers: 8 });
  await cdp.send('Input.dispatchKeyEvent', { type: 'keyUp', key: 'Tab', code: 'Tab', modifiers: 8 });
  check('Shift+Tab remains in dialog', await cdp.eval_value(`document.querySelector('[data-testid="provider-modal"]').contains(document.activeElement)`));
  await cdp.send('Input.dispatchKeyEvent', { type: 'keyDown', key: 'Escape', code: 'Escape' });
  await cdp.send('Input.dispatchKeyEvent', { type: 'keyUp', key: 'Escape', code: 'Escape' });
  await sleep(100);
  check('Escape closes editor and restores trigger', await cdp.eval_value(`!document.querySelector('[data-testid="provider-modal"]') && document.activeElement.dataset.testid?.startsWith('provider-edit-')`));

  await cdp.send('Emulation.setEmulatedMedia', { features: [{ name: 'prefers-reduced-motion', value: 'reduce' }] });
  await check_in_page(cdp, check, 'reduced motion disables page and surface animation', () => {
    const page = getComputedStyle(document.querySelector('.workspace_page'));
    const header = getComputedStyle(document.querySelector('.workspace_header'));
    return { ok: page.animationName === 'none' && header.transitionDuration === '0s' };
  });
  await cdp.send('Emulation.setEmulatedMedia', { features: [] });
  await check_in_page(cdp, check, 'surface sampling stops when the page is idle', async () => {
    await new Promise((resolve) => setTimeout(resolve, 400));
    let writes = 0;
    const observer = new MutationObserver((records) => { writes += records.length; });
    observer.observe(document.querySelector('.workspace_header'), { attributes: true, attributeFilter: ['style'] });
    await new Promise((resolve) => setTimeout(resolve, 350));
    observer.disconnect();
    return { ok: writes === 0, writes };
  });
  const performance_result = await cdp.eval_value(`(async () => {
    const long_tasks = [];
    const observer = new PerformanceObserver((list) => long_tasks.push(...list.getEntries().map((entry) => entry.duration)));
    observer.observe({type: 'longtask'});
    for (let index = 0; index < 45; index++) { window.scrollTo(0, index * 16); await new Promise(requestAnimationFrame); }
    await new Promise((resolve) => setTimeout(resolve, 100));
    observer.disconnect();
    return {scroll_frames: 45, long_tasks: long_tasks.length, max_long_task_ms: Math.max(0, ...long_tasks)};
  })()`);
  process.stdout.write('Scroll performance observation ' + JSON.stringify(performance_result) + '\n');
  check('no uncaught browser exceptions', cdp.exceptions.length === 0, cdp.exceptions[0] ?? '');
} finally {
  await runtime.close();
}
process.stdout.write(`${total - failed}/${total} workspace checks passed\n`);
if (failed) process.exitCode = 1;
