import { useT } from '../../../lib/ctx';
import { RouteEditor } from './RouteEditor.js';

interface AdvancedRoutesProps {
  open: boolean;
  dirty: boolean;
  onToggle: () => void;
}

/** Collapsible wrapper demoting the RouteEditor below the unified
 *  provider cards: card order is the primary failover chain; per-alias
 *  routes are the advanced override. RouteEditor mounts only while
 *  open — all editor state lives in AppPage (via the routes contexts),
 *  so collapsing and reopening loses nothing. */
export function AdvancedRoutes({ open, dirty, onToggle }: AdvancedRoutesProps) {
  const t = useT();
  return (
    <div className="card advanced-routes">
      <div className="card-head">
        <h3>
          {t('advanced.routes.title')}
          {dirty && (
            <span
              className="advanced-routes-dirty-dot"
              data-testid="advanced-routes-dirty"
              title={t('route.dirty')}
            />
          )}
        </h3>
        <div className="header-actions">
          <button
            className="btn small"
            onClick={onToggle}
            aria-expanded={open}
            data-testid="advanced-routes-toggle"
          >
            {open ? t('advanced.routes.collapse') : t('advanced.routes.expand')}
          </button>
        </div>
      </div>
      {open && (
        <>
          <p className="muted small">{t('advanced.routes.hint')}</p>
          <RouteEditor />
        </>
      )}
    </div>
  );
}
