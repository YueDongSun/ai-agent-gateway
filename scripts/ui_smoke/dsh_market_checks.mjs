import { cdp_client } from './cdp_client.mjs';
import { cdp_timeout_ms, read_cdp_endpoint } from './cdp_endpoint.mjs';
import { readFile } from 'node:fs/promises';

const responsive_viewports = [
  { width: 1200, height: 900 },
  { width: 900, height: 900 },
  { width: 420, height: 900 },
];
const target_timeout_ms = cdp_timeout_ms;

async function list_owned_targets(cdp_origin) {
  return read_cdp_endpoint(cdp_origin, '/json/list', { timeout_ms: target_timeout_ms });
}

async function attach_market_popup(cdp_origin, initial_target_ids, demo_url, sleep) {
  const expected_origin = new URL(demo_url).origin;
  const deadline = Date.now() + target_timeout_ms;
  while (Date.now() < deadline) {
    const targets = await list_owned_targets(cdp_origin);
    const popup = targets.find((target) => {
      if (target.type !== 'page' || initial_target_ids.has(target.id)) return false;
      try {
        const url = new URL(target.url);
        return url.origin === expected_origin && url.searchParams.get('view') === 'dsh_market';
      } catch { return false; }
    });
    if (popup) {
      const client = new cdp_client(new WebSocket(popup.webSocketDebuggerUrl));
      await client.connect();
      client.market_exception_records = [];
      client.web_socket.addEventListener('message', (event) => {
        const message = JSON.parse(event.data);
        if (message.method === 'Runtime.exceptionThrown') client.market_exception_records.push(message.params.exceptionDetails);
      });
      await client.send('Runtime.enable');
      await client.send('Page.enable');
      return { client, target_id: popup.id };
    }
    await sleep(100);
  }
  throw new Error('DSH marketplace button did not open a new same-origin marketplace target');
}

// Installed only in the new demo popup. Preserve the absent native bridge until
// mockIPC creates it, so the instrumentation does not change demo detection.
function trace_market_page(feed) {
  const trace = { commands: [], catalog: null, override: null, opened_urls: [], feed, fetches: [], fail_next: true };
  window.__aag_market_trace = trace;
  const original_fetch = window.fetch;
  window.fetch = async (target, options) => {
    if (String(target) !== 'https://dsh.market/plugins.json') return original_fetch(target, options);
    trace.fetches.push({ target: String(target), credentials: options?.credentials, cache: options?.cache });
    if (trace.fail_next) { trace.fail_next = false; throw new Error('smoke network unavailable'); }
    return new Response(JSON.stringify(trace.feed), { headers: { 'Content-Type': 'application/json' } });
  };
  let bridge;
  Object.defineProperty(window, '__TAURI_INTERNALS__', {
    configurable: true,
    get: () => bridge,
    set: (value) => {
      bridge = value;
      if (!value) return;
      let implementation = value.invoke;
      const traced_invoke = async function(command, args, options) {
        trace.commands.push({ command, args });
        const result = await implementation.call(value, command, args, options);
        if (command === 'dsh_market_get_catalog' || command === 'dsh_market_refresh') {
          if (!trace.override) trace.catalog = structuredClone(result);
          return trace.override ? structuredClone(trace.override) : result;
        }
        return result;
      };
      Object.defineProperty(value, 'invoke', {
        configurable: true,
        get: () => typeof implementation === 'function' ? traced_invoke : implementation,
        set: (next) => { implementation = next; },
      });
    },
  });
  const original_open = window.open;
  window.open = function(url, ...args) {
    trace.opened_urls.push(String(url));
    return original_open.call(this, url, ...args);
  };
}

