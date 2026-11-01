/** Browser-demo evidence for the suppliers workspace.
 *
 *  Drives the real page through its navigation and buttons against the
 *  synthetic in-memory supplier backend. Covers the provider-first create
 *  flow (API type + URL + key, fetch models before the first save, selected
 *  fetched models landing in the initial endpoint inventory), supplier CRUD,
 *  write-only secret preservation and explicit clearing, endpoint and model
 *  editing, cc-switch import preview -> apply, the saved/active generation
 *  banner, four client snippets, the routing preview, and the request trace.
 *
 *  Everything here is synthetic IPC evidence only: it proves neither native
 *  WebView2 behavior nor real upstream availability.
 */
import { run_supplier_discovery_locale_checks } from './supplier_discovery_locale_checks.mjs';

export async function run_supplier_checks({ cdp, check_in_page, check, sleep }) {
  await cdp.eval_value(`(() => {
    if (window.__aagDemoIsDemo !== true) throw new Error('supplier smoke requires synthetic IPC');
    const element = (id) => document.querySelector('[data-testid="' + id + '"]');
    const wait_for = async (predicate) => {
      const deadline = Date.now() + 5000;
      while (Date.now() < deadline) {
        const value = await predicate();
        if (value) return value;
        await new Promise((resolve) => setTimeout(resolve, 40));
      }
      throw new Error('supplier UI condition timed out');
    };
    const set_text = async (id, value) => {
      const target = await wait_for(() => element(id));
      const prototype = target instanceof HTMLTextAreaElement
        ? HTMLTextAreaElement.prototype
        : target instanceof HTMLSelectElement
          ? HTMLSelectElement.prototype
          : HTMLInputElement.prototype;
      Object.getOwnPropertyDescriptor(prototype, 'value').set.call(target, value);
      target.dispatchEvent(new Event('input', { bubbles: true }));
      target.dispatchEvent(new Event('change', { bubbles: true }));
      await new Promise((resolve) => setTimeout(resolve, 60));
    };
    const click = async (id) => {
      const target = await wait_for(() => {
        const item = element(id);
        return item && !item.disabled ? item : null;
      });
      target.focus();
      target.click();
      await new Promise((resolve) => setTimeout(resolve, 80));
    };
    const card_id_for_name = (name) => {
      const card = [...document.querySelectorAll('.supplier_card')].find((node) => node.querySelector('h3')?.textContent === name);
      return card ? card.getAttribute('data-testid').replace(/^suppliers_card_/, '') : null;
    };
    const last_model_input = () => {
      const rows = [...document.querySelectorAll('.supplier_models_table tbody tr')];
      return rows.length ? rows[rows.length - 1].querySelector('input') : null;
    };
    const last_model_tier = () => {
      const rows = [...document.querySelectorAll('.supplier_models_table tbody tr')];
      return rows.length ? rows[rows.length - 1].querySelector('select') : null;
    };
    window.__aag_supplier_smoke = { element, wait_for, set_text, click, card_id_for_name, last_model_input, last_model_tier };
    return true;
  })()`);

  try {
    await check_in_page(cdp, check, 'suppliers page mounts from the shell with seeded suppliers', async () => {
      const api = window.__aag_supplier_smoke;
      await api.click('suppliers_nav');
      await api.wait_for(() => api.element('suppliers_page'));
      await api.wait_for(() => api.element('suppliers_card_supplier_acme'));
      await api.wait_for(() => api.element('supplier-maintenance'));
      return {
        ok: !api.element('suppliers_card_supplier_pending_official') && !api.element('suppliers_official_notice') && Boolean(api.element('suppliers_card_supplier_acme')),
        detail: 'valid API supplier retained; imported login records absent',
      };
    });

    await run_supplier_discovery_locale_checks({ cdp, check_in_page, check });

    await check_in_page(cdp, check, 'create a supplier through the provider-first flow with fetched models', async () => {
      const api = window.__aag_supplier_smoke;
      await api.click('suppliers_add');
      await api.wait_for(() => api.element('suppliers_editor'));
      await api.set_text('suppliers_editor_name', 'Smoke supplier');
      // openai-chat keeps the anthropic endpoint addable for the later
      // endpoint-inventory check.
      await api.set_text('suppliers_editor_protocol', 'openai-chat');
      await api.set_text('suppliers_editor_base_url', 'https://smoke.example.com');
      await api.set_text('suppliers_editor_key_value', 'sk-smoke-secret');
      await api.click('suppliers_editor_fetch_models');
      await api.wait_for(() => api.element('suppliers_discovered'));
      // Selection drives the initial endpoint inventory: exclude the last
      // fetched id so two of the three advertised models are saved.
      await api.click('suppliers_discovered_2');
      await api.click('suppliers_editor_save');
      await api.wait_for(() => api.element('suppliers_notice'));
      const id = await api.wait_for(() => api.card_id_for_name('Smoke supplier'));
      const text = api.element('suppliers_card_' + id)?.textContent ?? '';
      const key_badge = api.element('suppliers_card_key_' + id)?.textContent ?? '';
      return {
        ok:
          Boolean(id) &&
          key_badge.includes('Key stored') &&
          text.includes('2 models') &&
          text.includes('OpenAI Chat Completions'),
        detail: 'id=' + id + ' key=' + key_badge.trim() + ' models=' + (text.includes('2 models') ? '2' : '?'),
      };
    });

    await check_in_page(cdp, check, 'edits keep the stored key untouched', async () => {
      const api = window.__aag_supplier_smoke;
      const id = api.card_id_for_name('Smoke supplier');
      await api.click('suppliers_edit_' + id);
      await api.wait_for(() => api.element('suppliers_editor'));
      await api.set_text('suppliers_editor_priority', '80');
      await api.click('suppliers_editor_save');
      await api.wait_for(() => api.element('suppliers_notice'));
      const key_badge = api.element('suppliers_card_key_' + id)?.textContent ?? '';
      return { ok: key_badge.includes('Key stored'), detail: 'key-after-edit=' + key_badge.trim() };
    });

    await check_in_page(cdp, check, 'fetching models on edit reuses the stored key', async () => {
      const api = window.__aag_supplier_smoke;
      const id = api.card_id_for_name('Smoke supplier');
      await api.click('suppliers_edit_' + id);
      await api.wait_for(() => api.element('suppliers_editor'));
      // The stored endpoint preselects its protocol and the key field stays
      // untouched, so the probe must ride the stored credential.
      await api.click('suppliers_editor_fetch_models');
      await api.wait_for(() => api.element('suppliers_discovered'));
      await api.click('suppliers_editor_save');
      await api.wait_for(() => api.element('suppliers_notice'));
      const text = api.element('suppliers_card_' + id)?.textContent ?? '';
      const key_badge = api.element('suppliers_card_key_' + id)?.textContent ?? '';
      return {
        ok: key_badge.includes('Key stored') && text.includes('3 models'),
        detail: 'key=' + key_badge.trim() + ' models=' + (text.includes('3 models') ? '3' : '?'),
      };
    });

    await check_in_page(cdp, check, 'explicit clear removes the stored key', async () => {
      const api = window.__aag_supplier_smoke;
      const id = api.card_id_for_name('Smoke supplier');
      await api.click('suppliers_edit_' + id);
      await api.wait_for(() => api.element('suppliers_editor'));
      await api.click('suppliers_editor_key_clear');
      await api.click('suppliers_editor_save');
      await api.wait_for(() => api.element('suppliers_notice'));
      const key_badge = api.element('suppliers_card_key_' + id)?.textContent ?? '';
      return { ok: key_badge.includes('No key'), detail: 'key-after-clear=' + key_badge.trim() };
    });

    await check_in_page(cdp, check, 'endpoint and model inventories save through the models command', async () => {
      const api = window.__aag_supplier_smoke;
      const id = api.card_id_for_name('Smoke supplier');
      await api.click('suppliers_edit_' + id);
      await api.wait_for(() => api.element('suppliers_editor'));
      await api.click('suppliers_tab_endpoints');
      await api.wait_for(() => api.element('suppliers_tab_endpoints_panel'));
      await api.click('suppliers_endpoint_add_anthropic');
      await api.click('suppliers_editor_save');
      await api.wait_for(() => api.element('suppliers_notice'));
      await api.click('suppliers_edit_' + id);
      await api.wait_for(() => api.element('suppliers_editor'));
      await api.click('suppliers_tab_models');
      await api.wait_for(() => api.element('suppliers_tab_models_panel'));
      await api.set_text('suppliers_models_endpoint', 'anthropic');
      await api.click('suppliers_model_add');
      await api.wait_for(() => api.last_model_input());
      const input = api.last_model_input();
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(input, 'claude-opus-4-8');
      input.dispatchEvent(new Event('input', { bubbles: true }));
      const tier = api.last_model_tier();
      Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, 'value').set.call(tier, '2');
      tier.dispatchEvent(new Event('change', { bubbles: true }));
      await new Promise((resolve) => setTimeout(resolve, 60));
      await api.click('suppliers_models_save');
      await api.wait_for(() => api.element('suppliers_notice'));
      // A scoped model save keeps the editor and any other draft edits open.
      // Close explicitly before continuing to another operation. The card counts models
      // across endpoints: 3 fetched openai-chat models + 1 manual anthropic
      // model.
      const card = await api.wait_for(() => {
        const node = api.element('suppliers_card_' + id);
        return node && (node.textContent ?? '').includes('Anthropic Messages') ? node : null;
      });
      const text = card.textContent ?? '';
      const retained_editor = Boolean(api.element('suppliers_editor'));
      await api.click('suppliers_editor_close');
      return {
        ok: retained_editor && text.includes('Anthropic Messages') && text.includes('4 models'),
        detail: 'card-models=' + (text.includes('4 models') ? '4' : '?'),
      };
    });

    await check_in_page(cdp, check, 'import preview shows the plan and apply materializes it', async () => {
      const api = window.__aag_supplier_smoke;
      await api.click('suppliers_import');
      await api.wait_for(() => api.element('suppliers_import_dialog'));
      await api.click('suppliers_import_preview');
      await api.wait_for(() => api.element('suppliers_import_preview_panel'));
      const changes = document.querySelectorAll('[data-testid^="suppliers_import_change_"]').length;
      await api.click('suppliers_import_apply');
      await api.wait_for(() => api.element('suppliers_notice'));
      const notice = api.element('suppliers_notice')?.textContent ?? '';
      // A successful apply closes the dialog and lands the notice on the page.
      await api.wait_for(() => api.element('suppliers_page'));
      return {
        ok: changes > 0 && notice.length > 0,
        detail: 'change-tags=' + changes + ' apply-notice=' + notice.slice(0, 60),
      };
    });

    await check_in_page(cdp, check, 'hot-update banner reports the saved generation state', async () => {
      const api = window.__aag_supplier_smoke;
      const stale = api.element('suppliers_hot_update_stale');
      const current = api.element('suppliers_hot_update_current');
      const not_running = api.element('suppliers_hot_update_not_running');
      return {
        ok: Boolean(stale || current || not_running),
        detail: 'stale=' + Boolean(stale) + ' current=' + Boolean(current) + ' stopped=' + Boolean(not_running),
      };
    });

    await check_in_page(cdp, check, 'four client snippets show the protocol paths', async () => {
      const api = window.__aag_supplier_smoke;
      await api.click('suppliers_advanced_toggle');
      await api.wait_for(() => api.element('suppliers_snippets'));
      const body = api.element('suppliers_snippets')?.textContent ?? '';
      return {
        ok:
          body.includes('/v1/messages') &&
          body.includes('/v1/chat/completions') &&
          body.includes('/v1/responses') &&
          body.includes('/v1beta/models'),
        detail:
          'messages=' + body.includes('/v1/messages') +
          ' chat=' + body.includes('/v1/chat/completions') +
          ' responses=' + body.includes('/v1/responses') +
          ' gemini=' + body.includes('/v1beta/models'),
      };
    });

    await check_in_page(cdp, check, 'routing preview separates aliases from exact ids', async () => {
      const api = window.__aag_supplier_smoke;
      await api.wait_for(() => api.element('suppliers_routing'));
      const mode = await api.wait_for(() => api.element('suppliers_routing_mode'));
      const alias_mode = (mode.textContent ?? '').length > 0;
      await api.set_text('suppliers_routing_alias', 'claude-opus-4-8');
      await new Promise((resolve) => setTimeout(resolve, 80));
      const after = api.element('suppliers_routing_mode')?.textContent ?? '';
      return { ok: alias_mode && after.length > 0, detail: 'mode-before=' + alias_mode + ' mode-after=' + after.trim() };
    });

    await check_in_page(cdp, check, 'request trace shows failover marks from the timeline', async () => {
      const api = window.__aag_supplier_smoke;
      // The demo has no live traffic, and earlier suite steps exercise
      // `diagnostics_clear_request_timeline`, so the shared synthetic timeline
      // can legitimately be empty here. Re-seed the labelled demo_req_* fixture
      // rows before asserting the panel — this is fixture setup, never real
      // request evidence.
      if (typeof window.__aagDemoSeedRequestTimeline !== 'function') {
        return { ok: false, detail: 'demo timeline seed hook missing' };
      }
      window.__aagDemoSeedRequestTimeline();
      const rows = await window.__TAURI_INTERNALS__.invoke('diagnostics_request_timeline');
      const invoke_len = Array.isArray(rows) ? rows.length : String(typeof rows);
      await api.click('suppliers_trace_refresh');
      // Wait for any terminal state instead of only the table so a backend
      // rejection surfaces as a failed check detail, not a generic timeout.
      const state = await api.wait_for(() => {
        if (api.element('suppliers_trace_table')) return 'table';
        if (api.element('suppliers_trace_error')) return 'error:' + (api.element('suppliers_trace_error')?.textContent ?? '');
        if (api.element('suppliers_trace_empty')) return 'empty';
        return null;
      });
      const failover = api.element('suppliers_trace_failover');
      return {
        ok: state === 'table' && Boolean(failover),
        detail: 'invoke_len=' + String(invoke_len) + ' state=' + String(state).slice(0, 40) +
          ' failover-badge=' + Boolean(failover),
      };
    });

    await check_in_page(cdp, check, 'catalog panel lists cached public sources', async () => {
      const api = window.__aag_supplier_smoke;
      await api.click('suppliers_catalog_load');
      const state = await api.wait_for(() => {
        if (api.element('suppliers_catalog_table')) return 'table';
        if (api.element('suppliers_catalog_unavailable')) return 'unavailable';
        if (api.element('suppliers_catalog_error')) return 'error:' + (api.element('suppliers_catalog_error')?.textContent ?? '');
        return null;
      });
      const body = api.element('suppliers_catalog_table')?.textContent ?? '';
      return {
        ok: state === 'table' && body.includes('models') && body.includes('EvalPlus'),
        detail: 'state=' + String(state).slice(0, 40) + ' sources=' + body.slice(0, 40),
      };
    });

    await check_in_page(cdp, check, 'forced supplier errors surface as a safe banner', async () => {
      const api = window.__aag_supplier_smoke;
      window.__aagDemoSetError('supplier_list');
      try {
        await api.click('suppliers_refresh');
        await api.wait_for(() => api.element('suppliers_error'));
        const text = api.element('suppliers_error')?.textContent ?? '';
        return { ok: text.length > 0 && !text.includes('sk-'), detail: 'error=' + text.slice(0, 40) };
      } finally {
        window.__aagDemoSetError(null);
        // Page functions run in the browser: use an in-page delay, not the
        // host-side `sleep` helper from the run harness.
        await new Promise((resolve) => setTimeout(resolve, 80));
      }
    });
  } finally {
    // Preserve the provider/routing checks under the sole management entry.
    await cdp.eval_value("window.__aag_open_client_management('claude_code')");
  }
}
