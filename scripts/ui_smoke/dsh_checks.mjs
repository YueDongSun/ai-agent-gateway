/** Browser-only DSH acceptance. No result here proves native IPC or upstream access. */
const responsive_viewports = [{ width: 1200, height: 900 }, { width: 900, height: 900 }, { width: 420, height: 900 }];

export async function run_dsh_checks({ cdp, check_in_page, check, sleep }) {
  const installed = await cdp.eval_value(`(async () => {
    if (window.__aagDemoIsDemo !== true) throw new Error('refusing DSH smoke mutations outside demo mode');
    const internals = window.__TAURI_INTERNALS__;
    if (typeof internals?.invoke !== 'function') throw new Error('missing synthetic IPC');
    const original_invoke = internals.invoke;
    const commands = [];
    internals.invoke = (command, args) => {
      if (command.startsWith('dsh_')) commands.push(command);
      return original_invoke(command, args);
    };
    const invoke = (command, args) => {
      if (window.__aagDemoIsDemo !== true) throw new Error('demo guard changed');
      return internals.invoke(command, args);
    };
    const element = (test_id) => document.querySelector('[data-testid="' + test_id + '"]');
    const wait_for = async (predicate, timeout_ms = 3500) => {
      const deadline = Date.now() + timeout_ms;
      while (Date.now() < deadline) {
        const result = await predicate();
        if (result) return result;
        await new Promise((resolve) => setTimeout(resolve, 60));
      }
      throw new Error('DSH browser condition timed out');
    };
    const click = async (test_id) => {
      const target = await wait_for(() => { const found = element(test_id); return found && !found.disabled ? found : null; });
      target.click();
      await new Promise((resolve) => setTimeout(resolve, 80));
    };
    const input = async (test_id, value) => {
      const target = element(test_id);
      if (!target || target.disabled) throw new Error('input unavailable: ' + test_id);
      const prototype = target instanceof HTMLSelectElement ? HTMLSelectElement.prototype : HTMLInputElement.prototype;
      Object.getOwnPropertyDescriptor(prototype, 'value').set.call(target, value);
      target.dispatchEvent(new Event(target instanceof HTMLSelectElement ? 'change' : 'input', { bubbles: true }));
      await new Promise((resolve) => setTimeout(resolve, 80));
    };
    const confirm = async () => {
      await wait_for(() => element('dsh_preview_dialog'));
      if (!element('dsh_acknowledge').checked) await click('dsh_acknowledge');
      await click('dsh_confirm');
      await wait_for(() => !element('dsh_preview_dialog'));
    };
    const snapshot = () => invoke('dsh_get_snapshot');
    const settled = async () => wait_for(async () => {
      const state = await snapshot();
      return !state.job && !element('dsh_pending') ? state : null;
    }, 5000);
    const operation = async (button_id) => {
      const before = (await snapshot()).receipts.length;
      await click(button_id);
      await confirm();
      await wait_for(async () => (await snapshot()).receipts.length > before, 5000);
      await settled();
      await wait_for(() => document.querySelectorAll('[data-testid="dsh_receipt"]').length > before);
      return snapshot();
    };
    const fixture = async () => JSON.stringify(await Promise.all([
      invoke('proxy_config_get'), invoke('proxy_status'), invoke('proxy_provider_list'), invoke('route_list'),
      invoke('provider_list', { appType: 'claude_code' }),
    ]));
    const initial_snapshot = await snapshot();
    const initial_fixture = await fixture();
    commands.length = 0;
    window.__aag_dsh_smoke = { invoke, element, wait_for, click, input, confirm, snapshot, settled, operation, fixture,
      initial_snapshot, initial_fixture, commands, restore: () => { internals.invoke = original_invoke; } };
    return true;
  })()`);
  if (!installed) throw new Error('could not install guarded DSH browser helpers');

  try {
    await check_in_page(cdp, check, 'DSH synthetic page mount is read-only and clearly labelled', async () => {
      const api = window.__aag_dsh_smoke;
      await api.click('dsh_nav');
      await api.wait_for(() => api.element('dsh_initialized'));
      await new Promise((resolve) => setTimeout(resolve, 250));
      const state = await api.snapshot();
      return {
        ok: Boolean(api.element('dsh_demo_notice')) && !state.initialized && !state.job && !state.receipts.length &&
          state.runtime.phase === 'stopped' && api.commands.every((command) => ['dsh_get_snapshot', 'dsh_list_upstreams', 'dsh_discover_runtime'].includes(command)),
        detail: 'initialized=' + state.initialized + ' receipts=' + state.receipts.length + ' commands=' + api.commands.join(','),
      };
    });

    await check_in_page(cdp, check, 'DSH workspace exposes reviewed provider switching without fake chat requests', async () => {
      const api = window.__aag_dsh_smoke;
      await api.click('dsh_open_webui_frontend');
      await api.wait_for(() => api.element('dsh_webui'));
      const shell = Boolean(api.element('dsh_webui')) && Boolean(api.element('dsh_webui_current_provider'));
      const native_button = Boolean(api.element('dsh_webui_open_native'));
      const provider_card = Boolean(api.element('dsh_webui_provider_proxy_demo_pool'));
      await api.click('dsh_webui_switch_proxy_demo_pool');
      await api.wait_for(() => api.element('dsh_preview_dialog'));
      const preview_text = api.element('dsh_preview_dialog')?.textContent ?? '';
      const locked_during_preview = api.element('dsh_webui_switch_proxy_demo_pool')?.disabled === true;
      await api.confirm();
      const state = await api.wait_for(async () => {
        const next = await api.snapshot();
        return next.settings.upstream_id === 'proxy:demo_pool' ? next : null;
      });
      await api.click('dsh_webui_back');
      return {
        ok: shell && native_button && provider_card && locked_during_preview &&
          preview_text.includes('Review provider switch') && preview_text.includes('AAG provider') &&
          state.settings.model_id === 'deepseek-chat' && !api.element('dsh_webui'),
        detail: 'shell=' + shell + ' provider-card=' + provider_card + ' preview-locked=' + locked_during_preview + ' saved=' + state.settings.upstream_id,
      };
    });

    await check_in_page(cdp, check, 'DSH preview requires acknowledgement and cancel leaves state untouched', async () => {
      const api = window.__aag_dsh_smoke;
      const before = JSON.stringify(await api.snapshot());
      await api.click('dsh_initialize');
      await api.wait_for(() => api.element('dsh_preview_dialog'));
      const blocked = api.element('dsh_confirm')?.disabled === true;
      const focused = api.element('dsh_preview_dialog').contains(document.activeElement);
      await api.click('dsh_cancel_preview');
      return { ok: blocked && focused && before === JSON.stringify(await api.snapshot()) && !api.element('dsh_preview_dialog'), detail: 'confirmation-blocked=' + blocked + ' dialog-focused=' + focused };
    });

    await check_in_page(cdp, check, 'DSH UI initializes managed state and renders a matching receipt', async () => {
      const api = window.__aag_dsh_smoke;
      const state = await api.operation('dsh_initialize');
      const receipt = state.receipts[0];
      return { ok: state.initialized && receipt.action === 'initialize' && receipt.status === 'succeeded' && Boolean(api.element('dsh_receipt')) && !api.element('dsh_initialize'), detail: 'status=' + receipt.status + ' initialized=' + state.initialized };
    });

    await check_in_page(cdp, check, 'DSH native Claude source exposes protocol and saves the chosen model', async () => {
      const api = window.__aag_dsh_smoke;
      await api.input('dsh_upstream', 'claude:demo_native');
      await api.input('dsh_model', 'claude-opus-4-6');
      const protocol = api.element('dsh_upstream_protocol')?.textContent ?? '';
      await api.click('dsh_preview_settings');
      const preview_text = api.element('dsh_preview_dialog')?.textContent ?? '';
      await api.confirm();
      await api.wait_for(() => !api.element('dsh_settings_dirty'));
      const state = await api.snapshot();
      const correct_source = state.settings.upstream_id === 'claude:demo_native';
      const correct_model = state.settings.model_id === 'claude-opus-4-6';
      const correct_protocol = protocol.includes('anthropic-messages');
      const named_source = preview_text.includes('Native Claude');
      const redacted_preview = !/sk-ant-|sk-demo/.test(preview_text);
      return { ok: correct_source && correct_model && correct_protocol && named_source && redacted_preview, detail: JSON.stringify({ source: state.settings.upstream_id, model: state.settings.model_id, protocol, correct_protocol, named_source, redacted_preview }) };
    });

    await check_in_page(cdp, check, 'DSH personalization and practical filters retain separate catalog groups', async () => {
      const api = window.__aag_dsh_smoke;
      await api.click('dsh_plugin_filter_personalization');
      const personalized = Boolean(api.element('dsh_plugin_skins')) && !api.element('dsh_plugin_cost_meter');
      await api.click('dsh_plugin_filter_practical');
      const practical = Boolean(api.element('dsh_plugin_cost_meter')) && Boolean(api.element('dsh_plugin_mcp_client')) && !api.element('dsh_plugin_skins');
      return { ok: personalized && practical, detail: 'personalization=' + personalized + ' practical=' + practical };
    });

    await check_in_page(cdp, check, 'DSH rejects a floating plugin version before requesting a preview', async () => {
      const api = window.__aag_dsh_smoke;
      const count = api.commands.filter((command) => command === 'dsh_preview_operation').length;
      await api.input('dsh_version_cost_meter', 'latest');
      const disabled = api.element('dsh_install_cost_meter')?.disabled;
      api.element('dsh_install_cost_meter').click();
      await new Promise((resolve) => setTimeout(resolve, 100));
      return { ok: disabled && count === api.commands.filter((command) => command === 'dsh_preview_operation').length, detail: 'floating-version-blocked=' + disabled };
    });

    await check_in_page(cdp, check, 'DSH exact install preserves independent configured and runtime states', async () => {
      const api = window.__aag_dsh_smoke;
      await api.input('dsh_version_cost_meter', '0.1.0-rc.3');
      const state = await api.operation('dsh_install_cost_meter');
      const plugin = state.plugins.find((entry) => entry.plugin_id === 'cost_meter');
      const card = api.element('dsh_plugin_cost_meter');
      return { ok: plugin.installed_version === '0.1.0-rc.3' && !plugin.enabled && plugin.runtime_phase === 'unknown' && card.textContent.includes('0.1.0-rc.3') && Boolean(api.element('dsh_enable_cost_meter')) && /尚未验证|Not verified/.test(card.textContent), detail: 'version=' + plugin.installed_version + ' enabled=' + plugin.enabled + ' runtime=' + plugin.runtime_phase };
    });

    await check_in_page(cdp, check, 'DSH enable persists without claiming runtime loading', async () => {
      const api = window.__aag_dsh_smoke;
      const plugin = (await api.operation('dsh_enable_cost_meter')).plugins.find((entry) => entry.plugin_id === 'cost_meter');
      return { ok: plugin.enabled && plugin.runtime_phase === 'unknown', detail: 'enabled=' + plugin.enabled + ' runtime=' + plugin.runtime_phase };
    });

    await check_in_page(cdp, check, 'DSH disable preserves the installed package', async () => {
      const api = window.__aag_dsh_smoke;
      const plugin = (await api.operation('dsh_disable_cost_meter')).plugins.find((entry) => entry.plugin_id === 'cost_meter');
      return { ok: !plugin.enabled && plugin.installed_version === '0.1.0-rc.3', detail: 'enabled=' + plugin.enabled + ' installed=' + plugin.installed_version };
    });

    await check_in_page(cdp, check, 'DSH re-enable restores configured state only', async () => {
      const api = window.__aag_dsh_smoke;
      const plugin = (await api.operation('dsh_enable_cost_meter')).plugins.find((entry) => entry.plugin_id === 'cost_meter');
      return { ok: plugin.enabled && plugin.runtime_phase === 'unknown', detail: 'enabled=' + plugin.enabled + ' runtime=' + plugin.runtime_phase };
    });

    await check_in_page(cdp, check, 'DSH remove clears the package and enabled configuration together', async () => {
      const api = window.__aag_dsh_smoke;
      const state = await api.operation('dsh_remove_cost_meter');
      const plugin = state.plugins.find((entry) => entry.plugin_id === 'cost_meter');
      return { ok: !plugin.installed_version && !plugin.enabled && Boolean(api.element('dsh_install_cost_meter')) && state.receipts[0].action === 'remove', detail: 'removed=' + !plugin.installed_version + ' enabled=' + plugin.enabled };
    });

    await check_in_page(cdp, check, 'DSH cancelled install preserves plugin state and records cancellation', async () => {
      const api = window.__aag_dsh_smoke;
      const before = await api.snapshot();
      const plugin_before = before.plugins.find((entry) => entry.plugin_id === 'cost_meter');
      await api.input('dsh_version_cost_meter', '0.1.0-rc.3');
      await api.click('dsh_install_cost_meter');
      await api.confirm();
      await api.wait_for(async () => (await api.snapshot()).job !== null);
      await api.click('dsh_cancel_operation');
      await api.wait_for(async () => (await api.snapshot()).receipts.length > before.receipts.length, 5000);
      const after = await api.settled();
      const receipt = after.receipts[0];
      return { ok: JSON.stringify(plugin_before) === JSON.stringify(after.plugins.find((entry) => entry.plugin_id === 'cost_meter')) && receipt.status === 'cancelled' && receipt.rollback_status === 'restored' && /已取消|Cancelled/.test(api.element('dsh_receipts')?.textContent ?? ''), detail: 'status=' + receipt.status + ' rollback=' + receipt.rollback_status };
    });

    await check_in_page(cdp, check, 'DSH package-operation failure stays visible without a success receipt', async () => {
      const api = window.__aag_dsh_smoke;
      const before = await api.snapshot();
      try {
        await api.click('dsh_install_cost_meter');
        await api.wait_for(() => api.element('dsh_preview_dialog'));
        window.__aagDemoSetError('dsh_execute_operation');
        await api.confirm();
        await api.wait_for(() => api.element('dsh_error'));
        const after = await api.snapshot();
        return { ok: after.receipts.length === before.receipts.length && !after.plugins.find((entry) => entry.plugin_id === 'cost_meter').installed_version && api.element('dsh_error').textContent.includes('dsh_execute_operation'), detail: 'failure-visible=' + Boolean(api.element('dsh_error')) + ' unchanged-receipts=' + (after.receipts.length === before.receipts.length) };
      } finally { window.__aagDemoSetError(null); }
    });

    await check_in_page(cdp, check, 'DSH synthetic start opens only the demo path and stops cleanly', async () => {
      const api = window.__aag_dsh_smoke;
      await api.click('dsh_start');
      await api.wait_for(() => api.element('dsh_open_web_ui')?.disabled === false);
      const running = await api.snapshot();
      const locked = api.element('dsh_upstream').matches(':disabled');
      await api.click('dsh_open_web_ui');
      const requested = api.commands.includes('dsh_open_web_ui') && Boolean(api.element('dsh_message'));
      await api.click('dsh_stop');
      await api.wait_for(() => api.element('dsh_start')?.disabled === false);
      const stopped = await api.snapshot();
      return { ok: running.runtime.phase === 'web_ready' && locked && requested && stopped.runtime.phase === 'stopped' && stopped.runtime.pid === null && api.element('dsh_open_web_ui').disabled, detail: 'start=' + running.runtime.phase + ' locked=' + locked + ' open-requested=' + requested + ' stop=' + stopped.runtime.phase };
    });

    await check_in_page(cdp, check, 'DSH stale settings confirmation is rejected without overwriting newer values', async () => {
      const api = window.__aag_dsh_smoke;
      await api.input('dsh_theme', 'dark');
      await api.click('dsh_preview_settings');
      const before = await api.snapshot();
      await api.invoke('dsh_save_settings', { settings: { ...before.settings, theme: 'light' }, expected_digest: before.settings_digest });
      await api.confirm();
      await api.wait_for(() => api.element('dsh_error')?.textContent.includes('DSH_SETTINGS_CHANGED'));
      const after = await api.snapshot();
      await api.click('dsh_refresh');
      await api.wait_for(() => api.element('dsh_preview_settings')?.disabled === true);
      const edits_preserved = api.element('dsh_theme').value === 'dark';
      await api.click('dsh_discard_settings');
      return { ok: after.settings.theme === 'light' && edits_preserved && api.element('dsh_theme').value === 'light' && !api.element('dsh_settings_dirty'), detail: 'saved-theme=' + after.settings.theme + ' edits-preserved=' + edits_preserved };
    });

    await check_in_page(cdp, check, 'DSH inventory error is visible and refresh recovers', async () => {
      const api = window.__aag_dsh_smoke;
      try {
        window.__aagDemoSetError('dsh_get_snapshot');
        await api.click('dsh_refresh');
        await api.wait_for(() => api.element('dsh_error')?.textContent.includes('dsh_get_snapshot'));
      } finally { window.__aagDemoSetError(null); }
      await api.click('dsh_refresh');
      await api.wait_for(() => !api.element('dsh_error') && !api.element('dsh_refresh').disabled);
      return { ok: Boolean(api.element('dsh_initialized')) && !api.element('dsh_error'), detail: 'recovered-with-existing-inventory=true' };
    });

    for (const viewport of responsive_viewports) {
      await cdp.send('Emulation.setDeviceMetricsOverride', { ...viewport, deviceScaleFactor: 1, mobile: false });
      await sleep(150);
      await check_in_page(cdp, check, 'DSH synthetic layout contains controls at viewport ' + viewport.width, async () => {
        const width = document.documentElement.clientWidth;
        const navigation = document.querySelector('.app-switcher')?.getBoundingClientRect();
        const fields = [...document.querySelectorAll('.dsh_page input, .dsh_page select, .dsh_page button')].filter((element) => element.getClientRects().length);
        const controls_fit = fields.every((element) => { const bounds = element.getBoundingClientRect(); return bounds.left >= 0 && bounds.right <= width + 1; });
        return { ok: document.documentElement.scrollWidth <= width + 1 && navigation?.right <= width + 1 && controls_fit, detail: 'width=' + width + ' document=' + document.documentElement.scrollWidth + ' controls-fit=' + controls_fit };
      });
    }
  } finally {
    await cdp.send('Emulation.clearDeviceMetricsOverride');
    await cdp.eval_value(`(async () => {
      const api = window.__aag_dsh_smoke;
      if (!api || window.__aagDemoIsDemo !== true) return false;
      window.__aagDemoSetError(null);
      try {
        const state = await api.snapshot();
        if (state.job) { await api.invoke('dsh_cancel_operation'); await api.settled(); }
        if (state.runtime.phase === 'web_ready' || state.runtime.phase === 'starting') await api.invoke('dsh_stop');
        const settled = await api.snapshot();
        await api.invoke('dsh_save_settings', { settings: api.initial_snapshot.settings, expected_digest: settled.settings_digest });
        api.element('dsh_cancel_preview')?.click();
        await window.__aag_open_client_management('claude_code');
        await api.wait_for(() => document.querySelector('.page-header h2')?.textContent === 'Claude');
        return true;
      } finally { api.restore(); }
    })()`);
    await sleep(150);
  }

  await check_in_page(cdp, check, 'DSH scenarios preserve Claude gateway provider and route fixtures', async () => {
    const api = window.__aag_dsh_smoke;
    const preserved = await api.fixture() === api.initial_fixture;
    delete window.__aag_dsh_smoke;
    return { ok: preserved && document.querySelector('.page-header h2')?.textContent === 'Claude', detail: 'other-fixtures-preserved=' + preserved };
  });
}
