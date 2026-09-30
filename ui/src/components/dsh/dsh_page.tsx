import { useCallback, useEffect, useRef, useState } from 'react';
import {
  dsh_apply_setup, dsh_preview_setup, dsh_cancel_operation, dsh_execute_operation, dsh_get_snapshot, dsh_list_upstreams,
  dsh_open_web_ui, dsh_preview_operation, dsh_save_settings, dsh_start, dsh_stop,
  type Dsh_operation_request, type Dsh_preview, type Dsh_setup_plan, type Dsh_settings, type Dsh_snapshot, type Dsh_upstream,
} from '../../lib/api/dsh.js';
import { isDemo } from '../../lib/mock/backend.js';
import { dsh_open_marketplace } from '../../lib/api/dsh_market.js';
import { useCtx } from '../../lib/ctx.js';
import { use_dsh_i18n } from './dsh_i18n.js';
import { Dsh_plugin_list } from './dsh_plugin_list.js';
import { Dsh_preview_dialog, type Dsh_confirmation } from './dsh_preview_dialog.js';
import { Dsh_settings_form } from './dsh_settings_form.js';
import { Dsh_onboarding } from './dsh_onboarding.js';
import { Dsh_webui } from './dsh_webui.js';
import { use_onboarding_i18n } from './dsh_onboarding_i18n.js';
import { use_discovery_text } from './dsh_discovery_text.js';
import './dsh_page.css';

interface Dsh_draft { settings: Dsh_settings; digest: string }
type Pending_preview =
  | { kind: 'operation'; value: Dsh_preview }
  | { kind: 'setup'; value: Dsh_confirmation; plan: Dsh_setup_plan }
  | { kind: 'settings'; value: Dsh_confirmation; draft: Dsh_draft };

const settings_keys: (keyof Dsh_settings)[] = [
  'upstream_id', 'model_id', 'theme', 'node_path', 'dsh_entry_path', 'pnpm_entry_path',
  'port', 'startup_timeout_secs', 'operation_timeout_secs',
];

