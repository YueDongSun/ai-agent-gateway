//! Codex adapter.
//!
//! Codex stores config at `~/.codex/config.toml` (TOML) and `~/.codex/auth.json`
//! (JSON). The provider's `settings_config` is a JSON object with shape:
//! `{ "auth": <auth.json content>, "config": "<TOML string>" }`.
//! Mirrors cc-switch `services/provider/live.rs:1012`.
//!
//! When the `codex_cli_home` setting is configured (independent CODEX_HOME),
//! writes go to that directory instead of the default `~/.codex`, isolating
//! the Codex CLI from the Codex desktop app's shared config.

use crate::config::{
    backup_file, get_backups_dir, get_codex_auth_path, get_codex_config_path, read_json_file,
    read_text_file, write_json_file, write_text_file,
};
use crate::error::AppError;
use crate::provider::Provider;
use crate::services::provider::LiveWriteResult;
use serde_json::Value;

/// Is Codex installed? (config dir exists)
#[allow(dead_code)]
pub fn detect() -> bool {
    get_codex_config_path().exists() || get_codex_auth_path().exists()
}

/// Read the current live config as `{config: <toml string>, auth: <auth.json value>}`,
/// or None if neither file exists.
#[allow(dead_code)]
pub fn read_live() -> Result<Option<Value>, AppError> {
    let config_path = get_codex_config_path();
    let auth_path = get_codex_auth_path();
    let config = read_text_file(&config_path)?;
    let auth = read_json_file(&auth_path)?;
    if config.is_none() && auth.is_none() {
        return Ok(None);
    }
    Ok(Some(serde_json::json!({
        "config": config.unwrap_or_default(),
        "auth": auth.unwrap_or(Value::Null),
    })))
}

/// Write `provider.settings_config` to the Codex mirror's `config.toml` +
/// `auth.json`, backing up existing files first. `settings_config` must be an
/// object with `auth` (any JSON) and `config` (TOML string) fields.
///
/// `codex_cli_home` optionally overrides the Codex config directory (isolated
/// CODEX_HOME); `None` uses the `~/.codex-cli` mirror default. The resolved
/// directory is never the native `~/.codex` — an override that points there
/// is refused (see the guard in `write_live`).
///
/// Retained for the direct cc-switch-style write path (tests / history /
/// `write_live_snapshot`). The live `provider_switch` flow no longer calls
/// this: a switch writes the mirror-proxy contract instead (see
/// `write_mirror_proxy_files`), so the mirror never carries login info.
pub fn write_live(
    provider: &Provider,
    codex_cli_home: Option<&str>,
) -> Result<LiveWriteResult, AppError> {
    // Resolve the mirror CODEX_HOME. `get_codex_mirror_dir` falls back to
    // `~/.codex-cli` when no override is set (never the native `~/.codex`),
    // but a user could still point the override directly at `~/.codex`.
    // Refuse that case so a codex live write can never mutate the Codex
    // Desktop app's shared config — and thereby clobber the user's ChatGPT
    // login state in the native `~/.codex/auth.json`.
    let mirror_dir = crate::config::get_codex_mirror_dir(codex_cli_home);
    if mirror_dir == crate::config::get_codex_config_dir() {
        return Err(AppError::Config(
            "codex_cli_home must not resolve to the native ~/.codex: refusing to write the Codex Desktop app's shared config".into(),
        ));
    }
    let obj = provider.settings_config.as_object().ok_or_else(|| {
        AppError::Config("Codex provider settings_config must be a JSON object".into())
    })?;
    let auth = obj
        .get("auth")
        .ok_or_else(|| AppError::Config("Codex settings_config missing 'auth' field".into()))?;
    let config_str = obj.get("config").and_then(|v| v.as_str()).ok_or_else(|| {
        AppError::Config("Codex settings_config missing 'config' string field".into())
    })?;

    let config_path = mirror_dir.join("config.toml");
    let auth_path = mirror_dir.join("auth.json");
    let ts = chrono::Utc::now().timestamp();
    let backup_dir = get_backups_dir()?;

    let mut written = Vec::new();
    let mut backups = Vec::new();

    // Backup + write config.toml.
    if config_path.exists() {
        let bak = backup_dir.join(format!("config.toml.{ts}.bak"));
        backup_file(&config_path, &bak)?;
        backups.push(bak.to_string_lossy().into_owned());
    }
    write_text_file(&config_path, config_str)?;
    written.push(config_path.to_string_lossy().into_owned());

    // Backup + write auth.json.
    // NOTE: this writes whatever credentials the provider row stores. Only
    // the retained direct-style write path uses it — the live
    // `provider_switch` flow writes the credential-free mirror-proxy
    // contract via `write_mirror_proxy_files` instead.
    if auth_path.exists() {
        let bak = backup_dir.join(format!("auth.json.{ts}.bak"));
        backup_file(&auth_path, &bak)?;
        backups.push(bak.to_string_lossy().into_owned());
    }
    write_json_file(&auth_path, auth)?;
    written.push(auth_path.to_string_lossy().into_owned());

    Ok(LiveWriteResult {
        app_type: "codex".into(),
        provider_id: provider.id.clone(),
        written,
        backup: backups.into_iter().next(),
    })
}

