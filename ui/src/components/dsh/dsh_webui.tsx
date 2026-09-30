import { useState } from 'react';
import type { Dsh_settings, Dsh_snapshot, Dsh_upstream } from '../../lib/api/dsh.js';
import type { Dsh_translate } from './dsh_i18n.js';

interface Dsh_webui_props {
  settings: Dsh_settings;
  snapshot: Dsh_snapshot;
  upstreams: Dsh_upstream[];
  busy: boolean;
  locked: boolean;
  t: Dsh_translate;
  on_back: () => void;
  on_open_native_webui: () => void;
  on_switch_provider: (upstream_id: string, model_id: string) => void;
}

function provider_test_id(upstream_id: string): string {
  return upstream_id.replace(/[^A-Za-z0-9_]/g, '_');
}

function source_kind_label(source_kind: Dsh_upstream['source_kind'], t: Dsh_translate): string {
  if (source_kind === 'claude_native') return t('native_claude');
  if (source_kind === 'gateway') return t('aag_gateway');
  return t('aag_provider');
}

function model_choices(upstream: Dsh_upstream, settings: Dsh_settings): string[] {
  const current_model = settings.upstream_id === upstream.id ? settings.model_id : null;
  return [...new Set([...upstream.models, ...(current_model ? [current_model] : [])])];
}

export function Dsh_webui({ settings, snapshot, upstreams, busy, locked, t, on_back, on_open_native_webui, on_switch_provider }: Dsh_webui_props) {
  const [model_by_upstream, set_model_by_upstream] = useState<Record<string, string>>({});
  const selected_upstream = upstreams.find((upstream) => upstream.id === settings.upstream_id);
  const selected_source_name = selected_upstream?.name ?? settings.upstream_id ?? t('no_upstream');
  const native_webui_ready = snapshot.runtime.phase === 'web_ready';

  return (
    <section className="dsh_webui" data-testid="dsh_webui" aria-label={t('webui_title')}>
      <aside className="dsh_webui_sidebar">
        <div className="dsh_webui_brand">
          <span className="dsh_webui_mark" aria-hidden="true">D</span>
          <div>
            <strong>{t('webui_title')}</strong>
            <span>{t('webui_subtitle')}</span>
          </div>
        </div>
        <button type="button" className="btn dsh_webui_back" onClick={on_back} data-testid="dsh_webui_back">
          <span aria-hidden="true">←</span>{t('back_to_manager')}
        </button>
        <div className="dsh_webui_provider_heading">
          <div><span className="dsh_eyebrow">DSH</span><h2>{t('providers')}</h2></div>
          <span className="dsh_count">{upstreams.filter((upstream) => upstream.available).length}</span>
        </div>
        <div className="dsh_webui_provider_list">
          {!upstreams.length && <p className="dsh_empty">{t('no_upstream')}</p>}
          {upstreams.map((upstream) => {
            const test_id = provider_test_id(upstream.id);
            const choices = model_choices(upstream, settings);
            const selected_model = model_by_upstream[upstream.id] ?? (settings.upstream_id === upstream.id
              ? settings.model_id ?? choices[0] ?? '' : choices[0] ?? '');
            const is_current = settings.upstream_id === upstream.id && settings.model_id === selected_model;
            const can_switch = upstream.available && Boolean(selected_model) && !is_current && !locked && !busy;
            return (
              <article className={`dsh_webui_provider${is_current ? ' is_current' : ''}${!upstream.available ? ' is_unavailable' : ''}`}
                key={upstream.id} data-testid={`dsh_webui_provider_${test_id}`}>
                <div className="dsh_webui_provider_name">
                  <strong>{upstream.name}</strong>
                  {is_current && <span className="dsh_badge dsh_good">{t('current')}</span>}
                </div>
                <div className="dsh_webui_provider_meta">
                  <span>{source_kind_label(upstream.source_kind, t)}</span><code>{upstream.protocol}</code>
                </div>
                {upstream.available ? <>
                  <label className="dsh_webui_model_field">
                    <span>{t('select_model')}</span>
                    <select value={selected_model} disabled={!choices.length || locked || busy}
                      data-testid={`dsh_webui_model_${test_id}`} aria-label={`${t('select_model')}: ${upstream.name}`}
                      onChange={(event) => set_model_by_upstream((current) => ({ ...current, [upstream.id]: event.target.value }))}>
                      {!choices.length && <option value="">{t('no_models')}</option>}
                      {choices.map((model) => <option key={model} value={model}>{model}</option>)}
                    </select>
                  </label>
                  <button type="button" className={`btn${is_current ? '' : ' primary'}`} disabled={!can_switch}
                    aria-pressed={is_current} data-testid={`dsh_webui_switch_${test_id}`}
                    onClick={() => on_switch_provider(upstream.id, selected_model)}>
                    {is_current ? t('current') : t('use_provider')}
                  </button>
                </> : <p className="dsh_webui_unavailable dsh_warning_text">{upstream.reason ?? t('provider_unavailable')}</p>}
              </article>
            );
          })}
        </div>
        <p className="dsh_hint">{t('provider_switch_note')}</p>
      </aside>
      <div className="dsh_webui_main">
        <header className="dsh_webui_topbar">
          <div className="dsh_webui_current">
            <span className="dsh_eyebrow">{t('upstream')}</span>
            <strong data-testid="dsh_webui_current_provider">{selected_source_name}</strong>
            <code>{settings.model_id ?? t('no_upstream')}</code>
          </div>
          <button type="button" className="btn primary" disabled={!native_webui_ready || busy}
            onClick={on_open_native_webui} data-testid="dsh_webui_open_native">
            {t('open_native')}
          </button>
        </header>
        <main className="dsh_webui_conversation">
          <div className="dsh_webui_welcome">
            <div className="dsh_webui_welcome_mark" aria-hidden="true">✦</div>
            <span className="dsh_eyebrow">DeepSeek Harness</span>
            <h2>{t('webui_welcome')}</h2>
            <p>{t('webui_welcome_note')}</p>
            <div className="dsh_webui_context">
              <div><span>{t('upstream')}</span><strong>{selected_source_name}</strong></div>
              <div><span>{t('model')}</span><strong>{settings.model_id ?? '—'}</strong></div>
              <div><span>{t('runtime')}</span><strong className={native_webui_ready ? 'dsh_good' : 'dsh_warning_text'}>{t(snapshot.runtime.phase)}</strong></div>
            </div>
          </div>
          <p className="dsh_notice dsh_webui_boundary">{t('webui_native_hint')}</p>
        </main>
        <footer className="dsh_webui_composer">
          <div className="dsh_webui_composer_box">
            <textarea rows={2} disabled placeholder={t('webui_composer_placeholder')} aria-label={t('webui_composer_placeholder')} />
            <button type="button" className="btn primary" disabled={!native_webui_ready || busy} onClick={on_open_native_webui}>
              {t('open_native')}
            </button>
          </div>
          <span className="dsh_hint">{t('webui_native_hint')}</span>
        </footer>
      </div>
    </section>
  );
}
