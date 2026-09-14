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
  reorderRouteHops,
  fetchProviderModels,
  type ProxyProvider,
  type ProxyHealth,
  type TestResult,
  type Route,
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
  /** Proxy provider edit modal (Claude upstream pool). */
  const [showProxyModal, setShowProxyModal] = useState(false);
  const [editingProxy, setEditingProxy] = useState<ProxyProvider | null>(null);
  const [proxyForm, setProxyForm] = useState({ id: '', name: '', type: 'anthropic', baseURL: '', apiKey: '' });
  /** Drag-reorder state. */
  const [dragIdx, setDragIdx] = useState<number | null>(null);
  const [overIdx, setOverIdx] = useState<number | null>(null);
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
      await startProxy();
      await refresh();
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

  const handleImport = async () => {
    setImporting(true);
    setError(null);
    try {
      const report = await importCcSwitchProviders();
      if (report.imported.length === 0 && report.skippedExisting.length === 0) {
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
            (routeCount > 0 ? ` · ${t('chain.importCcSwitch.routes', { n: String(routeCount) })}` : ''),
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

  const dropOn = async (targetIdx: number) => {
    if (dragIdx === null || dragIdx === targetIdx) {
      setDragIdx(null);
      setOverIdx(null);
      return;
    }
    const ids = providers.map((p) => p.id);
    const [moved] = ids.splice(dragIdx, 1);
    ids.splice(targetIdx, 0, moved);
    setDragIdx(null);
    setOverIdx(null);
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
      setProxyForm({ id: p.id, name: p.name, type: p.type, baseURL: p.baseURL, apiKey: p.apiKey });
    } else {
      setEditingProxy(null);
      setProxyForm({ id: '', name: '', type: 'anthropic', baseURL: '', apiKey: '' });
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
      models: [],
      enabled: true,
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

  const handleAddRoute = async () => {
    const alias = window.prompt(t('route.add') + ' — alias (e.g. claude-sonnet-5):');
    if (!alias?.trim()) return;
    try {
      await saveRoute({ alias: alias.trim(), description: '', hops: [] });
      setRoutes(await listRoutes());
    } catch (e) { setError(String(e)); }
  };

  const handleDeleteRoute = async (alias: string) => {
    if (!window.confirm(t('route.delete.confirm', { alias }))) return;
    try {
      await deleteRoute(alias);
      setRoutes(await listRoutes());
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

  const [routeDragIdx, setRouteDragIdx] = useState<{ alias: string; idx: number } | null>(null);
  const [routeOverIdx, setRouteOverIdx] = useState<{ alias: string; idx: number } | null>(null);

  const handleHopDrop = (alias: string, targetIdx: number) => {
    if (!routeDragIdx || routeDragIdx.alias !== alias || routeDragIdx.idx === targetIdx) {
      setRouteDragIdx(null);
      setRouteOverIdx(null);
      return;
    }
    const dragIdx = routeDragIdx.idx;
    setRoutes((prev) => prev.map((r) => {
      if (r.alias !== alias) return r;
      const hops = [...r.hops];
      const [moved] = hops.splice(dragIdx, 1);
      hops.splice(targetIdx, 0, moved);
      return { ...r, hops };
    }));
    setRouteDragIdx(null);
    setRouteOverIdx(null);
  };

  const handleSaveRoutes = async () => {
    try {
      for (const r of routes) {
        await saveRoute(r);
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

  /** Delete every route currently in `selectedAliases`. */
  const handleDeleteSelectedRoutes = async () => {
    const aliases = Array.from(selectedAliases);
    if (aliases.length === 0) return;
    if (!window.confirm(t('route.deleteSelectedConfirm', { n: String(aliases.length) }))) {
      return;
    }
    try {
      for (const a of aliases) {
        await deleteRoute(a);
      }
      setSelectedAliases(new Set());
      setRoutes(await listRoutes());
    } catch (e) { setError(String(e)); }
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

  return (
    <section className="page">
      <div className="page-header">
        <div>
          <h2>{cfg.label}</h2>
          <p className="muted">{t(surfaceKey)}</p>
        </div>
        <div className="header-actions">
          {cfg.ccSwitchUpstream && (
            <button className="btn" onClick={handleImport} disabled={importing}>
              {importing ? '…' : t('chain.importCcSwitch')}
            </button>
          )}
          {running ? (
            <button className="btn" onClick={handleStop} disabled={busy === 'stop'}>
              {t('app.gateway.stop')}
            </button>
          ) : (
            <button className="btn primary" onClick={handleStart} disabled={busy === 'start'}>
              {t('app.gateway.start')}
            </button>
          )}
        </div>
      </div>

      {error && <div className="error-banner">{error}</div>}
      {notice && <div className="notice-banner">{notice}</div>}

      {/* Gateway status card */}
      {cfg.showGatewayCard && (
        <div className="card endpoint-card">
          <div className="card-head">
            <h3>{t('app.gateway.title')}</h3>
            <span className={running ? 'pill on' : 'pill off'}>
              {running ? t('app.gateway.running') : t('app.gateway.stopped')}
            </span>
          </div>
          <div className="endpoint-row">
            <span className="endpoint-label">{t('app.gateway.url')}</span>
            <code className="endpoint-value">{gatewayUrl || '…'}</code>
            <button className="btn small" onClick={() => copy(gatewayUrl, 'url')}>
              {copied === 'url' ? '✓' : t('app.gateway.copy')}
            </button>
          </div>
          <div className="endpoint-row">
            <span className="endpoint-label">{t('app.gateway.apiKey')}</span>
            <code className="endpoint-value">{authToken || t('app.gateway.noKey')}</code>
            <button className="btn small" onClick={() => copy(authToken, 'key')}>
              {copied === 'key' ? '✓' : t('app.gateway.copy')}
            </button>
          </div>
          {cfg.ccSwitchUpstream && (
            <p className="muted small">{t('app.gateway.ccswitchHint')}</p>
          )}
          {cfg.launchers === 'codex' && (
            <div className="endpoint-row">
              <span className="endpoint-label">{t('codex.mirror.home')}</span>
              <code className="endpoint-value">{cliStatus?.configDir ?? '~/.codex-cli'}</code>
              <span className={cliStatus?.initialized ? 'pill on' : 'pill off'}>
                {cliStatus?.initialized ? '✓' : t('codex.mirror.notInitialized')}
              </span>
            </div>
          )}
        </div>
      )}

      {/* Upstream Providers pool (Claude + Codex) — proxy_providers with flow viz + force-pin */}
      {(appType === 'claude_code' || appType === 'codex') && (
        <div className="card">
          <div className="card-head">
            <h3>{t('claude.upstream.title')}</h3>
            <div className="header-actions">
              <button className="btn small" onClick={() => refresh()}>
                {t('common.refresh')}
              </button>
              <button className="btn small" onClick={() => openProxyModal()}>
                {t('claude.upstream.add')}
              </button>
            </div>
          </div>
          <p className="muted small">{t('claude.upstream.desc')}</p>
          {proxyProviders.length === 0 ? (
            <p className="muted">{t('claude.upstream.empty')}</p>
          ) : (
            <>
              {/* Failover flow visualization */}
              <div className="failover-flow">
                {proxyProviders
                  .filter((pp) => pp.enabled)
                  .map((pp, i, arr) => {
                    const bs = breakerOf(pp.id);
                    const isActive = health?.lastUsed === pp.id;
                    const isPinned = pinnedId === pp.id;
                    const isBroken = bs === 'open';
                    const nodeClass = [
                      'flow-node',
                      isPinned ? 'pinned' : isActive ? 'active' : '',
                      isBroken ? 'broken' : '',
                    ].filter(Boolean).join(' ');
                    return (
                      <div key={pp.id}>
                        {i > 0 && <div className="flow-arrow">↓</div>}
                        <div className={nodeClass}>
                          <span className="flow-node-id">
                            {pp.name || pp.id}
                          </span>
                          <span className="flow-node-url">{pp.baseURL || '—'}</span>
                          <div className="flow-node-status">
                            {bs && <span className={`breaker-dot ${breakerOfClass(bs)}`} title={bs} />}
                            {isActive && <span className="tag tag-current">{t('claude.upstream.activeTag')}</span>}
                            {isPinned && <span className="tag tag-pinned">{t('claude.upstream.pinnedTag')}</span>}
                            <button
                              className={`btn small ${testResults[pp.id]?.success ? 'test-ok' : testResults[pp.id] ? 'test-fail' : ''}`}
                              disabled={testingId === pp.id}
                              onClick={() => handleTestProvider(pp.id)}
                              title={t('claude.upstream.test.help')}
                            >
                              {testingId === pp.id ? '…' : t('claude.upstream.test')}
                            </button>
                            <label className="toggle-switch" title={t('claude.upstream.pin.help')}>
                              <input
                                type="checkbox"
                                checked={isPinned}
                                onChange={() => handleTogglePin(pp.id)}
                              />
                              <span className="toggle-slider" />
                            </label>
                          </div>
                        </div>
                        {/* Test result */}
                        {testResults[pp.id] && (
                          <div className={testResults[pp.id].success ? 'test-result ok' : 'test-result fail'}>
                            {testResults[pp.id].success ? (
                              <span>
                                ✓ {t('claude.upstream.test.ok', {
                                  latency: String(testResults[pp.id].latencyMs),
                                  model: testResults[pp.id].model ?? '',
                                })}
                              </span>
                            ) : (
                              <span>✗ {t('claude.upstream.test.fail', { error: testResults[pp.id].error ?? '' })}</span>
                            )}
                            <button className="hop-remove" onClick={() => clearTestResult(pp.id)}>×</button>
                          </div>
                        )}
                      </div>
                    );
                  })}
              </div>
            </>
          )}
        </div>
      )}

      {/* Failover Route editor — top-level card (was nested inside the
       * upstream pool card, losing ~80px to double padding).
       *
       * Layout:
       *  - Sticky toolbar at the top of the section: title + global
       *    batch delete + add route + save / discard. Lets the user
       *    perform mutations without scrolling.
       *  - Each route is rendered as a `.route-card` with a header row
       *    (alias, hop count, collapse toggle, per-route add/delete).
       *  - Hops are real `<table class="hop-table">` rows: order # /
       *    drag handle / provider select / model suggest / remove.
       *  - Drag-and-drop only fires from the handle column (not the
       *    whole row) so children <select>/<input> don't swallow the
       *    mousedown — that's the previous design's bug.
       *  - A `.restart-hint` banner sits at the bottom (centered +
       *    accent-bordered) to remind the user to restart the proxy. */}
      {(appType === 'claude_code' || appType === 'codex') && (
        <div className="card route-editor">
          <div className="route-toolbar">
            <h4 className="route-toolbar-title">{t('route.title')}</h4>
            <div className="route-toolbar-actions">
              {selectedAliases.size > 0 && (
                <button
                  className="btn small danger"
                  onClick={() => handleDeleteSelectedRoutes()}
                >
                  {t('route.deleteSelected', { n: String(selectedAliases.size) })}
                </button>
              )}
              <button className="btn small" onClick={handleAddRoute}>
                {t('route.add')}
              </button>
              {isRouteDirty && (
                <button className="btn small" onClick={handleDiscardRoutes}>
                  {t('route.discard')}
                </button>
              )}
              <button
                className={`btn small ${isRouteDirty ? 'primary' : ''}`}
                onClick={handleSaveRoutes}
              >
                {isRouteDirty ? `${t('common.save')} •` : t('common.save')}
              </button>
              {isRouteDirty && (
                <span className="route-dirty">{t('route.dirty')}</span>
              )}
            </div>
          </div>
          <p className="muted small route-desc">{t('route.desc')}</p>
          {filteredRoutes.length === 0 ? (
            <p className="muted route-empty">{t('route.empty')}</p>
          ) : (
            <div className="route-list">
              {filteredRoutes.map((route) => {
                const collapsed = collapsedRoutes.has(route.alias);
                const isSelected = selectedAliases.has(route.alias);
                return (
                  <div
                    key={route.alias}
                    className={`route-card ${isSelected ? 'selected' : ''}`}
                  >
                    <div className="route-card-head">
                      <label className="route-select">
                        <input
                          type="checkbox"
                          checked={isSelected}
                          onChange={() => toggleRouteSelection(route.alias)}
                          title={t('route.selectForBatch')}
                        />
                      </label>
                      <div className="route-card-title">
                        <strong>{route.alias}</strong>
                        <span className="muted small">
                          — {t('route.clientsRequest')}
                        </span>
                        <span className="route-hop-count">
                          {t('route.hopCount', { n: String(route.hops.length) })}
                        </span>
                      </div>
                      <div className="route-card-actions">
                        <button
                          className="btn small"
                          onClick={() => handleAddHop(route.alias)}
                          title={t('route.addHop')}
                        >
                          {t('route.addHop')}
                        </button>
                        <button
                          className="btn small danger"
                          onClick={() => handleDeleteRoute(route.alias)}
                          title={t('common.delete')}
                        >
                          {t('common.delete')}
                        </button>
                        <button
                          className="btn small icon"
                          onClick={() => toggleRouteCollapse(route.alias)}
                          title={
                            collapsed ? t('route.expand') : t('route.collapse')
                          }
                        >
                          {collapsed ? '▸' : '▾'}
                        </button>
                      </div>
                    </div>
                    {!collapsed && route.description && (
                      <p className="muted small route-card-desc">
                        {route.description}
                      </p>
                    )}
                    {!collapsed && (
                      <>
                        {route.hops.length === 0 ? (
                          <p className="muted small route-no-hops">
                            {t('route.noHops')}
                          </p>
                        ) : (
                          <table className="hop-table">
                            <colgroup>
                              <col className="hop-col-idx" />
                              <col className="hop-col-drag" />
                              <col className="hop-col-provider" />
                              <col className="hop-col-model" />
                              <col className="hop-col-remove" />
                            </colgroup>
                            <thead>
                              <tr>
                                <th>#</th>
                                <th />
                                <th>{t('route.hopProvider')}</th>
                                <th>{t('route.hopModel')}</th>
                                <th />
                              </tr>
                            </thead>
                            <tbody>
                              {route.hops.map((hop, i) => {
                                const isDragOver =
                                  routeOverIdx?.alias === route.alias &&
                                  routeOverIdx?.idx === i;
                                const isDragging =
                                  routeDragIdx?.alias === route.alias &&
                                  routeDragIdx?.idx === i;
                                return (
                                  <tr
                                    key={i}
                                    className={[
                                      'hop-row',
                                      isDragOver ? 'drag-over' : '',
                                      isDragging ? 'dragging' : '',
                                    ].filter(Boolean).join(' ')}
                                    onDragOver={(e) => {
                                      e.preventDefault();
                                      setRouteOverIdx({
                                        alias: route.alias,
                                        idx: i,
                                      });
                                    }}
                                    onDragLeave={() => setRouteOverIdx(null)}
                                    onDrop={(e) => {
                                      e.preventDefault();
                                      handleHopDrop(route.alias, i);
                                    }}
                                  >
                                    <td className="hop-idx">{i + 1}</td>
                                    <td className="hop-drag-cell">
                                      <span
                                        className="hop-drag-handle"
                                        draggable
                                        role="button"
                                        tabIndex={0}
                                        aria-label={t('route.hopDragHint')}
                                        title={t('route.hopDragHint')}
                                        onDragStart={(e) => {
                                          // dataTransfer required by Firefox
                                          e.dataTransfer.effectAllowed = 'move';
                                          e.dataTransfer.setData(
                                            'text/plain',
                                            String(i),
                                          );
                                          setRouteDragIdx({
                                            alias: route.alias,
                                            idx: i,
                                          });
                                        }}
                                        onDragEnd={() => {
                                          setRouteDragIdx(null);
                                          setRouteOverIdx(null);
                                        }}
                                      >
                                        ⋮⋮
                                      </span>
                                    </td>
                                    <td className="hop-provider-cell">
                                      <select
                                        className="input"
                                        value={hop.provider}
                                        onChange={(e) =>
                                          handleEditHop(
                                            route.alias,
                                            i,
                                            'provider',
                                            e.target.value,
                                          )
                                        }
                                      >
                                        <option value="">
                                          {t('route.hopProvider')}
                                        </option>
                                        {proxyProviders.map((pp) => (
                                          <option key={pp.id} value={pp.id}>
                                            {pp.name || pp.id}
                                          </option>
                                        ))}
                                      </select>
                                    </td>
                                    <td className="hop-model-cell">
                                      <ModelSuggest
                                        value={hop.model}
                                        options={knownModels}
                                        placeholder={t('route.hopModel')}
                                        ariaLabel={t('route.hopModel')}
                                        onChange={(v) =>
                                          handleEditHop(route.alias, i, 'model', v)
                                        }
                                        onFetch={
                                          hop.provider
                                            ? () => handleFetchModels(hop.provider)
                                            : undefined
                                        }
                                        fetching={fetchingId === hop.provider}
                                      />
                                    </td>
                                    <td className="hop-remove-cell">
                                      <button
                                        className="btn small danger icon"
                                        onClick={() =>
                                          handleRemoveHop(route.alias, i)
                                        }
                                        title={t('common.delete')}
                                        aria-label={t('common.delete')}
                                      >
                                        ×
                                      </button>
                                    </td>
                                  </tr>
                                );
                              })}
                            </tbody>
                          </table>
                        )}
                      </>
                    )}
                  </div>
                );
              })}
            </div>
          )}
          <p className="restart-hint">{t('route.restartHint')}</p>
        </div>
      )}

      {/* Editable provider list — its own card, full page width. */}
      {(appType === 'claude_code' || appType === 'codex') && (
        <div className="card">
          <div className="card-head">
            <h3>{t('claude.upstream.listTitle')}</h3>
            <div className="header-actions">
              <button className="btn small" onClick={() => openProxyModal()}>
                {t('claude.upstream.add')}
              </button>
            </div>
          </div>
          <ul className="provider-list">
            {proxyProviders.map((pp) => {
              const bs = breakerOf(pp.id);
              const isPinned = pinnedId === pp.id;
              return (
                <li
                  key={pp.id}
                  className={isPinned ? 'provider-row pinned' : 'provider-row'}
                >
                  <div className="provider-info">
                    <span className="provider-name">
                      {isPinned && <span className="tag tag-pinned">{t('claude.upstream.pinnedTag')}</span>}
                      {pp.name || pp.id}
                      {bs && <span className={`breaker-dot ${breakerOfClass(bs)}`} title={bs} />}
                    </span>
                    <span className="provider-url">
                      {pp.baseURL || '—'} · {pp.type}
                    </span>
                    {/* Show actual model mappings on the card */}
                    {Object.entries(pp.modelMapping ?? {}).length > 0 && (
                      <div className="provider-mappings">
                        {Object.entries(pp.modelMapping ?? {}).map(([display, upstream]) => (
                          <span key={display} className="mapping-tag">
                            {display} → {upstream}
                          </span>
                        ))}
                      </div>
                    )}
                  </div>
                  <div className="provider-actions">
                    <button className="btn small" onClick={() => openProxyModal(pp)}>
                      {t('common.edit')}
                    </button>
                    <button className="btn small danger" onClick={() => handleDeleteProxy(pp.id)}>
                      {t('common.delete')}
                    </button>
                  </div>
                </li>
              );
            })}
          </ul>
        </div>
      )}

      {/* Provider chain — hidden for Claude (upstream pool replaces it) */}
      {appType !== 'claude_code' && (
      <div className="card chain-card">
        <div className="card-head">
          <h3>{t('chain.title')}</h3>
          <button className="btn small" onClick={() => openModal()}>
            {t('chain.add')}
          </button>
        </div>
        <p className="muted small">{t('chain.desc')}</p>
        {providers.length === 0 ? (
          <p className="muted">{t('chain.empty')}</p>
        ) : (
          <ul className="provider-list">
            {providers.map((p, i) => {
              const pcfg = p.settingsConfig as Record<string, unknown>;
              const isCurrent = p.id === current;
              const url = cfg.cardUrl(pcfg);
              return (
                <li
                  key={p.id}
                  className={
                    isCurrent
                      ? 'provider-row current'
                      : overIdx === i
                        ? 'provider-row drag-over'
                        : 'provider-row'
                  }
                  draggable
                  onDragStart={() => setDragIdx(i)}
                  onDragOver={(e) => {
                    e.preventDefault();
                    setOverIdx(i);
                  }}
                  onDragLeave={() => setOverIdx(null)}
                  onDrop={(e) => {
                    e.preventDefault();
                    void dropOn(i);
                  }}
                  onDragEnd={() => {
                    setDragIdx(null);
                    setOverIdx(null);
                  }}
                >
                  <span className="drag-handle" title={t('chain.dragHint')}>⋮⋮</span>
                  <div className="provider-info">
                    <span className="provider-name">
                      {p.name || p.id}
                      {isCurrent && <span className="tag tag-current">{t('chain.current')}</span>}
                      {!p.inFailoverQueue && (
                        <span className="tag tag-paused">{t('chain.out')}</span>
                      )}
                    </span>
                    <span className="provider-url">
                      {url || '—'}
                      {cfg.cardMeta(pcfg) ? ` · ${cfg.cardMeta(pcfg)}` : ''}
                    </span>
                  </div>
                  <div className="provider-actions">
                    <button
                      className={p.inFailoverQueue ? 'btn small' : 'btn small primary'}
                      title={t('chain.toggle.help')}
                      onClick={() => handleToggleQueue(p)}
                    >
                      {p.inFailoverQueue ? t('chain.toggle.out') : t('chain.toggle.in')}
                    </button>
                    <button
                      className="btn small primary"
                      disabled={isCurrent || busy === `switch-${p.id}`}
                      onClick={() => handleSwitch(p.id)}
                    >
                      {busy === `switch-${p.id}`
                        ? '…'
                        : isCurrent
                          ? t('chain.inUse')
                          : t('chain.enable')}
                    </button>
                    <button className="btn small" onClick={() => openModal(p)}>
                      {t('common.edit')}
                    </button>
                    <button className="btn small danger" onClick={() => handleDelete(p.id)}>
                      {t('common.delete')}
                    </button>
                  </div>
                </li>
              );
            })}
          </ul>
        )}
      </div>
      )}

      {/* Launchers (Codex — Desktop + CLI only) */}
      {cfg.launchers === 'codex' && (
        <div className="card">
          <div className="card-head">
            <h3>{t('codex.open.title')}</h3>
          </div>
          <div className="launcher-grid">
            <div className="launcher">
              <div className="launcher-text">
                <strong>{t('codex.open.desktop')}</strong>
                <span className="muted small">{t('codex.open.desktop.help')}</span>
              </div>
              <button className="btn primary" onClick={handleOpenDesktop} disabled={busy === 'desktop'}>
                {busy === 'desktop' ? '…' : t('codex.open.desktop')}
              </button>
            </div>
            <div className="launcher">
              <div className="launcher-text">
                <strong>{t('codex.open.cli')}</strong>
                <span className="muted small">{t('codex.open.cli.help')}</span>
              </div>
              <button className="btn" onClick={handleOpenCli} disabled={busy === 'cli'}>
                {busy === 'cli' ? '…' : t('codex.open.cli')}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Launchers (Copilot — CLI only) */}
      {cfg.launchers === 'copilot' && (
        <div className="card">
          <div className="card-head">
            <h3>{t('copilot.open.title')}</h3>
          </div>
          <div className="launcher-grid">
            <div className="launcher">
              <div className="launcher-text">
                <strong>{t('copilot.open.cli')}</strong>
                <span className="muted small">{t('copilot.open.cli.help')}</span>
              </div>
              <button className="btn primary" onClick={handleOpenCopilot} disabled={busy === 'copilot'}>
                {busy === 'copilot' ? '…' : t('copilot.open.cli')}
              </button>
            </div>
          </div>
        </div>
      )}

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
    </section>
  );
}
