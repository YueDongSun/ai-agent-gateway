import { useEffect, useMemo, useRef, useState } from 'react';
import {
  workflow_error_message,
  workflow_evaluate_route,
  workflow_get_health,
  workflow_list_capabilities,
  workflow_probe_capability,
  workflow_route_preview,
  type Workflow_capability,
  type Workflow_health,
  type Workflow_observation,
  type Workflow_route_preview,
} from '../../lib/api/workflow_registry.js';
import './workflow_panel.css';

const stage_order = ['installed', 'configured', 'availability', 'connected', 'discovered', 'callable', 'probe'];
const date_formatter = new Intl.DateTimeFormat(undefined, { dateStyle: 'medium', timeStyle: 'short' });

function observation_is_stale(observation: Workflow_observation, now: number): boolean {
  if (!observation.expires_at) return false;
  const expiry = Date.parse(observation.expires_at);
  return Number.isFinite(expiry) && expiry <= now;
}

function observation_label(observation: Workflow_observation | undefined, now: number): string {
  if (!observation) return 'Unknown';
  if (observation_is_stale(observation, now)) return 'Stale';
  if (observation.status.toLowerCase() === 'unknown') return 'Unknown';
  return observation.status.replaceAll('_', ' ');
}

function format_observed_at(value: string | null): string {
  if (!value) return 'Not observed';
  const parsed = Date.parse(value);
  return Number.isFinite(parsed) ? date_formatter.format(parsed) : 'Timestamp unavailable';
}

function format_confidence(value: number | null): string {
  if (value === null || !Number.isFinite(value)) return 'Not provided';
  return `${(value * 100).toFixed(0)}%`;
}

function confidence_label(source: string): string {
  return source.toLowerCase().includes('typesafe') ? 'TypeSafe confidence' : 'Confidence';
}

function display_route_value(value: string | null | undefined): string {
  return value?.trim() ? value : 'Unavailable';
}

function observed_stage_order(observations: Workflow_observation[]): string[] {
  const present = new Set(observations.map((observation) => observation.stage));
  const additional_stages = [...present].filter((stage) => !stage_order.includes(stage)).sort();
  return [...stage_order, ...additional_stages];
}

function merge_health_observations(capabilities: Workflow_capability[], health: Workflow_health): Workflow_capability[] {
  return capabilities.map((capability) => {
    const observations = health.observations.filter((item) => item.capability_id === capability.id);
    if (observations.length === 0) return capability;
    const observations_by_stage = new Map(capability.observations.map((item) => [item.stage, item]));
    for (const observation of observations) observations_by_stage.set(observation.stage, observation);
    return { ...capability, observations: [...observations_by_stage.values()] };
  });
}

