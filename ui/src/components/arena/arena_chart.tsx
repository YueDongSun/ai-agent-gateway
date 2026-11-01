import { useCallback, useEffect, useId, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { arena_refresh_error_code, get_arena_snapshot, refresh_arena_evidence, type Arena_snapshot, type Arena_surface } from '../../lib/api/arena_visualization.js';
import { arena_gap, arena_geometry, arena_selected_assessment } from '../../lib/arena_geometry.js';
import './arena_chart.css';

const chart_config = { refresh_ms: 15_000, min_width: 260, chart_padding: 50 };
const refresh_messages: Record<string, string> = {
  fetch_timeout: 'The Arena source timed out. Retry to load its published ratings.',
  fetch_transport: 'The Arena source could not be reached. Check the connection and retry.',
  fetch_http_error: 'The Arena source returned an error. Retry to load its published ratings.',
  fetch_too_large: 'The Arena response exceeded its size limit. No unverified ratings were accepted.',
  fetch_decode_failed: 'The Arena response could not be validated. No unverified ratings were accepted.',
  fetch_schema_invalid: 'The Arena response could not be validated. No unverified ratings were accepted.',
  arena_evidence_unavailable: 'Arena ratings could not be loaded. Retry to fetch validated public evidence.',
};

export interface Arena_model_option { selector: string; label: string }
export interface Arena_chart_props {
  model_options: Arena_model_option[];
  fixed_surface?: Arena_surface;
  initial_selector?: string;
  context_label?: string;
  show_observed?: boolean;
  on_state?: (state: string) => void;
}

/** Model-only presentation shared by inventory previews and selected client plans. */
export function Arena_chart({ model_options, fixed_surface, initial_selector, context_label, show_observed = true, on_state }: Arena_chart_props) {
  const initial_selection = fixed_surface
    ? model_options.find((option) => option.selector === initial_selector)?.selector ?? model_options[0]?.selector ?? ''
    : initial_selector ?? '';
  const selection_key = fixed_surface ? JSON.stringify([fixed_surface, initial_selection, model_options.map((option) => option.selector)]) : 'gateway';
  const [selection, set_selection] = useState({ key: selection_key, value: initial_selection });
  const selector = selection.key === selection_key ? selection.value : initial_selection;
  const [selected_surface, set_selected_surface] = useState<Arena_surface>('claude');
  const surface = fixed_surface ?? selected_surface;
  const can_load = !fixed_surface || model_options.some((option) => option.selector === selector);
  const [needs_tools, set_needs_tools] = useState(false);
  const [needs_images, set_needs_images] = useState(false);
  const request_context = JSON.stringify([selector, surface, needs_tools, needs_images]);
  const [loaded, set_loaded] = useState<{ context: string; snapshot: Arena_snapshot } | null>(null);
  const snapshot = loaded?.context === request_context ? loaded.snapshot : null;
  const current = arena_selected_assessment(snapshot);
  const [busy, set_busy] = useState(false);
  const [refreshing, set_refreshing] = useState(false);
  const [error, set_error] = useState(false);
  const [refresh_error, set_refresh_error] = useState<string | null>(null);
  const source_error = snapshot?.source.lastError
    ? arena_refresh_error_code(snapshot.source.lastError)
    : snapshot && snapshot.entries.length === 0 && snapshot.source.fetchedAt !== null ? 'arena_evidence_unavailable' : null;
  const failure_code = refresh_error ?? source_error;
  const failed = error || failure_code !== null;
  const loading = busy || (can_load && !snapshot && !error);
  const mounted = useRef(false);
  const request_version = useRef(0);
  const refreshing_ref = useRef(false);
  const bootstrap_attempted = useRef(false);
  const [reload_version, set_reload_version] = useState(0);
  const chart_container = useRef<HTMLDivElement>(null);
  const [chart_width, set_chart_width] = useState(800);
  const marker_id = `arena_arrow_${useId().replace(/:/g, '')}`;

  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; request_version.current++; };
  }, []);

  const refresh_evidence = useCallback(async (only_if_stale = false) => {
    if (refreshing_ref.current) return;
    bootstrap_attempted.current = true;
    refreshing_ref.current = true;
    request_version.current++;
    set_refreshing(true);
    try {
      await refresh_arena_evidence(only_if_stale);
      if (mounted.current) set_refresh_error(null);
    } catch (cause) { if (mounted.current) set_refresh_error(arena_refresh_error_code(cause)); }
    finally {
      refreshing_ref.current = false;
      if (mounted.current) { set_refreshing(false); set_reload_version((version) => version + 1); }
    }
  }, []);

  useEffect(() => {
    if (!can_load) { set_loaded(null); set_busy(false); set_error(false); return; }
    let active = true;
    let in_flight = false;
    const load = async () => {
      if (in_flight || refreshing_ref.current || document.visibilityState === 'hidden') return;
      const version = ++request_version.current;
      in_flight = true;
      set_busy(true);
      try {
        const next = await get_arena_snapshot(selector || null, needs_tools, needs_images, surface);
        if (active && request_version.current === version) {
          set_loaded({ context: request_context, snapshot: next }); set_error(false);
          if (next.entries.length === 0 && !next.source.lastError) {
            if (!bootstrap_attempted.current) void refresh_evidence(true);
            else if (!refreshing_ref.current) set_refresh_error((prior) => prior ?? 'arena_evidence_unavailable');
          }
        }
      } catch { if (active && request_version.current === version) set_error(true); }
      finally { in_flight = false; if (active) set_busy(false); }
    };
    void load();
    const interval = window.setInterval(load, chart_config.refresh_ms);
    window.addEventListener('focus', load);
    document.addEventListener('visibilitychange', load);
    return () => { active = false; request_version.current++; clearInterval(interval); window.removeEventListener('focus', load); document.removeEventListener('visibilitychange', load); };
  }, [request_context, can_load, reload_version, refresh_evidence]);

  useEffect(() => { on_state?.(!can_load ? 'No configured model' : refreshing ? 'Refreshing' : loading ? 'Updating' : failed ? 'Error' : !snapshot?.entries.length ? 'No cached ratings' : !current ? 'Unrated selection' : snapshot.source.stale ? 'Cached · stale' : 'Ready'); }, [can_load, loading, refreshing, failed, snapshot, current, on_state]);
  useEffect(() => () => { on_state?.('Cached · paused'); }, [on_state]);
  const geometry = useMemo(() => arena_geometry(snapshot?.entries ?? []), [snapshot]);
  useLayoutEffect(() => {
    const container = chart_container.current;
    if (!container) return;
    const measure = () => set_chart_width(Math.max(chart_config.min_width, Math.floor(container.clientWidth)));
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(container);
    return () => observer.disconnect();
  }, [Boolean(geometry)]);
  const chart_x = (rating: number) => chart_config.chart_padding + ((geometry?.x(rating) ?? 60) - 60) / 680 * (chart_width - chart_config.chart_padding * 2);
  const fable = snapshot?.fableCandidates.find((candidate) => candidate.arena)?.arena ?? null;
  const gap = arena_gap(current, fable);
  const rank_y = (rank: number) => 80 + ((rank - 1) / Math.max(1, (geometry?.count ?? 1) - 1)) * 135;
  const flow = snapshot?.candidates ?? [];
  const flow_coordinates = geometry ? flow.flatMap((candidate, index) => candidate.arena
    ? [{ x: chart_x(candidate.arena.rating), y: rank_y(candidate.arena.rank), index }]
    : []) : [];

  if (!can_load) return <section className="card arena_panel" data-testid="arena-chart">
    <div className="card-head"><h3>Arena.ai · model position</h3><span className="tag" role="status" data-testid="arena-state">No configured model</span></div>
    <p className="muted" data-testid="arena-empty-plan">No router model is configured for this plan. Arena positions become available after a model is selected.</p>
  </section>;

  return <section className="card arena_panel" data-testid="arena-chart" data-surface={surface} data-selector={selector} aria-busy={loading || refreshing}>
    <div className="card-head"><h3>{fixed_surface ? 'Arena.ai · model position' : 'Router position · Arena overall'}</h3>
      <span className="tag" role="status" data-testid="arena-state">{refreshing ? 'Loading ratings' : loading ? 'Updating' : failed ? snapshot?.entries.length ? 'Cached · refresh failed' : 'Ratings unavailable' : !snapshot?.entries.length ? 'No cached ratings' : snapshot.source.stale ? 'Cached · stale' : snapshot.synthetic ? 'Synthetic demo' : 'Evidence ready'}</span>
    </div>
    {fixed_surface && <p className="muted small" data-testid="arena-plan-context">{context_label ? `${context_label} · ` : ''}Current plan preview · <code>{selector}</code></p>}
    <div className="arena_controls">
      {!fixed_surface && <label>Client route<select className="input" value={surface} onChange={(event) => set_selected_surface(event.target.value as Arena_surface)}><option value="claude">Claude</option><option value="codex">Codex</option><option value="chat">Chat</option><option value="gemini">Gemini</option></select></label>}
      <label>{fixed_surface ? 'Current plan model' : 'Router selector'}<select className="input" data-testid="arena-selector" value={selector} onChange={(event) => set_selection({ key: selection_key, value: event.target.value })}>
        {!fixed_surface && <><option value="">Current router model</option><option value="alias:fable">Fable tier</option><option value="alias:opus">Opus tier</option><option value="alias:sonnet">Sonnet tier</option><option value="alias:haiku">Haiku tier</option></>}
        {model_options.map((option) => <option key={option.selector} value={option.selector}>{option.label}</option>)}
      </select></label>
      <label className="arena_check"><input type="checkbox" data-testid="arena-needs-tools" checked={needs_tools} onChange={(event) => set_needs_tools(event.target.checked)} />Needs tools</label>
      <label className="arena_check"><input type="checkbox" data-testid="arena-needs-images" checked={needs_images} onChange={(event) => set_needs_images(event.target.checked)} />Needs images</label>
      <button className="btn small" disabled={refreshing} onClick={() => void refresh_evidence()} data-testid="arena-refresh">{failed ? 'Retry Arena evidence' : 'Refresh Arena evidence'}</button>
    </div>
    {failed && <p className="error-banner" role="alert" data-testid="arena-refresh-error">{refresh_messages[failure_code ?? 'arena_evidence_unavailable']} {snapshot?.entries.length ? 'Previously validated ratings remain visible.' : ''}</p>}
    {snapshot?.synthetic && <p className="notice-banner">Synthetic demo scores and paths. These are not published Arena results.</p>}
    {show_observed && <p className="muted small" data-testid="arena-current">{snapshot?.observed ? `Last recorded router model: ${snapshot.observed.model} · ${new Date(snapshot.observed.observedAt * 1000).toLocaleString()}` : 'No routed model has been recorded yet.'}</p>}
    {geometry ? <>
      <div ref={chart_container} className="arena_scroll" role="region" aria-label="Arena rating distribution and configured routing strategy">
        <svg viewBox={`0 0 ${chart_width} 325`} className="arena_svg" role="img" aria-labelledby={`${marker_id}_title ${marker_id}_desc`}>
          <title id={`${marker_id}_title`}>Overall Arena rating coordinates and routing candidate order</title>
          <desc id={`${marker_id}_desc`}>{geometry.count} rated models. Horizontal position is the published rating, vertical position is published rank. Whiskers show rating intervals. Numbered markers correspond to candidate positions; the complete fallback queue below also includes unrated models.</desc>
          <defs><marker id={marker_id} markerWidth="7" markerHeight="7" refX="6" refY="3.5" orient="auto"><path d="M0 0L7 3.5L0 7Z" fill="currentColor" /></marker></defs>
          {[0, .25, .5, .75, 1].map((fraction) => <g key={fraction}><line className="arena_grid" x1={chart_x(geometry.low + fraction * (geometry.high - geometry.low))} x2={chart_x(geometry.low + fraction * (geometry.high - geometry.low))} y1="60" y2="280" /><text x={chart_x(geometry.low + fraction * (geometry.high - geometry.low))} y="302" textAnchor="middle">{Math.round(geometry.low + fraction * (geometry.high - geometry.low))}</text></g>)}
          <text x="16" y="70">#1</text><text x="10" y="218">#{geometry.count}</text><text x="60" y="25">Higher preference rating →</text>
          {geometry.sample.map((point) => <circle key={point.modelName} className="arena_cloud" cx={chart_x(point.rating)} cy={rank_y(point.rank)} r="2.5"><title>{point.modelName} · {point.rating.toFixed(1)} · rank {point.rank}</title></circle>)}
          {geometry.bins.map((count, index) => <rect className="arena_bin" key={index} x={chart_config.chart_padding + index * (chart_width - chart_config.chart_padding * 2) / geometry.bins.length} y={280 - count / Math.max(1, ...geometry.bins) * 42} width={Math.max(1, (chart_width - chart_config.chart_padding * 2) / geometry.bins.length - 2)} height={count / Math.max(1, ...geometry.bins) * 42}><title>{count} models in this score band</title></rect>)}
          {flow_coordinates.map(({ x, y, index }) => <g key={`${flow[index].model}_${index}`}><circle className="arena_candidate" cx={x} cy={y} r="7" /><text className="arena_step" x={x} y={y + 3} textAnchor="middle">{index + 1}</text></g>)}
          {([['Current', current], ['Fable tier', fable]] as const).map(([label, point]) => point && <g key={label} className={label === 'Current' ? 'arena_selected' : 'arena_comparator'}>
            <line x1={chart_x(point.ratingLower)} x2={chart_x(point.ratingUpper)} y1={rank_y(point.rank)} y2={rank_y(point.rank)} stroke="currentColor" strokeWidth="3" />
            <circle cx={chart_x(point.rating)} cy={rank_y(point.rank)} r="9" fill="none" stroke="currentColor" strokeWidth="2" />
            <text x={chart_x(point.rating)} y={rank_y(point.rank) + (label === 'Current' ? 22 : -16)} textAnchor="middle">{label}</text>
          </g>)}
        </svg>
      </div>
      <div className="arena_summary" aria-live="polite">
        <span><strong>{current?.modelName ?? snapshot?.selectedSelector ?? 'Current model unavailable'}</strong>{current ? ` · ${current.rating.toFixed(1)} [${current.ratingLower.toFixed(1)}, ${current.ratingUpper.toFixed(1)}] · rank ${current.rank}/${geometry.count}` : ' · no comparable Arena row'}</span>
        <span>{fable ? `Fable → ${fable.modelName} · ${fable.rating.toFixed(1)}` : 'Fable tier has no comparable rated candidate.'}</span>
        {gap && <span data-testid="arena-gap">Current − Fable: {gap.difference > 0 ? '+' : ''}{gap.difference.toFixed(1)} rating points · {gap.intervals_overlap ? 'intervals overlap' : 'intervals do not overlap'}</span>}
      </div>
    </> : loading || refreshing ? <p className="muted arena_loading" role="status" data-testid="arena-loading">{refreshing ? 'Loading published Arena ratings…' : 'Loading the model position and routing preview…'}</p>
      : !failed && <p className="muted" data-testid="arena-empty">No comparable overall ranking is cached. Refresh the public source to load measured ratings; unrated models retain their identity without an invented score.</p>}
    <div className="arena_strategy" data-testid="arena-route-flow">
      <strong>{snapshot?.planSource === 'admitted_runtime' ? 'Admitted routing strategy' : 'Saved routing strategy preview'}</strong><span aria-hidden>→</span><code>{snapshot?.selectedSelector ?? 'No selection'}</code>
      <span className="muted">{flow.length} candidates · backend selection and fallback order</span>
      <ol className="routing_queue" aria-label={`Ordered fallback queue for ${snapshot?.selectedSelector ?? 'current selection'}`}>
        {flow.map((candidate, index) => <li key={`${candidate.provider ?? ''}:${candidate.model}:${index}`} className="routing_queue_item">
          {index > 0 && <span className="routing_fallback_arrow" data-testid="routing-fallback-arrow"><span aria-hidden>↓</span><span>Fallback {index}</span></span>}
          <article className="routing_candidate" data-testid="routing-candidate" data-provider={candidate.provider} data-model={candidate.model}>
            <div className="routing_candidate_heading"><span className="tag">{index + 1}</span><code>{candidate.model}</code><span className="tag">{candidate.pinned ? 'Pinned' : index === 0 ? 'Preferred' : 'Fallback'}</span></div>
            <span>Provider: <code>{candidate.provider ?? 'Not reported'}</code>{candidate.protocol ? ` · ${candidate.protocol}` : ''}</span>
            <span className="muted small">{candidate.tier === null ? 'Tier unknown' : `Tier ${candidate.tier}`} · {candidate.basis}{candidate.downgraded ? ' · downgrade' : ''} · {candidate.arena ? `Arena ${candidate.arena.rating.toFixed(1)}` : 'Arena unrated'}</span>
            {candidate.reason && <span className="small">Selection: {candidate.reason.replaceAll('_', ' ')}</span>}
            {candidate.utility && <span className="muted small">Saved allocation evidence · utility {candidate.utility.total.toFixed(2)} · quality {candidate.utility.quality.toFixed(2)} · cost efficiency {candidate.utility.costEfficiency?.toFixed(2) ?? 'unknown'} · freshness {candidate.utility.freshness?.toFixed(2) ?? 'unknown'} · task fit {candidate.utility.suitability.toFixed(2)}{candidate.utility.metadataStale ? ' · stale metadata' : ''}</span>}
            <span className="small">{candidate.health?.unavailable ? 'Recently unavailable' : candidate.health?.providerState === 'open' ? 'Provider circuit open' : candidate.health?.samples ? 'Observed model health' : 'Availability not yet observed'}
              {candidate.health?.successRate != null ? ` · ${(candidate.health.successRate * 100).toFixed(0)}% success (${candidate.health.samples} samples)` : ''}
              {candidate.health?.latencyMs != null ? ` · ${Math.round(candidate.health.latencyMs)} ms` : ''}</span>
            <span className="muted small">{candidate.cost && (candidate.cost.inputPerMillion != null || candidate.cost.outputPerMillion != null)
              ? `Reference USD / 1M tokens: input ${candidate.cost.inputPerMillion ?? 'unknown'} · output ${candidate.cost.outputPerMillion ?? 'unknown'} · ${candidate.cost.source}${candidate.cost.stale ? ' (stale)' : ''}`
              : 'Price unknown'}</span>
          </article>
        </li>)}
      </ol>
      {!flow.length && <span className="muted small">{snapshot?.planError ? 'No eligible plan for these requirements.' : 'No candidate path available.'}</span>}
    </div>
    <p className="muted small">Every candidate is shown in backend order, including unrated models and the same model on different providers. Arrows show ordered fallback from the preferred model. This is a current plan preview, not an execution trace; request capabilities, session affinity and circuit admission may change a request's path. Fable is a routing tier, not a separately measured score.</p>
    <p className="muted small">Source: <a href={snapshot?.sourceUrl || 'https://huggingface.co/datasets/lmarena-ai/leaderboard-dataset'} target="_blank" rel="noreferrer">Arena overall text rankings</a> · publication {snapshot?.publishedDate ?? 'unknown'} · {geometry?.count ?? 0} models · {!snapshot ? 'evidence pending' : snapshot.source.stale ? 'stale/missing cache' : 'cached evidence'}. Human-preference ratings and their intervals do not establish universal capability.</p>
  </section>;
}
