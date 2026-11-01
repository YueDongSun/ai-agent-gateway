/// Suppliers workspace page.
///
/// Full CRUD for suppliers with up to four protocol endpoints each, write-only
/// secrets, model inventories with capability tiers, cc-switch import
/// preview/apply, catalog status, four client snippets, routing-rules
/// preview, and a request trace for visible downgrades. All data flows
/// through the typed supplier IPC wrappers; the browser demo runs against the
/// synthetic in-memory mock only.
import { useCallback, useEffect, useRef, useState } from 'react';
import { useT } from '../../lib/ctx.js';
import {
  classify_supplier_error,
  supplier_list,
  supplier_status,
  type Supplier_status,
  type Supplier_view,
} from '../../lib/api/suppliers.js';
import { getProxyConfig, getProxyStatus, proxy_client_url, type ProxyStatus } from '../../lib/api/proxy.js';
import { Supplier_editor } from './supplier_editor.js';
import { Supplier_discovery_status } from './supplier_discovery_status.js';
import { Supplier_discovery_help } from './supplier_discovery_details.js';
import { Provider_screening_panel } from './provider_screening_panel.js';
import { Supplier_snippets } from './supplier_snippets.js';
import { Supplier_routing } from './supplier_routing.js';
import { Supplier_trace } from './supplier_trace.js';
import { Supplier_import_dialog } from './supplier_import.js';
import { Supplier_catalog } from './supplier_catalog.js';
import { Provider_icon } from './client_management/provider_icon.js';
import { Client_management_section } from './client_management_section.js';
import { Arena_chart } from './arena_chart.js';
import { Tailnet_instances } from './tailnet_instances.js';
import { supplier_import_maintain, type Supplier_maintenance_result } from '../../lib/api/supplier_maintenance.js';
import { use_page_actions } from '../page_actions.js';
import './suppliers_page.css';

interface Suppliers_page_props {
  on_gateway_status_refresh?: () => Promise<void>;
}

