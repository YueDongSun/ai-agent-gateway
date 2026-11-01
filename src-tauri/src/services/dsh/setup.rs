//! Review-bound onboarding. Planning is passive; applying changes only AAG's
//! managed settings/profile and never launches, downloads, or edits native apps.

use super::{
    config::{self, failure, Instance_lock, MANAGED_PROFILE, SETTINGS_FILE},
    discovery::{self, Discovery_environment, Dsh_discovery},
    plugins,
    types::{Dsh_settings, Dsh_snapshot},
    upstreams::Upstream_option,
    Dsh_service,
};
use crate::error::AppError;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    fs,
    path::{Path, PathBuf},
};

const PLAN_TTL_MS: u64 = 300_000;
const MAX_PENDING_PLANS: usize = 16;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dsh_setup_request {
    pub expected_digest: String,
    pub upstream_id: Option<String>,
    pub model_id: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Dsh_setup_plan {
    pub plan_id: String,
    pub expected_digest: String,
    pub expires_at_ms: u64,
    pub settings: Dsh_settings,
    pub changes: Vec<String>,
    pub warnings: Vec<String>,
    pub blockers: Vec<String>,
    pub source_reason: String,
    pub can_apply: bool,
}

pub(super) struct Pending_setup {
    pub(super) plan: Dsh_setup_plan,
    state_digest: String,
    upstream_digest: String,
    initialize_profile: bool,
}

impl Dsh_service {
    pub fn preview_setup(
        &self,
        request: Dsh_setup_request,
        upstreams: &[Upstream_option],
        preferred_upstream_id: Option<&str>,
        preferred_model_id: Option<&str>,
    ) -> Result<Dsh_setup_plan, AppError> {
        let discovery = self.discover_runtime()?;
        self.preview_setup_with_discovery(
            request,
            upstreams,
            preferred_upstream_id,
            preferred_model_id,
            discovery,
        )
    }

    fn preview_setup_with_discovery(
        &self,
        request: Dsh_setup_request,
        upstreams: &[Upstream_option],
        preferred_upstream_id: Option<&str>,
        preferred_model_id: Option<&str>,
        discovery: Dsh_discovery,
    ) -> Result<Dsh_setup_plan, AppError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| failure("state_lock_failed"))?;
        if state.job.is_some() || state.child.is_some() {
            return Err(failure("instance_busy"));
        }
        plugins::ensure_recovered(&self.root)?;
        let (settings, settings_digest) = config::read_settings(&self.root)?;
        if request.expected_digest != settings_digest {
            return Err(failure("settings_changed"));
        }
        let profile = self.root.join(MANAGED_PROFILE).join("package.json");
        let initialize_profile = config::read_optional(&profile)?.is_none();
        let mut plan = build_plan(
            settings,
            request,
            discovery,
            upstreams,
            preferred_upstream_id,
            preferred_model_id,
            initialize_profile,
        );
        // No filesystem lock is acquired here: even first-run planning must
        // leave the AAG managed directory absent. Applying rechecks the digest.
        let state_digest = config::state_digest(&self.root)?;
        plan.plan_id = config::operation_id();
        plan.expires_at_ms = config::now_ms() + PLAN_TTL_MS;
        state
            .setup_plans
            .retain(|_, pending| pending.plan.expires_at_ms >= config::now_ms());
        if state.setup_plans.len() >= MAX_PENDING_PLANS {
            state.setup_plans.clear();
        }
        state.setup_plans.insert(
            plan.plan_id.clone(),
            Pending_setup {
                upstream_digest: selected_upstream_digest(&plan.settings, upstreams),
                state_digest,
                initialize_profile,
                plan: plan.clone(),
            },
        );
        Ok(plan)
    }

    /// Apply only a consumed main-window preview. The in-process mutex and
    /// existing instance lock serialize setup with plugin/config/runtime work.
    pub fn apply_setup(
        &self,
        plan_id: String,
        upstreams: &[Upstream_option],
    ) -> Result<Dsh_snapshot, AppError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| failure("state_lock_failed"))?;
        if state.job.is_some() || state.child.is_some() {
            return Err(failure("instance_busy"));
        }
        let pending = state
            .setup_plans
            .remove(&plan_id)
            .ok_or_else(|| failure("setup_plan_not_found"))?;
        if config::now_ms() > pending.plan.expires_at_ms {
            return Err(failure("setup_plan_expired"));
        }
        if !pending.plan.can_apply {
            return Err(failure("setup_plan_blocked"));
        }
        if config::read_settings(&self.root)?.1 != pending.plan.expected_digest
            || config::state_digest(&self.root)? != pending.state_digest
        {
            return Err(failure("settings_changed"));
        }
        if selected_upstream_digest(&pending.plan.settings, upstreams) != pending.upstream_digest {
            return Err(failure("setup_upstream_changed"));
        }
        validate_selected_upstream(&pending.plan.settings, upstreams)?;
        config::validate_settings(&pending.plan.settings)?;
        // Re-read selected paths, not the environment: installation metadata
        // changed after review must not become a different executable choice.
        let discovery =
            discovery::discover(&pending.plan.settings, &Discovery_environment::default());
        if ["node", "dsh"].iter().any(|kind| {
            !discovery.candidates.iter().any(|candidate| {
                candidate.kind == *kind && candidate.source == "configured" && candidate.compatible
            })
        }) {
            return Err(failure("setup_runtime_changed"));
        }
        if !pending.plan.settings.pnpm_entry_path.is_empty()
            && !discovery
                .candidates
                .iter()
                .any(|candidate| candidate.kind == "pnpm" && candidate.compatible)
        {
            return Err(failure("setup_runtime_changed"));
        }
        let _instance_lock = Instance_lock::acquire(&self.root)?;
        plugins::ensure_recovered(&self.root)?;
        // A different AAG process may have written immediately before the
        // instance lock was acquired; never rely on the pre-lock digest alone.
        if config::state_digest(&self.root)? != pending.state_digest {
            return Err(failure("settings_changed"));
        }
        if !pending.plan.changes.is_empty() {
            commit_setup(&self.root, &pending, false)?;
        }
        state.previews.clear();
        state.setup_plans.clear();
        drop(state);
        self.snapshot()
    }
}