/// Session-state carriers in a CODEX_HOME that keep a Codex mirror stuck
/// on a dead login even after `auth.json` is emptied: the desktop's
/// global-state cache at the CODEX_HOME root (OUTSIDE the Chromium profile
/// the launcher wipes) and the `cache/` + `sessions/` trees written during
/// a logged-in session.
const MIRROR_SESSION_STATE_FILES: &[&str] =
    &[".codex-global-state.json", ".codex-global-state.json.bak"];
const MIRROR_SESSION_CACHE_DIRS: &[&str] = &["cache", "sessions"];

fn remove_mirror_path(mirror_dir: &std::path::Path, name: &str) -> Result<(), AppError> {
    let path = mirror_dir.join(name);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(_) => return Ok(()), // already absent
    };
    if metadata.file_type().is_symlink() {
        return Err(AppError::Config(format!(
            "refusing to remove {}: target is a symlink or reparse point",
            name
        )));
    }
    if path.is_dir() {
        std::fs::remove_dir_all(&path).map_err(|e| AppError::io(&path, e))?;
    } else {
        std::fs::remove_file(&path).map_err(|e| AppError::io(&path, e))?;
    }
    Ok(())
}

/// Reset every session-state carrier of a mirror CODEX_HOME: `auth.json`
/// becomes `{}` and the global-state / cache / sessions carriers from a
/// previous logged-in session are removed. Without this the desktop keeps
/// refreshing the dead cached session ("access token could not be
/// refreshed") instead of showing a clean login page. The fresh-mirror
/// path (open-desktop) calls this; the switch path never does — a login
/// session the user created inside the mirror must survive switches.
fn reset_mirror_session_state(mirror_dir: &std::path::Path) -> Result<(), AppError> {
    std::fs::write(mirror_dir.join("auth.json"), "{}")
        .map_err(|e| AppError::io(&mirror_dir.join("auth.json"), e))?;
    for name in MIRROR_SESSION_STATE_FILES {
        remove_mirror_path(mirror_dir, name)?;
    }
    for name in MIRROR_SESSION_CACHE_DIRS {
        remove_mirror_path(mirror_dir, name)?;
    }
    Ok(())
}

/// Classify the mirror's `auth.json` by SHAPE only — the contents are
/// never read by callers. `empty` = logged out ({} / null / missing);
/// `credentials_present` = something still stores a session there (older
/// AAG versions copied provider logins into this file).
pub(crate) fn classify_mirror_auth_json(mirror_dir: &std::path::Path) -> &'static str {
    match std::fs::read_to_string(mirror_dir.join("auth.json")) {
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(ref object)) if object.is_empty() => "empty",
            Ok(Value::Null) => "empty",
            Ok(_) => "credentials_present",
            Err(_) => "credentials_present",
        },
        Err(_) => "missing",
    }
}

/// Escape a value for a TOML basic string literal (the only quoting the
/// mirror config needs).
pub(crate) fn escape_toml_basic_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

/// How the mirror's `config.toml` exposes AAG to Codex:
///
/// - `Exclusive` (default, historic): the top-level `model_provider` is
///   forced to `aag` — every request routes through AAG's proxy, and a
///   ChatGPT login made inside the mirror sits unused.
/// - `Coexist`: no top-level provider/model is written, so Codex defaults
///   to its builtin `openai` provider — a ChatGPT login works for official
///   models out of the box — while `[model_providers.aag]` stays declared
///   and a `[profiles.aag]` profile points at it. The user manually
///   switches to AAG in the Codex model/provider picker when they want
///   third-party models.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(non_camel_case_types)]
pub enum Codex_mirror_mode {
    Exclusive,
    Coexist,
}

impl Codex_mirror_mode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Codex_mirror_mode::Exclusive => "exclusive",
            Codex_mirror_mode::Coexist => "coexist",
        }
    }

    /// Parse the persisted setting. Unknown/absent values fail safe to the
    /// historic exclusive behavior.
    pub fn from_setting(value: Option<&str>) -> Self {
        if value == Some("coexist") {
            Codex_mirror_mode::Coexist
        } else {
            Codex_mirror_mode::Exclusive
        }
    }
}

/// Read the persisted mirror mode (`KEY_CODEX_MIRROR_MODE`; absent =
/// exclusive).
pub(crate) fn codex_mirror_mode_from_db(db: &crate::database::Database) -> Codex_mirror_mode {
    Codex_mirror_mode::from_setting(
        db.get_setting(crate::database::dao::settings::KEY_CODEX_MIRROR_MODE)
            .ok()
            .flatten()
            .as_deref(),
    )
}

