export async function run_optimization_checks({ cdp, check_in_page, check }) {
  await check_in_page(cdp, check, 'B1 route explanation content mounts only while expanded', async () => {
    const details = document.querySelector('details.route_explanation');
    const toggle = document.querySelector('[data-testid="route_explanation_toggle"]');
    if (!details || !toggle) return 'missing-route-explanation-control';

    const initially_collapsed = !details.open;
    const initially_unmounted = !details.querySelector('[data-testid="route_explanation_contents"]') &&
      !details.querySelector('table.route_explanation_table');
    if (!initially_collapsed || !initially_unmounted) {
      return 'initially-collapsed=' + initially_collapsed + ' initially-unmounted=' + initially_unmounted;
    }

    toggle.click();
    let expanded = false;
    for (let attempt = 0; attempt < 20; attempt += 1) {
      await new Promise((resolve) => setTimeout(resolve, 40));
      if (details.open && details.querySelector('[data-testid="route_explanation_contents"]')) {
        expanded = true;
        break;
      }
    }
    const table_rendered = Boolean(details.querySelector('table.route_explanation_table'));
    toggle.click();
    let unmounted_after_close = false;
    for (let attempt = 0; attempt < 20; attempt += 1) {
      await new Promise((resolve) => setTimeout(resolve, 40));
      if (!details.open && !details.querySelector('[data-testid="route_explanation_contents"]')) {
        unmounted_after_close = true;
        break;
      }
    }
    return {
      ok: expanded && table_rendered && unmounted_after_close,
      detail: 'expanded=' + expanded + ' table=' + table_rendered + ' unmounted=' + unmounted_after_close,
    };
  });

  await check_in_page(cdp, check, 'B2 route dirty baseline and saved snapshot restart stay distinct', async () => {
    if (window.__aagDemoIsDemo !== true) return 'refusing synthetic route operations outside demo mode';
    const internals = window.__TAURI_INTERNALS__;
    if (!internals || typeof internals.invoke !== 'function') return 'demo IPC bridge is unavailable';
    const invoke_demo = async (command, args) => {
      if (window.__aagDemoIsDemo !== true) throw new Error('refusing synthetic invoke outside demo mode');
      return internals.invoke(command, args);
    };
    const wait_until = async (predicate, attempts = 30, interval_ms = 50) => {
      for (let attempt = 0; attempt < attempts; attempt += 1) {
        if (predicate()) return true;
        await new Promise((resolve) => setTimeout(resolve, interval_ms));
      }
      return predicate();
    };
    const original_fetch = window.fetch;
    const original_invoke = internals.invoke;
    let initial_status = null;
    let original_routes = null;
    let original_route = null;
    let route_fixture_touched = false;
    let route_ui_mutation_attempted = false;

    try {
      if (window.__aagDemoIsDemo !== true) throw new Error('refusing synthetic invoke outside demo mode');
      internals.invoke = (command, args) => {
        if (window.__aagDemoIsDemo !== true) {
          return Promise.reject(new Error('refusing synthetic invoke outside demo mode'));
        }
        return original_invoke(command, args);
      };
      const initial_status_result = await invoke_demo('proxy_status');
      initial_status = structuredClone(initial_status_result);
      if (initial_status.running) return 'expected stopped proxy before isolated snapshot check';
      if (initial_status.config_changed !== false) return 'stopped proxy reported a stale routing snapshot';

      const initial_dirty = Boolean(document.querySelector('.route-dirty'));
      const initial_save_button = document.querySelector('[data-testid="route-save"]');
      if (initial_dirty || !initial_save_button || /[•*]/.test(initial_save_button.textContent ?? '')) {
        return 'route editor did not begin at a clean snapshot';
      }

      original_routes = await invoke_demo('route_list');
      if (!Array.isArray(original_routes) || original_routes.length === 0) return 'missing demo route fixture';
      original_route = structuredClone(original_routes[0]);
      if (!original_route.hops?.length) return 'first demo route has no hop to exercise';

      window.fetch = (resource, options) => {
        const request_url = new URL(String(resource), window.location.href);
        if (request_url.origin === 'http://127.0.0.1:8989' && request_url.pathname === '/health') {
          return Promise.resolve(new Response(JSON.stringify({
            status: 'ok', runtime_kind: 'desktop', providers: [], models: [original_route.alias],
            availableModels: [original_route.alias], lastUsed: null, pinnedProvider: null,
          }), { status: 200, headers: { 'content-type': 'application/json' } }));
        }
        return original_fetch(resource, options);
      };

      const start_button = document.querySelector('[data-testid="gateway-start"]');
      if (!start_button) return 'missing-gateway-start-button';
      start_button.click();
      const started = await wait_until(() =>
        document.querySelector('[data-testid="gateway_status"]')?.classList.contains('running'),
      );
      if (!started) return 'demo-proxy-did-not-start';

      const start_snapshot = await invoke_demo('proxy_status');
      if (!start_snapshot.running || start_snapshot.config_changed !== false) {
        return 'start snapshot was not clean: ' + JSON.stringify(start_snapshot);
      }

      const mutated_route = structuredClone(original_route);
      mutated_route.description = (mutated_route.description ?? '') + ' [smoke fixture mutation]';
      route_fixture_touched = true;
      await invoke_demo('route_save', { route: mutated_route });
      const fixture_mutation_status = await invoke_demo('proxy_status');
      await invoke_demo('route_save', { route: original_route });
      const fixture_revert_status = await invoke_demo('proxy_status');
      if (fixture_mutation_status.config_changed !== true || fixture_revert_status.config_changed !== false) {
        return 'fixture mutation=' + fixture_mutation_status.config_changed + ' revert=' + fixture_revert_status.config_changed;
      }

      const route_selector = '.hop-row[data-route-alias="' + CSS.escape(original_route.alias) + '"][data-hop-idx="0"]';
      const hop_row = document.querySelector(route_selector);
      const provider_select = hop_row?.querySelector('select');
      const select_setter = Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, 'value')?.set;
      if (!provider_select || !select_setter) return 'missing-route-provider-select';
      const original_provider = provider_select.value;
      const alternate_provider = [...provider_select.options].map((option) => option.value)
        .find((value) => value && value !== original_provider);
      if (!alternate_provider) return 'route fixture has no alternate provider option';

      route_ui_mutation_attempted = true;
      select_setter.call(provider_select, alternate_provider);
      provider_select.dispatchEvent(new Event('change', { bubbles: true }));
      const dirty_after_edit = await wait_until(() => Boolean(document.querySelector('.route-dirty')));
      select_setter.call(provider_select, original_provider);
      provider_select.dispatchEvent(new Event('change', { bubbles: true }));
      const clean_after_revert = await wait_until(() => !document.querySelector('.route-dirty'));
      if (!dirty_after_edit || !clean_after_revert) {
        return 'local dirty edit=' + dirty_after_edit + ' reverted clean=' + clean_after_revert;
      }

      const model_input = hop_row.querySelector('.hop-model-cell input');
      const input_setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set;
      if (!model_input || !input_setter) return 'missing-route-model-input';
      const original_model = original_route.hops[0].model;
      const saved_model = original_model + '-smoke-saved';
      input_setter.call(model_input, saved_model);
      model_input.dispatchEvent(new Event('input', { bubbles: true }));
      const dirty_for_save = await wait_until(() => Boolean(document.querySelector('.route-dirty')));
      if (!dirty_for_save) return 'actual route edit did not become dirty';

      document.querySelector('[data-testid="route-save"]')?.click();
      let saved_change_visible = false;
      for (let attempt = 0; attempt < 30; attempt += 1) {
        await new Promise((resolve) => setTimeout(resolve, 50));
        const status = await invoke_demo('proxy_status');
        if (status.config_changed && document.querySelector('[data-testid="gateway_config_changed"]')) {
          saved_change_visible = true;
          break;
        }
      }
      if (!saved_change_visible) return 'saved route edit did not show the stale-snapshot badge';

      const restart_button = document.querySelector('[data-testid="route-restart"]');
      if (!restart_button) return 'missing existing route-toolbar restart button';
      restart_button.click();
      let restart_cleared_badge = false;
      for (let attempt = 0; attempt < 40; attempt += 1) {
        await new Promise((resolve) => setTimeout(resolve, 50));
        const status = await invoke_demo('proxy_status');
        if (status.running && status.config_changed === false &&
          !document.querySelector('[data-testid="gateway_config_changed"]')) {
          restart_cleared_badge = true;
          break;
        }
      }
      if (!restart_cleared_badge) return 'restart did not clear the stale-snapshot badge';

      return {
        ok: true,
        detail: 'fixture mutation=' + fixture_mutation_status.config_changed +
          ' revert=' + fixture_revert_status.config_changed +
          ' local dirty=' + dirty_after_edit + ' local revert=' + clean_after_revert +
          ' saved badge=true restart cleared=true',
      };
    } finally {
      const demo_mode_remains_active = window.__aagDemoIsDemo === true;
      window.fetch = original_fetch;
      try {
        if (!demo_mode_remains_active) throw new Error('demo mode changed during synthetic route check');

        if (initial_status && !initial_status.running) {
          const latest_status = await invoke_demo('proxy_status');
          if (latest_status.running) {
            const stop_button = document.querySelector('[data-testid="gateway-stop"]');
            if (stop_button) stop_button.click();
            else await invoke_demo('proxy_stop');
            let stopped = false;
            for (let attempt = 0; attempt < 30; attempt += 1) {
              if (!(await invoke_demo('proxy_status')).running) {
                stopped = true;
                break;
              }
              await new Promise((resolve) => setTimeout(resolve, 50));
            }
            if (!stopped) {
              await invoke_demo('proxy_stop');
              if ((await invoke_demo('proxy_status')).running) throw new Error('could not stop synthetic proxy during cleanup');
            }
          }
        }

        if (route_fixture_touched && original_route) {
          await invoke_demo('route_save', { route: original_route });
        }
        if (route_ui_mutation_attempted && original_route) {
          const row_selector = '.hop-row[data-route-alias="' + CSS.escape(original_route.alias) + '"][data-hop-idx="0"]';
          const current_model_input = document.querySelector(row_selector + ' .hop-model-cell input');
          const current_model_setter = current_model_input && Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set;
          if (current_model_input && current_model_setter) {
            current_model_setter.call(current_model_input, current_model_input.value + '-smoke-reset');
            current_model_input.dispatchEvent(new Event('input', { bubbles: true }));
            const discard_ready = await wait_until(() => Boolean(document.querySelector('[data-testid="route-discard"]')));
            if (discard_ready) {
              document.querySelector('[data-testid="route-discard"]')?.click();
              const route_reloaded = await wait_until(() =>
                !document.querySelector('.route-dirty') &&
                document.querySelector(row_selector + ' .hop-model-cell input')?.value === original_route.hops[0].model,
              );
              if (!route_reloaded) throw new Error('could not reload original route fixture during cleanup');
            } else {
              throw new Error('could not stage route discard during cleanup');
            }
          } else {
            throw new Error('could not restore route editor state during cleanup');
          }
        }

        const final_status = await invoke_demo('proxy_status');
        const final_routes = await invoke_demo('route_list');
        if (initial_status && (final_status.running !== initial_status.running ||
          final_status.config_changed !== initial_status.config_changed ||
          JSON.stringify(final_status.config) !== JSON.stringify(initial_status.config))) {
          throw new Error('proxy fixture state was not restored');
        }
        if (original_routes && JSON.stringify(final_routes) !== JSON.stringify(original_routes)) {
          throw new Error('route fixture state was not restored');
        }
      } finally {
        internals.invoke = original_invoke;
      }
    }
  });

  await check_in_page(cdp, check, 'A6 JSONC validation blocks a stale save and clears debounce paths', async () => {
    if (window.__aagDemoIsDemo !== true) return 'refusing synthetic config editor check outside demo mode';
    const internals = window.__TAURI_INTERNALS__;
    if (!internals || typeof internals.invoke !== 'function') return 'demo IPC bridge is unavailable';
    const original_invoke = internals.invoke;
    const wait_until = async (predicate, attempts = 24, interval_ms = 50) => {
      for (let attempt = 0; attempt < attempts; attempt += 1) {
        if (predicate()) return true;
        await new Promise((resolve) => setTimeout(resolve, interval_ms));
      }
      return predicate();
    };
    let save_client_config_calls = 0;

    try {
      if (window.__aagDemoIsDemo !== true) throw new Error('refusing config invoke wrapper outside demo mode');
      internals.invoke = (command, args) => {
        if (window.__aagDemoIsDemo !== true) {
          return Promise.reject(new Error('refusing config invoke outside demo mode'));
        }
        if (command === 'client_config_save') {
          save_client_config_calls += 1;
          return Promise.reject(new Error('synthetic smoke guard blocked config save'));
        }
        return original_invoke(command, args);
      };
      const kilo_tab = document.querySelector('[data-testid="client-settings-tab-kilo_cli"]');
      if (!kilo_tab) return 'missing-kilo-tab';
      kilo_tab.click();
      if (!await wait_until(() => document.querySelector('.page-header h2')?.textContent === 'Kilo CLI')) {
        return 'kilo-page-did-not-open';
      }
      const edit_button_ready = await wait_until(() => Boolean(document.querySelector('[data-testid="client-config-edit"]')));
      if (!edit_button_ready) return 'missing-client-config-edit-button';
      document.querySelector('[data-testid="client-config-edit"]')?.click();
      if (!await wait_until(() => Boolean(document.querySelector('[data-testid="config-editor-text"]')))) {
        return 'config-editor-did-not-open';
      }

      const modal = document.querySelector('[data-testid="config-editor-modal"]');
      const textarea = modal?.querySelector('[data-testid="config-editor-text"]');
      const save_button = modal?.querySelector('[data-testid="config-editor-save"]');
      if (!modal || !textarea || !save_button) return 'missing-config-editor-controls';
      const started_valid = !modal.querySelector('[data-testid="config-editor-invalid"]') && !save_button.disabled;
      if (!started_valid) return 'config editor did not start with valid enabled content';

      const textarea_setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')?.set;
      if (!textarea_setter) return 'missing-native-textarea-value-setter';
      textarea_setter.call(textarea, '{"provider":');
      textarea.dispatchEvent(new Event('input', { bubbles: true }));
      await new Promise((resolve) => setTimeout(resolve, 40));
      save_button.click();
      await new Promise((resolve) => setTimeout(resolve, 40));
      const invalid_save_blocked = save_client_config_calls === 0 &&
        Boolean(document.querySelector('[data-testid="config-editor-modal"]'));
      if (!invalid_save_blocked) return 'invalid pre-debounce edit invoked save_client_config';

      const corrected = '{"provider":{},"model":"smoke/corrected"}\n';
      const active_textarea = document.querySelector('[data-testid="config-editor-text"]');
      textarea_setter.call(active_textarea, corrected);
      active_textarea.dispatchEvent(new Event('input', { bubbles: true }));
      await new Promise((resolve) => setTimeout(resolve, 350));
      const correction_valid = !document.querySelector('[data-testid="config-editor-invalid"]') &&
        !document.querySelector('[data-testid="config-editor-save"]')?.disabled;
      if (!correction_valid) return 'corrected JSON did not clear debounced validation';

      const format_button = document.querySelector('[data-testid="config-editor-modal"] .modal-actions button:first-child');
      if (!format_button || format_button.disabled) return 'format control was not enabled after correction';
      format_button.click();
      await new Promise((resolve) => setTimeout(resolve, 80));
      const formatted_text = document.querySelector('[data-testid="config-editor-text"]')?.value ?? '';
      const formatted = formatted_text.includes('\n  "provider"');
      if (!formatted) return 'format control did not format corrected JSON';

      const cancel_textarea = document.querySelector('[data-testid="config-editor-text"]');
      textarea_setter.call(cancel_textarea, '{"provider":{},"model":"smoke/cancel"}\n');
      cancel_textarea.dispatchEvent(new Event('input', { bubbles: true }));
      await new Promise((resolve) => setTimeout(resolve, 35));
      document.querySelector('[data-testid="config_editor_cancel"]')?.click();
      const cancelled = await wait_until(() => !document.querySelector('[data-testid="config-editor-modal"]'));
      await new Promise((resolve) => setTimeout(resolve, 350));
      if (!cancelled) return 'cancel did not close the pending-validation editor';

      document.querySelector('[data-testid="client-config-edit"]')?.click();
      if (!await wait_until(() => Boolean(document.querySelector('[data-testid="config-editor-text"]')))) {
        return 'config editor did not reopen for unmount test';
      }
      const unmount_textarea = document.querySelector('[data-testid="config-editor-text"]');
      textarea_setter.call(unmount_textarea, '{"provider":');
      unmount_textarea.dispatchEvent(new Event('input', { bubbles: true }));
      await new Promise((resolve) => setTimeout(resolve, 35));
      const dsh_tab = document.querySelector('[data-testid="dsh_nav"]');
      if (!dsh_tab) return 'missing-navigation target for editor unmount';
      dsh_tab.click();
      const unmounted = await wait_until(() =>
        dsh_tab.classList.contains('active') && !document.querySelector('[data-testid="config-editor-modal"]'),
      );
      await new Promise((resolve) => setTimeout(resolve, 350));
      if (!unmounted) return 'leaving Kilo did not unmount the editor and pending timer';
      return {
        ok: save_client_config_calls === 0,
        detail: 'invalid save calls=' + save_client_config_calls +
          ' corrected=' + correction_valid + ' formatted=' + formatted +
          ' cancelled=' + cancelled + ' unmounted=' + unmounted,
      };
    } finally {
      const demo_mode_remains_active = window.__aagDemoIsDemo === true;
      try {
        if (!demo_mode_remains_active) throw new Error('demo mode changed during config editor check');
        document.querySelector('[data-testid="config_editor_cancel"]')?.click();
        await window.__aag_open_client_management('claude_code');
        await wait_until(() => document.querySelector('.client_management_body .page-header h2')?.textContent === 'Claude');
      } finally {
        internals.invoke = original_invoke;
      }
    }
  });

  await check_in_page(cdp, check, 'B3 diagnostics timeline refresh failure keeps rows and shows the error banner', async () => {
    if (window.__aagDemoIsDemo !== true) return 'refusing synthetic diagnostics invoke outside demo mode';
    const internals = window.__TAURI_INTERNALS__;
    if (!internals || typeof internals.invoke !== 'function') return 'demo IPC bridge is unavailable';
    const original_invoke = internals.invoke;
    const synthetic_event = {
      requestId: 'smoke_metadata_only_request',
      surface: 'chat_completions',
      modelRef: 'ref:smoke_model',
      timestampMs: Date.now(),
      status: 200,
      durationMs: 17,
      streamed: false,
      failedOver: null,
    };
    const wait_until = async (predicate, attempts = 30, interval_ms = 50) => {
      for (let attempt = 0; attempt < attempts; attempt += 1) {
        if (predicate()) return true;
        await new Promise((resolve) => setTimeout(resolve, interval_ms));
      }
      return predicate();
    };
    let provide_fixture_next = false;
    let reject_next_request = false;
    try {
      if (window.__aagDemoIsDemo !== true) throw new Error('refusing diagnostics invoke wrapper outside demo mode');
      internals.invoke = (command, args) => {
        if (window.__aagDemoIsDemo !== true) {
          return Promise.reject(new Error('refusing synthetic invoke outside demo mode'));
        }
        if (command === 'diagnostics_request_timeline') {
          if (reject_next_request) {
            reject_next_request = false;
            return Promise.reject(new Error('synthetic_timeline_read_failed'));
          }
          return Promise.resolve(provide_fixture_next ? [synthetic_event] : []);
        }
        return original_invoke(command, args);
      };

      document.querySelector('[data-testid="diagnostics_open"]')?.click();
      const opened_empty = await wait_until(() =>
        Boolean(document.querySelector('.diagnostics-modal')) &&
        Boolean(document.querySelector('[data-testid="timeline_empty"]')),
      );
      if (!opened_empty) return 'diagnostics timeline did not open in its empty state';

      provide_fixture_next = true;
      document.querySelector('[data-testid="timeline_refresh"]')?.click();
      const row_loaded = await wait_until(() =>
        document.querySelector('.diagnostics_timeline_item')?.getAttribute('title') === synthetic_event.requestId,
      );
      if (!row_loaded) return 'fixture row did not load before the forced failure';

      reject_next_request = true;
      document.querySelector('[data-testid="timeline_refresh"]')?.click();
      const banner_shown = await wait_until(() =>
        [...document.querySelectorAll('.diagnostics_timeline_section .notice-banner[role="alert"]')]
          .some((banner) => (banner.textContent ?? '').includes('synthetic_timeline_read_failed')),
      );
      const row_retained = document.querySelector('.diagnostics_timeline_item')?.getAttribute('title') === synthetic_event.requestId;
      if (!banner_shown) return 'refresh failure did not surface the error banner';
      if (!row_retained) return 'refresh failure dropped the loaded rows';

      provide_fixture_next = true;
      document.querySelector('[data-testid="timeline_refresh"]')?.click();
      const recovered = await wait_until(() =>
        ![...document.querySelectorAll('.diagnostics_timeline_section .notice-banner[role="alert"]')].length &&
        document.querySelector('.diagnostics_timeline_item')?.getAttribute('title') === synthetic_event.requestId,
      );
      return {
        ok: recovered,
        detail: 'banner=' + banner_shown + ' rows_retained=' + row_retained + ' recovered=' + recovered,
      };
    } finally {
      const demo_mode_remains_active = window.__aagDemoIsDemo === true;
      internals.invoke = original_invoke;
      document.querySelector('[data-testid="diagnostics_backdrop"]')?.dispatchEvent(
        new MouseEvent('click', { bubbles: true }),
      );
      await wait_until(() => !document.querySelector('.diagnostics-modal'));
      if (!demo_mode_remains_active) throw new Error('demo mode changed during timeline failure check');
    }
  });

  await check_in_page(cdp, check, 'B3 diagnostics refresh and clear reject a delayed stale response', async () => {
    if (window.__aagDemoIsDemo !== true) return 'refusing synthetic diagnostics invoke outside demo mode';
    const internals = window.__TAURI_INTERNALS__;
    if (!internals || typeof internals.invoke !== 'function') return 'demo IPC bridge is unavailable';
    const original_invoke = internals.invoke;
    const synthetic_event = {
      requestId: 'smoke_metadata_only_request',
      surface: 'chat_completions',
      modelRef: 'ref:smoke_model',
      timestampMs: Date.now(),
      status: 200,
      durationMs: 17,
      streamed: false,
      failedOver: null,
    };
    const wait_until = async (predicate, attempts = 24, interval_ms = 50) => {
      for (let attempt = 0; attempt < attempts; attempt += 1) {
        if (predicate()) return true;
        await new Promise((resolve) => setTimeout(resolve, interval_ms));
      }
      return predicate();
    };
    let provide_fixture_next = false;
    let delay_next_request = false;
    let delayed_refresh_resolve = null;

    try {
      if (window.__aagDemoIsDemo !== true) throw new Error('refusing diagnostics invoke wrapper outside demo mode');
      internals.invoke = (command, args) => {
        if (window.__aagDemoIsDemo !== true) {
          return Promise.reject(new Error('refusing diagnostics invoke outside demo mode'));
        }
        if (command === 'diagnostics_request_timeline') {
          if (delay_next_request) {
            delay_next_request = false;
            return new Promise((resolve) => { delayed_refresh_resolve = resolve; });
          }
          if (provide_fixture_next) {
            provide_fixture_next = false;
            return Promise.resolve([synthetic_event]);
          }
          return Promise.resolve([]);
        }
        return original_invoke(command, args);
      };

      document.querySelector('[data-testid="diagnostics_open"]')?.click();
      const opened_empty = await wait_until(() =>
        Boolean(document.querySelector('.diagnostics-modal')) &&
        Boolean(document.querySelector('[data-testid="timeline_empty"]')),
      );
      if (!opened_empty) return 'diagnostics timeline did not open in its empty state';

      const clear_button = document.querySelector('[data-testid="timeline_clear"]');
      if (!clear_button || clear_button.disabled) return 'clear control was disabled for the loaded empty timeline';
      clear_button.click();
      const cleared_empty = await wait_until(() =>
        Boolean(document.querySelector('[data-testid="timeline_empty"]')) &&
        !document.querySelector('[data-testid="timeline_clear"]')?.disabled,
      );
      if (!cleared_empty) return 'clearing an empty timeline did not retain the empty state';

      document.querySelector('[data-testid="timeline_refresh"]')?.click();
      const refreshed_empty = await wait_until(() =>
        Boolean(document.querySelector('[data-testid="timeline_empty"]')) &&
        !document.querySelector('[data-testid="timeline_clear"]')?.disabled,
      );
      if (!refreshed_empty) return 'refreshing an empty timeline did not retain the empty state';

      provide_fixture_next = true;
      document.querySelector('[data-testid="timeline_refresh"]')?.click();
      const fixture_visible = await wait_until(() =>
        document.querySelector('.diagnostics_timeline_item')?.getAttribute('title') === synthetic_event.requestId,
      );
      if (!fixture_visible) return 'metadata-only synthetic timeline fixture did not render';

      delay_next_request = true;
      document.querySelector('[data-testid="timeline_refresh"]')?.click();
      const delayed_started = await wait_until(() => typeof delayed_refresh_resolve === 'function', 12, 25);
      if (!delayed_started) return 'refresh did not start the delayed synthetic request';

      const active_clear_button = document.querySelector('[data-testid="timeline_clear"]');
      if (!active_clear_button || active_clear_button.disabled) return 'clear control was unavailable during delayed refresh';
      active_clear_button.click();
      const cleared_with_delayed_pending = await wait_until(() =>
        Boolean(document.querySelector('[data-testid="timeline_empty"]')) &&
        !document.querySelector('[data-testid="timeline_clear"]')?.disabled,
      );
      if (!cleared_with_delayed_pending) return 'clear did not show the empty state before the delayed response';

      delayed_refresh_resolve([synthetic_event]);
      delayed_refresh_resolve = null;
      await new Promise((resolve) => setTimeout(resolve, 100));
      const empty_after_stale_response = Boolean(document.querySelector('[data-testid="timeline_empty"]')) &&
        !document.querySelector('.diagnostics_timeline_item');
      return {
        ok: cleared_empty && refreshed_empty && fixture_visible && cleared_with_delayed_pending && empty_after_stale_response,
        detail: 'empty clear=' + cleared_empty + ' empty refresh=' + refreshed_empty + ' fixture=' + fixture_visible +
          ' clear before late response=' + cleared_with_delayed_pending +
          ' empty after stale response=' + empty_after_stale_response,
      };
    } finally {
      const demo_mode_remains_active = window.__aagDemoIsDemo === true;
      if (delayed_refresh_resolve) delayed_refresh_resolve([synthetic_event]);
      internals.invoke = original_invoke;
      document.querySelector('[data-testid="diagnostics_backdrop"]')?.dispatchEvent(
        new MouseEvent('click', { bubbles: true }),
      );
      await wait_until(() => !document.querySelector('.diagnostics-modal'));
      if (!demo_mode_remains_active) throw new Error('demo mode changed during diagnostics timeline check');
    }
  });

  // Measurement evidence for the deferred PROGRESS item ("per-row rendering
  // and bundle improvements have measurement evidence"): render the full
  // 200-entry timeline ring (REQUEST_TIMELINE_CAPACITY) and record the
  // median refresh-to-rows-painted time. The 1000 ms budget is a regression
  // gate, not a target; the measured median prints in the check label.
  const full_ring_probe = async () => {
    if (window.__aagDemoIsDemo !== true) return { ok: false, reason: 'demo-mode-required' };
    const internals = window.__TAURI_INTERNALS__;
    if (!internals || typeof internals.invoke !== 'function') return { ok: false, reason: 'demo-ipc-unavailable' };
    const original_invoke = internals.invoke;
    const row_count = 200;
    const render_budget_ms = 1000;
    const wait_until = async (predicate, attempts = 100, interval_ms = 10) => {
      for (let attempt = 0; attempt < attempts; attempt += 1) {
        if (predicate()) return true;
        await new Promise((resolve) => setTimeout(resolve, interval_ms));
      };
      return predicate();
    };
    const full_events = [];
    for (let i = 0; i < row_count; i += 1) {
      const hex_index = i.toString(16).padStart(8, '0');
      full_events.push({
        requestId: 'req_' + hex_index + '_' + hex_index.slice(0, 4).padEnd(12, '0'),
        surface: i % 2 === 0 ? 'chat_completions' : 'messages',
        modelRef: 'ref:smoke_model_' + (i % 4),
        timestampMs: 1771000000000 + i * 1000,
        status: i % 7 === 0 ? 429 : 200,
        durationMs: 5 + (i % 90),
        streamed: i % 3 !== 0,
        failedOver: i % 5 === 0 ? true : (i % 5 === 1 ? null : false),
      });
    }
    let provide_full_next = false;
    try {
      internals.invoke = (command, args) => {
        if (window.__aagDemoIsDemo !== true) {
          return Promise.reject(new Error('refusing synthetic invoke outside demo mode'));
        }
        if (command === 'diagnostics_request_timeline') {
          return Promise.resolve(provide_full_next ? structuredClone(full_events) : []);
        }
        return original_invoke(command, args);
      };

      document.querySelector('[data-testid="diagnostics_open"]')?.click();
      const opened_empty = await wait_until(() =>
        Boolean(document.querySelector('.diagnostics-modal')) &&
        Boolean(document.querySelector('[data-testid="timeline_empty"]')),
      );
      if (!opened_empty) return { ok: false, reason: 'modal-did-not-open-empty' };

      const samples_ms = [];
      for (let iteration = 0; iteration < 3; iteration += 1) {
        provide_full_next = true;
        const started_at = performance.now();
        document.querySelector('[data-testid="timeline_refresh"]')?.click();
        const rendered = await wait_until(() =>
          document.querySelectorAll('.diagnostics_timeline_item').length === row_count,
        );
        samples_ms.push(performance.now() - started_at);
        if (!rendered) return { ok: false, reason: 'rows-missing', iteration, rows: document.querySelectorAll('.diagnostics_timeline_item').length };
      }
      const sorted = [...samples_ms].sort((a, b) => a - b);
      const median_ms = sorted[1];
      const first_row_title = document.querySelector('.diagnostics_timeline_item')?.getAttribute('title') ?? '';
      return {
        ok: median_ms <= render_budget_ms && Boolean(first_row_title),
        median_ms: Math.round(median_ms * 10) / 10,
        samples_ms: samples_ms.map((ms) => Math.round(ms * 10) / 10),
        rows: document.querySelectorAll('.diagnostics_timeline_item').length,
        budget_ms: render_budget_ms,
      };
    } finally {
      const demo_mode_remains_active = window.__aagDemoIsDemo === true;
      internals.invoke = original_invoke;
      document.querySelector('[data-testid="diagnostics_backdrop"]')?.dispatchEvent(
        new MouseEvent('click', { bubbles: true }),
      );
      await wait_until(() => !document.querySelector('.diagnostics-modal'));
      if (!demo_mode_remains_active) throw new Error('demo mode changed during timeline render measurement');
    }
  };
  const full_ring_envelope = await cdp.eval(`(${full_ring_probe.toString()})()`);
  const full_ring_result = full_ring_envelope?.value;
  const full_ring_median = full_ring_result && Number.isFinite(full_ring_result.median_ms)
    ? full_ring_result.median_ms
    : '?';
  const full_ring_rows = full_ring_result && Number.isFinite(full_ring_result.rows) ? full_ring_result.rows : '?';
  check(
    'B3 full-ring timeline renders 200 rows median=' + full_ring_median + 'ms rows=' + full_ring_rows +
    ' (budget 1000ms)',
    Boolean(full_ring_result?.ok) && !full_ring_envelope.__async_error,
    JSON.stringify(full_ring_result ?? full_ring_envelope).slice(0, 240),
  );
}
