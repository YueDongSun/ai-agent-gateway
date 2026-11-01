/** Deterministic delayed-IPC regressions. Synthetic interactions, not native acceptance. */
export async function run_supplier_reliability_checks({ cdp, check, check_in_page, open_client_management }) {
  await cdp.eval_value(`(() => {
    if (window.__aagDemoIsDemo !== true) throw new Error('synthetic IPC required');
    const element = (id) => document.querySelector('[data-testid="' + id + '"]');
    const wait_for = async (read) => {
      for (let i = 0; i < 100; i++) { const value = await read(); if (value) return value; await new Promise((resolve) => setTimeout(resolve, 40)); }
      throw new Error('reliability interaction timed out');
    };
    const set_value = async (target, value) => {
      if (!target) throw new Error('reliability input is not mounted');
      const prototype = target instanceof HTMLSelectElement ? HTMLSelectElement.prototype : target instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
      Object.getOwnPropertyDescriptor(prototype, 'value').set.call(target, value);
      target.dispatchEvent(new Event('input', { bubbles: true })); target.dispatchEvent(new Event('change', { bubbles: true }));
      await new Promise((resolve) => setTimeout(resolve, 30));
    };
    const click = async (id) => {
      const target = await wait_for(() => { const node = element(id); return node && !node.disabled ? node : null; });
      target.click();
      await new Promise((resolve) => requestAnimationFrame(resolve));
    };
    window.__supplier_reliability = { element, wait_for, set_value, click };
    return true;
  })()`);

  await check_in_page(cdp, check, 'scoped supplier model save retains other protocol and general drafts', async () => {
    const { element, wait_for, set_value, click } = window.__supplier_reliability;
    const bridge = window.__TAURI_INTERNALS__; const original = bridge.invoke;
    const original_name = (await original('supplier_get', { id: 'supplier_acme' })).supplier.name;
    await click('suppliers_edit_supplier_acme');
    await wait_for(() => element('suppliers_editor_name'));
    await set_value(element('suppliers_editor_name'), original_name + ' draft');
    await click('suppliers_tab_models');
    await wait_for(() => element('suppliers_models_endpoint'));
    await set_value(element('suppliers_models_endpoint'), 'openai-chat');
    await set_value(await wait_for(() => document.querySelector('.supplier_models_table tbody input')), 'unsaved-other-protocol');
    await set_value(element('suppliers_models_endpoint'), 'anthropic');
    await set_value(await wait_for(() => document.querySelector('.supplier_models_table tbody input')), 'saved-scoped-model');
    await click('suppliers_models_save');
    const saved = await wait_for(async () => {
      const supplier = (await original('supplier_get', { id: 'supplier_acme' })).supplier;
      return supplier.endpoints.find((entry) => entry.protocol === 'anthropic').models.some((entry) => entry.modelId === 'saved-scoped-model') ? supplier : null;
    });
    await wait_for(() => { const button = element('suppliers_models_save'); return !element('suppliers_editor') || (button && !button.disabled); });
    const remains_open = Boolean(element('suppliers_editor'));
    const scoped_only = saved.name === original_name && saved.endpoints.find((entry) => entry.protocol === 'anthropic').models.some((entry) => entry.modelId === 'saved-scoped-model') && !saved.endpoints.find((entry) => entry.protocol === 'openai-chat').models.some((entry) => entry.modelId === 'unsaved-other-protocol');
    if (!remains_open) return { ok: false, remains_open, scoped_only };
    const dirty = element('suppliers_editor').getAttribute('data-dirty') === 'true';
    await click('suppliers_tab_general');
    const name_input = await wait_for(() => element('suppliers_editor_name'));
    const retained_name = name_input.value === original_name + ' draft';
    await click('suppliers_editor_save');
    await wait_for(() => !element('suppliers_editor'));
    const full = (await original('supplier_get', { id: 'supplier_acme' })).supplier;
    return { ok: scoped_only && dirty && retained_name && full.name === original_name + ' draft' && full.endpoints.find((entry) => entry.protocol === 'openai-chat').models.some((entry) => entry.modelId === 'unsaved-other-protocol'), scoped_only, dirty, retained_name };
  });

  await check_in_page(cdp, check, 'scoped save setup failure retains unsaved edits and records persistence', async () => {
    const { element, wait_for, set_value, click } = window.__supplier_reliability;
    const bridge = window.__TAURI_INTERNALS__; const original = bridge.invoke;
    await click('suppliers_edit_supplier_acme'); await wait_for(() => element('suppliers_editor_name'));
    await set_value(element('suppliers_editor_name'), 'Recoverable general draft');
    await click('suppliers_tab_models'); await wait_for(() => element('suppliers_models_endpoint'));
    await set_value(element('suppliers_models_endpoint'), 'anthropic');
    await set_value(await wait_for(() => document.querySelector('.supplier_models_table tbody input')), 'saved-before-setup-failure');
    bridge.invoke = async (command, args) => { const result = await original(command, args); return command === 'supplier_models_update' ? { ...result, setupError: 'supplier_setup_pending' } : result; };
    try {
      await click('suppliers_models_save'); await wait_for(() => element('suppliers_editor_setup_error'));
      const saved = (await original('supplier_get', { id: 'supplier_acme' })).supplier;
      await click('suppliers_tab_general');
      const name_input = await wait_for(() => element('suppliers_editor_name'));
      const retained = name_input.value === 'Recoverable general draft' && element('suppliers_editor').getAttribute('data-dirty') === 'true';
      const persisted = saved.name !== 'Recoverable general draft' && saved.endpoints.find((entry) => entry.protocol === 'anthropic').models.some((entry) => entry.modelId === 'saved-before-setup-failure');
      await click('suppliers_editor_close');
      await wait_for(() => !element('suppliers_editor'));
      return { ok: retained && persisted, retained, persisted };
    } finally { bridge.invoke = original; }
  });

  await check_in_page(cdp, check, 'supplier mutation owns its dialog until completion and prevents duplicate submits', async () => {
    const { element, wait_for, set_value, click } = window.__supplier_reliability;
    await wait_for(() => !element('suppliers_editor'));
    await click('suppliers_edit_supplier_acme'); await wait_for(() => element('suppliers_editor_name'));
    await set_value(element('suppliers_editor_name'), 'Delayed supplier');
    const bridge = window.__TAURI_INTERNALS__; const original = bridge.invoke;
    let release; let writes = 0; const gate = new Promise((resolve) => { release = resolve; });
    bridge.invoke = async (command, args) => { if (command === 'supplier_save') { writes++; await gate; } return original(command, args); };
    try {
      await click('suppliers_editor_save'); await wait_for(() => writes === 1 && element('suppliers_editor_close')?.disabled);
      const close_disabled = element('suppliers_editor_close').disabled;
      element('suppliers_editor_close').click();
      document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
      element('suppliers_editor_save').click();
      const retained = Boolean(element('suppliers_editor'));
      release(); await wait_for(() => !element('suppliers_editor'));
      return { ok: close_disabled && retained && writes === 1, close_disabled, retained, writes };
    } finally { release(); bridge.invoke = original; }
  });

  await check_in_page(cdp, check, 'import apply remains owned while non-cancellable persistence is running', async () => {
    const { element, wait_for, click } = window.__supplier_reliability;
    await click('suppliers_import');
    await wait_for(() => element('suppliers_import_preview'));
    await click('suppliers_import_preview'); await wait_for(() => element('suppliers_import_preview_panel'));
    const bridge = window.__TAURI_INTERNALS__; const original = bridge.invoke;
    let release; let writes = 0; const gate = new Promise((resolve) => { release = resolve; });
    bridge.invoke = async (command, args) => { if (command === 'supplier_import_apply') { writes++; await gate; } return original(command, args); };
    try {
      await click('suppliers_import_apply'); await wait_for(() => writes === 1 && element('suppliers_import_close')?.disabled);
      const close_disabled = element('suppliers_import_close').disabled;
      element('suppliers_import_close').click(); document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
      const retained = Boolean(element('suppliers_import_dialog'));
      release(); await wait_for(() => !element('suppliers_import_dialog'));
      return { ok: close_disabled && retained && writes === 1, close_disabled, retained, writes };
    } finally { release(); bridge.invoke = original; }
  });

  await open_client_management(cdp, 'claude_code');
  await cdp.eval_value(`(() => { const toggle = document.querySelector('[data-testid="advanced-routes-toggle"]'); if (toggle?.getAttribute('aria-expanded') !== 'true') toggle.click(); return true; })()`);
  await cdp.eval_value(`window.__supplier_reliability.wait_for(() => document.querySelector('.route-toolbar')).then(() => true)`);

  await check_in_page(cdp, check, 'new route rename stages locally and Discard removes the staged alias', async () => {
    const { element, wait_for, click } = window.__supplier_reliability;
    const old_prompt = window.prompt; const old_confirm = window.confirm;
    try {
      window.prompt = () => 'reliability-new'; [...document.querySelectorAll('.route-toolbar button')].find((entry) => /add route/i.test(entry.textContent)).click();
      await wait_for(() => element('route-alias-reliability-new'));
      window.prompt = () => 'reliability-renamed';
      const card = element('route-alias-reliability-new').closest('.route-card'); [...card.querySelectorAll('button')].find((entry) => entry.title === 'Rename').click();
      await wait_for(() => element('route-alias-reliability-renamed'));
      const before = await window.__TAURI_INTERNALS__.invoke('route_list');
      await click('route-discard'); await wait_for(() => !element('route-alias-reliability-renamed'));
      return { ok: !before.some((entry) => entry.alias.startsWith('reliability-')) && !element('route-alias-reliability-new') };
    } finally { window.prompt = old_prompt; window.confirm = old_confirm; }
  });

  await check_in_page(cdp, check, 'route save freezes mutating controls until every partial-write outcome settles', async () => {
    const { element, wait_for, click } = window.__supplier_reliability;
    const old_prompt = window.prompt; const bridge = window.__TAURI_INTERNALS__; const original = bridge.invoke;
    let release; let started = 0; let completed = 0; const gate = new Promise((resolve) => { release = resolve; });
    try {
      for (const alias of ['reliability-save-a', 'reliability-save-b']) { window.prompt = () => alias; [...document.querySelectorAll('.route-toolbar button')].find((entry) => /add route/i.test(entry.textContent)).click(); await wait_for(() => element('route-alias-' + alias)); }
      bridge.invoke = async (command, args) => {
        if (command === 'route_save') { started++; if (args.route.alias === 'reliability-save-a') throw new Error('synthetic route failure'); await gate; completed++; }
        return original(command, args);
      };
      await click('route-save'); await wait_for(() => started === 2 && element('route-save')?.disabled);
      await new Promise((resolve) => setTimeout(resolve, 80));
      const pending = element('route-save').disabled;
      const locked = [...document.querySelectorAll('.route-editor input, .route-editor select, .route-card-actions button:not(.icon), .route-toolbar-actions button')].filter((entry) => !entry.matches('[data-testid="route-restart"]')).every((entry) => entry.disabled);
      const handle = document.querySelector('.hop-drag-handle');
      const pointer_locked = !handle || handle.getAttribute('aria-disabled') === 'true';
      release(); await wait_for(() => completed === 1 && !element('route-save').disabled && document.querySelector('.error-banner'));
      const persisted = await original('route_list');
      const accurate = completed === 1 && persisted.some((entry) => entry.alias === 'reliability-save-b') && !persisted.some((entry) => entry.alias === 'reliability-save-a') && Boolean(element('route-alias-reliability-save-a')) && Boolean(document.querySelector('.error-banner'));
      const retried_aliases = [];
      bridge.invoke = (command, args) => { if (command === 'route_save') retried_aliases.push(args.route.alias); return original(command, args); };
      await click('route-save'); await wait_for(() => retried_aliases.length > 0 && !element('route-save').disabled && !element('route-discard'));
      const retried = await original('route_list');
      return { ok: pending && locked && pointer_locked && accurate && retried_aliases.join(',') === 'reliability-save-a' && retried.filter((entry) => entry.alias === 'reliability-save-a').length === 1 && retried.filter((entry) => entry.alias === 'reliability-save-b').length === 1, pending, locked, pointer_locked, accurate, retried_aliases };
    } finally { release(); bridge.invoke = original; window.prompt = old_prompt; }
  });

  await check_in_page(cdp, check, 'new route rename rejects local alias conflicts and Save persists only its final alias', async () => {
    const { element, wait_for, click } = window.__supplier_reliability;
    const old_prompt = window.prompt;
    try {
      window.prompt = () => 'reliability-stage';
      [...document.querySelectorAll('.route-toolbar button')].find((entry) => /add route/i.test(entry.textContent)).click();
      await wait_for(() => element('route-alias-reliability-stage'));
      const rename = () => [...element('route-alias-reliability-stage').closest('.route-card').querySelectorAll('button')].find((entry) => entry.title === 'Rename').click();
      window.prompt = () => 'reliability-save-b'; rename();
      await new Promise((resolve) => setTimeout(resolve, 40));
      const refused_conflict = Boolean(element('route-alias-reliability-stage')) && document.querySelectorAll('[data-testid="route-alias-reliability-save-b"]').length === 1;
      window.prompt = () => 'reliability-stage-renamed'; rename();
      await wait_for(() => element('route-alias-reliability-stage-renamed'));
      await click('route-save'); await wait_for(() => !element('route-save').disabled && !element('route-discard'));
      const persisted = await window.__TAURI_INTERNALS__.invoke('route_list');
      return { ok: refused_conflict && persisted.filter((entry) => entry.alias === 'reliability-stage-renamed').length === 1 && !persisted.some((entry) => entry.alias === 'reliability-stage'), refused_conflict };
    } finally { window.prompt = old_prompt; }
  });

  await check_in_page(cdp, check, 'route commit followed by apply rejection is saved and never resent on retry', async () => {
    const { element, wait_for, click } = window.__supplier_reliability;
    const old_prompt = window.prompt; const bridge = window.__TAURI_INTERNALS__; const original = bridge.invoke;
    let writes = 0;
    try {
      window.prompt = () => 'reliability-committed';
      [...document.querySelectorAll('.route-toolbar button')].find((entry) => /add route/i.test(entry.textContent)).click();
      await wait_for(() => element('route-alias-reliability-committed'));
      bridge.invoke = async (command, args) => {
        if (command === 'route_save') { writes++; await original(command, args); throw new Error('fixture runtime apply failed'); }
        return original(command, args);
      };
      await click('route-save'); await wait_for(() => writes === 1 && !element('route-save').disabled && !element('route-discard'));
      const saved = (await original('route_list')).some((entry) => entry.alias === 'reliability-committed');
      const notice = Boolean(document.querySelector('.notice-banner'));
      await click('route-save');
      await wait_for(() => !element('route-save').disabled);
      return { ok: saved && notice && writes === 1, saved, notice, writes };
    } finally { bridge.invoke = original; window.prompt = old_prompt; }
  });

  await check_in_page(cdp, check, 'unknown route outcome requires readback before retry can submit any writes', async () => {
    const { element, wait_for, click } = window.__supplier_reliability;
    const old_prompt = window.prompt; const bridge = window.__TAURI_INTERNALS__; const original = bridge.invoke;
    let writes = 0; let reads = 0; let allow_readback = false;
    try {
      window.prompt = () => 'reliability-unknown';
      [...document.querySelectorAll('.route-toolbar button')].find((entry) => /add route/i.test(entry.textContent)).click();
      await wait_for(() => element('route-alias-reliability-unknown'));
      bridge.invoke = async (command, args) => {
        if (command === 'route_save') { writes++; await original(command, args); throw new Error('fixture apply acknowledgement lost'); }
        if (command === 'route_list') { reads++; if (!allow_readback) throw new Error('fixture readback unavailable'); }
        return original(command, args);
      };
      await click('route-save'); await wait_for(() => reads >= 1 && !element('route-save').disabled);
      const unknown_visible = Boolean(document.querySelector('.error-banner')) && Boolean(element('route-discard'));
      await click('route-save'); await wait_for(() => reads >= 2 && !element('route-save').disabled);
      const no_duplicate = writes === 1;
      allow_readback = true;
      await click('route-save'); await wait_for(() => !element('route-save').disabled && !element('route-discard'));
      const saved = (await original('route_list')).filter((entry) => entry.alias === 'reliability-unknown').length === 1;
      return { ok: unknown_visible && no_duplicate && saved && writes === 1, unknown_visible, no_duplicate, saved, writes };
    } finally { bridge.invoke = original; window.prompt = old_prompt; }
  });

  await check_in_page(cdp, check, 'route discard locks edits, rename, removal and both reorder paths until reload settles', async () => {
    const { element, wait_for, set_value, click } = window.__supplier_reliability;
    const bridge = window.__TAURI_INTERNALS__; const original = bridge.invoke;
    const saved = await original('route_list');
    const row = document.querySelector('.hop-row');
    const input = row?.querySelector('.model-suggest input');
    if (!input) return { ok: false, detail: 'seeded hop missing' };
    await set_value(input, 'discard-this-draft');
    let release; let reads = 0; const gate = new Promise((resolve) => { release = resolve; });
    bridge.invoke = async (command, args) => { if (command === 'route_list') { reads++; await gate; } return original(command, args); };
    try {
      await click('route-discard'); await wait_for(() => reads === 1 && element('route-save')?.disabled);
      const locked = [...document.querySelectorAll('.route-editor input, .route-editor select, .route-card-actions button:not(.icon), .route-toolbar-actions button')].filter((entry) => !entry.matches('[data-testid="route-restart"]')).every((entry) => entry.disabled);
      const handle = document.querySelector('.hop-drag-handle');
      handle?.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowDown', bubbles: true }));
      handle?.dispatchEvent(new PointerEvent('pointerdown', { pointerId: 91, bubbles: true }));
      const owns = element('route-save').disabled && handle?.getAttribute('aria-disabled') === 'true';
      release(); await wait_for(() => !element('route-save').disabled);
      const after = await original('route_list');
      return { ok: locked && owns && JSON.stringify(saved) === JSON.stringify(after) && document.querySelector('.model-suggest input').value !== 'discard-this-draft' && !element('route-discard'), locked, owns };
    } finally { release(); bridge.invoke = original; }
  });
}

