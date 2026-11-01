import { Fragment, memo, useCallback, useEffect, useRef, useState } from 'react';
import './providers_panel.css';
import { useCtx } from '../../../lib/ctx';
import {
  getRoutingMode,
  setRoutingMode,
  type RoutingMode,
} from '../../../lib/api/proxy.js';
import { useProvidersActions, useProvidersData } from './providers_state.js';
import { Preset_picker } from './preset_picker.js';
import { ProviderCard } from './provider_card.js';
import { get_provider_ui_copy } from './provider_ui_copy.js';
import { account_display_cards } from '../../../lib/account_display_policy.js';

const ROUTING_MODES: RoutingMode[] = ['order', 'rotate', 'weight', 'usage'];

/** Standalone-pool section divider with its add button. Rendered inline
 *  before the first standalone card, or as the trailing entry point when
 *  no standalone rows exist yet (fresh install). */
function standalone_divider(t: ReturnType<typeof useCtx>['t'], a: ReturnType<typeof useProvidersActions>, d: ReturnType<typeof useProvidersData>) {
  return (
    <li className="standalone-divider" data-testid="standalone-divider">
      <span>{t('providers.standalone')}</span>
      <button
        className="btn small"
        onClick={a.onAddStandalone}
        disabled={d.cfg.detectOnly !== undefined}
        title={d.cfg.detectOnly?.reason ?? ''}
        data-testid="providers-add-standalone"
      >
        {t('providers.addStandalone')}
      </button>
    </li>
  );
}

/** Unified provider card list — the single failover-chain surface for an
 *  app page. Presets render first (drag order = failover priority,
 *  persisted via `provider_reorder`); standalone upstreams follow after a
 *  divider in pool order. Drag uses pointer events + elementFromPoint
 *  (HTML5 DnD is unreliable in WebView2); the handle also carries a
 *  keyboard reorder path (↑/↓/Home/End). */
