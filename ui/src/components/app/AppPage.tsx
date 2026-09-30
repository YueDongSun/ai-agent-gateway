import { useEffect, useMemo, useRef, useState } from 'react';
import {
  listProviders,
  getCurrentProvider,
  saveProvider,
  deleteProvider,
  switchProvider,
  reorderProviders,
  setProviderFailoverQueue,
} from '../../lib/api/providers';
import {
  getProxyConfig,
  getProxyStatus,
  startProxy,
  stopProxy,
  listProxyProviders,
  saveProxyProvider,
  deleteProxyProvider,
  getGatewayHealth,
  fetchProxyHealth,
  getPinnedProvider,
  setPinnedProvider,
  getResponsesCot,
  setResponsesCot,
  testProvider,
  listRoutes,
  saveRoute,
  deleteRoute,
  renameRoute,
  saveProxyConfig,
  fetchProviderModels,
  type ProxyProvider,
  type CircuitBreakerConfig,
  type ProxyHealth,
  type TestResult,
  type Route,
  type ProxyConfig,
} from '../../lib/api/proxy';
import { importCcSwitchProviders } from '../../lib/api/import';
import {
  codexCliStatus,
  codexMirrorSetMode,
  codexOpenCliTerminal,
  codexOpenDesktopWindow,
  type CodexCliStatus,
} from '../../lib/api/codexCli';
import {
  claudeMirrorSetMode,
  claudeMirrorStatus,
  claudeOpenDesktop,
  open_claude_cli_terminal,
  type ClaudeMirrorStatus,
} from '../../lib/api/claudeMirror';
import { copilotOpenCliTerminal } from '../../lib/api/copilot';
import { vscodeOpenWithExtension } from '../../lib/api/vscode';
import { APP_PAGES, CLAUDE_MODELS, CODEX_MODELS, type AppPageConfig } from '../../lib/appPages';
import type { AppType, Provider } from '../../lib/api/types';
import {
  defaultUpstreamTypeFor,
  mergeProviderCards,
  slugifyProviderId,
  upstreamIdOf,
  type UnifiedCard,
} from '../../lib/unified_providers.js';
import { useT } from '../../lib/ctx';
import { Launchers } from './Launchers';
import { Client_config_status } from './client_config_status';
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
import {
  RoutesActionsProvider,
  RoutesDataProvider,
  type RoutesActionsState,
  type RoutesDataState,
} from './routes_state.js';

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
  };
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
interface App_page_props {
  appType: AppType;
  on_gateway_status_refresh?: () => Promise<void>;
}

