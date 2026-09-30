import { useEffect, useState } from 'react';
import { get_provider_config_status } from '../../lib/api/providers';
import type { AppType, Provider_config_status } from '../../lib/api/types';
import { useT } from '../../lib/ctx';

interface Client_config_status_props {
  app_type: AppType;
  refresh_version: number;
}

interface Config_snapshot {
  app_type: AppType;
  status: Provider_config_status | null;
  error: string | null;
}

/** Discovery is read-only; file writes remain bound to switch/launch actions. */
export function Client_config_status({ app_type, refresh_version }: Client_config_status_props) {
  const t = useT();
  const [retry_version, set_retry_version] = useState(0);
  const [snapshot, set_snapshot] = useState<Config_snapshot | null>(null);

  useEffect(() => {
    let active = true;
    set_snapshot(null);
    get_provider_config_status(app_type)
      .then((status) => {
        if (active) set_snapshot({ app_type, status, error: null });
      })
      .catch((error: unknown) => {
        if (active) set_snapshot({ app_type, status: null, error: String(error) });
      });
    return () => { active = false; };
  }, [app_type, refresh_version, retry_version]);

  const current_snapshot = snapshot?.app_type === app_type ? snapshot : null;
  const status = current_snapshot?.status;
  const error = current_snapshot?.error;

  return (
    <div className="card" data-testid="client-config-status" aria-busy={!current_snapshot}>
      <div className="card-head">
        <h3>{t('client_config.title')}</h3>
        <button
          className="btn small"
          onClick={() => set_retry_version((version) => version + 1)}
          disabled={!current_snapshot}
          data-testid="client-config-refresh"
        >
          {t('common.refresh')}
        </button>
      </div>
      {!current_snapshot && <p className="muted small">{t('client_config.detecting')}</p>}
      {error && <p role="alert" className="error-banner">{t('client_config.failed', { message: error })}</p>}
      {status && (
        <>
          <p className="muted small" data-testid="client-config-summary">
            {t(`client_config.platform.${status.platform}`)} · {t(`client_config.source.${status.source}`)}
          </p>
          <p className="small" data-testid="client-config-mode">
            {t(`client_config.mode.${status.supported ? status.mode : 'unsupported'}`)}
          </p>
          {status.reason && <p className="muted small">{status.reason}</p>}
          {status.configPaths.length > 0 && (
            <>
              <p className="muted small">
                {t(status.existing ? 'client_config.existing' : 'client_config.will_create')}
              </p>
              <ul className="small" data-testid="client-config-paths">
                {status.configPaths.map((path) => (
                  <li key={path}><code style={{ overflowWrap: 'anywhere' }}>{path}</code></li>
                ))}
              </ul>
            </>
          )}
        </>
      )}
    </div>
  );
}
