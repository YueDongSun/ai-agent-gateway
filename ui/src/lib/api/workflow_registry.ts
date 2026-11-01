import { invoke } from '@tauri-apps/api/core';

export type Workflow_stage = 'installed' | 'connected' | 'discovered' | 'callable' | 'probe' | string;

export interface Workflow_observation {
  stage: Workflow_stage;
  status: string;
  observed_at: string | null;
  expires_at: string | null;
  evidence_source: string;
  summary: string;
  capability_id?: string;
}

export interface Workflow_capability {
  id: string;
  name: string;
  kind: string;
  group: string;
  owner: string;
  access_method: string;
  host_scope: string;
  source_ref: string;
  mcp_server_id: string | null;
  probe_supported?: boolean;
  summary: string;
  observations: Workflow_observation[];
}

export interface Workflow_capability_list {
  schema_version: 1;
  capabilities: Workflow_capability[];
  next_cursor?: string;
}

export interface Workflow_health {
  schema_version: 1;
  owner_status: string;
  observations: Workflow_observation[];
  next_cursor?: string;
}

export interface Workflow_route_preview {
  schema_version: 1;
  difficulty: string;
  model: string;
  fallback_model: string | null;
  final_gate_model: string | null;
  reasoning_effort: string;
  score: number;
  signals: string[];
  advice_status: string;
  summary: string;
  model_advice?: Workflow_model_evidence | null;
  jev_evidence?: Workflow_model_evidence | null;
  task_advice?: Workflow_task_advice | null;
}

export interface Workflow_task_advice {
  status: 'observation_only' | 'below_confidence_floor' | 'disabled' | 'busy' | 'unavailable' | 'recovery_required';
  source: 'strands_decider_local';
  checkpoint: string;
  workflow_class: 'artifact_change' | 'artifact_assessment' | 'other_or_unclear' | null;
  confidence: number | null;
  probabilities: Record<string, number>;
  latency_ms: number | null;
  observed_at: string;
  reason_code: string;
  summary: string;
}

export interface Workflow_model_evidence {
  status: string;
  source: string;
  model: string;
  confidence: number | null;
  summary: string;
}

export function workflow_list_capabilities(): Promise<Workflow_capability_list> {
  return invoke('workflow_list_capabilities');
}

export function workflow_get_health(): Promise<Workflow_health> {
  return invoke('workflow_get_health');
}

export function workflow_route_preview(task: string): Promise<Workflow_route_preview> {
  return invoke('workflow_route_preview', { task });
}

export function workflow_probe_capability(capability_id: string): Promise<Workflow_health> {
  return invoke('workflow_probe_capability', { capability_id });
}

export function workflow_evaluate_route(task: string, external_advice_confirmed = false): Promise<Workflow_route_preview> {
  return invoke('workflow_evaluate_route', { task, external_advice_confirmed });
}

/** Workflow errors expose only stable codes; arbitrary backend text stays private. */
export function workflow_error_message(error: unknown): string {
  const code = String(error).match(/workflow:[a-z_]+/)?.[0];
  return code
    ? `Workflow request failed (${code}). Refresh the capability view before retrying.`
    : 'Workflow request failed. No success was confirmed. Refresh and try again.';
}
