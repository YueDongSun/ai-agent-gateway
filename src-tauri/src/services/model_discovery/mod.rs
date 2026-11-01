//! Model auto-discovery service.
//!
//! Probes each eligible upstream pool provider's `/v1/models` catalog,
//! ranks same-family successors for every already-configured model id
//! (never-downgrade, family-scoped), and applies the upgrades: newly
//! discovered successors join the pool list and `model_mapping` values with
//! explicit capability-alias keys auto-track them. Precise request targets
//! (route hop models, display-name mapping values) are never rewritten.
//! Route aliases and mapping keys are never renamed. While the proxy is
//! running, changed pool or alias-policy rows are hot-applied to new requests
//! by the caller through the runtime reload seam; in-flight requests retain
//! their admitted snapshot.
//!
//! * `rank` — pure version-comparison logic (unit tested)
//! * `fetch` — upstream catalog probing (shared with `proxy_fetch_models`)
//! * `apply` — planning and persisting the DB mutations

pub(crate) mod apply;
pub(crate) mod fetch;
pub(crate) mod rank;
mod supplier_preflight;
mod supplier_urls;
mod url_candidates;
pub(crate) use supplier_preflight::record_outcome as record_supplier_outcome;

#[cfg(test)]
mod supplier_url_tests;

use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

use crate::database::dao::proxy_providers::ProxyProvider;
use crate::database::dao::settings;
use crate::database::Database;
use crate::error::AppError;

/// What started a refresh run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Trigger {
    Startup,
    Periodic,
    Manual,
}

/// Per-provider outcome of one refresh run.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProviderReport {
    pub provider_id: String,
    pub ok: bool,
    pub error: Option<String>,
    #[serde(flatten)]
    pub plan: apply::ProviderPlan,
}

/// Result of one discovery pass, stored in `app_meta` and returned over IPC.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RefreshReport {
    pub ran_at: String,
    pub trigger: Trigger,
    pub has_changes: bool,
    /// True when another refresh was already in flight and this call was
    /// dropped (dedup, not failure).
    pub deduplicated: bool,
    pub providers: Vec<ProviderReport>,
}

/// Serializes refresh runs across the manual command, the startup hook, and
/// the periodic scheduler so upstreams are never probed concurrently.
pub(crate) static REFRESH_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

/// Pool providers eligible for discovery: enabled, opted in, and probeable.
/// `anthropic_passthrough` rows carry no stored credential (the client's
/// credential is relayed per request), and `mock` rows point at synthetic
/// URLs, so neither can be probed meaningfully.
pub(crate) fn eligible_for_refresh(pp: &ProxyProvider) -> bool {
    pp.enabled
        && pp.auto_track_models
        && !matches!(pp.provider_type.as_str(), "mock" | "anthropic_passthrough")
        && !pp.base_url.trim().is_empty()
}

/// Minimum hours between periodic refreshes; absent = 24, `0` disables.
pub(crate) fn refresh_interval_hours(db: &Database) -> i64 {
    db.get_setting(settings::KEY_MODEL_DISCOVERY_INTERVAL_HOURS)
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .unwrap_or(24)
}

pub(crate) fn last_run_epoch(db: &Database) -> i64 {
    db.get_setting(settings::KEY_MODEL_DISCOVERY_LAST_RUN)
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .unwrap_or(0)
}

/// The most recent stored refresh report, if any.
pub(crate) fn stored_report(db: &Database) -> Option<RefreshReport> {
    let raw = db
        .get_setting(settings::KEY_MODEL_DISCOVERY_LAST_REPORT)
        .ok()
        .flatten()?;
    serde_json::from_str(&raw).ok()
}

/// True when changed routing state has not yet reached a live request
/// snapshot. Cleared after a successful hot reload or by `proxy_start`.
pub(crate) fn pending_restart(db: &Database) -> bool {
    db.get_setting(settings::KEY_MODEL_DISCOVERY_PENDING_RESTART)
        .ok()
        .flatten()
        .as_deref()
        == Some("1")
}

pub(crate) fn clear_pending_restart(db: &Database) {
    let _ = db.set_setting(settings::KEY_MODEL_DISCOVERY_PENDING_RESTART, "");
}

