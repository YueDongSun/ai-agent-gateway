//! Preview-bound plugin transactions. Package installation runs in a disposable
//! stage; the active profile is replaced only after validation and digest checks.
use super::{
    catalog,
    config::{self, failure, Instance_lock, MANAGED_PROFILE},
    owned_process, runtime,
    runtime_environment::Runtime_environment,
    types::*,
    Dsh_service, Pending_preview,
};
use crate::error::AppError;
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::Ordering,
};

const PREVIEW_TTL_MS: u64 = 300_000;

impl Dsh_service {
    pub fn preview(
        &self,
        request: Dsh_operation_request,
    ) -> Result<Dsh_operation_preview, AppError> {
        ensure_recovered(&self.root)?;
        validate_request(&self.root, &request)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| failure("state_lock_failed"))?;
        if state.job.is_some() || state.child.is_some() {
            return Err(failure("instance_busy"));
        }
        let preview_id = config::operation_id();
        let expires_at_ms = config::now_ms() + PREVIEW_TTL_MS;
        let current_digest = preview_digest(&self.root, &request)?;
        state
            .previews
            .retain(|_, item| item.expires_at_ms >= config::now_ms());
        if state.previews.len() >= 16 {
            state.previews.clear();
        }
        state.previews.insert(
            preview_id.clone(),
            Pending_preview {
                request: request.clone(),
                digest: current_digest,
                expires_at_ms,
            },
        );
        let changes = if request.action == "initialize" {
            vec!["Create an isolated DSH web profile and managed workspace under the AAG configuration directory.".into(),
                "Use the configured runtime's bundled base and WebUI compositions; no runtime download or global install.".into()]
        } else {
            vec![format!("{} {}{} in the isolated web profile.", request.action, request.package_name.as_deref().unwrap_or_default(), request.version.as_ref().map(|version| format!("@{version}")).unwrap_or_default()),
                "Keep the running instance stopped; preserve the original manifest, lockfile and package tree for rollback.".into()]
        };
        let mut warnings = vec!["The user's existing .dsh and .claude configuration is not adopted or modified.".into(),
            "Installed, configured-enabled and runtime-loaded are separate states; runtime plugin phase remains unknown until verified.".into()];
        if request.action == "install" || request.action == "remove" {
            warnings.push("pnpm may download dependencies from the public npm registry. Lifecycle scripts and pnpm hooks are disabled; plugins needing build scripts are unsupported.".into());
            warnings.push("Installed plugin code is not sandboxed when DSH is started. A plain module without dsh.bundle.patch is installed but is not enabled as a bundle.".into());
        }
        Ok(Dsh_operation_preview {
            preview_id,
            action: request.action,
            package_name: request.package_name,
            version: request.version,
            changes,
            warnings,
            expires_at_ms,
        })
    }

    /// Blocking: call from spawn_blocking at the IPC boundary. A preview is
    /// consumed exactly once, including on a failed or cancelled transaction.
    pub fn execute(&self, preview_id: String) -> Result<Dsh_receipt, AppError> {
        let (preview, cancellation, instance_lock) = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| failure("state_lock_failed"))?;
            if state.job.is_some() || state.child.is_some() {
                return Err(failure("instance_busy"));
            }
            let preview = state
                .previews
                .remove(&preview_id)
                .ok_or_else(|| failure("preview_not_found"))?;
            if config::now_ms() > preview.expires_at_ms {
                return Err(failure("preview_expired"));
            }
            if preview_digest(&self.root, &preview.request)? != preview.digest {
                return Err(failure("profile_changed"));
            }
            let instance_lock = Instance_lock::acquire(&self.root)?;
            ensure_recovered(&self.root)?;
            // A different process can commit between the first read and lock
            // acquisition. Refuse stale authorization before package execution.
            if preview_digest(&self.root, &preview.request)? != preview.digest {
                return Err(failure("profile_changed"));
            }
            validate_request(&self.root, &preview.request)?;
            state.cancellation.store(false, Ordering::SeqCst);
            state.job = Some(Dsh_job {
                operation_id: preview_id.clone(),
                action: preview.request.action.clone(),
                package_name: preview.request.package_name.clone(),
                phase: "running".into(),
            });
            (preview, state.cancellation.clone(), instance_lock)
        };
        let started_at_ms = config::now_ms();
        let request = &preview.request;
        let mut receipt = Dsh_receipt {
            operation_id: preview_id.clone(),
            action: request.action.clone(),
            package_name: request.package_name.clone(),
            version: request.version.clone(),
            status: "failed".into(),
            error_code: None,
            started_at_ms,
            finished_at_ms: started_at_ms,
            rollback_status: "not_needed".into(),
            restart_required: false,
        };
        let journal_dir = self.root.join("operations").join(&preview_id);
        let result = (|| {
            config::safe_path(&journal_dir)?;
            fs::create_dir_all(&journal_dir).map_err(|_| failure("journal_create_failed"))?;
            config::write_new(
                &journal_dir.join("request.json"),
                &serde_json::to_vec_pretty(request)?,
            )?;
            if cancellation.load(Ordering::SeqCst) {
                return Err(failure("cancelled"));
            }
            apply_operation(
                &self.root,
                &journal_dir,
                &preview,
                &cancellation,
                &mut receipt.rollback_status,
                &self.state,
            )
        })();
        match result {
            Ok(()) => {
                receipt.status = "succeeded".into();
                receipt.restart_required = true;
            }
            Err(error) => {
                let code = error
                    .to_string()
                    .strip_prefix("dsh:")
                    .unwrap_or("operation_failed")
                    .to_string();
                receipt.status = if receipt.rollback_status == "failed" {
                    "rollback_failed".into()
                } else {
                    match code.as_str() {
                        "cancelled" => "cancelled",
                        "timed_out" => "timed_out",
                        "rollback_failed" => "rollback_failed",
                        _ => "failed",
                    }
                    .into()
                };
                receipt.error_code = Some(code);
            }
        }
        Ok(finish_receipt(
            &journal_dir,
            receipt,
            &self.state,
            instance_lock,
        ))
    }
}

