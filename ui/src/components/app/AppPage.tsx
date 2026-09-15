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
  fetchProxyHealth,
  getPinnedProvider,
  setPinnedProvider,
  testProvider,
  listRoutes,
  saveRoute,
  deleteRoute,
  renameRoute,
  saveProxyConfig,
  fetchProviderModels,
  type ProxyProvider,
  type ProxyHealth,
  type TestResult,
  type Route,
  type ProxyConfig,
} from '../../lib/api/proxy';
import { importCcSwitchProviders } from '../../lib/api/import';
import {
  codexCliStatus,
  codexOpenCliTerminal,
  codexOpenDesktopWindow,
  type CodexCliStatus,
} from '../../lib/api/codexCli';
import { copilotOpenCliTerminal } from '../../lib/api/copilot';
import { APP_PAGES, CLAUDE_MODELS, CODEX_MODELS, type AppPageConfig } from '../../lib/appPages';
import type { AppType, Provider } from '../../lib/api/types';
import { useT } from '../../lib/ctx';
import { ModelSuggest } from '../ModelSuggest';
import { TestIcon } from '../Icons';
import { Launchers } from './Launchers';
import { GatewayPanel } from './GatewayPanel';
import { ProviderChain } from './ProviderChain';
import { UpstreamList } from './UpstreamList';
import { UpstreamPool } from './UpstreamPool';
import { ProxyPoolProvider, RoutesProvider, type ProxyPoolState, type RoutesState } from './state';
import { RouteEditor } from './RouteEditor';

interface FormState {
  id: string;
  name: string;
  values: Record<string, string>;
}

function emptyForm(cfg: AppPageConfig, gatewayUrl: string): FormState {
  const values: Record<string, string> = {};
  for (const f of cfg.fields) {
    if (f.key === 'baseUrl') values.baseUrl = gatewayUrl;
    else if (f.type === 'select' && f.options?.length) values[f.key] = f.options[0];
    else values[f.key] = '';
  }
  return { id: '', name: '', values };
}

/**
 * Unified per-app provider-chain page (cc-switch style). Every app gets the
 * same list: drag-reorderable provider cards, CURRENT badge on the selected
 * row, per-card failover-chain toggle, Enable/In-Use switch button. App
 * slots add the cc-switch endpoint card (Claude), the gateway card, and the
 * Codex launchers.
 */
