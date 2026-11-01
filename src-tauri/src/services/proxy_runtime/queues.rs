//! Failover queue construction. From the `providers` table, each app's
//! in-failover-queue rows in configured (sort_index) order synthesize
//! `queue-<id>` proxy_provider entries. Includes the self-loop guard;
//! the optional cc-switch auto-sync is an explicit orchestration step in
//! `proxy_runtime::maybe_auto_sync_cc_switch` (called by `start_proxy_impl`
//! before this builder, not hidden inside it).

use crate::database::Database;
use std::collections::HashMap;
use std::sync::Arc;

/// Resolve an app provider's settings_config to its upstream coordinates
/// (base_url, api_key, provider_type) for failover-queue synthesis.
/// Returns None when the row carries no usable upstream.
pub(crate) fn queue_endpoint_of(
    app_type: &str,
    cfg: &serde_json::Value,
) -> Option<(String, String, String)> {
    // Borrow the env block: cloning it per row to read two or three string
    // fields multiplied the proxy-start cost by the env size. The codex and
    // pi/opencode arms never read it, so no allocation happens for them.
    let empty_env = serde_json::Map::new();
    let env = cfg
        .get("env")
        .and_then(serde_json::Value::as_object)
        .unwrap_or(&empty_env);
    match app_type {
        // claude_vscode rows carry the same env shape the launcher injects
        // (ANTHROPIC_BASE_URL / ANTHROPIC_AUTH_TOKEN).
        "claude_code" | "claude_vscode" => {
            let base = env.get("ANTHROPIC_BASE_URL")?.as_str()?.to_string();
            if base.is_empty() {
                return None;
            }
            let key = env
                .get("ANTHROPIC_AUTH_TOKEN")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Some((base, key, "anthropic".into()))
        }
        "codex" => {
            let document = cfg
                .get("config")
                .and_then(|value| value.as_str())
                .map(|source| source.parse::<toml_edit::DocumentMut>())
                .transpose()
                .ok()?;
            let active = document.as_ref().and_then(|document| {
                let name = document.get("model_provider")?.as_str()?;
                document.get("model_providers")?.get(name)
            });
            let base = active
                .and_then(|value| value.get("base_url"))
                .and_then(toml_edit::Item::as_str)
                .or_else(|| cfg.get("baseUrl").and_then(|value| value.as_str()))?;
            if base.is_empty() {
                return None;
            }
            let key = cfg
                .get("apiKey")
                .and_then(|value| value.as_str())
                .or_else(|| cfg.get("auth")?.get("OPENAI_API_KEY")?.as_str())
                .unwrap_or("");
            let wire = active
                .and_then(|value| value.get("wire_api"))
                .and_then(toml_edit::Item::as_str)
                .or_else(|| cfg.get("wire_api").and_then(|value| value.as_str()))
                .unwrap_or("responses");
            let provider_type = match wire {
                "responses" => "openai-responses",
                "chat" | "chat_completions" => "openai",
                _ => return None,
            };
            Some((base.to_string(), key.to_string(), provider_type.into()))
        }
        "gemini" => {
            // Gemini providers carry the cc-switch-compatible env block:
            // GOOGLE_GEMINI_BASE_URL (upstream base) + GEMINI_API_KEY. The
            // optional `api` field selects the upstream wire — "gemini"
            // (native Generative Language API, default), "anthropic", or
            // "openai" — so a Gemini CLI chain can fail over across wires.
            let base = env.get("GOOGLE_GEMINI_BASE_URL")?.as_str()?.to_string();
            if base.is_empty() {
                return None;
            }
            let key = env
                .get("GEMINI_API_KEY")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let provider_type = match cfg.get("api").and_then(|v| v.as_str()).unwrap_or("") {
                "anthropic" => "anthropic",
                "openai" => "openai",
                _ => "gemini",
            };
            Some((base, key, provider_type.into()))
        }
        "copilot" => {
            let base = env.get("COPILOT_PROVIDER_BASE_URL")?.as_str()?.to_string();
            if base.is_empty() {
                return None;
            }
            let key = env
                .get("COPILOT_PROVIDER_API_KEY")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let wire = env
                .get("COPILOT_PROVIDER_TYPE")
                .and_then(|v| v.as_str())
                .unwrap_or("openai");
            let provider_type = if wire == "anthropic" {
                "anthropic"
            } else {
                "openai"
            };
            Some((base, key, provider_type.into()))
        }
        "pi" | "cline_cli" => {
            let base = cfg.get("baseUrl")?.as_str()?.to_string();
            if base.is_empty() {
                return None;
            }
            let key = cfg
                .get("apiKey")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let provider_type = match cfg.get("api").and_then(|v| v.as_str()).unwrap_or("") {
                "anthropic-messages" => "anthropic",
                _ => "openai",
            };
            Some((base, key, provider_type.into()))
        }
        "opencode" | "kilo_cli" | "kilo_vscode" => {
            // Shared OpenCode-compatible projection: { options: { baseURL,
            // apiKey }, models: { ... } }. Anthropic providers are rare in
            // this surface; default to openai unless the npm package hints
            // otherwise.
            let options = cfg.get("options").and_then(|v| v.as_object())?;
            let base = options.get("baseURL")?.as_str()?.to_string();
            if base.is_empty() {
                return None;
            }
            let key = options
                .get("apiKey")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let provider_type = match cfg.get("npm").and_then(|v| v.as_str()).unwrap_or("") {
                npm if npm.contains("anthropic") => "anthropic",
                _ => "openai",
            };
            Some((base, key, provider_type.into()))
        }
        _ => None,
    }
}

