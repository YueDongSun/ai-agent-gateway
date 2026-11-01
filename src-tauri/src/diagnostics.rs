//! Product diagnostics: bounded log retention, temporary debug mode, and
//! crash/lifecycle evidence without request or credential payloads.

use crate::database::Database;
use hmac::{Hmac, Mac};
use serde::Serialize;
use sha2::Sha256;
use std::collections::VecDeque;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Manager, Runtime};

pub const DEBUG_DURATION_SECS: i64 = 30 * 60;
pub const LOG_MAX_FILE_SIZE_BYTES: u64 = 2 * 1024 * 1024;
pub const LOG_RETAINED_FILE_COUNT: usize = 5;
/// Cap for the append-only panic/bootstrap record files. The main log
/// rotates via the plugin; these files truncate in place at half their size
/// when they exceed the cap (see `truncate_record_file_if_large`).
const PANIC_LOG_MAX_BYTES: u64 = 2 * 1024 * 1024;
pub const KEY_DIAGNOSTICS_DEBUG_UNTIL: &str = "diagnostics_debug_until";

/// Fixed capacity for the request timeline ring buffer.
pub const REQUEST_TIMELINE_CAPACITY: usize = 200;

const MAX_FRONTEND_KIND_CHARS: usize = 64;
const MAX_FRONTEND_MESSAGE_CHARS: usize = 512;
const APP_IDENTIFIER: &str = "org.aagateway.desktop";

static REQUEST_COUNTER: AtomicU64 = AtomicU64::new(1);
static REFERENCE_KEY: OnceLock<Option<[u8; 32]>> = OnceLock::new();

#[derive(Debug)]
#[allow(non_camel_case_types)]
pub struct Diagnostics_runtime {
    debug_expires_at: AtomicI64,
    generation: AtomicU64,
}

#[derive(Debug, Clone, Serialize)]
#[allow(non_camel_case_types)]
pub struct Diagnostics_state {
    pub debug_enabled: bool,
    pub debug_expires_at: Option<i64>,
    pub log_directory: String,
    pub max_file_size_bytes: u64,
    pub retained_file_count: usize,
}

impl Diagnostics_runtime {
    pub fn new(debug_expires_at: i64) -> Self {
        Self {
            debug_expires_at: AtomicI64::new(debug_expires_at),
            generation: AtomicU64::new(0),
        }
    }

    pub fn apply_current_level(&self) {
        let level = if self.is_debug_enabled() {
            log::LevelFilter::Debug
        } else {
            log::LevelFilter::Info
        };
        log::set_max_level(level);
    }

    pub fn is_debug_enabled(&self) -> bool {
        self.debug_expires_at.load(Ordering::Acquire) > unix_timestamp()
    }

    pub fn debug_expires_at(&self) -> Option<i64> {
        let value = self.debug_expires_at.load(Ordering::Acquire);
        (value > unix_timestamp()).then_some(value)
    }

    pub fn enable_until(&self, deadline: i64) -> u64 {
        self.debug_expires_at.store(deadline, Ordering::Release);
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        self.apply_current_level();
        generation
    }

    pub fn disable(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.debug_expires_at.store(0, Ordering::Release);
        self.apply_current_level();
    }

    fn disable_if_generation(&self, generation: u64) -> bool {
        if self.generation.load(Ordering::Acquire) != generation {
            return false;
        }
        self.debug_expires_at.store(0, Ordering::Release);
        self.apply_current_level();
        true
    }
}

fn configured_managed_directory(child: &str, error_code: &str) -> Result<Option<PathBuf>, String> {
    let Some(value) = std::env::var_os("AAG_CONFIG_DIR") else {
        return Ok(None);
    };
    let value = value.into_string().map_err(|_| error_code.to_string())?;
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    let config_directory = PathBuf::from(value);
    crate::config::private_fs::prepare_managed_private_dir(&config_directory)
        .map_err(|_| error_code.to_string())?;
    let directory = config_directory.join(child);
    crate::config::private_fs::prepare_managed_private_dir(&directory)
        .map_err(|_| error_code.to_string())?;
    Ok(Some(directory))
}

fn configured_log_directory() -> Result<Option<PathBuf>, String> {
    configured_managed_directory("logs", "diagnostics_log_directory_unavailable")
}

pub fn configured_webview_data_directory() -> Result<Option<PathBuf>, String> {
    configured_managed_directory("webview", "webview_data_directory_unavailable")
}

