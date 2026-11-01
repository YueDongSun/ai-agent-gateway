/** Delayed synthetic IPC verifies recheck ownership and truthful partial outcomes. */
export async function run_provider_recheck_checks({ cdp, check, check_in_page }) {
  await check_in_page(cdp, check, 'provider recheck preserves draft thresholds and refreshes preflight truth without duplicate submission', async () => {
    if (window.__aagDemoIsDemo !== true) throw new Error('synthetic IPC required');
    const element = (id) => document.querySelector('[data-testid="' + id + '"]');
    const wait_for = async (read) => {
      for (let index = 0; index < 120; index++) {
        const result = await read(); if (result) return result;
        await new Promise((resolve) => setTimeout(resolve, 30));
      }
      throw new Error('provider recheck interaction timed out');
    };
    const panel = await wait_for(() => element('provider-screening-panel'));
    await wait_for(() => panel.querySelector('[data-testid="provider-screening-recheck"]:not(:disabled)'));
    const latency = element('provider-screening-latency');
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(latency, '6000');
    latency.dispatchEvent(new Event('input', { bubbles: true }));
    latency.dispatchEvent(new Event('change', { bubbles: true }));
    await new Promise((resolve) => setTimeout(resolve, 30));
    const bridge = window.__TAURI_INTERNALS__; const original = bridge.invoke;
    const overview = structuredClone(await original('provider_screening_overview', {}));
    overview.enforced = false;
    const selected = overview.entries[1];
    let release; let checks = 0; let maintenance_calls = 0;
    const gate = new Promise((resolve) => { release = resolve; });
    bridge.invoke = async (command, args) => {
      if (command === 'supplier_import_maintain') { maintenance_calls++; throw new Error('unexpected synthetic maintenance'); }
      if (command === 'provider_screening_recheck') {
        checks++; await gate;
        return {
          providerId: args.providerId, checkStarted: false, eligible: null,
          checkError: 'provider_subscription_source_missing',
          changes: { policyChanged: false, registrationChanged: false, admissionCommitted: false, sourceCommitted: false },
          admissionVersion: null, runtimeReconciled: null, runtimeError: null, overview, overviewError: null,
        };
      }
      return original(command, args);
    };
    try {
      const button = [...panel.querySelectorAll('[data-testid="provider-screening-entry"]')]
        .find((entry) => entry.textContent.includes(selected.name)).querySelector('[data-testid="provider-screening-recheck"]');
      button.click();
      await wait_for(() => checks === 1 && button.disabled);
      button.click();
      const owns = [...panel.querySelectorAll('button, input')].every((node) => node.disabled);
      const destination = element('workspace_destination');
      Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, 'value').set.call(destination, 'usage');
      destination.dispatchEvent(new Event('change', { bubbles: true }));
      await new Promise((resolve) => setTimeout(resolve, 30));
      const busy_navigation_blocked = element('workspace_header').getAttribute('data-view') === 'suppliers'
        && panel.querySelector('[role="status"]')?.textContent.includes('Wait for the selected check');
      release();
      await wait_for(() => !element('provider-screening-refresh').disabled);
      const current_panel = element('provider-screening-panel');
      const actual_policy = current_panel.querySelector('[data-testid="provider-screening-counts"]').textContent.includes('Admission not activated');
      const prerequisite = current_panel.querySelector('[role="alert"]')?.textContent.includes('CC Switch');
      const retained = element('provider-screening-latency').value === '6000'
        && ![...current_panel.querySelectorAll('button')].find((node) => node.textContent === 'Save thresholds').disabled;
      return { ok: owns && busy_navigation_blocked && checks === 1 && maintenance_calls === 0 && actual_policy && prerequisite && retained, owns, busy_navigation_blocked, checks, maintenance_calls, actual_policy, prerequisite, retained };
    } finally { release(); bridge.invoke = original; }
  });

  await check_in_page(cdp, check, 'provider recheck shows committed admission and current overview after runtime apply failure', async () => {
    const element = (id) => document.querySelector('[data-testid="' + id + '"]');
    const wait_for = async (read) => {
      for (let index = 0; index < 120; index++) { const result = await read(); if (result) return result; await new Promise((resolve) => setTimeout(resolve, 30)); }
      throw new Error('provider partial result timed out');
    };
    const bridge = window.__TAURI_INTERNALS__; const original = bridge.invoke;
    const overview = structuredClone(await original('provider_screening_overview', {}));
    for (const entry of overview.entries) entry.admittedModels = [];
    let checks = 0; let parent_reads = 0; let overview_reads = 0; let maintenance_calls = 0;
    bridge.invoke = async (command, args) => {
      if (command === 'supplier_import_maintain') { maintenance_calls++; throw new Error('unexpected synthetic maintenance'); }
      if (command === 'provider_screening_overview') { overview_reads++; return structuredClone(overview); }
      if (command === 'supplier_status') parent_reads++;
      if (command === 'provider_screening_recheck') {
        checks++;
        return {
          providerId: args.providerId, checkStarted: true, eligible: null, checkError: 'provider_admission_conflict',
          changes: { policyChanged: false, registrationChanged: false, admissionCommitted: true, sourceCommitted: true },
          admissionVersion: 3, runtimeReconciled: false, runtimeError: 'provider_screening_runtime_reconcile_failed',
          overview: structuredClone(overview), overviewError: null,
        };
      }
      return original(command, args);
    };
    try {
      await wait_for(() => element('provider-screening-recheck') && !element('provider-screening-recheck').disabled);
      element('provider-screening-recheck').click();
      await wait_for(() => checks === 1 && parent_reads > 0 && !element('provider-screening-refresh').disabled);
      const panel = element('provider-screening-panel');
      const saved = panel.querySelector('[role="status"]')?.textContent.includes('selected admission state was saved');
      const apply_failed = panel.querySelector('[role="alert"]')?.textContent.includes('Runtime reconciliation failed');
      const truth = element('provider-screening-counts').textContent.includes('0 admitted providers');
      const retained = element('provider-screening-latency').value === '6000';
      return { ok: checks === 1 && parent_reads > 0 && maintenance_calls === 0 && saved && apply_failed && truth && retained, checks, parent_reads, overview_reads, maintenance_calls, saved, apply_failed, truth, retained };
    } finally { bridge.invoke = original; }
  });

  await check_in_page(cdp, check, 'successful selected recheck refreshes parent views without invoking source-wide maintenance', async () => {
    const element = (id) => document.querySelector('[data-testid="' + id + '"]');
    const wait_for = async (read) => {
      for (let index = 0; index < 120; index++) { const result = await read(); if (result) return result; await new Promise((resolve) => setTimeout(resolve, 30)); }
      throw new Error('provider successful result timed out');
    };
    const bridge = window.__TAURI_INTERNALS__; const original = bridge.invoke;
    const overview = structuredClone(await original('provider_screening_overview', {}));
    let checks = 0; let parent_reads = 0; let maintenance_calls = 0;
    bridge.invoke = async (command, args) => {
      if (command === 'supplier_import_maintain') { maintenance_calls++; throw new Error('unexpected synthetic maintenance'); }
      if (command === 'supplier_status') parent_reads++;
      if (command === 'provider_screening_recheck') {
        checks++;
        return {
          providerId: args.providerId, checkStarted: true, eligible: true, checkError: null,
          changes: { policyChanged: false, registrationChanged: false, admissionCommitted: true, sourceCommitted: true },
          admissionVersion: 4, runtimeReconciled: true, runtimeError: null,
          overview: structuredClone(overview), overviewError: null,
        };
      }
      return original(command, args);
    };
    try {
      await wait_for(() => element('provider-screening-recheck') && !element('provider-screening-recheck').disabled);
      element('provider-screening-recheck').click();
      await wait_for(() => checks === 1 && parent_reads > 0 && !element('provider-screening-refresh').disabled);
      const notice = element('provider-screening-panel').querySelector('[role="status"]')?.textContent ?? '';
      const saved = notice.includes('selected check was saved') && notice.includes('other provider records');
      return { ok: checks === 1 && parent_reads > 0 && maintenance_calls === 0 && saved, checks, parent_reads, maintenance_calls, saved };
    } finally { bridge.invoke = original; }
  });

  await check_in_page(cdp, check, 'lost recheck acknowledgement reads status and reports unknown without cancellation or automatic resend', async () => {
    const element = (id) => document.querySelector('[data-testid="' + id + '"]');
    const wait_for = async (read) => {
      for (let index = 0; index < 120; index++) { const result = await read(); if (result) return result; await new Promise((resolve) => setTimeout(resolve, 30)); }
      throw new Error('provider unknown outcome timed out');
    };
    const bridge = window.__TAURI_INTERNALS__; const original = bridge.invoke;
    let checks = 0; let status_reads = 0; let maintenance_calls = 0;
    bridge.invoke = async (command, args) => {
      if (command === 'supplier_import_maintain') { maintenance_calls++; throw new Error('unexpected synthetic maintenance'); }
      if (command === 'provider_screening_recheck') { checks++; throw new Error('private fixture acknowledgement'); }
      if (command === 'provider_screening_overview') status_reads++;
      return original(command, args);
    };
    try {
      await wait_for(() => element('provider-screening-recheck') && !element('provider-screening-recheck').disabled);
      element('provider-screening-recheck').click();
      await wait_for(() => checks === 1 && status_reads > 0 && !element('provider-screening-refresh').disabled);
      const panel = element('provider-screening-panel');
      const unknown = panel.querySelector('[role="alert"]')?.textContent.includes('acknowledgement was lost');
      const not_cancelled = panel.querySelector('[role="alert"]')?.textContent.includes('not cancelled');
      return { ok: checks === 1 && status_reads > 0 && maintenance_calls === 0 && unknown && not_cancelled && !panel.textContent.includes('private fixture acknowledgement'), checks, status_reads, maintenance_calls, unknown, not_cancelled };
    } finally { bridge.invoke = original; }
  });

  await check_in_page(cdp, check, 'screening threshold draft blocks navigation until local discard and cleanup permits destination changes', async () => {
    const element = (id) => document.querySelector('[data-testid="' + id + '"]');
    const wait_for = async (read) => {
      for (let index = 0; index < 120; index++) { const result = await read(); if (result) return result; await new Promise((resolve) => setTimeout(resolve, 30)); }
      throw new Error('screening draft navigation timed out');
    };
    const bridge = window.__TAURI_INTERNALS__; const original = bridge.invoke;
    let writes = 0;
    bridge.invoke = async (command, args) => {
      if (command === 'settings_set' && args.key === 'provider_screening_policy') writes++;
      return original(command, args);
    };
    const navigate = async (view) => {
      const destination = element('workspace_destination');
      Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, 'value').set.call(destination, view);
      destination.dispatchEvent(new Event('change', { bubbles: true }));
      await new Promise((resolve) => setTimeout(resolve, 40));
    };
    try {
      await wait_for(() => element('provider-screening-discard') && !element('provider-screening-discard').disabled);
      await navigate('usage');
      const blocked = element('workspace_header').getAttribute('data-view') === 'suppliers'
        && element('provider-screening-panel').querySelector('[role="status"]')?.textContent.includes('Save or discard threshold edits');
      const saved = await original('provider_screening_overview', {});
      element('provider-screening-discard').click();
      await wait_for(() => !element('provider-screening-discard'));
      const reset = element('provider-screening-latency').value === String(saved.policy.max_latency_ms);
      await navigate('usage');
      await wait_for(() => element('workspace_header').getAttribute('data-view') === 'usage');
      const allowed = !element('provider-screening-panel');
      await navigate('suppliers');
      await wait_for(() => element('provider-screening-recheck') && !element('provider-screening-recheck').disabled);
      return { ok: blocked && reset && allowed && writes === 0 && !element('provider-screening-discard'), blocked, reset, allowed, writes };
    } finally { bridge.invoke = original; }
  });
}

