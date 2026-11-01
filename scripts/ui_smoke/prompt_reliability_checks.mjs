/** Delayed synthetic IPC proves ownership, never backend cancellation. */
export async function run_prompt_reliability_checks({ cdp, check_in_page, check }) {
  await check_in_page(cdp, check, 'prompt stages retain one task while history, output editing and navigation are locked', async () => {
    const element = (id) => document.querySelector(`[data-testid="${id}"]`);
    const wait_for = async (read) => {
      for (let attempt = 0; attempt < 120; attempt++) {
        const value = read(); if (value) return value;
        await new Promise((resolve) => setTimeout(resolve, 30));
      }
      throw new Error('prompt ownership condition timed out');
    };
    const set_value = (node, value) => {
      const prototype = node instanceof HTMLSelectElement ? HTMLSelectElement.prototype : HTMLTextAreaElement.prototype;
      Object.getOwnPropertyDescriptor(prototype, 'value').set.call(node, value);
      node.dispatchEvent(new Event('input', { bubbles: true }));
      node.dispatchEvent(new Event('change', { bubbles: true }));
    };
    localStorage.setItem('aag.prompt.history', JSON.stringify([{ id: 'history-b', created_at: 1, input: 'History B', draft: 'Draft B', prompt: 'Output B', target_label: 'fixture' }]));
    set_value(element('workspace_destination'), 'prompt');
    await wait_for(() => element('prompt_input') && element('prompt_target')?.options.length);
    set_value(element('prompt_input'), 'Task A');
    await new Promise((resolve) => setTimeout(resolve, 30));
    const bridge = window.__TAURI_INTERNALS__; const original = bridge.invoke;
    const releases = {}; let calls = 0;
    bridge.invoke = async (command, args) => {
      if (command !== 'prompt_enhance_run') return original(command, args);
      calls++;
      await new Promise((resolve) => { releases[args.request.stage] = resolve; });
      return original(command, args);
    };
    try {
      element('prompt_run').click();
      await wait_for(() => releases.zh_expand);
      const locked_zh = element('prompt_history_item').disabled && element('prompt_output').disabled && element('prompt_input').disabled;
      element('prompt_history_item').click(); element('prompt_run').click();
      set_value(element('workspace_destination'), 'suppliers');
      await new Promise((resolve) => setTimeout(resolve, 30));
      const retained = Boolean(element('prompt_page')) && element('prompt_input')?.value === 'Task A' && calls === 1;
      history.back();
      await new Promise((resolve) => setTimeout(resolve, 120));
      const retained_history = Boolean(element('prompt_page')) && location.hash === '#/prompt';
      location.hash = '#/usage';
      await wait_for(() => location.hash === '#/prompt');
      history.forward();
      await new Promise((resolve) => setTimeout(resolve, 120));
      const retained_fragment_forward = Boolean(element('prompt_page')) && location.hash === '#/prompt';
      releases.zh_expand();
      await wait_for(() => releases.en_optimize);
      const locked_en = element('prompt_history_item').disabled && element('prompt_output').disabled && element('prompt_run').disabled;
      releases.en_optimize();
      await wait_for(() => !element('prompt_run').disabled);
      const completed = element('prompt_draft').value.includes('Task A') && element('prompt_output').value.includes('Task A') && calls === 2;
      const output = element('prompt_output').value;
      history.back();
      await wait_for(() => !element('prompt_page'));
      const original_previous_entry = Boolean(element('suppliers_page')) && (location.hash === '' || location.hash === '#/suppliers');
      history.forward();
      await wait_for(() => element('prompt_input'));
      const recovered = element('prompt_input').value === 'Task A' && element('prompt_output').value === output;
      return { ok: locked_zh && retained && retained_history && retained_fragment_forward && original_previous_entry && locked_en && completed && recovered, locked_zh, retained, retained_history, retained_fragment_forward, original_previous_entry, locked_en, completed, recovered };
    } finally {
      releases.zh_expand?.(); releases.en_optimize?.(); bridge.invoke = original;
    }
  });

  await check_in_page(cdp, check, 'English retry retains its edited brief and idle prompt settings survive navigation', async () => {
    const element = (id) => document.querySelector(`[data-testid="${id}"]`);
    const wait_for = async (read) => { for (let i = 0; i < 120; i++) { const result = read(); if (result) return result; await new Promise((resolve) => setTimeout(resolve, 30)); } throw new Error('prompt retry condition timed out'); };
    const set_value = async (node, value) => {
      const prototype = node instanceof HTMLSelectElement ? HTMLSelectElement.prototype : node instanceof HTMLInputElement ? HTMLInputElement.prototype : HTMLTextAreaElement.prototype;
      Object.getOwnPropertyDescriptor(prototype, 'value').set.call(node, value);
      node.dispatchEvent(new Event('input', { bubbles: true })); node.dispatchEvent(new Event('change', { bubbles: true }));
      await new Promise((resolve) => setTimeout(resolve, 30));
    };
    await set_value(element('prompt_draft'), 'EDITED ENGLISH RETRY BRIEF');
    const bridge = window.__TAURI_INTERNALS__; const original = bridge.invoke; const calls = [];
    bridge.invoke = async (command, args) => {
      if (command === 'prompt_enhance_run') {
        calls.push(args.request);
        if (calls.length === 1) throw { code: 'prompt_enhance_upstream_timeout', retryable: true };
      }
      return original(command, args);
    };
    try {
      element('prompt_optimize').click(); await wait_for(() => element('prompt_retry'));
      element('prompt_retry').click(); await wait_for(() => !element('prompt_optimize').disabled && !element('prompt_error'));
      const retry_exact = calls.length === 2 && calls.every((entry) => entry.stage === 'en_optimize' && entry.text === 'EDITED ENGLISH RETRY BRIEF') && JSON.stringify(calls[0].workspaceContext) === JSON.stringify(calls[1].workspaceContext) && element('prompt_draft').value === 'EDITED ENGLISH RETRY BRIEF';
      document.querySelector('.prompt_advanced').open = true;
      await set_value(element('prompt_instruction'), 'Retain this optional instruction');
      const saved_template = element('prompt_template_zh').value;
      await set_value(element('prompt_template_zh'), saved_template + ' unsaved template');
      await set_value(element('workspace_destination'), 'suppliers');
      const template_guarded = Boolean(element('prompt_page')) && element('prompt_template_zh').value.endsWith(' unsaved template');
      await set_value(element('prompt_template_zh'), saved_template);
      await set_value(element('workspace_destination'), 'suppliers'); await wait_for(() => !element('prompt_page'));
      await set_value(element('workspace_destination'), 'prompt'); await wait_for(() => element('prompt_instruction'));
      return { ok: retry_exact && template_guarded && element('prompt_instruction').value === 'Retain this optional instruction', retry_exact, template_guarded };
    } finally { bridge.invoke = original; }
  });
}

if (process.argv[1] && import.meta.url === (await import('node:url')).pathToFileURL(process.argv[1]).href) {
  const { create_smoke_runtime, find_browser, sleep } = await import('./smoke_runtime.mjs');
  const { attach_page } = await import('./cdp_client.mjs');
  const { check_in_page } = await import('./page_checks.mjs');
  let failures = 0;
  const check = (name, ok, detail = '') => { if (!ok) failures++; process.stdout.write(`${ok ? 'PASS' : 'FAIL'} ${name} ${detail}\n`); };
  const runtime = await create_smoke_runtime(find_browser(), { built_demo: process.argv.includes('--built-demo') });
  let cdp;
  try {
    cdp = await attach_page(runtime.cdp_origin);
    await cdp.send('Page.navigate', { url: runtime.demo_url });
    for (let attempt = 0; attempt < 200 && !await cdp.eval_value('window.__aagDemoIsDemo && !!document.querySelector("[data-testid=workspace_destination]")'); attempt++) await sleep(50);
    await run_prompt_reliability_checks({ cdp, check_in_page, check });
    check('no uncaught prompt ownership errors', cdp.exceptions.length === 0);
  } finally { await runtime.close(cdp); }
  if (failures) process.exitCode = 1;
}
