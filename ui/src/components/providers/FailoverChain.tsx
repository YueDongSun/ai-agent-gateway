/// FailoverChain — visualization of the homepage claude_code provider
/// list. Each row is a node; rows are joined by a horizontal arrow
/// showing the failover order. Layout mirrors cc-switch's
/// FailoverQueueManager (linked cards with arrows between them), but
/// using AAG's vanilla-CSS tokens so we don't pull in Tailwind/dnd-kit.
import type { Provider } from '../../lib/api/types';
import { useT } from '../../lib/ctx';

interface ChainEntry {
  provider: Provider;
  /** URL extracted from env.ANTHROPIC_BASE_URL (None if absent / unparseable). */
  baseUrl: string | null;
}

function extractBaseUrl(p: Provider): string | null {
  const env = p.settingsConfig?.env;
  if (!env || typeof env !== 'object') return null;
  const v = (env as Record<string, unknown>)['ANTHROPIC_BASE_URL'];
  return typeof v === 'string' && v.trim() ? v.trim() : null;
}

export function FailoverChain({ providers }: { providers: Provider[] }) {
  const t = useT();
  const entries: ChainEntry[] = providers.map((p) => ({
    provider: p,
    baseUrl: extractBaseUrl(p),
  }));
  if (entries.length === 0) {
    return (
      <div className="failover-chain glass">
        <div className="failover-chain-empty">{t('chain.empty')}</div>
      </div>
    );
  }
  return (
    <div className="failover-chain glass">
      {entries.map((e, i) => (
        <div key={e.provider.id} className="failover-chain-row">
          {i < entries.length - 1 && <div className="failover-chain-arrow" />}
          <div className="failover-chain-node">
            <div className="failover-chain-num">{i + 1}</div>
            <div className="failover-chain-info">
              <div className="failover-chain-name">
                {e.provider.name}
                {e.provider.syncLocked && (
                  <span className="failover-chain-badge locked">
                    🔒 {t('providers.locked')}
                  </span>
                )}
              </div>
              <div className="failover-chain-url">
                {e.baseUrl ?? t('chain.noUrl')}
              </div>
            </div>
            {!e.baseUrl && (
              <span className="failover-chain-badge empty-url">
                {t('chain.skipped')}
              </span>
            )}
          </div>
        </div>
      ))}
    </div>
  );
}