/// Failover queue construction. Each app's in-failover-queue provider rows
/// (in configured sort_index order) become `queue-<id>` provider instances
/// and per-surface chains. Returns (legacy global queue ids, per-surface
/// queues, queue-riding model-mapping routes, spread weight per queue id,
/// and the exact runtime-id -> (app_type, source provider row) provenance
/// used for tagged health identities).
#[allow(non_camel_case_types)] // repo style: snake_case type names are allowed explicitly
pub(crate) type Failover_queues = (
    Vec<String>,
    HashMap<String, Vec<String>>,
    HashMap<String, Vec<crate::database::dao::routes::RouteHop>>,
    HashMap<String, u32>,
    HashMap<String, crate::proxy::model_health::Queue_source>,
);

#[cfg(test)]
pub(crate) fn build_failover_queues(
    db: &Database,
    config: &crate::database::dao::proxy_config::ProxyConfig,
    providers: &mut HashMap<String, Arc<dyn crate::proxy::providers::Provider>>,
) -> Result<Failover_queues, String> {
    let mut transports = super::transports::Transport_cache::default();
    build_failover_queues_cached(db, config, providers, &mut transports, None)
}

pub(crate) fn build_failover_queues_cached(
    db: &Database,
    config: &crate::database::dao::proxy_config::ProxyConfig,
    providers: &mut HashMap<String, Arc<dyn crate::proxy::providers::Provider>>,
    transports: &mut super::transports::Transport_cache,
    previous: Option<&super::transports::Transport_cache>,
) -> Result<Failover_queues, String> {
    // Synthetic app-row instances have no independent screening receipt.
    // Enforced routing uses the admitted unified pool and exact model offers.
    if crate::services::provider_admission::enforcement_enabled(db) {
        return Ok(Default::default());
    }
    // Per-app failover chains (Phase 17, cc-switch style): each app's
    // provider list (configured sort_index order, only rows with
    // in_failover_queue = 1) becomes a failover queue for its entry
    // surface. Every request always starts from the first configured
    // position — the `is_current` marker is UI/live-config state only and
    // never reorders the chain:
    //   claude_code -> SURFACE_CLAUDE  (/v1/messages)
    //   codex       -> SURFACE_CODEX  (/v1/responses)
    //   copilot|pi  -> SURFACE_CHAT   (/v1/chat/completions)
    // When no explicit route / auto-route matches a model, the request tries
    // each upstream in chain order, model passed through.
    //
    // Self-loop guard (see `proxy::self_loop`): skip providers whose
    // upstream URL targets AAG's own listen address, so a failover chain
    // can't loop incoming requests back into this same proxy.
    let mut queue_ids: Vec<String> = Vec::new();
    let mut queues: HashMap<String, Vec<String>> = HashMap::new();
    // Per-app-provider model mappings (`settings_config.modelMapping`, e.g.
    // `{"gemini-2.5-pro": "gemini-2.5-pro-preview-06-05"}`) become routes
    // alias -> single hop on the synthesized queue provider. This is the
    // provider-config-driven model mapping for agents whose client model
    // differs from the upstream's (Gemini CLI preview-version pinning,
    // cross-wire rerouting like gemini-2.5-pro -> claude-sonnet-5).
    let mut queue_routes: HashMap<String, Vec<crate::database::dao::routes::RouteHop>> =
        HashMap::new();
    // Spread weight per synthesized queue provider, keyed by its `queue-*`
    // id. The router's Weight mode looks weights up by hop provider id, so a
    // queue hop can only honor the linked upstream's weight through this map.
    let mut queue_weights: HashMap<String, u32> = HashMap::new();
    // Exact provenance per synthesized queue provider id: the app row it was
    // built from. Health observations key on this identity, so literal
    // `queue-*` runtime ids, multi-app collisions, and runtime-id renames
    // cannot merge or split observation history.
    let mut queue_sources: HashMap<String, crate::proxy::model_health::Queue_source> =
        HashMap::new();
    // The linked pool row (`ccs-<preset-id>`, the same linkage the importer
    // and the UI use) owns the enable toggle and the spread weight; the
    // per-app preset row only stages client configuration. A row whose linked
    // upstream is disabled must leave the chain entirely — otherwise the
    // synthesized queue provider (always `enabled: true`) keeps serving
    // traffic the user believed was switched off.
    let pool_by_id: HashMap<String, crate::database::dao::proxy_providers::ProxyProvider> = db
        .get_all_proxy_providers()
        .map_err(String::from)?
        .into_iter()
        .map(|pp| (pp.id.clone(), pp))
        .collect();
    let mut sources = Vec::new();
    let mut id_counts = HashMap::<String, usize>::new();
    for (app_type, surface) in [
        ("claude_code", crate::proxy::router::SURFACE_CLAUDE),
        ("claude_vscode", crate::proxy::router::SURFACE_CLAUDE),
        ("codex", crate::proxy::router::SURFACE_CODEX),
        ("gemini", crate::proxy::router::SURFACE_GEMINI),
        ("copilot", crate::proxy::router::SURFACE_CHAT),
        ("pi", crate::proxy::router::SURFACE_CHAT),
        ("opencode", crate::proxy::router::SURFACE_CHAT),
        ("kilo_cli", crate::proxy::router::SURFACE_CHAT),
        ("cline_cli", crate::proxy::router::SURFACE_CHAT),
        ("kilo_vscode", crate::proxy::router::SURFACE_CHAT),
    ] {
        for (_, prov) in db.get_all_providers(app_type).map_err(String::from)? {
            if prov.in_failover_queue {
                *id_counts.entry(prov.id.clone()).or_default() += 1;
                sources.push((app_type, surface, prov));
            }
        }
    }
    for (app_type, surface, prov) in sources {
        let linked = pool_by_id.get(&format!("ccs-{}", prov.id));
        if linked.is_some_and(|upstream| !upstream.enabled) {
            log::info!(
                "proxy: failover chain skip provider_ref={} app_type={} reason=upstream_disabled",
                crate::diagnostics::safe_reference(&prov.id),
                app_type
            );
            continue;
        }
        let weight = linked.map(|upstream| upstream.weight).unwrap_or(1);
        let (base_url, api_key, provider_type) = match queue_endpoint_of(
            app_type,
            &prov.settings_config,
        ) {
            Some(v) => v,
            None => {
                log::warn!(
                        "proxy: failover chain skip provider_ref={} app_type={} reason=upstream_unavailable",
                        crate::diagnostics::safe_reference(&prov.id),
                        app_type
                    );
                continue;
            }
        };
        // Match host:port, not the full URL — loopback aliases and
        // full-endpoint bases (e.g. `http://127.0.0.1:8989/v1/messages`)
        // all resolve to the same listener and must trip the guard.
        if crate::proxy::self_loop::base_url_targets_self(&base_url, &config.host, config.port) {
            log::warn!(
                "proxy: failover chain skip provider_ref={} app_type={} reason=self_loop",
                crate::diagnostics::safe_reference(&prov.id),
                app_type
            );
            continue;
        }
        let legacy_id = format!("queue-{}", prov.id);
        let pid = if id_counts.get(&prov.id) == Some(&1) && !providers.contains_key(&legacy_id) {
            legacy_id
        } else {
            format!("queue-{app_type}-{}", prov.id)
        };
        let pp = crate::database::dao::proxy_providers::ProxyProvider {
            id: pid.clone(),
            name: format!("{} (failover queue)", prov.name),
            provider_type,
            api_key,
            base_url,
            models: vec![],
            enabled: true,
            timeout_ms: 60_000,
            weight,
            circuit_breaker: serde_json::from_str("{}").unwrap_or_default(),
            model_mapping: Default::default(),
            extra_headers: Default::default(),
            auto_track_models: true,
            icon: None,
            icon_color: None,
        };
        let provider = transports.build(&pp, previous)?;
        if providers.insert(pid.clone(), provider).is_some() {
            return Err("proxy_queue_identity_conflict".into());
        }
        queue_weights.insert(pid.clone(), weight);
        queue_sources.insert(
            pid.clone(),
            crate::proxy::model_health::Queue_source {
                app_type: app_type.to_string(),
                source_provider_id: prov.id.clone(),
            },
        );
        if let Some(mapping) = prov
            .settings_config
            .get("modelMapping")
            .and_then(|value| value.as_object())
        {
            for (display, upstream) in mapping {
                let Some(upstream) = upstream.as_str() else {
                    return Err("proxy_queue_invalid_model_mapping".into());
                };
                if display.is_empty() || upstream.is_empty() {
                    return Err("proxy_queue_invalid_model_mapping".into());
                }
                queue_routes.entry(display.clone()).or_default().push(
                    crate::database::dao::routes::RouteHop {
                        provider: pid.clone(),
                        model: upstream.to_string(),
                    },
                );
            }
        }
        if app_type == "claude_code" {
            queue_ids.push(pid.clone());
        }
        queues.entry(surface.to_string()).or_default().push(pid);
    }
    Ok((queue_ids, queues, queue_routes, queue_weights, queue_sources))
}

