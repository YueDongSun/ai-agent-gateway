import type { Discovery_report, Discovery_status } from '../api/tailnet_discovery.js';

let last_report: Discovery_report | null = null;
let active_request: string | null = null;
let cancelled = false;

/** Synthetic metadata only: this demo never invokes Tailscale or makes a probe. */
export async function mock_tailnet_discovery_command(command: string, args: Record<string, unknown>, configured_port = 8989): Promise<unknown> {
  if (command === 'tailnet_discovery_status') return {
    port: configured_port, phase: active_request ? 'scanning' : last_report ? 'complete' : 'idle', error: null,
    lastReport: last_report ? structuredClone(last_report) : null, synthetic: true,
  } satisfies Discovery_status;
  if (command === 'tailnet_discovery_cancel') {
    if (String(args.requestId) !== active_request) return false;
    cancelled = true;
    return true;
  }
  if (command === 'tailnet_discovery_scan') {
    const request_id = String(args.requestId ?? '');
    if (!/^[A-Za-z0-9_-]{1,80}$/.test(request_id)) throw new Error('tailnet_discovery_request_invalid');
    if (active_request) throw new Error('tailnet_discovery_busy');
    active_request = request_id;
    cancelled = false;
    try {
      await new Promise((resolve) => setTimeout(resolve, 120));
      if (cancelled) throw new Error('tailnet_discovery_cancelled');
      // Documentation-only fixture addresses (TEST-NET-1), unrelated to real peers.
      last_report = {
        requestId: request_id, port: configured_port, checkedAt: Date.now(), elapsedMs: 120,
        peerCount: 5, checkedCount: 4, limited: false,
        counts: { online: 2, authRequired: 1, offline: 1, unverified: 0, failed: 0, skipped: 1 },
        instances: [
          { deviceName: 'This device (demo)', address: '192.0.2.1', port: configured_port, version: '0.2.0', latencyMs: 8, isSelf: true },
          { deviceName: 'Team workstation (demo)', address: '192.0.2.2', port: configured_port, version: '0.2.0', latencyMs: 24, isSelf: false },
        ], synthetic: true,
      };
      return structuredClone(last_report);
    } finally { active_request = null; }
  }
  throw new Error('demo_command_unsupported');
}
