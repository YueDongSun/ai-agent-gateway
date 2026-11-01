use super::{read_proxy_status, start_proxy_impl, ProxyStatusDto, RunningProxy};
use crate::database::dao::proxy_config::ProxyConfig;
use crate::database::dao::proxy_providers::{CircuitBreakerConfig, ProxyProvider};
use crate::database::dao::routes::{Route, RouteHop};
use crate::database::Database;
use crate::proxy::server::ProxyAppState;
use crate::services::tailnet_publication::Publication_service;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

const TEST_AUTH_TOKEN: &str = "proxy-test-token";

fn mock_provider(id: &str) -> ProxyProvider {
    ProxyProvider {
        id: id.to_owned(),
        name: "loopback fixture".to_owned(),
        provider_type: "mock".to_owned(),
        api_key: String::new(),
        base_url: String::new(),
        models: vec!["mock-model".to_owned()],
        enabled: true,
        timeout_ms: 60_000,
        weight: 1,
        circuit_breaker: CircuitBreakerConfig::default(),
        model_mapping: HashMap::new(),
        extra_headers: HashMap::new(),
        auto_track_models: true,
        icon: None,
        icon_color: None,
    }
}

fn fixture_database(provider_id: &str, host: &str) -> Database {
    let db = Database::memory().expect("in-memory database");
    db.save_proxy_config(&ProxyConfig {
        host: host.to_owned(),
        port: 0,
        auth_token: TEST_AUTH_TOKEN.to_owned(),
        enabled: true,
    })
    .expect("save loopback proxy config");
    db.save_proxy_provider(&mock_provider(provider_id))
        .expect("save mock proxy provider");
    db
}

fn fixture_route(alias: &str, provider_id: &str, model: &str) -> Route {
    Route {
        alias: alias.to_owned(),
        description: "proxy lifecycle regression fixture".to_owned(),
        hops: vec![RouteHop {
            provider: provider_id.to_owned(),
            model: model.to_owned(),
        }],
    }
}

async fn start_fixture(db: &Database) -> Mutex<Option<RunningProxy>> {
    let state = Mutex::new(None);
    start_proxy_impl(db, &state, &Publication_service::new())
        .await
        .expect("start loopback proxy fixture");
    state
}

async fn read_status(state: &Mutex<Option<RunningProxy>>, db: &Database) -> ProxyStatusDto {
    read_proxy_status(state, db)
        .await
        .expect("read proxy status")
}

async fn stop_fixture(state: &Mutex<Option<RunningProxy>>) {
    if let Some(running) = state.lock().await.take() {
        running
            .request_shutdown()
            .wait()
            .await
            .expect("gracefully stop proxy server");
    }
}

#[tokio::test]
async fn imported_context_markers_remain_configuration_not_snapshot_repairs() {
    let db = fixture_database("mock-fixture", "127.0.0.1");
    db.save_route(&fixture_route(
        "fixture-route",
        "mock-fixture",
        "claude-sonnet-5[1M]",
    ))
    .expect("save route with imported context marker");

    let state = start_fixture(&db).await;

    let route = db
        .get_route("fixture-route")
        .expect("read unchanged route")
        .expect("route exists");
    assert_eq!(route.hops[0].model, "claude-sonnet-5[1M]");

    let status = read_status(&state, &db).await;
    assert!(status.running);
    assert!(
        !status.config_changed,
        "the startup snapshot must not rewrite route configuration"
    );

    stop_fixture(&state).await;
}

