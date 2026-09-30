import { useT } from '../../lib/ctx';
import { CopyIcon } from '../Icons';
import { Secret_value } from '../secret_value.js';
import type { AppPageConfig } from '../../lib/appPages';
import type { CodexCliStatus } from '../../lib/api/codexCli';
import type { ClaudeMirrorStatus } from '../../lib/api/claudeMirror';

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
  /** Claude mirror mode: 'token' (AAG proxy token) or 'passthrough'
   *  (subscription OAuth relayed by anthropic_passthrough rows). */
  claudeMode?: string;
  /** Flip the Claude mirror mode — rewrites the mirror settings.json
   *  immediately. */
  onToggleClaudeMode?: (passthrough: boolean) => void;
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
  claudeStatus,
  claudeStatusError,
  copied,
  busy,
  cotOn,
  onToggleCot,
  mirrorMode,
  onToggleMirrorMode,
  claudeMode,
  onToggleClaudeMode,
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
      {/* Claude mirror (CLAUDE_CONFIG_DIR): mode toggle + mirror-home row.
       * Like the Codex rows these rewrite the mirror settings.json
       * immediately on flip; the native ~/.claude is never touched. */}
      {cfg.launchers === 'claude' && (
        <div className="endpoint-row">
          <span className="endpoint-label">{t('claude.mirror.mode.title')}</span>
          <code className="endpoint-value">
            {claudeMode === 'passthrough'
              ? t('claude.mirror.mode.passthrough')
              : t('claude.mirror.mode.token')}
          </code>
          {onToggleClaudeMode && (
            <label className="toggle-switch" title={t('claude.mirror.mode.help')}>
              <input
                type="checkbox"
                checked={claudeMode === 'passthrough'}
                onChange={(e) => onToggleClaudeMode(e.target.checked)}
                data-testid="claude-mirror-mode-toggle"
                aria-label={t('claude.mirror.mode.title')}
              />
              <span className="toggle-slider" />
            </label>
          )}
        </div>
      )}
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
