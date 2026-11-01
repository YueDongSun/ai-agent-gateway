/// Request-trace panel: memory-only request events with failover marks, so
/// alias downgrades stay visible. Reads the shared diagnostics timeline
/// command — bodies, keys, and URLs are never recorded there.
import { useState } from 'react';
import { useT } from '../../lib/ctx.js';
import { diagnostics_request_timeline, type Request_event } from '../../lib/api/diagnostics.js';
import { classify_supplier_error } from '../../lib/api/suppliers.js';
import './suppliers_page.css';

export function Supplier_trace() {
  const t = useT();
  const [events, set_events] = useState<Request_event[] | null>(null);
  const [busy, set_busy] = useState(false);
  const [error, set_error] = useState<string | null>(null);

  async function refresh() {
    set_busy(true);
    set_error(null);
    try {
      set_events(await diagnostics_request_timeline());
    } catch (cause) {
      const info = classify_supplier_error(cause);
      set_error(info.text);
    } finally {
      set_busy(false);
    }
  }

  return (
    <section className="supplier_panel" aria-label={t('suppliers.trace.title')} data-testid="suppliers_trace">
      <h3>{t('suppliers.trace.title')}</h3>
      <p className="supplier_hint">{t('suppliers.trace.subtitle')}</p>
      <div className="supplier_row">
        <button type="button" className="btn" disabled={busy} data-testid="suppliers_trace_refresh" onClick={() => void refresh()}>
          {t('suppliers.trace.refresh')}
        </button>
      </div>
      {error && <div role="alert" className="supplier_error" data-testid="suppliers_trace_error">{error}</div>}
      {events === null ? (
        <p className="supplier_empty_note">{t('suppliers.trace.empty')}</p>
      ) : events.length === 0 ? (
        <p className="supplier_empty_note" data-testid="suppliers_trace_empty">{t('suppliers.trace.empty')}</p>
      ) : (
        <table className="supplier_table" data-testid="suppliers_trace_table">
          <thead>
            <tr>
              <th>{t('suppliers.trace.request')}</th>
              <th>{t('suppliers.trace.surface')}</th>
              <th>{t('suppliers.trace.model')}</th>
              <th>{t('suppliers.trace.plan')}</th>
              <th>{t('suppliers.trace.status')}</th>
              <th>{t('suppliers.trace.duration')}</th>
              <th>{t('suppliers.trace.failover')}</th>
            </tr>
          </thead>
          <tbody>
            {events.map((event) => (
              <tr key={event.requestId}>
                <td><code>{event.requestId}</code></td>
                <td>{event.surface}</td>
                <td><code>{event.modelRef}</code></td>
                <td>
                  {event.planKind ? (
                    <span className="supplier_tag" data-testid="suppliers_trace_plan">
                      {event.planKind}
                      {event.planTier !== null ? ` T${event.planTier}` : ''}
                      {event.configGeneration !== null ? ` g${event.configGeneration}` : ''}
                    </span>
                  ) : (
                    <span className="supplier_tag">—</span>
                  )}
                  {event.planDowngraded === true && (
                    <span className="supplier_tag warn" data-testid="suppliers_trace_downgrade">
                      {t('suppliers.trace.downgrade')}
                    </span>
                  )}
                </td>
                <td>{event.status}</td>
                <td>{event.durationMs} ms</td>
                <td>
                  {event.failedOver === true ? (
                    <span className="supplier_tag warn" data-testid="suppliers_trace_failover">{t('suppliers.trace.failover')}</span>
                  ) : (
                    <span className="supplier_tag">—</span>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      <p className="supplier_hint">{t('suppliers.trace.pending_note')}</p>
    </section>
  );
}
