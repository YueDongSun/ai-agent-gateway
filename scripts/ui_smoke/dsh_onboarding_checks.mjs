/** Synthetic UI evidence only. Run after the existing DSH checks initialize the demo profile. */
export async function run_dsh_onboarding_checks({ cdp, check_in_page, check }) {
  await cdp.eval_value(`(async () => {
    if (window.__aagDemoIsDemo !== true) throw new Error('onboarding smoke requires the isolated browser demo');
    const bridge = window.__TAURI_INTERNALS__;
    if (typeof bridge?.invoke !== 'function') throw new Error('demo IPC is unavailable');
    const original = bridge.invoke;
    const commands = [];
    const override = { discovery: null, blocked_plan: false };
    bridge.invoke = async (command, args) => {
      if (command.startsWith('dsh_')) commands.push({ command, args });
      const result = await original(command, args);
      if (command === 'dsh_discover_runtime' && override.discovery) return structuredClone(override.discovery);
      if (command === 'dsh_preview_setup' && override.blocked_plan) return { ...result, can_apply: false, blockers: ['runtime_dsh_invalid'] };
      return result;
    };
    const invoke = (command, args) => {
      if (window.__aagDemoIsDemo !== true) throw new Error('demo guard changed');
      return bridge.invoke(command, args);
    };
    const element = (id) => document.querySelector('[data-testid="' + id + '"]');
    const wait_for = async (predicate, timeout = 3500) => {
      const deadline = Date.now() + timeout;
      while (Date.now() < deadline) {
        const value = await predicate();
        if (value) return value;
        await new Promise((resolve) => setTimeout(resolve, 50));
      }
      throw new Error('onboarding UI condition timed out');
    };
    const click = async (id) => {
      const target = await wait_for(() => { const item = element(id); return item && !item.disabled ? item : null; });
      target.focus(); target.click();
      await new Promise((resolve) => setTimeout(resolve, 70));
    };
    const input = async (id, value) => {
      const target = element(id);
      if (!target || target.disabled) throw new Error('onboarding field unavailable: ' + id);
      const prototype = target instanceof HTMLSelectElement ? HTMLSelectElement.prototype : HTMLInputElement.prototype;
      Object.getOwnPropertyDescriptor(prototype, 'value').set.call(target, value);
      target.dispatchEvent(new Event(target instanceof HTMLSelectElement ? 'change' : 'input', { bubbles: true }));
      await new Promise((resolve) => setTimeout(resolve, 70));
    };
    const snapshot = () => invoke('dsh_get_snapshot');
    const save_settings = async (settings) => { const before = await snapshot(); return invoke('dsh_save_settings', { settings, expected_digest: before.settings_digest }); };
    const confirm = async () => { await click('dsh_acknowledge'); await click('dsh_confirm'); await wait_for(() => !element('dsh_preview_dialog')); };
    const initial = await snapshot();
    if (!initial.initialized || initial.runtime.phase !== 'stopped' || initial.job) { bridge.invoke = original; throw new Error('run onboarding UI checks after the existing DSH checks finish with an initialized, stopped demo profile'); }
    window.__aag_onboarding_smoke = { invoke, commands, override, element, wait_for, click, input, snapshot, save_settings, confirm, initial,
      restore: () => { bridge.invoke = original; } };
    return true;
  })()`);
  try {
    await check_in_page(cdp, check, 'DSH onboarding detects existing paths on mount without mutations', async () => {
      const api = window.__aag_onboarding_smoke;
      document.querySelector('.app-switcher button').click();
      await api.wait_for(() => !api.element('dsh_page'));
      const before = JSON.stringify(await api.snapshot()); api.commands.length = 0;
      await api.click('dsh_nav');
      await api.wait_for(() => api.element('dsh_discovery_summary'));
      const read_only = api.commands.every((item) => ['dsh_get_snapshot', 'dsh_list_upstreams', 'dsh_discover_runtime'].includes(item.command));
      return { ok: read_only && before === JSON.stringify(await api.snapshot()) && ['node', 'dsh', 'pnpm'].every((kind) => api.element('dsh_detected_' + kind)?.dataset.compatible === 'true'),
        detail: 'read-only=' + read_only + ' detected-components=3' };
    });

    await check_in_page(cdp, check, 'DSH detected runtime adoption requires a cancellable review', async () => {
      const api = window.__aag_onboarding_smoke;
      const state = await api.snapshot();
      await api.save_settings({ ...state.settings, node_path: '', dsh_entry_path: '', pnpm_entry_path: '', upstream_id: null, model_id: null });
      await api.click('dsh_refresh'); await api.wait_for(() => !api.element('dsh_setup_review').disabled);
      const before = JSON.stringify(await api.snapshot());
      await api.click('dsh_setup_review'); await api.wait_for(() => api.element('dsh_preview_dialog'));
      const preview = api.element('dsh_preview_dialog').textContent;
      const guarded = api.element('dsh_confirm').disabled;
      await api.click('dsh_cancel_preview');
      return { ok: guarded && preview.includes('C:\\Demo\\node.exe') && preview.includes('claude-sonnet-4-6') && before === JSON.stringify(await api.snapshot()), detail: 'guarded=' + guarded + ' cancelled-without-save=true' };
    });

    await check_in_page(cdp, check, 'DSH explicit quick setup saves detected paths and provider without install or start', async () => {
      const api = window.__aag_onboarding_smoke;
      await api.input('dsh_setup_selection', 'proxy:demo_pool');
      const before = await api.snapshot(); api.commands.length = 0;
      await api.click('dsh_setup_review'); await api.confirm();
      await api.wait_for(async () => (await api.snapshot()).settings.upstream_id === 'proxy:demo_pool');
      const state = await api.snapshot();
      const forbidden = api.commands.filter((item) => ['dsh_execute_operation', 'dsh_start'].includes(item.command));
      return { ok: state.settings.node_path === 'C:\\Demo\\node.exe' && state.settings.dsh_entry_path === 'C:\\Demo\\dsh\\cli.js' && state.settings.pnpm_entry_path === 'C:\\Demo\\pnpm.cjs' && state.settings.model_id === 'deepseek-chat' && state.initialized && state.runtime.phase === 'stopped' && state.receipts.length === before.receipts.length && !forbidden.length,
        detail: 'selected=' + state.settings.upstream_id + ' model=' + state.settings.model_id + ' installs-or-starts=' + forbidden.length };
    });

    await check_in_page(cdp, check, 'DSH onboarding preserves saved selections and unsaved settings drafts', async () => {
      const api = window.__aag_onboarding_smoke;
      const state = await api.snapshot();
      await api.save_settings({ ...state.settings, upstream_id: 'claude:demo_native', model_id: 'claude-opus-4-6' });
      await api.click('dsh_refresh'); await api.wait_for(() => api.element('dsh_model')?.value === 'claude-opus-4-6');
      const preserved = api.element('dsh_setup_selection').value === 'claude:demo_native' && api.element('dsh_setup_selection').disabled && api.element('dsh_setup_model').value === 'claude-opus-4-6';
      await api.input('dsh_model', 'claude-sonnet-4-6'); await api.click('dsh_discover_runtime');
      await api.wait_for(() => !api.element('dsh_discover_runtime').disabled);
      const draft_preserved = api.element('dsh_model').value === 'claude-sonnet-4-6' && api.element('dsh_setup_review').disabled && Boolean(api.element('dsh_setup_dirty')) && (await api.snapshot()).settings.model_id === 'claude-opus-4-6';
      await api.click('dsh_discard_settings');
      return { ok: preserved && draft_preserved, detail: 'saved-selection=' + preserved + ' unsaved-draft=' + draft_preserved };
    });

    await check_in_page(cdp, check, 'DSH quick setup rejects a plan made stale by another settings write', async () => {
      const api = window.__aag_onboarding_smoke;
      await api.click('dsh_setup_review'); await api.wait_for(() => api.element('dsh_preview_dialog'));
      const before = await api.snapshot();
      await api.save_settings({ ...before.settings, port: before.settings.port === 3142 ? 3143 : 3142 });
      const external = await api.snapshot();
      await api.confirm(); await api.wait_for(() => api.element('dsh_error'));
      const state = await api.snapshot();
      return { ok: api.element('dsh_error').textContent.includes('DSH_SETTINGS_CHANGED') && state.settings_digest === external.settings_digest && state.settings.port === external.settings.port && state.receipts.length === before.receipts.length,
        detail: 'stale-plan-rejected=' + (state.settings_digest === external.settings_digest) };
    });

    await check_in_page(cdp, check, 'DSH missing and unsupported runtimes remain blocked with explicit evidence', async () => {
      const api = window.__aag_onboarding_smoke;
      api.override.discovery = { candidates: [{ candidate_id: 'unsupported_dsh', kind: 'dsh', path: 'C:\\Demo\\unsupported\\cli.js', source: 'user_install', version: '9.9.9', compatible: false, validation: 'rejected', reason: 'unsupported_runtime_version' }], recommended: { node_path: '', dsh_entry_path: '', pnpm_entry_path: '' }, issues: ['node_not_found', 'dsh_not_found', 'pnpm_not_found'], scanned_locations: 3, scan_limited: false };
      const state = await api.snapshot();
      await api.save_settings({ ...state.settings, node_path: '', dsh_entry_path: '', pnpm_entry_path: '' });
      await api.click('dsh_refresh'); await api.click('dsh_discover_runtime');
      await api.wait_for(() => api.element('dsh_setup_missing'));
      api.override.blocked_plan = true;
      const apply_count = api.commands.filter((item) => item.command === 'dsh_apply_setup').length;
      await api.click('dsh_setup_review'); await api.click('dsh_acknowledge');
      const blocked = Boolean(api.element('dsh_setup_blockers')) && api.element('dsh_confirm').disabled;
      await api.click('dsh_cancel_preview');
      const no_apply = apply_count === api.commands.filter((item) => item.command === 'dsh_apply_setup').length;
      api.override.discovery = null; api.override.blocked_plan = false;
      await api.click('dsh_discover_runtime');
      return { ok: blocked && no_apply, detail: 'unsupported-blocked=' + blocked + ' no-apply=' + no_apply };
    });

    await check_in_page(cdp, check, 'DSH discovery errors do not claim setup success', async () => {
      const api = window.__aag_onboarding_smoke;
      window.__aagDemoSetError('dsh_discover_runtime');
      const before = JSON.stringify(await api.snapshot());
      await api.click('dsh_discover_runtime'); await api.wait_for(() => api.element('dsh_discovery_error'));
      const unchanged = before === JSON.stringify(await api.snapshot());
      window.__aagDemoSetError(null); await api.click('dsh_discover_runtime');
      await api.wait_for(() => !api.element('dsh_discovery_error') && !api.element('dsh_discover_runtime').disabled);
      return { ok: unchanged, detail: 'configuration-unchanged=' + unchanged };
    });
  } finally {
    await cdp.eval_value(`(async () => {
      const api = window.__aag_onboarding_smoke;
      if (!api) return;
      try {
        api.override.discovery = null; api.override.blocked_plan = false; window.__aagDemoSetError(null);
        if (api.element('dsh_preview_dialog')) await api.click('dsh_cancel_preview');
        await api.save_settings(api.initial.settings);
        await api.click('dsh_refresh');
      } finally { api.restore(); delete window.__aag_onboarding_smoke; }
      return true;
    })()`);
  }
}
