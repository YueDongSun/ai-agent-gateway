use super::{
    dsh_gateway_snapshot, read_proxy_status, start_proxy_impl, stop_proxy_impl, RunningProxy,
};
use crate::database::dao::proxy_config::ProxyConfig;
use crate::database::Database;
use crate::proxy::server::ProxyAppState;
use crate::services::tailnet_publication::Publication_service;
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::sync::{oneshot, Mutex};

#[allow(non_camel_case_types)]
struct Drain_fixture {
    db: Database,
    state: Arc<Mutex<Option<RunningProxy>>>,
    shutdown_observed: oneshot::Receiver<()>,
    release_drain: oneshot::Sender<()>,
    publication: Arc<Publication_service>,
}

fn drain_fixture(outcome: Result<(), String>, accept_shutdown_signal: bool) -> Drain_fixture {
    let db = Database::memory().unwrap();
    let config = ProxyConfig {
        host: "127.0.0.1".into(),
        port: 0,
        auth_token: "fixture_token".into(),
        enabled: true,
    };
    db.save_proxy_config(&config).unwrap();
    let (shutdown, shutdown_receiver) = oneshot::channel();
    let shutdown_receiver = accept_shutdown_signal.then_some(shutdown_receiver);
    let (observed_sender, shutdown_observed) = oneshot::channel();
    let (release_drain, drain_receiver) = oneshot::channel();
    let handle = tokio::spawn(async move {
        if let Some(shutdown_receiver) = shutdown_receiver {
            let _ = shutdown_receiver.await;
        }
        let _ = observed_sender.send(());
        let _ = drain_receiver.await;
        outcome
    });
    let running = RunningProxy {
        task: super::Proxy_task::new(handle, shutdown),
        config: config.clone(),
        configured_host: config.host.clone(),
        state: ProxyAppState {
            router: Arc::new(crate::proxy::router::Router::new(
                HashMap::new(),
                HashMap::new(),
            )),
            auth_token: config.auth_token,
            pinned_provider: Arc::new(std::sync::Mutex::new(None)),
            responses_cot_enabled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            passthrough_auth_allowed: false,
            db: None,
            live: None,
            allow_client_config_repair: false,
        },
        live: None,
    };
    Drain_fixture {
        db,
        state: Arc::new(Mutex::new(Some(running))),
        shutdown_observed,
        release_drain,
        publication: Arc::new(Publication_service::new()),
    }
}

#[tokio::test]
async fn proxy_stop_drain_keeps_status_responsive_and_rejects_restart() {
    let fixture = drain_fixture(Ok(()), true);
    let stop_state = Arc::clone(&fixture.state);
    let publication = fixture.publication.clone();
    let stop = tokio::spawn(async move { stop_proxy_impl(&stop_state, &publication).await });
    tokio::time::timeout(Duration::from_secs(2), fixture.shutdown_observed)
        .await
        .expect("shutdown observed promptly")
        .unwrap();
    let status = tokio::time::timeout(
        Duration::from_millis(200),
        read_proxy_status(&fixture.state, &fixture.db),
    )
    .await;
    let restart = tokio::time::timeout(
        Duration::from_millis(200),
        start_proxy_impl(&fixture.db, &fixture.state, &fixture.publication),
    )
    .await;
    let router_available = tokio::time::timeout(Duration::from_millis(200), async {
        fixture
            .state
            .lock()
            .await
            .as_ref()
            .and_then(RunningProxy::live_router)
            .is_some()
    })
    .await;
    let gateway = tokio::time::timeout(
        Duration::from_millis(200),
        dsh_gateway_snapshot(&fixture.state),
    )
    .await;
    fixture.release_drain.send(()).unwrap();
    let stopped = tokio::time::timeout(Duration::from_secs(2), stop)
        .await
        .expect("stop completes after drain")
        .unwrap();
    assert!(stopped.is_ok());
    assert!(
        status
            .expect("status must not await drain")
            .unwrap()
            .running
    );
    assert_eq!(
        restart.expect("start must reject without awaiting drain"),
        Err("proxy already running".into())
    );
    assert!(!router_available.expect("router lookup must not await drain"));
    assert!(gateway
        .expect("gateway lookup must not await drain")
        .unwrap()
        .is_none());
    assert!(fixture.state.lock().await.is_none());
}

