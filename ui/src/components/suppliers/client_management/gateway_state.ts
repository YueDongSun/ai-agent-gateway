import { useEffect, useRef, useState, type Dispatch, type SetStateAction } from 'react';
import {
  fetchProxyHealth,
  getProxyConfig,
  saveProxyConfig,
  setPinnedProvider,
  setResponsesCot,
  startProxy,
  stopProxy,
  proxy_client_url,
  type ProxyConfig,
  type ProxyHealth,
  type ProxyStatus,
} from '../../../lib/api/proxy.js';
import type { AppType } from '../../../lib/api/types.js';
import {
  create_gateway_actions,
  type Gateway_action_options,
  type Gateway_actions,
  type Gateway_commands,
  type Gateway_form,
} from './gateway_flow.js';

const gateway_health_interval_ms = 10_000;
const gateway_commands: Gateway_commands = {
  start_proxy: startProxy,
  stop_proxy: stopProxy,
  get_proxy_config: getProxyConfig,
  save_proxy_config: saveProxyConfig,
  set_pinned_provider: setPinnedProvider,
  set_responses_cot: setResponsesCot,
};

/** Poll-suppression digest for the health tick. `getGatewayHealth()` always
 *  deserializes a fresh object, so an identity check never bails; equal JSON
 *  means the rendered breaker/pin state cannot change, and the tick keeps the
 *  previous snapshot identity instead of re-rendering the routing page. */
function same_health_payload(prev: ProxyHealth | null, next: ProxyHealth): boolean {
  return JSON.stringify(prev) === JSON.stringify(next);
}

interface Gateway_state_options extends Pick<
  Gateway_action_options,
  'refresh_page' | 'translate' | 'flash' | 'set_busy' | 'set_error'
> {
  app_type: AppType;
}

interface Gateway_state extends Gateway_actions {
  running: boolean;
  gateway_url: string;
  auth_token: string;
  health: ProxyHealth | null;
  pinned_id: string | null;
  cot_on: boolean;
  /** Stable code from the last failed hot reload, while the previous
   *  routing snapshot keeps serving. Null when reloads are healthy. */
  hot_reload_error: string | null;
  settings_open: boolean;
  gateway_form: Gateway_form;
  set_gateway_form: Dispatch<SetStateAction<Gateway_form>>;
  set_settings_open: Dispatch<SetStateAction<boolean>>;
  apply_gateway_snapshot: (config: ProxyConfig, status: ProxyStatus) => void;
  apply_gateway_controls: (provider_id: string | null, cot_enabled: boolean) => void;
  refresh_gateway_health: (config: ProxyConfig) => Promise<void>;
}

/** Gateway-only view state. AppPage retains provider/route refresh ordering and
 *  shared feedback; its existing consumers still receive props/context values. */
export function use_gateway_state(options: Gateway_state_options): Gateway_state {
  const [running, set_running] = useState(false);
  const [gateway_url, set_gateway_url] = useState('');
  const [auth_token, set_auth_token] = useState('');
  const [health, set_health] = useState<ProxyHealth | null>(null);
  const [pinned_id, set_pinned_id] = useState<string | null>(null);
  const [cot_on, set_cot_on] = useState(false);
  const [hot_reload_error, set_hot_reload_error] = useState<string | null>(null);
  const [settings_open, set_settings_open] = useState(false);
  const [gateway_form, set_gateway_form] = useState<Gateway_form>({ host: '', port: '', auth_token: '' });
  /** The last snapshot applied to the display — the health poll dials this
   *  config, never a fresh `proxy_config_get`: while the proxy runs, the
   *  saved config may already point at a listener the restart has not
   *  adopted yet. */
  const config_ref = useRef<ProxyConfig | null>(null);

  const apply_gateway_snapshot = (config: ProxyConfig, status: ProxyStatus) => {
    // The displayed/copied URL and the renderer's own health fetch both use
    // the dialable endpoint, so a wildcard or alias bind still shows and
    // dials the loopback the clients are actually pointed at.
    config_ref.current = config;
    set_gateway_url(proxy_client_url(config));
    set_auth_token(config.authToken);
    set_running(status.running);
    set_hot_reload_error(status.hot_reload?.lastError ?? null);
    // Breaker dots describe a live listener; a stopped proxy has none.
    if (!status.running) set_health(null);
  };

  const apply_gateway_controls = (provider_id: string | null, cot_enabled: boolean) => {
    set_pinned_id(provider_id);
    set_cot_on(cot_enabled);
  };

  const refresh_gateway_health = async (config: ProxyConfig) => {
    try { set_health(await fetchProxyHealth(config)); } catch { set_health(null); }
  };

  // Health polling is independent of the shell's status indicator. Preserve
  // the routing-page cadence and pause it while the document is hidden.
  const app_type = options.app_type;
  useEffect(() => {
    const polls_health =
      (app_type === 'claude_code' || app_type === 'codex' || app_type === 'gemini') && running;
    if (!polls_health) return;
    let alive = true;
    const tick = async () => {
      const config = config_ref.current;
      if (!config) return;
      try {
        const next_health = await fetchProxyHealth(config);
        if (alive) set_health((prev) => (same_health_payload(prev, next_health) ? prev : next_health));
      } catch {
        if (alive) set_health(null);
      }
    };
    let interval_id: number | null = null;
    const start = () => {
      if (interval_id !== null) return;
      interval_id = window.setInterval(tick, gateway_health_interval_ms);
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
      alive = false;
      stop();
      document.removeEventListener('visibilitychange', on_visibility);
    };
  }, [app_type, running]);

  // Rebuild from current values and callbacks; never freeze page refresh,
  // translations, form fields, or pin state in an empty-dependency memo.
  const actions = create_gateway_actions({
    ...options,
    commands: gateway_commands,
    gateway_form,
    pinned_id,
    set_gateway_form,
    set_settings_open,
    set_pinned_id,
    set_cot_on,
  });

  return {
    running,
    gateway_url,
    auth_token,
    health,
    pinned_id,
    cot_on,
    hot_reload_error,
    settings_open,
    gateway_form,
    set_gateway_form,
    set_settings_open,
    apply_gateway_snapshot,
    apply_gateway_controls,
    refresh_gateway_health,
    ...actions,
  };
}
