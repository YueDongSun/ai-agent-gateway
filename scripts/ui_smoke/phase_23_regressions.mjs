const settle_ms = 350;

export async function run_phase_23_regressions({ cdp, check_in_page, check, sleep }) {
  await check_in_page(cdp, check, 'route save preserves untouched backend routes', async () => {
    if (!window.__aagDemoIsDemo) return 'not-demo-page';
    const api = window.__aag_smoke_api;
    if ((await api.getProxyStatus()).running) return 'gateway-must-be-stopped-for-initial-save';
    const before = structuredClone(await api.listRoutes());
    const save_button = document.querySelector('[data-testid="route-save"]');
    if (!save_button) return 'no-save-button';
    save_button.click();
    await new Promise((resolve) => setTimeout(resolve, 500));
    const after = await api.listRoutes();
    return { ok: before.length > 0 && JSON.stringify(after) === JSON.stringify(before), detail: 'before=' + before.length + ' after=' + after.length };
  });

  await check_in_page(cdp, check, 'route hop save persists and UI restore returns backend baseline', async () => {
    const api = window.__aag_smoke_api;
    const baseline = structuredClone(await api.listRoutes());
    const route = baseline.find((candidate) => candidate.hops.length > 0);
    if (!route) return 'no-routable-route';
    let card = [...document.querySelectorAll('.route-card')].find((candidate) => candidate.querySelector('.route-card-title strong')?.textContent === route.alias);
    const add_button = [...(card?.querySelectorAll('.route-card-actions button') ?? [])].find((button) => /Add Hop|添加跳转/.test(button.textContent));
    if (!add_button) return 'no-add-hop-button';
    add_button.click();
    await new Promise((resolve) => setTimeout(resolve, 150));
    const save_button = document.querySelector('[data-testid="route-save"]');
    if (!save_button) return 'no-save-after-add';
    save_button.click();
    await new Promise((resolve) => setTimeout(resolve, 500));
    const saved = structuredClone(await api.listRoutes());
    const saved_route = saved.find((candidate) => candidate.alias === route.alias);
    card = [...document.querySelectorAll('.route-card')].find((candidate) => candidate.querySelector('.route-card-title strong')?.textContent === route.alias);
    const remove_button = [...(card?.querySelectorAll('.hop-remove-cell button') ?? [])].at(-1);
    if (!remove_button) return 'no-remove-added-hop';
    remove_button.click();
    await new Promise((resolve) => setTimeout(resolve, 150));
    const restore_button = document.querySelector('[data-testid="route-save"]');
    if (!restore_button) return 'no-save-after-remove';
    restore_button.click();
    await new Promise((resolve) => setTimeout(resolve, 500));
    const restored = await api.listRoutes();
    return {
      ok: saved_route?.hops.length === route.hops.length + 1 && JSON.stringify(restored) === JSON.stringify(baseline),
      detail: 'saved=' + (saved_route?.hops.length ?? 'missing') + ' baseline=' + route.hops.length + ' restored=' + restored.length,
    };
  });

  // Cross-app route cache regression: the per-app tab cache must carry each
  // tab's baseline together with its routes. Saving on one app tab, then the
  // other, then returning to the first must show a clean editor — and a
  // later Save there must not revert the hop saved on the sibling tab.
  await check_in_page(cdp, check, 'cross-app save leaves returning sibling tab clean', async () => {
    const api = window.__aag_smoke_api;
    if (!api) return 'no-smoke-api';
    const claude_button = [...document.querySelectorAll('.client_management_switcher button')].find((button) => button.textContent.trim() === 'Claude');
    const codex_button = [...document.querySelectorAll('.client_management_switcher button')].find((button) => button.textContent.trim() === 'Codex');
    if (!claude_button || !codex_button) return 'missing-app-switcher-buttons';
    const settle = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
    const wait_for_page = async (label) => {
      for (let attempt = 0; attempt < 20; attempt += 1) {
        await settle(100);
        if (document.querySelector('.page-header h2')?.textContent === label) return true;
      }
      return false;
    };
    const card_for = (alias) => [...document.querySelectorAll('.route-card')].find((candidate) => candidate.querySelector('.route-card-title strong')?.textContent === alias);
    const add_hop_in = (card) => [...(card?.querySelectorAll('.route-card-actions button') ?? [])].find((button) => /Add Hop|添加跳转/.test(button.textContent));
    claude_button.click();
    if (!(await wait_for_page('Claude'))) return 'claude-tab-did-not-open';
    const claude_route = (await api.listRoutes()).find((candidate) => !candidate.alias.startsWith('codex-') && candidate.hops.length > 0);
    if (!claude_route) return 'no-routable-claude-route';
    const claude_add = add_hop_in(card_for(claude_route.alias));
    if (!claude_add) return 'no-add-hop-button-claude';
    claude_add.click();
    await settle(150);
    const claude_save = document.querySelector('[data-testid="route-save"]');
    if (!claude_save) return 'no-save-after-claude-add';
    claude_save.click();
    await settle(500);
    codex_button.click();
    if (!(await wait_for_page('Codex'))) return 'codex-tab-did-not-open';
    const codex_toggle = document.querySelector('[data-testid="advanced-routes-toggle"]');
    if (!codex_toggle) return 'no-codex-advanced-toggle';
    if (codex_toggle.getAttribute('aria-expanded') !== 'true') {
      codex_toggle.click();
      await settle(400);
    }
    const codex_route = (await api.listRoutes()).find((candidate) => candidate.alias.startsWith('codex-') && candidate.hops.length > 0);
    if (!codex_route) return 'no-routable-codex-route';
    let codex_card = null;
    for (let attempt = 0; attempt < 20; attempt += 1) {
      codex_card = card_for(codex_route.alias);
      if (codex_card) break;
      await settle(100);
    }
    const codex_add = add_hop_in(codex_card);
    if (!codex_add) return 'no-add-hop-button-codex';
    codex_add.click();
    await settle(150);
    const codex_save = document.querySelector('[data-testid="route-save"]');
    if (!codex_save) return 'no-save-after-codex-add';
    codex_save.click();
    await settle(500);
    window.__aag_smoke_cross_app_routes = { claude: claude_route.alias, codex: codex_route.alias };
    claude_button.click();
    if (!(await wait_for_page('Claude'))) return 'claude-tab-did-not-reopen';
    let dirty_seen = false;
    for (let attempt = 0; attempt < 20; attempt += 1) {
      if (document.querySelector('[data-testid="advanced-routes-dirty"]')) {
        dirty_seen = true;
        break;
      }
      await settle(100);
    }
    return { ok: !dirty_seen, detail: 'dirty-dot=' + dirty_seen };
  });

  await check_in_page(cdp, check, 'save on returned tab preserves sibling backend routes', async () => {
    const api = window.__aag_smoke_api;
    if (!api) return 'no-smoke-api';
    const fixture = window.__aag_smoke_cross_app_routes;
    if (!fixture) return 'no-cross-app-fixture-from-previous-check';
    const snapshot_after_codex_save = JSON.stringify(await api.listRoutes());
    const save_button = document.querySelector('[data-testid="route-save"]');
    if (!save_button) return 'no-save-button';
    const settle = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
    const wait_for_page = async (label) => {
      for (let attempt = 0; attempt < 20; attempt += 1) {
        await settle(100);
        if (document.querySelector('.page-header h2')?.textContent === label) return true;
      }
      return false;
    };
    const card_for = (alias) => [...document.querySelectorAll('.route-card')].find((candidate) => candidate.querySelector('.route-card-title strong')?.textContent === alias);
    let preserved = false;
    let snapshot_lengths = '';
    let cleanup_error = null;
    try {
      save_button.click();
      await settle(500);
      const after_save = JSON.stringify(await api.listRoutes());
      preserved = after_save === snapshot_after_codex_save;
      snapshot_lengths = ' snapshot_len=' + snapshot_after_codex_save.length + ' after_len=' + after_save.length;
    } finally {
      // Restore the seeded routes and the standard Claude tab whether or not
      // the assertion passed, so later checks start untouched.
      const claude_button = [...document.querySelectorAll('.client_management_switcher button')].find((button) => button.textContent.trim() === 'Claude');
      const codex_button = [...document.querySelectorAll('.client_management_switcher button')].find((button) => button.textContent.trim() === 'Codex');
      const remove_last_hop_and_save = async (alias) => {
        const remove_button = [...(card_for(alias)?.querySelectorAll('.hop-remove-cell button') ?? [])].at(-1);
        if (!remove_button) return 'no-remove-hop-' + alias;
        remove_button.click();
        await settle(150);
        const save = document.querySelector('[data-testid="route-save"]');
        if (!save) return 'no-save-' + alias;
        save.click();
        await settle(500);
        return null;
      };
      if (!claude_button || !codex_button) {
        cleanup_error = 'missing-app-switcher-buttons';
      } else {
        cleanup_error = await remove_last_hop_and_save(fixture.claude);
        if (!cleanup_error) {
          codex_button.click();
          if (await wait_for_page('Codex')) {
            cleanup_error = await remove_last_hop_and_save(fixture.codex);
            if (!cleanup_error) {
              claude_button.click();
              if (!(await wait_for_page('Claude'))) cleanup_error = 'claude-tab-did-not-restore';
            }
          } else cleanup_error = 'codex-tab-did-not-reopen';
        }
      }
      delete window.__aag_smoke_cross_app_routes;
    }
    return { ok: preserved, detail: 'preserved=' + preserved + snapshot_lengths + (cleanup_error ? ' cleanup=' + cleanup_error : '') };
  });

  await check_in_page(cdp, check, 'batch delete stages and discard preserves full backend routes', async () => {
    const api = window.__aag_smoke_api;
    const baseline = structuredClone(await api.listRoutes());
    const selectors = [...document.querySelectorAll('.route-card .route-select input')];
    if (selectors.length < 2) return 'need-two-visible-routes:' + selectors.length;
    const visible_before = document.querySelectorAll('.route-card').length;
    window.confirm = () => true;
    selectors.slice(0, 2).forEach((input) => input.click());
    await new Promise((resolve) => setTimeout(resolve, 100));
    const delete_button = [...document.querySelectorAll('.route-toolbar-actions button')].find((button) => button.classList.contains('danger'));
    if (!delete_button) return 'no-batch-delete-button';
    delete_button.click();
    await new Promise((resolve) => setTimeout(resolve, 200));
    const staged_count = document.querySelectorAll('.route-card').length;
    const discard_button = document.querySelector('[data-testid="route-discard"]');
    if (!discard_button) return 'no-discard-button';
    discard_button.click();
    await new Promise((resolve) => setTimeout(resolve, 500));
    const restored = await api.listRoutes();
    return {
      ok: staged_count === visible_before - 2 && document.querySelectorAll('.route-card').length === visible_before && JSON.stringify(restored) === JSON.stringify(baseline),
      detail: 'visible=' + visible_before + ' staged=' + staged_count + ' backend=' + restored.length + ' baseline=' + baseline.length,
    };
  });

  await check_in_page(cdp, check, 'upstream enable disable enable persists backend state', async () => {
    const api = window.__aag_smoke_api;
    const baseline = structuredClone(await api.listProxyProviders());
    const provider = baseline[0];
    if (!provider) return 'no-provider';
    const toggle_selector = () => '[data-testid="provider-toggle-' + CSS.escape(provider.id) + '"]';
    const toggle = document.querySelector(toggle_selector());
    if (!toggle) return 'no-upstream-toggle';
    toggle.click();
    await new Promise((resolve) => setTimeout(resolve, 500));
    const disabled = (await api.listProxyProviders()).find((candidate) => candidate.id === provider.id);
    document.querySelector(toggle_selector())?.click();
    await new Promise((resolve) => setTimeout(resolve, 500));
    const enabled = (await api.listProxyProviders()).find((candidate) => candidate.id === provider.id);
    return { ok: disabled?.enabled === !provider.enabled && enabled?.enabled === provider.enabled, detail: 'baseline=' + provider.enabled + ' disabled=' + disabled?.enabled + ' enabled=' + enabled?.enabled };
  });

  await check_in_page(cdp, check, 'upstream pin then unpin persists backend state', async () => {
    const api = window.__aag_smoke_api;
    const provider = (await api.listProxyProviders())[0];
    if (!provider) return 'no-provider';
    const pin_selector = () => '[data-testid="provider-pin-' + CSS.escape(provider.id) + '"]';
    const pin_toggle = document.querySelector(pin_selector());
    if (!pin_toggle) return 'no-pin-toggle';
    pin_toggle.click();
    await new Promise((resolve) => setTimeout(resolve, 300));
    const pinned = await api.getPinnedProvider();
    document.querySelector(pin_selector())?.click();
    await new Promise((resolve) => setTimeout(resolve, 300));
    const unpinned = await api.getPinnedProvider();
    return { ok: pinned === provider.id && unpinned === null, detail: 'pin=' + pinned + ' unpin=' + unpinned };
  });

  await cdp.eval_value("(() => { const toggle = document.querySelector('[data-testid=\"advanced-routes-toggle\"]'); if (toggle && toggle.getAttribute('aria-expanded') !== 'true') toggle.click(); return true; })()");
  const handle_deadline = Date.now() + 4000;
  while (Date.now() < handle_deadline) {
    const handles_visible = await cdp.eval_value("[...document.querySelectorAll('.hop-drag-handle')].filter(handle => { const bounds = handle.getBoundingClientRect(); return bounds.width > 0 && bounds.height > 0; }).length >= 2");
    if (handles_visible) break;
    await sleep(40);
  }
  const geometry_script = [
    "(() => {",
    "const first = document.querySelector('[data-testid^=\"hop-handle-\"][data-testid$=\"-0\"]');",
    "const alias = first?.closest('.hop-row')?.getAttribute('data-route-alias');",
    "const second = alias && document.querySelector('[data-testid=\"hop-handle-' + alias + '-1\"]');",
    "if (!first || !second || !alias) return null;",
    "first.scrollIntoView({ block: 'center', behavior: 'instant' }); second.scrollIntoView({ block: 'center', behavior: 'instant' });",
    "const source = first.getBoundingClientRect(); const target = second.getBoundingClientRect();",
    "const source_hit = document.elementFromPoint(source.x + source.width / 2, source.y + source.height / 2)?.closest('.hop-drag-handle') === first;",
    "const target_hit = document.elementFromPoint(target.x + target.width / 2, target.y + target.height / 2)?.closest('.hop-drag-handle') === second;",
    "const value = document.querySelector('.hop-row[data-route-alias=\"' + CSS.escape(alias) + '\"][data-hop-idx=\"0\"] select')?.value;",
    "return { alias, value, source_hit, target_hit, source: { x: source.x + source.width / 2, y: source.y + source.height / 2 }, target: { x: target.x + target.width / 2, y: target.y + target.height / 2 } };",
    "})()",
  ].join('');  const drag_geometry = await cdp.eval_value(geometry_script);
  if (!drag_geometry || !drag_geometry.source_hit || !drag_geometry.target_hit) {
    check('pointer hop drag reorders route', false, 'handles-not-visible-at-input-coordinates');
  } else {
    await cdp.send('Input.dispatchMouseEvent', { type: 'mousePressed', x: drag_geometry.source.x, y: drag_geometry.source.y, button: 'left', buttons: 1, clickCount: 1, pointerType: 'mouse' });
    await sleep(settle_ms);
    await cdp.send('Input.dispatchMouseEvent', { type: 'mouseMoved', x: drag_geometry.target.x, y: drag_geometry.target.y, button: 'left', buttons: 1, pointerType: 'mouse' });
    await sleep(settle_ms);
    await cdp.send('Input.dispatchMouseEvent', { type: 'mouseReleased', x: drag_geometry.target.x, y: drag_geometry.target.y, button: 'left', clickCount: 1, pointerType: 'mouse' });
    await sleep(settle_ms);
    const moved_value = await cdp.eval_value("document.querySelector('.hop-row[data-route-alias=\"" + drag_geometry.alias + "\"][data-hop-idx=\"1\"] select')?.value ?? ''");
    check('pointer hop drag reorders route', moved_value === drag_geometry.value, 'moved=' + moved_value + ' expected=' + drag_geometry.value);
    await cdp.eval_value("document.querySelector('[data-testid=\"route-discard\"]')?.click(); 1");
    await sleep(settle_ms);
  }

  // Claude mirror (CLAUDE_CONFIG_DIR) surface: the auth-mode toggle and
  // the Open Claude Desktop launcher must render on the Claude page.
  await check_in_page(cdp, check, 'claude mirror rows render on the claude page', async () => {
    const mode_toggle = document.querySelector('[data-testid="claude-mirror-mode-toggle"]');
    const open_button = document.querySelector('[data-testid="claude-open-desktop"]');
    const home_row = [...document.querySelectorAll('.endpoint-label')].some((label) =>
      (label.textContent || '').includes('CLAUDE_CONFIG_DIR'),
    );
    return {
      ok: !mode_toggle && Boolean(open_button) && home_row,
      detail: 'mode=' + Boolean(mode_toggle) + ' open=' + Boolean(open_button) + ' home=' + home_row,
    };
  });

  await check_in_page(cdp, check, 'slow previous-tab refresh cannot overwrite the switched tab', async () => {
    const api = window.__aag_smoke_api;
    if (!api) return 'no-smoke-api';
    if (!window.__aagDemoIsDemo) return 'not-demo-page';
    const claude_button = [...document.querySelectorAll('.client_management_switcher button')].find((button) => button.textContent.trim() === 'Claude');
    const codex_button = [...document.querySelectorAll('.client_management_switcher button')].find((button) => button.textContent.trim() === 'Codex');
    if (!claude_button || !codex_button) return 'no-app-switcher-buttons';
    const settle = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
    const wait_for_page = async (label) => {
      for (let attempt = 0; attempt < 20; attempt += 1) {
        await settle(100);
        if (document.querySelector('.page-header h2')?.textContent === label) return true;
      }
      return false;
    };

    // If we are already on Claude, move to Codex first so that clicking
    // Claude below starts a genuine previous-tab refresh.
    if (document.querySelector('.page-header h2')?.textContent === 'Claude') {
      codex_button.click();
      if (!(await wait_for_page('Codex'))) return 'codex-tab-did-not-open';
    }

    // Override the IPC invoke path so provider_list for claude_code is slow
    // while codex calls stay fast.
    window.__TAURI_INTERNALS__ = window.__TAURI_INTERNALS__ || {};
    const real_invoke = window.__TAURI_INTERNALS__.invoke;
    window.__TAURI_INTERNALS__._race_real_invoke = real_invoke;
    window.__TAURI_INTERNALS__.invoke = (cmd, args) => {
      if (cmd === 'provider_list' && args?.appType === 'claude_code') {
        return new Promise((resolve, reject) => {
          setTimeout(() => {
            real_invoke(cmd, args).then(resolve, reject);
          }, 700);
        });
      }
      return real_invoke(cmd, args);
    };

    try {
      // Start a Claude refresh, then switch to Codex before it resolves.
      claude_button.click();
      await settle(50);
      codex_button.click();
      if (!(await wait_for_page('Codex'))) return 'codex-tab-did-not-open';

      // Expand advanced routes so the route cards are in the DOM.
      const codex_toggle = document.querySelector('[data-testid="advanced-routes-toggle"]');
      if (codex_toggle && codex_toggle.getAttribute('aria-expanded') !== 'true') {
        codex_toggle.click();
        await settle(400);
      }

      // Wait for the delayed Claude refresh to land if it can overwrite state.
      await settle(900);

      const route_titles = [...document.querySelectorAll('.route-card-title strong')].map((el) => el.textContent.trim());
      const all_codex_routes = route_titles.length > 0 && route_titles.every((title) => title.startsWith('codex-'));
      // The global pool always includes the ccs-claude-1 standalone upstream,
      // so look only for claude-prefixed *preset* cards (data-testid keyed by
      // preset id). A stale Claude refresh writes claude-1 / claude-2 presets
      // onto the Codex page.
      const has_claude_preset = document.querySelector('[data-testid^="provider-card-claude-"]') !== null;
      return {
        ok: all_codex_routes && !has_claude_preset,
        detail: 'routes=' + JSON.stringify(route_titles) + ' claude_preset=' + has_claude_preset,
      };
    } finally {
      const real = window.__TAURI_INTERNALS__._race_real_invoke;
      if (real) window.__TAURI_INTERNALS__.invoke = real;
      delete window.__TAURI_INTERNALS__._race_real_invoke;
      // Return to the standard Claude tab for later checks.
      claude_button.click();
      for (let attempt = 0; attempt < 20; attempt += 1) {
        await settle(100);
        if (document.querySelector('.page-header h2')?.textContent === 'Claude') break;
      }
    }
  });

  await check_in_page(cdp, check, 'mirror mode mutations ignore stale outcomes and preserve fresh status fields', async () => {
    if (!window.__aagDemoIsDemo) return 'not-demo-page';
    const internals = window.__TAURI_INTERNALS__;
    if (!internals || typeof internals.invoke !== 'function') return 'no-demo-invoke';
    const buttons = [...document.querySelectorAll('.client_management_switcher button')];
    const button_for = (label) => buttons.find((button) => button.textContent.trim() === label);
    const settle = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
    const wait_for = async (predicate, timeout_ms = 2500) => {
      const deadline = Date.now() + timeout_ms;
      while (Date.now() < deadline) {
        if (predicate()) return true;
        await settle(25);
      }
      return Boolean(predicate());
    };
    const current_page = () => document.querySelector('.page-header h2')?.textContent?.trim();
    const switch_page = async (label) => {
      const button = button_for(label);
      if (!button) return false;
      if (current_page() !== label) button.click();
      return wait_for(() => current_page() === label);
    };
    const results = [];

    const run_mirror = async (config) => {
      const { app_label, other_label, toggle_selector, status_command, mode_command, mode_field, fresh_field, enabled_mode, disabled_mode } = config;
      if (!(await switch_page(app_label))) {
        results.push({ app_label, ok: false, detail: 'page-did-not-open' });
        return;
      }
      const original_invoke = internals.invoke;
      const baseline_status = await original_invoke(status_command);
      const baseline_mode = baseline_status[mode_field];
      const baseline_checked = baseline_mode === enabled_mode;
      const mode_for_checked = (checked) => checked ? enabled_mode : disabled_mode;
      if (!(await wait_for(() => document.querySelector(toggle_selector)?.checked === baseline_checked))) {
        results.push({ app_label, ok: false, detail: 'initial-mode-did-not-load' });
        return;
      }

      const requests = [];
      let fresh_snapshot_pending = false;
      let fresh_snapshot_calls = 0;
      let recovery_read_failure_pending = false;
      const fresh_marker = 'smoke-fresh-' + app_label.toLowerCase() + '-status';
      const endpoint_has_fresh_marker = () => [...document.querySelectorAll('.endpoint-value')]
        .some((element) => element.textContent?.trim() === fresh_marker);
      const invoke_with_controlled_modes = (command, args) => {
        if (command === mode_command) {
          return new Promise((resolve, reject) => requests.push({ mode: args?.mode, resolve, reject, settled: false }));
        }
        if (command === status_command && recovery_read_failure_pending) {
          recovery_read_failure_pending = false;
          return Promise.reject(new Error('smoke-recovery-read-' + app_label.toLowerCase()));
        }
        if (command === status_command && fresh_snapshot_pending && requests.some((request) => !request.settled)) {
          fresh_snapshot_pending = false;
          fresh_snapshot_calls += 1;
          return Promise.resolve({ ...baseline_status, [mode_field]: mode_for_checked(!baseline_checked), [fresh_field]: fresh_marker });
        }
        return original_invoke(command, args);
      };
      internals.invoke = invoke_with_controlled_modes;
      const click_mode = async (checked, request_count) => {
        const toggle = document.querySelector(toggle_selector);
        if (!toggle) return false;
        if (toggle.checked !== checked) toggle.click();
        return wait_for(() => document.querySelector(toggle_selector)?.checked === checked && requests.length === request_count);
      };
      const finish_request = (index, error = null) => {
        const request = requests[index];
        if (!request || request.settled) return;
        request.settled = true;
        if (error) request.reject(error);
        else request.resolve({ mode: request.mode, written: [] });
      };
      let test_ok = true;
      const details = [];
      const notice_matches_mode = (checked) => {
        const prefixes = config.success_notice_prefixes[checked ? 'enabled' : 'disabled'];
        return [...document.querySelectorAll('.notice-banner')].some((notice) => {
          const text = notice.textContent?.trim() ?? '';
          return prefixes.some((prefix) => text.startsWith(prefix));
        });
      };
      const stale_notice_observation_ms = 150;

      try {
        // An old failure must not roll back a mode selected by later successes.
        const first_checked = !baseline_checked;
        const first_ready = await click_mode(first_checked, 1);
        const second_ready = first_ready && await click_mode(baseline_checked, 2);
        if (second_ready) finish_request(1);
        const third_ready = second_ready && await click_mode(first_checked, 3);
        if (third_ready) finish_request(2);
        if (third_ready) {
          await settle(50);
          finish_request(0, new Error('smoke-obsolete-' + app_label.toLowerCase()));
          await settle(100);
          const stale_failure_ignored = document.querySelector(toggle_selector)?.checked === first_checked
            && !document.querySelector('.error-banner')?.textContent?.includes('smoke-obsolete-' + app_label.toLowerCase());
          test_ok = test_ok && stale_failure_ignored;
          details.push('obsolete_failure=' + stale_failure_ignored);
        } else {
          test_ok = false;
          details.push('obsolete_sequence=not-ready');
        }

        if (!(await switch_page(other_label)) || !(await switch_page(app_label))
          || !(await wait_for(() => document.querySelector(toggle_selector)?.checked === baseline_checked))) {
          test_ok = false;
          details.push('failed_pair_reset=failed');
        } else {
          const failed_pair_index = requests.length;
          const first_failure_ready = await click_mode(!baseline_checked, failed_pair_index + 1);
          const second_failure_ready = first_failure_ready && await click_mode(baseline_checked, failed_pair_index + 2);
          if (second_failure_ready) {
            finish_request(failed_pair_index + 1, new Error('smoke-newer-failed-' + app_label.toLowerCase()));
            await settle(100);
            finish_request(failed_pair_index, new Error('smoke-older-failed-' + app_label.toLowerCase()));
            await settle(100);
            const failed_pair_recovered = document.querySelector(toggle_selector)?.checked === baseline_checked;
            test_ok = test_ok && failed_pair_recovered;
            details.push('failed_pair_recovered=' + failed_pair_recovered);
          } else {
            test_ok = false;
            details.push('failed_pair_sequence=not-ready');
          }
        }

        // The latest failed request still rolls its mode back, but must retain
        // unrelated fields delivered by a real status poll while it was pending.
        if (!(await switch_page(other_label)) || !(await switch_page(app_label))
          || !(await wait_for(() => document.querySelector(toggle_selector)?.checked === baseline_checked))) {
          test_ok = false;
          details.push('status_reset=failed');
        } else {
          const latest_index = requests.length;
          fresh_snapshot_pending = true;
          const latest_ready = await click_mode(!baseline_checked, latest_index + 1);
          if (latest_ready) {
            const native_set_interval = window.setInterval;
            const visibility_descriptor = Object.getOwnPropertyDescriptor(document, 'visibilityState');
            let forced_visibility = 'hidden';
            try {
              Object.defineProperty(document, 'visibilityState', { configurable: true, get: () => forced_visibility });
              document.dispatchEvent(new Event('visibilitychange'));
              window.setInterval = function (callback, delay, ...args) {
                if (delay === 10_000) queueMicrotask(callback);
                return native_set_interval.call(window, callback, delay, ...args);
              };
              forced_visibility = 'visible';
              document.dispatchEvent(new Event('visibilitychange'));
            } finally {
              window.setInterval = native_set_interval;
              if (visibility_descriptor) Object.defineProperty(document, 'visibilityState', visibility_descriptor);
              else delete document.visibilityState;
            }
          }
          const fresh_read = latest_ready && await wait_for(() => fresh_snapshot_calls === 1, 2500);
          const fresh_visible = fresh_read && await wait_for(endpoint_has_fresh_marker);
          if (fresh_visible) {
            finish_request(latest_index, new Error('smoke-latest-' + app_label.toLowerCase()));
            await settle(100);
            const latest_failure_preserved_fields = document.querySelector(toggle_selector)?.checked === baseline_checked
              && endpoint_has_fresh_marker()
              && document.querySelector('.error-banner')?.textContent?.includes('smoke-latest-' + app_label.toLowerCase());
            test_ok = test_ok && latest_failure_preserved_fields;
            details.push('latest_failure_preserved_fields=' + latest_failure_preserved_fields);
          } else {
            test_ok = false;
            details.push('fresh_poll=' + (fresh_read ? 'field-not-visible' : 'not-observed'));
          }
        }

        if (!(await switch_page(other_label)) || !(await switch_page(app_label))
          || !(await wait_for(() => document.querySelector(toggle_selector)?.checked === baseline_checked))) {
          test_ok = false;
          details.push('recovery_read_reset=failed');
        } else {
          const recovery_index = requests.length;
          const recovery_ready = await click_mode(!baseline_checked, recovery_index + 1);
          if (recovery_ready) {
            recovery_read_failure_pending = true;
            finish_request(recovery_index, new Error('smoke-recovery-mutation-' + app_label.toLowerCase()));
            const recovery_read_reported = await wait_for(() => {
              const row_error = document.querySelector('[data-testid="' + app_label.toLowerCase() + '-mirror-error"]');
              return row_error?.getAttribute('title')?.includes('smoke-recovery-read-' + app_label.toLowerCase())
                && document.querySelector('.error-banner')?.textContent?.includes('smoke-recovery-mutation-' + app_label.toLowerCase());
            });
            test_ok = test_ok && recovery_read_reported;
            details.push('recovery_read_reported=' + recovery_read_reported);
          } else {
            test_ok = false;
            details.push('recovery_read_sequence=not-ready');
          }
        }

        let obsolete_success_ignored = false;
        if (!(await switch_page(other_label)) || !(await switch_page(app_label))
          || !(await wait_for(() => document.querySelector(toggle_selector)?.checked === baseline_checked))) {
          test_ok = false;
          details.push('obsolete_success_reset=failed');
        } else {
          const obsolete_success_index = requests.length;
          const obsolete_success_ready = await click_mode(!baseline_checked, obsolete_success_index + 1);
          const latest_success_ready = obsolete_success_ready && await click_mode(baseline_checked, obsolete_success_index + 2);
          if (latest_success_ready) {
            finish_request(obsolete_success_index + 1);
            const latest_success_notice_seen = await wait_for(() =>
              document.querySelector(toggle_selector)?.checked === baseline_checked
                && notice_matches_mode(baseline_checked),
            );
            finish_request(obsolete_success_index);
            const obsolete_notice_published = await wait_for(
              () => notice_matches_mode(!baseline_checked),
              stale_notice_observation_ms,
            );
            obsolete_success_ignored = latest_success_notice_seen
              && !obsolete_notice_published
              && notice_matches_mode(baseline_checked)
              && document.querySelector(toggle_selector)?.checked === baseline_checked;
            test_ok = test_ok && obsolete_success_ignored;
          } else {
            test_ok = false;
            details.push('obsolete_success_sequence=not-ready');
          }
        }
        details.push('obsolete_success_ignored=' + obsolete_success_ignored);

        // A pending mutation from this page must not publish an error after the
        // app switch has advanced the owning operation id.
        if (await switch_page(other_label)) {
          const switch_failure = 'smoke-app-switch-' + app_label.toLowerCase();
          const switch_ready = await switch_page(app_label)
            && await wait_for(() => document.querySelector(toggle_selector)?.checked === baseline_checked)
            && await click_mode(!baseline_checked, requests.length + 1);
          if (switch_ready && await switch_page(other_label)) {
            await settle(100);
            finish_request(requests.length - 1, new Error(switch_failure));
            await settle(100);
            const switch_ignored = !document.querySelector('.error-banner')?.textContent?.includes(switch_failure);
            test_ok = test_ok && switch_ignored;
            details.push('app_switch_ignored=' + switch_ignored);
          } else {
            test_ok = false;
            details.push('app_switch_sequence=not-ready');
          }
        } else {
          test_ok = false;
          details.push('app_switch_reset=failed');
        }

        let app_switch_success_ignored = false;
        let app_switch_success_pre_release_clean = false;
        if (!(await switch_page(app_label))
          || !(await wait_for(() => document.querySelector(toggle_selector)?.checked === baseline_checked))) {
          test_ok = false;
          details.push('app_switch_success_reset=failed');
        } else {
          const switch_success_index = requests.length;
          const switch_success_ready = await click_mode(!baseline_checked, switch_success_index + 1);
          if (switch_success_ready && await switch_page(other_label)) {
            app_switch_success_pre_release_clean = !notice_matches_mode(!baseline_checked);
            finish_request(switch_success_index);
            const obsolete_switch_notice_published = await wait_for(
              () => notice_matches_mode(!baseline_checked),
              stale_notice_observation_ms,
            );
            app_switch_success_ignored = app_switch_success_pre_release_clean && !obsolete_switch_notice_published
              && !notice_matches_mode(!baseline_checked);
            test_ok = test_ok && app_switch_success_ignored;
          } else {
            test_ok = false;
            details.push('app_switch_success_sequence=not-ready');
          }
        }
        details.push('app_switch_success_pre_release_clean=' + app_switch_success_pre_release_clean);
        details.push('app_switch_success_ignored=' + app_switch_success_ignored);
      } finally {
        for (let index = 0; index < requests.length; index += 1) finish_request(index);
        internals.invoke = original_invoke;
        if (current_page() === app_label) await switch_page(other_label);
        await switch_page('Claude');
      }
      results.push({ app_label, ok: test_ok, detail: details.join(',') });
    };

    await run_mirror({
      app_label: 'Codex', other_label: 'Claude', toggle_selector: '[data-testid="codex-mirror-mode-toggle"]',
      status_command: 'codex_cli_status', mode_command: 'codex_mirror_set_mode', mode_field: 'mirrorMode',
      fresh_field: 'configDir', enabled_mode: 'coexist', disabled_mode: 'exclusive',
      success_notice_prefixes: {
        enabled: ['Coexist mode on', '已开启共存模式'],
        disabled: ['Exclusive mode on', '已开启独占模式'],
      },
    });
    results.push({ app_label: 'Claude', ok: !document.querySelector('[data-testid="claude-mirror-mode-toggle"]'), detail: 'Login-forwarding UI removed' });
    await switch_page('Claude');
    return { ok: results.length === 2 && results.every((result) => result.ok), detail: JSON.stringify(results) };
  });
}