function Route_preview_card({ route }: { route: Workflow_route_preview }) {
  return <article className="workflow_route_result" data-testid="workflow_route_result">
    <div className="workflow_route_heading"><h4>Deterministic preview</h4><span className="workflow_tag">{route.advice_status.replaceAll('_', ' ')}</span></div>
    <p>{route.summary}</p>
    <dl className="workflow_route_facts">
      <div><dt>Difficulty</dt><dd>{route.difficulty}</dd></div>
      <div><dt>Suggested route</dt><dd>{display_route_value(route.model)}</dd></div>
      <div><dt>Fallback</dt><dd>{display_route_value(route.fallback_model)}</dd></div>
      <div><dt>Final gate</dt><dd>{display_route_value(route.final_gate_model)}</dd></div>
      <div><dt>Reasoning effort</dt><dd>{display_route_value(route.reasoning_effort)}</dd></div>
      <div><dt>Score</dt><dd>{Number.isFinite(route.score) ? route.score.toFixed(2) : 'Unavailable'}</dd></div>
    </dl>
    {route.signals.length > 0 && <p className="workflow_signals"><strong>Signals:</strong> {route.signals.join(', ')}</p>}
    {route.task_advice && <div className="workflow_advice" data-testid="workflow_task_advice">
      <h5>Local task observation</h5>
      <p>{route.task_advice.summary}</p>
      <dl className="workflow_route_facts">
        <div><dt>Observation status</dt><dd>{route.task_advice.status.replaceAll('_', ' ')}</dd></div>
        <div><dt>Work category</dt><dd>{route.task_advice.workflow_class?.replaceAll('_', ' ') ?? 'Not observed'}</dd></div>
        <div><dt>Distribution confidence</dt><dd>{format_confidence(route.task_advice.confidence)}</dd></div>
        <div><dt>Model</dt><dd>{route.task_advice.checkpoint}</dd></div>
        <div><dt>Observed</dt><dd>{format_observed_at(route.task_advice.observed_at)}</dd></div>
        <div><dt>Inference latency</dt><dd>{route.task_advice.latency_ms === null ? 'Not measured' : `${route.task_advice.latency_ms.toFixed(2)} ms`}</dd></div>
      </dl>
      <p className="workflow_muted">Observation mode. Confidence describes the answer distribution; classification does not change the route, dispatch work, or grant permission.</p>
    </div>}
    {route.model_advice && <div className="workflow_advice" data-testid="workflow_model_advice">
      <h5>Optional model advice</h5>
      <p>{route.model_advice.summary}</p>
      <dl className="workflow_route_facts">
        <div><dt>Advice status</dt><dd>{route.model_advice.status.replaceAll('_', ' ')}</dd></div>
        <div><dt>Source</dt><dd>{route.model_advice.source}</dd></div>
        <div><dt>Suggested model</dt><dd>{display_route_value(route.model_advice.model)}</dd></div>
        <div><dt>{confidence_label(route.model_advice.source)}</dt><dd>{format_confidence(route.model_advice.confidence)}</dd></div>
      </dl>
      <p className="workflow_muted">Advice is informational. It does not change the deterministic preview, dispatch work, or grant access.</p>
    </div>}
    {route.jev_evidence && <div className="workflow_advice" data-testid="workflow_jev_evidence">
      <h5>Original Jev evidence</h5>
      <p>{route.jev_evidence.summary}</p>
      <dl className="workflow_route_facts">
        <div><dt>Evidence status</dt><dd>{route.jev_evidence.status.replaceAll('_', ' ')}</dd></div>
        <div><dt>Source</dt><dd>{route.jev_evidence.source}</dd></div>
        <div><dt>Suggested model</dt><dd>{display_route_value(route.jev_evidence.model)}</dd></div>
        <div><dt>{confidence_label(route.jev_evidence.source)}</dt><dd>{format_confidence(route.jev_evidence.confidence)}</dd></div>
      </dl>
      <p className="workflow_muted">Jev evidence is retained separately from the final route and any independent fallback assessment.</p>
    </div>}
  </article>;
}

const stage_clock_interval_ms = 1000;

/** Renders the selected capability's observed-stage list and owns the 1 Hz
 *  staleness clock, so a tick re-renders only these labels instead of the
 *  whole panel (registry listbox, detail facts, route preview). The clock is
 *  paused while the document is hidden, like the other UI polls. */
function Observed_stages({ capability }: { capability: Workflow_capability }) {
  const [now, set_now] = useState(() => Date.now());
  useEffect(() => {
    let interval_id: number | null = null;
    const start = () => {
      if (interval_id !== null) return;
      interval_id = window.setInterval(() => set_now(Date.now()), stage_clock_interval_ms);
    };
    const stop = () => {
      if (interval_id !== null) {
        window.clearInterval(interval_id);
        interval_id = null;
      }
    };
    if (document.visibilityState !== 'hidden') start();
    const on_visibility = () => (document.visibilityState === 'hidden' ? stop() : start());
    document.addEventListener('visibilitychange', on_visibility);
    return () => {
      stop();
      document.removeEventListener('visibilitychange', on_visibility);
    };
  }, []);
  return <>
    <h5>Observed stages</h5>
    <div className="workflow_stages">{observed_stage_order(capability.observations).map((stage) => {
      const observation = [...capability.observations].reverse().find((item) => item.stage === stage);
      const label = observation_label(observation, now);
      return <article className="workflow_stage" key={stage} data-testid={`workflow_stage_${stage}`}>
        <div className="workflow_stage_heading"><strong>{stage}</strong><span className={`workflow_stage_status ${label.toLowerCase().replaceAll(' ', '_')}`}>{label}</span></div>
        <p>{observation?.summary ?? 'No observation recorded.'}</p>
        <small>{observation ? `${format_observed_at(observation.observed_at)} · ${observation.evidence_source}` : 'Evidence source unavailable'}</small>
      </article>;
    })}</div>
  </>;
}

