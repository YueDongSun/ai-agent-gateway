use super::*;
use crate::proxy::router::Router;
use std::{
    collections::HashMap,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

const EXISTING_KEY: &str = "fixture_existing_publication_key";

#[derive(Default)]
struct Fixture {
    plans: Mutex<Vec<PublicationPlan>>,
    starts: AtomicUsize,
    stops: AtomicUsize,
    hold_start_until_cancelled: AtomicBool,
    hold_stop: AtomicBool,
    start_failure: Mutex<Option<PublicationError>>,
    stop_failure: Mutex<Option<PublicationError>>,
}

struct FixtureLauncher(Arc<Fixture>);
struct FixtureProcess(Arc<Fixture>);

impl PublicationLauncher for FixtureLauncher {
    fn start(
        &self,
        _: &Path,
        plan: PublicationPlan,
        cancelled: &AtomicBool,
    ) -> Result<Box<dyn PublicationProcess>, PublicationError> {
        self.0.plans.lock().unwrap().push(plan);
        self.0.starts.fetch_add(1, Ordering::SeqCst);
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while self.0.hold_start_until_cancelled.load(Ordering::SeqCst)
            && !cancelled.load(Ordering::SeqCst)
        {
            if std::time::Instant::now() >= deadline {
                return Err(PublicationError::StartupTimeout);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        if let Some(error) = self.0.start_failure.lock().unwrap().take() {
            return Err(error);
        }
        // Deliberately admit at the cancellation boundary. The manager must
        // still stop this newly returned owned receipt before reporting done.
        Ok(Box::new(FixtureProcess(self.0.clone())))
    }
}

impl PublicationProcess for FixtureProcess {
    fn state(&mut self) -> Result<PublicationState, PublicationError> {
        Ok(PublicationState::Running)
    }
    fn stop(&mut self, cancelled: &AtomicBool) -> Result<(), PublicationError> {
        assert!(
            !cancelled.load(Ordering::SeqCst),
            "cleanup must retain its verification budget"
        );
        self.0.stops.fetch_add(1, Ordering::SeqCst);
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while self.0.hold_stop.load(Ordering::SeqCst) {
            if std::time::Instant::now() >= deadline {
                return Err(PublicationError::StopTimeout);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        self.0
            .stop_failure
            .lock()
            .unwrap()
            .take()
            .map_or(Ok(()), Err)
    }
}

fn state() -> ProxyAppState {
    ProxyAppState {
        router: Arc::new(Router::new(HashMap::new(), HashMap::new())),
        auth_token: EXISTING_KEY.into(),
        pinned_provider: Arc::new(Mutex::new(None)),
        responses_cot_enabled: Arc::new(AtomicBool::new(false)),
        passthrough_auth_allowed: true,
        db: None,
        live: None,
        allow_client_config_repair: false,
    }
}

fn config() -> PublicationConfig {
    PublicationConfig {
        enabled: true,
        port: 443,
    }
}

fn manager(fixture: &Arc<Fixture>) -> PublicationManager {
    PublicationManager::with_launcher(
        PathBuf::from("fixture_native_executable"),
        Arc::new(FixtureLauncher(fixture.clone())),
    )
}

async fn wait_for_start(fixture: &Fixture) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while fixture.starts.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn manager_validates_before_start_and_keeps_the_current_key() {
    let fixture = Arc::new(Fixture::default());
    let manager = manager(&fixture);
    assert_eq!(
        manager
            .start(
                PublicationConfig {
                    enabled: false,
                    port: 443
                },
                state()
            )
            .await
            .unwrap_err(),
        PublicationError::Disabled.to_string()
    );
    let mut absent_key = state();
    absent_key.auth_token.clear();
    assert_eq!(
        manager.start(config(), absent_key).await.unwrap_err(),
        PublicationError::ExistingKeyRequired.to_string()
    );
    assert_eq!(fixture.starts.load(Ordering::SeqCst), 0);
    assert_eq!(
        manager.start(config(), state()).await.unwrap().state,
        PublicationState::Running
    );
    let backend = fixture.plans.lock().unwrap()[0].backend();
    assert_eq!(backend.ip(), std::net::Ipv4Addr::LOCALHOST);
    let client = reqwest::Client::new();
    let endpoint = format!("http://{backend}/health");
    assert_eq!(
        client.get(&endpoint).send().await.unwrap().status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        client
            .get(&endpoint)
            .bearer_auth(EXISTING_KEY)
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::OK
    );
    assert_eq!(
        manager.stop().await.unwrap().state,
        PublicationState::Stopped
    );
    assert!(tokio::net::TcpStream::connect(backend).await.is_err());
    assert_eq!(fixture.stops.load(Ordering::SeqCst), 1);
    assert_eq!(
        manager.stop().await.unwrap().state,
        PublicationState::Stopped
    );
    assert_eq!(fixture.stops.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn manager_serializes_duplicate_starts_and_allows_restart_after_stop() {
    let fixture = Arc::new(Fixture::default());
    let manager = manager(&fixture);
    let (first, second) = tokio::join!(
        manager.start(config(), state()),
        manager.start(config(), state())
    );
    assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
    assert_eq!(
        first.err().or_else(|| second.err()).unwrap(),
        PublicationError::SlotOccupied.to_string()
    );
    assert_eq!(fixture.starts.load(Ordering::SeqCst), 1);
    manager.stop().await.unwrap();
    manager.start(config(), state()).await.unwrap();
    manager.stop().await.unwrap();
    assert_eq!(fixture.starts.load(Ordering::SeqCst), 2);
    assert_eq!(fixture.stops.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn manager_stop_cancels_startup_and_cleans_a_just_admitted_receipt() {
    let fixture = Arc::new(Fixture::default());
    fixture
        .hold_start_until_cancelled
        .store(true, Ordering::SeqCst);
    let manager = manager(&fixture);
    let starting_manager = manager.clone();
    let starting = tokio::spawn(async move { starting_manager.start(config(), state()).await });
    wait_for_start(&fixture).await;
    assert_eq!(
        manager.stop().await.unwrap().state,
        PublicationState::Stopped
    );
    assert_eq!(
        starting.await.unwrap().unwrap_err(),
        PublicationError::OperationCancelled.to_string()
    );
    assert_eq!(fixture.stops.load(Ordering::SeqCst), 1);
    let backend = fixture.plans.lock().unwrap()[0].backend();
    assert!(tokio::net::TcpStream::connect(backend).await.is_err());
}

#[tokio::test]
async fn manager_dropped_start_waiter_cannot_orphan_its_native_receipt() {
    let fixture = Arc::new(Fixture::default());
    fixture
        .hold_start_until_cancelled
        .store(true, Ordering::SeqCst);
    let manager = manager(&fixture);
    let starting_manager = manager.clone();
    let starting = tokio::spawn(async move { starting_manager.start(config(), state()).await });
    wait_for_start(&fixture).await;
    starting.abort();
    assert!(starting.await.unwrap_err().is_cancelled());
    assert_eq!(manager.status().await.state, PublicationState::Stopped);
    assert_eq!(fixture.stops.load(Ordering::SeqCst), 1);
    assert!(
        tokio::net::TcpStream::connect(fixture.plans.lock().unwrap()[0].backend())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn manager_start_failure_closes_only_its_frontend_and_can_retry() {
    let fixture = Arc::new(Fixture::default());
    *fixture.start_failure.lock().unwrap() = Some(PublicationError::CommandFailed);
    let manager = manager(&fixture);
    let unrelated = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    assert_eq!(
        manager.start(config(), state()).await.unwrap_err(),
        PublicationError::CommandFailed.to_string()
    );
    let backend = fixture.plans.lock().unwrap()[0].backend();
    assert!(tokio::net::TcpStream::connect(backend).await.is_err());
    assert!(
        tokio::net::TcpStream::connect(unrelated.local_addr().unwrap())
            .await
            .is_ok()
    );
    assert_eq!(manager.status().await.state, PublicationState::Stopped);
    manager.start(config(), state()).await.unwrap();
    manager.stop().await.unwrap();
}

#[tokio::test]
async fn manager_failed_stop_keeps_the_receipt_and_refuses_replacement() {
    let fixture = Arc::new(Fixture::default());
    *fixture.stop_failure.lock().unwrap() = Some(PublicationError::OwnershipLost);
    let manager = manager(&fixture);
    manager.start(config(), state()).await.unwrap();
    let backend = fixture.plans.lock().unwrap()[0].backend();
    assert_eq!(
        manager.stop().await.unwrap_err(),
        PublicationError::OwnershipLost.to_string()
    );
    assert!(tokio::net::TcpStream::connect(backend).await.is_err());
    assert_eq!(
        manager.status().await.state,
        PublicationState::OutcomeUnknown
    );
    assert_eq!(
        manager.start(config(), state()).await.unwrap_err(),
        PublicationError::OwnershipUnknown.to_string()
    );
    assert_eq!(fixture.starts.load(Ordering::SeqCst), 1);
    assert_eq!(
        manager.stop().await.unwrap().state,
        PublicationState::Stopped
    );
    assert_eq!(fixture.stops.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn manager_startup_unknown_ownership_is_retained_without_claiming_stopped() {
    let fixture = Arc::new(Fixture::default());
    *fixture.start_failure.lock().unwrap() = Some(PublicationError::OwnershipUnknown);
    let manager = manager(&fixture);
    assert_eq!(
        manager.start(config(), state()).await.unwrap_err(),
        PublicationError::OwnershipUnknown.to_string()
    );
    assert_eq!(
        manager.status().await.state,
        PublicationState::OutcomeUnknown
    );
    assert_eq!(
        manager.stop().await.unwrap_err(),
        PublicationError::OwnershipUnknown.to_string()
    );
    assert_eq!(
        manager.start(config(), state()).await.unwrap_err(),
        PublicationError::OwnershipUnknown.to_string()
    );
    assert_eq!(fixture.starts.load(Ordering::SeqCst), 1);
}

fn local_task() -> (
    crate::services::proxy_runtime::lifecycle::Proxy_task,
    tokio::sync::oneshot::Sender<()>,
) {
    let (shutdown, shutdown_receiver) = tokio::sync::oneshot::channel();
    let (fail, failure) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        tokio::select! {
            _ = shutdown_receiver => Ok(()),
            _ = failure => Err("fixture_local_server_failed".into()),
        }
    });
    (
        crate::services::proxy_runtime::lifecycle::Proxy_task::new(server, shutdown),
        fail,
    )
}

#[tokio::test]
async fn service_exit_rejects_actual_start_without_creating_a_native_publication() {
    let fixture = Arc::new(Fixture::default());
    let service =
        super::super::settings::Publication_service::with_fixture_manager(manager(&fixture));
    service.request_exit();
    assert_eq!(
        service.start(config(), state()).await.unwrap_err(),
        "tailnet_publication_exiting"
    );
    assert_eq!(
        service.stop().await.unwrap().state,
        PublicationState::Stopped
    );
    assert_eq!(fixture.starts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn service_current_local_failure_closes_frontend_but_old_completion_cannot_stop_replacement()
{
    let fixture = Arc::new(Fixture::default());
    let service =
        super::super::settings::Publication_service::with_fixture_manager(manager(&fixture));
    let (old, fail_old) = local_task();
    let old_completion = old.subscribe_completion();
    service.bind_gateway(old_completion.clone()).await;
    service.start(config(), state()).await.unwrap();
    service.stop().await.unwrap();

    let (replacement, fail_replacement) = local_task();
    service
        .bind_gateway(replacement.subscribe_completion())
        .await;
    service.start(config(), state()).await.unwrap();
    let backend = fixture.plans.lock().unwrap()[1].backend();
    fail_old.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(3), old_completion.clone().wait())
        .await
        .unwrap();
    // Deliver the stale source after replacement admission, including the
    // race an already-dispatched completion callback could otherwise cause.
    service.gateway_finished(old_completion).await;
    assert_eq!(service.status().await.state, PublicationState::Running);
    assert_eq!(fixture.stops.load(Ordering::SeqCst), 1);
    let response = reqwest::Client::new()
        .get(format!("http://{backend}/health"))
        .bearer_auth(EXISTING_KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);

    fail_replacement.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if service.status().await.state == PublicationState::Stopped {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(tokio::net::TcpStream::connect(backend).await.is_err());
    assert_eq!(fixture.stops.load(Ordering::SeqCst), 2);
    service.request_exit();
    service.stop().await.unwrap();
}

#[tokio::test]
async fn service_local_failure_during_startup_cleans_its_just_admitted_native_receipt() {
    let fixture = Arc::new(Fixture::default());
    fixture
        .hold_start_until_cancelled
        .store(true, Ordering::SeqCst);
    let service =
        super::super::settings::Publication_service::with_fixture_manager(manager(&fixture));
    let (local, fail_local) = local_task();
    service.bind_gateway(local.subscribe_completion()).await;
    let starting_service = service.clone();
    let starting = tokio::spawn(async move { starting_service.start(config(), state()).await });
    wait_for_start(&fixture).await;
    fail_local.send(()).unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), starting)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err(),
        "tailnet_publication_local_gateway_required"
    );
    assert_eq!(service.status().await.state, PublicationState::Stopped);
    assert_eq!(fixture.stops.load(Ordering::SeqCst), 1);
    let backend = fixture.plans.lock().unwrap()[0].backend();
    assert!(tokio::net::TcpStream::connect(backend).await.is_err());
    service.request_exit();
    service.stop().await.unwrap();
}

#[test]
fn pending_start_dropped_outside_runtime_keeps_admitted_receipt_for_new_runtime_reconcile() {
    let fixture = Arc::new(Fixture::default());
    let manager = manager(&fixture);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut starting = Box::pin(manager.start(config(), state()));
    let receipt = runtime.block_on(async {
        assert!(futures::poll!(&mut starting).is_pending());
        // Leave the caller unpolled after its owned operation admits the native
        // receipt. Dropping the caller must cover this completion boundary too.
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let slot = manager.inner.slot.lock().await;
                if let Some(active) = &slot.active {
                    break active.process.clone();
                }
                drop(slot);
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap()
    });
    assert!(tokio::runtime::Handle::try_current().is_err());
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(starting))).is_ok());
    assert_eq!(fixture.stops.load(Ordering::SeqCst), 0);
    drop(runtime);

    let recovery = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    recovery.block_on(async {
        {
            let slot = manager.inner.slot.lock().await;
            assert!(Arc::ptr_eq(
                &slot
                    .active
                    .as_ref()
                    .expect("admitted owner retained")
                    .process,
                &receipt
            ));
        }
        assert_eq!(
            manager.status().await.state,
            PublicationState::OutcomeUnknown
        );
        assert_eq!(fixture.stops.load(Ordering::SeqCst), 0);
        // The old frontend task was cancelled by its runtime. A reported
        // frontend join error does not waive native stop or lose its receipt.
        let result = manager.stop().await;
        assert!(
            result.is_ok()
                || result.as_ref().err().map(String::as_str)
                    == Some("tailnet_publication_frontend_task_failed")
        );
        assert_eq!(fixture.stops.load(Ordering::SeqCst), 1);
        assert_eq!(manager.status().await.state, PublicationState::Stopped);
    });
}

#[test]
fn runtime_teardown_during_scheduled_stop_retains_the_exact_receipt_until_reconciled() {
    let fixture = Arc::new(Fixture::default());
    let manager = manager(&fixture);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(manager.start(config(), state())).unwrap();
    let receipt = runtime.block_on(async {
        manager
            .inner
            .slot
            .lock()
            .await
            .active
            .as_ref()
            .unwrap()
            .process
            .clone()
    });
    fixture.hold_stop.store(true, Ordering::SeqCst);
    let mut stopping = Box::pin(manager.stop());
    runtime.block_on(async {
        assert!(futures::poll!(&mut stopping).is_pending());
        tokio::time::timeout(Duration::from_secs(3), async {
            while fixture.stops.load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    });
    drop(stopping);
    // No executor polls the async stop continuation after its blocking stop
    // returns; runtime teardown aborts that continuation while ownership stays.
    fixture.hold_stop.store(false, Ordering::SeqCst);
    drop(runtime);

    let recovery = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    recovery.block_on(async {
        {
            let slot = manager.inner.slot.lock().await;
            assert!(Arc::ptr_eq(
                &slot
                    .active
                    .as_ref()
                    .expect("cleanup owner retained")
                    .process,
                &receipt
            ));
        }
        assert_eq!(
            manager.status().await.state,
            PublicationState::OutcomeUnknown
        );
        manager.stop().await.unwrap();
        assert_eq!(manager.status().await.state, PublicationState::Stopped);
        assert_eq!(fixture.stops.load(Ordering::SeqCst), 2);
    });
}