pub fn log_builder() -> Result<tauri_plugin_log::Builder, String> {
    let builder = tauri_plugin_log::Builder::new()
        .level(log::LevelFilter::Warn)
        .level_for("ai_agent_gateway", log::LevelFilter::Debug)
        .level_for("ai_agent_gateway_lib", log::LevelFilter::Debug)
        .level_for("hyper", log::LevelFilter::Warn)
        .level_for("hyper_util", log::LevelFilter::Warn)
        .level_for("reqwest", log::LevelFilter::Warn)
        .level_for("rustls", log::LevelFilter::Warn)
        .level_for("tower", log::LevelFilter::Warn)
        .level_for("wry", log::LevelFilter::Warn)
        .rotation_strategy(tauri_plugin_log::RotationStrategy::KeepSome(
            LOG_RETAINED_FILE_COUNT,
        ))
        .timezone_strategy(tauri_plugin_log::TimezoneStrategy::UseLocal)
        .max_file_size(LOG_MAX_FILE_SIZE_BYTES as u128);
    Ok(match configured_log_directory()? {
        Some(directory) => builder.targets([
            tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::Stdout),
            tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::Folder {
                path: directory,
                file_name: None,
            }),
        ]),
        None => builder,
    })
}

pub fn restore_debug_deadline(db: &Database) -> i64 {
    let deadline = db
        .get_setting(KEY_DIAGNOSTICS_DEBUG_UNTIL)
        .ok()
        .flatten()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(0);
    if deadline > unix_timestamp() {
        deadline
    } else {
        let _ = db.set_setting(KEY_DIAGNOSTICS_DEBUG_UNTIL, "");
        0
    }
}

pub fn state<R: Runtime>(_app: &AppHandle<R>, runtime: &Diagnostics_runtime) -> Diagnostics_state {
    // The actual filesystem path remains native-only. The UI can open the
    // managed directory through a dedicated command without receiving an
    // absolute user-profile path over IPC.
    let log_directory = "managed_local_directory".to_string();
    Diagnostics_state {
        debug_enabled: runtime.is_debug_enabled(),
        debug_expires_at: runtime.debug_expires_at(),
        log_directory,
        max_file_size_bytes: LOG_MAX_FILE_SIZE_BYTES,
        retained_file_count: LOG_RETAINED_FILE_COUNT,
    }
}

pub fn enable_debug<R: Runtime>(
    app: &AppHandle<R>,
    db: &Database,
    runtime: &Diagnostics_runtime,
) -> Result<Diagnostics_state, String> {
    let deadline = unix_timestamp() + DEBUG_DURATION_SECS;
    db.set_setting(KEY_DIAGNOSTICS_DEBUG_UNTIL, &deadline.to_string())
        .map_err(|_| "diagnostics_state_write_failed".to_string())?;
    let generation = runtime.enable_until(deadline);
    log::info!("diagnostics: debug enabled duration_secs={DEBUG_DURATION_SECS}");
    schedule_expiry(app.clone(), generation, deadline);
    Ok(state(app, runtime))
}

pub fn disable_debug<R: Runtime>(
    app: &AppHandle<R>,
    db: &Database,
    runtime: &Diagnostics_runtime,
) -> Result<Diagnostics_state, String> {
    db.set_setting(KEY_DIAGNOSTICS_DEBUG_UNTIL, "")
        .map_err(|_| "diagnostics_state_write_failed".to_string())?;
    runtime.disable();
    log::info!("diagnostics: debug disabled");
    Ok(state(app, runtime))
}

pub fn schedule_restored_expiry<R: Runtime>(app: AppHandle<R>) {
    let runtime = app.state::<Diagnostics_runtime>();
    runtime.apply_current_level();
    if let Some(deadline) = runtime.debug_expires_at() {
        let generation = runtime.generation.fetch_add(1, Ordering::AcqRel) + 1;
        schedule_expiry(app.clone(), generation, deadline);
        log::info!("diagnostics: restored temporary debug deadline={deadline}");
    }
}

fn schedule_expiry<R: Runtime>(app: AppHandle<R>, generation: u64, deadline: i64) {
    let delay_secs = deadline.saturating_sub(unix_timestamp()) as u64;
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(delay_secs)).await;
        let runtime = app.state::<Diagnostics_runtime>();
        if runtime.disable_if_generation(generation) {
            let db = app.state::<Database>();
            if db.set_setting(KEY_DIAGNOSTICS_DEBUG_UNTIL, "").is_err() {
                log::warn!(
                    "diagnostics: expired setting clear failed code=diagnostics_state_write_failed"
                );
            }
            log::info!("diagnostics: debug expired");
        }
    });
}

pub fn open_log_directory<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    let directory = match configured_log_directory()? {
        Some(directory) => directory,
        None => app
            .path()
            .app_log_dir()
            .map_err(|_| "diagnostics_log_directory_unavailable".to_string())?,
    };
    fs::create_dir_all(&directory)
        .map_err(|_| "diagnostics_log_directory_unavailable".to_string())?;
    open_directory(&directory)
}

#[cfg(target_os = "windows")]
fn open_directory(path: &Path) -> Result<(), String> {
    std::process::Command::new("explorer.exe")
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|_| "diagnostics_log_directory_open_failed".to_string())
}

