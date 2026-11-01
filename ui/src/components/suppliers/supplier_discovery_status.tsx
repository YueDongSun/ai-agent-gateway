import type { Supplier_discovery_health } from '../../lib/api/suppliers_logic.js';
import { useCtx } from '../../lib/ctx.js';
import type { MessageKey } from '../../i18n/en.js';

const status_keys: Readonly<Record<string, MessageKey>> = {
  catalog_available: 'suppliers.discovery.status.available',
  authentication_failed: 'suppliers.discovery.status.authentication',
  catalog_unsupported: 'suppliers.discovery.status.unsupported',
  rate_limited: 'suppliers.discovery.status.rate_limited',
  temporarily_unavailable: 'suppliers.discovery.status.unavailable',
  timeout: 'suppliers.discovery.status.timeout',
  connectivity_failed: 'suppliers.discovery.status.connection',
  catalog_invalid: 'suppliers.discovery.status.invalid',
};

export function Supplier_discovery_status({ health }: { health?: Supplier_discovery_health | null }) {
  const { t, locale } = useCtx();
  const format_time = (timestamp: number) => {
    const date = new Date(timestamp * 1000);
    return Number.isFinite(date.getTime()) ? date.toLocaleString(locale === 'zh' ? 'zh-CN' : 'en-US') : t('suppliers.discovery.time_unknown');
  };
  if (!health) return <span className="supplier_hint" data-testid="supplier-discovery-health">{t('suppliers.discovery.status.unchecked')}</span>;
  return <span className="supplier_hint" data-testid="supplier-discovery-health">
    <span className="supplier_tag">{t(Object.hasOwn(status_keys, health.status) ? status_keys[health.status] : 'suppliers.discovery.status.unknown')}</span>
    {' · '}{t('suppliers.discovery.listed', { count: health.listedModelIds.length })}
    {' · '}{t('suppliers.discovery.latency', { latency_ms: health.latencyMs })}
    {' · '}{t('suppliers.discovery.checked', { when: format_time(health.checkedAt) })}
    {health.error && health.lastSuccessAt !== null && <> · {t('suppliers.discovery.last_success', { when: format_time(health.lastSuccessAt) })}</>}
    {health.consecutiveFailures > 0 && <> · {t('suppliers.discovery.failures', { count: health.consecutiveFailures })}</>}
    {' · '}{t('suppliers.discovery.unverified')}
  </span>;
}
