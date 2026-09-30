import { useT } from '../../lib/ctx';
import type { AppPageConfig } from '../../lib/appPages';

interface LaunchersProps {
  cfg: AppPageConfig;
  busy: string | null;
  codex_desktop_supported: boolean;
  claude_desktop_supported: boolean;
  onOpenDesktop: () => void;
  onOpenCli: () => void;
  onOpenCopilot: () => void;
  onOpenVscode: () => void;
  onOpenClaudeDesktop: () => void;
  on_open_claude_cli: () => void;
}

/** Codex Desktop + CLI / Claude Desktop / Copilot CLI / VS Code launcher
 * cards. Extracted from AppPage.tsx in the file-structure refactor. */
export function Launchers({
  cfg,
  busy,
  codex_desktop_supported,
  claude_desktop_supported,
  onOpenDesktop,
  onOpenCli,
  onOpenCopilot,
  onOpenVscode,
  onOpenClaudeDesktop,
  on_open_claude_cli,
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
                {!codex_desktop_supported && <span className="muted small">{t('launcher.desktop_unsupported')}</span>}
              </div>
              <button className="btn primary" onClick={onOpenDesktop} disabled={busy !== null || !codex_desktop_supported}>
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
                {!claude_desktop_supported && <span className="muted small">{t('launcher.desktop_unsupported')}</span>}
              </div>
              <button
                className="btn primary"
                onClick={onOpenClaudeDesktop}
                disabled={busy !== null || !claude_desktop_supported}
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
    </>
  );
}
