//! Immutable routing snapshots shared by HTTP and in-process gateway consumers.

use crate::database::Database;
use std::collections::HashMap;
use std::sync::Arc;

use super::transports::Transport_cache;

const SNAPSHOT_BUILD_ATTEMPTS: usize = 3;

/// Account quota is shared across protocol paths and duplicate supplier rows.
/// Credentials and origins are used only while assembling this private map;
/// the router receives opaque ordinal equality identities, never their inputs.
fn build_quota_scopes(
    rows: &[crate::database::dao::proxy_providers::ProxyProvider],
) -> HashMap<String, usize> {
    let mut resources = HashMap::new();
    let mut scopes = HashMap::new();
    for row in rows.iter().filter(|row| {
        row.enabled
            && row.provider_type != "anthropic_passthrough"
            && !row.api_key.trim().is_empty()
    }) {
        let Ok(url) = reqwest::Url::parse(&row.base_url) else {
            continue;
        };
        if !matches!(url.scheme(), "http" | "https") {
            continue;
        }
        let next = resources.len();
        let scope = *resources
            .entry((url.origin().ascii_serialization(), row.api_key.clone()))
            .or_insert(next);
        scopes.insert(row.id.clone(), scope);
    }
    scopes
}

#[derive(Clone)]
#[allow(non_camel_case_types)]
pub(crate) struct Runtime_snapshot {
    pub router: Arc<crate::proxy::router::Router>,
    pub db_providers: Vec<crate::database::dao::proxy_providers::ProxyProvider>,
    pub queue_count: usize,
    pub routing_revision: u64,
    pub generation: u64,
    pub pinned_provider: Option<String>,
    pub passthrough_auth_allowed: bool,
    pub transports: Transport_cache,
    // Reused across generations to serialize status refreshes for one provider
    // without blocking independent providers or already admitted streams.
    admission_locks: HashMap<String, Arc<tokio::sync::Mutex<()>>>,
}

pub(crate) async fn build_runtime_snapshot(
    db: &Database,
    config: &crate::database::dao::proxy_config::ProxyConfig,
) -> Result<Runtime_snapshot, String> {
    build_runtime_snapshot_from(db, config, None).await
}

pub(crate) async fn build_runtime_snapshot_from(
    db: &Database,
    config: &crate::database::dao::proxy_config::ProxyConfig,
    previous: Option<&Runtime_snapshot>,
) -> Result<Runtime_snapshot, String> {
    crate::services::provider_admission::refresh_runtime_subscriptions(db).await?;
    build_persisted_admission_snapshot_from(db, config, previous).await
}