#[cfg(target_os = "macos")]
fn open_directory(path: &Path) -> Result<(), String> {
    std::process::Command::new("open")
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|_| "diagnostics_log_directory_open_failed".to_string())
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn open_directory(path: &Path) -> Result<(), String> {
    std::process::Command::new("xdg-open")
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|_| "diagnostics_log_directory_open_failed".to_string())
}

pub fn summary<R: Runtime>(app: &AppHandle<R>, runtime: &Diagnostics_runtime) -> String {
    let diagnostics_state = state(app, runtime);
    format!(
        "AI Agent Gateway {}\nplatform={}\narchitecture={}\ndebug_enabled={}\ndebug_expires_at={}\nlog_retention={}x{} bytes",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        diagnostics_state.debug_enabled,
        diagnostics_state
            .debug_expires_at
            .map(|value| value.to_string())
            .unwrap_or_else(|| "none".into()),
        diagnostics_state.retained_file_count,
        diagnostics_state.max_file_size_bytes,
    )
}

pub fn record_frontend_error(
    kind: &str,
    message_chars: usize,
    stack_chars: usize,
    correlation_id: &str,
) {
    let safe_kind = safe_frontend_kind(kind);
    let safe_correlation_id = safe_code(correlation_id, "ui_error");
    log::error!(
        "frontend: error id={} kind={} message_chars={} stack_chars={}",
        safe_correlation_id,
        safe_kind,
        message_chars.min(MAX_FRONTEND_MESSAGE_CHARS),
        stack_chars.min(MAX_FRONTEND_MESSAGE_CHARS)
    );
}

pub fn next_request_id() -> String {
    let counter = REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("req_{:x}_{counter:x}", unix_timestamp())
}

pub fn install_panic_hook() {
    // Discard the default hook: it prints the raw panic payload and absolute
    // source path to stderr after our content-free record.
    let _previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        // Ordering is the only defense against a panic inside the hook:
        // release builds use `panic = "abort"` and Rust's panicking machinery
        // aborts outright on a panic-in-hook without re-entering this hook, so
        // neither catch_unwind nor a re-entry guard can protect the tail.
        // Write the file records FIRST so they survive even if the log call
        // below is what panics (the log plugin's formatter unwraps a
        // time-format call per record).
        let message = panic_info
            .payload()
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| {
                panic_info
                    .payload()
                    .downcast_ref::<String>()
                    .map(|value| value.as_str())
            })
            .unwrap_or("<non string payload>");
        let message_chars = message.chars().count();
        let (location_ref, location_line) = panic_info
            .location()
            .map(|value| (safe_reference(value.file()), value.line()))
            .unwrap_or_else(|| ("ref_unknown".into(), 0));
        let correlation_id = next_request_id();
        write_bootstrap_record(
            "native_panic",
            &format!(
                "id={correlation_id} location_ref={location_ref} location_line={location_line} message_chars={message_chars}"
            ),
        );
        // Also write the actual (truncated) message to a separate panic log so
        // teardown-class panics can be identified without leaking absolute paths.
        write_panic_record(&correlation_id, &location_ref, location_line, message);
        // Best-effort: a panic here aborts the process, but the file records
        // above are already on disk.
        log::error!(
            "native: panic id={} location_ref={} location_line={} message_chars={}",
            correlation_id,
            location_ref,
            location_line,
            message_chars
        );
    }));
}

fn write_panic_record(correlation_id: &str, location_ref: &str, location_line: u32, message: &str) {
    let Some(path) = panic_log_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        if fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    // Panic records must never grow without bound: a recurring per-request
    // panic class would otherwise append forever while the rotating main log
    // stays capped. Truncate-in-place keeps a bounded tail (most recent
    // entries win because the file is rewritten from the kept head).
    truncate_record_file_if_large(&path, PANIC_LOG_MAX_BYTES);
    let safe_message = sanitize_text(message, MAX_FRONTEND_MESSAGE_CHARS);
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(
            file,
            "{} id={} location_ref={} location_line={} message=\"{}\"",
            unix_timestamp(),
            correlation_id,
            location_ref,
            location_line,
            safe_message.replace('\\', "\\\\").replace('"', "\\\""),
        );
    }
}

