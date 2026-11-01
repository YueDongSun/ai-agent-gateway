//! One catalog refresh owner shared by IPC and the application-lifetime worker.
//! HTTP operations remain in the catalog service; this module owns admission,
//! due checks, cancellation and bounded completion reports only.

use super::{CatalogRefreshInput, CatalogSourceView};
use crate::{database::Database, error::AppError};
use serde::Serialize;
use std::future::Future;
use std::pin::Pin;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::Duration;
use tokio::sync::{mpsc, watch, Mutex as AsyncMutex};
use tokio::task::JoinHandle;

const REFRESH_INTERVAL: Duration = Duration::from_secs(3_600);
const STOP_TIMEOUT: Duration = Duration::from_secs(2);
const REPORT_CAPACITY: usize = 1;
const AUTO_SOURCES: &[&str] = &[
    super::SOURCE_MODELS_DEV,
    super::SOURCE_EVALPLUS,
    super::arena::SOURCE_ID,
];

type RefreshFuture =
    Pin<Box<dyn Future<Output = Result<CatalogSourceView, AppError>> + Send + 'static>>;
type RefreshAction =
    Arc<dyn Fn(Database, CatalogRefreshInput) -> RefreshFuture + Send + Sync + 'static>;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
#[allow(non_camel_case_types)]
pub(crate) struct Refresh_report {
    pub attempted: usize,
    pub succeeded: usize,
    pub failed: usize,
    /// Sources whose document, freshness or error state changed. Refreshed
    /// timestamps alone do not require rebuilding an admitted router.
    pub changed: usize,
}

#[derive(Default)]
struct WorkerLifecycle {
    task: Option<JoinHandle<()>>,
    joining: bool,
}

struct RefreshInner {
    gate: AsyncMutex<()>,
    lifecycle: Mutex<WorkerLifecycle>,
    stopped: AtomicBool,
    cancellation: watch::Sender<u64>,
    operation: RefreshAction,
    interval: Duration,
}

#[derive(Clone)]
#[allow(non_camel_case_types)]
pub(crate) struct Catalog_refresh_service {
    inner: Arc<RefreshInner>,
}

struct RefreshOutcome {
    view: CatalogSourceView,
    changed: bool,
}

struct StopLease {
    inner: Arc<RefreshInner>,
    task: Option<JoinHandle<()>>,
}

impl Drop for StopLease {
    fn drop(&mut self) {
        let mut lifecycle = self
            .inner
            .lifecycle
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        lifecycle.task = self.task.take();
        lifecycle.joining = false;
    }
}

impl Catalog_refresh_service {
    pub fn new() -> Self {
        let operation: RefreshAction =
            Arc::new(|db, input| Box::pin(async move { super::refresh_source(&db, &input).await }));
        Self::with_operation(operation, REFRESH_INTERVAL)
    }

    fn with_operation(operation: RefreshAction, interval: Duration) -> Self {
        let (cancellation, _) = watch::channel(0);
        Self {
            inner: Arc::new(RefreshInner {
                gate: AsyncMutex::new(()),
                lifecycle: Mutex::new(WorkerLifecycle::default()),
                stopped: AtomicBool::new(false),
                cancellation,
                operation,
                interval,
            }),
        }
    }

    /// Explicit IPC refreshes always attempt the requested source. The caller
    /// retains its existing runtime-reload/error presentation responsibility.
    pub async fn refresh(
        &self,
        db: &Database,
        input: &CatalogRefreshInput,
    ) -> Result<CatalogSourceView, AppError> {
        self.refresh_one(db, input, false)
            .await?
            .map(|outcome| outcome.view)
            .ok_or_else(stopped_error)
    }

    /// Cold graph reads share the worker's due check inside admission, so a
    /// queued reader reuses evidence fetched by the preceding operation.
    pub async fn refresh_if_stale(
        &self,
        db: &Database,
        input: &CatalogRefreshInput,
    ) -> Result<CatalogSourceView, AppError> {
        match self.refresh_one(db, input, true).await? {
            Some(outcome) => Ok(outcome.view),
            None => super::catalog_source_view(db, &input.source),
        }
    }