#[cfg(test)]
mod failover_queue_tests {
    use super::*;
    use crate::database::dao::proxy_config::ProxyConfig;
    use crate::proxy::providers::Provider as Provider_instance;

    /// The failover chain must follow the configured (sort_index) order and
    /// the `is_current` marker must never jump to the chain head — every
    /// request starts from the first configured position, and a deliberate
    /// user switch (or a persisted failover winner) must not reorder it.
    #[test]
    fn queue_follows_configured_order_and_current_marker_never_jumps_the_head() {
        let db = Database::memory().expect("in-memory database");
        // Configured order: c (0), a (1), b (2).
        for (id, sort_index, port) in [("a", 1_usize, 9001_u16), ("b", 2, 9002), ("c", 0, 9003)] {
            let mut row = crate::provider::Provider::new(
                id.into(),
                format!("row {id}"),
                serde_json::json!({
                    "env": {
                        "ANTHROPIC_BASE_URL": format!("http://127.0.0.1:{port}"),
                        "ANTHROPIC_AUTH_TOKEN": "fixture",
                    }
                }),
            );
            row.sort_index = Some(sort_index);
            db.save_provider("claude_code", &row)
                .expect("save provider row");
        }
        // Deliberate user switch to "b": the marker is UI/live-config state
        // and must not put "b" at the head of the routing chain.
        db.set_current_provider("b", "claude_code")
            .expect("set current");

        let config = ProxyConfig {
            host: "127.0.0.1".into(),
            port: 8989,
            auth_token: String::new(),
            enabled: true,
        };
        let mut providers: HashMap<String, Arc<dyn Provider_instance>> = HashMap::new();
        let (queue_ids, queues, _, _, sources) =
            build_failover_queues(&db, &config, &mut providers).expect("build queues");

        let chain = queues
            .get(crate::proxy::router::SURFACE_CLAUDE)
            .expect("claude surface chain");
        assert_eq!(
            chain,
            &[
                "queue-c".to_string(),
                "queue-a".to_string(),
                "queue-b".to_string(),
            ],
            "chain must follow configured sort_index order, not the current marker"
        );
        assert_eq!(
            &queue_ids, chain,
            "legacy global queue mirrors the claude_code chain"
        );
        assert_eq!(
            providers.len(),
            3,
            "every queue row builds a provider instance"
        );
        assert_eq!(
            sources.get("queue-c"),
            Some(&crate::proxy::model_health::Queue_source {
                app_type: "claude_code".into(),
                source_provider_id: "c".into(),
            }),
            "every synthesized queue id carries its exact app/source provenance"
        );
    }

