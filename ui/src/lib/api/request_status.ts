import { invoke } from '@tauri-apps/api/core';

/** Trusted manager view only; concrete identifiers never enter diagnostic logs. */
export interface Request_status {
  requestId: string;
  surface: string;
  requestedModel: string;
  resolvedModel: string | null;
  phase: 'requesting' | 'streaming' | 'completed' | 'error' | 'cancelled';
  fallbackCount: number;
  errorCode: string | null;
  configGeneration: number | null;
  status: number | null;
}

export function proxy_request_status(): Promise<Request_status[]> {
  return invoke('proxy_request_status');
}

const surface_names: Record<string, string> = {
  codex: 'responses', claude: 'messages', chat: 'chat', gemini: 'gemini',
};

export function request_status_for_surface(rows: Request_status[], surface: string): Request_status[] {
  return rows.filter((row) => row.surface === surface_names[surface]);
}

export function visible_request_status(rows: Request_status[]): Request_status[] {
  const latest_finished = rows.find((row) => row.phase !== 'requesting' && row.phase !== 'streaming');
  return rows.filter((row) => row === latest_finished || row.phase === 'requesting' || row.phase === 'streaming');
}
