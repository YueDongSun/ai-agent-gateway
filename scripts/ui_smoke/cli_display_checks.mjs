import { pathToFileURL } from 'node:url';
import { attach_page } from './cdp_client.mjs';
import { check_in_page } from './page_checks.mjs';
import { create_smoke_runtime, find_browser, sleep } from './smoke_runtime.mjs';
import { install_client_management_navigation } from './client_management_navigation.mjs';

/** Current-plan fixtures cross the existing IPC boundary without changing any
 * persisted records. Catalog entries and supply metadata are deliberate decoys. */
export async function run_cli_display_checks({ cdp, check }) {
  await cdp.eval_value(`(() => {
    if (window.__aagDemoIsDemo !== true) throw new Error('CLI display checks require synthetic IPC');
    const bridge = window.__TAURI_INTERNALS__;
    const original = bridge.invoke;
    const cases = {
      claude_code: { config: { model: 'legacy_not_selected', env: { ANTHROPIC_MODEL: 'router_claude', ANTHROPIC_DEFAULT_OPUS_MODEL: 'router_opus', ANTHROPIC_DEFAULT_SONNET_MODEL: 'router_claude', ANTHROPIC_DEFAULT_HAIKU_MODEL: 'router_haiku' } }, expected: ['router_claude', 'router_opus', 'router_haiku'] },
      codex: { config: { model: 'router_codex', modelCatalog: { models: [{ display: 'catalog_not_selected', model: 'upstream_not_selected' }] } }, expected: ['router_codex'] },
      copilot: { config: { model: 'router_copilot' }, expected: ['router_copilot'] },
      gemini: { config: { model: 'legacy_not_selected', env: { GEMINI_MODEL: 'router_gemini' } }, expected: ['router_gemini'] },
      opencode: { config: { model: 'router_opencode', models: { catalog_not_selected: {} } }, expected: ['router_opencode'] },
      kilo_cli: { config: { model: 'router_kilo', models: { catalog_not_selected: {} } }, expected: ['router_kilo'] },
      cline_cli: { config: { model: 'router_cline' }, expected: ['router_cline'] },
      claude_vscode: { config: { env: { ANTHROPIC_MODEL: 'router_claude_editor', ANTHROPIC_DEFAULT_SONNET_MODEL: 'router_claude_editor' } }, expected: ['router_claude_editor'] },
      kilo_vscode: { config: { models: { router_kilo_editor: {}, catalog_not_selected: {} } }, expected: ['router_kilo_editor'] },
    };
    const state = { original, cases, calls: [], mode: 'normal', release: null, delay_started: false };
    bridge.invoke = (command, args) => {
      if (/^(provider_|proxy_provider_|route_|supplier_)/.test(command)) state.calls.push({ command, app: args?.appType, selector: args?.selector, surface: args?.surface });
      if (command === 'provider_current') {
        if (state.mode === 'empty') return Promise.resolve(null);
        if (state.mode === 'error') return Promise.reject(new Error('PRIVATE_PROVIDER_ERROR_MUST_NOT_RENDER'));
        return Promise.resolve('selected_' + args.appType);
      }
      if (command === 'provider_get') {
        const fixture = cases[args.appType];
        if (!fixture) return Promise.resolve(null);
        const result = { id: args.id, name: 'PROVIDER_METADATA_MUST_STAY_PRIVATE', websiteUrl: 'https://private-provider.example', notes: 'SUPPLIER_NOTES_MUST_STAY_PRIVATE', settingsConfig: { ...fixture.config, apiKey: 'PRIVATE_KEY_MUST_NOT_RENDER', baseURL: 'https://private-provider.example' } };
        if (state.mode === 'delay' && args.appType === 'claude_code') {
          state.delay_started = true;
          return new Promise((resolve) => { state.release = () => resolve(result); });
        }
        if (state.mode === 'missing-model') result.settingsConfig = { models: { catalog_not_selected: {} }, baseURL: 'https://private-provider.example' };
        return Promise.resolve(result);
      }
      return original(command, args);
    };
    state.wait_for = async (predicate) => {
      for (let attempt = 0; attempt < 80; attempt++) {
        if (predicate()) return true;
        await new Promise((resolve) => setTimeout(resolve, 40));
      }
      return false;
    };
    state.navigate = async (app_type) => {
      const button = document.querySelector('[data-testid="app-tab-' + app_type + '"]');
      if (!button) throw new Error('Missing client navigation: ' + app_type);
      button.click();
      return state.wait_for(() => document.querySelector('[data-testid="cli-router-page"]')?.dataset.app === app_type && !document.querySelector('[data-testid="cli-plan-loading"]'));
    };
    state.models = () => [...document.querySelectorAll('[data-testid="cli-router-model"] code')].map((element) => element.textContent);
    window.__aag_cli_display = state;
    return true;
  })()`);
  try {
    const app_types = await cdp.eval_value('Object.keys(window.__aag_cli_display.cases)');
    for (const app_type of app_types) {
      await cdp.eval_value(`window.__aag_cli_display.target = ${JSON.stringify(app_type)}`);
      await check_in_page(cdp, check, `${app_type}: only the current plan model selections render`, async () => {
        const state = window.__aag_cli_display;
        state.calls.length = 0;
        if (!await state.navigate(state.target)) return 'client-plan-not-ready';
        const main = document.querySelector('main');
        const expected = state.cases[state.target].expected;
        const actual = state.models();
        const surface = state.target.startsWith('claude') ? 'claude' : state.target === 'codex' ? 'codex' : state.target === 'gemini' ? 'gemini' : 'chat';
        const snapshot_read = await state.wait_for(() => state.calls.some((call) => call.command === 'supplier_arena_snapshot' && call.selector === expected[0] && call.surface === surface));
        const unexpected_calls = state.calls.filter(({ command }) => !['provider_current', 'provider_get', 'supplier_arena_snapshot'].includes(command));
        const provider_ui = main.querySelector('.provider-card, .providers-panel, .route-editor, .provider_secondary_actions, [data-testid="provider-modal"], [data-testid="suppliers_page"]');
        const unexpected_controls = [...main.querySelectorAll('input, select')].filter((control) =>
          !control.closest('[data-testid="arena-chart"]') || !control.matches('[data-testid="arena-selector"], [data-testid="arena-needs-tools"], [data-testid="arena-needs-images"]'));
        const arena_options = [...(main.querySelector('[data-testid="arena-selector"]')?.options ?? [])].map((option) => option.value);
        const leaked = /PROVIDER_METADATA_MUST_STAY_PRIVATE|SUPPLIER_NOTES_MUST_STAY_PRIVATE|PRIVATE_KEY_MUST_NOT_RENDER|private-provider\.example|catalog_not_selected|upstream_not_selected|legacy_not_selected/.test(main.textContent);
        const navigation = document.querySelector('.app-switcher');
        const pagination_clean = !navigation.textContent.includes('MUST_STAY_PRIVATE') && !navigation.querySelector('.provider-card, [data-testid^="provider-"]');
        return { ok: snapshot_read && !main.querySelector('[data-testid="arena-current"]') && JSON.stringify(actual) === JSON.stringify(expected) && JSON.stringify(arena_options) === JSON.stringify(expected) && !provider_ui && unexpected_controls.length === 0 && !leaked && pagination_clean && unexpected_calls.length === 0,
          expected, actual, arena_options, unexpected_calls, provider_ui: Boolean(provider_ui), unexpected_controls: unexpected_controls.length, leaked, pagination_clean };
      });
    }
    await check_in_page(cdp, check, 'missing selected configuration and missing model both stay empty', async () => {
      const state = window.__aag_cli_display;
      state.mode = 'empty';
      state.calls.length = 0;
      await state.navigate('claude_code');
      const unselected = Boolean(document.querySelector('[data-testid="cli-plan-empty"]')) && state.models().length === 0 && document.querySelector('[data-testid="coding-agent-arena-state"]')?.dataset.state === 'empty' && !state.calls.some(({ command }) => command === 'supplier_arena_snapshot');
      state.mode = 'missing-model';
      state.calls.length = 0;
      document.querySelector('[data-testid="cli-plan-refresh"]').click();
      await state.wait_for(() => state.calls.some(({ command }) => command === 'provider_get') && !document.querySelector('[data-testid="cli-plan-loading"]'));
      return { ok: unselected && Boolean(document.querySelector('[data-testid="cli-plan-empty"]')) && state.models().length === 0 && !state.calls.some(({ command }) => command === 'supplier_arena_snapshot'), unselected };
    });
    await check_in_page(cdp, check, 'read failure removes stale models and recovers without exposing raw errors', async () => {
      const state = window.__aag_cli_display;
      state.mode = 'normal';
      await state.navigate('codex');
      state.mode = 'error';
      document.querySelector('[data-testid="cli-plan-refresh"]').click();
      await state.wait_for(() => Boolean(document.querySelector('[data-testid="cli-plan-error"]')));
      const rejected = Boolean(document.querySelector('[data-testid="cli-plan-error"]')) && state.models().length === 0 && !document.querySelector('[data-testid="arena-chart"]') && document.querySelector('[data-testid="coding-agent-arena-state"]')?.dataset.state === 'error' && !document.querySelector('main').textContent.includes('PRIVATE_PROVIDER_ERROR');
      state.mode = 'normal';
      document.querySelector('[data-testid="cli-plan-refresh"]').click();
      await state.wait_for(() => state.models()[0] === 'router_codex');
      return { ok: rejected && state.models()[0] === 'router_codex', rejected };
    });
    await check_in_page(cdp, check, 'a delayed prior client cannot replace the current plan', async () => {
      const state = window.__aag_cli_display;
      state.mode = 'delay';
      document.querySelector('[data-testid="app-tab-claude_code"]').click();
      if (!await state.wait_for(() => state.delay_started)) return 'delayed-read-not-started';
      await state.navigate('codex');
      state.release();
      state.mode = 'normal';
      await new Promise((resolve) => setTimeout(resolve, 100));
      return { ok: document.querySelector('[data-testid="cli-router-page"]')?.dataset.app === 'codex' && JSON.stringify(state.models()) === JSON.stringify(['router_codex']) };
    });
  } finally {
    await cdp.eval_value(`(() => { const state = window.__aag_cli_display; state.release?.(); window.__TAURI_INTERNALS__.invoke = state.original; delete window.__aag_cli_display; return true; })()`);
    await cdp.eval_value(`document.querySelector('[data-testid="suppliers_nav"]').click()`);
  }
  await check_in_page(cdp, check, 'provider management is available only after entering Suppliers', async () => {
    await window.__aag_open_client_management('claude_code');
    for (let attempt = 0; attempt < 60 && !document.querySelector('.client_management_body .provider-card'); attempt++) {
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
    return { ok: location.hash === '#/suppliers' && Boolean(document.querySelector('[data-testid="suppliers_page"]')) && Boolean(document.querySelector('.client_management_body .provider-card')) && !document.querySelector('[data-testid="cli-router-page"]') };
  });
}

async function main() {
  let runtime;
  let failures = 0;
  let checks = 0;
  const check = (name, ok, detail = '') => { checks++; if (!ok) failures++; process.stdout.write(`${ok ? 'PASS' : 'FAIL'} ${name}${detail ? ': ' + detail : ''}\n`); };
  try {
    const browser = find_browser();
    if (!browser) throw new Error('A supported browser is required');
    runtime = await create_smoke_runtime(browser, { built_demo: process.argv.includes('--built-demo') });
    const cdp = await attach_page(runtime.cdp_origin);
    await cdp.send('Page.navigate', { url: runtime.demo_url });
    const deadline = Date.now() + 30_000;
    while (Date.now() < deadline && !await cdp.eval_value('window.__aagDemoIsDemo && !!document.querySelector("[data-testid=suppliers_page]")')) await sleep(100);
    await install_client_management_navigation(cdp);
    await run_cli_display_checks({ cdp, check });
    check('zero uncaught browser exceptions', cdp.exceptions.length === 0, cdp.exceptions[0] ?? '');
  } finally { await runtime?.close(); }
  process.stdout.write(`${checks - failures}/${checks} CLI display checks passed\n`);
  if (failures) process.exitCode = 1;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) main().catch((error) => { process.stderr.write(String(error.stack ?? error)); process.exitCode = 1; });
