import { pathToFileURL } from 'node:url';
import { attach_page } from './cdp_client.mjs';
import { check_in_page } from './page_checks.mjs';
import { create_smoke_runtime, find_browser, sleep } from './smoke_runtime.mjs';

export async function run_prompt_workspace_checks({ cdp, check }) {
  await cdp.eval_value(`(() => {
    if (window.__aagDemoIsDemo !== true) throw new Error('workspace checks require synthetic IPC');
    const storage = Object.entries(localStorage);
    localStorage.removeItem('aag.prompt.workspaces.demo');
    const element = (id) => document.querySelector('[data-testid="' + id + '"]');
    const wait_for = async (read) => {
      for (let attempt = 0; attempt < 100; attempt++) { const value = read(); if (value) return value; await new Promise((resolve) => setTimeout(resolve, 40)); }
      throw new Error('workspace UI condition timed out');
    };
    const click = async (id) => { const button = await wait_for(() => { const node = element(id); return node && !node.disabled ? node : null; }); button.click(); await new Promise((resolve) => setTimeout(resolve, 60)); };
    const set_text = async (id, value) => {
      const input = await wait_for(() => element(id));
      const prototype = input instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
      Object.getOwnPropertyDescriptor(prototype, 'value').set.call(input, value);
      input.dispatchEvent(new Event('input', {bubbles:true})); await new Promise((resolve) => setTimeout(resolve, 60));
    };
    const select = async (id, value) => { const input = element(id); Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, 'value').set.call(input,value); input.dispatchEvent(new Event('change',{bubbles:true})); await new Promise((resolve) => setTimeout(resolve, 80)); };
    const original = window.__TAURI_INTERNALS__.invoke;
    const state = { storage, element, wait_for, click, set_text, select, original, calls: [], hold_zh: false, release: null, fail_preview: false };
    window.__TAURI_INTERNALS__.invoke = (command, args) => {
      if (command === 'prompt_workspace_preview' && state.fail_preview) return Promise.reject(new Error('prompt_workspace_path_unsafe'));
      if (command === 'prompt_enhance_run') {
        state.calls.push({stage:args.request.stage, context:structuredClone(args.request.workspaceContext)});
        if (state.hold_zh && args.request.stage === 'zh_expand') return Promise.resolve(original(command,args)).then((result) => new Promise((resolve) => { state.release = () => resolve(result); }));
      }
      return original(command,args);
    };
    window.__aag_prompt_workspace_test = state;
    element('prompt_nav').click();
    return true;
  })()`);
  try {
    await check_in_page(cdp, check, 'prompt workspace manager defaults to no file context', async () => {
      const api = window.__aag_prompt_workspace_test;
      await api.wait_for(() => api.element('prompt_workspace_select') && !api.element('prompt_workspace_select').disabled);
      return { ok: !api.element('prompt_workspace_use_context').checked && api.element('prompt_workspace_context_state').textContent.includes('Off') && !api.element('prompt_workspace_file_states') };
    });
    await check_in_page(cdp, check, 'workspace entries create, persist and select through visible controls', async () => {
      const api = window.__aag_prompt_workspace_test;
      await api.set_text('prompt_workspace_name','Team Alpha');
      await api.set_text('prompt_workspace_root','/example_project/alpha');
      await api.click('prompt_workspace_save');
      await api.wait_for(() => [...api.element('prompt_workspace_select').options].some((option) => option.textContent === 'Team Alpha'));
      const a = api.element('prompt_workspace_select').value;
      await api.click('prompt_workspace_add');
      await api.set_text('prompt_workspace_name','Team Beta');
      await api.set_text('prompt_workspace_root','/example_project/beta');
      await api.click('prompt_workspace_save');
      await api.wait_for(() => [...api.element('prompt_workspace_select').options].some((option) => option.textContent === 'Team Beta'));
      const b = api.element('prompt_workspace_select').value;
      api.first = a; api.second = b;
      await api.select('prompt_workspace_select',a);
      await api.wait_for(() => api.element('prompt_workspace_select').value === a && !api.element('prompt_workspace_select').disabled);
      return { ok: a !== b && api.element('prompt_workspace_path').textContent.includes('/alpha') && !api.element('prompt_workspace_use_context').checked };
    });
    await check_in_page(cdp, check, 'both stages retain opted-in files from the original workspace when global selection changes', async () => {
      const api = window.__aag_prompt_workspace_test;
      await api.click('prompt_workspace_use_context');
      await api.wait_for(() => api.element('prompt_workspace_file_states')?.textContent.includes('Ready'));
      await api.click('prompt_workspace_read_claude');
      await api.wait_for(() => api.element('prompt_workspace_file_states')?.querySelectorAll('li').length === 1);
      await api.set_text('prompt_input','Implement a workspace-aware task');
      api.hold_zh = true;
      await api.click('prompt_run');
      await api.wait_for(() => api.release);
      const registry = await window.__TAURI_INTERNALS__.invoke('prompt_workspace_list');
      await window.__TAURI_INTERNALS__.invoke('prompt_workspace_select',{id:api.second,expectedRevision:registry.revision});
      api.release(); api.hold_zh = false;
      await api.wait_for(() => api.calls.length >= 2 && api.element('prompt_output')?.value && !api.element('prompt_run').disabled);
      const calls = api.calls.slice(-2);
      return { ok: calls[0].stage === 'zh_expand' && calls[1].stage === 'en_optimize' && calls.every((call) => call.context.workspaceId === api.first && call.context.readAgents && !call.context.readClaude) && calls[0].context.expectedContextVersion === calls[1].context.expectedContextVersion && api.element('prompt_workspace_receipt').textContent.includes('Team Alpha') && !api.element('prompt_workspace_use_context').checked };
    });
    await check_in_page(cdp, check, 'next task resets context and no previous workspace bleeds into the request', async () => {
      const api = window.__aag_prompt_workspace_test;
      await api.click('prompt_workspace_refresh');
      await api.wait_for(() => api.element('prompt_workspace_select').value === api.second && !api.element('prompt_workspace_select').disabled);
      await api.set_text('prompt_input','New task without project context');
      await api.click('prompt_run');
      await api.wait_for(() => !api.element('prompt_run').disabled && api.calls.at(-1)?.stage === 'en_optimize');
      return { ok: api.calls.slice(-2).every((call) => call.context === undefined) && api.element('prompt_workspace_receipt').textContent.includes('Off') };
    });
    await check_in_page(cdp, check, 'workspace unsafe-file state blocks contextual run and opt-out restores normal use', async () => {
      const api = window.__aag_prompt_workspace_test;
      api.fail_preview = true;
      await api.click('prompt_workspace_use_context');
      await api.wait_for(() => api.element('prompt_workspace_context_state').textContent.includes('normal folder'));
      const blocked = api.element('prompt_run').disabled;
      await api.click('prompt_workspace_use_context'); api.fail_preview = false;
      return { ok: blocked && !api.element('prompt_run').disabled && !api.element('prompt_workspace_context_state').textContent.includes('prompt_workspace_') };
    });
    await check_in_page(cdp, check, 'workspace rename and floating quick-save/delete target the current metadata only', async () => {
      const api = window.__aag_prompt_workspace_test;
      const manage = document.querySelector('.prompt_workspace_manage');
      if (!manage.open) manage.querySelector('summary').click();
      await api.set_text('prompt_workspace_name','Team Beta renamed');
      await api.click('page-action-save');
      await api.wait_for(() => [...api.element('prompt_workspace_select').options].some((option) => option.textContent === 'Team Beta renamed'));
      await api.click('page-action-delete');
      await api.wait_for(() => ![...api.element('prompt_workspace_select').options].some((option) => option.textContent === 'Team Beta renamed'));
      return { ok: [...api.element('prompt_workspace_select').options].some((option) => option.textContent === 'Team Alpha') && api.element('prompt_workspace_select').value === '' };
    });
  } finally {
    await cdp.eval_value(`(() => { const api = window.__aag_prompt_workspace_test; api.release?.(); window.__TAURI_INTERNALS__.invoke = api.original; const previous = new Map(api.storage); for (const key of Object.keys(localStorage)) if (!previous.has(key)) localStorage.removeItem(key); for(const [key,value] of previous) localStorage.setItem(key,value); delete window.__aag_prompt_workspace_test; document.querySelector('[data-testid="suppliers_nav"]').click(); return true; })()`);
  }
}