/// Run one discovery pass across eligible pool providers: fetch each
/// upstream catalog, rank successors for every configured anchor, and apply
/// the never-downgrade upgrades. Concurrent callers are deduplicated.
pub(crate) async fn run_refresh(
    db: &Database,
    trigger: Trigger,
    proxy_running: bool,
    only_provider: Option<&str>,
) -> Result<RefreshReport, AppError> {
    let lock = REFRESH_LOCK.get_or_init(|| tokio::sync::Mutex::new(()));
    let Ok(_guard) = lock.try_lock() else {
        return Ok(RefreshReport {
            ran_at: chrono::Utc::now().to_rfc3339(),
            trigger,
            has_changes: false,
            deduplicated: true,
            providers: Vec::new(),
        });
    };

    let result = refresh_providers(db, trigger, only_provider).await;
    if only_provider.is_none() {
        let _ = db.set_setting(
            settings::KEY_MODEL_DISCOVERY_LAST_RUN,
            &chrono::Utc::now().timestamp().to_string(),
        );
    }
    let report = result?;
    if let Ok(json) = serde_json::to_string(&report) {
        let _ = db.set_setting(settings::KEY_MODEL_DISCOVERY_LAST_REPORT, &json);
    }
    if report.has_changes && proxy_running {
        let _ = db.set_setting(settings::KEY_MODEL_DISCOVERY_PENDING_RESTART, "1");
    }
    Ok(report)
}