/// Runtime paths arrive from discovery as display text and can be stored
/// (hand-edited, or synced from another device) with different case; on
/// Windows the same executable must still validate.
fn same_runtime_path(candidate: &str, stored: &str) -> bool {
    #[cfg(windows)]
    {
        candidate.to_lowercase() == stored.to_lowercase()
    }
    #[cfg(not(windows))]
    {
        candidate == stored
    }
}

fn build_plan(
    mut settings: Dsh_settings,
    request: Dsh_setup_request,
    discovery: Dsh_discovery,
    upstreams: &[Upstream_option],
    preferred_upstream_id: Option<&str>,
    preferred_model_id: Option<&str>,
    initialize_profile: bool,
) -> Dsh_setup_plan {
    let mut changes = Vec::new();
    let mut warnings = vec![
        "managed_scope_only".into(),
        "explicit_start_required".into(),
    ];
    let mut blockers = Vec::new();
    let source_was_unset = settings.upstream_id.is_none();
    let model_was_unset = settings.model_id.is_none();
    for (kind, value, proposed, change) in [
        (
            "node",
            &mut settings.node_path,
            discovery.recommended.node_path,
            "fill_node_path",
        ),
        (
            "dsh",
            &mut settings.dsh_entry_path,
            discovery.recommended.dsh_entry_path,
            "fill_dsh_entry_path",
        ),
        (
            "pnpm",
            &mut settings.pnpm_entry_path,
            discovery.recommended.pnpm_entry_path,
            "fill_pnpm_entry_path",
        ),
    ] {
        if value.is_empty() && !proposed.is_empty() {
            *value = proposed;
            changes.push(change.into());
        }
        if value.is_empty() {
            if kind == "pnpm" {
                warnings.push("pnpm_not_found".into());
            } else {
                blockers.push(format!("runtime_{kind}_required"));
            }
        } else if !discovery.candidates.iter().any(|candidate| {
            candidate.kind == kind && same_runtime_path(&candidate.path, value) && candidate.compatible
        }) {
            blockers.push(format!("runtime_{kind}_invalid"));
        }
    }
    if !settings.node_path.is_empty() {
        warnings.push("node_version_unverified".into());
    }
    if discovery.scan_limited {
        warnings.push("discovery_scan_limited".into());
    }
    if discovery
        .issues
        .iter()
        .any(|issue| issue == "acceptance_runtime_explicit")
    {
        warnings.push("acceptance_runtime_explicit".into());
    }
    let mut source_reason = "preserved";
    if settings.upstream_id.is_some()
        && request.upstream_id.is_some()
        && settings.upstream_id != request.upstream_id
    {
        blockers.push("explicit_selection_conflict".into());
    }
    if settings.model_id.is_some()
        && request.model_id.is_some()
        && settings.model_id != request.model_id
    {
        blockers.push("explicit_selection_conflict".into());
    }
    if settings.upstream_id.is_none() {
        if let Some(requested) = request.upstream_id {
            settings.upstream_id = Some(requested);
            source_reason = "explicit";
        } else if model_was_unset && preferred_upstream_id.is_some() {
            settings.upstream_id = preferred_upstream_id.map(str::to_owned);
            source_reason = "current_claude";
        } else {
            let available: Vec<_> = upstreams.iter().filter(|source| source.available).collect();
            if available.len() == 1 && model_was_unset && preferred_upstream_id.is_none() {
                settings.upstream_id = Some(available[0].id.clone());
                source_reason = "unique_available";
            } else {
                source_reason = "selection_required";
                blockers.push(
                    if available.is_empty() {
                        "upstream_required"
                    } else {
                        "upstream_selection_required"
                    }
                    .into(),
                );
            }
        }
    }
    if source_was_unset && settings.upstream_id.is_some() {
        changes.push("select_upstream".into());
    }
    if let Some(source_id) = settings.upstream_id.as_ref() {
        if let Some(source) = upstreams
            .iter()
            .find(|source| source.id == *source_id && source.available)
        {
            if settings.model_id.is_none() {
                if let Some(model) = request.model_id {
                    settings.model_id = Some(model);
                } else if source_reason == "current_claude"
                    && preferred_model_id.is_some_and(|model| {
                        source.models.iter().any(|candidate| candidate == model)
                    })
                {
                    settings.model_id = preferred_model_id.map(str::to_owned);
                } else if source.models.len() == 1 {
                    settings.model_id = Some(source.models[0].clone());
                } else {
                    blockers.push(
                        if source.models.is_empty() {
                            "model_required"
                        } else {
                            "model_selection_required"
                        }
                        .into(),
                    );
                }
            }
            if settings
                .model_id
                .as_ref()
                .is_some_and(|model| !source.models.contains(model))
            {
                blockers.push("model_unavailable".into());
            }
        } else {
            blockers.push("upstream_unavailable".into());
        }
    }
    if model_was_unset && settings.model_id.is_some() {
        changes.push("select_model".into());
    }
    if initialize_profile {
        changes.push("initialize_profile".into());
    }
    blockers.sort();
    blockers.dedup();
    Dsh_setup_plan {
        plan_id: String::new(),
        expected_digest: request.expected_digest,
        expires_at_ms: 0,
        settings,
        changes,
        warnings,
        can_apply: blockers.is_empty(),
        blockers,
        source_reason: source_reason.into(),
    }
}

