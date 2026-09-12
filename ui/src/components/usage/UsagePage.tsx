import { useCallback, useEffect, useState } from 'react';
import { useT } from '../../lib/ctx';
import {
  getUsageByModel,
  getUsageByProvider,
  getUsageSummary,
  recordUsage,
  type ModelUsage,
  type ProviderUsage,
  type UsageSummary,
} from '../../lib/api/usage';

type RangePreset = '24h' | '7d' | '30d' | 'all';

const PRESETS: { value: RangePreset; key: string }[] = [
  { value: '24h', key: 'usage.range.24h' },
  { value: '7d',  key: 'usage.range.7d'  },
  { value: '30d', key: 'usage.range.30d' },
  { value: 'all', key: 'usage.range.all' },
];

function presetToSince(preset: RangePreset): number | null {
  if (preset === 'all') return null;
  const now = Math.floor(Date.now() / 1000);
  const days = preset === '24h' ? 1 : preset === '7d' ? 7 : 30;
  return now - days * 86400;
}

function formatTokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(2)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}K`;
  return String(n);
}

function formatCost(usd: number): string {
  if (usd === 0) return '$0.00';
  if (usd < 0.01) return `$${usd.toFixed(5)}`;
  return `$${usd.toFixed(2)}`;
}

export function UsagePage() {
  const t = useT();
  const [preset, setPreset] = useState<RangePreset>('7d');
  const [summary, setSummary] = useState<UsageSummary | null>(null);
  const [byModel, setByModel] = useState<ModelUsage[]>([]);
  const [byProvider, setByProvider] = useState<ProviderUsage[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  const refresh = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const since = presetToSince(preset);
      const [s, m, p] = await Promise.all([
        getUsageSummary({ since }),
        getUsageByModel({ since }),
        getUsageByProvider({ since }),
      ]);
      setSummary(s);
      setByModel(m);
      setByProvider(p);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, [preset]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  // Dev-only: seed a sample usage record so the dashboard isn't empty.
  // (Production data comes from the proxy recording each request.)
  const handleSeedSample = async () => {
    setError(null);
    try {
      const now = Math.floor(Date.now() / 1000);
      await recordUsage({
        ts: now,
        appId: 'claude_code',
        providerId: 'anthropic',
        model: 'claude-3-5-sonnet',
        promptTokens: 1200,
        completionTokens: 800,
        costUsd: 0.012,
        durationMs: 2300,
      });
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <div className="usage-page">
      <header className="page-header">
        <h2>{t('usage.title')}</h2>
        <div className="usage-controls">
          {PRESETS.map((p) => (
            <button
              key={p.value}
              className={preset === p.value ? 'tab active' : 'tab'}
              onClick={() => setPreset(p.value)}
            >
              {t(p.key)}
            </button>
          ))}
          <button className="btn" onClick={handleSeedSample} title="Insert a sample record (dev)">
            {t('usage.sample')}
          </button>
        </div>
      </header>

      {error && <pre className="aag-error">{error}</pre>}

      {/* KPI hero */}
      <section className="usage-kpi-row">
        {summary ? (
          <>
            <KpiCard label={t('usage.kpi.requests')}  value={String(summary.totalRequests)} />
            <KpiCard label={t('usage.kpi.totalTokens')} value={formatTokens(summary.totalTokens)} />
            <KpiCard label={t('usage.kpi.prompt')}     value={formatTokens(summary.totalPromptTokens)} />
            <KpiCard label={t('usage.kpi.completion')} value={formatTokens(summary.totalCompletionTokens)} />
            <KpiCard label={t('usage.kpi.cost')}       value={formatCost(summary.totalCostUsd)} />
            <KpiCard
              label={t('usage.kpi.avgDuration')}
              value={`${Math.round(summary.avgDurationMs)}ms`}
            />
          </>
        ) : (
          <p className="muted">{loading ? t('common.loading') : t('common.error', { message: '' }) || 'No data.'}</p>
        )}
      </section>

      <div className="usage-tables">
        <section className="usage-table-section">
          <h4>{t('usage.byModel')}</h4>
          {byModel.length === 0 ? (
            <p className="muted">No usage records in this range.</p>
          ) : (
            <table className="usage-table">
              <thead>
                <tr>
                  <th>{t('usage.col.model')}</th>
                  <th>{t('usage.col.requests')}</th>
                  <th>{t('usage.col.tokens')}</th>
                  <th>{t('usage.col.cost')}</th>
                </tr>
              </thead>
              <tbody>
                {byModel.map((m) => (
                  <tr key={m.model}>
                    <td><code>{m.model}</code></td>
                    <td>{m.requestCount}</td>
                    <td>{formatTokens(m.totalTokens)}</td>
                    <td>{formatCost(m.totalCostUsd)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </section>

        <section className="usage-table-section">
          <h4>{t('usage.byProvider')}</h4>
          {byProvider.length === 0 ? (
            <p className="muted">No usage records in this range.</p>
          ) : (
            <table className="usage-table">
              <thead>
                <tr>
                  <th>{t('usage.col.provider')}</th>
                  <th>{t('usage.col.requests')}</th>
                  <th>{t('usage.col.tokens')}</th>
                  <th>{t('usage.col.cost')}</th>
                </tr>
              </thead>
              <tbody>
                {byProvider.map((p) => (
                  <tr key={p.providerId}>
                    <td><code>{p.providerId}</code></td>
                    <td>{p.requestCount}</td>
                    <td>{formatTokens(p.totalTokens)}</td>
                    <td>{formatCost(p.totalCostUsd)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </section>
      </div>
    </div>
  );
}

function KpiCard({ label, value }: { label: string; value: string }) {
  return (
    <div className="kpi-card">
      <div className="kpi-value">{value}</div>
      <div className="kpi-label">{label}</div>
    </div>
  );
}