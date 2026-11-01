import { invoke } from '@tauri-apps/api/core';

export interface Aag_instance {
  deviceName: string;
  address: string;
  port: number;
  version: string;
  latencyMs: number;
  isSelf: boolean;
}

export interface Discovery_counts {
  online: number;
  authRequired: number;
  offline: number;
  unverified: number;
  failed: number;
  skipped: number;
}

export interface Discovery_report {
  requestId: string;
  port: number;
  checkedAt: number;
  elapsedMs: number;
  peerCount: number;
  checkedCount: number;
  limited: boolean;
  counts: Discovery_counts;
  instances: Aag_instance[];
  synthetic?: boolean;
}

export interface Discovery_status {
  port: number;
  phase: 'idle' | 'scanning' | 'cancelling' | 'complete' | 'error' | 'cancelled';
  error: string | null;
  lastReport: Discovery_report | null;
  synthetic?: boolean;
}

export const get_tailnet_discovery_status = () => invoke<Discovery_status>('tailnet_discovery_status');
export const scan_tailnet_instances = (request_id: string) => invoke<Discovery_report>('tailnet_discovery_scan', { requestId: request_id });
export const cancel_tailnet_discovery = (request_id: string) => invoke<boolean>('tailnet_discovery_cancel', { requestId: request_id });

const known_errors = new Set([
  'tailnet_discovery_platform_unsupported', 'tailnet_discovery_process_unavailable',
  'tailnet_discovery_process_failed', 'tailnet_discovery_status_timeout',
  'tailnet_discovery_status_too_large', 'tailnet_discovery_status_invalid',
  'tailnet_discovery_peer_limit', 'tailnet_discovery_stopped', 'tailnet_discovery_busy',
  'tailnet_discovery_cancelled', 'tailnet_discovery_timed_out',
  'tailnet_discovery_port_invalid', 'tailnet_discovery_config_unavailable',
]);

export function tailnet_discovery_error(error: unknown): string {
  const code = String(error ?? '');
  return known_errors.has(code) ? code : 'tailnet_discovery_failed';
}

/** An observed instance is an IP literal, never a caller supplied URL. */
export function instance_endpoint(instance: Aag_instance): string {
  return `http://${instance.address.includes(':') ? `[${instance.address}]` : instance.address}:${instance.port}`;
}
