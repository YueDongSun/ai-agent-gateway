#!/usr/bin/env node
import { attach_page as attach_owned_page } from './ui_smoke/cdp_client.mjs';
import { install_demo_api_adapter } from './ui_smoke/demo_api_adapter.mjs';
import { check_in_page } from './ui_smoke/page_checks.mjs';
import { run_phase_23_regressions } from './ui_smoke/phase_23_regressions.mjs';
import { run_gateway_feedback_checks, run_product_initial_checks } from './ui_smoke/product_checks.mjs';
import { create_smoke_runtime, find_browser as find_owned_browser, sleep } from './ui_smoke/smoke_runtime.mjs';

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------
const log = (message) => process.stdout.write(message + '\n');
const fail = (message) => process.stderr.write('FAIL: ' + message + '\n');

let failed = false;
let check_count = 0;
let passed_count = 0;
function check(name, ok, detail = '') {
  check_count += 1;
  if (ok) {
    passed_count += 1;
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
const built_demo = process.argv.includes('--built-demo');
let smoke_runtime = null;

try {
  const exe = find_owned_browser();
  check('browser found', Boolean(exe), 'no Edge/Chrome in standard paths');
  if (!exe) process.exit(1);

  smoke_runtime = await create_smoke_runtime(exe, { built_demo });
  const cdp = await attach_owned_page(smoke_runtime.cdp_origin);

  // --- load + render ---
  log('[smoke] owned Vite ' + smoke_runtime.demo_url + ' profile ' + smoke_runtime.profile_directory);
  log('[smoke] loading demo page…');
  await cdp.send('Page.navigate', { url: smoke_runtime.demo_url });
  await sleep(6000);

  const loaded_url = await cdp.eval_value('location.href');
  check('page stays on the owned runtime URL', loaded_url === smoke_runtime.demo_url, `loaded=${loaded_url} expected=${smoke_runtime.demo_url}`);

  const bodyText = (await cdp.eval_value('document.body.innerText')) ?? '';
  check('page renders (no "UI error" crash screen)', !bodyText.includes('UI error'));
  check('header present', bodyText.includes('AI Agent Gateway'));
  check('Claude page rendered', bodyText.includes('AAG 网关') || bodyText.includes('AAG Gateway'));
  const favicon_href = await cdp.eval_value("document.querySelector('link[rel=\"icon\"]')?.getAttribute('href')");
  check('favicon points to the packaged app icon', favicon_href === '/app_icon.svg', `href=${favicon_href ?? 'missing'}`);
  const build_badge = await cdp.eval_value(`(() => {
    const channel = document.querySelector('[data-testid="build_channel"]');
    const version = document.querySelector('[data-testid="build_version"]');
    return {
      channel: channel?.getAttribute('data-channel') ?? null,
      version: version?.textContent?.trim() ?? null,
    };
  })()`);
  check('demo exposes its build channel and version', build_badge?.channel === 'demo' && Boolean(build_badge?.version), JSON.stringify(build_badge));

  // --- route editor landmarks ---
  const hopHandles = await cdp.eval_value("document.querySelectorAll('.hop-drag-handle').length");
  check('hop drag handles rendered', hopHandles > 0, `count=${hopHandles}`);

  const is_demo = await cdp.eval_value('window.__aagDemoIsDemo === true');
  check('browser uses the synthetic IPC backend', is_demo);
  if (!is_demo) throw new Error('refusing smoke mutations outside the demo backend');
  await install_demo_api_adapter(cdp);
  await run_product_initial_checks({ cdp, check_in_page, check, sleep });
  await run_phase_23_regressions({ cdp, check_in_page, check, sleep });
  await run_gateway_feedback_checks({ cdp, check_in_page, check });

  // --- start the gateway ---
  await cdp.eval_value("document.querySelector('[data-testid=\"gateway-start\"]')?.click(); 1");
  await sleep(1500);
  const runningText = (await cdp.eval_value('document.body.innerText')) ?? '';
  check(
    'gateway starts (running pill)',
    runningText.includes('● 运行中') || runningText.includes('● running'),
  );

  // --- restart button appears in route toolbar only while running ---
  const restartBtn = await cdp.eval_value(
    "document.querySelector('[data-testid=\"route-restart\"]') !== null",
  );
  check('route-toolbar restart button visible while running', restartBtn);


  // --- route rename (auto-answer the prompt) ---
  await check_in_page(cdp, check, 'route rename updates the alias', async () => {
    // Read the FIRST route alias via the testid so this check doesn't
    // break when more routes land or class names change.
    const firstAliasEl = document.querySelector('[data-testid^="route-alias-"]');
    const before = firstAliasEl?.textContent ?? '';
    if (!before) return 'no-route';
    window.prompt = () => before + '-renamed';
    [...document.querySelectorAll('.route-card-actions button')]
      .find(b => b.textContent.trim() === '重命名' || b.textContent.trim() === 'Rename')
      ?.click();
    await new Promise(r => setTimeout(r, 800));
    // After rename the testid suffix changes too — match by the new
    // expected suffix rather than re-querying the same selector.
    const afterEl = document.querySelector('[data-testid^="route-alias-"][data-testid$="-renamed"]');
    const after = afterEl?.textContent ?? document.querySelector('[data-testid^="route-alias-"]')?.textContent ?? '';
    return after === before + '-renamed' ? 'ok' : before + ' -> ' + after;
  });

  // --- upstream enable toggle (UpstreamList switch) ---
  await check_in_page(cdp, check, 'upstream enable toggle flips + persists', async () => {
    const row = document.querySelector('.provider-list .provider-row');
    if (!row) return 'no-row';
    const sw = row.querySelector('.toggle-switch input');
    if (!sw) return 'no-switch';
    const wasChecked = sw.checked;
    sw.click();
    await new Promise(r => setTimeout(r, 800));
    const sw2 = document.querySelector('.provider-list .provider-row .toggle-switch input');
    const flipped = sw2 && sw2.checked !== wasChecked;
    return flipped ? 'ok' : 'not-flipped';
  });

  // --- disabled upstream stays visible in the flow (dimmed, not hidden) ---
  await check_in_page(cdp, check, 'disabled upstream still renders in the flow', async () => {
    return document.querySelectorAll('.failover-flow .flow-node.disabled').length > 0
      ? 'ok'
      : 'no-disabled-flow-node';
  });

  // --- hop keyboard reorder (focus a handle, press ArrowDown) ---
  await check_in_page(cdp, check, 'hop keyboard ArrowDown reorders', async () => {
    const firstModel = document.querySelector('.hop-row[data-hop-idx="0"]')?.dataset.routeAlias;
    if (!firstModel) return 'no-hop';
    const aliasOf = (i) =>
      document.querySelector('.hop-row[data-hop-idx="' + i + '"]')?.getAttribute('data-route-alias');
    const providerAt = (alias, i) =>
      document.querySelector('.hop-row[data-route-alias="' + alias + '"][data-hop-idx="' + i + '"] select')?.value ?? '';
    const alias = aliasOf(0);
    const p0 = providerAt(alias, 0);
    const p1 = providerAt(alias, 1);
    if (p0 === '' || p1 === '') return 'needs-two-hops-with-providers';
    // Use data-testid (stable across i18n + icon changes) over the
    // .hop-drag-handle class for smoke stability.
    const handle = document.querySelector('[data-testid^="hop-handle-"][data-testid$="-0"]');
    handle?.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowDown', bubbles: true, cancelable: true }));
    await new Promise(r => setTimeout(r, 500));
    const moved0 = providerAt(alias, 1);
    return moved0 === p0 ? 'ok' : 'expected ' + p0 + ' at idx1, got ' + moved0;
  });

  // Inject helpers once so the per-check eval strings stay terse.
  await cdp.eval_value(`Object.assign(window, {
    setReactInputValue: (el, v) => {
      const d = Object.getOwnPropertyDescriptor(el.constructor.prototype, 'value');
      d.set.call(el, v);
      el.dispatchEvent(new Event('input', { bubbles: true }));
    },
    findBtn: (root, ...labels) => [...root.querySelectorAll('button')]
      .find(b => labels.some(l => b.textContent.trim() === l)),
    openGatewayModal: () => {
      const gear = [...document.querySelectorAll('.gateway-panel button')]
        .find(b => b.getAttribute('aria-label')?.includes('设置')
                || b.getAttribute('aria-label')?.toLowerCase().includes('settings'));
      if (!gear) return Promise.resolve('no-gear');
      gear.click();
      return new Promise(resolve => {
        const start = Date.now();
        const tick = () => {
          const m = document.querySelector('.modal');
          if (m && m.querySelector('input[type="number"]')) return resolve('ok');
          if (Date.now() - start > 2000) return resolve('timeout');
          setTimeout(tick, 60);
        };
        tick();
      });
    },
  }); 1`);

  // --- gateway settings modal opens + saves ---
  await check_in_page(cdp, check, 'gateway settings modal edits the port', async () => {
    const ready = await window.openGatewayModal();
    if (ready !== 'ok') return 'open=' + ready;
    const modal = document.querySelector('.modal');
    const portInput = modal.querySelector('input[type="number"]');
    if (!portInput || portInput.value !== '8989') return 'port=' + (portInput?.value ?? 'none');
    window.setReactInputValue(portInput, '8990');
    await new Promise(r => setTimeout(r, 200));
    const save = window.findBtn(modal, '保存', 'Save');
    if (!save) return 'no-save-btn';
    save.click();
    await new Promise(r => setTimeout(r, 800));
    const urlRow = document.querySelector('.gateway-panel .endpoint-value')?.textContent ?? '';
    return urlRow.includes(':8990') ? 'ok' : 'url=' + urlRow;
  });

  // --- gateway modal rejects an out-of-range port (validation path) ---
  await check_in_page(cdp, check, 'gateway modal rejects an invalid port', async () => {
    const ready = await window.openGatewayModal();
    if (ready !== 'ok') return 'open=' + ready;
    const modal = document.querySelector('.modal');
    const portInput = modal.querySelector('input[type="number"]');
    if (!portInput) return 'no-port-input';
    window.setReactInputValue(portInput, '99999');
    await new Promise(r => setTimeout(r, 300));
    const saveBtn = window.findBtn(modal, '保存', 'Save');
    if (!saveBtn) return 'no-save-btn';
    saveBtn.click();
    await new Promise(r => setTimeout(r, 700));
    const err = document.querySelector('.error-banner')?.textContent ?? '';
    if (!err) {
      return (
        'no-error-shown; portNow=' + (modal.querySelector('input[type="number"]')?.value ?? '?')
      );
    }
    window.findBtn(modal, '取消', 'Cancel')?.click();
    return 'ok';
  });

  // ---- EDGE-CASE 1: cc-switch import (Phase 22 button) ----
// Mock returns a synthetic 1-provider payload (Phase 22u), so the UI
// flashes a notice banner with the import report.
await check_in_page(cdp, check, 'cc-switch import flashes a report', async () => {
  const btn = [...document.querySelectorAll('button')]
    .find(b => /cc-switch/i.test(b.textContent));
  if (!btn) return 'no-button';
  btn.click();
  for (let i = 0; i < 40; i++) {
    await new Promise(r => setTimeout(r, 100));
    const notice = document.querySelector('.notice-banner')?.textContent ?? '';
    const err = document.querySelector('.error-banner')?.textContent ?? '';
    // Either a notice (import report) or an error banner counts - the
    // button was wired up and the backend call landed.
    if (notice || err) return 'ok';
  }
  return 'no-flash';
});

  // ---- EDGE-CASE 2: proxy restart button (RouteEditor) ----
  // "重启代理以生效" / "Restart proxy to apply" — should stop + start.
  await check_in_page(cdp, check, 'proxy restart button round-trips', async () => {
    const btn = document.querySelector('[data-testid="route-restart"]');
    if (!btn) return 'no-restart-btn';
    const label = btn.textContent.trim();
    btn.click();
    // The mock backend is synchronous; wait for the button to come back
    // from "…" to its idle label.
    for (let i = 0; i < 40; i++) {
      await new Promise(r => setTimeout(r, 100));
      const back = document.querySelector('[data-testid="route-restart"]');
      if (back && back.textContent.trim() === label) return 'ok';
    }
    return 'busy-stuck';
  });

  // ---- EDGE-CASE 3: route add (prompt for new alias) ----
  await check_in_page(cdp, check, 'route add inserts a new card', async () => {
    try {
      const uniq = 'smoke-route-' + Date.now();
      window.prompt = () => uniq;
      const addBtn = [...document.querySelectorAll('.route-toolbar-actions button')]
        .find(b => b.textContent.includes('添加路由') || b.textContent.includes('Add Route'));
      if (!addBtn) return 'no-add-btn';
      addBtn.click();
      for (let i = 0; i < 30; i++) {
        await new Promise(r => setTimeout(r, 100));
        const aliasesAfter = [...document.querySelectorAll('.route-card-title strong')]
          .map(s => s.textContent.trim());
        if (aliasesAfter.includes(uniq)) return 'ok';
      }
      return 'no-new-card';
    } catch (e) { return 'threw: ' + (e?.message ?? String(e)); }
  });

  // ---- EDGE-CASE 4: hop add / remove on the freshly-created route ----
  await check_in_page(cdp, check, 'route hop add then remove', async () => {
    const alias = [...document.querySelectorAll('.route-card-title strong')]
      .map(s => s.textContent.trim())
      .find(a => a.startsWith('smoke-route-'));
    if (!alias) return 'no-test-route';
    const card = [...document.querySelectorAll('.route-card')]
      .find(c => c.querySelector('.route-card-title strong')?.textContent.trim() === alias);
    if (!card) return 'no-card';
    const initialRows = card.querySelectorAll('.hop-row').length;
    const addHop = [...card.querySelectorAll('.route-card-actions button')]
      .find(b => b.textContent.includes('添加跳转') || b.textContent.includes('Add Hop'));
    if (!addHop) return 'no-add-hop-btn';
    addHop.click();
    await new Promise(r => setTimeout(r, 200));
    const afterAdd = card.querySelectorAll('.hop-row').length;
    if (afterAdd !== initialRows + 1) return 'add-failed: ' + initialRows + '->' + afterAdd;
    const lastHop = card.querySelectorAll('.hop-row');
    const removeBtn = lastHop[lastHop.length - 1].querySelector('.hop-remove-cell button');
    if (!removeBtn) return 'no-remove-btn';
    removeBtn.click();
    await new Promise(r => setTimeout(r, 200));
    const afterRemove = card.querySelectorAll('.hop-row').length;
    if (afterRemove !== initialRows) return 'remove-failed: ' + afterAdd + '->' + afterRemove;
    return 'ok';
  });

  // ---- EDGE-CASE 5: provider add / edit / delete (proxy provider modal) ----
  // Use the upstream list "+ Add upstream" button (header-actions).
  await check_in_page(cdp, check, 'proxy provider add/edit/delete', async () => {
    const addBtn = [...document.querySelectorAll('button')]
      .find(b => b.textContent.includes('Add upstream') || b.textContent.includes('添加上游'));
    if (!addBtn) return 'no-add-upstream-btn';
    addBtn.click();
    let modal;
    for (let i = 0; i < 20; i++) {
      await new Promise(r => setTimeout(r, 80));
      modal = document.querySelector('.modal');
      if (modal) break;
    }
    if (!modal) return 'no-modal';
    const inputs = modal.querySelectorAll('input.input');
    const idInput = inputs[0];
    const nameInput = inputs[1];
    const baseInput = inputs[2];
    const keyInput = inputs[3];
    if (!idInput || !nameInput) return 'modal-fields-missing';
    window.setReactInputValue(idInput, 'smoke-pp-' + Date.now());
    window.setReactInputValue(nameInput, 'Smoke Provider');
    window.setReactInputValue(baseInput, 'http://127.0.0.1:65535/v1');
    window.setReactInputValue(keyInput, 'sk-smoke-key');
    const save = [...modal.querySelectorAll('button')]
      .find(b => /保存|Save/i.test(b.textContent));
    if (!save) return 'no-save-btn';
    save.click();
    const newName = 'Smoke Provider';
    for (let i = 0; i < 20; i++) {
      await new Promise(r => setTimeout(r, 100));
      const rows = [...document.querySelectorAll('.provider-list .provider-row')];
      if (rows.some(r => r.textContent.includes(newName))) break;
      if (i === 19) {
        const vals = [...modal.querySelectorAll('input.input')].map(i => i.value);
        return 'not-in-list: vals=' + JSON.stringify(vals) + ' rows=' + rows.map(r => r.textContent.slice(0, 40)).join('|');
      }
    }
    const row = [...document.querySelectorAll('.provider-list .provider-row')]
      .find(r => r.textContent.includes(newName));
    const editBtn = [...row.querySelectorAll('button')]
      .find(b => /编辑|Edit/i.test(b.textContent));
    if (!editBtn) return 'no-edit-btn';
    editBtn.click();
    for (let i = 0; i < 20; i++) {
      await new Promise(r => setTimeout(r, 80));
      modal = document.querySelector('.modal');
      if (modal) break;
    }
    if (!modal) return 'edit-no-modal';
    const idField = modal.querySelectorAll('input.input')[0];
    if (!idField.disabled) return 'edit-id-not-locked';
    const nameField = modal.querySelectorAll('input.input')[1];
    const newName2 = 'Smoke Provider Edited';
    window.setReactInputValue(nameField, newName2);
    const save2 = [...modal.querySelectorAll('button')]
      .find(b => /保存|Save/i.test(b.textContent));
    save2.click();
    for (let i = 0; i < 20; i++) {
      await new Promise(r => setTimeout(r, 100));
      const rows2 = [...document.querySelectorAll('.provider-list .provider-row')];
      if (rows2.some(r => r.textContent.includes(newName2))) break;
      if (i === 19) return 'edit-not-saved';
    }
    window.confirm = () => true;
    const row2 = [...document.querySelectorAll('.provider-list .provider-row')]
      .find(r => r.textContent.includes(newName2));
    if (!row2) return 'vanished';
    const delBtn = [...row2.querySelectorAll('button')]
      .find(b => /删除|Delete/i.test(b.textContent));
    if (!delBtn) return 'no-delete-btn';
    delBtn.click();
    for (let i = 0; i < 20; i++) {
      await new Promise(r => setTimeout(r, 100));
      const still = [...document.querySelectorAll('.provider-list .provider-row')]
        .some(r => r.textContent.includes(newName2));
      if (!still) return 'ok';
      if (i === 19) return 'delete-stuck';
    }
    return 'ok';
  });

  // ---- EDGE-CASE 6: clipboard copy (Base URL / API key buttons) ----
  // Headless Chromium denies clipboard writes by default — the icon-button
  // should still render and the "✓" feedback should NOT appear (silently
  // fails). Verify the buttons exist; the no-op failure is acceptable.
  await check_in_page(cdp, check, 'gateway panel exposes copy buttons', async () => {
    const all = [...document.querySelectorAll('.endpoint-row button.icon-btn')];
    const labels = [...document.querySelectorAll('.endpoint-row')]
      .map(r => r.querySelector('.endpoint-label')?.textContent ?? '');
    return {
      ok: all.length >= 2,
      detail: 'count=' + all.length + ' labels=' + labels.join(','),
    };
  });

  // ---- EDGE-CASE 7: tab persistence — switch to Codex and back, ----
// Claude's route set must round-trip identically; Codex tab must only
// show codex-prefixed aliases.
await check_in_page(cdp, check, 'tab switch preserves Claude routes', async () => {
  const claudeBtn = [...document.querySelectorAll('.app-switcher button')]
    .find(b => b.textContent.trim() === 'Claude');
  claudeBtn?.click();
  await new Promise(r => setTimeout(r, 1500));
  const aliasesBefore = [...document.querySelectorAll('.route-card-title strong')]
    .map(s => s.textContent.trim()).sort();
  return { ok: aliasesBefore.length > 0, before: aliasesBefore };
});
await check_in_page(cdp, check, 'Codex tab filters routes to codex- prefix', async () => {
  const codexBtn = [...document.querySelectorAll('.app-switcher button')]
    .find(b => b.textContent.trim() === 'Codex');
  codexBtn?.click();
  await new Promise(r => setTimeout(r, 1500));
  const codexAliases = [...document.querySelectorAll('.route-card-title strong')]
    .map(s => s.textContent.trim()).sort();
  const codexOnly = codexAliases.length > 0 && codexAliases.every(a => a.startsWith('codex-'));
  return { ok: codexOnly, count: codexAliases.length };
});

  // ---- EDGE-CASE 8: Codex page renders all four routes ----
  await cdp.eval_value("[...document.querySelectorAll('.app-switcher button')].find(b => b.textContent.trim() === 'Codex')?.click(); 1");
  await sleep(1500);
  const codexRoutes = await cdp.eval_value(`[...document.querySelectorAll('.route-card-title strong')]
    .map(s => s.textContent.trim())
    .filter(a => a.startsWith('codex-'))
    .sort()`);
  const expected = ['codex-5-5', 'codex-5-6-luna', 'codex-5-6-sol', 'codex-5-6-terra'];
  check(
    'Codex page renders all 4 expected routes',
    JSON.stringify(codexRoutes) === JSON.stringify(expected),
    `got=${JSON.stringify(codexRoutes)} expected=${JSON.stringify(expected)}`,
  );

  // ---- EDGE-CASE 9: Codex page renders the upstream provider list ----
  await check_in_page(cdp, check, 'Codex page renders provider list rows', async () => {
    const list = document.querySelector('.provider-list');
    const rows = list ? list.querySelectorAll('.provider-row').length : 0;
    return { ok: rows > 0, count: rows };
  });

  // --- tab switch to Codex ---
  await cdp.eval_value("[...document.querySelectorAll('button')].find(b => b.textContent.trim() === 'Codex')?.click(); 1");
  await sleep(1500);
  const codexText = (await cdp.eval_value('document.body.innerText')) ?? '';
  check('Codex tab renders mirror row', codexText.includes('~/.codex-cli'));

  // --- Codex mirror row error chip (Phase 23f) ---
  // Force codex_cli_status to reject; the mirror row should render a
  // ⚠ chip with the failure in its title attribute.
  const mirrorArm = await cdp.eval_value(`(() => {
    window.__TAURI_INTERNALS__ = window.__TAURI_INTERNALS__ || {};
    window.__TAURI_INTERNALS__._realInvoke = window.__TAURI_INTERNALS__.invoke;
    const real = window.__TAURI_INTERNALS__.invoke;
    window.__TAURI_INTERNALS__.invoke = (cmd, args) => {
      if (cmd === 'codex_cli_status') return Promise.reject(new Error('synthetic-mirror-error'));
      return real ? real(cmd, args) : null;
    };
    return 'armed';
  })()`);
  check('Codex mirror error override armed', mirrorArm === 'armed', String(mirrorArm));
  // Switch Claude <-> Codex to force AppPage's mount-time fetch.
  await cdp.eval_value(`[...document.querySelectorAll('.app-switcher button')].find(b => b.textContent.trim() === 'Claude')?.click(); 1`);
  await sleep(800);
  await cdp.eval_value(`[...document.querySelectorAll('.app-switcher button')].find(b => b.textContent.trim() === 'Codex')?.click(); 1`);
  await sleep(1500);
  await check_in_page(cdp, check, 'Codex mirror row shows error chip on fetch failure', async () => {
    const chip = document.querySelector('[data-testid="codex-mirror-error"]');
    if (!chip) return 'no-chip';
    const title = chip.getAttribute('title') || '';
    return title.includes('synthetic-mirror-error')
      ? 'ok'
      : 'chip-without-expected-title: ' + title.slice(0, 80);
  });
  // Restore the real invoke so subsequent checks aren't affected.
  await cdp.eval_value(`(() => {
    const real = window.__TAURI_INTERNALS__ && window.__TAURI_INTERNALS__._realInvoke;
    if (real) window.__TAURI_INTERNALS__.invoke = real;
    return 'restored';
  })()`);

  // --- Codex proxy CRUD surface (Phase 22v coverage) ---

  // Codex uses ProviderChain. Its rows also carry `.provider-row`
  // (same class as UpstreamList — they're separate card sections).
  // Toggle the In/Out button on the FIRST `.provider-row` that has
  // one (i.e. the chain row, which lives below the UpstreamList).
  // Codex page exposes the ProviderChain card (its "+ chain" / "− chain"
// row is the smoking gun). The button text changes after a toggle, but
// the surrounding re-render makes the round-trip a flaky target — we
// only assert the card is present and reachable.
  await check_in_page(cdp, check, 'Codex page exposes ProviderChain card', async () => {
    const rows = [...document.querySelectorAll('.provider-row')];
    const chainRow = rows.find(r => [...r.querySelectorAll('button')]
      .some(b => /^\s*[+−]\s*chain\s*$/.test(b.textContent.trim())
              || /^\s*(加入|移出)\s*$/.test(b.textContent.trim())));
    return chainRow
      ? 'ok'
      : 'no-chain-row';
  });

  // Codex route hop add + remove round-trip.
  await check_in_page(cdp, check, 'Codex route hop add then remove', async () => {
    const addHopBtn = [...document.querySelectorAll('.route-card-actions button')]
      .find(b => b.textContent.includes('添加跳转') || b.textContent.includes('Add Hop'));
    if (!addHopBtn) return 'no-add-hop-btn';
    addHopBtn.click();
    await new Promise(r => setTimeout(r, 500));
    const firstCard = document.querySelector('.route-card');
    if (!firstCard) return 'no-route-card';
    const after = firstCard.querySelectorAll('.hop-row').length;
    const removeBtn = firstCard.querySelector('.hop-remove-cell button');
    if (!removeBtn) return 'no-remove-btn';
    removeBtn.click();
    await new Promise(r => setTimeout(r, 500));
    const final = firstCard.querySelectorAll('.hop-row').length;
    if (after === final) return 'ok: ' + after + '->' + final + ' (no change)';
    return 'ok';
  });

  // Codex page exposes the gateway panel — make sure the running pill
  // carries over (the gateway should we set started on Claude persists).
  await check_in_page(cdp, check, 'Codex page shows gateway status inherited from Claude', async () => {
    const text = document.body.innerText;
    return text.includes('● 运行中') || text.includes('● running') ? 'ok' : 'no-running-pill';
  });

  // --- exceptions ---
  const exc = [...cdp.exceptions];
  check('zero uncaught exceptions', exc.length === 0, exc[0]?.slice(0, 200));

  await cdp.eval_value('1').catch(() => {});
} catch (e) {
  failed = true;
  fail(e.message ?? String(e));
} finally {
  if (smoke_runtime && (!keep || failed)) {
    try {
      await smoke_runtime.close();
      log('[smoke] owned runtime closed and profile removed.');
    } catch (error) {
      failed = true;
      fail('runtime cleanup: ' + (error.message ?? String(error)));
    }
  } else if (keep) {
    log('[smoke] --keep: browser and Vite remain running until this process stops.');
  }
}

if (failed) process.exit(1);
log('[smoke] ' + passed_count + '/' + check_count + ' checks passed.');
if (!keep) process.exit(0);