/// Assemble only persisted admission. Singular recheck reconciliation must not
/// refresh, renew or revoke unselected providers while publishing its result.
pub(super) async fn build_persisted_admission_snapshot_from(
    db: &Database,
    config: &crate::database::dao::proxy_config::ProxyConfig,
    previous: Option<&Runtime_snapshot>,
) -> Result<Runtime_snapshot, String> {
    for _ in 0..SNAPSHOT_BUILD_ATTEMPTS {
        let routing_revision = db.routing_revision().map_err(String::from)?;
        let mut transports = Transport_cache::default();
        let old_transports = previous.map(|snapshot| &snapshot.transports);
        let (mut providers, mut routes, db_providers, source_identities) =
            build_providers_and_routes(db, config, &mut transports, old_transports).await?;
        let (queue_ids, queues, queue_routes, queue_weights, queue_sources) =
            super::queues::build_failover_queues_cached(
                db,
                config,
                &mut providers,
                &mut transports,
                old_transports,
            )?;
        for (alias, hops) in queue_routes {
            routes.entry(alias).or_insert(hops);
        }
        let pinned_provider = db
            .get_setting(crate::database::dao::settings::KEY_PROXY_PINNED_PROVIDER)
            .map_err(String::from)?
            .filter(|id| !id.trim().is_empty());
        // Plan capability aliases from the same pure reads as the rest of the
        // snapshot; the revision re-check below covers these reads too.
        let admitted_credentialless_provider_ids: std::collections::HashSet<String> = providers
            .iter()
            .filter(|(_, provider)| provider.enabled() && !provider.requires_client_credential())
            .map(|(id, _)| id.clone())
            .collect();
        let planner = super::planner::build_model_planner_for_runtime(
            db,
            &db_providers,
            &admitted_credentialless_provider_ids,
        )
        .map_err(|error| error.to_string())?;
        let desktop_default_model =
            select_desktop_default_model(db, &providers, &db_providers, &routes, planner.as_ref())?;
        let (mirror_roles_enabled, mirror_role_routes) =
            crate::services::mirror_models::build_mirror_role_routes(
                db,
                &providers,
                planner.as_ref(),
            )
            .map_err(String::from)?;
        let passthrough_auth_allowed = crate::proxy::server::host_is_loopback(&config.host)
            && db_providers.iter().any(|provider| {
                provider.enabled
                    && provider.provider_type == "anthropic_passthrough"
                    && providers.contains_key(&provider.id)
            });
        if routing_revision != db.routing_revision().map_err(String::from)? {
            continue;
        }
        if db_providers.iter().any(|provider| {
            providers.contains_key(&provider.id)
                && crate::services::provider_admission::source_identity(db, provider).as_ref()
                    != source_identities.get(&provider.id)
        }) {
            continue;
        }
        let queue_count = if crate::services::provider_admission::enforcement_enabled(db) {
            providers
                .iter()
                .flat_map(|(id, provider)| provider.models().iter().map(move |model| (id, model)))
                .collect::<std::collections::HashSet<_>>()
                .len()
        } else {
            queue_ids.len()
        };
        // Queue spread mode: a settings-level routing input (in the routing
        // fingerprint), decoded leniently — unknown values are `order`.
        let routing_mode = crate::proxy::router::Routing_mode::parse(
            db.get_setting(crate::database::dao::settings::KEY_ROUTING_MODE)
                .map_err(String::from)?
                .as_deref(),
        );
        // Same-tier native-protocol preference: on unless explicitly zeroed.
        let native_protocol_preference = db
            .get_setting("native_protocol_preference")
            .map_err(String::from)?
            .as_deref()
            .is_none_or(|value| value != "0");
        let mut provider_weights: std::collections::HashMap<String, u32> = db_providers
            .iter()
            .map(|pp| (pp.id.clone(), pp.weight))
            .collect();
        // Queue hops use synthesized `queue-*` ids; their spread weights come
        // from the linked pool row (see the queue builder). Extend rather than
        // replace so pool rows keep their own weights for direct routes.
        provider_weights.extend(queue_weights);
        let quota_scopes = build_quota_scopes(&db_providers);
        let admission_locks: HashMap<_, _> = db_providers
            .iter()
            .map(|provider| {
                let lock = previous
                    .and_then(|snapshot| snapshot.admission_locks.get(&provider.id))
                    .cloned()
                    .unwrap_or_else(|| Arc::new(tokio::sync::Mutex::new(())));
                (provider.id.clone(), lock)
            })
            .collect();
        let gate_rows: Arc<HashMap<_, _>> = Arc::new(
            db_providers
                .iter()
                .map(|provider| (provider.id.clone(), provider.clone()))
                .collect(),
        );
        let gate_sources = Arc::new(source_identities);
        let gate_locks = Arc::new(admission_locks.clone());
        let gate_db = db.clone();
        let admission_gate = move |provider_id: String, model: String| {
            let db = gate_db.clone();
            let rows = gate_rows.clone();
            let locks = gate_locks.clone();
            let sources = gate_sources.clone();
            async move {
                // Global screening policy does not erase the captured source
                // contract. Reject new attempts from a stale bound transport;
                // unchanged manual/synthesized providers keep off-policy use.
                if let Some(provider) = rows.get(&provider_id) {
                    if crate::services::provider_admission::source_identity(&db, provider).as_ref()
                        != sources.get(&provider_id)
                    {
                        return false;
                    }
                }
                if !crate::services::provider_admission::enforcement_enabled(&db) {
                    return true;
                }
                let Some(provider) = rows.get(&provider_id) else {
                    return false;
                };
                let Some(source) = sources.get(&provider_id) else {
                    return false;
                };
                let Some(lock) = locks.get(&provider_id) else {
                    return false;
                };
                let _guard = lock.lock().await;
                if crate::services::provider_admission::source_identity(&db, provider).as_ref()
                    != Some(source)
                {
                    return false;
                }
                crate::services::provider_admission::ensure_provider_admitted(&db, provider, &model)
                    .await
                    .is_ok()
                    && crate::services::provider_admission::source_identity(&db, provider).as_ref()
                        == Some(source)
            }
        };
        let mut router = crate::proxy::router::Router::new(providers, routes)
            .with_queue(queue_ids)
            .with_queues(queues)
            .with_queue_sources(queue_sources)
            .with_usage_db(db.clone())
            .with_planner(planner)
            .with_desktop_default_model(desktop_default_model)
            .with_mirror_role_routes(mirror_roles_enabled, mirror_role_routes)
            .with_routing_mode(routing_mode)
            .with_native_protocol_preference(native_protocol_preference)
            .with_provider_weights(provider_weights)
            .with_quota_scopes(quota_scopes)
            .with_provider_bindings(transports.source_bindings())
            .with_admission_gate(Arc::new(admission_gate));
        if let Some(previous) = previous {
            router.last_used = Arc::clone(&previous.router.last_used);
            router.affinity = Arc::clone(&previous.router.affinity);
            router.chain_spread = Arc::clone(&previous.router.chain_spread);
            router.usage_ledger = Arc::clone(&previous.router.usage_ledger);
            router.model_observations = Arc::clone(&previous.router.model_observations);
        }
        return Ok(Runtime_snapshot {
            router: Arc::new(router),
            db_providers,
            queue_count,
            routing_revision,
            generation: previous.map_or(1, |snapshot| snapshot.generation.saturating_add(1)),
            pinned_provider,
            passthrough_auth_allowed,
            transports,
            admission_locks,
        });
    }
    Err("routing_config_changed_during_start".into())
}

