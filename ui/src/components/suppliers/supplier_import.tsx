/// cc-switch import dialog: read-only preview of the derived plan, then a
/// confirmed whole-plan apply.
///
/// Mirrors `src-tauri/src/commands/supplier_import.rs`: the preview is
/// redacted and zero-write, and apply passes the opaque self-certifying
/// `planId` which binds the source fingerprint and the target generation.
/// Bad rows are listed as problems, never dropped silently. If the commands
/// are not registered yet the dialog shows a safe "feature unavailable"
/// state and never fabricates a preview or a fake success.
import { useRef, useState } from 'react';
import { use_accessible_dialog } from '../use_accessible_dialog.js';
import { use_navigation_block } from '../../hooks/use_navigation_block.js';
import { useT } from '../../lib/ctx.js';
import { backend_error_text } from '../../lib/backend_error.js';
import {
  classify_supplier_error,
  is_supplier_feature_unavailable,
  supplier_import_apply,
  supplier_import_preview,
  supplier_protocol_label_en,
  supplier_discovery_notice,
  type Supplier_import_preview,
} from '../../lib/api/suppliers.js';
import './suppliers_page.css';

export interface Supplier_import_dialog_props {
  generation: number;
  on_close: () => void;
  on_applied: (generation: number, message: string) => void;
  on_reload: () => void;
}

