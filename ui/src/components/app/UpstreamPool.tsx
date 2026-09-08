import { memo } from 'react';
import { useT } from '../../lib/ctx';
import { TestIcon } from '../Icons';
import { useProxyPoolActions, useProxyPoolData } from './proxy_pool_state.js';

interface UpstreamPoolProps {
  onRefresh: () => void;
  onAddProvider: () => void;
}

/** Upstream pool card with the failover-flow visualization. */
export const UpstreamPool = memo(function UpstreamPool({
  onRefresh,
  onAddProvider,
}: UpstreamPoolProps) {
  const t = useT();
  const {
    providers,
    health,
    pinnedId,
    testResults,
    testingId,
    breakerOf,
    breakerOfClass,
  } = useProxyPoolData();
  const { onTestProvider, onClearTestResult, onTogglePin } = useProxyPoolActions();

  return (
    <div className="card">
      <div className="card-head">
        <h3>{t('claude.upstream.title')}</h3>
        <div className="header-actions">
          <button className="btn small" onClick={onRefresh}>
            {t('common.refresh')}
          </button>
          <button className="btn small" onClick={onAddProvider}>
            {t('claude.upstream.add')}
          </button>
        </div>
      </div>
      <p className="muted small">{t('claude.upstream.desc')}</p>
      {providers.length === 0 ? (
        <p className="muted">{t('claude.upstream.empty')}</p>
      ) : (
        <>
          {/* Failover flow visualization. Disabled providers stay in the
           * chain but render dimmed with a DISABLED tag — hiding them
           * entirely made it look like they were deleted. */}
          <div className="failover-flow">
            {providers.map((pp) => {
                const bs = breakerOf(pp.id);
                const isActive = health?.lastUsed === pp.id;
                const isPinned = pinnedId === pp.id;
                const isBroken = bs === 'open';
                const nodeClass = [
                  'flow-node',
                  isPinned ? 'pinned' : isActive ? 'active' : '',
                  isBroken ? 'broken' : '',
                  pp.enabled ? '' : 'disabled',
                ]
                  .filter(Boolean)
                  .join(' ');
                return (
                  <div key={pp.id}>
                    <div className="flow-arrow">↓</div>
                    <div className={nodeClass}>
                      <span className="flow-node-id">
                        {pp.name || pp.id}
                        {!pp.enabled && (
                          <span className="tag tag-paused">{t('claude.upstream.disabledTag')}</span>
                        )}
                      </span>
                      <span className="flow-node-url">{pp.baseURL || '—'}</span>
                      <div className="flow-node-status">
                        {bs && (
                          <span
                            className={`breaker-dot ${breakerOfClass(bs)}`}
                            title={bs}
                          />
                        )}
                        {isActive && (
                          <span className="tag tag-current">{t('claude.upstream.activeTag')}</span>
                        )}
                        {isPinned && (
                          <span className="tag tag-pinned">
                            {t('claude.upstream.pinnedTag')}
                          </span>
                        )}
                        <button
                          className={`btn icon-btn ${
                            testResults[pp.id]?.success
                              ? 'test-ok'
                              : testResults[pp.id]
                                ? 'test-fail'
                                : ''
                          }`}
                          disabled={testingId === pp.id}
                          onClick={() => onTestProvider(pp.id)}
                          title={t('claude.upstream.test.help')}
                          data-tooltip={testingId === pp.id ? '…' : t('claude.upstream.test')}
                          aria-label={t('claude.upstream.test.help')}
                        >
                          {testingId === pp.id ? '…' : <TestIcon />}
                        </button>
                        <label className="toggle-switch" title={t('claude.upstream.pin.help')}>
                          <input
                            type="checkbox"
                            checked={isPinned}
                            onChange={() => onTogglePin(pp.id)}
                          />
                          <span className="toggle-slider" />
                        </label>
                      </div>
                    </div>
                    {/* Test result */}
                    {testResults[pp.id] && (
                      <div
                        className={
                          testResults[pp.id].success ? 'test-result ok' : 'test-result fail'
                        }
                      >
                        {testResults[pp.id].success ? (
                          <span>
                            ✓ {t('claude.upstream.test.ok', {
                              latency: String(testResults[pp.id].latencyMs),
                              model: testResults[pp.id].model ?? '',
                            })}
                          </span>
                        ) : (
                          <span>
                            ✗{' '}
                            {t('claude.upstream.test.fail', {
                              error: testResults[pp.id].error ?? '',
                            })}
                          </span>
                        )}
                        <button
                          className="hop-remove"
                          onClick={() => onClearTestResult(pp.id)}
                        >
                          ×
                        </button>
                      </div>
                    )}
                  </div>
                );
              })}
          </div>
        </>
      )}
    </div>
  );
});
