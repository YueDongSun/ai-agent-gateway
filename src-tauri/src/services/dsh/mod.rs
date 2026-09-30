//! A single, explicitly initialized DSH instance isolated from ~/.dsh and
//! ~/.claude. Reading a snapshot never initializes files or probes a service.
#![allow(non_camel_case_types)]
mod catalog;
mod config;
mod discovery;
pub mod setup;
pub mod setup_preferences;
pub use discovery::{Dsh_discovery, Dsh_runtime_candidate, Dsh_runtime_paths};
mod model_settings;
mod owned_process;
mod plugins;
mod runtime;
mod runtime_environment;
pub mod types;
pub mod upstreams;

use crate::error::AppError;
use config::{failure, Instance_lock};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};
pub use types::*;

pub(super) struct Pending_preview {
    request: Dsh_operation_request,
    digest: String,
    expires_at_ms: u64,
}

pub(super) struct Manager_state {
    previews: BTreeMap<String, Pending_preview>,
    setup_plans: BTreeMap<String, setup::Pending_setup>,
    runtime: Dsh_runtime,
    child: Option<owned_process::Owned_process>,
    runtime_lock: Option<Instance_lock>,
    bootstrap_url: Option<String>,
    runtime_id: Option<String>,
    job: Option<Dsh_job>,
    cancellation: Arc<AtomicBool>,
}

#[allow(non_camel_case_types)]
#[derive(Clone)]
pub struct Dsh_service {
    pub(super) root: Arc<PathBuf>,
    pub(super) state: Arc<Mutex<Manager_state>>,
}

impl Dsh_service {
    /// base_dir is AAG's resolved configuration directory, not the user's home.
    pub fn new(base_dir: PathBuf) -> Self {
        Self {
            root: Arc::new(base_dir.join("dsh")),
            state: Arc::new(Mutex::new(Manager_state {
                previews: BTreeMap::new(),
                setup_plans: BTreeMap::new(),
                runtime: Dsh_runtime::default(),
                child: None,
                runtime_lock: None,
                bootstrap_url: None,
                runtime_id: None,
                job: None,
                cancellation: Arc::new(AtomicBool::new(false)),
            })),
        }
    }

    pub fn snapshot(&self) -> Result<Dsh_snapshot, AppError> {
        let state = self
            .state
            .lock()
            .map_err(|_| failure("state_lock_failed"))?;
        let (settings, settings_digest) = config::read_settings(&self.root)?;
        let plugins = catalog::inventory(&self.root)?;
        let receipts = plugins::read_receipts(&self.root)?;
        Ok(Dsh_snapshot {
            settings,
            settings_digest,
            initialized: self
                .root
                .join(config::MANAGED_PROFILE)
                .join("package.json")
                .is_file(),
            managed_home: self.root.to_string_lossy().into_owned(),
            plugins,
            runtime: state.runtime.clone(),
            job: state.job.clone(),
            receipts,
        })
    }

    pub fn save_settings(
        &self,
        settings: Dsh_settings,
        expected_digest: String,
    ) -> Result<Dsh_snapshot, AppError> {
        config::validate_settings(&settings)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| failure("state_lock_failed"))?;
        if state.job.is_some() || state.child.is_some() {
            return Err(failure("instance_busy"));
        }
        let _instance_lock = Instance_lock::acquire(&self.root)?;
        plugins::ensure_recovered(&self.root)?;
        if config::read_settings(&self.root)?.1 != expected_digest {
            return Err(failure("settings_changed"));
        }
        config::write_atomic(
            &self.root.join(config::SETTINGS_FILE),
            &serde_json::to_vec_pretty(&settings)?,
        )?;
        state.previews.clear();
        state.setup_plans.clear();
        drop(state);
        self.snapshot()
    }

    pub fn cancel(&self) -> bool {
        if let Ok(state) = self.state.lock() {
            if state.job.is_some() || state.runtime.phase == "starting" {
                state.cancellation.store(true, Ordering::SeqCst);
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod acceptance_tests;

pub mod market_catalog;
#[cfg(test)]
mod package_acceptance_tests;