if (process.argv[1] && import.meta.url === (await import('node:url')).pathToFileURL(process.argv[1]).href) {
  const { create_smoke_runtime, find_browser, sleep } = await import('./smoke_runtime.mjs');
  const { attach_page } = await import('./cdp_client.mjs');
  const { check_in_page } = await import('./page_checks.mjs');
  let failed = 0; let total = 0;
  const check = (name, ok, detail = '') => { total++; if (!ok) failed++; process.stdout.write(`${ok ? 'PASS' : 'FAIL'} ${name}${detail ? ': ' + detail : ''}\n`); };
  const runtime = await create_smoke_runtime(find_browser(), { built_demo: process.argv.includes('--built-demo') });
  let cdp = null;
  try {
    cdp = await attach_page(runtime.cdp_origin);
    await cdp.send('Page.addScriptToEvaluateOnNewDocument', { source: `localStorage.setItem('aag.locale', 'en');` });
    await cdp.send('Page.navigate', { url: runtime.demo_url });
    const deadline = Date.now() + 30000;
    while (Date.now() < deadline && !await cdp.eval_value('window.__aagDemoIsDemo && !!document.querySelector("[data-testid=provider-screening-recheck]")')) await sleep(80);
    await run_provider_recheck_checks({ cdp, check, check_in_page });
    check('screening interactions have no uncaught errors', cdp.exceptions.length === 0);
  } finally { await runtime.close(cdp); }
  process.stdout.write(`${total - failed}/${total} provider recheck interaction checks passed\n`);
  if (failed) process.exitCode = 1;
}
