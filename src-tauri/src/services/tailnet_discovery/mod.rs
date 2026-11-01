//! Read-only, request-owned discovery of AAG listeners on verified Tailnet nodes.
//! A single operation is admitted at a time. No authentication, remote mutation,
//! range scanning, process installation, or publication policy belongs here.

mod inventory;
mod probe;
mod types;

use crate::database::Database;
use futures_util::{stream, StreamExt};
use inventory::{parse_inventory, Inventory, STATUS_LIMIT};
use probe::{build_client, probe_candidate, Probe_outcome};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio::sync::{oneshot, watch};
use types::Discovery_counts;
pub(crate) use types::{Discovery_report, Discovery_status};

const STATUS_TIMEOUT: Duration = Duration::from_secs(3);
const SCAN_TIMEOUT: Duration = Duration::from_secs(15);
const PROBE_CONCURRENCY: usize = 8;

struct Active_scan {
    request_id: String,
    cancelled: Arc<AtomicBool>,
    signal: watch::Sender<bool>,
}

struct Discovery_state {
    active: Option<Active_scan>,
    phase: String,
    error: Option<String>,
    report: Option<Discovery_report>,
}

struct Discovery_inner {
    state: Mutex<Discovery_state>,
}

#[derive(Clone)]
pub(crate) struct Tailnet_discovery_service {
    inner: Arc<Discovery_inner>,
}

impl Default for Tailnet_discovery_service {
    fn default() -> Self {
        Self::new()
    }
}

impl Tailnet_discovery_service {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(Discovery_inner {
                state: Mutex::new(Discovery_state {
                    active: None,
                    phase: "idle".into(),
                    error: None,
                    report: None,
                }),
            }),
        }
    }

    fn begin(&self, request_id: &str) -> Result<(Scan_lease, watch::Receiver<bool>), String> {
        if request_id.is_empty()
            || request_id.len() > 80
            || !request_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
        {
            return Err("tailnet_discovery_request_invalid".into());
        }
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if state.active.is_some() {
            return Err("tailnet_discovery_busy".into());
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        let (signal, receiver) = watch::channel(false);
        state.active = Some(Active_scan {
            request_id: request_id.into(),
            cancelled: cancelled.clone(),
            signal,
        });
        state.phase = "scanning".into();
        state.error = None;
        Ok((
            Scan_lease {
                inner: self.inner.clone(),
                request_id: request_id.into(),
                cancelled,
                finished: false,
            },
            receiver,
        ))
    }

    pub(crate) fn cancel(&self, request_id: &str) -> bool {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let Some(active) = state
            .active
            .as_ref()
            .filter(|active| active.request_id == request_id)
        else {
            return false;
        };
        active.cancelled.store(true, Ordering::SeqCst);
        let _ = active.signal.send(true);
        state.phase = "cancelling".into();
        true
    }

    pub(crate) fn cancel_all(&self) {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(active) = state.active.as_ref() {
            active.cancelled.store(true, Ordering::SeqCst);
            let _ = active.signal.send(true);
            state.phase = "cancelling".into();
        }
    }

    pub(crate) fn status(&self, port: u16) -> Discovery_status {
        let state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        Discovery_status {
            port,
            phase: state.phase.clone(),
            error: state.error.clone(),
            last_report: state.report.clone(),
        }
    }

    pub(crate) async fn scan(
        &self,
        db: &Database,
        request_id: String,
    ) -> Result<Discovery_report, String> {
        let port = discovery_port(db)?;
        let (mut lease, mut cancellation) = self.begin(&request_id)?;
        let (send, receive) = oneshot::channel();
        // The operation owns its task even if an IPC caller disappears. Its
        // admission lease remains held until bounded cleanup reaches terminal.
        tokio::spawn(async move {
            let started = Instant::now();
            let cancelled = lease.cancelled.clone();
            let outcome = scan_inner(request_id, port, cancelled, started, &mut cancellation).await;
            lease.finish(&outcome);
            drop(lease);
            let _ = send.send(outcome);
        });
        receive
            .await
            .map_err(|_| "tailnet_discovery_task_failed".to_string())?
    }
}

struct Scan_lease {
    inner: Arc<Discovery_inner>,
    request_id: String,
    cancelled: Arc<AtomicBool>,
    finished: bool,
}

impl Scan_lease {
    fn finish(&mut self, result: &Result<Discovery_report, String>) {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if state
            .active
            .as_ref()
            .is_some_and(|active| active.request_id == self.request_id)
        {
            match result {
                Ok(report) => {
                    state.report = Some(report.clone());
                    state.error = None;
                    state.phase = "complete".into();
                }
                Err(error) => {
                    state.error = Some(error.clone());
                    state.phase = if error == "tailnet_discovery_cancelled" {
                        "cancelled"
                    } else {
                        "error"
                    }
                    .into();
                }
            }
        }
        self.finished = true;
    }
}

impl Drop for Scan_lease {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::SeqCst);
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if state
            .active
            .as_ref()
            .is_some_and(|active| active.request_id == self.request_id)
        {
            state.active = None;
            if !self.finished {
                state.phase = "error".into();
                state.error = Some("tailnet_discovery_task_failed".into());
            }
        }
    }
}

pub(crate) fn discovery_port(db: &Database) -> Result<u16, String> {
    let publication = crate::services::tailnet_publication::settings::read_config(db)
        .map_err(|_| "tailnet_discovery_config_unavailable".to_string())?;
    let port = if publication.enabled {
        publication.port
    } else {
        db.get_proxy_config()
            .map_err(|_| "tailnet_discovery_config_unavailable".to_string())?
            .port
    };
    if port == 0 {
        return Err("tailnet_discovery_port_invalid".into());
    }
    Ok(port)
}

