import type { Workflow_capability, Workflow_capability_list, Workflow_health, Workflow_observation, Workflow_route_preview } from '../api/workflow_registry.js';

const demo_timestamp = '2026-10-01T00:00:00.000Z';

const demo_capabilities: Workflow_capability[] = [
  {
    id: 'demo_local_docs', name: 'Local documentation search', kind: 'cli', group: 'Research', owner: 'AAG demo fixture',
    access_method: 'Fixed read-only CLI adapter', host_scope: 'Local host', source_ref: 'synthetic:browser_demo', mcp_server_id: null,
    probe_supported: true, summary: 'Synthetic example for browsing the capability registry.',
    observations: [
      { stage: 'installed', status: 'unknown', observed_at: null, expires_at: null, evidence_source: 'synthetic_fixture', summary: 'Demo data does not inspect this host.' },
      { stage: 'connected', status: 'unknown', observed_at: null, expires_at: null, evidence_source: 'synthetic_fixture', summary: 'No live connection was checked.' },
      { stage: 'discovered', status: 'synthetic', observed_at: demo_timestamp, expires_at: null, evidence_source: 'synthetic_fixture', summary: 'Shown to demonstrate the UI only.' },
    ],
  },
  {
    id: 'demo_calendar_bridge', name: 'Calendar bridge', kind: 'mcp', group: 'Planning', owner: 'Example provider',
    access_method: 'MCP server metadata', host_scope: 'External host', source_ref: 'synthetic:browser_demo', mcp_server_id: 'demo_calendar',
    probe_supported: false, summary: 'Synthetic metadata example; no account or server is connected.',
    observations: [
      { stage: 'installed', status: 'unknown', observed_at: null, expires_at: null, evidence_source: 'synthetic_fixture', summary: 'Installation state is not observed.' },
      { stage: 'connected', status: 'unknown', observed_at: null, expires_at: null, evidence_source: 'synthetic_fixture', summary: 'Authentication state is not observed.' },
      { stage: 'callable', status: 'unknown', observed_at: null, expires_at: null, evidence_source: 'synthetic_fixture', summary: 'Callability is not observed.' },
    ],
  },
  {
    id: 'demo_task_router', name: 'Task route preview', kind: 'workflow', group: 'Planning', owner: 'AAG demo fixture',
    access_method: 'Local metadata only', host_scope: 'This app', source_ref: 'synthetic:browser_demo', mcp_server_id: null,
    probe_supported: false, summary: 'Preview-only workflow routing example.',
    observations: [
      { stage: 'discovered', status: 'synthetic', observed_at: demo_timestamp, expires_at: null, evidence_source: 'synthetic_fixture', summary: 'Routing preview does not dispatch a worker.' },
    ],
  },
];

function build_demo_health(): Workflow_health {
  return {
    schema_version: 1,
    owner_status: 'synthetic_demo_only',
    observations: demo_capabilities.flatMap((capability) => capability.observations.map((observation) => ({ ...observation, capability_id: capability.id }))),
  };
}

function build_demo_route(task: string, advice: boolean): Workflow_route_preview {
  const has_task = task.trim().length > 0;
  return {
    schema_version: 1,
    difficulty: has_task ? 'moderate' : 'unknown',
    model: 'demo_route_preview', fallback_model: 'demo_fallback_preview', final_gate_model: 'demo_final_gate',
    reasoning_effort: 'medium', score: has_task ? 0.42 : 0,
    signals: has_task ? ['synthetic_task_text', 'no_dispatch'] : ['empty_task'],
    advice_status: advice ? 'synthetic_only' : 'not_requested',
    summary: 'Synthetic browser demo preview. This route is illustrative and does not start a task.',
    model_advice: advice ? {
      status: 'synthetic_only', source: 'browser_demo_fixture', model: 'demo_advice_model', confidence: null,
      summary: 'Illustrative advice only. It does not change the route or grant authority.',
    } : null,
  };
}

export async function mock_workflow_command(command: string, args: Record<string, unknown>): Promise<unknown> {
  if (command === 'workflow_list_capabilities') {
    const result: Workflow_capability_list = { schema_version: 1, capabilities: structuredClone(demo_capabilities) };
    return result;
  }
  if (command === 'workflow_get_health') return build_demo_health();
  if (command === 'workflow_probe_capability') {
    const capability_id = String(args.capability_id ?? '');
    const capability = demo_capabilities.find((item) => item.id === capability_id);
    if (!capability || capability.probe_supported !== true) throw new Error('workflow:probe_unsupported');
    const probe_observation: Workflow_observation = {
      stage: 'probe', status: 'synthetic', observed_at: demo_timestamp, expires_at: null,
      evidence_source: 'synthetic_fixture', summary: 'Demo probe result; no host command ran.', capability_id,
    };
    const installed_observation: Workflow_observation = {
      stage: 'installed', status: 'synthetic', observed_at: demo_timestamp, expires_at: null,
      evidence_source: 'synthetic_fixture', summary: 'Synthetic installed observation; no host executable was inspected.', capability_id,
    };
    const health_observations = build_demo_health().observations
      .filter((item) => !(item.capability_id === capability_id && item.stage === 'installed'));
    return { schema_version: 1, owner_status: 'synthetic_demo_only', observations: [...health_observations, installed_observation, probe_observation] } satisfies Workflow_health;
  }
  if (command === 'workflow_route_preview' || command === 'workflow_evaluate_route') {
    const task = typeof args.task === 'string' ? args.task : '';
    if (command === 'workflow_route_preview') return build_demo_route(task, false);
    if (args.external_advice_confirmed === true) {
      const advised_route = build_demo_route(task, true);
      return { ...advised_route, jev_evidence: advised_route.model_advice } satisfies Workflow_route_preview;
    }
    const deterministic_route = build_demo_route(task, false);
    return {
      ...deterministic_route,
      advice_status: 'tool_restriction_unverified',
      model_advice: {
        status: 'tool_restriction_unverified', source: 'native_codex_subscription', model: '', confidence: null,
        summary: 'Native advice is unavailable; external advice was not confirmed or requested.',
      },
    } satisfies Workflow_route_preview;
  }
  throw new Error('workflow:unknown_command');
}