/// The mirror CODEX_HOME contract, in one place:
///
/// - `config.toml` points exclusively at AAG's proxy (`wire_api =
///   "responses"`, `requires_openai_auth = false`, AAG's own
///   `experimental_bearer_token`) — Codex authenticates to AAG only, and
///   AAG holds the upstream keys.
/// - `config.toml` points exclusively at AAG's proxy (`wire_api =
///   "responses"`, `requires_openai_auth = false`, AAG's own
///   `experimental_bearer_token`) — Codex authenticates to AAG only, and
///   AAG holds the upstream keys.
/// - `auth.json` is NEVER written by AAG with anything but `{}`. AAG never
///   puts login information into the mirror: not the native `~/.codex`
///   ChatGPT login, and not the provider row's stored credentials. With
///   `purge_auth = false` (the provider-switch path) auth.json is not
///   touched at all, so a login session the user created INSIDE the mirror
///   survives switches. With `purge_auth = true` (the open-desktop path,
///   which also wipes the mirror's Chromium profile) a stale auth.json —
///   including credential copies from older AAG versions — is purged
///   unconditionally, deliberately without a backup: the mirror must start
///   as a fresh, standalone, logged-out instance.
/// - `config.toml` is backed up before overwrite (it may hold a model setup
///   the user wants recoverable).
///
/// The mirror is an isolated AAG directory, hardened before any user
/// configuration is read or changed.
///
/// Called from both `provider_switch` (codex, `purge_auth = false`) and
/// `codex_open_desktop_window` (`purge_auth = true`), so every mirror write
/// path enforces the same proxy contract and the same never-write-credentials rule.
pub fn write_mirror_proxy_files(
    codex_cli_home: Option<&str>,
    proxy_base_url: &str,
    proxy_auth_token: &str,
    model: &str,
    mode: Codex_mirror_mode,
    purge_auth: bool,
) -> Result<LiveWriteResult, AppError> {
    let mirror_dir = crate::config::get_codex_mirror_dir(codex_cli_home);
    if crate::config::same_dir(&mirror_dir, &crate::config::get_codex_config_dir()) {
        return Err(AppError::Config(
            "codex_cli_home must not resolve to the native ~/.codex: refusing to write the Codex Desktop app's shared config".into(),
        ));
    }
    crate::config::private_fs::prepare_managed_private_dir(&mirror_dir)?;

    let config_path = mirror_dir.join("config.toml");
    let auth_path = mirror_dir.join("auth.json");
    for path in [&config_path, &auth_path] {
        crate::config::private_fs::validate_external_file_path(path)?;
    }

    // Exclusive: today's contract byte-for-byte — the top-level provider is
    // forced to `aag`, so every Codex request routes through AAG.
    // Coexist: no top-level provider/model (Codex defaults to its builtin
    // openai provider — the in-mirror ChatGPT login works for official
    // models), AAG stays declared, and a `[profiles.aag]` profile switches
    // to it manually. TOML ordering: table-free top-level keys must come
    // before any table header, hence two full templates.
    let mirror_config = match mode {
        Codex_mirror_mode::Exclusive => format!(
            "model = \"{model}\"\nmodel_provider = \"aag\"\n\n\
             [model_providers.aag]\n\
             name = \"AAG Gateway\"\n\
             base_url = \"{url}\"\n\
             wire_api = \"responses\"\n\
             requires_openai_auth = false\n\
             experimental_bearer_token = \"{auth}\"\n",
            model = escape_toml_basic_string(model),
            url = escape_toml_basic_string(proxy_base_url),
            auth = escape_toml_basic_string(proxy_auth_token),
        ),
        Codex_mirror_mode::Coexist => format!(
            "[model_providers.aag]\n\
             name = \"AAG Gateway\"\n\
             base_url = \"{url}\"\n\
             wire_api = \"responses\"\n\
             requires_openai_auth = false\n\
             experimental_bearer_token = \"{auth}\"\n\n\
             [profiles.aag]\n\
             model_provider = \"aag\"\n\
             model = \"{model}\"\n",
            url = escape_toml_basic_string(proxy_base_url),
            auth = escape_toml_basic_string(proxy_auth_token),
            model = escape_toml_basic_string(model),
        ),
    };

    let mut written = Vec::new();
    let mut backups = Vec::new();
    let (config_written, config_backups) =
        super::config_writes::write_files(&[super::config_writes::ConfigFileUpdate {
            path: &config_path,
            contents: mirror_config.as_bytes(),
            app_type: "codex",
        }])?;
    written.extend(config_written);
    backups.extend(config_backups);

    if purge_auth {
        // Fresh-mirror semantics (open-desktop): reset EVERY session-state
        // carrier so the mirror starts logged out with a clean login page.
        reset_mirror_session_state_guarded(codex_cli_home)?;
        written.push(auth_path.to_string_lossy().into_owned());
    }

    Ok(LiveWriteResult {
        app_type: "codex".into(),
        provider_id: String::new(),
        written,
        backup: backups.into_iter().next(),
    })
}