fn read_inventory(cancelled: Arc<AtomicBool>) -> Result<Inventory, String> {
    let executable =
        crate::services::tailnet_publication::trusted_tailscale_executable().map_err(|error| {
            if error == crate::services::tailnet_publication::PublicationError::PlatformUnsupported
            {
                "tailnet_discovery_platform_unsupported"
            } else {
                "tailnet_discovery_process_unavailable"
            }
            .to_string()
        })?;
    let arguments = vec!["status".into(), "--json".into()];
    let output = crate::services::owned_process::run_capture(
        &crate::services::owned_process::SpawnSpec::new(&executable, &arguments),
        &cancelled,
        Instant::now() + STATUS_TIMEOUT,
        STATUS_LIMIT,
    )
    .map_err(|error| {
        match error {
            crate::services::owned_process::NativeProcessError::OperationCancelled => {
                "tailnet_discovery_cancelled"
            }
            crate::services::owned_process::NativeProcessError::DeadlineExceeded => {
                "tailnet_discovery_status_timeout"
            }
            crate::services::owned_process::NativeProcessError::OutputLimitExceeded => {
                "tailnet_discovery_status_too_large"
            }
            _ => "tailnet_discovery_process_failed",
        }
        .to_string()
    })?;
    if output.exit_code != 0 {
        return Err("tailnet_discovery_process_failed".into());
    }
    parse_inventory(&output.stdout)
}

async fn scan_inner(
    request_id: String,
    port: u16,
    cancelled: Arc<AtomicBool>,
    started: Instant,
    cancellation: &mut watch::Receiver<bool>,
) -> Result<Discovery_report, String> {
    let process_cancel = cancelled.clone();
    // A cancelled request still awaits the bounded process owner's terminal
    // result. A new scan cannot overlap an old tailscale process during drain.
    let inventory = tokio::task::spawn_blocking(move || read_inventory(process_cancel))
        .await
        .map_err(|_| "tailnet_discovery_task_failed".to_string())??;
    if cancelled.load(Ordering::SeqCst) {
        return Err("tailnet_discovery_cancelled".into());
    }
    tokio::select! {
        biased;
        _ = cancellation.changed() => Err("tailnet_discovery_cancelled".into()),
        outcome = tokio::time::timeout(SCAN_TIMEOUT.saturating_sub(started.elapsed()), probe_inventory(request_id, port, inventory, started)) => {
            outcome.unwrap_or_else(|_| Err("tailnet_discovery_timed_out".into()))
        }
    }
}

async fn probe_inventory(
    request_id: String,
    port: u16,
    inventory: Inventory,
    started: Instant,
) -> Result<Discovery_report, String> {
    let client = build_client()?;
    let mut probes = stream::iter(inventory.candidates.into_iter().map(|candidate| {
        let client = client.clone();
        async move { probe_candidate(&client, candidate, port).await }
    }))
    .buffer_unordered(PROBE_CONCURRENCY);
    let mut counts = Discovery_counts {
        skipped: inventory.skipped,
        ..Discovery_counts::default()
    };
    let mut instances = Vec::new();
    let mut checked_count = 0;
    while let Some(outcome) = probes.next().await {
        checked_count += 1;
        match outcome {
            Probe_outcome::Online(instance) => {
                counts.online += 1;
                instances.push(instance);
            }
            Probe_outcome::AuthRequired => counts.auth_required += 1,
            Probe_outcome::Offline => counts.offline += 1,
            Probe_outcome::Unverified => counts.unverified += 1,
            Probe_outcome::Failed => counts.failed += 1,
        }
    }
    instances.sort_by(|left, right| {
        right
            .is_self
            .cmp(&left.is_self)
            .then_with(|| left.device_name.cmp(&right.device_name))
            .then_with(|| left.address.cmp(&right.address))
    });
    Ok(Discovery_report {
        request_id,
        port,
        checked_at: chrono::Utc::now().timestamp_millis(),
        elapsed_ms: started.elapsed().as_millis() as u64,
        peer_count: inventory.peer_count,
        checked_count,
        limited: inventory.limited,
        counts,
        instances,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admission_and_cancellation_are_request_owned() {
        let service = Tailnet_discovery_service::new();
        let (mut first, _receiver) = service.begin("request_a").unwrap();
        assert!(service.begin("request_b").is_err());
        assert!(!service.cancel("request_b"));
        assert!(service.cancel("request_a"));
        assert!(first.cancelled.load(Ordering::SeqCst));
        assert_eq!(service.status(8989).phase, "cancelling");
        first.finish(&Err("tailnet_discovery_cancelled".into()));
        drop(first);
        assert_eq!(service.status(8989).phase, "cancelled");
        let (_second, _receiver) = service.begin("request_b").unwrap();
        assert!(!service.cancel("request_a"));
    }

    #[test]
    fn validates_request_ids_and_saved_port_without_network_calls() {
        let service = Tailnet_discovery_service::new();
        for id in ["", "../other", "request with spaces"] {
            assert!(service.begin(id).is_err());
        }
        let db = Database::memory().unwrap();
        assert_eq!(discovery_port(&db).unwrap(), 8989);
        let mut config = db.get_proxy_config().unwrap();
        config.port = 9001;
        db.save_proxy_config(&config).unwrap();
        assert_eq!(discovery_port(&db).unwrap(), 9001);
    }
}
