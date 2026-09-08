import { useCallback, useEffect, useRef, useState } from 'react';
import { fetchProxyHealth, getProxyStatus } from '../lib/api/proxy.js';

const health_timeout_ms = 2500;
const polling_interval_ms = 5000;

export type Gateway_status = 'stopped' | 'running' | 'degraded' | 'unknown';

export function use_gateway_status(): {
  gateway_status: Gateway_status;
  refresh_gateway_status: () => Promise<void>;
} {
  const [gateway_status, set_gateway_status] = useState<Gateway_status>('unknown');
  const request_id_ref = useRef(0);
  const health_controller_ref = useRef<AbortController | null>(null);

  const refresh_gateway_status = useCallback(async () => {
    const request_id = request_id_ref.current + 1;
    request_id_ref.current = request_id;
    health_controller_ref.current?.abort();
    health_controller_ref.current = null;

    try {
      const status = await getProxyStatus();
      if (request_id !== request_id_ref.current) return;
      if (typeof status.running !== 'boolean') throw new Error('Invalid gateway status response');
      if (!status.running) {
        set_gateway_status('stopped');
        return;
      }

      const controller = new AbortController();
      health_controller_ref.current = controller;
      const timeout_id = window.setTimeout(() => controller.abort(), health_timeout_ms);
      try {
        const health = await fetchProxyHealth(status.config, controller.signal);
        if (request_id !== request_id_ref.current) return;
        const has_open_breaker = health.providers.some((provider) => provider.breaker?.state === 'open');
        set_gateway_status(has_open_breaker ? 'degraded' : 'running');
      } catch {
        if (request_id === request_id_ref.current) set_gateway_status('degraded');
      } finally {
        window.clearTimeout(timeout_id);
        if (health_controller_ref.current === controller) health_controller_ref.current = null;
      }
    } catch {
      if (request_id === request_id_ref.current) set_gateway_status('unknown');
    }
  }, []);

  useEffect(() => {
    void refresh_gateway_status();
    const interval_id = window.setInterval(() => {
      void refresh_gateway_status();
    }, polling_interval_ms);
    return () => {
      window.clearInterval(interval_id);
      request_id_ref.current += 1;
      health_controller_ref.current?.abort();
    };
  }, [refresh_gateway_status]);

  return { gateway_status, refresh_gateway_status };
}