pub(crate) async fn refresh_providers(
    db: &Database,
    trigger: Trigger,
    only_provider: Option<&str>,
) -> Result<RefreshReport, AppError> {
    let provider_ids = db.proxy_provider_ids_for_refresh(only_provider)?;
    let supplier_ids = db.supplier_discovery_ids()?;
    let routes = db.get_all_routes();
    let mut discovered_sources = std::collections::HashSet::new();
    let mut seen_suppliers = std::collections::HashSet::new();
    let mut report = RefreshReport {
        ran_at: chrono::Utc::now().to_rfc3339(),
        trigger,
        has_changes: false,
        deduplicated: false,
        providers: Vec::new(),
    };

    for provider_id in provider_ids {
        let owner_identity = db.supplier_discovery_owner(&provider_id);
        if let Ok(Some((supplier_id, _))) = &owner_identity {
            seen_suppliers.insert(supplier_id.clone());
        }
        let diagnostic_owner = owner_identity.as_ref().ok().and_then(Option::as_ref);
        let provider = match db.get_proxy_provider(&provider_id) {
            Ok(Some(provider)) => provider,
            Ok(None) => {
                report.providers.push(ProviderReport {
                    provider_id,
                    ok: false,
                    error: Some("supplier_import_excluded:provider_removed".into()),
                    plan: Default::default(),
                });
                continue;
            }
            // Preserve the existing legacy-row failure contract. Supplier-owned
            // corrupt rows are isolated and targeted calls decode only their ID.
            Err(error) if matches!(&owner_identity, Ok(None)) => return Err(error),
            Err(error) => {
                supplier_preflight::record_outcome(
                    diagnostic_owner
                        .map(|(id, _)| id.as_str())
                        .unwrap_or("unknown"),
                    &provider_id,
                    diagnostic_owner
                        .map(|(_, protocol)| protocol.as_str())
                        .unwrap_or("unknown"),
                    "configuration",
                    "failed",
                    &error.external_message(),
                );
                report.providers.push(ProviderReport {
                    provider_id,
                    ok: false,
                    error: Some(format!(
                        "supplier_import_failed:configuration:{}",
                        error.external_message()
                    )),
                    plan: Default::default(),
                });
                continue;
            }
        };
        let pp = &provider;
        let supplier = match owner_identity
            .as_ref()
            .map_err(|error| error.external_message())
            .and_then(|owner| {
                owner
                    .as_ref()
                    .map(|(supplier_id, _)| {
                        db.get_supplier_record_snapshot(&supplier_id)
                            .and_then(|(_, supplier)| {
                                supplier
                                    .ok_or_else(|| AppError::Message("supplier_not_found".into()))
                            })
                            .map_err(|error| error.external_message())
                    })
                    .transpose()
            }) {
            Ok(supplier) => supplier,
            Err(error) => {
                supplier_preflight::record_outcome(
                    diagnostic_owner
                        .map(|(id, _)| id.as_str())
                        .unwrap_or("unknown"),
                    &pp.id,
                    diagnostic_owner
                        .map(|(_, protocol)| protocol.as_str())
                        .unwrap_or("unknown"),
                    "configuration",
                    "failed",
                    &error,
                );
                report.providers.push(ProviderReport {
                    provider_id: pp.id.clone(),
                    ok: false,
                    error: Some(format!("supplier_import_failed:configuration:{}", error)),
                    plan: Default::default(),
                });
                continue;
            }
        };
        let owner = supplier.as_ref().and_then(|supplier| {
            supplier
                .endpoints
                .iter()
                .find(|endpoint| {
                    endpoint.proxy_provider_id.as_deref() == Some(&pp.id)
                        || endpoint.alternatives.iter().any(|alternative| {
                            alternative.legacy_proxy_provider_id.as_deref() == Some(&pp.id)
                        })
                })
                .map(|endpoint| (supplier, endpoint))
        });
        if supplier.is_some() && owner.is_none() {
            report.providers.push(ProviderReport {
                provider_id: pp.id.clone(),
                ok: false,
                error: Some("supplier_import_excluded:source_changed".into()),
                plan: Default::default(),
            });
            continue;
        }
        if let Some((supplier, endpoint)) = owner {
            if let Some(outcome) = supplier_preflight::check(db, supplier, endpoint, pp).await {
                report.providers.push(outcome);
                continue;
            }
        } else if !eligible_for_refresh(pp) {
            // Legacy providers retain their existing opt-in and invocation behavior.
            continue;
        }
        let log_outcome = |stage, outcome, reason: &str| {
            if let Some((supplier, endpoint)) = owner {
                supplier_preflight::record_outcome(
                    &supplier.id,
                    &pp.id,
                    &endpoint.protocol,
                    stage,
                    outcome,
                    reason,
                );
            }
        };
        let routes = match &routes {
            Ok(routes) => routes,
            Err(error) => {
                log_outcome("import", "failed", &error.external_message());
                report.providers.push(ProviderReport {
                    provider_id: pp.id.clone(),
                    ok: false,
                    error: Some(error.external_message()),
                    plan: apply::ProviderPlan::default(),
                });
                continue;
            }
        };
        if crate::services::provider_admission::enforcement_enabled(db) {
            if let Err(error) =
                crate::services::provider_admission::ensure_catalog_admitted(db, pp).await
            {
                log_outcome("preflight", "excluded", &error);
                report.providers.push(ProviderReport {
                    provider_id: pp.id.clone(),
                    ok: false,
                    error: Some(error),
                    plan: Default::default(),
                });
                continue;
            }
        }
        let auth_scheme = match crate::services::provider_admission::auth_scheme(db, pp) {
            Ok(scheme) => scheme,
            Err(error) => {
                log_outcome("preflight", "excluded", &error);
                report.providers.push(ProviderReport {
                    provider_id: pp.id.clone(),
                    ok: false,
                    error: Some(error),
                    plan: Default::default(),
                });
                continue;
            }
        };
        let started = std::time::Instant::now();
        match fetch::fetch_models_ids(pp, auth_scheme).await {
            Ok(models) => {
                if crate::services::provider_admission::enforcement_enabled(db) {
                    let model_ids = models
                        .iter()
                        .map(|model| model.id.clone())
                        .collect::<Vec<_>>();
                    if let Err(error) =
                        crate::services::provider_admission::refresh_model_admission(
                            db, pp, &model_ids,
                        )
                        .await
                    {
                        log_outcome("admission", "failed", &error);
                        report.providers.push(ProviderReport {
                            provider_id: pp.id.clone(),
                            ok: false,
                            error: Some(error),
                            plan: Default::default(),
                        });
                        continue;
                    }
                }
                let now = chrono::Utc::now().timestamp();
                let health = crate::database::dao::suppliers::SupplierDiscoveryHealth {
                    status: "catalog_available".into(),
                    checked_at: now,
                    last_success_at: Some(now),
                    latency_ms: started.elapsed().as_millis() as u64,
                    consecutive_failures: 0,
                    error: None,
                    listed_model_ids: models
                        .iter()
                        .map(|model| model.id.clone())
                        .collect::<std::collections::BTreeSet<_>>()
                        .into_iter()
                        .collect(),
                    inference_status: "unverified".into(),
                };
                match db.reconcile_supplier_catalog(pp, health) {
                    Ok(Some((added_models, inventory_changed))) => {
                        log_outcome(
                            "import",
                            if inventory_changed {
                                "imported"
                            } else {
                                "skipped"
                            },
                            if inventory_changed {
                                "catalog_updated"
                            } else {
                                "catalog_unchanged"
                            },
                        );
                        report.has_changes |= inventory_changed;
                        report.providers.push(ProviderReport {
                            provider_id: pp.id.clone(),
                            ok: true,
                            error: None,
                            plan: apply::ProviderPlan {
                                added_models,
                                ..Default::default()
                            },
                        });
                        if let Some((supplier, _)) = owner {
                            if discovered_sources.insert((supplier.id.clone(), crate::database::dao::provider_admission::provider_configuration_identity(pp))) {
                                let discovered =
                                    supplier_urls::discover(db, &supplier.id, pp, auth_scheme)
                                        .await;
                                report.has_changes |= discovered.changed;
                                report.providers.extend(discovered.entries);
                            }
                        }
                        continue;
                    }
                    Err(error) => {
                        log_outcome("import", "failed", &error.external_message());
                        report.providers.push(ProviderReport {
                            provider_id: pp.id.clone(),
                            ok: false,
                            error: Some(error.external_message()),
                            plan: Default::default(),
                        });
                        continue;
                    }
                    Ok(None) => {}
                }
                let plan = apply::plan_provider(pp, routes, &models);
                let (ok, error, plan) = match apply::apply_plan(db, pp, &plan) {
                    Ok(applied) => (true, None, applied),
                    Err(error) => (
                        false,
                        Some(error.external_message()),
                        apply::ProviderPlan::default(),
                    ),
                };
                report.has_changes |= plan.has_changes();
                log_outcome(
                    "import",
                    if ok { "imported" } else { "failed" },
                    error.as_deref().unwrap_or("catalog_updated"),
                );
                report.providers.push(ProviderReport {
                    provider_id: pp.id.clone(),
                    ok,
                    error,
                    plan,
                });
                if ok {
                    if let Some((supplier, _)) = owner {
                        if discovered_sources.insert((supplier.id.clone(), crate::database::dao::provider_admission::provider_configuration_identity(pp))) {
                            let discovered =
                                supplier_urls::discover(db, &supplier.id, pp, auth_scheme).await;
                            report.has_changes |= discovered.changed;
                            report.providers.extend(discovered.entries);
                        }
                    }
                }
            }
            Err(e) => {
                log_outcome("catalog", "failed", &e.to_string());
                let status = match &e {
                    fetch::FetchError::Http(401 | 403) => "authentication_failed",
                    fetch::FetchError::Http(404 | 405) => "catalog_unsupported",
                    fetch::FetchError::Http(429) => "rate_limited",
                    fetch::FetchError::Http(500..=599) => "temporarily_unavailable",
                    fetch::FetchError::Timeout => "timeout",
                    fetch::FetchError::Transport => "connectivity_failed",
                    _ => "catalog_invalid",
                };
                let health = crate::database::dao::suppliers::SupplierDiscoveryHealth {
                    status: status.into(),
                    checked_at: chrono::Utc::now().timestamp(),
                    last_success_at: None,
                    latency_ms: started.elapsed().as_millis() as u64,
                    consecutive_failures: 1,
                    error: Some(e.to_string()),
                    listed_model_ids: Vec::new(),
                    inference_status: "unverified".into(),
                };
                // A concurrent settings change must not acquire stale health.
                let health_error = db.record_supplier_discovery_failure(pp, health).err();
                report.providers.push(ProviderReport {
                    provider_id: pp.id.clone(),
                    ok: false,
                    error: Some(
                        health_error
                            .map(|error| error.external_message())
                            .unwrap_or_else(|| e.to_string()),
                    ),
                    plan: apply::ProviderPlan::default(),
                });
            }
        }
    }

    if only_provider.is_none() {
        for supplier_id in supplier_ids
            .iter()
            .filter(|id| !seen_suppliers.contains(*id))
        {
            supplier_preflight::record_outcome(
                supplier_id,
                supplier_id,
                "unknown",
                "preflight",
                "excluded",
                "no_configured_endpoint",
            );
            report.providers.push(ProviderReport {
                provider_id: format!("supplier:{supplier_id}"),
                ok: false,
                error: Some("supplier_import_excluded:no_configured_endpoint".into()),
                plan: Default::default(),
            });
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(
        id: &str,
        provider_type: &str,
        enabled: bool,
        auto_track: bool,
        base_url: &str,
    ) -> ProxyProvider {
        ProxyProvider {
            id: id.into(),
            name: id.into(),
            provider_type: provider_type.into(),
            api_key: String::new(),
            base_url: base_url.into(),
            models: Vec::new(),
            enabled,
            timeout_ms: 60_000,
            weight: 1,
            circuit_breaker: Default::default(),
            model_mapping: Default::default(),
            extra_headers: Default::default(),
            auto_track_models: auto_track,
            icon: None,
            icon_color: None,
        }
    }

    #[test]
    fn eligibility_filters_unprobeable_rows() {
        let base = "https://upstream.example/v1";
        assert!(eligible_for_refresh(&provider(
            "a",
            "anthropic",
            true,
            true,
            base
        )));
        assert!(!eligible_for_refresh(&provider(
            "b",
            "anthropic",
            false,
            true,
            base
        )));
        assert!(!eligible_for_refresh(&provider(
            "c",
            "anthropic",
            true,
            false,
            base
        )));
        assert!(!eligible_for_refresh(&provider(
            "d", "mock", true, true, "mock://x"
        )));
        assert!(!eligible_for_refresh(&provider(
            "e",
            "anthropic_passthrough",
            true,
            true,
            base
        )));
        assert!(!eligible_for_refresh(&provider(
            "f", "openai", true, true, "  "
        )));
    }

    #[test]
    fn interval_defaults_to_24_and_zero_disables() {
        let db = Database::memory().unwrap();
        assert_eq!(refresh_interval_hours(&db), 24);
        db.set_setting(settings::KEY_MODEL_DISCOVERY_INTERVAL_HOURS, "0")
            .unwrap();
        assert_eq!(refresh_interval_hours(&db), 0);
        db.set_setting(settings::KEY_MODEL_DISCOVERY_INTERVAL_HOURS, "6")
            .unwrap();
        assert_eq!(refresh_interval_hours(&db), 6);
        db.set_setting(settings::KEY_MODEL_DISCOVERY_INTERVAL_HOURS, "bogus")
            .unwrap();
        assert_eq!(refresh_interval_hours(&db), 24);
    }

    #[test]
    fn pending_restart_flag_roundtrip() {
        let db = Database::memory().unwrap();
        assert!(!pending_restart(&db));
        db.set_setting(settings::KEY_MODEL_DISCOVERY_PENDING_RESTART, "1")
            .unwrap();
        assert!(pending_restart(&db));
        clear_pending_restart(&db);
        assert!(!pending_restart(&db));
    }

    /// The IPC payload must be camelCase all the way down. `ProviderPlan` is
    /// `#[serde(flatten)]`ed into `ProviderReport`, and serde does not apply
    /// the outer struct's `rename_all` to flattened fields, so the plan (and
    /// the rewrites it contains) carry their own `rename_all`. The UI reads
    /// `addedModels`/`hopRewrites`/`mappingRewrites`/`skippedAmbiguous` and
    /// `oldModel`/`newModel`, so a regression here silently breaks a
    /// real-backend "Refresh models". `hopRewrites` stays on the wire for
    /// that contract but is always empty: precise hop models are locked.
    #[test]
    fn refresh_report_serializes_plan_keys_as_camel_case() {
        let report = RefreshReport {
            ran_at: "2026-01-01T00:00:00Z".into(),
            trigger: Trigger::Manual,
            has_changes: true,
            deduplicated: false,
            providers: vec![ProviderReport {
                provider_id: "p1".into(),
                ok: true,
                error: None,
                plan: apply::ProviderPlan {
                    added_models: vec!["minimax-m3.1".into()],
                    pruned_models: vec!["minimax-m2".into()],
                    hop_rewrites: vec![apply::HopRewrite {
                        alias: "max-tier".into(),
                        old_model: "minimax-m3".into(),
                        new_model: "minimax-m3.1".into(),
                    }],
                    mapping_rewrites: vec![apply::MappingRewrite {
                        display: "MiniMax M3".into(),
                        old_model: "minimax-m3".into(),
                        new_model: "minimax-m3.1".into(),
                    }],
                    skipped_ambiguous: vec!["x".into()],
                },
            }],
        };

        let value = serde_json::to_value(&report).unwrap();
        let provider = &value["providers"][0];
        assert_eq!(value["ranAt"], "2026-01-01T00:00:00Z");
        assert_eq!(value["hasChanges"], true);
        assert_eq!(provider["providerId"], "p1");
        assert_eq!(provider["addedModels"][0], "minimax-m3.1");
        assert_eq!(provider["prunedModels"][0], "minimax-m2");
        assert_eq!(provider["hopRewrites"][0]["alias"], "max-tier");
        assert_eq!(provider["hopRewrites"][0]["oldModel"], "minimax-m3");
        assert_eq!(provider["hopRewrites"][0]["newModel"], "minimax-m3.1");
        assert_eq!(provider["mappingRewrites"][0]["oldModel"], "minimax-m3");
        assert_eq!(provider["mappingRewrites"][0]["newModel"], "minimax-m3.1");
        assert_eq!(provider["skippedAmbiguous"][0], "x");
        // The snake_case aliases must not leak onto the wire.
        assert!(provider.get("added_models").is_none());
        assert!(provider["hopRewrites"][0].get("old_model").is_none());
    }

    async fn refresh_fixture(db: &Database, opt_out_during_fetch: bool) -> RefreshReport {
        use axum::{extract::State, routing::get, Json, Router};
        let app = Router::new()
            .fallback(get(
                |State((db, opt_out)): State<(Database, bool)>| async move {
                    if opt_out {
                        let mut current = db.get_all_proxy_providers().unwrap().remove(0);
                        current.auto_track_models = false;
                        current.name = "user edited while fetching".into();
                        db.save_proxy_provider(&current).unwrap();
                    }
                    Json(serde_json::json!({"data": [{"id": "minimax-m4"}]}))
                },
            ))
            .with_state((db.clone(), opt_out_during_fetch));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (shutdown, stopped) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = stopped.await;
                })
                .await
                .unwrap();
        });
        let mut snapshot = provider(
            "fixture",
            "openai",
            true,
            true,
            &format!("http://{address}"),
        );
        snapshot.models = vec!["minimax-m3".into()];
        db.save_proxy_provider(&snapshot).unwrap();
        for alias in ["aaa_first_route", "blocked_route"] {
            db.save_route(&crate::database::dao::routes::Route {
                alias: alias.into(),
                description: "user description".into(),
                hops: vec![crate::database::dao::routes::RouteHop {
                    provider: snapshot.id.clone(),
                    model: "minimax-m3".into(),
                }],
            })
            .unwrap();
        }
        let result = run_refresh(db, Trigger::Manual, true, Some("fixture")).await;
        let _ = shutdown.send(());
        server.await.unwrap();
        result.unwrap()
    }

    #[tokio::test]
    async fn invalid_providers_return_an_error_without_replacing_the_previous_report() {
        let db = Database::memory().unwrap();
        db.save_proxy_provider(&provider("fixture", "mock", true, true, "mock://fixture"))
            .unwrap();
        crate::database::dao::proxy_providers::install_proxy_provider_read_failure(&db);
        let previous = RefreshReport {
            ran_at: "2026-01-01T00:00:00Z".into(),
            trigger: Trigger::Manual,
            has_changes: false,
            deduplicated: false,
            providers: Vec::new(),
        };
        let previous_json = serde_json::to_string(&previous).unwrap();
        db.set_setting(settings::KEY_MODEL_DISCOVERY_LAST_REPORT, &previous_json)
            .unwrap();
        db.set_setting(settings::KEY_MODEL_DISCOVERY_LAST_RUN, "0")
            .unwrap();
        db.set_setting(settings::KEY_MODEL_DISCOVERY_PENDING_RESTART, "1")
            .unwrap();
        let started_at = chrono::Utc::now().timestamp();

        let error = run_refresh(&db, Trigger::Manual, true, None)
            .await
            .unwrap_err();
        assert_eq!(String::from(error), "database_error");
        assert!(last_run_epoch(&db) >= started_at);
        assert_eq!(
            db.get_setting(settings::KEY_MODEL_DISCOVERY_LAST_REPORT)
                .unwrap()
                .as_deref(),
            Some(previous_json.as_str())
        );
        assert!(pending_restart(&db));
    }

    #[tokio::test]
    async fn empty_provider_pool_remains_a_successful_refresh() {
        let db = Database::memory().unwrap();
        let report = run_refresh(&db, Trigger::Manual, false, None)
            .await
            .unwrap();
        assert!(!report.deduplicated);
        assert!(!report.has_changes);
        assert!(report.providers.is_empty());
        assert!(stored_report(&db).is_some());
    }

    #[tokio::test]
    async fn deduplicated_refresh_does_not_stamp_an_attempt() {
        let db = Database::memory().unwrap();
        let lock = REFRESH_LOCK.get_or_init(|| tokio::sync::Mutex::new(()));
        let _guard = lock.lock().await;
        let report = run_refresh(&db, Trigger::Manual, false, None)
            .await
            .unwrap();
        assert!(report.deduplicated);
        assert_eq!(last_run_epoch(&db), 0);
        assert!(stored_report(&db).is_none());
    }

    #[tokio::test]
    async fn invalid_routes_report_database_error_without_applying_updates() {
        let db = Database::memory().unwrap();
        crate::database::dao::routes::install_discovery_read_failure(&db);
        let report = refresh_fixture(&db, false).await;
        assert!(!report.deduplicated);
        assert!(!report.has_changes);
        assert!(!pending_restart(&db));
        assert_eq!(report.providers.len(), 1);
        assert!(!report.providers[0].ok);
        assert_eq!(report.providers[0].error.as_deref(), Some("database_error"));
        assert!(!report.providers[0].plan.has_changes());
        let stored = stored_report(&db).unwrap();
        assert!(!stored.has_changes);
        assert_eq!(stored.providers[0].error.as_deref(), Some("database_error"));
        assert_eq!(
            db.get_all_proxy_providers().unwrap()[0].models,
            vec!["minimax-m3"]
        );
        for alias in ["aaa_first_route", "blocked_route"] {
            let route = db.get_route(alias).unwrap().unwrap();
            assert_eq!(route.hops[0].model, "minimax-m3");
        }
    }

    #[tokio::test]
    async fn failed_transaction_reports_no_changes_or_pending_restart() {
        let db = Database::memory().unwrap();
        // Precise hops are locked and never written; the transactional
        // failure now comes from the provider pool write itself. The trigger
        // only fires for the discovery upgrade (models gaining minimax-m4),
        // so fixture setup and user edits are unaffected.
        {
            let conn = db.conn.lock().unwrap();
            conn.execute_batch(
                "CREATE TRIGGER discovery_provider_write_failure BEFORE UPDATE ON proxy_providers
                 WHEN NEW.models LIKE '%minimax-m4%'
                 BEGIN SELECT RAISE(FAIL, 'injected_provider_write_failure'); END;",
            )
            .unwrap();
        }
        let report = refresh_fixture(&db, false).await;
        assert!(!report.deduplicated);
        assert!(!report.has_changes);
        assert!(!pending_restart(&db));
        assert_eq!(report.providers.len(), 1);
        assert!(!report.providers[0].ok);
        assert_eq!(report.providers[0].error.as_deref(), Some("database_error"));
        assert!(!report.providers[0].plan.has_changes());
        assert!(!stored_report(&db).unwrap().has_changes);
        assert_eq!(
            db.get_all_proxy_providers().unwrap()[0].models,
            vec!["minimax-m3"]
        );
        for route in db.get_all_routes().unwrap() {
            assert_eq!(route.hops[0].model, "minimax-m3");
        }
    }

    #[tokio::test]
    async fn opt_out_while_fetching_returns_conflict_and_preserves_user_edit() {
        let db = Database::memory().unwrap();
        let report = refresh_fixture(&db, true).await;
        assert!(!report.deduplicated);
        assert!(!report.has_changes);
        assert!(!pending_restart(&db));
        assert_eq!(
            report.providers[0].error.as_deref(),
            Some(apply::PROVIDER_CONFLICT)
        );
        assert!(!report.providers[0].plan.has_changes());
        let current = db.get_all_proxy_providers().unwrap().remove(0);
        assert!(!current.auto_track_models);
        assert_eq!(current.name, "user edited while fetching");
        assert_eq!(current.models, vec!["minimax-m3"]);
    }
}