async function main() {
  let runtime; let cdp; let failures = 0; let total = 0;
  const check = (name, ok, detail='') => { total++; if(!ok)failures++; process.stdout.write(`${ok?'PASS':'FAIL'} ${name}${detail?': '+detail:''}\n`); };
  try {
    const browser=find_browser(); if(!browser)throw new Error('Supported browser required');
    runtime=await create_smoke_runtime(browser,{built_demo:process.argv.includes('--built-demo')});
    cdp=await attach_page(runtime.cdp_origin); await cdp.send('Page.navigate',{url:runtime.demo_url});
    const deadline=Date.now()+30000;
    while(Date.now()<deadline&&!await cdp.eval_value('window.__aagDemoIsDemo && !!document.querySelector("[data-testid=suppliers_page]")'))await sleep(100);
    await run_prompt_workspace_checks({cdp,check});
    check('zero uncaught workspace browser exceptions',cdp.exceptions.length===0,cdp.exceptions[0]??'');
  } finally { cdp?.web_socket.close(); await runtime?.close(); }
  process.stdout.write(`${total-failures}/${total} prompt workspace checks passed\n`); if(failures)process.exitCode=1;
}
if(process.argv[1]&&import.meta.url===pathToFileURL(process.argv[1]).href)main().catch((error)=>{process.stderr.write(String(error.stack??error));process.exitCode=1;});
