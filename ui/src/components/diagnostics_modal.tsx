import { useEffect, useState } from 'react';
import { useT } from '../lib/ctx.js';
import {
  diagnostics_disable_debug,
  diagnostics_enable_debug,
  diagnostics_get,
  diagnostics_get_summary,
  diagnostics_open_log_dir,
  type Diagnostics_state,
} from '../lib/api/diagnostics.js';

interface Diagnostics_modal_props {
  open: boolean;
  on_close: () => void;
}

function remaining_label(expires_at: number | null, now: number): string {
  if (!expires_at) return '0:00';
  const remaining = Math.max(0, expires_at - Math.floor(now / 1000));
  const minutes = Math.floor(remaining / 60);
  const seconds = remaining % 60;
  return `${minutes}:${String(seconds).padStart(2, '0')}`;
}

export function Diagnostics_modal({ open, on_close }: Diagnostics_modal_props) {
  const t = useT();
  const [state, set_state] = useState<Diagnostics_state | null>(null);
  const [now, set_now] = useState(Date.now());
  const [busy, set_busy] = useState(false);
  const [message, set_message] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    set_message(null);
    void diagnostics_get().then(set_state).catch((error) => set_message(String(error)));
    const timer = window.setInterval(() => set_now(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [open]);

  if (!open) return null;

  const debug_enabled = Boolean(
    state?.debug_enabled &&
      state.debug_expires_at &&
      state.debug_expires_at > Math.floor(now / 1000),
  );

  const toggle_debug = async () => {
    set_busy(true);
    set_message(null);
    try {
      const next = debug_enabled
        ? await diagnostics_disable_debug()
        : await diagnostics_enable_debug();
      set_state(next);
    } catch (error) {
      set_message(String(error));
    } finally {
      set_busy(false);
    }
  };

  const copy_summary = async () => {
    set_busy(true);
    set_message(null);
    try {
      const summary = await diagnostics_get_summary();
      await navigator.clipboard.writeText(summary);
      set_message(t('diagnostics.summary_copied'));
    } catch (error) {
      set_message(String(error));
    } finally {
      set_busy(false);
    }
  };

  const open_logs = async () => {
    set_busy(true);
    set_message(null);
    try {
      await diagnostics_open_log_dir();
    } catch (error) {
      set_message(String(error));
    } finally {
      set_busy(false);
    }
  };

  return (
    <div className="modal-backdrop" onClick={on_close} data-testid="diagnostics_backdrop">
      <section
        className="modal diagnostics-modal"
        role="dialog"
        aria-modal="true"
        aria-label={t('diagnostics.title')}
        onClick={(event) => event.stopPropagation()}
      >
        <h3>{t('diagnostics.title')}</h3>
        <p className="muted small">{t('diagnostics.description')}</p>
        <dl className="diagnostics-grid">
          <dt>{t('diagnostics.mode')}</dt>
          <dd data-testid="diagnostics_mode">
            {debug_enabled ? t('diagnostics.debug') : t('diagnostics.info')}
          </dd>
          <dt>{t('diagnostics.remaining')}</dt>
          <dd>{debug_enabled ? remaining_label(state?.debug_expires_at ?? null, now) : '—'}</dd>
          <dt>{t('diagnostics.retention')}</dt>
          <dd>
            {state
              ? `${state.retained_file_count} × ${Math.round(state.max_file_size_bytes / 1024 / 1024)} MiB`
              : '—'}
          </dd>
          <dt>{t('diagnostics.directory')}</dt>
          <dd className="diagnostics-path">{state?.log_directory || '—'}</dd>
        </dl>
        {message && <div className="notice-banner">{message}</div>}
        <div className="modal-actions diagnostics-actions">
          <button className="btn" onClick={copy_summary} disabled={busy || !state}>
            {t('diagnostics.copy_summary')}
          </button>
          <button className="btn" onClick={open_logs} disabled={busy || !state}>
            {t('diagnostics.open_logs')}
          </button>
          <button
            className={`btn ${debug_enabled ? '' : 'primary'}`}
            onClick={toggle_debug}
            disabled={busy || !state}
            data-testid="diagnostics_toggle"
          >
            {debug_enabled ? t('diagnostics.disable') : t('diagnostics.enable')}
          </button>
          <button className="btn" onClick={on_close} disabled={busy}>
            {t('common.close')}
          </button>
        </div>
      </section>
    </div>
  );
}
