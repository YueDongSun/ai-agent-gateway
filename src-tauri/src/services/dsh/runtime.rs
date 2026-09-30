//! Owned Node lifecycle with descendant cleanup, bounded execution and an
//! authenticated readiness probe. Raw child output never enters AAG logs.
use super::{
    config::{self, failure, Instance_lock, MANAGED_PROFILE},
    owned_process::Owned_process,
    runtime_environment::Runtime_environment,
    types::*,
    Dsh_service,
};
use crate::error::AppError;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    net::TcpListener,
    path::Path,
    sync::atomic::Ordering,
    thread,
    time::{Duration, Instant},
};

pub const SUPPORTED_DSH_VERSION: &str = "0.1.1-rc.2";
pub const SUPPORTED_PNPM_VERSION: &str = "10.34.5";

/// A dropped IPC future must not leave a child and exclusive instance lock in
/// the starting phase. Generation binding prevents cleanup of a replacement.
struct Startup_guard {
    state: std::sync::Weak<std::sync::Mutex<super::Manager_state>>,
    runtime_id: String,
}

impl Drop for Startup_guard {
    fn drop(&mut self) {
        let Some(state) = self.state.upgrade() else {
            return;
        };
        let Ok(mut state) = state.lock() else {
            return;
        };
        if state.runtime_id.as_deref() != Some(self.runtime_id.as_str())
            || state.runtime.phase != "starting"
        {
            return;
        }
        state.child.take();
        state.runtime_lock.take();
        state.bootstrap_url = None;
        state.runtime_id = None;
        state.runtime = Dsh_runtime {
            phase: "failed".into(),
            pid: None,
            web_url: None,
            error_code: Some("startup_cancelled".into()),
        };
    }
}

pub(super) fn validate_executables(
    settings: &Dsh_settings,
    need_dsh: bool,
) -> Result<(), AppError> {
    config::validate_settings(settings)?;
    if settings.node_path.is_empty() {
        return Err(failure("node_path_required"));
    }
    if need_dsh {
        if settings.dsh_entry_path.is_empty() {
            return Err(failure("dsh_entry_required"));
        }
        validate_package_entry(
            Path::new(&settings.dsh_entry_path),
            "@deepseek-ai/dsh",
            SUPPORTED_DSH_VERSION,
        )?;
    } else {
        if settings.pnpm_entry_path.is_empty() {
            return Err(failure("pnpm_entry_required"));
        }
        validate_package_entry(
            Path::new(&settings.pnpm_entry_path),
            "pnpm",
            SUPPORTED_PNPM_VERSION,
        )?;
    }
    Ok(())
}

fn validate_package_entry(entry: &Path, package_name: &str, version: &str) -> Result<(), AppError> {
    let resolved_entry =
        std::fs::canonicalize(entry).map_err(|_| failure("runtime_entry_unreadable"))?;
    for directory in resolved_entry.ancestors().skip(1).take(5) {
        if let Some(manifest) = config::read_json(&directory.join("package.json"))? {
            if manifest.get("name").and_then(Value::as_str) == Some(package_name) {
                if manifest.get("version").and_then(Value::as_str) != Some(version) {
                    return Err(failure("unsupported_runtime_version"));
                }
                let bin_name = if package_name == "pnpm" {
                    "pnpm"
                } else {
                    "dsh"
                };
                let declared = manifest
                    .get("bin")
                    .and_then(|bin| {
                        bin.as_str()
                            .or_else(|| bin.get(bin_name).and_then(Value::as_str))
                    })
                    .filter(|entry| super::discovery::safe_relative_entry(entry))
                    .ok_or_else(|| failure("runtime_bin_invalid"))?;
                let declared_entry = std::fs::canonicalize(directory.join(declared))
                    .map_err(|_| failure("runtime_entry_unreadable"))?;
                if !declared_entry.starts_with(directory) {
                    return Err(failure("runtime_entry_escape"));
                }
                if declared_entry != resolved_entry {
                    return Err(failure("runtime_entry_not_declared"));
                }
                return Ok(());
            }
        }
    }
    Err(failure("runtime_package_identity_missing"))
}