    pub fn start(&self, db: Database) -> Result<mpsc::Receiver<Refresh_report>, String> {
        let mut lifecycle = self
            .inner
            .lifecycle
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if lifecycle.joining
            || lifecycle
                .task
                .as_ref()
                .is_some_and(|task| !task.is_finished())
        {
            return Err("catalog_refresh_already_running".into());
        }
        // A terminal handle may remain after receiver closure or an explicit
        // stop request. Never replace a task whose terminal state is unknown.
        lifecycle.task.take();
        self.inner.stopped.store(false, Ordering::SeqCst);
        let (sender, receiver) = mpsc::channel(REPORT_CAPACITY);
        let service = self.clone();
        let cancellation = self.inner.cancellation.subscribe();
        let runtime = tokio::runtime::Handle::try_current()
            .unwrap_or_else(|_| tauri::async_runtime::handle().inner().clone());
        lifecycle.task = Some(runtime.spawn(async move {
            service.run_worker(db, sender, cancellation).await;
        }));
        Ok(receiver)
    }

    /// Exit hooks cannot await. Signal every admitted refresh and abort the
    /// owned worker, retaining its handle until stop/next-start observes it.
    pub fn request_stop(&self) {
        let lifecycle = self
            .inner
            .lifecycle
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.signal_stop(&lifecycle);
    }

    /// Abort and join the worker, then cross the refresh gate so no admitted
    /// manual HTTP operation remains alive when successful completion returns.
    pub async fn stop(&self) -> Result<(), String> {
        let mut lease = {
            let mut lifecycle = self
                .inner
                .lifecycle
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if lifecycle.joining {
                return Err("catalog_refresh_stop_pending".into());
            }
            self.signal_stop(&lifecycle);
            lifecycle.joining = true;
            StopLease {
                inner: self.inner.clone(),
                task: lifecycle.task.take(),
            }
        };
        let deadline = tokio::time::Instant::now() + STOP_TIMEOUT;
        let joined = if let Some(handle) = lease.task.as_mut() {
            match tokio::time::timeout_at(deadline, handle).await {
                Ok(Ok(())) => Ok(()),
                Ok(Err(error)) if error.is_cancelled() => Ok(()),
                Ok(Err(_)) => Err("catalog_refresh_worker_failed"),
                Err(_) => Err("catalog_refresh_stop_pending"),
            }
        } else {
            Ok(())
        };
        if joined == Err("catalog_refresh_stop_pending") {
            return Err("catalog_refresh_stop_pending".into());
        }
        // A polled-to-completion JoinHandle is consumed. On timeout or caller
        // cancellation the lease instead restores the unconsumed owned handle.
        lease.task.take();
        let drained = tokio::time::timeout_at(deadline, self.inner.gate.lock()).await;
        let result = if drained.is_err() {
            Err("catalog_refresh_stop_pending".into())
        } else {
            joined.map_err(str::to_string)
        };
        result
    }

    fn signal_stop(&self, lifecycle: &WorkerLifecycle) {
        self.inner.stopped.store(true, Ordering::SeqCst);
        self.inner
            .cancellation
            .send_modify(|generation| *generation = generation.wrapping_add(1));
        if let Some(task) = &lifecycle.task {
            task.abort();
        }
    }

    async fn refresh_one(
        &self,
        db: &Database,
        input: &CatalogRefreshInput,
        only_due: bool,
    ) -> Result<Option<RefreshOutcome>, AppError> {
        let mut cancellation = self.inner.cancellation.subscribe();
        let generation = *cancellation.borrow();
        if self.inner.stopped.load(Ordering::SeqCst) {
            return Err(stopped_error());
        }
        let _gate = tokio::select! {
            biased;
            _ = cancellation.changed() => return Err(stopped_error()),
            gate = self.inner.gate.lock() => gate,
        };
        if self.inner.stopped.load(Ordering::SeqCst) || *cancellation.borrow() != generation {
            return Err(stopped_error());
        }
        // Due status belongs inside the shared gate. A manual refresh that
        // won admission while the worker waited must not be fetched again.
        let before_view = super::catalog_source_view(db, &input.source)?;
        let before_document = cached_document(db, &input.source)?;
        let invalid_arena = only_due
            && input.source == super::SOURCE_LMARENA
            && before_document.as_deref().is_none_or(|raw| {
                super::arena::parse_arena_at(raw, chrono::Utc::now().date_naive()).is_err()
            });
        if only_due && !before_view.stale && !invalid_arena {
            return Ok(None);
        }
        let operation = (self.inner.operation)(db.clone(), input.clone());
        let view = tokio::select! {
            biased;
            _ = cancellation.changed() => return Err(stopped_error()),
            result = operation => result?,
        };
        let after_document = cached_document(db, &input.source)?;
        let changed = before_document != after_document
            || before_view.stale != view.stale
            || before_view.last_error != view.last_error;
        Ok(Some(RefreshOutcome { view, changed }))
    }