#[tokio::test]
async fn proxy_stop_drain_concurrent_stops_share_the_terminal_error() {
    let fixture = drain_fixture(Err("proxy_server_failed".into()), true);
    let stop_state = Arc::clone(&fixture.state);
    let publication = fixture.publication.clone();
    let first = tokio::spawn(async move { stop_proxy_impl(&stop_state, &publication).await });
    tokio::time::timeout(Duration::from_secs(2), fixture.shutdown_observed)
        .await
        .unwrap()
        .unwrap();
    let second = stop_proxy_impl(&fixture.state, &fixture.publication);
    tokio::pin!(second);
    assert!(futures::poll!(&mut second).is_pending());
    fixture.release_drain.send(()).unwrap();
    let (first, second) = tokio::time::timeout(Duration::from_secs(2), async {
        tokio::join!(first, second)
    })
    .await
    .unwrap();
    assert_eq!(first.unwrap(), Err("proxy_server_failed".into()));
    assert_eq!(second, Err("proxy_server_failed".into()));
    assert!(fixture.state.lock().await.is_none());
}

#[tokio::test]
async fn proxy_stop_drain_cancelled_stop_retains_execution_ownership() {
    let fixture = drain_fixture(Ok(()), true);
    let stop_state = Arc::clone(&fixture.state);
    let publication = fixture.publication.clone();
    let first = tokio::spawn(async move { stop_proxy_impl(&stop_state, &publication).await });
    tokio::time::timeout(Duration::from_secs(2), fixture.shutdown_observed)
        .await
        .unwrap()
        .unwrap();
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    assert_eq!(
        start_proxy_impl(&fixture.db, &fixture.state, &fixture.publication).await,
        Err("proxy already running".into())
    );
    let later = stop_proxy_impl(&fixture.state, &fixture.publication);
    tokio::pin!(later);
    assert!(futures::poll!(&mut later).is_pending());
    fixture.release_drain.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(2), later)
        .await
        .unwrap()
        .unwrap();
    assert!(fixture.state.lock().await.is_none());
}

#[tokio::test]
async fn proxy_stop_drain_completed_stop_cannot_remove_a_replacement() {
    let fixture = drain_fixture(Ok(()), true);
    let stop = stop_proxy_impl(&fixture.state, &fixture.publication);
    tokio::pin!(stop);
    assert!(futures::poll!(&mut stop).is_pending());
    tokio::time::timeout(Duration::from_secs(2), fixture.shutdown_observed)
        .await
        .unwrap()
        .unwrap();
    let mut guard = fixture.state.lock().await;
    let mut completion = guard.as_mut().unwrap().task.request_shutdown();
    fixture.release_drain.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(2), completion.wait())
        .await
        .unwrap()
        .unwrap();
    let replacement = drain_fixture(Ok(()), true);
    let replacement_running = replacement.state.lock().await.take().unwrap();
    let replacement_router = replacement_running.live_router().unwrap();
    assert!(!replacement_running.task.matches_shutdown(&completion));
    *guard = Some(replacement_running);
    drop(guard);
    tokio::time::timeout(Duration::from_secs(2), stop)
        .await
        .unwrap()
        .unwrap();
    let current_router = fixture
        .state
        .lock()
        .await
        .as_ref()
        .and_then(RunningProxy::live_router)
        .expect("replacement runtime must remain available");
    assert!(Arc::ptr_eq(&replacement_router, &current_router));
    replacement.release_drain.send(()).unwrap();
    tokio::time::timeout(
        Duration::from_secs(2),
        stop_proxy_impl(&fixture.state, &fixture.publication),
    )
    .await
    .unwrap()
    .unwrap();
}

#[tokio::test]
async fn proxy_stop_drain_failed_signal_still_observes_execution_completion() {
    let fixture = drain_fixture(Ok(()), false);
    let stop = stop_proxy_impl(&fixture.state, &fixture.publication);
    tokio::pin!(stop);
    assert!(futures::poll!(&mut stop).is_pending());
    tokio::time::timeout(Duration::from_secs(2), fixture.shutdown_observed)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        start_proxy_impl(&fixture.db, &fixture.state, &fixture.publication).await,
        Err("proxy already running".into())
    );
    fixture.release_drain.send(()).unwrap();
    let result = tokio::time::timeout(Duration::from_secs(2), stop)
        .await
        .unwrap();
    assert_eq!(result, Err("proxy_shutdown_signal_failed".into()));
    assert!(fixture.state.lock().await.is_none());
}