/// Receipt fidelity contract: the journal commit markers are authoritative and
/// the receipt is only their report. Persisting the report uses one bounded
/// retry and degrades gracefully; a persistence failure can never invert a
/// committed outcome, mask the real operation failure, or skip job/lock
/// cleanup (which must also survive a serialization failure).
pub(super) fn finish_receipt(
    journal_dir: &Path,
    mut receipt: Dsh_receipt,
    state: &std::sync::Mutex<super::Manager_state>,
    instance_lock: Instance_lock,
) -> Dsh_receipt {
    receipt.finished_at_ms = config::now_ms();
    let persisted = serde_json::to_vec_pretty(&receipt)
        .map_err(|_| failure("receipt_persist_failed"))
        .and_then(|bytes| persist_receipt(journal_dir, &bytes));
    if let Ok(mut guard) = state.lock() {
        guard.job = None;
        guard.previews.clear();
    }
    drop(instance_lock);
    if persisted.is_err() {
        // Best-effort forensic marker inside the owned journal so a missing
        // history entry stays discoverable; it never changes the outcome.
        let _ = config::write_new(&journal_dir.join("receipt_persist_failed.json"), b"{}\n");
    }
    receipt
}

/// Bounded single retry: write_atomic keeps a recovery copy; its failure
/// leaves the target absent, so a create_new fallback cannot double-write or
/// clobber a completed receipt.
pub(super) fn persist_receipt(journal_dir: &Path, bytes: &[u8]) -> Result<(), AppError> {
    let path = journal_dir.join("receipt.json");
    if config::write_atomic(&path, bytes).is_ok() {
        return Ok(());
    }
    config::write_new(&path, bytes)
}

