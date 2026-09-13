import { useEffect, useMemo, useState } from 'react';
import {
  APP_TYPES,
  type AppType,
  type Provider,
} from '../../lib/api/types';
import {
  deleteProvider,
  getCurrentProvider,
  listProviders,
  reorderProviders,
  saveProvider,
} from '../../lib/api/providers';
import { getProxyConfig, type ProxyConfig } from '../../lib/api/proxy';
import { importCcSwitchProviders } from '../../lib/api/import';
import { getSyncWithCcSwitch, setSyncWithCcSwitch, setProviderSyncLocked } from '../../lib/api/detect';
import { FailoverChain } from './FailoverChain';
import { useT } from '../../lib/ctx';
import { SCHEMAS, stringifyConfig, APP_PROTOCOL } from '../../lib/providerForm';
import { PROTOCOL_LABELS, type Protocol, type Vendor, listVendors, resolveEndpoint } from '../../lib/api/vendors';
import { highlightJson, validateJson } from '../../lib/jsonHighlight';

interface AddFormState {
  id: string;
  name: string;
  /** Smart-form field values, keyed by schema field key. */
  fields: Record<string, string>;
  /** Advanced mode: raw JSON editor takes over. */
  rawJson: string;
  /** Selected vendor id (empty = manual entry). */
  vendorId: string;
  /** Selected vendor protocol (when vendor has multiple). */
  protocol: string;
}

function emptyForm(): AddFormState {
  return { id: '', name: '', fields: {}, rawJson: '{}', vendorId: '', protocol: '' };
}

