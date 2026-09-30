import { memo } from 'react';
import { useT } from '../../lib/ctx';
import { TestIcon } from '../Icons';
import { useProvidersActions, useProvidersData } from './providers_state.js';
import type { UnifiedCard } from '../../lib/unified_providers';

/** Initials fallback for the icon tile when no icon character is set. */
function initialsOf(name: string): string {
  const parts = name.trim().split(/\s+/).filter(Boolean);
  if (parts.length === 0) return '?';
  return (parts[0][0] + (parts[1]?.[0] ?? '')).toUpperCase();
}

export interface ProviderCardProps {
  card: UnifiedCard;
  /** Index within the preset section (null for standalone cards — their
   *  order follows the pool, so they are not drag-reorderable). */
  presetIdx: number | null;
  isDragSource: boolean;
  isDragOver: boolean;
  onDragStart: (presetIdx: number, e: React.PointerEvent) => void;
  onDragMove: (e: React.PointerEvent) => void;
  onDragEnd: (e: React.PointerEvent) => void;
  onKeyDown: (presetIdx: number, e: React.KeyboardEvent) => void;
}

/** One unified provider card (cc-switch style): drag handle, icon tile,
 *  name + status badges, URL line, hover-revealed actions, and the test
 *  result row. Preset + linked upstream render as a single card; the
 *  upstream side (breaker/pin/enable/test) shows when one is linked. */
export const ProviderCard = memo(function ProviderCard({
  card,
  presetIdx,
  isDragSource,
  isDragOver,
  onDragStart,
  onDragMove,
  onDragEnd,
  onKeyDown,
}: ProviderCardProps) {
  const t = useT();
  const d = useProvidersData();
  const a = useProvidersActions();

  const { preset, upstream, proxyId, key, kind } = card;
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
  const iconChar = preset?.icon?.trim() || initialsOf(name);

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
    >
      <div className="provider-card-main">
        {presetIdx !== null ? (
          <span
            className="provider-card-drag"
            role="button"
            tabIndex={0}
            data-testid={`provider-drag-${key}`}
            aria-label={t('providers.dragHint')}
            title={t('providers.dragHint')}
            onPointerDown={(e) => onDragStart(presetIdx, e)}
            onPointerMove={onDragMove}
            onPointerUp={onDragEnd}
            onPointerCancel={onDragEnd}
            onKeyDown={(e) => onKeyDown(presetIdx, e)}
          >
            ⋮⋮
          </span>
        ) : (
          <span className="provider-card-drag-spacer" aria-hidden="true" />
        )}
        <span
          className="provider-card-icon"
          style={preset?.iconColor ? { color: preset.iconColor } : undefined}
        >
          {iconChar}
        </span>
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
          {preset && (
            <button
              className={preset.inFailoverQueue === false ? 'btn small primary' : 'btn small'}
              title={t('providers.chain.help')}
              onClick={() => a.onToggleQueue(card)}
              data-testid={`provider-chain-${key}`}
            >
              {preset.inFailoverQueue === false
                ? t('providers.chain.in')
                : t('providers.chain.out')}
            </button>
          )}
          {upstream && proxyId && (
            <>
              <label className="toggle-switch" title={t('providers.enabled.help')}>
                <input
                  type="checkbox"
                  checked={upstream.enabled}
                  onChange={(e) => a.onToggleEnabled(proxyId, e.target.checked)}
                  data-testid={`provider-toggle-${proxyId}`}
                  aria-label={t('providers.enabled.help')}
                />
                <span className="toggle-slider" />
              </label>
              <button
                className={`btn icon-btn ${
                  testResult?.success ? 'test-ok' : testResult ? 'test-fail' : ''
                }`}
                disabled={d.testingId === proxyId}
                onClick={() => a.onTestProvider(proxyId)}
                title={t('providers.test.help')}
                data-tooltip={d.testingId === proxyId ? '…' : t('providers.test')}
                aria-label={t('providers.test.help')}
                data-testid={`provider-test-${proxyId}`}
              >
                {d.testingId === proxyId ? '…' : <TestIcon />}
              </button>
              <label className="toggle-switch" title={t('providers.pin.help')}>
                <input
                  type="checkbox"
                  checked={isPinned}
                  onChange={() => a.onTogglePin(proxyId)}
                  data-testid={`provider-pin-${proxyId}`}
                  aria-label={t('providers.pin.help')}
                />
                <span className="toggle-slider" />
              </label>
            </>
          )}
          <button className="btn small" onClick={() => a.onEditCard(card)} data-testid={`provider-edit-${key}`}>
            {t('common.edit')}
          </button>
          <button
            className="btn small danger"
            onClick={() => a.onDeleteCard(card)}
            data-testid={`provider-delete-${key}`}
          >
            {t('common.delete')}
          </button>
        </div>
      </div>
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
          <button className="hop-remove" onClick={() => a.onClearTestResult(proxyId)}>
            ×
          </button>
        </div>
      )}
    </li>
  );
});
