import assert from 'node:assert/strict';
import { attach_page } from './ui_smoke/cdp_client.mjs';
import { create_smoke_runtime, find_browser, sleep } from './ui_smoke/smoke_runtime.mjs';

// Task-owned browser profile and demo IPC only. Never connects to a live gateway.
const runtime = await create_smoke_runtime(find_browser());
try {
  const page = await attach_page(runtime.cdp_origin);
  await page.send('Page.navigate', { url: runtime.demo_url });
  async function wait_for(expression) {
    const deadline = Date.now() + 12_000;
    while (Date.now() < deadline) {
      if (await page.eval_value(expression)) return;
      await sleep(50);
    }
    throw new Error(`Timed out: ${expression}`);
  }
  await wait_for('window.__aagDemoIsDemo === true && !!document.querySelector("[data-testid=app-tab-codex]")');
  await page.eval_value(`(() => {
    if (window.__aagDemoIsDemo !== true) throw new Error('demo required');
    window.mirror_test_rows = [{requestId:'request_a',surface:'responses',requestedModel:'5.6sol',resolvedModel:'gpt-5.6-sol-real',phase:'requesting',fallbackCount:0,errorCode:null,configGeneration:7,status:null}];
    const original = window.__TAURI_INTERNALS__.invoke;
    window.__TAURI_INTERNALS__.invoke = (command, args) => {
      if (command === 'proxy_request_status') return Promise.resolve(structuredClone(window.mirror_test_rows));
      if (command === 'codex_reasoning_models') return Promise.resolve([{model:'codex-5-6-sol',supportedEfforts:['low','medium','high','xhigh','max','ultra'],isDefault:true}]);
      if (command === 'provider_save' && args.appType === 'codex') window.mirror_test_saved = structuredClone(args.provider.settingsConfig);
      if (command === 'provider_switch' && args.appType === 'codex') window.mirror_test_switches = (window.mirror_test_switches || 0) + 1;
      return original(command, args);
    };
    original('provider_switch', {id:'codex-oai',appType:'codex'});
    document.querySelector('[data-testid=app-tab-codex]').click();
  })()`);
  await wait_for('document.querySelector("[data-testid=request_target]")?.textContent.includes("5.6sol → gpt-5.6-sol-real")');
  assert.equal(await page.eval_value('document.querySelector("[data-testid=request_phase]").textContent'), 'Requesting');
  console.log('PASS alias resolves to concrete model while requesting');
  await page.eval_value('window.mirror_test_rows[0].phase = "streaming"');
  await wait_for('document.querySelector("[data-testid=request_phase]")?.textContent === "Streaming"');
  console.log('PASS live requesting → streaming refresh');
  await page.eval_value('Object.assign(window.mirror_test_rows[0], {resolvedModel:"fallback-real",fallbackCount:1,errorCode:"upstream_timeout"})');
  await wait_for('document.querySelector("[data-testid=request_target]")?.textContent.includes("fallback-real") && document.querySelector("[data-testid=request_error]")?.textContent.includes("upstream_timeout")');
  console.log('PASS fallback target and safe error refresh');
  await page.eval_value('Object.assign(window.mirror_test_rows[0], {phase:"completed",status:200}); window.mirror_test_rows.unshift({...window.mirror_test_rows[0], requestId:"request_b",resolvedModel:"new-mapping-real",configGeneration:8,phase:"requesting",fallbackCount:0,errorCode:null})');
  await wait_for('document.querySelector("[data-request-id=request_b]")?.textContent.includes("new-mapping-real") && document.querySelector("[data-request-id=request_a]")?.textContent.includes("Completed")');
  console.log('PASS changed mapping on next request and previous terminal result retained');
  await wait_for('!!document.querySelector("[data-testid^=provider-edit-][data-testid$=codex-oai]")');
  await page.eval_value('document.querySelector("[data-testid^=provider-edit-][data-testid$=codex-oai]").click()');
  await wait_for('!!document.querySelector("[data-testid=codex_model]")');
  await page.eval_value(`(() => {
    const select = document.querySelector('[data-testid=codex_model]');
    select.value = 'codex-5-6-sol'; select.dispatchEvent(new Event('change', {bubbles:true}));
  })()`);
  await wait_for('document.querySelector("[data-testid=codex_reasoning_effort]")?.disabled === false');
  assert.deepEqual(await page.eval_value('Array.from(document.querySelector("[data-testid=codex_reasoning_effort]").options).map(option => option.value)'), ['', 'low', 'medium', 'high', 'xhigh', 'max', 'ultra']);
  await page.eval_value(`(() => {
    const select = document.querySelector('[data-testid=codex_reasoning_effort]');
    select.value = 'max'; select.dispatchEvent(new Event('change', {bubbles:true}));
  })()`);
  await wait_for('document.querySelector("[data-testid=provider-modal-save]")?.disabled === false');
  await page.eval_value('document.querySelector("[data-testid=provider-modal-save]").click()');
  await wait_for('!document.querySelector("[data-testid=provider-modal]") && window.mirror_test_saved?.model_reasoning_effort === "max"');
  assert.equal(await page.eval_value('window.mirror_test_saved.model'), 'codex-5-6-sol');
  assert.equal(await page.eval_value('window.mirror_test_switches'), 1);
  console.log('PASS interactive effort selection persists and reapplies the active Codex provider');
  await page.eval_value('document.querySelector("[data-testid=app-tab-claude_code]").click()');
  await wait_for('!!document.querySelector("[data-testid=request_status]") && !document.querySelector("[data-testid=request_target]")');
  console.log('PASS switching client surfaces clears stale request targets');
  assert.equal(page.exceptions.length, 0, page.exceptions.join('\n'));
} finally {
  await runtime.close();
}
