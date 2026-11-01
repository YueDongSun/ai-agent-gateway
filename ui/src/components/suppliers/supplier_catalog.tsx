/// Public model-catalog status and cached backend assessments.
/// Public visibility never implies entitlement to call a model.
import { useState } from 'react';
import { useT } from '../../lib/ctx.js';
import {
  classify_supplier_error,
  is_supplier_feature_unavailable,
  supplier_catalog_refresh,
  supplier_catalog_status,
  type Supplier_catalog_view,
} from '../../lib/api/suppliers.js';
import { Supplier_assessment } from './supplier_assessment.js';
import './suppliers_page.css';

export function Supplier_catalog() {
  const t = useT();
  const [sources, set_sources] = useState<Supplier_catalog_view[] | null>(null);
  const [busy, set_busy] = useState(false);
  const [unavailable, set_unavailable] = useState(false);
  const [error, set_error] = useState<string | null>(null);

  async function load() {
    set_busy(true);
    set_error(null);
    set_unavailable(false);
    try {
      const status = await supplier_catalog_status();
      set_sources(status.sources);
    } catch (cause) {
      if (is_supplier_feature_unavailable(cause)) {
        set_unavailable(true);
        set_sources(null);
      } else {
        set_error(classify_supplier_error(cause).text);
      }
    } finally {
      set_busy(false);
    }
  }

  async function refresh(source: string) {
    set_busy(true);
    set_error(null);
    try {
      const updated = await supplier_catalog_refresh(source);
      set_sources((current) =>
        (current ?? []).map((entry) => (entry.source === updated.source ? updated : entry)),
      );
    } catch (cause) {
      const info = classify_supplier_error(cause);
      set_error(info.kind === 'feature_unavailable' ? t('suppliers.catalog.unavailable') : info.text);
    } finally {
      set_busy(false);
    }
  }

  return (
    <section className="supplier_panel" aria-label={t('suppliers.catalog.title')} data-testid="suppliers_catalog">
      <h3>{t('suppliers.catalog.title')}</h3>
      <p className="supplier_hint">{t('suppliers.catalog.description')}</p>
      <div className="supplier_row">
        <button type="button" className="btn" disabled={busy} data-testid="suppliers_catalog_load" onClick={() => void load()}>
          {t('suppliers.refresh')}
        </button>
      </div>
      {unavailable && (
        <div role="status" className="supplier_notice" data-testid="suppliers_catalog_unavailable">
          {t('suppliers.catalog.unavailable')}
        </div>
      )}
      {error && <div role="alert" className="supplier_error" data-testid="suppliers_catalog_error">{error}</div>}
      {sources && (
        <table className="supplier_table" data-testid="suppliers_catalog_table">
          <thead>
            <tr>
              <th>{t('suppliers.models.source')}</th>
              <th>{t('suppliers.catalog.fetched', { when: '' })}</th>
              <th>{t('suppliers.catalog.stale')}</th>
              <th />
            </tr>
          </thead>
          <tbody>
            {sources.map((entry) => (
              <tr key={entry.source}>
                <td>{t(`suppliers.catalog.source.${entry.source}`)}</td>
                <td>
                  {entry.fetchedAt === null
                    ? t('suppliers.catalog.never')
                    : new Date(entry.fetchedAt * 1000).toISOString()}
                </td>
                <td>
                  {entry.stale ? (
                    <span className="supplier_tag warn">{t('suppliers.catalog.stale')}</span>
                  ) : (
                    <span className="supplier_tag">—</span>
                  )}
                  {entry.lastError !== null && (
                    <span className="supplier_hint"> {t('suppliers.catalog.error', { code: entry.lastError })}</span>
                  )}
                  {entry.incompleteRows !== undefined && entry.incompleteRows > 0 && (
                    <span className="supplier_hint"> {t('suppliers.catalog.incomplete_rows', { count: entry.incompleteRows })}</span>
                  )}
                </td>
                <td>
                  <button type="button" className="btn small" disabled={busy} data-testid={`suppliers_catalog_refresh_${entry.source}`} onClick={() => void refresh(entry.source)}>
                    {t('suppliers.catalog.refresh')}
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      <p className="supplier_hint">{t('suppliers.catalog.note')}</p>
      <Supplier_assessment />
    </section>
  );
}