fn selected_upstream_digest(settings: &Dsh_settings, upstreams: &[Upstream_option]) -> String {
    let source = upstreams
        .iter()
        .find(|source| Some(&source.id) == settings.upstream_id.as_ref());
    let snapshot = source.map(|source| {
        let mut models = source.models.clone();
        models.sort();
        json!({"id":source.id,"protocol":source.protocol,"models":models,"source_kind":source.source_kind,"available":source.available,"reason":source.reason})
    });
    config::digest(snapshot.unwrap_or_default().to_string().as_bytes())
}

fn validate_selected_upstream(
    settings: &Dsh_settings,
    upstreams: &[Upstream_option],
) -> Result<(), AppError> {
    let source = upstreams
        .iter()
        .find(|source| Some(&source.id) == settings.upstream_id.as_ref() && source.available)
        .ok_or_else(|| failure("setup_upstream_changed"))?;
    if !settings
        .model_id
        .as_ref()
        .is_some_and(|model| source.models.contains(model))
    {
        return Err(failure("setup_upstream_changed"));
    }
    Ok(())
}

struct Setup_target {
    path: PathBuf,
    stage: PathBuf,
    backup: PathBuf,
    expected: Option<Vec<u8>>,
    data: Vec<u8>,
    backed_up: bool,
    promoted: bool,
}

