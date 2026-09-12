import { useEffect, useState } from 'react';
import { useT } from '../../lib/ctx';
import {
  deeplinkParse,
  subscribeDeeplink,
  type DeeplinkAction,
} from '../../lib/api/deeplink';

interface ReceivedUrl {
  url: string;
  at: string;
  action: DeeplinkAction | null;
  actionError: string | null;
  /// Result of executing the action via `onExecute` (null = not yet executed).
  executeResult: string | null;
  executeError: string | null;
}

interface DeepLinkPageProps {
  /// Dispatcher for parsed actions. Lives in App so navigation
  /// (setView) + provider switches share the same handler.
  onExecute: (action: DeeplinkAction) => Promise<string>;
}

/// Format a `DeeplinkAction` as a short human-readable summary.
function formatAction(a: DeeplinkAction): string {
  switch (a.kind) {
    case 'switchProvider':
      return `Switch active provider → ${a.providerId} (app=${a.appType})`;
    case 'focusProvider':
      return `Open provider → ${a.providerId}`;
    case 'focusMcp':
      return `Open MCP server → ${a.serverId}`;
    case 'focusSkill':
      return `Open skill → ${a.skillId}`;
    case 'showDeeplinks':
      return 'Show deep-link page';
    case 'unknown':
      return `Unknown: ${a.reason}`;
  }
}

/// Placeholder page that renders received deep-link URLs and their parsed
/// action. v2 (Step 5): per-row Execute button dispatches the parsed action
/// via the App-owned `onExecute` callback.
export function DeepLinkPage({ onExecute }: DeepLinkPageProps) {
  const t = useT();
  const [urls, setUrls] = useState<ReceivedUrl[]>([]);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    void subscribeDeeplink((incoming) => {
      if (cancelled) return;
      void (async () => {
        const at = new Date().toLocaleString();
        const parsed: ReceivedUrl[] = await Promise.all(
          incoming.map(async (url) => {
            try {
              const action = await deeplinkParse(url);
              return {
                url,
                at,
                action,
                actionError: null,
                executeResult: null,
                executeError: null,
              };
            } catch (e) {
              return {
                url,
                at,
                action: null,
                actionError: String(e),
                executeResult: null,
                executeError: null,
              };
            }
          }),
        );
        if (!cancelled) setUrls((prev) => [...parsed, ...prev]);
      })();
    }).then((fn) => {
      if (cancelled) {
        fn();
      } else {
        unlisten = fn;
      }
    });
    return () => {
      cancelled = true;
      if (unlisten) unlisten();
    };
  }, []);

  const handleExecute = async (idx: number) => {
    setUrls((prev) => {
      const next = prev.slice();
      const row = next[idx];
      if (!row?.action) return prev;
      // Mark busy by clearing the previous result (caller will re-render with
      // the new state once onExecute resolves).
      next[idx] = { ...row, executeResult: null, executeError: null };
      // Fire-and-forget the async call; the result is written back via a
      // second setState once the promise resolves.
      void (async () => {
        try {
          const result = await onExecute(row.action!);
          setUrls((curr) => {
            const n = curr.slice();
            const r = n[idx];
            if (r) n[idx] = { ...r, executeResult: result, executeError: null };
            return n;
          });
        } catch (e) {
          setUrls((curr) => {
            const n = curr.slice();
            const r = n[idx];
            if (r)
              n[idx] = { ...r, executeResult: null, executeError: String(e) };
            return n;
          });
        }
      })();
      return next;
    });
  };

  return (
    <div className="deeplink-page">
      <header className="page-header">
        <h2>{t('deeplink.title')}</h2>
        <p className="muted" style={{ margin: 0, fontSize: 12 }}>
          {t('deeplink.intro')}
        </p>
      </header>

      <section className="list-section">
        {urls.length === 0 ? (
          <p className="muted">
            {t('deeplink.empty')}
          </p>
        ) : (
          <ul className="deeplink-list">
            {urls.map((u, i) => (
              <li key={`${u.at}-${i}`} className="deeplink-card">
                <div className="deeplink-card-main">
                  <code className="deeplink-url">{u.url}</code>
                  <span className="deeplink-time">{u.at}</span>
                </div>
                {u.action && (
                  <div className="deeplink-action">
                    <span className="deeplink-action-kind">
                      [{u.action.kind}]
                    </span>{' '}
                    {formatAction(u.action)}
                    {u.action.kind !== 'unknown' && (
                      <button
                        className="btn primary deeplink-execute"
                        onClick={() => void handleExecute(i)}
                      >
                        {t('deeplink.execute')}
                      </button>
                    )}
                  </div>
                )}
                {u.actionError && (
                  <div className="deeplink-action deeplink-action-error">
                    {t('deeplink.error', { message: u.actionError })}
                  </div>
                )}
                {u.executeResult && (
                  <div className="deeplink-execute-result">→ {u.executeResult}</div>
                )}
                {u.executeError && (
                  <div className="deeplink-execute-error">
                    execute error: {u.executeError}
                  </div>
                )}
              </li>
            ))}
          </ul>
        )}
      </section>
    </div>
  );
}