/// Cap a diagnostic record file at `max_bytes`, keeping the most recent half
/// (cut on a line boundary when one exists in the window).
///
/// Concurrency and durability:
/// - A process-wide mutex serializes concurrent panic hooks, so two panics
///   cannot interleave read-modify-write and tear each other's records.
/// - The rewrite goes through a sibling temp file + rename (atomic on the
///   same volume), so a crash mid-truncation leaves either the old or the
///   new file — never an empty one.
/// - Only the tail window is read (bounded allocation) instead of the whole
///   file, so a runaway record file costs O(keep) memory on the panic path.
///
/// Never fails: an untruncateable file is simply appended to as before.
fn truncate_record_file_if_large(path: &Path, max_bytes: u64) {
    static RECORD_FILE_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let Ok(_guard) = RECORD_FILE_GUARD.lock().map_err(|error| error.into_inner()) else {
        return;
    };
    let Ok(metadata) = fs::metadata(path) else {
        return;
    };
    if metadata.len() <= max_bytes {
        return;
    }
    let keep_amount = (max_bytes as usize / 2).max(1);
    // Read only the tail: seek to len - keep_amount - slack and take the rest,
    // so a multi-hundred-MB runaway log never allocates in full on the
    // panic thread. The slack lets the scan find a line boundary early.
    let keep_from = (metadata.len() as usize).saturating_sub(keep_amount);
    let contents = {
        use std::io::{Read, Seek, SeekFrom};
        let Ok(mut file) = fs::File::open(path) else {
            return;
        };
        if file.seek(SeekFrom::Start(keep_from as u64)).is_err() {
            return;
        }
        let mut tail = Vec::with_capacity(keep_amount + 64);
        if file.read_to_end(&mut tail).is_err() {
            return;
        }
        tail
    };
    let start = match contents.iter().position(|byte| *byte == b'\n') {
        // Boundary found strictly before the tail's end: start the next line.
        // When the window has no inner boundary (one huge line), keep the
        // mid-line tail as-is — a truncated first record is acceptable for a
        // diagnostic tail.
        Some(offset) if offset + 1 < contents.len() => offset + 1,
        _ => 0,
    };
    // Atomic replace: write the kept tail to a sibling temp file, then
    // rename over the original. fs::rename replaces an existing target on
    // the same volume on all supported platforms.
    let tmp = path.with_extension("log.trunc-tmp");
    if fs::write(&tmp, &contents[start..]).is_err() {
        return;
    }
    if fs::rename(&tmp, path).is_err() {
        let _ = fs::remove_file(&tmp);
    }
}

fn panic_log_path() -> Option<PathBuf> {
    match configured_log_directory().ok()? {
        Some(directory) => Some(directory.join("panic.log")),
        None => dirs::data_local_dir()
            .map(|root| root.join(APP_IDENTIFIER).join("logs").join("panic.log")),
    }
}

pub fn record_fatal_startup(stage: &str, message: &str) {
    let safe_stage = safe_code(stage, "startup_failure");
    let correlation_id = next_request_id();
    write_bootstrap_record(
        &safe_stage,
        &format!(
            "id={correlation_id} message_chars={}",
            message.chars().count().min(MAX_FRONTEND_MESSAGE_CHARS)
        ),
    );
}

fn write_bootstrap_record(stage: &str, message: &str) {
    let Some(path) = bootstrap_log_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        if fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    truncate_record_file_if_large(&path, PANIC_LOG_MAX_BYTES);
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{} stage={} {}", unix_timestamp(), stage, message);
    }
}

fn bootstrap_log_path() -> Option<PathBuf> {
    match configured_log_directory().ok()? {
        Some(directory) => Some(directory.join("bootstrap.log")),
        None => dirs::data_local_dir()
            .map(|root| root.join(APP_IDENTIFIER).join("logs").join("bootstrap.log")),
    }
}

pub(crate) fn safe_reference(value: &str) -> String {
    let key = REFERENCE_KEY.get_or_init(|| {
        let mut key = [0_u8; 32];
        getrandom::getrandom(&mut key).ok().map(|_| key)
    });
    let Some(key) = key else {
        return "ref_unavailable".to_string();
    };
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(key) else {
        return "ref_unavailable".to_string();
    };
    mac.update(value.as_bytes());
    let digest = mac.finalize().into_bytes();
    format!("ref_{}", hex::encode(&digest[..6]))
}

fn safe_frontend_kind(kind: &str) -> String {
    match kind {
        "window_error" | "unhandled_rejection" | "react_error_boundary" => kind.to_string(),
        _ => "frontend_error".to_string(),
    }
}

fn safe_code(value: &str, fallback: &str) -> String {
    if !value.is_empty()
        && value.len() <= MAX_FRONTEND_KIND_CHARS
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        value.to_string()
    } else {
        fallback.to_string()
    }
}

