import { run_optimization_checks } from './optimization_checks.mjs';

const responsive_viewports = [
  { width: 1200, height: 900 },
  { width: 900, height: 900 },
  { width: 420, height: 900 },
];

export async function run_product_initial_checks({ cdp, check_in_page, check, sleep }) {
  await check_in_page(cdp, check, 'header and gateway card agree on initial stopped state', async () => {
    const api = window.__aag_smoke_api;
    const status = await api.getProxyStatus();
    const header = document.querySelector('[data-testid="gateway_status"]');
    return {
      ok: !status.running && header?.classList.contains('stopped') && Boolean(document.querySelector('[data-testid="gateway-start"]')),
      detail: 'header=' + header?.className + ' running=' + status.running,
    };
  });

  await check_in_page(cdp, check, 'gateway key stays masked except during explicit reveal and copies the original value', async () => {
    const api = window.__aag_smoke_api;
    const key_value = (await api.getProxyConfig()).authToken;
    const value_element = document.querySelector('[data-testid="gateway_key_value"]');
    const visibility_button = document.querySelector('[data-testid="gateway_key_visibility"]');
    const copy_button = document.querySelector('[data-testid="gateway_key_copy"]');
    if (!key_value || !value_element || !visibility_button || !copy_button) return 'missing-key-fixture-or-controls';
    if (document.body.innerText.includes(key_value)) return 'key-visible-before-reveal';
    visibility_button.click();
    await new Promise((resolve) => setTimeout(resolve, 100));
    const revealed = value_element.textContent === key_value && visibility_button.getAttribute('aria-pressed') === 'true';
    visibility_button.click();
    await new Promise((resolve) => setTimeout(resolve, 100));
    const masked = !document.body.innerText.includes(key_value) && visibility_button.getAttribute('aria-pressed') === 'false';
    const previous_descriptor = Object.getOwnPropertyDescriptor(navigator.clipboard, 'writeText');
    let copied_value = null;
    try {
      Object.defineProperty(navigator.clipboard, 'writeText', {
        configurable: true,
        value: async (value) => { copied_value = value; },
      });
      copy_button.click();
      await new Promise((resolve) => setTimeout(resolve, 100));
      return { ok: revealed && masked && copied_value === key_value, detail: 'reveal=' + revealed + ' mask=' + masked + ' copy=' + (copied_value === key_value) };
    } finally {
      if (previous_descriptor) Object.defineProperty(navigator.clipboard, 'writeText', previous_descriptor);
      else delete navigator.clipboard.writeText;
    }
  });

  await check_in_page(cdp, check, 'route header columns align with body controls', async () => {
    const table = document.querySelector('.hop-table');
    const row = table?.querySelector('tbody tr');
    const headers = [...(table?.querySelectorAll('thead th') ?? [])];
    const cells = [...(row?.querySelectorAll('td') ?? [])];
    if (!row || headers.length !== 5 || cells.length !== 5) return 'missing-five-column-table';
    const aligned = headers.every((header, index) => {
      const heading_bounds = header.getBoundingClientRect();
      const cell_bounds = cells[index].getBoundingClientRect();
      return Math.abs(heading_bounds.left - cell_bounds.left) <= 1 && Math.abs(heading_bounds.width - cell_bounds.width) <= 1;
    });
    return { ok: getComputedStyle(row).display === 'table-row' && aligned, detail: 'display=' + getComputedStyle(row).display + ' aligned=' + aligned };
  });

  await check_in_page(cdp, check, 'switching applications hides a revealed gateway key', async () => {
    const visibility_button = document.querySelector('[data-testid="gateway_key_visibility"]');
    const codex_button = [...document.querySelectorAll('.client_management_switcher button')].find((button) => button.textContent.trim() === 'Codex');
    const claude_button = [...document.querySelectorAll('.client_management_switcher button')].find((button) => button.textContent.trim() === 'Claude');
    if (!visibility_button || !codex_button || !claude_button) return 'missing-key-or-app-controls';
    visibility_button.click();
    await new Promise((resolve) => setTimeout(resolve, 100));
    if (visibility_button.getAttribute('aria-pressed') !== 'true') return 'key-not-revealed';
    codex_button.click();
    let hidden_on_codex = false;
    try {
      for (let attempt = 0; attempt < 20; attempt += 1) {
        await new Promise((resolve) => setTimeout(resolve, 100));
        if (document.querySelector('.page-header h2')?.textContent === 'Codex') {
          hidden_on_codex = document.querySelector('[data-testid="gateway_key_visibility"]')?.getAttribute('aria-pressed') === 'false';
          break;
        }
      }
    } finally {
      claude_button.click();
    }
    for (let attempt = 0; attempt < 20; attempt += 1) {
      await new Promise((resolve) => setTimeout(resolve, 100));
      if (document.querySelector('.page-header h2')?.textContent === 'Claude' && document.querySelector('[data-testid="gateway_key_visibility"]')?.getAttribute('aria-pressed') === 'false') return { ok: hidden_on_codex, detail: 'key-remasked-across-tabs=' + hidden_on_codex };
    }
    return 'claude-did-not-restore-masked-key';
  });

  await check_in_page(cdp, check, 'theme toggle settles with readable header and route controls', async () => {
    const theme_button = document.querySelector('.theme-btn');
    const root = document.documentElement;
    if (!theme_button) return 'missing-theme-button';
    const initial_theme = root.dataset.theme;
    const parse_color = (value) => {
      const values = value.match(/\d+(?:\.\d+)?/g)?.slice(0, 3).map(Number);
      return values?.length === 3 ? values : null;
    };
    const luminance = (color) => color
      .map((value) => value / 255)
      .map((value) => value <= 0.03928 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4)
      .reduce((total, value, index) => total + value * [0.2126, 0.7152, 0.0722][index], 0);
    const contrast = (foreground, background) => {
      const fg = parse_color(foreground);
      const bg = parse_color(background);
      if (!fg || !bg) return null;
      const [light, dark] = [luminance(fg), luminance(bg)].sort((left, right) => right - left);
      return (light + 0.05) / (dark + 0.05);
    };
    const snapshot = () => {
      const header = document.querySelector('.aag-header');
      const route_control = document.querySelector('.hop-row select');
      if (!header || !route_control) return null;
      const header_style = getComputedStyle(header);
      const button_style = getComputedStyle(theme_button);
      const route_style = getComputedStyle(route_control);
      return {
        theme: root.dataset.theme,
        header: contrast(button_style.color, header_style.backgroundColor),
        route: contrast(route_style.color, route_style.backgroundColor),
      };
    };
    const initial_snapshot = snapshot();
    theme_button.click();
    await new Promise((resolve) => setTimeout(resolve, 350));
    const switched_snapshot = snapshot();
    theme_button.click();
    await new Promise((resolve) => setTimeout(resolve, 350));
    const restored_theme = root.dataset.theme;
    const snapshots = [initial_snapshot, switched_snapshot];
    const readable = snapshots.every((entry) => entry && entry.header >= 4.5 && entry.route >= 4.5);
    const covers_both = new Set(snapshots.map((entry) => entry?.theme)).size === 2;
    return {
      ok: readable && covers_both && restored_theme === initial_theme,
      detail: JSON.stringify({ initial_snapshot, switched_snapshot, restored_theme, initial_theme }),
    };
  });

  try {
    for (const viewport of responsive_viewports) {
      await cdp.send('Emulation.setDeviceMetricsOverride', { ...viewport, deviceScaleFactor: 1, mobile: false });
      await cdp.eval_value('window.scrollTo(0, 0)');
      await sleep(150);
      await check_in_page(cdp, check, 'layout contains controls at viewport ' + viewport.width, async () => {
        const viewport_width = document.documentElement.clientWidth;
        const header = document.querySelector('.aag-header')?.getBoundingClientRect();
        const navigation = document.querySelector('.app-switcher')?.getBoundingClientRect();
        const sidebar = document.querySelector('[data-testid="workspace_sidebar"]')?.getBoundingClientRect();
        const main = document.querySelector('.aag-main')?.getBoundingClientRect();
        const table_region = document.querySelector('.hop_table_wrap');
        if (!header || !navigation || !main || !table_region) return 'missing-layout-landmark';
        const page_fits = document.documentElement.scrollWidth <= viewport_width + 1;
        const navigation_fits = navigation.left >= 0 && navigation.right <= viewport_width + 1 && (sidebar ? navigation.bottom <= sidebar.bottom + 1 : navigation.bottom <= header.bottom + 1);
        const sidebar_fits = !sidebar || (viewport_width > 900 ? main.left >= sidebar.right - 1 : header.top >= sidebar.bottom - 1);
        const header_fits = main.top >= header.bottom - 1;
        const table_scrolls = viewport_width >= 600 || table_region.scrollWidth > table_region.clientWidth;
        return {
          ok: page_fits && navigation_fits && header_fits && sidebar_fits && table_scrolls,
          detail: 'viewport=' + viewport_width + ' document=' + document.documentElement.scrollWidth + ' nav=' + navigation_fits + ' header=' + header_fits + ' table=' + table_scrolls,
        };
      });
    }
  } finally {
    await cdp.send('Emulation.clearDeviceMetricsOverride');
    await sleep(150);
  }

  // Direct JSON editor (Kilo family): open from the client-configuration
  // card, edit, save, and verify the round-trip plus the status refresh.
  // Returns to Claude at the end so later checks observe the default page.
  await check_in_page(cdp, check, 'kilo direct JSON editor round-trips through the client configuration card', async () => {
    const kilo_button = [...document.querySelectorAll('.client_management_switcher button')].find((button) => button.textContent.trim() === 'Kilo CLI');
    if (!kilo_button) return 'missing-kilo-tab';
    kilo_button.click();
    for (let attempt = 0; attempt < 12; attempt += 1) {
      await new Promise((resolve) => setTimeout(resolve, 60));
      if (document.querySelector('[data-testid="client-config-status"]')) break;
    }
    const edit_button = document.querySelector('[data-testid="client-config-edit"]');
    if (!edit_button) return 'missing-edit-json-button';
    edit_button.click();
    let editor = null;
    for (let attempt = 0; attempt < 12; attempt += 1) {
      await new Promise((resolve) => setTimeout(resolve, 60));
      editor = document.querySelector('[data-testid="config-editor-modal"]');
      if (editor && document.querySelector('[data-testid="config-editor-text"]')?.value) break;
    }
    const textarea = document.querySelector('[data-testid="config-editor-text"]');
    if (!editor || !textarea?.value) return 'editor-did-not-open';
    const had_skeleton = textarea.value.includes('provider');
    const edited = '{\n  // smoke round-trip\n  "provider": { "smoke": { "npm": "@ai-sdk/openai-compatible" } },\n  "model": "smoke/demo"\n}\n';
    // React controlled textarea: use the native value setter so React's
    // change tracking sees the programmatic edit (a plain .value= write is
    // ignored by the controlled component).
    const value_setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value').set;
    value_setter.call(textarea, edited);
    textarea.dispatchEvent(new Event('input', { bubbles: true }));
    await new Promise((resolve) => setTimeout(resolve, 120));
    const invalid_banner = document.querySelector('[data-testid="config-editor-invalid"]');
    if (invalid_banner) return 'valid-jsonc-flagged-invalid';
    const save_button = document.querySelector('[data-testid="config-editor-save"]');
    if (!save_button || save_button.disabled) return 'save-disabled-for-valid-document';
    save_button.click();
    let saved = false;
    for (let attempt = 0; attempt < 16; attempt += 1) {
      await new Promise((resolve) => setTimeout(resolve, 60));
      if (!document.querySelector('[data-testid="config-editor-modal"]')) { saved = true; break; }
    }
    if (!saved) return 'editor-did-not-close-after-save';
    // Re-open and confirm the saved contents (mock is in-memory).
    document.querySelector('[data-testid="client-config-edit"]')?.click();
    let reopened = '';
    for (let attempt = 0; attempt < 12; attempt += 1) {
      await new Promise((resolve) => setTimeout(resolve, 60));
      reopened = document.querySelector('[data-testid="config-editor-text"]')?.value ?? '';
      if (reopened) break;
    }
    if (!reopened.includes('smoke round-trip')) return 'edited-contents-did-not-persist';
    document.querySelector('[data-testid="config_editor_cancel"]')?.click();
    let closed = false;
    for (let attempt = 0; attempt < 12; attempt += 1) {
      await new Promise((resolve) => setTimeout(resolve, 60));
      if (!document.querySelector('[data-testid="config-editor-modal"]')) { closed = true; break; }
    }
    if (!closed) return 'editor-did-not-close-after-cancel';
    const claude_button = [...document.querySelectorAll('.client_management_switcher button')].find((button) => button.textContent.trim() === 'Claude');
    claude_button?.click();
    await new Promise((resolve) => setTimeout(resolve, 120));
    return { ok: had_skeleton, detail: 'skeleton=' + had_skeleton + ' persisted=true' };
  });

  // Unified pool: create a standalone upstream row from the pool section
  // ("+ Add upstream"), verify it lands as a card, then delete it again so
  // the fixture state is unchanged for later checks.
  await check_in_page(cdp, check, 'standalone pool row can be created and removed from the pool section', async () => {
    const claude_button = [...document.querySelectorAll('.client_management_switcher button')].find((button) => button.textContent.trim() === 'Claude');
    if (!claude_button) return 'missing-claude-tab';
    claude_button.click();
    let add_button = null;
    for (let attempt = 0; attempt < 20; attempt += 1) {
      await new Promise((resolve) => setTimeout(resolve, 60));
      add_button = document.querySelector('[data-testid="providers-add-standalone"]');
      if (add_button) break;
    }
    if (!add_button) return 'missing-add-standalone-button';
    add_button.click();
    let modal = null;
    for (let attempt = 0; attempt < 12; attempt += 1) {
      await new Promise((resolve) => setTimeout(resolve, 60));
      modal = document.querySelector('[data-testid="provider-modal"]');
      if (modal) break;
    }
    if (!modal) return 'modal-did-not-open';
    const name_input = modal.querySelector('input:not([type="hidden"])');
    if (!name_input) return 'missing-name-input';
    const native_setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set;
    native_setter.call(name_input, 'smoke-pool-row');
    name_input.dispatchEvent(new Event('input', { bubbles: true }));
    await new Promise((resolve) => setTimeout(resolve, 80));
    // Empty base URL must block the save (validation contract).
    let save = modal.querySelector('[data-testid="provider-modal-save"]');
    save?.click();
    await new Promise((resolve) => setTimeout(resolve, 150));
    if (!document.querySelector('[data-testid="provider-modal"]')) return 'empty-base-url-save-was-accepted';
    // Fill the upstream base URL and save for real. The base-URL field is
    // identified by its deepseek placeholder (several other empty inputs —
    // id/icon/website — precede it in DOM order).
    const base_url_input = [...modal.querySelectorAll('input')].find((input) => input.placeholder === 'https://api.deepseek.com/v1');
    if (!base_url_input) return 'missing-base-url-input';
    native_setter.call(base_url_input, 'https://smoke-pool.example.invalid/v1');
    base_url_input.dispatchEvent(new Event('input', { bubbles: true }));
    await new Promise((resolve) => setTimeout(resolve, 80));
    save = document.querySelector('[data-testid="provider-modal-save"]');
    save?.click();
    let created = false;
    for (let attempt = 0; attempt < 20; attempt += 1) {
      await new Promise((resolve) => setTimeout(resolve, 80));
      if (!document.querySelector('[data-testid="provider-modal"]') && document.body.innerText.includes('smoke-pool-row')) { created = true; break; }
    }
    if (!created) return 'pool-row-was-not-created';
    // Delete the row again so the fixture state is unchanged for later
    // checks (the mock pool persists for the page session).
    window.confirm = () => true;
    const delete_button = document.querySelector('[data-testid="provider-delete-smoke-pool-row"]');
    if (!delete_button) return 'missing-delete-button';
    delete_button.click();
    let removed = false;
    for (let attempt = 0; attempt < 20; attempt += 1) {
      await new Promise((resolve) => setTimeout(resolve, 80));
      if (!document.body.innerText.includes('smoke-pool-row')) { removed = true; break; }
    }
    if (!removed) return 'pool-row-was-not-removed';
    return 'ok';
  });

  await run_optimization_checks({ cdp, check_in_page, check });
}

