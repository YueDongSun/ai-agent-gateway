import { Fragment, memo, useCallback, useState } from 'react';
import './providers_panel.css';
import { useT } from '../../lib/ctx';
import { useProvidersActions, useProvidersData } from './providers_state.js';
import { ProviderCard } from './provider_card.js';

/** Unified provider card list — the single failover-chain surface for an
 *  app page. Presets render first (drag order = failover priority,
 *  persisted via `provider_reorder`); standalone upstreams follow after a
 *  divider in pool order. Drag uses pointer events + elementFromPoint
 *  (HTML5 DnD is unreliable in WebView2); the handle also carries a
 *  keyboard reorder path (↑/↓/Home/End). */
export const ProvidersPanel = memo(function ProvidersPanel() {
  const t = useT();
  const d = useProvidersData();
  const a = useProvidersActions();
  const [dragIdx, setDragIdx] = useState<number | null>(null);
  const [overIdx, setOverIdx] = useState<number | null>(null);

  /** Presets always occupy the leading run of the card list, so the
   *  array index is the preset index for preset cards. */
  const presetCount = d.cards.reduce(
    (n, c, i) => (c.kind === 'preset' && i === n ? n + 1 : n),
    0,
  );
  const hasStandalone = d.cards.some((c) => c.kind === 'standalone');

  /** Resolve the preset card under a pointer position, or null. */
  const cardRowAt = (x: number, y: number): HTMLElement | null => {
    const el = document.elementFromPoint(x, y);
    const card = el?.closest?.('.provider-card[data-preset-idx]') as HTMLElement | null;
    return card;
  };

  const handleDragStart = useCallback((fromIdx: number, e: React.PointerEvent) => {
    e.preventDefault();
    (e.currentTarget as HTMLElement).setPointerCapture?.(e.pointerId);
    setDragIdx(fromIdx);
    setOverIdx(fromIdx);
  }, []);

  const handleDragMove = useCallback(
    (e: React.PointerEvent) => {
      if (dragIdx === null) return;
      const row = cardRowAt(e.clientX, e.clientY);
      if (!row) return;
      const idx = Number(row.dataset.presetIdx);
      if (!Number.isNaN(idx)) setOverIdx(idx);
    },
    [dragIdx],
  );

  const handleDragEnd = useCallback(
    (e: React.PointerEvent) => {
      if (dragIdx === null) return;
      const row = cardRowAt(e.clientX, e.clientY);
      const toIdx = row ? Number(row.dataset.presetIdx) : dragIdx;
      (e.currentTarget as HTMLElement).releasePointerCapture?.(e.pointerId);
      setDragIdx(null);
      setOverIdx(null);
      if (toIdx !== dragIdx) a.onReorder(dragIdx, toIdx);
    },
    [dragIdx, a],
  );

  /** Keyboard reorder for the drag handle (↑/↓ move, Home/End to the
   *  ends). Mirrors the pointer-drag path so the handle isn't fake
   *  accessible. */
  const handleKeyDown = useCallback(
    (idx: number, e: React.KeyboardEvent) => {
      let toIdx: number | null = null;
      if (e.key === 'ArrowUp') toIdx = Math.max(0, idx - 1);
      else if (e.key === 'ArrowDown') toIdx = Math.min(presetCount - 1, idx + 1);
      else if (e.key === 'Home') toIdx = 0;
      else if (e.key === 'End') toIdx = presetCount - 1;
      else return;
      e.preventDefault();
      e.stopPropagation();
      if (toIdx === null || toIdx === idx) return;
      a.onReorder(idx, toIdx);
      // Keep focus on the moved handle (same visual position) so ↑/↓ repeat.
      requestAnimationFrame(() => {
        document
          .querySelector<HTMLElement>(
            `.provider-card[data-preset-idx="${toIdx}"] .provider-card-drag`,
          )
          ?.focus();
      });
    },
    [presetCount, a],
  );

  return (
    <div className="card providers-panel" data-testid="providers-panel">
      <div className="card-head">
        <h3>{t('providers.title')}</h3>
        <div className="header-actions">
          <button
            className="btn small"
            onClick={a.onRefresh}
            data-testid="providers-refresh"
          >
            {t('common.refresh')}
          </button>
          <button
            className="btn small"
            onClick={a.onAdd}
            disabled={d.cfg.detectOnly !== undefined}
            title={d.cfg.detectOnly?.reason ?? ''}
            data-testid="providers-add"
          >
            {t('providers.add')}
          </button>
        </div>
      </div>
      {d.cfg.detectOnly && (
        <p className="muted small" data-testid="detect-only-notice">
          {d.cfg.detectOnly.reason}
        </p>
      )}
      {d.cfg.detectOnly === undefined && (
        <p className="muted small">{t('providers.desc')}</p>
      )}
      {d.cards.length === 0 ? (
        <p className="muted">{t('providers.empty')}</p>
      ) : (
        <ul className="providers-list">
          {d.cards.map((card, i) => {
            const presetIdx = card.kind === 'preset' ? i : null;
            return (
              <Fragment key={card.key}>
                {card.kind === 'standalone' && i === presetCount && (
                  <li className="standalone-divider">{t('providers.standalone')}</li>
                )}
                <ProviderCard
                  card={card}
                  presetIdx={presetIdx}
                  isDragSource={presetIdx !== null && dragIdx === presetIdx}
                  isDragOver={
                    presetIdx !== null && overIdx === presetIdx && dragIdx !== null
                  }
                  onDragStart={handleDragStart}
                  onDragMove={handleDragMove}
                  onDragEnd={handleDragEnd}
                  onKeyDown={handleKeyDown}
                />
              </Fragment>
            );
          })}
        </ul>
      )}
    </div>
  );
});