export async function run_mirror_reliability_checks({ cdp, check, check_in_page, open_client_management }) {
  await cdp.eval_value(`(() => {
    const bridge = window.__TAURI_INTERNALS__;
    window.__mirror_reliability_original = bridge.invoke;
    bridge.invoke = (command, args) => command === 'codex_cli_status' ? Promise.reject(new Error('synthetic status unavailable')) : window.__mirror_reliability_original(command, args);
    return true;
  })()`);
  try {
    await open_client_management(cdp, 'codex');
    await check_in_page(cdp, check, 'Codex mode apply recovers unknown status and prevents duplicate mutations', async () => {
      const bridge = window.__TAURI_INTERNALS__; const original = window.__mirror_reliability_original;
      const element = (id) => document.querySelector(`[data-testid="${id}"]`);
      const wait_for = async (read) => { for (let i = 0; i < 120; i++) { const value = read(); if (value) return value; await new Promise((resolve) => setTimeout(resolve, 30)); } throw new Error('mirror ownership condition timed out'); };
      await wait_for(() => element('codex-mirror-error'));
      const initial_mode = (await original('codex_cli_status')).mirrorMode;
      let release; let writes = 0; let status_reads = 0; const gate = new Promise((resolve) => { release = resolve; });
      bridge.invoke = async (command, args) => {
        if (command === 'codex_mirror_set_mode') { writes++; await gate; }
        if (command === 'codex_cli_status' && status_reads++ === 0) throw new Error('synthetic first readback failure');
        return original(command, args);
      };
      try {
        element('codex-mirror-mode-toggle').click();
        await wait_for(() => writes === 1 && element('codex-mirror-mode-toggle').disabled);
        element('codex-mirror-mode-toggle').click();
        release();
        await wait_for(() => element('codex-mirror-mode-toggle').checked && !element('codex-mirror-mode-toggle').disabled && !element('codex-mirror-error'));
        const actual = await original('codex_cli_status');
        return { ok: writes === 1 && status_reads === 2 && actual.mirrorMode === 'coexist' && Boolean(document.querySelector('.error-banner')), writes, status_reads, mode: actual.mirrorMode };
      } finally { release(); bridge.invoke = original; await original('codex_mirror_set_mode', { mode: initial_mode }); }
    });
  } finally {
    await cdp.eval_value(`(() => { window.__TAURI_INTERNALS__.invoke = window.__mirror_reliability_original; delete window.__mirror_reliability_original; return true; })()`);
  }
}

