import type { ProxyConfig } from '../../../lib/api/proxy.js';

export interface Gateway_form {
  host: string;
  port: string;
  auth_token: string;
}

export interface Gateway_commands {
  start_proxy: () => Promise<string>;
  stop_proxy: () => Promise<void>;
  get_proxy_config: () => Promise<ProxyConfig>;
  save_proxy_config: (config: ProxyConfig) => Promise<void>;
  set_pinned_provider: (provider_id: string) => Promise<void>;
  set_responses_cot: (enabled: boolean) => Promise<void>;
}

export interface Gateway_action_options {
  commands: Gateway_commands;
  gateway_form: Gateway_form;
  pinned_id: string | null;
  refresh_page: () => Promise<void>;
  translate: (key: string, vars?: Record<string, string | number>) => string;
  flash: (message: string) => void;
  set_busy: (action: string | null) => void;
  set_error: (message: string | null) => void;
  set_gateway_form: (form: Gateway_form) => void;
  set_settings_open: (open: boolean) => void;
  set_pinned_id: (provider_id: string | null) => void;
  set_cot_on: (enabled: boolean) => void;
}

export interface Gateway_actions {
  start_gateway: () => Promise<void>;
  restart_gateway: () => Promise<void>;
  stop_gateway: () => Promise<void>;
  open_gateway_settings: () => Promise<void>;
  save_gateway_settings: () => Promise<void>;
  toggle_gateway_pin: (provider_id: string) => Promise<void>;
  toggle_gateway_cot: (enabled: boolean) => Promise<void>;
}

/** Gateway orchestration without React or a live IPC dependency. Callers create
 *  the actions from the current render's form, pin and page callbacks. */
export function create_gateway_actions(options: Gateway_action_options): Gateway_actions {
  const { commands, translate, flash, set_busy, set_error, refresh_page } = options;

  const start_gateway = async () => {
    set_busy('start');
    try {
      const message = await commands.start_proxy();
      const fallback = message.match(/port (\d+) was in use/);
      if (fallback) {
        flash(translate('app.gateway.portFallback', { port: fallback[1] }));
      }
      await refresh_page();
    } catch (error) {
      const raw = String(error);
      set_error(
        raw.includes('proxy_bind_exhausted') || raw.includes('no free port')
          ? translate('app.gateway.portConflict', { message: raw })
          : raw,
      );
    } finally {
      set_busy(null);
    }
  };

  const restart_gateway = async () => {
    set_busy('restart');
    try {
      try { await commands.stop_proxy(); } catch { /* wasn't running */ }
      await commands.start_proxy();
      await refresh_page();
      flash(translate('app.gateway.restarted'));
    } catch (error) {
      set_error(String(error));
    } finally {
      set_busy(null);
    }
  };

  const stop_gateway = async () => {
    set_busy('stop');
    try {
      await commands.stop_proxy();
      await refresh_page();
    } catch (error) {
      set_error(String(error));
    } finally {
      set_busy(null);
    }
  };

  const open_gateway_settings = async () => {
    options.set_settings_open(true);
    try {
      const config = await commands.get_proxy_config();
      options.set_gateway_form({
        host: config.host,
        port: String(config.port),
        auth_token: config.authToken,
      });
    } catch (error) {
      set_error(String(error));
    }
  };

  const save_gateway_settings = async () => {
    // Re-read the baseline so changing connection fields preserves unrelated
    // settings updated since the modal opened.
    let baseline: ProxyConfig;
    try {
      baseline = await commands.get_proxy_config();
    } catch (error) {
      set_error(String(error));
      return;
    }
    const port = Number(options.gateway_form.port);
    if (!Number.isInteger(port) || port < 1 || port > 65535) {
      set_error(translate('app.gateway.invalidPort'));
      return;
    }
    const host = options.gateway_form.host.trim();
    try {
      new URL(`http://${host}:${port}`);
    } catch {
      set_error(translate('app.gateway.invalidHost'));
      return;
    }
    try {
      await commands.save_proxy_config({
        ...baseline,
        host,
        port,
        authToken: options.gateway_form.auth_token.trim(),
      });
      options.set_settings_open(false);
      flash(translate('app.gateway.savedRestartHint'));
      await refresh_page();
    } catch (error) {
      set_error(String(error));
    }
  };

  const toggle_gateway_pin = async (provider_id: string) => {
    const next = options.pinned_id === provider_id ? '' : provider_id;
    try {
      await commands.set_pinned_provider(next);
      options.set_pinned_id(next || null);
      flash(next ? translate('providers.pinned', { name: provider_id }) : translate('providers.unpinned'));
    } catch (error) {
      set_error(String(error));
    }
  };

  const toggle_gateway_cot = async (enabled: boolean) => {
    options.set_cot_on(enabled);
    try {
      await commands.set_responses_cot(enabled);
    } catch (error) {
      options.set_cot_on(!enabled);
      set_error(String(error));
      return;
    }
    flash(translate(enabled ? 'app.gateway.cot.onFlash' : 'app.gateway.cot.offFlash'));
  };

  return {
    start_gateway,
    restart_gateway,
    stop_gateway,
    open_gateway_settings,
    save_gateway_settings,
    toggle_gateway_pin,
    toggle_gateway_cot,
  };
}
