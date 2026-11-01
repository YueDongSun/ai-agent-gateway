//! Saved opt-in policy and the application-owned publication lifecycle.

use super::{
    metadata, PublicationConfig, PublicationManager, PublicationSnapshot, PublicationState,
};
use crate::services::proxy_runtime::lifecycle::Proxy_completion;
use crate::{database::Database, proxy::server::ProxyAppState};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex, OnceLock,
};

pub(crate) const KEY_CONFIG: &str = "tailnet_publication_config";
const DEFAULT_PORT: u16 = 8989;
const CONFIG_LIMIT: usize = 4096;

pub(crate) fn read_config(db: &Database) -> Result<PublicationConfig, String> {
    let saved = db.get_setting(KEY_CONFIG).map_err(String::from)?;
    match saved {
        None => Ok(PublicationConfig {
            enabled: false,
            port: DEFAULT_PORT,
        }),
        Some(saved) if saved.len() <= CONFIG_LIMIT => {
            serde_json::from_str(&saved).map_err(|_| "tailnet_publication_config_invalid".into())
        }
        Some(_) => Err("tailnet_publication_config_invalid".into()),
    }
}

pub(crate) fn save_config(db: &Database, config: &PublicationConfig) -> Result<(), String> {
    // Revalidate even callers constructing this internal DTO directly.
    let encoded =
        serde_json::to_string(config).map_err(|_| "tailnet_publication_config_invalid")?;
    let _: PublicationConfig =
        serde_json::from_str(&encoded).map_err(|_| "tailnet_publication_config_invalid")?;
    db.set_setting(KEY_CONFIG, &encoded).map_err(String::from)
}

#[allow(non_camel_case_types)]
#[derive(Clone)]
pub(crate) struct Publication_service {
    inner: Arc<PublicationServiceState>,
}

