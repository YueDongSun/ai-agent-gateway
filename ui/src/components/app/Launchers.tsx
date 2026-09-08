import { useT } from '../../lib/ctx';
import type { AppPageConfig } from '../../lib/appPages';

interface LaunchersProps {
  cfg: AppPageConfig;
  busy: string | null;
  onOpenDesktop: () => void;
  onOpenCli: () => void;
  onOpenCopilot: () => void;
}

/** Codex Desktop + CLI / Copilot CLI launcher cards.
 * Extracted from AppPage.tsx in the file-structure refactor. */
export function Launchers({ cfg, busy, onOpenDesktop, onOpenCli, onOpenCopilot }: LaunchersProps) {
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
              </div>
              <button className="btn primary" onClick={onOpenDesktop} disabled={busy === 'desktop'}>
                {busy === 'desktop' ? '…' : t('codex.open.desktop')}
              </button>
            </div>
            <div className="launcher">
              <div className="launcher-text">
                <strong>{t('codex.open.cli')}</strong>
                <span className="muted small">{t('codex.open.cli.help')}</span>
              </div>
              <button className="btn" onClick={onOpenCli} disabled={busy === 'cli'}>
                {busy === 'cli' ? '…' : t('codex.open.cli')}
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
              <button className="btn primary" onClick={onOpenCopilot} disabled={busy === 'copilot'}>
                {busy === 'copilot' ? '…' : t('copilot.open.cli')}
              </button>
            </div>
          </div>
        </div>
      )}
    </>
  );
}