export function Dsh_page() {
  const t = use_dsh_i18n();
  const onboarding_t = use_onboarding_i18n();
  const explain = use_discovery_text();
  const { locale, theme } = useCtx();
  const [opening_market, set_opening_market] = useState(false);
  const [show_webui, set_show_webui] = useState(false);
  const [snapshot, set_snapshot] = useState<Dsh_snapshot | null>(null);
  const [upstreams, set_upstreams] = useState<Dsh_upstream[]>([]);
  const [draft, set_draft] = useState<Dsh_draft | null>(null);
  const [loading, set_loading] = useState(true);
  const [busy, set_busy] = useState(false);
  const [executing, set_executing] = useState(false);
  const [starting, set_starting] = useState(false);
  const [stopping, set_stopping] = useState(false);
  const [cancelling, set_cancelling] = useState(false);
  const [error, set_error] = useState<string | null>(null);
  const [message, set_message] = useState<string | null>(null);
  const [preview, set_preview] = useState<Pending_preview | null>(null);
  const mounted = useRef(false);
  const read_sequence = useRef(0);
  const last_snapshot = useRef<Dsh_snapshot | null>(null);
  const preview_trigger = useRef<HTMLElement | null>(null);

  const accept_snapshot = useCallback((next: Dsh_snapshot) => {
    if (!mounted.current) return;
    const previous = last_snapshot.current;
    last_snapshot.current = next;
    set_snapshot(next);
    set_draft((current) => {
      const unchanged = current && previous && current.digest === previous.settings_digest &&
        settings_keys.every((key) => current.settings[key] === previous.settings[key]);
      return !current || unchanged ? { settings: { ...next.settings }, digest: next.settings_digest } : current;
    });
  }, []);

  const refresh = useCallback(async () => {
    const sequence = ++read_sequence.current;
    const [snapshot_result, upstream_result] = await Promise.allSettled([dsh_get_snapshot(), dsh_list_upstreams()]);
    if (!mounted.current || sequence !== read_sequence.current) return;
    if (snapshot_result.status === 'fulfilled') accept_snapshot(snapshot_result.value);
    if (upstream_result.status === 'fulfilled') set_upstreams(upstream_result.value);
    const failures = [snapshot_result, upstream_result].flatMap((result) => result.status === 'rejected' ? [String(result.reason)] : []);
    if (failures.length) set_error(failures.join('\n'));
    set_loading(false);
  }, [accept_snapshot]);

  useEffect(() => {
    mounted.current = true;
    void refresh();
    return () => { mounted.current = false; read_sequence.current += 1; };
  }, [refresh]);

  const runtime_active = snapshot?.runtime.phase === 'starting' || snapshot?.runtime.phase === 'web_ready';
  const has_job = Boolean(snapshot?.job) || executing;
  useEffect(() => {
    if (!has_job && !runtime_active && !starting) return;
    let cancelled = false;
    let timer: number | undefined;
    const poll = async () => {
      try {
        const next = await dsh_get_snapshot();
        if (!cancelled) accept_snapshot(next);
      } catch (failure) {
        if (!cancelled && mounted.current) set_error(String(failure));
      }
      if (!cancelled) timer = window.setTimeout(poll, has_job ? 800 : 2500);
    };
    timer = window.setTimeout(poll, 800);
    return () => { cancelled = true; window.clearTimeout(timer); };
  }, [has_job, runtime_active, starting, accept_snapshot]);

  const locked = busy || has_job || runtime_active || starting || stopping || Boolean(preview);
  const dirty = Boolean(snapshot && draft && settings_keys.some((key) => snapshot.settings[key] !== draft.settings[key]));
  const stale = Boolean(snapshot && draft && snapshot.settings_digest !== draft.digest);

  const run = async (operation: () => Promise<void>) => {
    if (busy) return;
    set_busy(true);
    set_error(null);
    set_message(null);
    try { await operation(); }
    catch (failure) { if (mounted.current) set_error(String(failure)); }
    finally { if (mounted.current) set_busy(false); }
  };

  const review_operation = (request: Dsh_operation_request) => {
    preview_trigger.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    void run(async () => {
      const next = await dsh_preview_operation(request);
      if (mounted.current) set_preview({ kind: 'operation', value: next });
    });
  };

  const review_settings = () => {
    if (!snapshot || !draft) return;
    preview_trigger.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const changes = settings_keys.filter((key) => snapshot.settings[key] !== draft.settings[key]).map((key) => {
      const label = key === 'upstream_id' ? t('upstream') : key === 'model_id' ? t('model') : t(key);
      const render_value = (value: unknown) => key === 'upstream_id'
        ? upstreams.find((upstream) => upstream.id === value)?.name ?? String(value ?? '—') : String(value ?? '—');
      return `${label}: ${render_value(snapshot.settings[key])} → ${render_value(draft.settings[key])}`;
    });
    set_preview({ kind: 'settings', draft: { settings: { ...draft.settings }, digest: draft.digest },
      value: { title: t('settings'), changes, warnings: [t('settings_risk'), t('search_note')] } });
  };

  const review_provider_switch = (upstream_id: string, model_id: string) => {
    if (!snapshot || !draft || locked) return;
    const source = upstreams.find((upstream) => upstream.id === upstream_id);
    if (!source?.available || !model_id || !source.models.includes(model_id)) return;
    const next_settings = { ...draft.settings, upstream_id, model_id };
    const changes = settings_keys.filter((key) => snapshot.settings[key] !== next_settings[key]).map((key) => {
      const label = key === 'upstream_id' ? t('upstream') : key === 'model_id' ? t('model') : t(key);
      const render_value = (value: unknown) => key === 'upstream_id'
        ? upstreams.find((upstream) => upstream.id === value)?.name ?? String(value ?? '—') : String(value ?? '—');
      return `${label}: ${render_value(snapshot.settings[key])} → ${render_value(next_settings[key])}`;
    });
    if (!changes.length) return;
    preview_trigger.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    set_preview({ kind: 'settings', draft: { settings: next_settings, digest: draft.digest }, value: {
      title: t('provider_switch'), changes, warnings: [t('provider_switch_note'), t('settings_risk')],
    } });
  };

  const review_setup = (selection: { upstream_id: string | null; model_id: string | null }) => {
    if (!snapshot || !draft || draft.digest !== snapshot.settings_digest || settings_keys.some((key) => draft.settings[key] !== snapshot.settings[key])) return;
    preview_trigger.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    void run(async () => {
      const plan = await dsh_preview_setup({ expected_digest: snapshot.settings_digest, ...selection });
      if (!mounted.current) return;
      const selected_source = upstreams.find((upstream) => upstream.id === plan.settings.upstream_id);
      const concrete_settings = [
        `${t('node_path')}: ${plan.settings.node_path || '—'}`,
        `${t('dsh_entry_path')}: ${plan.settings.dsh_entry_path || '—'}`,
        `${t('pnpm_entry_path')}: ${plan.settings.pnpm_entry_path || '—'}`,
        `${t('upstream')}: ${selected_source?.name || plan.settings.upstream_id || '—'} · ${explain(plan.source_reason)}`,
        `${t('model')}: ${plan.settings.model_id || '—'}`,
      ];
      set_preview({ kind: 'setup', plan, value: {
        title: onboarding_t('review_title'), changes: [...plan.changes.map(explain), ...concrete_settings],
        warnings: [...plan.warnings.map(explain), onboarding_t('no_install'), onboarding_t('preview_bound'), t('search_note')],
        blockers: plan.blockers.map(explain), can_confirm: plan.can_apply, expires_at_ms: plan.expires_at_ms,
      } });
    });
  };

  const confirm_preview = () => {
    if (!preview) return;
    const confirmed = preview;
    set_preview(null);
    void run(async () => {
      if (confirmed.kind === 'setup') {
        if (!confirmed.plan.can_apply) return;
        try {
          const next = await dsh_apply_setup(confirmed.plan.plan_id);
          if (!mounted.current) return;
          accept_snapshot(next);
          set_draft({ settings: { ...next.settings }, digest: next.settings_digest });
          set_message(onboarding_t('saved'));
        } finally { await refresh(); }
      } else if (confirmed.kind === 'settings') {
        const next = await dsh_save_settings(confirmed.draft.settings, confirmed.draft.digest);
        if (!mounted.current) return;
        accept_snapshot(next);
        set_draft({ settings: { ...next.settings }, digest: next.settings_digest });
        set_message(t('saved'));
      } else {
        set_executing(true);
        try {
          const receipt = await dsh_execute_operation(confirmed.value.preview_id);
          if (mounted.current) {
            if (receipt.status === 'succeeded') set_message(`${t('succeeded')} · ${receipt.action}`);
            else set_error(`${t(receipt.status)}${receipt.error_code ? ` · ${receipt.error_code}` : ''}`);
          }
        } finally {
          await refresh();
          if (mounted.current) set_executing(false);
        }
      }
    });
  };

  const start_runtime = () => void run(async () => {
    set_starting(true);
    try { accept_snapshot(await dsh_start()); }
    finally {
      await refresh();
      if (mounted.current) set_starting(false);
    }
  });

  const stop_runtime = async () => {
    if (stopping) return;
    set_stopping(true);
    set_error(null);
    try { accept_snapshot(await dsh_stop()); }
    catch (failure) { if (mounted.current) set_error(String(failure)); }
    finally {
      await refresh();
      if (mounted.current) set_stopping(false);
    }
  };

  const cancel_operation = async () => {
    if (cancelling) return;
    set_cancelling(true);
    try {
      const cancelled = await dsh_cancel_operation();
      if (mounted.current) set_message(t(cancelled ? 'cancel_requested' : 'no_job'));
    } catch (failure) { if (mounted.current) set_error(String(failure)); }
    finally { if (mounted.current) set_cancelling(false); }
  };

  const open_marketplace = () => {
    if (opening_market) return;
    set_error(null);
    if (isDemo) {
      const market_url = new URL(window.location.href);
      market_url.search = new URLSearchParams({ view: 'dsh_market', locale, theme }).toString();
      market_url.hash = '';
      // The popup must be created synchronously inside the trusted user gesture.
      window.open(market_url.href, '_blank', 'noopener,noreferrer');
      set_message(t('marketplace_opened'));
      return;
    }
    set_opening_market(true);
    void dsh_open_marketplace(locale, theme)
      .then(() => { if (mounted.current) set_message(t('marketplace_opened')); })
      .catch((failure) => { if (mounted.current) set_error(String(failure)); })
      .finally(() => { if (mounted.current) set_opening_market(false); });
  };

  const open_native_webui = () => void run(async () => {
    await dsh_open_web_ui();
    if (mounted.current) set_message(t('open_done'));
  });

  const open_dsh_webui = () => {
    if (!snapshot || busy) return;
    set_error(null);
    set_message(null);
    set_show_webui(true);
  };

  const dialog = preview?.kind === 'operation'
    ? { title: `${t(`action_${preview.value.action}`)}${preview.value.package_name ? ` · ${preview.value.package_name}` : ''}`, ...preview.value }
    : preview?.value;

  return (
    <div className="dsh_page" data-testid="dsh_page">
      <div className="dsh_page_heading">
        <div><span className="dsh_eyebrow">DeepSeek Harness</span><h1>{t('title')}</h1><p>{t('subtitle')}</p></div>
        <div className="dsh_actions" style={{ marginTop: 0 }}>
          <button type="button" className="btn primary" disabled={loading || busy || !snapshot || show_webui || Boolean(preview)}
            onClick={open_dsh_webui} data-testid="dsh_open_webui_frontend" aria-pressed={show_webui}>{t('workspace')}</button>
          <button type="button" className="btn primary" disabled={opening_market} onClick={open_marketplace} data-testid="dsh_open_marketplace">
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.6" aria-hidden="true"><rect x="3" y="3" width="7" height="7" rx="1" /><rect x="14" y="3" width="7" height="7" rx="1" /><rect x="3" y="14" width="7" height="7" rx="1" /><path d="M14 17.5h7M17.5 14v7" /></svg>
            {t('marketplace')}
          </button>
          <button type="button" className="btn" disabled={loading || busy} data-testid="dsh_refresh"
            onClick={() => { set_error(null); void run(refresh); }}>{t('refresh')}</button>
        </div>
      </div>
      {isDemo && <div className="dsh_notice dsh_demo" data-testid="dsh_demo_notice">{t('demo')}</div>}
      <p className="dsh_boundary">{t('boundary')}</p>
      {error && <div className="dsh_notice dsh_error" role="alert" data-testid="dsh_error">{error}</div>}
      {message && <div className="dsh_notice" role="status" data-testid="dsh_message">{message}</div>}
      {loading && <div className="dsh_empty" role="status">{t('loading')}</div>}
      {!loading && !snapshot && <button type="button" className="btn" onClick={() => void run(refresh)}>{t('retry')}</button>}
      {snapshot && (show_webui ? <Dsh_webui settings={draft?.settings ?? snapshot.settings} snapshot={snapshot} upstreams={upstreams}
        busy={busy} locked={locked} t={t} on_back={() => set_show_webui(false)} on_open_native_webui={open_native_webui}
        on_switch_provider={review_provider_switch} /> : <>
          <Dsh_onboarding settings={snapshot.settings} upstreams={upstreams} locked={locked} dirty={dirty} stale={stale} on_review={review_setup} />
          <section className="dsh_card dsh_runtime_card" aria-labelledby="dsh_runtime_heading">
            <div className="dsh_section_heading">
              <div><h2 id="dsh_runtime_heading">{t('runtime')}</h2><p>{t('runtime_note')}</p></div>
              <span className={`dsh_runtime_status dsh_runtime_${snapshot.runtime.phase}`} data-testid="dsh_runtime_status" role="status">{t(snapshot.runtime.phase)}</span>
            </div>
            <div className="dsh_runtime_metrics">
              <div><strong>{snapshot.plugins.filter((plugin) => plugin.installed_version).length}</strong><span>{t('installed_count')}</span></div>
              <div><strong>{snapshot.plugins.filter((plugin) => plugin.enabled).length}</strong><span>{t('enabled_count')}</span></div>
              <div><strong>—</strong><span>{t('verified_count')}</span></div>
            </div>
            <div className="dsh_managed_home">
              <span className="dsh_hint">{t('managed_home')}</span><code>{snapshot.managed_home}</code>
              <span className={snapshot.initialized ? 'dsh_good' : 'dsh_warning_text'} data-testid="dsh_initialized">{t(snapshot.initialized ? 'initialized' : 'not_initialized')}</span>
            </div>
            <p className="dsh_hint">{t('workspace_note')}</p>
            {snapshot.runtime.error_code && <p className="dsh_error" role="alert">{snapshot.runtime.error_code}</p>}
            <div className="dsh_actions">
              {!snapshot.initialized && <button type="button" className="btn primary" disabled={locked}
                onClick={() => review_operation({ action: 'initialize', package_name: null, version: null })} data-testid="dsh_initialize">{t('initialize')}</button>}
              <button type="button" className="btn primary" disabled={locked || !snapshot.initialized || dirty}
                onClick={start_runtime} data-testid="dsh_start">{t('start')}</button>
              <button type="button" className="btn" disabled={stopping || has_job || (!runtime_active && !starting) || (busy && !starting)}
                onClick={() => void stop_runtime()} data-testid="dsh_stop">{t('stop')}</button>
              <button type="button" className="btn" disabled={busy || snapshot.runtime.phase !== 'web_ready'} data-testid="dsh_open_web_ui"
                onClick={open_native_webui}>{t('open')}</button>
            </div>
            {runtime_active && <p className="dsh_hint">{t('running_lock')}</p>}
          </section>
          {has_job && <div className="dsh_notice dsh_pending" role="status" data-testid="dsh_pending">
            <div><strong>{t('pending')}</strong><span>{snapshot.job?.action} {snapshot.job?.package_name}</span></div>
            <button type="button" className="btn" disabled={cancelling || !snapshot.job} onClick={() => void cancel_operation()} data-testid="dsh_cancel_operation">{t(cancelling ? 'busy' : 'cancel_operation')}</button>
          </div>}
          {draft && <Dsh_settings_form settings={draft.settings} upstreams={upstreams} locked={locked} dirty={dirty} stale={stale} t={t}
            on_change={(settings) => set_draft({ ...draft, settings })} on_review={review_settings}
            on_discard={() => set_draft({ settings: { ...snapshot.settings }, digest: snapshot.settings_digest })} />}
          <Dsh_plugin_list plugins={snapshot.plugins} locked={locked} initialized={snapshot.initialized} t={t} on_preview={review_operation} />
          <section className="dsh_card" aria-labelledby="dsh_receipts_heading" data-testid="dsh_receipts">
            <div className="dsh_section_heading"><div><h2 id="dsh_receipts_heading">{t('receipt_title')}</h2><p>{t('receipts_note')}</p></div></div>
            {!snapshot.receipts.length ? <p className="dsh_empty">{t('no_receipts')}</p> : <ol className="dsh_receipt_list">
              {[...snapshot.receipts].sort((left, right) => right.finished_at_ms - left.finished_at_ms).map((receipt) => <li key={receipt.operation_id} data-testid="dsh_receipt">
                <div className="dsh_receipt_heading"><strong>{t(`action_${receipt.action}`)}{receipt.package_name ? ` · ${receipt.package_name}` : ''}</strong>
                  <span className={receipt.status === 'succeeded' ? 'dsh_good' : 'dsh_warning_text'}>{t(receipt.status)}</span></div>
                <div className="dsh_receipt_detail"><code>{receipt.operation_id}</code><time dateTime={new Date(receipt.finished_at_ms).toISOString()}>{new Date(receipt.finished_at_ms).toLocaleString()}</time>
                  {receipt.version && <span>v{receipt.version}</span>}<span>{t('rollback')}: {t(receipt.rollback_status)}</span>
                  {receipt.restart_required && <span>{t('restart_required')}</span>}</div>
                {receipt.error_code && <code className="dsh_error">{receipt.error_code}</code>}
              </li>)}
            </ol>}
          </section>
        </>)}
      {dialog && <Dsh_preview_dialog preview={dialog} t={t} return_focus={preview_trigger.current} on_cancel={() => set_preview(null)} on_confirm={confirm_preview} />}
    </div>
  );
}
