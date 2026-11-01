import { useEffect, useState } from 'react';
import { get_provider_config_status } from '../../../lib/api/providers';
import type { AppType, Provider_config_status } from '../../../lib/api/types';
import { useT } from '../../../lib/ctx';
import { Config_editor_modal } from './config_editor_modal';

interface Client_config_status_props {
  app_type: AppType;
  refresh_version: number;
}

interface Config_snapshot {
  app_type: AppType;
  status: Provider_config_status | null;
  error: string | null;
}

/** App types whose live config file supports the direct JSON editor.
 *  Mirrors EDITABLE_APP_TYPES in src-tauri/src/services/provider/config_editor.rs. */
const EDITABLE_APP_TYPES: readonly AppType[] = ['kilo_cli', 'kilo_vscode'];

/** The backend sends stable reason identifiers, not display text; map them
 *  to localized keys. Unknown identifiers render nothing instead of leaking
 *  a raw key string. */
const reason_keys: Record<string, string> = {
  client_config_platform_unsupported: 'client_config.reason.platform_unsupported',
  client_config_write_unsupported: 'client_config.reason.write_unsupported',
};

/** Discovery is read-only; file writes remain bound to switch/launch actions
 *  — except the scoped direct-JSON editor for the Kilo family. */
export function Client_config_status({ app_type, refresh_version }: Client_config_status_props) {
  const t = useT();
  const [retry_version, set_retry_version] = useState(0);
  const [snapshot, set_snapshot] = useState<Config_snapshot | null>(null);
  const [editor_open, set_editor_open] = useState(false);

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
  const editable = EDITABLE_APP_TYPES.includes(app_type);

  return (
    <div className="card" data-testid="client-config-status" aria-busy={!current_snapshot}>
      <div className="card-head">
        <h3>{t('client_config.title')}</h3>
        <div style={{ display: 'flex', gap: '0.5rem' }}>
          {editable && (
            <button
              className="btn small"
              onClick={() => set_editor_open(true)}
              data-testid="client-config-edit"
            >
              {t('client_config.edit_json')}
            </button>
          )}
          <button
            className="btn small"
            onClick={() => set_retry_version((version) => version + 1)}
            disabled={!current_snapshot}
            data-testid="client-config-refresh"
          >
            {t('common.refresh')}
          </button>
        </div>
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
          {status.reason && reason_keys[status.reason] && (
            <p className="muted small">{t(reason_keys[status.reason])}</p>
          )}
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
      {editor_open && (
        <Config_editor_modal
          app_type={app_type}
          onClose={() => set_editor_open(false)}
          on_saved={() => set_retry_version((version) => version + 1)}
        />
      )}
    </div>
  );
}