/// Keep launcher/native-home comparisons on the shared guarded path policy.
pub(crate) fn same_codex_dir(a: &std::path::Path, b: &std::path::Path) -> bool {
    crate::config::same_dir(a, b)
}

/// Default model for the mirror config. Priority:
/// 1. the active Codex provider's stored model (`settings_config.model`),
/// 2. the first `codex-*` route alias the router actually serves,
/// 3. `gpt-5` as a last resort.
fn mirror_model(db: &crate::database::Database) -> String {
    db.get_current_provider("codex")
        .ok()
        .flatten()
        .and_then(|id| db.get_provider_by_id(&id, "codex").ok().flatten())
        .and_then(|p| {
            p.settings_config
                .get("model")
                .and_then(|v| v.as_str())
                .map(String::from)
        })
        .or_else(|| {
            db.get_all_routes().ok().and_then(|routes| {
                routes
                    .into_iter()
                    .find(|r| r.alias.starts_with("codex-"))
                    .map(|r| r.alias)
            })
        })
        .unwrap_or_else(|| "gpt-5".into())
}

/// Explicitly reset the mirror's login/session state (auth.json + global
/// state + cache/sessions carriers). Invoked only on demand via the
/// `codex_mirror_session_reset` command — neither the switch path nor
/// open-desktop resets a login the user created inside the mirror.
pub(crate) fn reset_mirror_session_state_guarded(
    codex_cli_home: Option<&str>,
) -> Result<(), AppError> {
    let mirror_dir = crate::config::get_codex_mirror_dir(codex_cli_home);
    if crate::config::same_dir(&mirror_dir, &crate::config::get_codex_config_dir()) {
        return Err(AppError::Config(
            "codex_cli_home must not resolve to the native ~/.codex: refusing to reset the Codex Desktop app's shared session state".into(),
        ));
    }
    if !mirror_dir.exists() {
        return Ok(());
    }
    crate::config::private_fs::prepare_managed_private_dir(&mirror_dir)?;
    reset_mirror_session_state(&mirror_dir)
}

// -----------------------------------------------------------------------------
// Mirror auto-heal (server-side hook for the upstream-4xx auth-expired path)
// -----------------------------------------------------------------------------
//
// When the proxy surfaces an upstream `AuthExpired` 4xx, the Codex mirror is
// almost certainly stuck on a dead cached session in `~/.codex-cli/auth.json`.
// Resetting that state restores the clean login page — without that, every
// request hits the same dead-refresh-token path and Codex Desktop keeps
// surfacing its "access token could not be refreshed" dialog.
//
// The trigger lives at the response boundary in `proxy::server`; this module
// owns the filesystem side-effect. Two safety rails prevent the heal from
// interfering with a healthy mirror session:
//
// 1. **Shape guard**: only heal when `auth.json` is `credentials_present` —
//    the mirror is logged in but stale. `empty` / `missing` means the user
//    already saw the login page; no heal needed.
//
// 2. **M-time guard**: only heal when `auth.json` is older than
//    `MIRROR_HEAL_MIN_AUTH_AGE_SECS`. A fresh `auth.json` (the user just
//    signed in inside the mirror) must NOT be wiped — that would defeat
//    `purge_auth = false` semantics in `provider_switch`.
//
// 3. **Process-level OnceLock**: at most one heal per process. A second
//    desktop launch in the same process reuses the gate, but never wipes a
//    freshly re-created login.

use std::sync::OnceLock;

/// Mirror `auth.json` must be at least this old (seconds) before auto-heal
/// will touch it. Conservative default: 60 s — long enough that any in-flight
/// login flow inside the mirror window has time to complete its first write.
const MIRROR_HEAL_MIN_AUTH_AGE_SECS: u64 = 60;

/// Per-process gate. Holds the canonical mirror-dir path on which auto-heal
/// has already fired. `None` means heal has not run yet.
static MIRROR_HEAL_FIRED: OnceLock<String> = OnceLock::new();

/// Outcome of an auto-heal probe. Public so callers (and tests) can branch on
/// it without re-reading the filesystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MirrorHealOutcome {
    /// `auth.json` shape was `empty` / `missing`, or m-time was too fresh —
    /// the user is likely mid-login. No filesystem mutation.
    SkippedHealthyOrFresh,
    /// A previous heal already ran for this mirror dir this process.
    SkippedAlreadyFired,
    /// The native `~/.codex` was targeted — refused for safety. Same guard
    /// as the manual `codex_mirror_session_reset` command.
    RefusedNativeTarget,
    /// Heal ran: `auth.json` and the session carriers were reset.
    Healed,
}

