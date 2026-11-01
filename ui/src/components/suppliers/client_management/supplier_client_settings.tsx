import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { use_page_actions } from '../../page_actions.js';
import {
  listProviders,
  getCurrentProvider,
  saveProvider,
  deleteProvider,
  switchProvider,
  reorderProviders,
  setProviderFailoverQueue,
} from '../../../lib/api/providers';
import {
  getProxyStatus,
  startProxy,
  stopProxy,
  listProxyProviders,
  saveProxyProvider,
  deleteProxyProvider,
  getPinnedProvider,
  getResponsesCot,
  testProvider,
  listRoutes,
  saveRoute,
  deleteRoute,
  renameRoute,
  fetchProviderModels,
  refreshProviderModels,
  setProviderAutoTrack,
  listSupplierMappedProviders,
  type ProxyProvider,
  type CircuitBreakerConfig,
  type TestResult,
  type Route,
} from '../../../lib/api/proxy';
import { importCcSwitchProviders } from '../../../lib/api/import';
import { use_gateway_state } from './gateway_state.js';
import { run_cc_switch_import } from './import_flow.js';
import {
  codexCliStatus,
  codexMirrorSetMode,
  codexOpenCliTerminal,
  codexOpenDesktopWindow,
  type CodexCliStatus,
} from '../../../lib/api/codexCli';
import {
  claudeMirrorStatus,
  claudeOpenDesktop,
  open_claude_cli_terminal,
  type ClaudeMirrorStatus,
} from '../../../lib/api/claudeMirror';
import { copilotOpenCliTerminal } from '../../../lib/api/copilot';
import { managed_cli_open_terminal, managed_cli_status, type Managed_cli_status } from '../../../lib/api/managed_cli';
import { geminiOpenCliTerminal } from '../../../lib/api/gemini';
import { vscodeOpenWithExtension } from '../../../lib/api/vscode';
import { APP_PAGES, CLAUDE_MODELS, CODEX_MODELS, type AppPageConfig } from '../../../lib/appPages';
import type { AppType, Provider } from '../../../lib/api/types';
import {
  defaultUpstreamTypeFor,
  mergeProviderCards,
  slugifyProviderId,
  upstreamIdOf,
  type UnifiedCard,
} from '../../../lib/unified_providers.js';
import { useT } from '../../../lib/ctx';
import { backend_error_text } from '../../../lib/backend_error.js';
import { Launchers } from './Launchers';
import { Client_config_status } from './client_config_status';
import { Live_request_status } from '../../app/request_status.js';
import { GatewayPanel } from './GatewayPanel';
import {
  ProvidersActionsProvider,
  ProvidersDataProvider,
  type ProvidersActionsState,
  type ProvidersDataState,
} from './providers_state.js';
import { ProvidersPanel } from './providers_panel.js';
import { ProviderModal, type ProviderFormState } from './provider_modal.js';
import { AdvancedRoutes } from './advanced_routes.js';
import { Route_explanation } from './route_explanation.js';
import {
  RoutesActionsProvider,
  RoutesDataProvider,
  RoutesDragProvider,
  type RoutesActionsState,
  type RoutesDataState,
  type RoutesDragState,
} from './routes_state.js';
import { use_discovery_report } from '../../../hooks/use_discovery_report.js';
import { Supplier_discovery_report } from '../supplier_discovery_details.js';
import { use_navigation_block } from '../../../hooks/use_navigation_block.js';
import { persist_route_changes, read_route_rename, read_route_writeback, type Route_rename_readback, type Route_write_readback } from './route_persistence.js';
import {
  PROVIDER_PRESET_TEMPLATES,
  identity_model_mapping,
  type ProviderPresetTemplate,
} from '../../../lib/provider_presets.js';

/** Upstream defaults for newly created linked rows (matches the seeded
 *  pool's breaker shape; edits preserve the stored values verbatim). */
const DEFAULT_CIRCUIT_BREAKER: CircuitBreakerConfig = {
  failureThreshold: 3,
  cooldownMs: 5000,
  halfOpenRetries: 1,
  successThreshold: 2,
};

/** Parse the models textarea (comma/newline separated) into a list. */
function parseModelList(raw: string): string[] {
  return raw
    .split(/[,\n]/)
    .map((m) => m.trim())
    .filter(Boolean);
}

/** Render a modelMapping object as editable JSON text (empty string when
 *  the mapping has no entries, so the textarea starts clean). */
function formatModelMapping(mapping: Record<string, string> | undefined): string {
  return mapping && Object.keys(mapping).length > 0
    ? JSON.stringify(mapping, null, 2)
    : '';
}

/** Render an extraHeaders object as editable JSON text (same contract as
 *  `formatModelMapping`: empty string when there are no headers). */
function formatHeaderJson(headers: Record<string, string> | undefined): string {
  return headers && Object.keys(headers).length > 0
    ? JSON.stringify(headers, null, 2)
    : '';
}

function emptyProviderForm(
  cfg: AppPageConfig,
  gatewayUrl: string,
  appType: AppType,
): ProviderFormState {
  const values: Record<string, string> = {};
  for (const f of cfg.fields) {
    if (f.key === 'baseUrl') values.baseUrl = gatewayUrl;
    else if (f.type === 'select' && f.options?.length) values[f.key] = f.options[0];
    else values[f.key] = '';
  }
  return {
    id: '',
    name: '',
    values,
    icon: '',
    iconColor: '',
    websiteUrl: '',
    notes: '',
    upstreamType: defaultUpstreamTypeFor(appType),
    upstreamBaseURL: '',
    upstreamAPIKey: '',
    upstreamModels: '',
    upstreamModelMapping: '',
    upstreamExtraHeaders: '',
    upstreamAutoTrack: true,
  };
}

/** Per-route signature cache, keyed by Route object identity. Every route
 *  edit path replaces only the touched route object (immutable map/spread),
 *  so unchanged routes and baseline entries hit the cache and a dirty check
 *  stops re-stringifying the whole table on each keystroke. WeakMap drops
 *  signatures with the discarded route objects. */
const route_signature_cache = new WeakMap<Route, string>();
function route_signature(route: Route): string {
  let signature = route_signature_cache.get(route);
  if (signature === undefined) {
    signature = JSON.stringify(route);
    route_signature_cache.set(route, signature);
  }
  return signature;
}

/** Table-level equality as per-alias signature sequences — same verdict as
 *  comparing the two stringified arrays, without serializing them wholesale. */
function routes_equal(left: Route[], right: Route[]): boolean {
  if (left.length !== right.length) return false;
  return left.every((route, i) => route_signature(route) === route_signature(right[i]));
}

/** Poll-suppression digest for the small flat status payloads. `invoke()`
 *  always deserializes a fresh object, so an identity check never bails and
 *  an unchanged 10 s tick would otherwise re-render the whole page. */
function same_status_payload(left: unknown, right: unknown): boolean {
  return JSON.stringify(left) === JSON.stringify(right);
}

/** Advanced-routes open state per app. Module-scoped so tab switches
 *  (which remount the page) don't re-collapse a section the user opened;
 *  resets on app restart, same as the route edit cache. */
const advancedRoutesOpenByApp = new Map<string, boolean>();

/**
 * Unified per-app provider-chain page (cc-switch style). Every app gets the
 * same list: drag-reorderable provider cards, CURRENT badge on the selected
 * row, per-card failover-chain toggle, Enable/In-Use switch button. App
 * slots add the cc-switch endpoint card (Claude), the gateway card, and the
 * Codex launchers.
 */
interface Supplier_client_settings_props {
  appType: AppType;
  on_gateway_status_refresh?: () => Promise<void>;
}

