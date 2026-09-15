import { useT } from '../../lib/ctx';
import { ModelSuggest } from '../ModelSuggest';
import { useRoutes } from './state';

/** Failover route editor card. Consumes `useRoutes()` for all state +
 * handlers; the AppPage owns the provider value (routes, baseline refs,
 * selection/collapse state, pointer-drag machinery, model fetch cache).
 * Layout: sticky toolbar -> per-route `.route-card` -> hop `<table>` with
 * pointer-based drag-reorder (handle column only, so <select>/<input>
 * children don't swallow the pointerdown). */
export function RouteEditor() {
  const t = useT();
  const r = useRoutes();

  return (
    <div className="card route-editor">
      <div className="route-toolbar">
        <h4 className="route-toolbar-title">{t('route.title')}</h4>
        <div className="route-toolbar-actions">
          {r.selectedAliases.size > 0 && (
            <button className="btn small danger" onClick={() => r.handleDeleteSelectedRoutes()}>
              {t('route.deleteSelected', { n: String(r.selectedAliases.size) })}
            </button>
          )}
          <button className="btn small" onClick={r.handleAddRoute}>
            {t('route.add')}
          </button>
          {r.isRouteDirty && (
            <button className="btn small" onClick={r.handleDiscardRoutes}>
              {t('route.discard')}
            </button>
          )}
          <button
            className={`btn small ${r.isRouteDirty ? 'primary' : ''}`}
            onClick={r.handleSaveRoutes}
          >
            {r.isRouteDirty ? `${t('common.save')} •` : t('common.save')}
          </button>
          {r.isRouteDirty && <span className="route-dirty">{t('route.dirty')}</span>}
          {r.proxyRunning && (
            <button
              className="btn small"
              onClick={r.onRestartProxy}
              disabled={r.restartBusy}
              title={t('route.restartNow')}
            >
              {r.restartBusy ? '…' : t('route.restartNow')}
            </button>
          )}
        </div>
      </div>
      <p className="muted small route-desc">{t('route.desc')}</p>
      {r.filteredRoutes.length === 0 ? (
        <p className="muted route-empty">{t('route.empty')}</p>
      ) : (
        <div className="route-list">
          {r.filteredRoutes.map((route) => {
            const collapsed = r.collapsedRoutes.has(route.alias);
            const isSelected = r.selectedAliases.has(route.alias);
            return (
              <div key={route.alias} className={`route-card ${isSelected ? 'selected' : ''}`}>
                <div className="route-card-head">
                  <label className="route-select">
                    <input
                      type="checkbox"
                      checked={isSelected}
                      onChange={() => r.toggleRouteSelection(route.alias)}
                      title={t('route.selectForBatch')}
                    />
                  </label>
                  <div className="route-card-title">
                    <strong>{route.alias}</strong>
                    <span className="muted small">— {t('route.clientsRequest')}</span>
                    <span className="route-hop-count">
                      {t('route.hopCount', { n: String(route.hops.length) })}
                    </span>
                  </div>
                  <div className="route-card-actions">
                    <button
                      className="btn small"
                      onClick={() => r.handleAddHop(route.alias)}
                      title={t('route.addHop')}
                    >
                      {t('route.addHop')}
                    </button>
                    <button
                      className="btn small"
                      onClick={() => r.handleRenameRoute(route.alias)}
                      title={t('route.rename')}
                    >
                      {t('route.rename')}
                    </button>
                    <button
                      className="btn small danger"
                      onClick={() => r.handleDeleteRoute(route.alias)}
                      title={t('common.delete')}
                    >
                      {t('common.delete')}
                    </button>
                    <button
                      className="btn small icon"
                      onClick={() => r.toggleRouteCollapse(route.alias)}
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
                              r.routeOverIdx?.alias === route.alias && r.routeOverIdx?.idx === i;
                            const isDragging =
                              r.pointerDrag?.alias === route.alias &&
                              r.pointerDrag.fromIdx === i;
                            return (
                              <tr
                                key={i}
                                data-route-alias={route.alias}
                                data-hop-idx={i}
                                className={[
                                  'hop-row',
                                  isDragOver ? 'drag-over' : '',
                                  isDragging ? 'dragging' : '',
                                ]
                                  .filter(Boolean)
                                  .join(' ')}
                              >
                                <td className="hop-idx">{i + 1}</td>
                                <td className="hop-drag-cell">
                                  <span
                                    className="hop-drag-handle"
                                    role="button"
                                    tabIndex={0}
                                    aria-label={t('route.hopDragHint')}
                                    title={t('route.hopDragHint')}
                                    onPointerDown={(e) => r.handlePointerDown(route.alias, i, e)}
                                    onPointerMove={r.handlePointerMove}
                                    onPointerUp={r.handlePointerUp}
                                    onPointerCancel={r.handlePointerUp}
                                    onKeyDown={(e) =>
                                      r.handleHopKeyDown(route.alias, i, e, route.hops.length)
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
                                      r.handleEditHop(route.alias, i, 'provider', e.target.value)
                                    }
                                  >
                                    <option value="">{t('route.hopProvider')}</option>
                                    {r.proxyProviders.map((pp) => (
                                      <option key={pp.id} value={pp.id}>
                                        {pp.name || pp.id}
                                      </option>
                                    ))}
                                  </select>
                                </td>
                                <td className="hop-model-cell">
                                  <ModelSuggest
                                    value={hop.model}
                                    options={r.knownModels}
                                    placeholder={t('route.hopModel')}
                                    ariaLabel={t('route.hopModel')}
                                    onChange={(v) => r.handleEditHop(route.alias, i, 'model', v)}
                                    onFetch={
                                      hop.provider
                                        ? () => r.handleFetchModels(hop.provider)
                                        : undefined
                                    }
                                    fetching={r.fetchingId === hop.provider}
                                  />
                                </td>
                                <td className="hop-remove-cell">
                                  <button
                                    className="btn small danger icon"
                                    onClick={() => r.handleRemoveHop(route.alias, i)}
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
  );
}
