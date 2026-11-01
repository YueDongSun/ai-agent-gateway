/** Browser-demo evidence only; backend file-safety is covered by Rust tests. */
export function install_mcp_smoke_helpers() {
    if (window.__aagDemoIsDemo !== true) throw new Error('MCP smoke requires synthetic IPC');
    const bridge = window.__TAURI_INTERNALS__;
    const original = bridge.invoke;
    const calls = [];
    const override = { expired_preview: false, delay_save: false, release_save: null };
    bridge.invoke = async (command, args) => {
      if (command.startsWith('mcp_')) calls.push({ command, args });
      if (command === 'mcp_save_server' && override.delay_save) await new Promise((resolve) => { override.release_save = resolve; });
      const result = await original(command, args);
      return command === 'mcp_preview_sync' && override.expired_preview ? { ...result, expires_at: new Date(Date.now() - 1000).toISOString() } : result;
    };
    const element = (id) => document.querySelector('[data-testid="' + id + '"]');
    const pending_waits = new Set();
    // The CDP client owns a host-side deadline. DOM observation keeps these
    // waits responsive when Chromium suspends timers in a background page.
    const wait_for = async (predicate) => {
      const initial = await predicate();
      if (initial) return initial;
      return new Promise((resolve_wait, reject_wait) => {
        let settled = false;
        let checking = false;
        let check_again = false;
        const finish = (error, value) => {
          if (settled) return;
          settled = true;
          observer.disconnect();
          pending_waits.delete(cancel);
          if (error) reject_wait(error); else resolve_wait(value);
        };
        const cancel = () => finish(new Error('MCP smoke restored before condition settled'));
        const inspect = async () => {
          if (settled) return;
          if (checking) { check_again = true; return; }
          checking = true;
          try {
            do {
              check_again = false;
              const value = await predicate();
              if (value) finish(null, value);
            } while (!settled && check_again);
          } catch (error) { finish(error); }
          finally { checking = false; }
        };
        const observer = new MutationObserver(() => { void inspect(); });
        observer.observe(document.documentElement, {
          subtree: true, childList: true, attributes: true, characterData: true,
        });
        pending_waits.add(cancel);
        // Recheck after subscribing so a mutation during the initial await
        // cannot be missed.
        void inspect();
      });
    };
    const click = async (id) => {
      const target = await wait_for(() => { const item = element(id); return item && !item.disabled ? item : null; });
      target.focus();
      target.click();
      // Page timers can be delayed far beyond their requested duration during
      // a full smoke run. Let the event finish; checks wait for their UI state.
      await Promise.resolve();
    };
    const input = async (id, value) => {
      const target = element(id);
      const prototype = target instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
      Object.getOwnPropertyDescriptor(prototype, 'value').set.call(target, value);
      target.dispatchEvent(new Event('input', { bubbles: true }));
      await Promise.resolve();
    };
    window.__aag_mcp_smoke = { element, wait_for, click, input, calls, override,
      invoke: (command, args) => bridge.invoke(command, args),
      count: (command) => calls.filter((call) => call.command === command).length,
      restore: () => {
        for (const cancel of [...pending_waits]) cancel();
        bridge.invoke = original;
      } };
    return true;
}

