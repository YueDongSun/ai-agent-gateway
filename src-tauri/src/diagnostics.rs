//! Product diagnostics: bounded log retention, temporary debug mode, and
//! crash/lifecycle evidence without request or credential payloads.

use crate::database::Database;
use hmac::{Hmac, Mac};
use serde::Serialize;
use sha2::Sha256;
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
pub const KEY_DIAGNOSTICS_DEBUG_UNTIL: &str = "diagnostics_debug_until";

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

pub fn log_builder() -> tauri_plugin_log::Builder {
    tauri_plugin_log::Builder::new()
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
        .max_file_size(LOG_MAX_FILE_SIZE_BYTES as u128)
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
    let directory = app
        .path()
        .app_log_dir()
        .map_err(|_| "diagnostics_log_directory_unavailable".to_string())?;
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
        log::error!(
            "native: panic id={} location_ref={} location_line={} message_chars={}",
            correlation_id,
            location_ref,
            location_line,
            message_chars
        );
        write_bootstrap_record(
            "native_panic",
            &format!(
                "id={correlation_id} location_ref={location_ref} location_line={location_line} message_chars={message_chars}"
            ),
        );
        // Also write the actual (truncated) message to a separate panic log so
        // teardown-class panics can be identified without leaking absolute paths.
        write_panic_record(&correlation_id, &location_ref, location_line, message);
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

fn panic_log_path() -> Option<PathBuf> {
    dirs::data_local_dir().map(|root| root.join(APP_IDENTIFIER).join("logs").join("panic.log"))
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
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{} stage={} {}", unix_timestamp(), stage, message);
    }
}

fn bootstrap_log_path() -> Option<PathBuf> {
    dirs::data_local_dir().map(|root| root.join(APP_IDENTIFIER).join("logs").join("bootstrap.log"))
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
}
