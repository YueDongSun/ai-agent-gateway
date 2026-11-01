import { invoke } from '@tauri-apps/api/core';

export interface Diagnostics_state {
  debug_enabled: boolean;
  debug_expires_at: number | null;
  log_directory: string;
  max_file_size_bytes: number;
  retained_file_count: number;
}

export interface Request_event {
  requestId: string;
  surface: string;
  modelRef: string;
  timestampMs: number;
  status: number;
  durationMs: number;
  streamed: boolean;
  failedOver: boolean | null;
  /** Capability-routing decision (`precise` / `alias`), when the planner resolved the request. */
  planKind: string | null;
  planTier: number | null;
  planDowngraded: boolean | null;
  /** Stable snake_case codes only. */
  planBasis: string | null;
  planReason: string | null;
  /** Routing snapshot generation the request was admitted under. */
  configGeneration: number | null;
}

export async function diagnostics_get(): Promise<Diagnostics_state> {
  return invoke<Diagnostics_state>('diagnostics_get');
}

export async function diagnostics_enable_debug(): Promise<Diagnostics_state> {
  return invoke<Diagnostics_state>('diagnostics_enable_debug');
}

export async function diagnostics_disable_debug(): Promise<Diagnostics_state> {
  return invoke<Diagnostics_state>('diagnostics_disable_debug');
}

export async function diagnostics_open_log_dir(): Promise<void> {
  return invoke('diagnostics_open_log_dir');
}

export async function diagnostics_get_summary(): Promise<string> {
  return invoke<string>('diagnostics_get_summary');
}

export async function diagnostics_record_frontend_error(
  kind: string,
  message_chars: number,
  stack_chars: number,
  correlation_id: string,
): Promise<void> {
  return invoke('diagnostics_record_frontend_error', {
    kind,
    messageChars: message_chars,
    stackChars: stack_chars,
    correlationId: correlation_id,
  });
}

export async function diagnostics_request_timeline(): Promise<Request_event[]> {
  return invoke<Request_event[]>('diagnostics_request_timeline');
}

export async function diagnostics_clear_request_timeline(): Promise<void> {
  return invoke('diagnostics_clear_request_timeline');
}