if (process.argv[1] && import.meta.url === (await import('node:url')).pathToFileURL(process.argv[1]).href) {
  const { create_smoke_runtime, find_browser, sleep, format_error } = await import('./smoke_runtime.mjs');
  const { attach_page } = await import('./cdp_client.mjs');
  const { check_in_page } = await import('./page_checks.mjs');
  const { install_client_management_navigation, open_client_management } = await import('./client_management_navigation.mjs');
  let failures = 0; let total = 0;
  const check = (name, ok, detail = '') => { total++; if (!ok) failures++; process.stdout.write(`${ok ? 'PASS' : 'FAIL'} ${name}${detail ? ': ' + detail : ''}\n`); };
  const runtime = await create_smoke_runtime(find_browser(), { built_demo: process.argv.includes('--built-demo') });
  let cdp = null;
  try {
    cdp = await attach_page(runtime.cdp_origin);
    await cdp.send('Page.addScriptToEvaluateOnNewDocument', { source: `localStorage.setItem('aag.locale', 'en');` });
    await cdp.send('Page.navigate', { url: runtime.demo_url });
    const deadline = Date.now() + 30000;
    while (Date.now() < deadline && !await cdp.eval_value('window.__aagDemoIsDemo && !!document.querySelector("[data-testid=suppliers_edit_supplier_acme]")')) await sleep(80);
    if (!await cdp.eval_value('window.__aagDemoIsDemo === true && !!document.querySelector("[data-testid=suppliers_edit_supplier_acme]")')) {
      throw new Error('Synthetic supplier workspace did not become ready within 30 seconds');
    }
    await install_client_management_navigation(cdp);
    await run_supplier_reliability_checks({ cdp, check, check_in_page, open_client_management });
    await run_mirror_reliability_checks({ cdp, check, check_in_page, open_client_management });
    check('no uncaught reliability interaction errors', cdp.exceptions.length === 0, cdp.exceptions[0] ?? '');
  } catch (error) {
    process.stderr.write('Supplier reliability runner failed: ' + format_error(error) + '\n');
    throw error;
  } finally {
    try { await runtime.close(cdp); }
    catch (error) { process.stderr.write('Supplier reliability cleanup failed: ' + format_error(error) + '\n'); throw error; }
  }
  process.stdout.write(`${total - failures}/${total} supplier reliability checks passed\n`);
  if (failures) process.exitCode = 1;
}