export function ProvidersPage() {
  const t = useT();
  const tabKey = (at: AppType): string =>
    at === 'claude_code' ? 'providers.tab.claudeCode' : `providers.tab.${at}`;
  const [appType, setAppType] = useState<AppType>('claude_code');
  const [providers, setProviders] = useState<Provider[]>([]);
  const [current, setCurrent] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [showAdd, setShowAdd] = useState(false);
  const [form, setForm] = useState<AddFormState>(emptyForm);
  const [advanced, setOpenAdvanced] = useState(false);
  const [vendors, setVendors] = useState<Vendor[]>([]);
  /** When editing, the original provider row (id/createdAt/sortIndex preserved on save). */
  const [editing, setEditing] = useState<Provider | null>(null);
  /** "Import from cc-switch" button state. */
  const [importing, setImporting] = useState(false);
  /** Auto-sync on/off (cc-switch -> AAG, fires on proxy_start). */
  const [syncOn, setSyncOn] = useState(false);
  /** Drag-reorder: index of the row currently being dragged (null = idle). */
  const [dragIdx, setDragIdx] = useState<number | null>(null);
  /** Drag-reorder: index of the row currently highlighted as a drop target. */
  const [overIdx, setOverIdx] = useState<number | null>(null);
  useEffect(() => {
    void (async () => {
      try { setSyncOn(await getSyncWithCcSwitch()); } catch {}
    })();
  }, []);
  const toggleSync = async (next: boolean) => {
    try {
      await setSyncWithCcSwitch(next);
      setSyncOn(next);
    } catch (e) { setError(String(e)); }
  };
  const toggleLock = async (p: Provider) => {
    try {
      await setProviderSyncLocked(p.id, appType, !p.syncLocked);
      await refresh(appType);
    } catch (e) { setError(String(e)); }
  };

  // Load vendors once for the picker.
  useEffect(() => {
    void (async () => {
      try {
        setVendors(await listVendors());
      } catch {
        /* vendors are optional — manual entry still works */
      }
    })();
  }, []);

  // Proxy endpoint (URL + API key) for the endpoint card — the thing the
  // user hands to cc-switch.
  const [endpoint, setEndpoint] = useState<ProxyConfig | null>(null);
  const [copied, setCopied] = useState<string | null>(null);
  useEffect(() => {
    void (async () => {
      try {
        setEndpoint(await getProxyConfig());
      } catch {
        /* card just hides its values */
      }
    })();
  }, []);
  const copy = async (label: string, value: string) => {
    try {
      await navigator.clipboard.writeText(value);
      setCopied(label);
      setTimeout(() => setCopied(null), 1500);
    } catch {
      /* clipboard unavailable — user can select the text manually */
    }
  };

  const selectedVendor = vendors.find((v) => v.id === form.vendorId) ?? null;

  const schema = SCHEMAS[appType];
  const noFields = schema.fields.length === 0;
  const rawValidation = useMemo(() => validateJson(form.rawJson), [form.rawJson]);

  // Compute the current JSON config: either rawJson (advanced) or built
  // from the smart fields.
  const previewJson = useMemo(() => {
    if (advanced) return form.rawJson;
    if (noFields) return '{}';
    const values: Record<string, string> = {};
    for (const f of schema.fields) {
      values[f.key] = form.fields[f.key] ?? '';
    }
    try {
      return JSON.stringify(schema.build(values), null, 2);
    } catch {
      return '{}';
    }
  }, [advanced, form.rawJson, form.fields, schema, noFields]);

  const refresh = async (app: AppType) => {
    setLoading(true);
    setError(null);
    try {
      const [list, cur] = await Promise.all([
        listProviders(app),
        getCurrentProvider(app),
      ]);
      setProviders(list);
      setCurrent(cur);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    void refresh(appType);
  }, [appType]);

  // Reset the form whenever the app tab changes (so fields match the new
  // schema). Pre-populate field defaults from the schema (first option of
  // each select, sensible defaults).
  useEffect(() => {
    const defaults: Record<string, string> = {};
    for (const f of schema.fields) {
      defaults[f.key] = '';
    }
    if (appType === 'codex') defaults.providerType = 'openai';
    if (appType === 'copilot') defaults.providerType = 'openai';
    setForm({ id: '', name: '', fields: defaults, rawJson: stringifyConfig(schema.build(defaults)), vendorId: '', protocol: '' });
    setOpenAdvanced(false);
  }, [appType, schema]);

  const handleImportCcSwitch = async () => {
    setImporting(true);
    setError(null);
    try {
      const report = await importCcSwitchProviders();
      // Guard against shape drift (e.g. a backend returning snake_case)
      // so we surface a clear message instead of a TypeError.
      const imported = report?.imported ?? [];
      const skipped = report?.skippedExisting ?? [];
      const total = report?.totalInSource ?? imported.length + skipped.length;
      // Refresh every app tab so imported rows show up regardless of
      // which tab the user is currently on.
      await refresh(appType);
      if (total === 0) {
        setError(t('providers.importCcSwitch.empty'));
      } else {
        setError(
          t('providers.importCcSwitch.report', {
            imported: imported.length,
            skipped: skipped.length,
            total,
          }),
        );
      }
    } catch (e) {
      setError(t('providers.importCcSwitch.failed', { message: String(e) }));
    } finally {
      setImporting(false);
    }
  };

  const handleDelete = async (id: string) => {
    if (!confirm(t('providers.delete.confirm', { id }))) return;
    setError(null);
    try {
      await deleteProvider(id, appType);
      await refresh(appType);
    } catch (e) {
      setError(String(e));
    }
  };

  const openEdit = (p: Provider) => {
    // Prefill smart fields from the stored settings_config via the
    // schema's extract(); fall back to raw JSON mode when extraction
    // yields nothing recognizable.
    const extracted = schema.extract?.(p.settingsConfig ?? {}) ?? {};
    const recognized = Object.values(extracted).some((v) => Boolean(v));
    const fields = { ...form.fields };
    for (const f of schema.fields) {
      fields[f.key] = extracted[f.key] ?? '';
    }
    setEditing(p);
    setShowAdd(true);
    setOpenAdvanced(!recognized);
    setForm({
      id: p.id,
      name: p.name,
      fields,
      rawJson: stringifyConfig(p.settingsConfig ?? {}),
      vendorId: '',
      protocol: '',
    });
  };

  const handleAdd = async () => {
    setError(null);
    try {
      const validation = advanced ? validateJson(form.rawJson) : { ok: true as const, value: undefined };
      if (advanced && !validation.ok) {
        setError(t('providers.invalidJson', { message: validation.error ?? '' }));
        return;
      }
      const settingsConfig = advanced
        ? (validation.value as Record<string, unknown>)
        : schema.build(form.fields as Record<string, string | number | null>);
      const p: Provider = {
        id: form.id,
        name: form.name,
        settingsConfig,
        // Editing preserves the original row's metadata.
        createdAt: editing?.createdAt ?? Math.floor(Date.now() / 1000),
        sortIndex: editing?.sortIndex ?? providers.length + 1,
        notes: editing?.notes ?? null,
        inFailoverQueue: editing?.inFailoverQueue ?? false,
      };
      await saveProvider(appType, p);
      setShowAdd(false);
      setEditing(null);
      setForm(emptyForm());
      await refresh(appType);
      setError(editing ? t('providers.saved') : null);
    } catch (e) {
      setError(String(e));
    }
  };

  // When the user flips to raw JSON mode, seed rawJson with the
  // current built config so they don't lose what they typed.
  const toggleAdvanced = () => {
    if (!advanced) {
      setForm({ ...form, rawJson: previewJson });
    }
    setOpenAdvanced(!advanced);
  };

  // ── Vendor picker logic ──
  // Picking a vendor fills baseUrl + apiKey from the vendor, choosing the
  // protocol the current app speaks when the vendor has it (falls back to
  // the vendor's first protocol). Switching protocols re-resolves the
  // endpoint (endpoints[protocol] ?? baseUrl).
  const pickVendor = (vendorId: string) => {
    const v = vendors.find((x) => x.id === vendorId) ?? null;
    if (!v) {
      // Manual entry: clear vendor-derived values but keep the rest.
      setForm({ ...form, vendorId: '', protocol: '' });
      return;
    }
    const want = APP_PROTOCOL[appType] ?? '';
    const protocol =
      v.protocols.includes(want) ? want : (v.protocols[0] ?? '');
    const baseUrl = schema.fields.some((f) => f.key === 'baseUrl')
      ? resolveEndpoint(v, protocol)
      : '';
    setForm({
      ...form,
      vendorId,
      protocol,
      // Auto-fill name from the vendor when empty (user can override).
      name: form.name || v.name,
      fields: {
        ...form.fields,
        ...(schema.fields.some((f) => f.key === 'baseUrl') ? { baseUrl } : {}),
        ...(schema.fields.some((f) => f.key === 'apiKey') ? { apiKey: v.apiKey } : {}),
      },
    });
  };

  const pickProtocol = (protocol: string) => {
    const v = selectedVendor;
    if (!v) return;
    const baseUrl = resolveEndpoint(v, protocol);
    setForm({
      ...form,
      protocol,
      fields: {
        ...form.fields,
        ...(schema.fields.some((f) => f.key === 'baseUrl') ? { baseUrl } : {}),
      },
    });
  };

  return (
    <div className="providers-page">
      <header className="page-header">
        <h2>{t('providers.title')}</h2>
        <div className="app-tabs">
          {APP_TYPES.map((a) => (
            <button
              key={a.value}
              className={a.value === appType ? 'tab active' : 'tab'}
              onClick={() => setAppType(a.value)}
            >
              {t(tabKey(a.value))}
            </button>
          ))}
        </div>
      </header>

      {error && <pre className="aag-error">{error}</pre>}

      {appType === 'claude_code' && endpoint && (
        <section className="endpoint-card">
          <h3>{t('providers.endpoint.title')}</h3>
          <p className="muted">{t('providers.endpoint.hint')}</p>
          <div className="endpoint-row">
            <span className="endpoint-label">{t('providers.endpoint.url')}</span>
            <code>http://{endpoint.host}:{endpoint.port}</code>
            <button
              className="btn"
              onClick={() => copy('url', `http://${endpoint.host}:${endpoint.port}`)}
            >
              {copied === 'url' ? '✓' : t('providers.endpoint.copy')}
            </button>
          </div>
          <div className="endpoint-row">
            <span className="endpoint-label">{t('providers.endpoint.apiKey')}</span>
            <code>{endpoint.authToken || t('providers.endpoint.noKey')}</code>
            <button
              className="btn"
              onClick={() => copy('key', endpoint.authToken)}
              disabled={!endpoint.authToken}
            >
              {copied === 'key' ? '✓' : t('providers.endpoint.copy')}
            </button>
          </div>
          <label className="settings-toggle sync-toggle">
            <input
              type="checkbox"
              checked={syncOn}
              onChange={(e) => toggleSync(e.target.checked)}
            />
            <span>
              <strong>{t('providers.syncFromCcSwitch')}</strong>
              <small className="muted">{t('providers.syncFromCcSwitch.help')}</small>
            </span>
          </label>
        </section>
      )}

      <section className="list-section">
        {appType === 'claude_code' && <FailoverChain providers={providers} />}
        <div className="list-header">
          <h3>{t('providers.section', { app: t(tabKey(appType)) })}</h3>
          {appType === 'claude_code' && (
            <p className="muted">{t('providers.chainHint')}</p>
          )}
          {appType === 'claude_code' && (
            <p className="muted">{t('providers.dragHint')}</p>
          )}
          <div className="list-header-actions">
            <button
              className="btn"
              onClick={handleImportCcSwitch}
              disabled={importing}
              title={t('providers.importCcSwitch.help')}
            >
              {importing ? t('common.loading') : t('providers.importCcSwitch')}
            </button>
            <button
              className="btn primary"
              onClick={() => {
                setEditing(null);
                setShowAdd(true);
              }}
            >
              {t('providers.add')}
            </button>
          </div>
        </div>

        {loading ? (
          <p className="muted">{t('common.loading')}</p>
        ) : providers.length === 0 ? (
          <p className="muted">{t('providers.empty')}</p>
        ) : (
          <ul className="provider-list">
            {providers.map((p, idx) => {
              const isCurrent = p.id === current;
              const baseUrl = (p.settingsConfig?.env as Record<string, unknown> | undefined)?.ANTHROPIC_BASE_URL;
              const dropTarget = overIdx === idx && dragIdx !== null && dragIdx !== idx;
              return (
                <li
                  key={p.id}
                  className={[
                    isCurrent ? 'provider-card-cc current' : 'provider-card-cc',
                    dropTarget ? 'drag-over' : '',
                  ].filter(Boolean).join(' ')}
                  draggable={appType === 'claude_code'}
                  onDragStart={(e) => {
                    if (appType !== 'claude_code') return;
                    setDragIdx(idx);
                    e.dataTransfer.effectAllowed = 'move';
                    e.dataTransfer.setData('text/plain', String(idx));
                  }}
                  onDragOver={(e) => {
                    if (appType !== 'claude_code' || dragIdx === null) return;
                    e.preventDefault();
                    e.dataTransfer.dropEffect = 'move';
                    setOverIdx(idx);
                  }}
                  onDragLeave={() => {
                    if (overIdx === idx) setOverIdx(null);
                  }}
                  onDrop={async (e) => {
                    if (appType !== 'claude_code' || dragIdx === null) return;
                    e.preventDefault();
                    const from = dragIdx;
                    const to = idx;
                    setDragIdx(null);
                    setOverIdx(null);
                    if (from === to) return;
                    const next = [...providers];
                    const [moved] = next.splice(from, 1);
                    next.splice(to, 0, moved);
                    setProviders(next);
                    try {
                      await reorderProviders(appType, next.map((x) => x.id));
                    } catch (err) {
                      setError(String(err));
                      await refresh(appType);
                    }
                  }}
                  onDragEnd={() => {
                    setDragIdx(null);
                    setOverIdx(null);
                  }}
                >
                  <span className="handle" aria-hidden>⋮⋮</span>
                  <div className="provider-card-main">
                    <div className="provider-card-name">
                      <strong>{p.name}</strong>
                      {isCurrent && <span className="tag tag-current">{t('providers.current')}</span>}
                      {appType === 'claude_code' && p.syncLocked && (
                        <span className="tag tag-locked">🔒 {t('providers.locked')}</span>
                      )}
                      {appType === 'claude_code' && !baseUrl && (
                        <span className="tag tag-no-url">{t('chain.skipped')}</span>
                      )}
                    </div>
                    <code className="provider-card-id">{p.id}</code>
                    {p.notes && <p className="provider-card-notes">{p.notes}</p>}
                  </div>
                  <div className="provider-actions">
                    {appType === 'claude_code' && (
                      <button
                        className={p.syncLocked ? 'btn active' : 'btn'}
                        onClick={() => toggleLock(p)}
                        title={t('providers.syncLocked.help')}
                      >
                        {p.syncLocked ? `🔒 ${t('providers.locked')}` : '🔓'}
                      </button>
                    )}
                    <button className="btn" onClick={() => openEdit(p)}>
                      {t('providers.edit')}
                    </button>
                    <button className="btn danger" onClick={() => handleDelete(p.id)}>
                      {t('providers.delete')}
                    </button>
                  </div>
                </li>
              );
            })}
          </ul>
        )}
      </section>

      {showAdd && (
        <div className="modal-backdrop" onClick={() => setShowAdd(false)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <h3>
              {editing
                ? t('providers.modal.edit', { name: editing.name })
                : t('providers.modal.addFor', { app: t(tabKey(appType)) })}
            </h3>

            <label>
              <span>{t('providers.modal.id')} <small>(unique, no spaces)</small></span>
              <input
                value={form.id}
                onChange={(e) => setForm({ ...form, id: e.target.value })}
                placeholder="my-provider"
                disabled={Boolean(editing)}
              />
            </label>
            <label>
              <span>{t('providers.modal.name')}</span>
              <input
                value={form.name}
                onChange={(e) => setForm({ ...form, name: e.target.value })}
                placeholder="My Provider"
              />
            </label>

            {vendors.length > 0 && !advanced && (
              <div className="vendor-picker">
                <label>
                  <span>{t('providers.modal.vendor')}</span>
                  <select
                    value={form.vendorId}
                    onChange={(e) => pickVendor(e.target.value)}
                  >
                    <option value="">{t('providers.modal.vendorManual')}</option>
                    {vendors.map((v) => (
                      <option key={v.id} value={v.id}>{v.name}</option>
                    ))}
                  </select>
                </label>
                {selectedVendor && selectedVendor.protocols.length > 1 && (
                  <label>
                    <span>{t('providers.modal.protocol')}</span>
                    <select
                      value={form.protocol}
                      onChange={(e) => pickProtocol(e.target.value)}
                    >
                      {selectedVendor.protocols.map((p) => (
                        <option key={p} value={p}>
                          {PROTOCOL_LABELS[p as Protocol] ?? p}
                          {APP_PROTOCOL[appType] === p ? ` (${t('providers.modal.protocolRecommended')})` : ''}
                        </option>
                      ))}
                    </select>
                  </label>
                )}
              </div>
            )}

            {!noFields && !advanced && (
              <div className="smart-form">
                {schema.fields.map((f) => (
                  <label key={f.key}>
                    <span>
                      {f.label}
                      {f.required && <small className="req"> *</small>}
                    </span>
                    {f.type === 'select' ? (
                      <select
                        value={form.fields[f.key] ?? ''}
                        onChange={(e) =>
                          setForm({
                            ...form,
                            fields: { ...form.fields, [f.key]: e.target.value },
                          })
                        }
                      >
                        <option value="">— select —</option>
                        {(f.options ?? []).map((o) => (
                          <option key={o.value} value={o.value}>{o.label}</option>
                        ))}
                      </select>
                    ) : (
                      <input
                        type={f.type === 'password' ? 'password' : 'text'}
                        value={form.fields[f.key] ?? ''}
                        onChange={(e) =>
                          setForm({
                            ...form,
                            fields: { ...form.fields, [f.key]: e.target.value },
                          })
                        }
                        placeholder={f.placeholder}
                      />
                    )}
                  </label>
                ))}
              </div>
            )}

            {noFields && (
              <div className="muted" style={{ marginBottom: 12 }}>
                {t('providers.modal.unsupported')}
              </div>
            )}

            <details className="json-preview">
              <summary>{t('providers.modal.preview')}</summary>
              <pre
                className="json-preview-block"
                dangerouslySetInnerHTML={{ __html: highlightJson(previewJson) }}
              />
            </details>

            <button type="button" className="btn link-btn" onClick={toggleAdvanced}>
              {advanced ? t('providers.modal.advancedOff') : t('providers.modal.advancedOn')}
            </button>

            {advanced && (
              <label>
                <span>{t('providers.modal.settingsConfig')}</span>
                <textarea
                  rows={10}
                  value={form.rawJson}
                  onChange={(e) => setForm({ ...form, rawJson: e.target.value })}
                  spellCheck={false}
                  className={rawValidation.ok ? '' : 'json-invalid'}
                />
                {!rawValidation.ok && (
                  <small className="json-error">{t('providers.invalidJson', { message: rawValidation.error ?? '' })}</small>
                )}
              </label>
            )}

            <div className="modal-actions">
              <button
                className="btn"
                onClick={() => {
                  setShowAdd(false);
                  setEditing(null);
                }}
              >
                {t('providers.modal.cancel')}
              </button>
              <button
                className="btn primary"
                onClick={handleAdd}
                disabled={!form.id || !form.name || (advanced && !rawValidation.ok)}
              >
                {t('providers.modal.save')}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}