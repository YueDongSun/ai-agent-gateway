import { memo, useId, useRef, useState } from 'react';
import { useCtx } from '../../../lib/ctx';
import { TestIcon } from '../../Icons';
import { useProvidersActions, useProvidersData } from './providers_state.js';
import type { UnifiedCard } from '../../../lib/unified_providers';
import { Provider_icon } from './provider_icon.js';
import { get_provider_ui_copy } from './provider_ui_copy.js';

export interface ProviderCardProps {
  card: UnifiedCard;
  /** Index within the preset section (null for standalone cards — their
   *  order follows the pool, so they are not drag-reorderable). */
  presetIdx: number | null;
  reorder_disabled?: boolean;
  isDragSource: boolean;
  isDragOver: boolean;
  onDragStart: (presetIdx: number, e: React.PointerEvent) => void;
  onDragMove: (e: React.PointerEvent) => void;
  onDragEnd: (e: React.PointerEvent) => void;
  /** Abort the drag without reordering (the gesture was cancelled). */
  onDragCancel: (e: React.PointerEvent) => void;
  onKeyDown: (presetIdx: number, e: React.KeyboardEvent) => void;
}

/** One unified provider card (cc-switch style): drag handle, icon tile,
 *  name + status badges, URL line, explicit action disclosure, and the test
 *  result row. Preset + linked upstream render as a single card; the
 *  upstream side (breaker/pin/enable/test) shows when one is linked. */