#[tokio::test]
async fn saved_routes_hot_reload_without_restarting_the_listener() {
    let db = fixture_database("mock-fixture", "127.0.0.1");
    db.save_route(&fixture_route(
        "fixture-route",
        "mock-fixture",
        "mock-model",
    ))
    .expect("save initial route");

    let state = start_fixture(&db).await;
    let before = state
        .lock()
        .await
        .as_ref()
        .and_then(RunningProxy::live_router)
        .expect("initial router");

    super::save_route_impl(
        &db,
        &state,
        fixture_route("hot-route", "mock-fixture", "mock-model-hot"),
    )
    .await
    .expect("save hot route");

    let mut after = None;
    for _ in 0..50 {
        let candidate = state
            .lock()
            .await
            .as_ref()
            .and_then(RunningProxy::live_router)
            .expect("live router");
        if !Arc::ptr_eq(&before, &candidate) {
            after = Some(candidate);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let after = after.expect("hot reload published a new router");
    assert!(
        after.list_models().iter().any(|model| model == "hot-route"),
        "the hot-reloaded router must expose the newly saved route"
    );
    assert!(
        before
            .list_models()
            .iter()
            .all(|model| model != "hot-route"),
        "the old request snapshot must remain immutable"
    );
    assert!(
        !read_status(&state, &db).await.config_changed,
        "a successfully applied save must leave the gateway current"
    );

    stop_fixture(&state).await;
}

#[tokio::test]
async fn pinned_provider_changes_hot_apply_without_restart() {
    let db = fixture_database("mock-fixture", "127.0.0.1");
    let state = start_fixture(&db).await;

    super::set_pinned_provider_impl(&db, &state, "mock-fixture".to_owned())
        .await
        .expect("pin provider");

    let mut pinned = None;
    for _ in 0..50 {
        pinned = state
            .lock()
            .await
            .as_ref()
            .and_then(|running| running.live.as_ref().map(|live| live.pinned_provider()))
            .flatten();
        if pinned.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(
        pinned.as_deref(),
        Some("mock-fixture"),
        "the pin must reach the live snapshot without a restart"
    );
    assert_eq!(
        super::get_pinned_provider_impl(&state, &db)
            .await
            .expect("read pin"),
        Some("mock-fixture".to_owned())
    );
    assert!(
        !read_status(&state, &db).await.config_changed,
        "an applied pin change must leave the gateway current"
    );

    super::set_pinned_provider_impl(&db, &state, String::new())
        .await
        .expect("clear pin");
    let mut cleared = false;
    for _ in 0..50 {
        let current = state
            .lock()
            .await
            .as_ref()
            .and_then(|running| running.live.as_ref().map(|live| live.pinned_provider()))
            .flatten();
        if current.is_none() {
            cleared = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(cleared, "clearing the pin must reach the live snapshot");

    stop_fixture(&state).await;
}

#[tokio::test]
async fn persisted_pin_is_reported_while_the_proxy_is_stopped() {
    let db = fixture_database("mock-fixture", "127.0.0.1");
    let state = Mutex::new(None);

    super::set_pinned_provider_impl(&db, &state, "mock-fixture".to_owned())
        .await
        .expect("pin provider while stopped");
    assert_eq!(
        super::get_pinned_provider_impl(&state, &db)
            .await
            .expect("read pin while stopped"),
        Some("mock-fixture".to_owned()),
        "a persisted pin must stay visible while the proxy is stopped"
    );

    super::set_pinned_provider_impl(&db, &state, String::new())
        .await
        .expect("clear pin while stopped");
    assert_eq!(
        super::get_pinned_provider_impl(&state, &db)
            .await
            .expect("read cleared pin"),
        None
    );
}

#[tokio::test]
async fn route_edits_mark_status_dirty_revert_clean_and_restart_clean() {
    let db = fixture_database("mock-fixture", "127.0.0.1");
    let original_route = fixture_route("fixture-route", "mock-fixture", "mock-model");
    db.save_route(&original_route).expect("save initial route");

    let state = start_fixture(&db).await;
    let initial_status = read_status(&state, &db).await;
    assert!(initial_status.running);
    assert!(!initial_status.config_changed);

    let changed_route = fixture_route("fixture-route", "mock-fixture", "mock-model-v2");
    db.save_route(&changed_route).expect("save changed route");
    assert!(read_status(&state, &db).await.config_changed);

    db.save_route(&original_route)
        .expect("restore original route");
    assert!(!read_status(&state, &db).await.config_changed);

    stop_fixture(&state).await;
    let restarted_state = start_fixture(&db).await;
    let restarted_status = read_status(&restarted_state, &db).await;
    assert!(restarted_status.running);
    assert!(!restarted_status.config_changed);
    stop_fixture(&restarted_state).await;
}

#[tokio::test]
async fn listener_and_auth_edits_mark_status_dirty_until_reverted() {
    let db = fixture_database("mock-fixture", "127.0.0.1");
    let state = start_fixture(&db).await;
    let original_config = db.get_proxy_config().expect("read saved config");
    assert!(!read_status(&state, &db).await.config_changed);

    let mut changed_config = original_config.clone();
    changed_config.host = "127.0.0.2".to_owned();
    db.save_proxy_config(&changed_config)
        .expect("save listener host change");
    assert!(read_status(&state, &db).await.config_changed);

    changed_config = original_config.clone();
    changed_config.port = 0;
    db.save_proxy_config(&changed_config)
        .expect("save listener port change");
    assert!(read_status(&state, &db).await.config_changed);

    changed_config = original_config.clone();
    changed_config.auth_token = "changed-proxy-test-token".to_owned();
    db.save_proxy_config(&changed_config)
        .expect("save auth change");
    assert!(read_status(&state, &db).await.config_changed);

    db.save_proxy_config(&original_config)
        .expect("restore saved listener and auth config");
    assert!(!read_status(&state, &db).await.config_changed);
    stop_fixture(&state).await;
}

#[tokio::test]
async fn unreadable_runtime_settings_refuse_start_without_publishing_a_listener() {
    use crate::database::dao::settings::{
        KEY_PROXY_PINNED_PROVIDER, KEY_PROXY_RESPONSES_COT_ENABLED,
    };

    for key in [KEY_PROXY_PINNED_PROVIDER, KEY_PROXY_RESPONSES_COT_ENABLED] {
        let db = fixture_database("mock-fixture", "127.0.0.1");
        db.conn
            .lock()
            .expect("lock fixture database")
            .execute(
                "INSERT OR REPLACE INTO app_meta (key, value) VALUES (?1, ?2)",
                rusqlite::params![key, b"unreadable_setting_fixture".as_slice()],
            )
            .expect("seed unreadable setting value");
        let state = Mutex::new(None);
        let outcome = start_proxy_impl(&db, &state, &Publication_service::new()).await;
        let published = state.lock().await.is_some();
        if published {
            stop_fixture(&state).await;
        }
        assert_eq!(outcome, Err("database_error".into()), "{key}");
        assert!(
            !published,
            "failed settings reads must not publish a listener"
        );
    }
}

#[tokio::test]
async fn startup_restores_the_saved_pin_and_responses_cot_setting() {
    use crate::database::dao::settings::{
        KEY_PROXY_PINNED_PROVIDER, KEY_PROXY_RESPONSES_COT_ENABLED,
    };

    let db = fixture_database("mock-fixture", "127.0.0.1");
    db.set_setting(KEY_PROXY_PINNED_PROVIDER, "mock-fixture")
        .expect("save pinned fixture provider");
    db.set_setting(KEY_PROXY_RESPONSES_COT_ENABLED, "1")
        .expect("enable fixture responses cot");
    let state = start_fixture(&db).await;
    let (pinned, cot_enabled) = {
        let guard = state.lock().await;
        let running = guard.as_ref().expect("started fixture");
        let pinned = running.state.pinned_provider.lock().unwrap().clone();
        let cot_enabled = running
            .state
            .responses_cot_enabled
            .load(std::sync::atomic::Ordering::Relaxed);
        (pinned, cot_enabled)
    };
    stop_fixture(&state).await;
    assert_eq!(pinned.as_deref(), Some("mock-fixture"));
    assert!(cot_enabled);
}

#[tokio::test]
async fn missing_runtime_settings_keep_the_existing_startup_defaults() {
    let db = fixture_database("mock-fixture", "127.0.0.1");
    let state = start_fixture(&db).await;
    let (pinned, cot_enabled) = {
        let guard = state.lock().await;
        let running = guard.as_ref().expect("started fixture");
        let pinned = running.state.pinned_provider.lock().unwrap().clone();
        let cot_enabled = running
            .state
            .responses_cot_enabled
            .load(std::sync::atomic::Ordering::Relaxed);
        (pinned, cot_enabled)
    };
    stop_fixture(&state).await;
    assert!(pinned.is_none());
    assert!(!cot_enabled);
}

#[tokio::test]
async fn concurrent_starts_allow_one_runtime_and_reject_overwrite() {
    let db = fixture_database("mock-fixture", "127.0.0.1");
    let state = Mutex::new(None);
    let publication = Publication_service::new();
    let (first, second) = tokio::join!(
        start_proxy_impl(&db, &state, &publication),
        start_proxy_impl(&db, &state, &publication),
    );
    assert_ne!(first.is_ok(), second.is_ok());
    let loser = if first.is_err() { first } else { second };
    assert_eq!(loser.unwrap_err(), "proxy already running");

    let status = read_status(&state, &db).await;
    assert!(status.running);
    assert!(
        !status.config_changed,
        "the losing start must not overwrite the active listener config"
    );

    let router_before = state
        .lock()
        .await
        .as_ref()
        .and_then(RunningProxy::live_router)
        .expect("active router");
    assert_eq!(
        start_proxy_impl(&db, &state, &publication)
            .await
            .unwrap_err(),
        "proxy already running"
    );
    let router_after = state
        .lock()
        .await
        .as_ref()
        .and_then(RunningProxy::live_router)
        .expect("active router remains available");
    assert!(Arc::ptr_eq(&router_before, &router_after));

    stop_fixture(&state).await;
}

fn test_app_state(auth_token: &str) -> ProxyAppState {
    ProxyAppState {
        router: Arc::new(crate::proxy::router::Router::new(
            HashMap::new(),
            HashMap::new(),
        )),
        auth_token: auth_token.to_owned(),
        pinned_provider: Arc::new(std::sync::Mutex::new(None)),
        responses_cot_enabled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        passthrough_auth_allowed: false,
        db: None,
        live: None,
        allow_client_config_repair: false,
    }
}

#[tokio::test]
async fn finished_server_handle_is_reported_as_stopped() {
    let db = Database::memory().expect("in-memory database");
    let saved_config = ProxyConfig {
        host: "127.0.0.1".to_owned(),
        port: 23_456,
        auth_token: TEST_AUTH_TOKEN.to_owned(),
        enabled: true,
    };
    db.save_proxy_config(&saved_config)
        .expect("save config for finished handle");

    let handle = tokio::spawn(async { Ok(()) });
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while !handle.is_finished() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("server fixture completes promptly");
    let (shutdown, shutdown_rx) = tokio::sync::oneshot::channel();
    drop(shutdown_rx);
    let running = RunningProxy {
        task: super::Proxy_task::new(handle, shutdown),
        config: saved_config.clone(),
        configured_host: saved_config.host.clone(),
        state: test_app_state(&saved_config.auth_token),
        live: None,
    };
    let state = Mutex::new(Some(running));

    let status = read_status(&state, &db).await;
    assert!(!status.running);
    assert!(!status.config_changed);

    let running = state.lock().await.take();
    if let Some(running) = running {
        running
            .request_shutdown()
            .wait()
            .await
            .expect("completed server fixture result");
    }
}

#[tokio::test]
async fn bracketed_ipv6_configured_host_is_retained_separately_from_listener_host() {
    let db = Database::memory().expect("in-memory database");
    let saved_config = ProxyConfig {
        host: "[::1]".to_owned(),
        port: 23_456,
        auth_token: TEST_AUTH_TOKEN.to_owned(),
        enabled: true,
    };
    db.save_proxy_config(&saved_config)
        .expect("save bracketed IPv6 config");

    let (shutdown, shutdown_rx) = tokio::sync::oneshot::channel();
    let handle = tokio::spawn(async move {
        let _ = shutdown_rx.await;
        Ok(())
    });
    let running = RunningProxy {
        task: super::Proxy_task::new(handle, shutdown),
        config: ProxyConfig {
            host: "::1".to_owned(),
            ..saved_config.clone()
        },
        configured_host: saved_config.host.clone(),
        state: test_app_state(&saved_config.auth_token),
        live: None,
    };
    let state = Mutex::new(Some(running));

    let status = read_status(&state, &db).await;
    assert!(status.running);
    assert!(
        !status.config_changed,
        "the bracketed configured host should match its effective IPv6 listener"
    );

    stop_fixture(&state).await;
}

fn assert_sql_change_advances_revision(db: &Database, sql: &str) {
    let before = db
        .routing_revision()
        .expect("read revision before SQL update");
    db.conn
        .lock()
        .expect("lock in-memory database")
        .execute(sql, [])
        .unwrap_or_else(|error| panic!("execute fixture SQL {sql:?}: {error}"));
    let after = db
        .routing_revision()
        .expect("read revision after SQL update");
    assert_ne!(
        before, after,
        "routing revision must include routing input changed by {sql:?}"
    );
}

#[test]
fn routing_revision_tracks_pool_route_and_agent_routing_inputs() {
    let db = Database::memory().expect("in-memory database");
    db.save_proxy_provider(&mock_provider("revision-provider"))
        .expect("save revision provider");

    for sql in [
        "UPDATE proxy_providers SET id = 'revision-provider-renamed' WHERE id = 'revision-provider'",
        "UPDATE proxy_providers SET type = 'openai' WHERE id = 'revision-provider-renamed'",
        "UPDATE proxy_providers SET api_key = 'fixture-key' WHERE id = 'revision-provider-renamed'",
        "UPDATE proxy_providers SET base_url = 'http://127.0.0.1:9001/v1' WHERE id = 'revision-provider-renamed'",
        "UPDATE proxy_providers SET models = '[\"mock-model-v2\"]' WHERE id = 'revision-provider-renamed'",
        "UPDATE proxy_providers SET enabled = 0 WHERE id = 'revision-provider-renamed'",
        "UPDATE proxy_providers SET timeout_ms = 1234 WHERE id = 'revision-provider-renamed'",
        "UPDATE proxy_providers SET weight = 9 WHERE id = 'revision-provider-renamed'",
        "UPDATE proxy_providers SET circuit_breaker = '{\"failureThreshold\":2}' WHERE id = 'revision-provider-renamed'",
        "UPDATE proxy_providers SET model_mapping = '{\"display-model\":\"upstream-model\"}' WHERE id = 'revision-provider-renamed'",
        "UPDATE proxy_providers SET extra_headers = '{\"x-fixture\":\"value\"}' WHERE id = 'revision-provider-renamed'",
    ] {
        assert_sql_change_advances_revision(&db, sql);
    }

    db.save_route(&fixture_route(
        "revision-route",
        "revision-provider-renamed",
        "route-model",
    ))
    .expect("save revision route");
    assert_sql_change_advances_revision(
        &db,
        "UPDATE routes SET alias = 'revision-route-renamed' WHERE alias = 'revision-route'",
    );
    assert_sql_change_advances_revision(
        &db,
        "UPDATE routes SET hops = '[{\"provider\":\"revision-provider-renamed\",\"model\":\"route-model-v2\"}]' WHERE alias = 'revision-route-renamed'",
    );

    db.conn
        .lock()
        .expect("lock in-memory database")
        .execute(
            "INSERT INTO providers (
                id, app_type, name, settings_config, created_at, sort_index,
                is_current, in_failover_queue
             ) VALUES ('agent-fixture', 'codex', 'agent fixture', '{}', 1, 0, 1, 1)",
            [],
        )
        .expect("insert agent routing fixture");
    for sql in [
        "UPDATE providers SET id = 'agent-fixture-renamed' WHERE id = 'agent-fixture' AND app_type = 'codex'",
        "UPDATE providers SET settings_config = '{\"modelCatalog\":{\"models\":[]}}' WHERE id = 'agent-fixture-renamed' AND app_type = 'codex'",
        "UPDATE providers SET sort_index = 7 WHERE id = 'agent-fixture-renamed' AND app_type = 'codex'",
        "UPDATE providers SET created_at = 2 WHERE id = 'agent-fixture-renamed' AND app_type = 'codex'",
        "UPDATE providers SET in_failover_queue = 0 WHERE id = 'agent-fixture-renamed' AND app_type = 'codex'",
        "UPDATE providers SET is_current = 0 WHERE id = 'agent-fixture-renamed' AND app_type = 'codex'",
        "UPDATE providers SET app_type = 'claude_code' WHERE id = 'agent-fixture-renamed' AND app_type = 'codex'",
    ] {
        assert_sql_change_advances_revision(&db, sql);
    }
}
