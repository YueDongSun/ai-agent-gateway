import { useT } from '../../../lib/ctx';
import { CopyIcon } from '../../Icons';
import { Secret_value } from '../../secret_value.js';
import type { AppPageConfig } from '../../../lib/appPages';
import type { CodexCliStatus } from '../../../lib/api/codexCli';
import type { ClaudeMirrorStatus } from '../../../lib/api/claudeMirror';
import { TailnetPublicationControls } from './tailnet_publication_controls.js';

interface GatewayPanelProps {
  cfg: AppPageConfig;
  running: boolean;
  /** True when the running proxy's snapshot is stale (routes or pool providers
   *  were edited since the last `proxy_start`). */
  config_changed: boolean;
  /** Stable code from the last failed hot reload; the previous routing
   *  snapshot keeps serving until a later reload succeeds or the gateway
   *  restarts. */
  hotReloadError?: string | null;
  gatewayUrl: string;
  authToken: string;
  cliStatus: CodexCliStatus | null;
  /** Last error from the Codex CLI status fetch (Phase 23f). When
   *  set, the mirror row renders a warning chip instead of the
   *  initialized pill, so the user can see why the row is stale. */
  cliStatusError?: string | null;
  /** Claude mirror (CLAUDE_CONFIG_DIR) status — rendered on the Claude
   *  page. `null` while loading; an error keeps the row stale. */
  claudeStatus?: ClaudeMirrorStatus | null;
  claudeStatusError?: string | null;
  copied: string | null;
  busy: string | null;
  /** `/v1/responses` CoT prompt toggle state (Codex surface only). */
  cotOn: boolean;
  /** Flip the CoT prompt toggle — immediate effect, no restart. */
  onToggleCot: (on: boolean) => void;
  /** Codex mirror mode: 'exclusive' (AAG routes everything) or 'coexist'
   *  (ChatGPT login stays default; AAG selectable via [profiles.aag]). */
  mirrorMode: string;
  /** Flip the Codex mirror mode — rewrites the mirror config.toml
   *  immediately; auth.json is never touched. */
  onToggleMirrorMode: (coexist: boolean) => void;
  onCopy: (text: string, key: string) => void;
  onStart: () => void;
  onStop: () => void;
  /** Opens the gateway settings modal (port + auth token). */
  onEditSettings?: () => void;
  /** Restart the proxy so a stale config snapshot is rebuilt. */
  on_restart?: () => void;
}

/** AAG Gateway status card — the accent-gradient control panel at the top of
 * the page. Contains Start/Stop, URL + API key copy, optional Codex mirror
 * row. Extracted from AppPage.tsx in the file-structure refactor. */
export function GatewayPanel({
  cfg,
  running,
  config_changed,
  hotReloadError,
  gatewayUrl,
  authToken,
  cliStatus,
  cliStatusError,
  claudeStatus,
  claudeStatusError,
  copied,
  busy,
  cotOn,
  onToggleCot,
  mirrorMode,
  onToggleMirrorMode,
  onCopy,
  onStart,
  onStop,
  onEditSettings,
  on_restart,
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
        {running && config_changed && (
          <div className="gateway_config_changed">
            <span className="pill warning" data-testid="gateway_config_changed">
              {t('app.gateway.config_changed')}
            </span>
            {on_restart && (
              <button
                className="btn small"
                onClick={on_restart}
                disabled={busy === 'restart'}
                data-testid="gateway_restart_config"
              >
                {busy === 'restart' ? '…' : t('route.restartNow')}
              </button>
            )}
          </div>
        )}
        {running && hotReloadError && (
          <span
            className="pill warning hot_reload_error"
            title={t('app.gateway.hotReloadFailed.help', { code: hotReloadError })}
            data-testid="gateway_hot_reload_error"
          >
            {t('app.gateway.hotReloadFailed')}
          </span>
        )}
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
      <TailnetPublicationControls gateway_running={running} on_copy={onCopy} />
      {/* Built-in chain-of-thought prompt for the Codex (/v1/responses)
       * surface. Live toggle — unlike the settings-modal fields (host /
       * port / token), flipping it takes effect on the running gateway
       * immediately, so it lives on the panel itself. */}
      {cfg.surface === 'codex' && (
        <div className="endpoint-row">
          <span className="endpoint-label">{t('app.gateway.cot.title')}</span>
          <code className="endpoint-value">
            {cotOn ? t('app.gateway.cot.on') : t('app.gateway.cot.off')}
          </code>
          <label className="toggle-switch" title={t('app.gateway.cot.help')}>
            <input
              type="checkbox"
              checked={cotOn}
              onChange={(e) => onToggleCot(e.target.checked)}
              data-testid="gateway-cot-toggle"
              aria-label={t('app.gateway.cot.title')}
            />
            <span className="toggle-slider" />
          </label>
        </div>
      )}
      {/* Codex mirror mode: exclusive (historic — every request routes
       * through AAG) vs coexist (ChatGPT login stays the desktop default,
       * AAG declared as a manually selectable third-party provider). Like
       * the CoT row this takes effect immediately (the mirror's
       * config.toml is rewritten on flip), so it lives on the panel. */}
      {cfg.launchers === 'codex' && (
        <div className="endpoint-row">
          <span className="endpoint-label">{t('codex.mirror.mode.title')}</span>
          <code className="endpoint-value">
            {mirrorMode === 'coexist'
              ? t('codex.mirror.mode.coexist')
              : t('codex.mirror.mode.exclusive')}
          </code>
          <label className="toggle-switch" title={t('codex.mirror.mode.help')}>
            <input
              type="checkbox"
              checked={mirrorMode === 'coexist'}
              onChange={(e) => onToggleMirrorMode(e.target.checked)}
              data-testid="codex-mirror-mode-toggle"
              disabled={busy === 'codex_mirror_mode'}
              aria-label={t('codex.mirror.mode.title')}
            />
            <span className="toggle-slider" />
          </label>
        </div>
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
      {/* Isolated Claude mirror status; imported login forwarding is not offered. */}
      {cfg.launchers === 'claude' && (
        <div className="endpoint-row">
          <span className="endpoint-label">{t('claude.mirror.home')}</span>
          <code className="endpoint-value">
            {claudeStatus?.mirrorDir ?? '~/.claude-cli'}
          </code>
          {claudeStatusError ? (
            <span
              className="pill off"
              title={claudeStatusError}
              data-testid="claude-mirror-error"
            >
              ⚠
            </span>
          ) : (
            <span className={claudeStatus?.initialized ? 'pill on' : 'pill off'}>
              {claudeStatus?.initialized ? '✓' : t('claude.mirror.notInitialized')}
            </span>
          )}
          <button
            className="btn icon-btn"
            onClick={() => onCopy(claudeStatus?.mirrorDir ?? '~/.claude-cli', 'claude_mirror')}
            title={t('app.gateway.copy')}
            data-tooltip={t('app.gateway.copy')}
            aria-label={t('app.gateway.copy')}
          >
            {copied === 'claude_mirror' ? '✓' : <CopyIcon />}
          </button>
        </div>
      )}
    </div>
  );
}
