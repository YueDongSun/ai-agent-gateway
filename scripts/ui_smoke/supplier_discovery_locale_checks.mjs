/** Locale switching against synthetic discovery evidence; no upstream requests. */
export async function run_supplier_discovery_locale_checks({ cdp, check_in_page, check }) {
  await check_in_page(cdp, check, 'supplier discovery help, health, saved results and draft failures follow EN/CN without another model refresh', async () => {
    if (window.__aagDemoIsDemo !== true) throw new Error('discovery locale smoke requires synthetic IPC');
    const api = window.__aag_supplier_smoke;
    const bridge = window.__TAURI_INTERNALS__;
    const original_invoke = bridge.invoke;
    const original_locale = document.documentElement.lang === 'zh' ? 'zh' : 'en';
    const original_locale_storage = localStorage.getItem('aag.locale');
    const original_expanded = api.element('client-management-toggle')?.getAttribute('aria-expanded') === 'true';
    const original_client = document.querySelector('[data-testid^="client-settings-tab-"][aria-pressed="true"]')
      ?.getAttribute('data-testid').replace('client-settings-tab-', '') ?? 'claude_code';
    const original_help_open = Boolean(api.element('supplier-discovery-help')?.open);
    const provider_rows = await original_invoke('proxy_provider_list');
    const provider_id = provider_rows[0]?.id;
    if (!provider_id) throw new Error('discovery locale smoke needs a seeded proxy provider');
    const result = (ok, error, added_models = []) => ({
      providerId: provider_id, ok, ...(error ? { error } : {}), addedModels: added_models,
      hopRewrites: [], mappingRewrites: [], skippedAmbiguous: [],
    });
    const report = {
      ranAt: '2026-01-15T00:00:00.000Z', trigger: 'manual', hasChanges: true, deduplicated: false,
      providers: [
        result(true, null, ['locale-fixture-model']),
        result(false, 'supplier_import_excluded:subscription_expired'),
        result(true, 'supplier_url_skipped:unknown:search:document_1:openapi_http_404'),
        result(false, 'supplier_url_failed:anthropic:validation:url_validation_http_401'),
      ],
    };
    let model_requests = 0;
    let draft_requests = 0;
    const set_locale = async (locale) => {
      if (document.documentElement.lang !== locale) {
        if (api.element('suppliers_editor')) throw new Error('close the supplier editor before switching locale');
        const button = await api.wait_for(() => document.querySelector('[data-testid="workspace_header"] .lang-btn'));
        button.click();
        await api.wait_for(() => document.documentElement.lang === locale);
      }
    };
    const labels = () => {
      const card = api.element('suppliers_card_supplier_acme');
      const report_panel = api.element('supplier-discovery-report');
      return {
        health: card?.querySelector('[data-testid="supplier-discovery-health"]')?.textContent ?? '',
        help: api.element('supplier-discovery-help')?.textContent ?? '',
        tooltip: api.element('providers-refresh-models')?.getAttribute('title') ?? '',
        report: report_panel?.textContent ?? '',
        rows: report_panel?.querySelectorAll('[data-testid="supplier-discovery-result"]').length ?? 0,
      };
    };
    const is_english = (text) => text.health.includes('Catalog authentication failed')
      && text.health.includes('2 listed models') && text.health.includes('2 consecutive failures')
      && text.health.includes('Generation unverified')
      && text.help.includes('How automatic URL discovery works')
      && text.tooltip.includes('Check configured suppliers')
      && text.report.includes('Completed') && text.report.includes('Excluded')
      && text.report.includes('Subscription expired') && text.report.includes('Skipped')
      && text.report.includes('Authentication failed') && text.rows === 4;
    const is_chinese = (text) => text.health.includes('模型目录认证失败')
      && text.health.includes('已列出 2 个模型') && text.health.includes('连续失败 2 次')
      && text.health.includes('尚未验证模型调用') && !text.health.includes('Generation unverified')
      && text.help.includes('自动发现接口地址的工作方式')
      && text.tooltip.includes('检查已配置供应商的可用性')
      && text.report.includes('已完成') && text.report.includes('已排除')
      && text.report.includes('订阅已过期') && text.report.includes('已跳过')
      && text.report.includes('认证失败') && text.rows === 4;
    const check_draft_failure = async (locale) => {
      await api.wait_for(() => !api.element('suppliers_editor'));
      await set_locale(locale);
      await api.click('suppliers_edit_supplier_acme');
      await api.wait_for(() => api.element('suppliers_editor'));
      try {
        await api.click('suppliers_editor_fetch_models');
        const message = await api.wait_for(() => {
          const error = api.element('suppliers_editor_error');
          const fetch = api.element('suppliers_editor_fetch_models');
          return error && fetch && !fetch.disabled ? error.textContent : null;
        });
        const context = locale === 'zh' ? '获取模型失败' : 'Fetching models failed';
        const cause = locale === 'zh' ? '认证失败' : 'Authentication failed';
        return message.includes(context) && message.includes(cause) && message.includes('401')
          && !message.includes('supplier_models_http_error_401');
      } finally {
        await api.click('suppliers_editor_close');
        await api.wait_for(() => !api.element('suppliers_editor'));
      }
    };
    bridge.invoke = async (command, args) => {
      if (command === 'supplier_discover_models') {
        draft_requests += 1;
        throw 'supplier_models_http_error_401';
      }
      if (command === 'proxy_refresh_models') {
        model_requests += 1;
        return structuredClone(report);
      }
      if (command === 'model_discovery_status') {
        return { report: model_requests ? structuredClone(report) : null, pendingRestart: false, intervalHours: 24, lastRunEpoch: 0 };
      }
      const response = await original_invoke(command, args);
      if (command !== 'supplier_list') return response;
      const view = structuredClone(response);
      const endpoint = view.suppliers.find((supplier) => supplier.id === 'supplier_acme')?.endpoints[0];
      if (!endpoint) throw new Error('discovery locale smoke needs the seeded supplier endpoint');
      endpoint.discoveryHealth = {
        status: 'authentication_failed', checkedAt: 1_760_000_000, lastSuccessAt: 1_759_999_000,
        latencyMs: 12, consecutiveFailures: 2, error: 'proxy_models_http_error_401',
        listedModelIds: ['locale-fixture-a', 'locale-fixture-b'], inferenceStatus: 'unverified',
      };
      return view;
    };
    try {
      await set_locale('en');
      await api.click('suppliers_refresh');
      await api.wait_for(() => labels().health.includes('Catalog authentication failed'));
      const help = await api.wait_for(() => api.element('supplier-discovery-help'));
      if (!help.open) help.querySelector('summary').click();
      await api.wait_for(() => help.open);
      await window.__aag_open_client_management('claude_code');
      await api.click('providers-refresh-models');
      await api.wait_for(() => labels().rows === 4);
      const report_panel = api.element('supplier-discovery-report');
      if (!report_panel.open) report_panel.querySelector('summary').click();
      await api.wait_for(() => is_english(labels()));
      const english = is_english(labels());
      await set_locale('zh');
      await api.wait_for(() => is_chinese(labels()));
      const chinese = is_chinese(labels());
      await set_locale('en');
      await api.wait_for(() => is_english(labels()));
      const restored_english = is_english(labels());
      const draft_english = await check_draft_failure('en');
      const draft_chinese = await check_draft_failure('zh');
      return {
        ok: english && chinese && restored_english && draft_english && draft_chinese
          && model_requests === 1 && draft_requests === 2,
        detail: `en=${english} zh=${chinese} en_again=${restored_english} model_refreshes=${model_requests} draft_en=${draft_english} draft_zh=${draft_chinese} draft_requests=${draft_requests}`,
      };
    } finally {
      bridge.invoke = original_invoke;
      if (api.element('suppliers_editor')) {
        await api.click('suppliers_editor_close');
        await api.wait_for(() => !api.element('suppliers_editor'));
      }
      await set_locale(original_locale);
      if (original_locale_storage === null) localStorage.removeItem('aag.locale');
      else localStorage.setItem('aag.locale', original_locale_storage);
      // Remount the management controller after restoring the bridge so the
      // injected persisted report cannot leak into subsequent smoke checks.
      const toggle = api.element('client-management-toggle');
      if (toggle?.getAttribute('aria-expanded') === 'true') {
        toggle.click();
        await api.wait_for(() => !api.element('providers-panel'));
      }
      await api.click('suppliers_refresh');
      await api.wait_for(() => {
        const health = labels().health;
        return health.includes(original_locale === 'zh' ? '尚未检查模型目录' : 'Catalog not checked');
      });
      const help = api.element('supplier-discovery-help');
      if (help) help.open = original_help_open;
      if (original_expanded) await window.__aag_open_client_management(original_client);
    }
  });
}
