import { useT } from '../../../lib/ctx';
import type { AppPageConfig } from '../../../lib/appPages';
import type { Managed_cli_status } from '../../../lib/api/managed_cli';

interface LaunchersProps {
  cfg: AppPageConfig;
  busy: string | null;
  /** null while the mirror status has not loaded (or failed to load): the
   * button stays enabled so a click surfaces the real backend error instead
   * of showing a misleading "Windows only" hint. */
  codex_desktop_supported: boolean | null;
  claude_desktop_supported: boolean | null;
  onOpenDesktop: () => void;
  onOpenCli: () => void;
  onOpenCopilot: () => void;
  onOpenGemini: () => void;
  onOpenVscode: () => void;
  onOpenClaudeDesktop: () => void;
  on_open_claude_cli: () => void;
  managed_cli: Managed_cli_status | null;
  managed_cli_error: string | null;
  on_retry_managed_cli_status: () => void;
  on_open_managed_cli: () => void;
}

/** Per-app desktop, terminal, and VS Code launcher cards. */
export function Launchers({
  cfg,
  busy,
  codex_desktop_supported,
  claude_desktop_supported,
  onOpenDesktop,
  onOpenCli,
  onOpenCopilot,
  onOpenGemini,
  onOpenVscode,
  onOpenClaudeDesktop,
  on_open_claude_cli,
  managed_cli,
  managed_cli_error,
  on_retry_managed_cli_status,
  on_open_managed_cli,
}: LaunchersProps) {
  const t = useT();

  return (
    <>
      {/* Launchers (Codex — Desktop + CLI only) */}
      {cfg.launchers === 'codex' && (
        <div className="card">
          <div className="card-head">
            <h3>{t('codex.open.title')}</h3>
          </div>
          <div className="launcher-grid">
            <div className="launcher">
              <div className="launcher-text">
                <strong>{t('codex.open.desktop')}</strong>
                <span className="muted small">{t('codex.open.desktop.help')}</span>
                {codex_desktop_supported === false && <span className="muted small">{t('launcher.desktop_unsupported')}</span>}
              </div>
              <button className="btn primary" onClick={onOpenDesktop} disabled={busy !== null || codex_desktop_supported === false}>
                {busy === 'desktop' ? '…' : t('codex.open.desktop')}
              </button>
            </div>
            <div className="launcher">
              <div className="launcher-text">
                <strong>{t('codex.open.cli')}</strong>
                <span className="muted small">{t('codex.open.cli.help')}</span>
              </div>
              <button className="btn" onClick={onOpenCli} disabled={busy !== null}>
                {busy === 'cli' ? '…' : t('codex.open.cli')}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Launchers (Claude — Desktop and CLI via CLAUDE_CONFIG_DIR mirror) */}
      {cfg.launchers === 'claude' && (
        <div className="card">
          <div className="card-head">
            <h3>{t('claude.open.title')}</h3>
          </div>
          <div className="launcher-grid">
            <div className="launcher">
              <div className="launcher-text">
                <strong>{t('claude.open.cli')}</strong>
                <span className="muted small">{t('claude.open.cli_help')}</span>
              </div>
              <button
                className="btn primary"
                onClick={on_open_claude_cli}
                disabled={busy !== null}
                data-testid="claude-open-cli"
              >
                {busy === 'claude_cli' ? '…' : t('claude.open.cli')}
              </button>
            </div>
            <div className="launcher">
              <div className="launcher-text">
                <strong>{t('claude.open.desktop')}</strong>
                <span className="muted small">{t('claude.open.desktop.help')}</span>
                {claude_desktop_supported === false && <span className="muted small">{t('launcher.desktop_unsupported')}</span>}
              </div>
              <button
                className="btn primary"
                onClick={onOpenClaudeDesktop}
                disabled={busy !== null || claude_desktop_supported === false}
                data-testid="claude-open-desktop"
              >
                {busy === 'claude_desktop' ? '…' : t('claude.open.desktop')}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Launchers (Copilot — CLI only) */}
      {cfg.launchers === 'copilot' && (
        <div className="card">
          <div className="card-head">
            <h3>{t('copilot.open.title')}</h3>
          </div>
          <div className="launcher-grid">
            <div className="launcher">
              <div className="launcher-text">
                <strong>{t('copilot.open.cli')}</strong>
                <span className="muted small">{t('copilot.open.cli.help')}</span>
              </div>
              <button className="btn primary" onClick={onOpenCopilot} disabled={busy !== null}>
                {busy === 'copilot' ? '…' : t('copilot.open.cli')}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Launchers (Gemini CLI — reads the applied ~/.gemini/.env itself) */}
      {cfg.launchers === 'gemini' && (
        <div className="card">
          <div className="card-head">
            <h3>{t('gemini.open.title')}</h3>
          </div>
          <div className="launcher-grid">
            <div className="launcher">
              <div className="launcher-text">
                <strong>{t('gemini.open.cli')}</strong>
                <span className="muted small">{t('gemini.open.cli.help')}</span>
              </div>
              <button className="btn primary" onClick={onOpenGemini} disabled={busy !== null}>
                {busy === 'gemini' ? '…' : t('gemini.open.cli')}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Launchers (VS Code — open with AAG injected) */}
      {cfg.launchers === 'vscode' && (
        <div className="card">
          <div className="card-head">
            <h3>{t('vscode.open.title')}</h3>
          </div>
          <div className="launcher-grid">
            <div className="launcher">
              <div className="launcher-text">
                <strong>{t('vscode.open.vscode')}</strong>
                <span className="muted small">{t('vscode.open.vscode.help')}</span>
              </div>
              <button className="btn primary" onClick={onOpenVscode} disabled={busy !== null}>
                {busy === 'vscode' ? '…' : t('vscode.open.vscode')}
              </button>
            </div>
          </div>
        </div>
      )}

      {cfg.launchers === 'managed_cli' && (
        <div className="card">
          <div className="card-head"><h3>{cfg.label}</h3></div>
          <div className="launcher-grid">
            <div className="launcher">
              <div className="launcher-text">
                <strong>{cfg.label}</strong>
                <span className="muted small">{t(cfg.appType === 'kilo_cli' ? 'launcher.kilo.help' : 'launcher.cline.help')}</span>
                {managed_cli_error && (
                  <>
                    <span className="error-text" data-testid="managed-cli-status-error">
                      {t('launcher.managed_status_failed', { label: cfg.label, message: managed_cli_error })}
                    </span>
                    <button className="btn" onClick={on_retry_managed_cli_status} data-testid="managed-cli-status-retry">
                      {t('launcher.managed_status_retry')}
                    </button>
                  </>
                )}
                {managed_cli?.appType === cfg.appType && !managed_cli.available && (
                  <a
                    href={managed_cli.installUrl}
                    target="_blank"
                    rel="noreferrer"
                    data-testid="managed-cli-install"
                  >
                    {t('launcher.install')}
                  </a>
                )}
              </div>
              <button
                className="btn primary"
                onClick={on_open_managed_cli}
                disabled={busy !== null || (managed_cli?.appType === cfg.appType && !managed_cli.available)}
                data-testid="managed-cli-open"
              >
                {busy === 'managed_cli' ? '…' : t('launcher.open', { label: cfg.label })}
              </button>
            </div>
          </div>
        </div>
      )}
    </>
  );
}