/// Opt-in live probe against the real on-disk database (`~/.aag/aag.db`).
/// Doubly gated — `#[ignore]` plus an environment value — so the normal
/// suite never touches live configuration. `AAG_MODEL_DISCOVERY_PROBE=dry`
/// fetches upstream catalogs and prints the plans without writing;
/// `=apply` runs the real never-downgrade pass after taking a consistent
/// `VACUUM INTO` backup. Keys and base URLs are never printed.
///
/// ```text
/// cargo test --locked --offline --lib services::model_discovery::live_probe \
///   -- --ignored --nocapture --test-threads=1
/// ```
#[cfg(test)]
mod live_probe {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    #[ignore = "live probe against the real database; opt in with AAG_MODEL_DISCOVERY_PROBE=dry|apply"]
    async fn live_discovery_probe() {
        let mode = std::env::var("AAG_MODEL_DISCOVERY_PROBE").unwrap_or_default();
        if mode != "dry" && mode != "apply" {
            eprintln!("skip: set AAG_MODEL_DISCOVERY_PROBE=dry|apply to run the live probe");
            return;
        }

        let db = Database::init().expect("open live database");
        {
            let conn = db.conn.lock().unwrap();
            conn.busy_timeout(Duration::from_secs(15))
                .expect("set busy timeout");
        }

        let backup_dir = crate::config::get_backups_dir().expect("backups dir");
        std::fs::create_dir_all(&backup_dir).expect("create backups dir");
        let backup_path = backup_dir.join(format!(
            "pre_discovery_probe_{}.db",
            chrono::Utc::now().format("%Y%m%dT%H%M%S")
        ));
        {
            let conn = db.conn.lock().unwrap();
            conn.execute(
                "VACUUM INTO ?1",
                rusqlite::params![backup_path.to_string_lossy().as_ref()],
            )
            .expect("VACUUM INTO backup");
        }
        println!("backup: {}", backup_path.display());

        if mode == "apply" {
            let report = run_refresh(&db, Trigger::Manual, false, None)
                .await
                .expect("model discovery failed");
            println!(
                "\n=== apply pass: has_changes={} deduplicated={} providers={}",
                report.has_changes,
                report.deduplicated,
                report.providers.len()
            );
            for pr in &report.providers {
                match (&pr.error, pr.plan.has_changes()) {
                    (Some(e), _) => println!("\n[{}] fetch/apply failed: {}", pr.provider_id, e),
                    (None, false) => println!("\n[{}] ok, no changes", pr.provider_id),
                    (None, true) => {
                        println!("\n[{}] applied:", pr.provider_id);
                        print_plan(&pr.plan);
                    }
                }
            }
        } else {
            let providers = db.get_all_proxy_providers().unwrap_or_default();
            let routes = db.get_all_routes().unwrap_or_default();
            println!(
                "\n=== dry pass over {} eligible provider(s), {} route(s)",
                providers.iter().filter(|p| eligible_for_refresh(p)).count(),
                routes.len()
            );
            for pp in providers.iter().filter(|p| eligible_for_refresh(p)) {
                if let Err(error) =
                    crate::services::provider_admission::ensure_catalog_admitted(&db, pp).await
                {
                    println!("\n[{}] admission failed: {}", pp.id, error);
                    continue;
                }
                let auth_scheme = match crate::services::provider_admission::auth_scheme(&db, pp) {
                    Ok(scheme) => scheme,
                    Err(error) => {
                        println!("\n[{}] auth context failed: {}", pp.id, error);
                        continue;
                    }
                };
                match fetch::fetch_models(pp, auth_scheme).await {
                    Ok(fetched) => {
                        let plan = apply::plan_provider(pp, &routes, &fetched.models);
                        println!(
                            "\n[{}] upstream lists {} model(s); plan has_changes={}",
                            pp.id,
                            fetched.models.len(),
                            plan.has_changes()
                        );
                        print_plan(&plan);
                    }
                    Err(e) => println!("\n[{}] fetch failed: {}", pp.id, e),
                }
            }
        }
    }

    fn print_plan(plan: &apply::ProviderPlan) {
        if !plan.added_models.is_empty() {
            println!("  pool models added ({}):", plan.added_models.len());
            for m in &plan.added_models {
                println!("    + {m}");
            }
        }
        for h in &plan.hop_rewrites {
            println!("  hop [{}] {} -> {}", h.alias, h.old_model, h.new_model);
        }
        for m in &plan.mapping_rewrites {
            println!(
                "  mapping value {} -> {} (key kept)",
                m.old_model, m.new_model
            );
        }
        if !plan.skipped_ambiguous.is_empty() {
            println!(
                "  ambiguous anchors skipped (no unique successor): {:?}",
                plan.skipped_ambiguous
            );
        }
    }
}