fn validate_request(root: &Path, request: &Dsh_operation_request) -> Result<(), AppError> {
    if !["initialize", "install", "remove", "enable", "disable"].contains(&request.action.as_str())
    {
        return Err(failure("invalid_action"));
    }
    let profile = root.join(MANAGED_PROFILE);
    let manifest = config::read_json(&profile.join("package.json"))?;
    if request.action == "initialize" {
        if request.package_name.is_some() || request.version.is_some() {
            return Err(failure("invalid_initialize_request"));
        }
        if manifest.is_some() {
            return Err(failure("already_initialized"));
        }
        return Ok(());
    }
    if manifest.is_none() {
        return Err(failure("not_initialized"));
    }
    let package_name = request
        .package_name
        .as_deref()
        .ok_or_else(|| failure("package_required"))?;
    let entry = catalog::validate_package(package_name)?;
    if entry.essential {
        return Err(failure("essential_bundle_protected"));
    }
    if request.action == "install" {
        if !request
            .version
            .as_deref()
            .is_some_and(catalog::exact_version)
        {
            return Err(failure("exact_version_required"));
        }
    } else if request.version.is_some() {
        return Err(failure("unexpected_version"));
    }
    if request.action != "install" {
        let installed = catalog::installed_manifest(&profile, package_name)?
            .ok_or_else(|| failure("package_not_installed"))?;
        if request.action == "enable"
            && catalog::bundle_patch(&profile, package_name, &installed)?.is_none()
        {
            return Err(failure("package_is_not_bundle"));
        }
    }
    Ok(())
}

fn preview_digest(root: &Path, request: &Dsh_operation_request) -> Result<String, AppError> {
    let mut state = config::state_digest(root)?.into_bytes();
    // Enabling loads executable composition. Bind both installed metadata and
    // actual patch bytes, not only the top-level profile membership list.
    if request.action == "enable" {
        let profile = root.join(MANAGED_PROFILE);
        let package_name = request
            .package_name
            .as_deref()
            .ok_or_else(|| failure("package_required"))?;
        let installed = catalog::installed_manifest(&profile, package_name)?
            .ok_or_else(|| failure("package_not_installed"))?;
        let patch = catalog::bundle_patch(&profile, package_name, &installed)?
            .ok_or_else(|| failure("package_is_not_bundle"))?;
        let manifest_bytes = serde_json::to_vec(&installed)?;
        state.extend_from_slice(config::digest(&manifest_bytes).as_bytes());
        state.extend_from_slice(
            config::digest(
                &config::read_optional(&patch)?.ok_or_else(|| failure("bundle_patch_missing"))?,
            )
            .as_bytes(),
        );
    }
    Ok(config::digest(&state))
}