async function prepare_market_page() {
  const wait_for = async (predicate, timeout = 6000) => {
    const deadline = Date.now() + timeout;
    while (Date.now() < deadline) {
      const result = await predicate();
      if (result) return result;
      await new Promise((resolve) => setTimeout(resolve, 40));
    }
    throw new Error('market smoke condition timed out');
  };
  await wait_for(() => window.__aagDemoIsDemo === true && window.__aag_market_trace?.catalog && document.querySelector('[data-testid="market_card"]'));
  const element = (id) => [...document.querySelectorAll('[data-testid]')].find((item) => item.getAttribute('data-testid') === id);
  const pause = (ms = 80) => new Promise((resolve) => setTimeout(resolve, ms));
  const click = async (id) => {
    const target = element(id);
    if (!target || target.disabled) throw new Error('missing or disabled market control: ' + id);
    target.focus();
    target.click();
    await pause();
  };
  const input = async (id, value) => {
    const target = element(id);
    if (!target) throw new Error('missing market input: ' + id);
    Object.getOwnPropertyDescriptor(target.constructor.prototype, 'value').set.call(target, value);
    target.dispatchEvent(new Event(target instanceof HTMLSelectElement ? 'change' : 'input', { bubbles: true }));
    await pause();
  };
  const cards = () => [...document.querySelectorAll('[data-testid="market_card"]')];
  const ids = () => cards().map((item) => item.getAttribute('data-entry-id'));
  const trace = window.__aag_market_trace;
  const entries = () => (trace.override ?? trace.catalog).entries;
  const displayed_entries = () => ids().map((id) => entries().find((entry) => entry.id === id));
  const reset = async () => { if (element('market_clear_filters')) await click('market_clear_filters'); await input('market_search', ''); };
  const count = () => Number((element('market_results_count')?.textContent ?? '').replace(/[^0-9]/g, ''));
  window.__aag_market_smoke = { trace, element, pause, wait_for, click, input, cards, ids, entries, displayed_entries, reset, count };
  return true;
}