export function Suppliers_page({ on_gateway_status_refresh }: Suppliers_page_props) {
  const t = useT();
  const [suppliers, set_suppliers] = useState<Supplier_view[]>([]);
  const [status, set_status] = useState<Supplier_status | null>(null);
  const [proxy_status, set_proxy_status] = useState<ProxyStatus | null>(null);
  const [listen_url, set_listen_url] = useState<string | null>(null);
  const [busy, set_busy] = useState(true);
  const [error, set_error] = useState<string | null>(null);
  const [notice, set_notice] = useState<string | null>(null);
  const [editor, set_editor] = useState<{ id: number; supplier: Supplier_view | null; delete_requested?: boolean } | null>(null);
  const dialog_sequence = useRef(0);
  const [selected_id, set_selected_id] = useState<string | null>(null);
  const [maintenance, set_maintenance] = useState<Supplier_maintenance_result | null>(null);
  const [maintenance_error, set_maintenance_error] = useState(false);
  const [import_open, set_import_open] = useState<number | null>(null);
  const [section, set_section] = useState<'inventory' | 'arena' | 'tailnet'>('inventory');
  const [arena_state, set_arena_state] = useState('Not loaded');
  const [tailnet_state, set_tailnet_state] = useState('Not scanned');
  const mounted = useRef(false);
  const request_sequence = useRef(0);

  const refresh = useCallback(async (scope: 'reconcile_imports' | 'read_only' = 'reconcile_imports') => {
    const sequence = ++request_sequence.current;
    set_busy(true);
    set_error(null);
    try {
      if (scope === 'reconcile_imports') {
        try {
          const receipt = await supplier_import_maintain();
          if (mounted.current && sequence === request_sequence.current) { set_maintenance(receipt); set_maintenance_error(false); }
        } catch { if (mounted.current && sequence === request_sequence.current) set_maintenance_error(true); }
      }
      const [list, supplier_state, gateway_state] = await Promise.all([
        supplier_list(),
        supplier_status(),
        getProxyStatus(),
      ]);
      if (!mounted.current || sequence !== request_sequence.current) return;
      set_suppliers(list.suppliers.filter((supplier) => supplier.availability !== 'pending_unsupported'));
      set_status(supplier_state);
      set_proxy_status(gateway_state);
      try {
        const config = await getProxyConfig();
        if (mounted.current) set_listen_url(proxy_client_url(config));
      } catch {
        if (mounted.current) set_listen_url(null);
      }
    } catch (cause) {
      if (mounted.current && sequence === request_sequence.current) {
        set_error(classify_supplier_error(cause).text);
      }
    } finally {
      if (mounted.current && sequence === request_sequence.current) set_busy(false);
    }
  }, []);

  const refresh_screening = useCallback(() => refresh('read_only'), [refresh]);

  useEffect(() => {
    mounted.current = true;
    void refresh();
    return () => {
      mounted.current = false;
      ++request_sequence.current;
    };
  }, [refresh]);

  function open_editor(supplier: Supplier_view | null, delete_requested = false) {
    set_editor({ id: ++dialog_sequence.current, supplier, delete_requested });
  }

  function handle_changed(editor_id: number, generation: number, message: string, close_editor = true) {
    set_notice(message);
    set_status((current) => current ? { ...current, generation: Math.max(current.generation, generation) } : current);
    if (close_editor) set_editor((current) => current?.id === editor_id ? null : current);
    void refresh();
  }

  const selected_supplier = suppliers.find((supplier) => supplier.id === selected_id);
  const maintenance_blocked = maintenance_error || Boolean(maintenance?.setupError);
  use_page_actions({ target: section === 'inventory' ? selected_supplier?.name ?? 'Suppliers' : section === 'arena' ? 'Router position' : 'Tailnet discovery', state: busy ? 'busy' : error || maintenance_blocked ? 'error' : 'idle',
    on_delete: section === 'inventory' && selected_supplier && !editor ? () => open_editor(selected_supplier, true) : undefined,
    delete_disabled: maintenance_blocked || busy }, 0);
  const connections_blocked = busy || maintenance_blocked || !maintenance;
  const sections = [
    { id: 'inventory' as const, label: 'Providers', state: busy ? 'Loading' : `${suppliers.length} entries` },
    { id: 'arena' as const, label: 'Router position', state: arena_state },
    { id: 'tailnet' as const, label: 'Tailnet instances', state: tailnet_state },
  ];
  return (
    <>
    <div className="supplier_page" data-testid="suppliers_page">
      <header className="supplier_page_heading">
        <div>
          <p className="supplier_eyebrow">{t('suppliers.eyebrow')}</p>
          <h2>{t('suppliers.title')}</h2>
          <p className="supplier_muted">{t('suppliers.subtitle')}</p>
        </div>
        <div className="supplier_row">
          <button type="button" className="btn primary" disabled={connections_blocked} data-testid="suppliers_add" onClick={() => open_editor(null)}>
            + {t('suppliers.add')}
          </button>
          <button type="button" className="btn" disabled={connections_blocked} data-testid="suppliers_import" onClick={() => set_import_open(++dialog_sequence.current)}>
            {t('suppliers.import')}
          </button>
          <button type="button" className="btn" disabled={busy} data-testid="suppliers_refresh" onClick={() => void refresh()}>
            {t('suppliers.refresh')}
          </button>
        </div>
      </header>

      <section className="supplier_overview" aria-label={t('suppliers.status.title')}>
        <div>
          <strong data-testid="suppliers_count">{suppliers.length}</strong>
          <span>{t('suppliers.overview.suppliers')}</span>
        </div>
        <div>
          <strong data-testid="suppliers_enabled_endpoints">{status?.enabledEndpointCount ?? 0}</strong>
          <span>{t('suppliers.overview.enabled_endpoints')}</span>
        </div>
        <div>
          <strong data-testid="suppliers_configured_endpoints">{status?.configuredEndpointCount ?? 0}</strong>
          <span>{t('suppliers.overview.configured')}</span>
        </div>
        <div>
          <strong data-testid="suppliers_import_state">{maintenance_blocked ? 'Retry' : maintenance ? 'Ready' : 'Checking'}</strong>
          <span>Import consistency</span>
        </div>
      </section>

      <nav className="supplier_sections" role="tablist" aria-label="Supplier workspace views">
        {sections.map((item, index) => <button key={item.id} id={`supplier-tab-${item.id}`} role="tab" aria-selected={section === item.id} aria-controls={`supplier-panel-${item.id}`} tabIndex={section === item.id ? 0 : -1}
          disabled={Boolean(editor || import_open)} data-testid={`supplier-section-${item.id}`} onClick={() => set_section(item.id)}
          onKeyDown={(event) => { let next = index; if (event.key === 'ArrowRight') next = (index + 1) % sections.length; else if (event.key === 'ArrowLeft') next = (index + sections.length - 1) % sections.length; else if (event.key === 'Home') next = 0; else if (event.key === 'End') next = sections.length - 1; else return; event.preventDefault(); set_section(sections[next].id); document.getElementById(`supplier-tab-${sections[next].id}`)?.focus(); }}>
          <strong>{item.label}</strong><span>{item.state}</span>
        </button>)}
      </nav>
      <Hot_update_banner status={status} proxy_status={proxy_status} />
      <Notice_line notice={notice} />
      {error && <div role="alert" className="supplier_error" data-testid="suppliers_error">{error}</div>}

      <section className="supplier_notice" data-testid="supplier-maintenance" aria-live="polite">
        <div className="supplier_row"><strong>{maintenance_blocked ? 'Import reconciliation needs attention' : maintenance ? (maintenance.mergeConflictCount ? 'Import policy ready · account differences need review' : 'Import policy is up to date') : 'Checking imported records…'}</strong>
          <button className="btn small" disabled={busy} onClick={() => void refresh()} data-testid="supplier-maintenance-run">Reconcile imports</button></div>
        {maintenance && <p>{maintenance.removedOauthSuppliers + maintenance.removedOauthProviders} login-based records removed · {maintenance.mergedSuppliers} duplicate entries merged{maintenance.backupCreated ? ' · recovery backup retained' : ''}</p>}
        {(maintenance?.preservedStaticSuppliers ?? 0) > 0 && <p>{maintenance?.preservedStaticSuppliers} converted API connections preserved.</p>}
        {(maintenance?.mergeConflictCount ?? 0) > 0 && <p data-testid="supplier-merge-conflicts">{maintenance?.mergeConflictCount} groups kept separate because credentials or manually edited endpoints differ. Review their existing settings before combining accounts.</p>}
        {maintenance?.setupError && <p role="alert">Records are saved. Routing setup needs attention; use Reconcile imports to retry.</p>}
        {maintenance_error && <p role="alert">The application could not reconcile its imported records. Valid records are preserved; retry before editing connections.</p>}
      </section>
      {section === 'arena' && <div role="tabpanel" id="supplier-panel-arena" aria-labelledby="supplier-tab-arena"><Arena_chart suppliers={suppliers} on_state={set_arena_state} /></div>}
      {section === 'tailnet' && <div role="tabpanel" id="supplier-panel-tailnet" aria-labelledby="supplier-tab-tailnet"><Tailnet_instances on_state={set_tailnet_state} /></div>}
      {section === 'inventory' && <div role="tabpanel" id="supplier-panel-inventory" aria-labelledby="supplier-tab-inventory">

      <Supplier_discovery_help />

      <Provider_screening_panel refresh_key={status?.generation ?? 0} on_changed={refresh_screening} />

      <section className="supplier_list" aria-label={t('suppliers.overview.suppliers')} aria-busy={busy}>
        {busy && suppliers.length === 0 ? (
          <p role="status" className="supplier_empty_note">{t('suppliers.loading')}</p>
        ) : suppliers.length === 0 ? (
          <div className="supplier_empty_note">
            <h3>{t('suppliers.empty.title')}</h3>
            <p>{t('suppliers.empty.body')}</p>
          </div>
        ) : (
          suppliers.map((supplier) => (
            <article className="supplier_card" key={supplier.id} data-testid={`suppliers_card_${supplier.id}`} data-selected={selected_id === supplier.id}>
              <div className="supplier_card_heading">
                <div className="supplier_card_identity">
                  <Provider_icon
                    icon={supplier.icon}
                    color={supplier.iconColor}
                    name={supplier.name}
                  />
                  <div>
                    <h3>{supplier.name}</h3>
                    <code data-testid={`suppliers_card_id_${supplier.id}`}>{supplier.id}</code>
                  </div>
                </div>
                <div className="supplier_row">
                  <span className={`supplier_tag${supplier.enabled ? '' : ' warn'}`}>
                    {supplier.enabled ? t('suppliers.card.enabled') : t('suppliers.card.disabled')}
                  </span>
                  <span className="supplier_tag" data-testid={`suppliers_card_availability_${supplier.id}`}>
                    {t(`suppliers.card.availability.${supplier.availability}`)}
                  </span>
                  <span className="supplier_tag">{t('suppliers.card.priority', { value: supplier.priority })}</span>
                </div>
              </div>
              <p className="supplier_hint">
                {supplier.baseUrl ?? '—'}
                {supplier.urlRequiresReview ? ` · ${t('suppliers.card.url_review')}` : ''}
              </p>
              <div className="supplier_row">
                <span className="supplier_tag" data-testid={`suppliers_card_key_${supplier.id}`}>
                  {supplier.apiKeyConfigured ? t('suppliers.card.key_configured') : t('suppliers.card.key_none')}
                </span>
                <span className="supplier_tag">{t('suppliers.card.endpoints', { count: supplier.endpoints.length })}</span>
                <span className="supplier_tag">
                  {t('suppliers.card.models', {
                    count: supplier.endpoints.reduce((sum, endpoint) => sum + endpoint.models.length, 0),
                  })}
                </span>
                {supplier.endpoints.some((endpoint) => endpoint.legacyProjectionAvailable) && (
                  <span className="supplier_tag">{t('suppliers.card.legacy_projection')}</span>
                )}
              </div>
              <ul className="supplier_endpoint_list">
                {supplier.endpoints.map((endpoint) => (
                  <li key={endpoint.id}>
                    <span className="supplier_tag">{t(`suppliers.protocol.${endpoint.protocol}`)}</span>{' '}
                    <code>{endpoint.effectiveBaseUrl ?? '—'}</code>{' '}
                    <span className="supplier_hint">
                      {t('suppliers.endpoints.auth_label')}: {t(`suppliers.endpoints.auth.${endpoint.authMode}`)}
                    </span>
                    <Supplier_discovery_status health={endpoint.discoveryHealth} />
                    {Boolean(endpoint.alternatives?.length) && <details className="supplier_details"><summary>Alternate endpoints ({endpoint.alternatives?.length})</summary>{endpoint.alternatives?.map((alternative) => <p className="supplier_hint" key={alternative.id} data-testid="supplier-alternate-endpoint"><span className="supplier_tag">{alternative.enabled ? 'Alternate · enabled' : 'Alternate · disabled'}</span> <code>{alternative.endpointUrl ?? 'URL needs review'}</code> · {alternative.modelIds.length} models · {alternative.headerNames.length} headers · {alternative.apiKeyConfigured ? 'Key configured' : 'No key'}</p>)}</details>}
                  </li>
                ))}
              </ul>
              <div className="supplier_row supplier_card_actions">
                <button type="button" className="btn small" disabled={connections_blocked} aria-pressed={selected_id === supplier.id} onClick={() => set_selected_id(supplier.id)} data-testid={`suppliers_select_${supplier.id}`}>{selected_id === supplier.id ? 'Selected' : 'Select'}</button>
                <button type="button" className="btn" disabled={connections_blocked} data-testid={`suppliers_edit_${supplier.id}`} onClick={() => { set_selected_id(supplier.id); open_editor(supplier); }}>
                  {t('suppliers.card.edit')}
                </button>
              </div>
            </article>
          ))
        )}
      </section>

      <details className="supplier_advanced" data-testid="suppliers_advanced">
        <summary data-testid="suppliers_advanced_toggle">{t('suppliers.advanced')}</summary>
        <Supplier_snippets listen_url={listen_url} />
        <Supplier_routing suppliers={suppliers} />
        <Supplier_trace />
        <Supplier_catalog />
      </details>

      </div>}

      {editor && (
        <Supplier_editor
          key={editor.id}
          supplier={editor.supplier}
          delete_requested={editor.delete_requested}
          generation={status?.generation ?? 0}
          on_close={() => set_editor((current) => current?.id === editor.id ? null : current)}
          on_changed={(generation, message, close_editor) => handle_changed(editor.id, generation, message, close_editor)}
          on_reload={() => void refresh()}
        />
      )}
      {import_open !== null && (
        <Supplier_import_dialog
          key={import_open}
          generation={status?.generation ?? 0}
          on_close={() => set_import_open((current) => current === import_open ? null : current)}
          on_applied={(generation, message) => {
            set_notice(message);
            set_status((current) => current ? { ...current, generation: Math.max(current.generation, generation) } : current);
            set_import_open((current) => current === import_open ? null : current);
          }}
          on_reload={() => void refresh()}
        />
      )}
    </div>
    {section === 'inventory' && maintenance && !maintenance_blocked && <Client_management_section on_gateway_status_refresh={on_gateway_status_refresh} />}
    </>
  );
}

function Notice_line(props: { notice: string | null }) {
  if (!props.notice) return null;
  return (
    <div role="status" className="supplier_notice" data-testid="suppliers_notice">
      {props.notice}
    </div>
  );
}

/** Saved vs active configuration generation. Config changes take effect on
 *  the running gateway only after a successful hot update or restart; the
 *  failure case (rejected save) surfaces through the page error line. */
function Hot_update_banner(props: { status: Supplier_status | null; proxy_status: ProxyStatus | null }) {
  const t = useT();
  if (!props.status || !props.proxy_status) return null;
  const running = props.proxy_status.running;
  const stale = running && props.proxy_status.config_changed;
  return (
    <div
      role="status"
      className={stale ? 'supplier_notice warn' : 'supplier_notice'}
      data-testid={stale ? 'suppliers_hot_update_stale' : running ? 'suppliers_hot_update_current' : 'suppliers_hot_update_not_running'}
    >
      <strong>
        {t('suppliers.status.generation')}: {props.status.generation}
      </strong>
      <p>
        {stale
          ? t('suppliers.status.stale', { generation: props.status.generation })
          : running
            ? t('suppliers.status.current')
            : t('suppliers.status.not_running')}
      </p>
    </div>
  );
}
