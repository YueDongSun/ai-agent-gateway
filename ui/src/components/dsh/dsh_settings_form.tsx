import type { Dsh_settings, Dsh_upstream } from '../../lib/api/dsh.js';
import type { Dsh_translate } from './dsh_i18n.js';
import { Dsh_protocol_hint } from './dsh_protocol_hint.js';
import { useRef } from 'react';
import { use_page_actions } from '../page_actions.js';

interface Dsh_settings_form_props {
  settings: Dsh_settings;
  upstreams: Dsh_upstream[];
  locked: boolean;
  dirty: boolean;
  stale: boolean;
  t: Dsh_translate;
  on_change: (settings: Dsh_settings) => void;
  on_review: () => void;
  on_discard: () => void;
}

const runtime_path_fields = ['node_path', 'dsh_entry_path', 'pnpm_entry_path'] as const;
const timeout_fields = ['startup_timeout_secs', 'operation_timeout_secs'] as const;

export function Dsh_settings_form({ settings, upstreams, locked, dirty, stale, t, on_change, on_review, on_discard }: Dsh_settings_form_props) {
  const selected_upstream = upstreams.find((upstream) => upstream.id === settings.upstream_id);
  const unavailable_selection = Boolean(settings.upstream_id && (!selected_upstream || !selected_upstream.available));
  const update_field = <K extends keyof Dsh_settings>(key: K, value: Dsh_settings[K]) => on_change({ ...settings, [key]: value });
  const form_ref = useRef<HTMLFormElement>(null);
  use_page_actions({ target: 'DSH settings', state: locked ? 'busy' : stale ? 'error' : dirty ? 'dirty' : 'clean',
    on_save: () => form_ref.current?.requestSubmit(), save_disabled: locked || !dirty || stale || unavailable_selection,
  }, 5);

  return (
    <section className="dsh_card" aria-labelledby="dsh_settings_heading">
      <div className="dsh_section_heading">
        <div><h2 id="dsh_settings_heading">{t('settings')}</h2><p>{t('settings_note')}</p></div>
        {dirty && <span className="dsh_badge dsh_warning_text" data-testid="dsh_settings_dirty">{t('unsaved')}</span>}
      </div>
      <form ref={form_ref} onSubmit={(event) => { event.preventDefault(); on_review(); }}>
        <fieldset disabled={locked} className="dsh_form_fields">
          <div className="dsh_settings_grid">
            <label className="dsh_field">
              <span>{t('upstream')}</span>
              <select value={settings.upstream_id ?? ''} data-testid="dsh_upstream" onChange={(event) => {
                const next_upstream = upstreams.find((upstream) => upstream.id === event.target.value);
                on_change({ ...settings, upstream_id: event.target.value || null, model_id: next_upstream?.models[0] ?? null });
              }}>
                <option value="">{t('no_upstream')}</option>
                {settings.upstream_id && !selected_upstream && <option value={settings.upstream_id} disabled>{settings.upstream_id} · unavailable</option>}
                {upstreams.map((upstream) => <option key={upstream.id} value={upstream.id} disabled={!upstream.available}>
                  {upstream.name} · {upstream.protocol}{upstream.available ? '' : ` · ${upstream.reason ?? 'unavailable'}`}
                </option>)}
              </select>
            </label>
            <label className="dsh_field">
              <span>{t('model')}</span>
              <input list="dsh_model_choices" value={settings.model_id ?? ''} disabled={!settings.upstream_id}
                required={Boolean(settings.upstream_id)} maxLength={250} data-testid="dsh_model"
                onChange={(event) => update_field('model_id', event.target.value || null)} />
              <datalist id="dsh_model_choices">{selected_upstream?.models.map((model) => <option key={model} value={model} />)}</datalist>
            </label>
            <label className="dsh_field">
              <span>{t('theme')}</span>
              <select value={settings.theme} onChange={(event) => update_field('theme', event.target.value as Dsh_settings['theme'])} data-testid="dsh_theme">
                {(['system', 'light', 'dark'] as const).map((theme) => <option key={theme} value={theme}>{t(theme)}</option>)}
              </select>
            </label>
            <label className="dsh_field">
              <span>{t('port')}</span>
              <input type="number" min={1024} max={65535} step={1} required value={settings.port}
                onChange={(event) => update_field('port', Number(event.target.value))} data-testid="dsh_port" />
            </label>
          </div>
          {selected_upstream && <div className="dsh_protocol" data-testid="dsh_upstream_protocol">
            <span className="dsh_badge">{selected_upstream.source_kind === 'claude_native' ? 'Native Claude' : selected_upstream.source_kind === 'gateway' ? 'AAG Gateway' : 'AAG Provider'}</span>
            <code>{selected_upstream.protocol}</code>
            {!selected_upstream.available && <span className="dsh_error">{selected_upstream.reason}</span>}
          </div>}
          {selected_upstream && <Dsh_protocol_hint upstream={selected_upstream} />}
          <p className="dsh_hint">{t('model_note')}</p>
          <p className="dsh_hint">{t('upstream_note')}</p>
          <details className="dsh_runtime_paths" open={!settings.node_path || !settings.dsh_entry_path}>
            <summary>{t('runtime_paths')}</summary>
            <p className="dsh_hint">{t('path_note')}</p>
            <p className="dsh_hint">{t('supported_runtime')}</p>
            {runtime_path_fields.map((field) => <label key={field} className="dsh_field">
              <span>{t(field)}</span>
              <input value={settings[field]} spellCheck={false} autoComplete="off" data-testid={`dsh_${field === 'dsh_entry_path' ? 'entry_path' : field}`}
                onChange={(event) => update_field(field, event.target.value)} />
            </label>)}
            <div className="dsh_settings_grid">
              {timeout_fields.map((field) => <label key={field} className="dsh_field">
                <span>{t(field)}</span><input type="number" min={5} max={field === 'startup_timeout_secs' ? 180 : 600} step={1}
                  value={settings[field]} required data-testid={`dsh_${field}`}
                  onChange={(event) => update_field(field, Number(event.target.value))} />
              </label>)}
            </div>
          </details>
        </fieldset>
        <p className="dsh_notice dsh_warning">{t('search_note')}</p>
        {stale && <p role="alert" className="dsh_error">{t('stale_settings')}</p>}
        <div className="dsh_actions">
          <button type="submit" className="btn primary" disabled={locked || !dirty || stale || unavailable_selection} data-testid="dsh_preview_settings">{t('preview_settings')}</button>
          <button type="button" className="btn" disabled={locked || (!dirty && !stale)} onClick={on_discard} data-testid="dsh_discard_settings">{t('discard')}</button>
        </div>
      </form>
    </section>
  );
}
