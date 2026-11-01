import { useEffect, useState } from 'react';
import { proxy_request_status, request_status_for_surface, visible_request_status, type Request_status } from '../../lib/api/request_status.js';
import { useT } from '../../lib/ctx';

const refresh_interval_ms = 500;

export function Request_status_rows({ rows }: { rows: Request_status[] }) {
  const t = useT();
  return <div data-testid="request_status" aria-live="polite">
    <h4>{t('requestStatus.title')}</h4>
    {rows.length === 0 && <p className="muted small">{t('requestStatus.empty')}</p>}
    {rows.map((row) => <div className="endpoint-row" key={row.requestId} data-request-id={row.requestId}>
      <code className="endpoint-value" data-testid="request_target">
        {row.requestedModel} → {row.resolvedModel ?? t('requestStatus.resolving')}
      </code>
      <span className="pill" data-testid="request_phase">{t(`requestStatus.${row.phase}`)}</span>
      {row.fallbackCount > 0 && <span className="pill warning">{t('requestStatus.fallback', { count: row.fallbackCount })}</span>}
      {row.errorCode && <span className="muted small" data-testid="request_error">{row.errorCode}{row.status && row.status >= 400 ? ` (${row.status})` : ''}</span>}
    </div>)}
  </div>;
}

/** Serial polling reads authoritative admitted targets, including hot-reloaded mappings. */
export function Live_request_status({ surface, refresh_version }: { surface: string; refresh_version: number }) {
  const t = useT();
  const [rows, set_rows] = useState<Request_status[]>([]);
  const [read_failed, set_read_failed] = useState(false);
  useEffect(() => {
    let disposed = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let fetching = false;
    set_rows([]);
    async function refresh() {
      if (disposed || fetching) return;
      fetching = true;
      try {
        if (document.visibilityState !== 'hidden') {
          const next = request_status_for_surface(await proxy_request_status(), surface);
          if (!disposed) {
            // Keep overlapping active requests visible; retain the latest finished request.
            set_rows(visible_request_status(next));
            set_read_failed(false);
          }
        }
      } catch {
        if (!disposed) set_read_failed(true);
      } finally {
        fetching = false;
        if (!disposed) timer = setTimeout(refresh, refresh_interval_ms);
      }
    }
    const on_visible = () => {
      if (document.visibilityState !== 'hidden' && !fetching) {
        clearTimeout(timer);
        void refresh();
      }
    };
    void refresh();
    document.addEventListener('visibilitychange', on_visible);
    return () => { disposed = true; clearTimeout(timer); document.removeEventListener('visibilitychange', on_visible); };
  }, [surface, refresh_version]);
  return <>
    <Request_status_rows rows={rows} />
    {read_failed && <p role="status" className="muted small">{t('requestStatus.unavailable')}</p>}
  </>;
}
