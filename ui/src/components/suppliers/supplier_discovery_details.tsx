import { useCtx, useT } from '../../lib/ctx.js';
import type { ModelDiscoveryReport, ProxyProvider } from '../../lib/api/proxy.js';
import { supplier_discovery_error_text } from '../../lib/supplier_discovery_text.js';

export function Supplier_discovery_help() {
  const t = useT();
  return <details className="supplier_notice supplier_details" data-testid="supplier-discovery-help">
    <summary>{t('suppliers.discovery.help_title')}</summary>
    <p>{t('suppliers.discovery.help_filter')}</p>
    <p>{t('suppliers.discovery.help_urls')}</p>
    <p>{t('suppliers.discovery.help_limits')}</p>
  </details>;
}

export function Supplier_discovery_report({ report, providers }: {
  report: ModelDiscoveryReport | null | undefined;
  providers: Array<Pick<ProxyProvider, 'id' | 'name'>>;
}) {
  const { t, locale } = useCtx();
  if (!report) return null;
  const names = new Map(providers.map((provider) => [provider.id, provider.name]));
  const date = new Date(report.ranAt);
  const checked_at = Number.isFinite(date.getTime()) ? date.toLocaleString(locale === 'zh' ? 'zh-CN' : 'en-US') : t('suppliers.discovery.time_unknown');
  return <details className="supplier_notice supplier_details" data-testid="supplier-discovery-report">
    <summary>{t('suppliers.discovery.results', { count: report.providers.length })}</summary>
    <p>{t('suppliers.discovery.checked', { when: checked_at })} · {t('suppliers.discovery.unverified')}</p>
    {report.deduplicated ? <p>{t('suppliers.discovery.deduplicated')}</p>
      : report.providers.length === 0 ? <p>{t('suppliers.discovery.empty')}</p>
        : <ul className="supplier_endpoint_list">
          {report.providers.map((entry, index) => <li key={`${entry.providerId}:${index}`} data-testid="supplier-discovery-result">
            <strong>{names.get(entry.providerId) ?? (entry.providerId.startsWith('supplier:')
              ? t('suppliers.discovery.supplier_record', { id: entry.providerId.slice('supplier:'.length) })
              : t('suppliers.discovery.connection_id', { id: entry.providerId }))}</strong>{' · '}
            {entry.error
              ? supplier_discovery_error_text(entry.error, t) ?? t('suppliers.discovery.reason.unknown')
              : entry.ok ? t('suppliers.discovery.completed', { count: entry.addedModels.length })
                : t('suppliers.discovery.reason.unknown')}
          </li>)}
        </ul>}
  </details>;
}