    /// Self-loop guard: a provider whose upstream URL targets AAG's own
    /// listener must be excluded from the chain (it would loop every client
    /// call back into the proxy until the per-hop timeout fires), and rows
    /// with `in_failover_queue = 0` stay out as well.
    #[test]
    fn queue_excludes_self_loop_and_opted_out_rows() {
        let db = Database::memory().expect("in-memory database");
        for (id, base_url, in_queue) in [
            ("ok", "http://127.0.0.1:9001", true),
            // Same host:port as the proxy config below — self-loop.
            ("loopback", "http://127.0.0.1:8989", true),
            // localhost alias of the same listener — also a self-loop.
            ("alias", "http://localhost:8989", true),
            // Fine upstream, but the user opted it out of the chain.
            ("opted_out", "http://127.0.0.1:9002", false),
        ] {
            let mut row = crate::provider::Provider::new(
                id.into(),
                format!("row {id}"),
                serde_json::json!({
                    "env": {
                        "ANTHROPIC_BASE_URL": base_url,
                        "ANTHROPIC_AUTH_TOKEN": "fixture",
                    }
                }),
            );
            row.in_failover_queue = in_queue;
            db.save_provider("claude_code", &row)
                .expect("save provider row");
        }

        let config = ProxyConfig {
            host: "127.0.0.1".into(),
            port: 8989,
            auth_token: String::new(),
            enabled: true,
        };
        let mut providers: HashMap<String, Arc<dyn Provider_instance>> = HashMap::new();
        let (_, queues, _, _, _) =
            build_failover_queues(&db, &config, &mut providers).expect("build queues");

        let chain = queues
            .get(crate::proxy::router::SURFACE_CLAUDE)
            .expect("claude surface chain");
        assert_eq!(
            chain,
            &["queue-ok".to_string()],
            "only the legitimate upstream survives the guards"
        );
        assert_eq!(
            providers.len(),
            1,
            "excluded rows build no provider instance"
        );
    }