export const ProviderCard = memo(function ProviderCard({
  card,
  presetIdx,
  reorder_disabled = false,
  isDragSource,
  isDragOver,
  onDragStart,
  onDragMove,
  onDragEnd,
  onDragCancel,
  onKeyDown,
}: ProviderCardProps) {
  const { t, locale } = useCtx();
  const copy = get_provider_ui_copy(locale);
  const [details_open, set_details_open] = useState(false);
  const details_id = useId();
  const disclosure_ref = useRef<HTMLButtonElement>(null);
  const d = useProvidersData();
  const a = useProvidersActions();

  const { preset, upstream, proxyId, key, kind } = card;
  const supplierManaged = card.supplierManaged;
  const name = preset?.name || upstream?.name || key;
  const url = preset ? d.cfg.cardUrl(preset.settingsConfig) : (upstream?.baseURL ?? '');
  const meta = preset
    ? d.cfg.cardMeta(preset.settingsConfig)
    : upstream
      ? [
          upstream.type,
          upstream.models?.length
            ? t('providers.modelCount', { n: String(upstream.models.length) })
            : '',
        ]
            .filter(Boolean)
            .join(' · ')
      : '';

  const isCurrent = Boolean(preset && preset.id === d.currentId);
  const isPinned = Boolean(proxyId && d.pinnedId === proxyId);
  const isActive = Boolean(d.running && proxyId && d.health?.lastUsed === proxyId);
  const isDisabled = Boolean(upstream && !upstream.enabled);
  const breakerState = proxyId ? d.breakerOf(proxyId) : null;
  const testResult = proxyId ? d.testResults[proxyId] : undefined;
  const cardClass = [
    'provider-card',
    kind === 'standalone' ? 'standalone' : '',
    isCurrent ? 'current' : '',
    isActive ? 'active' : '',
    isDisabled ? 'disabled' : '',
    isDragSource ? 'dragging' : '',
    isDragOver ? 'drag-over' : '',
  ]
    .filter(Boolean)
    .join(' ');

  return (
    <li
      className={cardClass}
      data-testid={`provider-card-${key}`}
      data-preset-idx={presetIdx ?? undefined}
      onKeyDown={(event) => {
        if (event.key === 'Escape' && details_open) {
          event.stopPropagation();
          set_details_open(false);
          disclosure_ref.current?.focus();
        }
      }}
    >
      <div className="provider-card-main">
        {presetIdx !== null ? (
          <span
            className="provider-card-drag"
            role="button"
            tabIndex={reorder_disabled ? -1 : 0}
            aria-disabled={reorder_disabled}
            data-testid={`provider-drag-${key}`}
            aria-label={t('providers.dragHint')}
            title={t('providers.dragHint')}
            onPointerDown={(e) => { if (!reorder_disabled) onDragStart(presetIdx, e); }}
            onPointerMove={onDragMove}
            onPointerUp={onDragEnd}
            onPointerCancel={onDragCancel}
            onKeyDown={(e) => { if (!reorder_disabled) onKeyDown(presetIdx, e); }}
          >
            ⋮⋮
          </span>
        ) : (
          <span className="provider-card-drag-spacer" aria-hidden="true" />
        )}
        <Provider_icon
          className="provider-card-icon"
          icon={preset?.icon ?? upstream?.icon}
          color={preset?.iconColor ?? upstream?.iconColor}
          name={name}
        />
        <div className="provider-card-body">
          <span className="provider-card-title">
            {name}
            {isCurrent && <span className="tag tag-current">{t('providers.current')}</span>}
            {preset &&
              (card.priority !== null ? (
                <span className="priority-badge" title={t('providers.dragHint')}>
                  P{card.priority}
                </span>
              ) : (
                <span className="tag tag-paused">{t('providers.out')}</span>
              ))}
            {isDisabled && <span className="tag tag-paused">{t('providers.disabledTag')}</span>}
            {supplierManaged && (
              <span className="tag" title={t('providers.managed.help')} data-testid={`provider-managed-${key}`}>
                {t('providers.managed')}
              </span>
            )}
            {isPinned && <span className="tag tag-pinned">{t('providers.pinnedTag')}</span>}
            {isActive && <span className="tag tag-current">{t('providers.activeTag')}</span>}
            {breakerState && (
              <span
                className={`breaker-dot ${d.breakerOfClass(breakerState)}`}
                title={breakerState}
              />
            )}
            {preset && !upstream && (
              <span className="tag" title={t('providers.upstream.missing.help')}>
                {t('providers.upstream.missing')}
              </span>
            )}
          </span>
          <span className="provider-card-url">
            {url || '—'}
            {meta ? ` · ${meta}` : ''}
          </span>
        </div>
        <div className="provider-card-actions">
          {/* The backend resolves the target before writing; Claude and
           * Codex use their isolated AAG configurations. */}
          {preset && d.cfg.detectOnly === undefined && (
            <button
              className="btn small primary"
              disabled={Boolean(d.busy)}
              aria-busy={d.busy === `switch-${preset.id}`}
              onClick={() => a.onSwitch(preset.id)}
              data-testid={`provider-switch-${key}`}
            >
              {d.busy === `switch-${preset.id}`
                ? '…'
                : isCurrent
                  ? t('providers.reapply')
                  : t('providers.enable')}
            </button>
          )}
          {!supplierManaged && (
            <button className="btn small" disabled={Boolean(d.busy)} onClick={() => a.onEditCard(card)} data-testid={`provider-edit-${key}`}>
              {t('common.edit')}
            </button>
          )}
          <button
            ref={disclosure_ref}
            className="btn small provider_more"
            aria-expanded={details_open}
            aria-controls={details_id}
            aria-label={`${details_open ? copy.less : copy.more}: ${name}`}
            onClick={() => set_details_open((open) => !open)}
            data-testid={`provider-more-${key}`}
          >
            {details_open ? copy.less : copy.more}<span aria-hidden="true">{details_open ? '−' : '+'}</span>
          </button>
        </div>
      </div>
      <div id={details_id} hidden={!details_open} className="provider_secondary_actions" data-testid={`provider-details-${key}`}>
          {preset && (
            <button
              className={preset.inFailoverQueue === false ? 'btn small primary' : 'btn small'}
              title={t('providers.chain.help')}
              onClick={() => a.onToggleQueue(card)}
              disabled={Boolean(d.busy)}
              data-testid={`provider-chain-${key}`}
            >
              {preset.inFailoverQueue === false
                ? t('providers.chain.in')
                : t('providers.chain.out')}
            </button>
          )}
          {upstream && proxyId && (
            <>
              {/* Supplier-projected rows take their enabled state from the
               * supplier + endpoint flags; a local flip would be overwritten
               * by the next supplier save, so it is not offered. */}
              {!supplierManaged && (
                <label className="provider_control" title={t('providers.enabled.help')}>
                  <span className="toggle-switch">
                    <input
                      type="checkbox"
                      checked={upstream.enabled}
                      disabled={Boolean(d.busy)}
                      onChange={(e) => a.onToggleEnabled(proxyId, e.target.checked)}
                      data-testid={`provider-toggle-${proxyId}`}
                      aria-label={t('providers.enabled.help')}
                    />
                    <span className="toggle-slider" />
                  </span>
                  <span>{copy.enabled}</span>
                </label>
              )}
              <button
                className={`btn icon-btn ${
                  testResult?.success ? 'test-ok' : testResult ? 'test-fail' : ''
                }`}
                disabled={d.testingId !== null || !upstream.enabled || !upstream.models.some((model) => model.trim())}
                onClick={() => a.onTestProvider(proxyId)}
                title={t('providers.test.help')}
                data-tooltip={d.testingId === proxyId ? '…' : t('providers.test')}
                aria-label={t('providers.test.help')}
                aria-describedby={`${details_id}-test-prerequisite`}
                data-testid={`provider-test-${proxyId}`}
              >
                {d.testingId === proxyId ? '…' : <TestIcon />}
              </button>
              <label className="provider_control" title={t('providers.pin.help')}>
                <span className="toggle-switch">
                  <input
                    type="checkbox"
                    checked={isPinned}
                    disabled={Boolean(d.busy)}
                    onChange={() => a.onTogglePin(proxyId)}
                    data-testid={`provider-pin-${proxyId}`}
                    aria-label={t('providers.pin.help')}
                  />
                  <span className="toggle-slider" />
                </span>
                <span>{copy.pinned}</span>
              </label>
              <label
                className="provider_control"
                title={t('providers.autoTrack.help')}
              >
                <span className="toggle-switch">
                  <input
                    type="checkbox"
                    checked={upstream.autoTrackModels}
                    disabled={Boolean(d.busy)}
                    onChange={(e) => a.onToggleAutoTrack(proxyId, e.target.checked)}
                    data-testid={`provider-autotrack-${proxyId}`}
                    aria-label={t('providers.autoTrack.help')}
                  />
                  <span className="toggle-slider" />
                </span>
                <span>{copy.auto_track}</span>
              </label>
              <button
                className="btn icon-btn"
                disabled={d.refreshingModels !== null}
                onClick={() => a.onRefreshModels(proxyId)}
                title={t('providers.refreshModels.help')}
                aria-label={t('providers.refreshModels.help')}
                data-testid={`provider-refresh-models-${proxyId}`}
              >
                {d.refreshingModels === proxyId ? '…' : '⟳'}
              </button>
            </>
          )}
          {!supplierManaged && (
              <button
                className="btn small danger"
                disabled={Boolean(d.busy)}
                onClick={() => a.onDeleteCard(card)}
                data-testid={`provider-delete-${key}`}
              >
                {t('common.delete')}
              </button>
          )}
      </div>
      {details_open && upstream && proxyId && (
        <p className="muted small" id={`${details_id}-test-prerequisite`} data-testid={`provider-test-prerequisite-${proxyId}`}>
          {t('providers.test.prerequisite')}
        </p>
      )}
      {testResult && proxyId && (
        <div
          className={`provider-test-result ${
            testResult.success ? 'test-result ok' : 'test-result fail'
          }`}
          data-testid={`provider-test-result-${proxyId}`}
        >
          {testResult.success ? (
            <span>
              ✓{' '}
              {t('providers.test.ok', {
                latency: String(testResult.latencyMs),
                model: testResult.model ?? '',
              })}
            </span>
          ) : (
            <span>✗ {t('providers.test.fail', { error: testResult.error ?? '' })}</span>
          )}
          <button className="hop-remove" aria-label={t('common.close')} onClick={() => a.onClearTestResult(proxyId)}>
            ×
          </button>
        </div>
      )}
    </li>
  );
});
