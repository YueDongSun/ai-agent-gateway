//! Source-authoritative instruction synchronization for explicitly enabled mirrors.
//! One application-owned worker and launch callers share the same mutation gate.

mod codex_preferences;
mod manifest;
mod plugin_instructions;
mod scan;
#[cfg(test)]
mod tests;

use crate::{config, database::Database};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Condvar, Mutex,
};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub(crate) const KEY_ENABLED: &str = "mirror_instruction_sync_enabled";
const RECONCILE_INTERVAL: Duration = Duration::from_secs(5);
const STOP_TIMEOUT: Duration = Duration::from_secs(2);

/// Instruction synchronization is on unless explicitly disabled: an unset
/// key (fresh installs) or a stored `"1"` both enable it, and only a stored
/// `"0"` opts out. Never writes the setting — a stored user choice stays
/// untouched.
pub(crate) fn instruction_sync_enabled(db: &Database) -> Result<bool, crate::error::AppError> {
    Ok(db
        .get_setting(KEY_ENABLED)?
        .as_deref()
        .is_none_or(|value| value != "0"))
}

#[derive(Clone, Copy)]
pub(crate) enum Client {
    Codex,
    Claude,
}

impl Client {
    fn name(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }
    fn files(self) -> &'static [&'static str] {
        match self {
            Self::Codex => &["AGENTS.md", "AGENTS.override.md"],
            Self::Claude => &["CLAUDE.md", "CLAUDE.local.md"],
        }
    }
    fn directories(self) -> &'static [&'static str] {
        match self {
            Self::Codex => &["agent_policy", "rules", "skills", "agents", "prompts"],
            Self::Claude => &["skills", "agents", "commands", "rules", "prompts"],
        }
    }
    /// The primary instruction file first-run seeding copies.
    pub(crate) fn seed_instruction_file(self) -> &'static str {
        match self {
            Self::Codex => "AGENTS.md",
            Self::Claude => "CLAUDE.md",
        }
    }
    /// Instruction roots first-run seeding populates. This is the single
    /// shared root vocabulary: continuous sync (`directories`/`files` above)
    /// is a superset, and seeding must never copy a root sync would not.
    pub(crate) fn seed_roots(self) -> &'static [&'static str] {
        match self {
            Self::Codex => &["skills", "agents"],
            Self::Claude => &["skills", "agents", "commands"],
        }
    }
    /// Stable app-type token for mirror audit writes.
    pub(crate) fn instructions_app_type(self) -> &'static str {
        match self {
            Self::Codex => "codex_instructions",
            Self::Claude => "claude_instructions",
        }
    }
    fn allows(self, relative: &Path) -> bool {
        let parts: Vec<_> = relative.components().collect();
        if parts.is_empty()
            || parts
                .iter()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
        {
            return false;
        }
        let first = parts[0].as_os_str().to_string_lossy();
        if parts.len() == 1 {
            return self.files().iter().any(|name| scan::name_eq(&first, name));
        }
        self.directories()
            .iter()
            .any(|name| scan::name_eq(&first, name))
            && !(scan::name_eq(&first, "skills")
                && scan::name_eq(&parts[1].as_os_str().to_string_lossy(), ".system"))
    }
}

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
#[allow(non_camel_case_types)]
pub(crate) struct Client_report {
    pub status: String,
    pub copied: usize,
    pub deleted: usize,
    pub unchanged: usize,
    pub unmanaged: usize,
    pub error: Option<String>,
    pub config_updated: bool,
}