struct PublicationServiceState {
    manager: OnceLock<PublicationManager>,
    exiting: AtomicBool,
    last_error: Mutex<Option<String>>,
    exit_signal: tokio::sync::watch::Sender<bool>,
    operation: tokio::sync::Mutex<()>,
    gateway: Mutex<Option<Proxy_completion>>,
    watcher: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Publication_service {
    pub fn new() -> Self {
        let (exit_signal, _) = tokio::sync::watch::channel(false);
        Self {
            inner: Arc::new(PublicationServiceState {
                manager: OnceLock::new(),
                exiting: AtomicBool::new(false),
                last_error: Mutex::new(None),
                exit_signal,
                operation: tokio::sync::Mutex::new(()),
                gateway: Mutex::new(None),
                watcher: Mutex::new(None),
            }),
        }
    }

    pub fn cancel(&self) {
        if let Some(manager) = self.inner.manager.get() {
            manager.shutdown_handle().cancel();
        }
    }

    pub fn request_exit(&self) {
        self.inner.exiting.store(true, Ordering::SeqCst);
        self.inner.exit_signal.send_replace(true);
        self.cancel();
    }

    fn record<T>(&self, result: Result<T, String>) -> Result<T, String> {
        *self
            .inner
            .last_error
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = result.as_ref().err().cloned();
        result
    }

    pub async fn start(
        &self,
        config: PublicationConfig,
        state: ProxyAppState,
    ) -> Result<PublicationSnapshot, String> {
        let _operation = self.inner.operation.lock().await;
        if self.inner.exiting.load(Ordering::SeqCst) {
            return self.record(Err("tailnet_publication_exiting".into()));
        }
        let completion = self
            .inner
            .gateway
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        if completion
            .as_ref()
            .is_none_or(Proxy_completion::has_finished)
        {
            return self.record(Err("tailnet_publication_local_gateway_required".into()));
        }
        config
            .plan(&state.auth_token, "127.0.0.1:1")
            .map_err(|error| error.to_string())?;
        let manager = match self.inner.manager.get() {
            Some(manager) => manager,
            None => {
                let executable =
                    self.record(metadata::trusted_executable().map_err(|error| error.to_string()))?;
                self.inner
                    .manager
                    .get_or_init(|| PublicationManager::new(executable))
            }
        };
        let mut exiting = self.inner.exit_signal.subscribe();
        let result = tokio::select! {
            biased;
            _ = async {
                while !*exiting.borrow_and_update() {
                    if exiting.changed().await.is_err() { break; }
                }
            } => Err("tailnet_publication_exiting".into()),
            _ = completion.unwrap().wait() => Err("tailnet_publication_local_gateway_required".into()),
            result = manager.start(config, state) => result,
        };
        self.record(result)
    }

    pub async fn start_saved(&self, db: &Database, state: ProxyAppState) -> Result<(), String> {
        let config = self.record(read_config(db))?;
        if !config.enabled {
            return Ok(());
        }
        self.start(config, state).await.map(|_| ())
    }

    pub async fn stop(&self) -> Result<PublicationSnapshot, String> {
        self.cancel();
        let _operation = self.inner.operation.lock().await;
        let result = self.stop_owned().await;
        if self.inner.exiting.load(Ordering::SeqCst) {
            self.stop_watcher().await;
        }
        result
    }

    async fn stop_owned(&self) -> Result<PublicationSnapshot, String> {
        let result = match self.inner.manager.get() {
            Some(manager) => manager.stop().await,
            None => Ok(stopped_snapshot()),
        };
        self.record(result)
    }

    pub async fn status(&self) -> PublicationSnapshot {
        let _operation = self.inner.operation.lock().await;
        let mut snapshot = match self.inner.manager.get() {
            Some(manager) => manager.status().await,
            None => stopped_snapshot(),
        };
        if snapshot.error.is_none() {
            snapshot.error = self
                .inner
                .last_error
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone();
        }
        snapshot
    }

    /// Called while proxy state is owned, before publishing its new run.
    pub async fn bind_gateway(&self, completion: Proxy_completion) {
        let _operation = self.inner.operation.lock().await;
        self.stop_watcher().await;
        *self
            .inner
            .gateway
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(completion.clone());
        if self.inner.exiting.load(Ordering::SeqCst) {
            return;
        }
        let service = self.clone();
        let mut exiting = self.inner.exit_signal.subscribe();
        let watcher = tokio::spawn(async move {
            tokio::select! {
                biased;
                _ = async {
                    while !*exiting.borrow_and_update() {
                        if exiting.changed().await.is_err() { break; }
                    }
                } => {},
                _ = completion.clone().wait() => service.gateway_finished(completion).await,
            }
        });
        *self
            .inner
            .watcher
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(watcher);
    }

    async fn stop_watcher(&self) {
        let watcher = self
            .inner
            .watcher
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        if let Some(watcher) = watcher {
            watcher.abort();
            let _ = watcher.await;
        }
    }

    pub(super) async fn gateway_finished(&self, completion: Proxy_completion) {
        {
            let binding = self
                .inner
                .gateway
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if !binding
                .as_ref()
                .is_some_and(|current| current.same_source(&completion))
                || !completion.has_finished()
            {
                return;
            }
            // Bind replacement and this immediate cancellation share the
            // source lock. The old source cannot cancel a newly bound run.
            self.cancel();
        }
        let _operation = self.inner.operation.lock().await;
        let current = self
            .inner
            .gateway
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .is_some_and(|current| current.same_source(&completion));
        if !current {
            return;
        }
        self.cancel();
        let _ = self.stop_owned().await;
    }

    #[cfg(test)]
    pub(super) fn with_fixture_manager(manager: PublicationManager) -> Self {
        let service = Self::new();
        assert!(service.inner.manager.set(manager).is_ok());
        service
    }
}

fn stopped_snapshot() -> PublicationSnapshot {
    PublicationSnapshot {
        state: PublicationState::Stopped,
        port: None,
        error: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_policy_defaults_off_and_roundtrips_an_explicit_port() {
        let db = Database::memory().unwrap();
        let default = read_config(&db).unwrap();
        assert!(!default.enabled);
        assert_eq!(default.port, 8989);
        save_config(
            &db,
            &PublicationConfig {
                enabled: true,
                port: 8443,
            },
        )
        .unwrap();
        let saved = read_config(&db).unwrap();
        assert!(saved.enabled);
        assert_eq!(saved.port, 8443);
    }

    #[test]
    fn malformed_policy_cannot_enable_publication_or_overwrite_valid_policy() {
        let db = Database::memory().unwrap();
        for raw in [
            r#"{"enabled":true,"port":0}"#,
            r#"{"enabled":true,"port":65536}"#,
            r#"{"enabled":"true","port":443}"#,
            r#"{"enabled":true,"port":443,"backend":"other"}"#,
            "not-json",
        ] {
            db.set_setting(KEY_CONFIG, raw).unwrap();
            assert!(read_config(&db).is_err());
        }
        save_config(
            &db,
            &PublicationConfig {
                enabled: false,
                port: 443,
            },
        )
        .unwrap();
        assert!(save_config(
            &db,
            &PublicationConfig {
                enabled: true,
                port: 0
            }
        )
        .is_err());
        assert!(!read_config(&db).unwrap().enabled);
    }
}