    async fn refresh_due(&self, db: &Database) -> Refresh_report {
        let mut report = Refresh_report::default();
        for source in AUTO_SOURCES {
            if self.inner.stopped.load(Ordering::SeqCst) {
                break;
            }
            let input = CatalogRefreshInput {
                source: (*source).into(),
            };
            match self.refresh_one(db, &input, true).await {
                Ok(None) => {}
                Ok(Some(outcome)) => {
                    report.attempted += 1;
                    if outcome.view.last_error.is_none() && !outcome.view.stale {
                        report.succeeded += 1;
                    } else {
                        report.failed += 1;
                    }
                    report.changed += usize::from(outcome.changed);
                }
                Err(_) => {
                    if !self.inner.stopped.load(Ordering::SeqCst) {
                        report.attempted += 1;
                        report.failed += 1;
                    }
                }
            }
        }
        report
    }

    async fn run_worker(
        &self,
        db: Database,
        sender: mpsc::Sender<Refresh_report>,
        mut cancellation: watch::Receiver<u64>,
    ) {
        let mut interval = tokio::time::interval(self.inner.interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        while !self.inner.stopped.load(Ordering::SeqCst) {
            tokio::select! {
                biased;
                _ = cancellation.changed() => break,
                _ = interval.tick() => {},
            }
            let report = self.refresh_due(&db).await;
            if self.inner.stopped.load(Ordering::SeqCst) {
                break;
            }
            if report.attempted > 0 {
                log::info!(
                    "catalog refresh: attempted={} succeeded={} failed={} changed={}",
                    report.attempted,
                    report.succeeded,
                    report.failed,
                    report.changed
                );
            }
            // Backpressure preserves a pending change report. It holds no
            // refresh gate, and cancellation drops this wait immediately.
            tokio::select! {
                biased;
                _ = cancellation.changed() => break,
                result = sender.send(report) => if result.is_err() { break; },
            }
        }
    }
}

fn cached_document(db: &Database, source: &str) -> Result<Option<String>, AppError> {
    crate::services::suppliers::get_public_catalog_cache(db, source)
        .map(|cache| cache.map(|cache| cache.raw_json))
}

fn stopped_error() -> AppError {
    AppError::Config("catalog_refresh_stopped".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::suppliers::{
        record_public_catalog_error, save_public_catalog_cache, PublicCatalogCacheInput,
    };
    use std::sync::atomic::AtomicUsize;
    use tokio::sync::Notify;

    fn store(db: &Database, source: &str, raw: &str, fetched_at: i64) {
        save_public_catalog_cache(
            db,
            PublicCatalogCacheInput {
                source: source.into(),
                raw_json: raw.into(),
                fetched_at,
            },
        )
        .unwrap();
    }

    fn fresh_other_sources(db: &Database) {
        for source in &AUTO_SOURCES[1..] {
            store(
                db,
                source,
                &fixture_document(source),
                chrono::Utc::now().timestamp(),
            );
        }
    }

    fn fixture_document(source: &str) -> String {
        if source == super::super::arena::SOURCE_ID {
            serde_json::json!({"num_rows_total":1,"rows":[{"row":{
                "model_name":"Fixture Model","rating":1500.0,"rating_lower":1490.0,
                "rating_upper":1510.0,"vote_count":1000,"rank":1,"category":"overall",
                "leaderboard_publish_date":chrono::Utc::now().date_naive().to_string()
            }}]})
            .to_string()
        } else {
            "{}".into()
        }
    }

    #[tokio::test]
    async fn stale_refresh_bootstraps_once_and_manual_retry_stays_explicit() {
        let db = Database::memory().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let operation: RefreshAction = {
            let calls = calls.clone();
            Arc::new(move |db, input| {
                let calls = calls.clone();
                Box::pin(async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    tokio::task::yield_now().await;
                    store(
                        &db,
                        &input.source,
                        &fixture_document(&input.source),
                        chrono::Utc::now().timestamp(),
                    );
                    super::super::catalog_source_view(&db, &input.source)
                })
            })
        };
        let service = Catalog_refresh_service::with_operation(operation, REFRESH_INTERVAL);
        let input = CatalogRefreshInput {
            source: super::super::SOURCE_LMARENA.into(),
        };
        let (first, second) = tokio::join!(
            service.refresh_if_stale(&db, &input),
            service.refresh_if_stale(&db, &input)
        );
        assert!(!first.unwrap().stale);
        assert!(!second.unwrap().stale);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(super::super::load_evidence(&db).arena.is_some());
        service.refresh(&db, &input).await.unwrap();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "explicit refresh still forces a fetch"
        );
    }

