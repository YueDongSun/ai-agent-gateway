function adapter_script() {
  return `(() => {
    if (window.__aagDemoIsDemo !== true) throw new Error('refusing smoke API adapter outside demo mode');
    const internals = window.__TAURI_INTERNALS__;
    if (!internals || typeof internals.invoke !== 'function') throw new Error('demo IPC bridge is unavailable');
    const invoke = (command, args) => internals.invoke(command, args);
    window.__aag_smoke_api = Object.freeze({
      getProxyConfig: () => invoke('proxy_config_get'),
      getProxyStatus: () => invoke('proxy_status'),
      listProxyProviders: () => invoke('proxy_provider_list'),
      listRoutes: () => invoke('route_list'),
      getPinnedProvider: () => invoke('proxy_get_pinned_provider'),
    });
    return true;
  })()`;
}

export async function install_demo_api_adapter(cdp) {
  const installed = await cdp.eval_value(adapter_script());
  if (installed !== true) throw new Error('demo smoke API adapter did not install');
}