#[allow(dead_code)] // defense-in-depth helper exercised by adversarial tests
fn sanitize_text(value: &str, max_chars: usize) -> String {
    value
        .replace('\u{ff1a}', ":")
        .replace('\u{ff1d}', "=")
        .split_whitespace()
        .map(|part| {
            if token_is_private(part) {
                "[redacted]".to_string()
            } else {
                part.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max_chars)
        .collect()
}

fn token_is_private(part: &str) -> bool {
    let trimmed = part.trim_matches(|character: char| {
        matches!(
            character,
            '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | ',' | ';' | '|' | '"' | '\''
        )
    });
    let lower = trimmed.to_ascii_lowercase();
    if lower.is_empty() {
        return false;
    }
    if lower.contains("://")
        || lower.starts_with("www.")
        || lower.contains('@')
        || lower.contains('\\')
        || lower.starts_with('/')
        || lower.starts_with("~/")
        || (lower.len() >= 3
            && lower.as_bytes()[0].is_ascii_alphabetic()
            && lower.as_bytes()[1] == b':'
            && matches!(lower.as_bytes()[2], b'/' | b'\\'))
        || lower.ends_with(".ts.net")
        || lower.starts_with("sk-")
        || lower.starts_with("ghp_")
        || lower.starts_with("github_pat_")
    {
        return true;
    }
    if [
        "api_key",
        "apikey",
        "auth_token",
        "authorization",
        "bearer",
        "password",
        "passwd",
        "secret",
        "token=",
        "token:",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
    {
        return true;
    }
    let address = lower
        .trim_matches(|character| matches!(character, '[' | ']' | '(' | ')' | ',' | ';'))
        .split_once('%')
        .map(|(head, _)| head)
        .unwrap_or(&lower);
    if address.parse::<std::net::IpAddr>().is_ok() {
        return true;
    }
    let without_port = if address.matches(':').count() == 1 {
        address
            .rsplit_once(':')
            .filter(|(_, port)| port.chars().all(|character| character.is_ascii_digit()))
            .map(|(host, _)| host)
            .unwrap_or(address)
    } else {
        address
    };
    if without_port.parse::<std::net::IpAddr>().is_ok() {
        return true;
    }
    looks_like_domain(without_port)
}

fn looks_like_domain(value: &str) -> bool {
    let candidate = value.trim_end_matches('.');
    let labels: Vec<_> = candidate.split('.').collect();
    if labels.len() < 2 {
        return false;
    }
    let Some(tld) = labels.last() else {
        return false;
    };
    tld.len() >= 2
        && tld.chars().all(|character| character.is_ascii_alphabetic())
        && labels.iter().all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '-')
        })
}

/// One metadata-only request record. Must never hold a prompt, response
/// body, tool payload, credential, upstream URL, domain, or IP.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
#[allow(non_camel_case_types)]
pub struct Request_event {
    /// Opaque correlation id already in use.
    pub request_id: String,
    /// One of the HTTP surfaces, e.g. chat / messages / responses / models.
    pub surface: String,
    /// `safe_reference()` of the requested model.
    pub model_ref: String,
    pub timestamp_ms: u64,
    pub status: u16,
    pub duration_ms: u64,
    pub streamed: bool,
    pub failed_over: Option<bool>,
    /// Capability-routing decision summary (`precise` / `alias`), present
    /// only when the planner resolved the request. Codes only.
    pub plan_kind: Option<String>,
    pub plan_tier: Option<u8>,
    pub plan_downgraded: Option<bool>,
    pub plan_basis: Option<String>,
    pub plan_reason: Option<String>,
    /// Routing snapshot generation the request was admitted under, so a
    /// finished request is never explained by a later routing table.
    pub config_generation: Option<u64>,
}

/// Content-free routing-decision summary attached to a request event.
/// Values are stable snake_case codes; the timeline boundary sanitizes
/// them regardless of the caller.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[allow(non_camel_case_types)]
pub struct Plan_trace {
    pub kind: Option<String>,
    pub tier: Option<u8>,
    pub downgraded: Option<bool>,
    pub basis: Option<String>,
    pub reason: Option<String>,
}

/// Reduce a plan label to a fixed set; unknown labels become `other`.
fn safe_plan_kind(value: &str) -> Option<String> {
    match value {
        "" => None,
        "exact" | "precise" | "alias" => Some(value.to_string()),
        _ => Some("other".to_string()),
    }
}

/// Keep only snake_case code shapes; anything else becomes `invalid` so a
/// future caller cannot smuggle free text into the timeline.
fn safe_plan_code(value: &str) -> Option<String> {
    if value.is_empty() {
        return None;
    }
    let code_shape = value.len() <= 48
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_');
    Some(if code_shape {
        value.to_string()
    } else {
        "invalid".to_string()
    })
}

/// Thread-safe ring buffer for `Request_event` with fixed capacity.
/// The buffer itself is immutable; only its contents are mutated under a lock.
#[allow(non_camel_case_types)]
struct Request_timeline {
    records: std::sync::Mutex<VecDeque<Request_event>>,
}

impl Request_timeline {
    fn push(&self, mut event: Request_event) {
        let mut records = self
            .records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if records.len() >= REQUEST_TIMELINE_CAPACITY {
            records.pop_front();
        }
        event.timestamp_ms = unix_timestamp_ms();
        records.push_back(event);
    }

