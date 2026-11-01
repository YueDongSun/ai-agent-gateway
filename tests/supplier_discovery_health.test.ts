import { createRequire } from 'node:module';
import { expect, it } from 'vitest';
import { Supplier_discovery_status } from '../ui/src/components/suppliers/supplier_discovery_status.js';
import { supplier_discovery_notice, type Supplier_discovery_health } from '../ui/src/lib/api/suppliers_logic.js';
import { I18nCtx, type I18nContextValue } from '../ui/src/lib/ctx.js';
import { t, type Locale } from '../ui/src/i18n/index.js';

const ui_require = createRequire(new URL('../ui/package.json', import.meta.url));
const { createElement } = ui_require('react');
const { renderToStaticMarkup } = ui_require('react-dom/server');

function render_health(locale: Locale, health?: Supplier_discovery_health) {
  const value: I18nContextValue = {
    locale, t: (key, vars) => t(locale, key, vars), setLocale: () => {},
    theme: 'light', themeOverride: 'system', setThemeOverride: () => {},
  };
  return renderToStaticMarkup(createElement(I18nCtx.Provider, { value }, createElement(Supplier_discovery_status, { health })));
}

it('shows listing evidence without claiming successful generation', () => {
  const health: Supplier_discovery_health = {
    status: 'catalog_available', checkedAt: 1_760_000_000, lastSuccessAt: 1_760_000_000,
    latencyMs: 123, consecutiveFailures: 0, error: null,
    listedModelIds: ['model-a', 'model-b'], inferenceStatus: 'unverified',
  };
  const html = render_health('en', health);
  expect(html).toContain('Catalog available');
  expect(html).toContain('2 listed models');
  expect(html).toContain('Generation unverified');
  const failed = render_health('en', { ...health, status: 'authentication_failed', consecutiveFailures: 2 });
  expect(failed).toContain('Catalog authentication failed');
  expect(failed).toContain('2 consecutive failures');
});

it('does not turn missing health or unknown codes into availability', () => {
  expect(render_health('en')).toContain('Catalog not checked');
});

it('renders discovery evidence and missing-health explanations in Chinese through the shared locale', () => {
  const html = render_health('zh', {
    status: 'authentication_failed', checkedAt: 1_760_000_000, lastSuccessAt: 1_760_000_000,
    latencyMs: 123, consecutiveFailures: 2, error: 'proxy_models_http_error_401',
    listedModelIds: ['model-a', 'model-b'], inferenceStatus: 'unverified',
  });
  expect(html).toContain('模型目录认证失败');
  expect(html).toContain('已列出 2 个模型');
  expect(html).toContain('连续失败 2 次');
  expect(html).toContain('尚未验证模型调用');
  expect(html).not.toContain('Generation unverified');
  expect(render_health('zh')).toContain('尚未检查模型目录');
});

it('reports committed settings separately from incomplete discovery without echoing error details', () => {
  expect(supplier_discovery_notice(null)).toBe('');
  expect(supplier_discovery_notice('private fixture error')).toContain('saved settings are preserved');
  expect(supplier_discovery_notice('private fixture error')).not.toContain('private fixture error');
  const chinese = supplier_discovery_notice('private fixture error', (key) => t('zh', key));
  expect(chinese).toContain('已保存的设置会保留');
  expect(chinese).not.toContain('private fixture error');
});