export async function run_dsh_market_checks({ cdp, cdp_origin, demo_url, check_in_page, check, sleep }) {
  // The historical snapshot supplies test metadata only, never runtime catalog data.
  const source_fixture = JSON.parse(await readFile(new URL('../../shared/dsh_market_catalog.json', import.meta.url), 'utf8'));
  const entries = source_fixture.entries.slice(0, 160);
  const feed_fixture = { schemaVersion: 2, generatedAt: source_fixture.registry_generated_at, plugins: entries.map((entry) => ({
    fullName: entry.full_name, description: entry.description, descriptionEn: entry.description_en, descriptionZh: entry.description_zh,
    stars: entry.stars, updatedAt: entry.updated_at, topics: [...entry.categories, ...entry.use_cases, ...entry.modules],
    packageName: entry.package_name, version: entry.version, license: entry.license, verifiedCommit: entry.source_commit,
  })) };
  let popup = null;
  const initial_target_ids = new Set((await list_owned_targets(cdp_origin)).map((target) => target.id));
  const fixture_ready = await cdp.eval_value('(' + (async () => {
    if (window.__aagDemoIsDemo !== true) throw new Error('refusing marketplace smoke outside demo mode');
    const invoke = window.__TAURI_INTERNALS__.invoke;
    const fixture = async () => JSON.stringify(await Promise.all([
      invoke('proxy_config_get'), invoke('proxy_status'), invoke('proxy_provider_list'), invoke('route_list'),
      invoke('provider_list', { appType: 'claude_code' }), invoke('dsh_get_snapshot'),
    ]));
    window.__aag_market_main = { fixture, before: await fixture(), storage: Object.entries(localStorage), url: location.href };
    document.querySelector('[data-testid="dsh_nav"]')?.click();
    return true;
  }).toString() + ')()');
  if (!fixture_ready) throw new Error('market main-page fixture could not be saved');

  try {
    await check_in_page(cdp, check, 'DSH page exposes a concrete marketplace button', async () => {
      for (let attempt = 0; attempt < 60; attempt += 1) {
        const button = document.querySelector('[data-testid="dsh_open_marketplace"]');
        if (button) return { ok: button.tagName === 'BUTTON' && !button.disabled && Boolean(button.textContent.trim()), detail: button.textContent.trim() };
        await new Promise((resolve) => setTimeout(resolve, 50));
      }
      return 'marketplace-button-missing';
    });
    await cdp.send('Runtime.evaluate', {
      expression: 'document.querySelector(\'[data-testid="dsh_open_marketplace"]\').click()',
      returnByValue: true,
      userGesture: true,
    });
    popup = await attach_market_popup(cdp_origin, initial_target_ids, demo_url, sleep);
    const market_cdp = popup.client;
    // The target list can report a pending URL before its about:blank document
    // commits. Wait for this exact popup's document, never attach another tab.
    const popup_ready_deadline = Date.now() + target_timeout_ms;
    let popup_ready = false;
    while (Date.now() < popup_ready_deadline) {
      try {
        const url = new URL(await market_cdp.eval_value('location.href'));
        popup_ready = url.origin === new URL(demo_url).origin && url.searchParams.get('view') === 'dsh_market';
      } catch (error) {
        if (!/Execution context was destroyed|Cannot find context/.test(String(error))) throw error;
      }
      if (popup_ready) break;
      await sleep(50);
    }
    if (!popup_ready) throw new Error('marketplace popup did not commit its same-origin document within 8s');
    await check_in_page(market_cdp, check, 'DSH marketplace opens in a separate same-origin tab without an opener', async () => ({
      ok: new URL(location.href).searchParams.get('view') === 'dsh_market' && window.opener === null,
      detail: 'view=' + new URL(location.href).searchParams.get('view') + ' opener-null=' + (window.opener === null),
    }));

    await market_cdp.send('Page.addScriptToEvaluateOnNewDocument', { source: '(' + trace_market_page.toString() + ')(' + JSON.stringify(feed_fixture) + ')' });
    // Settle the original live load before replacing its document. Reloading
    // during lazy imports or an IPC response can report an orphaned rejection.
    const initial_load_deadline = Date.now() + 35_000;
    let initial_load_settled = false;
    while (Date.now() < initial_load_deadline) {
      initial_load_settled = await market_cdp.eval_value(`Boolean(
        document.querySelector('[data-testid="market_refresh"]')?.disabled === false &&
        (document.querySelector('[data-testid="market_card"]') || document.querySelector('[data-testid="market_error"]'))
      )`);
      if (initial_load_settled) break;
      await sleep(100);
    }
    if (!initial_load_settled) throw new Error('initial marketplace request did not settle before fixture reload');
    await market_cdp.send('Page.reload', { ignoreCache: false });
    await sleep(150);
    await check_in_page(market_cdp, check, 'Initial network failure shows retry without a bundled or cached list', async () => {
      for (let attempt = 0; attempt < 120; attempt += 1) {
        const error = document.querySelector('[data-testid="market_error"]');
        if (error) {
          const trace = window.__aag_market_trace;
          const ok = trace.fetches.length === 1 && trace.catalog === null && !document.querySelector('[data-testid="market_card"]') &&
            !document.querySelector('[data-testid="market_empty"]');
          error.querySelector('button')?.click();
          return { ok, detail: 'requests=' + trace.fetches.length + ' bundled-list=false' };
        }
        await new Promise((resolve) => setTimeout(resolve, 50));
      }
      return 'initial-fetch-error-missing';
    });
    const market_ready_deadline = Date.now() + 30_000;
    let market_ready = false;
    while (Date.now() < market_ready_deadline) {
      market_ready = await market_cdp.eval_value('window.__aagDemoIsDemo === true && Boolean(window.__aag_market_trace?.catalog) && Boolean(document.querySelector(\'[data-testid="market_card"]\'))');
      if (market_ready) break;
      await sleep(100);
    }
    if (!market_ready) throw new Error('owned marketplace did not render its real source catalog within 30s');
    const helpers_ready = await market_cdp.eval_value('(' + prepare_market_page.toString() + ')()');
    if (!helpers_ready) throw new Error('market demo helpers did not install');

    await check_in_page(market_cdp, check, 'Standalone market mounts the source catalog without gateway or runtime commands', async () => {
      const api = window.__aag_market_smoke;
      const catalog = api.trace.catalog;
      await api.pause(300);
      const commands = api.trace.commands.map((record) => record.command);
      return {
        ok: Boolean(api.element('market_page')) && !document.querySelector('.aag-header') &&
          catalog.entry_count === catalog.entries.length && catalog.entry_count > 100 && api.cards().length > 0 && api.cards().length <= 12 &&
          api.count() === catalog.entry_count && commands.length > 0 && commands.every((command) => command === 'dsh_market_get_catalog') &&
          api.trace.fetches.length === 2 && api.trace.fetches.every((request) => request.credentials === 'omit' && request.cache === 'no-store') &&
          catalog.source_registry === 'dsh.market' && catalog.registry_commit === '' && catalog.cache_status === 'refreshed',
        detail: 'entries=' + catalog.entry_count + ' rendered=' + api.cards().length + ' commands=' + commands.join(','),
      };
    });

    await check_in_page(market_cdp, check, 'Reopening fetches changed content and preserves the active search', async () => {
      const api = window.__aag_market_smoke;
      const original_feed = structuredClone(api.trace.feed);
      const original_count = api.entries().length;
      const requests_before = api.trace.fetches.length;
      await api.input('market_search', 'example/live_refresh');
      api.trace.feed.plugins.push({ fullName: 'example/live_refresh', description: 'Browser automation workflow tools', topics: ['browser', 'automation'], stars: 7 });
      window.dispatchEvent(new Event('dsh_market_opened'));
      await api.wait_for(() => api.entries().length === original_count + 1 && api.ids().includes('example/live_refresh'));
      const updated = api.trace.fetches.length === requests_before + 1 && api.element('market_search').value === 'example/live_refresh' && api.count() === 1;
      api.trace.fail_next = true;
      window.dispatchEvent(new Event('dsh_market_opened'));
      await api.wait_for(() => api.element('market_error'));
      const retained = api.ids().includes('example/live_refresh') && api.count() === 1 && api.element('market_error').textContent.includes('previous catalog');
      api.trace.feed = original_feed;
      window.dispatchEvent(new Event('dsh_market_opened'));
      await api.wait_for(() => !api.element('market_error') && api.entries().length === original_count && !api.element('market_refresh').disabled);
      await api.reset();
      return { ok: updated && retained, detail: 'fresh-response=' + updated + ' filters-preserved=' + updated + ' failed-reopen-retained=' + retained };
    });

    await check_in_page(market_cdp, check, 'Marketplace EN and CN switches update labels and preserve catalog state', async () => {
      const api = window.__aag_market_smoke;
      const before = api.ids().join('|');
      await api.click('market_locale_en');
      const english = api.element('market_title')?.textContent.includes('Make DSH work your way.') &&
        api.element('market_search')?.labels?.[0]?.textContent.includes('Search the plugin catalog') &&
        api.element('market_locale_en')?.getAttribute('aria-pressed') === 'true';
      await api.click('market_locale_zh');
      const chinese = api.element('market_title')?.textContent.includes('让 DSH 更懂你的工作方式。') &&
        api.element('market_search')?.labels?.[0]?.textContent.includes('搜索插件目录') &&
        api.element('market_locale_zh')?.getAttribute('aria-pressed') === 'true';
      await api.click('market_locale_en');
      return { ok: english && chinese && before === api.ids().join('|'), detail: 'en=' + english + ' cn=' + chinese + ' unchanged-cards=' + (before === api.ids().join('|')) };
    });

    await check_in_page(market_cdp, check, 'Function and practical-workflow filters intersect independently', async () => {
      const api = window.__aag_market_smoke;
      const categories = [...document.querySelectorAll('[data-testid^="market_category_"]')].map((item) => item.getAttribute('data-testid').slice('market_category_'.length)).filter((value) => value !== 'all');
      const use_cases = [...document.querySelectorAll('[data-testid^="market_use_case_"]')].map((item) => item.getAttribute('data-testid').slice('market_use_case_'.length)).filter((value) => value !== 'all');
      let choice = null;
      for (const category of categories) {
        const by_category = api.entries().filter((entry) => entry.categories.includes(category));
        for (const use_case of use_cases) {
          const intersection = by_category.filter((entry) => entry.use_cases.includes(use_case));
          const by_use_case = api.entries().filter((entry) => entry.use_cases.includes(use_case));
          if (intersection.length > 0 && intersection.length < by_category.length && intersection.length < by_use_case.length) {
            choice = { category, use_case, by_category, intersection, by_use_case };
            break;
          }
        }
        if (choice) break;
      }
      if (!choice) return 'snapshot-has-no-independent-category-workflow-intersection';
      await api.click('market_category_' + choice.category);
      const function_ok = api.count() === choice.by_category.length && api.displayed_entries().every((entry) => entry?.categories.includes(choice.category));
      await api.click('market_use_case_' + choice.use_case);
      const intersect_ok = api.count() === choice.intersection.length && api.displayed_entries().every((entry) => entry?.categories.includes(choice.category) && entry.use_cases.includes(choice.use_case));
      await api.click('market_category_all');
      const retained = api.element('market_use_case_' + choice.use_case)?.getAttribute('aria-pressed') === 'true' && api.count() === choice.by_use_case.length;
      await api.reset();
      return { ok: function_ok && intersect_ok && retained, detail: choice.category + '/' + choice.use_case + ': function=' + function_ok + ' intersection=' + intersect_ok + ' workflow-retained=' + retained };
    });

    await check_in_page(market_cdp, check, 'Marketplace search finds actual repository metadata and clears an empty result', async () => {
      const api = window.__aag_market_smoke;
      const candidate = api.entries().find((entry) => entry.full_name && entry.full_name.length > 8);
      if (!candidate) return 'catalog-missing-searchable-repository';
      await api.input('market_search', candidate.full_name);
      const found = api.ids().includes(candidate.id) && api.count() > 0 && api.count() < api.entries().length;
      await api.input('market_search', 'aag_smoke_impossible_query_53c9d044');
      const empty = Boolean(api.element('market_empty')) && api.cards().length === 0 && api.count() === 0;
      const commands_before_clear = api.trace.commands.length;
      await api.click('market_clear_filters');
      const recovered = api.count() === api.entries().length && api.cards().length > 0 && !api.element('market_empty');
      return { ok: found && empty && recovered && api.trace.commands.length === commands_before_clear, detail: 'repo-found=' + found + ' empty=' + empty + ' clear-recovered=' + recovered };
    });

    await check_in_page(market_cdp, check, 'Star sorting and pagination preserve a bounded, nonduplicated result page', async () => {
      const api = window.__aag_market_smoke;
      await api.input('market_sort', 'stars');
      const first = api.ids();
      const first_entries = api.displayed_entries();
      const first_sorted = first_entries.every((entry, index) => index === 0 || first_entries[index - 1].stars >= entry.stars);
      await api.click('market_next');
      const second = api.ids();
      const next_entries = api.displayed_entries();
      const second_sorted = next_entries.every((entry, index) => index === 0 || next_entries[index - 1].stars >= entry.stars);
      const crosses_page = first_entries.at(-1).stars >= next_entries[0].stars;
      const disjoint = second.length > 0 && second.length <= 12 && second.every((id) => !first.includes(id));
      await api.click('market_previous');
      return { ok: first_sorted && second_sorted && crosses_page && disjoint && first.join('|') === api.ids().join('|'), detail: 'sorted=' + (first_sorted && second_sorted && crosses_page) + ' disjoint=' + disjoint + ' restored-first-page=' + (first.join('|') === api.ids().join('|')) };
    });

    await check_in_page(market_cdp, check, 'Name and update sorting reorder real catalog results', async () => {
      const api = window.__aag_market_smoke;
      await api.input('market_sort', 'name');
      const named = api.displayed_entries();
      const names_sorted = named.every((entry, index) => index === 0 || named[index - 1].name.localeCompare(entry.name) <= 0);
      await api.input('market_sort', 'recent');
      const recent = api.displayed_entries();
      const dates_sorted = recent.every((entry, index) => index === 0 || Date.parse(recent[index - 1].updated_at ?? '1970-01-01') >= Date.parse(entry.updated_at ?? '1970-01-01'));
      await api.input('market_sort', 'stars');
      return { ok: names_sorted && dates_sorted && named.map((entry) => entry.id).join('|') !== recent.map((entry) => entry.id).join('|'), detail: 'name-order=' + names_sorted + ' date-order=' + dates_sorted };
    });

    await check_in_page(market_cdp, check, 'Plugin details expose the live feed and mark its unprovided revision as unknown', async () => {
      const api = window.__aag_market_smoke;
      const entry = api.displayed_entries()[0];
      await api.click('market_detail_' + entry.id);
      const dialog = api.element('market_detail_dialog');
      const repository = api.element('market_source_repository');
      const registry = api.element('market_source_registry');
      const collector = api.element('market_source_collector');
      const catalog = api.trace.catalog;
      const source_links = [repository, registry];
      const safe_links = source_links.every((link) => {
        try { const url = new URL(link?.getAttribute('href')); return url.protocol === 'https:' && (url.hostname === 'github.com' || url.href === 'https://dsh.market/plugins.json') && link.getAttribute('rel')?.includes('noopener'); }
        catch { return false; }
      });
      const source_matches = repository?.getAttribute('href') === entry.repo_url && registry?.getAttribute('href') === catalog.registry_url && !collector;
      const warning = dialog?.textContent.includes('A source listing is not a trust certificate.') && dialog.textContent.includes('not the plugin code');
      const focused = dialog?.contains(document.activeElement);
      const commit = api.element('market_registry_commit')?.textContent === 'Not provided by the marketplace';
      await api.click('market_detail_close');
      const focus_restored = document.activeElement?.getAttribute('data-testid') === 'market_detail_' + entry.id;
      return { ok: safe_links && source_matches && warning && focused && commit && focus_restored, detail: 'safe-links=' + safe_links + ' provenance=' + source_matches + ' warning=' + warning + ' pinned=' + commit + ' focus-restored=' + focus_restored };
    });

    await check_in_page(market_cdp, check, 'Refresh failure keeps the previous source catalog visible and retry recovers', async () => {
      const api = window.__aag_market_smoke;
      const before = api.ids().join('|');
      const count_before = api.count();
      let preserved = false;
      try {
        window.__aagDemoSetError('dsh_market_refresh');
        await api.click('market_refresh');
        await api.wait_for(() => api.element('market_error'));
        preserved = api.ids().join('|') === before && api.count() === count_before && api.element('market_error').textContent.includes('previous catalog');
      } finally { window.__aagDemoSetError(null); }
      await api.click('market_refresh');
      await api.wait_for(() => !api.element('market_error') && !api.element('market_refresh')?.disabled);
      return { ok: preserved && api.ids().join('|') === before && api.count() === count_before, detail: 'failed-refresh-preserved=' + preserved + ' recovered=' + !api.element('market_error') };
    });

    await check_in_page(market_cdp, check, 'Untrusted plugin metadata stays inert and unsafe source URLs are not links', async () => {
      const api = window.__aag_market_smoke;
      const sample = structuredClone(api.trace.catalog.entries[0]);
      const marker = '<img src=x onerror="window.__aag_market_payload_executed=true">';
      const hostile = {
        ...sample, id: 'aag_smoke/hostile_metadata', name: marker, full_name: 'aag_smoke/hostile_metadata',
        description: '<script>window.__aag_market_payload_executed=true</script>', description_en: null, description_zh: null,
        repo_url: 'javascript:window.__aag_market_payload_executed=true', source_commit_url: 'https://github.com.evil.invalid/payload',
        source_commit: '7'.repeat(40), install_spec: marker, package_name: marker,
      };
      api.trace.override = { ...api.trace.catalog, entry_count: 1, entries: [hostile], registry_url: 'javascript:alert(1)', collector_url: 'https://github.com.evil.invalid/collector' };
      const opened_before = api.trace.opened_urls.length;
      try {
        await api.click('market_refresh');
        await api.wait_for(() => api.count() === 1);
        const literal_card = api.cards()[0]?.textContent.includes(marker);
        await api.click('market_detail_' + hostile.id);
        const dialog = api.element('market_detail_dialog');
        const sources = ['repository', 'registry', 'collector', 'commit'].map((kind) => api.element('market_source_' + kind)).filter(Boolean);
        const sources_inert = sources.every((element) => !element.hasAttribute('href') && (element.disabled || element.getAttribute('aria-disabled') === 'true'));
        const injected_elements = api.element('market_page').querySelectorAll('[onerror], [src="x"], script, iframe').length + dialog.querySelectorAll('[onerror], [src="x"], script, iframe').length;
        const no_execution = !window.__aag_market_payload_executed && api.trace.opened_urls.length === opened_before;
        const literal_detail = dialog?.textContent.includes(marker) && dialog.textContent.includes(hostile.description);
        await api.click('market_detail_close');
        return { ok: literal_card && literal_detail && sources_inert && injected_elements === 0 && no_execution, detail: 'literal-card=' + literal_card + ' literal-detail=' + literal_detail + ' sources-inert=' + sources_inert + ' injected-elements=' + injected_elements + ' executed=' + !no_execution };
      } finally {
        api.element('market_detail_close')?.click();
        api.trace.override = null;
        await api.click('market_refresh');
        await api.wait_for(() => api.count() === api.trace.catalog.entry_count);
        delete window.__aag_market_payload_executed;
      }
    });

    for (const theme of ['light', 'dark']) {
      await market_cdp.eval_value('window.__aag_market_smoke.expected_theme = ' + JSON.stringify(theme));
      const current_theme = await market_cdp.eval_value('document.documentElement.dataset.theme');
      if (current_theme !== theme) await market_cdp.eval_value('window.__aag_market_smoke.click("market_theme")');
      for (const viewport of responsive_viewports) {
        await market_cdp.send('Emulation.setDeviceMetricsOverride', { ...viewport, deviceScaleFactor: 1, mobile: false });
        await sleep(150);
        await check_in_page(market_cdp, check, 'Marketplace ' + theme + ' layout and details fit viewport ' + viewport.width, async () => {
          const api = window.__aag_market_smoke;
          const width = document.documentElement.clientWidth;
          const theme_matches = document.documentElement.dataset.theme === api.expected_theme && api.element('market_page').dataset.theme === api.expected_theme;
          const visible_controls = () => [...api.element('market_page').querySelectorAll('button, input, select')].filter((element) => element.getClientRects().length);
          const horizontal_fit = (element) => { const box = element.getBoundingClientRect(); return box.left >= -1 && box.right <= document.documentElement.clientWidth + 1; };
          const page_fits = document.documentElement.scrollWidth <= width + 1 && visible_controls().every(horizontal_fit) && api.cards().every(horizontal_fit);
          const entry = api.displayed_entries()[0];
          await api.click('market_detail_' + entry.id);
          const dialog = api.element('market_detail_dialog');
          const bounds = dialog.getBoundingClientRect();
          const dialog_fits = horizontal_fit(dialog) && bounds.top >= -1 && bounds.bottom <= window.innerHeight + 1 &&
            [...dialog.querySelectorAll('button, input, select, a')].filter((element) => element.getClientRects().length).every(horizontal_fit);
          const focused = dialog.contains(document.activeElement);
          await api.click('market_detail_close');
          return { ok: theme_matches && page_fits && dialog_fits && focused, detail: 'theme=' + document.documentElement.dataset.theme + ' width=' + width + ' document=' + document.documentElement.scrollWidth + ' page=' + page_fits + ' dialog=' + dialog_fits + ' dialog-box=' + JSON.stringify({ left: bounds.left, right: bounds.right, top: bounds.top, bottom: bounds.bottom }) + ' focus=' + focused };
        });
      }
    }

    await check_in_page(market_cdp, check, 'Marketplace interactions never invoke install, gateway, provider or runtime operations', async () => {
      const api = window.__aag_market_smoke;
      const allowed = ['dsh_market_get_catalog', 'dsh_market_refresh', 'dsh_market_open_source'];
      const commands = api.trace.commands.map((entry) => entry.command);
      const unexpected = commands.filter((command) => !allowed.includes(command));
      return { ok: unexpected.length === 0 && api.trace.override === null && api.trace.opened_urls.length === 0, detail: 'calls=' + commands.length + ' commands=' + [...new Set(commands)].join(',') + ' unexpected=' + unexpected.join(',') };
    });
    // Attribute any exception to its source. Without the URL and script id a
    // failure here reports only `name:line`, which cannot distinguish
    // application code from a third-party script injected into this browser.
    const exception_summary = market_cdp.market_exception_records.map((record) => ({
      text: record.text,
      description: record.exception?.description,
      url: record.url ?? record.stackTrace?.callFrames?.[0]?.url ?? null,
      script_id: record.scriptId ?? record.stackTrace?.callFrames?.[0]?.scriptId ?? null,
      frames: record.stackTrace?.callFrames?.slice(0, 4).map((frame) => ({ name: frame.functionName, line: frame.lineNumber, column: frame.columnNumber })),
    }));
    check('marketplace has zero uncaught browser exceptions', market_cdp.exceptions.length === 0, JSON.stringify(exception_summary));
    await check_in_page(cdp, check, 'Marketplace popup preserves its opener route', () => ({
      ok: location.href === new URL('#/dsh', window.__aag_market_main.url).href,
    }));
  } finally {
    if (popup) {
      await cdp.send('Target.closeTarget', { targetId: popup.target_id });
      popup.client.web_socket.close();
    }
    await cdp.eval_value('(' + (async () => {
      const state = window.__aag_market_main;
      if (!state || window.__aagDemoIsDemo !== true) return false;
      const previous = new Map(state.storage);
      for (const key of Object.keys(localStorage)) if (!previous.has(key)) localStorage.removeItem(key);
      for (const [key, value] of previous) localStorage.setItem(key, value);
      await window.__aag_open_client_management('claude_code');
      for (let attempt = 0; attempt < 60; attempt += 1) {
        if (document.querySelector('.page-header h2')?.textContent === 'Claude') return true;
        await new Promise((resolve) => setTimeout(resolve, 50));
      }
      throw new Error('market smoke did not restore the original Claude page');
    }).toString() + ')()');
  }

  await check_in_page(cdp, check, 'Marketplace popup leaves the original app and DSH fixtures unchanged', async () => {
    const state = window.__aag_market_main;
    const preserved = await state.fixture() === state.before;
    // This test explicitly restores Suppliers client management above; its route reflects that
    // navigation. Check the full expected URL, including origin/path/query.
    const original_url = location.href === new URL('#/suppliers', state.url).href;
    delete window.__aag_market_main;
    return { ok: preserved && original_url && document.querySelector('.page-header h2')?.textContent === 'Claude', detail: 'fixtures=' + preserved + ' original-url=' + original_url };
  });
}