export const ProvidersPanel = memo(function ProvidersPanel() {
  const { t, locale } = useCtx();
  const copy = get_provider_ui_copy(locale);
  const d = useProvidersData();
  const a = useProvidersActions();
  const [dragIdx, setDragIdx] = useState<number | null>(null);
  const [overIdx, setOverIdx] = useState<number | null>(null);
  const [showPresets, setShowPresets] = useState(false);
  const [routingMode, setRoutingModeState] = useState<RoutingMode>('order');
  const [routingModeError, set_routing_mode_error] = useState<string | null>(null);
  const [routingModeChanging, set_routing_mode_changing] = useState(false);
  const [search_query, set_search_query] = useState('');
  const search_ref = useRef<HTMLInputElement>(null);
  const normalized_query = search_query.trim().toLocaleLowerCase();
  const filtered = normalized_query.length > 0;
  const admitted = account_display_cards(d.cards, d.cfg.appType);
  // Keep source indices: a filtered visual index is never a persisted priority.
  const visible_cards = admitted.cards
    .filter(({ card }) => !filtered || [
      card.preset?.name,
      card.upstream?.name,
      card.preset ? d.cfg.cardUrl(card.preset.settingsConfig) : '',
      card.upstream?.baseURL,
    ].some((value) => value?.toLocaleLowerCase().includes(normalized_query)));
  const clear_search = () => {
    set_search_query('');
    search_ref.current?.focus();
  };

  useEffect(() => {
    set_search_query('');
    setDragIdx(null);
    setOverIdx(null);
  }, [d.cfg]);

  /** Failover-queue spread mode (rotate / weight / usage). The setting is
   *  global: it reorders only queue-built chains, never hand-ordered routes. */
  useEffect(() => {
    let cancelled = false;
    getRoutingMode()
      .then((mode) => {
        if (!cancelled) setRoutingModeState(mode);
      })
      .catch(() => {
        /* keep the default when the read fails */
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const handleRoutingMode = useCallback(
    (mode: RoutingMode) => {
      if (mode === routingMode || routingModeChanging) return;
      const previous = routingMode;
      setRoutingModeState(mode);
      set_routing_mode_error(null);
      set_routing_mode_changing(true);
      setRoutingMode(mode)
        .catch((error) => {
          // Roll back to the persisted mode: leaving the optimistic value
          // would make the selector lie, and selecting it again would
          // short-circuit as a no-op.
          setRoutingModeState(previous);
          set_routing_mode_error(
            t('providers.routingMode.failed', { message: String(error) }),
          );
        })
        .finally(() => set_routing_mode_changing(false));
    },
    [routingMode, routingModeChanging, t],
  );

  const preset_indices = admitted.cards.filter(({ card }) => card.kind === 'preset').map(({ source_index }) => source_index);
  const hasStandalone = admitted.cards.some(({ card }) => card.kind === 'standalone');

  /** Resolve the preset card under a pointer position, or null. */
  const cardRowAt = (x: number, y: number): HTMLElement | null => {
    const el = document.elementFromPoint(x, y);
    const card = el?.closest?.('.provider-card[data-preset-idx]') as HTMLElement | null;
    return card;
  };

  const handleDragStart = useCallback((fromIdx: number, e: React.PointerEvent) => {
    if (filtered || d.busy) return;
    e.preventDefault();
    (e.currentTarget as HTMLElement).setPointerCapture?.(e.pointerId);
    setDragIdx(fromIdx);
    setOverIdx(fromIdx);
  }, [filtered, d.busy]);

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
      if (!filtered && !d.busy && toIdx !== dragIdx) a.onReorder(dragIdx, toIdx);
    },
    [dragIdx, a, filtered, d.busy],
  );

  /** A cancelled gesture is NOT a drop: the browser cancels the pointer
   *  when a touch scroll, palm rejection, or a second pointer takes over.
   *  Reordering there would persist a chain order the user never chose. */
  const handleDragCancel = useCallback((e: React.PointerEvent) => {
    (e.currentTarget as HTMLElement).releasePointerCapture?.(e.pointerId);
    setDragIdx(null);
    setOverIdx(null);
  }, []);

  /** Keyboard reorder for the drag handle (↑/↓ move, Home/End to the
   *  ends). Mirrors the pointer-drag path so the handle isn't fake
   *  accessible. */
  const handleKeyDown = useCallback(
    (idx: number, e: React.KeyboardEvent) => {
      if (filtered || d.busy) return;
      let toIdx: number | null = null;
      const position = preset_indices.indexOf(idx);
      if (position < 0) return;
      if (e.key === 'ArrowUp') toIdx = preset_indices[Math.max(0, position - 1)];
      else if (e.key === 'ArrowDown') toIdx = preset_indices[Math.min(preset_indices.length - 1, position + 1)];
      else if (e.key === 'Home') toIdx = preset_indices[0];
      else if (e.key === 'End') toIdx = preset_indices[preset_indices.length - 1];
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
    [preset_indices, a, filtered, d.busy],
  );

  return (
    <div className="card providers-panel" data-testid="providers-panel">
      <div className="card-head">
        <h3>{t('providers.title')}</h3>
        <div className="header-actions">
          <select
            className="input routing-mode-select"
            value={routingMode}
            aria-label={t('providers.routingMode')}
            title={t('providers.routingMode.help')}
            data-testid="routing-mode-select"
            disabled={routingModeChanging}
            onChange={(e) => handleRoutingMode(e.target.value as RoutingMode)}
          >
            {ROUTING_MODES.map((mode) => (
              <option key={mode} value={mode}>
                {t(`providers.routingMode.${mode}`)}
              </option>
            ))}
          </select>
          {routingModeError && (
            <span
              className="muted small routing-mode-error"
              role="alert"
              data-testid="routing-mode-error"
            >
              {routingModeError}
            </span>
          )}
          <button
            className="btn small"
            onClick={a.onRefresh}
            data-testid="providers-refresh"
          >
            {t('common.refresh')}
          </button>
          <button
            className="btn small"
            disabled={d.cfg.detectOnly !== undefined || d.refreshingModels !== null}
            onClick={() => a.onRefreshModels()}
            title={t('providers.refreshModels.help')}
            data-testid="providers-refresh-models"
          >
            {d.refreshingModels === 'all' ? '…' : t('providers.refreshModels.all')}
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
          <button
            className="btn small"
            disabled={d.cfg.detectOnly !== undefined}
            title={t('providers.addFromTemplate.help')}
            data-testid="providers-add-template"
            onClick={() => setShowPresets(true)}
          >
            {t('providers.addFromTemplate')}
          </button>
        </div>
      </div>
      {showPresets && (
        <Preset_picker
          onClose={() => setShowPresets(false)}
          onPick={(templateId) => {
            setShowPresets(false);
            a.onAddFromTemplate(templateId);
          }}
        />
      )}
      {d.cfg.detectOnly && (
        <p className="muted small" data-testid="detect-only-notice">
          {d.cfg.detectOnly.reason}
        </p>
      )}
      {d.cfg.detectOnly === undefined && (
        <p className="muted small">{t('providers.desc')}</p>
      )}
      {admitted.omitted_count > 0 && <p className="muted small" role="status" data-testid="unsupported-account-configurations">{locale === 'zh'
        ? `已省略 ${admitted.omitted_count} 个不支持的账户配置；保留的数据未更改。`
        : `${admitted.omitted_count} unsupported account configurations omitted. Stored records are preserved.`}</p>}
      {admitted.cards.length > 0 && (
        <div className="provider_search_toolbar">
          <label className="provider_search_field">
            <span>{copy.search}</span>
            <input
              ref={search_ref}
              type="search"
              className="input"
              value={search_query}
              placeholder={copy.search_hint}
              data-testid="providers-search"
              onChange={(event) => set_search_query(event.target.value)}
              onKeyDown={(event) => { if (event.key === 'Escape') clear_search(); }}
            />
          </label>
          {search_query && <button className="btn small" data-testid="providers-search-clear" onClick={clear_search}>{copy.clear}</button>}
          <span className="muted small provider_search_count" role="status" data-testid="providers-search-count">{copy.results(visible_cards.length, admitted.cards.length)}</span>
          {filtered && <p className="muted small provider_search_hint">{copy.reorder_hint}</p>}
        </div>
      )}
      {admitted.cards.length === 0 ? (
        <p className="muted">
          {t('providers.empty')}
          {d.pool_supported && (
            <>
              {' '}
              <button
                className="btn small"
                onClick={a.onAddStandalone}
                disabled={d.cfg.detectOnly !== undefined}
                data-testid="providers-add-standalone-empty"
              >
                {t('providers.addStandalone')}
              </button>
            </>
          )}
        </p>
      ) : (
        <ul className="providers-list">
          {visible_cards.length === 0 && <li className="provider_search_empty" data-testid="providers-search-empty">{copy.no_results}</li>}
          {visible_cards.map(({ card, source_index }, visible_index) => {
            const presetIdx = card.kind === 'preset' ? source_index : null;
            return (
              <Fragment key={card.key}>
                {card.kind === 'standalone' && visible_cards[visible_index - 1]?.card.kind !== 'standalone' && standalone_divider(t, a, d)}
                <ProviderCard
                  card={card}
                  presetIdx={presetIdx}
                  reorder_disabled={filtered || Boolean(d.busy)}
                  isDragSource={presetIdx !== null && dragIdx === presetIdx}
                  isDragOver={
                    presetIdx !== null && overIdx === presetIdx && dragIdx !== null
                  }
                  onDragStart={handleDragStart}
                  onDragMove={handleDragMove}
                  onDragEnd={handleDragEnd}
                  onDragCancel={handleDragCancel}
                  onKeyDown={handleKeyDown}
                />
              </Fragment>
            );
          })}
          {/* Pool section header renders even when zero standalone rows
           * exist — otherwise the first "+ Add upstream" (the most common
           * state on a fresh install) has no entry point. Only on pages
           * that surface standalone rows (pool_supported). */}
          {d.pool_supported && !hasStandalone && !filtered && standalone_divider(t, a, d)}
        </ul>
      )}
    </div>
  );
});