export function Supplier_import_dialog(props: Supplier_import_dialog_props) {
  const t = useT();
  const [preview, set_preview] = useState<Supplier_import_preview | null>(null);
  const [busy, set_busy] = useState(false);
  const operation_running = useRef(false);
  const [unavailable, set_unavailable] = useState(false);
  const [error, set_error] = useState<string | null>(null);
  const request_close = () => { if (!operation_running.current) props.on_close(); };
  const dialog_ref = use_accessible_dialog(request_close, busy);
  use_navigation_block(busy, () => set_error(t('suppliers.import.navigation_blocked')));

  async function run_preview() {
    if (operation_running.current) return;
    operation_running.current = true;
    set_busy(true);
    set_error(null);
    set_unavailable(false);
    try {
      set_preview(await supplier_import_preview());
    } catch (cause) {
      if (is_supplier_feature_unavailable(cause)) {
        set_unavailable(true);
        set_preview(null);
      } else {
        set_error(classify_supplier_error(cause).text);
      }
    } finally {
      operation_running.current = false;
      set_busy(false);
    }
  }

  async function run_apply() {
    if (!preview || operation_running.current) return;
    operation_running.current = true;
    set_busy(true);
    set_error(null);
    try {
      const result = await supplier_import_apply(preview.planId);
      const discovery_notice = supplier_discovery_notice(result.discoveryError, t);
      if (result.setupError) {
        props.on_applied(result.appliedGeneration, t('suppliers.setup_partial', { code: backend_error_text(classify_supplier_error(result.setupError).text, t) }) + discovery_notice);
      } else if (result.alreadyApplied) {
        props.on_applied(result.appliedGeneration, t('suppliers.import.already_applied') + discovery_notice);
      } else {
        props.on_applied(
          result.appliedGeneration,
          t('suppliers.import.applied', {
            created: result.createdCount,
            refreshed: result.refreshedCount,
            generation: result.appliedGeneration,
          }) + discovery_notice,
        );
      }
      props.on_reload();
    } catch (cause) {
      const info = classify_supplier_error(cause);
      set_error(info.kind === 'generation_conflict' ? t('suppliers.status.apply_failed') : info.text);
      if (info.kind === 'generation_conflict') props.on_reload();
    } finally {
      operation_running.current = false;
      set_busy(false);
    }
  }

  return (
    <div className="supplier_backdrop" role="presentation">
      <div ref={dialog_ref} tabIndex={-1} className="supplier_dialog wide" role="dialog" aria-modal="true" aria-label={t('suppliers.import.title')} data-testid="suppliers_import_dialog">
        <div className="supplier_dialog_heading">
          <h2>{t('suppliers.import.title')}</h2>
          <button type="button" className="btn small" disabled={busy} data-testid="suppliers_import_close" onClick={request_close}>
            {t('suppliers.import.close')}
          </button>
        </div>
        <p className="supplier_hint">{t('suppliers.import.description')}</p>
        <p className="supplier_hint">{t('suppliers.import.plan_note')}</p>
        <p className="supplier_hint" data-testid="supplier-discovery-import-help">{t('suppliers.discovery.import_help')}</p>
        {unavailable && (
          <div role="status" className="supplier_notice" data-testid="suppliers_import_unavailable">
            {t('suppliers.import.unavailable')}
          </div>
        )}
        {error && <div role="alert" className="supplier_error" data-testid="suppliers_import_error">{backend_error_text(error, t)}</div>}
        <div className="supplier_row">
          <button type="button" className="btn" disabled={busy} data-testid="suppliers_import_preview" onClick={() => void run_preview()}>
            {t('suppliers.import.preview')}
          </button>
          <button type="button" className="btn primary" disabled={busy || !preview} data-testid="suppliers_import_apply" onClick={() => void run_apply()}>
            {t('suppliers.import.apply')}
          </button>
        </div>
        {preview && (
          <div data-testid="suppliers_import_preview_panel">
            <p className="supplier_notice" role="status" data-testid="suppliers_import_account_policy">
              Static API-key endpoints only. Account-login imports are excluded.
              {` Excluded: ${(preview as Supplier_import_preview & { excludedOauthCount?: number }).excludedOauthCount ?? 0}.`}
            </p>
            <p className="supplier_hint">{preview.sourceLabel}</p>
            <p className="supplier_hint">{t('suppliers.import.demo_note')}</p>
            <p className="supplier_hint">{t('suppliers.import.total_rows', { count: preview.totalRows })}</p>
            <p className="supplier_hint" data-testid="suppliers_import_merged_summary">{new Set(preview.plans.map((entry) => entry.supplierId)).size} providers · {preview.plans.length} protocol endpoints. Matching account endpoints are merged.</p>
            <table className="supplier_table">
              <thead>
                <tr>
                  <th>{t('suppliers.editor.name')}</th>
                  <th>{t('suppliers.endpoints.protocol')}</th>
                  <th>{t('suppliers.models.source')}</th>
                  <th>{t('suppliers.models.tier')}</th>
                </tr>
              </thead>
              <tbody>
                {preview.plans.map((entry) => (
                  <tr key={`${entry.supplierId}:${entry.protocol}:${entry.sourceId}`} data-testid={`suppliers_import_row_${entry.sourceId}`}>
                    <td>
                      <strong>{entry.name}</strong>
                      <br />
                      <small className="supplier_hint">
                        <code>{entry.sourceAppType}</code> · <code>{entry.sourceId}</code>
                        {entry.apiKeyConfigured ? ` · ${t('suppliers.card.key_configured')}` : ''}
                        {entry.legacyProjectionPlanned ? ` · ${t('suppliers.card.legacy_projection')}` : ''}
                      </small>
                    </td>
                    <td>{supplier_protocol_label_en(entry.protocol)}<ul className="supplier_hint">{[...new Set([entry.baseUrl, ...entry.sourceEndpointUrls].filter((url): url is string => Boolean(url)))].map((url) => <li key={url}><code>{url}</code></li>)}</ul></td>
                    <td>
                      <span className={`supplier_tag${entry.change === 'unchanged' ? '' : ' warn'}`} data-testid={`suppliers_import_change_${entry.sourceId}`}>
                        {t(`suppliers.import.change.${entry.change}`)}
                      </span>
                    </td>
                    <td>
                      <code>{entry.modelIds.join(', ')}</code>
                      <br />
                      <small className="supplier_hint">{t('suppliers.card.models', { count: entry.modelCount })}</small>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
            {preview.problems.length > 0 && (
              <>
                <h4>{t('suppliers.import.problems')}</h4>
                <ul data-testid="suppliers_import_problems">
                  {preview.problems.map((problem, index) => (
                    <li key={`${problem.code}|${index}`} className="supplier_hint">
                      <code>{problem.code}</code>
                      {problem.sourceAppType || problem.sourceId
                        ? ` — ${problem.sourceAppType ?? '?'}:${problem.sourceId ?? '?'}`
                        : ''}{' '}
                      — {problem.detail}
                    </li>
                  ))}
                </ul>
              </>
            )}
          </div>
        )}
      </div>
    </div>
  );
}
