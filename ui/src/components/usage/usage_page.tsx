/// Usage & cost dashboard (cc-switch parity surface, gap-analysis item 1's
/// UI half). Reads `usage_report` / `usage_prune` through the typed IPC
/// wrappers; the browser demo runs against the synthetic in-memory mock
/// only. Windows are explicit epoch bounds computed here — the backend
/// never guesses at a calendar.
import { useCallback, useEffect, useRef, useState } from 'react';
import { useT } from '../../lib/ctx.js';
import {
  usagePrune,
  usageReport,
  type UsageReport,
} from '../../lib/api/usage.js';
import './usage_page.css';

type WindowPreset = '24h' | '7d' | '30d' | 'all';

const WINDOW_SECONDS: Record<WindowPreset, number | null> = {
  '24h': 86_400,
  '7d': 7 * 86_400,
  '30d': 30 * 86_400,
  all: null,
};

/** App ids recorded by the proxy (`usage_recorder` maps the request surface
 *  1:1: `/v1/messages` → claude, `/v1/responses` → codex,
 *  `/v1/chat/completions` → chat, `/v1beta/models/…` → gemini). */
const APP_FILTERS = ['', 'claude', 'codex', 'chat', 'gemini'] as const;

function format_tokens(value: number): string {
  if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(2)}M`;
  if (value >= 1_000) return `${(value / 1_000).toFixed(1)}k`;
  return String(value);
}

function format_cost(value: number): string {
  return `$${value.toFixed(value >= 100 ? 2 : 4)}`;
}

export function Usage_page() {
  const t = useT();
  const [preset, set_preset] = useState<WindowPreset>('7d');
  const [app_filter, set_app_filter] = useState<string>('');
  const [report, set_report] = useState<UsageReport | null>(null);
  const [busy, set_busy] = useState(true);
  const [error, set_error] = useState<string | null>(null);
  const [notice, set_notice] = useState<string | null>(null);
  const [pruning, set_pruning] = useState(false);
  const mounted = useRef(false);
  const request_sequence = useRef(0);

  const refresh = useCallback(async () => {
    const sequence = ++request_sequence.current;
    set_busy(true);
    set_error(null);
    try {
      const span = WINDOW_SECONDS[preset];
      const since = span === null ? null : Math.floor(Date.now() / 1000) - span;
      const next = await usageReport({
        since,
        appId: app_filter || null,
      });
      if (!mounted.current || sequence !== request_sequence.current) return;
      set_report(next);
    } catch (e) {
      if (!mounted.current || sequence !== request_sequence.current) return;
      set_error(String(e));
    } finally {
      if (mounted.current && sequence === request_sequence.current) set_busy(false);
    }
  }, [preset, app_filter]);

  useEffect(() => {
    mounted.current = true;
    void refresh();
    return () => {
      mounted.current = false;
    };
  }, [refresh]);

  const handle_prune = async () => {
    set_pruning(true);
    set_error(null);
    set_notice(null);
    try {
      const before = Math.floor(Date.now() / 1000) - 30 * 86_400;
      const removed = await usagePrune(before);
      set_notice(t('usage.prune.done', { count: removed }));
      await refresh();
    } catch (e) {
      set_error(String(e));
    } finally {
      set_pruning(false);
    }
  };

  const summary = report?.summary;

  return (
    <div className="usage_page" data-testid="usage_page">
      <header className="usage_heading">
        <div>
          <p className="usage_eyebrow">{t('usage.eyebrow')}</p>
          <h2>{t('usage.title')}</h2>
          <p className="usage_muted">{t('usage.subtitle')}</p>
        </div>
        <div className="usage_row">
          <div className="usage_segmented" role="group" aria-label={t('usage.window')}>
            {(['24h', '7d', '30d', 'all'] as WindowPreset[]).map((window) => (
              <button
                key={window}
                type="button"
                className={preset === window ? 'active' : ''}
                data-testid={`usage_window_${window}`}
                aria-pressed={preset === window}
                onClick={() => set_preset(window)}
              >
                {t(`usage.window.${window}`)}
              </button>
            ))}
          </div>
          <select
            className="input usage_app_filter"
            value={app_filter}
            aria-label={t('usage.appFilter')}
            data-testid="usage_app_filter"
            onChange={(e) => set_app_filter(e.target.value)}
          >
            {APP_FILTERS.map((app) => (
              <option key={app || 'all'} value={app}>
                {app ? t(`usage.app.${app}`) : t('usage.app.all')}
              </option>
            ))}
          </select>
          <button
            type="button"
            className="btn"
            disabled={busy}
            data-testid="usage_refresh"
            onClick={() => void refresh()}
          >
            {t('common.refresh')}
          </button>
        </div>
      </header>

      {error && (
        <div role="alert" className="usage_error" data-testid="usage_error">
          {error}
        </div>
      )}
      {notice && (
        <div role="status" className="usage_notice" data-testid="usage_notice">
          {notice}
        </div>
      )}

      {report?.isEmpty && !busy && (
        <p className="usage_empty" data-testid="usage_empty">
          {t('usage.empty')}
        </p>
      )}

      {summary && !report.isEmpty && (
        <>
          <section className="usage_summary" aria-label={t('usage.summary')}>
            <article className="usage_stat" data-testid="usage_stat_requests">
              <span className="usage_stat_label">{t('usage.stat.requests')}</span>
              <strong>{summary.totalRequests.toLocaleString()}</strong>
            </article>
            <article className="usage_stat" data-testid="usage_stat_tokens">
              <span className="usage_stat_label">{t('usage.stat.tokens')}</span>
              <strong>{format_tokens(summary.totalTokens)}</strong>
              <span className="usage_stat_detail">
                {t('usage.stat.tokensDetail', {
                  input: format_tokens(summary.totalPromptTokens),
                  output: format_tokens(summary.totalCompletionTokens),
                })}
              </span>
            </article>
            <article className="usage_stat" data-testid="usage_stat_cost">
              <span className="usage_stat_label">{t('usage.stat.cost')}</span>
              <strong>{format_cost(summary.totalCostUsd)}</strong>
            </article>
            <article className="usage_stat" data-testid="usage_stat_latency">
              <span className="usage_stat_label">{t('usage.stat.latency')}</span>
              <strong>{Math.round(summary.avgDurationMs).toLocaleString()} ms</strong>
            </article>
          </section>

          <section className="usage_tables">
            <div className="card usage_table_card" data-testid="usage_by_model">
              <div className="card-head">
                <h3>{t('usage.byModel')}</h3>
              </div>
              <table className="usage_table">
                <thead>
                  <tr>
                    <th>{t('usage.column.model')}</th>
                    <th>{t('usage.column.requests')}</th>
                    <th>{t('usage.column.tokens')}</th>
                    <th>{t('usage.column.cost')}</th>
                  </tr>
                </thead>
                <tbody>
                  {report.byModel.map((row) => (
                    <tr key={row.model} data-testid="usage_model_row">
                      <td className="usage_cell_name">{row.model}</td>
                      <td>{row.requestCount.toLocaleString()}</td>
                      <td>{format_tokens(row.totalTokens)}</td>
                      <td>{format_cost(row.totalCostUsd)}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
            <div className="card usage_table_card" data-testid="usage_by_provider">
              <div className="card-head">
                <h3>{t('usage.byProvider')}</h3>
              </div>
              <table className="usage_table">
                <thead>
                  <tr>
                    <th>{t('usage.column.provider')}</th>
                    <th>{t('usage.column.requests')}</th>
                    <th>{t('usage.column.tokens')}</th>
                    <th>{t('usage.column.cost')}</th>
                  </tr>
                </thead>
                <tbody>
                  {report.byProvider.map((row) => (
                    <tr key={row.providerId} data-testid="usage_provider_row">
                      <td className="usage_cell_name">{row.providerId}</td>
                      <td>{row.requestCount.toLocaleString()}</td>
                      <td>{format_tokens(row.totalTokens)}</td>
                      <td>{format_cost(row.totalCostUsd)}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          </section>
        </>
      )}

      <section className="card usage_prune_card" aria-label={t('usage.prune.title')}>
        <div className="usage_prune_row">
          <div>
            <h3>{t('usage.prune.title')}</h3>
            <p className="usage_muted">{t('usage.prune.desc')}</p>
          </div>
          <button
            type="button"
            className="btn danger"
            disabled={pruning || busy}
            data-testid="usage_prune"
            onClick={() => void handle_prune()}
          >
            {pruning ? t('common.loading') : t('usage.prune.action')}
          </button>
        </div>
      </section>
    </div>
  );
}