impl Client_report {
    fn failure(code: &str) -> Self {
        Self {
            status: if code == "mirror_sync_source_unavailable" {
                "source_unavailable"
            } else {
                "blocked"
            }
            .into(),
            error: Some(code.into()),
            ..Self::default()
        }
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
#[allow(non_camel_case_types)]
pub(crate) struct Sync_report {
    pub codex: Client_report,
    pub claude: Client_report,
}

impl Sync_report {
    pub fn for_client(&self, client: &str) -> Result<(), String> {
        let report = match client {
            "codex" => &self.codex,
            "claude" | "claude_code" | "claude_desktop" => &self.claude,
            _ => return Err("mirror_sync_client_invalid".into()),
        };
        if report.status == "synced" {
            Ok(())
        } else {
            Err(report
                .error
                .clone()
                .unwrap_or_else(|| "mirror_sync_unavailable".into()))
        }
    }
    fn failure(code: &str) -> Self {
        Self {
            codex: Client_report::failure(code),
            claude: Client_report::failure(code),
        }
    }
}

struct Sync_inner {
    gate: Mutex<()>,
    stopped: AtomicBool,
    wake: (Mutex<()>, Condvar),
    worker: Mutex<Option<JoinHandle<()>>>,
}

#[derive(Clone)]
#[allow(non_camel_case_types)]
pub(crate) struct Mirror_sync_service {
    inner: Arc<Sync_inner>,
}

impl Mirror_sync_service {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Sync_inner {
                gate: Mutex::new(()),
                stopped: AtomicBool::new(false),
                wake: (Mutex::new(()), Condvar::new()),
                worker: Mutex::new(None),
            }),
        }
    }

    pub fn start(&self, db: Database) -> Result<(), String> {
        if !instruction_sync_enabled(&db).map_err(|_| "mirror_sync_configuration_unavailable")? {
            return Err("mirror_sync_disabled".into());
        }
        let mut worker = self
            .inner
            .worker
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if worker.as_ref().is_some_and(|worker| !worker.is_finished()) {
            return Ok(());
        }
        if let Some(previous) = worker.take() {
            let _ = previous.join();
        }
        self.inner.stopped.store(false, Ordering::SeqCst);
        let service = self.clone();
        *worker = Some(std::thread::Builder::new().name("aag_instruction_sync".into()).spawn(move || {
            let mut previous_status = String::new();
            while !service.inner.stopped.load(Ordering::SeqCst) {
                let report = service.reconcile(&db);
                let status = format!("{}:{}:{:?}:{:?}", report.codex.status, report.claude.status, report.codex.error, report.claude.error);
                if status != previous_status || report.codex.copied + report.codex.deleted + report.claude.copied + report.claude.deleted > 0 {
                    log::info!("mirror instructions: codex={} claude={} copied={} deleted={} unmanaged={}", report.codex.status, report.claude.status,
                        report.codex.copied + report.claude.copied, report.codex.deleted + report.claude.deleted, report.codex.unmanaged + report.claude.unmanaged);
                    previous_status = status;
                }
                let lock = service.inner.wake.0.lock().unwrap_or_else(|error| error.into_inner());
                if service.inner.stopped.load(Ordering::SeqCst) { break; }
                drop(service.inner.wake.1.wait_timeout(lock, RECONCILE_INTERVAL));
            }
        }).map_err(|_| "mirror_sync_worker_start_failed")?);
        Ok(())
    }

    pub fn stop(&self) -> Result<(), String> {
        let mut worker = self
            .inner
            .worker
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        {
            // Use the same condition lock as the waiter so shutdown cannot
            // lose its notification between the stopped check and the wait.
            let _wake = self
                .inner
                .wake
                .0
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            self.inner.stopped.store(true, Ordering::SeqCst);
            self.inner.wake.1.notify_all();
        }
        let deadline = Instant::now() + STOP_TIMEOUT;
        while worker.as_ref().is_some_and(|worker| !worker.is_finished())
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        if worker.as_ref().is_some_and(|worker| !worker.is_finished()) {
            return Err("mirror_sync_stop_pending".into());
        }
        if let Some(worker) = worker.take() {
            worker.join().map_err(|_| "mirror_sync_worker_failed")?;
        }
        Ok(())
    }

    pub fn reconcile(&self, db: &Database) -> Sync_report {
        let _gate = self
            .inner
            .gate
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if self.inner.stopped.load(Ordering::SeqCst) {
            return Sync_report::failure("mirror_sync_stopped");
        }
        match instruction_sync_enabled(db) {
            Ok(true) => {}
            Ok(false) => return Sync_report::failure("mirror_sync_disabled"),
            Err(_) => return Sync_report::failure("mirror_sync_configuration_unavailable"),
        }
        let (Ok(codex), Ok(claude)) = (
            db.get_setting(crate::database::dao::settings::KEY_CODEX_CLI_HOME),
            db.get_setting(crate::database::dao::settings::KEY_CLAUDE_CLI_HOME),
        ) else {
            return Sync_report::failure("mirror_sync_configuration_unavailable");
        };
        // Skills project from the AAG-owned SSOT root: reconcile the shared
        // roots into it first, and fall back to the raw shared roots only if
        // that reconcile fails.
        let home = config::get_home_dir();
        let trusted = match crate::services::skills_ssot::reconcile(db) {
            Ok(root) => vec![root],
            Err(error) => {
                log::warn!("mirror instructions: skills ssot reconcile failed error={error}");
                vec![home.join(".cc-switch/skills"), home.join(".agents/skills")]
            }
        };
        let backups = config::get_app_config_dir().join("backups/mirror_instructions");
        let codex_native = config::get_codex_config_dir();
        let claude_native = config::get_claude_config_dir();
        let codex_mirror = config::get_codex_mirror_dir(codex.as_deref());
        let claude_mirror = config::get_claude_mirror_dir(claude.as_deref());
        let codex_block = match super::session_authority::client_config::managed_launch_required(db)
        {
            Ok(true) => Some("session_client_authority_unavailable".to_string()),
            Ok(false) => None,
            Err(code) => Some(code),
        };
        if (codex_block.is_none() && overlaps(&codex_mirror, &claude_mirror))
            || overlaps(&backups, &codex_native)
            || overlaps(&backups, &claude_native)
            || trusted.iter().any(|root| overlaps(&backups, root))
        {
            return Sync_report::failure("mirror_sync_directory_invalid");
        }
        Sync_report {
            codex: if let Some(code) = codex_block {
                Client_report::failure(&code)
            } else if overlaps(&codex_mirror, &claude_native) {
                Client_report::failure("mirror_sync_directory_invalid")
            } else {
                reconcile_client(
                    Client::Codex,
                    &codex_native,
                    &codex_mirror,
                    &trusted,
                    &backups,
                    &self.inner.stopped,
                )
            },
            claude: if overlaps(&claude_mirror, &codex_native) {
                Client_report::failure("mirror_sync_directory_invalid")
            } else {
                reconcile_client(
                    Client::Claude,
                    &claude_native,
                    &claude_mirror,
                    &trusted,
                    &backups,
                    &self.inner.stopped,
                )
            },
        }
    }
}

