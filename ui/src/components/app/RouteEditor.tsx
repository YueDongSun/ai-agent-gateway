import { memo } from 'react';
import { useT } from '../../lib/ctx';
import { ModelSuggest } from '../ModelSuggest';
import { useRoutesActions, useRoutesData } from './routes_state.js';
import type { Route, RouteHop } from '../../lib/api/proxy';

export function RouteEditor() {
  const t = useT();
  return (
    <div className="card route-editor">
      <Toolbar />
      <RouteList />
      <p className="restart-hint">{t('route.restartHint')}</p>
    </div>
  );
}

function ToolbarImpl() {
  const t = useT();
  const a = useRoutesActions();
  const d = useRoutesData();
  return (
    <>
      <div className="route-toolbar">
        <h4 className="route-toolbar-title">{t('route.title')}</h4>
        <div className="route-toolbar-actions">
          {d.selectedAliases.size > 0 && (
            <button
              className="btn small danger"
              onClick={() => a.handleDeleteSelectedRoutes()}
            >
              {t('route.deleteSelected', { n: String(d.selectedAliases.size) })}
            </button>
          )}
          <button className="btn small" onClick={a.handleAddRoute}>
            {t('route.add')}
          </button>
          {d.isRouteDirty && (
            <button
              className="btn small"
              onClick={a.handleDiscardRoutes}
              data-testid="route-discard"
            >
              {t('route.discard')}
            </button>
          )}
          <button
            className={`btn small ${d.isRouteDirty ? 'primary' : ''}`}
            onClick={a.handleSaveRoutes}
            data-testid="route-save"
          >
            {d.isRouteDirty ? `${t('common.save')} •` : t('common.save')}
          </button>
          {d.isRouteDirty && <span className="route-dirty">{t('route.dirty')}</span>}
          {a.proxyRunning && (
            <button
              className="btn small"
              onClick={a.onRestartProxy}
              disabled={a.restartBusy}
              data-testid="route-restart"
              title={t('route.restartNow')}
            >
              {a.restartBusy ? '…' : t('route.restartNow')}
            </button>
          )}
        </div>
      </div>
      <p className="muted small route-desc">{t('route.desc')}</p>
    </>
  );
}
const Toolbar = memo(ToolbarImpl);

function RouteListImpl() {
  const t = useT();
  const d = useRoutesData();
  if (d.filteredRoutes.length === 0) {
    return <p className="muted route-empty">{t('route.empty')}</p>;
  }
  return (
    <div className="route-list">
      {d.filteredRoutes.map((route) => (
        <RouteCard key={route.alias} route={route} />
      ))}
    </div>
  );
}
const RouteList = memo(RouteListImpl);

function RouteCardImpl({ route }: { route: Route }) {
  const t = useT();
  const a = useRoutesActions();
  const d = useRoutesData();
  const collapsed = d.collapsedRoutes.has(route.alias);
  const isSelected = d.selectedAliases.has(route.alias);
  return (
    <div
      className={`route-card ${isSelected ? 'selected' : ''}`}
    >
      <div className="route-card-head">
        <label className="route-select">
          <input
            type="checkbox"
            checked={isSelected}
            onChange={() => a.toggleRouteSelection(route.alias)}
            title={t('route.selectForBatch')}
          />
        </label>
        <div className="route-card-title">
          <strong data-testid={`route-alias-${route.alias}`}>{route.alias}</strong>
          <span className="muted small">— {t('route.clientsRequest')}</span>
          <span className="route-hop-count">
            {t('route.hopCount', { n: String(route.hops.length) })}
          </span>
        </div>
        <div className="route-card-actions">
          <button
            className="btn small"
            onClick={() => a.handleAddHop(route.alias)}
            title={t('route.addHop')}
          >
            {t('route.addHop')}
          </button>
          <button
            className="btn small"
            onClick={() => a.handleRenameRoute(route.alias)}
            title={t('route.rename')}
          >
            {t('route.rename')}
          </button>
          <button
            className="btn small danger"
            onClick={() => a.handleDeleteRoute(route.alias)}
            title={t('common.delete')}
          >
            {t('common.delete')}
          </button>
          <button
            className="btn small icon"
            onClick={() => a.toggleRouteCollapse(route.alias)}
            title={collapsed ? t('route.expand') : t('route.collapse')}
          >
            {collapsed ? '▸' : '▾'}
          </button>
        </div>
      </div>
      {!collapsed && route.description && (
        <p className="muted small route-card-desc">{route.description}</p>
      )}
      {!collapsed && (
        <>
          {route.hops.length === 0 ? (
            <p className="muted small route-no-hops">{t('route.noHops')}</p>
          ) : (
            <div className="hop_table_wrap">
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
                  {route.hops.map((hop, i) => (
                    <HopRow
                      key={i}
                      route={route}
                      hop={hop}
                      index={i}
                    />
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </>
      )}
    </div>
  );
}
const RouteCard = memo(RouteCardImpl);

function HopRowImpl({
  route,
  hop,
  index,
}: {
  route: Route;
  hop: RouteHop;
  index: number;
}) {
  const t = useT();
  const a = useRoutesActions();
  const d = useRoutesData();
  const isDragOver =
    d.routeOverIdx?.alias === route.alias && d.routeOverIdx?.idx === index;
  const isDragging =
    d.pointerDrag?.alias === route.alias && d.pointerDrag.fromIdx === index;
  return (
    <tr
      data-route-alias={route.alias}
      data-hop-idx={index}
      className={[
        'hop-row',
        isDragOver ? 'drag-over' : '',
        isDragging ? 'dragging' : '',
      ]
        .filter(Boolean)
        .join(' ')}
    >
      <td className="hop-idx">{index + 1}</td>
      <td className="hop-drag-cell">
        <span
          className="hop-drag-handle"
          role="button"
          tabIndex={0}
          data-testid={`hop-handle-${route.alias}-${index}`}
          aria-label={t('route.hopDragHint')}
          title={t('route.hopDragHint')}
          onPointerDown={(e) => a.handlePointerDown(route.alias, index, e)}
          onPointerMove={a.handlePointerMove}
          onPointerUp={a.handlePointerUp}
          onPointerCancel={a.handlePointerUp}
          onKeyDown={(e) =>
            a.handleHopKeyDown(route.alias, index, e, route.hops.length)
          }
        >
          ⋮⋮
        </span>
      </td>
      <td className="hop-provider-cell">
        <select
          className="input"
          value={hop.provider}
          onChange={(e) =>
            a.handleEditHop(route.alias, index, 'provider', e.target.value)
          }
        >
          <option value="">{t('route.hopProvider')}</option>
          {d.proxyProviders.map((pp) => (
            <option key={pp.id} value={pp.id}>
              {pp.name || pp.id}
            </option>
          ))}
        </select>
      </td>
      <td className="hop-model-cell">
        <ModelSuggest
          value={hop.model}
          options={d.knownModels}
          placeholder={t('route.hopModel')}
          ariaLabel={t('route.hopModel')}
          onChange={(v) => a.handleEditHop(route.alias, index, 'model', v)}
          onFetch={
            hop.provider ? () => a.handleFetchModels(hop.provider) : undefined
          }
          fetching={d.fetchingId === hop.provider}
        />
      </td>
      <td className="hop-remove-cell">
        <button
          className="btn small danger icon"
          onClick={() => a.handleRemoveHop(route.alias, index)}
          title={t('common.delete')}
          aria-label={t('common.delete')}
        >
          ×
        </button>
      </td>
    </tr>
  );
}
const HopRow = memo(HopRowImpl);