export function Workflow_panel() {
  const [capabilities, set_capabilities] = useState<Workflow_capability[]>([]);
  const [selected_id, set_selected_id] = useState('');
  const [query, set_query] = useState('');
  const [kind_filter, set_kind_filter] = useState('all');
  const [group_filter, set_group_filter] = useState('all');
  const [busy, set_busy] = useState(false);
  const [route_busy, set_route_busy] = useState(false);
  const [error, set_error] = useState<string | null>(null);
  const [task, set_task] = useState('');
  const [route, set_route] = useState<Workflow_route_preview | null>(null);
  const [route_task, set_route_task] = useState<string | null>(null);
  const [external_advice_confirmed, set_external_advice_confirmed] = useState(false);
  const [health_status, set_health_status] = useState('Unknown');
  const [refreshed_at, set_refreshed_at] = useState<string | null>(null);
  const mounted = useRef(false);
  const request_sequence = useRef(0);
  const route_sequence = useRef(0);
  const selected_capability = capabilities.find((item) => item.id === selected_id) ?? null;

  async function refresh() {
    const sequence = ++request_sequence.current;
    set_busy(true); set_error(null);
    try {
      const [listing, health] = await Promise.all([workflow_list_capabilities(), workflow_get_health()]);
      if (!mounted.current || sequence !== request_sequence.current) return;
      const merged = merge_health_observations(listing.capabilities, health);
      set_capabilities(merged);
      set_selected_id((current) => merged.some((item) => item.id === current) ? current : merged[0]?.id ?? '');
      set_health_status(health.owner_status);
      set_refreshed_at(new Date().toISOString());
    } catch (cause) {
      if (mounted.current && sequence === request_sequence.current) set_error(workflow_error_message(cause));
    } finally {
      if (mounted.current && sequence === request_sequence.current) set_busy(false);
    }
  }

  useEffect(() => {
    mounted.current = true;
    void refresh();
    return () => { mounted.current = false; ++request_sequence.current; ++route_sequence.current; };
  }, []);

  async function probe_selected() {
    if (!selected_capability || selected_capability.probe_supported !== true || busy) return;
    const sequence = ++request_sequence.current;
    set_busy(true); set_error(null);
    try {
      const health = await workflow_probe_capability(selected_capability.id);
      if (!mounted.current || sequence !== request_sequence.current) return;
      set_capabilities((current) => merge_health_observations(current, health));
      set_health_status(health.owner_status);
      set_refreshed_at(new Date().toISOString());
    } catch (cause) {
      if (mounted.current && sequence === request_sequence.current) set_error(workflow_error_message(cause));
    } finally {
      if (mounted.current && sequence === request_sequence.current) set_busy(false);
    }
  }

  async function preview_route(with_advice: boolean) {
    const sequence = ++route_sequence.current;
    const submitted_task = task;
    set_route(null);
    set_route_task(null);
    set_route_busy(true); set_error(null);
    try {
      const result = with_advice
        ? await workflow_evaluate_route(submitted_task, external_advice_confirmed)
        : await workflow_route_preview(submitted_task);
      if (mounted.current && sequence === route_sequence.current) {
        set_route(result);
        set_route_task(submitted_task);
      }
    } catch (cause) {
      if (mounted.current && sequence === route_sequence.current) {
        set_route(null);
        set_route_task(null);
        set_error(workflow_error_message(cause));
      }
    } finally {
      if (mounted.current && sequence === route_sequence.current) set_route_busy(false);
    }
  }

  const kinds = useMemo(() => [...new Set(capabilities.map((item) => item.kind))].sort(), [capabilities]);
  const groups = useMemo(() => [...new Set(capabilities.map((item) => item.group))].sort(), [capabilities]);
  const visible_capabilities = useMemo(() => capabilities.filter((item) => {
    const matches_query = `${item.name} ${item.id} ${item.kind} ${item.group} ${item.owner} ${item.summary}`.toLowerCase().includes(query.trim().toLowerCase());
    return matches_query && (kind_filter === 'all' || item.kind === kind_filter) && (group_filter === 'all' || item.group === group_filter);
  }), [capabilities, query, kind_filter, group_filter]);

  return <section className="workflow_panel" aria-label="Workflow capabilities" data-testid="workflow_panel">
    <header className="workflow_panel_heading">
      <div><p className="mcp_eyebrow">WORKFLOW REGISTRY</p><h3>Capabilities and task routing</h3><p className="workflow_muted">Inspect ownership and observations, then preview a task route. A stage does not imply that later stages are ready.</p></div>
      <div className="workflow_actions"><button className="btn" disabled={busy} data-testid="workflow_refresh" onClick={() => { void refresh(); }}>Refresh</button></div>
    </header>
    {error && <div role="alert" className="workflow_error" data-testid="workflow_error">{error}</div>}
    {health_status === 'synthetic_demo_only' && <div className="workflow_demo_notice" role="note" data-testid="workflow_demo_notice">Browser demo records and results are synthetic. They do not describe host installation, authentication, connectivity, or a real probe.</div>}
    <div className="workflow_registry_meta"><span>Reported registry owner status: <strong>{health_status.replaceAll('_', ' ')}</strong></span><span>{refreshed_at ? `Last refreshed ${format_observed_at(refreshed_at)}` : 'Not refreshed'}</span></div>
    <div className="workflow_filters">
      <label>Search<input type="search" data-testid="workflow_search" value={query} onChange={(event) => set_query(event.target.value)} placeholder="Name, owner, or summary" /></label>
      <label>Kind<select data-testid="workflow_kind_filter" value={kind_filter} onChange={(event) => set_kind_filter(event.target.value)}><option value="all">All kinds</option>{kinds.map((kind) => <option key={kind} value={kind}>{kind}</option>)}</select></label>
      <label>Group<select data-testid="workflow_group_filter" value={group_filter} onChange={(event) => set_group_filter(event.target.value)}><option value="all">All groups</option>{groups.map((group) => <option key={group} value={group}>{group}</option>)}</select></label>
    </div>
    <div className="workflow_registry_layout" aria-busy={busy}>
      <div className="workflow_capability_list" role="listbox" aria-label="Capabilities" data-testid="workflow_capabilities">
        {visible_capabilities.length === 0 ? <p className="workflow_empty">{busy ? 'Loading capability metadata…' : 'No capabilities match these filters.'}</p> : visible_capabilities.map((capability) => <button type="button" role="option" aria-selected={selected_id === capability.id} className="workflow_capability_choice" data-testid={`workflow_capability_${capability.id}`} key={capability.id} onClick={() => set_selected_id(capability.id)}>
          <span className="workflow_capability_choice_heading"><strong>{capability.name}</strong><span className="workflow_tag">{capability.kind}</span></span>
          <span className="workflow_muted">{capability.group} · {capability.owner}</span>
        </button>)}
      </div>
      <div className="workflow_capability_detail" data-testid="workflow_capability_detail">
        {selected_capability ? <>
          <div className="workflow_detail_heading"><div><h4>{selected_capability.name}</h4><code>{selected_capability.id}</code></div>
            {selected_capability.probe_supported === true && <button className="btn" disabled={busy} data-testid="workflow_probe" onClick={() => { void probe_selected(); }}>Probe supported capability</button>}
          </div>
          <p>{selected_capability.summary}</p>
          <dl className="workflow_detail_facts">
            <div><dt>Owner</dt><dd>{selected_capability.owner}</dd></div><div><dt>Access method</dt><dd>{selected_capability.access_method}</dd></div>
            <div><dt>Host requirement</dt><dd>{selected_capability.host_scope}</dd></div><div><dt>Source</dt><dd>{selected_capability.source_ref}</dd></div>
            <div><dt>MCP server</dt><dd>{selected_capability.mcp_server_id ?? 'None'}</dd></div>
          </dl>
          <Observed_stages capability={selected_capability} />
        </> : <p className="workflow_empty">Select a capability to inspect its metadata and observations.</p>}
      </div>
    </div>
    <div className="workflow_route_preview">
      <div><p className="mcp_eyebrow">TASK ROUTING</p><h4>Preview a route</h4><p className="workflow_muted">The deterministic preview is read-only. Model advice is optional and cannot change permissions or dispatch work.</p></div>
      <label>Task description<textarea data-testid="workflow_task_input" value={task} onChange={(event) => {
        set_task(event.target.value);
        ++route_sequence.current;
        set_route(null);
        set_route_task(null);
        set_route_busy(false);
        set_error(null);
        set_external_advice_confirmed(false);
      }} rows={3} placeholder="Describe the task to route" /></label>
      <label className="workflow_external_advice_consent"><input type="checkbox" data-testid="workflow_external_advice_confirmed" checked={external_advice_confirmed} onChange={(event) => set_external_advice_confirmed(event.target.checked)} />
        I agree to send this task description to TypeSafe for optional external model advice.
      </label>
      <p className="workflow_muted">The task is sent only when this box is checked and the private Jev setting is enabled. AAG applies configured redaction, which may not remove every sensitive detail. TypeSafe uses separate provider usage. Native GPT-6 Luna advice remains unavailable until executable-tool restrictions are verified. Deterministic preview does not send the task externally.</p>
      <div className="workflow_actions"><button className="btn primary" disabled={route_busy} data-testid="workflow_route_preview" onClick={() => { void preview_route(false); }}>Preview route</button><button className="btn" disabled={route_busy} data-testid="workflow_route_advice" onClick={() => { void preview_route(true); }}>Evaluate with optional model advice</button></div>
      {route_busy && <p role="status" className="workflow_muted">Preparing preview…</p>}
      {route && route_task === task && <><Route_preview_card route={route} /><p className="workflow_muted" data-testid="workflow_route_submitted_task"><strong>Previewed task:</strong> {route_task}</p></>}
    </div>
    <p className="workflow_boundary">Capability metadata and observations are separate evidence. Missing, stale, or unknown stages stay visible; no combined ready state is inferred here.</p>
  </section>;
}
