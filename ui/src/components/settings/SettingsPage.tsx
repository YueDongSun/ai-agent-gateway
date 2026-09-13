/// Device-level settings page.
///
/// Note (Phase 15.9): AAG no longer writes live agent config files at
/// all — provider "switch" only marks the row in the DB, and the proxy
/// is the only outward surface. So there is nothing left to conflict
/// over; the page stays as the home for future toggles.
import { useT } from '../../lib/ctx';

export function SettingsPage() {
  const t = useT();
  return (
    <div className="settings-page">
      <header className="page-header">
        <h2>{t('settings.title')}</h2>
      </header>
      <p className="muted">{t('settings.noConflictNeeded')}</p>
    </div>
  );
}
