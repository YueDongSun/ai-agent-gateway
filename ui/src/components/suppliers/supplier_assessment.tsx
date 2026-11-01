import { useState } from 'react';
import { useT } from '../../lib/ctx.js';
import {
  classify_supplier_error,
  model_catalog_model,
  type Model_arena_assessment,
  type Model_catalog_entry,
} from '../../lib/api/suppliers.js';

const assessment_basis_keys: Record<string, string> = {
  manual_override: 'manualOverride',
  arena_evidence: 'arenaEvidence',
  coding_evidence: 'codingEvidence',
  family_ordinal: 'familyOrdinal',
  family_role: 'familyRole',
  name_inference: 'nameInference',
  pending: 'pending',
};

/** Keep published ranking facts separate from the active-supply routing tier. */
export function Supplier_arena_evidence({ arena }: { arena: Model_arena_assessment }) {
  const t = useT();
  const cohort_key = arena.comparedOfferedModels === null
    ? 'suppliers.assessment.arena.published_cohort'
    : 'suppliers.assessment.arena.offered_cohort';

  return (
    <div data-testid="suppliers_assessment_arena">
      <table className="supplier_table">
        <tbody>
          <tr><th scope="row">{t('suppliers.assessment.arena.model')}</th><td><code>{arena.modelName}</code></td></tr>
          <tr><th scope="row">{t('suppliers.assessment.arena.published')}</th><td>{arena.publishedDate}</td></tr>
          <tr><th scope="row">{t('suppliers.assessment.arena.rank')}</th><td>{t('suppliers.assessment.arena.rank_value', { rank: arena.rank, total: arena.totalModels })}</td></tr>
          <tr><th scope="row">{t('suppliers.assessment.arena.rating')}</th><td>{arena.rating}</td></tr>
          <tr><th scope="row">{t('suppliers.assessment.arena.interval')}</th><td>{arena.ratingLower}–{arena.ratingUpper}</td></tr>
          <tr><th scope="row">{t('suppliers.assessment.arena.votes')}</th><td>{arena.voteCount}</td></tr>
          <tr><th scope="row">{t('suppliers.assessment.arena.category')}</th><td>{arena.category}</td></tr>
          <tr><th scope="row">{t('suppliers.assessment.condition')}</th><td>
            {arena.condition ?? t('suppliers.assessment.unknown')}
            {arena.conditionAmbiguous && <span className="supplier_tag warn">{t('suppliers.assessment.arena.ambiguous')}</span>}
          </td></tr>
          <tr><th scope="row">{t('suppliers.assessment.arena.source')}</th><td>
            <a href={arena.sourceUrl} target="_blank" rel="noreferrer">{t(`suppliers.catalog.source.${arena.sourceId}`)}</a>
            {' · '}<code>{arena.sourceConfig}</code>{' / '}<code>{arena.sourceSplit}</code>
          </td></tr>
        </tbody>
      </table>
      <p className="supplier_hint" data-testid="suppliers_assessment_arena_cohort">
        {t(cohort_key, { tier: arena.tier, count: arena.comparedOfferedModels ?? 0 })}
        {' '}<code>{arena.policy}</code>
      </p>
      <p className="supplier_hint">{t('suppliers.assessment.arena.note')}</p>
    </div>
  );
}