/// Resolve Desktop role selectors only while assembling a routing generation.
/// A disabled/empty supplier inventory must not revive old legacy supply.
fn select_desktop_default_model(
    db: &Database,
    providers: &HashMap<String, Arc<dyn crate::proxy::providers::Provider>>,
    db_providers: &[crate::database::dao::proxy_providers::ProxyProvider],
    routes: &HashMap<String, Vec<crate::database::dao::routes::RouteHop>>,
    planner: &dyn crate::proxy::planner::Model_planner,
) -> Result<Option<String>, String> {
    let is_static_supply = |provider: &Arc<dyn crate::proxy::providers::Provider>| {
        provider.enabled() && !provider.requires_client_credential()
    };
    let has_static_route = |model: &str| {
        if let Some(hops) = routes.get(model) {
            // Explicit routes restrict the actual planned chain. A static
            // offer elsewhere cannot make an OAuth-only override usable.
            let planned = match planner.resolve(model, Default::default()) {
                Ok(crate::proxy::planner::Resolution::Precise { hops, .. })
                | Ok(crate::proxy::planner::Resolution::Alias { hops, .. }) => Some(hops),
                Ok(crate::proxy::planner::Resolution::Exact(_)) => None,
                Err(_) => return false,
            };
            return hops.iter().any(|hop| {
                let eligible = planned.as_ref().is_none_or(|planned| {
                    planned.iter().any(|candidate| {
                        candidate.provider == hop.provider && candidate.model == hop.model
                    })
                });
                eligible
                    && providers.get(&hop.provider).is_some_and(|provider| {
                        is_static_supply(provider) && provider.models().contains(&hop.model)
                    })
            });
        }
        providers.values().any(|provider| {
            is_static_supply(provider) && provider.models().iter().any(|offered| offered == model)
        })
    };
    if let Some(model) =
        crate::services::supplier_setup::selected_model(db).map_err(String::from)?
    {
        return Ok(has_static_route(&model).then_some(model));
    }
    let (_, suppliers) = db.get_supplier_snapshot().map_err(String::from)?;
    if !suppliers.is_empty() {
        return Ok(None);
    }
    Ok(db_providers
        .iter()
        .find_map(|row| {
            let provider = providers.get(&row.id)?;
            if !is_static_supply(provider) {
                return None;
            }
            provider
                .models()
                .iter()
                .find(|model| !model.trim().is_empty())
                .cloned()
        })
        .filter(|model| has_static_route(model)))
}

pub(crate) async fn build_providers_and_routes(
    db: &Database,
    config: &crate::database::dao::proxy_config::ProxyConfig,
    transports: &mut Transport_cache,
    previous: Option<&Transport_cache>,
) -> Result<
    (
        HashMap<String, Arc<dyn crate::proxy::providers::Provider>>,
        HashMap<String, Vec<crate::database::dao::routes::RouteHop>>,
        Vec<crate::database::dao::proxy_providers::ProxyProvider>,
        HashMap<String, String>,
    ),
    String,