/// Probe and (at most once per process) heal the Codex mirror's dead-session
/// state. Safe to call from any request handler — the per-process gate and
/// m-time guard make it a no-op unless the mirror is genuinely stuck on a
/// dead login AND no fresh login has been written recently.
///
/// Returns the outcome so the caller can log it; never panics, never writes
/// outside the mirror dir, never touches the native `~/.codex`.
pub(crate) fn maybe_heal_mirror_on_auth_expired(codex_cli_home: Option<&str>) -> MirrorHealOutcome {
    let mirror_dir = crate::config::get_codex_mirror_dir(codex_cli_home);
    if crate::config::same_dir(&mirror_dir, &crate::config::get_codex_config_dir()) {
        return MirrorHealOutcome::RefusedNativeTarget;
    }

    let key = mirror_dir.to_string_lossy().into_owned();
    if MIRROR_HEAL_FIRED.get().map(String::as_str) == Some(key.as_str()) {
        return MirrorHealOutcome::SkippedAlreadyFired;
    }

    if !mirror_dir.exists() {
        return MirrorHealOutcome::SkippedHealthyOrFresh;
    }
    match classify_mirror_auth_json(&mirror_dir) {
        "credentials_present" => {}
        _ => return MirrorHealOutcome::SkippedHealthyOrFresh,
    }

    // M-time guard: refuse to wipe a fresh auth.json. The user may be
    // signing in inside the mirror right now.
    let auth_path = mirror_dir.join("auth.json");
    let age_ok = match std::fs::metadata(&auth_path).and_then(|meta| meta.modified()) {
        Ok(modified) => match std::time::SystemTime::now().duration_since(modified) {
            Ok(age) => age.as_secs() >= MIRROR_HEAL_MIN_AUTH_AGE_SECS,
            Err(_) => false,
        },
        Err(_) => false,
    };
    if !age_ok {
        return MirrorHealOutcome::SkippedHealthyOrFresh;
    }

    // First call for this mirror dir in this process: fire the heal and
    // claim the gate. If another thread races past the gate first,
    // `set()` returns Err — treat that as `SkippedAlreadyFired`.
    let claim = MIRROR_HEAL_FIRED.set(key);
    if claim.is_err() {
        return MirrorHealOutcome::SkippedAlreadyFired;
    }

    if let Err(error) = reset_mirror_session_state(&mirror_dir) {
        log::warn!(
            "codex mirror auto-heal: reset failed mirror_dir={} error={}",
            crate::diagnostics::safe_reference(&mirror_dir.to_string_lossy()),
            error
        );
        // The gate is already set; surface the failure but don't roll it
        // back. A second hit this process is harmless either way.
    } else {
        log::info!(
            "codex mirror auto-heal: stale auth.json detected, reset to clean login state mirror_dir={}",
            crate::diagnostics::safe_reference(&mirror_dir.to_string_lossy())
        );
    }
    MirrorHealOutcome::Healed
}

