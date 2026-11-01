import { Fragment, useState } from 'react';
import { useT } from '../../../lib/ctx';
import { useRoutesData } from './routes_state.js';

interface Route_explanation_props {
  /** The currently pinned proxy provider id (null = no pin). */
  pinned_provider_id: string | null;
  /** The provider id with the most recent successful request, when known. */
  current_provider_id: string | null;
}

export function Route_explanation(props: Route_explanation_props) {
  const t = useT();
  const d = useRoutesData();
  const [expanded, set_expanded] = useState(false);
  if (d.filteredRoutes.length === 0) return null;
  return (
    <details className="card route_explanation" onToggle={(event) => set_expanded(event.currentTarget.open)}>
      <summary data-testid="route_explanation_toggle">{t('route.explanation.title')}</summary>
      {expanded && <Route_explanation_contents {...props} />}
    </details>
  );
}

function Route_explanation_contents({
  pinned_provider_id,
  current_provider_id,
}: Route_explanation_props) {
  const t = useT();
  const d = useRoutesData();
  const provider_by_id = new Map(d.proxyProviders.map((provider) => [provider.id, provider]));
  const referenced_provider_ids = new Set(
    d.filteredRoutes.flatMap((route) => route.hops.map((hop) => hop.provider)),
  );
  const breaker_providers = d.proxyProviders.filter((provider) =>
    referenced_provider_ids.has(provider.id),
  );

  if (d.filteredRoutes.length === 0) return null;

  return (
    <div data-testid="route_explanation_contents">
      <div className="route_explanation_section">
        <h5>{t('route.explanation.failover_rules')}</h5>
        <ul className="route_explanation_list">
          <li>{t('route.explanation.failover_retry')}</li>
          <li>{t('route.explanation.failover_terminal')}</li>
          <li>{t('route.explanation.failover_note')}</li>
        </ul>
      </div>

      {d.filteredRoutes.map((route) => (
        <div key={route.alias} className="route_explanation_block">
          <h5>
            <code>{route.alias}</code>
            <span className="muted small"> — {t('route.clientsRequest')}</span>
          </h5>
          {route.hops.length === 0 ? (
            <p className="muted small">{t('route.noHops')}</p>
          ) : (
            <div className="hop_table_wrap">
              <table className="hop-table route_explanation_table">
                <colgroup>
                  <col className="hop-col-idx" />
                  <col className="hop-col-provider" />
                  <col className="hop-col-model" />
                </colgroup>
                <thead>
                  <tr>
                    <th>#</th>
                    <th>{t('route.hopProvider')}</th>
                    <th>{t('route.hopModel')}</th>
                  </tr>
                </thead>
                <tbody>
                  {route.hops.map((hop, index) => {
                    const provider = provider_by_id.get(hop.provider);
                    const mapping = provider?.modelMapping?.[route.alias];
                    const mapping_matches_hop = mapping !== undefined && mapping === hop.model;
                    return (
                      <Fragment key={`${hop.provider}-${index}`}>
                        <tr>
                          <td className="hop-idx">{index + 1}</td>
                          <td>
                            <span>{provider?.name || hop.provider}</span>{' '}
                            <code className="muted small">({hop.provider})</code>
                            {pinned_provider_id === hop.provider && (
                              <span className="pill on route_explanation_status" style={{ marginLeft: '0.4rem' }}>
                                {t('providers.pinnedTag')}
                              </span>
                            )}
                            {current_provider_id === hop.provider && (
                              <span className="pill on route_explanation_status" style={{ marginLeft: '0.4rem' }}>
                                {t('providers.activeTag')}
                              </span>
                            )}
                            {provider && !provider.enabled && (
                              <span className="pill off route_explanation_status" style={{ marginLeft: '0.4rem' }}>
                                {t('providers.disabledTag')}
                              </span>
                            )}
                          </td>
                          <td>{hop.model || <span className="muted small">—</span>}</td>
                        </tr>
                        {mapping_matches_hop && (
                          <tr>
                            <td colSpan={3} className="muted small route_explanation_mapping">
                              {t('route.explanation.mapping', {
                                display: route.alias,
                                upstream: mapping,
                              })}
                            </td>
                          </tr>
                        )}
                      </Fragment>
                    );
                  })}
                </tbody>
              </table>
            </div>
          )}
        </div>
      ))}

      <div className="route_explanation_section">
        <h5>{t('route.explanation.breaker_title')}</h5>
        {breaker_providers.length === 0 ? (
          <p className="muted small">{t('route.explanation.breaker_unavailable')}</p>
        ) : (
          <div className="route_explanation_breakers">
            {breaker_providers.map((provider) => (
              <div key={provider.id} className="route_explanation_breaker">
                <strong>{provider.name || provider.id}</strong>{' '}
                <code className="muted small">({provider.id})</code>
                <ul className="route_explanation_list">
                  <li>
                    <strong>{t('route.explanation.breaker_failure_threshold')}:</strong>{' '}
                    {provider.circuitBreaker.failureThreshold}
                  </li>
                  <li>
                    <strong>{t('route.explanation.breaker_cooldown')}:</strong>{' '}
                    {provider.circuitBreaker.cooldownMs} ms
                  </li>
                  <li>
                    <strong>{t('route.explanation.breaker_half_open')}:</strong>{' '}
                    {provider.circuitBreaker.halfOpenRetries}
                  </li>
                  <li>
                    <strong>{t('route.explanation.breaker_success')}:</strong>{' '}
                    {provider.circuitBreaker.successThreshold}
                  </li>
                </ul>
              </div>
            ))}
          </div>
        )}
      </div>

      <p className="muted small route_explanation_note">
        {t('route.explanation.snapshot_note')}
      </p>
    </div>
  );
}