> {
    let mut db_providers = db.get_all_proxy_providers().map_err(String::from)?;
    let admission_enforced = crate::services::provider_admission::enforcement_enabled(db);
    if admission_enforced {
        for provider in &mut db_providers {
            let admitted: std::collections::HashSet<_> =
                crate::services::provider_admission::admitted_models(db, provider)
                    .into_iter()
                    .collect();
            provider.models.retain(|model| admitted.contains(model));
        }
    }

    let mut providers: HashMap<String, Arc<dyn crate::proxy::providers::Provider>> = HashMap::new();
    let mut source_identities = HashMap::new();
    for pp in &db_providers {
        if admission_enforced && (!pp.enabled || pp.models.is_empty()) {
            continue;
        }
        // Self-loop guard: never register a pool provider whose upstream is
        // AAG's own listener. Unlike the failover-queue path, explicit routes
        // and pinned dispatch reach these providers directly, so the guard
        // must live here too (it now also catches the full-endpoint form).
        if crate::proxy::self_loop::base_url_targets_self(&pp.base_url, &config.host, config.port) {
            log::warn!(
                "proxy: skip provider_ref={} reason=self_loop",
                crate::diagnostics::safe_reference(&pp.id)
            );
            continue;
        }
        let source_identity = crate::services::provider_admission::source_identity(db, pp);
        let auth_scheme = crate::services::provider_admission::auth_scheme(db, pp)?;
        if crate::services::provider_admission::source_identity(db, pp) != source_identity {
            return Err("provider_subscription_source_changed_during_build".into());
        }
        let provider = transports.build_with_auth(pp, previous, auth_scheme)?;
        providers.insert(pp.id.clone(), provider);
        if let Some(identity) = source_identity {
            source_identities.insert(pp.id.clone(), identity);
        }
    }

    // Build routes from DB
    let db_routes = db.get_all_routes().map_err(String::from)?;
    let mut routes: HashMap<String, Vec<crate::database::dao::routes::RouteHop>> = HashMap::new();
    for route in db_routes {
        routes.insert(route.alias, route.hops);
    }

    // Auto-create routes from the active Codex provider's modelCatalog.
    // Each entry { display, model, provider } maps a "display" model name
    // (what Codex Desktop sends) to an upstream model on a specific
    // proxy_provider. This is the model-mapping feature: the user configures
    // "GPT-5 -> deepseek-v4-flash on deepseek" in the provider edit modal,
    // and the proxy automatically routes requests accordingly.
    // Only creates routes that don't already exist (won't overwrite manual routes).
    if let Ok(Some(cur_id)) = db.get_current_provider("codex") {
        if let Ok(Some(prov)) = db.get_provider_by_id(&cur_id, "codex") {
            if let Some(mc) = prov.settings_config.get("modelCatalog") {
                if let Some(models) = mc.get("models").and_then(|v| v.as_array()) {
                    for entry in models {
                        let display = match entry.get("display").and_then(|v| v.as_str()) {
                            Some(d) if !d.is_empty() => d.to_string(),
                            _ => continue,
                        };
                        let upstream_model = entry
                            .get("model")
                            .and_then(|v| v.as_str())
                            .unwrap_or(&display)
                            .to_string();
                        let provider_id = entry
                            .get("provider")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        if provider_id.is_empty() {
                            continue;
                        }
                        // Skip if the route already exists (manual route takes priority).
                        if routes.contains_key(&display) {
                            continue;
                        }
                        // Skip if the referenced proxy_provider doesn't exist.
                        let prov_exists = providers.contains_key(&provider_id);
                        if !prov_exists {
                            log::warn!(
                                "proxy: model catalog skip route_ref={} model_ref={} provider_ref={} reason=provider_not_found",
                                crate::diagnostics::safe_reference(&display),
                                crate::diagnostics::safe_reference(&upstream_model),
                                crate::diagnostics::safe_reference(&provider_id)
                            );
                            continue;
                        }
                        log::info!(
                            "proxy: model catalog auto-route route_ref={} provider_ref={} model_ref={}",
                            crate::diagnostics::safe_reference(&display),
                            crate::diagnostics::safe_reference(&provider_id),
                            crate::diagnostics::safe_reference(&upstream_model)
                        );
                        routes.insert(
                            display,
                            vec![crate::database::dao::routes::RouteHop {
                                provider: provider_id,
                                model: upstream_model,
                            }],
                        );
                    }
                }
            }
        }
    }

    // Auto-create routes from proxy_provider model_mapping.
    // Each proxy_provider can have a `model_mapping` field (JSON object:
    // `{ "claude-sonnet-5": "deepseek-v4-pro", ... }`). For each entry,
    // create a route: alias = display model, hop = { this provider, upstream model }.
    // Only creates routes that don't already exist (manual routes + modelCatalog
    // routes take priority).
    for pp in &db_providers {
        if pp.model_mapping.is_empty() {
            continue;
        }
        for (display, upstream_model) in &pp.model_mapping {
            if routes.contains_key(display) {
                continue;
            }
            log::info!(
                "proxy: model mapping auto-route route_ref={} provider_ref={} model_ref={}",
                crate::diagnostics::safe_reference(display),
                crate::diagnostics::safe_reference(&pp.id),
                crate::diagnostics::safe_reference(upstream_model)
            );
            routes.insert(
                display.clone(),
                vec![crate::database::dao::routes::RouteHop {
                    provider: pp.id.clone(),
                    model: upstream_model.clone(),
                }],
            );
        }
    }
    if admission_enforced {
        for hops in routes.values_mut() {
            hops.retain(|hop| {
                providers
                    .get(&hop.provider)
                    .is_some_and(|provider| provider.models().contains(&hop.model))
            });
        }
    }
    Ok((providers, routes, db_providers, source_identities))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screened_mock(db: &Database, id: &str) -> crate::config::private_fs::PrivateTempDir {
        use crate::database::dao::provider_admission::{
            Admission_evidence, Admission_probe, Admission_state,
        };
        use crate::services::provider_screening::{
            ScreeningConfig, SubscriptionEvidence, SubscriptionState,
        };
        use crate::services::subscription_source::{SourceReason, SubscriptionObservation};
        let root = crate::config::private_fs::private_temp_dir(
            &std::env::temp_dir().canonicalize().unwrap(),
        )
        .unwrap();
        let source = root.path().join("source.db");
        let connection = rusqlite::Connection::open(&source).unwrap();
        connection.execute_batch("CREATE TABLE providers(id TEXT,app_type TEXT,name TEXT,settings_config TEXT,website_url TEXT,category TEXT,created_at INTEGER,sort_index INTEGER,notes TEXT,icon TEXT,icon_color TEXT,meta TEXT,is_current INTEGER,in_failover_queue INTEGER);").unwrap();
        let settings = serde_json::json!({"env":{"ANTHROPIC_BASE_URL":"http://127.0.0.1:9123","ANTHROPIC_AUTH_TOKEN":"screening_fixture_key"}});
        connection.execute("INSERT INTO providers(id,app_type,name,settings_config,meta,is_current,in_failover_queue) VALUES('source','claude','Fixture',?1,'{}',0,0)", [settings.to_string()]).unwrap();
        let provider = serde_json::from_value::<crate::database::dao::proxy_providers::ProxyProvider>(serde_json::json!({
            "id":id,"name":id,"type":"mock","apiKey":"screening_fixture_key","baseURL":"http://127.0.0.1:9123",
            "models":["claude-opus-4-8","claude-sonnet-4-5","claude-opus-4-8"],"enabled":true
        })).unwrap();
        db.save_proxy_provider(&provider).unwrap();
        let plan = crate::services::provider_admission::bind_source(
            db,
            &provider,
            &source,
            "claude_code:source",
            true,
        )
        .unwrap();
        let original = db.provider_subscription_value(id).unwrap().unwrap();
        let mut stored: serde_json::Value = serde_json::from_str(&original).unwrap();
        let evidence = SubscriptionEvidence {
            state: SubscriptionState::NotRequired,
            provider_ref: id.into(),
            evidence_id: format!("cc_switch:{}", plan.binding),
            provenance: "explicit_test_fixture".into(),
            valid_until: None,
        };
        stored["observation"] = serde_json::to_value(SubscriptionObservation {
            evidence: evidence.clone(),
            probe_allowed: true,
            reason: SourceReason::VerifiedAllowance,
            source_binding: plan.binding,
            source_record_ref: "claude_code:source".into(),
            checked_at: chrono::Utc::now().timestamp(),
            status_http: None,
        })
        .unwrap();
        assert!(db
            .compare_exchange_provider_subscription(id, Some(&original), &stored.to_string())
            .unwrap());
        let evidence = Admission_evidence {
            revision: evidence.evidence_id,
            state: Admission_state::NotRequired,
            valid_until: None,
        };
        let policy =
            crate::services::provider_admission::policy_revision(&ScreeningConfig::default());
        let previous_version = db
            .provider_admission_record(id)
            .unwrap()
            .map(|record| record.version)
            .unwrap_or(0);
        let pending = db
            .provider_admission_begin(&provider, &evidence, &policy, previous_version)
            .unwrap();
        db.provider_admission_finish(
            &provider,
            &evidence,
            &policy,
            pending,
            Admission_probe {
                probe_model: "claude-opus-4-8".into(),
                checked_at: chrono::Utc::now().timestamp(),
                eligible: true,
                reason: "accepted".into(),
                latency_ms: Some(1),
                response: Some("ok".into()),
                response_truncated: false,
                response_redacted: false,
                upstream_status: None,
                upstream_code: None,
            },
            provider.models.clone(),
        )
        .unwrap();
        root
    }

    #[tokio::test]
    async fn enforced_runtime_retains_all_admitted_pairs_and_revocation_blocks_old_snapshot() {
        let db = Database::memory().unwrap();
        let _first = screened_mock(&db, "first");
        let _second = screened_mock(&db, "second");
        let mut legacy = crate::provider::Provider::new(
            "legacy_unscreened".into(),
            "Legacy".into(),
            serde_json::json!({"env":{"ANTHROPIC_BASE_URL":"http://127.0.0.1:9999","ANTHROPIC_AUTH_TOKEN":"unreviewed_fixture"}}),
        );
        legacy.in_failover_queue = true;
        db.save_provider("claude_code", &legacy).unwrap();
        db.set_provider_admission_enforced(true).unwrap();
        db.save_route(&crate::database::dao::routes::Route {
            alias: "manual".into(),
            description: String::new(),
            hops: vec![
                crate::database::dao::routes::RouteHop {
                    provider: "second".into(),
                    model: "claude-sonnet-4-5".into(),
                },
                crate::database::dao::routes::RouteHop {
                    provider: "first".into(),
                    model: "claude-opus-4-8".into(),
                },
            ],
        })
        .unwrap();
        let config = db.get_proxy_config().unwrap();
        let before = build_runtime_snapshot(&db, &config).await.unwrap();
        assert_eq!(
            before.queue_count, 4,
            "count exact admitted pairs, not removed legacy queue ids"
        );
        let hops = before
            .router
            .preview_model("chat", "alias:fable", Default::default(), None)
            .unwrap();
        assert_eq!(hops.len(), 4);
        assert_eq!(
            hops.iter()
                .map(|hop| (&hop.provider, &hop.model))
                .collect::<std::collections::HashSet<_>>()
                .len(),
            4
        );
        assert!(!before
            .router
            .health()
            .iter()
            .any(|row| row.id.starts_with("queue-")));
        let manual = before
            .router
            .preview_model("chat", "manual", Default::default(), None)
            .unwrap();
        assert_eq!(
            manual
                .iter()
                .map(|hop| hop.provider.as_str())
                .collect::<Vec<_>>(),
            vec!["second", "first"]
        );
        assert!(before
            .router
            .resolve_model("chat", "unoffered-model", Default::default())
            .is_err());
        let original = db.provider_subscription_value("first").unwrap().unwrap();
        let mut revoked: serde_json::Value = serde_json::from_str(&original).unwrap();
        revoked["observation"]["evidence"]["state"] = serde_json::json!("inactive");
        revoked["observation"]["probeAllowed"] = serde_json::json!(false);
        assert!(db
            .compare_exchange_provider_subscription("first", Some(&original), &revoked.to_string())
            .unwrap());
        before
            .router
            .complete_for(
                "chat",
                "admission_fixture",
                &crate::proxy::canonical::ChatCompletionRequest {
                    model: "claude-opus-4-8".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(
            before
                .router
                .last_used
                .lock()
                .unwrap()
                .get("admission_fixture")
                .map(String::as_str),
            Some("second")
        );
        let after = build_runtime_snapshot_from(&db, &config, Some(&before))
            .await
            .unwrap();
        assert_eq!(after.queue_count, 2);
        assert!(Arc::ptr_eq(
            &before.admission_locks["second"],
            &after.admission_locks["second"]
        ));
    }

    #[tokio::test]
    async fn enabling_enforcement_blocks_legacy_router_instances_before_reload() {
        let db = Database::memory().unwrap();
        let provider = serde_json::from_value(
            serde_json::json!({"id":"old","name":"Old","type":"mock","models":["m"]}),
        )
        .unwrap();
        db.save_proxy_provider(&provider).unwrap();
        let snapshot = build_runtime_snapshot(&db, &db.get_proxy_config().unwrap())
            .await
            .unwrap();
        let request = crate::proxy::canonical::ChatCompletionRequest {
            model: "m".into(),
            ..Default::default()
        };
        snapshot
            .router
            .complete_for("chat", "fixture", &request)
            .await
            .unwrap();
        db.set_provider_admission_enforced(true).unwrap();
        assert!(snapshot
            .router
            .complete_for("chat", "fixture", &request)
            .await
            .is_err());
        let enforced =
            build_runtime_snapshot_from(&db, &db.get_proxy_config().unwrap(), Some(&snapshot))
                .await
                .unwrap();
        assert_eq!(enforced.queue_count, 0);
        assert!(enforced
            .router
            .resolve_model("chat", "m", Default::default())
            .is_err());
    }

    #[tokio::test]
    async fn old_snapshot_cannot_borrow_rescreened_source_identity_with_unchanged_provider_config()
    {
        let db = Database::memory().unwrap();
        let _original_source = screened_mock(&db, "source_bound");
        db.set_provider_admission_enforced(true).unwrap();
        let config = db.get_proxy_config().unwrap();
        let before = build_runtime_snapshot(&db, &config).await.unwrap();
        let original_provider = db.get_proxy_provider("source_bound").unwrap().unwrap();
        let request = crate::proxy::canonical::ChatCompletionRequest {
            model: "claude-opus-4-8".into(),
            ..Default::default()
        };
        before
            .router
            .complete_for("chat", "source_fixture", &request)
            .await
            .unwrap();
        let _replacement_source = screened_mock(&db, "source_bound");
        assert_eq!(
            db.get_proxy_provider("source_bound").unwrap().unwrap(),
            original_provider
        );
        assert!(before
            .router
            .complete_for("chat", "source_fixture", &request)
            .await
            .is_err());
        let after = build_runtime_snapshot_from(&db, &config, Some(&before))
            .await
            .unwrap();
        after
            .router
            .complete_for("chat", "source_fixture", &request)
            .await
            .unwrap();
    }

    #[test]
    fn quota_scopes_group_protocols_and_duplicate_rows_by_effective_key_and_origin() {
        let row = |id: &str, key: &str, base: &str, kind: &str, enabled: bool| {
            serde_json::from_value(serde_json::json!({
            "id":id,"name":"fixture","type":kind,"apiKey":key,"baseURL":base,"enabled":enabled,"models":["m"]
        })).unwrap()
        };
        let rows = vec![
            row(
                "anthropic",
                "shared_fixture",
                "https://quota.example.invalid/anthropic/v1",
                "anthropic",
                true,
            ),
            row(
                "responses",
                "shared_fixture",
                "https://quota.example.invalid:443/v1/responses",
                "openai-responses",
                true,
            ),
            row(
                "duplicate",
                "shared_fixture",
                "https://quota.example.invalid/v1",
                "openai",
                true,
            ),
            row(
                "other-key",
                "independent_fixture",
                "https://quota.example.invalid/v1",
                "openai",
                true,
            ),
            row(
                "other-origin",
                "shared_fixture",
                "https://other.example.invalid/v1",
                "openai",
                true,
            ),
            row(
                "empty-key",
                "",
                "https://quota.example.invalid/v1",
                "openai",
                true,
            ),
            row(
                "passthrough",
                "shared_fixture",
                "https://quota.example.invalid/v1",
                "anthropic_passthrough",
                true,
            ),
            row(
                "disabled",
                "shared_fixture",
                "https://quota.example.invalid/v1",
                "openai",
                false,
            ),
        ];
        let scopes = build_quota_scopes(&rows);
        assert_eq!(scopes["anthropic"], scopes["responses"]);
        assert_eq!(scopes["anthropic"], scopes["duplicate"]);
        assert_ne!(scopes["anthropic"], scopes["other-key"]);
        assert_ne!(scopes["anthropic"], scopes["other-origin"]);
        for excluded in ["empty-key", "passthrough", "disabled"] {
            assert!(!scopes.contains_key(excluded));
        }
    }
    use crate::database::dao::proxy_config::ProxyConfig;

    #[tokio::test]
    async fn desktop_default_is_frozen_with_the_routing_generation() {
        use crate::services::suppliers::{save_supplier, SupplierSaveInput};
        let db = Database::memory().unwrap();
        let input: SupplierSaveInput = serde_json::from_value(serde_json::json!({
            "expectedGeneration": 0, "name": "Desktop fixture",
            "baseUrl": "https://provider.example.invalid", "apiKey": "fixture",
            "endpoints": [{"protocol":"openai-chat", "models":[
                {"modelId":"first-model"}, {"modelId":"second-model"}
            ]}]
        }))
        .unwrap();
        save_supplier(&db, input).unwrap();
        let config = ProxyConfig::default();
        db.set_setting("unified_default_model", "first-model")
            .unwrap();
        let first = build_runtime_snapshot(&db, &config).await.unwrap();
        assert_eq!(first.router.desktop_default_model(), Some("first-model"));
        db.set_setting("unified_default_model", "second-model")
            .unwrap();
        assert_ne!(first.routing_revision, db.routing_revision().unwrap());
        let second = build_runtime_snapshot_from(&db, &config, Some(&first))
            .await
            .unwrap();
        assert_eq!(second.router.desktop_default_model(), Some("second-model"));
        assert_eq!(first.router.desktop_default_model(), Some("first-model"));
        assert_eq!(second.generation, first.generation + 1);
    }

    #[tokio::test]
    async fn desktop_legacy_fallback_does_not_revive_supply_when_a_supplier_exists() {
        use crate::services::suppliers::{save_supplier, SupplierSaveInput};
        let db = Database::memory().unwrap();
        let legacy = serde_json::from_value(serde_json::json!({
            "id":"legacy", "name":"Legacy fixture", "type":"mock", "models":["legacy-model"]
        }))
        .unwrap();
        db.save_proxy_provider(&legacy).unwrap();
        let config = ProxyConfig::default();
        let before = build_runtime_snapshot(&db, &config).await.unwrap();
        assert_eq!(before.router.desktop_default_model(), Some("legacy-model"));
        let input: SupplierSaveInput = serde_json::from_value(serde_json::json!({
            "expectedGeneration": 0, "name": "Disabled fixture", "enabled":false,
            "baseUrl":"https://provider.example.invalid", "endpoints":[]
        }))
        .unwrap();
        save_supplier(&db, input).unwrap();
        let after = build_runtime_snapshot(&db, &config).await.unwrap();
        assert_eq!(after.router.desktop_default_model(), None);
    }

    #[tokio::test]
    async fn desktop_default_requires_a_viable_static_hop_in_an_explicit_route() {
        use crate::database::dao::routes::{Route, RouteHop};
        use crate::services::suppliers::{save_supplier, SupplierSaveInput};
        let db = Database::memory().unwrap();
        let input: SupplierSaveInput = serde_json::from_value(serde_json::json!({
            "expectedGeneration":0, "name":"Static fixture",
            "baseUrl":"https://provider.example.invalid", "apiKey":"fixture",
            "endpoints":[{"protocol":"openai-chat", "models":[{"modelId":"selected-model"}]}]
        }))
        .unwrap();
        save_supplier(&db, input).unwrap();
        let static_provider = crate::services::suppliers::resolved_supplier_snapshot(&db)
            .unwrap()
            .endpoints[0]
            .legacy_proxy_provider_id
            .clone()
            .unwrap();
        let passthrough = serde_json::from_value(serde_json::json!({
            "id":"oauth-only", "name":"OAuth fixture", "type":"anthropic_passthrough",
            "baseURL":"https://oauth.example.invalid", "models":["selected-model"]
        }))
        .unwrap();
        db.save_proxy_provider(&passthrough).unwrap();
        let mut route = Route {
            alias: "selected-model".into(),
            description: String::new(),
            hops: vec![RouteHop {
                provider: "oauth-only".into(),
                model: "selected-model".into(),
            }],
        };
        db.save_route(&route).unwrap();
        let config = ProxyConfig::default();
        let oauth_only = build_runtime_snapshot(&db, &config).await.unwrap();
        assert_eq!(oauth_only.router.desktop_default_model(), None);

        // A usable fallback in the same actual chain restores availability.
        route.hops.push(RouteHop {
            provider: static_provider.clone(),
            model: "selected-model".into(),
        });
        db.save_route(&route).unwrap();
        let static_fallback = build_runtime_snapshot(&db, &config).await.unwrap();
        assert_eq!(
            static_fallback.router.desktop_default_model(),
            Some("selected-model")
        );

        // A static provider cannot turn an unoffered hop model into supply.
        route.hops = vec![RouteHop {
            provider: static_provider,
            model: "unoffered-model".into(),
        }];
        db.save_route(&route).unwrap();
        let unoffered = build_runtime_snapshot(&db, &config).await.unwrap();
        assert_eq!(unoffered.router.desktop_default_model(), None);
    }

    /// A hot-apply rebuild must keep the spread and usage state; otherwise a
    /// settings change would silently reset rotation offsets and observed
    /// traffic accounting on the next request.
    #[tokio::test]
    async fn rebuild_carries_spread_and_usage_state() {
        let db = Database::memory().expect("memory database");
        let config = ProxyConfig::default();
        let first = build_runtime_snapshot(&db, &config)
            .await
            .expect("first snapshot");
        first
            .router
            .usage_ledger
            .lock()
            .unwrap()
            .record("provider-a", 120, 30);
        assert_eq!(
            first
                .router
                .usage_ledger
                .lock()
                .unwrap()
                .tokens("provider-a"),
            150
        );

        let second = build_runtime_snapshot_from(&db, &config, Some(&first))
            .await
            .expect("rebuilt snapshot");
        assert!(
            Arc::ptr_eq(&first.router.usage_ledger, &second.router.usage_ledger),
            "the usage ledger must be the same handle across generations"
        );
        assert_eq!(
            second
                .router
                .usage_ledger
                .lock()
                .unwrap()
                .tokens("provider-a"),
            150,
            "a rebuild must not reset observed usage"
        );
        assert!(
            Arc::ptr_eq(&first.router.chain_spread, &second.router.chain_spread),
            "spread state keeps its existing carry-over contract"
        );
        assert!(
            Arc::ptr_eq(
                &first.router.model_observations,
                &second.router.model_observations
            ),
            "model health observations keep the shared owner across generations"
        );
    }

    #[tokio::test]
    async fn mirror_role_snapshot_freezes_assessed_tiers_and_refreshes_disabled_supply() {
        use crate::proxy::router::SURFACE_CODEX;
        use crate::services::suppliers::{save_supplier, SupplierSaveInput};
        use serde_json::json;
        let db = Database::memory().unwrap();
        let first: SupplierSaveInput = serde_json::from_value(json!({
            "expectedGeneration":0,"name":"priority backup","priority":0,
            "baseUrl":"https://backup.example.invalid","apiKey":"fixture",
            "endpoints":[{"protocol":"openai-chat","models":[{"modelId":"backup-text","tierOverride":0}]}]
        }))
        .unwrap();
        save_supplier(&db, first).unwrap();
        let selected: SupplierSaveInput = serde_json::from_value(json!({
            "expectedGeneration":1,"name":"selected model","priority":10,
            "baseUrl":"https://selected.example.invalid","apiKey":"fixture",
            "endpoints":[{"protocol":"openai-chat","models":[{"modelId":"MiniMax-M3.1-Flash","tierOverride":3}]}]
        }))
        .unwrap();
        let selected_id = save_supplier(&db, selected).unwrap().supplier_id;
        db.set_setting(crate::services::mirror_models::KEY_ENABLED, "1")
            .unwrap();
        db.set_setting(
            crate::services::supplier_setup::KEY_UNIFIED_DEFAULT_MODEL,
            "MiniMax-M3.1-Flash",
        )
        .unwrap();
        let config = ProxyConfig::default();
        let before = build_runtime_snapshot(&db, &config).await.unwrap();
        let models = |snapshot: &Runtime_snapshot| {
            snapshot
                .router
                .resolve_model(SURFACE_CODEX, "gpt-5.6-sol", Default::default())
                .unwrap()
                .iter()
                .map(|hop| hop.model.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(models(&before), ["MiniMax-M3.1-Flash", "backup-text"]);
        let disabled: SupplierSaveInput = serde_json::from_value(json!({
            "id":selected_id,"expectedGeneration":2,"name":"selected model","enabled":false
        }))
        .unwrap();
        save_supplier(&db, disabled).unwrap();
        let after = build_runtime_snapshot_from(&db, &config, Some(&before))
            .await
            .unwrap();
        assert_eq!(
            models(&before),
            ["MiniMax-M3.1-Flash", "backup-text"],
            "in-flight generation remains frozen"
        );
        assert_eq!(
            models(&after),
            ["backup-text"],
            "disabled supplier projection cannot revive as legacy supply"
        );
        assert_eq!(
            after.router.mirror_role_models(SURFACE_CODEX, None).len(),
            3
        );
    }

    #[tokio::test]
    async fn enabled_mirror_roles_without_supply_remain_unavailable() {
        let db = Database::memory().unwrap();
        db.set_setting(crate::services::mirror_models::KEY_ENABLED, "1")
            .unwrap();
        let snapshot = build_runtime_snapshot(&db, &ProxyConfig::default())
            .await
            .unwrap();
        assert!(snapshot.router.mirror_roles_enabled());
        assert!(snapshot
            .router
            .mirror_role_models(crate::proxy::router::SURFACE_CODEX, None)
            .is_empty());
        assert_eq!(
            snapshot
                .router
                .resolve_model(
                    crate::proxy::router::SURFACE_CODEX,
                    "gpt-5.6-sol",
                    Default::default()
                )
                .unwrap_err()
                .code,
            "model_alias_no_candidates"
        );
    }
}