export function AppPage({ appType }: { appType: AppType }) {
  const t = useT();
  const cfg = APP_PAGES[appType];
  const [providers, setProviders] = useState<Provider[]>([]);
  const [current, setCurrent] = useState<string | null>(null);
  const [running, setRunning] = useState(false);
  const [gatewayUrl, setGatewayUrl] = useState('');
  const [authToken, setAuthToken] = useState('');
  const [cliStatus, setCliStatus] = useState<CodexCliStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [showModal, setShowModal] = useState(false);
  const [form, setForm] = useState<FormState>(() => emptyForm(cfg, ''));
  const [editing, setEditing] = useState<Provider | null>(null);
  const [copied, setCopied] = useState<string | null>(null);
  const [importing, setImporting] = useState(false);
  /** Proxy providers (upstream pool) — used in the model catalog editor dropdown + Claude upstream pool. */
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
  /** Proxy provider edit modal (Claude upstream pool). `models` is the raw
   *  textarea text (one id per line / comma separated) — parsed on save. */
  const [showProxyModal, setShowProxyModal] = useState(false);
  /** Gateway settings modal (port + auth token). */
  const [showGatewayModal, setShowGatewayModal] = useState(false);
  const [gatewayForm, setGatewayForm] = useState({ port: '', authToken: '' });
  /** Config snapshot for the gateway modal (enabled bit rides along). */
  const gatewayCfgRef = useRef<ProxyConfig | null>(null);
  const [editingProxy, setEditingProxy] = useState<ProxyProvider | null>(null);
  const [proxyForm, setProxyForm] = useState({
    id: '',
    name: '',
    type: 'anthropic',
    baseURL: '',
    apiKey: '',
    models: '',
  });
  /** Route editor: aliases selected for batch delete. */
  const [selectedAliases, setSelectedAliases] = useState<Set<string>>(new Set());
  /** Route editor: aliases whose hop list is collapsed. */
  const [collapsedRoutes, setCollapsedRoutes] = useState<Set<string>>(new Set());
  /** Cached upstream model ids fetched via ⤓ per provider id. */
  const [fetchedModels, setFetchedModels] = useState<Record<string, string[]>>({});
  /** Which provider is currently being fetched. */
  const [fetchingId, setFetchingId] = useState<string | null>(null);

  const flash = (msg: string) => {
    setNotice(msg);
    setTimeout(() => setNotice(null), 4000);
  };

  const refresh = async () => {
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
      // Load proxy providers + health for Claude and Codex (upstream pool + breaker viz).
      if (appType === 'claude_code' || appType === 'codex') {
        try {
          const [pp, pin] = await Promise.all([listProxyProviders(), getPinnedProvider()]);
          setProxyProviders(pp);
          setPinnedId(pin);
        } catch { /* optional */ }
        if (st.running) {
          try { setHealth(await fetchProxyHealth(pc)); } catch { setHealth(null); }
        }
      }
      // Load routes for Claude and Codex (route editor).
      if (appType === 'claude_code' || appType === 'codex') {
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
    }
    void refresh();
    if (cfg.launchers === 'codex') {
      codexCliStatus().then(setCliStatus).catch(() => setCliStatus(null));
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [appType]);

  /** Poll proxy /health while the gateway runs so breaker states in the
   *  upstream pool stay live (they'd otherwise freeze at mount-time data).
   *  Only for pages that show the pool (Claude + Codex). */
  useEffect(() => {
    const pollsHealth =
      (appType === 'claude_code' || appType === 'codex') && running;
    if (!pollsHealth) return;
    let alive = true;
    const id = window.setInterval(async () => {
      try {
        const pc = await getProxyConfig();
        const h = await fetchProxyHealth(pc);
        if (alive) setHealth(h);
      } catch {
        if (alive) setHealth(null);
      }
    }, 10_000);
    return () => {
      alive = false;
      window.clearInterval(id);
    };
  }, [appType, running]);

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
        raw.includes('no free port')
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

  // ---- Gateway settings (port + auth token) ----

  const openGatewayModal = async () => {
    try {
      const cfg = await getProxyConfig();
      gatewayCfgRef.current = cfg;
      setGatewayForm({ port: String(cfg.port), authToken: cfg.authToken });
      setShowGatewayModal(true);
    } catch (e) {
      setError(String(e));
    }
  };

  const handleSaveGateway = async () => {
    const base = gatewayCfgRef.current;
    if (!base) return;
    const port = Number(gatewayForm.port);
    if (!Number.isInteger(port) || port < 1 || port > 65535) {
      setError(t('app.gateway.invalidPort'));
      return;
    }
    try {
      await saveProxyConfig({ ...base, port, authToken: gatewayForm.authToken.trim() });
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
        flash(t('chain.importCcSwitch.empty'));
      } else {
        // importedRoutes may be empty for non-Claude imports — only
        // include the suffix when cc-switch actually wrote something
        // useful (i.e. ANTHROPIC_DEFAULT_* mapping landed).
        const routes = report.importedRoutes ?? [];
        const routeCount = new Set(routes.map((r) => r.alias)).size;
        flash(
          t('chain.importCcSwitch.report', {
            imported: String(report.imported.length),
            skipped: String(report.skippedExisting.length),
            total: String(report.totalInSource),
          }) +
            (routeCount > 0 ? ` · ${t('chain.importCcSwitch.routes', { n: String(routeCount) })}` : '') +
            (noKeyCount > 0 ? ` · ${t('chain.importCcSwitch.noKey', { n: String(noKeyCount) })}` : ''),
        );
      }
      await refresh();
    } catch (e) {
      setError(t('chain.importCcSwitch.failed', { message: String(e) }));
    } finally {
      setImporting(false);
    }
  };

  const openModal = (p?: Provider) => {
    if (p) {
      setEditing(p);
      setForm({
        id: p.id,
        name: p.name,
        values: { ...cfg.extractForm(p) },
      });
    } else {
      setEditing(null);
      setForm(emptyForm(cfg, gatewayUrl));
    }
    setShowModal(true);
  };

  const handleSave = async () => {
    const id = form.id.trim() || form.name.trim().toLowerCase().replace(/\s+/g, '-');
    const builtConfig = cfg.buildConfig(id, form.values);
    const provider: Provider = {
      id,
      name: form.name.trim() || id,
      settingsConfig: builtConfig,
      createdAt: editing?.createdAt ?? Math.floor(Date.now() / 1000),
      sortIndex: editing?.sortIndex ?? null,
      notes: editing?.notes ?? null,
      inFailoverQueue: editing?.inFailoverQueue ?? true,
    };
    try {
      await saveProvider(appType, provider);
      setShowModal(false);
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  };

  const handleDelete = async (id: string) => {
    if (!window.confirm(t('chain.delete.confirm', { id }))) return;
    try {
      await deleteProvider(id, appType);
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  };

  const handleSwitch = async (id: string) => {
    setBusy(`switch-${id}`);
    try {
      const r = await switchProvider(id, appType);
      const name = providers.find((p) => p.id === id)?.name ?? id;
      flash(
        r.written.length > 0
          ? t('chain.switched.wrote', { name, count: String(r.written.length) })
          : t('chain.switched', { name }),
      );
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  };

  const handleToggleQueue = async (p: Provider) => {
    try {
      await setProviderFailoverQueue(p.id, appType, !p.inFailoverQueue);
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  };

  const handleOpenDesktop = async () => {
    setBusy('desktop');
    try {
      const r = await codexOpenDesktopWindow();
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
      flash(t('codex.open.copilotLaunched', { provider: r.providerId }));
    } catch (e) {
      setError(t('codex.open.failed', { message: String(e) }));
    } finally {
      setBusy(null);
    }
  };

  const surfaceKey = useMemo(
    () => `app.endpoint.${cfg.surface}` as const,
    [cfg.surface],
  );

  // ---- Proxy provider CRUD (Claude upstream pool) ----
  const openProxyModal = (p?: ProxyProvider) => {
    if (p) {
      setEditingProxy(p);
      setProxyForm({
        id: p.id,
        name: p.name,
        type: p.type,
        baseURL: p.baseURL,
        apiKey: p.apiKey,
        models: (p.models ?? []).join(', '),
      });
    } else {
      setEditingProxy(null);
      setProxyForm({ id: '', name: '', type: 'anthropic', baseURL: '', apiKey: '', models: '' });
    }
    setShowProxyModal(true);
  };

  const handleSaveProxy = async () => {
    const id = proxyForm.id.trim() || proxyForm.name.trim().toLowerCase().replace(/\s+/g, '-');
    const pp: ProxyProvider = {
      id,
      name: proxyForm.name.trim() || id,
      type: proxyForm.type,
      apiKey: proxyForm.apiKey.trim(),
      baseURL: proxyForm.baseURL.trim(),
      // Preserve the model list: parse the textarea (comma/newline separated)
      // instead of hardcoding [] — editing used to silently wipe it.
      models: proxyForm.models
        .split(/[,\n]/)
        .map((m) => m.trim())
        .filter(Boolean),
      // Preserve the row's enabled bit — hardcoding true used to silently
      // re-enable a deliberately disabled provider on edit.
      enabled: editingProxy?.enabled ?? true,
      timeoutMs: editingProxy?.timeoutMs ?? 60000,
      weight: editingProxy?.weight ?? 1,
      circuitBreaker: editingProxy?.circuitBreaker ?? { failureThreshold: 3, cooldownMs: 5000, halfOpenRetries: 1, successThreshold: 2 },
      modelMapping: editingProxy?.modelMapping ?? {},
    };
    try {
      await saveProxyProvider(pp);
      setShowProxyModal(false);
      flash(t('claude.saved'));
      await refresh();
    } catch (e) { setError(String(e)); }
  };

  const handleDeleteProxy = async (id: string) => {
    if (!window.confirm(t('claude.deletePool.confirm'))) return;
    try {
      await deleteProxyProvider(id);
      flash(t('claude.deleted'));
      await refresh();
    } catch (e) { setError(String(e)); }
  };

  const handleTogglePin = async (id: string) => {
    const next = pinnedId === id ? '' : id;
    try {
      await setPinnedProvider(next);
      setPinnedId(next || null);
      flash(next ? t('claude.upstream.pinned', { name: id }) : t('claude.upstream.unpinned'));
    } catch (e) { setError(String(e)); }
  };

  /** Enable/disable an upstream provider (persisted immediately; the
   *  proxy router skips disabled providers on its next rebuild). */
  const handleToggleEnabled = async (id: string, enabled: boolean) => {
    const target = proxyProviders.find((pp) => pp.id === id);
    if (!target) return;
    try {
      await saveProxyProvider({ ...target, enabled });
      await refresh();
    } catch (e) { setError(String(e)); }
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
      const patch = (rs: Route[]) =>
        rs.map((r) => (r.alias === alias ? { ...r, alias: next } : r));
      setRoutes(patch);
      baselineRef.current = patch(baselineRef.current);
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
      for (const r of toSave) {
        await saveRoute(r);
      }
      for (const r of toDelete) {
        await deleteRoute(r.alias);
      }
      // Update baseline and clear cache — edits are now persisted.
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

  /** Memoized routes slice for the RouteEditor context. Only changes when
   * one of its fields actually changes; the many handlers are stable
   * closures over the state setters so they don't churn the memo. */
  const routesValue = useMemo<RoutesState>(
    () => ({
      routes,
      isRouteDirty,
      filteredRoutes,
      selectedAliases,
      collapsedRoutes,
      knownModels,
      fetchedModels,
      fetchingId,
      proxyProviders: proxyProviders.map((pp) => ({ id: pp.id, name: pp.name })),
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
      routeOverIdx,
      pointerDrag,
      proxyRunning: running,
      restartBusy: busy === 'restart',
      onRestartProxy: handleRestartProxy,
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
      proxyProviders,
      routeOverIdx,
      pointerDrag,
      running,
      busy,
    ],
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

  /** Memoized pool value - only changes when one of the 6 slice fields
   * actually changes, so consumers don't re-render on unrelated AppPage
   * state churn (busy, error, notice, ...). Must stay AFTER the handler
   * declarations above — see the TDZ note near the top of the file. */
  const proxyPoolValue = useMemo<ProxyPoolState>(
    () => ({
      providers: proxyProviders,
      pinnedId,
      health,
      testResults,
      testingId,
      breakerOf,
      breakerOfClass,
      onTestProvider: handleTestProvider,
      onClearTestResult: clearTestResult,
      onTogglePin: handleTogglePin,
      onToggleEnabled: handleToggleEnabled,
    }),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [proxyProviders, pinnedId, health, testResults, testingId],
  );

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
              {importing ? '…' : t('chain.importCcSwitch')}
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
        copied={copied}
        busy={busy}
        onCopy={copy}
        onStart={handleStart}
        onStop={handleStop}
        onEditSettings={openGatewayModal}
      />

      {/* Upstream Providers pool + Upstream List share the ProxyPoolContext
       * (providers, pin, breaker, test results) so both cards read from one
       * slice instead of plumbing 6 props each. */}
      {(appType === 'claude_code' || appType === 'codex') && (
        <ProxyPoolProvider value={proxyPoolValue}>
          <UpstreamPool
            onRefresh={() => refresh()}
            onAddProvider={() => openProxyModal()}
          />
        </ProxyPoolProvider>
      )}


      {/* Failover Route editor - consumes RoutesContext (state + handlers
       * owned by AppPage so the tab-switch cache refs survive remounts). */}
      {(appType === 'claude_code' || appType === 'codex') && (
        <RoutesProvider value={routesValue}>
          <RouteEditor />
        </RoutesProvider>
      )}


      {/* Editable provider list - its own card, full page width. Same
       * ProxyPoolContext as the upstream pool above. */}
      {(appType === 'claude_code' || appType === 'codex') && (
        <ProxyPoolProvider value={proxyPoolValue}>
          <UpstreamList
            onAdd={() => openProxyModal()}
            onEdit={openProxyModal}
            onDelete={handleDeleteProxy}
          />
        </ProxyPoolProvider>
      )}

      {/* Provider chain — hidden for Claude (upstream pool replaces it) */}
      {appType !== 'claude_code' && (
        <ProviderChain
          cfg={cfg}
          providers={providers}
          current={current}
          busy={busy}
          onAdd={() => openModal()}
          onEdit={openModal}
          onDelete={handleDelete}
          onSwitch={handleSwitch}
          onToggleQueue={handleToggleQueue}
          onReorder={async (fromIdx, toIdx) => {
            if (fromIdx === toIdx) return;
            const ids = providers.map((p) => p.id);
            const [moved] = ids.splice(fromIdx, 1);
            ids.splice(toIdx, 0, moved);
            try {
              await reorderProviders(appType, ids);
              await refresh();
            } catch (e) {
              setError(String(e));
            }
          }}
        />
      )}

      <Launchers
        cfg={cfg}
        busy={busy}
        onOpenDesktop={handleOpenDesktop}
        onOpenCli={handleOpenCli}
        onOpenCopilot={handleOpenCopilot}
      />

      {/* Add/edit modal */}
      {showModal && (
        <div className="modal-backdrop" onClick={() => setShowModal(false)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <h3>
              {editing
                ? t('chain.modal.edit', { name: editing.name || editing.id })
                : t('chain.modal.add')}
            </h3>
            <label>
              {t('chain.modal.name')}
              <input
                className="input"
                value={form.name}
                onChange={(e) => setForm({ ...form, name: e.target.value })}
              />
            </label>
            {cfg.fields.map((f) => (
              <label key={f.key}>
                {f.label}
                {f.type === 'select' ? (
                  <select
                    className="input"
                    value={form.values[f.key] ?? ''}
                    onChange={(e) =>
                      setForm({ ...form, values: { ...form.values, [f.key]: e.target.value } })
                    }
                  >
                    {(f.options ?? []).map((o) => (
                      <option key={o} value={o}>
                        {o}
                      </option>
                    ))}
                  </select>
                ) : (
                  <input
                    className="input"
                    type={f.type === 'password' ? 'password' : 'text'}
                    placeholder={f.placeholder}
                    value={form.values[f.key] ?? ''}
                    onChange={(e) =>
                      setForm({ ...form, values: { ...form.values, [f.key]: e.target.value } })
                    }
                  />
                )}
              </label>
            ))}
            <div className="modal-actions">
              <button className="btn" onClick={() => setShowModal(false)}>
                {t('common.cancel')}
              </button>
              <button className="btn primary" onClick={handleSave}>
                {t('common.save')}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Proxy provider edit modal (Claude upstream pool + model mapping) */}
      {showProxyModal && (
        <div className="modal-backdrop" onClick={() => setShowProxyModal(false)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <h3>
              {editingProxy
                ? t('claude.upstream.modal.edit', { name: editingProxy.name || editingProxy.id })
                : t('claude.upstream.modal.add')}
            </h3>
            <label>
              {t('claude.upstream.modal.id')}
              <input className="input" value={proxyForm.id} disabled={Boolean(editingProxy)}
                onChange={(e) => setProxyForm({ ...proxyForm, id: e.target.value })} />
            </label>
            <label>
              {t('claude.upstream.modal.name')}
              <input className="input" value={proxyForm.name}
                onChange={(e) => setProxyForm({ ...proxyForm, name: e.target.value })} />
            </label>
            <label>
              {t('claude.upstream.modal.type')}
              <select className="input" value={proxyForm.type}
                onChange={(e) => setProxyForm({ ...proxyForm, type: e.target.value })}>
                <option value="anthropic">anthropic</option>
                <option value="openai">openai</option>
                <option value="mock">mock</option>
              </select>
            </label>
            <label>
              {t('claude.upstream.modal.baseUrl')}
              <input className="input" placeholder="https://api.deepseek.com/v1" value={proxyForm.baseURL}
                onChange={(e) => setProxyForm({ ...proxyForm, baseURL: e.target.value })} />
            </label>
            <label>
              {t('claude.upstream.modal.apiKey')}
              <input className="input" type="password" value={proxyForm.apiKey}
                onChange={(e) => setProxyForm({ ...proxyForm, apiKey: e.target.value })} />
            </label>
            <label>
              {t('claude.upstream.modal.models')}
              <textarea
                className="input"
                rows={3}
                placeholder="model-a, model-b&#10;model-c"
                value={proxyForm.models}
                onChange={(e) => setProxyForm({ ...proxyForm, models: e.target.value })}
              />
            </label>
            <div className="modal-actions">
              <button className="btn" onClick={() => setShowProxyModal(false)}>
                {t('common.cancel')}
              </button>
              <button className="btn primary" onClick={handleSaveProxy}>
                {t('common.save')}
              </button>
            </div>
          </div>
        </div>
      )}
      {/* Gateway settings modal (port + auth token) */}
      {showGatewayModal && (
        <div className="modal-backdrop" onClick={() => setShowGatewayModal(false)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <h3>{t('app.gateway.editSettings')}</h3>
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
                type="text"
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