#[cfg(test)]
mod auto_heal_tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    /// Build a temp mirror dir that is NOT the native `~/.codex`. The
    /// test process does not need to mutate the real mirror; each test
    /// case uses its own sub-directory so the per-process OnceLock
    /// gates do not collide.
    fn fresh_mirror_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aag-mirror-heal-{}-{}-{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        // AAG never writes to the native `~/.codex`; the guard inside
        // `same_codex_dir` checks the resolved path. The temp dir will
        // not match the native path, so the heal runs.
        dir
    }

    #[test]
    fn heal_runs_when_credentials_are_present_and_old() {
        let mirror = fresh_mirror_dir("runs");
        fs::write(
            mirror.join("auth.json"),
            r#"{"OPENAI_API_KEY":"stale","tokens":{"access_token":"dead","refresh_token":"dead"}}"#,
        )
        .unwrap();
        // Force m-time well past the minimum-age guard.
        let stale = std::time::SystemTime::now()
            - std::time::Duration::from_secs(MIRROR_HEAL_MIN_AUTH_AGE_SECS + 60);
        filetime_set(&mirror.join("auth.json"), stale);

        let outcome = maybe_heal_mirror_on_auth_expired(Some(mirror.to_str().unwrap()));
        assert_eq!(outcome, MirrorHealOutcome::Healed);
        let auth_after = fs::read_to_string(mirror.join("auth.json")).unwrap();
        assert_eq!(
            auth_after.trim(),
            "{}",
            "auth.json must be emptied after heal"
        );

        // Second call for the same dir this process: gate fires.
        let outcome2 = maybe_heal_mirror_on_auth_expired(Some(mirror.to_str().unwrap()));
        assert_eq!(outcome2, MirrorHealOutcome::SkippedAlreadyFired);

        let _ = fs::remove_dir_all(&mirror);
    }

    #[test]
    fn heal_skips_when_auth_is_already_empty() {
        let mirror = fresh_mirror_dir("empty");
        fs::write(mirror.join("auth.json"), "{}").unwrap();
        let outcome = maybe_heal_mirror_on_auth_expired(Some(mirror.to_str().unwrap()));
        assert_eq!(outcome, MirrorHealOutcome::SkippedHealthyOrFresh);
        let _ = fs::remove_dir_all(&mirror);
    }

    #[test]
    fn heal_skips_when_auth_is_too_fresh() {
        let mirror = fresh_mirror_dir("fresh");
        fs::write(
            mirror.join("auth.json"),
            r#"{"OPENAI_API_KEY":"just-signed-in"}"#,
        )
        .unwrap();
        // m-time is now() by default — well below the 60 s floor.
        let outcome = maybe_heal_mirror_on_auth_expired(Some(mirror.to_str().unwrap()));
        assert_eq!(outcome, MirrorHealOutcome::SkippedHealthyOrFresh);
        // The fresh auth.json must be preserved verbatim.
        let preserved = fs::read_to_string(mirror.join("auth.json")).unwrap();
        assert!(preserved.contains("just-signed-in"));
        let _ = fs::remove_dir_all(&mirror);
    }

    #[test]
    fn heal_refuses_native_codex_dir() {
        // Even if the native `~/.codex` had credentials, the heal must
        // refuse to touch it. We don't actually need to write to the
        // real native dir for this assertion — the dir-path guard runs
        // before any filesystem read.
        let native = crate::config::get_codex_config_dir();
        let outcome = maybe_heal_mirror_on_auth_expired(Some(native.to_str().unwrap()));
        assert_eq!(outcome, MirrorHealOutcome::RefusedNativeTarget);
    }

    /// Best-effort m-time setter that doesn't pull in the `filetime` crate.
    /// Uses `std::fs::File::set_modified` (stable since 1.75).
    fn filetime_set(path: &std::path::Path, time: std::time::SystemTime) {
        let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        if let Err(error) = file.set_modified(time) {
            // Some Windows filesystems disallow backward m-time; fall back
            // to the kernel-set time so the test still runs (it will then
            // fall into the `too fresh` branch, not the heal branch).
            eprintln!("note: could not set mtime ({error}); test may fall back");
        }
    }
}

/// Gather the mirror-proxy inputs from the database and write the
/// credential-free mirror contract. The single entry point for both
/// `provider_switch` (codex, `purge_auth = false`) and
/// `codex_open_desktop_window` (`purge_auth = true`).
///
/// `purge_auth = false`: only config.toml is written — a login session the
/// user created inside the mirror survives switches, and AAG still never
/// writes any credential.
pub fn switch_codex_mirror(
    db: &crate::database::Database,
    codex_cli_home: Option<&str>,
    purge_auth: bool,
) -> Result<LiveWriteResult, AppError> {
    let proxy_cfg = db.get_proxy_config().map_err(AppError::from)?;
    let gateway_url = format!("http://{}:{}/v1", proxy_cfg.host, proxy_cfg.port);
    let mut result = write_mirror_proxy_files(
        codex_cli_home,
        &gateway_url,
        &proxy_cfg.auth_token,
        &mirror_model(db),
        codex_mirror_mode_from_db(db),
        purge_auth,
    )?;
    result.provider_id = db
        .get_current_provider("codex")
        .ok()
        .flatten()
        .unwrap_or_default();
    Ok(result)
}

/// Prepare a selected provider before it becomes current in the database.
/// Reading the old current provider here would write its model during a switch.
pub fn switch_codex_mirror_for_provider(
    db: &crate::database::Database,
    codex_cli_home: Option<&str>,
    purge_auth: bool,
    provider: &Provider,
) -> Result<LiveWriteResult, AppError> {
    let proxy_cfg = db.get_proxy_config()?;
    let gateway_url = format!("http://{}:{}/v1", proxy_cfg.host, proxy_cfg.port);
    let model = match provider
        .settings_config
        .get("model")
        .and_then(serde_json::Value::as_str)
        .filter(|model| !model.trim().is_empty())
    {
        Some(model) => model.to_string(),
        None => db
            .get_all_routes()?
            .into_iter()
            .find(|route| route.alias.starts_with("codex-"))
            .map(|route| route.alias)
            .unwrap_or_else(|| "gpt-5".into()),
    };
    let mode = db.get_setting(crate::database::dao::settings::KEY_CODEX_MIRROR_MODE)?;
    let mut result = write_mirror_proxy_files(
        codex_cli_home,
        &gateway_url,
        &proxy_cfg.auth_token,
        &model,
        Codex_mirror_mode::from_setting(mode.as_deref()),
        purge_auth,
    )?;
    result.provider_id = provider.id.clone();
    Ok(result)
}