fn apply_operation(
    root: &Path,
    journal: &Path,
    preview: &Pending_preview,
    cancellation: &std::sync::atomic::AtomicBool,
    rollback_status: &mut String,
    state: &std::sync::Mutex<super::Manager_state>,
) -> Result<(), AppError> {
    let request = &preview.request;
    let profile = root.join(MANAGED_PROFILE);
    let stage = journal.join("stage");
    config::safe_path(&stage)?;
    fs::create_dir(&stage).map_err(|_| failure("stage_create_failed"))?;
    let environment = Runtime_environment::new(root);
    let mut manifest =
        config::read_json(&profile.join("package.json"))?.unwrap_or_else(config::profile_manifest);
    if !manifest.is_object()
        || manifest
            .pointer("/dsh/profile/bundles")
            .and_then(Value::as_array)
            .is_none()
    {
        return Err(failure("invalid_profile_manifest"));
    }
    let mut targets = vec!["package.json"];
    match request.action.as_str() {
        "initialize" => {
            environment.prepare()?;
        }
        "enable" | "disable" => {
            environment.prepare()?;
            let package_name = request.package_name.as_deref().unwrap_or_default();
            let bundles = manifest
                .pointer_mut("/dsh/profile/bundles")
                .and_then(Value::as_array_mut)
                .ok_or_else(|| failure("invalid_profile_manifest"))?;
            bundles.retain(|value| value.as_str() != Some(package_name));
            if request.action == "enable" {
                bundles.push(Value::String(package_name.into()));
            }
        }
        "install" | "remove" => {
            let (settings, _) = config::read_settings(root)?;
            config::validate_settings(&settings)?;
            runtime::validate_executables(&settings, false)?;
            // Reject foreign specs before letting the package manager resolve
            // existing dependencies. Registry-only exact versions are required.
            if manifest.get("dependencies").is_none() {
                manifest["dependencies"] = serde_json::json!({});
            }
            let dependencies = manifest
                .get_mut("dependencies")
                .and_then(Value::as_object_mut)
                .ok_or_else(|| failure("invalid_dependencies"))?;
            for (name, version) in dependencies.iter() {
                catalog::validate_package(name)?;
                if !version.as_str().is_some_and(catalog::exact_version) {
                    return Err(failure("unmanaged_dependency_spec"));
                }
            }
            let package_name = request.package_name.as_deref().unwrap_or_default();
            if request.action == "install" {
                dependencies.insert(
                    package_name.into(),
                    Value::String(request.version.clone().unwrap_or_default()),
                );
            } else {
                dependencies.remove(package_name);
            }
            // Copy only the declarative dependency map, not package scripts or
            // pnpm configuration from an externally edited package manifest.
            let stage_manifest = serde_json::json!({"name":"aag_dsh_plugin_stage","private":true,"dependencies":dependencies});
            config::write_new(
                &stage.join("package.json"),
                &serde_json::to_vec_pretty(&stage_manifest)?,
            )?;
            if let Some(lockfile) = config::read_optional(&profile.join("pnpm-lock.yaml"))? {
                config::write_new(&stage.join("pnpm-lock.yaml"), &lockfile)?;
            }
            config::write_new(
                &stage.join(".npmrc"),
                b"ignore-scripts=true\nregistry=https://registry.npmjs.org/\n",
            )?;
            let mut command = environment.prepared_command(&settings.node_path)?;
            command
                .arg(&settings.pnpm_entry_path)
                .args([
                    "install",
                    "--ignore-scripts",
                    "--ignore-workspace",
                    "--no-frozen-lockfile",
                    "--node-linker=hoisted",
                    "--config.manage-package-manager-versions=false",
                    "--config.ignore-pnpmfile=true",
                    "--registry=https://registry.npmjs.org/",
                    "--reporter=silent",
                ])
                .current_dir(&stage);
            command.env("NPM_CONFIG_USERCONFIG", stage.join(".npmrc"));
            owned_process::run_bounded(command, settings.operation_timeout_secs, cancellation)?;
            if request.action == "install" {
                let installed = catalog::installed_manifest(&stage, package_name)?
                    .ok_or_else(|| failure("package_missing_after_install"))?;
                if installed.get("name").and_then(Value::as_str) != Some(package_name)
                    || installed.get("version").and_then(Value::as_str)
                        != request.version.as_deref()
                {
                    return Err(failure("installed_identity_mismatch"));
                }
                if catalog::bundle_patch(&stage, package_name, &installed)?.is_some() {
                    let bundles = manifest
                        .pointer_mut("/dsh/profile/bundles")
                        .and_then(Value::as_array_mut)
                        .ok_or_else(|| failure("invalid_profile_manifest"))?;
                    if !bundles
                        .iter()
                        .any(|value| value.as_str() == Some(package_name))
                    {
                        bundles.push(Value::String(package_name.into()));
                    }
                } else {
                    manifest
                        .pointer_mut("/dsh/profile/bundles")
                        .and_then(Value::as_array_mut)
                        .ok_or_else(|| failure("invalid_profile_manifest"))?
                        .retain(|value| value.as_str() != Some(package_name));
                }
            } else {
                manifest
                    .pointer_mut("/dsh/profile/bundles")
                    .and_then(Value::as_array_mut)
                    .ok_or_else(|| failure("invalid_profile_manifest"))?
                    .retain(|value| value.as_str() != Some(package_name));
            }
            targets.extend(["pnpm-lock.yaml", "node_modules"]);
        }
        _ => return Err(failure("invalid_action")),
    }
    if cancellation.load(Ordering::SeqCst) {
        return Err(failure("cancelled"));
    }
    let _commit_guard = state.lock().map_err(|_| failure("state_lock_failed"))?;
    if preview_digest(root, request)? != preview.digest {
        return Err(failure("profile_changed"));
    }
    // Dependency changes remain staged; the active profile is still untouched.
    if stage.join("package.json").exists() {
        fs::remove_file(stage.join("package.json")).map_err(|_| failure("stage_write_failed"))?;
    }
    config::write_new(
        &stage.join("package.json"),
        &serde_json::to_vec_pretty(&manifest)?,
    )?;
    config::safe_path(&profile)?;
    fs::create_dir_all(&profile).map_err(|_| failure("profile_create_failed"))?;
    config::write_new(
        &journal.join("commit_started.json"),
        &serde_json::to_vec(
            &serde_json::json!({"before_digest":preview.digest,"targets":targets}),
        )?,
    )?;
    let mut committed: Vec<(&str, bool, bool)> = Vec::new();
    let commit_result = (|| {
        for name in targets {
            let destination = profile.join(name);
            config::safe_path(&destination)?;
            let backup = journal.join(format!("backup_{name}"));
            let existed = destination.exists();
            if existed {
                fs::rename(&destination, &backup).map_err(|_| failure("backup_failed"))?;
            }
            committed.push((name, existed, false));
            if stage.join(name).exists() {
                fs::rename(stage.join(name), &destination)
                    .map_err(|_| failure("profile_commit_failed"))?;
                if let Some(item) = committed.last_mut() {
                    item.2 = true;
                }
            }
        }
        if config::read_json(&profile.join("package.json"))?.as_ref() != Some(&manifest) {
            return Err(failure("profile_readback_mismatch"));
        }
        Ok(())
    })();
    if let Err(original_error) = commit_result {
        let mut restored = true;
        for (name, existed, promoted) in committed.into_iter().rev() {
            if promoted
                && fs::rename(profile.join(name), journal.join(format!("failed_{name}"))).is_err()
            {
                restored = false;
                continue;
            }
            if existed
                && fs::rename(journal.join(format!("backup_{name}")), profile.join(name)).is_err()
            {
                restored = false;
            }
        }
        *rollback_status = if restored { "restored" } else { "failed" }.into();
        if restored {
            config::write_new(
                &journal.join("commit_finished.json"),
                br#"{"status":"restored"}"#,
            )?;
        }
        // Keep the original failure code even when restoration also fails.
        // The receipt's status/rollback_status separately report that failure.
        return Err(original_error);
    }
    config::write_new(
        &journal.join("commit_finished.json"),
        br#"{"status":"committed"}"#,
    )?;
    Ok(())
}