fn overlaps(left: &Path, right: &Path) -> bool {
    if config::same_dir(left, right)
        || scan::path_within(left, right)
        || scan::path_within(right, left)
    {
        return true;
    }
    // Resolve the nearest existing ancestor before appending missing names.
    // A new mirror below an existing short-path alias must be checked before
    // any directory is created or hardened. The private writer separately
    // refuses reparse points on the supplied destination path.
    let resolved = |path: &Path| {
        path.ancestors().find_map(|ancestor| {
            std::fs::canonicalize(ancestor)
                .ok()
                .map(|resolved| resolved.join(path.strip_prefix(ancestor).unwrap()))
        })
    };
    match (resolved(left), resolved(right)) {
        (Some(left), Some(right)) => {
            scan::path_within(&left, &right) || scan::path_within(&right, &left)
        }
        _ => false,
    }
}

fn reconcile_client(
    client: Client,
    native: &Path,
    mirror: &Path,
    trusted: &[PathBuf],
    backups: &Path,
    stopped: &AtomicBool,
) -> Client_report {
    let result = (|| {
        require_unmanaged_client(client)?;
        if !scan::plain_absolute_path(native)
            || !scan::plain_absolute_path(mirror)
            || (matches!(client, Client::Claude)
                && overlaps(mirror, &config::get_home_dir().join(".codex-cli")))
            || overlaps(native, mirror)
            || overlaps(mirror, backups)
            || overlaps(native, backups)
            || trusted
                .iter()
                .any(|root| overlaps(mirror, root) || overlaps(backups, root))
        {
            return Err("mirror_sync_directory_invalid".into());
        }
        let mut inventory = scan::source_inventory(client, native, trusted, stopped)?;
        let mut config_update = None;
        if matches!(client, Client::Codex) {
            let mut budget = std::mem::take(&mut inventory.budget);
            let inputs = codex_preferences::prepare(native, mirror, &mut budget)?;
            plugin_instructions::extend_inventory(
                native,
                mirror,
                trusted,
                &inputs,
                &mut inventory,
                &mut budget,
                stopped,
            )?;
            config_update = inputs.update;
            inventory.budget = budget;
        }
        let mut report = manifest::apply(client, native, mirror, backups, inventory, stopped)?;
        if let Some(update) = config_update {
            scan::check_cancelled(stopped)?;
            update.commit()?;
            report.config_updated = true;
        }
        Ok(report)
    })();
    result.unwrap_or_else(|code: String| Client_report::failure(&code))
}

/// Low-level sync helpers do not possess a database setting snapshot. An
/// authority object nevertheless excludes every legacy Codex mutation lane.
fn require_unmanaged_client(client: Client) -> Result<(), String> {
    if matches!(client, Client::Codex)
        && super::session_authority::client_config::authority_object_present()?
    {
        return Err("session_client_authority_unavailable".into());
    }
    Ok(())
}