#[cfg(test)]
mod mirror_tests {
    use super::*;

    /// The mirror contract is credential-free: config.toml carries only
    /// AAG's proxy endpoint and token, never any provider credential. With
    /// `purge_auth = true` (open-desktop fresh-mirror semantics) a stale
    /// token-bearing auth.json is purged to `{}`; with `purge_auth = false`
    /// (switch) auth.json is left untouched so the user's own mirror login
    /// survives a switch.
    #[test]
    fn mirror_proxy_files_are_credential_free_and_purge_is_explicit() {
        // IsolatedHome sets AAG_HOME_DIR/AAG_CONFIG_DIR so the backup path
        // lands under the test-owned root, never the real ~/.aag.
        let home = crate::services::provider::test_support::isolated_home("codex-mirror");
        let mirror = home.root.join("codex-mirror");
        let _ = std::fs::remove_dir_all(&mirror);
        std::fs::create_dir_all(&mirror).unwrap();
        let stale_auth = r#"{"tokens":{"id_token":"stale-login","access_token":"stale"},"OPENAI_API_KEY":"sk-stale"}"#;
        std::fs::write(mirror.join("auth.json"), stale_auth).unwrap();
        std::fs::write(
            mirror.join(".codex-global-state.json"),
            r#"{"cachedAccount":"stale"}"#,
        )
        .unwrap();
        std::fs::write(mirror.join(".codex-global-state.json.bak"), "{}").unwrap();
        std::fs::create_dir_all(mirror.join("cache").join("codex_apps_tools")).unwrap();
        std::fs::write(
            mirror
                .join("cache")
                .join("codex_apps_tools")
                .join("app.json"),
            r#"{"refresh_token":"stale"}"#,
        )
        .unwrap();
        std::fs::create_dir_all(mirror.join("sessions").join("2026")).unwrap();
        std::fs::write(
            mirror.join("sessions").join("2026").join("rollout.jsonl"),
            r#"{"account_id":"stale"}"#,
        )
        .unwrap();

        // Switch semantics: config written, auth.json untouched.
        let result = write_mirror_proxy_files(
            mirror.to_str(),
            "http://127.0.0.1:8989/v1",
            "aag-proxy-token",
            "gpt-5",
            Codex_mirror_mode::Exclusive,
            false,
        )
        .expect("write mirror contract");

        let written_names: Vec<String> = result
            .written
            .iter()
            .map(|p| {
                std::path::Path::new(p)
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert!(written_names.contains(&"config.toml".to_string()));
        assert!(
            !written_names.contains(&"auth.json".to_string()),
            "a switch must not touch auth.json"
        );
        let auth = std::fs::read_to_string(mirror.join("auth.json")).unwrap();
        assert_eq!(
            auth, stale_auth,
            "switch must preserve the mirror's own auth state"
        );
        assert!(
            mirror.join(".codex-global-state.json").exists(),
            "switch must not touch the app's global state either"
        );

        let config = std::fs::read_to_string(mirror.join("config.toml")).unwrap();
        assert!(
            config.contains("base_url = \"http://127.0.0.1:8989/v1\""),
            "{config}"
        );
        assert!(config.contains("wire_api = \"responses\""), "{config}");
        assert!(config.contains("requires_openai_auth = false"), "{config}");
        assert!(
            config.contains("experimental_bearer_token = \"aag-proxy-token\""),
            "{config}"
        );
        assert!(config.contains("model = \"gpt-5\""), "{config}");
        assert!(
            !config.contains("sk-"),
            "no provider key may reach the mirror: {config}"
        );

        // Desktop-launch semantics: the stale login is purged to `{}`.
        let result = write_mirror_proxy_files(
            mirror.to_str(),
            "http://127.0.0.1:8989/v1",
            "aag-proxy-token",
            "gpt-5",
            Codex_mirror_mode::Exclusive,
            true,
        )
        .expect("write mirror contract");
        let written_names: Vec<String> = result
            .written
            .iter()
            .map(|p| {
                std::path::Path::new(p)
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert!(written_names.contains(&"auth.json".to_string()));
        let auth = std::fs::read_to_string(mirror.join("auth.json")).unwrap();
        assert_eq!(
            auth.trim(),
            "{}",
            "open-desktop must start the mirror logged out"
        );
        assert!(
            !mirror.join(".codex-global-state.json").exists()
                && !mirror.join(".codex-global-state.json.bak").exists()
                && !mirror.join("cache").exists()
                && !mirror.join("sessions").exists(),
            "open-desktop must drop every session-state carrier so the login page is clean"
        );

        // TOML escaping: quotes and backslashes in a token must not break
        // the generated config.
        write_mirror_proxy_files(
            mirror.to_str(),
            "http://127.0.0.1:8989/v1",
            "tok\"ens\\x",
            "m",
            Codex_mirror_mode::Exclusive,
            false,
        )
        .unwrap();
        let config = std::fs::read_to_string(mirror.join("config.toml")).unwrap();
        assert!(
            config.contains("experimental_bearer_token = \"tok\\\"ens\\\\x\""),
            "token must be TOML-escaped, got: {config}"
        );
    }

    /// auth.json classification is shape-only: {} / null / missing are
    /// "empty" (logged out); anything else counts as occupied.
    #[test]
    fn mirror_auth_json_is_classified_by_shape_only() {
        let home = crate::services::provider::test_support::isolated_home("codex-classify");
        let mirror = home.root.join("codex-mirror");
        let _ = std::fs::remove_dir_all(&mirror);
        std::fs::create_dir_all(&mirror).unwrap();
        assert_eq!(classify_mirror_auth_json(&mirror), "missing");
        std::fs::write(mirror.join("auth.json"), "{}").unwrap();
        assert_eq!(classify_mirror_auth_json(&mirror), "empty");
        std::fs::write(mirror.join("auth.json"), "null").unwrap();
        assert_eq!(classify_mirror_auth_json(&mirror), "empty");
        std::fs::write(
            mirror.join("auth.json"),
            r#"{"tokens":{"access_token":"x"}}"#,
        )
        .unwrap();
        assert_eq!(classify_mirror_auth_json(&mirror), "credentials_present");
    }

    /// Coexist mode keeps the ChatGPT login usable as the default: no
    /// top-level provider/model is written, AAG stays declared, and a
    /// `[profiles.aag]` profile points at it for manual switching.
    #[test]
    fn coexist_template_omits_top_level_provider_and_declares_profile() {
        let home = crate::services::provider::test_support::isolated_home("codex-coexist");
        let mirror = home.root.join("codex-mirror");
        let _ = std::fs::remove_dir_all(&mirror);
        std::fs::create_dir_all(&mirror).unwrap();

        write_mirror_proxy_files(
            mirror.to_str(),
            "http://127.0.0.1:8989/v1",
            "aag-proxy-token",
            "claude-via-aag",
            Codex_mirror_mode::Coexist,
            false,
        )
        .expect("write coexist mirror contract");
        let config = std::fs::read_to_string(mirror.join("config.toml")).unwrap();

        // No top-level keys before the first table header: Codex defaults
        // to its builtin openai provider, so an in-mirror ChatGPT login
        // serves official models without AAG in the path.
        let first_table = config.find('[').unwrap();
        assert!(
            !config[..first_table].contains("model_provider"),
            "coexist must not force a top-level model_provider: {config}"
        );
        assert!(
            !config[..first_table].contains("model ="),
            "coexist must not force a top-level model: {config}"
        );
        // AAG stays declared and reachable via the profile.
        assert!(config.contains("[model_providers.aag]"), "{config}");
        assert!(config.contains("[profiles.aag]"), "{config}");
        assert!(config.contains("model_provider = \"aag\""), "{config}");
        assert!(config.contains("model = \"claude-via-aag\""), "{config}");
        assert!(
            config.contains("experimental_bearer_token = \"aag-proxy-token\""),
            "{config}"
        );
    }

    /// Mode parsing fails safe to exclusive for unknown/absent values.
    #[test]
    fn mirror_mode_parsing_fails_safe_to_exclusive() {
        assert_eq!(
            Codex_mirror_mode::from_setting(Some("coexist")),
            Codex_mirror_mode::Coexist
        );
        assert_eq!(
            Codex_mirror_mode::from_setting(Some("exclusive")),
            Codex_mirror_mode::Exclusive
        );
        assert_eq!(
            Codex_mirror_mode::from_setting(Some("nonsense")),
            Codex_mirror_mode::Exclusive
        );
        assert_eq!(
            Codex_mirror_mode::from_setting(None),
            Codex_mirror_mode::Exclusive
        );
    }

    /// The native-`~/.codex` guard must recognize the native directory in
    /// every spelling — trailing separators and (on Windows) case — because
    /// a live write into the native directory would clobber the user's
    /// ChatGPT login.
    #[test]
    fn native_codex_dir_is_recognized_in_any_spelling() {
        let native = crate::config::get_codex_config_dir();
        assert!(crate::config::same_dir(&native, &native));
        let trailing = native.as_os_str().to_string_lossy().to_string() + "\\";
        assert!(
            crate::config::same_dir(std::path::Path::new(&trailing), &native),
            "trailing separator must not evade the guard"
        );
        #[cfg(windows)]
        {
            let upper = native.as_os_str().to_string_lossy().to_uppercase();
            assert!(
                crate::config::same_dir(std::path::Path::new(&upper), &native),
                "case differences must not evade the guard"
            );
        }
    }
}