pub(super) fn read_receipts(root: &Path) -> Result<Vec<Dsh_receipt>, AppError> {
    let directory = root.join("operations");
    config::safe_path(&directory)?;
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let mut paths: Vec<PathBuf> = Vec::new();
    for entry in fs::read_dir(directory).map_err(|_| failure("receipts_unreadable"))? {
        let entry = entry.map_err(|_| failure("receipts_unreadable"))?;
        let path = entry.path().join("receipt.json");
        if path.exists() {
            paths.push(path);
        }
    }
    // Receipts carry timestamps; IDs are deliberately not chronological.
    let mut receipts = Vec::new();
    for path in paths.drain(..) {
        if let Some(data) = config::read_optional(&path)? {
            receipts.push(
                serde_json::from_slice::<Dsh_receipt>(&data)
                    .map_err(|_| failure("invalid_receipt"))?,
            );
        }
    }
    receipts.sort_by_key(|item| std::cmp::Reverse(item.started_at_ms));
    receipts.truncate(30);
    Ok(receipts)
}

/// A crashed stage cannot affect the live profile. A crash during promotion
/// requires review of retained backups; never guess which user's edits to erase.
pub(super) fn ensure_recovered(root: &Path) -> Result<(), AppError> {
    let directory = root.join("operations");
    config::safe_path(&directory)?;
    if !directory.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(directory).map_err(|_| failure("receipts_unreadable"))? {
        let entry = entry.map_err(|_| failure("receipts_unreadable"))?;
        let journal = entry.path();
        if config::read_optional(&journal.join("commit_started.json"))?.is_none() {
            continue;
        }
        let completed = config::read_json(&journal.join("commit_finished.json"))?;
        if !completed
            .as_ref()
            .and_then(|value| value.get("status"))
            .and_then(Value::as_str)
            .is_some_and(|status| matches!(status, "committed" | "restored"))
        {
            return Err(failure("interrupted_transaction"));
        }
    }
    Ok(())
}