    /// The enable toggle and the spread weight live on the linked pool row
    /// (`ccs-<preset-id>`), not on the per-app preset row: a disabled upstream
    /// leaves the chain entirely, and an enabled one carries its weight into
    /// the synthesized queue provider and the router-facing weight map.
    #[test]
    fn queue_honors_linked_upstream_enabled_and_weight() {
        let db = Database::memory().expect("in-memory database");
        for (id, sort_index, enabled, weight, port) in [
            ("on", 0_usize, true, 1_u32, 9001_u16),
            ("weighted", 1, true, 3, 9003),
            ("off", 2, false, 1, 9002),
        ] {
            let mut row = crate::provider::Provider::new(
                id.into(),
                format!("row {id}"),
                serde_json::json!({
                    "env": {
                        "ANTHROPIC_BASE_URL": format!("http://127.0.0.1:{port}"),
                        "ANTHROPIC_AUTH_TOKEN": "fixture",
                    }
                }),
            );
            row.sort_index = Some(sort_index);
            db.save_provider("claude_code", &row)
                .expect("save provider row");
            db.save_proxy_provider(&crate::database::dao::proxy_providers::ProxyProvider {
                id: format!("ccs-{id}"),
                name: format!("upstream {id}"),
                provider_type: "anthropic".into(),
                api_key: "fixture".into(),
                base_url: format!("http://127.0.0.1:{port}"),
                models: vec![],
                enabled,
                timeout_ms: 60_000,
                weight,
                circuit_breaker: Default::default(),
                model_mapping: Default::default(),
                extra_headers: Default::default(),
                auto_track_models: true,
                icon: None,
                icon_color: None,
            })
            .expect("save upstream row");
        }

        let config = ProxyConfig {
            host: "127.0.0.1".into(),
            port: 8989,
            auth_token: String::new(),
            enabled: true,
        };
        let mut providers: HashMap<String, Arc<dyn Provider_instance>> = HashMap::new();
        let (_, queues, _, weights, sources) =
            build_failover_queues(&db, &config, &mut providers).expect("build queues");
        assert_eq!(
            sources.get("queue-on").map(|source| source.app_type.as_str()),
            Some("claude_code"),
            "linked members carry their app provenance too"
        );

        let chain = queues
            .get(crate::proxy::router::SURFACE_CLAUDE)
            .expect("claude surface chain");
        assert_eq!(
            chain,
            &["queue-on".to_string(), "queue-weighted".to_string()],
            "a disabled linked upstream must leave the chain"
        );
        assert_eq!(
            weights.get("queue-weighted"),
            Some(&3),
            "the linked upstream weight must reach the queue provider"
        );
        assert_eq!(weights.get("queue-on"), Some(&1));
        assert!(!weights.contains_key("queue-off"));
    }