impl Dsh_service {
    pub async fn start(
        &self,
        llm_settings: Value,
        secret_env: BTreeMap<String, String>,
        expected_settings_digest: String,
    ) -> Result<Dsh_snapshot, AppError> {
        let (settings, captured_settings_digest) = config::read_settings(&self.root)?;
        if captured_settings_digest != expected_settings_digest {
            return Err(failure("settings_changed"));
        }
        validate_executables(&settings, true)?;
        if !llm_settings.is_object() {
            return Err(failure("invalid_llm_settings"));
        }
        for key in secret_env.keys() {
            if !["AAG_DSH_API_KEY", "ANTHROPIC_AUTH_TOKEN"].contains(&key.as_str()) {
                return Err(failure("invalid_secret_environment"));
            }
        }
        let (token, runtime_id) = (secure_token()?, secure_token()?);
        let base_url = format!("http://127.0.0.1:{}", settings.port);
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(2))
            .build()
            .map_err(|_| failure("probe_client_failed"))?;
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| failure("state_lock_failed"))?;
            if state.job.is_some() || state.child.is_some() {
                return Err(failure("instance_busy"));
            }
            let instance_lock = Instance_lock::acquire(&self.root)?;
            super::plugins::ensure_recovered(&self.root)?;
            if config::read_settings(&self.root)?.1 != expected_settings_digest {
                return Err(failure("settings_changed"));
            }
            if !self
                .root
                .join(MANAGED_PROFILE)
                .join("package.json")
                .is_file()
            {
                return Err(failure("not_initialized"));
            }
            // Readiness must never adopt an already-listening foreign process.
            let listener = TcpListener::bind(("127.0.0.1", settings.port))
                .map_err(|_| failure("port_occupied"))?;
            for file in [".credentials.yaml", "credentials.yaml"] {
                if !secret_env.is_empty()
                    && config::read_optional(&self.root.join(file))?
                        .is_some_and(|data| data.iter().any(|byte| !byte.is_ascii_whitespace()))
                {
                    return Err(failure("credential_store_conflict"));
                }
            }
            if config::read_optional(&self.root.join("settings.yaml"))?.is_some() {
                return Err(failure("legacy_settings_file_requires_review"));
            }
            let environment = Runtime_environment::new(&self.root);
            let mut command = environment.prepared_command(&settings.node_path)?;
            let settings_path = self.root.join("settings.json");
            let mut effective_settings =
                config::read_json(&settings_path)?.unwrap_or_else(|| serde_json::json!({}));
            let binding_path = self.root.join("aag_model_binding.json");
            let previous_binding = config::read_json(&binding_path)?;
            let next_binding = super::model_settings::apply(
                &mut effective_settings,
                &llm_settings,
                previous_binding,
            )?;
            effective_settings
                .as_object_mut()
                .ok_or_else(|| failure("invalid_managed_settings"))?
                .insert(
                    "ui-theme".into(),
                    serde_json::json!({"preference":settings.theme}),
                );
            config::write_atomic(
                &settings_path,
                &serde_json::to_vec_pretty(&effective_settings)?,
            )?;
            config::write_atomic(&binding_path, &serde_json::to_vec_pretty(&next_binding)?)?;
            let overlay_path = self.root.join("aag_runtime.patch.json");
            config::write_atomic(
                &overlay_path,
                &serde_json::to_vec_pretty(
                    &serde_json::json!([{"id":"settings","config":{"path":settings_path}}]),
                )?,
            )?;
            let preload_path = self.root.join("dsh_preload.mjs");
            config::write_atomic(
                &preload_path,
                include_bytes!("../../../resources/dsh_preload.mjs"),
            )?;

            command
                .arg("--expose-internals")
                .arg("--import")
                .arg(
                    url::Url::from_file_path(&preload_path)
                        .map_err(|_| failure("invalid_preload_path"))?
                        .as_str(),
                )
                .arg(&settings.dsh_entry_path)
                .args(["--profile", "web", "--patch"])
                .arg(&overlay_path)
                .args([
                    "--host",
                    "127.0.0.1",
                    "--port",
                    &settings.port.to_string(),
                    "--no-open",
                ])
                .current_dir(&environment.workspace)
                .envs(secret_env)
                .env("AAG_DSH_CONTROL_TOKEN", &token)
                .env("AAG_DSH_RUNTIME_ID", &runtime_id)
                .env("AAG_DSH_PORT", settings.port.to_string());
            drop(listener);
            let child = Owned_process::spawn(command)?;
            state.runtime = Dsh_runtime {
                phase: "starting".into(),
                pid: Some(child.id()),
                web_url: None,
                error_code: None,
            };
            state.child = Some(child);
            state.runtime_id = Some(runtime_id.clone());
            state.runtime_lock = Some(instance_lock);
            state.cancellation.store(false, Ordering::SeqCst);
            // Bootstrap URL is deliberately retained only in process memory.
            state.bootstrap_url = Some(format!("{base_url}/__aag_bootstrap?token={token}"));
        }
        let _startup_guard = Startup_guard {
            state: std::sync::Arc::downgrade(&self.state),
            runtime_id: runtime_id.clone(),
        };
        let deadline = Instant::now() + Duration::from_secs(settings.startup_timeout_secs);
        let result = loop {
            {
                let mut state = self
                    .state
                    .lock()
                    .map_err(|_| failure("state_lock_failed"))?;
                if state.runtime_id.as_deref() != Some(runtime_id.as_str()) {
                    return Err(failure("runtime_generation_changed"));
                }
                if state.cancellation.load(Ordering::SeqCst) || state.child.is_none() {
                    break Err(failure("cancelled"));
                }
                match state
                    .child
                    .as_mut()
                    .ok_or_else(|| failure("process_missing"))?
                    .try_wait()
                {
                    Ok(None) => {}
                    Ok(Some(_)) => break Err(failure("runtime_exited")),
                    Err(error) => break Err(error),
                }
            }
            if let Ok(response) = client
                .get(format!("{base_url}/__aag_health"))
                .bearer_auth(&token)
                .send()
                .await
            {
                if response.status().is_success() {
                    if let Ok(health) = response.json::<Value>().await {
                        if health.get("runtime_id").and_then(Value::as_str)
                            == Some(runtime_id.as_str())
                            && health.get("ready").and_then(Value::as_bool) == Some(true)
                        {
                            // Confirm that the actual DSH HTML application has
                            // mounted, rather than counting the guard's health
                            // endpoint alone as application readiness.
                            if let Ok(page) = client
                                .get(format!("{base_url}/"))
                                .bearer_auth(&token)
                                .send()
                                .await
                            {
                                if page.status().is_success()
                                    && page
                                        .headers()
                                        .get(reqwest::header::CONTENT_TYPE)
                                        .and_then(|value| value.to_str().ok())
                                        .is_some_and(|value| value.contains("text/html"))
                                {
                                    if let Ok(body) = page.text().await {
                                        if body.contains("__DSH_BOOT__")
                                            || (body.contains("<html") && body.contains("<script"))
                                        {
                                            break Ok(());
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if Instant::now() >= deadline {
                break Err(failure("startup_timed_out"));
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        };
        if let Err(error) = result {
            let mut state = self
                .state
                .lock()
                .map_err(|_| failure("state_lock_failed"))?;
            if state.runtime_id.as_deref() != Some(runtime_id.as_str()) {
                return Err(failure("runtime_generation_changed"));
            }
            state.child.take();
            state.runtime_lock.take();
            state.bootstrap_url = None;
            state.runtime_id = None;
            state.runtime = Dsh_runtime {
                phase: "failed".into(),
                pid: None,
                web_url: None,
                error_code: Some(
                    error
                        .to_string()
                        .strip_prefix("dsh:")
                        .unwrap_or("startup_failed")
                        .into(),
                ),
            };
            return Err(error);
        }
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| failure("state_lock_failed"))?;
            if state.runtime_id.as_deref() != Some(runtime_id.as_str()) {
                return Err(failure("runtime_generation_changed"));
            }
            if state.cancellation.load(Ordering::SeqCst)
                || !matches!(
                    state.child.as_mut().map(|child| child.try_wait()),
                    Some(Ok(None))
                )
            {
                state.child.take();
                state.runtime_lock.take();
                state.bootstrap_url = None;
                state.runtime_id = None;
                state.runtime = Dsh_runtime {
                    phase: "failed".into(),
                    pid: None,
                    web_url: None,
                    error_code: Some("cancelled".into()),
                };
                return Err(failure("cancelled"));
            }
            state.runtime.phase = "web_ready".into();
            state.runtime.web_url = Some(base_url);
        }
        spawn_observer(std::sync::Arc::downgrade(&self.state), runtime_id);
        self.snapshot()
    }

    /// App-exit hook: request cancellation, allow the owned package worker to
    /// reap descendants and persist its receipt, then stop the owned runtime.
    pub fn shutdown(&self) -> Result<(), AppError> {
        self.cancel();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let busy = self
                .state
                .lock()
                .map_err(|_| failure("state_lock_failed"))?
                .job
                .is_some();
            if !busy {
                self.stop()?;
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(failure("shutdown_timed_out"));
            }
            thread::sleep(Duration::from_millis(25));
        }
    }

    pub fn stop(&self) -> Result<Dsh_snapshot, AppError> {
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| failure("state_lock_failed"))?;
            if state.job.is_some() {
                return Err(failure("instance_busy"));
            }
            state.cancellation.store(true, Ordering::SeqCst);
            state.child.take();
            state.runtime_lock.take();
            state.bootstrap_url = None;
            state.runtime_id = None;
            state.runtime = Dsh_runtime::default();
        }
        self.snapshot()
    }

    /// The authenticated bootstrap URL goes only to the native window builder.
    /// Never expose it in snapshots, receipts, diagnostic output or application logs.
    pub fn validated_web_url(&self) -> Result<String, AppError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| failure("state_lock_failed"))?;
        if state.runtime.phase != "web_ready"
            || state
                .child
                .as_mut()
                .ok_or_else(|| failure("runtime_not_ready"))?
                .try_wait()?
                .is_some()
        {
            return Err(failure("runtime_not_ready"));
        }
        state
            .bootstrap_url
            .clone()
            .ok_or_else(|| failure("runtime_not_ready"))
    }
}

fn secure_token() -> Result<String, AppError> {
    let mut bytes = [0_u8; 32];
    getrandom::getrandom(&mut bytes).map_err(|_| failure("secure_random_unavailable"))?;
    Ok(hex::encode(bytes))
}

fn spawn_observer(
    state: std::sync::Weak<std::sync::Mutex<super::Manager_state>>,
    runtime_id: String,
) {
    thread::spawn(move || loop {
        thread::sleep(Duration::from_millis(250));
        let Some(state_arc) = state.upgrade() else {
            break;
        };
        let Ok(mut state) = state_arc.lock() else {
            break;
        };
        if state.runtime_id.as_deref() != Some(runtime_id.as_str()) {
            break;
        }
        let Some(child) = state.child.as_mut() else {
            break;
        };
        if !matches!(child.try_wait(), Ok(None)) {
            state.child.take();
            state.runtime_lock.take();
            state.bootstrap_url = None;
            state.runtime_id = None;
            state.runtime = Dsh_runtime {
                phase: "failed".into(),
                pid: None,
                web_url: None,
                error_code: Some("runtime_exited".into()),
            };
            break;
        }
    });
}