export function Supplier_assessment() {
  const t = useT();
  const [selector, set_selector] = useState('alias:fable');
  const [entry, set_entry] = useState<Model_catalog_entry | null>(null);
  const [busy, set_busy] = useState(false);
  const [error, set_error] = useState<string | null>(null);

  async function lookup() {
    const selected = selector.trim();
    if (!selected || busy) return;
    set_busy(true);
    set_entry(null);
    set_error(null);
    try {
      set_entry(await model_catalog_model(selected));
    } catch (cause) {
      const info = classify_supplier_error(cause);
      set_error(info.kind === 'feature_unavailable' ? t('suppliers.assessment.unavailable') : info.text);
    } finally {
      set_busy(false);
    }
  }

  function fact(value: boolean | number | null): string {
    if (value === null) return t('suppliers.assessment.unknown');
    if (typeof value === 'boolean') return t(value ? 'suppliers.assessment.yes' : 'suppliers.assessment.no');
    return String(value);
  }

  return (
    <details className="supplier_details" data-testid="suppliers_assessment">
      <summary>{t('suppliers.assessment.title')}</summary>
      <p className="supplier_hint">{t('suppliers.assessment.note')}</p>
      <form className="supplier_row" onSubmit={(event) => { event.preventDefault(); void lookup(); }}>
        <label className="supplier_field">
          <span>{t('suppliers.assessment.selector')}</span>
          <input
            value={selector}
            maxLength={256}
            disabled={busy}
            data-testid="suppliers_assessment_selector"
            onChange={(event) => { set_selector(event.target.value); set_entry(null); set_error(null); }}
          />
        </label>
        <button type="submit" className="btn" disabled={busy || !selector.trim()} data-testid="suppliers_assessment_lookup">
          {t('suppliers.assessment.lookup')}
        </button>
      </form>
      {error && <div role="alert" className="supplier_error" data-testid="suppliers_assessment_error">{error}</div>}
      {entry && (
        <div data-testid="suppliers_assessment_result">
          <p>
            <code>{entry.selector}</code>{' · '}
            <span className={`supplier_tag${entry.catalogOnly ? ' warn' : ''}`}>
              {t(entry.catalogOnly ? 'suppliers.assessment.catalog_only' : 'suppliers.assessment.offered')}
            </span>
          </p>
          {entry.assessment ? (
            <>
              <table className="supplier_table">
                <thead>
                  <tr>
                    <th>{t('suppliers.models.tier')}</th>
                    <th>{t('suppliers.assessment.basis')}</th>
                    <th>{t('suppliers.assessment.confidence')}</th>
                    <th>{t('suppliers.assessment.condition')}</th>
                  </tr>
                </thead>
                <tbody>
                  <tr>
                    <td>{fact(entry.assessment.tier)}</td>
                    <td>{t(`suppliers.assessment.basis.${entry.assessment.basis}`)}</td>
                    <td>{entry.assessment.confidence === null ? t('suppliers.assessment.unknown') : `${Math.round(entry.assessment.confidence * 100)}%`}</td>
                    <td>{entry.assessment.condition ?? t('suppliers.assessment.unknown')}</td>
                  </tr>
                </tbody>
              </table>
              <p className="supplier_hint">{t('suppliers.assessment.confidence_note')}</p>
              {entry.assessment.arena && <Supplier_arena_evidence arena={entry.assessment.arena} />}
              {entry.assessment.coding && (
                <p className="supplier_hint">
                  HumanEval+ {entry.assessment.coding.humanevalPlus}%; MBPP+ {entry.assessment.coding.mbppPlus}%;{' '}
                  prompted={String(entry.assessment.coding.prompted)}
                </p>
              )}
            </>
          ) : (
            <p className="supplier_hint">{t('suppliers.assessment.alias_tier', { tier: entry.aliasTier ?? '?' })}</p>
          )}
          {entry.capabilities && (
            <table className="supplier_table" data-testid="suppliers_assessment_capabilities">
              <thead>
                <tr>
                  <th>{t('suppliers.models.tools')}</th>
                  <th>{t('suppliers.models.images')}</th>
                  <th>{t('suppliers.models.context')}</th>
                  <th>{t('suppliers.models.max_output')}</th>
                </tr>
              </thead>
              <tbody>
                <tr>
                  <td>{fact(entry.capabilities.supportsTools)}</td>
                  <td>{fact(entry.capabilities.supportsImages)}</td>
                  <td>{fact(entry.capabilities.contextWindow)}</td>
                  <td>{fact(entry.capabilities.maxOutputTokens)}</td>
                </tr>
              </tbody>
            </table>
          )}
          <ul>
            {entry.sources.map((source) => (
              <li key={source.source}>
                {t(`suppliers.catalog.source.${source.source}`)}{' · '}
                {source.stale ? t('suppliers.catalog.stale') : t('suppliers.assessment.cached')}
                {source.incompleteRows !== undefined && source.incompleteRows > 0 && (
                  <span> · {t('suppliers.catalog.incomplete_rows', { count: source.incompleteRows })}</span>
                )}
              </li>
            ))}
          </ul>
          {entry.selectorKind === 'precise' && (
            <p className="supplier_hint">{t('suppliers.assessment.offer_count', { count: entry.offeredBy.length })}</p>
          )}
          {entry.aliasPreview.length > 0 ? (
            <>
              <p className="supplier_hint">{t('suppliers.assessment.alias_preview', { tier: entry.aliasTier ?? '?' })}</p>
              <table className="supplier_table" data-testid="suppliers_assessment_alias_preview">
                <thead>
                  <tr>
                    <th>{t('suppliers.models.model_id')}</th>
                    <th>{t('suppliers.models.tier')}</th>
                    <th>{t('suppliers.assessment.basis')}</th>
                    <th>{t('suppliers.assessment.condition')}</th>
                  </tr>
                </thead>
                <tbody>
                  {entry.aliasPreview.map((row) => (
                    <tr key={`${row.endpointRef}|${row.modelId}`}>
                      <td><code>{row.modelId}</code></td>
                      <td>{fact(row.tier)} {row.downgraded && <span className="supplier_tag warn">{t('suppliers.trace.downgrade')}</span>}</td>
                      <td>{assessment_basis_keys[row.basis]
                        ? t(`suppliers.assessment.basis.${assessment_basis_keys[row.basis]}`)
                        : <code>{row.basis}</code>}</td>
                      <td><code>{row.reason}</code></td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </>
          ) : entry.selectorKind === 'alias' ? (
            <p className="supplier_empty_note">{t('suppliers.routing.none')}</p>
          ) : null}
        </div>
      )}
    </details>
  );
}