    /// Multi-app membership of the same provider row id synthesizes distinct
    /// runtime ids with distinct provenance: the health identities of
    /// `queue-claude_code-x` and `queue-codex-x` never merge, while a
    /// single-app member keeps the plain `queue-x` id.
    #[test]
    fn multi_app_members_carry_disambiguated_ids_and_provenance() {
        let db = Database::memory().expect("in-memory database");
        for (app_type, port) in [("claude_code", 9001_u16), ("codex", 9002)] {
            let settings = match app_type {
                "claude_code" => serde_json::json!({
                    "env": {
                        "ANTHROPIC_BASE_URL": format!("http://127.0.0.1:{port}"),
                        "ANTHROPIC_AUTH_TOKEN": "fixture",
                    }
                }),
                _ => serde_json::json!({
                    "config": format!(
                        "model_provider = \"p\"\n[model_providers.p]\nbase_url = \"http://127.0.0.1:{port}\"\nwire_api = \"chat\"\n"
                    ),
                    "apiKey": "fixture",
                }),
            };
            let mut row = crate::provider::Provider::new(
                "shared".into(),
                format!("row {app_type} shared"),
                settings,
            );
            row.in_failover_queue = true;
            db.save_provider(app_type, &row).expect("save provider row");
        }
        let config = ProxyConfig {
            host: "127.0.0.1".into(),
            port: 8989,
            auth_token: String::new(),
            enabled: true,
        };
        let mut providers: HashMap<String, Arc<dyn Provider_instance>> = HashMap::new();
        let (_, queues, _, _, sources) =
            build_failover_queues(&db, &config, &mut providers).expect("build queues");

        let claude = queues
            .get(crate::proxy::router::SURFACE_CLAUDE)
            .expect("claude chain");
        assert_eq!(claude, &["queue-claude_code-shared".to_string()]);
        let codex = queues
            .get(crate::proxy::router::SURFACE_CODEX)
            .expect("codex chain");
        assert_eq!(codex, &["queue-codex-shared".to_string()]);
        let claude_source = sources
            .get("queue-claude_code-shared")
            .expect("claude provenance");
        assert_eq!(
            claude_source.identity(),
            crate::proxy::model_health::Health_identity::Queue {
                app_type: "claude_code".into(),
                source_provider_id: "shared".into(),
            }
        );
        let codex_source = sources
            .get("queue-codex-shared")
            .expect("codex provenance");
        assert_eq!(codex_source.app_type, "codex");
        assert_eq!(codex_source.source_provider_id, "shared");
        assert_ne!(
            claude_source.identity(),
            codex_source.identity(),
            "multi-app collision on one row id must not merge health histories"
        );
    }
}
