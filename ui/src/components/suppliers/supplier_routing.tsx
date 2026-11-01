/// Client-side preview of the supplier selection rules: capability aliases vs
/// exact ids, hard capability filtering, tier ordering, and traceable
/// downgrades. Live per-request decisions land in the trace panel; this panel
/// explains what would happen before anything is sent.
import { useEffect, useMemo, useState } from 'react';
import { useT } from '../../lib/ctx.js';
import {
  CAPABILITY_ALIASES,
  capability_tier_for_alias,
  model_catalog_model,
  preview_routing,
  supplier_protocol_label_en,
  type Model_catalog_entry,
  type Routing_candidate,
  type Supplier_view,
} from '../../lib/api/suppliers.js';
import './suppliers_page.css';

const routing_basis_labels: Record<string, string> = {
  manual_override: 'manualOverride',
  arena_evidence: 'arenaEvidence',
  coding_evidence: 'codingEvidence',
  family_ordinal: 'familyOrdinal',
  family_role: 'familyRole',
  name_inference: 'nameInference',
  pending: 'pending',
};

export interface Supplier_routing_props {
  suppliers: Supplier_view[];
}

export function Supplier_routing(props: Supplier_routing_props) {
  const t = useT();
  const [selection, set_selection] = useState<string>(CAPABILITY_ALIASES[0]);
  const [needs_tools, set_needs_tools] = useState(true);
  const [needs_images, set_needs_images] = useState(false);
  const [queue_entry, set_queue_entry] = useState<Model_catalog_entry | null>(null);
  const [queue_unavailable, set_queue_unavailable] = useState(false);

  // The authoritative queue comes from the backend planner; the local table
  // above stays as an offline explanation of the selection rules.
  useEffect(() => {
    const selected = selection.trim();
    if (!selected) return;
    let cancelled = false;
    set_queue_entry(null);
    set_queue_unavailable(false);
    model_catalog_model(selected)
      .then((entry) => { if (!cancelled) set_queue_entry(entry); })
      .catch(() => { if (!cancelled) set_queue_unavailable(true); });
    return () => { cancelled = true; };
  }, [selection]);

  const all_models = useMemo(() => {
    const rows: Array<{ id: string; label: string }> = [];
    for (const supplier of props.suppliers) {
      for (const endpoint of supplier.endpoints) {
        for (const model of endpoint.models) {
          rows.push({ id: model.modelId, label: `${model.modelId} · ${supplier.name}` });
        }
      }
    }
    return rows;
  }, [props.suppliers]);

  const candidates = useMemo<Routing_candidate[]>(() => {
    const rows: Routing_candidate[] = [];
    for (const supplier of props.suppliers) {
      for (const endpoint of supplier.endpoints) {
        for (const model of endpoint.models) {
          rows.push({
            supplierId: supplier.id,
            supplierName: supplier.name,
            supplierEnabled: supplier.enabled,
            supplierPriority: supplier.priority,
            endpointProtocol: endpoint.protocol,
            endpointEnabled: endpoint.enabled,
            endpointPriority: endpoint.priority,
            endpointConfigured: endpoint.effectiveBaseUrl !== null,
            model: model,
          });
        }
      }
    }
    return rows;
  }, [props.suppliers]);

  const is_alias = (CAPABILITY_ALIASES as readonly string[]).includes(selection);
  const min_tier = capability_tier_for_alias(selection);
  const preview = preview_routing(candidates, {
    needsTools: needs_tools,
    needsImages: needs_images,
    minTier: is_alias ? min_tier : null,
  });
  const eligible = is_alias
    ? preview.eligible
    : preview.eligible.filter((row) => row.model.modelId === selection);

  return (
    <section className="supplier_panel" aria-label={t('suppliers.routing.title')} data-testid="suppliers_routing">
      <h3>{t('suppliers.routing.title')}</h3>
      <div className="supplier_row">
        <label className="supplier_field">
          <span>{t('suppliers.routing.alias_label')}</span>
          <select value={selection} data-testid="suppliers_routing_alias" onChange={(event) => set_selection(event.target.value)}>
            <optgroup label={t('suppliers.routing.alias_capability')}>
              {CAPABILITY_ALIASES.map((alias) => (
                <option key={alias} value={alias}>{alias}</option>
              ))}
            </optgroup>
            <optgroup label={t('suppliers.routing.alias_exacted')}>
              {all_models.map((model) => (
                <option key={model.id} value={model.id}>{model.label}</option>
              ))}
            </optgroup>
          </select>
        </label>
        <label className="supplier_check">
          <input type="checkbox" checked={needs_tools} data-testid="suppliers_routing_tools" onChange={(event) => set_needs_tools(event.target.checked)} />
          {t('suppliers.routing.needs_tools')}
        </label>
        <label className="supplier_check">
          <input type="checkbox" checked={needs_images} data-testid="suppliers_routing_images" onChange={(event) => set_needs_images(event.target.checked)} />
          {t('suppliers.routing.needs_images')}
        </label>
      </div>
      <p className="supplier_hint">{t('suppliers.routing.alias_hint')}</p>
      <p className="supplier_hint" data-testid="suppliers_routing_mode">
        {is_alias ? t('suppliers.routing.alias_capability') : t('suppliers.routing.alias_exacted')}
      </p>
      <h4>{t('suppliers.routing.eligible')}</h4>
      {eligible.length === 0 ? (
        <p className="supplier_empty_note" data-testid="suppliers_routing_none">{t('suppliers.routing.none')}</p>
      ) : (
        <table className="supplier_table" data-testid="suppliers_routing_table">
          <thead>
            <tr>
              <th>{t('suppliers.models.model_id')}</th>
              <th>{t('suppliers.overview.suppliers')}</th>
              <th>{t('suppliers.endpoints.protocol')}</th>
              <th>{t('suppliers.models.tier')}</th>
              <th>{t('suppliers.models.source')}</th>
            </tr>
          </thead>
          <tbody>
            {eligible.map((row) => (
              <tr key={`${row.supplierId}|${row.endpointProtocol}|${row.model.modelId}`}>
                <td><code>{row.model.modelId}</code></td>
                <td>{row.supplierName}</td>
                <td>{supplier_protocol_label_en(row.endpointProtocol)}</td>
                <td>
                  {row.tier === null ? '?' : row.tier}
                  <span className="supplier_tag">{t(`suppliers.models.tier_origin.${row.tier_origin}`)}</span>
                </td>
                <td>{row.model.source}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      {preview.excluded.length > 0 && (
        <details className="supplier_details" data-testid="suppliers_routing_excluded">
          <summary>{t('suppliers.routing.excluded')} ({preview.excluded.length})</summary>
          <ul>
            {preview.excluded.map((row) => (
              <li key={`${row.candidate.supplierId}|${row.candidate.endpointProtocol}|${row.candidate.model.modelId}`}>
                <code>{row.candidate.model.modelId}</code> — {t(`suppliers.routing.excluded.${row.reason}`)}
              </li>
            ))}
          </ul>
        </details>
      )}
      <p className="supplier_hint">{t('suppliers.routing.preview_note')}</p>
      <p className="supplier_hint">{t('suppliers.routing.exact_note')}</p>
      <p className="supplier_hint">{t('suppliers.routing.downgrade_note')}</p>
      <h4>{t('suppliers.routing.backend_queue')}</h4>
      {queue_unavailable ? (
        <p className="supplier_hint">{t('suppliers.routing.backend_unavailable')}</p>
      ) : queue_entry === null ? (
        <p className="supplier_hint">{t('suppliers.routing.backend_loading')}</p>
      ) : queue_entry.aliasPreview.length === 0 ? (
        <p className="supplier_empty_note" data-testid="suppliers_routing_backend_none">{t('suppliers.routing.none')}</p>
      ) : (
        <table className="supplier_table" data-testid="suppliers_routing_backend_queue">
          <thead>
            <tr>
              <th>{t('suppliers.models.model_id')}</th>
              <th>{t('suppliers.models.tier')}</th>
              <th>{t('suppliers.assessment.basis')}</th>
              <th>{t('suppliers.assessment.condition')}</th>
            </tr>
          </thead>
          <tbody>
            {queue_entry.aliasPreview.map((row) => (
              <tr key={`${row.endpointRef}|${row.modelId}`}>
                <td><code>{row.modelId}</code></td>
                <td>
                  {row.tier === null ? '?' : row.tier}
                  {row.downgraded && <span className="supplier_tag warn">{t('suppliers.trace.downgrade')}</span>}
                </td>
                <td>{routing_basis_labels[row.basis]
                  ? t(`suppliers.assessment.basis.${routing_basis_labels[row.basis]}`)
                  : <code>{row.basis}</code>}</td>
                <td><code>{row.reason}</code></td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </section>
  );
}
