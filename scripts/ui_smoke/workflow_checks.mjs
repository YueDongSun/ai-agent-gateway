/** Synthetic workflow UI evidence only; no host capability is probed. */
export async function run_workflow_checks({ cdp, check_in_page, check }) {
  await cdp.eval_value(`(() => {
    if (window.__aagDemoIsDemo !== true) throw new Error('Workflow smoke requires synthetic IPC');
    const bridge = window.__TAURI_INTERNALS__;
    const original = bridge.invoke;
    const calls = [];
    const all_calls = [];
    bridge.invoke = (command, args) => {
      all_calls.push({ command, args });
      if (command.startsWith('workflow_')) calls.push({ command, args });
      return original(command, args);
    };
    const element = (id) => document.querySelector('[data-testid="' + id + '"]');
    const wait_for = async (predicate) => {
      const deadline = Date.now() + 4000;
      while (Date.now() < deadline) {
        const value = predicate();
        if (value) return value;
        await new Promise((resolve) => setTimeout(resolve, 40));
      }
      throw new Error('Workflow UI condition timed out');
    };
    const click = async (id) => {
      const target = await wait_for(() => { const item = element(id); return item && !item.disabled ? item : null; });
      target.click();
    };
    const input = async (id, value) => {
      const target = element(id);
      if (!target) throw new Error('Workflow input is missing: ' + id);
      const prototype = target instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype :
        target instanceof HTMLSelectElement ? HTMLSelectElement.prototype : HTMLInputElement.prototype;
      Object.getOwnPropertyDescriptor(prototype, 'value').set.call(target, value);
      target.dispatchEvent(new Event(target instanceof HTMLSelectElement ? 'change' : 'input', { bubbles: true }));
      await new Promise((resolve) => setTimeout(resolve, 70));
    };
    window.__aag_workflow_smoke = { element, wait_for, click, input, calls, all_calls,
      count: (command) => calls.filter((call) => call.command === command).length,
      restore: () => { bridge.invoke = original; } };
    return true;
  })()`);
  try {
    await check_in_page(cdp, check, 'Workflow demo shows separate unknown stages and synthetic boundary', async () => {
      const api = window.__aag_workflow_smoke;
      await api.click('mcp_nav');
      await api.wait_for(() => api.element('workflow_capability_demo_local_docs') && api.element('workflow_demo_notice'));
      const detail = api.element('workflow_capability_detail');
      const installed = api.element('workflow_stage_installed')?.textContent ?? '';
      const discovered = api.element('workflow_stage_discovered')?.textContent ?? '';
      const callable = api.element('workflow_stage_callable')?.textContent ?? '';
      const notice = api.element('workflow_demo_notice').textContent;
      const read_only = api.calls.every((call) => ['workflow_list_capabilities', 'workflow_get_health'].includes(call.command));
      return { ok: Boolean(detail) && /Unknown/.test(installed) && /synthetic/i.test(discovered) &&
        /Unknown/.test(callable) && /no host|do not describe host/i.test(notice) && read_only,
      detail: 'separate-stages=true synthetic-notice=true read-only=' + read_only };
    });
    await check_in_page(cdp, check, 'Workflow kind, group, and search filters select current records', async () => {
      const api = window.__aag_workflow_smoke;
      await api.input('workflow_kind_filter', 'mcp');
      const mcp_only = Boolean(api.element('workflow_capability_demo_calendar_bridge')) &&
        !api.element('workflow_capability_demo_local_docs') && !api.element('workflow_capability_demo_task_router');
      await api.input('workflow_kind_filter', 'all');
      await api.input('workflow_group_filter', 'Research');
      const research_only = Boolean(api.element('workflow_capability_demo_local_docs')) &&
        !api.element('workflow_capability_demo_calendar_bridge');
      await api.input('workflow_group_filter', 'all');
      await api.input('workflow_search', 'calendar');
      const search_only = Boolean(api.element('workflow_capability_demo_calendar_bridge')) &&
        !api.element('workflow_capability_demo_local_docs');
      await api.input('workflow_search', '');
      return { ok: mcp_only && research_only && search_only,
        detail: 'kind=' + mcp_only + ' group=' + research_only + ' search=' + search_only };
    });
    await check_in_page(cdp, check, 'Workflow host-only metadata offers no probe and supported demo probe remains synthetic', async () => {
      const api = window.__aag_workflow_smoke;
      await api.click('workflow_capability_demo_calendar_bridge');
      const host_only = api.element('workflow_capability_detail')?.textContent.includes('External host') &&
        !api.element('workflow_probe');
      const before = api.count('workflow_probe_capability');
      const installed_before = api.element('workflow_stage_installed')?.textContent ?? '';
      await api.click('workflow_capability_demo_local_docs');
      await api.click('workflow_probe');
      await api.wait_for(() => /Demo probe result; no host command ran/.test(api.element('workflow_stage_probe')?.textContent ?? ''));
      const installed_after = api.element('workflow_stage_installed')?.textContent ?? '';
      const probe = api.calls.filter((call) => call.command === 'workflow_probe_capability').at(-1);
      const installed_refreshed = /Unknown/.test(installed_before) && /Synthetic/.test(installed_after) &&
        /synthetic installed observation/i.test(installed_after);
      return { ok: host_only && installed_refreshed && api.count('workflow_probe_capability') === before + 1 &&
        probe?.args?.capability_id === 'demo_local_docs' &&
        /synthetic/i.test(api.element('workflow_stage_probe').textContent),
      detail: 'host-probe-absent=' + host_only + ' installed-unknown-to-synthetic=' + installed_refreshed + ' demo-probe-only=true' };
    });
    await check_in_page(cdp, check, 'Workflow deterministic preview and optional advice never dispatch', async () => {
      const api = window.__aag_workflow_smoke;
      const before = api.all_calls.length;
      await api.input('workflow_task_input', 'Summarize a local document');
      const consent = api.element('workflow_external_advice_confirmed');
      if (!consent || consent.checked) return 'external-advice-consent-not-default-off';
      await api.click('workflow_route_preview');
      await api.wait_for(() => api.element('workflow_route_result')?.textContent.includes('demo_route_preview'));
      const plain = api.element('workflow_route_result').textContent;
      const no_advice = !api.element('workflow_model_advice') && /not requested/i.test(plain);
      const submitted = api.element('workflow_route_submitted_task')?.textContent.includes('Summarize a local document');
      await api.click('workflow_route_advice');
      await api.wait_for(() => /tool restriction unverified/i.test(api.element('workflow_model_advice')?.textContent ?? ''));
      const no_consent_call = api.calls.filter((call) => call.command === 'workflow_evaluate_route').at(-1);
      const native_unavailable = /native_codex_subscription/.test(api.element('workflow_model_advice').textContent) &&
        /Unavailable/.test(api.element('workflow_model_advice').textContent) && !api.element('workflow_jev_evidence');
      await api.click('workflow_external_advice_confirmed');
      await api.click('workflow_route_advice');
      await api.wait_for(() => api.element('workflow_jev_evidence')?.textContent.includes('demo_advice_model'));
      const advised = api.element('workflow_route_result').textContent;
      const consent_call = api.calls.filter((call) => call.command === 'workflow_evaluate_route').at(-1);
      const commands = api.calls.map((call) => call.command);
      const no_dispatch = api.all_calls.slice(before).every(({ command }) =>
        !/^(agent_|worker_|task_|workflow_dispatch|workflow_execute|gateway_start)/.test(command));
      return { ok: no_advice && submitted && native_unavailable &&
        no_consent_call?.args?.external_advice_confirmed === false &&
        consent_call?.args?.external_advice_confirmed === true &&
        /demo_route_preview/.test(advised) && /demo_advice_model/.test(advised) &&
        /does not change.*dispatch work/i.test(advised) && no_dispatch && api.count('workflow_route_preview') === 1 &&
        api.count('workflow_evaluate_route') === 2 && commands.every((command) =>
          ['workflow_list_capabilities', 'workflow_get_health', 'workflow_probe_capability', 'workflow_route_preview', 'workflow_evaluate_route'].includes(command)),
      detail: 'submitted-task=' + submitted + ' default-no-consent=' + native_unavailable +
        ' synthetic-jev-after-consent=true no-dispatch=' + no_dispatch };
    });
    await check_in_page(cdp, check, 'Workflow refresh error retains records without a success claim', async () => {
      const api = window.__aag_workflow_smoke;
      const before = api.element('workflow_capabilities')?.querySelectorAll('[role="option"]').length;
      window.__aagDemoSetError('workflow_list_capabilities');
      try {
        await api.click('workflow_refresh');
        await api.wait_for(() => api.element('workflow_error'));
        const message = api.element('workflow_error').textContent;
        const after = api.element('workflow_capabilities')?.querySelectorAll('[role="option"]').length;
        return { ok: before === 3 && after === before && /No success was confirmed/.test(message) &&
          Boolean(api.element('workflow_capability_demo_local_docs')),
        detail: 'records-retained=' + (after === before) + ' failure-visible=true' };
      } finally {
        window.__aagDemoSetError(null);
      }
    });
  } finally {
    await cdp.eval_value(`(async () => {
      window.__aagDemoSetError(null);
      window.__aag_workflow_smoke?.restore(); delete window.__aag_workflow_smoke;
      await window.__aag_open_client_management('claude_code');
      return true;
    })()`);
    await cdp.eval_value(`(async () => { for (let attempt = 0; attempt < 40; attempt++) {
      if (document.querySelector('[data-testid="gateway-start"]')) return true;
      await new Promise((resolve) => setTimeout(resolve, 50));
    } throw new Error('Claude page not restored after workflow checks'); })()`);
  }
}