export async function run_mcp_checks({ cdp, check_in_page, check }) {
  await cdp.eval_value('(' + install_mcp_smoke_helpers.toString() + ')()');
  try {
    await check_in_page(cdp, check, 'MCP mount and refresh are read-only with English empty state', async () => {
      const api = window.__aag_mcp_smoke;
      await api.click('mcp_nav'); await api.wait_for(() => api.element('mcp_add') && !api.element('mcp_add').disabled);
      await api.click('mcp_refresh');
      await api.wait_for(() => api.element('mcp_page')?.textContent.includes('Your tools start here'));
      const read_only = api.calls.every((call) => call.command === 'mcp_list_servers');
      return { ok: read_only && api.element('mcp_page').textContent.includes('Your tools start here'), detail: 'read-only=' + read_only };
    });
    await check_in_page(cdp, check, 'MCP editor rejects invalid JSON then saves a masked definition without a client write', async () => {
      const api = window.__aag_mcp_smoke;
      await api.click('mcp_add'); await api.wait_for(() => api.element('mcp_id'));
      await api.input('mcp_id', 'smoke_mcp'); await api.input('mcp_name', 'Automation fixture');
      await api.input('mcp_config', '{broken');
      await api.click('mcp_save');
      await api.wait_for(() => api.element('mcp_editor_error'));
      if (!api.element('mcp_editor_error') || api.count('mcp_save_server') !== 0) return 'invalid-json-crossed-ipc';
      await api.input('mcp_config', JSON.stringify({ command: 'node', args: ['/synthetic/server.mjs'], env: { TEST_TOKEN: 'synthetic_mcp_secret' } }));
      await api.click('mcp_save');
      await api.wait_for(() => api.element('mcp_server_smoke_mcp'));
      const summary = (await api.invoke('mcp_list_servers'))[0];
      return { ok: !api.element('mcp_editor') && !document.body.innerText.includes('synthetic_mcp_secret') && !JSON.stringify(summary).includes('synthetic_mcp_secret') && api.count('mcp_apply_sync') === 0,
        detail: 'definition-saved=true secret-not-rendered=true client-writes=' + api.count('mcp_apply_sync') };
    });
    await check_in_page(cdp, check, 'MCP metadata editing preserves private configuration and search filters current rows', async () => {
      const api = window.__aag_mcp_smoke;
      await api.click('mcp_edit_smoke_mcp');
      if (api.element('mcp_config') || api.element('mcp_replace_config').checked) return 'private-config-loaded-into-editor';
      await api.input('mcp_name', 'Renamed fixture'); await api.click('mcp_save');
      await api.wait_for(() => !api.element('mcp_editor'));
      const last_save = api.calls.filter((call) => call.command === 'mcp_save_server').at(-1);
      await api.input('mcp_search', 'not_matching');
      const filtered = !api.element('mcp_server_smoke_mcp');
      await api.input('mcp_search', 'renamed');
      const found = Boolean(api.element('mcp_server_smoke_mcp'));
      await api.input('mcp_search', '');
      return { ok: last_save.args.input.server_config === undefined && filtered && found, detail: 'private-config-omitted=true search-current=' + (filtered && found) };
    });
    await check_in_page(cdp, check, 'MCP duplicate Add cannot overwrite an existing private definition', async () => {
      const api = window.__aag_mcp_smoke;
      const before = JSON.stringify(await api.invoke('mcp_list_servers'));
      await api.click('mcp_add'); await api.input('mcp_id', 'smoke_mcp'); await api.input('mcp_name', 'Unintended replacement');
      await api.input('mcp_config', JSON.stringify({ command: 'node', args: ['different.mjs'] }));
      await api.click('mcp_save'); await api.wait_for(() => api.element('mcp_editor_error'));
      const rejected = api.element('mcp_editor_error').textContent.includes('MCP_SERVER_EXISTS');
      await api.click('mcp_cancel_editor');
      return { ok: rejected && before === JSON.stringify(await api.invoke('mcp_list_servers')), detail: 'duplicate-rejected=' + rejected + ' existing-config-unchanged=true' };
    });
    await check_in_page(cdp, check, 'MCP stale metadata edits do not overwrite newer client selections', async () => {
      const api = window.__aag_mcp_smoke;
      await api.click('mcp_edit_smoke_mcp'); await api.input('mcp_name', 'Stale name');
      await api.invoke('mcp_set_enabled', { id: 'smoke_mcp', client: 'codex', enabled: true });
      await api.click('mcp_save'); await api.wait_for(() => api.element('mcp_editor_error'));
      const server = (await api.invoke('mcp_list_servers'))[0];
      const rejected = api.element('mcp_editor_error').textContent.includes('MCP_REVISION_MISMATCH');
      await api.click('mcp_cancel_editor'); await api.click('mcp_refresh');
      return { ok: rejected && server.enabled_codex && server.name === 'Renamed fixture', detail: 'stale-edit-rejected=' + rejected + ' newer-selection-preserved=true' };
    });
    await check_in_page(cdp, check, 'MCP client flags require an acknowledged cancellable sync review', async () => {
      const api = window.__aag_mcp_smoke;
      await api.click('mcp_claude_smoke_mcp');
      if (!(await api.invoke('mcp_list_servers'))[0].enabled_claude || api.count('mcp_apply_sync') !== 0) return 'flag-mutated-client';
      await api.click('mcp_preview_claude'); await api.wait_for(() => api.element('mcp_review'));
      const guarded = api.element('mcp_apply').disabled;
      const masked = !api.element('mcp_preview_config').textContent.includes('synthetic_mcp_secret');
      const dialog = api.element('mcp_review');
      dialog.focus(); dialog.dispatchEvent(new KeyboardEvent('keydown', { key: 'Tab', bubbles: true, cancelable: true }));
      const focus_contained = dialog.contains(document.activeElement);
      await api.click('mcp_cancel_review');
      await api.wait_for(() => api.count('mcp_discard_review') > 0);
      return { ok: guarded && masked && focus_contained && api.count('mcp_apply_sync') === 0, detail: 'guarded=' + guarded + ' focus-contained=' + focus_contained + ' writes=0 cancelled-review-released=true' };
    });
    await check_in_page(cdp, check, 'MCP confirmed sync reports its receipt and rejects token replay', async () => {
      const api = window.__aag_mcp_smoke;
      await api.click('mcp_preview_claude'); await api.click('mcp_acknowledge'); await api.click('mcp_apply');
      await api.wait_for(() => !api.element('mcp_review') && api.element('mcp_notice'));
      const call = api.calls.filter((item) => item.command === 'mcp_apply_sync').at(-1);
      let replay_rejected = false;
      try { await api.invoke('mcp_apply_sync', call.args); } catch { replay_rejected = true; }
      return { ok: replay_rejected && /configuration updated/.test(api.element('mcp_notice').textContent), detail: 'receipt-visible=true token-replay-rejected=' + replay_rejected };
    });
    await check_in_page(cdp, check, 'MCP expired reviews cannot apply and busy editors retain keyboard focus', async () => {
      const api = window.__aag_mcp_smoke;
      const before = api.count('mcp_apply_sync');
      api.override.expired_preview = true;
      await api.click('mcp_preview_claude'); await api.click('mcp_acknowledge');
      const blocked = api.element('mcp_apply').disabled && api.element('mcp_review').textContent.includes('review expired');
      await api.click('mcp_cancel_review'); api.override.expired_preview = false;
      await api.click('mcp_edit_smoke_mcp'); api.override.delay_save = true;
      await api.click('mcp_save'); await api.wait_for(() => api.override.release_save);
      const dialog = api.element('mcp_editor');
      dialog.dispatchEvent(new KeyboardEvent('keydown', { key: 'Tab', bubbles: true, cancelable: true }));
      const contained = document.activeElement === dialog && api.element('mcp_save').disabled;
      api.override.delay_save = false; api.override.release_save(); api.override.release_save = null;
      await api.wait_for(() => !api.element('mcp_editor'));
      return { ok: blocked && contained && api.count('mcp_apply_sync') === before, detail: 'expired-blocked=' + blocked + ' busy-focus-contained=' + contained };
    });
    await check_in_page(cdp, check, 'MCP stale sync and backend failures stay actionable without false success', async () => {
      const api = window.__aag_mcp_smoke;
      await api.click('mcp_preview_claude');
      const current = (await api.invoke('mcp_list_servers'))[0];
      await api.invoke('mcp_set_enabled', { id: 'smoke_mcp', client: 'codex', enabled: !current.enabled_codex });
      await api.click('mcp_acknowledge'); await api.click('mcp_apply');
      await api.wait_for(() => api.element('mcp_review_error'));
      const rejected = api.element('mcp_review_error').textContent.includes('MCP_REVIEW_STALE') && api.element('mcp_apply').disabled;
      await api.click('mcp_cancel_review');
      window.__aagDemoSetError('mcp_list_servers'); await api.click('mcp_refresh');
      await api.wait_for(() => api.element('mcp_error'));
      const failure_visible = api.element('mcp_error').textContent.includes('No success was confirmed');
      window.__aagDemoSetError(null); await api.click('mcp_refresh');
      return { ok: rejected && failure_visible, detail: 'stale-rejected=' + rejected + ' read-failure-visible=' + failure_visible };
    });
    await check_in_page(cdp, check, 'MCP Codex preview has no apply action and deletion is separately confirmed', async () => {
      const api = window.__aag_mcp_smoke;
      const before = api.count('mcp_apply_sync');
      await api.click('mcp_preview_codex'); await api.wait_for(() => api.element('mcp_review'));
      const no_apply = !api.element('mcp_apply') && api.element('mcp_review').textContent.includes('preview-only');
      await api.click('mcp_cancel_review');
      await api.click('mcp_delete_smoke_mcp'); await api.click('mcp_cancel_delete');
      const cancelled = Boolean(api.element('mcp_server_smoke_mcp')) && api.count('mcp_delete_server') === 0;
      await api.click('mcp_delete_smoke_mcp'); await api.click('mcp_confirm_delete');
      await api.wait_for(() => !api.element('mcp_server_smoke_mcp') && !api.element('mcp_delete_dialog'));
      return { ok: no_apply && cancelled && api.count('mcp_apply_sync') === before && (await api.invoke('mcp_list_servers')).length === 0,
        detail: 'codex-read-only=' + no_apply + ' delete-cancelled=' + cancelled + ' registry-delete-only=true' };
    });
    await check_in_page(cdp, check, 'MCP an outdated editor cannot resurrect a deleted definition', async () => {
      const api = window.__aag_mcp_smoke;
      await api.invoke('mcp_save_server', { input: { id: 'deleted_fixture', name: 'Deleted fixture', create_only: true, server_config: { command: 'node', args: ['fixture.mjs'] }, enabled_claude: false, enabled_codex: false } });
      await api.click('mcp_refresh'); await api.click('mcp_edit_deleted_fixture');
      await api.click('mcp_replace_config'); await api.input('mcp_config', JSON.stringify({ command: 'node', args: ['replacement.mjs'] }));
      await api.invoke('mcp_delete_server', { id: 'deleted_fixture' });
      await api.click('mcp_save'); await api.wait_for(() => api.element('mcp_editor_error'));
      const rejected = api.element('mcp_editor_error').textContent.includes('MCP_REVISION_MISMATCH');
      await api.click('mcp_cancel_editor'); await api.click('mcp_refresh');
      return { ok: rejected && (await api.invoke('mcp_list_servers')).length === 0, detail: 'deleted-row-remains-absent=' + rejected };
    });
  } finally {
    await cdp.eval_value(`(async () => {
      window.__aagDemoSetError(null);
      window.__aag_mcp_smoke?.override.release_save?.();
      window.__aag_mcp_smoke?.restore(); delete window.__aag_mcp_smoke;
      await window.__aag_open_client_management('claude_code');
      return true;
    })()`);
    await cdp.eval_value(`(async () => { for (let i = 0; i < 40; i++) { if (document.querySelector('[data-testid="gateway-start"]')) return true; await new Promise(r => setTimeout(r, 50)); } throw new Error('Claude page not restored after MCP checks'); })()`);
  }
}
