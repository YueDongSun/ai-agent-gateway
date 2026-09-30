import { useCallback, useEffect, useRef, useState } from 'react';
import { dsh_discover_runtime, type Dsh_discovery, type Dsh_runtime_candidate, type Dsh_settings, type Dsh_upstream } from '../../lib/api/dsh.js';
import { use_discovery_text } from './dsh_discovery_text.js';
import { use_onboarding_i18n } from './dsh_onboarding_i18n.js';
import { Dsh_protocol_hint } from './dsh_protocol_hint.js';
import './dsh_onboarding.css';

interface Dsh_onboarding_props {
  settings: Dsh_settings;
  upstreams: Dsh_upstream[];
  locked: boolean;
  dirty: boolean;
  stale: boolean;
  on_review: (selection: { upstream_id: string | null; model_id: string | null }) => void;
}
const runtime_components = [
  { kind: 'node', key: 'node_path' }, { kind: 'dsh', key: 'dsh_entry_path' }, { kind: 'pnpm', key: 'pnpm_entry_path' },
] as const;

export function Dsh_onboarding({ settings, upstreams, locked, dirty, stale, on_review }: Dsh_onboarding_props) {
  const t = use_onboarding_i18n();
  const explain = use_discovery_text();
  const [discovery, set_discovery] = useState<Dsh_discovery | null>(null);
  const [loading, set_loading] = useState(true);
  const [error, set_error] = useState<string | null>(null);
  const [proposed_source, set_proposed_source] = useState<string | null>(null);
  const [proposed_model, set_proposed_model] = useState<string | null>(null);
  const mounted = useRef(false);
  const sequence = useRef(0);
  const inspect_runtime = useCallback(async () => {
    const request = ++sequence.current;
    set_loading(true); set_error(null);
    try {
      const next = await dsh_discover_runtime();
      if (mounted.current && request === sequence.current) set_discovery(next);
    } catch (failure) {
      if (mounted.current && request === sequence.current) set_error(String(failure));
    } finally { if (mounted.current && request === sequence.current) set_loading(false); }
  }, []);
  useEffect(() => {
    mounted.current = true;
    void inspect_runtime();
    return () => { mounted.current = false; sequence.current += 1; };
  }, [inspect_runtime]);
  useEffect(() => { set_proposed_source(null); set_proposed_model(null); }, [settings.upstream_id, settings.model_id]);
  const source_id = settings.upstream_id || proposed_source;
  const model_id = settings.model_id || proposed_model;
  const selected_source = upstreams.find((upstream) => upstream.id === source_id);
  const available_sources = upstreams.filter((upstream) => upstream.available);
  const candidate_for = (kind: Dsh_runtime_candidate['kind'], path: string) => path
    ? discovery?.candidates.find((candidate) => candidate.kind === kind && candidate.path === path)
    : discovery?.candidates.find((candidate) => candidate.kind === kind && candidate.compatible) ?? discovery?.candidates.find((candidate) => candidate.kind === kind);
  const missing_runtime = discovery && (!discovery.recommended.node_path || !discovery.recommended.dsh_entry_path);

  return <section className="dsh_card dsh_onboarding" aria-labelledby="dsh_onboarding_heading" data-testid="dsh_onboarding">
    <div className="dsh_section_heading"><div><span className="dsh_eyebrow">{t('eyebrow')}</span><h2 id="dsh_onboarding_heading">{t('title')}</h2><p>{t('subtitle')}</p></div>
      <button type="button" className="btn" disabled={loading || locked} onClick={() => void inspect_runtime()} data-testid="dsh_discover_runtime">{t('discover')}</button></div>
    <p className="dsh_onboarding_boundary">{t('read_only')}</p>
    {loading && <p role="status" data-testid="dsh_discovery_status">{t('discovering')}</p>}
    {error && <div className="dsh_notice dsh_error" role="alert" data-testid="dsh_discovery_error"><strong>{t('discovery_error')}</strong><p>{error}</p></div>}
    {discovery && <>
      <div className="dsh_detected_grid">{runtime_components.map(({ kind, key }) => {
        const path = settings[key] || discovery.recommended[key];
        const candidate = candidate_for(kind, path);
        return <article className="dsh_detected_runtime" key={kind} data-testid={`dsh_detected_${kind}`} data-compatible={candidate?.compatible ?? false} data-path={candidate?.path ?? ''}>
          <div className="dsh_detected_heading"><h3>{t(kind)}</h3><span className={`dsh_badge ${candidate?.compatible ? 'dsh_good' : 'dsh_warning_text'}`}>{t(candidate ? candidate.compatible ? 'detected' : 'unsupported' : 'missing')}</span></div>
          <dl><div><dt>{t('detected_path')}</dt><dd><code>{candidate?.path || path || '—'}</code></dd></div>
            <div><dt>{t('reported_version')}</dt><dd>{candidate?.version || t('version_unknown')}</dd></div>
            <div><dt>{t('provenance')}</dt><dd>{candidate ? explain(candidate.source) : '—'}</dd></div>
            <div><dt>{t('validation')}</dt><dd>{candidate ? explain(candidate.validation) : t('missing')}</dd></div></dl>
          {candidate?.reason && <p className="dsh_warning_text">{explain(candidate.reason)}</p>}
          {kind === 'node' && candidate && <p className="dsh_hint">{t('unverified')}</p>}
        </article>;
      })}</div>
      <div className="dsh_discovery_summary" data-testid="dsh_discovery_summary"><span>{t('scan_summary')}: <strong>{discovery.scanned_locations}</strong></span><span>{t('candidates')}: <strong>{discovery.candidates.length}</strong></span></div>
      {missing_runtime && <div className="dsh_notice dsh_warning" data-testid="dsh_setup_missing"><strong>{t('runtime_missing')}</strong><p>{t('missing_guidance')}</p></div>}
      {discovery.scan_limited && <p className="dsh_hint">{t('scan_limited')}</p>}
      {discovery.issues.length > 0 && <ul className="dsh_discovery_issues">{discovery.issues.map((issue, index) => <li key={`${issue}_${index}`}>{explain(issue)}</li>)}</ul>}
      <details className="dsh_detection_details"><summary>{t('all_candidates')}</summary><p className="dsh_hint">{t('supported_versions')}</p>
        {!discovery.candidates.length ? <p>{t('empty_candidates')}</p> : <ul>{discovery.candidates.map((candidate) => <li key={candidate.candidate_id}><div><strong>{t(candidate.kind)}</strong><span>{explain(candidate.source)}</span><span className={candidate.compatible ? 'dsh_good' : 'dsh_warning_text'}>{t(candidate.compatible ? 'detected' : 'unsupported')}</span></div><code>{candidate.path}</code><p>{candidate.version || t('version_unknown')} · {explain(candidate.validation)}{candidate.reason ? ` · ${explain(candidate.reason)}` : ''}</p></li>)}</ul>}
      </details>
    </>}
    <div className="dsh_onboarding_connection"><h3>{t('proposed_connection')}</h3>
      <fieldset className="dsh_form_fields" disabled={locked || dirty || stale}><div className="dsh_settings_grid">
        <label className="dsh_field"><span>{t('source')}</span><select value={source_id ?? ''} disabled={Boolean(settings.upstream_id)} data-testid="dsh_setup_selection" onChange={(event) => {
          const next = upstreams.find((upstream) => upstream.id === event.target.value);
          set_proposed_source(event.target.value || null); set_proposed_model(next?.models.length === 1 ? next.models[0] : null);
        }}><option value="">{t('select_source')}</option>{source_id && !selected_source && <option value={source_id} disabled>{source_id} · {t('unavailable')}</option>}{upstreams.map((upstream) => <option key={upstream.id} value={upstream.id} disabled={!upstream.available}>{upstream.name} · {upstream.protocol}{upstream.available ? '' : ` · ${t('unavailable')}`}</option>)}</select></label>
        <label className="dsh_field"><span>{t('model')}</span><input value={model_id ?? ''} placeholder={t('select_model')} list="dsh_setup_models" maxLength={250} disabled={Boolean(settings.model_id) || !source_id} onChange={(event) => set_proposed_model(event.target.value || null)} data-testid="dsh_setup_model" /><datalist id="dsh_setup_models">{selected_source?.models.map((model) => <option key={model} value={model} />)}</datalist></label>
      </div></fieldset>
      {settings.upstream_id ? <p className="dsh_hint">{t('source_preserved')}{settings.model_id ? ` ${t('model_preserved')}` : ''}</p> : <p className="dsh_hint">{available_sources.length ? t('source_ambiguous') : t('source_none')}</p>}
      {selected_source && <Dsh_protocol_hint upstream={selected_source} />}
      <p className="dsh_hint">{t('recommendation_note')}</p>
    </div>
    {dirty && <p className="dsh_notice dsh_warning" data-testid="dsh_setup_dirty">{t('dirty_block')}</p>}
    {stale && <p className="dsh_error" role="alert">{t('stale_block')}</p>}
    <div className="dsh_actions"><button type="button" className="btn primary" disabled={locked || dirty || stale || loading || !discovery} data-testid="dsh_setup_review" onClick={() => on_review({ upstream_id: source_id, model_id })}>{t('review')}</button><span className="dsh_hint">{t('initialize_separately')}</span></div>
  </section>;
}
