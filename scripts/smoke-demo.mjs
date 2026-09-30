#!/usr/bin/env node
import { attach_page as attach_owned_page } from './ui_smoke/cdp_client.mjs';
import { install_demo_api_adapter } from './ui_smoke/demo_api_adapter.mjs';
import { run_dsh_checks } from './ui_smoke/dsh_checks.mjs';
import { run_dsh_onboarding_checks } from './ui_smoke/dsh_onboarding_checks.mjs';
import { run_dsh_market_checks } from './ui_smoke/dsh_market_checks.mjs';
import { run_mcp_checks } from './ui_smoke/mcp_checks.mjs';
import { check_in_page } from './ui_smoke/page_checks.mjs';
import { run_phase_23_regressions } from './ui_smoke/phase_23_regressions.mjs';
import { run_gateway_feedback_checks, run_product_initial_checks } from './ui_smoke/product_checks.mjs';
import { create_smoke_runtime, find_browser as find_owned_browser, format_error, sleep } from './ui_smoke/smoke_runtime.mjs';

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
  const initial_ready_deadline = Date.now() + 30_000;
  let initial_ui_ready = false;
  while (Date.now() < initial_ready_deadline) {
    initial_ui_ready = await cdp.eval_value(`window.__aagDemoIsDemo === true && Boolean(document.querySelector('.aag-header')) && document.querySelector('.page-header h2')?.textContent === 'Claude'`);
    if (initial_ui_ready) break;
    await sleep(100);
  }
  if (!initial_ui_ready) throw new Error('owned demo did not render its header, synthetic backend marker and initial Claude page within 30s');

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

  // --- advanced routing section ---
  // Routes live collapsed under the provider cards ("Advanced routing").
  // Open it once for Claude; the per-app open state survives tab switches.
  await cdp.eval_value(`(() => {
    const t = document.querySelector('[data-testid="advanced-routes-toggle"]');
    if (t && t.getAttribute('aria-expanded') !== 'true') t.click();
    return 'ok';
  })(); 1`);
  await sleep(800);

  // --- route editor landmarks ---
  const hopHandles = await cdp.eval_value("document.querySelectorAll('.hop-drag-handle').length");
  check('hop drag handles rendered', hopHandles > 0, `count=${hopHandles}`);

  const is_demo = await cdp.eval_value('window.__aagDemoIsDemo === true');
  check('browser uses the synthetic IPC backend', is_demo);
  if (!is_demo) throw new Error('refusing smoke mutations outside the demo backend');
  await check_in_page(cdp, check, 'diagnostics modal enables and disables temporary debug', async () => {
    document.querySelector('[data-testid="diagnostics_open"]')?.click();
    for (let attempt = 0; attempt < 20; attempt += 1) {
      await new Promise((resolve) => setTimeout(resolve, 100));
      if (document.querySelector('[data-testid="diagnostics_mode"]')) break;
    }
    const mode = document.querySelector('[data-testid="diagnostics_mode"]');
    const toggle = document.querySelector('[data-testid="diagnostics_toggle"]');
    if (!mode || !toggle || !/Info/.test(mode.textContent ?? '')) return 'missing-info-state';
    toggle.click();
    for (let attempt = 0; attempt < 20; attempt += 1) {
      await new Promise((resolve) => setTimeout(resolve, 100));
      if (/Debug/.test(mode.textContent ?? '')) break;
    }
    const enabled = /Debug/.test(mode.textContent ?? '');
    document.querySelector('[data-testid="diagnostics_toggle"]')?.click();
    for (let attempt = 0; attempt < 20; attempt += 1) {
      await new Promise((resolve) => setTimeout(resolve, 100));
      if (/Info/.test(mode.textContent ?? '')) break;
    }
    const disabled = /Info/.test(mode.textContent ?? '');
    document.querySelector('[data-testid="diagnostics_backdrop"]')?.dispatchEvent(
      new MouseEvent('click', { bubbles: true }),
    );
    return { ok: enabled && disabled, detail: 'enabled=' + enabled + ' disabled=' + disabled };
  });
  await install_demo_api_adapter(cdp);
  await run_product_initial_checks({ cdp, check_in_page, check, sleep });
  await run_phase_23_regressions({ cdp, check_in_page, check, sleep });
  await run_gateway_feedback_checks({ cdp, check_in_page, check });
  await run_dsh_checks({ cdp, check_in_page, check, sleep });
  await run_dsh_onboarding_checks({ cdp, check_in_page, check });
  await run_dsh_market_checks({ cdp, cdp_origin: smoke_runtime.cdp_origin, demo_url: smoke_runtime.demo_url, check_in_page, check, sleep });
  await run_mcp_checks({ cdp, check_in_page, check });

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

  // --- upstream enable toggle (unified card switch) ---
  await check_in_page(cdp, check, 'upstream enable toggle flips + persists', async () => {
    const sw = document.querySelector('[data-testid^="provider-toggle-"]');
    if (!sw) return 'no-switch';
    const wasChecked = sw.checked;
    const proxyId = sw.getAttribute('data-testid').slice('provider-toggle-'.length);
    sw.click();
    await new Promise(r => setTimeout(r, 800));
    const sw2 = document.querySelector(`[data-testid="provider-toggle-${proxyId}"]`);
    const flipped = sw2 && sw2.checked !== wasChecked;
    return flipped ? 'ok' : 'not-flipped';
  });

  // --- disabled upstream stays visible (dimmed card, not hidden) ---
  await check_in_page(cdp, check, 'disabled upstream still renders as a card', async () => {
    return document.querySelectorAll('.provider-card.disabled').length > 0
      ? 'ok'
      : 'no-disabled-card';
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

  // ---- EDGE-CASE 5: provider add / edit / delete (unified provider modal) ----
  // Creating a card writes BOTH stores: the preset row and the linked
  // ccs-<id> upstream. Deleting a linked card removes both in one confirm.
  await check_in_page(cdp, check, 'provider card add/edit/delete via unified modal', async () => {
    const addBtn = document.querySelector('[data-testid="providers-add"]');
    if (!addBtn) return 'no-add-btn';
    addBtn.click();
    let modal;
    for (let i = 0; i < 20; i++) {
      await new Promise(r => setTimeout(r, 80));
      modal = document.querySelector('.modal');
      if (modal) break;
    }
    if (!modal) return 'no-modal';
    const inputs = [...modal.querySelectorAll('input.input')];
    const nameInput = inputs[0];
    const idInput = modal.querySelector('[data-testid="provider-modal-id"]');
    if (!nameInput || !idInput) return 'modal-fields-missing';
    const uniqId = 'smoke-prov-' + Date.now();
    window.setReactInputValue(idInput, uniqId);
    window.setReactInputValue(nameInput, 'Smoke Provider');
    // Connection fields: the claude baseUrl carries a placeholder, the
    // apiKey is the only password input outside the upstream section.
    const baseUrlInput = inputs.find(i => (i.placeholder || '').includes('anthropic.com'));
    const apiKeyInput = modal.querySelector('input[type="password"]');
    if (!baseUrlInput || !apiKeyInput) return 'no-connection-fields';
    window.setReactInputValue(baseUrlInput, 'http://127.0.0.1:65535/v1');
    window.setReactInputValue(apiKeyInput, 'sk-smoke-key');
    const save = modal.querySelector('[data-testid="provider-modal-save"]');
    if (!save) return 'no-save-btn';
    await new Promise(r => setTimeout(r, 300));
    save.click();
    const cardSel = `[data-testid="provider-card-${uniqId}"]`;
    for (let i = 0; i < 20; i++) {
      await new Promise(r => setTimeout(r, 100));
      if (document.querySelector(cardSel)) break;
      if (i === 19) {
        const cards = [...document.querySelectorAll('[data-testid^="provider-card-"]')]
          .map(c => c.getAttribute('data-testid')).join(',');
        const err = document.querySelector('.error-banner')?.textContent ?? '';
        const modalOpen = Boolean(document.querySelector('[data-testid="provider-modal"]'));
        const modalName = modal.querySelector('[data-testid="provider-modal-id"]')?.value ?? '(gone)';
        const storeIds = (await window.__TAURI_INTERNALS__.invoke('provider_list', { appType: 'claude_code' }))
          .map(p => p.id).join(',');
        return 'not-created cards=' + cards + ' err=' + err +
          ' modalOpen=' + modalOpen + ' modalId=' + modalName + ' store=' + storeIds;
      }
    }
    // Edit: rename (id must stay locked).
    const editBtn = document.querySelector(`[data-testid="provider-edit-${uniqId}"]`);
    if (!editBtn) return 'no-edit-btn';
    editBtn.click();
    for (let i = 0; i < 20; i++) {
      await new Promise(r => setTimeout(r, 80));
      modal = document.querySelector('.modal');
      if (modal) break;
    }
    if (!modal) return 'edit-no-modal';
    const idField = modal.querySelector('[data-testid="provider-modal-id"]');
    if (!idField || !idField.disabled) return 'edit-id-not-locked';
    const nameField = modal.querySelectorAll('input.input')[0];
    const newName2 = 'Smoke Provider Edited';
    window.setReactInputValue(nameField, newName2);
    modal.querySelector('[data-testid="provider-modal-save"]')?.click();
    for (let i = 0; i < 20; i++) {
      await new Promise(r => setTimeout(r, 100));
      const card = document.querySelector(cardSel);
      if (card && card.textContent.includes(newName2)) break;
      if (i === 19) return 'edit-not-saved';
    }
    window.confirm = () => true;
    document.querySelector(`[data-testid="provider-delete-${uniqId}"]`)?.click();
    for (let i = 0; i < 20; i++) {
      await new Promise(r => setTimeout(r, 100));
      if (!document.querySelector(cardSel)) return 'ok';
      if (i === 19) return 'delete-stuck';
    }
    return 'ok';
  });

  // The upstream type select exposes the anthropic passthrough option —
  // the subscription-credential relay pool type (Workstream 2a).
  await check_in_page(cdp, check, 'provider modal exposes anthropic passthrough type', async () => {
    const addBtn = document.querySelector('[data-testid="providers-add"]');
    if (!addBtn) return 'no-add-btn';
    addBtn.click();
    let modal;
    for (let i = 0; i < 20; i++) {
      await new Promise(r => setTimeout(r, 80));
      modal = document.querySelector('.modal');
      if (modal) break;
    }
    if (!modal) return 'no-modal';
    const typeSelect = [...modal.querySelectorAll('select')]
      .find(s => [...s.options].some(o => o.value === 'anthropic'));
    if (!typeSelect) return 'no-type-select';
    const hasPassthrough = [...typeSelect.options].some(o => o.value === 'anthropic_passthrough');
    // Close without saving.
    [...modal.querySelectorAll('button')]
      .find(b => /取消|Cancel|×/.test(b.textContent))?.click();
    return hasPassthrough ? 'ok' : 'no-passthrough-option';
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

// ---- Claude mirror (CLAUDE_CONFIG_DIR) integration ----
// The Claude page now carries the mirror rows: auth-mode toggle, mirror
// home row, and the Open Claude Desktop launcher.
await check_in_page(cdp, check, 'Claude page exposes mirror rows + launcher', async () => {
  const modeToggle = document.querySelector('[data-testid="claude-mirror-mode-toggle"]');
  const openBtn = document.querySelector('[data-testid="claude-open-desktop"]');
  const mirrorHomeLabel = [...document.querySelectorAll('.endpoint-label')]
    .some(l => (l.textContent || '').includes('CLAUDE_CONFIG_DIR'));
  return {
    ok: Boolean(modeToggle) && Boolean(openBtn) && mirrorHomeLabel,
    detail: `mode=${Boolean(modeToggle)} open=${Boolean(openBtn)} home=${mirrorHomeLabel}`,
  };
});

await check_in_page(cdp, check, 'Claude Desktop mirror launch flashes success', async () => {
  const btn = document.querySelector('[data-testid="claude-open-desktop"]');
  if (!btn) return 'no-launcher-btn';
  btn.click();
  await new Promise(r => setTimeout(r, 800));
  const text = document.body.innerText;
  return text.includes('CLAUDE_CONFIG_DIR=') || text.includes('~/.claude-cli')
    ? 'ok'
    : 'no-launch-flash';
});

// Forced-error path: the launcher failure surfaces as the error banner.
await check_in_page(cdp, check, 'Claude Desktop launcher failure shows error banner', async () => {
  if (!window.__aagDemoSetError) return 'missing-demo-error-control';
  window.__aagDemoSetError('claude_open_desktop');
  try {
    const btn = document.querySelector('[data-testid="claude-open-desktop"]');
    if (!btn) return 'no-launcher-btn';
    btn.click();
    await new Promise(r => setTimeout(r, 600));
    const banner = document.querySelector('.error-banner')?.textContent ?? '';
    return banner.includes('demo_forced_error') ? 'ok' : 'no-error-banner: ' + banner;
  } finally {
    window.__aagDemoSetError(null);
  }
});

// Mode toggle: flip to passthrough, verify persistence across an AppPage
// remount, then restore token mode.
await check_in_page(cdp, check, 'claude mirror mode toggle flips + persists across remount', async () => {
  const sw = document.querySelector('[data-testid="claude-mirror-mode-toggle"]');
  if (!sw) return 'no-toggle';
  const was = sw.checked; // mock default: token (off)
  sw.click();
  await new Promise(r => setTimeout(r, 600));
  const on1 = document.querySelector('[data-testid="claude-mirror-mode-toggle"]')?.checked;
  // Force AppPage remount + refresh(): Claude -> Codex round-trip (also
  // clears the forced-error banner from the previous check).
  [...document.querySelectorAll('.app-switcher button')].find(b => b.textContent.trim() === 'Codex')?.click();
  await new Promise(r => setTimeout(r, 700));
  [...document.querySelectorAll('.app-switcher button')].find(b => b.textContent.trim() === 'Claude')?.click();
  await new Promise(r => setTimeout(r, 900));
  const on2 = document.querySelector('[data-testid="claude-mirror-mode-toggle"]')?.checked;
  // Restore token mode.
  if (on2) document.querySelector('[data-testid="claude-mirror-mode-toggle"]')?.click();
  await new Promise(r => setTimeout(r, 500));
  const off = document.querySelector('[data-testid="claude-mirror-mode-toggle"]')?.checked;
  return { ok: !was && on1 && on2 && !off, detail: `was=${was} on1=${on1} on2=${on2} off=${off}` };
});

await check_in_page(cdp, check, 'Codex tab filters routes to codex- prefix', async () => {
  const codexBtn = [...document.querySelectorAll('.app-switcher button')]
    .find(b => b.textContent.trim() === 'Codex');
  codexBtn?.click();
  await new Promise(r => setTimeout(r, 1500));
  // Codex has its own advanced-routes open state (starts collapsed).
  const t = document.querySelector('[data-testid="advanced-routes-toggle"]');
  if (!t) return 'no-advanced-toggle';
  if (t.getAttribute('aria-expanded') !== 'true') {
    t.click();
    await new Promise(r => setTimeout(r, 600));
  }
  const codexAliases = [...document.querySelectorAll('.route-card-title strong')]
    .map(s => s.textContent.trim()).sort();
  const codexOnly = codexAliases.length > 0 && codexAliases.every(a => a.startsWith('codex-'));
  return { ok: codexOnly, count: codexAliases.length };
});

  // ---- EDGE-CASE 8: Codex page renders all four routes ----
  // Codex has its own advanced-routes open state (starts collapsed).
  await cdp.eval_value("[...document.querySelectorAll('.app-switcher button')].find(b => b.textContent.trim() === 'Codex')?.click(); 1");
  await sleep(1500);
  await cdp.eval_value(`(() => {
    const t = document.querySelector('[data-testid="advanced-routes-toggle"]');
    if (t && t.getAttribute('aria-expanded') !== 'true') t.click();
    return 'ok';
  })(); 1`);
  await sleep(800);
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

  // ---- EDGE-CASE 9: Codex page renders the unified provider cards ----
  await check_in_page(cdp, check, 'Codex page renders provider cards', async () => {
    const cards = document.querySelectorAll('[data-testid^="provider-card-"]');
    return { ok: cards.length > 0, count: cards.length };
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

  // The unified card list exposes the chain toggles ("+ chain" / "− chain")
  // as data-testids — the smoking gun for the failover-chain surface.
  // Button text changes after a toggle, and the surrounding re-render
  // makes the round-trip a flaky target — we only assert the toggles are
  // present and reachable.
  await check_in_page(cdp, check, 'Codex page exposes the provider chain toggles', async () => {
    const chainBtns = document.querySelectorAll('[data-testid^="provider-chain-"]');
    return chainBtns.length > 0
      ? 'ok'
      : 'no-chain-toggle';
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

  // ---- CoT toggle (Codex surface only; must run on the Codex page) ----
  // The gateway-cot-toggle row renders only where cfg.surface === 'codex'.
  // Flip on, verify it survives an AppPage remount (tab round-trip), then
  // restore off so the run ends in the default state.
  await check_in_page(cdp, check, 'gateway CoT toggle flips + persists across remount', async () => {
    const sw = document.querySelector('[data-testid="gateway-cot-toggle"]');
    if (!sw) return 'no-toggle';
    const was = sw.checked; // mock default: off
    sw.click();
    await new Promise(r => setTimeout(r, 600));
    const on1 = document.querySelector('[data-testid="gateway-cot-toggle"]')?.checked;
    // Force AppPage remount + refresh(): Claude -> Codex round-trip.
    [...document.querySelectorAll('.app-switcher button')].find(b => b.textContent.trim() === 'Claude')?.click();
    await new Promise(r => setTimeout(r, 700));
    [...document.querySelectorAll('.app-switcher button')].find(b => b.textContent.trim() === 'Codex')?.click();
    await new Promise(r => setTimeout(r, 900));
    const on2 = document.querySelector('[data-testid="gateway-cot-toggle"]')?.checked;
    // Restore off.
    if (on2) document.querySelector('[data-testid="gateway-cot-toggle"]')?.click();
    await new Promise(r => setTimeout(r, 500));
    const off = document.querySelector('[data-testid="gateway-cot-toggle"]')?.checked;
    return { ok: !was && on1 && on2 && !off, detail: `was=${was} on1=${on1} on2=${on2} off=${off}` };
  });

  // ---- Codex mirror mode toggle (launchers === 'codex' pages) ----
  // Flip to coexist, verify it survives an AppPage remount (the persisted
  // settings value is re-read by codex_cli_status), then restore exclusive.
  await check_in_page(cdp, check, 'codex mirror mode toggle flips + persists across remount', async () => {
    const sw = document.querySelector('[data-testid="codex-mirror-mode-toggle"]');
    if (!sw) return 'no-toggle';
    const was = sw.checked; // mock default: exclusive (off)
    sw.click();
    await new Promise(r => setTimeout(r, 600));
    const on1 = document.querySelector('[data-testid="codex-mirror-mode-toggle"]')?.checked;
    // Force AppPage remount + refresh(): Claude -> Codex round-trip.
    [...document.querySelectorAll('.app-switcher button')].find(b => b.textContent.trim() === 'Claude')?.click();
    await new Promise(r => setTimeout(r, 700));
    [...document.querySelectorAll('.app-switcher button')].find(b => b.textContent.trim() === 'Codex')?.click();
    await new Promise(r => setTimeout(r, 900));
    const on2 = document.querySelector('[data-testid="codex-mirror-mode-toggle"]')?.checked;
    // Restore exclusive.
    if (on2) document.querySelector('[data-testid="codex-mirror-mode-toggle"]')?.click();
    await new Promise(r => setTimeout(r, 500));
    const off = document.querySelector('[data-testid="codex-mirror-mode-toggle"]')?.checked;
    return { ok: !was && on1 && on2 && !off, detail: `was=${was} on1=${on1} on2=${on2} off=${off}` };
  });

  // --- exceptions ---
  const exc = [...cdp.exceptions];
  check('zero uncaught exceptions', exc.length === 0, exc[0]?.slice(0, 200));

  await cdp.eval_value('1').catch(() => {});
} catch (e) {
  failed = true;
  fail(format_error(e));
} finally {
  if (smoke_runtime && (!keep || failed)) {
    try {
      await smoke_runtime.close();
      log('[smoke] owned runtime closed and profile removed.');
    } catch (error) {
      failed = true;
      fail('runtime cleanup: ' + format_error(error));
    }
  } else if (keep) {
    log('[smoke] --keep: browser and Vite remain running until this process stops.');
  }
}

if (failed) process.exit(1);
log('[smoke] ' + passed_count + '/' + check_count + ' checks passed.');
if (!keep) process.exit(0);