export async function run_gateway_feedback_checks({ cdp, check_in_page, check }) {
  await check_in_page(cdp, check, 'header reflects a successful gateway start', async () => {
    const start_button = document.querySelector('[data-testid="gateway-start"]');
    if (!start_button) return 'missing-start-button';
    start_button.click();
    for (let attempt = 0; attempt < 30; attempt += 1) {
      await new Promise((resolve) => setTimeout(resolve, 100));
      if (document.querySelector('[data-testid="gateway_status"]')?.classList.contains('running') && document.querySelector('[data-testid="gateway-stop"]')) return 'ok';
    }
    return 'header-did-not-follow-start';
  });

  await check_in_page(cdp, check, 'stopping the gateway cancels delayed health without a stale status', async () => {
    const original_fetch = window.fetch;
    let health_started = false;
    let health_aborted = false;
    let stopped = false;
    try {
      window.fetch = (resource, options) => {
        if (!String(resource).endsWith('/health') || !options?.signal) return original_fetch(resource, options);
        health_started = true;
        return new Promise((resolve, reject) => {
          const abort = () => { health_aborted = true; reject(new DOMException('synthetic cancelled health', 'AbortError')); };
          if (options.signal.aborted) abort();
          else options.signal.addEventListener('abort', abort, { once: true });
        });
      };
      for (let attempt = 0; attempt < 55 && !health_started; attempt += 1) await new Promise((resolve) => setTimeout(resolve, 100));
      if (!health_started) return 'health-poll-did-not-start';
      const stop_button = document.querySelector('[data-testid="gateway-stop"]');
      if (!stop_button) return 'missing-stop-during-health';
      stop_button.click();
      for (let attempt = 0; attempt < 20; attempt += 1) {
        await new Promise((resolve) => setTimeout(resolve, 100));
        if (document.querySelector('[data-testid="gateway_status"]')?.classList.contains('stopped') && health_aborted) { stopped = true; break; }
      }
    } finally {
      window.fetch = original_fetch;
    }
    const start_button = document.querySelector('[data-testid="gateway-start"]');
    if (!start_button) return 'gateway-not-stopped-for-restore';
    start_button.click();
    for (let attempt = 0; attempt < 20; attempt += 1) {
      await new Promise((resolve) => setTimeout(resolve, 100));
      if (document.querySelector('[data-testid="gateway_status"]')?.classList.contains('running')) return { ok: stopped && health_aborted, detail: 'stopped=' + stopped + ' aborted=' + health_aborted };
    }
    return 'gateway-did-not-recover-after-cancelled-health';
  });

  await check_in_page(cdp, check, 'failed health is degraded and recovers without claiming stopped', async () => {
    const original_fetch = window.fetch;
    const refresh_button = () => document.querySelector('[data-testid="providers-refresh"]');
    const button = refresh_button();
    if (!button) return 'missing-refresh-button';
    let degraded = false;
    try {
      window.fetch = async (resource, options) => String(resource).endsWith('/health')
        ? new Response(JSON.stringify({ status: 'ok', providers: [] }), { status: 503, headers: { 'content-type': 'application/json' } })
        : original_fetch(resource, options);
      button.click();
      for (let attempt = 0; attempt < 30; attempt += 1) {
        await new Promise((resolve) => setTimeout(resolve, 100));
        if (document.querySelector('[data-testid="gateway_status"]')?.classList.contains('degraded')) { degraded = true; break; }
      }
    } finally {
      window.fetch = original_fetch;
      refresh_button()?.click();
    }
    let recovered = false;
    for (let attempt = 0; attempt < 30; attempt += 1) {
      await new Promise((resolve) => setTimeout(resolve, 100));
      if (document.querySelector('[data-testid="gateway_status"]')?.classList.contains('running')) { recovered = true; break; }
    }
    return { ok: degraded && recovered && Boolean(document.querySelector('[data-testid="gateway-stop"]')), detail: 'degraded=' + degraded + ' recovered=' + recovered };
  });

  await check_in_page(cdp, check, 'failed runtime status is unknown and recovers', async () => {
    if (!window.__aagDemoSetError) return 'missing-demo-error-control';
    let unknown = false;
    try {
      window.__aagDemoSetError('proxy_status');
      for (let attempt = 0; attempt < 65; attempt += 1) {
        await new Promise((resolve) => setTimeout(resolve, 100));
        if (document.querySelector('[data-testid="gateway_status"]')?.classList.contains('unknown')) { unknown = true; break; }
      }
    } finally {
      window.__aagDemoSetError(null);
      document.querySelector('[data-testid="providers-refresh"]')?.click();
    }
    for (let attempt = 0; attempt < 20; attempt += 1) {
      await new Promise((resolve) => setTimeout(resolve, 100));
      if (document.querySelector('[data-testid="gateway_status"]')?.classList.contains('running')) return { ok: unknown, detail: 'unknown=' + unknown + ' recovered=true' };
    }
    return 'runtime-status-did-not-recover';
  });

  await check_in_page(cdp, check, 'header reflects a successful gateway stop', async () => {
    const stop_button = document.querySelector('[data-testid="gateway-stop"]');
    if (!stop_button) return 'missing-stop-button';
    stop_button.click();
    for (let attempt = 0; attempt < 30; attempt += 1) {
      await new Promise((resolve) => setTimeout(resolve, 100));
      if (document.querySelector('[data-testid="gateway_status"]')?.classList.contains('stopped') && document.querySelector('[data-testid="gateway-start"]')) return 'ok';
    }
    return 'header-did-not-follow-stop';
  });
}
