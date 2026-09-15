import { useT } from '../../lib/ctx';
import type { ProxyProvider } from '../../lib/api/proxy';
import { useProxyPool } from './state';

interface UpstreamListProps {
  onAdd: () => void;
  onEdit: (pp: ProxyProvider) => void;
  onDelete: (id: string) => void;
}

/** Editable upstream provider list card (full page width). Shows breaker
 * dots, pin tags, enable toggles, and model-mapping chips per row.
 * Provider / pin / breaker state come from `useProxyPool()`; only the
 * three CRUD callbacks come from AppPage. */
export function UpstreamList({ onAdd, onEdit, onDelete }: UpstreamListProps) {
  const t = useT();
  const {
    providers: proxyProviders,
    pinnedId,
    breakerOf,
    breakerOfClass,
    onToggleEnabled,
  } = useProxyPool();

  return (
    <div className="card">
      <div className="card-head">
        <h3>{t('claude.upstream.listTitle')}</h3>
        <div className="header-actions">
          <button className="btn small" onClick={onAdd}>
            {t('claude.upstream.add')}
          </button>
        </div>
      </div>
      {/* Round 1: explicit empty state - was rendering an empty <ul>. */}
      {proxyProviders.length === 0 ? (
        <p className="muted">{t('claude.upstream.empty')}</p>
      ) : (
        <ul className="provider-list">
          {proxyProviders.map((pp) => {
            const bs = breakerOf(pp.id);
            const isPinned = pinnedId === pp.id;
            const rowClass = [
              'provider-row',
              isPinned ? 'pinned' : '',
              pp.enabled ? '' : 'disabled',
            ]
              .filter(Boolean)
              .join(' ');
            return (
              <li key={pp.id} className={rowClass}>
                <div className="provider-info">
                  <span className="provider-name">
                    {isPinned && <span className="tag tag-pinned">{t('claude.upstream.pinnedTag')}</span>}
                    {!pp.enabled && <span className="tag tag-paused">{t('claude.upstream.disabledTag')}</span>}
                    {pp.name || pp.id}
                    {bs && <span className={`breaker-dot ${breakerOfClass(bs)}`} title={bs} />}
                  </span>
                  <span className="provider-url">
                    {pp.baseURL || '-'} · {pp.type}
                    {(pp.models?.length ?? 0) > 0 && (
                      <span
                        className="tag"
                        title={pp.models.join(', ')}
                        style={{ marginLeft: 8 }}
                      >
                        {t('claude.upstream.modelCount', { n: String(pp.models.length) })}
                      </span>
                    )}
                  </span>
                  {/* Show actual model mappings on the card */}
                  {Object.entries(pp.modelMapping ?? {}).length > 0 && (
                    <div className="provider-mappings">
                      {Object.entries(pp.modelMapping ?? {}).map(([display, upstream]) => (
                        <span key={display} className="mapping-tag">
                          {display} → {upstream}
                        </span>
                      ))}
                    </div>
                  )}
                </div>
                <div className="provider-actions">
                  <label
                    className="toggle-switch"
                    title={t('claude.upstream.enabled.help')}
                  >
                    <input
                      type="checkbox"
                      checked={pp.enabled}
                      onChange={(e) => onToggleEnabled(pp.id, e.target.checked)}
                      aria-label={t('claude.upstream.enabled.help')}
                    />
                    <span className="toggle-slider" />
                  </label>
                  <button className="btn small" onClick={() => onEdit(pp)}>
                    {t('common.edit')}
                  </button>
                  <button className="btn small danger" onClick={() => onDelete(pp.id)}>
                    {t('common.delete')}
                  </button>
                </div>
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}
