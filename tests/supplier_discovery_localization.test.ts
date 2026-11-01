import { describe, expect, it } from 'vitest';
import { backend_error_text } from '../ui/src/lib/backend_error.js';
import { t, type Locale } from '../ui/src/i18n/index.js';
import { createRequire } from 'node:module';
import { I18nCtx, type I18nContextValue } from '../ui/src/lib/ctx.js';
import { Supplier_discovery_help, Supplier_discovery_report } from '../ui/src/components/suppliers/supplier_discovery_details.js';
import type { ModelDiscoveryReport } from '../ui/src/lib/api/proxy.js';

const ui_require = createRequire(new URL('../ui/package.json', import.meta.url));
const { createElement } = ui_require('react');
const { renderToStaticMarkup } = ui_require('react-dom/server');
function render_view(locale: Locale, child: unknown): string {
  const value: I18nContextValue = { locale, t: (key, vars) => t(locale, key, vars), setLocale: () => {}, theme: 'light', themeOverride: 'system', setThemeOverride: () => {} };
  return renderToStaticMarkup(createElement(I18nCtx.Provider, { value }, child));
}

const translated = (locale: Locale, code: string) => backend_error_text(code, (key, vars) => t(locale, key, vars));

describe('supplier discovery explanations', () => {
  it('explains exclusions, URL validation failures and missing metadata in both locales', () => {
    expect(translated('zh', 'supplier_import_excluded:subscription_expired')).toContain('已排除');
    expect(translated('zh', 'supplier_import_excluded:subscription_expired')).toContain('订阅已过期');
    expect(translated('en', 'supplier_import_excluded:subscription_expired')).toContain('Subscription expired');
    const validation = translated('zh', 'supplier_url_failed:anthropic:validation:url_validation_http_401');
    expect(validation).toContain('Anthropic');
    expect(validation).toContain('地址验证');
    expect(validation).toContain('认证失败');
    const missing = translated('zh', 'supplier_url_skipped:unknown:search:document_2:openapi_http_404');
    expect(missing).toContain('已跳过');
    expect(missing).toContain('API 描述');
    expect(missing).not.toContain('document_2');
  });

  it('keeps failure stage and cause understandable without echoing unknown backend details', () => {
    const failure = translated('zh', 'supplier_import_failed:configuration:database_error');
    expect(failure).toContain('配置');
    expect(failure).toContain(t('zh', 'error.database'));
    const unknown = translated('zh', 'supplier_url_failed:anthropic:validation:https://private.example/secret');
    expect(unknown).not.toContain('private.example');
    expect(unknown).not.toContain('secret');
    expect(unknown).toContain('未能完成检查');
    expect(translated('en', 'Please enter a name.')).toBe('Please enter a name.');
  });

  it('localizes partial results and availability failures without hiding preserved settings', () => {
    const partial = translated('zh', 'supplier_discovery_partial:proxy_models_timeout,supplier_import_excluded:url_unavailable:proxy_models_http_error_503');
    expect(partial).toContain('已保存的设置');
    expect(partial).toContain('超时');
    expect(partial).toContain('503');
  });

  it('explains draft screening and request preparation failures without raw codes', () => {
    expect(translated('zh', 'supplier_subscription_screening_required')).toContain('可用性检查');
    expect(translated('zh', 'url_http_client_failed')).toContain('无法创建检查请求');
    expect(translated('zh', 'supplier_endpoint_self_loop')).toContain('上游接口地址');
  });

  it('renders the same persisted report and help in the selected locale without mutating results', () => {
    const report: ModelDiscoveryReport = {
      ranAt: '2026-10-31T00:00:00Z', trigger: 'manual', hasChanges: true, deduplicated: false,
      providers: [
        { providerId: 'fixture-active', ok: true, addedModels: ['fixture-model'], hopRewrites: [], mappingRewrites: [], skippedAmbiguous: [] },
        { providerId: 'fixture-expired', ok: false, error: 'supplier_import_excluded:subscription_expired', addedModels: [], hopRewrites: [], mappingRewrites: [], skippedAmbiguous: [] },
        { providerId: 'fixture-active', ok: true, error: 'supplier_url_skipped:unknown:search:document_2:openapi_http_404', addedModels: [], hopRewrites: [], mappingRewrites: [], skippedAmbiguous: [] },
        { providerId: 'supplier:fixture-empty', ok: false, error: 'supplier_import_excluded:no_configured_endpoint', addedModels: [], hopRewrites: [], mappingRewrites: [], skippedAmbiguous: [] },
      ],
    };
    const before = JSON.stringify(report);
    const view = createElement(Supplier_discovery_report, { report, providers: [{ id: 'fixture-active', name: 'Fixture supplier' }] });
    const en_html = render_view('en', view);
    const zh_html = render_view('zh', view);
    expect(en_html).toContain('Last discovery results (4)');
    expect(en_html).toContain('Subscription expired');
    expect(zh_html).toContain('最近一次发现结果（4 条）');
    expect(zh_html).toContain('已完成 · 新增 1 个模型');
    expect(zh_html).toContain('已排除');
    expect(zh_html).toContain('订阅已过期');
    expect(zh_html).toContain('已跳过');
    expect(zh_html).toContain('Fixture supplier');
    expect(zh_html).toContain('连接 fixture-expired');
    expect(zh_html).toContain('供应商记录 fixture-empty');
    expect(zh_html).not.toContain('supplier:fixture-empty');
    expect(zh_html).not.toContain('supplier_import_excluded');
    expect(JSON.stringify(report)).toBe(before);
    const help = render_view('zh', createElement(Supplier_discovery_help));
    expect(help).toContain('自动发现接口地址的工作方式');
    expect(help).toContain('重复运行不会创建重复条目');
    expect(help).toContain('不代表已验证模型调用成功');
  });
});
