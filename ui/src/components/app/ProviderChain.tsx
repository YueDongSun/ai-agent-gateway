import { useState } from 'react';
import { useT } from '../../lib/ctx';
import type { AppPageConfig } from '../../lib/appPages';
import type { Provider } from '../../lib/api/types';

interface ProviderChainProps {
  cfg: AppPageConfig;
  providers: Provider[];
  current: string | null;
  busy: string | null;
  onAdd: () => void;
  onEdit: (p: Provider) => void;
  onDelete: (id: string) => void;
  onSwitch: (id: string) => void;
  onToggleQueue: (p: Provider) => void;
  onReorder: (fromIdx: number, toIdx: number) => void;
}

/** Provider chain card - drag-reorderable, per-row switch / edit / delete
 * buttons. Hidden for Claude (the upstream pool card replaces it). Local
 * drag/over state stays in the component to avoid tying AppPage state to
 * HTML5 dragover timing. Extracted from AppPage.tsx in the file-structure
 * refactor. */
export function ProviderChain({
  cfg,
  providers,
  current,
  busy,
  onAdd,
  onEdit,
  onDelete,
  onSwitch,
  onToggleQueue,
  onReorder,
}: ProviderChainProps) {
  const t = useT();
  const [dragIdx, setDragIdx] = useState<number | null>(null);
  const [overIdx, setOverIdx] = useState<number | null>(null);

  return (
    <div className="card chain-card">
      <div className="card-head">
        <h3>{t('chain.title')}</h3>
        <button className="btn small" onClick={onAdd}>
          {t('chain.add')}
        </button>
      </div>
      <p className="muted small">{t('chain.desc')}</p>
      {providers.length === 0 ? (
        <p className="muted">{t('chain.empty')}</p>
      ) : (
        <ul className="provider-list">
          {providers.map((p, i) => {
            const pcfg = p.settingsConfig as Record<string, unknown>;
            const isCurrent = p.id === current;
            const url = cfg.cardUrl(pcfg);
            return (
              <li
                key={p.id}
                className={
                  isCurrent
                    ? 'provider-row current'
                    : overIdx === i
                      ? 'provider-row drag-over'
                      : 'provider-row'
                }
                draggable
                onDragStart={() => setDragIdx(i)}
                onDragOver={(e) => {
                  e.preventDefault();
                  setOverIdx(i);
                }}
                onDragLeave={() => setOverIdx(null)}
                onDrop={(e) => {
                  e.preventDefault();
                  if (dragIdx !== null) onReorder(dragIdx, i);
                  setDragIdx(null);
                  setOverIdx(null);
                }}
                onDragEnd={() => {
                  setDragIdx(null);
                  setOverIdx(null);
                }}
              >
                <span className="drag-handle" title={t('chain.dragHint')}>⋮⋮</span>
                <div className="provider-info">
                  <span className="provider-name">
                    {p.name || p.id}
                    {isCurrent && <span className="tag tag-current">{t('chain.current')}</span>}
                    {!p.inFailoverQueue && (
                      <span className="tag tag-paused">{t('chain.out')}</span>
                    )}
                  </span>
                  <span className="provider-url">
                    {url || '—'}
                    {cfg.cardMeta(pcfg) ? ` · ${cfg.cardMeta(pcfg)}` : ''}
                  </span>
                </div>
                <div className="provider-actions">
                  <button
                    className={p.inFailoverQueue ? 'btn small' : 'btn small primary'}
                    title={t('chain.toggle.help')}
                    onClick={() => onToggleQueue(p)}
                  >
                    {p.inFailoverQueue ? t('chain.toggle.out') : t('chain.toggle.in')}
                  </button>
                  <button
                    className="btn small primary"
                    disabled={isCurrent || busy === `switch-${p.id}`}
                    onClick={() => onSwitch(p.id)}
                  >
                    {busy === `switch-${p.id}`
                      ? '…'
                      : isCurrent
                        ? t('chain.inUse')
                        : t('chain.enable')}
                  </button>
                  <button className="btn small" onClick={() => onEdit(p)}>
                    {t('common.edit')}
                  </button>
                  <button className="btn small danger" onClick={() => onDelete(p.id)}>
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