/// Two-file promotion reuses the plugin journal marker contract. Failures retain
/// reviewable backups; interruption blocks further managed mutations instead of
/// guessing whether later user edits should be overwritten. `inject_failure`
/// is only supplied by unit tests and cannot be set over IPC or environment.
fn commit_setup(
    root: &Path,
    pending: &Pending_setup,
    inject_failure: bool,
) -> Result<(), AppError> {
    let journal = root.join("operations").join(&pending.plan.plan_id);
    config::safe_path(&journal)?;
    let stage = journal.join("stage");
    fs::create_dir_all(&stage).map_err(|_| failure("stage_create_failed"))?;
    let settings_path = root.join(SETTINGS_FILE);
    let mut targets = vec![Setup_target {
        expected: config::read_optional(&settings_path)?,
        path: settings_path,
        stage: stage.join("settings.json"),
        backup: journal.join("backup_settings.json"),
        data: serde_json::to_vec_pretty(&pending.plan.settings)?,
        backed_up: false,
        promoted: false,
    }];
    if pending.initialize_profile {
        let profile_path = root.join(MANAGED_PROFILE).join("package.json");
        if config::read_optional(&profile_path)?.is_some() {
            return Err(failure("settings_changed"));
        }
        targets.push(Setup_target {
            path: profile_path,
            stage: stage.join("profile.json"),
            backup: journal.join("backup_profile.json"),
            expected: None,
            data: serde_json::to_vec_pretty(&config::profile_manifest())?,
            backed_up: false,
            promoted: false,
        });
    }
    for target in &targets {
        config::safe_path(&target.path)?;
        fs::create_dir_all(
            target
                .path
                .parent()
                .ok_or_else(|| failure("invalid_managed_path"))?,
        )
        .map_err(|_| failure("profile_create_failed"))?;
        config::write_new(&target.stage, &target.data)?;
    }
    if config::state_digest(root)? != pending.state_digest {
        return Err(failure("settings_changed"));
    }
    config::write_new(
        &journal.join("commit_started.json"),
        &serde_json::to_vec(
            &json!({"action":"setup","before_digest":pending.state_digest,"targets":[SETTINGS_FILE,MANAGED_PROFILE]}),
        )?,
    )?;
    let result = (|| {
        for (index, target) in targets.iter_mut().enumerate() {
            if inject_failure && index == 1 {
                return Err(failure("setup_injected_failure"));
            }
            if config::read_optional(&target.path)? != target.expected {
                return Err(failure("settings_changed"));
            }
            if target.expected.is_some() {
                fs::rename(&target.path, &target.backup).map_err(|_| failure("backup_failed"))?;
                target.backed_up = true;
            }
            fs::rename(&target.stage, &target.path).map_err(|_| failure("setup_commit_failed"))?;
            target.promoted = true;
        }
        for target in &targets {
            if config::read_optional(&target.path)?.as_ref() != Some(&target.data) {
                return Err(failure("setup_readback_mismatch"));
            }
        }
        config::write_new(
            &journal.join("commit_finished.json"),
            br#"{"status":"committed"}"#,
        )?;
        Ok(())
    })();
    if let Err(error) = result {
        let mut restored = true;
        for target in targets.iter_mut().rev() {
            if target.promoted
                && (config::read_optional(&target.path).ok().flatten().as_ref()
                    != Some(&target.data)
                    || fs::rename(&target.path, target.stage.with_extension("failed")).is_err())
            {
                restored = false;
                continue;
            }
            if target.backed_up && fs::rename(&target.backup, &target.path).is_err() {
                restored = false;
            }
        }
        if restored {
            config::write_new(
                &journal.join("commit_finished.json"),
                br#"{"status":"restored"}"#,
            )
            .map_err(|_| failure("setup_rollback_failed"))?;
            return Err(error);
        }
        return Err(failure("setup_rollback_failed"));
    }
    Ok(())
}

#[cfg(test)]
#[path = "setup_tests.rs"]
mod tests;