    struct StopOnDrop(Catalog_refresh_service);
    impl Drop for StopOnDrop {
        fn drop(&mut self) {
            self.0.request_stop();
        }
    }

    struct InFlight {
        active: Arc<AtomicUsize>,
        dropped: Arc<AtomicUsize>,
    }
    impl Drop for InFlight {
        fn drop(&mut self) {
            self.active.fetch_sub(1, Ordering::SeqCst);
            self.dropped.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn pending_action(
        started: Arc<Notify>,
        active: Arc<AtomicUsize>,
        dropped: Arc<AtomicUsize>,
    ) -> RefreshAction {
        Arc::new(move |_db, _input| {
            let (started, active, dropped) = (started.clone(), active.clone(), dropped.clone());
            Box::pin(async move {
                active.fetch_add(1, Ordering::SeqCst);
                let _in_flight = InFlight { active, dropped };
                started.notify_one();
                std::future::pending::<()>().await;
                unreachable!()
            })
        })
    }

    #[tokio::test]
    async fn manual_refreshes_share_one_admission_gate() {
        let db = Database::memory().unwrap();
        let active = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let dropped = Arc::new(AtomicUsize::new(0));
        let calls = Arc::new(AtomicUsize::new(0));
        let operation: RefreshAction = {
            let (active, maximum, dropped, calls) = (
                active.clone(),
                maximum.clone(),
                dropped.clone(),
                calls.clone(),
            );
            Arc::new(move |db, input| {
                let (active, maximum, dropped, calls) = (
                    active.clone(),
                    maximum.clone(),
                    dropped.clone(),
                    calls.clone(),
                );
                Box::pin(async move {
                    let count = active.fetch_add(1, Ordering::SeqCst) + 1;
                    maximum.fetch_max(count, Ordering::SeqCst);
                    let _in_flight = InFlight { active, dropped };
                    calls.fetch_add(1, Ordering::SeqCst);
                    tokio::task::yield_now().await;
                    store(
                        &db,
                        &input.source,
                        &fixture_document(&input.source),
                        chrono::Utc::now().timestamp(),
                    );
                    super::super::catalog_source_view(&db, &input.source)
                })
            })
        };
        let service = Catalog_refresh_service::with_operation(operation, REFRESH_INTERVAL);
        let input = CatalogRefreshInput {
            source: super::super::SOURCE_MODELS_DEV.into(),
        };
        let (first, second) =
            tokio::join!(service.refresh(&db, &input), service.refresh(&db, &input));
        assert!(!first.unwrap().stale);
        assert!(!second.unwrap().stale);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(maximum.load(Ordering::SeqCst), 1);
        assert_eq!(active.load(Ordering::SeqCst), 0);
        assert_eq!(dropped.load(Ordering::SeqCst), 2);
        service.stop().await.unwrap();
    }

    #[tokio::test]
    async fn automatic_due_status_is_rechecked_after_waiting_for_the_gate() {
        let db = Database::memory().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let operation: RefreshAction = {
            let calls = calls.clone();
            Arc::new(move |db, input| {
                calls.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move { super::super::catalog_source_view(&db, &input.source) })
            })
        };
        let service = Catalog_refresh_service::with_operation(operation, REFRESH_INTERVAL);
        let _stop = StopOnDrop(service.clone());
        let gate = service.inner.gate.lock().await;
        let mut reports = service.start(db.clone()).unwrap();
        tokio::task::yield_now().await;
        for source in AUTO_SOURCES {
            store(
                &db,
                source,
                &fixture_document(source),
                chrono::Utc::now().timestamp(),
            );
        }
        drop(gate);
        let report = tokio::time::timeout(Duration::from_secs(2), reports.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(report, Refresh_report::default());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        service.stop().await.unwrap();
    }

    #[tokio::test]
    async fn failed_refresh_retains_cache_and_retries_on_the_next_periodic_pass() {
        let db = Database::memory().unwrap();
        fresh_other_sources(&db);
        let source = super::super::SOURCE_MODELS_DEV;
        store(
            &db,
            source,
            "{\"retained\":{\"models\":{}}}",
            chrono::Utc::now().timestamp() - super::super::CATALOG_TTL_SECS - 1,
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let operation: RefreshAction = {
            let calls = calls.clone();
            Arc::new(move |db, input| {
                let call = calls.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move {
                    if call == 0 {
                        record_public_catalog_error(&db, &input.source, "fetch_timeout")?;
                    } else {
                        store(
                            &db,
                            &input.source,
                            "{\"refreshed\":{\"models\":{}}}",
                            chrono::Utc::now().timestamp(),
                        );
                    }
                    super::super::catalog_source_view(&db, &input.source)
                })
            })
        };
        let service =
            Catalog_refresh_service::with_operation(operation, Duration::from_millis(100));
        let _stop = StopOnDrop(service.clone());
        let mut reports = service.start(db.clone()).unwrap();
        let failed = tokio::time::timeout(Duration::from_secs(2), reports.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            failed,
            Refresh_report {
                attempted: 1,
                succeeded: 0,
                failed: 1,
                changed: 1
            }
        );
        assert_eq!(
            cached_document(&db, source).unwrap().as_deref(),
            Some("{\"retained\":{\"models\":{}}}")
        );
        let recovered = tokio::time::timeout(Duration::from_secs(2), reports.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            recovered,
            Refresh_report {
                attempted: 1,
                succeeded: 1,
                failed: 0,
                changed: 1
            }
        );
        service.stop().await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            cached_document(&db, source).unwrap().as_deref(),
            Some("{\"refreshed\":{\"models\":{}}}")
        );
    }

