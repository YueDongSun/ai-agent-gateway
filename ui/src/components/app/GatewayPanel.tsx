import { useT } from '../../lib/ctx';
import { CopyIcon } from '../Icons';
import { Secret_value } from '../secret_value.js';
import type { AppPageConfig } from '../../lib/appPages';
import type { CodexCliStatus } from '../../lib/api/codexCli';

interface GatewayPanelProps {
  cfg: AppPageConfig;
  running: boolean;
  gatewayUrl: string;
  authToken: string;
  cliStatus: CodexCliStatus | null;
  /** Last error from the Codex CLI status fetch (Phase 23f). When
   *  set, the mirror row renders a warning chip instead of the
   *  initialized pill, so the user can see why the row is stale. */
  cliStatusError?: string | null;
  copied: string | null;
  busy: string | null;
  onCopy: (text: string, key: string) => void;
  onStart: () => void;
  onStop: () => void;
  /** Opens the gateway settings modal (port + auth token). */
  onEditSettings?: () => void;
}

/** AAG Gateway status card — the accent-gradient control panel at the top of
 * the page. Contains Start/Stop, URL + API key copy, optional Codex mirror
 * row. Extracted from AppPage.tsx in the file-structure refactor. */
export function GatewayPanel({
  cfg,
  running,
  gatewayUrl,
  authToken,
  cliStatus,
  cliStatusError,
  copied,
  busy,
  onCopy,
  onStart,
  onStop,
  onEditSettings,
}: GatewayPanelProps) {
  const t = useT();

  if (!cfg.showGatewayCard) return null;

  return (
    <div className="card endpoint-card gateway-panel">
      <div className="card-head">
        <h3>{t('app.gateway.title')}</h3>
        <span className={running ? 'pill on' : 'pill off'}>
          {running ? t('app.gateway.running') : t('app.gateway.stopped')}
        </span>
        {running ? (
          <button
            className="btn"
            onClick={onStop}
            disabled={busy === 'stop'}
            data-testid="gateway-stop"
          >
            {t('app.gateway.stop')}
          </button>
        ) : (
          <button
            className="btn primary"
            onClick={onStart}
            disabled={busy === 'start'}
            data-testid="gateway-start"
          >
            {t('app.gateway.start')}
          </button>
        )}
        {onEditSettings && (
          <button
            className="btn icon-btn"
            onClick={onEditSettings}
            title={t('app.gateway.editSettings')}
            data-tooltip={t('app.gateway.editSettings')}
            aria-label={t('app.gateway.editSettings')}
          >
            ⚙
          </button>
        )}
      </div>
      <div className="endpoint-row">
        <span className="endpoint-label">{t('app.gateway.url')}</span>
        <code className="endpoint-value">{gatewayUrl || '…'}</code>
        <button
          className="btn icon-btn"
          onClick={() => onCopy(gatewayUrl, 'url')}
          title={t('app.gateway.copy')}
          data-tooltip={t('app.gateway.copy')}
          aria-label={t('app.gateway.copy')}
        >
          {copied === 'url' ? '✓' : <CopyIcon />}
        </button>
      </div>
      <div className="endpoint-row">
        <span className="endpoint-label">{t('app.gateway.apiKey')}</span>
        <Secret_value
          key={cfg.label}
          value={authToken}
          empty_label={t('app.gateway.noKey')}
          hidden_label={t('app.gateway.hidden_key')}
          show_label={t('app.gateway.show_key')}
          hide_label={t('app.gateway.hide_key')}
          test_id="gateway_key"
        />
        <button
          className="btn icon-btn"
          onClick={() => onCopy(authToken, 'key')}
          disabled={!authToken}
          data-testid="gateway_key_copy"
          title={t('app.gateway.copy')}
          data-tooltip={t('app.gateway.copy')}
          aria-label={t('app.gateway.copy')}
        >
          {copied === 'key' ? '✓' : <CopyIcon />}
        </button>
      </div>
      {cfg.ccSwitchUpstream && (
        <p className="muted small">{t('app.gateway.ccswitchHint')}</p>
      )}
      {cfg.launchers === 'codex' && (
        <div className="endpoint-row">
          <span className="endpoint-label">{t('codex.mirror.home')}</span>
          <code className="endpoint-value">{cliStatus?.configDir ?? '~/.codex-cli'}</code>
          {cliStatusError ? (
            <span
              className="pill off"
              title={cliStatusError}
              data-testid="codex-mirror-error"
            >
              ⚠
            </span>
          ) : (
            <span className={cliStatus?.initialized ? 'pill on' : 'pill off'}>
              {cliStatus?.initialized ? '✓' : t('codex.mirror.notInitialized')}
            </span>
          )}
          <button
            className="btn icon-btn"
            onClick={() => onCopy(cliStatus?.configDir ?? '~/.codex-cli', 'mirror')}
            title={t('app.gateway.copy')}
            data-tooltip={t('app.gateway.copy')}
            aria-label={t('app.gateway.copy')}
          >
            {copied === 'mirror' ? '✓' : <CopyIcon />}
          </button>
        </div>
      )}
    </div>
  );
}