    fn snapshot(&self) -> Vec<Request_event> {
        self.records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .cloned()
            .collect()
    }

    fn clear(&self) {
        self.records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }
}

static REQUEST_TIMELINE: OnceLock<Request_timeline> = OnceLock::new();

fn get_request_timeline() -> &'static Request_timeline {
    REQUEST_TIMELINE.get_or_init(|| Request_timeline {
        records: std::sync::Mutex::new(VecDeque::with_capacity(REQUEST_TIMELINE_CAPACITY)),
    })
}

fn safe_request_id(value: &str) -> String {
    let Some(parts) = value.strip_prefix("req_") else {
        return safe_reference(value);
    };
    let mut segments = parts.split('_');
    let timestamp = segments.next().unwrap_or_default();
    let counter = segments.next().unwrap_or_default();
    let is_lower_hex = |segment: &str| {
        !segment.is_empty()
            && segment
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    };
    if value.len() <= 64
        && is_lower_hex(timestamp)
        && is_lower_hex(counter)
        && segments.next().is_none()
    {
        value.to_string()
    } else {
        safe_reference(value)
    }
}

fn safe_surface(value: &str) -> String {
    match value {
        "chat" | "messages" | "responses" | "models" | "gemini" => value.to_string(),
        _ => "other".to_string(),
    }
}

/// Record one metadata-only request event using a raw requested model.
///
/// The requested model is transformed with `safe_reference`; the request id
/// is retained only when it matches the opaque format emitted by
/// `next_request_id`, and the surface is reduced to a fixed set of known
/// HTTP surface labels.
pub fn record_request_event(
    request_id: &str,
    surface: &str,
    model: &str,
    status: u16,
    duration_ms: u64,
    streamed: bool,
    failed_over: Option<bool>,
) {
    let model_ref = safe_reference(model);
    record_request_event_with_model_ref(
        request_id,
        surface,
        &model_ref,
        status,
        duration_ms,
        streamed,
        failed_over,
        &Plan_trace::default(),
        None,
    );
}

/// Record an event when the caller has already converted the model identifier
/// with `safe_reference`. Invalid references are transformed again so this
/// boundary never stores arbitrary caller-provided model text. The plan
/// summary and config generation are sanitized here too — this boundary
/// stores codes only, never free text.
pub(crate) fn record_request_event_with_model_ref(
    request_id: &str,
    surface: &str,
    model_ref: &str,
    status: u16,
    duration_ms: u64,
    streamed: bool,
    failed_over: Option<bool>,
    plan: &Plan_trace,
    config_generation: Option<u64>,
) {
    let safe_model_ref = if is_safe_reference(model_ref) {
        model_ref.to_string()
    } else {
        safe_reference(model_ref)
    };
    get_request_timeline().push(Request_event {
        request_id: safe_request_id(request_id),
        surface: safe_surface(surface),
        model_ref: safe_model_ref,
        timestamp_ms: 0,
        status,
        duration_ms,
        streamed,
        failed_over,
        plan_kind: plan.kind.as_deref().and_then(safe_plan_kind),
        plan_tier: plan.tier,
        plan_downgraded: plan.downgraded,
        plan_basis: plan.basis.as_deref().and_then(safe_plan_code),
        plan_reason: plan.reason.as_deref().and_then(safe_plan_code),
        config_generation,
    });
}