export function Supplier_client_settings({ appType, on_gateway_status_refresh }: Supplier_client_settings_props) {
  const t = useT();
  const cfg = APP_PAGES[appType];
  const [providers, setProviders] = useState<Provider[]>([]);
  const [current, setCurrent] = useState<string | null>(null);
  const [cliStatus, setCliStatus] = useState<CodexCliStatus | null>(null);
  /** Last error from `codexCliStatus` (null = never failed or
   *  recovered). The mirror row renders a warning chip when set. */
  const [cliStatusError, setCliStatusError] = useState<string | null>(null);
  const [claudeStatus, setClaudeStatus] = useState<ClaudeMirrorStatus | null>(null);
  /** Last error from `claudeMirrorStatus` — mirrors cliStatusError. */
  const [claudeStatusError, setClaudeStatusError] = useState<string | null>(null);
  const [managed_cli, set_managed_cli] = useState<Managed_cli_status | null>(null);
  const [managed_cli_error, set_managed_cli_error] = useState<{ appType: AppType; message: string } | null>(null);
  const managed_cli_request_id_ref = useRef(0);
  const codex_mirror_request_id_ref = useRef(0);
  const codex_mirror_mutation_ref = useRef(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [config_refresh_version, set_config_refresh_version] = useState(0);
  const refresh_client_config = () => set_config_refresh_version((version) => version + 1);
  /** Unified provider modal (preset identity + connection + upstream). */
  const [showProviderModal, setShowProviderModal] = useState(false);
  const [providerForm, setProviderForm] = useState<ProviderFormState>(() =>
    emptyProviderForm(cfg, '', appType),
  );
  /** Card being edited in the unified modal (null = create). */
  const [editingCard, setEditingCard] = useState<UnifiedCard | null>(null);
  const [copied, setCopied] = useState<string | null>(null);
  const [importing, setImporting] = useState(false);
  /** Proxy providers (upstream pool, global) — linked `ccs-*` rows pair
   *  with presets on the unified cards; standalones render on routing
   *  pages and feed the hop-provider dropdowns. */
  const [proxyProviders, setProxyProviders] = useState<ProxyProvider[]>([]);
  /** Test results per provider id. */
  const [testResults, setTestResults] = useState<Record<string, TestResult>>({});
  /** Which provider is currently being tested. */
  const [testingId, setTestingId] = useState<string | null>(null);
  /** Whether the running proxy's routing snapshot is stale. */
  const [config_changed, set_config_changed] = useState(false);
  /** Unified provider modal busy + inline error. The save path writes the
   *  preset and its linked upstream in sequence, so Save stays disabled and
   *  failures render inside the modal instead of a banner behind it. */
  const [provider_saving, set_provider_saving] = useState(false);
  const [provider_modal_error, set_provider_modal_error] = useState<string | null>(null);
  const provider_baseline_ref = useRef<ProviderFormState | null>(null);
  /** Failover routes (alias → ordered hops). Used on Claude + Codex pages. */
  const [routes, setRoutes] = useState<Route[]>([]);
  /** Baseline routes from last save/load — used to detect unsaved changes. */
  const baselineRef = useRef<Route[]>([]);
  /** One route operation owns every mutating handler until all writes settle. */
  const [routes_saving, set_routes_saving] = useState(false);
  const route_operation_running = useRef(false);
  const pending_route_readback = useRef<Route_write_readback | null>(null);
  const pending_rename_readback = useRef<Route_rename_readback | null>(null);
  const [route_outcome_unknown, set_route_outcome_unknown] = useState(false);
  const routes_ref = useRef(routes);
  routes_ref.current = routes;
  /** Monotonic refresh token: a response from a superseded refresh (tab
   *  switch, manual refresh) must not overwrite the current app's state. */
  const refresh_seq_ref = useRef(0);
  /** Upstream ids owned by supplier endpoint projections. Supplier saves
   *  rewrite those rows, so the card list hides direct edits for them. */
  const [supplier_managed_ids, set_supplier_managed_ids] = useState<Set<string>>(
    () => new Set(),
  );
  /** Whether routes have unsaved edits. Per-route signatures (identity-keyed
   *  cache above) keep this O(touched routes) per keystroke instead of two
   *  whole-table stringifies. */
  const isRouteDirty = useMemo(
    () => !routes_equal(routes, baselineRef.current),
    [routes],
  );
  use_navigation_block(routes_saving || isRouteDirty || route_outcome_unknown, () => setError(t('route.navigationBlocked')));
  /** Advanced-routes section expanded (per app; see module map above). */
  const [advancedRoutesOpen, setAdvancedRoutesOpen] = useState(
    () => advancedRoutesOpenByApp.get(appType) ?? false,
  );
  /** Route editor: aliases selected for batch delete. */
  const [selectedAliases, setSelectedAliases] = useState<Set<string>>(new Set());
  /** Route editor: aliases whose hop list is collapsed. */
  const [collapsedRoutes, setCollapsedRoutes] = useState<Set<string>>(new Set());
  /** Model auto-discovery: last stored report + pending hot-apply flag. */
  const { discovery_status, refresh_discovery_status } = use_discovery_report();
  /** Cached upstream model ids fetched via ⤓ per provider id. */
  const [fetchedModels, setFetchedModels] = useState<Record<string, string[]>>({});
  /** Which provider is currently being fetched. */
  const [fetchingId, setFetchingId] = useState<string | null>(null);

  // ---- Latest-ref pattern for stable action context values ----
  // Mutable state and callbacks that action handlers close over are written
  // into these refs before the JSX return. The `useMemo` action objects below
  // have empty deps (stable identity across renders) and every method reads
  // `ref.current` at call time, so consumers always see current state and a
  // pointermove no longer re-renders the whole context tree.
  const routes_actions_ref = useRef<RoutesActionsState | null>(null);
  const providers_actions_ref = useRef<ProvidersActionsState | null>(null);

  /** Ref the timer so back-to-back flashes cancel the previous toast. */
  const flashTimerRef = useRef<number | null>(null);
  const flash = (msg: string) => {
    setNotice(msg);
    if (flashTimerRef.current !== null) window.clearTimeout(flashTimerRef.current);
    flashTimerRef.current = window.setTimeout(() => {
      setNotice(null);
      flashTimerRef.current = null;
    }, 4000);
  };

  const {
    running,
    gateway_url: gatewayUrl,
    auth_token: authToken,
    health,
    pinned_id: pinnedId,
    cot_on: cotOn,
    hot_reload_error: hotReloadError,
    settings_open: showGatewayModal,
    gateway_form: gatewayForm,
    set_gateway_form: setGatewayForm,
    set_settings_open: setShowGatewayModal,
    apply_gateway_snapshot,
    apply_gateway_controls,
    refresh_gateway_health,
    start_gateway: handleStart,
    restart_gateway: handleRestartProxy,
    stop_gateway: handleStop,
    open_gateway_settings: openGatewayModal,
    save_gateway_settings: handleSaveGateway,
    toggle_gateway_pin: handleTogglePin,
    toggle_gateway_cot: handleToggleCot,
  } = use_gateway_state({
    app_type: appType,
    refresh_page: () => refresh(),
    translate: t,
    flash,
    set_busy: setBusy,
    set_error: setError,
  });

  /** App types whose Client_config_status card already performed its own
   *  initial fetch: bumping the refresh version again would tear down that
   *  first request and re-fetch it, doubling the IPC on every mount. */
  const config_refreshed_apps_ref = useRef(new Set<AppType>());

  /** Reload this page from the backend. Every write is gated on the refresh
   *  token, so a response that lands after a tab switch (or a newer manual
   *  refresh) cannot overwrite the active app's provider list or routes. */
  const refresh = async () => {
    const seq = ++refresh_seq_ref.current;
    if (config_refreshed_apps_ref.current.has(appType)) {
      refresh_client_config();
    } else {
      config_refreshed_apps_ref.current.add(appType);
    }
    try {
      // `proxy_status.config` is the running listener config while running
      // and the saved config while stopped — the single truth for the
      // displayed URL/key, so no separate `proxy_config_get` is needed.
      const [list, cur, st] = await Promise.all([
        listProviders(appType),
        getCurrentProvider(appType),
        getProxyStatus(),
      ]);
      if (seq !== refresh_seq_ref.current) return;
      setProviders(list);
      setCurrent(cur);
      apply_gateway_snapshot(st.config, st);
      set_config_changed(st.config_changed ?? false);
      // The upstream pool is global — load it on every page so linked
      // `ccs-*` rows render on the unified cards wherever their preset
      // lives. Supplier-managed ids come from the same optional group.
      try {
        const [pp, pin, cot, mapped] = await Promise.all([
          listProxyProviders(),
          getPinnedProvider(),
          getResponsesCot(),
          listSupplierMappedProviders(),
        ]);
        if (seq !== refresh_seq_ref.current) return;
        setProxyProviders(pp);
        set_supplier_managed_ids(new Set(mapped));
        apply_gateway_controls(pin, cot);
      } catch { /* optional */ }
      // Health (breaker states) + routes are routing-page concerns:
      // Claude, Codex, and Gemini.
      if (appType === 'claude_code' || appType === 'codex' || appType === 'gemini') {
        if (st.running) {
          await refresh_gateway_health(st.config);
        }
        // Routes are global to every routing page (one `routes` table), so
        // there is no per-app edit cache: switching tabs reloads the table.
        // A failed read keeps the last known list instead of blanking the
        // editor and baselining deletions against an empty set.
        try {
          const loaded = await listRoutes();
          if (seq !== refresh_seq_ref.current) return;
          if (!route_operation_running.current && !pending_route_readback.current && !pending_rename_readback.current && routes_equal(routes_ref.current, baselineRef.current)) {
            setRoutes(loaded);
            baselineRef.current = loaded;
          }
        } catch { /* keep the last known routes */ }
      }
      if (seq !== refresh_seq_ref.current) return;
      setError(null);
      void on_gateway_status_refresh?.();
    } catch (e) {
      if (seq !== refresh_seq_ref.current) return;
      setError(String(e));
    }
  };

  const refresh_proxy_config_status = async () => {
    try {
      const status = await getProxyStatus();
      set_config_changed(status.running && status.config_changed);
    } catch {
      // Preserve the last known status when the read-only status command fails.
    }
  };

  /** Track previous appType for route caching on tab switch. */
  const prevAppRef = useRef(appType);

  useEffect(() => {
    // Routes are global; unsaved edits are page state, not a cache. Leaving
    // the page drops them, and stepping into one reloads the shared table.
    if (prevAppRef.current !== appType) {
      codex_mirror_request_id_ref.current += 1;
      prevAppRef.current = appType;
      setAdvancedRoutesOpen(advancedRoutesOpenByApp.get(appType) ?? false);
      // Selection and collapse state are per-app editor chrome — a switch
      // must not carry a sibling app's batch-delete selection in.
      setSelectedAliases(new Set());
      setCollapsedRoutes(new Set());
    }
    void refresh();
    let alive = true;
    managed_cli_request_id_ref.current += 1;
    set_managed_cli(null);
    set_managed_cli_error(null);
    if (cfg.launchers === 'managed_cli' && (appType === 'kilo_cli' || appType === 'cline_cli')) {
      managed_cli_status(appType).then((status) => {
        if (alive) {
          set_managed_cli(status);
          set_managed_cli_error(null);
        }
      }).catch((e) => {
        if (alive) set_managed_cli_error({ appType, message: String(e) });
      });
    }
    if (cfg.launchers === 'codex') {
      const request_id = codex_mirror_request_id_ref.current;
      codexCliStatus()
        .then((s) => {
          // Same `alive` gate as the managed CLI branch: this read is
          // issued for the app that was active when the effect ran, so a
          // late answer must not write the Codex row onto the page the
          // user has since switched to (the gateway card and the launcher
          // read these values on every app).
          if (!alive || codex_mirror_request_id_ref.current !== request_id) return;
          setCliStatus(s);
          setCliStatusError(null);
        })
        .catch((e) => {
          if (!alive || codex_mirror_request_id_ref.current !== request_id) return;
          setCliStatus(null);
          setCliStatusError(String(e));
        });
    }
    if (cfg.launchers === 'claude') {
      claudeMirrorStatus()
        .then((s) => {
          if (!alive) return;
          setClaudeStatus(s);
          setClaudeStatusError(null);
        })
        .catch((e) => {
          if (!alive) return;
          setClaudeStatus(null);
          setClaudeStatusError(String(e));
        });
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
    return () => { alive = false; };
  }, [appType]);

  /** Keep the gateway snapshot and the configured-vs-running badge current
   *  even when another app action, a crash, or background model discovery
   *  changes the live state. Pause the poll while the window is hidden.
   *  `apply_gateway_snapshot` only writes state setters (and a ref), so the
   *  first-render identity captured here stays correct. */
  useEffect(() => {
    let active = true;
    let interval_id: number | null = null;
    const tick = async () => {
      try {
        const status = await getProxyStatus();
        if (active) {
          apply_gateway_snapshot(status.config, status);
          set_config_changed(status.running && status.config_changed);
        }
      } catch {
        // Preserve the last known value when the read-only status check fails.
      }
    };
    const start = () => {
      if (interval_id !== null) return;
      void tick();
      interval_id = window.setInterval(() => void tick(), 5_000);
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
      active = false;
      stop();
      document.removeEventListener('visibilitychange', on_visibility);
    };
  }, []);

  /** Mirror CODEX_HOME row staleness fix (Phase 22u): refresh the
   *  Codex CLI status every 10 s on the Codex page so the row tracks
   *  settings changes without a tab switch. Paused when hidden. */
  useEffect(() => {
    if (cfg.launchers !== 'codex') return;
    let active = true;
    const tick = () => {
      const request_id = codex_mirror_request_id_ref.current;
      codexCliStatus()
        .then((s) => {
          if (!active || codex_mirror_request_id_ref.current !== request_id) return;
          // Keep the previous object when the payload is unchanged so an
          // idle 10 s tick does not re-render the whole page.
          setCliStatus((prev) => (same_status_payload(prev, s) ? prev : s));
          setCliStatusError(null);
        })
        .catch((e) => {
          if (!active || codex_mirror_request_id_ref.current !== request_id) return;
          setCliStatus(null);
          setCliStatusError(String(e));
        });
    };
    let intervalId: number | null = null;
    const start = () => {
      if (intervalId !== null) return;
      intervalId = window.setInterval(tick, 10_000);
    };
    const stop = () => {
      if (intervalId !== null) {
        window.clearInterval(intervalId);
        intervalId = null;
      }
    };
    if (document.visibilityState !== 'hidden') start();
    const onVis = () => (document.visibilityState === 'hidden' ? stop() : start());
    document.addEventListener('visibilitychange', onVis);
    return () => {
      active = false;
      stop();
      document.removeEventListener('visibilitychange', onVis);
    };
  }, [cfg.launchers]);

  /** Same 10 s staleness fix for the Claude mirror status row. */
  useEffect(() => {
    if (cfg.launchers !== 'claude') return;
    let active = true;
    const tick = () => {
      claudeMirrorStatus()
        .then((s) => {
          if (!active) return;
          // Same suppression as the Codex mirror tick: unchanged payload
          // keeps the previous object identity.
          setClaudeStatus((prev) => (same_status_payload(prev, s) ? prev : s));
          setClaudeStatusError(null);
        })
        .catch((e) => {
          if (!active) return;
          setClaudeStatus(null);
          setClaudeStatusError(String(e));
        });
    };
    let intervalId: number | null = null;
    const start = () => {
      if (intervalId !== null) return;
      intervalId = window.setInterval(tick, 10_000);
    };
    const stop = () => {
      if (intervalId !== null) {
        window.clearInterval(intervalId);
        intervalId = null;
      }
    };
    if (document.visibilityState !== 'hidden') start();
    const onVis = () => (document.visibilityState === 'hidden' ? stop() : start());
    document.addEventListener('visibilitychange', onVis);
    return () => {
      active = false;
      stop();
      document.removeEventListener('visibilitychange', onVis);
    };
  }, [cfg.launchers]);

  /** Memoized pool value is defined further down, after its handler
   *  dependencies (breakerOf / breakerOfClass / handleTestProvider /
   *  clearTestResult) — a `const` useMemo above those declarations hits
   *  the temporal dead zone at first render (white-screen crash). */

  /** Ref the timer so back-to-back copies reset the indicator window; a
   * pending clear also never fires on an unmounted page. */
  const copied_timer_ref = useRef<number | null>(null);
  useEffect(() => () => {
    if (copied_timer_ref.current !== null) window.clearTimeout(copied_timer_ref.current);
  }, []);

  const copy = async (text: string, key: string) => {
    try {
      await navigator.clipboard.writeText(text);
      setCopied(key);
      if (copied_timer_ref.current !== null) window.clearTimeout(copied_timer_ref.current);
      copied_timer_ref.current = window.setTimeout(() => {
        setCopied(null);
        copied_timer_ref.current = null;
      }, 1500);
    } catch {
      /* clipboard unavailable */
    }
  };

  const handleImport = () => run_cc_switch_import({
    running,
    import_providers: importCcSwitchProviders,
    stop_proxy: stopProxy,
    start_proxy: startProxy,
    refresh_page: refresh,
    translate: t,
    flash,
    set_importing: setImporting,
    set_error: setError,
  });

  // ---- Unified provider cards (presets + linked upstreams) ----

  /** Open the unified modal: `card` omitted = create a new preset card;
   *  `{ standalone: true }` = create a new standalone pool row (identity +
   *  upstream section only, saved straight to `proxy_providers`). */
  const openProviderModal = (card?: UnifiedCard | { standalone: true }) => {
    if (card && 'standalone' in card) {
      // Reuse the blank-preset form (the per-app `values` are irrelevant for
      // a standalone row — the modal hides the connection section) instead
      // of duplicating the 14-field payload.
      setProviderForm(emptyProviderForm(cfg, '', appType));
      // A synthetic standalone card with a null upstream marks the save
      // path as "create pool row" while keeping the modal's standalone
      // layout (real standalone cards carry the existing row here).
      setEditingCard({ kind: 'standalone', key: 'new', preset: null, upstream: null, proxyId: null, priority: null, supplierManaged: false });
      set_provider_modal_error(null);
      setShowProviderModal(true);
      return;
    }
    if (card?.preset) {
      const preset = card.preset;
      setProviderForm({
        id: preset.id,
        name: preset.name,
        values: { ...cfg.extractForm(preset) },
        icon: preset.icon ?? '',
        iconColor: preset.iconColor ?? '',
        websiteUrl: preset.websiteUrl ?? '',
        notes: preset.notes ?? '',
        upstreamType: card.upstream?.type ?? defaultUpstreamTypeFor(appType),
        upstreamBaseURL: '',
        upstreamAPIKey: '',
        upstreamModels: (card.upstream?.models ?? []).join(', '),
        upstreamModelMapping: formatModelMapping(card.upstream?.modelMapping),
        upstreamExtraHeaders: formatHeaderJson(card.upstream?.extraHeaders),
        upstreamAutoTrack: card.upstream?.autoTrackModels ?? true,
      });
      setEditingCard(card);
    } else if (card?.upstream) {
      // Standalone upstream card: identity + upstream section only.
      const upstream = card.upstream;
      setProviderForm({
        id: upstream.id,
        name: upstream.name,
        values: {},
        icon: upstream.icon ?? '',
        iconColor: upstream.iconColor ?? '',
        websiteUrl: '',
        notes: '',
        upstreamType: upstream.type,
        upstreamBaseURL: upstream.baseURL,
        upstreamAPIKey: upstream.apiKey,
        upstreamModels: (upstream.models ?? []).join(', '),
        upstreamModelMapping: formatModelMapping(upstream.modelMapping),
        upstreamExtraHeaders: formatHeaderJson(upstream.extraHeaders),
        upstreamAutoTrack: upstream.autoTrackModels,
      });
      setEditingCard(card);
    } else {
      setProviderForm(emptyProviderForm(cfg, gatewayUrl, appType));
      setEditingCard(null);
    }
    set_provider_modal_error(null);
    setShowProviderModal(true);
  };

  /** Open the unified modal prefilled from a vendor template: the normal
   *  save path then creates the preset card, the linked `ccs-<id>` upstream
   *  (failover-chain member by default), and the per-model routes via the
   *  seeded identity `modelMapping` at the next proxy start. Only the API
   *  key is left for the user to paste. */
  const openProviderModalFromTemplate = (template: ProviderPresetTemplate) => {
    const values: Record<string, string> = {};
    for (const f of cfg.fields) {
      if (f.key === 'baseUrl') values.baseUrl = template.baseURL;
      else if (f.type === 'select' && f.options?.length) values[f.key] = f.options[0];
      else values[f.key] = '';
    }
    setProviderForm({
      id: template.id,
      name: template.name,
      values,
      icon: template.icon,
      iconColor: template.iconColor,
      websiteUrl: template.websiteUrl,
      notes: '',
      upstreamType: 'anthropic',
      upstreamBaseURL: '',
      upstreamAPIKey: '',
      upstreamModels: template.models.join(', '),
      upstreamModelMapping: JSON.stringify(
        identity_model_mapping(template.models),
        null,
        2,
      ),
      upstreamExtraHeaders: '',
      upstreamAutoTrack: true,
    });
    setEditingCard(null);
    set_provider_modal_error(null);
    setShowProviderModal(true);
  };

  /** Save the unified modal. Standalone cards write the pool row directly;
   *  preset cards write the providers row, then sync the linked `ccs-<id>`
   *  upstream (creating it when the connection fields carry a usable base
   *  URL; skipped for empty or self-loop URLs, mirroring the importer's
   *  self-loop guard). */
  const handleSaveCard = async () => {
    if (provider_saving) return;
    set_provider_saving(true);
    set_provider_modal_error(null);
    // Optional per-upstream model mapping (JSON textarea): empty keeps the
    // stored mapping untouched; invalid JSON blocks the save.
    let parsedModelMapping: Record<string, string> | undefined;
    if (providerForm.upstreamModelMapping.trim()) {
      try {
        parsedModelMapping = JSON.parse(
          providerForm.upstreamModelMapping,
        ) as Record<string, string>;
      } catch {
        set_provider_modal_error(t('providers.modal.modelMappingInvalid'));
        set_provider_saving(false);
        return;
      }
    }
    // Optional per-upstream extra headers (JSON textarea): empty keeps the
    // stored headers untouched; invalid JSON blocks the save.
    let parsedExtraHeaders: Record<string, string> | undefined;
    if (providerForm.upstreamExtraHeaders.trim()) {
      try {
        parsedExtraHeaders = JSON.parse(
          providerForm.upstreamExtraHeaders,
        ) as Record<string, string>;
      } catch {
        set_provider_modal_error(t('providers.modal.extraHeadersInvalid'));
        set_provider_saving(false);
        return;
      }
    }
    try {
      if (editingCard?.kind === 'standalone') {
        const existing = editingCard.upstream;
        const id = providerForm.id.trim() || slugifyProviderId(providerForm.name);
        const name = providerForm.name.trim();
        const baseUrl = providerForm.upstreamBaseURL.trim();
        // A pool row without an id/name is unusable (empty primary key would
        // collide on every save) and one without a base URL cannot route —
        // the DAO accepts both silently, so the form is the guard.
        if (!name && !id) {
          set_provider_modal_error(t('providers.modal.nameRequired'));
          return;
        }
        if (!baseUrl) {
          set_provider_modal_error(t('providers.modal.baseURLRequired'));
          return;
        }
        await saveProxyProvider({
          id,
          name: name || id,
          type: providerForm.upstreamType,
          apiKey: providerForm.upstreamAPIKey.trim(),
          baseURL: baseUrl,
          models: parseModelList(providerForm.upstreamModels),
          // Preserve the row's fields the modal does not expose — editing
          // must not silently wipe enabled/breaker state.
          enabled: existing?.enabled ?? true,
          timeoutMs: existing?.timeoutMs ?? 60000,
          weight: existing?.weight ?? 1,
          circuitBreaker: existing?.circuitBreaker ?? DEFAULT_CIRCUIT_BREAKER,
          modelMapping: parsedModelMapping ?? existing?.modelMapping ?? {},
          extraHeaders: parsedExtraHeaders ?? existing?.extraHeaders ?? {},
          autoTrackModels: providerForm.upstreamAutoTrack,
          icon: providerForm.icon.trim() || null,
          iconColor: providerForm.iconColor || null,
        });
      } else {
        const id = providerForm.id.trim() || slugifyProviderId(providerForm.name);
        // A preset without a resolvable id cannot be addressed: `providers.id`
        // is the primary key and the linked upstream id is derived from it
        // (`ccs-<id>`), so the DAO accepts an empty row silently, the card
        // renders blank, and every later nameless save collides with it. Same
        // guard the standalone path above already applies.
        if (!id) {
          set_provider_modal_error(t('providers.modal.providerNameRequired'));
          return;
        }
        const builtConfig = cfg.buildConfig(id, providerForm.values);
        if (appType === 'codex') {
          // Keep declared model capabilities, aliases and unrelated Codex
          // settings. The form owns the selected effort, including clearing
          // back to default.
          const previous_config = editingCard?.preset?.settingsConfig ?? {};
          for (const [key, value] of Object.entries(previous_config)) {
            if (!(key in builtConfig) && key !== 'model_reasoning_effort' && key !== 'model') {
              (builtConfig as Record<string, unknown>)[key] = value;
            }
          }
        }
        const provider: Provider = {
          id,
          name: providerForm.name.trim() || id,
          settingsConfig: builtConfig,
          createdAt: editingCard?.preset?.createdAt ?? Math.floor(Date.now() / 1000),
          sortIndex: editingCard?.preset?.sortIndex ?? null,
          notes: providerForm.notes.trim() || null,
          websiteUrl: providerForm.websiteUrl.trim() || null,
          icon: providerForm.icon.trim() || null,
          iconColor: providerForm.iconColor || null,
          inFailoverQueue: editingCard?.preset?.inFailoverQueue ?? true,
        };
        await saveProvider(appType, provider);

        const baseUrl = (providerForm.values.baseUrl ?? '').trim();
        const apiKey = (providerForm.values.apiKey ?? '').trim();
        const linkedUpstream =
          editingCard?.upstream ??
          proxyProviders.find((pp) => pp.id === upstreamIdOf(id)) ??
          null;
        const selfLoop = Boolean(gatewayUrl) && baseUrl === gatewayUrl;
        if (linkedUpstream || (baseUrl && !selfLoop)) {
          await saveProxyProvider({
            id: upstreamIdOf(id),
            name: provider.name,
            type: providerForm.upstreamType,
            apiKey,
            baseURL: baseUrl,
            models: parseModelList(providerForm.upstreamModels),
            enabled: linkedUpstream?.enabled ?? true,
            timeoutMs: linkedUpstream?.timeoutMs ?? 60000,
            weight: linkedUpstream?.weight ?? 1,
            circuitBreaker: linkedUpstream?.circuitBreaker ?? DEFAULT_CIRCUIT_BREAKER,
            modelMapping: parsedModelMapping ?? linkedUpstream?.modelMapping ?? {},
            extraHeaders: parsedExtraHeaders ?? linkedUpstream?.extraHeaders ?? {},
            autoTrackModels: providerForm.upstreamAutoTrack,
            // The pool row mirrors the identity icon so it reads correctly
            // wherever it surfaces as a standalone card.
            icon: provider.icon,
            iconColor: provider.iconColor,
          });
        }
        if (appType === 'codex' && current === id) {
          // Saving the active Codex row must apply effort and catalog changes.
          await switchProvider(id, appType);
          refresh_client_config();
        }
      }
      setShowProviderModal(false);
      flash(t('providers.saved'));
      await refresh();
    } catch (e) {
      set_provider_modal_error(String(e));
    } finally {
      set_provider_saving(false);
    }
  };

  /** Delete a unified card: both sides of a linked card go in one confirm. */
  const handleDeleteCard = async (card: UnifiedCard) => {
    if (busy !== null) return 'cancelled';
    const label = card.preset?.name || card.upstream?.name || card.key;
    const confirmMsg =
      card.preset && card.upstream
        ? t('providers.delete.confirmLinked', { id: label, upstream: card.upstream.id })
        : t('providers.delete.confirm', { id: label });
    if (!window.confirm(confirmMsg)) return 'cancelled';
    setBusy(`delete-${card.key}`);
    setError(null);
    set_provider_modal_error(null);
    try {
      if (card.preset) await deleteProvider(card.preset.id, appType);
      if (card.upstream) await deleteProxyProvider(card.upstream.id);
      flash(t('providers.deleted'));
      await refresh();
      return 'deleted';
    } catch (e) {
      setError(String(e));
      if (showProviderModal) set_provider_modal_error(String(e));
      return 'failed';
    } finally {
      setBusy(null);
    }
  };

  const handleSwitch = async (id: string) => {
    if (busy !== null) return;
    setBusy(`switch-${id}`);
    setError(null);
    setNotice(null);
    try {
      const r = await switchProvider(id, appType);
      setCurrent(r.provider_id);
      const name = providers.find((p) => p.id === id)?.name ?? id;
      flash(
        r.written.length > 0
          ? t('providers.switched.wrote', { name, count: String(r.written.length) })
          : t('providers.switched.environment', { name }),
      );
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  };

  const handleToggleQueue = async (card: UnifiedCard) => {
    if (!card.preset) return;
    try {
      await setProviderFailoverQueue(
        card.preset.id,
        appType,
        card.preset.inFailoverQueue === false,
      );
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  };

  /** Reorder preset cards (persisted chain order = failover priority). */
  const handleReorderCards = async (fromIdx: number, toIdx: number) => {
    const ids = providers.map((p) => p.id);
    if (fromIdx === toIdx || fromIdx < 0 || fromIdx >= ids.length) return;
    const [moved] = ids.splice(fromIdx, 1);
    ids.splice(toIdx, 0, moved);
    try {
      await reorderProviders(appType, ids);
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  };

  const handleOpenDesktop = async () => {
    const request_id = ++codex_mirror_request_id_ref.current;
    setBusy('desktop');
    try {
      const r = await codexOpenDesktopWindow();
      if (codex_mirror_request_id_ref.current !== request_id) return;
      const mirror_mode = r.mirrorMode;
      if (mirror_mode) {
        setCliStatus((status) => status ? { ...status, mirrorMode: mirror_mode } : status);
      }
      refresh_client_config();
      const launched = t('codex.open.desktopLaunched', { dir: r.codexHome });
      flash(r.mirrorModeChanged ? `${launched} ${t('codex.mirror.mode.exclusiveFlash')}` : launched);
    } catch (e) {
      if (codex_mirror_request_id_ref.current !== request_id) return;
      // Preparation can succeed before process launch fails. Read the actual
      // mode so the control still describes the configuration on disk.
      try {
        const confirmed_status = await codexCliStatus();
        if (codex_mirror_request_id_ref.current !== request_id) return;
        setCliStatus(confirmed_status);
        setCliStatusError(null);
      } catch (status_error) {
        if (codex_mirror_request_id_ref.current !== request_id) return;
        setCliStatus(null);
        setCliStatusError(String(status_error));
      }
      setError(t('codex.open.failed', { message: String(e) }));
    } finally {
      setBusy(null);
    }
  };

  const handleOpenCli = async () => {
    setBusy('cli');
    try {
      const r = await codexOpenCliTerminal();
      refresh_client_config();
      flash(t('codex.open.cliLaunched', { dir: r.codexHome }));
    } catch (e) {
      setError(t('codex.open.failed', { message: String(e) }));
    } finally {
      setBusy(null);
    }
  };

  /** Shared body for terminal launchers: invoke, refresh the client-config
   *  card, flash the launched message, surface failures. One factory keeps
   *  the busy key / flash key wiring mechanically consistent per launcher. */
  const terminal_launcher = (
    busy_key: string,
    invoke_launcher: () => Promise<{ providerId: string }>,
    flash_key: string,
  ) => async () => {
    setBusy(busy_key);
    try {
      const r = await invoke_launcher();
      refresh_client_config();
      flash(t(flash_key, { provider: r.providerId }));
    } catch (e) {
      setError(t('codex.open.failed', { message: String(e) }));
    } finally {
      setBusy(null);
    }
  };

  const handle_open_managed_cli = async () => {
    if (appType !== 'kilo_cli' && appType !== 'cline_cli') return;
    setBusy('managed_cli');
    try {
      const receipt = await managed_cli_open_terminal(appType);
      refresh_client_config();
      flash(t('launcher.launched', { label: cfg.label, binary: receipt.binary }));
    } catch (e) {
      const error_text = String(e);
      if (error_text.includes('managed_cli_provider_required')) {
        setError(t('launcher.managed_provider_required', { label: cfg.label }));
      } else if (error_text.includes('managed_cli_not_found')) {
        setError(t('launcher.managed_cli_not_found', { label: cfg.label }));
      } else {
        setError(t('codex.open.failed', { message: error_text }));
      }
    } finally {
      setBusy(null);
    }
  };

  const handleOpenCopilot = terminal_launcher(
    'copilot',
    copilotOpenCliTerminal,
    'codex.open.copilotLaunched',
  );

  const handleOpenGemini = terminal_launcher(
    'gemini',
    geminiOpenCliTerminal,
    'codex.open.geminiLaunched',
  );

  const handleOpenVscode = async () => {
    if (appType !== 'claude_vscode' && appType !== 'kilo_vscode') return;
    setBusy('vscode');
    try {
      const r = await vscodeOpenWithExtension(appType);
      refresh_client_config();
      flash(t('vscode.open.launched', { binary: r.binary }));
    } catch (e) {
      setError(t('vscode.open.failed', { message: String(e) }));
    } finally {
      setBusy(null);
    }
  };

  const surfaceKey = useMemo(
    () => `app.endpoint.${cfg.surface}` as const,
    [cfg.surface],
  );

  /** Flip the Codex mirror mode (exclusive ⇄ coexist). Optimistic — the
   *  backend persists the setting and rewrites the mirror's config.toml
   *  immediately. Recovery reads the actual mode because a previous local
   *  value may itself belong to another optimistic request. */
  const handleToggleMirrorMode = async (coexist: boolean) => {
    if (codex_mirror_mutation_ref.current) return;
    const recovering_unknown = cliStatus === null;
    if (recovering_unknown) {
      codex_mirror_mutation_ref.current = true;
      setBusy('codex_mirror_mode');
    }
    const request_id = ++codex_mirror_request_id_ref.current;
    const next_mode = coexist ? 'coexist' : 'exclusive';
    setCliStatus((status) => (status ? { ...status, mirrorMode: next_mode } : status));
    try {
      await codexMirrorSetMode(next_mode);
      if (recovering_unknown) {
        const confirmed_status = await codexCliStatus();
        if (codex_mirror_request_id_ref.current !== request_id) return;
        setCliStatus((status) => status ? { ...status, mirrorMode: confirmed_status.mirrorMode } : confirmed_status);
        setCliStatusError(null);
      }
    } catch (e) {
      if (codex_mirror_request_id_ref.current !== request_id) return;
      try {
        const confirmed_status = await codexCliStatus();
        if (codex_mirror_request_id_ref.current !== request_id) return;
        setCliStatus((status) => status ? { ...status, mirrorMode: confirmed_status.mirrorMode } : confirmed_status);
        setCliStatusError(null);
      } catch (status_error) {
        if (codex_mirror_request_id_ref.current !== request_id) return;
        setCliStatus(null);
        setCliStatusError(String(status_error));
      }
      setError(String(e));
      return;
    } finally {
      if (recovering_unknown) {
        codex_mirror_mutation_ref.current = false;
        setBusy((current) => current === 'codex_mirror_mode' ? null : current);
      }
    }
    if (codex_mirror_request_id_ref.current !== request_id) return;
    flash(
      coexist
        ? t('codex.mirror.mode.coexistFlash')
        : t('codex.mirror.mode.exclusiveFlash'),
    );
  };

  /** Open a Claude Desktop instance against AAG's CLAUDE_CONFIG_DIR
   *  mirror (settings.json rewritten first; native ~/.claude untouched). */
  const handleOpenClaudeDesktop = async () => {
    setBusy('claude_desktop');
    try {
      const r = await claudeOpenDesktop();
      refresh_client_config();
      flash(t('claude.open.launched', { dir: r.claudeHome }));
    } catch (e) {
      setError(t('claude.open.failed', { message: String(e) }));
    } finally {
      setBusy(null);
    }
  };

  const handle_open_claude_cli = async () => {
    setBusy('claude_cli');
    setError(null);
    setNotice(null);
    try {
      const result = await open_claude_cli_terminal();
      refresh_client_config();
      flash(t('claude.open.cli_launched', { dir: result.claudeHome }));
    } catch (error) {
      setError(t('claude.open.cli_failed', { message: String(error) }));
    } finally {
      setBusy(null);
    }
  };

  /** Enable/disable an upstream provider. Optimistic: flip local state
   *  first, then persist. The router skips disabled providers on its
   *  next rebuild — no full refresh needed for a single-bit flip. */
  const handleToggleEnabled = async (id: string, enabled: boolean) => {
    const target = proxyProviders.find((pp) => pp.id === id);
    if (!target || target.enabled === enabled) return;
    setProxyProviders((prev) =>
      prev.map((p) => (p.id === id ? { ...p, enabled } : p)),
    );
    try {
      await saveProxyProvider({ ...target, enabled });
      await refresh_proxy_config_status();
    } catch (e) {
      // Roll back on failure.
      setProxyProviders((prev) =>
        prev.map((p) => (p.id === id ? { ...p, enabled: target.enabled } : p)),
      );
      setError(String(e));
    }
  };

  /** Get breaker state for a proxy provider from the health data. */
  const breakerOf = (id: string): string | null => {
    if (!health) return null;
    const ph = health.providers.find((p) => p.id === id);
    return ph?.breaker?.state ?? null;
  };

  /** CSS class for breaker state. */
  const breakerOfClass = (state: string | null): string => {
    if (state === 'open') return 'breaker-open';
    if (state === 'half-open') return 'breaker-half';
    return 'breaker-closed';
  };

  // ---- Route CRUD (failover chain editor) ----

  /** Filter routes to those relevant for the current app. */
  const filteredRoutes = useMemo(() => {
    if (appType === 'codex') {
      return routes.filter((r) => r.alias.startsWith('codex-'));
    }
    // Claude: show all routes except codex-prefixed ones
    return routes.filter((r) => !r.alias.startsWith('codex-'));
  }, [routes, appType]);

  /** Stage a new empty route locally. Persisted together with everything
   *  else on Save — keeps the dirty indicator truthful and avoids the old
   *  immediate-reload wiping unsaved hop edits on other routes. */
  const handleAddRoute = () => {
    if (route_operation_running.current) return;
    const alias = window.prompt(t('route.add') + ' — alias (e.g. claude-sonnet-5):');
    const id = alias?.trim();
    if (!id) return;
    if (routes.some((r) => r.alias === id) || pending_rename_readback.current?.target_alias === id) {
      flash(t('route.exists', { alias: id }));
      return;
    }
    setRoutes((prev) => [...prev, { alias: id, description: '', hops: [] }]);
    // Ensure the new route is expanded so hops can be added right away.
    setCollapsedRoutes((prev) => {
      const next = new Set(prev);
      next.delete(id);
      return next;
    });
  };

  /** Stage a route removal locally (persisted on Save; Discard restores). */
  const handleDeleteRoute = (alias: string) => {
    if (route_operation_running.current) return;
    if (!window.confirm(t('route.delete.confirm', { alias }))) return;
    setRoutes((prev) => prev.filter((r) => r.alias !== alias));
    setSelectedAliases((prev) => {
      const next = new Set(prev);
      next.delete(alias);
      return next;
    });
  };

  /** New aliases rename within the draft. Existing rows rename immediately
   *  through the atomic DAO, then rebase the alias while preserving hop edits. */
  const adopt_route_rename = (from: string, to: string, baseline: Route[]) => {
    setRoutes((current) => current.map((route) => route.alias === from ? { ...route, alias: to } : route));
    baselineRef.current = baseline;
    const remap = (aliases: Set<string>) => new Set([...aliases].map((id) => id === from ? to : id));
    setSelectedAliases(remap);
    setCollapsedRoutes(remap);
  };

  const handleRenameRoute = async (alias: string) => {
    if (route_operation_running.current) return;
    if (pending_route_readback.current || pending_rename_readback.current) { setError(t('route.outcomeUnknown')); return; }
    const next = window.prompt(t('route.rename.prompt', { alias }))?.trim();
    if (!next || next === alias) return;
    if (routes.some((r) => r.alias === next) || baselineRef.current.some((r) => r.alias === next)) {
      flash(t('route.exists', { alias: next }));
      return;
    }
    const remap = (rs: Route[]) => rs.map((r) => r.alias === alias ? { ...r, alias: next } : r);
    const remap_selection = (aliases: Set<string>) => new Set([...aliases].map((id) => id === alias ? next : id));
    // A new route has no backend row yet: renaming remains part of its draft.
    if (!baselineRef.current.some((r) => r.alias === alias)) {
      setRoutes(remap);
      setSelectedAliases(remap_selection);
      setCollapsedRoutes(remap_selection);
      return;
    }
    route_operation_running.current = true;
    set_routes_saving(true);
    setPointerDrag(null);
    setRouteOverIdx(null);
    try {
      // Existing rows keep the established immediate atomic-rename contract.
      await renameRoute(alias, next);
      adopt_route_rename(alias, next, remap(baselineRef.current));
      flash(t('route.renamed', { from: alias, to: next }));
      await refresh_proxy_config_status();
    } catch (e) {
      const readback = { source: baselineRef.current.find((route) => route.alias === alias)!, target_alias: next };
      try {
        const receipt = await read_route_rename(readback, baselineRef.current, listRoutes);
        if (receipt.outcome === 'renamed') {
          adopt_route_rename(alias, next, receipt.baseline);
          flash(t('route.savedApplyPending'));
          await refresh_proxy_config_status();
        } else if (receipt.outcome === 'unchanged') setError(String(e));
        else throw new Error('route_rename_readback_unknown');
      } catch {
        pending_rename_readback.current = readback;
        set_route_outcome_unknown(true);
        setError(t('route.outcomeUnknown'));
      }
    }
    finally { route_operation_running.current = false; set_routes_saving(false); }
  };

  const handleAddHop = (alias: string) => {
    if (route_operation_running.current) return;
    setRoutes((prev) => prev.map((r) =>
      r.alias === alias
        ? { ...r, hops: [...r.hops, { provider: proxyProviders[0]?.id ?? '', model: '' }] }
        : r
    ));
  };

  const handleRemoveHop = (alias: string, hopIdx: number) => {
    if (route_operation_running.current) return;
    setRoutes((prev) => prev.map((r) =>
      r.alias === alias
        ? { ...r, hops: r.hops.filter((_, i) => i !== hopIdx) }
        : r
    ));
  };

  const handleEditHop = (alias: string, hopIdx: number, field: 'provider' | 'model', value: string) => {
    if (route_operation_running.current) return;
    setRoutes((prev) => prev.map((r) =>
      r.alias === alias
        ? { ...r, hops: r.hops.map((h, i) => i === hopIdx ? { ...h, [field]: value } : h) }
        : r
    ));
  };

  const [routeOverIdx, setRouteOverIdx] = useState<{ alias: string; idx: number } | null>(null);
  /** Pointer-based drag (more reliable than HTML5 DnD in WebView2). */
  const [pointerDrag, setPointerDrag] = useState<{ alias: string; fromIdx: number } | null>(null);

  /** Reorder a hop by moving it from `fromIdx` to `toIdx` (pointer drag). */
  const applyHopMove = (alias: string, fromIdx: number, toIdx: number) => {
    if (route_operation_running.current) return;
    if (fromIdx === toIdx) return;
    setRoutes((prev) => prev.map((r) => {
      if (r.alias !== alias) return r;
      const hops = [...r.hops];
      const [moved] = hops.splice(fromIdx, 1);
      hops.splice(toIdx, 0, moved);
      return { ...r, hops };
    }));
  };

  /** Keyboard reorder for the hop drag handle (↑/↓ move, Home/End to ends).
   *  Mirrors the pointer-drag path so the handle isn't fake-accessible. */
  const handleHopKeyDown = (
    alias: string,
    idx: number,
    e: React.KeyboardEvent,
    hopCount: number,
  ) => {
    if (route_operation_running.current) return;
    let toIdx: number | null = null;
    if (e.key === 'ArrowUp') toIdx = Math.max(0, idx - 1);
    else if (e.key === 'ArrowDown') toIdx = Math.min(hopCount - 1, idx + 1);
    else if (e.key === 'Home') toIdx = 0;
    else if (e.key === 'End') toIdx = hopCount - 1;
    else return;
    e.preventDefault();
    e.stopPropagation();
    applyHopMove(alias, idx, toIdx);
    // Keep focus on the moved handle (same visual position) so ↑/↓ repeat.
    requestAnimationFrame(() => {
      document
        .querySelector<HTMLElement>(
          `.hop-row[data-route-alias="${CSS.escape(alias)}"][data-hop-idx="${toIdx}"] .hop-drag-handle`,
        )
        ?.focus();
    });
  };

  /** Resolve the hop row under a pointer position, or null. */
  const hopRowAt = (x: number, y: number): { alias: string; idx: number } | null => {
    const el = document.elementFromPoint(x, y);
    const row = el?.closest?.('.hop-row') as HTMLElement | null;
    if (!row) return null;
    const alias = row.dataset.routeAlias;
    const idx = Number(row.dataset.hopIdx);
    if (!alias || Number.isNaN(idx)) return null;
    return { alias, idx };
  };

  const handlePointerDown = (alias: string, fromIdx: number, e: React.PointerEvent) => {
    if (route_operation_running.current) return;
    e.preventDefault();
    (e.currentTarget as HTMLElement).setPointerCapture?.(e.pointerId);
    setPointerDrag({ alias, fromIdx });
    setRouteOverIdx({ alias, idx: fromIdx });
  };

  const handlePointerMove = (e: React.PointerEvent) => {
    if (route_operation_running.current) return;
    if (!pointerDrag) return;
    const target = hopRowAt(e.clientX, e.clientY);
    if (target && target.alias === pointerDrag.alias) {
      setRouteOverIdx({ alias: target.alias, idx: target.idx });
    } else if (routeOverIdx) {
      setRouteOverIdx(null);
    }
  };

  const handlePointerUp = (e: React.PointerEvent) => {
    if (route_operation_running.current) return;
    if (!pointerDrag) return;
    const target = hopRowAt(e.clientX, e.clientY);
    const toIdx = target && target.alias === pointerDrag.alias ? target.idx : pointerDrag.fromIdx;
    applyHopMove(pointerDrag.alias, pointerDrag.fromIdx, toIdx);
    (e.currentTarget as HTMLElement).releasePointerCapture?.(e.pointerId);
    setPointerDrag(null);
    setRouteOverIdx(null);
  };

  /** A cancelled gesture is NOT a drop. The browser cancels the pointer
   *  when a touch scroll, palm rejection, or a second pointer takes over —
   *  applying the move there would reorder the hop (and dirty the editor)
   *  for a drag the user never finished. */
  const handlePointerCancel = (e: React.PointerEvent) => {
    if (!pointerDrag) return;
    (e.currentTarget as HTMLElement).releasePointerCapture?.(e.pointerId);
    setPointerDrag(null);
    setRouteOverIdx(null);
  };

  /** Reconcile the editor state with the backend: save routes that are new
   *  or differ from the baseline, delete routes that were removed while
   *  editing (add/remove stage locally), skip untouched routes. */
  const handleSaveRoutes = async () => {
    if (route_operation_running.current) return;
    route_operation_running.current = true;
    set_routes_saving(true);
    setPointerDrag(null);
    setRouteOverIdx(null);
    setError(null);
    try {
      let saved_apply_pending = false;
      if (pending_rename_readback.current) {
        const readback = pending_rename_readback.current;
        try {
          const receipt = await read_route_rename(readback, baselineRef.current, listRoutes);
          if (receipt.outcome === 'unknown') { setError(t('route.outcomeUnknown')); return; }
          pending_rename_readback.current = null;
          set_route_outcome_unknown(false);
          if (receipt.outcome === 'renamed') {
            adopt_route_rename(readback.source.alias, readback.target_alias, receipt.baseline);
            flash(t('route.savedApplyPending'));
          } else setError(t('route.renameNotApplied'));
        } catch { setError(t('route.outcomeUnknown')); }
        return;
      }
      // Reconcile an earlier unknown result before submitting any new writes.
      if (pending_route_readback.current) {
        try {
          const receipt = await read_route_writeback(pending_route_readback.current, baselineRef.current, listRoutes);
          baselineRef.current = receipt.baseline;
          saved_apply_pending = receipt.persisted_after_rejection.length > 0;
          pending_route_readback.current = null;
          set_route_outcome_unknown(false);
        } catch {
          setError(t('route.outcomeUnknown'));
          return;
        }
      }
      const baselineByAlias = new Map(baselineRef.current.map((r) => [r.alias, r]));
      const currentAliases = new Set(routes.map((r) => r.alias));
      const toSave = routes.filter((r) => {
        const base = baselineByAlias.get(r.alias);
        return !base || route_signature(base) !== route_signature(r);
      });
      const toDelete = baselineRef.current.filter((r) => !currentAliases.has(r.alias));
      const receipt = await persist_route_changes({ to_save: toSave, to_delete: toDelete }, baselineRef.current, {
        save: saveRoute, remove: deleteRoute, read: listRoutes,
      });
      baselineRef.current = receipt.baseline;
      pending_route_readback.current = receipt.pending_readback;
      set_route_outcome_unknown(receipt.pending_readback !== null);
      // The draft stays visible, including every failed save/delete. A retry
      // compares against the successful subset and never repeats those writes.
      setRoutes((current) => [...current]);
      await refresh_proxy_config_status();
      if (receipt.pending_readback) {
        setError(t('route.outcomeUnknown'));
      } else if (receipt.failed_aliases.length > 0) {
        setError(t('route.savePartial', { saved: receipt.saved_count, failed: receipt.failed_aliases.length }));
      } else if (saved_apply_pending || receipt.persisted_after_rejection.length > 0) {
        baselineRef.current = routes;
        flash(t('route.savedApplyPending'));
      } else {
        baselineRef.current = routes;
        flash(t('route.saved'));
      }
    } catch (e) { setError(String(e)); }
    finally { route_operation_running.current = false; set_routes_saving(false); }
  };

  const handleDiscardRoutes = async () => {
    if (route_operation_running.current) return;
    route_operation_running.current = true;
    set_routes_saving(true);
    setPointerDrag(null);
    setRouteOverIdx(null);
    try {
      const fresh = await listRoutes();
      setRoutes(fresh);
      baselineRef.current = fresh;
      pending_route_readback.current = null;
      pending_rename_readback.current = null;
      set_route_outcome_unknown(false);
      setError(null);
    } catch {
      // Keep the last known routes; blanking here would baseline the editor
      // against an empty table and turn the next save into a mass delete.
      setError(t('route.discardFailed'));
    } finally {
      route_operation_running.current = false;
      set_routes_saving(false);
    }
  };

  /** Toggle a route's inclusion in the batch-delete set. */
  const toggleRouteSelection = (alias: string) => {
    if (route_operation_running.current) return;
    setSelectedAliases((prev) => {
      const next = new Set(prev);
      if (next.has(alias)) next.delete(alias);
      else next.add(alias);
      return next;
    });
  };

  /** Stage removal of every route in `selectedAliases` (persisted on Save). */
  const handleDeleteSelectedRoutes = () => {
    if (route_operation_running.current) return;
    const aliases = Array.from(selectedAliases);
    if (aliases.length === 0) return;
    if (!window.confirm(t('route.deleteSelectedConfirm', { n: String(aliases.length) }))) {
      return;
    }
    const doomed = new Set(aliases);
    setRoutes((prev) => prev.filter((r) => !doomed.has(r.alias)));
    setSelectedAliases(new Set());
  };

  /** Toggle a route's hop list visibility. */
  const toggleRouteCollapse = (alias: string) => {
    setCollapsedRoutes((prev) => {
      const next = new Set(prev);
      if (next.has(alias)) next.delete(alias);
      else next.add(alias);
      return next;
    });
  };

  /** Fetch upstream /v1/models from a provider and cache the ids.
   *  Works like cc-switch's ⤓ button in ModelInputWithFetch. */
  const handleFetchModels = async (providerId: string) => {
    if (fetchingId) return; // don't double-fire
    setFetchingId(providerId);
    try {
      const raw = await fetchProviderModels(providerId);
      const ids = raw.map((m) => m.id).sort();
      setFetchedModels((prev) => ({ ...prev, [providerId]: ids }));
      if (ids.length === 0) flash(t('route.fetchModels.empty', { provider: providerId }));
    } catch (e) {
      setError(t('route.fetchModels.failed', { message: String(e) }));
    } finally {
      setFetchingId(null);
    }
  };

  /** Model auto-discovery: probe upstream /v1/models and auto-apply newer
   *  same-family models (pool list + hop models + mapping values; aliases
   *  and mapping keys are never renamed). Proxy-id omitted = all eligible
   *  rows. Changes made while the proxy runs wait behind the restart flag. */
  const handleRefreshModels = async (proxyId?: string) => {
    if (busy !== null) return;
    setBusy(`refresh-models-${proxyId ?? 'all'}`);
    setError(null);
    setNotice(null);
    try {
      const report = await refreshProviderModels(proxyId);
      const added = report.providers.reduce((n, p) => n + p.addedModels.length, 0);
      const hops = report.providers.reduce((n, p) => n + p.hopRewrites.length, 0);
      const failed = report.providers.filter((p) => !p.ok);
      if (failed.length > 0 && failed.length === report.providers.length) {
        setError(failed.find((p) => p.error)?.error ?? 'supplier_discovery_failed');
      } else {
        flash(t('providers.refresh.done', { added: String(added), hops: String(hops) }));
      }
      await refresh();
      await refresh_discovery_status();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  };

  /** `proxy_set_auto_track` — per-provider discovery opt-out. */
  const handleToggleAutoTrack = async (proxyId: string, enabled: boolean) => {
    try {
      await setProviderAutoTrack(proxyId, enabled);
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  };

  /** Combined model suggestion list for the hop model datalist. The
   *  official Claude + Codex ids cover the gateway-side names; we
   *  also include each upstream provider's `models` list (the user
   *  might have populated those with model hints). Free-text is still
   *  accepted — datalist is a suggestion, not a constraint. */
  const knownModels = useMemo(() => {
    const set = new Set<string>();
    for (const m of CLAUDE_MODELS) set.add(m);
    for (const m of CODEX_MODELS) set.add(m);
    for (const pp of proxyProviders) {
      for (const m of pp.models ?? []) set.add(m);
    }
    for (const ms of Object.values(fetchedModels)) {
      for (const m of ms) set.add(m);
    }
    return Array.from(set);
  }, [proxyProviders, fetchedModels]);

  const routesDataValue = useMemo<RoutesDataState>(
    () => ({
      routes,
      isRouteDirty,
      proxy_running: running,
      restart_busy: busy === 'restart',
      routes_saving,
      outcome_unknown: route_outcome_unknown,
      filteredRoutes,
      selectedAliases,
      collapsedRoutes,
      knownModels,
      fetchedModels,
      fetchingId,
      proxyProviders: proxyProviders.map((pp) => ({
        id: pp.id,
        name: pp.name,
        enabled: pp.enabled,
        circuitBreaker: pp.circuitBreaker,
        modelMapping: pp.modelMapping,
      })),
    }),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [
      routes,
      isRouteDirty,
      running,
      busy,
      routes_saving,
      route_outcome_unknown,
      filteredRoutes,
      selectedAliases,
      collapsedRoutes,
      knownModels,
      fetchedModels,
      fetchingId,
      proxyProviders,
    ],
  );

  /** Narrow drag value: a pointermove only replaces this small object, so the
   *  shared routes data value above — and the provider <select> options it
   *  carries — keeps its identity during a drag. The drag classes live in the
   *  drag context; none of the handlers below freeze a callback. */
  const routes_drag_value = useMemo<RoutesDragState>(
    () => ({ routeOverIdx, pointerDrag }),
    [routeOverIdx, pointerDrag],
  );

  /** Test a proxy provider's connectivity. */
  const handleTestProvider = async (providerId: string) => {
    setTestingId(providerId);
    try {
      const result = await testProvider(providerId);
      setTestResults((prev) => ({ ...prev, [providerId]: result }));
    } catch (e) {
      setTestResults((prev) => ({
        ...prev,
        [providerId]: { success: false, error: String(e), latencyMs: 0, providerId },
      }));
    } finally {
      setTestingId(null);
    }
  };

  /** Clear a test result. */
  const clearTestResult = (providerId: string) => {
    setTestResults((prev) => {
      const next = { ...prev };
      delete next[providerId];
      return next;
    });
  };

  // Actions from the visible tree must never observe an uncommitted render.
  useLayoutEffect(() => {
    routes_actions_ref.current = {
      toggleRouteSelection,
      toggleRouteCollapse,
      handleDeleteSelectedRoutes,
      handleAddRoute,
      handleDeleteRoute,
      handleRenameRoute,
      handleAddHop,
      handleRemoveHop,
      handleEditHop,
      handleFetchModels,
      handleSaveRoutes,
      handleDiscardRoutes,
      hopRowAt,
      handlePointerDown,
      handlePointerMove,
      handlePointerUp,
      handlePointerCancel,
      handleHopKeyDown,
      onRestartProxy: handleRestartProxy,
    };

    providers_actions_ref.current = {
      onSwitch: handleSwitch,
      onToggleQueue: handleToggleQueue,
      onToggleEnabled: handleToggleEnabled,
      onTogglePin: handleTogglePin,
      onTestProvider: handleTestProvider,
      onClearTestResult: clearTestResult,
      onRefreshModels: handleRefreshModels,
      onToggleAutoTrack: handleToggleAutoTrack,
      onAdd: () => openProviderModal(),
      onAddStandalone: () => openProviderModal({ standalone: true }),
      onAddFromTemplate: (templateId) => {
        const template = PROVIDER_PRESET_TEMPLATES.find((x) => x.id === templateId);
        if (template) openProviderModalFromTemplate(template);
      },
      onEditCard: (card) => openProviderModal(card),
      onDeleteCard: handleDeleteCard,
      onReorder: handleReorderCards,
      onRefresh: () => refresh(),
    };
  });

  const routesActionsValue = useMemo<RoutesActionsState>(
    () => ({
      toggleRouteSelection: (alias) => routes_actions_ref.current!.toggleRouteSelection(alias),
      toggleRouteCollapse: (alias) => routes_actions_ref.current!.toggleRouteCollapse(alias),
      handleDeleteSelectedRoutes: () => routes_actions_ref.current!.handleDeleteSelectedRoutes(),
      handleAddRoute: () => routes_actions_ref.current!.handleAddRoute(),
      handleDeleteRoute: (alias) => routes_actions_ref.current!.handleDeleteRoute(alias),
      handleRenameRoute: (alias) => routes_actions_ref.current!.handleRenameRoute(alias),
      handleAddHop: (alias) => routes_actions_ref.current!.handleAddHop(alias),
      handleRemoveHop: (alias, hopIdx) => routes_actions_ref.current!.handleRemoveHop(alias, hopIdx),
      handleEditHop: (alias, hopIdx, field, value) =>
        routes_actions_ref.current!.handleEditHop(alias, hopIdx, field, value),
      handleFetchModels: (providerId) => routes_actions_ref.current!.handleFetchModels(providerId),
      handleSaveRoutes: () => routes_actions_ref.current!.handleSaveRoutes(),
      handleDiscardRoutes: () => routes_actions_ref.current!.handleDiscardRoutes(),
      hopRowAt: (x, y) => routes_actions_ref.current!.hopRowAt(x, y),
      handlePointerDown: (alias, fromIdx, e) => routes_actions_ref.current!.handlePointerDown(alias, fromIdx, e),
      handlePointerMove: (e) => routes_actions_ref.current!.handlePointerMove(e),
      handlePointerUp: (e) => routes_actions_ref.current!.handlePointerUp(e),
      handlePointerCancel: (e) => routes_actions_ref.current!.handlePointerCancel(e),
      handleHopKeyDown: (alias, idx, e, hopCount) =>
        routes_actions_ref.current!.handleHopKeyDown(alias, idx, e, hopCount),
      onRestartProxy: () => routes_actions_ref.current!.onRestartProxy(),
    }),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [],
  );

  const providersActionsValue = useMemo<ProvidersActionsState>(
    () => ({
      onSwitch: (id) => providers_actions_ref.current!.onSwitch(id),
      onToggleQueue: (card) => providers_actions_ref.current!.onToggleQueue(card),
      onToggleEnabled: (id, enabled) => providers_actions_ref.current!.onToggleEnabled(id, enabled),
      onTogglePin: (id) => providers_actions_ref.current!.onTogglePin(id),
      onTestProvider: (id) => providers_actions_ref.current!.onTestProvider(id),
      onClearTestResult: (id) => providers_actions_ref.current!.onClearTestResult(id),
      onRefreshModels: (proxyId) => providers_actions_ref.current!.onRefreshModels(proxyId),
      onToggleAutoTrack: (id, enabled) => providers_actions_ref.current!.onToggleAutoTrack(id, enabled),
      onAdd: () => providers_actions_ref.current!.onAdd(),
      onAddStandalone: () => providers_actions_ref.current!.onAddStandalone(),
      onAddFromTemplate: (id) => providers_actions_ref.current!.onAddFromTemplate(id),
      onEditCard: (card) => providers_actions_ref.current!.onEditCard(card),
      onDeleteCard: (card) => providers_actions_ref.current!.onDeleteCard(card),
      onReorder: (from, to) => providers_actions_ref.current!.onReorder(from, to),
      onRefresh: () => providers_actions_ref.current!.onRefresh(),
    }),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [],
  );

  /** Unified card list — presets (chain order) + linked `ccs-*`
   *  upstreams; standalone pool rows only on the routing pages, where
   *  routes reference them. */
  const cards = useMemo(
    () =>
      mergeProviderCards(providers, proxyProviders, {
        includeStandalone: appType === 'claude_code' || appType === 'codex',
        supplierMappedIds: supplier_managed_ids,
      }),
    [providers, proxyProviders, appType, supplier_managed_ids],
  );

  /** Card-panel data slice — cards / current / pins / health / test
   *  results. (Memoized after its handler dependencies — see the
   *  temporal-dead-zone note above.) */
  const providersDataValue = useMemo<ProvidersDataState>(
    () => ({
      cards,
      currentId: current,
      pinnedId,
      health,
      testResults,
      testingId,
      running,
      busy,
      refreshingModels: busy?.startsWith('refresh-models-')
        ? busy.slice('refresh-models-'.length)
        : null,
      cfg,
      pool_supported: appType === 'claude_code' || appType === 'codex',
      breakerOf,
      breakerOfClass,
    }),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [cards, current, pinnedId, health, testResults, testingId, running, busy, cfg],
  );

  const gateway_baseline_ref = useRef<typeof gatewayForm | null>(null);
  useLayoutEffect(() => {
    if (!showProviderModal) provider_baseline_ref.current = null;
    else if (!provider_baseline_ref.current) provider_baseline_ref.current = { ...providerForm, values: { ...providerForm.values } };
    if (!showGatewayModal) gateway_baseline_ref.current = null;
    else if (!gateway_baseline_ref.current) gateway_baseline_ref.current = { ...gatewayForm };
  }, [showProviderModal, providerForm, showGatewayModal, gatewayForm]);
  const creating = !editingCard || (!editingCard.preset && !editingCard.upstream);
  const provider_dirty = creating || Boolean(provider_baseline_ref.current && JSON.stringify(provider_baseline_ref.current) !== JSON.stringify(providerForm));
  const gateway_dirty = Boolean(gateway_baseline_ref.current && JSON.stringify(gateway_baseline_ref.current) !== JSON.stringify(gatewayForm));
  use_page_actions(showProviderModal ? {
    target: editingCard?.preset?.name || editingCard?.upstream?.name || 'New client connection',
    state: provider_saving || busy !== null ? 'busy' : provider_modal_error ? 'error' : provider_dirty ? 'dirty' : 'clean',
    on_save: handleSaveCard,
    on_delete: !creating && !editingCard?.supplierManaged ? async () => {
      const outcome = await handleDeleteCard(editingCard!);
      if (outcome === 'deleted') setShowProviderModal(false);
      else if (outcome === 'failed') throw new Error('Client connection removal failed');
    } : undefined,
    save_disabled: !provider_dirty || !providerForm.name.trim() || provider_saving || busy !== null,
    delete_disabled: provider_saving || busy !== null,
  } : showGatewayModal ? {
    target: 'Gateway settings', state: busy ? 'busy' : error ? 'error' : gateway_dirty ? 'dirty' : 'clean',
    on_save: handleSaveGateway, save_disabled: busy !== null || !gateway_dirty,
  } : advancedRoutesOpen && (appType === 'claude_code' || appType === 'codex' || appType === 'gemini') ? {
    target: `${cfg.label} routes`, state: routes_saving ? 'busy' : error ? 'error' : isRouteDirty ? 'dirty' : 'clean',
    on_save: async () => { setError(null); await handleSaveRoutes(); },
    on_delete: selectedAliases.size ? handleDeleteSelectedRoutes : undefined,
    save_disabled: routes_saving || (!isRouteDirty && !route_outcome_unknown), delete_disabled: routes_saving || !selectedAliases.size,
  } : null, showProviderModal || showGatewayModal ? 20 : 5);

  return (
    <section className="page">
      <div className="page-header">
        <div>
          <h2>{cfg.label}</h2>
          <p className="muted">{t(surfaceKey)}</p>
        </div>
        <div className="header-actions">
          {/* Round 1: Start/Stop moved into the gateway card head (see below)
           * so the gateway panel becomes a self-contained control surface.
           * Only the "Import from cc-switch" action remains in the page
           * header — it's not about the live gateway. */}
          {cfg.ccSwitchUpstream && (
            <button className="btn" onClick={handleImport} disabled={importing}>
              {importing ? '…' : t('providers.importCcSwitch')}
            </button>
          )}
        </div>
      </div>

      {error && <div className="error-banner">{backend_error_text(error, t)}</div>}
      {notice && <div className="notice-banner">{notice}</div>}
      <Supplier_discovery_report report={discovery_status?.report} providers={proxyProviders} />

      {/* Model auto-discovery changed routing that is not yet in the live
       * snapshot (hot apply failed or the gateway was not running). */}
      {discovery_status?.pendingRestart && (
        <div className="notice-banner" data-testid="discovery-restart-pending">
          <span>{t('gateway.restartPending')}</span>
          <button
            className="btn small"
            disabled={busy !== null}
            onClick={handleRestartProxy}
            data-testid="discovery-restart-now"
          >
            {busy === 'restart' ? '…' : t('route.restartNow')}
          </button>
        </div>
      )}

      <GatewayPanel
        cfg={cfg}
        running={running}
        config_changed={config_changed}
        hotReloadError={hotReloadError}
        gatewayUrl={gatewayUrl}
        authToken={authToken}
        cliStatus={cliStatus}
        cliStatusError={cliStatusError}
        claudeStatus={claudeStatus}
        claudeStatusError={claudeStatusError}
        copied={copied}
        busy={busy}
        cotOn={cotOn}
        onToggleCot={handleToggleCot}
        mirrorMode={cliStatus?.mirrorMode ?? 'exclusive'}
        onToggleMirrorMode={handleToggleMirrorMode}
        onCopy={copy}
        onStart={handleStart}
        onStop={handleStop}
        onEditSettings={openGatewayModal}
        on_restart={handleRestartProxy}
      />

      <Client_config_status app_type={appType} refresh_version={config_refresh_version} />
      {cfg.showGatewayCard && <Live_request_status key={appType} surface={cfg.surface} refresh_version={config_refresh_version} />}

      {/* Unified provider cards — the single failover-chain surface
       * (per-app presets + linked `ccs-*` upstreams merged; standalone
       * pool rows render after a divider on routing pages). Card order =
       * failover priority, persisted via provider_reorder. */}
      <ProvidersDataProvider value={providersDataValue}>
        <ProvidersActionsProvider value={providersActionsValue}>
          <ProvidersPanel />
        </ProvidersActionsProvider>
      </ProvidersDataProvider>

      {/* Advanced per-alias routing — collapsed by default; the card
       * order above is the primary chain. Consumes RoutesContext (state +
       * handlers owned by AppPage so the tab-switch cache refs survive
       * remounts). */}
      {(appType === 'claude_code' || appType === 'codex' || appType === 'gemini') && (
        <RoutesDataProvider value={routesDataValue}>
          <RoutesDragProvider value={routes_drag_value}>
            <RoutesActionsProvider value={routesActionsValue}>
              <AdvancedRoutes
                open={advancedRoutesOpen}
                dirty={isRouteDirty}
                onToggle={() =>
                  setAdvancedRoutesOpen((prev) => {
                    const next = !prev;
                    advancedRoutesOpenByApp.set(appType, next);
                    return next;
                  })
                }
              />
              <Route_explanation
                pinned_provider_id={pinnedId}
                current_provider_id={health?.lastUsed ?? null}
              />
            </RoutesActionsProvider>
          </RoutesDragProvider>
        </RoutesDataProvider>
      )}

      <Launchers
        cfg={cfg}
        busy={busy}
        codex_desktop_supported={cliStatus?.desktopSupported ?? null}
        claude_desktop_supported={claudeStatus?.desktopSupported ?? null}
        onOpenDesktop={handleOpenDesktop}
        onOpenCli={handleOpenCli}
        onOpenCopilot={handleOpenCopilot}
        onOpenGemini={handleOpenGemini}
        onOpenVscode={handleOpenVscode}
        onOpenClaudeDesktop={handleOpenClaudeDesktop}
        on_open_claude_cli={handle_open_claude_cli}
        managed_cli={managed_cli?.appType === appType ? managed_cli : null}
        managed_cli_error={managed_cli_error?.appType === appType ? managed_cli_error.message : null}
        on_retry_managed_cli_status={() => {
          if (appType !== 'kilo_cli' && appType !== 'cline_cli') return;
          const request_id = ++managed_cli_request_id_ref.current;
          set_managed_cli_error(null);
          managed_cli_status(appType).then((status) => {
            if (request_id !== managed_cli_request_id_ref.current) return;
            set_managed_cli(status);
            set_managed_cli_error(null);
          }).catch((e) => {
            if (request_id === managed_cli_request_id_ref.current) {
              set_managed_cli_error({ appType, message: String(e) });
            }
          });
        }}
        on_open_managed_cli={handle_open_managed_cli}
      />

      {/* Unified provider modal (identity + connection + upstream) */}
      {showProviderModal && (
        <ProviderModal
          cfg={cfg}
          editing={editingCard}
          form={providerForm}
          setForm={setProviderForm}
          onSave={handleSaveCard}
          onClose={() => setShowProviderModal(false)}
          saving={provider_saving}
          deleting={Boolean(busy?.startsWith('delete-'))}
          error={provider_modal_error}
        />
      )}

      {/* Gateway settings modal (port + auth token) */}
      {showGatewayModal && (
        <div className="modal-backdrop" onClick={() => setShowGatewayModal(false)}>
          <div className="modal" role="dialog" aria-modal="true" aria-label={t('app.gateway.editSettings')} onClick={(e) => e.stopPropagation()}>
            <h3>{t('app.gateway.editSettings')}</h3>
            <label>
              {t('app.gateway.hostLabel')}
              <input
                className="input"
                value={gatewayForm.host}
                placeholder="127.0.0.1"
                onChange={(e) => setGatewayForm({ ...gatewayForm, host: e.target.value })}
              />
            </label>
            <label>
              {t('app.gateway.portLabel')}
              <input
                className="input"
                type="number"
                min={1}
                max={65535}
                value={gatewayForm.port}
                onChange={(e) => setGatewayForm({ ...gatewayForm, port: e.target.value })}
              />
            </label>
            <label>
              {t('app.gateway.apiKey')}
              <input
                className="input"
                type="password"
                value={gatewayForm.auth_token}
                placeholder="(empty = accept any key)"
                onChange={(e) => setGatewayForm({ ...gatewayForm, auth_token: e.target.value })}
              />
            </label>
            <p className="muted small">{t('app.gateway.savedRestartHint')}</p>
            <div className="modal-actions">
              <button className="btn" onClick={() => setShowGatewayModal(false)}>
                {t('common.cancel')}
              </button>
              <button className="btn primary" onClick={handleSaveGateway}>
                {t('common.save')}
              </button>
            </div>
          </div>
        </div>
      )}
    </section>
  );
}