    #[tokio::test]
    async fn duplicate_start_is_rejected_and_stop_reaps_an_in_flight_worker() {
        let db = Database::memory().unwrap();
        fresh_other_sources(&db);
        let started = Arc::new(Notify::new());
        let active = Arc::new(AtomicUsize::new(0));
        let dropped = Arc::new(AtomicUsize::new(0));
        let service = Catalog_refresh_service::with_operation(
            pending_action(started.clone(), active.clone(), dropped.clone()),
            REFRESH_INTERVAL,
        );
        let _stop = StopOnDrop(service.clone());
        let mut reports = service.start(db.clone()).unwrap();
        assert_eq!(
            service.start(db).err().as_deref(),
            Some("catalog_refresh_already_running")
        );
        tokio::time::timeout(Duration::from_secs(2), started.notified())
            .await
            .unwrap();
        service.request_stop();
        assert!(service.inner.lifecycle.lock().unwrap().task.is_some());
        service.stop().await.unwrap();
        assert_eq!(active.load(Ordering::SeqCst), 0);
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
        assert!(service.inner.lifecycle.lock().unwrap().task.is_none());
        assert!(reports.recv().await.is_none());
    }

    #[tokio::test]
    async fn stop_also_cancels_manual_refresh_and_drains_the_shared_gate() {
        let db = Database::memory().unwrap();
        let started = Arc::new(Notify::new());
        let active = Arc::new(AtomicUsize::new(0));
        let dropped = Arc::new(AtomicUsize::new(0));
        let service = Catalog_refresh_service::with_operation(
            pending_action(started.clone(), active.clone(), dropped.clone()),
            REFRESH_INTERVAL,
        );
        let _stop = StopOnDrop(service.clone());
        let worker_service = service.clone();
        let refresh = tokio::spawn(async move {
            worker_service
                .refresh(
                    &db,
                    &CatalogRefreshInput {
                        source: super::super::SOURCE_MODELS_DEV.into(),
                    },
                )
                .await
        });
        tokio::time::timeout(Duration::from_secs(2), started.notified())
            .await
            .unwrap();
        service.stop().await.unwrap();
        let result = refresh.await.unwrap();
        assert!(matches!(result, Err(AppError::Config(code)) if code == "catalog_refresh_stopped"));
        assert_eq!(active.load(Ordering::SeqCst), 0);
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
        assert!(service.inner.gate.try_lock().is_ok());
    }

    #[tokio::test]
    async fn cancelling_a_stop_wait_releases_lifecycle_ownership() {
        let operation: RefreshAction = Arc::new(|db, input| {
            Box::pin(async move { super::super::catalog_source_view(&db, &input.source) })
        });
        let service = Catalog_refresh_service::with_operation(operation, REFRESH_INTERVAL);
        let gate = service.inner.gate.lock().await;
        let mut stopping = Box::pin(service.stop());
        tokio::select! {
            biased;
            _ = &mut stopping => panic!("stop must wait for the admitted gate"),
            _ = tokio::task::yield_now() => {},
        }
        assert!(service.inner.lifecycle.lock().unwrap().joining);
        drop(stopping);
        assert!(!service.inner.lifecycle.lock().unwrap().joining);
        drop(gate);
        service.stop().await.unwrap();
    }
}
