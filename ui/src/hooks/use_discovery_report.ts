import { useCallback, useEffect, useRef, useState } from 'react';
import { getDiscoveryStatus, type ModelDiscoveryStatus } from '../lib/api/proxy.js';

const polling_interval_ms = 60_000;

/// Model auto-discovery status: last stored report, pending hot-apply flag,
/// and schedule. Polls so background periodic refreshes surface without
/// user action; `refresh` re-fetches immediately after a manual run.
export function use_discovery_report(): {
  discovery_status: ModelDiscoveryStatus | null;
  refresh_discovery_status: () => Promise<void>;
} {
  const [discovery_status, set_discovery_status] = useState<ModelDiscoveryStatus | null>(null);
  const request_id_ref = useRef(0);

  const refresh_discovery_status = useCallback(async () => {
    const request_id = request_id_ref.current + 1;
    request_id_ref.current = request_id;
    try {
      const status = await getDiscoveryStatus();
      if (request_id !== request_id_ref.current) return;
      set_discovery_status(status);
    } catch {
      if (request_id !== request_id_ref.current) return;
      // Browser-demo / IPC-less contexts: keep the previous snapshot.
    }
  }, []);

  useEffect(() => {
    void refresh_discovery_status();
    const interval_id = window.setInterval(() => {
      void refresh_discovery_status();
    }, polling_interval_ms);
    return () => {
      window.clearInterval(interval_id);
      request_id_ref.current += 1;
    };
  }, [refresh_discovery_status]);

  return { discovery_status, refresh_discovery_status };
}