export function AppPage({ appType, on_gateway_status_refresh }: App_page_props) {
  const t = useT();
  const cfg = APP_PAGES[appType];
  const [providers, setProviders] = useState<Provider[]>([]);
  const [current, setCurrent] = useState<string | null>(null);
  const [running, setRunning] = useState(false);
  const [gatewayUrl, setGatewayUrl] = useState('');
  const [authToken, setAuthToken] = useState('');
  const [cliStatus, setCliStatus] = useState<CodexCliStatus | null>(null);
  /** Last error from `codexCliStatus` (null = never failed or
   *  recovered). The mirror row renders a warning chip when set. */
  const [cliStatusError, setCliStatusError] = useState<string | null>(null);
  const [claudeStatus, setClaudeStatus] = useState<ClaudeMirrorStatus | null>(null);
  /** Last error from `claudeMirrorStatus` — mirrors cliStatusError. */
  const [claudeStatusError, setClaudeStatusError] = useState<string | null>(null);
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
  /** Live health data (breaker states) from the proxy /health endpoint. */
  const [health, setHealth] = useState<ProxyHealth | null>(null);
  /** Currently pinned provider id (force-pin mode). null = normal failover. */
  const [pinnedId, setPinnedId] = useState<string | null>(null);
  /** Test results per provider id. */
  const [testResults, setTestResults] = useState<Record<string, TestResult>>({});
  /** Which provider is currently being tested. */
  const [testingId, setTestingId] = useState<string | null>(null);
  /** Failover routes (alias → ordered hops). Used on Claude + Codex pages. */
  const [routes, setRoutes] = useState<Route[]>([]);
  /** Baseline routes from last save/load — used to detect unsaved changes. */
  const baselineRef = useRef<Route[]>([]);
  /** Per-app-type cache of unsaved route edits. Survives tab switches. */
  const routeCacheRef = useRef<Record<string, Route[]>>({});
  /** Whether routes have unsaved edits. */
  const isRouteDirty = useMemo(
    () => JSON.stringify(routes) !== JSON.stringify(baselineRef.current),
    [routes],
  );
  /** Gateway settings modal (bind host + port + auth token). */
  const [showGatewayModal, setShowGatewayModal] = useState(false);
  const [gatewayForm, setGatewayForm] = useState({ host: '', port: '', authToken: '' });
  /** Built-in chain-of-thought prompt toggle for /v1/responses (Codex
   *  surface). Live: flipping it updates the running proxy immediately. */
  const [cotOn, setCotOn] = useState(false);
  /** Advanced-routes section expanded (per app; see module map above). */
  const [advancedRoutesOpen, setAdvancedRoutesOpen] = useState(
    () => advancedRoutesOpenByApp.get(appType) ?? false,
  );
  /** Route editor: aliases selected for batch delete. */
  const [selectedAliases, setSelectedAliases] = useState<Set<string>>(new Set());
  /** Route editor: aliases whose hop list is collapsed. */
  const [collapsedRoutes, setCollapsedRoutes] = useState<Set<string>>(new Set());
  /** Cached upstream model ids fetched via ⤓ per provider id. */
  const [fetchedModels, setFetchedModels] = useState<Record<string, string[]>>({});
  /** Which provider is currently being fetched. */
  const [fetchingId, setFetchingId] = useState<string | null>(null);

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

  const refresh = async () => {
    refresh_client_config();
    try {
      const [list, cur, pc, st] = await Promise.all([
        listProviders(appType),
        getCurrentProvider(appType),
        getProxyConfig(),
        getProxyStatus(),
      ]);
      setProviders(list);
      setCurrent(cur);
      setGatewayUrl(`http://${pc.host}:${pc.port}`);
      setAuthToken(pc.authToken);
      setRunning(st.running);
      // The upstream pool is global — load it on every page so linked
      // `ccs-*` rows render on the unified cards wherever their preset
      // lives.
      try {
        const [pp, pin, cot] = await Promise.all([
          listProxyProviders(),
          getPinnedProvider(),
          getResponsesCot(),
        ]);
        setProxyProviders(pp);
        setPinnedId(pin);
        setCotOn(cot);
      } catch { /* optional */ }
      // Health (breaker states) + routes are routing-page concerns:
      // Claude, Codex, and Gemini.
      if (appType === 'claude_code' || appType === 'codex' || appType === 'gemini') {
        if (st.running) {
          try { setHealth(await fetchProxyHealth(pc)); } catch { setHealth(null); }
        }
        // If we have cached unsaved edits for this app type, restore them
        // instead of overwriting from the backend.
        const cached = routeCacheRef.current[appType];
        if (cached) {
          setRoutes(cached);
        } else {
          try {
            const loaded = await listRoutes();
            setRoutes(loaded);
            baselineRef.current = loaded;
          } catch { setRoutes([]); baselineRef.current = []; }
        }
      }
      setError(null);
      void on_gateway_status_refresh?.();
    } catch (e) {
      setError(String(e));
    }
  };

  /** Track previous appType for route caching on tab switch. */
  const prevAppRef = useRef(appType);

  useEffect(() => {
    // Cache routes from the previous app type before refreshing.
    if (prevAppRef.current !== appType) {
      if (routes.length > 0 || isRouteDirty) {
        routeCacheRef.current[prevAppRef.current] = routes;
      }
      prevAppRef.current = appType;
      setAdvancedRoutesOpen(advancedRoutesOpenByApp.get(appType) ?? false);
    }
    void refresh();
    if (cfg.launchers === 'codex') {
      codexCliStatus()
        .then((s) => {
          setCliStatus(s);
          setCliStatusError(null);
        })
        .catch((e) => {
          setCliStatus(null);
          setCliStatusError(String(e));
        });
    }
    if (cfg.launchers === 'claude') {
      claudeMirrorStatus()
        .then((s) => {
          setClaudeStatus(s);
          setClaudeStatusError(null);
        })
        .catch((e) => {
          setClaudeStatus(null);
          setClaudeStatusError(String(e));
        });
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [appType]);

  /** Poll proxy /health while the gateway runs so breaker states in the
   *  upstream pool stay live. Only for pages that show the pool
   *  (Claude + Codex), and paused when the tab is hidden. */
  useEffect(() => {
    const pollsHealth =
      (appType === 'claude_code' || appType === 'codex' || appType === 'gemini') && running;
    if (!pollsHealth) return;
    let alive = true;
    const tick = async () => {
      try {
        const h = await getGatewayHealth();
        if (alive) setHealth(h);
      } catch {
        if (alive) setHealth(null);
      }
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
      alive = false;
      stop();
      document.removeEventListener('visibilitychange', onVis);
    };
  }, [appType, running]);

  /** Mirror CODEX_HOME row staleness fix (Phase 22u): refresh the
   *  Codex CLI status every 10 s on the Codex page so the row tracks
   *  settings changes without a tab switch. Paused when hidden. */
  useEffect(() => {
    if (cfg.launchers !== 'codex') return;
    const tick = () => {
      codexCliStatus()
        .then((s) => {
          setCliStatus(s);
          setCliStatusError(null);
        })
        .catch((e) => {
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
      stop();
      document.removeEventListener('visibilitychange', onVis);
    };
  }, [cfg.launchers]);

  /** Same 10 s staleness fix for the Claude mirror status row. */
  useEffect(() => {
    if (cfg.launchers !== 'claude') return;
    const tick = () => {
      claudeMirrorStatus()
        .then((s) => {
          setClaudeStatus(s);
          setClaudeStatusError(null);
        })
        .catch((e) => {
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
      stop();
      document.removeEventListener('visibilitychange', onVis);
    };
  }, [cfg.launchers]);

  /** Memoized pool value is defined further down, after its handler
   *  dependencies (breakerOf / breakerOfClass / handleTestProvider /
   *  clearTestResult) — a `const` useMemo above those declarations hits
   *  the temporal dead zone at first render (white-screen crash). */

  const copy = async (text: string, key: string) => {
    try {
      await navigator.clipboard.writeText(text);
      setCopied(key);
      setTimeout(() => setCopied(null), 1500);
    } catch {
      /* clipboard unavailable */
    }
  };

  const handleStart = async () => {
    setBusy('start');
    try {
      const msg = await startProxy();
      // Surface the backend's port-fallback note ("port 8989 was in use;
      // saved as the new default") — the gateway URL row updates too, but
      // the toast explains why it changed.
      const fallback = msg.match(/port (\d+) was in use/);
      if (fallback) {
        flash(t('app.gateway.portFallback', { port: fallback[1] }));
      }
      await refresh();
    } catch (e) {
      const raw = String(e);
      setError(
        raw.includes('proxy_bind_exhausted') || raw.includes('no free port')
          ? t('app.gateway.portConflict', { message: raw })
          : raw,
      );
    } finally {
      setBusy(null);
    }
  };

  /** Stop + start the local proxy (routes/providers only reload at start). */
  const handleRestartProxy = async () => {
    setBusy('restart');
    try {
      try { await stopProxy(); } catch { /* wasn't running */ }
      await startProxy();
      await refresh();
      flash(t('app.gateway.restarted'));
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  };

  const handleStop = async () => {
    setBusy('stop');
    try {
      await stopProxy();
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  };

  // ---- Gateway settings (bind host + port + auth token) ----

  const openGatewayModal = async () => {
    // The save handler re-reads via getProxyConfig() instead of threading
    // the snapshot through a ref — StrictMode / remount can't invalidate
    // a single await chain.
    setShowGatewayModal(true);
    try {
      const cfg = await getProxyConfig();
      setGatewayForm({ host: cfg.host, port: String(cfg.port), authToken: cfg.authToken });
    } catch (e) {
      setError(String(e));
    }
  };

  const handleSaveGateway = async () => {
    // Re-fetch the baseline — any concurrent edit (auto-start, settings)
    // would otherwise be silently overwritten.
    let base: ProxyConfig;
    try {
      base = await getProxyConfig();
    } catch (e) {
      setError(String(e));
      return;
    }
    const port = Number(gatewayForm.port);
    if (!Number.isInteger(port) || port < 1 || port > 65535) {
      setError(t('app.gateway.invalidPort'));
      return;
    }
    // Backend parses host:port with .expect() — a bad host would panic
    // proxy start, so validate here. `new URL` round-trips through WHATWG
    // parsing which accepts localhost, IPv4 dotted-quads, and IPv6
    // literals (with or without brackets); rejecting anything else.
    const h = gatewayForm.host.trim();
    try {
      // eslint-disable-next-line no-new
      new URL(`http://${h}:${port}`);
    } catch {
      setError(t('app.gateway.invalidHost'));
      return;
    }
    try {
      await saveProxyConfig({ ...base, host: h, port, authToken: gatewayForm.authToken.trim() });
      setShowGatewayModal(false);
      flash(t('app.gateway.savedRestartHint'));
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  };

  const handleImport = async () => {
    setImporting(true);
    setError(null);
    try {
      const report = await importCcSwitchProviders();
      const noKeyCount = report.skippedNoKey?.length ?? 0;
      if (report.imported.length === 0 && report.skippedExisting.length === 0 && noKeyCount === 0) {
        flash(t('providers.importCcSwitch.empty'));
      } else {
        // importedRoutes may be empty for non-Claude imports — only
        // include the suffix when cc-switch actually wrote something
        // useful (i.e. ANTHROPIC_DEFAULT_* mapping landed).
        const routes = report.importedRoutes ?? [];
        const routeCount = new Set(routes.map((r) => r.alias)).size;
        const queuedCount = report.queuedProviders?.length ?? 0;
        const passthroughCount = report.passthroughProviders?.length ?? 0;
        // The router snapshot is immutable between restarts: when the
        // gateway is running and the import actually changed something,
        // restart it in place so the new queue/routes take effect without
        // a manual stop/start.
        let restarted = false;
        if (running && report.imported.length > 0) {
          try { await stopProxy(); } catch { /* wasn't running */ }
          await startProxy();
          restarted = true;
        }
        flash(
          t('providers.importCcSwitch.report', {
            imported: String(report.imported.length),
            skipped: String(report.skippedExisting.length),
            total: String(report.totalInSource),
          }) +
            (routeCount > 0 ? ` · ${t('providers.importCcSwitch.routes', { n: String(routeCount) })}` : '') +
            (noKeyCount > 0 ? ` · ${t('providers.importCcSwitch.noKey', { n: String(noKeyCount) })}` : '') +
            (queuedCount > 0 ? ` · ${t('providers.importCcSwitch.queued', { n: String(queuedCount) })}` : '') +
            (passthroughCount > 0 ? ` · ${t('providers.importCcSwitch.passthrough', { n: String(passthroughCount) })}` : '') +
            (restarted ? ` · ${t('providers.importCcSwitch.restarted')}` : ''),
        );
      }
      await refresh();
    } catch (e) {
      setError(t('providers.importCcSwitch.failed', { message: String(e) }));
    } finally {
      setImporting(false);
    }
  };

  // ---- Unified provider cards (presets + linked upstreams) ----

  /** Open the unified modal: `card` omitted = create a new preset card. */
  const openProviderModal = (card?: UnifiedCard) => {
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
      });
      setEditingCard(card);
    } else if (card?.upstream) {
      // Standalone upstream card: identity + upstream section only.
      const upstream = card.upstream;
      setProviderForm({
        id: upstream.id,
        name: upstream.name,
        values: {},
        icon: '',
        iconColor: '',
        websiteUrl: '',
        notes: '',
        upstreamType: upstream.type,
        upstreamBaseURL: upstream.baseURL,
        upstreamAPIKey: upstream.apiKey,
        upstreamModels: (upstream.models ?? []).join(', '),
        upstreamModelMapping: formatModelMapping(upstream.modelMapping),
        upstreamExtraHeaders: formatHeaderJson(upstream.extraHeaders),
      });
      setEditingCard(card);
    } else {
      setProviderForm(emptyProviderForm(cfg, gatewayUrl, appType));
      setEditingCard(null);
    }
    setShowProviderModal(true);
  };

  /** Save the unified modal. Standalone cards write the pool row directly;
   *  preset cards write the providers row, then sync the linked `ccs-<id>`
   *  upstream (creating it when the connection fields carry a usable base
   *  URL; skipped for empty or self-loop URLs, mirroring the importer's
   *  self-loop guard). */
  const handleSaveCard = async () => {
    // Optional per-upstream model mapping (JSON textarea): empty keeps the
    // stored mapping untouched; invalid JSON blocks the save.
    let parsedModelMapping: Record<string, string> | undefined;
    if (providerForm.upstreamModelMapping.trim()) {
      try {
        parsedModelMapping = JSON.parse(
          providerForm.upstreamModelMapping,
        ) as Record<string, string>;
      } catch {
        setError(t('providers.modal.modelMappingInvalid'));
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
        setError(t('providers.modal.extraHeadersInvalid'));
        return;
      }
    }
    try {
      if (editingCard?.kind === 'standalone') {
        const existing = editingCard.upstream;
        const id = providerForm.id.trim() || slugifyProviderId(providerForm.name);
        await saveProxyProvider({
          id,
          name: providerForm.name.trim() || id,
          type: providerForm.upstreamType,
          apiKey: providerForm.upstreamAPIKey.trim(),
          baseURL: providerForm.upstreamBaseURL.trim(),
          models: parseModelList(providerForm.upstreamModels),
          // Preserve the row's fields the modal does not expose — editing
          // must not silently wipe enabled/breaker state.
          enabled: existing?.enabled ?? true,
          timeoutMs: existing?.timeoutMs ?? 60000,
          weight: existing?.weight ?? 1,
          circuitBreaker: existing?.circuitBreaker ?? DEFAULT_CIRCUIT_BREAKER,
          modelMapping: parsedModelMapping ?? existing?.modelMapping ?? {},
          extraHeaders: parsedExtraHeaders ?? existing?.extraHeaders ?? {},
        });
      } else {
        const id = providerForm.id.trim() || slugifyProviderId(providerForm.name);
        const builtConfig = cfg.buildConfig(id, providerForm.values);
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
          });
        }
      }
      setShowProviderModal(false);
      flash(t('providers.saved'));
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  };

  /** Delete a unified card: both sides of a linked card go in one confirm. */
  const handleDeleteCard = async (card: UnifiedCard) => {
    const label = card.preset?.name || card.upstream?.name || card.key;
    const confirmMsg =
      card.preset && card.upstream
        ? t('providers.delete.confirmLinked', { id: label, upstream: card.upstream.id })
        : t('providers.delete.confirm', { id: label });
    if (!window.confirm(confirmMsg)) return;
    try {
      if (card.preset) await deleteProvider(card.preset.id, appType);
      if (card.upstream) await deleteProxyProvider(card.upstream.id);
      flash(t('providers.deleted'));
      await refresh();
    } catch (e) {
      setError(String(e));
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
    setBusy('desktop');
    try {
      const r = await codexOpenDesktopWindow();
      refresh_client_config();
      flash(t('codex.open.desktopLaunched', { dir: r.codexHome }));
    } catch (e) {
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

  const handleOpenCopilot = async () => {
    setBusy('copilot');
    try {
      const r = await copilotOpenCliTerminal();
      refresh_client_config();
      flash(t('codex.open.copilotLaunched', { provider: r.providerId }));
    } catch (e) {
      setError(t('codex.open.failed', { message: String(e) }));
    } finally {
      setBusy(null);
    }
  };

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

  const handleTogglePin = async (id: string) => {
    const next = pinnedId === id ? '' : id;
    try {
      await setPinnedProvider(next);
      setPinnedId(next || null);
      flash(next ? t('providers.pinned', { name: id }) : t('providers.unpinned'));
    } catch (e) { setError(String(e)); }
  };

  /** Flip the /v1/responses chain-of-thought prompt toggle. Optimistic —
   *  the backend applies it to the running proxy immediately; roll back
   *  locally only when the command fails. */
  const handleToggleCot = async (next: boolean) => {
    setCotOn(next);
    try {
      await setResponsesCot(next);
    } catch (e) {
      setCotOn(!next);
      setError(String(e));
      return;
    }
    flash(next ? t('app.gateway.cot.onFlash') : t('app.gateway.cot.offFlash'));
  };

  /** Flip the Codex mirror mode (exclusive ⇄ coexist). Optimistic — the
   *  backend persists the setting and rewrites the mirror's config.toml
   *  immediately; roll back locally only when the command fails. */
  const handleToggleMirrorMode = async (coexist: boolean) => {
    const prev = cliStatus;
    const nextMode = coexist ? 'coexist' : 'exclusive';
    setCliStatus((s) => (s ? { ...s, mirrorMode: nextMode } : s));
    try {
      await codexMirrorSetMode(nextMode);
    } catch (e) {
      setCliStatus(prev);
      setError(String(e));
      return;
    }
    flash(
      coexist
        ? t('codex.mirror.mode.coexistFlash')
        : t('codex.mirror.mode.exclusiveFlash'),
    );
  };

  /** Flip the Claude mirror mode (token ⇄ passthrough). Optimistic — the
   *  backend persists the setting and rewrites the mirror's settings.json
   *  immediately; roll back locally only when the command fails. */
  const handleToggleClaudeMode = async (passthrough: boolean) => {
    const prev = claudeStatus;
    const nextMode = passthrough ? 'passthrough' : 'token';
    setClaudeStatus((s) => (s ? { ...s, mode: nextMode } : s));
    try {
      await claudeMirrorSetMode(nextMode);
    } catch (e) {
      setClaudeStatus(prev);
      setError(String(e));
      return;
    }
    flash(
      passthrough
        ? t('claude.mirror.mode.passthroughFlash')
        : t('claude.mirror.mode.tokenFlash'),
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
    const alias = window.prompt(t('route.add') + ' — alias (e.g. claude-sonnet-5):');
    const id = alias?.trim();
    if (!id) return;
    if (routes.some((r) => r.alias === id)) {
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
    if (!window.confirm(t('route.delete.confirm', { alias }))) return;
    setRoutes((prev) => prev.filter((r) => r.alias !== alias));
    setSelectedAliases((prev) => {
      const next = new Set(prev);
      next.delete(alias);
      return next;
    });
  };

  /** Rename a route's alias in place. Persisted immediately (single-key
   *  atomic op) — unlike add/remove, which stage until Save. Unsaved hop
   *  edits on the renamed route are preserved by renaming in local state
   *  first, then patching the backend row and re-basing the baseline. */
  const handleRenameRoute = async (alias: string) => {
    const next = window.prompt(t('route.rename.prompt', { alias }))?.trim();
    if (!next || next === alias) return;
    if (routes.some((r) => r.alias === next)) {
      flash(t('route.exists', { alias: next }));
      return;
    }
    try {
      await renameRoute(alias, next);
      // Patch local state + baseline so the dirty flag stays truthful.
      const remap = (rs: Route[]) =>
        rs.map((r) => (r.alias === alias ? { ...r, alias: next } : r));
      setRoutes(remap);
      baselineRef.current = remap(baselineRef.current);
      delete routeCacheRef.current[alias];
      flash(t('route.renamed', { from: alias, to: next }));
    } catch (e) { setError(String(e)); }
  };

  const handleAddHop = (alias: string) => {
    setRoutes((prev) => prev.map((r) =>
      r.alias === alias
        ? { ...r, hops: [...r.hops, { provider: proxyProviders[0]?.id ?? '', model: '' }] }
        : r
    ));
  };

  const handleRemoveHop = (alias: string, hopIdx: number) => {
    setRoutes((prev) => prev.map((r) =>
      r.alias === alias
        ? { ...r, hops: r.hops.filter((_, i) => i !== hopIdx) }
        : r
    ));
  };

  const handleEditHop = (alias: string, hopIdx: number, field: 'provider' | 'model', value: string) => {
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
    e.preventDefault();
    (e.currentTarget as HTMLElement).setPointerCapture?.(e.pointerId);
    setPointerDrag({ alias, fromIdx });
    setRouteOverIdx({ alias, idx: fromIdx });
  };

  const handlePointerMove = (e: React.PointerEvent) => {
    if (!pointerDrag) return;
    const target = hopRowAt(e.clientX, e.clientY);
    if (target && target.alias === pointerDrag.alias) {
      setRouteOverIdx({ alias: target.alias, idx: target.idx });
    } else if (routeOverIdx) {
      setRouteOverIdx(null);
    }
  };

  const handlePointerUp = (e: React.PointerEvent) => {
    if (!pointerDrag) return;
    const target = hopRowAt(e.clientX, e.clientY);
    const toIdx = target && target.alias === pointerDrag.alias ? target.idx : pointerDrag.fromIdx;
    applyHopMove(pointerDrag.alias, pointerDrag.fromIdx, toIdx);
    (e.currentTarget as HTMLElement).releasePointerCapture?.(e.pointerId);
    setPointerDrag(null);
    setRouteOverIdx(null);
  };

  /** Reconcile the editor state with the backend: save routes that are new
   *  or differ from the baseline, delete routes that were removed while
   *  editing (add/remove stage locally), skip untouched routes. */
  const handleSaveRoutes = async () => {
    try {
      const baselineByAlias = new Map(baselineRef.current.map((r) => [r.alias, r]));
      const currentAliases = new Set(routes.map((r) => r.alias));
      const toSave = routes.filter((r) => {
        const base = baselineByAlias.get(r.alias);
        return !base || JSON.stringify(base) !== JSON.stringify(r);
      });
      const toDelete = baselineRef.current.filter((r) => !currentAliases.has(r.alias));
      // Independent IPC calls — fan out in parallel. Each route's write is
      // its own unit of work; tokio serializes SQLite but Tauri can issue
      // them concurrently.
      await Promise.all([
        ...toSave.map((r) => saveRoute(r)),
        ...toDelete.map((r) => deleteRoute(r.alias)),
      ]);
      baselineRef.current = routes;
      delete routeCacheRef.current[appType];
      flash(t('route.saved'));
      // Reload from backend to get canonical state.
      const fresh = await listRoutes();
      setRoutes(fresh);
      baselineRef.current = fresh;
    } catch (e) { setError(String(e)); }
  };

  const handleDiscardRoutes = async () => {
    delete routeCacheRef.current[appType];
    try {
      const fresh = await listRoutes();
      setRoutes(fresh);
      baselineRef.current = fresh;
    } catch { setRoutes([]); baselineRef.current = []; }
  };

  /** Toggle a route's inclusion in the batch-delete set. */
  const toggleRouteSelection = (alias: string) => {
    setSelectedAliases((prev) => {
      const next = new Set(prev);
      if (next.has(alias)) next.delete(alias);
      else next.add(alias);
      return next;
    });
  };

  /** Stage removal of every route in `selectedAliases` (persisted on Save). */
  const handleDeleteSelectedRoutes = () => {
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
      filteredRoutes,
      selectedAliases,
      collapsedRoutes,
      knownModels,
      fetchedModels,
      fetchingId,
      routeOverIdx,
      pointerDrag,
      proxyProviders: proxyProviders.map((pp) => ({ id: pp.id, name: pp.name })),
      routeCacheRef,
    }),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [
      routes,
      isRouteDirty,
      filteredRoutes,
      selectedAliases,
      collapsedRoutes,
      knownModels,
      fetchedModels,
      fetchingId,
      routeOverIdx,
      pointerDrag,
      proxyProviders,
    ],
  );

  const routesActionsValue: RoutesActionsState = {
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
    handleHopKeyDown,
    onRestartProxy: handleRestartProxy,
    proxyRunning: running,
    restartBusy: busy === 'restart',
  };

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

  /** Unified card list — presets (chain order) + linked `ccs-*`
   *  upstreams; standalone pool rows only on the routing pages, where
   *  routes reference them. */
  const cards = useMemo(
    () =>
      mergeProviderCards(providers, proxyProviders, {
        includeStandalone: appType === 'claude_code' || appType === 'codex',
      }),
    [providers, proxyProviders, appType],
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
      cfg,
      breakerOf,
      breakerOfClass,
    }),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [cards, current, pinnedId, health, testResults, testingId, running, busy, cfg],
  );

  const providersActionsValue: ProvidersActionsState = {
    onSwitch: handleSwitch,
    onToggleQueue: handleToggleQueue,
    onToggleEnabled: handleToggleEnabled,
    onTogglePin: handleTogglePin,
    onTestProvider: handleTestProvider,
    onClearTestResult: clearTestResult,
    onAdd: () => openProviderModal(),
    onEditCard: (card) => openProviderModal(card),
    onDeleteCard: handleDeleteCard,
    onReorder: handleReorderCards,
    onRefresh: () => refresh(),
  };

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

      {error && <div className="error-banner">{error}</div>}
      {notice && <div className="notice-banner">{notice}</div>}

      <GatewayPanel
        cfg={cfg}
        running={running}
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
        claudeMode={claudeStatus?.mode ?? 'token'}
        onToggleClaudeMode={handleToggleClaudeMode}
        onCopy={copy}
        onStart={handleStart}
        onStop={handleStop}
        onEditSettings={openGatewayModal}
      />

      <Client_config_status app_type={appType} refresh_version={config_refresh_version} />

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
          </RoutesActionsProvider>
        </RoutesDataProvider>
      )}

      <Launchers
        cfg={cfg}
        busy={busy}
        codex_desktop_supported={cliStatus?.desktopSupported ?? false}
        claude_desktop_supported={claudeStatus?.desktopSupported ?? false}
        onOpenDesktop={handleOpenDesktop}
        onOpenCli={handleOpenCli}
        onOpenCopilot={handleOpenCopilot}
        onOpenVscode={handleOpenVscode}
        onOpenClaudeDesktop={handleOpenClaudeDesktop}
        on_open_claude_cli={handle_open_claude_cli}
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
        />
      )}

      {/* Gateway settings modal (port + auth token) */}
      {showGatewayModal && (
        <div className="modal-backdrop" onClick={() => setShowGatewayModal(false)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
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
                value={gatewayForm.authToken}
                placeholder="(empty = accept any key)"
                onChange={(e) => setGatewayForm({ ...gatewayForm, authToken: e.target.value })}
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