fn is_safe_reference(value: &str) -> bool {
    if value == "ref_unavailable" {
        return true;
    }
    let Some(digest) = value.strip_prefix("ref_") else {
        return false;
    };
    digest.len() == 12
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Return a snapshot of all retained request events, oldest first.
pub fn request_timeline() -> Vec<Request_event> {
    get_request_timeline().snapshot()
}

/// Clear all retained request events.
pub fn clear_request_timeline() {
    get_request_timeline().clear();
}

fn unix_timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

fn unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::Digest;

    #[test]
    fn configured_diagnostics_stay_inside_the_disposable_config_directory() {
        let fixture = crate::services::provider::test_support::isolated_home("diagnostics-paths");
        let directory = fixture.config_dir().join("logs");
        let webview_directory = fixture.config_dir().join("webview");
        assert_eq!(configured_log_directory().unwrap(), Some(directory.clone()));
        assert_eq!(
            configured_webview_data_directory().unwrap(),
            Some(webview_directory.clone())
        );
        assert!(webview_directory.is_dir());
        assert!(log_builder().is_ok());
        assert_eq!(panic_log_path(), Some(directory.join("panic.log")));
        assert_eq!(bootstrap_log_path(), Some(directory.join("bootstrap.log")));
        write_bootstrap_record("fixture", "metadata_only");
        assert!(fs::read_to_string(directory.join("bootstrap.log"))
            .unwrap()
            .contains("stage=fixture metadata_only"));
    }

    #[test]
    fn invalid_diagnostics_override_never_falls_back_to_live_logs() {
        let _fixture = crate::services::provider::test_support::isolated_home("diagnostics-invalid");
        for value in [".", "relative_fixture", std::path::MAIN_SEPARATOR_STR] {
            std::env::set_var("AAG_CONFIG_DIR", value);
            assert!(configured_log_directory().is_err());
            assert!(configured_webview_data_directory().is_err());
            assert!(log_builder().is_err());
            assert_eq!(panic_log_path(), None);
            assert_eq!(bootstrap_log_path(), None);
        }
    }

    #[test]
    fn truncate_record_file_keeps_recent_half_on_a_line_boundary() {
        let directory = std::env::temp_dir().join(format!(
            "aag-truncate-test-{}-{}",
            std::process::id(),
            unix_timestamp()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("records.log");
        // Two lines: an old line and a recent line, padded so the file
        // exceeds the (tiny) cap passed below.
        let old_line = format!("{}\n", "o".repeat(600));
        let recent_line = format!("{}\n", "r".repeat(600));
        std::fs::write(&path, format!("{old_line}{recent_line}")).unwrap();

        truncate_record_file_if_large(&path, 512);

        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(!contents.contains('o'), "old half must be dropped: {contents:?}");
        assert!(contents.contains('r'), "recent half must survive");
        assert!(contents.starts_with('r') || contents.is_empty(), "must start on a line boundary");
        std::fs::remove_dir_all(&directory).ok();
    }

    #[test]
    fn truncate_record_file_ignores_small_and_missing_files() {
        let missing = std::env::temp_dir().join("aag-truncate-missing-no-such-file.log");
        truncate_record_file_if_large(&missing, 128); // must not panic or create
        assert!(!missing.exists());
    }

    #[test]
    fn sanitizer_removes_urls_and_credential_markers() {
        let secret_like = format!("{}-{}", "sk", "test-value");
        let sanitized = sanitize_text(
            &format!("failed https://private.example/path Authorization=Bearer {secret_like}"),
            512,
        );
        assert_eq!(sanitized, "failed [redacted] [redacted] [redacted]");
    }

    #[test]
    fn private_identifier_reference_is_keyed_and_content_free() {
        let private = "private-provider-identity";
        let first = safe_reference(private);
        assert_eq!(first, safe_reference(private));
        assert!(first == "ref_unavailable" || first.starts_with("ref_"));
        assert!(!first.contains(private));
        let public_digest = hex::encode(&Sha256::digest(private.as_bytes())[..6]);
        assert_ne!(first, format!("ref_{public_digest}"));
    }

    #[test]
    fn sanitizer_removes_embedded_private_addresses_paths_and_unicode_separators() {
        let private_values = [
            "https://user:pass@private.example.invalid/path?token=1#secret",
            "private.person@example.invalid",
            r"C:\Users\private-user\secret.txt",
            "/home/private-user/secret.txt",
            r"\\private-host\share\secret.txt",
            "100.64.12.34",
            "[fd7a:115c:a1e0::42]",
            "private-node.tail123.ts.net",
            "private.example.invalid",
            "Bearer\u{2028}sk-private-value",
            "authorization\u{ff1a}secret-value",
        ];
        let sanitized = sanitize_text(&format!("stable_code {}", private_values.join(" | ")), 2048);
        assert!(sanitized.contains("stable_code"));
        for private in private_values {
            assert!(
                !sanitized.contains(private),
                "private value survived sanitizer: {private} in {sanitized}"
            );
        }
    }

    #[test]
    fn stale_generation_cannot_disable_new_debug_session() {
        let runtime = Diagnostics_runtime::new(0);
        let first = runtime.enable_until(unix_timestamp() + 60);
        let second = runtime.enable_until(unix_timestamp() + 120);
        assert!(!runtime.disable_if_generation(first));
        assert!(runtime.is_debug_enabled());
        assert!(runtime.disable_if_generation(second));
        assert!(!runtime.is_debug_enabled());
    }

    #[test]
    fn debug_deadline_restores_only_future_values() {
        let db = Database::memory().unwrap();
        let future = unix_timestamp() + 60;
        db.set_setting(KEY_DIAGNOSTICS_DEBUG_UNTIL, &future.to_string())
            .unwrap();
        assert_eq!(restore_debug_deadline(&db), future);
        db.set_setting(
            KEY_DIAGNOSTICS_DEBUG_UNTIL,
            &(unix_timestamp() - 1).to_string(),
        )
        .unwrap();
        assert_eq!(restore_debug_deadline(&db), 0);
        assert_eq!(db.get_setting(KEY_DIAGNOSTICS_DEBUG_UNTIL).unwrap(), None);
    }

    #[test]
    fn request_timeline_respects_capacity() {
        clear_request_timeline();
        for i in 0..(REQUEST_TIMELINE_CAPACITY + 50) {
            record_request_event(
                &format!("req_{i:x}_1"),
                "chat",
                "model",
                200,
                100,
                false,
                Some(false),
            );
        }
        let events = request_timeline();
        assert_eq!(
            events.len(),
            REQUEST_TIMELINE_CAPACITY,
            "timeline must never exceed capacity"
        );
        assert_eq!(
            events[0].request_id, "req_32_1",
            "oldest 50 records must have been dropped"
        );
        clear_request_timeline();
        assert!(request_timeline().is_empty());
    }

    #[test]
    fn request_event_never_contains_private_identifiers() {
        clear_request_timeline();
        // Feed adversarial inputs that would contain URLs, IPs, domains,
        // or key-shaped tokens if not redacted.
        let adversarial_id = "https://user:pass@evil.example.invalid/path?token=sk-attack#frag";
        let adversarial_model = "model@192.0.2.9/private-repo";

        record_request_event(
            adversarial_id,
            "http://localhost:8080/chat",
            adversarial_model,
            401,
            0,
            true,
            Some(true),
        );

        let events = request_timeline();
        assert_eq!(events.len(), 1);
        let json = serde_json::to_string(&events[0]).unwrap();

        // Assert no URL, IP, domain, or credential token survives serialization.
        assert!(
            !json.contains("evil.example"),
            "domain must not appear in timeline JSON"
        );
        assert!(
            !json.contains("https://") && !json.contains("http://"),
            "URL schemes must not appear in timeline JSON"
        );
        assert!(
            !json.contains("192.0.2.9"),
            "IP address must not appear in timeline JSON"
        );
        assert!(
            !json.contains("localhost"),
            "localhost must not appear in timeline JSON"
        );
        assert!(
            !json.contains("sk-attack"),
            "secret token must not appear in timeline JSON"
        );
        assert!(
            !json.contains("user:pass"),
            "credentials must not appear in timeline JSON"
        );
        assert!(
            !json.contains("token="),
            "token= must not appear in timeline JSON"
        );
        assert!(
            events[0].request_id.starts_with("ref_"),
            "adversarial request id must be replaced by a safe reference"
        );
        assert_eq!(events[0].surface, "other");
        assert!(
            events[0].model_ref.starts_with("ref_"),
            "model_ref must be a safe reference"
        );
        clear_request_timeline();
    }

    #[test]
    fn prepared_model_reference_is_preserved_without_rehashing() {
        clear_request_timeline();
        let model_ref = safe_reference("private-model-name");
        record_request_event_with_model_ref(
            "req_abc123_1",
            "chat",
            &model_ref,
            200,
            5,
            false,
            None,
            &Plan_trace::default(),
            None,
        );
        let events = request_timeline();
        assert_eq!(events[0].model_ref, model_ref);
        assert_eq!(events[0].failed_over, None);
        clear_request_timeline();
    }

    #[test]
    fn request_timeline_fields_are_approved_only() {
        clear_request_timeline();
        record_request_event(
            "req_abc123_1",
            "chat",
            "gpt-4o",
            200,
            42,
            true,
            Some(false),
        );
        let events = request_timeline();
        let json = serde_json::to_string(&events[0]).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        let keys: Vec<_> = parsed
            .as_object()
            .map(|o| o.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        let allowed = [
            "requestId",
            "surface",
            "modelRef",
            "timestampMs",
            "status",
            "durationMs",
            "streamed",
            "failedOver",
            "planKind",
            "planTier",
            "planDowngraded",
            "planBasis",
            "planReason",
            "configGeneration",
        ];
        assert_eq!(
            keys.len(),
            allowed.len(),
            "timeline record must have exactly 14 fields"
        );
        for key in allowed {
            assert!(
                keys.contains(&key.to_string()),
                "approved field '{key}' missing from timeline record"
            );
        }
        clear_request_timeline();
    }

    #[test]
    fn plan_trace_is_sanitized_to_codes_only() {
        clear_request_timeline();
        record_request_event_with_model_ref(
            "req_abc123_1",
            "chat",
            "ref_000000000000",
            200,
            5,
            false,
            None,
            &Plan_trace {
                kind: Some("sneaky text".into()),
                tier: Some(3),
                downgraded: Some(true),
                basis: Some("Manual Override".into()),
                reason: Some("https://evil.example.invalid/x".into()),
            },
            Some(7),
        );
        let events = request_timeline();
        assert_eq!(events[0].plan_kind.as_deref(), Some("other"));
        assert_eq!(events[0].plan_tier, Some(3));
        assert_eq!(events[0].plan_downgraded, Some(true));
        assert_eq!(events[0].plan_basis.as_deref(), Some("invalid"));
        assert_eq!(events[0].plan_reason.as_deref(), Some("invalid"));
        assert_eq!(events[0].config_generation, Some(7));
        clear_request_timeline();
    }
}
